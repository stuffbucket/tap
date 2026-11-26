//! Replaying a pane's stream into a terminal, checkpoints and seek.
//!
//! **Positions.** A replay position is `(item, intra)`: every event before
//! `items[item]` has been applied, and the first `intra` bytes of that event
//! (when it is output). Positions order lexicographically, so "the state at
//! source time t" is the position `(first event after t, 0)`.
//!
//! **Checkpoints** are libghostty-vt snapshots of the terminal, always taken
//! with the parser at ground. The indexer takes one at every segment
//! boundary and whenever 256 KiB of output or 10 s of source time have
//! passed since the last one. Each is keyed by `(engine fingerprint, pane,
//! stream byte offset)`; at most one is kept per offset, and it records its
//! exact position, so a resize that happens at the same offset is replayed
//! after it.
//!
//! **Seek** restores the nearest checkpoint at or before the target position
//! and replays the rest. Checkpoints live in memory; persisting them under
//! `Session.tap/checkpoints/<fingerprint>/` is left for later.

use std::collections::BTreeMap;

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
    /// libghostty-vt snapshot bytes.
    pub snapshot: Vec<u8>,
}

/// The checkpoints of one session, in memory.
#[derive(Debug, Clone, Default)]
pub struct Checkpoints {
    by_pane: BTreeMap<PaneId, BTreeMap<u64, Checkpoint>>,
}

impl Checkpoints {
    /// Every checkpoint of a pane, in stream order.
    pub fn pane(&self, pane: &PaneId) -> impl Iterator<Item = &Checkpoint> {
        self.by_pane.get(pane).into_iter().flat_map(|m| m.values())
    }

    /// Looks a checkpoint up by its key.
    pub fn get(&self, key: &CheckpointKey) -> Option<&Checkpoint> {
        if key.engine != vt::engine_fingerprint() {
            return None;
        }
        self.by_pane.get(&key.pane)?.get(&key.byte)
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

    pub(crate) fn insert(&mut self, checkpoint: Checkpoint) {
        self.by_pane
            .entry(checkpoint.key.pane.clone())
            .or_default()
            .entry(checkpoint.key.byte)
            .or_insert(checkpoint);
    }
}

/// A terminal being driven through a pane's events.
pub(crate) struct Replay<'s> {
    stream: &'s PaneStream,
    pub term: vt::Terminal,
    position: Position,
}

impl<'s> Replay<'s> {
    /// A fresh terminal at the start of the pane.
    pub fn start(stream: &'s PaneStream) -> Result<Self, VtError> {
        let (cols, rows) = stream.initial_size;
        Ok(Self {
            stream,
            term: vt::Terminal::new(cols, rows)?,
            position: Position { item: 0, intra: 0 },
        })
    }

    /// The terminal restored from a checkpoint.
    pub fn resume(stream: &'s PaneStream, checkpoint: &Checkpoint) -> Result<Self, VtError> {
        Ok(Self {
            stream,
            term: vt::Terminal::restore(&checkpoint.snapshot)?,
            position: checkpoint.position,
        })
    }

    pub fn position(&self) -> Position {
        self.position
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

    fn done(&self) -> bool {
        self.position.item >= self.stream.items.len()
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
                    self.position = Position {
                        item: self.position.item + 1,
                        intra: 0,
                    };
                }
            }
            ItemKind::Resize { cols, rows } => {
                self.term.resize(cols, rows)?;
                self.position.item += 1;
            }
            ItemKind::Input { .. } | ItemKind::Other => self.position.item += 1,
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
                self.position = Position {
                    item: self.position.item + 1,
                    intra: 0,
                };
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
    engine: String,
    last: Option<(u64, u64)>,
    store: &'c mut Checkpoints,
    pub error: Option<VtError>,
}

impl<'c> Checkpointer<'c> {
    pub fn new(pane: PaneId, store: &'c mut Checkpoints) -> Self {
        Self {
            pane,
            engine: vt::engine_fingerprint(),
            last: None,
            store,
            error: None,
        }
    }

    /// Takes a checkpoint if the policy's byte or time budget has run out.
    pub fn periodic(&mut self, replay: &Replay<'_>) {
        let due = self.last.is_none_or(|(byte, at)| {
            replay.byte() >= byte + CHECKPOINT_BYTES || replay.at_us() >= at + CHECKPOINT_US
        });
        if due {
            self.take(replay);
        }
    }

    /// Takes a checkpoint now. The replay must be at ground.
    pub fn take(&mut self, replay: &Replay<'_>) {
        let byte = replay.byte();
        if self.last.is_some_and(|(b, _)| b == byte) {
            return;
        }
        match replay.term.snapshot() {
            Ok(snapshot) => {
                self.last = Some((byte, replay.at_us()));
                self.store.insert(Checkpoint {
                    key: CheckpointKey {
                        engine: self.engine.clone(),
                        pane: self.pane.clone(),
                        byte,
                    },
                    position: replay.position(),
                    at_us: replay.at_us(),
                    snapshot,
                });
            }
            Err(e) => {
                self.error.get_or_insert(e);
            }
        }
    }
}
