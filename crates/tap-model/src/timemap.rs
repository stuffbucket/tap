//! The time map: source microseconds → edited microseconds, built from the
//! time-only ops of an edit list.
//!
//! # Semantics
//!
//! The map is defined as an exact piecewise-linear function `exact(s)` over
//! rationals, and `to_edited(s) = floor(exact(s))`. All arithmetic is exact
//! (`u128` rationals, reduced after every step); the only rounding is that
//! final floor, so error never accumulates, however long the session or
//! however many segments it has.
//!
//! `exact` is the integral of a *density* (edited µs per source µs) over the
//! kept source time, plus the holds:
//!
//! - **Trim** `{in, out}` keeps `[in, out]`. Several trims keep the
//!   intersection; trims that do not overlap are an error
//!   ([`EvalError::DisjointTrims`]). Below `in` the map is 0; above `out` it
//!   is [`TimeMap::edited_duration`].
//! - **Skip** `{from, to}` sets the density of `[from, to)` to 0: the range
//!   collapses to one edited instant (the *cut instant*), at which its events
//!   apply.
//! - **`CollapseIdle`** finds idle gaps — maximal stretches with no event,
//!   counting every event of every pane except `marker`, with the recording's
//!   start and end and the op's range acting as gap edges — and for each gap
//!   `[g0, g1)` longer than `threshold_us` cuts `[g0 + ⌊max/2⌋, g1 − ⌈max/2⌉)`,
//!   keeping `max_us` of source time split around the cut (the pause after
//!   the last output and the pause before the next keystroke both survive).
//!   Gaps are measured on the **source** recording, never on the result of
//!   other ops.
//! - **Speed** `{range, constant(r)}` multiplies the density of `range` by
//!   `1/r`. Overlapping speeds multiply (2× inside 2× is 4×). A speed over a
//!   cut has no effect there (0 stays 0). Ramps are not supported yet.
//! - **Hold** `{at, duration_us}` inserts `duration_us` of edited time between
//!   source microseconds `at` and `at + 1`, so the frame showing every event
//!   up to and including `at` is frozen. Holds at the same instant add up. A
//!   hold at `out` extends the end. A hold whose `at` is outside `[in, out]`
//!   or inside a cut is *inert*: it has no effect, and its index is reported
//!   by [`TimeMap::inert_ops`]. Holds are never scaled by speed.
//!
//! Disabled entries are ignored. Byte-changing and render-time ops, typing
//! retiming and speed ramps make [`TimeMap::build`] fail with
//! [`EvalError::Unsupported`], listing every such op. `tap-eval` evaluates
//! whole edit lists: it builds this map from the time-only ops plus a skip
//! for every excision, and refuses only typing retiming and speed ramps.
//!
//! ## Overlaps and order
//!
//! Every time-only op is anchored to source coordinates and contributes to
//! one of four order-independent combinations: trims intersect, cuts (skips
//! and collapsed gaps) take their union, speeds multiply, holds add. The map
//! is therefore a function of the *multiset* of enabled ops: **any two
//! supported time-only ops commute, in every combination, including when they
//! overlap.** Some consequences, all tested:
//!
//! - Skip ∘ Skip: overlapping or adjacent skips merge; a skip repeated is the
//!   same as one (union is idempotent).
//! - Skip ∘ Speed, `CollapseIdle` ∘ Speed: cut time stays cut; the kept part
//!   of a collapsed gap is still subject to the speed (so a 2× over a gap
//!   collapsed to 500 ms leaves 250 ms).
//! - Skip ∘ `CollapseIdle`, `CollapseIdle` ∘ `CollapseIdle`: union of cuts,
//!   so where two collapses overlap the shorter `max_us` wins.
//! - Trim ∘ anything: the other op is clipped to `[in, out]`.
//! - Speed ∘ Speed and Hold ∘ Hold are *not* idempotent (they compound), but
//!   they still commute.
//!
//! ## Inverse
//!
//! [`TimeMap::to_source`]`(e)` is the largest `s` in `[in, out]` with
//! `to_edited(s) <= e`: a cut instant maps to the *end* of the cut (the
//! source state on screen there), an instant inside a hold maps to the held
//! source instant, and anything at or past the end maps to `out`. Where the
//! density is at least 1 (no speed-up) `to_source(to_edited(s)) == s` for
//! every kept `s`; under a speed-up several source microseconds share one
//! edited microsecond and `to_source` returns the last of them.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use thiserror::Error as ThisError;

use crate::edl::{EditEntry, EditOp, Rate, Src, SrcRange};
use crate::event::{EventKind, RawEvent};
use crate::pane_id::PaneId;

/// An op [`TimeMap::build`] cannot evaluate yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedOp {
    /// Index of the entry in the edit list.
    pub index: usize,
    /// The op's tag.
    pub op: &'static str,
    /// Why it is not supported.
    pub reason: &'static str,
}

impl fmt::Display for UnsupportedOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {} ({})", self.index, self.op, self.reason)
    }
}

/// Why an edit list could not be turned into a time map.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum EvalError {
    /// Enabled ops this build cannot evaluate yet. Every such op is listed.
    #[error("the edit list has operations that cannot be evaluated yet: {}", list(.0))]
    Unsupported(Vec<UnsupportedOp>),
    /// An op is malformed or refers to something that does not exist.
    #[error("edit #{index} ({op}): {reason}")]
    Invalid {
        /// Index of the entry in the edit list.
        index: usize,
        /// The op's tag.
        op: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// Two trims keep ranges that do not overlap, so nothing would remain.
    #[error("trims #{first} and #{second} do not overlap, so nothing would remain")]
    DisjointTrims {
        /// The trim with the latest `in`.
        first: usize,
        /// The trim with the earliest `out`.
        second: usize,
    },
    /// Exact arithmetic overflowed: the speeds are too extreme or the
    /// edited duration exceeds `u64` microseconds.
    #[error("the time map overflows exact arithmetic; the speeds are too extreme")]
    Overflow,
}

fn list(ops: &[UnsupportedOp]) -> String {
    ops.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Exact non-negative rational, always reduced, `d > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Q {
    n: u128,
    d: u128,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Q {
    const ZERO: Self = Self { n: 0, d: 1 };
    const ONE: Self = Self { n: 1, d: 1 };

    fn new(n: u128, d: u128) -> Self {
        let g = gcd(n, d).max(1);
        Self { n: n / g, d: d / g }
    }

    fn mul(self, o: Self) -> Option<Self> {
        let g1 = gcd(self.n, o.d).max(1);
        let g2 = gcd(o.n, self.d).max(1);
        Some(Self::new(
            (self.n / g1).checked_mul(o.n / g2)?,
            (self.d / g2).checked_mul(o.d / g1)?,
        ))
    }

    fn add(self, o: Self) -> Option<Self> {
        let g = gcd(self.d, o.d);
        let d = (self.d / g).checked_mul(o.d)?;
        let n = self
            .n
            .checked_mul(o.d / g)?
            .checked_add(o.n.checked_mul(self.d / g)?)?;
        Some(Self::new(n, d))
    }

    /// `self + k * rate`.
    fn add_scaled(self, k: u64, rate: Self) -> Option<Self> {
        self.add(Self::new(u128::from(k).checked_mul(rate.n)?, rate.d))
    }
}

/// `floor(base + off * rate)`, computed without reducing so that its
/// intermediates are bounded by the check done at build time for the whole
/// piece (see [`check_piece`]).
fn floor_at(base: Q, off: u64, rate: Q) -> u128 {
    let n = base.n * rate.d + u128::from(off) * rate.n * base.d;
    n / (base.d * rate.d)
}

/// Proves [`floor_at`] cannot overflow for any `off <= len`.
fn check_piece(base: Q, len: u64, rate: Q) -> Option<()> {
    base.n
        .checked_mul(rate.d)?
        .checked_add(u128::from(len).checked_mul(rate.n)?.checked_mul(base.d)?)?;
    base.d.checked_mul(rate.d)?;
    Some(())
}

/// A rational number, used to report a segment's rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fraction {
    /// Numerator.
    pub num: u128,
    /// Denominator (non-zero).
    pub den: u128,
}

/// What happens during a [`Segment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Source time plays; `edited_per_source` is the density (1/1 is real
    /// time, 1/2 is double speed).
    Play {
        /// Edited microseconds per source microsecond.
        edited_per_source: Fraction,
    },
    /// Source time is cut: the segment takes no edited time.
    Cut,
    /// Playback is frozen after source instant `at`.
    Hold {
        /// The held source instant.
        at: u64,
    },
}

/// One piece of the map. Segments are contiguous in both source and edited
/// time; a hold has an empty source range at the boundary where it is
/// inserted. The sum of the edited lengths is exactly
/// [`TimeMap::edited_duration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Source microseconds covered.
    pub source: Range<u64>,
    /// Edited microseconds covered.
    pub edited: Range<u64>,
    /// What happens.
    pub kind: SegmentKind,
}

/// Source facts the evaluator needs: the duration, when anything happened,
/// and where each pane's output bytes fall in time.
#[derive(Debug, Clone, Default)]
pub struct SourceTimeline {
    duration_us: u64,
    activity: Vec<u64>,
    outputs: BTreeMap<PaneId, Vec<(u64, u64)>>,
}

impl SourceTimeline {
    /// An empty timeline of `duration_us` with no panes and no activity.
    pub fn new(duration_us: u64) -> Self {
        Self {
            duration_us,
            ..Self::default()
        }
    }

    /// Adds a pane's events: their times count as activity (except markers),
    /// and its playback stream's bytes become addressable by [`Src::Byte`].
    pub fn add_pane(&mut self, pane: &PaneId, events: &[RawEvent]) {
        let mut offsets = Vec::new();
        let mut end = 0u64;
        for event in events {
            match &event.kind {
                EventKind::Marker { .. } => continue,
                EventKind::Output(output) => {
                    // The playback stream: what is replayed and indexed.
                    let len = output
                        .playback
                        .as_ref()
                        .map_or(&output.data, |p| &p.data)
                        .len();
                    if len > 0 {
                        end += len as u64;
                        offsets.push((end, event.at_us));
                    }
                }
                _ => {}
            }
            self.activity.push(event.at_us);
        }
        self.outputs.insert(pane.clone(), offsets);
        self.normalise();
    }

    /// Adds instants at which something happened (for tests and for callers
    /// that know of activity outside the panes).
    pub fn add_activity(&mut self, times: impl IntoIterator<Item = u64>) {
        self.activity.extend(times);
        self.normalise();
    }

    fn normalise(&mut self) {
        let duration = self.duration_us;
        self.activity.retain(|&t| t <= duration);
        self.activity.sort_unstable();
        self.activity.dedup();
    }

    /// The source recording's length.
    pub fn duration_us(&self) -> u64 {
        self.duration_us
    }

    /// Resolves a source coordinate to a source microsecond.
    pub fn resolve(&self, src: &Src) -> Result<u64, String> {
        match src {
            Src::Us(us) if *us <= self.duration_us => Ok(*us),
            Src::Us(us) => Err(format!(
                "source time {us}µs is past the end of the recording ({}µs)",
                self.duration_us
            )),
            Src::Byte { pane, offset } => {
                let chunks = self
                    .outputs
                    .get(pane)
                    .ok_or_else(|| format!("there is no pane {:?}", pane.as_str()))?;
                let i = chunks.partition_point(|&(end, _)| end <= *offset);
                chunks.get(i).map(|&(_, at)| at).ok_or_else(|| {
                    format!(
                        "byte offset {offset} is past the end of pane {:?}'s output ({} bytes)",
                        pane.as_str(),
                        chunks.last().map_or(0, |&(end, _)| end)
                    )
                })
            }
        }
    }

    /// The idle gaps within `[from, to]`: consecutive pairs of `from`, every
    /// activity instant strictly between, and `to`, with positive length.
    pub fn idle_gaps(&self, from: u64, to: u64) -> Vec<(u64, u64)> {
        let first = self.activity.partition_point(|&t| t <= from);
        let last = self.activity.partition_point(|&t| t < to);
        let inner = self.activity.get(first..last).unwrap_or(&[]);
        let points: Vec<u64> = std::iter::once(from)
            .chain(inner.iter().copied())
            .chain(std::iter::once(to))
            .collect();
        points
            .windows(2)
            .filter(|w| w[0] < w[1])
            .map(|w| (w[0], w[1]))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    start: u64,
    end: u64,
    /// Density: edited µs per source µs; zero for a cut.
    rate: Q,
    /// Exact edited time at `start`.
    base: Q,
    /// Hold inserted between `end - 1` and `end`.
    hold_after: u64,
}

/// A monotone map from source time to edited time. See the module docs.
#[derive(Debug, Clone)]
pub struct TimeMap {
    lo: u64,
    hi: u64,
    pieces: Vec<Piece>,
    end_floor: u64,
    edited_duration: u64,
    inert: Vec<usize>,
}

fn unsupported(op: &EditOp) -> Option<&'static str> {
    match op {
        EditOp::Trim { .. }
        | EditOp::Skip { .. }
        | EditOp::CollapseIdle { .. }
        | EditOp::Hold { .. }
        | EditOp::Speed {
            rate: Rate::Constant(_),
            ..
        } => None,
        EditOp::Speed {
            rate: Rate::Ramp { .. },
            ..
        } => Some("speed ramps are not supported yet"),
        EditOp::RetimeTyping { .. } => Some("needs typing segments from the index"),
        EditOp::Excise { .. } | EditOp::ReplaceText { .. } | EditOp::RedactInput { .. } => {
            Some("changes bytes; tap-eval evaluates it with the time map")
        }
        EditOp::Mask { .. } => Some("render-time; tap-eval resolves it"),
    }
}

struct Ctx<'a> {
    timeline: &'a SourceTimeline,
    index: usize,
    op: &'static str,
}

impl Ctx<'_> {
    fn invalid(&self, reason: String) -> EvalError {
        EvalError::Invalid {
            index: self.index,
            op: self.op,
            reason,
        }
    }

    fn at(&self, src: &Src) -> Result<u64, EvalError> {
        self.timeline.resolve(src).map_err(|r| self.invalid(r))
    }

    fn span(&self, from: &Src, to: &Src) -> Result<(u64, u64), EvalError> {
        let (a, b) = (self.at(from)?, self.at(to)?);
        if a > b {
            return Err(self.invalid(format!("the range starts at {a}µs, after its end at {b}µs")));
        }
        Ok((a, b))
    }

    fn range(&self, range: &SrcRange) -> Result<(u64, u64), EvalError> {
        self.span(&range.from, &range.to)
    }
}

impl TimeMap {
    /// Evaluates the enabled time-only ops of `entries` against `timeline`.
    pub fn build(entries: &[EditEntry], timeline: &SourceTimeline) -> Result<Self, EvalError> {
        let enabled = || entries.iter().enumerate().filter(|(_, e)| e.enabled);
        let rejected: Vec<UnsupportedOp> = enabled()
            .filter_map(|(index, e)| {
                unsupported(&e.op).map(|reason| UnsupportedOp {
                    index,
                    op: e.op.name(),
                    reason,
                })
            })
            .collect();
        if !rejected.is_empty() {
            return Err(EvalError::Unsupported(rejected));
        }

        let duration = timeline.duration_us();
        let (mut lo, mut lo_by, mut hi, mut hi_by) = (0, 0, duration, 0);
        let mut cuts = Vec::new();
        let mut speeds = Vec::new();
        let mut holds = Vec::new();
        for (index, entry) in enabled() {
            let ctx = Ctx {
                timeline,
                index,
                op: entry.op.name(),
            };
            match &entry.op {
                EditOp::Trim { r#in, out } => {
                    let (a, b) = ctx.span(r#in, out)?;
                    if a >= lo {
                        (lo, lo_by) = (a, index);
                    }
                    if b <= hi {
                        (hi, hi_by) = (b, index);
                    }
                }
                EditOp::Skip { from, to } => cuts.push(ctx.span(from, to)?),
                EditOp::CollapseIdle {
                    range,
                    threshold_us,
                    max_us,
                } => {
                    if max_us > threshold_us {
                        return Err(ctx.invalid(format!(
                            "maxUs ({max_us}) exceeds thresholdUs ({threshold_us})"
                        )));
                    }
                    let (from, to) = match range {
                        Some(r) => ctx.range(r)?,
                        None => (0, duration),
                    };
                    let (head, tail) = (max_us / 2, max_us - max_us / 2);
                    for (g0, g1) in timeline.idle_gaps(from, to) {
                        if g1 - g0 > *threshold_us {
                            cuts.push((g0 + head, g1 - tail));
                        }
                    }
                }
                EditOp::Speed {
                    range,
                    rate: Rate::Constant(r),
                } => {
                    if r.num == 0 || r.den == 0 {
                        return Err(ctx.invalid("rate num and den must both be at least 1".into()));
                    }
                    let (a, b) = ctx.range(range)?;
                    speeds.push((a, b, Q::new(u128::from(r.den), u128::from(r.num))));
                }
                EditOp::Hold { at, duration_us } => holds.push((index, ctx.at(at)?, *duration_us)),
                // Rejected above with the full list; kept as an error rather
                // than a panic in case the two lists ever drift apart.
                op => {
                    return Err(EvalError::Unsupported(vec![UnsupportedOp {
                        index,
                        op: op.name(),
                        reason: unsupported(op).unwrap_or("not a time-only op"),
                    }]));
                }
            }
        }
        if lo > hi {
            return Err(EvalError::DisjointTrims {
                first: lo_by,
                second: hi_by,
            });
        }

        // Cuts: clip to [lo, hi), then union.
        let mut clipped: Vec<(u64, u64)> = cuts
            .into_iter()
            .map(|(a, b)| (a.max(lo), b.min(hi)))
            .filter(|(a, b)| a < b)
            .collect();
        clipped.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (a, b) in clipped {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        let in_cut = |s: u64| {
            let i = merged.partition_point(|&(a, _)| a <= s);
            i > 0 && s < merged[i - 1].1
        };

        // Holds: keyed by the boundary they precede (`at + 1`), or at the end.
        let mut hold_after: BTreeMap<u64, u64> = BTreeMap::new();
        let mut end_hold = 0u64;
        let mut inert = Vec::new();
        for (index, at, duration_us) in holds {
            if at < lo || at > hi || in_cut(at) {
                inert.push(index);
            } else if at == hi {
                end_hold = end_hold
                    .checked_add(duration_us)
                    .ok_or(EvalError::Overflow)?;
            } else {
                let slot = hold_after.entry(at + 1).or_default();
                *slot = slot.checked_add(duration_us).ok_or(EvalError::Overflow)?;
            }
        }

        // Pieces between every breakpoint, each wholly cut or wholly kept.
        let mut points = vec![lo, hi];
        points.extend(merged.iter().flat_map(|&(a, b)| [a, b]));
        points.extend(
            speeds
                .iter()
                .flat_map(|&(a, b, _)| [a.clamp(lo, hi), b.clamp(lo, hi)]),
        );
        points.extend(hold_after.keys().copied());
        points.sort_unstable();
        points.dedup();
        let mut raw: Vec<(u64, u64, Q)> = points
            .windows(2)
            .map(|w| (w[0], w[1], if in_cut(w[0]) { Q::ZERO } else { Q::ONE }))
            .collect();
        for &(a, b, density) in &speeds {
            let first = raw.partition_point(|p| p.0 < a.clamp(lo, hi));
            let last = raw.partition_point(|p| p.0 < b.clamp(lo, hi));
            for piece in &mut raw[first..last] {
                piece.2 = piece.2.mul(density).ok_or(EvalError::Overflow)?;
            }
        }

        // Merge equal neighbours and accumulate exact edited time.
        let mut pieces: Vec<Piece> = Vec::new();
        let mut base = Q::ZERO;
        for (start, end, rate) in raw {
            let hold = hold_after.get(&end).copied().unwrap_or(0);
            match pieces.last_mut() {
                Some(last) if last.rate == rate && last.hold_after == 0 => {
                    last.end = end;
                    last.hold_after = hold;
                    check_piece(last.base, end - last.start, rate).ok_or(EvalError::Overflow)?;
                }
                _ => {
                    check_piece(base, end - start, rate).ok_or(EvalError::Overflow)?;
                    pieces.push(Piece {
                        start,
                        end,
                        rate,
                        base,
                        hold_after: hold,
                    });
                }
            }
            base = base
                .add_scaled(end - start, rate)
                .and_then(|q| q.add(Q::new(u128::from(hold), 1)))
                .ok_or(EvalError::Overflow)?;
        }
        let end_floor = u64::try_from(base.n / base.d).map_err(|_| EvalError::Overflow)?;
        let edited_duration = end_floor.checked_add(end_hold).ok_or(EvalError::Overflow)?;
        Ok(Self {
            lo,
            hi,
            pieces,
            end_floor,
            edited_duration,
            inert,
        })
    }

    /// First kept source microsecond (the latest trim `in`, else 0).
    pub fn source_start(&self) -> u64 {
        self.lo
    }

    /// Last kept source microsecond (the earliest trim `out`, else the
    /// recording's duration).
    pub fn source_end(&self) -> u64 {
        self.hi
    }

    /// Length of the edited result in microseconds.
    pub fn edited_duration(&self) -> u64 {
        self.edited_duration
    }

    /// Indices of enabled holds that had no effect because their instant was
    /// trimmed or cut.
    pub fn inert_ops(&self) -> &[usize] {
        &self.inert
    }

    /// Maps a source microsecond to an edited microsecond. Monotone
    /// non-decreasing; a cut range maps to its cut instant.
    pub fn to_edited(&self, source_us: u64) -> u64 {
        if source_us > self.hi {
            return self.edited_duration;
        }
        if source_us == self.hi {
            return self.end_floor;
        }
        if source_us <= self.lo {
            return 0;
        }
        let k = self.pieces.partition_point(|p| p.start <= source_us) - 1;
        let piece = &self.pieces[k];
        let value = floor_at(piece.base, source_us - piece.start, piece.rate);
        u64::try_from(value).expect("bounded by edited_duration, checked at build")
    }

    /// The largest kept source microsecond whose edited time is at most
    /// `edited_us`. See the module docs.
    pub fn to_source(&self, edited_us: u64) -> u64 {
        let (mut lo, mut hi) = (self.lo, self.hi);
        if self.to_edited(hi) <= edited_us {
            return hi;
        }
        // Invariant: to_edited(lo) <= edited_us < to_edited(hi).
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.to_edited(mid) <= edited_us {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The map as contiguous segments, in order.
    pub fn segments(&self) -> Vec<Segment> {
        let mut out = Vec::with_capacity(self.pieces.len() + 1);
        for p in &self.pieces {
            let e0 = floor_at(p.base, 0, p.rate) as u64;
            let e1 = floor_at(p.base, p.end - p.start, p.rate) as u64;
            let kind = if p.rate.n == 0 {
                SegmentKind::Cut
            } else {
                SegmentKind::Play {
                    edited_per_source: Fraction {
                        num: p.rate.n,
                        den: p.rate.d,
                    },
                }
            };
            out.push(Segment {
                source: p.start..p.end,
                edited: e0..e1,
                kind,
            });
            if p.hold_after > 0 {
                out.push(Segment {
                    source: p.end..p.end,
                    edited: e1..e1 + p.hold_after,
                    kind: SegmentKind::Hold { at: p.end - 1 },
                });
            }
        }
        if self.edited_duration > self.end_floor {
            out.push(Segment {
                source: self.hi..self.hi,
                edited: self.end_floor..self.edited_duration,
                kind: SegmentKind::Hold { at: self.hi },
            });
        }
        out
    }
}
