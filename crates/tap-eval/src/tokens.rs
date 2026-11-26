//! A small VT tokenizer, for classifying what a range of bytes does.
//!
//! The index finds marks by feeding libghostty-vt, which delimits sequences
//! exactly. The checks here only need to know which sequences a range holds
//! (does the next command position the cursor absolutely? does an excised
//! command transmit a Kitty image?), over ranges that start at ground, so a
//! plain state machine is enough: printable text, C0 controls, ESC
//! sequences, CSI sequences, OSC strings and DCS/APC/PM/SOS strings.
//! Bytes from 0x80 up are text (the terminal decodes UTF-8; it does not
//! treat them as C1 controls). CAN and SUB abort a sequence; ESC starts a
//! new one; a string ends at ST (`ESC \`) or, for OSC, BEL.

use std::ops::Range;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// One token, borrowing from the bytes it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<'a> {
    /// Printable bytes.
    Text(&'a [u8]),
    /// A C0 control other than ESC, or DEL.
    Control(u8),
    /// `ESC` intermediates final.
    Esc {
        /// Bytes 0x20–0x2F.
        intermediates: &'a [u8],
        /// The final byte.
        final_byte: u8,
    },
    /// `ESC [` private-marker params intermediates final.
    Csi {
        /// `<`, `=`, `>` or `?` right after `[`, if any.
        private: Option<u8>,
        /// Parameter bytes (digits, `;`, `:`).
        params: &'a [u8],
        /// Bytes 0x20–0x2F.
        intermediates: &'a [u8],
        /// The final byte.
        final_byte: u8,
    },
    /// `ESC ]` payload, without its terminator.
    Osc(&'a [u8]),
    /// `ESC P`, `ESC _`, `ESC ^` or `ESC X` (the byte after ESC is `kind`)
    /// and its payload.
    Str {
        /// `P` (DCS), `_` (APC), `^` (PM) or `X` (SOS).
        kind: u8,
        /// The payload, without its terminator.
        payload: &'a [u8],
    },
    /// A sequence the bytes end inside of, or one CAN or SUB aborted.
    Incomplete(&'a [u8]),
}

impl Token<'_> {
    /// Whether it is SGR (`CSI … m`), which changes no cell and moves
    /// nothing.
    pub fn is_sgr(&self) -> bool {
        matches!(
            self,
            Token::Csi {
                private: None,
                intermediates: [],
                final_byte: b'm',
                ..
            }
        )
    }

    /// The numeric parameters of a CSI sequence (an empty parameter is
    /// `None`); empty for anything else.
    pub fn params(&self) -> Vec<Option<u32>> {
        let Token::Csi { params, .. } = self else {
            return Vec::new();
        };
        if params.is_empty() {
            return Vec::new();
        }
        params
            .split(|&b| b == b';')
            .map(|p| {
                let digits = p.split(|&b| b == b':').next().unwrap_or(&[]);
                std::str::from_utf8(digits).ok()?.parse().ok()
            })
            .collect()
    }

    /// Whether it is a plain CSI (no private marker, no intermediates)
    /// with this final byte.
    pub fn is_csi(&self, f: u8) -> bool {
        matches!(
            self,
            Token::Csi {
                private: None,
                intermediates: [],
                final_byte,
                ..
            } if *final_byte == f
        )
    }

    /// Whether it is a CSI with this private marker and final byte, and no
    /// intermediates.
    pub fn is_private_csi(&self, marker: u8, f: u8) -> bool {
        matches!(
            self,
            Token::Csi {
                private: Some(p),
                intermediates: [],
                final_byte,
                ..
            } if *p == marker && *final_byte == f
        )
    }
}

/// Iterates over the tokens of `bytes`, which must start at ground, with
/// each token's range in `bytes`.
pub fn tokens(bytes: &[u8]) -> Tokens<'_> {
    Tokens { bytes, at: 0 }
}

/// See [`tokens`].
pub struct Tokens<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = (Range<usize>, Token<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.bytes;
        let start = self.at;
        let first = *bytes.get(start)?;
        if first != ESC && (first < 0x20 || first == 0x7f) {
            self.at += 1;
            return Some((start..self.at, Token::Control(first)));
        }
        if first != ESC {
            let len = bytes[start..]
                .iter()
                .position(|&b| b < 0x20 || b == 0x7f)
                .unwrap_or(bytes.len() - start);
            self.at += len;
            return Some((start..self.at, Token::Text(&bytes[start..self.at])));
        }
        let (end, token) = sequence(bytes, start);
        self.at = end;
        Some((start..end, token))
    }
}

/// Reads the sequence whose ESC is at `start`; returns where it ends.
fn sequence(bytes: &[u8], start: usize) -> (usize, Token<'_>) {
    let incomplete = |end: usize| (end, Token::Incomplete(&bytes[start..end]));
    let Some(&kind) = bytes.get(start + 1) else {
        return incomplete(bytes.len());
    };
    match kind {
        b'[' => {
            let mut i = start + 2;
            let private = match bytes.get(i) {
                Some(&b @ (b'<' | b'=' | b'>' | b'?')) => {
                    i += 1;
                    Some(b)
                }
                _ => None,
            };
            let params_start = i;
            while let Some(&b) = bytes.get(i) {
                if (0x30..=0x3f).contains(&b) {
                    i += 1;
                } else {
                    break;
                }
            }
            let params_end = i;
            while let Some(&b) = bytes.get(i) {
                if (0x20..=0x2f).contains(&b) {
                    i += 1;
                } else {
                    break;
                }
            }
            match bytes.get(i) {
                Some(&f) if (0x40..=0x7e).contains(&f) => (
                    i + 1,
                    Token::Csi {
                        private,
                        params: &bytes[params_start..params_end],
                        intermediates: &bytes[params_end..i],
                        final_byte: f,
                    },
                ),
                Some(&(CAN | SUB)) => incomplete(i + 1),
                // ESC or anything else malformed ends it here; the caller
                // reads what follows as a new token.
                Some(_) => incomplete(i),
                None => incomplete(bytes.len()),
            }
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let body = start + 2;
            let mut i = body;
            while let Some(&b) = bytes.get(i) {
                match b {
                    BEL if kind == b']' => {
                        return (i + 1, Token::Osc(&bytes[body..i]));
                    }
                    ESC if bytes.get(i + 1) == Some(&b'\\') => {
                        let payload = &bytes[body..i];
                        let token = if kind == b']' {
                            Token::Osc(payload)
                        } else {
                            Token::Str { kind, payload }
                        };
                        return (i + 2, token);
                    }
                    ESC => return incomplete(i),
                    CAN | SUB => return incomplete(i + 1),
                    _ => i += 1,
                }
            }
            incomplete(bytes.len())
        }
        _ => {
            let mut i = start + 1;
            while let Some(&b) = bytes.get(i) {
                if (0x20..=0x2f).contains(&b) {
                    i += 1;
                } else {
                    break;
                }
            }
            match bytes.get(i) {
                Some(&f) if (0x30..=0x7e).contains(&f) => (
                    i + 1,
                    Token::Esc {
                        intermediates: &bytes[start + 1..i],
                        final_byte: f,
                    },
                ),
                Some(_) => incomplete(i),
                None => incomplete(bytes.len()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_controls_and_sequences() {
        let bytes = b"ab\r\x1b[?1049h\x1b[31mc\x1b]2;t\x07\x1b_Ga=T;AA\x1b\\\x1b(0\x1b[5";
        let got: Vec<_> = tokens(bytes).collect();
        assert_eq!(got[0], (0..2, Token::Text(b"ab")));
        assert_eq!(got[1], (2..3, Token::Control(b'\r')));
        assert!(got[2].1.is_private_csi(b'?', b'h'));
        assert_eq!(got[2].1.params(), vec![Some(1049)]);
        assert!(got[3].1.is_sgr());
        assert_eq!(got[4].1, Token::Text(b"c"));
        assert_eq!(got[5].1, Token::Osc(b"2;t"));
        assert_eq!(
            got[6].1,
            Token::Str {
                kind: b'_',
                payload: b"Ga=T;AA"
            }
        );
        assert_eq!(
            got[7].1,
            Token::Esc {
                intermediates: b"(",
                final_byte: b'0'
            }
        );
        assert_eq!(got[8].1, Token::Incomplete(b"\x1b[5"));
        assert_eq!(got.len(), 9);
    }
}
