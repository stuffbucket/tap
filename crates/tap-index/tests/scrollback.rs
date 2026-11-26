//! What a snapshot does and does not reproduce once libghostty-vt's byte
//! limit on scrollback is pruning history. These pin down why the index's
//! terminals enforce the limit after every event, why checkpoints are only
//! taken within it, and why seek's guarantee about old history is "the
//! newest rows of the same history" rather than "the same rows" (see the
//! crate README, "Checkpoints and seek").

use tap_index::vt::{Scrollback, Terminal};

/// About five 400 KB pages of history.
const LIMIT: usize = 2_000_000;

fn terminal() -> Terminal {
    Terminal::new(100, 30, Scrollback::Bytes(LIMIT)).unwrap()
}

fn plain(line: usize) -> Vec<u8> {
    format!("line {line} {}\r\n", "x".repeat(line % 90)).into_bytes()
}

/// Whether `a` and `b` agree on everything but the oldest history: the
/// shorter's whole screen is the newest rows of the longer's.
fn newest_rows_agree(a: &Terminal, b: &Terminal) -> bool {
    let rows = a.total_rows().min(b.total_rows());
    a.text_newest(rows) == b.text_newest(rows)
        && a.vt_dump_newest(rows) == b.vt_dump_newest(rows)
        && a.cursor() == b.cursor()
}

fn restore(term: &Terminal) -> Terminal {
    Terminal::restore(&term.snapshot().unwrap()).unwrap()
}

#[test]
fn a_terminal_over_its_limit_restores_with_less_history() {
    let mut live = terminal();
    for line in 0..10_000 {
        live.write(&plain(line));
    }
    // Pruning keeps it within the limit while it grows a page at a time.
    assert!(live.within_scrollback_limit());
    let restored = restore(&live);
    assert_eq!(restored.total_rows(), live.total_rows());
    assert_eq!(restored.vt_dump(), live.vt_dump());

    // A wider screen reflows history into pages of fewer, wider rows, so the
    // same history takes more pages. Nothing is dropped until the screen
    // next grows a page, but the snapshot decoder refuses every page of
    // history past the limit.
    live.resize(133, 40).unwrap();
    assert!(!live.within_scrollback_limit());
    let restored = restore(&live);
    assert!(
        restored.total_rows() < live.total_rows(),
        "{} rows restored of {}",
        restored.total_rows(),
        live.total_rows()
    );
    assert!(newest_rows_agree(&restored, &live));

    // Enforcing the limit drops those pages from the live terminal too, and
    // a restore is exact again.
    live.enforce_scrollback_limit();
    assert!(live.within_scrollback_limit());
    let restored = restore(&live);
    assert_eq!(restored.total_rows(), live.total_rows());
    assert_eq!(restored.vt_dump(), live.vt_dump());
    assert_eq!(restored.snapshot().unwrap(), live.snapshot().unwrap());
}

#[test]
fn a_restored_terminal_can_keep_fewer_old_rows() {
    // Every line carries a hyperlink of its own, so pages need more room
    // for hyperlinks than a standard page has. A snapshot does not carry how
    // much: the decoder rebuilds each page's allocation from size hints,
    // and a new page's room depends on what the last few pages used. So
    // after a restore, pages hold and count different numbers of rows and
    // bytes, and the limit drops old pages at different moments.
    let link = |line: usize| {
        format!(
            "\x1b]8;;https://example.com/{line}\x1b\\link {line}\x1b]8;;\x1b\\ {}\r\n",
            "y".repeat(line % 50)
        )
        .into_bytes()
    };
    let mut live = terminal();
    for line in 0..5_000 {
        live.write(&link(line));
    }
    assert!(live.within_scrollback_limit());
    let mut restored = restore(&live);
    assert_eq!(restored.vt_dump(), live.vt_dump());

    let mut apart = 0;
    for line in 5_000..10_000 {
        live.write(&link(line));
        restored.write(&link(line));
        if line % 50 == 0 {
            assert!(newest_rows_agree(&restored, &live), "at line {line}");
            apart = apart.max(restored.total_rows().abs_diff(live.total_rows()));
        }
    }
    // If libghostty-vt ever carries page capacity in snapshots, this fails
    // and the README's seek guarantee can say "the same rows".
    assert!(apart > 0, "the restored terminal kept the same history");
}
