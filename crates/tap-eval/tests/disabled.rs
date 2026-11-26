//! Disabled edits only skip evaluation: every save, export and round trip
//! keeps them exactly as given.

mod support;

use std::process::Command;

use tap_eval::Outcome;
use tap_index::SessionIndex;
use tap_model::edl::{
    ByteRange, EditEntry, EditOp, MaskStyle, Rate, Ratio, SegId, SeqRange, Src, SrcRange,
    TrackedRef,
};
use tap_model::{Edits, Session};

use support::*;

/// One disabled entry of every op kind (both kinds of rate), with byte
/// coordinates, as a user might have switched them off.
fn disabled() -> Vec<EditEntry> {
    let byte = |offset| Src::Byte {
        pane: main_pane(),
        offset,
    };
    let segment = SegId {
        pane: main_pane(),
        start: 347,
        end: 653,
    };
    [
        EditOp::Trim {
            r#in: Src::Us(1_000),
            out: byte(2000),
        },
        EditOp::Skip {
            from: byte(700),
            to: Src::Us(9_000_000),
        },
        EditOp::CollapseIdle {
            range: Some(SrcRange {
                from: byte(10),
                to: Src::Us(20_000_000),
            }),
            threshold_us: 1_500_000,
            max_us: 500_000,
        },
        EditOp::Speed {
            range: SrcRange {
                from: Src::Us(0),
                to: byte(1200),
            },
            rate: Rate::Constant(Ratio { num: 3, den: 2 }),
        },
        EditOp::Speed {
            range: SrcRange {
                from: Src::Us(0),
                to: Src::Us(5_000_000),
            },
            rate: Rate::Ramp {
                from: Ratio { num: 1, den: 1 },
                to: Ratio { num: 4, den: 1 },
            },
        },
        EditOp::RetimeTyping {
            segment: segment.clone(),
            cadence_us: 80_000,
            jitter_seed: Some(7),
        },
        EditOp::Hold {
            at: byte(900),
            duration_us: 1_000_000,
        },
        EditOp::Excise { segment },
        EditOp::ReplaceText {
            pane: main_pane(),
            bytes: ByteRange {
                start: 1100,
                end: 1105,
            },
            text: "xxxxx".into(),
            expect: "xxxxx".into(),
        },
        EditOp::RedactInput {
            pane: main_pane(),
            events: SeqRange { start: 3, end: 9 },
        },
        EditOp::Mask {
            pane: main_pane(),
            anchor: TrackedRef { byte: 1500 },
            cells: 4,
            style: MaskStyle::Solid {
                color: "#000000".into(),
            },
        },
    ]
    .into_iter()
    .map(|op| EditEntry { enabled: false, op })
    .collect()
}

fn with_edits(entries: Vec<EditEntry>) -> Session {
    let mut session = fixture("osc133");
    session.edits = Edits {
        edits: entries,
        ..Edits::default()
    };
    session.validate().unwrap();
    session
}

#[test]
fn disabled_edits_have_no_effect() {
    let session = with_edits(disabled());
    let raw = SessionIndex::build(&session).unwrap();
    let evaluation = tap_eval::evaluate(&session, &raw).unwrap();
    assert!(evaluation.outcomes.iter().all(|o| *o == Outcome::Disabled));
    assert!(evaluation.programs.values().all(|p| p.is_identity()));
    assert_eq!(
        evaluation.time_map.edited_duration(),
        session.manifest.duration_us
    );
}

#[test]
fn disabled_edits_survive_saves_and_exports() {
    let entries = disabled();
    // edits.json.
    let json = serde_json::to_string(&Edits {
        edits: entries.clone(),
        ..Edits::default()
    })
    .unwrap();
    let back: Edits = serde_json::from_str(&json).unwrap();
    assert_eq!(back.edits, entries);
    assert!(back.edits.iter().all(|e| !e.enabled));

    // A package, written and opened, and its edits saved again.
    let session = with_edits(entries.clone());
    let dir = std::env::temp_dir().join(format!("tap-eval-disabled-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    tap_model::package::create(&dir, &session).unwrap();
    let opened = tap_model::package::open(&dir).unwrap();
    assert_eq!(opened.edits.edits, entries);
    tap_model::package::save_edits(&dir, &opened.edits).unwrap();
    assert_eq!(tap_model::package::open(&dir).unwrap().edits.edits, entries);
    std::fs::remove_dir_all(&dir).unwrap();

    // An export of the edited session, alone and among enabled edits: the
    // enabled excision is baked in (its time cut stays as a skip), the
    // disabled entries stay as given, in order.
    let raw = SessionIndex::build(&session).unwrap();
    let evaluation = tap_eval::evaluate(&session, &raw).unwrap();
    let baked = tap_eval::bake::bake(&session, &raw, &entries, &evaluation);
    assert_eq!(baked.edits.edits, entries);
    let mut mixed = entries.clone();
    mixed.insert(3, EditEntry::new(excise(984, 1255)));
    let session = with_edits(mixed.clone());
    let evaluation = tap_eval::evaluate(&session, &raw).unwrap();
    assert!(matches!(evaluation.outcomes[3], Outcome::Excised { .. }));
    let baked = tap_eval::bake::bake(&session, &raw, &mixed, &evaluation);
    let mut expected = entries.clone();
    expected.insert(3, baked.edits.edits[3].clone());
    assert_eq!(baked.edits.edits[3].op.name(), "skip");
    assert_eq!(baked.edits.edits, expected);

    // `tap-eval apply` writes the same into a package.
    let edits_file =
        std::env::temp_dir().join(format!("tap-eval-disabled-{}.json", std::process::id()));
    std::fs::write(&edits_file, serde_json::to_string(&mixed).unwrap()).unwrap();
    let recording = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tap-index/tests/fixtures/osc133.tap");
    let out_dir =
        std::env::temp_dir().join(format!("tap-eval-disabled-out-{}", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_tap-eval"))
        .arg("apply")
        .arg(&recording)
        .arg("--edits")
        .arg(&edits_file)
        .arg("--out")
        .arg(format!("{}/", out_dir.display()))
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let written = tap_model::package::open(&out_dir).unwrap();
    assert_eq!(written.edits.edits, expected);
    std::fs::remove_dir_all(&out_dir).unwrap();
    std::fs::remove_file(&edits_file).unwrap();
}
