//! The time map: unit cases, an independent per-microsecond reference model,
//! and properties (monotone, invertible where it can be, exact, commutative).

use proptest::prelude::*;
use tap_model::PaneId;
use tap_model::edl::{EditEntry, EditOp, Rate, Ratio, SegId, Src, SrcRange};
use tap_model::event::{EventKind, Output, RawEvent, TextEncoding};
use tap_model::timemap::{EvalError, SegmentKind, SourceTimeline, TimeMap};

fn us(v: u64) -> Src {
    Src::Us(v)
}

fn range(a: u64, b: u64) -> SrcRange {
    SrcRange {
        from: us(a),
        to: us(b),
    }
}

fn trim(a: u64, b: u64) -> EditOp {
    EditOp::Trim {
        r#in: us(a),
        out: us(b),
    }
}

fn skip(a: u64, b: u64) -> EditOp {
    EditOp::Skip {
        from: us(a),
        to: us(b),
    }
}

fn speed(a: u64, b: u64, num: u32, den: u32) -> EditOp {
    EditOp::Speed {
        range: range(a, b),
        rate: Rate::Constant(Ratio { num, den }),
    }
}

fn hold(at: u64, duration_us: u64) -> EditOp {
    EditOp::Hold {
        at: us(at),
        duration_us,
    }
}

fn collapse(r: Option<(u64, u64)>, threshold_us: u64, max_us: u64) -> EditOp {
    EditOp::CollapseIdle {
        range: r.map(|(a, b)| range(a, b)),
        threshold_us,
        max_us,
    }
}

fn build(ops: &[EditOp], timeline: &SourceTimeline) -> Result<TimeMap, EvalError> {
    let entries: Vec<EditEntry> = ops.iter().cloned().map(EditEntry::new).collect();
    TimeMap::build(&entries, timeline)
}

fn map(duration: u64, ops: &[EditOp]) -> TimeMap {
    build(ops, &SourceTimeline::new(duration)).unwrap()
}

#[test]
fn no_edits_is_the_identity() {
    let m = map(1000, &[]);
    assert_eq!(m.edited_duration(), 1000);
    for s in [0, 1, 500, 999, 1000] {
        assert_eq!(m.to_edited(s), s);
        assert_eq!(m.to_source(s), s);
    }
    assert_eq!(m.to_edited(5000), 1000);
    assert_eq!(m.to_source(5000), 1000);
}

#[test]
fn trim_keeps_in_through_out() {
    let m = map(100, &[trim(10, 90)]);
    assert_eq!(m.edited_duration(), 80);
    assert_eq!((m.source_start(), m.source_end()), (10, 90));
    assert_eq!(
        [
            m.to_edited(0),
            m.to_edited(10),
            m.to_edited(50),
            m.to_edited(90),
            m.to_edited(95)
        ],
        [0, 0, 40, 80, 80]
    );
    assert_eq!(
        [
            m.to_source(0),
            m.to_source(40),
            m.to_source(80),
            m.to_source(1000)
        ],
        [10, 50, 90, 90]
    );
}

#[test]
fn skip_maps_to_its_cut_instant() {
    let m = map(100, &[skip(20, 30)]);
    assert_eq!(m.edited_duration(), 90);
    for s in 20..=30 {
        assert_eq!(m.to_edited(s), 20);
    }
    assert_eq!(m.to_edited(31), 21);
    assert_eq!(m.to_source(19), 19);
    assert_eq!(
        m.to_source(20),
        30,
        "a cut instant shows the state at the end of the cut"
    );
    // A skip at the very end still applies its bytes: the last frame is the
    // state at the end of the recording, unlike a trim.
    let m = map(100, &[skip(90, 100)]);
    assert_eq!((m.edited_duration(), m.to_source(90)), (90, 100));
    let m = map(100, &[trim(0, 90)]);
    assert_eq!((m.edited_duration(), m.to_source(90)), (90, 90));
}

#[test]
fn speed_is_exact_and_floored() {
    let m = map(100, &[speed(0, 100, 2, 1)]);
    assert_eq!(m.edited_duration(), 50);
    assert_eq!([m.to_edited(1), m.to_edited(2), m.to_edited(3)], [0, 1, 1]);
    assert_eq!(m.to_source(1), 3, "the last source µs that maps there");
    assert_eq!(map(10, &[speed(0, 10, 3, 1)]).edited_duration(), 3);
    assert_eq!(map(100, &[speed(0, 100, 1, 3)]).edited_duration(), 300);
    // 2x over [0,50) and [25,75) is 4x where they overlap:
    // 25/2 + 25/4 + 25/2 + 25 = 56.25.
    assert_eq!(
        map(100, &[speed(0, 50, 2, 1), speed(25, 75, 2, 1)]).edited_duration(),
        56
    );
    // A speed over a cut leaves the cut a cut.
    assert_eq!(
        map(100, &[skip(0, 50), speed(0, 100, 1, 2)]).edited_duration(),
        100
    );
}

#[test]
fn hold_freezes_after_its_instant() {
    let m = map(100, &[hold(50, 1000)]);
    assert_eq!(m.edited_duration(), 1100);
    assert_eq!([m.to_edited(50), m.to_edited(51)], [50, 1051]);
    for e in [50, 500, 1050] {
        assert_eq!(m.to_source(e), 50);
    }
    assert_eq!(m.to_source(1051), 51);
    // Holds at the same instant add up; a hold at the end extends it.
    assert_eq!(map(100, &[hold(10, 5), hold(10, 7)]).edited_duration(), 112);
    let m = map(100, &[hold(100, 30)]);
    assert_eq!(
        (m.to_edited(100), m.edited_duration(), m.to_source(129)),
        (100, 130, 100)
    );
    let last = m.segments().pop().unwrap();
    assert_eq!(last.kind, SegmentKind::Hold { at: 100 });
    // Holds are not scaled by speed.
    assert_eq!(
        map(100, &[speed(0, 100, 2, 1), hold(10, 40)]).edited_duration(),
        90
    );
}

#[test]
fn holds_on_removed_instants_are_inert_and_reported() {
    let entries: Vec<EditEntry> = [
        trim(10, 90),
        skip(40, 60),
        hold(5, 100),
        hold(50, 100),
        hold(60, 1),
        hold(95, 100),
        hold(39, 2),
    ]
    .into_iter()
    .map(EditEntry::new)
    .collect();
    let m = TimeMap::build(&entries, &SourceTimeline::new(100)).unwrap();
    assert_eq!(m.inert_ops(), [2, 3, 5]);
    assert_eq!(m.edited_duration(), 80 - 20 + 1 + 2);
}

#[test]
fn collapse_idle_keeps_max_around_each_long_gap() {
    let mut t = SourceTimeline::new(6000);
    t.add_activity([0, 10, 5000, 5010]);
    // Gap (10, 5000) is 4990 > 1000: keep 50 after 10 and 50 before 5000.
    let m = build(&[collapse(None, 1000, 100)], &t).unwrap();
    assert_eq!(m.edited_duration(), 6000 - 4890);
    assert_eq!(m.to_edited(60), 60);
    assert_eq!(m.to_edited(4950), 60);
    assert_eq!(m.to_edited(5000), 110);
    // Restricted to a range, the range edges act as gap edges.
    let m = build(&[collapse(Some((2000, 3000)), 500, 0)], &t).unwrap();
    assert_eq!(m.edited_duration(), 5000);
    // The gaps before the first and after the last event count.
    let mut t = SourceTimeline::new(10_000);
    t.add_activity([4000]);
    assert_eq!(
        build(&[collapse(None, 1000, 200)], &t)
            .unwrap()
            .edited_duration(),
        400
    );
    // Two collapses over the same gap: the shorter max wins.
    let mut t = SourceTimeline::new(1000);
    t.add_activity([0, 1000]);
    let both = build(&[collapse(None, 100, 50), collapse(None, 100, 10)], &t).unwrap();
    assert_eq!(both.edited_duration(), 10);
    // A speed over a collapsed gap still scales the kept pause.
    let sped = build(&[collapse(None, 100, 50), speed(0, 1000, 2, 1)], &t).unwrap();
    assert_eq!(sped.edited_duration(), 25);
}

#[test]
fn markers_are_not_activity_but_everything_else_is() {
    let pane = PaneId::new("main").unwrap();
    let events = vec![
        RawEvent {
            at_us: 0,
            seq: 0,
            kind: EventKind::Marker { label: "m".into() },
        },
        RawEvent {
            at_us: 500,
            seq: 1,
            kind: EventKind::Marker { label: "m".into() },
        },
        RawEvent {
            at_us: 900,
            seq: 2,
            kind: EventKind::Visibility { visible: true },
        },
    ];
    let mut t = SourceTimeline::new(1000);
    t.add_pane(&pane, &events);
    assert_eq!(t.idle_gaps(0, 1000), [(0, 900), (900, 1000)]);
}

fn output(at_us: u64, seq: u64, data: &[u8]) -> RawEvent {
    RawEvent {
        at_us,
        seq,
        kind: EventKind::Output(Output {
            data: data.to_vec(),
            data_encoding: TextEncoding::Unspecified,
            playback: None,
        }),
    }
}

#[test]
fn byte_offsets_resolve_to_their_events() {
    let pane = PaneId::new("main").unwrap();
    let mut t = SourceTimeline::new(100);
    t.add_pane(
        &pane,
        &[
            output(10, 0, b"abc"),
            output(15, 1, b""),
            output(20, 2, b"de"),
        ],
    );
    let at = |offset| {
        t.resolve(&Src::Byte {
            pane: pane.clone(),
            offset,
        })
    };
    assert_eq!(
        [at(0), at(2), at(3), at(4)],
        [Ok(10), Ok(10), Ok(20), Ok(20)]
    );
    assert!(
        at(5)
            .unwrap_err()
            .contains("past the end of pane \"main\"'s output (5 bytes)")
    );
    let other = PaneId::new("other").unwrap();
    assert!(
        t.resolve(&Src::Byte {
            pane: other,
            offset: 0
        })
        .unwrap_err()
        .contains("no pane")
    );
    let m = build(
        &[EditOp::Skip {
            from: Src::Byte {
                pane: pane.clone(),
                offset: 1,
            },
            to: Src::Byte { pane, offset: 3 },
        }],
        &t,
    )
    .unwrap();
    assert_eq!(m.edited_duration(), 90);
}

#[test]
fn errors_name_the_op() {
    let t = SourceTimeline::new(100);
    let err = |ops: &[EditOp]| build(ops, &t).unwrap_err().to_string();
    assert_eq!(
        err(&[skip(0, 1), trim(50, 40)]),
        "edit #1 (trim): the range starts at 50µs, after its end at 40µs"
    );
    assert_eq!(
        err(&[skip(0, 101)]),
        "edit #0 (skip): source time 101µs is past the end of the recording (100µs)"
    );
    assert_eq!(
        err(&[collapse(None, 10, 11)]),
        "edit #0 (collapseIdle): maxUs (11) exceeds thresholdUs (10)"
    );
    assert_eq!(
        err(&[speed(0, 10, 0, 1)]),
        "edit #0 (speed): rate num and den must both be at least 1"
    );
    assert_eq!(
        err(&[trim(0, 10), trim(20, 30)]),
        "trims #1 and #0 do not overlap, so nothing would remain"
    );
}

#[test]
fn unsupported_ops_are_listed_not_ignored() {
    let pane = PaneId::new("main").unwrap();
    let seg = SegId {
        pane: pane.clone(),
        start: 0,
        end: 10,
    };
    let entries = vec![
        EditEntry::new(skip(0, 10)),
        EditEntry::new(EditOp::Excise {
            segment: seg.clone(),
        }),
        EditEntry::new(EditOp::Speed {
            range: range(0, 10),
            rate: Rate::Ramp {
                from: Ratio { num: 1, den: 1 },
                to: Ratio { num: 2, den: 1 },
            },
        }),
        EditEntry::disabled(EditOp::RetimeTyping {
            segment: seg.clone(),
            cadence_us: 1,
            jitter_seed: None,
        }),
        EditEntry::new(EditOp::RetimeTyping {
            segment: seg,
            cadence_us: 1,
            jitter_seed: None,
        }),
        EditEntry::new(EditOp::ReplaceText {
            pane: pane.clone(),
            bytes: tap_model::edl::ByteRange { start: 0, end: 1 },
            text: "a".into(),
            expect: "a".into(),
        }),
        EditEntry::new(EditOp::RedactInput {
            pane: pane.clone(),
            events: tap_model::edl::SeqRange { start: 0, end: 1 },
        }),
        EditEntry::new(EditOp::Mask {
            pane,
            anchor: tap_model::edl::TrackedRef { byte: 0 },
            cells: 3,
            style: tap_model::edl::MaskStyle::Blur,
        }),
    ];
    let err = TimeMap::build(&entries, &SourceTimeline::new(100)).unwrap_err();
    let EvalError::Unsupported(ops) = &err else {
        panic!("{err}")
    };
    let listed: Vec<(usize, &str)> = ops.iter().map(|o| (o.index, o.op)).collect();
    assert_eq!(
        listed,
        [
            (1, "excise"),
            (2, "speed"),
            (4, "retimeTyping"),
            (5, "replaceText"),
            (6, "redactInput"),
            (7, "mask")
        ]
    );
    let message = err.to_string();
    assert!(
        message.contains("#2 speed (speed ramps are not supported yet)"),
        "{message}"
    );
    assert!(message.contains("#1 excise (changes bytes"), "{message}");
}

#[test]
fn disabled_entries_have_no_effect() {
    let entries = vec![
        EditEntry::disabled(trim(10, 20)),
        EditEntry::disabled(speed(0, 100, 9, 1)),
    ];
    assert_eq!(
        TimeMap::build(&entries, &SourceTimeline::new(100))
            .unwrap()
            .edited_duration(),
        100
    );
}

#[test]
fn long_sessions_do_not_drift() {
    // 100 hours, 10 000 alternating 3x / 7/5x slices plus skips: compare with
    // the exact sum over a common denominator of 21.
    let duration = 100 * 3600 * 1_000_000u64;
    let slices = 10_000u64;
    let width = duration / slices;
    let mut ops = Vec::new();
    let mut exact_21ths: u128 = 0; // edited time in units of 1/21 µs
    for i in 0..slices {
        let (a, b) = (i * width, (i + 1) * width);
        let (num, den) = if i % 2 == 0 { (3, 1) } else { (7, 5) };
        ops.push(speed(a, b, num, den));
        let mut kept = u128::from(b - a);
        if i % 3 == 0 {
            ops.push(skip(a + 1, a + 1 + 7_777));
            kept -= 7_777;
        }
        exact_21ths += kept * 21 * u128::from(den) / u128::from(num);
    }
    let m = map(duration, &ops);
    assert_eq!(u128::from(m.edited_duration()), exact_21ths / 21);
    // Every slice boundary lands on its exact value too.
    let mut acc: u128 = 0;
    for i in 0..slices {
        let a = i * width;
        assert_eq!(u128::from(m.to_edited(a)), acc / 21, "slice {i}");
        let (num, den) = if i % 2 == 0 { (3, 1) } else { (7, 5) };
        let kept = u128::from(width) - if i % 3 == 0 { 7_777 } else { 0 };
        acc += kept * 21 * den / num;
    }
    // A float accumulator would be off by now; this map is not.
    let segments = m.segments();
    let total: u64 = segments.iter().map(|s| s.edited.end - s.edited.start).sum();
    assert_eq!(total, m.edited_duration());
}

#[test]
fn extreme_rates_overflow_cleanly() {
    let ops: Vec<EditOp> = (0..12u32)
        .map(|i| speed(0, 1 << 60, u32::MAX - i, u32::MAX - 100 - i))
        .collect();
    match build(&ops, &SourceTimeline::new(1 << 60)) {
        Ok(m) => assert!(m.edited_duration() > 0),
        Err(e) => assert_eq!(e, EvalError::Overflow),
    }
}

// ---------------------------------------------------------------------------
// Reference model: per-microsecond, written independently of the
// implementation, for small durations.

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frac(u128, u128);

fn gcd(a: u128, b: u128) -> u128 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Frac {
    fn add(self, o: Frac) -> Frac {
        let (n, d) = (self.0 * o.1 + o.0 * self.1, self.1 * o.1);
        let g = gcd(n, d).max(1);
        Frac(n / g, d / g)
    }
    fn mul(self, o: Frac) -> Frac {
        let (n, d) = (self.0 * o.0, self.1 * o.1);
        let g = gcd(n, d).max(1);
        Frac(n / g, d / g)
    }
}

/// `Ok(to_edited for every s in 0..=duration+1)` or `Err(())` for disjoint trims.
fn reference(duration: u64, activity: &[u64], ops: &[EditOp]) -> Result<Vec<u64>, ()> {
    let n = duration as usize;
    let (mut lo, mut hi) = (0u64, duration);
    let get = |s: &Src| match s {
        Src::Us(v) => *v,
        Src::Byte { .. } => unreachable!(),
    };
    for op in ops {
        if let EditOp::Trim { r#in, out } = op {
            lo = lo.max(get(r#in));
            hi = hi.min(get(out));
        }
    }
    if lo > hi {
        return Err(());
    }
    let mut removed = vec![false; n];
    let mut density = vec![Frac(1, 1); n];
    let mut holds: Vec<(u64, u64)> = Vec::new();
    let mut acts: Vec<u64> = activity.to_vec();
    acts.sort_unstable();
    acts.dedup();
    for op in ops {
        match op {
            EditOp::Skip { from, to } => {
                (get(from)..get(to)).for_each(|t| removed[t as usize] = true)
            }
            EditOp::CollapseIdle {
                range,
                threshold_us,
                max_us,
            } => {
                let (r0, r1) = range
                    .as_ref()
                    .map_or((0, duration), |r| (get(&r.from), get(&r.to)));
                let mut edges = vec![r0];
                edges.extend(acts.iter().copied().filter(|&t| t > r0 && t < r1));
                edges.push(r1);
                for w in edges.windows(2) {
                    if w[1] > w[0] && w[1] - w[0] > *threshold_us {
                        (w[0] + max_us / 2..w[1] - (max_us - max_us / 2))
                            .for_each(|t| removed[t as usize] = true);
                    }
                }
            }
            EditOp::Speed {
                range,
                rate: Rate::Constant(r),
            } => {
                for t in get(&range.from)..get(&range.to) {
                    density[t as usize] =
                        density[t as usize].mul(Frac(u128::from(r.den), u128::from(r.num)));
                }
            }
            EditOp::Hold { at, duration_us } => holds.push((get(at), *duration_us)),
            _ => {}
        }
    }
    let kept = |t: u64| t >= lo && t < hi && !removed[t as usize];
    let mut out = Vec::with_capacity(n + 2);
    let mut exact = Frac(0, 1);
    for s in 0..=duration {
        if s < lo {
            out.push(0);
            continue;
        }
        if s > lo {
            let t = s - 1;
            if kept(t) {
                exact = exact.add(density[t as usize]);
            }
            for &(at, d) in &holds {
                if at == t && kept(t) {
                    exact = exact.add(Frac(u128::from(d), 1));
                }
            }
        }
        out.push(if s <= hi {
            (exact.0 / exact.1) as u64
        } else {
            0
        });
    }
    let at_hi: u64 = holds
        .iter()
        .filter(|&&(at, _)| at == hi)
        .map(|&(_, d)| d)
        .sum();
    let end = out[hi as usize] + at_hi;
    for v in out.iter_mut().skip(hi as usize + 1) {
        *v = end;
    }
    out.push(end);
    Ok(out)
}

fn ordered(a: u64, b: u64) -> (u64, u64) {
    (a.min(b), a.max(b))
}

fn arb_op(d: u64, max_rate: u32) -> impl Strategy<Value = EditOp> {
    let t = move || 0..=d;
    prop_oneof![
        1 => (t(), t()).prop_map(|(a, b)| { let (a, b) = ordered(a, b); trim(a, b) }),
        3 => (t(), t()).prop_map(|(a, b)| { let (a, b) = ordered(a, b); skip(a, b) }),
        2 => (prop::option::of((t(), t())), 0..=d / 2, 0..=d / 2).prop_map(|(r, x, y)| {
            let (max, threshold) = ordered(x, y);
            collapse(r.map(|(a, b)| ordered(a, b)), threshold, max)
        }),
        3 => (t(), t(), 1..=max_rate, 1..=max_rate).prop_map(|(a, b, num, den)| {
            let (a, b) = ordered(a, b);
            speed(a, b, num, den)
        }),
        2 => (t(), 0..500u64).prop_map(|(at, dur)| hold(at, dur)),
    ]
}

fn arb_case(max_d: u64, max_rate: u32) -> impl Strategy<Value = (u64, Vec<u64>, Vec<EditOp>)> {
    (1..=max_d).prop_flat_map(move |d| {
        (
            Just(d),
            prop::collection::vec(0..=d, 0..12),
            prop::collection::vec(arb_op(d, max_rate), 0..7),
        )
    })
}

fn timeline(d: u64, activity: &[u64]) -> SourceTimeline {
    let mut t = SourceTimeline::new(d);
    t.add_activity(activity.iter().copied());
    t
}

fn check_segments(m: &TimeMap) -> Result<(), TestCaseError> {
    let segments = m.segments();
    let mut source = m.source_start();
    let mut edited = 0;
    let mut sum = 0;
    for s in &segments {
        prop_assert_eq!(s.source.start, source);
        prop_assert_eq!(s.edited.start, edited);
        prop_assert!(s.source.start <= s.source.end && s.edited.start <= s.edited.end);
        match s.kind {
            SegmentKind::Cut => prop_assert_eq!(s.edited.start, s.edited.end),
            SegmentKind::Hold { .. } => prop_assert_eq!(s.source.start, s.source.end),
            SegmentKind::Play { .. } => {
                prop_assert_eq!(m.to_edited(s.source.start), s.edited.start);
            }
        }
        source = s.source.end;
        edited = s.edited.end;
        sum += s.edited.end - s.edited.start;
    }
    prop_assert_eq!(source, m.source_end());
    prop_assert_eq!(edited, m.edited_duration());
    prop_assert_eq!(sum, m.edited_duration());
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The map equals the reference at every microsecond, and `to_source` is
    /// exactly "the largest kept s with to_edited(s) <= e" at every edited
    /// microsecond.
    #[test]
    fn matches_the_reference_model((d, activity, ops) in arb_case(300, 6)) {
        let expected = reference(d, &activity, &ops);
        let built = build(&ops, &timeline(d, &activity));
        let (m, expected) = match (built, expected) {
            (Err(EvalError::DisjointTrims { .. }), Err(())) => return Ok(()),
            (Ok(m), Ok(e)) => (m, e),
            (got, want) => return Err(TestCaseError::fail(format!("got {got:?}, reference {want:?}"))),
        };
        for s in 0..=d + 1 {
            prop_assert_eq!(m.to_edited(s), expected[s as usize], "s = {}", s);
        }
        prop_assert_eq!(m.edited_duration(), expected[d as usize + 1]);
        let (lo, hi) = (m.source_start(), m.source_end());
        let mut s = lo;
        for e in 0..=m.edited_duration() + 1 {
            while s < hi && expected[s as usize + 1] <= e {
                s += 1;
            }
            prop_assert_eq!(m.to_source(e), s, "e = {}", e);
        }
        check_segments(&m)?;
    }

    /// Monotone; to_source is a right inverse up to rounding; and where
    /// nothing is sped up, to_source(to_edited(s)) == s for every kept s.
    #[test]
    fn monotone_and_invertible((d, activity, ops) in arb_case(1 << 40, 64), probes in prop::collection::vec(any::<u64>(), 64)) {
        let Ok(m) = build(&ops, &timeline(d, &activity)) else { return Ok(()) };
        check_segments(&m)?;
        let segments = m.segments();
        let mut points: Vec<u64> = probes.iter().map(|p| p % (d + 2)).collect();
        for s in &segments {
            points.extend([s.source.start, s.source.end, s.source.start.saturating_sub(1), s.source.end + 1]);
        }
        points.sort_unstable();
        let edited: Vec<u64> = points.iter().map(|&s| m.to_edited(s)).collect();
        prop_assert!(edited.windows(2).all(|w| w[0] <= w[1]), "not monotone");
        for &s in &points {
            let e = m.to_edited(s);
            let back = m.to_source(e);
            prop_assert_eq!(m.to_edited(back), e);
            prop_assert!(back >= m.source_start() && back <= m.source_end());
        }
        for e in probes.iter().map(|p| p % (m.edited_duration() + 2)) {
            prop_assert!(m.to_edited(m.to_source(e)) <= e);
        }
        let no_speed_up = segments.iter().all(|s| match s.kind {
            SegmentKind::Play { edited_per_source: f } => f.num >= f.den,
            _ => true,
        });
        if no_speed_up {
            for s in &segments {
                if let SegmentKind::Play { .. } = s.kind {
                    for p in [s.source.start, s.source.end - 1, s.source.start + (s.source.end - s.source.start) / 2] {
                        prop_assert_eq!(m.to_source(m.to_edited(p)), p);
                    }
                }
            }
            prop_assert_eq!(m.to_source(m.to_edited(m.source_end())), m.source_end());
        }
    }

    /// Order never matters: every permutation of the ops gives the same map.
    #[test]
    fn ops_commute(((d, activity, ops), shuffled) in arb_case(1 << 30, 8).prop_flat_map(|case| {
        let ops = case.2.clone();
        (Just(case), Just(ops).prop_shuffle())
    })) {
        let t = timeline(d, &activity);
        match (build(&ops, &t), build(&shuffled, &t)) {
            (Ok(a), Ok(b)) => {
                prop_assert_eq!(a.segments(), b.segments());
                prop_assert_eq!(a.edited_duration(), b.edited_duration());
                prop_assert_eq!(a.inert_ops().len(), b.inert_ops().len());
            }
            (Err(EvalError::DisjointTrims { .. }), Err(EvalError::DisjointTrims { .. })) => {}
            (a, b) => return Err(TestCaseError::fail(format!("{a:?} vs {b:?}"))),
        }
    }

    /// Every pair of time-only op kinds commutes, including when they overlap.
    #[test]
    fn every_pair_commutes(a in arb_op(10_000, 8), b in arb_op(10_000, 8), activity in prop::collection::vec(0..=10_000u64, 0..20)) {
        let t = timeline(10_000, &activity);
        let ab = build(&[a.clone(), b.clone()], &t);
        let ba = build(&[b, a], &t);
        match (ab, ba) {
            (Ok(x), Ok(y)) => prop_assert_eq!(x.segments(), y.segments()),
            (Err(EvalError::DisjointTrims { .. }), Err(EvalError::DisjointTrims { .. })) => {}
            (x, y) => return Err(TestCaseError::fail(format!("{x:?} vs {y:?}"))),
        }
    }
}
