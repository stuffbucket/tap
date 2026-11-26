//! Replaying a pane's stream into a terminal, checkpoints and seek.
//!
//! **Positions.** A replay position is `(item, intra)`: every event before
//! `items[item]` has been applied, and the first `intra` bytes of that event
//! (when it is output). Positions order lexicographically, so "the state at
//! source time t" is the position `(first event after t, 0)`.
//!
//! **Scrollback.** The terminals checkpoints, seek and the reference
//! replay use keep Ghostty's default scrollback
//! ([`vt::GHOSTTY_SCROLLBACK_BYTES`]) unless told otherwise, and every
//! replay holds them to that limit at the end of every event
//! (`Replay::finish_event`), so at every event boundary they are in a state
//! a snapshot can restore. What seek then guarantees is in the crate
//! README, "Checkpoints and seek".
//!
//! **Checkpoints** are libghostty-vt snapshots, always taken with the parser
//! at ground. The indexer tries to take one at every segment boundary and
//! whenever 256 KiB of output, 10 s of source time or
//! [`CHECKPOINT_RESIZES`] resizes have passed since the last one, and takes
//! it unless
//!
//! - the terminal is over its scrollback limit, which can only happen inside
//!   an event ([`vt::Terminal::within_scrollback_limit`]), and restoring the
//!   snapshot would bring back fewer pages; the next ground point is tried
//!   instead, or
//! - it would store more new bytes than [`CHECKPOINT_ALLOWANCE`] plus
//!   [`CHECKPOINT_RATIO`] times the output since the last checkpoint, and
//!   fewer than `CHECKPOINT_RESIZES` resizes have passed; none is then tried
//!   until enough output has passed to pay for it.
//!
//! Each is keyed by `(engine fingerprint, pane, stream byte offset)`; at
//! most one is kept per offset, and it records its exact position, so a
//! resize that happens at the same offset is replayed after it.
//!
//! **Storage.** A snapshot is a sequence of independently checksummed
//! records, and each page of history is one record. A page that has
//! scrolled into history does not change until a reflow, a clear or a
//! reset rewrites it, so consecutive checkpoints encode it to the same
//! bytes. [`Checkpoints`] keeps every distinct record once and each
//! checkpoint as a list of shared records, so storage grows with the
//! history the session produced, not with the number of checkpoints times
//! the history each holds. The allowance bounds it whatever the output:
//! at most `CHECKPOINT_ALLOWANCE` per checkpoint, plus `CHECKPOINT_RATIO`
//! per byte of output, plus one snapshot per `CHECKPOINT_RESIZES` resizes.
//!
//! **Seek** restores the nearest checkpoint at or before the target position
//! and replays the rest: at most 256 KiB of output and 10 s, or the output
//! that pays for the largest snapshot (its size over `CHECKPOINT_RATIO`),
//! and at most `CHECKPOINT_RESIZES` resizes. Checkpoints live in memory;
//! persisting them under `Session.tap/checkpoints/<fingerprint>/` is left
//! for later.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::Serialize;
use tap_model::PaneId;

use crate::stream::{ItemKind, PaneStream};
use crate::vt::{self, VtError};

/// Take a checkpoint after this much output…
pub const CHECKPOINT_BYTES: u64 = 256 * 1024;
/// …or this much source time, whichever comes first.
pub const CHECKPOINT_US: u64 = 10_000_000;
/// Largest slice written in one call, so the byte and time policy can be
/// checked inside a long output event.
const SLICE: u64 = 64 * 1024;
/// New bytes any checkpoint may store…
pub const CHECKPOINT_ALLOWANCE: u64 = 16 * 1024;
/// …plus this many per byte of output since the last checkpoint.
pub const CHECKPOINT_RATIO: u64 = 16;
/// A checkpoint is taken whatever it costs once this many resizes have
/// passed since the last one, so a seek replays at most this many reflows.
pub const CHECKPOINT_RESIZES: u64 = 16;

/// A replay position: events before `item` applied, plus `intra` bytes of
/// `item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Position {
    /// Index of the next event to apply.
    pub item: usize,
    /// Bytes of that event already applied.
    pub intra: u64,
}

/// Identifies a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CheckpointKey {
    /// [`vt::engine_fingerprint`] of the engine that took it.
    pub engine: String,
    /// The pane.
    pub pane: PaneId,
    /// Hash of the byte program up to `byte`
    /// ([`crate::program::PaneProgram::prefix_hash`]): the same for the
    /// raw stream at every offset, and for any program before its first
    /// splice.
    pub program: u64,
    /// Stream byte offset.
    pub byte: u64,
}

/// A terminal snapshot at a known position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Its key.
    pub key: CheckpointKey,
    /// Where in the pane's events it was taken.
    pub position: Position,
    /// Source time of the last event applied.
    pub at_us: u64,
    /// The snapshot's records, shared with other checkpoints.
    records: Vec<Arc<[u8]>>,
}

impl Checkpoint {
    /// The libghostty-vt snapshot bytes.
    pub fn snapshot(&self) -> Vec<u8> {
        self.records.concat()
    }

    /// Length of the snapshot in bytes.
    pub fn snapshot_len(&self) -> u64 {
        self.records.iter().map(|r| r.len() as u64).sum()
    }
}

/// Cuts a snapshot into its envelope and records, so records repeated in
/// other snapshots can be shared. The framing is libghostty-vt's: a 10-byte
/// envelope (`GHOSTSNP`, version), then records of a 2-byte tag, a 4-byte
/// little-endian payload length, a 4-byte CRC and the payload. Anything that
/// does not parse is kept whole.
fn records(snapshot: &[u8]) -> Vec<&[u8]> {
    const ENVELOPE: usize = 10;
    const HEADER: usize = 10;
    if snapshot.len() < ENVELOPE || !snapshot.starts_with(b"GHOSTSNP") {
        return vec![snapshot];
    }
    let mut pieces = vec![&snapshot[..ENVELOPE]];
    let mut at = ENVELOPE;
    while at < snapshot.len() {
        let Some(len) = snapshot
            .get(at + 2..at + 6)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            return vec![snapshot];
        };
        let end = at + HEADER + len;
        if end > snapshot.len() {
            return vec![snapshot];
        }
        pieces.push(&snapshot[at..end]);
        at = end;
    }
    pieces
}

/// A record's first ten bytes, zero-padded.
fn record_key(record: &[u8]) -> [u8; 10] {
    let mut key = [0; 10];
    let n = record.len().min(10);
    key[..n].copy_from_slice(&record[..n]);
    key
}

/// The checkpoints of one session, in memory.
#[derive(Debug, Clone)]
pub struct Checkpoints {
    engine: String,
    by_pane: BTreeMap<PaneId, BTreeMap<u64, Checkpoint>>,
    /// Every distinct snapshot record, once, by its first ten bytes (for a
    /// record: tag, length and CRC), so finding one hashes ten bytes rather
    /// than the whole record.
    records: HashMap<[u8; 10], Vec<Arc<[u8]>>>,
    stored_bytes: u64,
}

impl Checkpoints {
    /// No checkpoints, for terminals with this engine fingerprint.
    pub fn new(engine: String) -> Self {
        Self {
            engine,
            by_pane: BTreeMap::new(),
            records: HashMap::new(),
            stored_bytes: 0,
        }
    }

    /// Every checkpoint of a pane, in stream order.
    pub fn pane(&self, pane: &PaneId) -> impl Iterator<Item = &Checkpoint> {
        self.by_pane.get(pane).into_iter().flat_map(|m| m.values())
    }

    /// Looks a checkpoint up by its key.
    pub fn get(&self, key: &CheckpointKey) -> Option<&Checkpoint> {
        if key.engine != self.engine {
            return None;
        }
        self.by_pane
            .get(&key.pane)?
            .get(&key.byte)
            .filter(|c| c.key == *key)
    }

    /// The last checkpoint of a pane at or before `position`.
    pub fn at_or_before(&self, pane: &PaneId, position: Position) -> Option<&Checkpoint> {
        self.pane(pane)
            .filter(|c| c.position <= position)
            .max_by_key(|c| c.position)
    }

    /// Number of checkpoints.
    pub fn len(&self) -> usize {
        self.by_pane.values().map(BTreeMap::len).sum()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of snapshot records held: each distinct record once.
    pub fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }

    /// Sum of the checkpoints' snapshot lengths, as if none shared a record.
    pub fn snapshot_bytes(&self) -> u64 {
        self.by_pane
            .values()
            .flat_map(BTreeMap::values)
            .map(Checkpoint::snapshot_len)
            .sum()
    }

    /// The stored copy of `record`, if there is one.
    fn stored(&self, record: &[u8]) -> Option<&Arc<[u8]>> {
        self.records
            .get(&record_key(record))?
            .iter()
            .find(|stored| ***stored == *record)
    }

    /// Bytes of `snapshot` that are not stored yet.
    fn new_bytes(&self, snapshot: &[u8]) -> u64 {
        records(snapshot)
            .into_iter()
            .filter(|r| self.stored(r).is_none())
            .map(|r| r.len() as u64)
            .sum()
    }

    fn insert(&mut self, key: CheckpointKey, position: Position, at_us: u64, snapshot: &[u8]) {
        if self
            .by_pane
            .get(&key.pane)
            .is_some_and(|slots| slots.contains_key(&key.byte))
        {
            return;
        }
        let records = records(snapshot)
            .into_iter()
            .map(|record| match self.stored(record) {
                Some(shared) => Arc::clone(shared),
                None => {
                    let shared: Arc<[u8]> = record.into();
                    self.stored_bytes += record.len() as u64;
                    self.records
                        .entry(record_key(record))
                        .or_default()
                        .push(Arc::clone(&shared));
                    shared
                }
            })
            .collect();
        self.by_pane.entry(key.pane.clone()).or_default().insert(
            key.byte,
            Checkpoint {
                key,
                position,
                at_us,
                records,
            },
        );
    }
}

/// A terminal being driven through a pane's events (raw or edited).
pub struct Replay<'s> {
    stream: &'s PaneStream,
    /// The terminal.
    pub term: vt::Terminal,
    position: Position,
    /// Resize events applied.
    resizes: u64,
}

impl<'s> Replay<'s> {
    /// A fresh terminal at the start of the pane.
    pub fn start(stream: &'s PaneStream, scrollback: vt::Scrollback) -> Result<Self, VtError> {
        let (cols, rows) = stream.initial_size;
        Ok(Self {
            stream,
            term: vt::Terminal::new(cols, rows, scrollback)?,
            position: Position { item: 0, intra: 0 },
            resizes: 0,
        })
    }

    /// The terminal restored from a checkpoint.
    pub fn resume(stream: &'s PaneStream, checkpoint: &Checkpoint) -> Result<Self, VtError> {
        Ok(Self {
            stream,
            term: vt::Terminal::restore(&checkpoint.snapshot())?,
            position: checkpoint.position,
            resizes: 0,
        })
    }

    /// Where the replay is.
    pub fn position(&self) -> Position {
        self.position
    }

    /// The stream being replayed.
    pub fn stream(&self) -> &'s PaneStream {
        self.stream
    }

    /// Stream offset of everything applied so far.
    pub fn byte(&self) -> u64 {
        self.stream
            .items
            .get(self.position.item)
            .map_or(self.stream.len(), |item| item.offset + self.position.intra)
    }

    /// Source time of the last event applied (0 before any).
    pub fn at_us(&self) -> u64 {
        let Position { item, intra } = self.position;
        if intra > 0 {
            self.stream.items[item].at_us
        } else if item > 0 {
            self.stream.items[item - 1].at_us
        } else {
            0
        }
    }

    /// Whether every event has been applied.
    pub fn done(&self) -> bool {
        self.position.item >= self.stream.items.len()
    }

    /// Moves past the current event once all of it has been applied, and
    /// holds the terminal to its scrollback limit.
    ///
    /// libghostty-vt only drops history when the screen grows a page, so a
    /// reflow or a page outgrowing its allocation can leave it over the
    /// limit until then, and a terminal restored from a snapshot taken in
    /// that state would come back with less history. Enforcing the limit
    /// after every event, on every replay, keeps the terminals the index
    /// compares in that respect identical, and ready to checkpoint at every
    /// event boundary. Ghostty would drop the same oldest pages a little
    /// later, at its next page.
    fn finish_event(&mut self) {
        self.position = Position {
            item: self.position.item + 1,
            intra: 0,
        };
        self.term.enforce_scrollback_limit();
    }

    /// Applies the next piece of the current event: up to `limit` stream
    /// bytes of output, or the whole of any other event.
    fn step(&mut self, limit: Option<u64>) -> Result<(), VtError> {
        let item = &self.stream.items[self.position.item];
        match item.kind {
            ItemKind::Output { len } => {
                let mut n = (len - self.position.intra).min(SLICE);
                if let Some(limit) = limit {
                    n = n.min(limit.saturating_sub(self.byte()).max(1));
                }
                if n > 0 {
                    let from = (item.offset + self.position.intra) as usize;
                    self.term.write(&self.stream.bytes[from..from + n as usize]);
                    self.term.take_signals();
                }
                self.position.intra += n;
                if self.position.intra >= len {
                    self.finish_event();
                }
            }
            ItemKind::Resize { cols, rows } => {
                self.term.resize(cols, rows)?;
                self.resizes += 1;
                self.finish_event();
            }
            ItemKind::Input { .. } | ItemKind::Other => self.finish_event(),
        }
        Ok(())
    }

    /// Finishes a sequence left open, so the parser is at ground. Only
    /// output bytes are consumed; events in between are applied in order.
    fn settle(&mut self) -> Result<(), VtError> {
        while !self.term.at_ground() && !self.done() {
            let item = &self.stream.items[self.position.item];
            let ItemKind::Output { len } = item.kind else {
                self.step(None)?;
                continue;
            };
            let from = (item.offset + self.position.intra) as usize;
            let to = (item.offset + len) as usize;
            let consumed = match self.term.write_sequence(&self.stream.bytes[from..to])? {
                Some(step) => step.consumed as u64,
                None => (to - from) as u64,
            };
            self.term.take_signals();
            self.position.intra += consumed;
            if self.position.intra >= len {
                self.finish_event();
            }
        }
        Ok(())
    }

    /// Applies output until at least `byte` stream bytes have been applied
    /// and the parser is at ground. Events after the last byte needed are
    /// not applied. `at_ground` runs whenever a piece ends at ground.
    pub fn run_to_byte(
        &mut self,
        byte: u64,
        at_ground: &mut dyn FnMut(&Self),
    ) -> Result<(), VtError> {
        while self.byte() < byte && !self.done() {
            self.step(Some(byte))?;
            if self.term.at_ground() {
                at_ground(self);
            }
        }
        self.settle()
    }

    /// Applies every event before `position.item`, then exactly
    /// `position.intra` bytes of that event, without finishing a sequence
    /// the last byte leaves open.
    pub fn run_to_position(&mut self, position: Position) -> Result<(), VtError> {
        self.run_to_item(position.item, &mut |_| {})?;
        while self.position < position && !self.done() {
            let target = self.stream.items[self.position.item].offset + position.intra;
            self.step(Some(target))?;
        }
        Ok(())
    }

    /// Applies every event before `item`.
    pub fn run_to_item(
        &mut self,
        item: usize,
        at_ground: &mut dyn FnMut(&Self),
    ) -> Result<(), VtError> {
        while self.position.item < item.min(self.stream.items.len()) {
            self.step(None)?;
            if self.term.at_ground() {
                at_ground(self);
            }
        }
        Ok(())
    }
}

/// Decides when the indexer takes checkpoints and keeps them.
pub(crate) struct Checkpointer<'c> {
    pane: PaneId,
    /// Stream offset, source time and resize count of the last checkpoint
    /// taken.
    last: Option<(u64, u64, u64)>,
    /// The last checkpoint tried cost more than its allowance; none is tried
    /// again before this stream offset, unless resizes force one.
    retry_at: u64,
    store: &'c mut Checkpoints,
    pub error: Option<VtError>,
}

impl<'c> Checkpointer<'c> {
    pub fn new(pane: PaneId, store: &'c mut Checkpoints) -> Self {
        Self {
            pane,
            last: None,
            retry_at: 0,
            store,
            error: None,
        }
    }

    /// Resizes since the last checkpoint.
    fn resizes(&self, replay: &Replay<'_>) -> u64 {
        replay.resizes - self.last.map_or(0, |(_, _, resizes)| resizes)
    }

    /// Takes a checkpoint if the policy's byte, time or resize budget has
    /// run out.
    pub fn periodic(&mut self, replay: &Replay<'_>) {
        let due = self.last.is_none_or(|(byte, at, _)| {
            replay.byte() >= byte + CHECKPOINT_BYTES || replay.at_us() >= at + CHECKPOINT_US
        }) || self.resizes(replay) >= CHECKPOINT_RESIZES;
        if due {
            self.take(replay);
        }
    }

    /// Takes a checkpoint now, if a restore would reproduce the terminal and
    /// its new bytes are within the allowance (or resizes force it). The
    /// replay must be at ground.
    pub fn take(&mut self, replay: &Replay<'_>) {
        let byte = replay.byte();
        let forced = self.resizes(replay) >= CHECKPOINT_RESIZES;
        if self.last.is_some_and(|(b, _, _)| b == byte) || (byte < self.retry_at && !forced) {
            return;
        }
        let snapshot = match replay.term.snapshot() {
            Ok(snapshot) => snapshot,
            Err(e) => {
                self.error.get_or_insert(e);
                return;
            }
        };
        // Over its limit, a restore may refuse pages of history; only trying
        // tells.
        if !replay.term.within_scrollback_limit()
            && !vt::Terminal::restore(&snapshot)
                .is_ok_and(|t| t.primary_pages() == replay.term.primary_pages())
        {
            return;
        }
        let base = self.last.map_or(0, |(b, _, _)| b);
        let cost = self.store.new_bytes(&snapshot);
        if !forced && cost > CHECKPOINT_ALLOWANCE + CHECKPOINT_RATIO * (byte - base) {
            // Wait until the output since the last checkpoint pays for it.
            self.retry_at = base + (cost - CHECKPOINT_ALLOWANCE).div_ceil(CHECKPOINT_RATIO);
            return;
        }
        self.last = Some((byte, replay.at_us(), replay.resizes));
        let key = CheckpointKey {
            engine: self.store.engine.clone(),
            pane: self.pane.clone(),
            program: replay.stream.program.prefix_hash(byte),
            byte,
        };
        self.store
            .insert(key, replay.position(), replay.at_us(), &snapshot);
    }
}
