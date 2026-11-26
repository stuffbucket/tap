//! ReplaceText: rewrite text in output, in place.
//!
//! A replacement rewrites only printable bytes, so it is exact only when
//! the text was drawn as plain text: the replaced range is a run of
//! printable characters written at ground, or several runs separated only
//! by SGR sequences (which are kept, so colours survive), and the
//! replacement takes exactly the cells the original did. [`plan`] checks
//! that from the runs index and splits the replacement so each original
//! character is replaced by characters of the same total width (or, failing
//! that, each SGR-delimited piece by text of the same width), so text typed
//! one key at a time is still rewritten one key at a time. The evaluator
//! then refolds the edited program and checks that the cells the range
//! drew read `expect`.
//!
//! Anything else is refused with [`NeedsMask`]: the text has to be covered
//! at render time instead.

use std::ops::Range;

use serde::Serialize;
use tap_index::Runs;
use tap_index::vt::codepoint_width;

/// Why a replacement cannot be a byte rewrite, so the text needs a mask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum NeedsMask {
    /// This byte of the range is not printable text written at ground (it
    /// is inside an escape sequence or a control character, or the range
    /// cuts a character in two).
    NotPlainText {
        /// The byte.
        byte: u64,
    },
    /// The text is drawn in pieces separated by something other than SGR,
    /// such as cursor movement: cursor-addressed text, as full-screen
    /// programs draw it.
    CursorAddressed {
        /// Where the next piece starts.
        byte: u64,
    },
    /// The range crosses an output event whose recorded bytes differ from
    /// the bytes it plays (a Kitty file transfer normalised at capture).
    PlaybackDiffers {
        /// Index of the event in the pane.
        event: usize,
    },
    /// The replacement contains control characters.
    NotPrintable,
    /// The replacement does not take the same cells as the original.
    WidthMismatch {
        /// Cells the original takes.
        original: u32,
        /// Cells the replacement takes.
        replacement: u32,
    },
    /// After the rewrite, the cells read something other than `expect`.
    ExpectMismatch {
        /// What they read.
        found: String,
    },
}

/// Source bytes and the bytes played in their place.
pub type Rewrite = (Range<u64>, Vec<u8>);

/// A character of the original: its bytes and width, and which run piece
/// it belongs to.
struct Original {
    bytes: Range<u64>,
    width: u32,
    piece: usize,
}

/// Takes replacement characters from `repl[*j..]` until they fill `width`
/// cells, plus any zero-width characters after them. `None` when they
/// overshoot or run out.
fn take(repl: &[(char, u32)], j: &mut usize, width: u32) -> Option<String> {
    let mut out = String::new();
    let mut acc = 0;
    while acc < width {
        let &(c, w) = repl.get(*j)?;
        acc += w;
        out.push(c);
        *j += 1;
    }
    while let Some(&(c, 0)) = repl.get(*j) {
        out.push(c);
        *j += 1;
    }
    (acc == width).then_some(out)
}

/// The splices that rewrite source bytes `range` of `stream` as `text`:
/// one per original character where widths allow, else one per
/// SGR-delimited piece.
pub fn plan(
    runs: &Runs,
    stream: &[u8],
    range: &Range<u64>,
    text: &str,
) -> Result<Vec<Rewrite>, NeedsMask> {
    let containing = |byte: u64| {
        let i = runs.runs.partition_point(|r| r.bytes.end <= byte);
        runs.runs
            .get(i)
            .filter(|r| r.bytes.start <= byte)
            .map(|_| i)
    };
    let first = containing(range.start).ok_or(NeedsMask::NotPlainText { byte: range.start })?;
    let last = containing(range.end - 1).ok_or(NeedsMask::NotPlainText {
        byte: range.end - 1,
    })?;
    if let Some(r) = runs.runs[first + 1..=last]
        .iter()
        .find(|r| !r.joins_previous)
    {
        return Err(NeedsMask::CursorAddressed {
            byte: r.bytes.start,
        });
    }

    let mut original = Vec::new();
    for (piece, run) in runs.runs[first..=last].iter().enumerate() {
        let bytes = &stream[run.bytes.start as usize..run.bytes.end as usize];
        let decoded = std::str::from_utf8(bytes).map_err(|_| NeedsMask::NotPlainText {
            byte: run.bytes.start,
        })?;
        let from = range.start.max(run.bytes.start);
        let to = range.end.min(run.bytes.end);
        let (lo, hi) = (
            (from - run.bytes.start) as usize,
            (to - run.bytes.start) as usize,
        );
        if !decoded.is_char_boundary(lo) {
            return Err(NeedsMask::NotPlainText { byte: from });
        }
        if !decoded.is_char_boundary(hi) {
            return Err(NeedsMask::NotPlainText { byte: to });
        }
        for (offset, c) in decoded[lo..hi].char_indices() {
            let start = from + offset as u64;
            original.push(Original {
                bytes: start..start + c.len_utf8() as u64,
                width: u32::from(codepoint_width(c)),
                piece,
            });
        }
    }

    if text.chars().any(|c| c.is_control()) {
        return Err(NeedsMask::NotPrintable);
    }
    let repl: Vec<(char, u32)> = text
        .chars()
        .map(|c| (c, u32::from(codepoint_width(c))))
        .collect();
    let total = |w: &mut dyn Iterator<Item = u32>| w.sum::<u32>();
    let mismatch = || NeedsMask::WidthMismatch {
        original: total(&mut original.iter().map(|o| o.width)),
        replacement: total(&mut repl.iter().map(|r| r.1)),
    };

    // One splice per original character.
    let mut j = 0;
    let per_char: Option<Vec<_>> = original
        .iter()
        .map(|o| {
            if o.width == 0 {
                Some((o.bytes.clone(), Vec::new()))
            } else {
                take(&repl, &mut j, o.width).map(|s| (o.bytes.clone(), s.into_bytes()))
            }
        })
        .collect();
    if let Some(splices) = per_char
        && j == repl.len()
    {
        return Ok(splices);
    }

    // One per piece between SGR sequences.
    let mut j = 0;
    let mut splices = Vec::new();
    let pieces = last - first + 1;
    for piece in 0..pieces {
        let chars: Vec<&Original> = original.iter().filter(|o| o.piece == piece).collect();
        let (Some(a), Some(b)) = (chars.first(), chars.last()) else {
            continue;
        };
        let width = chars.iter().map(|o| o.width).sum();
        let s = take(&repl, &mut j, width).ok_or_else(mismatch)?;
        splices.push((a.bytes.start..b.bytes.end, s.into_bytes()));
    }
    if j != repl.len() {
        return Err(mismatch());
    }
    Ok(splices)
}
