//! `tap-eval` end to end: preview, apply, plan-delete and redact on a real
//! recording.

mod support;

use std::path::PathBuf;
use std::process::Command;

use tap_index::SessionIndex;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tap-index/tests/fixtures")
        .join(format!("{name}.tap"))
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tap-eval"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tap-eval-cli-{}-{name}", std::process::id()))
}

#[test]
fn preview_without_edits_prints_the_unedited_transcript() {
    let path = fixture_path("osc133");
    let (ok, out, err) = run(&["preview", path.to_str().unwrap()]);
    assert!(ok, "{err}");
    let raw = SessionIndex::build(&support::fixture("osc133")).unwrap();
    let expected = format!(
        "# duration 23.1s -> 23.1s\n\n{}",
        raw.transcript().to_text()
    );
    assert_eq!(out, expected);
}

#[test]
fn preview_prints_what_each_edit_did_and_the_edited_transcript() {
    let edits = temp("edits.json");
    std::fs::write(
        &edits,
        r#"[
          {"op": "excise", "segment": {"pane": "main", "start": 104, "end": 623}},
          {"op": "excise", "segment": {"pane": "main", "start": 2371, "end": 2743}},
          {"op": "replaceText", "pane": "main", "bytes": {"start": 975, "end": 989},
           "text": "**************", "expect": "**************"}
        ]"#,
    )
    .unwrap();
    let path = fixture_path("edits");
    let (ok, out, err) = run(&[
        "preview",
        path.to_str().unwrap(),
        "--edits",
        edits.to_str().unwrap(),
    ]);
    std::fs::remove_file(&edits).unwrap();
    assert!(ok, "{err}");
    let head: Vec<&str> = out.lines().take(4).collect();
    assert_eq!(
        head,
        [
            "#0 excise: excised bytes 104..623 and 96 input events; fixups: cursor shape, modes, character sets, kitty keyboard flags, palette, title, working directory (\\e[4 q\\e[?1h\\e)0\\e[=5;1u\\e]4;1;rgb:12/34/56\\e\\\\\\e]2;tap title\\e\\\\\\e]7;file://fixture/tmp\\e\\\\)",
            "#1 excise: refused, played as a skip: {\"reason\":\"kittyImageUsedLater\",\"image\":\"i=31\",\"transmitted\":2587,\"used\":2935}; {\"reason\":\"absolutePositioning\",\"byte\":2929,\"sequence\":\"\\\\e[3;1H\"}",
            "#2 replaceText: rewritten in place (program bytes 535..549, 14 splices)",
            "# duration 28.5s -> 19.6s",
        ]
    );
    assert!(out.contains("    token **************\n"), "{out}");
    assert!(!out.contains("printf '\\e]2;tap title"), "{out}");
}

#[test]
fn plan_delete_and_redact_print_json() {
    let path = fixture_path("edits");
    let path = path.to_str().unwrap();
    let (ok, out, err) = run(&["plan-delete", path, "main", "1718", "2160"]);
    assert!(ok, "{err}");
    let plan: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(plan["plan"], "skip");
    assert_eq!(plan["reasons"][0]["reason"], "alternateScreen");
    assert_eq!(plan["op"]["op"], "skip");

    let (ok, out, err) = run(&["redact", path, "sk-live-4f9a2b"]);
    assert!(ok, "{err}");
    let redaction: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(redaction["edits"].as_array().unwrap().len(), 5);
    assert_eq!(redaction["report"]["replaced"], 3);
    assert_eq!(
        redaction["report"]["remaining"][0]["context"],
        "inside OSC 133;C (not drawn)"
    );
}

#[test]
fn apply_writes_the_edited_session() {
    let edits = temp("apply.json");
    std::fs::write(
        &edits,
        r#"[{"op": "excise", "segment": {"pane": "main", "start": 347, "end": 653}}]"#,
    )
    .unwrap();
    let path = fixture_path("osc133");
    let (ok, _, err) = run(&[
        "apply",
        path.to_str().unwrap(),
        "--edits",
        edits.to_str().unwrap(),
        "--out",
        temp("out.tap").to_str().unwrap(),
    ]);
    // The excision's time cut is an edit, which a .tap JSON file cannot hold.
    assert!(!ok);
    assert!(err.contains("write a package"), "{err}");
    let dir = temp("out");
    let out = format!("{}/", dir.display());
    let (ok, stdout, err) = run(&[
        "apply",
        path.to_str().unwrap(),
        "--edits",
        edits.to_str().unwrap(),
        "--out",
        &out,
    ]);
    std::fs::remove_file(&edits).unwrap();
    assert!(ok, "{err}");
    assert!(
        stdout.starts_with("#0 excise: excised bytes 347..653"),
        "{stdout}"
    );
    let baked = tap_model::package::open(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let text = SessionIndex::build(&baked).unwrap().transcript().to_text();
    assert!(!text.contains("nonexistent"), "{text}");
    assert!(text.contains("echo hello world"), "{text}");
}
