//! Masks: render-time covers, resolved to the cells they cover over time.
//!
//! A mask names the cell drawn by one printable output byte (its anchor)
//! and a number of cells from it in reading order. Nothing is painted
//! here: [`resolve`] replays the edited program with the scrollback seek
//! uses, pins the anchor's cell when it is drawn, and follows the pin as
//! the screen scrolls, reflows and switches, so a renderer only has to
//! paint [`MaskSpan::rects`] during `[from_us, to_us)`. A span with no
//! rectangles means the cells are not on the active screen then (the
//! other screen is showing). The region ends when the cell is gone
//! (history erased, a reset) or at the end of the recording.

use serde::Serialize;
use tap_index::Replay;
use tap_index::stream::{ItemKind, PaneStream};
use tap_index::vt::{self, Scrollback, VtError};
use tap_model::PaneId;
use tap_model::edl::MaskStyle;

use crate::{byte_position, end_position, next_cell};

/// Cells of one row: `cols` cells from column `col` of row `row` (screen
/// coordinates, scrollback included, as seek's terminal counts them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CellRect {
    /// Row.
    pub row: u32,
    /// First column.
    pub col: u16,
    /// Number of columns.
    pub cols: u16,
}

/// Where a mask's cells are during a stretch of source time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskSpan {
    /// First source microsecond.
    pub from_us: u64,
    /// End (exclusive).
    pub to_us: u64,
    /// The cells; empty while the other screen is active.
    pub rects: Vec<CellRect>,
}

/// A mask, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskRegion {
    /// Index of the edit-list entry.
    pub entry: usize,
    /// The pane.
    pub pane: PaneId,
    /// How to cover the cells.
    pub style: MaskStyle,
    /// How many cells.
    pub cells: u32,
    /// The anchor, as a program byte.
    pub anchor: u64,
    /// Where the cells are, over time.
    pub spans: Vec<MaskSpan>,
}

/// A mask to resolve.
pub(crate) struct Wanted {
    pub entry: usize,
    /// Program byte of the anchor (the first byte of a printed character).
    pub anchor: u64,
    pub cells: u32,
    pub style: MaskStyle,
}

/// `cells` cells from `start` in reading order on a screen `cols` wide.
fn rects(start: (u16, u32), cells: u32, cols: u16) -> Vec<CellRect> {
    let mut out = Vec::new();
    let (mut col, mut row) = start;
    let mut left = cells;
    while left > 0 && cols > 0 {
        if col >= cols {
            col = 0;
            row += 1;
        }
        let n = u32::from(cols - col).min(left) as u16;
        out.push(CellRect { row, col, cols: n });
        left -= u32::from(n);
        col += n;
    }
    out
}

struct Active {
    region: MaskRegion,
    pin: vt::Pin,
    alternate: bool,
    /// The rectangles of the open span.
    current: Vec<CellRect>,
    since: u64,
}

impl Active {
    fn close(&mut self, at: u64) {
        if at > self.since || self.region.spans.is_empty() {
            self.region.spans.push(MaskSpan {
                from_us: self.since,
                to_us: at.max(self.since),
                rects: std::mem::take(&mut self.current),
            });
        }
        self.since = at;
    }
}

/// Resolves masks over a pane's edited stream. Returns each entry's region,
/// or why it covers nothing.
#[allow(clippy::type_complexity)]
pub(crate) fn resolve(
    pane: &PaneId,
    stream: &PaneStream,
    mut wanted: Vec<Wanted>,
) -> Result<Vec<(usize, Result<MaskRegion, String>)>, VtError> {
    let mut out = Vec::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    wanted.sort_by_key(|w| (w.anchor, w.entry));
    let mut replay = Replay::start(stream, Scrollback::Bytes(vt::GHOSTTY_SCROLLBACK_BYTES))?;
    let mut active: Vec<Active> = Vec::new();
    let mut next = 0;
    for (index, item) in stream.items.iter().enumerate() {
        while let Some(w) = wanted.get(next) {
            let at = byte_position(stream, w.anchor);
            if at.item != index {
                break;
            }
            next += 1;
            replay.run_to_position(at)?;
            if !replay.term.at_ground() {
                out.push((
                    w.entry,
                    Err("the anchor is inside an escape sequence".into()),
                ));
                continue;
            }
            let cell = next_cell(&replay.term);
            let char_len = stream.bytes[w.anchor as usize..]
                .first()
                .map_or(1, |&b| match b {
                    0xf0..=0xf7 => 4,
                    0xe0..=0xef => 3,
                    0xc0..=0xdf => 2,
                    _ => 1,
                });
            replay.run_to_position(end_position(stream, w.anchor + char_len))?;
            let Some(pin) = replay.term.pin(cell.0, cell.1) else {
                out.push((w.entry, Err("the anchor's cell could not be found".into())));
                continue;
            };
            let alternate = replay.term.on_alternate_screen();
            active.push(Active {
                region: MaskRegion {
                    entry: w.entry,
                    pane: pane.clone(),
                    style: w.style.clone(),
                    cells: w.cells,
                    anchor: w.anchor,
                    spans: Vec::new(),
                },
                pin,
                alternate,
                current: rects(cell, w.cells, replay.term.size().0),
                since: item.at_us,
            });
        }
        replay.run_to_item(index + 1, &mut |_| {})?;
        if !matches!(item.kind, ItemKind::Output { .. } | ItemKind::Resize { .. }) {
            continue;
        }
        let term = &replay.term;
        let mut finished = Vec::new();
        for (k, a) in active.iter_mut().enumerate() {
            let rects_now = if term.on_alternate_screen() != a.alternate {
                Vec::new()
            } else {
                match term.pin_position(&a.pin) {
                    Some(cell) => rects(cell, a.region.cells, term.size().0),
                    None => {
                        a.close(item.at_us);
                        finished.push(k);
                        continue;
                    }
                }
            };
            if rects_now != a.current {
                a.close(item.at_us);
                a.current = rects_now;
            }
        }
        for k in finished.into_iter().rev() {
            let a = active.remove(k);
            out.push((a.region.entry, Ok(a.region)));
        }
    }
    for w in &wanted[next..] {
        out.push((w.entry, Err("the anchor is not in the program".into())));
    }
    for mut a in active {
        a.close(stream.end_us);
        out.push((a.region.entry, Ok(a.region)));
    }
    Ok(out)
}
