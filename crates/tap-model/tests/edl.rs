//! The edit list's JSON form.

use serde_json::{Value, json};
use tap_model::PaneId;
use tap_model::edits::Edits;
use tap_model::edl::{
    ByteRange, EditEntry, EditOp, MaskStyle, OpClass, Rate, Ratio, SegId, SeqRange, Src, SrcRange,
    TrackedRef,
};

fn pane() -> PaneId {
    PaneId::new("main").unwrap()
}

fn every_op() -> Vec<EditOp> {
    let byte = |offset| Src::Byte {
        pane: pane(),
        offset,
    };
    let seg = SegId {
        pane: pane(),
        start: 10,
        end: 20,
    };
    vec![
        EditOp::Trim {
            r#in: Src::Us(1),
            out: byte(9),
        },
        EditOp::Skip {
            from: byte(0),
            to: Src::Us(5),
        },
        EditOp::CollapseIdle {
            range: None,
            threshold_us: 1_500_000,
            max_us: 500_000,
        },
        EditOp::CollapseIdle {
            range: Some(SrcRange {
                from: Src::Us(0),
                to: Src::Us(9),
            }),
            threshold_us: 2,
            max_us: 1,
        },
        EditOp::Speed {
            range: SrcRange {
                from: Src::Us(0),
                to: Src::Us(9),
            },
            rate: Rate::Constant(Ratio { num: 3, den: 2 }),
        },
        EditOp::Speed {
            range: SrcRange {
                from: Src::Us(0),
                to: Src::Us(9),
            },
            rate: Rate::Ramp {
                from: Ratio { num: 1, den: 1 },
                to: Ratio { num: 4, den: 1 },
            },
        },
        EditOp::RetimeTyping {
            segment: seg.clone(),
            cadence_us: 80_000,
            jitter_seed: Some(7),
        },
        EditOp::RetimeTyping {
            segment: seg.clone(),
            cadence_us: 80_000,
            jitter_seed: None,
        },
        EditOp::Hold {
            at: Src::Us(3),
            duration_us: 1_000_000,
        },
        EditOp::Excise { segment: seg },
        EditOp::ReplaceText {
            pane: pane(),
            bytes: ByteRange { start: 4, end: 8 },
            text: "****".into(),
            expect: "$ ****".into(),
        },
        EditOp::RedactInput {
            pane: pane(),
            events: SeqRange { start: 2, end: 9 },
        },
        EditOp::Mask {
            pane: pane(),
            anchor: TrackedRef { byte: 40 },
            cells: 12,
            style: MaskStyle::Blur,
        },
        EditOp::Mask {
            pane: pane(),
            anchor: TrackedRef { byte: 40 },
            cells: 12,
            style: MaskStyle::Pixelate,
        },
        EditOp::Mask {
            pane: pane(),
            anchor: TrackedRef { byte: 40 },
            cells: 12,
            style: MaskStyle::Solid {
                color: "#000".into(),
            },
        },
    ]
}

#[test]
fn every_op_round_trips() {
    for op in every_op() {
        for (enabled, pre_bake) in [(true, false), (false, false), (false, true), (true, true)] {
            let entry = EditEntry {
                enabled,
                op: op.clone(),
                pre_bake,
            };
            let text = serde_json::to_string(&entry).unwrap();
            let back: EditEntry = serde_json::from_str(&text).unwrap();
            assert_eq!(back, entry, "{text}");
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["op"], op.name());
            assert_eq!(value.get("enabled").is_some(), !enabled, "{text}");
            assert_eq!(value.get("preBake").is_some(), pre_bake, "{text}");
        }
    }
}

#[test]
fn json_spelling_is_stable() {
    let entries: Vec<EditEntry> = serde_json::from_value(json!([
        { "op": "trim", "in": { "us": 1 }, "out": { "byte": { "pane": "main", "offset": 9 } } },
        { "op": "skip", "from": { "byte": { "pane": "main", "offset": 0 } }, "to": { "us": 5 } },
        { "op": "collapseIdle", "thresholdUs": 1500000, "maxUs": 500000 },
        { "op": "collapseIdle", "range": { "from": { "us": 0 }, "to": { "us": 9 } }, "thresholdUs": 2, "maxUs": 1 },
        { "op": "speed", "range": { "from": { "us": 0 }, "to": { "us": 9 } }, "rate": { "constant": { "num": 3, "den": 2 } } },
        { "op": "speed", "range": { "from": { "us": 0 }, "to": { "us": 9 } },
          "rate": { "ramp": { "from": { "num": 1, "den": 1 }, "to": { "num": 4, "den": 1 } } } },
        { "op": "retimeTyping", "segment": { "pane": "main", "start": 10, "end": 20 }, "cadenceUs": 80000, "jitterSeed": 7 },
        { "op": "retimeTyping", "segment": { "pane": "main", "start": 10, "end": 20 }, "cadenceUs": 80000 },
        { "op": "hold", "at": { "us": 3 }, "durationUs": 1000000 },
        { "op": "excise", "segment": { "pane": "main", "start": 10, "end": 20 } },
        { "op": "replaceText", "pane": "main", "bytes": { "start": 4, "end": 8 }, "text": "****", "expect": "$ ****" },
        { "op": "redactInput", "pane": "main", "events": { "start": 2, "end": 9 } },
        { "op": "mask", "pane": "main", "anchor": { "byte": 40 }, "cells": 12, "style": "blur" },
        { "op": "mask", "pane": "main", "anchor": { "byte": 40 }, "cells": 12, "style": "pixelate" },
        { "op": "mask", "pane": "main", "anchor": { "byte": 40 }, "cells": 12, "style": { "solid": { "color": "#000" } } },
    ]))
    .unwrap();
    let ops: Vec<EditOp> = entries.into_iter().map(|e| e.op).collect();
    assert_eq!(ops, every_op());
}

#[test]
fn classes_match_the_design() {
    let classes: Vec<OpClass> = every_op().iter().map(EditOp::class).collect();
    use OpClass::{ByteChanging as B, RenderTime as R, TimeOnly as T};
    assert_eq!(classes, [T, T, T, T, T, T, T, T, T, B, B, B, R, R, R]);
}

#[test]
fn bad_entries_are_rejected_with_reasons() {
    let cases = [
        (json!({ "op": "zoom" }), "unknown variant `zoom`"),
        (
            json!({ "op": "skip", "from": { "us": 0 }, "to": { "us": 1 }, "extra": 1 }),
            "unknown field `extra`",
        ),
        (
            json!({ "op": "skip", "from": { "us": 0 } }),
            "missing field `to`",
        ),
        (
            json!({ "op": "skip", "from": { "seconds": 0 }, "to": { "us": 1 } }),
            "unknown variant `seconds`",
        ),
        (
            json!({ "op": "skip", "from": { "us": -1 }, "to": { "us": 1 } }),
            "invalid value",
        ),
        (
            json!({ "op": "hold", "at": { "byte": { "pane": "../x", "offset": 0 } }, "durationUs": 1 }),
            "pane id",
        ),
        (
            json!({ "op": "hold", "at": { "us": 0 }, "durationUs": 1, "enabled": "no" }),
            "`enabled` must be a boolean",
        ),
        (
            json!({ "from": { "us": 0 }, "to": { "us": 1 } }),
            "missing field `op`",
        ),
        (json!([]), "invalid type"),
    ];
    for (value, expected) in cases {
        let message = serde_json::from_value::<EditEntry>(value.clone())
            .unwrap_err()
            .to_string();
        assert!(
            message.contains(expected),
            "{value}: expected {expected:?} in {message:?}"
        );
    }
}

#[test]
fn edits_document_round_trips() {
    let edits = Edits {
        edits: every_op().into_iter().map(EditEntry::new).collect(),
        ..Edits::default()
    };
    let text = serde_json::to_string_pretty(&edits).unwrap();
    assert_eq!(serde_json::from_str::<Edits>(&text).unwrap(), edits);
    let err = serde_json::from_str::<Edits>(
        r#"{"formatVersion":1,"edits":[],"overlays":[],"camera":[]}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field `camera`"), "{err}");
}
