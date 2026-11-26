//! Excise: remove a command as if it never happened.
//!
//! An excision removes the bytes of a prompt-to-prompt range and plays
//! fixups in their place, so that everything after it runs on a terminal
//! in the same state it would have been in, minus what the command drew.
//! That is exact only when all of these hold, and [`check`] reports every
//! one that fails:
//!
//! 1. The range starts at an OSC 133 prompt mark (`P` or `A`, primary) and
//!    ends at one or at the end of the pane's output.
//! 2. The parser is at ground at both ends (checked by the evaluator, which
//!    replays to them).
//! 3. The primary screen is active at the start and throughout.
//! 4. No Kitty image transmitted inside is used after it (placed, deleted
//!    by id, animated, or drawn by Unicode placeholders), and nothing
//!    inside deletes images when Kitty graphics are used after it.
//! 5. After the range, nothing positions the cursor absolutely on the
//!    primary screen before the screen is cleared: without the command's
//!    rows the cursor is higher than it was, so absolute positions land
//!    elsewhere. A clear is ED 2 next to a cursor positioning (as `clear`
//!    writes it), or a full reset; after it the two terminals agree again.
//!    Positioning inside an alternate-screen program does not count.
//! 6. The range changes no state that cannot be read back ([`Unreadable`])
//!    in a way that matters later.
//! 7. Every state difference has a fixup ([`crate::state::fixups`]) and,
//!    played, the fixups make the state equal (checked by the evaluator).
//!
//! When any fails the edit is played as a skip instead (the time is cut
//! but the bytes still apply), which is always exact.

use std::ops::Range;

use serde::Serialize;
use tap_index::{MarkKind, PaneIndex};

use crate::tokens::{Token, tokens};

/// An end of the excised range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    /// Its first byte.
    Start,
    /// Just past its last byte.
    End,
}

/// State libghostty-vt keeps but does not let us read, so an excision that
/// changes it cannot be fixed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unreadable {
    /// The cursor saved by DECSC (`ESC 7`, `CSI s`, mode 1048) or reset by
    /// DECSTR, when something after the range restores it.
    SavedCursor,
    /// Private modes saved by XTSAVE (`CSI ? Pm s`), when something after
    /// the range restores them.
    SavedModes,
    /// The Kitty keyboard flags stack below its top, when the range pushes
    /// or pops more than it balances.
    KittyKeyboardStack,
    /// XTSHIFTESCAPE (`CSI > Ps s`).
    MouseShiftCapture,
    /// DECSASD (`CSI Ps $ }`), the active status display.
    StatusDisplay,
}

/// Why an excision is refused (and played as a skip instead).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum Refusal {
    /// An end is not at a primary OSC 133 prompt mark (or the end of the
    /// pane's output).
    NotAtPrompt {
        /// Which end.
        side: Side,
        /// Its byte.
        byte: u64,
    },
    /// The parser is inside a sequence at an end.
    NotAtGround {
        /// Which end.
        side: Side,
        /// Its byte.
        byte: u64,
    },
    /// The alternate screen is active at the start or entered inside.
    AlternateScreen {
        /// Where.
        byte: u64,
    },
    /// A Kitty image transmitted inside is used after the range.
    KittyImageUsedLater {
        /// The image, as `i=…` or `I=…`.
        image: String,
        /// Where it is transmitted.
        transmitted: u64,
        /// Where it is first used after the range.
        used: u64,
    },
    /// Kitty images are deleted inside and Kitty graphics used after.
    KittyImageDeleted {
        /// The deletion.
        byte: u64,
    },
    /// The cursor is positioned absolutely after the range, before a
    /// clear.
    AbsolutePositioning {
        /// The sequence's first byte.
        byte: u64,
        /// The sequence, escaped.
        sequence: String,
    },
    /// The range changes state that cannot be read back.
    UnreadableState {
        /// Which.
        state: Unreadable,
        /// The sequence that changes it.
        byte: u64,
    },
    /// State the range changed has no sequence to restore it.
    Unfixable {
        /// What.
        state: String,
    },
    /// After the fixups, the state still differs.
    FixupMismatch {
        /// The components that differ.
        states: Vec<String>,
    },
    /// The range crosses an output event whose recorded bytes (`data`)
    /// differ from the bytes it plays (`playbackData`: a Kitty file
    /// transfer normalised at capture). Byte coordinates count the playback
    /// bytes, but splicing them would leave the recorded ones inconsistent.
    PlaybackDiffers {
        /// Index of the event in the pane.
        event: usize,
        /// Its playback bytes.
        bytes: Range<u64>,
    },
    /// Another excision overlaps it.
    Overlaps {
        /// Index of that edit.
        op: usize,
    },
}

/// Whether the token switches screens: `Some(true)` entering the alternate
/// screen, `Some(false)` leaving it.
pub(crate) fn screen_switch(token: &Token<'_>) -> Option<bool> {
    let set = if token.is_private_csi(b'?', b'h') {
        true
    } else if token.is_private_csi(b'?', b'l') {
        false
    } else {
        return None;
    };
    token
        .params()
        .iter()
        .any(|p| matches!(p, Some(47 | 1047 | 1049)))
        .then_some(set)
}

/// Whether the token puts the cursor at an absolute row.
fn absolute(token: &Token<'_>) -> bool {
    let p = token.params();
    token.is_csi(b'H')
        || token.is_csi(b'f')
        || token.is_csi(b'd')
        || token.is_csi(b'r')
        || (token.is_csi(b'u') && p.is_empty())
        || (token.is_csi(b's') && !p.is_empty())
        || (token.is_private_csi(b'?', b'h') && p.contains(&Some(6)))
        || (token.is_private_csi(b'?', b'l') && p.contains(&Some(1048)))
        || matches!(
            token,
            Token::Esc {
                intermediates: [],
                final_byte: b'8'
            } | Token::Esc {
                intermediates: [b'#'],
                final_byte: b'8'
            }
        )
}

fn is_erase_display(token: &Token<'_>) -> bool {
    token.is_csi(b'J') && token.params().first() == Some(&Some(2))
}

fn is_reset(token: &Token<'_>) -> bool {
    matches!(
        token,
        Token::Esc {
            intermediates: [],
            final_byte: b'c'
        }
    )
}

/// Sequences that neither print nor move the cursor, between the halves of
/// a clear.
fn quiet(token: &Token<'_>) -> bool {
    token.is_sgr()
        || (token.is_csi(b'J') && !is_erase_display(token))
        || token.is_csi(b'K')
        || matches!(token, Token::Osc(_) | Token::Str { .. })
        || ((token.is_private_csi(b'?', b'h') || token.is_private_csi(b'?', b'l'))
            && screen_switch(token).is_none()
            && !absolute(token))
}

fn kitty_keys(payload: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let control = payload.strip_prefix(b"G")?;
    let control = control.split(|&b| b == b';').next().unwrap_or(&[]);
    Some(
        control
            .split(|&b| b == b',')
            .filter_map(|kv| match kv {
                [k, b'=', v @ ..] => Some((*k, v)),
                _ => None,
            })
            .collect(),
    )
}

/// Placeholder for Kitty's Unicode placeholder character, U+10EEEE.
const PLACEHOLDER: &[u8] = "\u{10EEEE}".as_bytes();

/// Checks every precondition that needs only the pane's marks and bytes
/// (1, 3, 4, 5 and 6). `stream` is the pane's raw stream.
pub fn check(pane: &PaneIndex, range: &Range<u64>) -> Vec<Refusal> {
    let stream = pane.stream_bytes();
    let len = stream.len() as u64;
    let (a, b) = (range.start, range.end);
    let mut refusals = Vec::new();

    let prompt_at = |byte: u64| {
        pane.marks
            .iter()
            .any(|m| m.byte == byte && m.kind == MarkKind::Prompt)
    };
    if !prompt_at(a) {
        refusals.push(Refusal::NotAtPrompt {
            side: Side::Start,
            byte: a,
        });
    }
    if b != len && !prompt_at(b) {
        refusals.push(Refusal::NotAtPrompt {
            side: Side::End,
            byte: b,
        });
    }

    // 3. Primary screen at the start and throughout.
    let switch =
        |m: &&tap_index::Mark| matches!(m.kind, MarkKind::AltScreenEnter | MarkKind::AltScreenExit);
    let active_at_start = pane
        .marks
        .iter()
        .filter(switch)
        .rfind(|m| m.end <= a)
        .is_some_and(|m| m.kind == MarkKind::AltScreenEnter);
    let entered = pane
        .marks
        .iter()
        .find(|m| m.kind == MarkKind::AltScreenEnter && m.byte >= a && m.byte < b)
        .map(|m| m.byte);
    if active_at_start {
        refusals.push(Refusal::AlternateScreen { byte: a });
    } else if let Some(byte) = entered {
        refusals.push(Refusal::AlternateScreen { byte });
    }

    let inside: Vec<(Range<usize>, Token<'_>)> = tokens(&stream[a as usize..b as usize]).collect();
    let after: Vec<(Range<usize>, Token<'_>)> = tokens(&stream[b as usize..]).collect();
    let at_in = |r: &Range<usize>| a + r.start as u64;
    let at_after = |r: &Range<usize>| b + r.start as u64;

    // 4. Kitty images.
    let mut transmitted: Vec<(String, u64)> = Vec::new();
    let mut deleted = None;
    for (r, token) in &inside {
        let Token::Str {
            kind: b'_',
            payload,
        } = token
        else {
            continue;
        };
        let Some(keys) = kitty_keys(payload) else {
            continue;
        };
        let action = keys
            .iter()
            .find(|(k, _)| *k == b'a')
            .map_or(b"t".as_slice(), |(_, v)| v);
        match action {
            b"t" | b"T" => {
                for (k, v) in &keys {
                    if matches!(k, b'i' | b'I') && *v != b"0" {
                        transmitted.push((
                            format!("{}={}", *k as char, String::from_utf8_lossy(v)),
                            at_in(r),
                        ));
                    }
                }
            }
            b"d" => {
                deleted.get_or_insert(at_in(r));
            }
            _ => {}
        }
    }
    let mut kitty_after = None;
    let mut used = Vec::new();
    for (r, token) in &after {
        match token {
            Token::Str {
                kind: b'_',
                payload,
            } => {
                let Some(keys) = kitty_keys(payload) else {
                    continue;
                };
                kitty_after.get_or_insert(at_after(r));
                for (k, v) in &keys {
                    let id = format!("{}={}", *k as char, String::from_utf8_lossy(v));
                    if let Some((image, at)) = transmitted.iter().find(|(i, _)| *i == id)
                        && !used.iter().any(|(i, _, _)| i == image)
                    {
                        used.push((image.clone(), *at, at_after(r)));
                    }
                }
            }
            Token::Text(text) if !transmitted.is_empty() => {
                if let Some(k) = text
                    .windows(PLACEHOLDER.len())
                    .position(|w| w == PLACEHOLDER)
                {
                    for (image, at) in &transmitted {
                        if !used.iter().any(|(i, _, _)| i == image) {
                            used.push((image.clone(), *at, at_after(r) + k as u64));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (image, transmitted, used) in used {
        refusals.push(Refusal::KittyImageUsedLater {
            image,
            transmitted,
            used,
        });
    }
    if let (Some(byte), Some(_)) = (deleted, kitty_after) {
        refusals.push(Refusal::KittyImageDeleted { byte });
    }

    // 5. Absolute positioning after the range, before a clear.
    let mut alt = false;
    let mut i = 0;
    'scan: while i < after.len() {
        let (r, token) = &after[i];
        if let Some(enter) = screen_switch(token) {
            alt = enter;
        }
        if alt {
            i += 1;
            continue;
        }
        if is_reset(token) {
            break;
        }
        let next = || {
            after[i + 1..]
                .iter()
                .find(|(_, t)| !quiet(t))
                .map(|(_, t)| t)
        };
        if is_erase_display(token) && next().is_some_and(|t| t.is_csi(b'H') || t.is_csi(b'f')) {
            break 'scan;
        }
        if absolute(token) {
            if (token.is_csi(b'H') || token.is_csi(b'f')) && next().is_some_and(is_erase_display) {
                break 'scan;
            }
            refusals.push(Refusal::AbsolutePositioning {
                byte: at_after(r),
                sequence: crate::state::escape(
                    &stream[(b as usize + r.start)..(b as usize + r.end)],
                ),
            });
            break;
        }
        i += 1;
    }

    // 6. Unreadable state.
    let mut saved_cursor = None;
    let mut saved_modes = None;
    let mut depth = 0i64;
    let mut stack_change = None;
    let mut unbalanced = false;
    for (r, token) in &inside {
        let at = at_in(r);
        let p = token.params();
        if matches!(
            token,
            Token::Esc {
                intermediates: [],
                final_byte: b'7'
            }
        ) || (token.is_csi(b's') && p.is_empty())
            || (token.is_private_csi(b'?', b'h') && p.contains(&Some(1048)))
            || matches!(
                token,
                Token::Csi {
                    private: None,
                    intermediates: [b'!'],
                    final_byte: b'p',
                    ..
                }
            )
        {
            saved_cursor.get_or_insert(at);
        }
        if token.is_private_csi(b'?', b's') {
            saved_modes.get_or_insert(at);
        }
        if token.is_private_csi(b'>', b'u') {
            stack_change.get_or_insert(at);
            depth += 1;
        }
        if token.is_private_csi(b'<', b'u') {
            stack_change.get_or_insert(at);
            depth -= i64::from(p.first().copied().flatten().unwrap_or(1).max(1));
            unbalanced |= depth < 0;
        }
        if token.is_private_csi(b'>', b's') {
            refusals.push(Refusal::UnreadableState {
                state: Unreadable::MouseShiftCapture,
                byte: at,
            });
        }
        if matches!(
            token,
            Token::Csi {
                private: None,
                intermediates: [b'$'],
                final_byte: b'}',
                ..
            }
        ) {
            refusals.push(Refusal::UnreadableState {
                state: Unreadable::StatusDisplay,
                byte: at,
            });
        }
    }
    if let Some(byte) = stack_change
        && (unbalanced || depth != 0)
    {
        refusals.push(Refusal::UnreadableState {
            state: Unreadable::KittyKeyboardStack,
            byte,
        });
    }
    if saved_cursor.is_some() || saved_modes.is_some() {
        let mut alt = false;
        let (mut cursor_live, mut modes_live) = (saved_cursor.is_some(), saved_modes.is_some());
        for (_, token) in &after {
            if let Some(enter) = screen_switch(token) {
                // Entering the alternate screen with 1049 saves the cursor.
                if enter && token.params().contains(&Some(1049)) && !alt {
                    cursor_live = false;
                }
                alt = enter;
            }
            if is_reset(token) {
                break;
            }
            let p = token.params();
            if !alt {
                let save = matches!(
                    token,
                    Token::Esc {
                        intermediates: [],
                        final_byte: b'7'
                    }
                ) || (token.is_csi(b's') && p.is_empty())
                    || (token.is_private_csi(b'?', b'h') && p.contains(&Some(1048)));
                let restore = matches!(
                    token,
                    Token::Esc {
                        intermediates: [],
                        final_byte: b'8'
                    }
                ) || (token.is_csi(b'u') && p.is_empty())
                    || (token.is_private_csi(b'?', b'l') && p.contains(&Some(1048)));
                if save {
                    cursor_live = false;
                }
                if restore && cursor_live {
                    if let Some(byte) = saved_cursor.take() {
                        refusals.push(Refusal::UnreadableState {
                            state: Unreadable::SavedCursor,
                            byte,
                        });
                    }
                    cursor_live = false;
                }
            }
            if token.is_private_csi(b'?', b'r') && modes_live {
                if let Some(byte) = saved_modes.take() {
                    refusals.push(Refusal::UnreadableState {
                        state: Unreadable::SavedModes,
                        byte,
                    });
                }
                modes_live = false;
            }
        }
    }
    refusals
}
