//! The terminal engine, behind the few operations Tap's index needs.
//!
//! This is the only module that uses libghostty-rs (`libghostty-vt`), so the
//! engine can be swapped for `stuffbucket/libghostty-render`'s Rust binding
//! without touching the rest of the crate. Nothing here talks to a PTY: no
//! write-back callback is installed, so the terminal's replies to queries are
//! dropped.
//!
//! The terminal reports what the index cares about as [`Signal`]s, collected
//! while bytes are written and drained with [`Terminal::take_signals`]:
//! OSC 133 semantic prompt marks (through libghostty-vt's semantic prompt
//! callback), full resets (its reset callback) and title changes.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use libghostty_vt as ghostty;
use libghostty_vt::effect::{PromptKind, SemanticPromptKind};
use libghostty_vt::fmt::{Extras, Format, Formatter};
use libghostty_vt::grid_ref::TrackedGridRef;
use libghostty_vt::point::{Coordinate, Point, Space};
use libghostty_vt::screen::Screen;
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::Boundary;

/// Cell size in pixels given to the terminal on resize. Only pixel-size
/// reports and image placement depend on it, and the index answers no
/// queries, so any fixed value works; it is part of the engine fingerprint
/// because snapshots record it.
const CELL_WIDTH_PX: u32 = 10;
const CELL_HEIGHT_PX: u32 = 20;

/// How much history a terminal keeps above its active area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scrollback {
    /// Everything, for reading a whole command's output.
    Unlimited,
    /// At most this many bytes of page memory, as Ghostty's
    /// `scrollback-limit-bytes` configures it. libghostty-vt drops whole
    /// pages of the oldest history to stay under it.
    Bytes(usize),
}

/// Ghostty's default `scrollback-limit-bytes` at the commit libghostty-vt
/// is built from (`src/config/Config.zig`): what a Ghostty window running
/// the recorded program would have kept.
pub const GHOSTTY_SCROLLBACK_BYTES: usize = 50_000_000;

/// Something the engine failed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtError(String);

impl fmt::Display for VtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "libghostty-vt: {}", self.0)
    }
}

impl std::error::Error for VtError {}

fn err(context: &str) -> impl Fn(ghostty::Error) -> VtError + '_ {
    move |e| VtError(format!("{context}: {e}"))
}

/// What the terminal reported while bytes were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// OSC 133;P or ;A: a prompt starts.
    PromptStart {
        /// A continuation or secondary prompt rather than a primary one.
        continuation: bool,
    },
    /// OSC 133;C: the command was accepted and its output follows. Carries the
    /// decoded `cmdline_url`/`cmdline` when the shell sent one.
    OutputStart {
        /// The command line, if sent.
        command: Option<Vec<u8>>,
    },
    /// OSC 133;D: the command finished.
    CommandEnd {
        /// Its exit status, if sent.
        exit_code: Option<i32>,
    },
    /// OSC 133 input start (B), which Tap's shim never sends.
    InputStart,
    /// RIS (`ESC c`).
    FullReset,
    /// OSC 0/2 changed the title.
    TitleChanged,
}

/// Where [`Terminal::write_sequence`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceStep {
    /// Bytes consumed from the slice.
    pub consumed: usize,
    /// The stream is at ground after them.
    pub ground: bool,
    /// An OSC started: its first byte (ESC, or the C1 introducer) is this
    /// many bytes before the end of the consumed bytes. It may lie in an
    /// earlier slice.
    pub osc_start_back: Option<usize>,
}

/// A cell position that follows its cell as the terminal scrolls and
/// reflows. It is lost when the cell is (scrollback cleared, full reset).
pub struct Pin(TrackedGridRef);

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pin")
    }
}

/// A libghostty-vt terminal that never writes to a PTY.
pub struct Terminal {
    term: ghostty::Terminal<'static, 'static>,
    signals: Rc<RefCell<Vec<Signal>>>,
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

/// Identifies the engine for checkpoint keys: libghostty-vt's version, build
/// metadata and optimisation mode, and the settings this module applies,
/// including the scrollback limit. Snapshots are only exchanged between
/// terminals with the same fingerprint.
pub fn engine_fingerprint(scrollback: Scrollback) -> String {
    let version = ghostty::build_info::version_string().to_string_lossy();
    let build = ghostty::build_info::build_version()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let optimize = ghostty::build_info::optimize_mode()
        .map(|m| format!("{m:?}"))
        .unwrap_or_else(|_| "unknown".into());
    let scrollback = match scrollback {
        Scrollback::Unlimited => "unlimited".to_string(),
        Scrollback::Bytes(bytes) => bytes.to_string(),
    };
    format!(
        "libghostty-vt {version}{}{build} {optimize} cell={CELL_WIDTH_PX}x{CELL_HEIGHT_PX} scrollback={scrollback}",
        if build.is_empty() { "" } else { "+" }
    )
}

impl Terminal {
    /// A fresh terminal of the given size.
    pub fn new(cols: u16, rows: u16, scrollback: Scrollback) -> Result<Self, VtError> {
        let mut term = ghostty::Terminal::new(cols.max(1), rows.max(1)).map_err(err("new"))?;
        term.resize(cols.max(1), rows.max(1), CELL_WIDTH_PX, CELL_HEIGHT_PX)
            .map_err(err("resize"))?;
        // `Some(usize::MAX)` is libghostty-vt's "no byte limit"; `None` would
        // be its default of one page or so. No line limit either way.
        let bytes = match scrollback {
            Scrollback::Unlimited => usize::MAX,
            Scrollback::Bytes(bytes) => bytes,
        };
        term.set_scrollback_max_bytes(Some(bytes))
            .and_then(|t| t.set_scrollback_max_lines(None))
            .map_err(err("scrollback limit"))?;
        Self::install(term)
    }

    /// Restores a terminal from [`Terminal::snapshot`] bytes. The snapshot
    /// carries the scrollback limit.
    pub fn restore(snapshot: &[u8]) -> Result<Self, VtError> {
        let decoder = ghostty::snapshot::Decoder::new_buf(snapshot).map_err(err("decoder"))?;
        let term = decoder.decode().map_err(err("decode snapshot"))?;
        Self::install(term)
    }

    fn install(mut term: ghostty::Terminal<'static, 'static>) -> Result<Self, VtError> {
        let signals = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&signals);
        term.on_semantic_prompt(move |_, event| {
            let signal = match event.kind() {
                Ok(SemanticPromptKind::PromptStart) => Signal::PromptStart {
                    continuation: !matches!(event.prompt_kind(), Ok(PromptKind::Primary)),
                },
                Ok(SemanticPromptKind::InputStart) => Signal::InputStart,
                Ok(SemanticPromptKind::OutputStart) => {
                    let command = event.command();
                    Signal::OutputStart {
                        command: (!command.is_empty()).then(|| command.to_vec()),
                    }
                }
                Ok(SemanticPromptKind::CommandEnd) => Signal::CommandEnd {
                    exit_code: event.exit_code(),
                },
                _ => return,
            };
            sink.borrow_mut().push(signal);
        })
        .map_err(err("semantic prompt callback"))?;
        let sink = Rc::clone(&signals);
        term.on_reset(move |_| sink.borrow_mut().push(Signal::FullReset))
            .map_err(err("reset callback"))?;
        let sink = Rc::clone(&signals);
        term.on_title_changed(move |_| sink.borrow_mut().push(Signal::TitleChanged))
            .map_err(err("title callback"))?;
        Ok(Self { term, signals })
    }

    /// Columns and rows.
    pub fn size(&self) -> (u16, u16) {
        (self.term.cols().unwrap_or(0), self.term.rows().unwrap_or(0))
    }

    /// Resizes the grid, reflowing the primary screen.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), VtError> {
        self.term
            .resize(cols.max(1), rows.max(1), CELL_WIDTH_PX, CELL_HEIGHT_PX)
            .map_err(err("resize"))
    }

    /// Writes bytes.
    pub fn write(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// Writes bytes until the stream returns to ground or an OSC starts,
    /// whichever comes first. Call it only when the stream is not at ground
    /// (a ground stop at ground consumes nothing). `None` means every byte
    /// was consumed without reaching either.
    pub fn write_sequence(&mut self, bytes: &[u8]) -> Result<Option<SequenceStep>, VtError> {
        let stop = self
            .term
            .vt_write_until(bytes, Boundary::GROUND | Boundary::OSC_START)
            .map_err(err("vt_write_until"))?;
        Ok(stop.map(|stop| SequenceStep {
            consumed: stop.consumed,
            ground: stop.reached.contains(Boundary::GROUND),
            // An ESC ] stop consumed the ']': the ESC is one byte before it.
            osc_start_back: stop.reached.contains(Boundary::OSC_START).then_some(
                match stop.introducer {
                    Some(ghostty::terminal::StringIntroducer::Esc) => 2,
                    _ => 1,
                },
            ),
        }))
    }

    /// Whether the parser is between sequences and characters.
    pub fn at_ground(&self) -> bool {
        self.term.is_vt_ground().unwrap_or(true)
    }

    /// Whether the alternate screen is active.
    pub fn on_alternate_screen(&self) -> bool {
        matches!(self.term.active_screen(), Ok(Screen::Alternate))
    }

    /// Cursor column and row in the active area.
    pub fn cursor(&self) -> (u16, u16) {
        (
            self.term.cursor_x().unwrap_or(0),
            self.term.cursor_y().unwrap_or(0),
        )
    }

    /// The title set by OSC 0/2, or empty.
    pub fn title(&self) -> String {
        self.term
            .title()
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .unwrap_or_default()
    }

    /// Drains the signals reported since the last call, in order.
    pub fn take_signals(&self) -> Vec<Signal> {
        std::mem::take(&mut *self.signals.borrow_mut())
    }

    /// Pins the cursor's cell.
    pub fn pin_cursor(&self) -> Option<Pin> {
        let (x, y) = self.cursor();
        self.term
            .track_grid_ref(Point::Active(Coordinate { x, y: y.into() }))
            .ok()
            .map(Pin)
    }

    /// The pinned cell's row in screen coordinates (scrollback included).
    fn screen_row(&self, pin: &Pin) -> Option<u32> {
        let grid_ref = pin.0.snapshot(&self.term).ok()??;
        self.term
            .point_from_grid_ref(grid_ref, Space::Screen)
            .ok()?
            .map(|c| c.y)
    }

    /// The cursor's row in screen coordinates.
    fn cursor_screen_row(&self) -> Option<u32> {
        let (x, y) = self.cursor();
        let grid_ref = self
            .term
            .grid_ref(Point::Active(Coordinate { x, y: y.into() }))
            .ok()?;
        self.term
            .point_from_grid_ref(grid_ref, Space::Screen)
            .ok()?
            .map(|c| c.y)
    }

    /// Plain text of whole rows, from the pinned cell's row through the
    /// cursor's row: soft wraps joined, trailing blanks trimmed.
    pub fn rows_text(&self, from: &Pin) -> String {
        let (Some(first), Some(last)) = (self.screen_row(from), self.cursor_screen_row()) else {
            return String::new();
        };
        if last < first {
            return String::new();
        }
        let cols = self.size().0;
        self.screen_text_between((0, first), (cols.saturating_sub(1), last))
    }

    /// Plain text of a command's output, soft wraps joined and trailing blanks
    /// trimmed.
    ///
    /// With `semantic` (the shell marked its commands with OSC 133, so cells
    /// are tagged prompt, input or output), this is libghostty-vt's output
    /// selection at the pinned cell, where the output started. Otherwise, or
    /// when that finds nothing, it is the text from the pinned cell to the
    /// cell before the cursor.
    pub fn output_text(&self, from: &Pin, semantic: bool) -> String {
        if self.on_alternate_screen() {
            return String::new();
        }
        if semantic
            && let Some(start) = from.0.snapshot(&self.term).ok().flatten()
            && let Ok(Some(selection)) = self.term.select_output(start)
        {
            return self.format(Some(selection));
        }
        let Some(start_row) = self.screen_row(from) else {
            return String::new();
        };
        let Some(start) = from.0.snapshot(&self.term).ok().flatten() else {
            return String::new();
        };
        let start_col = self
            .term
            .point_from_grid_ref(start, Space::Screen)
            .ok()
            .flatten()
            .map_or(0, |c| c.x);
        let Some(cursor_row) = self.cursor_screen_row() else {
            return String::new();
        };
        let (cx, _) = self.cursor();
        let cols = self.size().0;
        // The last cell written is the one before the cursor.
        let end = if cx > 0 {
            (cx - 1, cursor_row)
        } else if cursor_row > 0 {
            (cols.saturating_sub(1), cursor_row - 1)
        } else {
            return String::new();
        };
        if (end.1, end.0) < (start_row, start_col) {
            return String::new();
        }
        self.screen_text_between((start_col, start_row), end)
    }

    /// Plain text between two screen coordinates (inclusive).
    fn screen_text_between(&self, start: (u16, u32), end: (u16, u32)) -> String {
        let Ok(a) = self.term.grid_ref(Point::Screen(Coordinate {
            x: start.0,
            y: start.1,
        })) else {
            return String::new();
        };
        let Ok(b) = self
            .term
            .grid_ref(Point::Screen(Coordinate { x: end.0, y: end.1 }))
        else {
            return String::new();
        };
        self.format(Some(Selection::new(&self.term, a, b, false)))
    }

    /// Plain text of everything: scrollback and the active screen.
    pub fn screen_text(&self) -> String {
        let all = self.term.select_all().ok().flatten();
        self.format(all)
    }

    /// Everything on the screen and in scrollback as VT sequences, with
    /// every extra the formatter knows (palette, modes, scrolling region,
    /// tab stops, working directory, keyboard modes, cursor, style,
    /// hyperlink, protection, Kitty keyboard flags, character sets). Two
    /// terminals that print the same here look and behave the same.
    pub fn vt_dump(&self) -> Vec<u8> {
        self.vt(self.term.select_all().ok().flatten())
    }

    fn vt(&self, selection: Option<Selection<'_>>) -> Vec<u8> {
        let Ok(mut formatter) = Formatter::new(&self.term) else {
            return Vec::new();
        };
        let configured = formatter
            .set_format(Format::Vt)
            .and_then(|f| f.set_unwrap(false))
            .and_then(|f| f.set_trim(false))
            .and_then(|f| f.set_extras(Extras::ALL))
            .and_then(|f| f.set_selection(selection));
        if configured.is_err() {
            return Vec::new();
        }
        formatter
            .format_alloc(None)
            .map(|bytes| bytes.to_vec())
            .unwrap_or_default()
    }

    fn format(&self, selection: Option<Selection<'_>>) -> String {
        let Ok(mut formatter) = Formatter::new(&self.term) else {
            return String::new();
        };
        let configured = formatter
            .set_format(Format::Plain)
            .and_then(|f| f.set_unwrap(true))
            .and_then(|f| f.set_trim(true))
            .and_then(|f| f.set_selection(selection));
        if configured.is_err() {
            return String::new();
        }
        // Trailing blank lines (and the line feed a selection may end with)
        // carry nothing.
        formatter
            .format_alloc(None)
            .map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_string())
            .unwrap_or_default()
    }

    /// Rows of the primary or alternate screen, whichever is active,
    /// scrollback included.
    pub fn total_rows(&self) -> usize {
        self.term.total_rows().unwrap_or(0)
    }

    /// Drops the oldest pages of history until the terminal is within its
    /// scrollback limit, as libghostty-vt does when the screen next grows a
    /// page. See [`Terminal::within_scrollback_limit`].
    pub fn enforce_scrollback_limit(&mut self) {
        if let Ok(Some(limit)) = self.term.scrollback_max_bytes() {
            // Setting the limit again enforces it and changes nothing else.
            let _ = self.term.set_scrollback_max_bytes(Some(limit));
        }
    }

    /// Whether the primary screen's pages fit its scrollback limit.
    ///
    /// libghostty-vt enforces the byte limit lazily: a column resize reflows
    /// history into new pages, and a page that outgrows its pool item moves
    /// to a larger allocation, without dropping old pages; the next page
    /// the screen grows into drops as many as it takes to get back under the
    /// limit. A snapshot holds every page, but the decoder refuses each page
    /// of history that would put the restored screen over the limit, so a
    /// terminal restored while over it comes back with fewer rows. Within
    /// it, a restore brings back every page, unless the restored pages take
    /// more room than the originals: the decoder rebuilds a page's
    /// allocation from size hints, and a page with a large hyperlink table
    /// can come back larger.
    ///
    /// The primary screen's resident page bytes are exactly the page
    /// memory the limit counts while no page is compressed, which this
    /// module never asks for. The alternate screen keeps no history.
    pub fn within_scrollback_limit(&self) -> bool {
        let Ok(Some(limit)) = self.term.scrollback_max_bytes() else {
            return true;
        };
        self.term
            .memory_usage()
            .is_ok_and(|usage| usage.primary.resident_bytes <= limit as u64)
    }

    /// Pages in the primary screen, history included.
    pub fn primary_pages(&self) -> u64 {
        self.term
            .memory_usage()
            .map_or(0, |usage| usage.primary.pages)
    }

    /// Rows of the oldest line on the screen (scrollback included): the top
    /// row and the rows it soft-wraps onto.
    pub fn oldest_line_rows(&self) -> usize {
        let total = self.total_rows();
        let wrapped = |y: usize| {
            u32::try_from(y).ok().is_some_and(|y| {
                self.term
                    .grid_ref(Point::Screen(Coordinate { x: 0, y }))
                    .and_then(|r| r.row())
                    .and_then(|row| row.is_wrapped())
                    .unwrap_or(false)
            })
        };
        (0..total)
            .position(|y| !wrapped(y))
            .map_or(total, |y| y + 1)
    }

    /// Selection of the newest `rows` rows of the screen, scrollback
    /// included.
    fn newest(&self, rows: usize) -> Option<Selection<'_>> {
        let total = self.total_rows();
        let rows = rows.min(total);
        if rows == 0 {
            return None;
        }
        let cols = self.size().0;
        let first = u32::try_from(total - rows).ok()?;
        let last = u32::try_from(total - 1).ok()?;
        let a = self
            .term
            .grid_ref(Point::Screen(Coordinate { x: 0, y: first }))
            .ok()?;
        let b = self
            .term
            .grid_ref(Point::Screen(Coordinate {
                x: cols.saturating_sub(1),
                y: last,
            }))
            .ok()?;
        Some(Selection::new(&self.term, a, b, false))
    }

    /// The newest `rows` rows of the screen (scrollback included) as VT
    /// sequences with every extra, like [`Terminal::vt_dump`].
    pub fn vt_dump_newest(&self, rows: usize) -> Vec<u8> {
        self.vt(self.newest(rows))
    }

    /// Plain text of the newest `rows` rows of the screen, scrollback
    /// included, like [`Terminal::screen_text`].
    pub fn text_newest(&self, rows: usize) -> String {
        self.format(self.newest(rows))
    }

    /// Encodes the complete terminal state. The stream must be at ground.
    pub fn snapshot(&self) -> Result<Vec<u8>, VtError> {
        let bytes = self
            .term
            .encode_snapshot_alloc(None)
            .map_err(err("encode snapshot"))?;
        Ok(bytes.map(|b| b.to_vec()).unwrap_or_default())
    }
}
