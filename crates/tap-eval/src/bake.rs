//! Writing an edited session out.
//!
//! Exports of an edited session must contain what it plays, not what was
//! recorded: [`bake`] makes a new [`Session`] whose output events carry
//! their program bytes (so excised commands and rewritten text are gone),
//! whose withheld input events have no bytes (marked redacted, time and
//! origin kept) and whose excised commands' input events are dropped. The
//! raw session is not touched.
//!
//! Output events no edit touched are kept exactly as recorded
//! (`playbackData` and encodings included).
//!
//! **A bake never makes protected text visible.** Its edit list keeps
//! everything that still shows or hides something, anchored to the new
//! bytes:
//!
//! - enabled time-only ops, a byte anchor moved to the same byte, or, when
//!   that byte was removed, replaced by the moment it resolved to (events
//!   keep their times, so that is exact);
//! - every enabled excision, done or refused, as the skip of its time;
//! - every mask that resolved, anchored to the program byte it resolved to,
//!   so it covers the same cells over the same times;
//! - every `ReplaceText` that needed a mask, as masks (blur) over each piece
//!   of drawn text in its range; one whose range draws nothing is kept,
//!   moved.
//!
//! Applied edits (rewrites, withheld input) are in the bytes and are
//! dropped, and so are edits that had no effect (superseded rewrites, inert
//! masks): dropping them changes nothing anyone sees. When a protecting edit
//! cannot be carried (a mask would have to anchor in an event whose
//! recorded and playback bytes differ), the bake fails with
//! [`BakeError::Uncarried`] listing each one.
//!
//! Entries that were not evaluated (disabled, or already `preBake`) are
//! moved onto the new bytes when every byte they name survives unchanged;
//! otherwise they are kept as given and marked `preBake`, which evaluation
//! refuses, so they can never touch other bytes.

use std::ops::Range;

use serde::Serialize;
use tap_index::vt::codepoint_width;
use tap_index::{InputEdit, Location, PaneProgram, SessionIndex};
use tap_model::edl::{ByteRange, EditEntry, EditOp, MaskStyle, SegId, Src, SrcRange, TrackedRef};
use tap_model::event::{EventKind, Input, Output, TextEncoding};
use tap_model::{PaneId, Session};

use crate::{Evaluation, Outcome};

/// The session as `evaluation` plays it. `raw` is the session's index and
/// `entries` the edit list `evaluation` was made from.
pub fn bake(
    session: &Session,
    raw: &SessionIndex,
    entries: &[EditEntry],
    evaluation: &Evaluation,
) -> Result<Session, BakeError> {
    let mut out = session.clone();
    for pane in &raw.panes {
        let Some(program) = evaluation.programs.get(&pane.pane) else {
            continue;
        };
        let played = program.render(pane.stream_bytes());
        let edited = tap_index::stream::PaneStream::with_program(
            pane.pane.clone(),
            pane.stream().initial_size,
            session.pane_events(&pane.pane),
            session.manifest.duration_us,
            program.clone(),
        );
        let Some(events) = out.events.get_mut(&pane.pane) else {
            continue;
        };
        let mut kept = Vec::with_capacity(events.len());
        for (index, event) in events.drain(..).enumerate() {
            let mut event = event;
            match &mut event.kind {
                EventKind::Output(output) => {
                    let item = &edited.items[index];
                    let len = match item.kind {
                        tap_index::stream::ItemKind::Output { len } => len,
                        _ => 0,
                    };
                    let data = played[item.offset as usize..(item.offset + len) as usize].to_vec();
                    // An event no edit touched keeps exactly what was
                    // recorded, `playbackData` and encodings included.
                    let raw_item = &pane.stream().items[index];
                    if let tap_index::stream::ItemKind::Output { len: raw_len } = raw_item.kind
                        && pane.stream_bytes()
                            [raw_item.offset as usize..(raw_item.offset + raw_len) as usize]
                            == data[..]
                    {
                        kept.push(event);
                        continue;
                    }
                    let encoding = if std::str::from_utf8(&data).is_ok() {
                        output.data_encoding
                    } else {
                        TextEncoding::Base64
                    };
                    *output = Output {
                        data,
                        data_encoding: encoding,
                        playback: None,
                    };
                }
                EventKind::Input(input) => match program.inputs().get(&index) {
                    Some(InputEdit::Removed { .. }) => continue,
                    Some(InputEdit::Redacted { .. }) => {
                        *input = Input {
                            data: Vec::new(),
                            data_encoding: input.data_encoding,
                            origin: input.origin,
                            redacted: Some(true),
                        };
                    }
                    None => {}
                },
                _ => {}
            }
            kept.push(event);
        }
        *events = kept;
    }

    let anchors = Anchors {
        session,
        raw,
        evaluation,
        timeline: session.timeline(),
    };
    let mut edits = Vec::new();
    let mut uncarried = Vec::new();
    for (index, (entry, outcome)) in entries.iter().zip(&evaluation.outcomes).enumerate() {
        // Entries never evaluated: disabled, or already counting another
        // recording's bytes. Moved onto the new bytes when every byte they
        // name survives, else kept as given and marked preBake.
        if !entry.enabled || entry.pre_bake {
            edits.push(match (entry.pre_bake, anchors.op(&entry.op)) {
                (false, Some(op)) => EditEntry {
                    op,
                    ..entry.clone()
                },
                _ => EditEntry {
                    pre_bake: true,
                    ..entry.clone()
                },
            });
            continue;
        }
        match (&entry.op, outcome) {
            // Time-only: still applies; byte anchors moved.
            (_, Outcome::Time) => match anchors.op(&entry.op) {
                Some(op) => edits.push(EditEntry::new(op)),
                None => uncarried.push(Uncarried {
                    entry: index,
                    reason: "its byte anchors name no byte of the recording".into(),
                }),
            },
            // The excision's bytes are applied; its time cut stays.
            (EditOp::Excise { segment }, Outcome::Excised { .. } | Outcome::Refused { .. }) => {
                let pane = raw.pane(&segment.pane).ok();
                let time = |b: u64| {
                    pane.map_or(0, |p| {
                        if b >= p.stream_len {
                            p.stream().end_us
                        } else {
                            p.stream().time_of_byte(b)
                        }
                    })
                };
                edits.push(EditEntry::new(EditOp::Skip {
                    from: Src::Us(time(segment.start)),
                    to: Src::Us(time(segment.end)),
                }));
            }
            // A mask keeps covering the same cells: anchored to the program
            // byte it resolved to.
            (
                EditOp::Mask {
                    pane, cells, style, ..
                },
                Outcome::Masked { region },
            ) => edits.push(EditEntry::new(EditOp::Mask {
                pane: pane.clone(),
                anchor: TrackedRef {
                    byte: evaluation.masks[*region].anchor,
                },
                cells: *cells,
                style: style.clone(),
            })),
            // A rewrite that needed a mask gets one: over each piece of
            // drawn text in its range.
            (EditOp::ReplaceText { pane, bytes, .. }, Outcome::NeedsMask { .. }) => {
                match anchors.masks_for(pane, bytes.start..bytes.end) {
                    Ok(masks) if !masks.is_empty() => {
                        edits.extend(masks.into_iter().map(EditEntry::new));
                    }
                    // Nothing drawn (text inside a sequence): kept as it
                    // is, so it is refused the same way again.
                    Ok(_) => match anchors.op(&entry.op) {
                        Some(op) => edits.push(EditEntry::new(op)),
                        None => uncarried.push(Uncarried {
                            entry: index,
                            reason: "its bytes do not survive".into(),
                        }),
                    },
                    Err(reason) => uncarried.push(Uncarried {
                        entry: index,
                        reason,
                    }),
                }
            }
            // Applied to the bytes (rewrites, withheld input), or without
            // effect (superseded, inert masks): dropping them changes nothing
            // anyone sees.
            _ => {}
        }
    }
    if !uncarried.is_empty() {
        return Err(BakeError::Uncarried(uncarried));
    }
    out.edits.edits = edits;
    Ok(out)
}

/// An edit whose protection a bake cannot carry into the exported session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uncarried {
    /// Index of the entry in the edit list.
    pub entry: usize,
    /// Why.
    pub reason: String,
}

/// Why a session cannot be baked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BakeError {
    /// Edits that hide or protect text could not be carried: baking would
    /// make the text visible. Every such edit is listed.
    #[error("these edits cannot be carried into the export without showing what they hide: {}", list(.0))]
    Uncarried(Vec<Uncarried>),
}

fn list(items: &[Uncarried]) -> String {
    items
        .iter()
        .map(|u| format!("#{} ({})", u.entry, u.reason))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Moves byte coordinates onto the baked bytes.
struct Anchors<'a> {
    session: &'a Session,
    raw: &'a SessionIndex,
    evaluation: &'a Evaluation,
    timeline: tap_model::SourceTimeline,
}

impl Anchors<'_> {
    fn program(&self, pane: &PaneId) -> Option<&PaneProgram> {
        self.evaluation.programs.get(pane)
    }

    /// The baked offset of a byte that survives (or of the end of the
    /// stream).
    fn byte(&self, pane: &PaneId, offset: u64) -> Option<u64> {
        match self.program(pane)?.locate(offset) {
            Location::Kept(p) => Some(p),
            Location::Removed { .. } => None,
        }
    }

    /// The baked range of bytes that all survive, unchanged and contiguous.
    fn range(&self, pane: &PaneId, start: u64, end: u64) -> Option<(u64, u64)> {
        let program = self.program(pane)?;
        if start >= end
            || program
                .splices()
                .iter()
                .any(|s| s.source.start < end && start < s.source.end)
        {
            return None;
        }
        let p = self.byte(pane, start)?;
        Some((p, p + (end - start)))
    }

    /// A time anchor: the byte moved when it survives, else the moment it
    /// resolved to (bakes keep every event's time, so that is exact).
    fn src(&self, src: &Src) -> Option<Src> {
        match src {
            Src::Us(_) => Some(src.clone()),
            Src::Byte { pane, offset } => match self.byte(pane, *offset) {
                Some(p) => Some(Src::Byte {
                    pane: pane.clone(),
                    offset: p,
                }),
                None => self.timeline.resolve(src).ok().map(Src::Us),
            },
        }
    }

    fn src_range(&self, r: &SrcRange) -> Option<SrcRange> {
        Some(SrcRange {
            from: self.src(&r.from)?,
            to: self.src(&r.to)?,
        })
    }

    fn segment(&self, s: &SegId) -> Option<SegId> {
        let (start, end) = self.range(&s.pane, s.start, s.end)?;
        Some(SegId {
            pane: s.pane.clone(),
            start,
            end,
        })
    }

    /// The op with every coordinate moved onto the baked bytes, or `None`
    /// when a byte it names does not survive.
    fn op(&self, op: &EditOp) -> Option<EditOp> {
        Some(match op {
            EditOp::Trim { r#in, out } => EditOp::Trim {
                r#in: self.src(r#in)?,
                out: self.src(out)?,
            },
            EditOp::Skip { from, to } => EditOp::Skip {
                from: self.src(from)?,
                to: self.src(to)?,
            },
            EditOp::CollapseIdle {
                range,
                threshold_us,
                max_us,
            } => EditOp::CollapseIdle {
                range: match range {
                    Some(r) => Some(self.src_range(r)?),
                    None => None,
                },
                threshold_us: *threshold_us,
                max_us: *max_us,
            },
            EditOp::Speed { range, rate } => EditOp::Speed {
                range: self.src_range(range)?,
                rate: *rate,
            },
            EditOp::Hold { at, duration_us } => EditOp::Hold {
                at: self.src(at)?,
                duration_us: *duration_us,
            },
            EditOp::RetimeTyping {
                segment,
                cadence_us,
                jitter_seed,
            } => EditOp::RetimeTyping {
                segment: self.segment(segment)?,
                cadence_us: *cadence_us,
                jitter_seed: *jitter_seed,
            },
            EditOp::Excise { segment } => EditOp::Excise {
                segment: self.segment(segment)?,
            },
            EditOp::ReplaceText {
                pane,
                bytes,
                text,
                expect,
            } => {
                let (start, end) = self.range(pane, bytes.start, bytes.end)?;
                EditOp::ReplaceText {
                    pane: pane.clone(),
                    bytes: ByteRange { start, end },
                    text: text.clone(),
                    expect: expect.clone(),
                }
            }
            // Sequence numbers survive a bake: dropped events take theirs
            // with them, the rest keep theirs.
            EditOp::RedactInput { .. } => op.clone(),
            EditOp::Mask {
                pane,
                anchor,
                cells,
                style,
            } => {
                let (byte, _) = self.range(pane, anchor.byte, anchor.byte + 1)?;
                EditOp::Mask {
                    pane: pane.clone(),
                    anchor: TrackedRef { byte },
                    cells: *cells,
                    style: style.clone(),
                }
            }
        })
    }

    /// Masks over each piece of text the raw bytes `range` draw (one per
    /// printable run it crosses), anchored to the baked bytes.
    fn masks_for(&self, pane: &PaneId, range: Range<u64>) -> Result<Vec<EditOp>, String> {
        let index = self.raw.pane(pane).map_err(|e| e.to_string())?;
        let runs = index.runs().map_err(|e| e.to_string())?;
        let stream = index.stream_bytes();
        let events = self.session.pane_events(pane);
        let mut out = Vec::new();
        for run in runs
            .runs
            .iter()
            .filter(|r| r.bytes.start < range.end && range.start < r.bytes.end)
        {
            let Ok(text) =
                std::str::from_utf8(&stream[run.bytes.start as usize..run.bytes.end as usize])
            else {
                return Err(format!("the text at byte {} is not UTF-8", run.bytes.start));
            };
            let chars: Vec<(u64, u32)> = text
                .char_indices()
                .map(|(i, c)| (run.bytes.start + i as u64, u32::from(codepoint_width(c))))
                .filter(|&(at, _)| at >= range.start && at < range.end)
                .collect();
            let cells: u32 = chars.iter().map(|c| c.1).sum();
            let Some(&(first, _)) = chars.first() else {
                continue;
            };
            if cells == 0 {
                continue;
            }
            let item = index.stream().output_item_at(first);
            if let Some(event) = item
                && let EventKind::Output(o) = &events[event].kind
                && o.playback.as_ref().is_some_and(|p| p.data != o.data)
            {
                return Err(format!(
                    "byte {first} is in event {event}, whose recorded bytes differ from the bytes it plays, so no mask can anchor there"
                ));
            }
            let Some((byte, _)) = self.range(pane, first, first + 1) else {
                return Err(format!("byte {first} does not survive the bake"));
            };
            out.push(EditOp::Mask {
                pane: pane.clone(),
                anchor: TrackedRef { byte },
                cells,
                style: MaskStyle::Blur,
            });
        }
        Ok(out)
    }
}
