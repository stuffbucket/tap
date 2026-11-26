//! Byte programs: what a pane plays after byte-changing edits.
//!
//! A pane's *raw stream* is its output events' playback bytes, concatenated
//! (see [`crate::stream`]); it is never changed. A [`PaneProgram`] describes
//! the stream an edited recording plays instead, as an ordered list of
//! [`Splice`]s over the raw stream: each removes a non-empty range of source
//! bytes and plays other bytes (possibly none) in its place. Everything
//! between splices is kept. A program with no splices is the raw stream.
//!
//! Positions in the raw stream are *source* offsets; positions in the
//! program are *program* offsets. [`PaneProgram::locate`] maps a source byte
//! to the program and [`PaneProgram::to_source`] maps back: a kept byte maps
//! to exactly one program byte and back again, a removed byte maps to the
//! bytes its splice plays, and an inserted program byte maps to the splice
//! that inserted it.
//!
//! **Events.** The program keeps the pane's events: each output event plays
//! the program bytes derived from its source bytes. A splice's inserted bytes
//! are derived from one source byte, its [`Attach`] point: the last byte it
//! removes (a replacement appears when the text it replaces would have), or
//! the first byte after the range (an excision's fixups apply after every
//! event inside the excised range, resizes included, together with what
//! follows). [`PaneProgram::boundary`] gives the program offset of the
//! boundary before a source byte under this rule, and is what event and
//! input offsets map through.
//!
//! **Input.** Input events are never replayed to the terminal, but they are
//! part of what an edited recording exports, so the program also records
//! which input events are withheld ([`InputEdit`]).
//!
//! **Identity.** Checkpoints and thumbnails are keyed by a hash of the
//! program up to an offset ([`PaneProgram::prefix_hash`]): the splices whose
//! inserted bytes start before it. With the raw stream fixed, the splices
//! determine every byte of the prefix, so two programs with the same prefix
//! hash at an offset play the same bytes up to it, and a checkpoint taken
//! before the first splice is shared with the raw stream's.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use serde::Serialize;

/// Where a splice's inserted bytes belong in the pane's events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attach {
    /// With the last source byte the splice removes.
    Last,
    /// With the first source byte after the removed range (or at the end of
    /// the pane's output when the range runs to it).
    Next,
}

/// One edit to the raw stream: `source` is not played; `bytes` are played
/// in its place.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Splice {
    /// The source bytes removed; never empty.
    pub source: Range<u64>,
    /// The bytes played instead; may be empty.
    #[serde(serialize_with = "lossy")]
    pub bytes: Vec<u8>,
    /// Which source byte the inserted bytes belong to.
    pub attach: Attach,
    /// Index of the edit-list entry that made it.
    pub op: usize,
}

fn lossy<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&String::from_utf8_lossy(bytes))
}

/// What happens to an input event in the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum InputEdit {
    /// Its bytes are withheld; its time and origin are kept and it is
    /// marked redacted.
    Redacted {
        /// The edit that withheld them.
        op: usize,
    },
    /// The event is dropped: it belonged to a command that was excised.
    Removed {
        /// The edit that removed it.
        op: usize,
    },
}

/// Where a source byte is in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The byte is kept, at this program offset (for the end of the source
    /// stream, the end of the program).
    Kept(u64),
    /// The byte was removed by splice `splice`, which plays `program` in its
    /// place.
    Removed {
        /// Index of the splice in [`PaneProgram::splices`].
        splice: usize,
        /// The program bytes the splice plays.
        program: Range<u64>,
    },
}

/// Where a program byte came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Source byte at this offset.
    Source(u64),
    /// Byte `offset` of the bytes splice `splice` inserted.
    Inserted {
        /// Index of the splice in [`PaneProgram::splices`].
        splice: usize,
        /// Offset into its bytes.
        offset: u64,
    },
}

/// Why splices do not make a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// A splice removes nothing or runs past the end of the stream.
    Range {
        /// The splice's op.
        op: usize,
        /// Its range.
        source: Range<u64>,
        /// Length of the stream.
        len: u64,
    },
    /// Two splices remove overlapping ranges.
    Overlap {
        /// The earlier splice's op.
        first: usize,
        /// The later splice's op.
        second: usize,
    },
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range { op, source, len } => write!(
                f,
                "edit #{op} splices bytes {}..{}, which is empty or past the end of the stream ({len} bytes)",
                source.start, source.end
            ),
            Self::Overlap { first, second } => {
                write!(f, "edits #{first} and #{second} splice overlapping bytes")
            }
        }
    }
}

impl std::error::Error for ProgramError {}

/// A pane's edited playback stream: splices over the raw stream, and the
/// input events it withholds. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneProgram {
    source_len: u64,
    splices: Vec<Splice>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    inputs: BTreeMap<usize, InputEdit>,
    /// Program offset where each splice's bytes start.
    #[serde(skip)]
    starts: Vec<u64>,
    /// Source bytes removed by splices before each one.
    #[serde(skip)]
    removed_before: Vec<u64>,
    #[serde(skip)]
    len: u64,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

impl PaneProgram {
    /// The raw stream of `source_len` bytes, unedited.
    pub fn identity(source_len: u64) -> Self {
        Self {
            source_len,
            len: source_len,
            ..Self::default()
        }
    }

    /// The program that plays `splices` over a raw stream of `source_len`
    /// bytes and withholds `inputs` (keyed by event index). Splices may be
    /// given in any order; they must not overlap.
    pub fn new(
        source_len: u64,
        mut splices: Vec<Splice>,
        inputs: BTreeMap<usize, InputEdit>,
    ) -> Result<Self, ProgramError> {
        splices.sort_by_key(|s| (s.source.start, s.op));
        for s in &splices {
            if s.source.start >= s.source.end || s.source.end > source_len {
                return Err(ProgramError::Range {
                    op: s.op,
                    source: s.source.clone(),
                    len: source_len,
                });
            }
        }
        for pair in splices.windows(2) {
            if pair[1].source.start < pair[0].source.end {
                return Err(ProgramError::Overlap {
                    first: pair[0].op,
                    second: pair[1].op,
                });
            }
        }
        let mut starts = Vec::with_capacity(splices.len());
        let mut removed_before = Vec::with_capacity(splices.len());
        let (mut removed, mut inserted) = (0u64, 0u64);
        for s in &splices {
            starts.push(s.source.start - removed + inserted);
            removed_before.push(removed);
            removed += s.source.end - s.source.start;
            inserted += s.bytes.len() as u64;
        }
        Ok(Self {
            source_len,
            len: source_len - removed + inserted,
            splices,
            inputs,
            starts,
            removed_before,
        })
    }

    /// Whether the program plays the raw stream unchanged and withholds no
    /// input.
    pub fn is_identity(&self) -> bool {
        self.splices.is_empty() && self.inputs.is_empty()
    }

    /// Length of the raw stream.
    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// Length of the program.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the program plays nothing.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The splices, in source order.
    pub fn splices(&self) -> &[Splice] {
        &self.splices
    }

    /// The input events the program withholds, by event index.
    pub fn inputs(&self) -> &BTreeMap<usize, InputEdit> {
        &self.inputs
    }

    /// The program bytes splice `index` plays.
    pub fn splice_range(&self, index: usize) -> Range<u64> {
        let start = self.starts[index];
        start..start + self.splices[index].bytes.len() as u64
    }

    /// Index of the splice whose removed range contains source byte `s`.
    fn splice_containing(&self, s: u64) -> Option<usize> {
        let k = self.splices.partition_point(|x| x.source.start <= s);
        (k > 0 && s < self.splices[k - 1].source.end).then(|| k - 1)
    }

    /// Where source byte `s` (or the end of the stream, `s == source_len`)
    /// is in the program.
    pub fn locate(&self, s: u64) -> Location {
        if let Some(k) = self.splice_containing(s) {
            return Location::Removed {
                splice: k,
                program: self.splice_range(k),
            };
        }
        if s >= self.source_len {
            return Location::Kept(self.len);
        }
        // Splices before `s` all end at or before it.
        let k = self.splices.partition_point(|x| x.source.start < s);
        let p = match k.checked_sub(1) {
            None => s,
            Some(last) => {
                let range = self.splice_range(last);
                range.end + (s - self.splices[last].source.end)
            }
        };
        Location::Kept(p)
    }

    /// Where program byte `p` came from. `p` must be less than
    /// [`PaneProgram::len`].
    pub fn to_source(&self, p: u64) -> Origin {
        let k = self.starts.partition_point(|&start| start <= p);
        match k.checked_sub(1) {
            None => Origin::Source(p),
            Some(k) => {
                let range = self.splice_range(k);
                if p < range.end {
                    Origin::Inserted {
                        splice: k,
                        offset: p - range.start,
                    }
                } else {
                    Origin::Source(self.splices[k].source.end + (p - range.end))
                }
            }
        }
    }

    /// The program offset of the boundary just before source byte `x`:
    /// everything derived from source bytes before `x` (kept bytes, and the
    /// bytes of splices attached to a byte before `x`). The end of the
    /// stream maps to the end of the program.
    pub fn boundary(&self, x: u64) -> u64 {
        if x >= self.source_len {
            return self.len;
        }
        let k = self.splices.partition_point(|s| s.source.start < x);
        let Some(last) = k.checked_sub(1) else {
            return x;
        };
        let s = &self.splices[last];
        let start = self.starts[last];
        let attach = match s.attach {
            Attach::Last => s.source.end - 1,
            Attach::Next => s.source.end,
        };
        let inserted = if attach < x { s.bytes.len() as u64 } else { 0 };
        let kept_after = x.saturating_sub(s.source.end);
        start + inserted + kept_after
    }

    /// Hash identifying the program's first `p` bytes, given the raw stream:
    /// of every splice whose bytes start before `p`. The raw stream's is the
    /// same at every offset, and so is any program's before its first splice.
    pub fn prefix_hash(&self, p: u64) -> u64 {
        let k = self.starts.partition_point(|&start| start < p);
        self.splices[..k].iter().fold(FNV_OFFSET, |hash, s| {
            let hash = fnv(hash, &s.source.start.to_le_bytes());
            let hash = fnv(hash, &s.source.end.to_le_bytes());
            let hash = fnv(hash, &(s.bytes.len() as u64).to_le_bytes());
            fnv(hash, &s.bytes)
        })
    }

    /// Source bytes removed by splices before splice `k`.
    pub fn removed_before(&self, k: usize) -> u64 {
        self.removed_before[k]
    }

    /// The program's bytes, given the raw stream.
    pub fn render(&self, raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len as usize);
        let mut at = 0usize;
        for s in &self.splices {
            out.extend_from_slice(&raw[at..s.source.start as usize]);
            out.extend_from_slice(&s.bytes);
            at = s.source.end as usize;
        }
        out.extend_from_slice(&raw[at..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splice(range: Range<u64>, bytes: &[u8], attach: Attach, op: usize) -> Splice {
        Splice {
            source: range,
            bytes: bytes.to_vec(),
            attach,
            op,
        }
    }

    #[test]
    fn maps_bytes_both_ways() {
        let raw = b"0123456789";
        let p = PaneProgram::new(
            10,
            vec![
                splice(2..4, b"ab", Attach::Last, 0),
                splice(6..9, b"", Attach::Next, 1),
            ],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(p.render(raw), b"01ab459");
        assert_eq!(p.len(), 7);
        assert_eq!(p.locate(1), Location::Kept(1));
        assert_eq!(
            p.locate(3),
            Location::Removed {
                splice: 0,
                program: 2..4
            }
        );
        assert_eq!(p.locate(4), Location::Kept(4));
        assert_eq!(p.locate(9), Location::Kept(6));
        assert_eq!(p.locate(10), Location::Kept(7));
        assert_eq!(
            p.to_source(2),
            Origin::Inserted {
                splice: 0,
                offset: 0
            }
        );
        assert_eq!(p.to_source(5), Origin::Source(5));
        assert_eq!(p.to_source(6), Origin::Source(9));
        // "ab" belongs to byte 3; nothing is inserted for 6..9.
        assert_eq!(p.boundary(3), 2);
        assert_eq!(p.boundary(4), 4);
        assert_eq!(p.boundary(7), 6);
        assert_eq!(p.boundary(9), 6);
        assert_eq!(p.boundary(10), 7);
    }

    #[test]
    fn next_attached_bytes_go_with_the_byte_after() {
        let p = PaneProgram::new(
            6,
            vec![splice(1..3, b"xyz", Attach::Next, 0)],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(p.render(b"abcdef"), b"axyzdef");
        assert_eq!(p.boundary(1), 1);
        assert_eq!(p.boundary(3), 1);
        assert_eq!(p.boundary(4), 5);
        // At the end of the stream the bytes are still played.
        let end = PaneProgram::new(
            3,
            vec![splice(1..3, b"xyz", Attach::Next, 0)],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(end.boundary(3), 4);
        assert_eq!(end.locate(3), Location::Kept(4));
    }

    #[test]
    fn prefix_hash_changes_only_after_a_splice() {
        let identity = PaneProgram::identity(10);
        let p = PaneProgram::new(
            10,
            vec![splice(5..6, b"Q", Attach::Last, 0)],
            BTreeMap::new(),
        )
        .unwrap();
        for offset in 0..=5 {
            assert_eq!(p.prefix_hash(offset), identity.prefix_hash(offset));
        }
        assert_ne!(p.prefix_hash(6), identity.prefix_hash(6));
    }

    #[test]
    fn rejects_overlaps_and_empty_ranges() {
        let overlap = PaneProgram::new(
            10,
            vec![
                splice(2..5, b"", Attach::Next, 0),
                splice(4..6, b"", Attach::Next, 1),
            ],
            BTreeMap::new(),
        );
        assert_eq!(
            overlap,
            Err(ProgramError::Overlap {
                first: 0,
                second: 1
            })
        );
        assert!(
            PaneProgram::new(
                10,
                vec![splice(3..3, b"", Attach::Next, 0)],
                BTreeMap::new()
            )
            .is_err()
        );
        assert!(
            PaneProgram::new(
                10,
                vec![splice(8..11, b"", Attach::Next, 0)],
                BTreeMap::new()
            )
            .is_err()
        );
    }
}
