//! The transcript: one block per command, as text.

use std::fmt::Write as _;

use serde::Serialize;

use crate::line_editor::{self, Keys, REDACTED};
use crate::scan::{Mark, MarkKind};
use crate::segment::{BoundarySource, Plan, Segment, SegmentKind};
use crate::stream::{ItemKind, PaneStream};

/// A session's transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transcript {
    /// One per pane, in manifest order.
    pub panes: Vec<PaneTranscript>,
}

/// A pane's transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneTranscript {
    /// The pane id.
    pub pane: String,
    /// One block per top-level segment, in order.
    pub blocks: Vec<Block>,
}

/// A transcript block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    /// The segment's id.
    pub segment_id: String,
    /// `command`, `prompt` or `output`.
    pub kind: SegmentKind,
    /// Where the segment's boundary came from.
    pub boundary_source: BoundarySource,
    /// Screen text of the prompt rows (commands and prompts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The command line (commands and prompts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandText>,
    /// What the command printed, as on screen when it ended: soft wraps
    /// joined, trailing blanks trimmed. Empty for full-screen programs and
    /// for output cleared before the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Exit status, when the shell reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Source time the segment starts.
    pub start_us: u64,
    /// Source time the segment ends.
    pub end_us: u64,
    /// Where the time went.
    pub durations: Durations,
    /// Full-screen programs the command ran.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tui: Vec<TuiSpan>,
}

/// A command line and how sure the transcript is of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandText {
    /// The text.
    pub text: String,
    /// Where it came from.
    pub source: CommandSource,
    /// Reconstructed through keys the line editor cannot follow (history
    /// recall, cursor movement, completion): the command that ran may differ.
    pub approximate: bool,
    /// Some keystrokes were recorded without their bytes; they show as `•••`.
    pub redacted: bool,
}

/// Where a command line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandSource {
    /// The shell's OSC 133;C `cmdline_url`.
    Osc133,
    /// Keys typed at the prompt, through a simple line editor.
    InputLane,
}

/// How a block's time divides up. Together these cover the block up to the
/// command's end; the moment between the end and the next prompt is not
/// counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Durations {
    /// From the prompt to the first key.
    pub idle_us: u64,
    /// From the first key to the command starting.
    pub typing_us: u64,
    /// From the command starting to it ending.
    pub running_us: u64,
}

/// A full-screen program inside a block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiSpan {
    /// The TUI segment's id.
    pub segment_id: String,
    /// The program, from the command line or the title it set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Source time it starts.
    pub start_us: u64,
    /// Source time it ends.
    pub end_us: u64,
    /// `end_us - start_us`.
    pub duration_us: u64,
}

/// Screen text captured while replaying, per top-level segment.
#[derive(Debug, Clone, Default)]
pub(crate) struct Captured {
    pub prompt: Option<String>,
    pub output: Option<String>,
}

pub(crate) fn block(
    stream: &PaneStream,
    marks: &[Mark],
    segment: &Segment,
    plan: &Plan,
    captured: Captured,
) -> Block {
    let command = command_text(stream, plan);
    let tui = segment
        .children
        .iter()
        .filter(|c| c.kind == SegmentKind::Tui)
        .map(|span| TuiSpan {
            segment_id: span.id.clone(),
            program: program_name(command.as_ref(), marks, span),
            start_us: span.start_us,
            end_us: span.end_us,
            duration_us: span.end_us.saturating_sub(span.start_us),
        })
        .collect();
    let prompt_us = segment.start_us;
    let first_key_us = plan.first_input.map(|(_, at)| at);
    let output_us = plan.output_start.map(|(_, at)| at);
    let end_us = plan
        .command_end
        .map_or(segment.end_us, |(_, at)| at)
        .max(output_us.unwrap_or(0));
    let durations = match segment.kind {
        SegmentKind::Output => Durations {
            running_us: segment.end_us.saturating_sub(segment.start_us),
            ..Durations::default()
        },
        _ => Durations {
            idle_us: first_key_us
                .or(output_us)
                .unwrap_or(segment.end_us)
                .saturating_sub(prompt_us),
            typing_us: match (first_key_us, output_us) {
                (Some(key), Some(out)) => out.saturating_sub(key),
                (Some(key), None) => segment.end_us.saturating_sub(key),
                _ => 0,
            },
            running_us: output_us.map_or(0, |out| end_us.saturating_sub(out)),
        },
    };
    Block {
        segment_id: segment.id.clone(),
        kind: segment.kind,
        boundary_source: segment.boundary_source,
        prompt: captured.prompt,
        command,
        output: captured.output,
        exit_code: plan.exit_code,
        start_us: segment.start_us,
        end_us: segment.end_us,
        durations,
        tui,
    }
}

fn command_text(stream: &PaneStream, plan: &Plan) -> Option<CommandText> {
    if let Some(line) = &plan.command_line {
        return Some(CommandText {
            text: line.clone(),
            source: CommandSource::Osc133,
            approximate: false,
            redacted: false,
        });
    }
    if plan.input_items.is_empty() {
        return None;
    }
    let keys = plan
        .input_items
        .iter()
        .filter_map(|&i| match &stream.items[i].kind {
            ItemKind::Input { data, redacted, .. } => Some(Keys {
                data,
                redacted: *redacted,
            }),
            _ => None,
        });
    let typed = line_editor::reconstruct(keys);
    Some(CommandText {
        text: typed.text,
        source: CommandSource::InputLane,
        approximate: typed.approximate,
        redacted: typed.redacted,
    })
}

/// The program a TUI span ran: the first word of the command line (after
/// any `NAME=value` assignments and a leading `sudo`/`exec`/`command`), else
/// the last title it set.
fn program_name(command: Option<&CommandText>, marks: &[Mark], span: &Segment) -> Option<String> {
    if let Some(command) = command
        && !command.redacted
        && let Some(word) = command
            .text
            .split_whitespace()
            .find(|w| !w.contains('=') && !matches!(*w, "sudo" | "exec" | "command" | "env"))
    {
        let name = word.rsplit('/').next().unwrap_or(word);
        if !name.is_empty() && name != REDACTED {
            return Some(name.to_string());
        }
    }
    marks
        .iter()
        .rev()
        .filter(|m| m.byte >= span.start_byte && m.byte < span.end_byte)
        .filter_map(|m| match &m.kind {
            MarkKind::Title { title } if !title.is_empty() => Some(title.clone()),
            _ => None,
        })
        .next()
}

fn seconds(us: u64) -> String {
    format!("{}.{:01}s", us / 1_000_000, (us % 1_000_000) / 100_000)
}

fn clock(us: u64) -> String {
    let ms = us / 1000;
    format!(
        "{:02}:{:02}.{:03}",
        ms / 60_000,
        (ms / 1000) % 60,
        ms % 1000
    )
}

impl Transcript {
    /// The transcript as text for a terminal: one block per command, the
    /// output indented under it.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (index, pane) in self.panes.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            let _ = writeln!(out, "# pane {}", pane.pane);
            for block in &pane.blocks {
                // Output outside any command that left nothing on screen
                // (often only a shell's partial-line marker) is not worth a
                // block in the text form; the JSON form keeps it.
                if block.kind == SegmentKind::Output
                    && block.output.as_deref().is_none_or(str::is_empty)
                    && block.tui.is_empty()
                {
                    continue;
                }
                out.push('\n');
                block.write_text(&mut out);
            }
        }
        out
    }
}

impl Block {
    fn write_text(&self, out: &mut String) {
        let source = match self.boundary_source {
            BoundarySource::Start => "start",
            BoundarySource::Osc133 => "osc133",
            BoundarySource::EnterKey => "enter-key",
            BoundarySource::AltScreen => "alt-screen",
            BoundarySource::Clear => "clear",
            BoundarySource::Idle => "idle",
        };
        let _ = write!(out, "[{} {}] ", clock(self.start_us), source);
        match self.kind {
            SegmentKind::Output => out.push_str("(output)"),
            SegmentKind::Prompt => {
                let _ = write!(out, "{} (no command)", self.prompt.as_deref().unwrap_or(""));
            }
            _ => {
                let prompt = self.prompt.as_deref().unwrap_or("");
                let _ = write!(out, "{prompt}");
                if let Some(command) = &self.command {
                    if !prompt.is_empty() {
                        out.push(' ');
                    }
                    if command.text.is_empty() && command.approximate {
                        out.push_str("(not known: recalled from history or edited)");
                    } else {
                        out.push_str(&command.text);
                        if command.approximate {
                            out.push_str("  (approximate)");
                        }
                    }
                }
            }
        }
        out.push('\n');
        if let Some(output) = &self.output {
            for line in output.lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
        for tui in &self.tui {
            let _ = writeln!(
                out,
                "    [full screen: {} for {}]",
                tui.program.as_deref().unwrap_or("program"),
                seconds(tui.duration_us)
            );
        }
        let mut facts = Vec::new();
        if let Some(code) = self.exit_code {
            facts.push(format!("exit {code}"));
        }
        let d = self.durations;
        if self.kind == SegmentKind::Output {
            facts.push(format!("{} of output", seconds(d.running_us)));
        } else {
            facts.push(format!("idle {}", seconds(d.idle_us)));
            facts.push(format!("typing {}", seconds(d.typing_us)));
            if self.kind == SegmentKind::Command {
                facts.push(format!("running {}", seconds(d.running_us)));
            }
        }
        let _ = writeln!(out, "    -- {}", facts.join(", "));
    }
}
