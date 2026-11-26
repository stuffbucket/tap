//! Reconstructing a command line from the keys typed at a prompt.
//!
//! Used when the shell did not send its command line with OSC 133;C (no
//! shell integration, or input that was not recorded). It is a deliberately
//! small line editor: printable keys insert at the end, Backspace deletes a
//! character, ^U the line and ^W a word, and bracketed paste inserts its text.
//! Anything that needs the shell's own state — history recall, cursor
//! movement, completion, other control keys — makes the result
//! *approximate*: the text is what the simple editor ends with, flagged so
//! the transcript can say it may differ from what ran.

/// What a run of keystrokes typed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reconstructed {
    /// The command text. Redacted keystrokes show as `•••`.
    pub text: String,
    /// Keys the simple editor cannot follow were pressed.
    pub approximate: bool,
    /// Some keystrokes were recorded without their bytes.
    pub redacted: bool,
    /// Enter was pressed (outside a bracketed paste); keys after it were not
    /// read.
    pub entered: bool,
}

/// Shown for keystrokes recorded without their bytes.
pub const REDACTED: &str = "•••";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Char(char),
    Redacted,
}

/// One input event's worth of keys.
#[derive(Debug, Clone, Copy)]
pub struct Keys<'a> {
    /// The bytes typed (empty when redacted).
    pub data: &'a [u8],
    /// Whether the bytes were withheld.
    pub redacted: bool,
}

/// Runs the line editor over keystrokes until the first Enter.
pub fn reconstruct<'a>(keys: impl IntoIterator<Item = Keys<'a>>) -> Reconstructed {
    let mut line: Vec<Token> = Vec::new();
    let mut out = Reconstructed::default();
    let mut paste = false;
    for key in keys {
        if key.redacted {
            line.push(Token::Redacted);
            out.redacted = true;
            continue;
        }
        let text = String::from_utf8_lossy(key.data);
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                let sequence = take_escape(&mut chars);
                match sequence.as_str() {
                    "[200~" => paste = true,
                    "[201~" => paste = false,
                    _ => out.approximate = true,
                }
                continue;
            }
            if paste {
                line.push(Token::Char(if c == '\r' { '\n' } else { c }));
                continue;
            }
            match c {
                '\r' | '\n' => {
                    out.entered = true;
                    out.text = render(&line);
                    return out;
                }
                '\x7f' | '\x08' => {
                    line.pop();
                }
                '\x15' => line.clear(),
                '\x17' => {
                    while matches!(line.last(), Some(Token::Char(c)) if c.is_whitespace()) {
                        line.pop();
                    }
                    while matches!(line.last(), Some(Token::Char(c)) if !c.is_whitespace()) {
                        line.pop();
                    }
                }
                c if c.is_control() => out.approximate = true,
                c => line.push(Token::Char(c)),
            }
        }
    }
    out.text = render(&line);
    out
}

/// Consumes the rest of an escape sequence after ESC and returns it
/// (without the ESC): a CSI or SS3 sequence, or one following byte.
fn take_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut sequence = String::new();
    match chars.next() {
        Some('[') => {
            sequence.push('[');
            for c in chars.by_ref() {
                sequence.push(c);
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
        Some('O') => {
            sequence.push('O');
            if let Some(c) = chars.next() {
                sequence.push(c);
            }
        }
        Some(c) => sequence.push(c),
        None => {}
    }
    sequence
}

fn render(line: &[Token]) -> String {
    let mut text = String::new();
    let mut previous_redacted = false;
    for token in line {
        match token {
            Token::Char(c) => {
                text.push(*c);
                previous_redacted = false;
            }
            Token::Redacted => {
                if !previous_redacted {
                    text.push_str(REDACTED);
                }
                previous_redacted = true;
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[&[u8]]) -> Reconstructed {
        reconstruct(keys.iter().map(|data| Keys {
            data,
            redacted: false,
        }))
    }

    #[test]
    fn follows_typing_and_simple_edits() {
        let r = typed(&[b"echo hellp", b"\x7f", b"o w\xc3\xb6rld", b"\r", b"ignored"]);
        assert_eq!(r.text, "echo hello wörld");
        assert!(r.entered && !r.approximate && !r.redacted);
        assert_eq!(typed(&[b"garbage", b"\x15", b"ls"]).text, "ls");
        assert_eq!(
            typed(&[b"git comit  ", b"\x17", b"commit"]).text,
            "git commit"
        );
    }

    #[test]
    fn history_movement_and_completion_are_approximate() {
        for keys in [
            &[&b"\x1b[A"[..], b"\r"][..],
            &[b"ech", b"\t", b"\r"],
            &[b"ab\x1bOD"],
        ] {
            assert!(typed(keys).approximate, "{keys:?}");
        }
    }

    #[test]
    fn bracketed_paste_inserts_text_and_enter_inside_it() {
        let r = typed(&[b"\x1b[200~echo a\recho b\x1b[201~", b"\r"]);
        assert_eq!(r.text, "echo a\necho b");
        assert!(r.entered && !r.approximate);
    }

    #[test]
    fn redacted_keys_show_as_one_placeholder_per_run() {
        let r = reconstruct([
            Keys {
                data: b"ls ",
                redacted: false,
            },
            Keys {
                data: b"",
                redacted: true,
            },
            Keys {
                data: b"",
                redacted: true,
            },
        ]);
        assert_eq!(r.text, format!("ls {REDACTED}"));
        assert!(r.redacted && !r.entered);
    }
}
