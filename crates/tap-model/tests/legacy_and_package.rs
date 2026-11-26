//! Import of the single-file `.tap` JSON form, export back, and packages.

mod common;

use std::fs;

use common::{TempDir, legacy_projects};
use serde_json::{Value, json};
use tap_model::edl::{EditEntry, EditOp, Src};
use tap_model::event::{EventKind, TextEncoding};
use tap_model::legacy::{export_json, import_json};
use tap_model::{Error, package, tlog};

fn read(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn finds_the_fixtures() {
    let names: Vec<String> = legacy_projects()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for expected in [
        "every-event.tap",
        "kitty-animation.tap",
        "split-capture.tap",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing from {names:?}"
        );
    }
}

#[test]
fn json_round_trips_exactly() {
    for path in legacy_projects() {
        let original: Value = serde_json::from_str(&read(&path)).unwrap();
        let session =
            import_json(&read(&path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let exported = export_json(&session).unwrap();
        assert!(exported.ends_with("}\n"));
        let back: Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(back, original, "{} does not round-trip", path.display());
    }
}

#[test]
fn package_round_trips_exactly() {
    let tmp = TempDir::new("pkg-roundtrip");
    for (i, path) in legacy_projects().iter().enumerate() {
        let session = import_json(&read(path)).unwrap();
        let dir = tmp.path().join(format!("{i}.tap"));
        package::create(&dir, &session).unwrap();
        let reopened = package::open(&dir).unwrap();
        assert_eq!(reopened, session, "{}", path.display());
        let original: Value = serde_json::from_str(&read(path)).unwrap();
        let back: Value = serde_json::from_str(&export_json(&reopened).unwrap()).unwrap();
        assert_eq!(
            back,
            original,
            "{} does not round-trip through a package",
            path.display()
        );
    }
}

#[test]
fn import_keeps_order_encodings_and_metadata() {
    let session = import_json(&read(
        &common::repo_root().join("crates/tap-model/tests/fixtures/every-event.tap"),
    ))
    .unwrap();
    let main = tap_model::PaneId::new("main").unwrap();
    let events = session.pane_events(&main);
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        (0..events.len() as u64).collect::<Vec<_>>()
    );
    let EventKind::Output(first) = &events[1].kind else {
        panic!()
    };
    assert_eq!(first.data_encoding, TextEncoding::Unspecified);
    assert!(
        String::from_utf8(first.data.clone())
            .unwrap()
            .contains("漢字 🎉")
    );
    let EventKind::Output(binary) = &events[3].kind else {
        panic!()
    };
    assert_eq!(binary.data, [0xff, 0x00, 0xfe]);
    assert_eq!(
        session.manifest.metadata.shell_integration.as_deref(),
        Some(&["fish".to_owned(), "zsh".to_owned()][..])
    );
    assert_eq!(session.manifest.engine_fingerprint, "libghostty-vt 1.0.1");
    assert!(session.edits.edits.is_empty());
    assert_eq!(session.edits.overlays.len(), 3);
}

#[test]
fn export_writes_non_utf8_as_base64() {
    let mut session = import_json(&read(
        &common::repo_root().join("crates/tap-model/tests/fixtures/split-capture.tap"),
    ))
    .unwrap();
    let left = tap_model::PaneId::new("left").unwrap();
    let EventKind::Output(o) = &mut session.events.get_mut(&left).unwrap()[0].kind else {
        panic!()
    };
    o.data = vec![0xc3, 0x28];
    o.data_encoding = TextEncoding::Utf8;
    let out: Value = serde_json::from_str(&export_json(&session).unwrap()).unwrap();
    assert_eq!(out["panes"][0]["events"][0]["data"], "wyg=");
    assert_eq!(out["panes"][0]["events"][0]["encoding"], "base64");
}

fn minimal() -> Value {
    json!({
        "version": 1, "title": "t", "createdAt": "x", "durationUs": 100,
        "canvas": { "width": 640, "height": 480, "background": "#000", "fps": 60 },
        "panes": [{ "id": "main", "cols": 80, "rows": 24, "fontSize": 16,
                    "layout": { "x": 0, "y": 0, "width": 1, "height": 1 },
                    "events": [{ "atUs": 0, "type": "output", "data": "hi" }] }],
        "overlays": [],
        "metadata": { "terminal": "ghostty", "terminalEngine": "e", "timebase": "microseconds" }
    })
}

fn import_error(edit: impl FnOnce(&mut Value)) -> String {
    let mut value = minimal();
    edit(&mut value);
    match import_json(&value.to_string()) {
        Ok(_) => panic!("accepted {value}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn minimal_project_imports() {
    import_json(&minimal().to_string()).unwrap();
}

#[test]
fn invalid_projects_fail_with_clear_messages() {
    type Edit = Box<dyn FnOnce(&mut Value)>;
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|v| v["version"] = json!(2)),
            "unsupported tap project version: 2",
        ),
        (
            Box::new(|v| v["version"] = json!("1")),
            "unsupported tap project version: \"1\"",
        ),
        (
            Box::new(|v| v["extra"] = json!(true)),
            "unknown field `extra`",
        ),
        (
            Box::new(|v| {
                v.as_object_mut().unwrap().remove("title");
            }),
            "missing field `title`",
        ),
        (Box::new(|v| v["durationUs"] = json!(-1)), "durationUs"),
        (
            Box::new(|v| v["durationUs"] = json!(9_007_199_254_740_992_u64)),
            "safe integer",
        ),
        (
            Box::new(|v| v["canvas"]["width"] = json!(0)),
            "canvas.width",
        ),
        (Box::new(|v| v["canvas"]["fps"] = json!(241)), "canvas.fps"),
        (
            Box::new(|v| v["canvas"]["height"] = json!(8193)),
            "canvas.height",
        ),
        (
            Box::new(|v| v["canvas"]["zoom"] = json!(1)),
            "canvas: unknown field `zoom`",
        ),
        (Box::new(|v| v["panes"] = json!([])), "at least one pane"),
        (
            Box::new(|v| v["panes"][0]["id"] = json!("../etc")),
            "panes[0].id",
        ),
        (
            Box::new(|v| v["panes"][0]["id"] = json!("CON")),
            "reserved device name",
        ),
        (
            Box::new(|v| v["panes"][0]["cols"] = json!(65536)),
            "panes[0].cols",
        ),
        (
            Box::new(|v| v["panes"][0]["fontSize"] = json!(0)),
            "fontSize",
        ),
        (
            Box::new(|v| v["panes"][0]["layout"]["width"] = json!(2)),
            "fit inside the normalized canvas",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["type"] = json!("bell")),
            "panes[0].events[0]: unknown variant `bell`",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["color"] = json!("red")),
            "panes[0].events[0]: unknown field `color`",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["atUs"] = json!(1.5)),
            "panes[0].events[0]",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["atUs"] = json!(101)),
            "beyond project duration",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["encoding"] = json!("base64")),
            "not valid base64",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["encoding"] = json!("hex")),
            "unknown variant `hex`",
        ),
        (
            Box::new(|v| v["panes"][0]["events"][0]["playbackEncoding"] = json!("utf8")),
            "playbackEncoding but no playbackData",
        ),
        (
            Box::new(
                |v| v["panes"][0]["events"] = json!([{ "atUs": 5, "type": "marker", "label": "a" }, { "atUs": 4, "type": "marker", "label": "b" }]),
            ),
            "sorted",
        ),
        (
            Box::new(|v| {
                v["panes"][0]["events"] =
                    json!([{ "atUs": 0, "type": "resize", "cols": 0, "rows": 1 }])
            }),
            "cols must be",
        ),
        (
            Box::new(|v| {
                v["panes"][0]["events"] =
                    json!([{ "atUs": 0, "type": "scroll", "delta": 1_000_001 }])
            }),
            "scroll delta",
        ),
        (
            Box::new(|v| {
                v["panes"][0]["events"] =
                    json!([{ "atUs": 0, "type": "input", "data": "", "origin": "mouse" }])
            }),
            "unknown variant `mouse`",
        ),
        (
            Box::new(|v| {
                let p = v["panes"][0].clone();
                let mut q = p.clone();
                q["id"] = json!("MAIN");
                v["panes"] = json!([p, q]);
            }),
            "unique id",
        ),
        (
            Box::new(
                |v| v["overlays"] = json!([{ "id": "a", "type": "text", "startUs": 5, "endUs": 4, "x": 0, "y": 0, "text": "" }]),
            ),
            "invalid time range",
        ),
        (
            Box::new(|v| v["presentation"] = json!({ "padding": 1 })),
            "presentation: missing field",
        ),
        (
            Box::new(|v| v["metadata"]["terminal"] = json!("kitty")),
            "metadata: unknown variant `kitty`",
        ),
    ];
    for (edit, expected) in cases {
        let message = import_error(edit);
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message:?}"
        );
    }
}

#[test]
fn malformed_json_fails_cleanly() {
    for text in [
        "",
        "[]",
        "null",
        "{",
        "{\"version\":1",
        "{\"version\":1,\"title\":\"\\ud800\"}",
        "\u{0}",
    ] {
        assert!(import_json(text).is_err(), "{text:?}");
    }
}

#[test]
fn export_refuses_sessions_with_edits() {
    let mut session = import_json(&minimal().to_string()).unwrap();
    session.edits.edits.push(EditEntry::new(EditOp::Skip {
        from: Src::Us(0),
        to: Src::Us(1),
    }));
    let message = export_json(&session).unwrap_err().to_string();
    assert!(message.contains("cannot hold edits"), "{message}");
}

#[test]
fn create_refuses_an_existing_path() {
    let tmp = TempDir::new("pkg-exists");
    let session = import_json(&minimal().to_string()).unwrap();
    let dir = tmp.path().join("s.tap");
    fs::create_dir(&dir).unwrap();
    let err = package::create(&dir, &session).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    // Nothing is left behind beside it.
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn package_layout_is_as_documented() {
    let tmp = TempDir::new("pkg-layout");
    let session = import_json(&read(
        &common::repo_root().join("crates/tap-model/tests/fixtures/split-capture.tap"),
    ))
    .unwrap();
    let dir = tmp.path().join("Session.tap");
    package::create(&dir, &session).unwrap();
    let mut names: Vec<String> = walk(&dir);
    names.sort();
    assert_eq!(
        names,
        [
            "edits.json",
            "events/left.tlog",
            "events/right.tlog",
            "manifest.json"
        ]
    );
    let manifest: Value = serde_json::from_str(&read(&dir.join("manifest.json"))).unwrap();
    assert_eq!(manifest["formatVersion"], 1);
    assert_eq!(manifest["panes"][0]["fontSize"], json!(16));
    assert_eq!(manifest["engineFingerprint"], "libghostty-vt 1.0.1");
    let edits: Value = serde_json::from_str(&read(&dir.join("edits.json"))).unwrap();
    assert_eq!(
        edits,
        json!({ "formatVersion": 1, "edits": [], "overlays": [] })
    );
    let log = fs::read(dir.join("events/left.tlog")).unwrap();
    assert_eq!(&log[..8], b"\x89TLOG\r\n\x1a");
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            out.extend(walk(&path).into_iter().map(|n| format!("{name}/{n}")));
        } else {
            out.push(name);
        }
    }
    out
}

#[test]
fn open_ignores_derived_dirs_and_reports_damage_precisely() {
    let tmp = TempDir::new("pkg-damage");
    let session = import_json(&read(
        &common::repo_root().join("crates/tap-model/tests/fixtures/kitty-animation.tap"),
    ))
    .unwrap();
    let dir = tmp.path().join("k.tap");
    package::create(&dir, &session).unwrap();
    for derived in package::DERIVED_DIRS {
        fs::create_dir_all(dir.join(derived).join("junk")).unwrap();
        fs::write(dir.join(derived).join("junk/x.bin"), b"whatever").unwrap();
    }
    assert_eq!(package::open(&dir).unwrap(), session);

    let log_path = dir.join("events/main.tlog");
    let mut bytes = fs::read(&log_path).unwrap();
    let records: Vec<_> = tlog::Records::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let (pos, _) = records[100];
    bytes[pos.offset as usize + 12] ^= 0x40;
    fs::write(&log_path, &bytes).unwrap();
    let message = package::open(&dir).unwrap_err().to_string();
    assert!(message.contains("main.tlog"), "{message}");
    assert!(
        message.contains(&format!("record 100 at byte {}", pos.offset)),
        "{message}"
    );
    let recovery = tlog::recover(&bytes);
    assert_eq!(recovery.events.len(), 100);
    assert_eq!(recovery.valid_len, pos.offset);
}

#[test]
fn open_rejects_unlisted_logs_and_newer_versions() {
    let tmp = TempDir::new("pkg-strict");
    let session = import_json(&minimal().to_string()).unwrap();
    let dir = tmp.path().join("s.tap");
    package::create(&dir, &session).unwrap();

    tlog::write_file(&dir.join("events/ghost.tlog"), &[]).unwrap();
    let message = package::open(&dir).unwrap_err().to_string();
    assert!(message.contains("ghost.tlog"), "{message}");
    fs::remove_file(dir.join("events/ghost.tlog")).unwrap();

    let manifest_path = dir.join("manifest.json");
    let original = read(&manifest_path);
    fs::write(
        &manifest_path,
        original.replace("\"formatVersion\": 1", "\"formatVersion\": 2"),
    )
    .unwrap();
    let message = package::open(&dir).unwrap_err().to_string();
    assert!(
        message.contains("format version 2 is not supported"),
        "{message}"
    );
    fs::write(&manifest_path, &original).unwrap();

    fs::remove_file(dir.join("events/main.tlog")).unwrap();
    let message = package::open(&dir).unwrap_err().to_string();
    assert!(message.contains("main.tlog"), "{message}");
}

#[test]
fn save_edits_replaces_only_edits() {
    let tmp = TempDir::new("pkg-edits");
    let session = import_json(&minimal().to_string()).unwrap();
    let dir = tmp.path().join("s.tap");
    package::create(&dir, &session).unwrap();
    let log_before = fs::read(dir.join("events/main.tlog")).unwrap();

    let mut edits = session.edits.clone();
    edits.edits.push(EditEntry::new(EditOp::Trim {
        r#in: Src::Us(10),
        out: Src::Us(90),
    }));
    package::save_edits(&dir, &edits).unwrap();
    let reopened = package::open(&dir).unwrap();
    assert_eq!(reopened.edits, edits);
    assert_eq!(reopened.time_map().unwrap().edited_duration(), 80);
    assert_eq!(fs::read(dir.join("events/main.tlog")).unwrap(), log_before);

    let mut bad = edits.clone();
    bad.edits.push(EditEntry::new(EditOp::Skip {
        from: Src::Byte {
            pane: tap_model::PaneId::new("nope").unwrap(),
            offset: 0,
        },
        to: Src::Us(5),
    }));
    let message = package::save_edits(&dir, &bad).unwrap_err().to_string();
    assert!(message.contains("unknown pane \"nope\""), "{message}");
    assert_eq!(package::open(&dir).unwrap().edits, edits);
    // No temp files are left behind.
    assert!(
        walk(&dir).iter().all(|n| !n.contains(".tmp")),
        "{:?}",
        walk(&dir)
    );
}
