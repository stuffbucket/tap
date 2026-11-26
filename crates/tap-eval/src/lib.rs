//! Tap's edit evaluator: an edit decision list as a byte program per pane,
//! a time map and mask regions. See `docs/editor-design.md`, "Edits: a
//! non-destructive EDL", and this crate's README.
//!
//! [`evaluate`] runs every enabled op of an edit list against a session and
//! its index:
//!
//! - **Time-only ops** go into [`tap_model::TimeMap`].
//! - **Excise** removes a prompt-to-prompt range and plays fixups that
//!   restore the state it changed ([`excise`], [`state`]). When a
//!   precondition fails it is [`Outcome::Refused`] with every reason and
//!   played as a skip instead. Either way its time is cut.
//! - **`ReplaceText`** rewrites printable bytes in place when the text was
//!   drawn as plain text and the replacement takes the same cells, then
//!   checks the refolded text reads `expect` ([`replace`]); otherwise it is
//!   [`Outcome::NeedsMask`].
//! - **`RedactInput`** withholds the bytes of input events.
//! - **Mask** is resolved to the cells it covers over time
//!   ([`mask::MaskRegion`]); nothing is painted here.
//!
//! Only typing retiming and speed ramps are refused outright
//! ([`EvalError::Unsupported`]).
//!
//! The result's [`Evaluation::programs`] feed
//! [`tap_index::SessionIndex::build_programs`], so the transcript,
//! checkpoints and seek of the edited session come from the same index
//! code as the raw one; [`bake::bake`] writes the edited session out.
//!
//! Nothing here is published (`publish = false`).

pub mod bake;
pub mod excise;
pub mod mask;
pub mod redact;
pub mod replace;
pub mod search;
pub mod state;
pub mod tokens;

use std::collections::BTreeMap;
use std::ops::Range;

use serde::Serialize;
use tap_index::stream::{ItemKind, PaneStream};
use tap_index::vt::{self, Scrollback, VtError};
use tap_index::{
    Attach, IndexError, IndexOptions, InputEdit, Location, PaneIndex, PaneProgram, Position,
    ProgramError, Replay, SessionIndex, Splice,
};
use tap_model::edl::{EditEntry, EditOp, OpClass, Rate, Src};
use tap_model::timemap::UnsupportedOp;
use tap_model::{PaneId, Session, SourceTimeline, TimeMap};

pub use excise::{Refusal, Side, Unreadable};
pub use mask::{CellRect, MaskRegion, MaskSpan};
pub use replace::NeedsMask;
pub use state::{FixupPart, Fixups};

/// Why an edit list could not be evaluated.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// Enabled ops this build cannot evaluate yet (typing retiming, speed
    /// ramps). Every such op is listed.
    #[error("the edit list has operations that cannot be evaluated yet: {}", list(.0))]
    Unsupported(Vec<UnsupportedOp>),
    /// An op is malformed or refers to something that does not exist.
    #[error("edit #{index} ({op}): {reason}")]
    Invalid {
        /// Index of the entry in the edit list.
        index: usize,
        /// The op's tag.
        op: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// The time map could not be built.
    #[error(transparent)]
    Time(#[from] tap_model::EvalError),
    /// The index or the terminal engine failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The splices do not make a program (a bug: overlaps are resolved
    /// before splices are made).
    #[error(transparent)]
    Program(#[from] ProgramError),
}

impl From<VtError> for EvalError {
    fn from(e: VtError) -> Self {
        Self::Index(IndexError::Vt(e))
    }
}

fn list(ops: &[UnsupportedOp]) -> String {
    ops.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What became of one entry of the edit list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Outcome {
    /// The entry is disabled.
    Disabled,
    /// A time-only op, applied by the time map.
    Time,
    /// The command was excised: its bytes are gone and `fixups` play in
    /// their place. Its time is cut.
    #[serde(rename_all = "camelCase")]
    Excised {
        /// The source bytes removed.
        removed: Range<u64>,
        /// What plays instead.
        fixups: Fixups,
        /// Input events dropped with it (typed at or during the command).
        inputs_removed: usize,
    },
    /// The excision was refused; it is played as a skip (its time is cut,
    /// its bytes still apply).
    Refused {
        /// Every precondition that failed.
        reasons: Vec<Refusal>,
    },
    /// The text was rewritten in place.
    Replaced {
        /// The program bytes now drawing it.
        program: Range<u64>,
        /// How many splices it took (one per character where widths allow).
        splices: usize,
    },
    /// The text cannot be rewritten; it needs a mask.
    NeedsMask {
        /// Why.
        reason: NeedsMask,
    },
    /// Another edit removed or rewrote these bytes first.
    Superseded {
        /// Index of that edit.
        by: usize,
    },
    /// The bytes of these input events are withheld.
    Redacted {
        /// The events' indices in the pane.
        events: Vec<usize>,
    },
    /// The mask resolved to a region.
    Masked {
        /// Index in [`Evaluation::masks`].
        region: usize,
    },
    /// The mask covers nothing.
    Inert {
        /// Why.
        reason: String,
    },
}

/// An evaluated edit list.
#[derive(Debug, Clone)]
pub struct Evaluation {
    /// Each pane's byte program.
    pub programs: BTreeMap<PaneId, PaneProgram>,
    /// Source time → edited time, including the time excisions cut.
    pub time_map: TimeMap,
    /// One per entry of the edit list.
    pub outcomes: Vec<Outcome>,
    /// The masks, resolved.
    pub masks: Vec<MaskRegion>,
}

impl Evaluation {
    /// Indexes what the edited session plays (transcript, checkpoints,
    /// seek), with `options`.
    pub fn index(
        &self,
        session: &Session,
        options: IndexOptions,
    ) -> Result<SessionIndex, IndexError> {
        SessionIndex::build_programs(session, &self.programs, options)
    }
}

/// Evaluates the session's own edit list against `raw`, the session's
/// index.
pub fn evaluate(session: &Session, raw: &SessionIndex) -> Result<Evaluation, EvalError> {
    evaluate_entries(session, raw, &session.edits.edits)
}

/// The output item holding stream byte `p`, as a replay position; the end
/// of the last output event for the end of the stream.
pub fn byte_position(stream: &PaneStream, p: u64) -> Position {
    let out_len = |item: &tap_index::stream::Item| match item.kind {
        ItemKind::Output { len } => len,
        _ => 0,
    };
    if p < stream.len() {
        let mut i = stream
            .items
            .partition_point(|it| it.offset + out_len(it) <= p);
        while let Some(item) = stream.items.get(i) {
            let len = out_len(item);
            if len > 0 && item.offset <= p && p < item.offset + len {
                return Position {
                    item: i,
                    intra: p - item.offset,
                };
            }
            i += 1;
        }
    }
    last_output(stream).map_or(Position { item: 0, intra: 0 }, |i| Position {
        item: i,
        intra: out_len(&stream.items[i]),
    })
}

/// The position just after stream byte `p - 1`.
pub fn end_position(stream: &PaneStream, p: u64) -> Position {
    if p == 0 {
        return Position { item: 0, intra: 0 };
    }
    let at = byte_position(stream, p - 1);
    Position {
        item: at.item,
        intra: at.intra + 1,
    }
}

fn last_output(stream: &PaneStream) -> Option<usize> {
    stream
        .items
        .iter()
        .rposition(|it| matches!(it.kind, ItemKind::Output { len } if len > 0))
        .or_else(|| {
            stream
                .items
                .iter()
                .rposition(|it| matches!(it.kind, ItemKind::Output { .. }))
        })
}

/// The cell the next printed character goes to, in screen coordinates.
pub(crate) fn next_cell(term: &vt::Terminal) -> (u16, u32) {
    let (x, y) = term.cursor_on_screen();
    if term.cursor_pending_wrap() {
        (0, y + 1)
    } else {
        (x, y)
    }
}

/// The last cell printed, in screen coordinates.
pub(crate) fn last_cell(term: &vt::Terminal) -> (u16, u32) {
    let (x, y) = term.cursor_on_screen();
    if term.cursor_pending_wrap() {
        (x, y)
    } else if x > 0 {
        (x - 1, y)
    } else {
        (term.size().0.saturating_sub(1), y.saturating_sub(1))
    }
}

/// Source time of stream byte `byte`, or of the end of the pane.
fn time_of(stream: &PaneStream, byte: u64) -> u64 {
    if byte >= stream.len() {
        stream.end_us
    } else {
        stream.time_of_byte(byte)
    }
}

/// A planned replacement: its entry, its rewrites, `expect`, and the OSC
/// field (with the sequence's source bytes) when it rewrites one.
type Planned = (
    usize,
    Vec<replace::Rewrite>,
    String,
    Option<(replace::Field, Range<u64>)>,
);

struct ExciseCandidate {
    index: usize,
    range: Range<u64>,
    refusals: Vec<Refusal>,
}

struct Excised {
    index: usize,
    range: Range<u64>,
    fixups: Fixups,
}

/// Evaluates `entries` (in place of the session's own list) against `raw`,
/// the session's unedited index.
pub fn evaluate_entries(
    session: &Session,
    raw: &SessionIndex,
    entries: &[EditEntry],
) -> Result<Evaluation, EvalError> {
    let enabled = || entries.iter().enumerate().filter(|(_, e)| e.enabled);
    let unsupported: Vec<UnsupportedOp> = enabled()
        .filter_map(|(index, e)| {
            let reason = match &e.op {
                EditOp::RetimeTyping { .. } => "typing retiming is not implemented yet",
                EditOp::Speed {
                    rate: Rate::Ramp { .. },
                    ..
                } => "speed ramps are not supported yet",
                _ => return None,
            };
            Some(UnsupportedOp {
                index,
                op: e.op.name(),
                reason,
            })
        })
        .collect();
    if !unsupported.is_empty() {
        return Err(EvalError::Unsupported(unsupported));
    }
    for (index, entry) in enabled() {
        for pane in entry.op.panes() {
            if raw.pane(pane).is_err() {
                return Err(EvalError::Invalid {
                    index,
                    op: entry.op.name(),
                    reason: format!("there is no pane {:?}", pane.as_str()),
                });
            }
        }
    }

    let mut outcomes: Vec<Option<Outcome>> = entries
        .iter()
        .map(|e| match (e.enabled, e.op.class()) {
            (false, _) => Some(Outcome::Disabled),
            (true, OpClass::TimeOnly) => Some(Outcome::Time),
            _ => None,
        })
        .collect();
    let mut programs = BTreeMap::new();
    let mut masks = Vec::new();
    let mut cuts: Vec<(usize, u64, u64)> = Vec::new();
    for pane in &raw.panes {
        let program = evaluate_pane(session, pane, entries, &mut outcomes, &mut cuts, &mut masks)?;
        programs.insert(pane.pane.clone(), program);
    }

    // The time map: time-only ops as they are, every excision (done or
    // refused) as a cut of its time, everything else without effect.
    let mut timed: Vec<EditEntry> = entries
        .iter()
        .map(|e| EditEntry {
            enabled: e.enabled && e.op.class() == OpClass::TimeOnly,
            op: e.op.clone(),
        })
        .collect();
    for (index, from, to) in cuts {
        timed[index] = EditEntry::new(EditOp::Skip {
            from: Src::Us(from),
            to: Src::Us(to),
        });
    }
    let mut timeline = SourceTimeline::new(session.manifest.duration_us);
    for (pane, events) in &session.events {
        timeline.add_pane(pane, events);
    }
    let time_map = TimeMap::build(&timed, &timeline)?;

    let outcomes = outcomes
        .into_iter()
        .map(|o| {
            o.unwrap_or(Outcome::Inert {
                reason: "not evaluated".into(),
            })
        })
        .collect();
    Ok(Evaluation {
        programs,
        time_map,
        outcomes,
        masks,
    })
}

fn invalid(index: usize, op: &EditOp, reason: String) -> EvalError {
    EvalError::Invalid {
        index,
        op: op.name(),
        reason,
    }
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

fn evaluate_pane(
    session: &Session,
    pane: &PaneIndex,
    entries: &[EditEntry],
    outcomes: &mut [Option<Outcome>],
    cuts: &mut Vec<(usize, u64, u64)>,
    masks: &mut Vec<MaskRegion>,
) -> Result<PaneProgram, EvalError> {
    let stream = pane.stream();
    let raw_len = stream.len();
    let events = session.pane_events(&pane.pane);
    let mine = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.enabled && e.op.panes().iter().all(|p| **p == pane.pane));
    // Output events whose recorded bytes differ from the bytes they play:
    // no splice may touch them.
    let differing: Vec<(usize, Range<u64>)> = events
        .iter()
        .enumerate()
        .filter_map(
            |(index, event)| match (&event.kind, &stream.items[index].kind) {
                (tap_model::EventKind::Output(o), ItemKind::Output { len })
                    if o.playback.as_ref().is_some_and(|p| p.data != o.data) =>
                {
                    let at = stream.items[index].offset;
                    Some((index, at..at + len))
                }
                _ => None,
            },
        )
        .collect();
    let crossing = |range: &Range<u64>| {
        differing
            .iter()
            .find(|(_, bytes)| {
                overlaps(bytes, range) || (range.is_empty() && bytes.contains(&range.start))
            })
            .cloned()
    };

    // Excisions: static preconditions, then overlaps (the earliest start
    // wins).
    let mut candidates = Vec::new();
    for (index, entry) in mine.clone() {
        let EditOp::Excise { segment } = &entry.op else {
            continue;
        };
        if segment.start >= segment.end || segment.end > raw_len {
            return Err(invalid(
                index,
                &entry.op,
                format!(
                    "segment {}..{} is empty or past the end of the pane's output ({raw_len} bytes)",
                    segment.start, segment.end
                ),
            ));
        }
        let range = segment.start..segment.end;
        cuts.push((
            index,
            time_of(stream, range.start),
            time_of(stream, range.end),
        ));
        let mut refusals = excise::check(pane, &range);
        if let Some((event, bytes)) = crossing(&range) {
            refusals.push(Refusal::PlaybackDiffers { event, bytes });
        }
        candidates.push(ExciseCandidate {
            index,
            refusals,
            range,
        });
    }
    candidates.sort_by_key(|c| (c.range.start, c.index));
    for i in 1..candidates.len() {
        if let Some(other) = candidates[..i]
            .iter()
            .find(|c| c.refusals.is_empty() && overlaps(&c.range, &candidates[i].range))
        {
            let op = other.index;
            candidates[i].refusals.push(Refusal::Overlaps { op });
        }
    }

    // Fixups, in source order, each against the program so far.
    let mut excised: Vec<Excised> = Vec::new();
    let scrollback = Scrollback::Bytes(vt::GHOSTTY_SCROLLBACK_BYTES);
    let mut original = Replay::start(stream, scrollback)?;
    for candidate in &mut candidates {
        if !candidate.refusals.is_empty() {
            continue;
        }
        let (a, b) = (candidate.range.start, candidate.range.end);
        original.run_to_position(byte_position(stream, a))?;
        if !original.term.at_ground() {
            candidate.refusals.push(Refusal::NotAtGround {
                side: Side::Start,
                byte: a,
            });
        }
        original.run_to_position(byte_position(stream, b))?;
        if !original.term.at_ground() {
            candidate.refusals.push(Refusal::NotAtGround {
                side: Side::End,
                byte: b,
            });
        }
        if !candidate.refusals.is_empty() {
            continue;
        }
        let target = state::TermState::read(&original.term);
        match fix(session, pane, &excised, candidate, &target)? {
            Ok(fixups) => excised.push(Excised {
                index: candidate.index,
                range: candidate.range.clone(),
                fixups,
            }),
            Err(refusal) => candidate.refusals.push(refusal),
        }
    }
    for candidate in &candidates {
        if !candidate.refusals.is_empty() {
            outcomes[candidate.index] = Some(Outcome::Refused {
                reasons: candidate.refusals.clone(),
            });
        }
    }

    // Input events: those typed at or during an excised command go with
    // it; RedactInput withholds the rest.
    let mut inputs = BTreeMap::new();
    let mut removed_count: BTreeMap<usize, usize> = BTreeMap::new();
    for (event, item) in stream.items.iter().enumerate() {
        if !matches!(item.kind, ItemKind::Input { .. }) {
            continue;
        }
        if let Some(e) = excised.iter().find(|e| e.range.contains(&item.offset)) {
            inputs.insert(event, InputEdit::Removed { op: e.index });
            *removed_count.entry(e.index).or_default() += 1;
        }
    }
    for (index, entry) in mine.clone() {
        let EditOp::RedactInput { events: seqs, .. } = &entry.op else {
            continue;
        };
        if seqs.start > seqs.end {
            return Err(invalid(
                index,
                &entry.op,
                format!("event range {}..{} is reversed", seqs.start, seqs.end),
            ));
        }
        let mut redacted = Vec::new();
        for (event, raw_event) in events.iter().enumerate() {
            if raw_event.seq >= seqs.start
                && raw_event.seq < seqs.end
                && matches!(stream.items[event].kind, ItemKind::Input { .. })
                && !inputs.contains_key(&event)
            {
                inputs.insert(event, InputEdit::Redacted { op: index });
                redacted.push(event);
            }
        }
        outcomes[index] = Some(Outcome::Redacted { events: redacted });
    }

    // Replacements: plan each against the raw runs, then drop those inside
    // an excision or overlapping an earlier replacement.
    let runs = pane.runs()?;
    let fields = if mine
        .clone()
        .any(|(_, e)| matches!(e.op, EditOp::ReplaceText { .. }))
    {
        replace::osc_fields(&stream.bytes)
    } else {
        Vec::new()
    };
    let mut replaces: Vec<Planned> = Vec::new();
    for (index, entry) in mine.clone() {
        let EditOp::ReplaceText {
            bytes,
            text,
            expect,
            ..
        } = &entry.op
        else {
            continue;
        };
        if bytes.start >= bytes.end || bytes.end > raw_len {
            return Err(invalid(
                index,
                &entry.op,
                format!(
                    "bytes {}..{} are empty or past the end of the pane's output ({raw_len} bytes)",
                    bytes.start, bytes.end
                ),
            ));
        }
        let range = bytes.start..bytes.end;
        if let Some(e) = excised.iter().find(|e| overlaps(&e.range, &range)) {
            outcomes[index] = Some(Outcome::Superseded { by: e.index });
            continue;
        }
        if let Some((event, _)) = crossing(&range) {
            outcomes[index] = Some(Outcome::NeedsMask {
                reason: NeedsMask::PlaybackDiffers { event },
            });
            continue;
        }
        if let Some(planned) = replace::plan_in_field(&fields, &range, text) {
            match planned {
                Ok((field, splices)) => replaces.push((
                    index,
                    splices,
                    expect.clone(),
                    Some((field.field, field.osc.clone())),
                )),
                Err(reason) => outcomes[index] = Some(Outcome::NeedsMask { reason }),
            }
            continue;
        }
        match replace::plan(runs, &stream.bytes, &range, text) {
            Ok(splices) => replaces.push((index, splices, expect.clone(), None)),
            Err(reason) => outcomes[index] = Some(Outcome::NeedsMask { reason }),
        }
    }
    replaces.sort_by_key(|(index, splices, _, _)| (splices.first().map(|s| s.0.start), *index));
    let mut kept: Vec<Planned> = Vec::new();
    for replace in replaces {
        let span =
            replace.1.first().map_or(0, |s| s.0.start)..replace.1.last().map_or(0, |s| s.0.end);
        if let Some(other) = kept.iter().find(|k| {
            let other = k.1.first().map_or(0, |s| s.0.start)..k.1.last().map_or(0, |s| s.0.end);
            overlaps(&other, &span)
        }) {
            outcomes[replace.0] = Some(Outcome::Superseded { by: other.0 });
        } else {
            kept.push(replace);
        }
    }

    // Build the program; drop replacements whose refolded text does not
    // read `expect`, until every one does.
    let (program, edited) = loop {
        let mut splices: Vec<Splice> = excised
            .iter()
            .map(|e| Splice {
                source: e.range.clone(),
                bytes: e.fixups.bytes.clone(),
                attach: Attach::Next,
                op: e.index,
            })
            .collect();
        for (index, parts, _, _) in &kept {
            splices.extend(parts.iter().map(|(source, bytes)| Splice {
                source: source.clone(),
                bytes: bytes.clone(),
                attach: Attach::Last,
                op: *index,
            }));
        }
        let program = PaneProgram::new(raw_len, splices, inputs.clone())?;
        let edited = edited_stream(session, pane, program.clone());
        let failed = verify_replacements(&edited, &program, &kept)?;
        if failed.is_empty() {
            break (program, edited);
        }
        for (index, found) in failed {
            outcomes[index] = Some(Outcome::NeedsMask {
                reason: NeedsMask::ExpectMismatch { found },
            });
            kept.retain(|k| k.0 != index);
        }
    };

    for e in &excised {
        outcomes[e.index] = Some(Outcome::Excised {
            removed: e.range.clone(),
            fixups: e.fixups.clone(),
            inputs_removed: removed_count.get(&e.index).copied().unwrap_or(0),
        });
    }
    for (index, _, _, _) in &kept {
        let ranges: Vec<Range<u64>> = program
            .splices()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.op == *index)
            .map(|(k, _)| program.splice_range(k))
            .collect();
        outcomes[*index] = Some(Outcome::Replaced {
            program: ranges.first().map_or(0, |r| r.start)..ranges.last().map_or(0, |r| r.end),
            splices: ranges.len(),
        });
    }

    // Masks: anchors mapped into the program, then resolved over it.
    let mut wanted = Vec::new();
    for (index, entry) in mine {
        let EditOp::Mask {
            anchor,
            cells,
            style,
            ..
        } = &entry.op
        else {
            continue;
        };
        let at = anchor.byte;
        if let Some((event, _)) = crossing(&(at..at + 1)) {
            outcomes[index] = Some(Outcome::Inert {
                reason: format!(
                    "byte {at} is in event {event}, whose recorded bytes differ from the bytes it plays"
                ),
            });
            continue;
        }
        let printed = at < raw_len
            && runs.runs.iter().any(|r| {
                r.bytes.contains(&at)
                    && std::str::from_utf8(
                        &stream.bytes[r.bytes.start as usize..r.bytes.end as usize],
                    )
                    .is_ok_and(|s| s.is_char_boundary((at - r.bytes.start) as usize))
            });
        if !printed {
            outcomes[index] = Some(Outcome::Inert {
                reason: format!("byte {at} does not start a printed character"),
            });
            continue;
        }
        let anchor = match program.locate(at) {
            Location::Kept(p) => p,
            Location::Removed { splice, program: p } => {
                let s = &program.splices()[splice];
                if s.attach == Attach::Last && s.source.start == at && !s.bytes.is_empty() {
                    p.start
                } else {
                    outcomes[index] = Some(Outcome::Inert {
                        reason: format!("byte {at} was removed by edit #{}", s.op),
                    });
                    continue;
                }
            }
        };
        wanted.push(mask::Wanted {
            entry: index,
            anchor,
            cells: *cells,
            style: style.clone(),
        });
    }
    for (entry, result) in mask::resolve(&pane.pane, &edited, wanted)? {
        outcomes[entry] = Some(match result {
            Ok(region) => {
                masks.push(region);
                Outcome::Masked {
                    region: masks.len() - 1,
                }
            }
            Err(reason) => Outcome::Inert { reason },
        });
    }
    Ok(program)
}

fn edited_stream(session: &Session, pane: &PaneIndex, program: PaneProgram) -> PaneStream {
    let info = &pane.stream();
    PaneStream::with_program(
        pane.pane.clone(),
        info.initial_size,
        session.pane_events(&pane.pane),
        session.manifest.duration_us,
        program,
    )
}

/// The fixups for one excision: the state the edited program has where the
/// splice plays, made equal to `target`, and checked.
fn fix(
    session: &Session,
    pane: &PaneIndex,
    excised: &[Excised],
    candidate: &ExciseCandidate,
    target: &state::TermState,
) -> Result<Result<Fixups, Refusal>, EvalError> {
    let stream = pane.stream();
    let mut splices: Vec<Splice> = excised
        .iter()
        .map(|e| Splice {
            source: e.range.clone(),
            bytes: e.fixups.bytes.clone(),
            attach: Attach::Next,
            op: e.index,
        })
        .collect();
    splices.push(Splice {
        source: candidate.range.clone(),
        bytes: Vec::new(),
        attach: Attach::Next,
        op: candidate.index,
    });
    let program = PaneProgram::new(stream.len(), splices, BTreeMap::new())?;
    let k = program
        .splices()
        .iter()
        .position(|s| s.op == candidate.index)
        .unwrap_or(0);
    let p = program.splice_range(k).start;
    let edited = edited_stream(session, pane, program);
    // The splice's bytes belong to the event holding the first byte after
    // the range (or the last output event at the end).
    let item = stream
        .output_item_at(candidate.range.end)
        .or_else(|| last_output(stream))
        .unwrap_or(0);
    let position = Position {
        item,
        intra: p.saturating_sub(edited.items[item].offset),
    };
    let mut replay = Replay::start(&edited, Scrollback::Bytes(vt::GHOSTTY_SCROLLBACK_BYTES))?;
    replay.run_to_position(position)?;
    let from = state::TermState::read(&replay.term);
    let place = state::Place::read(&replay.term);
    let mut fixups = match state::fixups(&from, target) {
        Ok(f) => f,
        Err(state::Unfixable(state)) => return Ok(Err(Refusal::Unfixable { state })),
    };
    replay.term.write(fixups.bytes());
    if state::Place::read(&replay.term) != place {
        match state::cursor_back(target, place.cursor) {
            Ok(cup) => {
                replay.term.write(&cup);
                fixups.push_part("cursor", &cup);
            }
            Err(state::Unfixable(state)) => return Ok(Err(Refusal::Unfixable { state })),
        }
    }
    replay.term.take_signals();
    let mut states = state::TermState::read(&replay.term).differences(target);
    let now = state::Place::read(&replay.term);
    if now.cursor != place.cursor || now.pending_wrap != place.pending_wrap {
        states.push("cursor".into());
    }
    if now.alternate != place.alternate {
        states.push("active screen".into());
    }
    if !states.is_empty() {
        return Ok(Err(Refusal::FixupMismatch { states }));
    }
    Ok(Ok(fixups))
}

/// Refolds the edited program and reads what each replacement made: the
/// cells it drew, right after drawing them, or the value the terminal
/// reads back from the OSC field it rewrote. Returns the replacements that
/// do not read `expect`, with what they read.
fn verify_replacements(
    edited: &PaneStream,
    program: &PaneProgram,
    kept: &[Planned],
) -> Result<Vec<(usize, String)>, EvalError> {
    let ranges_of = |index: usize| -> Vec<Range<u64>> {
        program
            .splices()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.op == index)
            .map(|(k, _)| program.splice_range(k))
            .collect()
    };
    let mut stops: Vec<(u64, u64, usize, &str)> = kept
        .iter()
        .filter(|k| k.3.is_none())
        .filter_map(|(index, _, expect, _)| {
            let ranges = ranges_of(*index);
            Some((
                ranges.first()?.start,
                ranges.last()?.end,
                *index,
                expect.as_str(),
            ))
        })
        .collect();
    stops.sort();
    let mut failed = Vec::new();
    if !stops.is_empty() {
        let mut replay = Replay::start(edited, Scrollback::Unlimited)?;
        for (start, end, index, expect) in stops {
            replay.run_to_position(byte_position(edited, start))?;
            let first = next_cell(&replay.term);
            replay.run_to_position(end_position(edited, end))?;
            let last = last_cell(&replay.term);
            let found = replay.term.text_between(first, last);
            if found.trim_end() != expect.trim_end() {
                failed.push((index, found));
            }
        }
    }

    // OSC fields: the value the terminal keeps, read just after the
    // sequence (the command line through the mark the index reads).
    let mut fields: Vec<(u64, u64, usize, &str, replace::Field)> = kept
        .iter()
        .filter_map(|(index, _, expect, field)| {
            let (field, osc) = field.as_ref()?;
            let (Location::Kept(start), Location::Kept(end)) =
                (program.locate(osc.start), program.locate(osc.end))
            else {
                return None;
            };
            Some((start, end, *index, expect.as_str(), *field))
        })
        .collect();
    fields.sort_by_key(|f| (f.0, f.1, f.2));
    if fields.is_empty() {
        return Ok(failed);
    }
    let mut marks = None;
    let mut replay = Replay::start(edited, Scrollback::Unlimited)?;
    for (start, end, index, expect, field) in fields {
        let found = match field {
            replace::Field::CommandLine => {
                if marks.is_none() {
                    marks = Some(tap_index::scan::scan(edited)?);
                }
                marks
                    .iter()
                    .flatten()
                    .find_map(|m| match &m.kind {
                        tap_index::MarkKind::CommandStart { command } if m.byte == start => {
                            Some(command.clone().unwrap_or_default())
                        }
                        _ => None,
                    })
                    .unwrap_or_default()
            }
            replace::Field::Title | replace::Field::WorkingDirectory => {
                let at = if end >= edited.len() {
                    Position {
                        item: edited.items.len(),
                        intra: 0,
                    }
                } else {
                    byte_position(edited, end)
                };
                replay.run_to_position(at)?;
                let value = if field == replace::Field::Title {
                    replay.term.title_bytes()
                } else {
                    replay.term.pwd()
                };
                String::from_utf8_lossy(&value).into_owned()
            }
            // Not kept by the terminal: what was written is what there is.
            replace::Field::IconName => ranges_of(index)
                .iter()
                .map(|r| {
                    String::from_utf8_lossy(&edited.bytes[r.start as usize..r.end as usize])
                        .into_owned()
                })
                .collect(),
        };
        if found != expect {
            failed.push((index, found));
        }
    }
    Ok(failed)
}

/// What deleting a block in the transcript does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "plan")]
pub enum DeletePlan {
    /// Excise it: the command never happened.
    Excise {
        /// The op to add.
        op: EditOp,
        /// What plays in its place.
        fixups: Fixups,
    },
    /// Excising is not exact, so skip it: its time is cut and its bytes
    /// apply instantly (its output stays in scrollback). Always exact.
    Skip {
        /// The op to add.
        op: EditOp,
        /// Why it cannot be excised.
        reasons: Vec<Refusal>,
    },
}

/// Plans the transcript's Delete of the segment `segment` (bytes of the
/// pane's raw stream), after the session's own edits: an excision when
/// every precondition holds, else a skip with the reasons.
pub fn plan_delete(
    session: &Session,
    raw: &SessionIndex,
    segment: &tap_model::edl::SegId,
) -> Result<DeletePlan, EvalError> {
    let excise = EditOp::Excise {
        segment: segment.clone(),
    };
    let mut entries = session.edits.edits.clone();
    entries.push(EditEntry::new(excise.clone()));
    let evaluation = evaluate_entries(session, raw, &entries)?;
    let pane = raw.pane(&segment.pane)?;
    let time = |b: u64| time_of(pane.stream(), b);
    let skip = EditOp::Skip {
        from: Src::Us(time(segment.start)),
        to: Src::Us(time(segment.end)),
    };
    Ok(match evaluation.outcomes.last() {
        Some(Outcome::Excised { fixups, .. }) => DeletePlan::Excise {
            op: excise,
            fixups: fixups.clone(),
        },
        Some(Outcome::Refused { reasons }) => DeletePlan::Skip {
            op: skip,
            reasons: reasons.clone(),
        },
        _ => DeletePlan::Skip {
            op: skip,
            reasons: Vec::new(),
        },
    })
}
