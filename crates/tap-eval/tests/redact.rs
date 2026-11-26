//! "Redact this": bullets wherever the text can be rewritten (drawn text,
//! pieces drawn by a full-screen program, command lines in OSC 133;C,
//! titles, working directories), masks elsewhere, the keystrokes withheld,
//! and a report of what remains.

mod support;

use std::path::Path;

use tap_eval::redact;
use tap_eval::{CellRect, Outcome};
use tap_index::{MarkKind, SessionIndex};
use tap_model::EventKind;
use tap_model::edl::{ByteRange, EditOp, MaskStyle, SeqRange, TrackedRef};

use support::*;

const SECRET: &str = "sk-live-4f9a2b";
const BULLETS: &str = "••••••••••••••";

fn rewrite(start: u64, end: u64, text: &str, expect: &str) -> EditOp {
    EditOp::ReplaceText {
        pane: main_pane(),
        bytes: ByteRange { start, end },
        text: text.into(),
        expect: expect.into(),
    }
}

/// Every spelling of `secret` a viewer could recover: as is, and
/// percent-encoded (`cmdline_url`, OSC 7).
fn spellings(secret: &str) -> Vec<Vec<u8>> {
    let encoded: String = secret
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    vec![secret.as_bytes().to_vec(), encoded.into_bytes()]
}

fn holds(haystack: &[u8], secret: &str) -> bool {
    spellings(secret)
        .iter()
        .any(|s| haystack.windows(s.len()).any(|w| w == s.as_slice()))
}

#[test]
fn redacts_a_secret_everywhere_it_can() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    let redaction = redact::plan(&session, &raw, SECRET).unwrap();

    let seven = "•••••••";
    let ops: Vec<EditOp> = redaction.edits.iter().map(|e| e.op.clone()).collect();
    assert_eq!(
        ops,
        vec![
            // Typed (echoed), printed, printed with a colour change inside.
            rewrite(894, 908, BULLETS, BULLETS),
            rewrite(975, 989, BULLETS, BULLETS),
            rewrite(1281, 1300, BULLETS, BULLETS),
            // The full-screen program's two pieces, either side of a CUP.
            rewrite(2017, 2024, seven, seven),
            rewrite(2031, 2038, seven, seven),
            // The command line zsh's integration sent in OSC 133;C.
            rewrite(
                927,
                968,
                &format!("cmdline_url=echo%20token%20{}", "%E2%80%A2".repeat(14)),
                &format!("echo token {BULLETS}"),
            ),
            // The keys typed for `echo token sk-live-4f9a2b`, "s" to "b".
            EditOp::RedactInput {
                pane: main_pane(),
                events: SeqRange {
                    start: 244,
                    end: 271
                }
            },
        ]
    );
    let report = &redaction.report;
    assert_eq!(
        (report.replaced, report.masked, report.keystrokes),
        (6, 0, 14)
    );
    assert!(report.untraced.is_empty());
    assert!(report.remaining.is_empty(), "{:?}", report.remaining);

    // The transcript of the edited session shows bullets, command line
    // included, and the screen does too.
    let edited = edited_index(&session, &redaction.evaluation);
    let blocks = edited.pane(&main_pane()).unwrap().blocks();
    assert_eq!(blocks[3].output.as_deref(), Some("token ••••••••••••••"));
    assert_eq!(
        blocks[3].command.as_ref().map(|c| c.text.as_str()),
        Some("echo token ••••••••••••••")
    );
    assert_eq!(blocks[4].output.as_deref(), Some("key=••••••••••••••"));
    let term = edited.seek(&main_pane(), blocks[4].start_us).unwrap();
    let text = term.screen_text();
    assert!(text.contains("tap$ echo token ••••••••••••••\n"), "{text}");
}

#[test]
fn no_copy_of_the_secret_survives_outside_the_raw_events() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    // The raw events hold it, and keep holding it: they are never changed.
    assert!(holds(
        raw.pane(&main_pane()).unwrap().stream_bytes(),
        SECRET
    ));
    let redaction = redact::plan(&session, &raw, SECRET).unwrap();
    let evaluation = &redaction.evaluation;

    // The edited program: output bytes and the input it keeps.
    let edited = edited_index(&session, evaluation);
    let pane = edited.pane(&main_pane()).unwrap();
    assert!(!holds(pane.stream_bytes(), SECRET));
    for item in &pane.stream().items {
        if let tap_index::stream::ItemKind::Input { data, .. } = &item.kind {
            assert!(!holds(data, SECRET));
        }
    }
    // Transcript (text and JSON) and the index JSON.
    let transcript = edited.transcript();
    assert!(!holds(transcript.to_text().as_bytes(), SECRET));
    assert!(!holds(
        serde_json::to_string(&transcript).unwrap().as_bytes(),
        SECRET
    ));
    assert!(!holds(
        serde_json::to_string(&edited).unwrap().as_bytes(),
        SECRET
    ));
    for mark in &pane.marks {
        if let MarkKind::CommandStart { command: Some(c) } | MarkKind::Title { title: c } =
            &mark.kind
        {
            assert!(!c.contains(SECRET), "{c}");
        }
    }

    // Exports: the baked session as a package (every file of it) and as
    // .tap JSON (the plan leaves no edit to keep, so the JSON form holds
    // it), and indexes built from them.
    let mut entries = session.edits.edits.clone();
    entries.extend(redaction.edits.iter().cloned());
    let baked = tap_eval::bake::bake(&session, &raw, &entries, evaluation).unwrap();
    let dir = std::env::temp_dir().join(format!("tap-eval-nowhere-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    tap_model::package::create(&dir, &baked).unwrap();
    for file in walk(&dir) {
        let bytes = std::fs::read(&file).unwrap();
        assert!(!holds(&bytes, SECRET), "{}", file.display());
    }
    let reopened = tap_model::package::open(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(reopened, baked);
    let json = tap_model::legacy::export_json(&baked).unwrap();
    assert!(!holds(json.as_bytes(), SECRET));
    let reindexed = SessionIndex::build(&tap_model::legacy::import_json(&json).unwrap()).unwrap();
    assert!(!holds(
        serde_json::to_string(&reindexed).unwrap().as_bytes(),
        SECRET
    ));
    assert_eq!(reindexed.transcript(), transcript);
    // The input events keep their times, without their bytes.
    let events = baked.pane_events(&main_pane());
    assert_eq!(events.len(), session.pane_events(&main_pane()).len());
    let withheld = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::Input(i) if i.redacted == Some(true)))
        .count();
    assert_eq!(withheld, 14);

    // And the edits that do it name no secret either.
    assert!(!holds(
        serde_json::to_string(&redaction.edits).unwrap().as_bytes(),
        SECRET
    ));
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn titles_working_directories_and_quoted_command_lines_are_rewritten() {
    // A secret with a space, so it is spelled differently in each place:
    // quoted in `cmdline=` (printf %q), percent-encoded in OSC 7.
    let secret = "sk live";
    let p = b"\x1b]133;P\x07$ ";
    let session = synthetic(&[
        p,
        b"\x1b]133;C;cmdline=echo\\ sk\\ live\x07",
        b"\x1b]2;title sk live\x07\x1b]1;icon sk live\x1b\\\x1b]7;file://h/tmp/sk%20live\x07x\r\n",
        b"\x1b]133;D;0\x07",
        p,
    ]);
    let raw = SessionIndex::build(&session).unwrap();
    let marks = &raw.pane(&main_pane()).unwrap().marks;
    assert!(marks.iter().any(|m| m.kind
        == MarkKind::CommandStart {
            command: Some("echo sk live".into())
        }));
    let redaction = redact::plan(&session, &raw, secret).unwrap();
    let fields: Vec<String> = redaction
        .edits
        .iter()
        .filter_map(|e| match &e.op {
            EditOp::ReplaceText { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let dots = "•••••••";
    let encoded = "%E2%80%A2".repeat(7);
    assert_eq!(
        fields,
        [
            format!("cmdline_url=echo%20{encoded}"),
            format!("title {dots}"),
            format!("icon {dots}"),
            format!("file://h/tmp/{encoded}"),
        ]
    );
    assert!(
        redaction.evaluation.outcomes[..4]
            .iter()
            .all(|o| matches!(o, Outcome::Replaced { .. })),
        "{:?}",
        redaction.evaluation.outcomes
    );
    assert!(
        redaction.report.remaining.is_empty(),
        "{:?}",
        redaction.report.remaining
    );

    let edited = edited_index(&session, &redaction.evaluation);
    let pane = edited.pane(&main_pane()).unwrap();
    assert!(!holds(pane.stream_bytes(), secret));
    assert!(!holds(pane.stream_bytes(), "sk\\ live"));
    let term = edited
        .seek(&main_pane(), session.manifest.duration_us)
        .unwrap();
    assert_eq!(term.title(), format!("title {dots}"));
    assert_eq!(term.pwd(), format!("file://h/tmp/{encoded}").into_bytes());
    assert_eq!(
        pane.blocks()[0].command.as_ref().map(|c| c.text.clone()),
        Some(format!("echo {dots}"))
    );
}

#[test]
fn a_mask_follows_its_cells() {
    // The full-screen program's copy, masked instead of rewritten: the
    // mask covers its cells while the alternate screen shows, and is hidden
    // once the primary screen is back.
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    let evaluation = evaluate(
        &session,
        &raw,
        vec![EditOp::Mask {
            pane: main_pane(),
            anchor: TrackedRef { byte: 2017 },
            cells: 14,
            style: MaskStyle::Blur,
        }],
    );
    let Outcome::Masked { region } = evaluation.outcomes[0] else {
        panic!("{:?}", evaluation.outcomes[0]);
    };
    let region = &evaluation.masks[region];
    assert_eq!(
        region.spans[0].rects,
        vec![CellRect {
            row: 4,
            col: 4,
            cols: 14
        }]
    );
    assert!(region.spans[1..].iter().all(|s| s.rects.is_empty()));
    let pane = raw.pane(&main_pane()).unwrap();
    assert_eq!(region.spans[0].from_us, pane.stream().time_of_byte(2017));
    assert_eq!(
        region.spans.last().unwrap().to_us,
        session.manifest.duration_us
    );
}
