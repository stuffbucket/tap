//! A bake never makes protected text visible, and edits it cannot move
//! onto the new bytes never touch other bytes.

mod support;

use std::ops::Range;

use tap_eval::bake::{BakeError, bake};
use tap_eval::{MaskSpan, Outcome, evaluate, evaluate_entries};
use tap_index::SessionIndex;
use tap_model::edl::{ByteRange, EditEntry, EditOp, MaskStyle, PRE_BAKE, SegId, Src, TrackedRef};
use tap_model::event::{EventKind, Playback, TextEncoding};
use tap_model::{Session, TimeMap};

use support::*;

const SECRET: &str = "sk-live-4f9a2b";

fn replace(range: Range<u64>, text: &str) -> EditOp {
    EditOp::ReplaceText {
        pane: main_pane(),
        bytes: ByteRange {
            start: range.start,
            end: range.end,
        },
        text: text.into(),
        expect: text.into(),
    }
}

fn mask(byte: u64, cells: u32) -> EditOp {
    EditOp::Mask {
        pane: main_pane(),
        anchor: TrackedRef { byte },
        cells,
        style: MaskStyle::Blur,
    }
}

fn with_edits(mut session: Session, entries: Vec<EditEntry>) -> Session {
    session.edits.edits = entries;
    session.validate().unwrap();
    session
}

/// Where each resolved mask covers cells, over time.
fn coverage(session: &Session) -> Vec<Vec<MaskSpan>> {
    let raw = SessionIndex::build(session).unwrap();
    let evaluation = evaluate(session, &raw).unwrap();
    let mut out: Vec<Vec<MaskSpan>> = evaluation.masks.iter().map(|m| m.spans.clone()).collect();
    out.sort_by_key(|spans| format!("{spans:?}"));
    out
}

#[test]
fn a_bake_masks_the_same_cells_over_the_same_times() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    // The redaction plan, with the full-screen copy masked instead of
    // rewritten piece by piece, and a rewrite of the printed "日本語" that
    // needs a mask (three cells for six).
    let mut entries: Vec<EditEntry> = tap_eval::redact::plan(&session, &raw, SECRET)
        .unwrap()
        .edits
        .into_iter()
        .filter(|e| !matches!(&e.op, EditOp::ReplaceText { bytes, .. } if bytes.start >= 2017 && bytes.end <= 2038))
        .collect();
    entries.push(EditEntry::new(mask(2017, 14)));
    let wide = {
        let stream = raw.pane(&main_pane()).unwrap().stream_bytes();
        let needle = "日本語".as_bytes();
        let at = stream
            .windows(needle.len())
            .enumerate()
            .filter(|(_, w)| *w == needle)
            .nth(1)
            .unwrap()
            .0 as u64;
        at..at + needle.len() as u64
    };
    entries.push(EditEntry::new(replace(wide.clone(), "abc")));
    let session = with_edits(session, entries.clone());
    let evaluation = evaluate(&session, &raw).unwrap();
    assert!(matches!(evaluation.outcomes[5], Outcome::Masked { .. }));
    assert!(matches!(evaluation.outcomes[6], Outcome::NeedsMask { .. }));
    let before = coverage(&session);
    assert_eq!(before.len(), 1);

    let baked = bake(&session, &raw, &entries, &evaluation).unwrap();
    // The rewrites and withheld keys are in the bytes; the mask and the
    // rewrite that needed one are masks.
    let ops: Vec<&str> = baked.edits.edits.iter().map(|e| e.op.name()).collect();
    assert_eq!(ops, ["mask", "mask"]);
    let after = coverage(&baked);
    // The mask covers the same cells over the same times…
    assert!(after.contains(&before[0]), "{after:?}");
    // …and the text the rewrite could not hide is now covered: the six
    // cells "日本語" takes after "wide: ", from when it was printed.
    let printed = raw
        .pane(&main_pane())
        .unwrap()
        .stream()
        .time_of_byte(wide.start);
    let covered = after
        .iter()
        .find(|spans| spans[0].from_us == printed)
        .expect("a mask over the wide text");
    assert_eq!(covered[0].rects.len(), 1);
    assert_eq!((covered[0].rects[0].col, covered[0].rects[0].cols), (6, 6));
    assert_eq!(covered.last().unwrap().to_us, session.manifest.duration_us);

    // A bake of the bake changes nothing more.
    let raw_baked = SessionIndex::build(&baked).unwrap();
    let again = evaluate(&baked, &raw_baked).unwrap();
    let twice = bake(&baked, &raw_baked, &baked.edits.edits, &again).unwrap();
    assert_eq!(twice.edits.edits, baked.edits.edits);
    assert_eq!(coverage(&twice), after);
}

#[test]
fn a_mask_that_cannot_be_carried_refuses_the_bake() {
    // Text drawn inside an event whose recorded bytes differ from the
    // bytes it plays: a rewrite there needs a mask, and no mask can anchor
    // there, so baking would show it.
    let mut session = tap_index::open(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitty-file.tap"),
    )
    .unwrap();
    let events = session.events.get_mut(&main_pane()).unwrap();
    let EventKind::Output(output) = &mut events[3].kind else {
        unreachable!()
    };
    let mut played = output.playback.as_ref().unwrap().data.clone();
    played.extend_from_slice(b"visible");
    output.playback = Some(Playback {
        data: played,
        encoding: TextEncoding::Base64,
    });
    let raw = SessionIndex::build(&session).unwrap();
    let entries = vec![EditEntry::new(replace(58..65, "xxxxxxx"))];
    assert_eq!(
        &raw.pane(&main_pane()).unwrap().stream_bytes()[58..65],
        b"visible"
    );
    let evaluation = evaluate_entries(&session, &raw, &entries).unwrap();
    assert!(matches!(evaluation.outcomes[0], Outcome::NeedsMask { .. }));
    match bake(&session, &raw, &entries, &evaluation) {
        Err(BakeError::Uncarried(list)) => {
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].entry, 0);
            assert!(list[0].reason.contains("event 3"), "{}", list[0].reason);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_disabled_edit_reenabled_after_a_bake_names_the_same_bytes_or_is_refused() {
    let session = fixture("osc133");
    let raw = SessionIndex::build(&session).unwrap();
    let source = raw.pane(&main_pane()).unwrap().stream_bytes().to_vec();
    let find = |needle: &str| {
        let at = source
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .unwrap() as u64;
        at..at + needle.len() as u64
    };
    // "cat: …" is printed by the command excised below; "after clear" and
    // "three" come later, "hello" before.
    let gone = find("No such file");
    let later = find("after clear\r\n").start..find("after clear\r\n").start + 11;
    let three = find("three\r\n").start..find("three\r\n").start + 5;
    let hello = find("hello world\r\n").start..find("hello world\r\n").start + 5;
    let entries = vec![
        EditEntry::new(excise(347, 653)),
        EditEntry::disabled(replace(hello.clone(), "HELLO")),
        EditEntry::disabled(replace(gone.clone(), "xxxxxxxxxxxx")),
        EditEntry::disabled(replace(later.clone(), "AFTER CLEAR")),
        EditEntry::disabled(mask(three.start, 5)),
        EditEntry::disabled(excise(1915, 2186)),
        EditEntry::disabled(EditOp::Skip {
            from: Src::Byte {
                pane: main_pane(),
                offset: gone.start,
            },
            to: Src::Byte {
                pane: main_pane(),
                offset: later.start,
            },
        }),
    ];
    let session = with_edits(session, entries.clone());
    let evaluation = evaluate(&session, &raw).unwrap();
    let baked = bake(&session, &raw, &entries, &evaluation).unwrap();
    assert_eq!(baked.edits.edits.len(), entries.len());
    let marked: Vec<bool> = baked.edits.edits.iter().map(|e| e.pre_bake).collect();
    assert_eq!(marked, [false, false, true, false, false, false, false]);

    // Switch every one on in the export.
    let reenabled: Vec<EditEntry> = baked
        .edits
        .edits
        .iter()
        .map(|e| EditEntry {
            enabled: true,
            ..e.clone()
        })
        .collect();
    let baked = with_edits(baked, reenabled.clone());
    let raw_baked = SessionIndex::build(&baked).unwrap();
    let target = raw_baked.pane(&main_pane()).unwrap().stream_bytes();

    // Each moved edit names the bytes it named before.
    let same = |old: &Range<u64>, new: &ByteRange| {
        assert_eq!(
            &target[new.start as usize..new.end as usize],
            &source[old.start as usize..old.end as usize]
        );
    };
    let EditOp::ReplaceText { bytes, .. } = &reenabled[1].op else {
        unreachable!()
    };
    same(&hello, bytes);
    let EditOp::ReplaceText { bytes, .. } = &reenabled[3].op else {
        unreachable!()
    };
    same(&later, bytes);
    let EditOp::Mask { anchor, .. } = &reenabled[4].op else {
        unreachable!()
    };
    assert_eq!(
        &target[anchor.byte as usize..anchor.byte as usize + 5],
        b"three"
    );
    let EditOp::Excise {
        segment: SegId { start, end, .. },
    } = &reenabled[5].op
    else {
        unreachable!()
    };
    assert_eq!(&target[*start as usize..*end as usize], &source[1915..2186]);
    // The skip's start was excised: it starts at the moment it did.
    let EditOp::Skip { from, .. } = &reenabled[6].op else {
        unreachable!()
    };
    let pane = raw.pane(&main_pane()).unwrap();
    assert_eq!(*from, Src::Us(pane.stream().time_of_byte(gone.start)));

    let evaluation = evaluate(&baked, &raw_baked).unwrap();
    let outcome = |i: usize| &evaluation.outcomes[i];
    assert!(matches!(outcome(1), Outcome::Replaced { .. }));
    assert!(matches!(outcome(3), Outcome::Replaced { .. }));
    assert!(matches!(outcome(4), Outcome::Masked { .. }));
    assert!(
        matches!(outcome(5), Outcome::Excised { .. }),
        "{:?}",
        outcome(5)
    );
    // The one whose bytes are gone is refused, with the reason.
    assert_eq!(
        *outcome(2),
        Outcome::PreBake {
            reason: PRE_BAKE.into()
        }
    );
    // It touches nothing: every splice comes from another edit.
    let program = &evaluation.programs[&main_pane()];
    assert!(program.splices().iter().all(|s| s.op != 2));
    let edited = edited_index(&baked, &evaluation);
    let text = edited.transcript().to_text();
    assert!(text.contains("HELLO world"), "{text}");
    assert!(text.contains("AFTER CLEAR"), "{text}");

    // tap-model's time map refuses it too.
    let timed = vec![EditEntry {
        pre_bake: true,
        ..EditEntry::new(EditOp::Skip {
            from: Src::Us(0),
            to: Src::Us(1),
        })
    }];
    assert_eq!(
        TimeMap::build(&timed, &baked.timeline()).unwrap_err(),
        tap_model::EvalError::Invalid {
            index: 0,
            op: "skip",
            reason: PRE_BAKE.into()
        }
    );
}
