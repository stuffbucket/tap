//! Segmentation: a pane's stream as command blocks, with TUI spans, output
//! paragraphs and idle gaps inside them.
//!
//! Top-level segments tile the stream: each byte belongs to exactly one. A
//! command segment runs from its prompt to the next prompt, so excising one
//! removes a whole prompt-to-prompt span (`docs/editor-design.md`).
//!
//! Boundaries come, in order of preference, from:
//!
//! 1. **OSC 133 marks** (`P`, `C`, `D`) written by Tap's shell integration
//!    or the shell itself. Used whenever the pane has any prompt mark.
//! 2. **Enter in the input lane** otherwise: a carriage return or line feed
//!    typed (not a terminal reply, not inside a bracketed paste, not while
//!    the alternate screen is active) and confirmed by the terminal moving to
//!    a new line within a second. The prompt of each command is taken to
//!    start at the last output event before its first keystroke, which is
//!    where a shell redraws its prompt.
//!
//! Inside a command's running span (and inside output that belongs to no
//! command), alternate-screen spans become TUI segments, and the rest is
//! split into output paragraphs at clears, full resets and idle gaps of at
//! least two seconds. Idle gaps anywhere in a segment are segments of their
//! own, so the editor can shorten them.

use serde::Serialize;

use crate::id::segment_id;
use crate::scan::{Mark, MarkKind};
use crate::stream::{ItemKind, PaneStream};
use tap_model::event::InputOrigin;

/// Gaps at least this long are idle gaps and split output into paragraphs.
pub const IDLE_GAP_US: u64 = 2_000_000;
/// How long after Enter the terminal must move to a new line to confirm it.
const ENTER_CONFIRM_US: u64 = 1_000_000;

/// What a segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SegmentKind {
    /// A prompt, the command typed at it, and what the command printed.
    Command,
    /// A prompt that was left without running a command (the recording
    /// ended, or the line was abandoned).
    Prompt,
    /// Output that belongs to no command: what a shell printed before its
    /// first prompt, or a whole pane that could not be split into commands.
    Output,
    /// An alternate-screen program (vim, less, htop).
    Tui,
    /// Part of the output, between clears and idle gaps.
    Paragraph,
    /// At least two seconds in which nothing was recorded.
    Idle,
}

/// Where a segment's starting boundary came from, so the editor can show how
/// sure it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoundarySource {
    /// The start of the pane: there is no boundary before it.
    Start,
    /// An OSC 133 semantic prompt mark.
    Osc133,
    /// An Enter keystroke in the input lane, confirmed against the output.
    EnterKey,
    /// The alternate screen was entered or left.
    AltScreen,
    /// The screen was cleared (ED 2 or 3) or fully reset (RIS).
    Clear,
    /// An idle gap of at least two seconds.
    Idle,
}

/// A span of a pane's recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Stable id: a hash of the pane, the kind and the start offset (and,
    /// for idle gaps, the start time). It survives re-segmentation as long
    /// as the boundary does.
    pub id: String,
    /// What it is.
    pub kind: SegmentKind,
    /// Where its starting boundary came from.
    pub boundary_source: BoundarySource,
    /// First stream byte.
    pub start_byte: u64,
    /// One past the last stream byte (equal to `start_byte` for idle gaps).
    pub end_byte: u64,
    /// Source time it starts.
    pub start_us: u64,
    /// Source time it ends.
    pub end_us: u64,
    /// TUI spans, paragraphs and idle gaps inside it, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Segment>,
}

/// What the transcript needs to know about a top-level segment, beyond its
/// extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Where the prompt starts drawing (the prompt row is pinned here).
    pub prompt_byte: Option<u64>,
    /// The first keystroke at the prompt: item index and time.
    pub first_input: Option<(usize, u64)>,
    /// Where the command's output starts (after OSC 133;C, or after the
    /// line feed that confirmed Enter), and when.
    pub output_start: Option<(u64, u64)>,
    /// Where the command ended (OSC 133;D, or the next prompt), and when.
    pub command_end: Option<(u64, u64)>,
    /// Exit status from OSC 133;D.
    pub exit_code: Option<i32>,
    /// Command line from OSC 133;C.
    pub command_line: Option<String>,
    /// Items whose keys make up the command line, in order.
    pub input_items: Vec<usize>,
}

/// A pane, segmented.
#[derive(Debug, Clone)]
pub(crate) struct Segmentation {
    /// Top-level segments, tiling the stream.
    pub segments: Vec<Segment>,
    /// One plan per top-level segment.
    pub plans: Vec<Plan>,
    /// Whether boundaries came from OSC 133 (so cells carry semantic tags).
    pub semantic: bool,
}

/// Input that a person (or a script standing in for one) typed, as opposed
/// to the recording terminal's replies to queries.
fn typed(origin: Option<InputOrigin>) -> bool {
    origin != Some(InputOrigin::TerminalReply)
}

/// Segments a pane.
pub(crate) fn segment(stream: &PaneStream, marks: &[Mark]) -> Segmentation {
    let has_prompts = marks.iter().any(|m| m.kind == MarkKind::Prompt);
    let (bounds, source, plans) = if has_prompts {
        osc133_commands(stream, marks)
    } else {
        enter_key_commands(stream, marks)
    };
    let mut segments = Vec::new();
    let mut all_plans = Vec::new();
    let total = stream.len();
    let first = bounds.first().map_or(total, |b| b.0);
    if first > 0 || bounds.is_empty() {
        let end_us = if bounds.is_empty() {
            stream.end_us
        } else {
            stream.time_of_byte(first)
        };
        all_plans.push(Plan {
            prompt_byte: None,
            first_input: None,
            output_start: Some((0, 0)),
            command_end: Some((first, end_us)),
            exit_code: None,
            command_line: None,
            input_items: Vec::new(),
        });
        segments.push(build_segment(
            stream,
            marks,
            SegmentKind::Output,
            BoundarySource::Start,
            (0, first),
            (0, end_us),
            Some((0, first)),
        ));
    }
    for (index, ((start, kind), plan)) in bounds.iter().zip(plans).enumerate() {
        let end = bounds.get(index + 1).map_or(total, |b| b.0);
        let start_us = stream.time_of_byte(*start);
        let end_us = if end == total {
            stream.end_us
        } else {
            stream.time_of_byte(end)
        };
        let running = plan
            .output_start
            .map(|(b, _)| (b, plan.command_end.map_or(end, |(e, _)| e)));
        segments.push(build_segment(
            stream,
            marks,
            *kind,
            source,
            (*start, end),
            (start_us, end_us),
            running,
        ));
        all_plans.push(plan);
    }
    Segmentation {
        segments,
        plans: all_plans,
        semantic: has_prompts,
    }
}

type Bounds = Vec<(u64, SegmentKind)>;

/// Commands from OSC 133 marks: one segment per primary prompt.
fn osc133_commands(stream: &PaneStream, marks: &[Mark]) -> (Bounds, BoundarySource, Vec<Plan>) {
    // A prompt drawn again with nothing in between (no command, no keys) is
    // a redraw of the same prompt, not a new one.
    let mut prompts: Vec<&Mark> = Vec::new();
    for mark in marks.iter().filter(|m| m.kind == MarkKind::Prompt) {
        if let Some(previous) = prompts.last() {
            let commands_between = marks.iter().any(|m| {
                m.byte > previous.byte
                    && m.byte < mark.byte
                    && matches!(
                        m.kind,
                        MarkKind::CommandStart { .. } | MarkKind::CommandEnd { .. }
                    )
            });
            let keys_between = (previous.event + 1..mark.event).any(|i| {
                matches!(&stream.items[i].kind, ItemKind::Input { origin, .. } if typed(*origin))
            });
            if !commands_between && !keys_between {
                continue;
            }
        }
        prompts.push(mark);
    }
    let mut bounds = Vec::new();
    let mut plans = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        let end = prompts.get(index + 1).map_or(u64::MAX, |p| p.byte);
        let next_event = prompts.get(index + 1).map_or(usize::MAX, |p| p.event);
        let in_range = |m: &&Mark| m.byte > prompt.byte && m.byte < end;
        let start = marks
            .iter()
            .filter(in_range)
            .find(|m| matches!(m.kind, MarkKind::CommandStart { .. }));
        let finish = start.and_then(|c| {
            marks
                .iter()
                .filter(in_range)
                .find(|m| m.byte > c.byte && matches!(m.kind, MarkKind::CommandEnd { .. }))
        });
        let keys_until = start
            .map_or(next_event, |c| c.event)
            .min(stream.items.len());
        let input_items: Vec<usize> = (prompt.event + 1..keys_until)
            .filter(|&i| {
                matches!(&stream.items[i].kind, ItemKind::Input { origin, .. } if typed(*origin))
            })
            .collect();
        bounds.push((
            prompt.byte,
            if start.is_some() {
                SegmentKind::Command
            } else {
                SegmentKind::Prompt
            },
        ));
        plans.push(Plan {
            prompt_byte: Some(prompt.end),
            first_input: input_items.first().map(|&i| (i, stream.items[i].at_us)),
            output_start: start.map(|c| (c.end, c.at_us)),
            command_end: finish.map(|d| (d.byte, d.at_us)),
            exit_code: finish.and_then(|d| match d.kind {
                MarkKind::CommandEnd { exit_code } => exit_code,
                _ => None,
            }),
            command_line: start.and_then(|c| match &c.kind {
                MarkKind::CommandStart { command } => command.clone(),
                _ => None,
            }),
            input_items,
        });
    }
    (bounds, BoundarySource::Osc133, plans)
}

/// Whether the alternate screen is active at stream offset `byte`, from the
/// screen-switch marks.
fn alternate_at(marks: &[Mark], byte: u64) -> bool {
    marks
        .iter()
        .rev()
        .find(|m| {
            m.end <= byte && matches!(m.kind, MarkKind::AltScreenEnter | MarkKind::AltScreenExit)
        })
        .is_some_and(|m| m.kind == MarkKind::AltScreenEnter)
}

/// Commands from Enter keystrokes, for panes without OSC 133 marks.
fn enter_key_commands(stream: &PaneStream, marks: &[Mark]) -> (Bounds, BoundarySource, Vec<Plan>) {
    struct Run {
        first: usize,
        keys: Vec<usize>,
        enter: Option<(usize, u64)>, // item, offset where output starts
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut current: Option<Run> = None;
    let mut paste = false;
    for (index, item) in stream.items.iter().enumerate() {
        let ItemKind::Input {
            data,
            origin,
            redacted,
        } = &item.kind
        else {
            continue;
        };
        if !typed(*origin) || alternate_at(marks, item.offset) {
            continue;
        }
        let run = current.get_or_insert_with(|| Run {
            first: index,
            keys: Vec::new(),
            enter: None,
        });
        run.keys.push(index);
        if *redacted {
            continue;
        }
        let mut enter = false;
        let mut rest = data.as_slice();
        while !rest.is_empty() {
            if rest.starts_with(b"\x1b[200~") {
                paste = true;
            } else if rest.starts_with(b"\x1b[201~") {
                paste = false;
            } else if !paste && (rest[0] == b'\r' || rest[0] == b'\n') {
                enter = true;
                break;
            }
            rest = &rest[1..];
        }
        if !enter {
            continue;
        }
        if let Some(line_start) = confirm_enter(stream, index) {
            run.enter = Some((index, line_start));
            runs.extend(current.take());
        }
    }
    runs.extend(current);

    let mut bounds = Vec::new();
    let mut plans: Vec<Plan> = Vec::new();
    let mut floor = 0u64;
    for run in &runs {
        // The prompt starts with the last output written before the first
        // key, as long as that is after the previous command's output began.
        let prompt = stream.items[..run.first]
            .iter()
            .rev()
            .find(|item| matches!(item.kind, ItemKind::Output { len } if len > 0))
            .map_or(floor, |item| item.offset)
            .max(floor);
        if let Some(&(last, _)) = bounds.last()
            && prompt <= last
        {
            // Two runs with no prompt between them (keys typed while a
            // command ran): keep them in one segment.
            if let Some(plan) = plans.last_mut() {
                plan.input_items.extend(&run.keys);
            }
            continue;
        }
        bounds.push((
            prompt,
            if run.enter.is_some() {
                SegmentKind::Command
            } else {
                SegmentKind::Prompt
            },
        ));
        let output_start = run
            .enter
            .map(|(item, line_start)| (line_start, stream.items[item].at_us));
        if let Some((line_start, _)) = output_start {
            floor = line_start;
        }
        plans.push(Plan {
            prompt_byte: Some(prompt),
            first_input: Some((run.first, stream.items[run.first].at_us)),
            output_start,
            command_end: None,
            exit_code: None,
            command_line: None,
            input_items: run.keys.clone(),
        });
    }
    // A command ends where the next prompt starts.
    for index in 1..bounds.len() {
        let next = bounds[index].0;
        if plans[index - 1].output_start.is_some() {
            plans[index - 1].command_end = Some((next, stream.time_of_byte(next)));
        }
    }
    (bounds, BoundarySource::EnterKey, plans)
}

/// The offset just past the first line feed the terminal wrote within a
/// second of the Enter in item `index`, if it wrote one.
fn confirm_enter(stream: &PaneStream, index: usize) -> Option<u64> {
    let at = stream.items[index].at_us;
    for item in &stream.items[index + 1..] {
        if item.at_us > at + ENTER_CONFIRM_US {
            break;
        }
        if let ItemKind::Output { len } = item.kind {
            let bytes = &stream.bytes[item.offset as usize..(item.offset + len) as usize];
            if let Some(lf) = bytes.iter().position(|&b| b == b'\n') {
                return Some(item.offset + lf as u64 + 1);
            }
        }
    }
    None
}

fn build_segment(
    stream: &PaneStream,
    marks: &[Mark],
    kind: SegmentKind,
    source: BoundarySource,
    (start, end): (u64, u64),
    (start_us, end_us): (u64, u64),
    running: Option<(u64, u64)>,
) -> Segment {
    let pane = &stream.pane;
    let mut children = Vec::new();

    // Idle gaps: consecutive events of this segment (any lane) at least
    // IDLE_GAP_US apart. The segment's own start and end bound the first and
    // last gap.
    let mut times: Vec<(u64, u64)> = vec![(start_us, start)];
    for item in &stream.items {
        let inside = item.offset >= start && (item.offset < end || end == stream.len());
        if inside && item.at_us >= start_us && item.at_us <= end_us {
            times.push((item.at_us, item.offset));
        }
    }
    times.push((end_us, end));
    let mut idle_splits = Vec::new();
    for pair in times.windows(2) {
        let ((a, _), (b, byte)) = (pair[0], pair[1]);
        if b.saturating_sub(a) >= IDLE_GAP_US {
            children.push(Segment {
                id: segment_id(pane, "idle", byte, Some(a)),
                kind: SegmentKind::Idle,
                boundary_source: BoundarySource::Idle,
                start_byte: byte,
                end_byte: byte,
                start_us: a,
                end_us: b,
                children: Vec::new(),
            });
            idle_splits.push(byte);
        }
    }

    if let Some((run_start, run_end)) = running {
        // TUI spans.
        let mut tuis: Vec<(u64, u64, u64, u64)> = Vec::new(); // bytes and times
        let mut open =
            alternate_at(marks, run_start).then_some((run_start, stream.time_of_byte(run_start)));
        for mark in marks
            .iter()
            .filter(|m| m.byte >= run_start && m.byte < run_end)
        {
            match mark.kind {
                MarkKind::AltScreenEnter if open.is_none() => open = Some((mark.byte, mark.at_us)),
                MarkKind::AltScreenExit => {
                    if let Some((b, t)) = open.take() {
                        tuis.push((b, mark.end.min(run_end), t, mark.at_us));
                    }
                }
                _ => {}
            }
        }
        if let Some((b, t)) = open {
            tuis.push((
                b,
                run_end,
                t,
                stream.time_of_byte(run_end.saturating_sub(1)),
            ));
        }
        let in_tui = |byte: u64| tuis.iter().any(|&(a, b, _, _)| byte >= a && byte < b);

        // Split points, each with the source of the boundary it makes.
        let mut splits: Vec<(u64, BoundarySource)> = Vec::new();
        let mut last_clear_event = None;
        for mark in marks
            .iter()
            .filter(|m| m.byte >= run_start && m.byte < run_end)
        {
            if matches!(mark.kind, MarkKind::Clear { .. } | MarkKind::FullReset)
                && !in_tui(mark.byte)
            {
                // A clear right after another in the same write (`ESC[3J
                // ESC[H ESC[2J`) is one visual boundary.
                if last_clear_event != Some(mark.event) && mark.byte > run_start {
                    splits.push((mark.byte, BoundarySource::Clear));
                }
                last_clear_event = Some(mark.event);
            }
        }
        for &byte in &idle_splits {
            if byte > run_start && byte < run_end && !in_tui(byte) {
                splits.push((byte, BoundarySource::Idle));
            }
        }
        for &(a, b, ta, tb) in &tuis {
            children.push(Segment {
                id: segment_id(pane, "tui", a, None),
                kind: SegmentKind::Tui,
                boundary_source: BoundarySource::AltScreen,
                start_byte: a,
                end_byte: b,
                start_us: ta,
                end_us: tb,
                children: Vec::new(),
            });
            splits.push((a, BoundarySource::AltScreen));
            splits.push((b, BoundarySource::AltScreen));
        }
        splits.push((run_start, source));
        splits.push((run_end, source));
        splits.sort_by_key(|&(byte, _)| byte);
        splits.dedup_by_key(|&mut (byte, _)| byte);

        for pair in splits.windows(2) {
            let ((a, why), (b, _)) = (pair[0], pair[1]);
            if b <= a || in_tui(a) {
                continue;
            }
            children.push(Segment {
                id: segment_id(pane, "paragraph", a, None),
                kind: SegmentKind::Paragraph,
                boundary_source: why,
                start_byte: a,
                end_byte: b,
                start_us: stream.time_of_byte(a),
                end_us: stream.time_of_byte(b - 1),
                children: Vec::new(),
            });
        }
    }
    children.sort_by_key(|s| (s.start_byte, s.start_us, s.kind != SegmentKind::Idle));

    Segment {
        id: segment_id(pane, kind_name(kind), start, None),
        kind,
        boundary_source: source,
        start_byte: start,
        end_byte: end,
        start_us,
        end_us,
        children,
    }
}

fn kind_name(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Command => "command",
        SegmentKind::Prompt => "prompt",
        SegmentKind::Output => "output",
        SegmentKind::Tui => "tui",
        SegmentKind::Paragraph => "paragraph",
        SegmentKind::Idle => "idle",
    }
}
