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
//! Its edit list keeps what still applies to the new bytes: enabled
//! time-only ops and masks, with byte anchors moved to program offsets, and
//! every enabled excision as the skip of its time. Enabled byte-changing ops
//! are applied and dropped, and so are those that did not apply (rewrites
//! that need a mask, edits superseded by another, masks that cover nothing).
//! Disabled entries are kept exactly as given, whatever their op; note that
//! their byte coordinates still count the original recording's playback
//! stream.

use tap_model::Session;
use tap_model::edl::{EditEntry, EditOp, Src, SrcRange, TrackedRef};
use tap_model::event::{EventKind, Input, Output, TextEncoding};

use tap_index::{InputEdit, Location, PaneProgram, SessionIndex};

use crate::{Evaluation, Outcome};

fn program_offset(program: &PaneProgram, offset: u64) -> u64 {
    match program.locate(offset) {
        Location::Kept(p) => p,
        Location::Removed { program, .. } => program.start,
    }
}

/// The session as `evaluation` plays it. `raw` is the session's index and
/// `entries` the edit list `evaluation` was made from.
pub fn bake(
    session: &Session,
    raw: &SessionIndex,
    entries: &[EditEntry],
    evaluation: &Evaluation,
) -> Session {
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

    // Byte offsets in the remaining edits count the new bytes.
    let src = |s: &Src| match s {
        Src::Byte { pane, offset } => Src::Byte {
            pane: pane.clone(),
            offset: evaluation
                .programs
                .get(pane)
                .map_or(*offset, |p| program_offset(p, *offset)),
        },
        other => other.clone(),
    };
    let range = |r: &SrcRange| SrcRange {
        from: src(&r.from),
        to: src(&r.to),
    };
    let mut edits = Vec::new();
    for (entry, outcome) in entries.iter().zip(&evaluation.outcomes) {
        // Disabled edits had no effect and are kept exactly as given.
        if !entry.enabled {
            edits.push(entry.clone());
            continue;
        }
        let op = match (&entry.op, outcome) {
            (EditOp::Trim { r#in, out }, _) => EditOp::Trim {
                r#in: src(r#in),
                out: src(out),
            },
            (EditOp::Skip { from, to }, _) => EditOp::Skip {
                from: src(from),
                to: src(to),
            },
            (
                EditOp::CollapseIdle {
                    range: r,
                    threshold_us,
                    max_us,
                },
                _,
            ) => EditOp::CollapseIdle {
                range: r.as_ref().map(range),
                threshold_us: *threshold_us,
                max_us: *max_us,
            },
            (EditOp::Speed { range: r, rate }, _) => EditOp::Speed {
                range: range(r),
                rate: *rate,
            },
            (EditOp::Hold { at, duration_us }, _) => EditOp::Hold {
                at: src(at),
                duration_us: *duration_us,
            },
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
                EditOp::Skip {
                    from: Src::Us(time(segment.start)),
                    to: Src::Us(time(segment.end)),
                }
            }
            (
                EditOp::Mask {
                    pane, cells, style, ..
                },
                Outcome::Masked { region },
            ) => EditOp::Mask {
                pane: pane.clone(),
                anchor: TrackedRef {
                    byte: evaluation.masks[*region].anchor,
                },
                cells: *cells,
                style: style.clone(),
            },
            _ => continue,
        };
        edits.push(EditEntry {
            enabled: entry.enabled,
            op,
        });
    }
    out.edits.edits = edits;
    out
}
