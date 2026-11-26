//! Terminal state an excision carries across its splice, and the fixup
//! bytes that carry it.
//!
//! An excised command's output disappears, but what it changed about the
//! terminal must not: a later command may rely on a mode it set, and the
//! title or palette it left is part of what every later frame shows.
//! [`TermState`] is everything about a terminal libghostty-vt lets us read
//! that is not the screen's content or the cursor's position. Diffing the
//! state just before the splice (in the edited program) with the state the
//! original reached at the end of the excised range gives the bytes
//! ([`fixups`]) that, played at the splice, make the two equal. The
//! evaluator then checks they did.
//!
//! What cannot be read is listed in [`crate::excise::Unreadable`]; an
//! excision whose bytes touch it is refused instead.

use std::fmt::Write as _;

use serde::Serialize;
use tap_index::vt::{Color, CursorShape, DynamicColors, ModeValue, StateExtra, Terminal};

/// Modes the fixups never touch: the screen switches (an excised range is
/// on the primary screen throughout, so these agree), DECSC through mode
/// 1048 (a transition, not state), and 132-column mode, whose change clears
/// the screen ([`fixups`] refuses it).
const SKIPPED_MODES: [u16; 5] = [47, 1047, 1048, 1049, 3];

/// Everything readable about a terminal except its content and cursor
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermState {
    /// Every recognised mode.
    pub modes: Vec<ModeValue>,
    /// The 256-colour palette.
    pub palette: Vec<Color>,
    /// The palette OSC 104 restores.
    pub default_palette: Vec<Color>,
    /// OSC 10–12 colours.
    pub colors: DynamicColors,
    /// Kitty keyboard flags (top of the stack).
    pub kitty_flags: u8,
    /// Title bytes.
    pub title: Vec<u8>,
    /// Working directory (OSC 7 URI).
    pub pwd: Vec<u8>,
    /// OSC 22 pointer shape.
    pub mouse_shape: &'static str,
    /// DECSCUSR shape and blink.
    pub cursor_shape: (CursorShape, bool),
    /// The formatter's sequences for the state with no getter.
    pub extras: Vec<(StateExtra, Vec<u8>)>,
}

/// Where the cursor is and which screen is active: what fixups must leave
/// as they found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// Cursor column and row in the active area.
    pub cursor: (u16, u16),
    /// A wrap is pending.
    pub pending_wrap: bool,
    /// The alternate screen is active.
    pub alternate: bool,
    /// Columns and rows.
    pub size: (u16, u16),
}

impl Place {
    /// Reads it.
    pub fn read(term: &Terminal) -> Self {
        Self {
            cursor: term.cursor(),
            pending_wrap: term.cursor_pending_wrap(),
            alternate: term.on_alternate_screen(),
            size: term.size(),
        }
    }
}

impl TermState {
    /// Reads it.
    pub fn read(term: &Terminal) -> Self {
        Self {
            modes: term.modes(),
            palette: term.palette(),
            default_palette: term.default_palette(),
            colors: term.dynamic_colors(),
            kitty_flags: term.kitty_keyboard_flags(),
            title: term.title_bytes(),
            pwd: term.pwd(),
            mouse_shape: term.mouse_shape(),
            cursor_shape: term.cursor_shape(),
            extras: StateExtra::ALL
                .iter()
                .map(|&e| (e, term.state_extra(e)))
                .collect(),
        }
    }

    fn extra(&self, extra: StateExtra) -> &[u8] {
        self.extras
            .iter()
            .find(|(e, _)| *e == extra)
            .map_or(&[], |(_, b)| b.as_slice())
    }

    fn mode(&self, number: u16, ansi: bool) -> bool {
        self.modes
            .iter()
            .any(|m| m.number == number && m.ansi == ansi && m.set)
    }

    /// The names of the components that differ from `other`.
    pub fn differences(&self, other: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (a, b) in self.modes.iter().zip(&other.modes) {
            if a.set != b.set {
                out.push(format!("mode {}", mode_name(a)));
            }
        }
        let checks: [(&str, bool); 8] = [
            ("palette", self.palette != other.palette),
            ("colors", self.colors != other.colors),
            (
                "kitty keyboard flags",
                self.kitty_flags != other.kitty_flags,
            ),
            ("title", self.title != other.title),
            ("working directory", self.pwd != other.pwd),
            ("pointer shape", self.mouse_shape != other.mouse_shape),
            ("cursor shape", self.cursor_shape != other.cursor_shape),
            (
                "default palette",
                self.default_palette != other.default_palette,
            ),
        ];
        out.extend(
            checks
                .into_iter()
                .filter(|(_, differs)| *differs)
                .map(|(name, _)| name.to_string()),
        );
        for &(extra, ref bytes) in &self.extras {
            if bytes.as_slice() != other.extra(extra) {
                out.push(extra_name(extra).to_string());
            }
        }
        out
    }
}

fn mode_name(m: &ModeValue) -> String {
    if m.ansi {
        m.number.to_string()
    } else {
        format!("?{}", m.number)
    }
}

/// A short name for an extra.
pub fn extra_name(extra: StateExtra) -> &'static str {
    match extra {
        StateExtra::Charsets => "character sets",
        StateExtra::ScrollingRegion => "scrolling region",
        StateExtra::Tabstops => "tab stops",
        StateExtra::Keyboard => "modifyOtherKeys",
        StateExtra::Style => "text style",
        StateExtra::Hyperlink => "hyperlink",
        StateExtra::Protection => "character protection",
    }
}

/// One piece of the fixups: which state it restores and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixupPart {
    /// The state, as [`TermState::differences`] names it.
    pub state: String,
    /// The bytes, as text with escapes spelled out.
    pub bytes: String,
}

/// The bytes played at an excision's splice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Fixups {
    /// The bytes, as text with escapes spelled out.
    pub text: String,
    /// What each part restores.
    pub parts: Vec<FixupPart>,
    #[serde(skip)]
    pub(crate) bytes: Vec<u8>,
}

impl Fixups {
    pub(crate) fn push_part(&mut self, state: impl Into<String>, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parts.push(FixupPart {
            state: state.into(),
            bytes: escape(bytes),
        });
        self.bytes.extend_from_slice(bytes);
        self.text = escape(&self.bytes);
    }

    /// The raw bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether nothing needed restoring.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Bytes as text: printable ASCII as itself, ESC as `\e`, other controls
/// and non-ASCII bytes as `\xNN`.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
    out
}

fn osc(body: &[u8]) -> Vec<u8> {
    let mut out = b"\x1b]".to_vec();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\x1b\\");
    out
}

fn color(c: Color) -> String {
    format!("rgb:{:02x}/{:02x}/{:02x}", c[0], c[1], c[2])
}

/// Character sets as the formatter writes them: G0–G3 designations (final
/// byte, `None` for the default UTF-8) and the GL and GR slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Charsets {
    slots: [Option<u8>; 4],
    gl: u8,
    gr: u8,
}

fn charsets(extra: &[u8]) -> Charsets {
    let mut out = Charsets {
        slots: [None; 4],
        gl: 0,
        gr: 2,
    };
    let mut i = 0;
    while i < extra.len() {
        match &extra[i..] {
            [0x1b, inter @ (b'(' | b')' | b'*' | b'+'), f, ..] => {
                let slot = match inter {
                    b'(' => 0,
                    b')' => 1,
                    b'*' => 2,
                    _ => 3,
                };
                out.slots[slot] = Some(*f);
                i += 3;
            }
            [0x0e, ..] => {
                out.gl = 1;
                i += 1;
            }
            [0x1b, b'n', ..] => {
                out.gl = 2;
                i += 2;
            }
            [0x1b, b'o', ..] => {
                out.gl = 3;
                i += 2;
            }
            [0x1b, b'~', ..] => {
                out.gr = 1;
                i += 2;
            }
            [0x1b, b'|', ..] => {
                out.gr = 3;
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

/// What [`fixups`] cannot restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unfixable(pub String);

/// The bytes that take a terminal in state `from` to `to`, leaving the
/// cursor where it is unless restoring a piece of state moves it (the
/// caller puts it back). Fails on state that has no sequence to restore
/// it.
pub fn fixups(from: &TermState, to: &TermState) -> Result<Fixups, Unfixable> {
    let mut out = Fixups::default();

    if from.cursor_shape != to.cursor_shape {
        let (shape, blink) = to.cursor_shape;
        let base = match shape {
            CursorShape::Block => 1,
            CursorShape::Underline => 3,
            CursorShape::Bar => 5,
            CursorShape::BlockHollow | CursorShape::Other => {
                return Err(Unfixable(format!("cursor shape {shape:?}")));
            }
        };
        let ps = if blink { base } else { base + 1 };
        out.push_part("cursor shape", format!("\x1b[{ps} q").as_bytes());
    }

    let mut modes = Vec::new();
    for (a, b) in from.modes.iter().zip(&to.modes) {
        if a.set == b.set || (!a.ansi && SKIPPED_MODES.contains(&a.number)) {
            if a.set != b.set && a.number == 3 && !a.ansi {
                return Err(Unfixable("132-column mode (DECCOLM)".into()));
            }
            continue;
        }
        let prefix = if b.ansi { "" } else { "?" };
        let suffix = if b.set { 'h' } else { 'l' };
        modes.extend_from_slice(format!("\x1b[{prefix}{}{suffix}", b.number).as_bytes());
    }
    out.push_part("modes", &modes);

    let region = to.extra(StateExtra::ScrollingRegion);
    if from.extra(StateExtra::ScrollingRegion) != region {
        let mut bytes = b"\x1b[r".to_vec();
        if to.mode(69, false) {
            bytes.extend_from_slice(b"\x1b[s");
        }
        bytes.extend_from_slice(region);
        out.push_part("scrolling region", &bytes);
    }

    let tabs = to.extra(StateExtra::Tabstops);
    if from.extra(StateExtra::Tabstops) != tabs {
        out.push_part("tab stops", tabs);
    }

    let (a, b) = (
        charsets(from.extra(StateExtra::Charsets)),
        charsets(to.extra(StateExtra::Charsets)),
    );
    if a != b {
        let mut bytes = Vec::new();
        for (slot, (x, y)) in a.slots.iter().zip(&b.slots).enumerate() {
            match (x, y) {
                (x, Some(f)) if x != &Some(*f) => {
                    bytes.extend_from_slice(&[0x1b, b"()*+"[slot], *f]);
                }
                (Some(_), None) => {
                    // No sequence designates UTF-8 again; only a reset does.
                    return Err(Unfixable(format!("character set G{slot} back to UTF-8")));
                }
                _ => {}
            }
        }
        if a.gl != b.gl {
            bytes.extend_from_slice(match b.gl {
                0 => b"\x0f",
                1 => b"\x0e",
                2 => b"\x1bn",
                _ => b"\x1bo",
            });
        }
        if a.gr != b.gr {
            bytes.extend_from_slice(match b.gr {
                1 => b"\x1b~",
                2 => b"\x1b}",
                _ => b"\x1b|",
            });
        }
        out.push_part("character sets", &bytes);
    }

    let keyboard = to.extra(StateExtra::Keyboard);
    if from.extra(StateExtra::Keyboard) != keyboard {
        let mut bytes = b"\x1b[>4m".to_vec();
        bytes.extend_from_slice(keyboard);
        out.push_part("modifyOtherKeys", &bytes);
    }

    if from.kitty_flags != to.kitty_flags {
        out.push_part(
            "kitty keyboard flags",
            format!("\x1b[={};1u", to.kitty_flags).as_bytes(),
        );
    }

    if from.palette != to.palette || from.default_palette != to.default_palette {
        let (mut set, mut reset) = (String::new(), String::new());
        for (i, (x, y)) in from.palette.iter().zip(&to.palette).enumerate() {
            if x == y {
                continue;
            }
            if to.default_palette.get(i) == Some(y) {
                let _ = write!(reset, ";{i}");
            } else {
                let _ = write!(set, ";{i};{}", color(*y));
            }
        }
        let mut bytes = Vec::new();
        if !set.is_empty() {
            bytes.extend(osc(format!("4{set}").as_bytes()));
        }
        if !reset.is_empty() {
            bytes.extend(osc(format!("104{reset}").as_bytes()));
        }
        out.push_part("palette", &bytes);
    }

    let dynamic = [
        (from.colors.foreground, to.colors.foreground, 10),
        (from.colors.background, to.colors.background, 11),
        (from.colors.cursor, to.colors.cursor, 12),
    ];
    let mut bytes = Vec::new();
    for (x, y, n) in dynamic {
        if x != y {
            bytes.extend(match y {
                Some(c) => osc(format!("{n};{}", color(c)).as_bytes()),
                None => osc(format!("{}", n + 100).as_bytes()),
            });
        }
    }
    out.push_part("colors", &bytes);

    let printable = |b: &[u8]| b.iter().all(|&c| c >= 0x20 && c != 0x7f);
    if from.title != to.title {
        if !printable(&to.title) {
            return Err(Unfixable("a title with control characters".into()));
        }
        let mut body = b"2;".to_vec();
        body.extend_from_slice(&to.title);
        out.push_part("title", &osc(&body));
    }
    if from.pwd != to.pwd {
        if !printable(&to.pwd) {
            return Err(Unfixable(
                "a working directory with control characters".into(),
            ));
        }
        let mut body = b"7;".to_vec();
        body.extend_from_slice(&to.pwd);
        out.push_part("working directory", &osc(&body));
    }
    if from.mouse_shape != to.mouse_shape {
        if to.mouse_shape.is_empty() {
            return Err(Unfixable("an unknown pointer shape".into()));
        }
        out.push_part(
            "pointer shape",
            &osc(format!("22;{}", to.mouse_shape).as_bytes()),
        );
    }

    let style = to.extra(StateExtra::Style);
    if from.extra(StateExtra::Style) != style {
        let mut bytes = b"\x1b[0m".to_vec();
        bytes.extend_from_slice(style);
        out.push_part("text style", &bytes);
    }
    let link = to.extra(StateExtra::Hyperlink);
    if from.extra(StateExtra::Hyperlink) != link {
        let mut bytes = osc(b"8;;");
        bytes.extend_from_slice(link);
        out.push_part("hyperlink", &bytes);
    }
    let protection = to.extra(StateExtra::Protection);
    if from.extra(StateExtra::Protection) != protection {
        let mut bytes = b"\x1b[0\"q".to_vec();
        bytes.extend_from_slice(protection);
        out.push_part("character protection", &bytes);
    }
    Ok(out)
}

/// The CUP that puts the cursor back at `cursor` (active-area column and
/// row), given the state the fixups left: in origin mode CUP counts from
/// the top of the scrolling region.
pub fn cursor_back(state: &TermState, cursor: (u16, u16)) -> Result<Vec<u8>, Unfixable> {
    let mut row = u32::from(cursor.1);
    if state.mode(6, false) {
        let region = state.extra(StateExtra::ScrollingRegion);
        let top = std::str::from_utf8(region)
            .ok()
            .and_then(|s| s.strip_prefix("\x1b["))
            .and_then(|s| s.split(';').next())
            .and_then(|t| t.parse::<u32>().ok())
            .map_or(0, |t| t - 1);
        row = row
            .checked_sub(top)
            .ok_or_else(|| Unfixable("the cursor above an origin-mode scrolling region".into()))?;
    }
    Ok(format!("\x1b[{};{}H", row + 1, cursor.0 + 1).into_bytes())
}
