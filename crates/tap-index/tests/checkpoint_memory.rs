//! Checkpoint storage grows with the session, not with its square, and
//! seek never has more than a bounded stretch of the recording to replay.

use tap_index::replay::{
    CHECKPOINT_ALLOWANCE, CHECKPOINT_BYTES, CHECKPOINT_RATIO, CHECKPOINT_RESIZES,
};
use tap_index::{IndexOptions, Scrollback, SessionIndex};

mod support;

use support::long_session::long_session;

/// Output bytes written in one call while replaying.
const SLICE: u64 = 64 * 1024;

struct Measured {
    stream_len: u64,
    checkpoints: u64,
    stored: u64,
    snapshots: u64,
}

fn measure(minutes: u64, options: IndexOptions) -> Measured {
    let session = long_session(minutes, 0, 0x7a9_2026);
    let index = SessionIndex::build_with(&session, options).unwrap();
    let pane = &index.panes[0];
    let events = session.pane_events(&pane.pane);
    let checkpoints: Vec<_> = index.checkpoints().pane(&pane.pane).collect();
    let largest = checkpoints.iter().map(|c| c.snapshot_len()).max().unwrap();

    // Seek replays at most the stretch between two checkpoints: the policy's
    // 256 KiB (plus the slice it is checked after), or as much output as
    // pays for the largest snapshot, and at most CHECKPOINT_RESIZES resizes.
    let widest = (CHECKPOINT_BYTES + SLICE).max(largest / CHECKPOINT_RATIO + SLICE);
    for pair in checkpoints.windows(2) {
        let bytes = pair[1].key.byte - pair[0].key.byte;
        let time = pair[1].at_us - pair[0].at_us;
        assert!(
            bytes <= widest || time <= 10_000_000 + 1_000_000,
            "{bytes} bytes and {time}µs between checkpoints at {} and {}",
            pair[0].key.byte,
            pair[1].key.byte
        );
        let resizes = events[pair[0].position.item..pair[1].position.item]
            .iter()
            .filter(|e| e.kind.type_name() == "resize")
            .count() as u64;
        assert!(resizes <= CHECKPOINT_RESIZES);
    }

    let measured = Measured {
        stream_len: pane.stream_len,
        checkpoints: checkpoints.len() as u64,
        stored: index.checkpoints().stored_bytes(),
        snapshots: index.checkpoints().snapshot_bytes(),
    };
    // The allowance caps what each checkpoint adds.
    assert!(
        measured.stored
            <= CHECKPOINT_ALLOWANCE * measured.checkpoints + CHECKPOINT_RATIO * measured.stream_len,
        "{} bytes stored for {} checkpoints over {} bytes of output",
        measured.stored,
        measured.checkpoints,
        measured.stream_len
    );
    measured
}

/// Doubling the session about doubles what checkpoints store.
fn assert_linear(options: IndexOptions) {
    let short = measure(6, options);
    let long = measure(12, options);
    let growth = long.stored as f64 / short.stored as f64;
    let output = long.stream_len as f64 / short.stream_len as f64;
    eprintln!(
        "{options:?}: {} → {} bytes of output, {} → {} checkpoints, {} → {} bytes stored ({growth:.2}×), {} → {} bytes of snapshots",
        short.stream_len,
        long.stream_len,
        short.checkpoints,
        long.checkpoints,
        short.stored,
        long.stored,
        short.snapshots,
        long.snapshots
    );
    assert!(
        growth < output * 1.3,
        "storage grew {growth:.2}× for {output:.2}× the output"
    );
}

#[test]
fn checkpoint_storage_grows_linearly_with_ghostty_scrollback() {
    // Below Ghostty's 50 MB limit nothing is pruned, so each snapshot holds
    // all the history so far, and only sharing keeps storage linear.
    assert_linear(IndexOptions::default());
}

#[test]
fn checkpoint_storage_grows_linearly_while_history_is_pruned() {
    assert_linear(IndexOptions {
        scrollback: Scrollback::Bytes(2_000_000),
    });
}
