//! Byte coordinates count the playback stream, and no edit splices bytes
//! whose recorded and playback forms differ.

mod support;

use std::path::Path;

use tap_eval::{NeedsMask, Outcome, Refusal};
use tap_index::SessionIndex;
use tap_model::edl::{ByteRange, EditEntry, EditOp, MaskStyle, Src, TrackedRef};
use tap_model::{EventKind, Session};

use support::*;

/// Two commands; the first prints a Kitty file transfer that capture
/// normalised: event 3's `data` names a file, its `playbackData` carries
/// the pixel (34 bytes instead of 46).
fn kitty_file() -> Session {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitty-file.tap");
    tap_index::open(&path).unwrap()
}

#[test]
fn byte_coordinates_count_the_playback_stream() {
    let session = kitty_file();
    let raw = SessionIndex::build(&session).unwrap();
    let pane = raw.pane(&main_pane()).unwrap();
    let recorded: usize = session
        .pane_events(&main_pane())
        .iter()
        .map(|e| match &e.kind {
            EventKind::Output(o) => o.data.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(pane.stream_len, 131);
    assert_eq!(recorded, 143);
    // tap-model's time map resolves every offset to the time the index
    // gives the same byte.
    let timeline = session.timeline();
    for offset in 0..pane.stream_len {
        let at = timeline
            .resolve(&Src::Byte {
                pane: main_pane(),
                offset,
            })
            .unwrap();
        assert_eq!(at, pane.stream().time_of_byte(offset), "byte {offset}");
    }
    assert!(
        timeline
            .resolve(&Src::Byte {
                pane: main_pane(),
                offset: 131
            })
            .is_err()
    );
}

#[test]
fn edits_crossing_a_normalised_transfer_are_refused() {
    let session = kitty_file();
    let raw = SessionIndex::build(&session).unwrap();
    let transfer = 24..58;
    let ops = vec![
        excise(0, 75),
        EditOp::ReplaceText {
            pane: main_pane(),
            bytes: ByteRange { start: 20, end: 30 },
            text: "xxxxxxxxxx".into(),
            expect: "xxxxxxxxxx".into(),
        },
        EditOp::Mask {
            pane: main_pane(),
            anchor: TrackedRef { byte: 40 },
            cells: 2,
            style: MaskStyle::Blur,
        },
        // The second command touches only plain events.
        EditOp::ReplaceText {
            pane: main_pane(),
            bytes: ByteRange {
                start: 99,
                end: 104,
            },
            text: "PLAIN".into(),
            expect: "PLAIN".into(),
        },
    ];
    let evaluation = evaluate(&session, &raw, ops);
    assert_eq!(
        evaluation.outcomes[0],
        Outcome::Refused {
            reasons: vec![Refusal::PlaybackDiffers {
                event: 3,
                bytes: transfer
            }]
        }
    );
    assert_eq!(
        evaluation.outcomes[1],
        Outcome::NeedsMask {
            reason: NeedsMask::PlaybackDiffers { event: 3 }
        }
    );
    assert!(
        matches!(&evaluation.outcomes[2], Outcome::Inert { reason } if reason.contains("event 3"))
    );
    assert!(
        matches!(evaluation.outcomes[3], Outcome::Replaced { .. }),
        "{:?}",
        evaluation.outcomes[3]
    );
    let edited = edited_index(&session, &evaluation);
    assert_eq!(
        edited.pane(&main_pane()).unwrap().blocks()[1]
            .output
            .as_deref(),
        Some("PLAIN text")
    );
}

#[test]
fn an_export_keeps_untouched_events_as_recorded() {
    let session = kitty_file();
    let raw = SessionIndex::build(&session).unwrap();
    let entries = vec![EditEntry::new(excise(75, 121))];
    let evaluation = tap_eval::evaluate_entries(&session, &raw, &entries).unwrap();
    assert!(matches!(evaluation.outcomes[0], Outcome::Excised { .. }));
    let baked = tap_eval::bake::bake(&session, &raw, &entries, &evaluation);
    let before = session.pane_events(&main_pane());
    let after = baked.pane_events(&main_pane());
    // The transfer keeps its recorded bytes and its playback bytes.
    assert_eq!(after[3], before[3]);
    assert!(matches!(&after[3].kind, EventKind::Output(o) if o.playback.is_some()));
    // Re-indexing the export plays what the edited session plays.
    let reindexed = SessionIndex::build(&baked).unwrap();
    let edited = edited_index(&session, &evaluation);
    assert_eq!(
        reindexed.pane(&main_pane()).unwrap().stream_bytes(),
        edited.pane(&main_pane()).unwrap().stream_bytes()
    );
}
