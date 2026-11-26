//! Evaluation as a whole: unedited sessions index exactly as before, only
//! retiming and ramps are unsupported, random edit lists never panic, and
//! seek over an edited program keeps tap-index's guarantee.

mod support;

use tap_eval::{EvalError, Outcome, evaluate_entries};
use tap_index::{Location, Origin, SessionIndex};
use tap_model::edl::{
    ByteRange, EditEntry, EditOp, MaskStyle, Rate, Ratio, SegId, SeqRange, Src, SrcRange,
    TrackedRef,
};
use tap_model::{Session, TimeMap};

use support::*;

const FIXTURES: [&str; 4] = ["osc133", "no-integration", "redacted", "edits"];

fn time_ops(duration: u64) -> Vec<EditOp> {
    vec![
        EditOp::Skip {
            from: Src::Us(duration / 4),
            to: Src::Us(duration / 3),
        },
        EditOp::Speed {
            range: SrcRange {
                from: Src::Us(duration / 2),
                to: Src::Us(duration),
            },
            rate: Rate::Constant(Ratio { num: 2, den: 1 }),
        },
        EditOp::CollapseIdle {
            range: None,
            threshold_us: 1_500_000,
            max_us: 500_000,
        },
    ]
}

#[test]
fn sessions_without_byte_edits_index_byte_identically() {
    for name in FIXTURES {
        let session = fixture(name);
        let raw = SessionIndex::build(&session).unwrap();
        for ops in [Vec::new(), time_ops(session.manifest.duration_us)] {
            let evaluation = evaluate(&session, &raw, ops.clone());
            assert!(evaluation.programs.values().all(|p| p.is_identity()));
            assert!(evaluation.outcomes.iter().all(|o| *o == Outcome::Time));
            assert!(evaluation.masks.is_empty());
            // The time map is tap-model's for the same ops.
            let entries: Vec<EditEntry> = ops.into_iter().map(EditEntry::new).collect();
            let expected = TimeMap::build(&entries, &session.timeline()).unwrap();
            assert_eq!(evaluation.time_map.segments(), expected.segments());

            let edited = edited_index(&session, &evaluation);
            assert_eq!(
                serde_json::to_string(&edited).unwrap(),
                serde_json::to_string(&raw).unwrap(),
                "{name}: index"
            );
            assert_eq!(edited.transcript().to_text(), raw.transcript().to_text());
            assert_eq!(
                serde_json::to_string(&edited.transcript()).unwrap(),
                serde_json::to_string(&raw.transcript()).unwrap()
            );
            for pane in &raw.panes {
                let a: Vec<_> = raw.checkpoints().pane(&pane.pane).collect();
                let b: Vec<_> = edited.checkpoints().pane(&pane.pane).collect();
                assert_eq!(a, b, "{name}: checkpoints");
                assert!(a.iter().zip(&b).all(|(x, y)| x.snapshot() == y.snapshot()));
                for event in session.pane_events(&pane.pane) {
                    let x = raw.seek(&pane.pane, event.at_us).unwrap();
                    let y = edited.seek(&pane.pane, event.at_us).unwrap();
                    assert_eq!(observe(&x), observe(&y), "{name}: seek {}", event.at_us);
                }
            }
        }
    }
}

#[test]
fn only_retiming_and_ramps_are_unsupported() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    let entries: Vec<EditEntry> = vec![
        EditEntry::new(excise(104, 623)),
        EditEntry::new(EditOp::RetimeTyping {
            segment: SegId {
                pane: main_pane(),
                start: 104,
                end: 623,
            },
            cadence_us: 80_000,
            jitter_seed: None,
        }),
        EditEntry::new(EditOp::Speed {
            range: SrcRange {
                from: Src::Us(0),
                to: Src::Us(1_000_000),
            },
            rate: Rate::Ramp {
                from: Ratio { num: 1, den: 1 },
                to: Ratio { num: 4, den: 1 },
            },
        }),
        EditEntry::new(EditOp::RedactInput {
            pane: main_pane(),
            events: SeqRange { start: 0, end: 10 },
        }),
    ];
    match evaluate_entries(&session, &raw, &entries) {
        Err(EvalError::Unsupported(ops)) => {
            let listed: Vec<(usize, &str)> = ops.iter().map(|o| (o.index, o.op)).collect();
            assert_eq!(listed, [(1, "retimeTyping"), (2, "speed")]);
        }
        other => panic!("{other:?}"),
    }
}

/// Seek over the edited index against a replay from the start, at every
/// event and checkpoint time: the fixtures never reach the scrollback limit,
/// so tap-index's guarantee is exact equality.
fn check_seek(session: &Session, edited: &SessionIndex, label: &str) {
    for pane in &edited.panes {
        let mut times: Vec<u64> = session
            .pane_events(&pane.pane)
            .iter()
            .map(|e| e.at_us)
            .collect();
        times.extend(edited.checkpoints().pane(&pane.pane).map(|c| c.at_us));
        times.push(session.manifest.duration_us);
        times.sort_unstable();
        times.dedup();
        for t in times {
            let seek = edited.seek(&pane.pane, t).unwrap();
            let replay = edited.replay_from_start(&pane.pane, t).unwrap();
            assert_eq!(observe(&seek), observe(&replay), "{label}: seek to {t}");
        }
    }
}

#[test]
fn seek_over_edited_programs_keeps_its_guarantee() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    // What "redact sk-live-4f9a2b" plans (see tests/redact.rs): three
    // rewrites, a mask over the full-screen copy, the keystrokes withheld.
    let bullets = "••••••••••••••";
    let mut ops: Vec<EditOp> = [894..908, 975..989, 1281..1300]
        .into_iter()
        .map(|r| EditOp::ReplaceText {
            pane: main_pane(),
            bytes: ByteRange {
                start: r.start,
                end: r.end,
            },
            text: bullets.into(),
            expect: bullets.into(),
        })
        .collect();
    ops.push(EditOp::Mask {
        pane: main_pane(),
        anchor: TrackedRef { byte: 2017 },
        cells: 14,
        style: MaskStyle::Blur,
    });
    ops.push(EditOp::RedactInput {
        pane: main_pane(),
        events: SeqRange {
            start: 244,
            end: 271,
        },
    });
    ops.push(excise(104, 623));
    ops.push(excise(2743, 3074));
    let evaluation = evaluate(&session, &raw, ops);
    let done: Vec<&str> = evaluation
        .outcomes
        .iter()
        .map(|o| match o {
            Outcome::Replaced { .. } => "replaced",
            Outcome::Masked { .. } => "masked",
            Outcome::Redacted { .. } => "redacted",
            Outcome::Excised { .. } => "excised",
            _ => "other",
        })
        .collect();
    assert_eq!(
        done,
        [
            "replaced", "replaced", "replaced", "masked", "redacted", "excised", "excised"
        ]
    );
    let edited = edited_index(&session, &evaluation);
    assert!(edited.checkpoints().len() > 5);
    check_seek(&session, &edited, "edits");

    let session = fixture("osc133");
    let raw = SessionIndex::build(&session).unwrap();
    let evaluation = evaluate(&session, &raw, vec![excise(347, 653), excise(1915, 2186)]);
    check_seek(&session, &edited_index(&session, &evaluation), "osc133");
}

/// A random edit op over a session's pane `main`.
fn random_op(rng: &mut Rng, raw: &SessionIndex, session: &Session) -> EditOp {
    let pane = raw.pane(&main_pane()).unwrap();
    let len = pane.stream_len;
    let prompts: Vec<u64> = pane
        .marks
        .iter()
        .filter(|m| m.kind == tap_index::MarkKind::Prompt)
        .map(|m| m.byte)
        .chain([len])
        .collect();
    let duration = session.manifest.duration_us;
    let seqs = session.pane_events(&main_pane()).len() as u64;
    let texts = [
        "•••",
        "ab",
        "日本",
        "x",
        "",
        "\u{301}a",
        "\n",
        "abcdefghijklmn",
    ];
    match rng.below(9) {
        0 | 1 => {
            let a = prompts[rng.below(prompts.len() as u64) as usize];
            let b = if rng.below(4) == 0 {
                rng.below(len + 1)
            } else {
                prompts[rng.below(prompts.len() as u64) as usize]
            };
            excise(a.min(b), a.max(b) + u64::from(a == b))
        }
        2 => {
            // Part of a printed run, replaced by as many x's: usually exact.
            let runs = &pane.runs().unwrap().runs;
            let run = &runs[rng.below(runs.len() as u64) as usize];
            let start = run.bytes.start + rng.below(run.bytes.end - run.bytes.start);
            let end = (start + 1 + rng.below(8)).min(run.bytes.end);
            let text = "x".repeat((end - start) as usize);
            EditOp::ReplaceText {
                pane: main_pane(),
                bytes: ByteRange { start, end },
                expect: text.clone(),
                text,
            }
        }
        3 => {
            let start = rng.below(len.max(1));
            let end = (start + 1 + rng.below(20)).min(len);
            let text = texts[rng.below(texts.len() as u64) as usize].to_string();
            let expect = if rng.below(3) == 0 {
                "nope".into()
            } else {
                text.clone()
            };
            EditOp::ReplaceText {
                pane: main_pane(),
                bytes: ByteRange { start, end },
                text,
                expect,
            }
        }
        4 => {
            let start = rng.below(seqs + 1);
            EditOp::RedactInput {
                pane: main_pane(),
                events: SeqRange {
                    start,
                    end: start + rng.below(30),
                },
            }
        }
        5 => EditOp::Mask {
            pane: main_pane(),
            anchor: TrackedRef {
                byte: rng.below(len + 2),
            },
            cells: 1 + rng.below(30) as u32,
            style: MaskStyle::Pixelate,
        },
        6 => {
            let a = rng.below(duration + 1);
            let b = rng.below(duration + 1);
            EditOp::Skip {
                from: Src::Us(a.min(b)),
                to: Src::Us(a.max(b)),
            }
        }
        7 => EditOp::Hold {
            at: Src::Byte {
                pane: main_pane(),
                offset: rng.below(len.max(1)),
            },
            duration_us: rng.below(2_000_000),
        },
        _ => EditOp::CollapseIdle {
            range: None,
            threshold_us: 1_000_000,
            max_us: rng.below(1_000_000),
        },
    }
}

#[test]
fn random_edit_lists_never_panic_and_round_trip() {
    let mut rng = Rng(0x7a9e_11ed);
    let indexes: Vec<(Session, SessionIndex)> = FIXTURES
        .iter()
        .map(|name| {
            let session = fixture(name);
            let raw = SessionIndex::build(&session).unwrap();
            (session, raw)
        })
        .collect();
    let (mut ok, mut errors, mut excised, mut replaced) = (0, 0, 0, 0);
    for round in 0..150 {
        let (session, raw) = &indexes[rng.below(indexes.len() as u64) as usize];
        let entries: Vec<EditEntry> = (0..1 + rng.below(6))
            .map(|_| EditEntry {
                enabled: rng.below(8) != 0,
                op: random_op(&mut rng, raw, session),
            })
            .collect();
        let evaluation = match evaluate_entries(session, raw, &entries) {
            Ok(e) => e,
            Err(EvalError::Invalid { .. } | EvalError::Time(_)) => {
                errors += 1;
                continue;
            }
            Err(e) => panic!("round {round}: {e}"),
        };
        ok += 1;
        excised += evaluation
            .outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Excised { .. }))
            .count();
        replaced += evaluation
            .outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Replaced { .. }))
            .count();
        for pane in &raw.panes {
            let program = &evaluation.programs[&pane.pane];
            let raw_bytes = pane.stream_bytes();
            let played = program.render(raw_bytes);
            for s in 0..pane.stream_len {
                if let Location::Kept(p) = program.locate(s) {
                    assert_eq!(program.to_source(p), Origin::Source(s));
                    assert_eq!(played[p as usize], raw_bytes[s as usize]);
                }
            }
            for p in 0..program.len() {
                if let Origin::Source(s) = program.to_source(p) {
                    assert_eq!(program.locate(s), Location::Kept(p));
                }
            }
        }
        let edited = edited_index(session, &evaluation);
        let duration = session.manifest.duration_us;
        for pane in &edited.panes {
            for _ in 0..4 {
                let t = rng.below(duration + 1);
                let seek = edited.seek(&pane.pane, t).unwrap();
                let replay = edited.replay_from_start(&pane.pane, t).unwrap();
                assert_eq!(observe(&seek), observe(&replay), "round {round}: seek {t}");
            }
        }
    }
    println!("{ok} evaluated, {errors} invalid, {excised} excised, {replaced} replaced");
    // The random lists exercise the interesting paths.
    assert!(ok > 75 && errors > 0, "{ok} evaluated, {errors} invalid");
    assert!(
        excised > 15 && replaced > 15,
        "{excised} excised, {replaced} replaced"
    );
}
