//! ReplaceText: text rewritten in place when it was drawn as plain text and
//! the replacement takes the same cells, else a typed reason to mask it.

mod support;

use std::ops::Range;

use tap_eval::search::find_text;
use tap_eval::{NeedsMask, Outcome};
use tap_index::SessionIndex;
use tap_model::Session;
use tap_model::edl::{ByteRange, EditOp};

use support::*;

const SECRET: &str = "sk-live-4f9a2b";
const BULLETS: &str = "••••••••••••••";

fn replace(range: &Range<u64>, text: &str, expect: &str) -> EditOp {
    EditOp::ReplaceText {
        pane: main_pane(),
        bytes: ByteRange {
            start: range.start,
            end: range.end,
        },
        text: text.into(),
        expect: expect.into(),
    }
}

fn setup() -> (Session, SessionIndex) {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    (session, raw)
}

/// The raw bytes of the first `n`-th occurrence of `needle` in the stream.
fn bytes_of(raw: &SessionIndex, needle: &[u8], n: usize) -> Range<u64> {
    let stream = raw.pane(&main_pane()).unwrap().stream_bytes();
    let at = stream
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .nth(n)
        .unwrap()
        .0 as u64;
    at..at + needle.len() as u64
}

#[test]
fn finds_the_secret_as_plain_text_and_on_the_screen() {
    let (_, raw) = setup();
    let found = find_text(raw.pane(&main_pane()).unwrap(), SECRET).unwrap();
    let plain: Vec<_> = found.iter().filter_map(|o| o.plain.clone()).collect();
    // Typed (echoed key by key), printed, and printed with SGR inside.
    assert_eq!(plain, vec![894..908, 975..989, 1281..1300]);
    // The full-screen program's copy is drawn in two pieces: found on the
    // screen, traced to the byte that drew its first cell.
    let screen: Vec<_> = found.iter().filter(|o| o.plain.is_none()).collect();
    assert_eq!(screen.len(), 1);
    assert_eq!(screen[0].anchor, Some(2017));
    assert!(screen[0].alternate);
    assert_eq!(screen[0].cells, 14);
}

#[test]
fn rewrites_plain_text_and_checks_expect() {
    let (session, raw) = setup();
    let printed = 975..989;
    let evaluation = evaluate(&session, &raw, vec![replace(&printed, BULLETS, BULLETS)]);
    let Outcome::Replaced { program, splices } = &evaluation.outcomes[0] else {
        panic!("{:?}", evaluation.outcomes[0]);
    };
    // One splice per character, so the text still appears as it did.
    assert_eq!(*splices, 14);
    let edited = edited_index(&session, &evaluation);
    let pane = edited.pane(&main_pane()).unwrap();
    assert_eq!(
        &pane.stream_bytes()[program.start as usize..program.end as usize],
        BULLETS.as_bytes()
    );
    let block = &pane.blocks()[3];
    assert_eq!(block.output.as_deref(), Some("token ••••••••••••••"));
    // The typed copy is untouched.
    assert!(
        pane.stream_bytes()
            .windows(SECRET.len())
            .any(|w| w == SECRET.as_bytes())
    );
}

#[test]
fn keeps_sgr_inside_the_replaced_text() {
    let (session, raw) = setup();
    let with_sgr = 1281..1300;
    assert_eq!(
        &raw.pane(&main_pane()).unwrap().stream_bytes()[1281..1300],
        b"sk-live-\x1b[31m4f9a2b"
    );
    let evaluation = evaluate(&session, &raw, vec![replace(&with_sgr, BULLETS, BULLETS)]);
    let Outcome::Replaced { program, .. } = &evaluation.outcomes[0] else {
        panic!("{:?}", evaluation.outcomes[0]);
    };
    let edited = edited_index(&session, &evaluation);
    let pane = edited.pane(&main_pane()).unwrap();
    assert_eq!(
        &pane.stream_bytes()[program.start as usize..program.end as usize],
        "••••••••\x1b[31m••••••".as_bytes()
    );
    assert_eq!(
        pane.blocks()[4].output.as_deref(),
        Some("key=••••••••••••••")
    );
    // The last six are still red on screen (palette colour 1, which the
    // formatter writes as 38;5;1).
    let term = edited
        .seek(&main_pane(), pane.segments[5].start_us)
        .unwrap();
    let dump = String::from_utf8_lossy(&term.vt_dump()).into_owned();
    assert!(
        dump.contains("key=••••••••\x1b[0m\x1b[38;5;1m••••••"),
        "{dump}"
    );
}

#[test]
fn width_mismatch_needs_a_mask() {
    let (session, raw) = setup();
    // The printed "日本語" (6 cells), not the typed one.
    let wide = bytes_of(&raw, "日本語".as_bytes(), 1);
    let evaluation = evaluate(
        &session,
        &raw,
        vec![replace(&wide, "abc", "abc"), replace(&wide, "日本", "日本")],
    );
    assert_eq!(
        evaluation.outcomes[0],
        Outcome::NeedsMask {
            reason: NeedsMask::WidthMismatch {
                original: 6,
                replacement: 3
            }
        }
    );
    assert_eq!(
        evaluation.outcomes[1],
        Outcome::NeedsMask {
            reason: NeedsMask::WidthMismatch {
                original: 6,
                replacement: 4
            }
        }
    );
    // Six narrow characters fill the same cells: two per wide one.
    let evaluation = evaluate(&session, &raw, vec![replace(&wide, "abcdef", "abcdef")]);
    assert!(
        matches!(evaluation.outcomes[0], Outcome::Replaced { splices: 3, .. }),
        "{:?}",
        evaluation.outcomes[0]
    );
    let edited = edited_index(&session, &evaluation);
    assert_eq!(
        edited.pane(&main_pane()).unwrap().blocks()[5]
            .output
            .as_deref(),
        Some("wide: abcdef ok")
    );
}

#[test]
fn cursor_addressed_text_needs_a_mask() {
    let (session, raw) = setup();
    // The full-screen program draws "sk-live", moves the cursor, then
    // "-4f9a2b".
    let tui = bytes_of(&raw, b"sk-live\x1b[5;12H-4f9a2b", 0);
    assert_eq!(tui.start, 2017);
    let evaluation = evaluate(&session, &raw, vec![replace(&tui, BULLETS, BULLETS)]);
    assert_eq!(
        evaluation.outcomes[0],
        Outcome::NeedsMask {
            reason: NeedsMask::CursorAddressed { byte: 2031 }
        }
    );
    // A range that starts inside an escape sequence is not plain text.
    let csi = bytes_of(&raw, b"[5;12H", 1);
    let evaluation = evaluate(&session, &raw, vec![replace(&csi, "abcdef", "abcdef")]);
    assert_eq!(
        evaluation.outcomes[0],
        Outcome::NeedsMask {
            reason: NeedsMask::NotPlainText { byte: csi.start }
        }
    );
}

#[test]
fn a_wrong_expect_needs_a_mask() {
    let (session, raw) = setup();
    let evaluation = evaluate(&session, &raw, vec![replace(&(975..989), BULLETS, "xxxx")]);
    assert_eq!(
        evaluation.outcomes[0],
        Outcome::NeedsMask {
            reason: NeedsMask::ExpectMismatch {
                found: BULLETS.into()
            }
        }
    );
    // Nothing was rewritten.
    assert!(evaluation.programs[&main_pane()].is_identity());
}

#[test]
fn an_excision_supersedes_a_replacement_inside_it() {
    let (session, raw) = setup();
    let evaluation = evaluate(
        &session,
        &raw,
        vec![replace(&(975..989), BULLETS, BULLETS), excise(839, 1105)],
    );
    assert_eq!(evaluation.outcomes[0], Outcome::Superseded { by: 1 });
    assert!(matches!(evaluation.outcomes[1], Outcome::Excised { .. }));
}
