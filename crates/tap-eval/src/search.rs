//! Finding text in a pane's output, and the bytes that drew it.
//!
//! Text is found two ways:
//!
//! - **In the bytes**: every chain of printable runs (runs joined by SGR
//!   only, see [`tap_index::runs`]) is searched for the text. A match is
//!   plain text, so a [`crate::replace`] rewrite can apply to its bytes.
//! - **On the screen**: the pane is replayed and libghostty-vt's search
//!   runs over the screen and its scrollback at the moments its content is
//!   about to go (before each clear, reset, screen switch and resize, at
//!   the end of each top-level segment, and at the end of the pane). This
//!   finds text drawn in pieces (a full-screen program addressing the
//!   cursor). Each match's first cell is traced back to the byte that drew
//!   it through the runs index, which is where a mask is anchored. A match
//!   whose first byte is a plain match's is the same text and is dropped.
//!
//! Text drawn and erased between those moments, or whose cells moved since
//! they were drawn (see layout epochs in [`tap_index::runs`]), is found in
//! the bytes if it is plain, and otherwise reported without an anchor.

use std::ops::Range;

use serde::Serialize;
use tap_index::stream::ItemKind;
use tap_index::vt::{Scrollback, codepoint_width};
use tap_index::{MarkKind, PaneIndex, Position, Replay};

use crate::{EvalError, byte_position};

/// One place the text was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    /// The stream bytes drawing it, when it is plain text (rewritable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain: Option<Range<u64>>,
    /// The stream byte that drew its first cell, when it could be traced:
    /// where a mask is anchored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<u64>,
    /// The stream bytes of the characters that drew it, merged where they
    /// are contiguous, when every cell could be traced: for plain text the
    /// one range, for text drawn in pieces (a full-screen program
    /// addressing the cursor between them) one range per piece.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pieces: Option<Vec<Range<u64>>>,
    /// Cells it takes.
    pub cells: u32,
    /// Source time it was drawn (plain) or seen (on the screen).
    pub at_us: u64,
    /// On the alternate screen.
    pub alternate: bool,
}

/// Every occurrence of `needle` in a pane (raw or edited); offsets are the
/// index's stream offsets.
pub fn find_text(pane: &PaneIndex, needle: &str) -> Result<Vec<Occurrence>, EvalError> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let stream = pane.stream();
    let runs = pane.runs()?;
    let cells: u32 = needle.chars().map(|c| u32::from(codepoint_width(c))).sum();
    let mut found = Vec::new();

    for chain in runs.chains(&stream.bytes) {
        let valid = chain.runs.clone().all(|r| {
            let r = &runs.runs[r];
            std::str::from_utf8(&stream.bytes[r.bytes.start as usize..r.bytes.end as usize]).is_ok()
        });
        if !valid {
            continue;
        }
        let byte_at = |t: usize| {
            let k = chain.text_starts.partition_point(|&s| s <= t) - 1;
            let run = &runs.runs[chain.runs.start + k];
            (run, run.bytes.start + (t - chain.text_starts[k]) as u64)
        };
        let last_len = needle.chars().last().map_or(1, char::len_utf8);
        for (t, _) in chain.text.match_indices(needle) {
            let (run, start) = byte_at(t);
            let (_, last) = byte_at(t + needle.len() - last_len);
            let bytes = start..last + last_len as u64;
            found.push(Occurrence {
                plain: Some(bytes.clone()),
                pieces: Some(Vec::from([bytes])),
                anchor: Some(start),
                cells,
                at_us: run.at_us,
                alternate: run.alternate,
            });
        }
    }

    // Moments the screen is searched.
    let mut stops: Vec<Position> = pane
        .marks
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                MarkKind::AltScreenEnter
                    | MarkKind::AltScreenExit
                    | MarkKind::Clear { .. }
                    | MarkKind::FullReset
            )
        })
        .map(|m| byte_position(stream, m.byte))
        .collect();
    stops.extend(
        pane.segments
            .iter()
            .filter(|s| s.end_byte < stream.len())
            .map(|s| byte_position(stream, s.end_byte)),
    );
    stops.extend(
        stream
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i.kind, ItemKind::Resize { .. }))
            .map(|(item, _)| Position { item, intra: 0 }),
    );
    stops.push(Position {
        item: stream.items.len(),
        intra: 0,
    });
    stops.sort();
    stops.dedup();

    let mut replay = Replay::start(stream, Scrollback::Unlimited)?;
    let mut seen: Vec<(Option<u64>, u32, (u16, u32))> = Vec::new();
    for stop in stops {
        replay.run_to_position(stop)?;
        let position = replay.position();
        let before = replay.byte();
        let at_us = replay.at_us();
        let alternate = replay.term.on_alternate_screen();
        let epoch = runs.epoch_at(position);
        let shift = runs.shift_at(position);
        for m in replay.term.find(needle.as_bytes()) {
            if replay.term.text_between(m.start, m.end) != needle {
                continue;
            }
            let anchor = runs
                .cell_source(
                    &stream.bytes,
                    before,
                    epoch,
                    alternate,
                    (m.start.0, m.start.1 + shift),
                )
                .map(|s| s.bytes.start);
            if anchor.is_some() && found.iter().any(|o| o.anchor == anchor) {
                continue;
            }
            let key = (anchor, epoch, m.start);
            if seen
                .iter()
                .any(|s| (anchor.is_some() && s.0 == anchor) || (anchor.is_none() && *s == key))
            {
                continue;
            }
            seen.push(key);
            // Trace every cell. The second cell of a wide character is not
            // its own source.
            let cols = replay.term.size().0;
            let mut pieces: Option<Vec<Range<u64>>> = Some(Vec::new());
            let (mut cell, mut skip) = (m.start, 0u8);
            loop {
                if skip > 0 {
                    skip -= 1;
                } else {
                    let source = runs.cell_source(
                        &stream.bytes,
                        before,
                        epoch,
                        alternate,
                        (cell.0, cell.1 + shift),
                    );
                    match (source, pieces.as_mut()) {
                        (Some(source), Some(list)) => {
                            let wide = std::str::from_utf8(
                                &stream.bytes
                                    [source.bytes.start as usize..source.bytes.end as usize],
                            )
                            .ok()
                            .and_then(|c| c.chars().next())
                            .is_some_and(|c| codepoint_width(c) == 2);
                            skip = u8::from(wide);
                            match list.last_mut() {
                                Some(last) if last.end == source.bytes.start => {
                                    last.end = source.bytes.end;
                                }
                                _ => list.push(source.bytes),
                            }
                        }
                        _ => pieces = None,
                    }
                }
                if cell == m.end || pieces.is_none() {
                    break;
                }
                cell = if cell.0 + 1 >= cols {
                    (0, cell.1 + 1)
                } else {
                    (cell.0 + 1, cell.1)
                };
            }
            found.push(Occurrence {
                plain: None,
                pieces,
                anchor,
                cells,
                at_us,
                alternate,
            });
        }
    }
    Ok(found)
}
