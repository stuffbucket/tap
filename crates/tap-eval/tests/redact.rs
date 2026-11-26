//! "Redact this": bullets where the text can be rewritten, masks where it
//! cannot, the keystrokes withheld, and a report of what remains.

mod support;

use tap_eval::redact;
use tap_eval::{CellRect, Outcome};
use tap_index::SessionIndex;
use tap_model::EventKind;
use tap_model::edl::{EditOp, MaskStyle, SeqRange, TrackedRef};

use support::*;

const SECRET: &str = "sk-live-4f9a2b";

#[test]
fn redacts_a_secret_everywhere_it_can() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    let redaction = redact::plan(&session, &raw, SECRET).unwrap();

    let ops: Vec<&str> = redaction.edits.iter().map(|e| e.op.name()).collect();
    assert_eq!(
        ops,
        [
            "replaceText",
            "replaceText",
            "replaceText",
            "mask",
            "redactInput"
        ]
    );
    assert_eq!(
        redaction.edits[3].op,
        EditOp::Mask {
            pane: main_pane(),
            anchor: TrackedRef { byte: 2017 },
            cells: 14,
            style: MaskStyle::Blur
        }
    );
    // The keys typed for `echo token sk-live-4f9a2b`, from "s" to "b".
    assert_eq!(
        redaction.edits[4].op,
        EditOp::RedactInput {
            pane: main_pane(),
            events: SeqRange {
                start: 244,
                end: 271
            }
        }
    );
    let report = &redaction.report;
    assert_eq!(
        (report.replaced, report.masked, report.keystrokes),
        (3, 1, 14)
    );
    assert!(report.untraced.is_empty());
    // What is left: the command line zsh's integration sent in OSC 133;C,
    // never drawn but in the bytes.
    let remaining: Vec<(&str, &str)> = report
        .remaining
        .iter()
        .map(|r| (r.lane, r.context.as_str()))
        .collect();
    assert_eq!(remaining, [("output", "inside OSC 133;C (not drawn)")]);

    // The mask follows the full-screen program's cells while it shows, and
    // is hidden once the primary screen is back.
    let evaluation = &redaction.evaluation;
    let Outcome::Masked { region } = evaluation.outcomes[3] else {
        panic!("{:?}", evaluation.outcomes[3]);
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

    // The transcript of the edited session shows bullets.
    let edited = edited_index(&session, evaluation);
    let blocks = edited.pane(&main_pane()).unwrap().blocks();
    assert_eq!(blocks[3].output.as_deref(), Some("token ••••••••••••••"));
    assert_eq!(blocks[4].output.as_deref(), Some("key=••••••••••••••"));
    // On screen the typed command reads bullets too…
    let term = edited.seek(&main_pane(), blocks[4].start_us).unwrap();
    let text = term.screen_text();
    assert!(text.contains("tap$ echo token ••••••••••••••\n"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    // …but the transcript's command line comes from the OSC 133;C mark,
    // which the report lists as remaining.
    assert_eq!(
        blocks[3].command.as_ref().map(|c| c.text.as_str()),
        Some("echo token sk-live-4f9a2b")
    );
}

#[test]
fn exports_of_the_redacted_session_hold_no_keystrokes_or_text() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    let redaction = redact::plan(&session, &raw, SECRET).unwrap();
    let mut entries = session.edits.edits.clone();
    entries.extend(redaction.edits.iter().cloned());
    let baked = tap_eval::bake::bake(&session, &raw, &entries, &redaction.evaluation);

    let events = baked.pane_events(&main_pane());
    let typed: Vec<u8> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Input(i) => Some(i.data.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!typed.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
    let withheld: Vec<_> = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::Input(i) if i.redacted == Some(true)))
        .collect();
    assert_eq!(withheld.len(), 14);
    // Times and order are kept.
    assert_eq!(events.len(), session.pane_events(&main_pane()).len());

    // The export's output holds the secret only where the report said. The
    // mask is an edit, so the export is a package (the legacy JSON form
    // holds no edits).
    let dir = std::env::temp_dir().join(format!("tap-eval-redact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    tap_model::package::create(&dir, &baked).unwrap();
    let reimported = tap_model::package::open(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(reimported, baked);
    let reindexed = SessionIndex::build(&reimported).unwrap();
    let bytes = reindexed.pane(&main_pane()).unwrap().stream_bytes();
    let hits: Vec<usize> = bytes
        .windows(SECRET.len())
        .enumerate()
        .filter(|(_, w)| *w == SECRET.as_bytes())
        .map(|(i, _)| i)
        .collect();
    let reported: Vec<usize> = redaction
        .report
        .remaining
        .iter()
        .map(|r| r.at.start as usize)
        .collect();
    assert_eq!(hits, reported);
    // Re-indexing the export gives the edited session's transcript.
    let edited = edited_index(&session, &redaction.evaluation);
    assert_eq!(reindexed.transcript(), edited.transcript());
    // The mask still applies to the export, at its new offset.
    let masks: Vec<_> = reimported
        .edits
        .edits
        .iter()
        .filter(|e| e.op.name() == "mask")
        .collect();
    assert_eq!(masks.len(), 1);
}
