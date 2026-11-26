//! "Redact this": hide a secret everywhere it can be recovered from the
//! edited program.
//!
//! [`plan`] turns a string into edits, Descript-style:
//!
//! - wherever it was drawn as plain text, a `ReplaceText` with bullets of
//!   the same width (one per cell), checked against what the refolded
//!   screen reads; where a program drew it in pieces (a full-screen program
//!   addressing the cursor), one per piece;
//! - in OSC 133;C marks whose command line (as libghostty-vt decodes it)
//!   holds it, a `ReplaceText` turning each `cmdline=`/`cmdline_url=`
//!   option into `cmdline_url=` with the redacted command line; in OSC
//!   0/1/2 titles and OSC 7 working directories (percent-decoded), one
//!   rewriting the value;
//! - wherever a rewrite of drawn text is refused, a `Mask` anchored to the
//!   byte that drew its first cell;
//! - a `RedactInput` for the keystrokes (or paste) that typed it.
//!
//! It then evaluates the plan and reports what is left: every place the
//! secret remains in the edited program's output in any spelling (masked
//! text is still in the bytes, as is text inside sequences the plan does
//! not rewrite) and in its input, and occurrences seen on the screen that
//! could not be traced to a byte. The raw events always keep it.

use std::ops::Range;

use serde::Serialize;
use tap_index::stream::ItemKind;
use tap_index::vt::codepoint_width;
use tap_index::{InputEdit, SessionIndex};
use tap_model::edl::{ByteRange, EditEntry, EditOp, MaskStyle, SeqRange, TrackedRef};
use tap_model::event::InputOrigin;
use tap_model::{PaneId, Session};

use crate::replace::Field;
use crate::search::find_text;
use crate::tokens::{Token, tokens};
use crate::{EvalError, Evaluation, Outcome, evaluate_entries};

/// Where the secret's bytes are still present after the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Remaining {
    /// The pane.
    pub pane: PaneId,
    /// `output` (program bytes) or `input` (an input event's bytes).
    pub lane: &'static str,
    /// Program bytes, or the input event's index.
    pub at: Range<u64>,
    /// What they are: `masked`, `inside OSC …`, `inside APC`, `drawn as
    /// text` or `typed`.
    pub context: String,
}

/// An occurrence seen on the screen that no byte could be traced to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Untraced {
    /// The pane.
    pub pane: PaneId,
    /// When it was seen.
    pub at_us: u64,
}

/// What [`plan`] did and what it could not do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Bytes rewritten with bullets.
    pub replaced: usize,
    /// Masks placed.
    pub masked: usize,
    /// Input events withheld.
    pub keystrokes: usize,
    /// Seen on the screen, but not traceable to a byte: neither rewritten
    /// nor masked.
    pub untraced: Vec<Untraced>,
    /// Where the secret's bytes remain.
    pub remaining: Vec<Remaining>,
}

/// A redaction: the edits to add and the report.
#[derive(Debug, Clone)]
pub struct Redaction {
    /// The edits, to append to the session's list.
    pub edits: Vec<EditEntry>,
    /// What was done and what remains.
    pub report: Report,
    /// The session's list with the edits appended, evaluated.
    pub evaluation: Evaluation,
}

/// Bullets taking the same cells as `secret`.
pub fn bullets(secret: &str) -> String {
    secret
        .chars()
        .map(|c| "•".repeat(usize::from(codepoint_width(c))))
        .collect()
}

/// Plans the redaction of `secret` in every pane, after the session's own
/// edits.
pub fn plan(session: &Session, raw: &SessionIndex, secret: &str) -> Result<Redaction, EvalError> {
    let mut edits = Vec::new();
    let mut untraced = Vec::new();
    let replacement = bullets(secret);
    // Edits that rewrite text the terminal keeps but never draws; a refused
    // one is left for the report, since a mask cannot cover it.
    let mut hidden = Vec::new();
    for pane in &raw.panes {
        let stream = pane.stream_bytes();
        let replace = |bytes: Range<u64>, text: String, expect: String| {
            EditEntry::new(EditOp::ReplaceText {
                pane: pane.pane.clone(),
                bytes: ByteRange {
                    start: bytes.start,
                    end: bytes.end,
                },
                text,
                expect,
            })
        };
        for occurrence in find_text(pane, secret)? {
            match (&occurrence.pieces, occurrence.anchor) {
                // Plain text (SGR inside it kept): bullets as wide.
                _ if occurrence.plain.is_some() => {
                    let bytes = occurrence.plain.clone().unwrap_or_default();
                    edits.push(replace(bytes, replacement.clone(), replacement.clone()));
                }
                // Drawn in pieces: rewrite each with bullets as wide.
                (Some(pieces), _) => {
                    for piece in pieces {
                        let original = String::from_utf8_lossy(
                            &stream[piece.start as usize..piece.end as usize],
                        );
                        let text = bullets(&original);
                        edits.push(replace(piece.clone(), text.clone(), text));
                    }
                }
                (None, Some(anchor)) => {
                    edits.push(EditEntry::new(mask(&pane.pane, anchor, occurrence.cells)));
                }
                (None, None) => untraced.push(Untraced {
                    pane: pane.pane.clone(),
                    at_us: occurrence.at_us,
                }),
            }
        }

        // Command lines in OSC 133;C, matched on the value libghostty-vt
        // decodes (cmdline_url is percent-encoded, cmdline quoted as by
        // printf %q): each cmdline or cmdline_url option becomes
        // cmdline_url with the command line redacted.
        for mark in &pane.marks {
            let tap_index::MarkKind::CommandStart {
                command: Some(command),
            } = &mark.kind
            else {
                continue;
            };
            if !command.contains(secret) {
                continue;
            }
            let redacted = command.replace(secret, &replacement);
            let option = format!(
                "cmdline_url={}",
                String::from_utf8_lossy(&percent_encoded(redacted.as_bytes()))
            );
            for range in command_line_options(stream, mark.byte..mark.end) {
                hidden.push(edits.len());
                edits.push(replace(range, option.clone(), redacted.clone()));
            }
        }

        // Titles, icon names and working directories.
        for field in crate::replace::osc_fields(stream) {
            let value = &stream[field.value.start as usize..field.value.end as usize];
            let rewritten = match field.field {
                Field::Title | Field::IconName => {
                    replace_bytes(value, secret.as_bytes(), replacement.as_bytes())
                }
                Field::WorkingDirectory => {
                    replace_encoded(value, secret.as_bytes(), replacement.as_bytes())
                }
                Field::CommandLine => None,
            };
            let Some(text) = rewritten.and_then(|v| String::from_utf8(v).ok()) else {
                continue;
            };
            hidden.push(edits.len());
            edits.push(replace(field.value.clone(), text.clone(), text));
        }
        edits.extend(keystrokes(session, pane, secret.as_bytes()));
    }

    // Evaluate; rewrites that are refused become masks.
    let base = session.edits.edits.len();
    let mut all: Vec<EditEntry> = session.edits.edits.clone();
    all.extend(edits.iter().cloned());
    let first = evaluate_entries(session, raw, &all)?;
    for (k, edit) in edits.iter_mut().enumerate() {
        if hidden.contains(&k) {
            continue;
        }
        if let (
            Outcome::NeedsMask { .. },
            EditOp::ReplaceText {
                pane, bytes, text, ..
            },
        ) = (&first.outcomes[base + k], &edit.op)
        {
            let cells = text.chars().map(|c| u32::from(codepoint_width(c))).sum();
            edit.op = mask(pane, bytes.start, cells);
        }
    }
    all.truncate(base);
    all.extend(edits.iter().cloned());
    let evaluation = evaluate_entries(session, raw, &all)?;

    let count =
        |f: &dyn Fn(&Outcome) -> bool| evaluation.outcomes[base..].iter().filter(|o| f(o)).count();
    let report = Report {
        replaced: count(&|o| matches!(o, Outcome::Replaced { .. })),
        masked: count(&|o| matches!(o, Outcome::Masked { .. })),
        keystrokes: evaluation.outcomes[base..]
            .iter()
            .map(|o| match o {
                Outcome::Redacted { events } => events.len(),
                _ => 0,
            })
            .sum(),
        untraced,
        remaining: remaining(session, raw, &evaluation, secret.as_bytes()),
    };
    Ok(Redaction {
        edits,
        report,
        evaluation,
    })
}

fn mask(pane: &PaneId, anchor: u64, cells: u32) -> EditOp {
    EditOp::Mask {
        pane: pane.clone(),
        anchor: TrackedRef { byte: anchor },
        cells,
        style: MaskStyle::Blur,
    }
}

/// `RedactInput`s for the typed input events whose bytes, run together,
/// contain `secret`: one per stretch of matching events with no other input
/// event between them.
fn keystrokes(session: &Session, pane: &tap_index::PaneIndex, secret: &[u8]) -> Vec<EditEntry> {
    let events = session.pane_events(&pane.pane);
    // Typed input, run together, with the event each byte came from.
    let mut typed = Vec::new();
    let mut owner = Vec::new();
    for (event, item) in pane.stream().items.iter().enumerate() {
        if let ItemKind::Input {
            data,
            origin,
            redacted: false,
        } = &item.kind
            && *origin != Some(InputOrigin::TerminalReply)
        {
            typed.extend_from_slice(data);
            owner.extend(std::iter::repeat_n(event, data.len()));
        }
    }
    let mut matched: Vec<usize> = Vec::new();
    let mut at = 0;
    while let Some(k) = typed[at..]
        .windows(secret.len().max(1))
        .position(|w| w == secret)
    {
        let start = at + k;
        matched.extend(&owner[start..start + secret.len()]);
        at = start + secret.len().max(1);
    }
    matched.sort_unstable();
    matched.dedup();
    let inputs: Vec<usize> = pane
        .stream()
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.kind, ItemKind::Input { .. }))
        .map(|(e, _)| e)
        .collect();
    let mut out: Vec<EditEntry> = Vec::new();
    let mut group: Option<(usize, usize)> = None;
    for event in matched {
        group = match group {
            Some((first, last)) if !inputs.iter().any(|&i| i > last && i < event) => {
                Some((first, event))
            }
            Some((first, last)) => {
                out.push(redact(&pane.pane, events[first].seq, events[last].seq));
                Some((event, event))
            }
            None => Some((event, event)),
        };
    }
    if let Some((first, last)) = group {
        out.push(redact(&pane.pane, events[first].seq, events[last].seq));
    }
    out
}

fn redact(pane: &PaneId, first: u64, last: u64) -> EditEntry {
    EditEntry::new(EditOp::RedactInput {
        pane: pane.clone(),
        events: SeqRange {
            start: first,
            end: last + 1,
        },
    })
}

/// Percent-encodes everything but RFC 3986's unreserved characters, as a
/// shell integration writes a command line into `cmdline_url`.
fn percent_encoded(secret: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in secret {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
    out
}

/// Percent-decodes `value`, with the bytes of `value` each decoded byte
/// came from.
fn percent_decoded(value: &[u8]) -> (Vec<u8>, Vec<Range<usize>>) {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let (mut out, mut spans, mut i) = (Vec::new(), Vec::new(), 0);
    while i < value.len() {
        if value[i] == b'%'
            && let (Some(&h), Some(&l)) = (value.get(i + 1), value.get(i + 2))
            && let (Some(h), Some(l)) = (hex(h), hex(l))
        {
            out.push(h * 16 + l);
            spans.push(i..i + 3);
            i += 3;
        } else {
            out.push(value[i]);
            spans.push(i..i + 1);
            i += 1;
        }
    }
    (out, spans)
}

/// `value` with every occurrence of `secret` replaced by `with`, or `None`
/// when it has none.
fn replace_bytes(value: &[u8], secret: &[u8], with: &[u8]) -> Option<Vec<u8>> {
    let hits = occurrences(value, secret);
    if hits.is_empty() {
        return None;
    }
    let (mut out, mut at) = (Vec::new(), 0);
    for hit in hits {
        if hit < at {
            continue;
        }
        out.extend_from_slice(&value[at..hit]);
        out.extend_from_slice(with);
        at = hit + secret.len();
    }
    out.extend_from_slice(&value[at..]);
    Some(out)
}

/// A percent-encoded `value` whose decoded form has `secret` replaced by
/// `with` (encoded); the rest of `value` is kept as it was spelled.
fn replace_encoded(value: &[u8], secret: &[u8], with: &[u8]) -> Option<Vec<u8>> {
    let (decoded, spans) = percent_decoded(value);
    let hits = occurrences(&decoded, secret);
    if hits.is_empty() {
        return None;
    }
    let (mut out, mut at) = (Vec::new(), 0);
    for hit in hits {
        if hit < at {
            continue;
        }
        out.extend_from_slice(
            &value[spans.get(at).map_or(value.len(), |s| s.start)..spans[hit].start],
        );
        out.extend(percent_encoded(with));
        at = hit + secret.len();
    }
    out.extend_from_slice(&value[spans.get(at).map_or(value.len(), |s| s.start)..]);
    Some(out)
}

/// The `cmdline=` and `cmdline_url=` options of the OSC 133;C sequence at
/// `osc` in `stream`.
fn command_line_options(stream: &[u8], osc: Range<u64>) -> Vec<Range<u64>> {
    let bytes = &stream[osc.start as usize..osc.end as usize];
    let Some(body) = bytes.strip_prefix(b"\x1b]") else {
        return Vec::new();
    };
    let body = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))
        .unwrap_or(body);
    let mut out = Vec::new();
    let mut at = osc.start + 2;
    for option in body.split(|&b| b == b';') {
        if option.starts_with(b"cmdline=") || option.starts_with(b"cmdline_url=") {
            out.push(at..at + option.len() as u64);
        }
        at += option.len() as u64 + 1;
    }
    out
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Where the secret's bytes remain in the evaluated program.
fn remaining(
    session: &Session,
    raw: &SessionIndex,
    evaluation: &Evaluation,
    secret: &[u8],
) -> Vec<Remaining> {
    let mut out = Vec::new();
    let encoded = percent_encoded(secret);
    for pane in &raw.panes {
        let Some(program) = evaluation.programs.get(&pane.pane) else {
            continue;
        };
        let bytes = program.render(pane.stream_bytes());
        let mut hits: Vec<(usize, usize)> = occurrences(&bytes, secret)
            .into_iter()
            .map(|i| (i, secret.len()))
            .collect();
        if encoded != secret {
            hits.extend(
                occurrences(&bytes, &encoded)
                    .into_iter()
                    .map(|i| (i, encoded.len())),
            );
        }
        let tokens: Vec<_> = tokens(&bytes).collect();
        // Strings whose percent-decoded payload holds it in any spelling.
        for (r, token) in &tokens {
            if let Token::Osc(payload) | Token::Str { payload, .. } = token
                && !occurrences(&percent_decoded(payload).0, secret).is_empty()
                && !hits.iter().any(|&(at, _)| r.contains(&at))
            {
                hits.push((r.start, r.end - r.start));
            }
        }
        hits.sort_unstable();
        for (at, len) in hits {
            let masked = evaluation
                .masks
                .iter()
                .any(|m| m.pane == pane.pane && m.anchor == at as u64);
            let token = tokens.iter().find(|(r, _)| r.start <= at && at < r.end);
            let context = match token.map(|(_, t)| t) {
                _ if masked => "masked at render time".to_string(),
                Some(Token::Osc(payload)) => {
                    let number: String = payload
                        .iter()
                        .take_while(|b| b.is_ascii_digit())
                        .map(|&b| b as char)
                        .collect();
                    let kind = payload.get(number.len() + 1).map(|&b| b as char);
                    match (number.as_str(), kind) {
                        ("133", Some(k)) => format!("inside OSC 133;{k} (not drawn)"),
                        _ => format!("inside OSC {number} (not drawn)"),
                    }
                }
                Some(Token::Str { kind, .. }) => {
                    let name = match kind {
                        b'P' => "DCS",
                        b'_' => "APC",
                        b'^' => "PM",
                        _ => "SOS",
                    };
                    format!("inside {name} (not drawn)")
                }
                Some(Token::Text(_)) => "drawn as text".to_string(),
                _ => "inside an escape sequence (not drawn)".to_string(),
            };
            out.push(Remaining {
                pane: pane.pane.clone(),
                lane: "output",
                at: at as u64..(at + len) as u64,
                context,
            });
        }
        for (event, raw_event) in session.pane_events(&pane.pane).iter().enumerate() {
            if program.inputs().get(&event).is_some_and(|e| {
                matches!(e, InputEdit::Redacted { .. } | InputEdit::Removed { .. })
            }) {
                continue;
            }
            if let tap_model::EventKind::Input(input) = &raw_event.kind
                && !occurrences(&input.data, secret).is_empty()
            {
                out.push(Remaining {
                    pane: pane.pane.clone(),
                    lane: "input",
                    at: event as u64..event as u64 + 1,
                    context: "typed".into(),
                });
            }
        }
    }
    out
}
