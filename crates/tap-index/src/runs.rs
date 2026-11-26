//! Printable runs: which stream bytes drew which cells.
//!
//! A *run* is a maximal stretch of printable bytes written with the parser
//! at ground: no control character and no escape sequence inside it (it may
//! span events, since event boundaries are not in the bytes). Folding the
//! stream records, for each run, its byte range and the cell its first
//! character was drawn in, in screen coordinates (scrollback included), so
//! a cell found on the screen at some moment can be traced back to the
//! bytes that drew it ([`Runs::cell_source`]), and so text in output can be
//! found by its bytes ([`Runs::chains`]).
//!
//! **Layout epochs.** With all scrollback kept, a cell keeps its screen
//! coordinates as the screen scrolls; they change only when cells move:
//! a resize reflows, ED 3 or a reset erases history, insert and delete
//! line or character, scroll up and down, reverse index, a scrolling
//! region that new lines scroll inside, or a switch between the primary
//! and alternate screens. Each of those starts a new epoch, and a cell is
//! traced back only to runs of the epoch it is looked up in. Text whose
//! cells have moved since it was drawn is not traced (an editor falls back
//! to masking it).
//!
//! **Chains.** Runs separated only by SGR sequences draw contiguous text
//! with a style change inside it (`key=sk-live-\e[31m4f9a2b`), so each run
//! records whether only SGR sequences separate it from the run before.

use std::ops::Range;

use serde::Serialize;

use crate::replay::Position;
use crate::stream::{ItemKind, PaneStream};
use crate::vt::{self, VtError};

const ESC: u8 = 0x1b;
/// Longest sequence kept for classification. Longer ones are never SGR in
/// practice and are classified as "something else".
const KEEP: usize = 64;

/// One printable run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Its stream bytes.
    pub bytes: Range<u64>,
    /// Index of the event holding its first byte.
    pub event: usize,
    /// Source time of that event.
    pub at_us: u64,
    /// The cell its first character was drawn in: column, and row in screen
    /// coordinates.
    pub start: (u16, u32),
    /// The cursor after it, in screen coordinates.
    pub end: (u16, u32),
    /// Whether a wrap was pending after it (its last character is in the
    /// last column).
    pub end_pending_wrap: bool,
    /// Columns of the screen it was drawn on.
    pub cols: u16,
    /// Drawn on the alternate screen.
    pub alternate: bool,
    /// Layout epoch it was drawn in.
    pub epoch: u32,
    /// Only SGR sequences (or nothing) separate it from the previous run,
    /// which drew the text just before it.
    pub joins_previous: bool,
}

/// A chain of runs drawing contiguous text: runs joined by SGR sequences
/// only. `text` is their characters; `pieces` maps byte offsets of `text`
/// to stream bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// Indices of its runs in [`Runs::runs`].
    pub runs: Range<usize>,
    /// The characters the runs print, decoded as UTF-8 (lossily).
    pub text: String,
    /// For each run, the offset of its text in `text`.
    pub text_starts: Vec<usize>,
}

/// The runs of a pane's stream.
#[derive(Debug, Clone, Default)]
pub struct Runs {
    /// Every run, in stream order.
    pub runs: Vec<Run>,
    /// Where each layout epoch after the first starts.
    pub epochs: Vec<Position>,
}

/// A cell, in screen coordinates: column and row (scrollback included).
pub type Cell = (u16, u32);

/// A character of a run: its stream bytes and the cell it was drawn in.
pub type DrawnChar = (Range<u64>, Cell);

/// Where a cell's character came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSource {
    /// Index of the run in [`Runs::runs`].
    pub run: usize,
    /// The stream bytes of the character drawn in the cell.
    pub bytes: Range<u64>,
}

/// Records every run of a pane. The terminal keeps all scrollback, so cells
/// keep their coordinates for as long as their epoch lasts.
pub fn fold(stream: &PaneStream) -> Result<Runs, VtError> {
    let (cols, rows) = stream.initial_size;
    let mut folder = Folder {
        term: vt::Terminal::new(cols, rows, vt::Scrollback::Unlimited)?,
        runs: Runs::default(),
        epoch: 0,
        sgr_only: false,
        last_end: None,
        seq: Vec::new(),
        region: false,
        item: 0,
        item_offset: 0,
        at_us: 0,
    };
    for (index, item) in stream.items.iter().enumerate() {
        match &item.kind {
            ItemKind::Output { len } => {
                folder.item = index;
                folder.item_offset = item.offset;
                folder.at_us = item.at_us;
                let start = item.offset as usize;
                folder.feed(&stream.bytes[start..start + *len as usize], item.offset)?;
            }
            ItemKind::Resize { cols, rows } => {
                folder.term.resize(*cols, *rows)?;
                folder.new_epoch(Position {
                    item: index + 1,
                    intra: 0,
                });
            }
            ItemKind::Input { .. } | ItemKind::Other => {}
        }
    }
    Ok(folder.runs)
}

struct Folder {
    term: vt::Terminal,
    runs: Runs,
    epoch: u32,
    /// Only SGR sequences since the last run ended.
    sgr_only: bool,
    /// Where the last run ended, if nothing has been written since.
    last_end: Option<u64>,
    /// The sequence being parsed, up to [`KEEP`] bytes.
    seq: Vec<u8>,
    /// A scrolling region may be set, so line feeds may scroll inside it.
    region: bool,
    item: usize,
    item_offset: u64,
    at_us: u64,
}

impl Folder {
    fn new_epoch(&mut self, at: Position) {
        self.epoch += 1;
        self.runs.epochs.push(at);
        self.sgr_only = false;
        self.last_end = None;
    }

    fn position(&self, byte: u64) -> Position {
        Position {
            item: self.item,
            intra: byte - self.item_offset,
        }
    }

    fn feed(&mut self, data: &[u8], base: u64) -> Result<(), VtError> {
        let mut i = 0;
        while i < data.len() {
            if self.term.at_ground() {
                let k = data[i..]
                    .iter()
                    .position(|&b| b < 0x20 || b == 0x7f)
                    .unwrap_or(data.len() - i);
                if k > 0 {
                    self.print(&data[i..i + k], base + i as u64);
                    i += k;
                    continue;
                }
                let b = data[i];
                self.term.write(&data[i..=i]);
                i += 1;
                self.last_end = None;
                if b == ESC {
                    self.seq.clear();
                    self.seq.push(ESC);
                } else {
                    self.sgr_only = false;
                    if self.region && matches!(b, b'\n' | 0x0b | 0x0c) {
                        self.new_epoch(self.position(base + i as u64));
                    }
                }
                continue;
            }
            let alternate = self.term.on_alternate_screen();
            match self.term.write_sequence(&data[i..])? {
                None => {
                    self.keep(&data[i..]);
                    i = data.len();
                }
                Some(step) => {
                    self.keep(&data[i..i + step.consumed]);
                    i += step.consumed;
                    if step.ground {
                        self.end_sequence(alternate, self.position(base + i as u64));
                    }
                }
            }
            self.term.take_signals();
        }
        self.term.take_signals();
        Ok(())
    }

    fn keep(&mut self, bytes: &[u8]) {
        let room = KEEP.saturating_sub(self.seq.len());
        self.seq.extend_from_slice(&bytes[..room.min(bytes.len())]);
    }

    fn end_sequence(&mut self, was_alternate: bool, at: Position) {
        let class = classify(&self.seq);
        if class != Class::Sgr {
            self.sgr_only = false;
        }
        match class {
            Class::Moves => self.new_epoch(at),
            Class::Region(set) => self.region = set,
            Class::LineFeed if self.region => self.new_epoch(at),
            _ => {}
        }
        if self.term.on_alternate_screen() != was_alternate {
            self.new_epoch(at);
        }
    }

    fn print(&mut self, bytes: &[u8], at: u64) {
        let (x, y) = self.term.cursor_on_screen();
        let pending = self.term.cursor_pending_wrap();
        self.term.write(bytes);
        let end = self.term.cursor_on_screen();
        let end_pending_wrap = self.term.cursor_pending_wrap();
        let cols = self.term.size().0;
        if let Some(run) = self.runs.runs.last_mut()
            && self.last_end == Some(at)
        {
            run.bytes.end = at + bytes.len() as u64;
            run.end = end;
            run.end_pending_wrap = end_pending_wrap;
        } else {
            // A pending wrap puts the first character at the start of the
            // next row.
            let start = if pending { (0, y + 1) } else { (x, y) };
            self.runs.runs.push(Run {
                bytes: at..at + bytes.len() as u64,
                event: self.item,
                at_us: self.at_us,
                start,
                end,
                end_pending_wrap,
                cols,
                alternate: self.term.on_alternate_screen(),
                epoch: self.epoch,
                joins_previous: self.sgr_only,
            });
        }
        self.last_end = Some(at + bytes.len() as u64);
        self.sgr_only = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Select graphic rendition: changes no cell.
    Sgr,
    /// Moves cells: insert or delete lines or characters, scroll, reverse
    /// index, erase scrollback, reset.
    Moves,
    /// DECSTBM: `true` when it sets a region (which may not be the whole
    /// screen), `false` when it resets to the whole screen.
    Region(bool),
    /// IND or NEL: a line feed.
    LineFeed,
    Other,
}

fn classify(seq: &[u8]) -> Class {
    match seq {
        [ESC, b'c'] | [ESC, b'M'] => Class::Moves,
        [ESC, b'D'] | [ESC, b'E'] => Class::LineFeed,
        [ESC, b'[', rest @ ..] => {
            let Some((&last, body)) = rest.split_last() else {
                return Class::Other;
            };
            let plain = body
                .iter()
                .all(|b| b.is_ascii_digit() || matches!(b, b';' | b':'));
            match last {
                b'm' if plain => Class::Sgr,
                b'L' | b'M' | b'S' | b'T' | b'@' | b'P' if plain => Class::Moves,
                b'J' if body == b"3" => Class::Moves,
                b'r' if plain => Class::Region(!body.is_empty()),
                _ => Class::Other,
            }
        }
        _ => Class::Other,
    }
}

/// The cells a character of display width `w` occupies when printed at
/// `pos` on a screen `cols` wide, and where the next character goes.
fn advance(pos: (u16, u32), w: u16, cols: u16) -> ((u16, u32), (u16, u32)) {
    let mut at = pos;
    if at.0 >= cols || (w > 0 && at.0 + w > cols) {
        at = (0, at.1 + 1);
    }
    (at, (at.0 + w, at.1))
}

impl Runs {
    /// The layout epoch in effect at `position`.
    pub fn epoch_at(&self, position: Position) -> u32 {
        self.epochs.partition_point(|&p| p <= position) as u32
    }

    /// Each character of a run, with its bytes and the cell it was drawn
    /// in. Zero-width characters share the cell before them. `None` when
    /// the run is not valid UTF-8.
    pub fn cells(&self, stream: &[u8], run: usize) -> Option<Vec<DrawnChar>> {
        let r = &self.runs[run];
        let text =
            std::str::from_utf8(&stream[r.bytes.start as usize..r.bytes.end as usize]).ok()?;
        let mut pos = r.start;
        let mut last = r.start;
        let mut out = Vec::new();
        for (offset, c) in text.char_indices() {
            let w = u16::from(vt::codepoint_width(c));
            let bytes =
                r.bytes.start + offset as u64..r.bytes.start + (offset + c.len_utf8()) as u64;
            if w == 0 {
                out.push((bytes, last));
                continue;
            }
            let (cell, next) = advance(pos, w, r.cols);
            out.push((bytes, cell));
            last = cell;
            pos = next;
        }
        Some(out)
    }

    /// The run that last drew `cell` (screen coordinates) before stream
    /// byte `before`, in epoch `epoch` on the given screen, and the bytes
    /// of the character in it. `None` when no run of that epoch drew it.
    pub fn cell_source(
        &self,
        stream: &[u8],
        before: u64,
        epoch: u32,
        alternate: bool,
        cell: (u16, u32),
    ) -> Option<CellSource> {
        let last = self.runs.partition_point(|r| r.bytes.end <= before);
        for index in (0..last).rev() {
            let r = &self.runs[index];
            if r.epoch != epoch || r.alternate != alternate {
                if r.epoch < epoch {
                    break;
                }
                continue;
            }
            let lin = |(x, y): (u16, u32)| u64::from(y) * u64::from(r.cols) + u64::from(x);
            let end = lin(r.end) + u64::from(r.end_pending_wrap);
            let (first, at) = (lin(r.start), lin(cell));
            if at < first || at >= end.max(first + 1) {
                continue;
            }
            let cells = self.cells(stream, index)?;
            let found = cells.iter().rev().find(|(_, c)| *c == cell)?;
            return Some(CellSource {
                run: index,
                bytes: found.0.clone(),
            });
        }
        None
    }

    /// The runs grouped into chains of contiguous text.
    pub fn chains(&self, stream: &[u8]) -> Vec<Chain> {
        let mut chains: Vec<Chain> = Vec::new();
        for (index, run) in self.runs.iter().enumerate() {
            let text =
                String::from_utf8_lossy(&stream[run.bytes.start as usize..run.bytes.end as usize]);
            match chains.last_mut() {
                Some(chain) if run.joins_previous && chain.runs.end == index => {
                    chain.text_starts.push(chain.text.len());
                    chain.text.push_str(&text);
                    chain.runs.end = index + 1;
                }
                _ => chains.push(Chain {
                    runs: index..index + 1,
                    text: text.into_owned(),
                    text_starts: vec![0],
                }),
            }
        }
        chains
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
        PaneStream::new(PaneId::new("p").unwrap(), (10, 3), &events, 10_000)
    }

    #[test]
    fn runs_span_events_and_chains_span_sgr() {
        let s = stream(&[b"ab", b"c\x1b[1md\x1b[0me\r\nf\x1b[Ag"]);
        let runs = fold(&s).unwrap();
        let shape: Vec<_> = runs
            .runs
            .iter()
            .map(|r| (r.bytes.clone(), r.start, r.joins_previous))
            .collect();
        assert_eq!(
            shape,
            vec![
                (0..3, (0, 0), false),
                (7..8, (3, 0), true),
                (12..13, (4, 0), true),
                (15..16, (0, 1), false),
                (19..20, (1, 0), false),
            ]
        );
        let chains = runs.chains(&s.bytes);
        assert_eq!(chains[0].text, "abcde");
        assert_eq!(chains[0].text_starts, vec![0, 3, 4]);
        // The cell drawn last at (1, 0) is g's, not b's.
        let source = runs
            .cell_source(&s.bytes, s.len(), 0, false, (1, 0))
            .unwrap();
        assert_eq!(source.bytes, 19..20);
        let source = runs.cell_source(&s.bytes, 19, 0, false, (1, 0)).unwrap();
        assert_eq!(source.bytes, 1..2);
    }

    #[test]
    fn wide_characters_and_wraps_map_to_their_cells() {
        let s = stream(&["abcdefghi日x".as_bytes()]);
        let runs = fold(&s).unwrap();
        assert_eq!(runs.runs.len(), 1);
        let cells = runs.cells(&s.bytes, 0).unwrap();
        // 日 does not fit in the last column, so it wraps.
        assert_eq!(cells[9], (9..12, (0, 1)));
        assert_eq!(cells[10], (12..13, (2, 1)));
        assert_eq!(
            runs.cell_source(&s.bytes, s.len(), 0, false, (2, 1))
                .unwrap()
                .bytes,
            12..13
        );
    }

    #[test]
    fn moving_cells_starts_an_epoch() {
        let s = stream(&[b"ab\x1b[Lcd\x1b[?1049hx"]);
        let runs = fold(&s).unwrap();
        let epochs: Vec<_> = runs.runs.iter().map(|r| (r.epoch, r.alternate)).collect();
        assert_eq!(epochs, vec![(0, false), (1, false), (2, true)]);
    }
}
