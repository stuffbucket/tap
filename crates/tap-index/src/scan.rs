//! Mark extraction: where each OSC 133 mark, screen switch, clear and reset
//! is in a pane's stream.
//!
//! The stream is fed through a terminal one escape sequence at a time:
//! plain text up to the next ESC is written in one call, then the sequence
//! that ESC starts is written with `vt_write_until` until the parser is back
//! at ground (stopping early where an OSC starts, so its first byte is known
//! even when another sequence was cut short by it). After each sequence the
//! index looks at what changed, so everything it records is attributed to
//! the exact sequence that caused it:
//!
//! - **OSC 133 marks** come from libghostty-vt's semantic prompt callback.
//!   The mark's offset is the first byte (ESC) of the OSC that was being
//!   parsed when the callback ran.
//! - **Full reset** (RIS) comes from the reset callback, at the ESC of its
//!   sequence.
//! - **Alternate screen** enter and exit are read from the terminal's state
//!   (`active_screen`) at every ground stop, at the ESC of the sequence that
//!   changed it. Reading state rather than matching bytes covers every way
//!   in (modes 47, 1047, 1049, several modes in one DECSET, a reset).
//! - **Clear** (ED 2 and ED 3) leaves no state to read and has no callback,
//!   so the sequence's own bytes are classified. The terminal delimits the
//!   sequence, so text that merely looks like `ESC [ 2 J` inside an OSC
//!   payload, or a sequence split across reads, is never misread.
//!
//! Terminal replies are dropped (no PTY write-back is installed), and resize
//! events are applied in order.

use serde::Serialize;

use crate::stream::{ItemKind, PaneStream};
use crate::vt::{self, Signal, VtError};

const ESC: u8 = 0x1b;
/// Longest sequence prefix kept for classification; ED is at most a few bytes.
const KEEP: usize = 16;

/// A boundary found in the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// What it is.
    #[serde(flatten)]
    pub kind: MarkKind,
    /// Offset of the first byte of the sequence (its ESC).
    pub byte: u64,
    /// Offset just past the sequence, where the parser is back at ground.
    pub end: u64,
    /// Source time of the output event holding `byte`.
    pub at_us: u64,
    /// Index of that event in the pane.
    pub event: usize,
}

/// The kinds of [`Mark`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MarkKind {
    /// OSC 133;P or ;A for a primary prompt.
    Prompt,
    /// OSC 133;P/A for a continuation or secondary prompt.
    ContinuationPrompt,
    /// OSC 133;B.
    InputStart,
    /// OSC 133;C: the command was accepted.
    CommandStart {
        /// The command line from `cmdline_url`/`cmdline`, if sent.
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// OSC 133;D: the command finished.
    CommandEnd {
        /// Its exit status, if sent.
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// The alternate screen became active.
    AltScreenEnter,
    /// The primary screen became active again.
    AltScreenExit,
    /// ED 2 (`scrollback: false`) or ED 3 (`scrollback: true`).
    Clear {
        /// Whether scrollback was erased (ED 3) rather than the screen.
        scrollback: bool,
    },
    /// RIS (`ESC c`).
    FullReset,
    /// OSC 0/2 set the title.
    Title {
        /// The new title.
        title: String,
    },
}

/// Finds every mark in a pane's stream.
pub fn scan(stream: &PaneStream) -> Result<Vec<Mark>, VtError> {
    let (cols, rows) = stream.initial_size;
    let mut scanner = Scanner {
        term: vt::Terminal::new(cols, rows)?,
        marks: Vec::new(),
        alternate: false,
        seq_start: 0,
        seq_bytes: Vec::new(),
        osc_start: None,
        event: 0,
        at_us: 0,
    };
    for (index, item) in stream.items.iter().enumerate() {
        match &item.kind {
            ItemKind::Output { len } => {
                scanner.event = index;
                scanner.at_us = item.at_us;
                let start = item.offset as usize;
                let end = start + *len as usize;
                scanner.feed(&stream.bytes[start..end], item.offset)?;
            }
            ItemKind::Resize { cols, rows } => scanner.term.resize(*cols, *rows)?,
            ItemKind::Input { .. } | ItemKind::Other => {}
        }
    }
    Ok(scanner.marks)
}

struct Scanner {
    term: vt::Terminal,
    marks: Vec<Mark>,
    alternate: bool,
    /// Offset of the ESC that began the sequence being parsed.
    seq_start: u64,
    /// The first bytes of that sequence.
    seq_bytes: Vec<u8>,
    /// Offset of the first byte of the last OSC that started.
    osc_start: Option<u64>,
    event: usize,
    at_us: u64,
}

impl Scanner {
    fn feed(&mut self, data: &[u8], base: u64) -> Result<(), VtError> {
        let mut i = 0;
        while i < data.len() {
            if self.term.at_ground() {
                let Some(k) = data[i..].iter().position(|&b| b == ESC) else {
                    self.term.write(&data[i..]);
                    // A partial UTF-8 character may be left: it is finished
                    // in the next event, as a "sequence" that changes nothing.
                    self.begin_sequence(base + data.len() as u64);
                    break;
                };
                if k > 0 {
                    self.term.write(&data[i..i + k]);
                    i += k;
                }
                self.begin_sequence(base + i as u64);
                self.seq_bytes.push(ESC);
                self.term.write(&data[i..=i]);
                i += 1;
                continue;
            }
            match self.term.write_sequence(&data[i..])? {
                None => {
                    self.keep(&data[i..]);
                    i = data.len();
                }
                Some(step) => {
                    self.keep(&data[i..i + step.consumed]);
                    i += step.consumed;
                    if let Some(back) = step.osc_start_back {
                        self.osc_start = Some((base + i as u64).saturating_sub(back as u64));
                    }
                    if step.ground {
                        self.end_sequence(base + i as u64);
                    }
                }
            }
        }
        Ok(())
    }

    fn begin_sequence(&mut self, at: u64) {
        self.seq_start = at;
        self.seq_bytes.clear();
    }

    fn keep(&mut self, bytes: &[u8]) {
        let room = KEEP.saturating_sub(self.seq_bytes.len());
        self.seq_bytes
            .extend_from_slice(&bytes[..room.min(bytes.len())]);
    }

    /// The sequence that started at `seq_start` ended just before `end`.
    fn end_sequence(&mut self, end: u64) {
        for signal in self.term.take_signals() {
            let at = match signal {
                Signal::PromptStart { .. }
                | Signal::InputStart
                | Signal::OutputStart { .. }
                | Signal::CommandEnd { .. }
                | Signal::TitleChanged => self.osc_start.unwrap_or(self.seq_start),
                Signal::FullReset => self.seq_start,
            };
            let kind = match signal {
                Signal::PromptStart {
                    continuation: false,
                } => MarkKind::Prompt,
                Signal::PromptStart { continuation: true } => MarkKind::ContinuationPrompt,
                Signal::InputStart => MarkKind::InputStart,
                Signal::OutputStart { command } => MarkKind::CommandStart {
                    command: command.map(|c| String::from_utf8_lossy(&c).into_owned()),
                },
                Signal::CommandEnd { exit_code } => MarkKind::CommandEnd { exit_code },
                Signal::FullReset => MarkKind::FullReset,
                Signal::TitleChanged => MarkKind::Title {
                    title: self.term.title(),
                },
            };
            self.push(kind, at, end);
        }
        if let Some(scrollback) = classify_erase_display(&self.seq_bytes) {
            self.push(MarkKind::Clear { scrollback }, self.seq_start, end);
        }
        let alternate = self.term.on_alternate_screen();
        if alternate != self.alternate {
            self.alternate = alternate;
            let kind = if alternate {
                MarkKind::AltScreenEnter
            } else {
                MarkKind::AltScreenExit
            };
            self.push(kind, self.seq_start, end);
        }
        self.osc_start = None;
    }

    fn push(&mut self, kind: MarkKind, byte: u64, end: u64) {
        self.marks.push(Mark {
            kind,
            byte,
            end,
            at_us: self.at_us,
            event: self.event,
        });
    }
}

/// `Some(scrollback)` when a complete sequence is ED 2 (`ESC [ 2 J`) or
/// ED 3 (`ESC [ 3 J`).
fn classify_erase_display(seq: &[u8]) -> Option<bool> {
    match seq {
        [ESC, b'[', b'2', b'J'] => Some(false),
        [ESC, b'[', b'3', b'J'] => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tap_model::PaneId;
    use tap_model::event::{EventKind, Output, RawEvent, TextEncoding};

    fn stream(chunks: &[&[u8]]) -> PaneStream {
        let events: Vec<RawEvent> = chunks
            .iter()
            .enumerate()
            .map(|(i, data)| RawEvent {
                at_us: i as u64 * 1000,
                seq: i as u64,
                kind: EventKind::Output(Output {
                    data: data.to_vec(),
                    data_encoding: TextEncoding::Unspecified,
                    playback: None,
                }),
            })
            .collect();
        PaneStream::new(PaneId::new("p").unwrap(), (20, 5), &events, 10_000)
    }

    fn kinds(marks: &[Mark]) -> Vec<(&MarkKind, u64, u64)> {
        marks.iter().map(|m| (&m.kind, m.byte, m.end)).collect()
    }

    #[test]
    fn marks_point_at_the_esc_even_when_split_across_events() {
        let s = stream(&[
            b"ab\x1b]13",
            b"3;P\x07cd\x1b]133;C\x1b\\x\x1b",
            b"]133;D;7\x07",
        ]);
        let marks = scan(&s).unwrap();
        assert_eq!(
            kinds(&marks),
            vec![
                (&MarkKind::Prompt, 2, 10),
                (&MarkKind::CommandStart { command: None }, 12, 21),
                (&MarkKind::CommandEnd { exit_code: Some(7) }, 22, 32),
            ]
        );
    }

    #[test]
    fn finds_screen_switches_clears_and_resets() {
        let s = stream(&[b"x\x1b[?25;1049hy\x1b[2Jz\x1b[?1049l\x1b[3J\x1b[0J\x1bc"]);
        let marks = scan(&s).unwrap();
        assert_eq!(
            kinds(&marks),
            vec![
                (&MarkKind::AltScreenEnter, 1, 12),
                (&MarkKind::Clear { scrollback: false }, 13, 17),
                (&MarkKind::AltScreenExit, 18, 26),
                (&MarkKind::Clear { scrollback: true }, 26, 30),
                (&MarkKind::FullReset, 34, 36),
            ]
        );
    }

    #[test]
    fn an_osc_that_cuts_a_csi_short_is_found_at_its_own_esc() {
        let s = stream(&[b"\x1b[1\x1b]133;P\x07"]);
        let marks = scan(&s).unwrap();
        assert_eq!(kinds(&marks), vec![(&MarkKind::Prompt, 3, 11)]);
    }

    #[test]
    fn a_clear_split_across_events_is_found() {
        let s = stream(&[b"ab\x1b[", b"2", b"Jc"]);
        let marks = scan(&s).unwrap();
        assert_eq!(
            kinds(&marks),
            vec![(&MarkKind::Clear { scrollback: false }, 2, 6)]
        );
    }
}
