//! The edit decision list: non-destructive edits anchored to source
//! coordinates. See `docs/editor-design.md`, "Edits: a non-destructive EDL".
//!
//! Every op names the recording it changes by **source** coordinates (a
//! source microsecond, or a pane plus a byte offset into that pane's output),
//! never by edited time, so the order of the list does not matter for the
//! time-only ops (see [`crate::timemap`]) and re-segmenting a recording does
//! not move them.
//!
//! JSON form: each op is an object tagged by `"op"`, with camelCase fields:
//!
//! ```json
//! { "op": "skip", "from": { "us": 1000000 }, "to": { "byte": { "pane": "main", "offset": 4096 } } }
//! { "op": "speed", "range": { "from": { "us": 0 }, "to": { "us": 5000000 } }, "rate": { "constant": { "num": 2, "den": 1 } } }
//! { "op": "collapseIdle", "thresholdUs": 1500000, "maxUs": 500000, "enabled": false }
//! ```
//!
//! An entry may carry `"enabled": false`; disabled entries are kept in the
//! list (so the UI can offer them again) but have no effect.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::pane_id::PaneId;

/// A point in the source recording.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Src {
    /// A source time in microseconds: `{"us": 1500000}`.
    Us(u64),
    /// The moment a pane's output byte was recorded:
    /// `{"byte": {"pane": "main", "offset": 4096}}`.
    ///
    /// `offset` counts bytes of the pane's *playback stream*: each `output`
    /// event's `playbackData` when it has one, its `data` otherwise,
    /// concatenated in event order. That is the stream that is replayed and
    /// indexed, and every byte coordinate in the edit list ([`SegId`],
    /// [`ByteRange`], [`TrackedRef`]) counts the same bytes. It resolves to
    /// the `at_us` of the event that carries that byte.
    Byte {
        /// The pane whose output stream is meant.
        pane: PaneId,
        /// Zero-based offset into that stream.
        offset: u64,
    },
}

/// A half-open source range `[from, to)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcRange {
    /// Inclusive start.
    pub from: Src,
    /// Exclusive end.
    pub to: Src,
}

/// A positive rational `num / den`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ratio {
    /// Numerator, at least 1.
    pub num: u32,
    /// Denominator, at least 1.
    pub den: u32,
}

/// A playback speed: `2/1` plays twice as fast, `1/2` at half speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Rate {
    /// One speed over the whole range: `{"constant": {"num": 2, "den": 1}}`.
    Constant(Ratio),
    /// A speed that changes linearly from `from` to `to` across the range.
    Ramp {
        /// Speed at the start of the range.
        from: Ratio,
        /// Speed at the end of the range.
        to: Ratio,
    },
}

/// A command segment, identified by its source byte range in one pane's
/// playback stream (see [`Src::Byte`]) so the reference survives
/// re-segmentation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegId {
    /// The pane.
    pub pane: PaneId,
    /// First byte of the segment.
    pub start: u64,
    /// One past its last byte.
    pub end: u64,
}

/// A half-open range of byte offsets in a pane's playback stream (see
/// [`Src::Byte`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    /// Inclusive start.
    pub start: u64,
    /// Exclusive end.
    pub end: u64,
}

/// A half-open range of event sequence numbers in one pane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeqRange {
    /// Inclusive start.
    pub start: u64,
    /// Exclusive end.
    pub end: u64,
}

/// A grid cell followed through scrolling and reflow: the cell drawn by the
/// printable output byte at `byte`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedRef {
    /// Offset of the byte in the pane's playback stream (see
    /// [`Src::Byte`]).
    pub byte: u64,
}

/// How a mask covers cells.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum MaskStyle {
    /// Blur the cells.
    Blur,
    /// Pixelate the cells.
    Pixelate,
    /// Paint the cells one colour.
    Solid {
        /// CSS colour.
        color: String,
    },
}

/// One edit. The three groups differ in what they may change; see the
/// variant docs and [`OpClass`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditOp {
    /// Time-only. Keep only `[in, out]`. Playback starts from the terminal
    /// state at `in` and never shows events after `out`.
    Trim {
        /// First kept instant.
        r#in: Src,
        /// Last kept instant.
        out: Src,
    },
    /// Time-only. Jump cut: `[from, to)` takes no time; its bytes apply
    /// instantly and their output stays in scrollback.
    Skip {
        /// Start of the cut.
        from: Src,
        /// End of the cut (exclusive).
        to: Src,
    },
    /// Time-only. Shorten every idle gap longer than `threshold_us` to
    /// `max_us`, within `range` or the whole recording.
    CollapseIdle {
        /// Where to look for gaps; the whole recording when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<SrcRange>,
        /// Gaps longer than this are collapsed.
        threshold_us: u64,
        /// How long a collapsed gap lasts; at most `threshold_us`.
        max_us: u64,
    },
    /// Time-only. Play `range` at `rate`.
    Speed {
        /// The affected source range.
        range: SrcRange,
        /// The speed.
        rate: Rate,
    },
    /// Time-only. Even out typing in a segment to `cadence_us` per key.
    RetimeTyping {
        /// The segment whose typing is retimed.
        segment: SegId,
        /// Time per keystroke.
        cadence_us: u64,
        /// Seed for small random variation, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jitter_seed: Option<u64>,
    },
    /// Time-only. Freeze the frame after source instant `at` for
    /// `duration_us`.
    Hold {
        /// The instant to hold.
        at: Src,
        /// How long to hold it.
        duration_us: u64,
    },
    /// Byte-changing. Remove a command as if it never happened: the bytes
    /// from one OSC 133 prompt mark to the next (or the end of the output)
    /// are not played, fixups restore the terminal state they changed, and
    /// their time is cut. When that cannot be exact it plays as a skip; see
    /// `crates/tap-eval`.
    Excise {
        /// The command's segment.
        segment: SegId,
    },
    /// Byte-changing. Replace output text in place, checked against
    /// `expect`. Only text drawn as plain text, replaced by text taking the
    /// same cells, can be; otherwise it needs a mask (see `crates/tap-eval`).
    ReplaceText {
        /// The pane.
        pane: PaneId,
        /// The output bytes to replace: printable characters, and the SGR
        /// sequences between them, which are kept.
        bytes: ByteRange,
        /// The replacement text.
        text: String,
        /// What the cells those bytes draw must read (plain text, soft
        /// wraps joined, trailing blanks trimmed) right after they are
        /// drawn with the replacement.
        expect: String,
    },
    /// Byte-changing. Withhold the bytes of some input events (their time
    /// and origin stay, marked redacted), so exports do not carry them.
    RedactInput {
        /// The pane.
        pane: PaneId,
        /// The input events, by sequence number.
        events: SeqRange,
    },
    /// Render-time. Cover cells without touching bytes.
    Mask {
        /// The pane.
        pane: PaneId,
        /// The first covered cell.
        anchor: TrackedRef,
        /// How many cells to cover.
        cells: u32,
        /// How to cover them.
        style: MaskStyle,
    },
}

/// What an op is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpClass {
    /// Changes only timing; never the bytes, so never invalidates checkpoints.
    TimeOnly,
    /// Produces a different byte program.
    ByteChanging,
    /// Changes only how frames are drawn.
    RenderTime,
}

impl EditOp {
    /// The op's `"op"` tag.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Trim { .. } => "trim",
            Self::Skip { .. } => "skip",
            Self::CollapseIdle { .. } => "collapseIdle",
            Self::Speed { .. } => "speed",
            Self::RetimeTyping { .. } => "retimeTyping",
            Self::Hold { .. } => "hold",
            Self::Excise { .. } => "excise",
            Self::ReplaceText { .. } => "replaceText",
            Self::RedactInput { .. } => "redactInput",
            Self::Mask { .. } => "mask",
        }
    }

    /// Which group the op belongs to.
    pub fn class(&self) -> OpClass {
        match self {
            Self::Trim { .. }
            | Self::Skip { .. }
            | Self::CollapseIdle { .. }
            | Self::Speed { .. }
            | Self::RetimeTyping { .. }
            | Self::Hold { .. } => OpClass::TimeOnly,
            Self::Excise { .. } | Self::ReplaceText { .. } | Self::RedactInput { .. } => {
                OpClass::ByteChanging
            }
            Self::Mask { .. } => OpClass::RenderTime,
        }
    }

    /// Every pane the op refers to, for validation against the manifest.
    pub fn panes(&self) -> Vec<&PaneId> {
        fn src(s: &Src) -> Option<&PaneId> {
            match s {
                Src::Us(_) => None,
                Src::Byte { pane, .. } => Some(pane),
            }
        }
        match self {
            Self::Trim { r#in: a, out: b } | Self::Skip { from: a, to: b } => {
                [src(a), src(b)].into_iter().flatten().collect()
            }
            Self::CollapseIdle { range, .. } => range
                .iter()
                .flat_map(|r| [src(&r.from), src(&r.to)])
                .flatten()
                .collect(),
            Self::Speed { range, .. } => [src(&range.from), src(&range.to)]
                .into_iter()
                .flatten()
                .collect(),
            Self::Hold { at, .. } => src(at).into_iter().collect(),
            Self::RetimeTyping { segment, .. } | Self::Excise { segment } => vec![&segment.pane],
            Self::ReplaceText { pane, .. }
            | Self::RedactInput { pane, .. }
            | Self::Mask { pane, .. } => vec![pane],
        }
    }
}

/// One entry in the edit list: an op and whether it is switched on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EditEntry {
    /// Whether the op takes effect. Serialised only when `false`.
    pub enabled: bool,
    /// The op.
    pub op: EditOp,
}

impl EditEntry {
    /// An enabled entry.
    pub fn new(op: EditOp) -> Self {
        Self { enabled: true, op }
    }
}

impl From<EditOp> for EditEntry {
    fn from(op: EditOp) -> Self {
        Self::new(op)
    }
}

impl Serialize for EditEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.op).map_err(serde::ser::Error::custom)?;
        if !self.enabled
            && let Some(map) = value.as_object_mut()
        {
            map.insert("enabled".into(), false.into());
        }
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EditEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut map = serde_json::Map::deserialize(deserializer)?;
        let enabled = match map.remove("enabled") {
            None => true,
            Some(serde_json::Value::Bool(b)) => b,
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "`enabled` must be a boolean, not {other}"
                )));
            }
        };
        let op = EditOp::deserialize(serde_json::Value::Object(map)).map_err(D::Error::custom)?;
        Ok(Self { enabled, op })
    }
}
