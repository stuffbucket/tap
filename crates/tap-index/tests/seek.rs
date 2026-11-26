//! Golden equivalence: seeking through a checkpoint gives the terminal that
//! replaying from the start does, as the crate README's "Checkpoints and
//! seek" defines it:
//!
//! - the same active screen and terminal state: size, cursor, active screen,
//!   title, and every mode, colour and style the VT dump carries;
//! - the same scrollback, except that when the two keep different amounts
//!   of history, the shorter one is the newest part of the longer one: its
//!   rows after its oldest line are the longer one's newest rows (screen
//!   text and VT dump identical), and its text, soft wraps joined, ends the
//!   longer one's.
//!
//! Sessions that never reach the scrollback limit must match exactly.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use tap_index::{IndexOptions, Position, Scrollback, SessionIndex, vt};
use tap_model::Session;

/// Deterministic pseudo-random numbers (xorshift64*), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Everything observable about a terminal that seek must reproduce.
#[derive(Debug, PartialEq, Eq)]
struct State {
    text: String,
    /// Screen and scrollback as VT with every extra: styles, modes, palette,
    /// cursor style, scrolling region, tab stops, character sets.
    vt: Vec<u8>,
    cursor: (u16, u16),
    size: (u16, u16),
    alternate: bool,
    title: String,
}

/// The state of a terminal, its scrollback cut to the newest `rows` rows of
/// the screen.
fn state(term: &vt::Terminal, rows: usize) -> State {
    State {
        text: term.text_newest(rows),
        vt: term.vt_dump_newest(rows),
        cursor: term.cursor(),
        size: term.size(),
        alternate: term.on_alternate_screen(),
        title: term.title(),
    }
}

/// Says how two states differ, briefly.
fn difference(a: &State, b: &State) -> String {
    let mut out = Vec::new();
    if a.text != b.text {
        let (la, lb): (Vec<_>, Vec<_>) = (a.text.lines().collect(), b.text.lines().collect());
        let first = la
            .iter()
            .zip(&lb)
            .position(|(x, y)| x != y)
            .unwrap_or(la.len().min(lb.len()));
        out.push(format!(
            "text ({} vs {} lines) first differs at line {first}: {:?} vs {:?}",
            la.len(),
            lb.len(),
            la.get(first),
            lb.get(first)
        ));
    }
    for (name, differs) in [
        ("cursor", a.cursor != b.cursor),
        ("size", a.size != b.size),
        ("screen", a.alternate != b.alternate),
        ("title", a.title != b.title),
        ("vt", a.vt != b.vt),
    ] {
        if differs {
            out.push(name.to_string());
        }
    }
    format!(
        "{} (cursor {:?} vs {:?})",
        out.join("; "),
        a.cursor,
        b.cursor
    )
}

/// What [`check`] saw.
#[derive(Debug, Default)]
struct Seen {
    /// Seeks compared.
    seeks: usize,
    /// Seeks that resumed from a checkpoint after the start of the pane.
    resumed: usize,
    /// Seeks whose re-encoded snapshot bytes differed from the replay's.
    /// libghostty-vt's snapshot encoding is not canonical: a restored
    /// terminal can encode the same state with different bytes (for
    /// example, after reflow its pages are laid out differently), so these
    /// are counted rather than compared.
    snapshot_differs: usize,
    /// Seeks where the seek and the replay kept different amounts of
    /// scrollback.
    history_differs: usize,
    /// The largest difference in scrollback rows between the two.
    most_rows_apart: usize,
}

/// Seeks every pane to `samples` random times, every checkpoint time and,
/// with `every_event`, every event time and the microsecond either side,
/// and compares with a replay from the start.
fn check(
    session: &Session,
    options: IndexOptions,
    samples: usize,
    seed: u64,
    every_event: bool,
) -> Seen {
    let index = SessionIndex::build_with(session, options).unwrap();
    let mut rng = Rng(seed);
    let mut seen = Seen::default();
    for pane in &index.panes {
        let mut times: Vec<u64> = (0..samples)
            .map(|_| rng.below(session.manifest.duration_us + 1))
            .collect();
        for event in session
            .pane_events(&pane.pane)
            .iter()
            .filter(|_| every_event)
        {
            times.extend([event.at_us.saturating_sub(1), event.at_us, event.at_us + 1]);
        }
        for checkpoint in index.checkpoints().pane(&pane.pane) {
            times.extend([checkpoint.at_us, checkpoint.at_us + 1]);
        }
        times.sort_unstable();
        times.dedup();
        for at in times {
            let sought_term = index.seek(&pane.pane, at).unwrap();
            let replayed_term = index.replay_from_start(&pane.pane, at).unwrap();
            seen.seeks += 1;

            // Everything both kept: the whole screen when they kept the same
            // history. Otherwise, all of the shorter one but its oldest line
            // must be the newest rows of the longer one; that line may have
            // lost its start to pruning and been reflowed without it, so
            // only its text must be the end of the longer one's line.
            let (s_rows, r_rows) = (sought_term.total_rows(), replayed_term.total_rows());
            let rows = if s_rows == r_rows {
                s_rows
            } else {
                let shorter = if s_rows < r_rows {
                    &sought_term
                } else {
                    &replayed_term
                };
                s_rows.min(r_rows) - shorter.oldest_line_rows()
            };
            let sought = state(&sought_term, rows);
            let replayed = state(&replayed_term, rows);
            assert!(
                sought == replayed,
                "pane {:?} at {at}µs: seek differs from replay in the newest {rows} rows ({s_rows} vs {r_rows} kept): {}",
                pane.pane,
                difference(&sought, &replayed)
            );
            let (s_text, r_text) = (sought_term.screen_text(), replayed_term.screen_text());
            if s_rows == r_rows {
                assert!(
                    s_text == r_text && sought_term.vt_dump() == replayed_term.vt_dump(),
                    "pane {:?} at {at}µs: seek differs from replay",
                    pane.pane
                );
                if sought_term.at_ground()
                    && sought_term.snapshot().unwrap() != replayed_term.snapshot().unwrap()
                {
                    seen.snapshot_differs += 1;
                }
            } else {
                assert!(
                    s_text.ends_with(&r_text) || r_text.ends_with(&s_text),
                    "pane {:?} at {at}µs: neither history ends the other",
                    pane.pane
                );
                seen.history_differs += 1;
                seen.most_rows_apart = seen.most_rows_apart.max(s_rows.abs_diff(r_rows));
            }

            let target = Position {
                item: session
                    .pane_events(&pane.pane)
                    .partition_point(|e| e.at_us <= at),
                intra: 0,
            };
            if index
                .checkpoints()
                .at_or_before(&pane.pane, target)
                .is_some_and(|c| c.key.byte > 0)
            {
                seen.resumed += 1;
            }
        }
    }
    seen
}

fn fixture(path: PathBuf) -> Session {
    tap_index::open(&path).unwrap()
}

fn here(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn seek_matches_replay_on_recorded_shell_sessions() {
    for name in ["osc133", "no-integration", "redacted"] {
        let session = fixture(here(&format!("tests/fixtures/{name}.tap")));
        let seen = check(&session, IndexOptions::default(), 200, 0x7a9_1d3c, true);
        assert!(
            seen.resumed > seen.seeks * 9 / 10,
            "{name}: only {} of {} seeks used a checkpoint",
            seen.resumed,
            seen.seeks
        );
        // Far below the limit, the history is the same, and without resizes
        // the snapshot bytes are the same too.
        assert_eq!(seen.history_differs, 0, "{name}");
        assert_eq!(seen.snapshot_differs, 0, "{name}");
    }
}

#[test]
fn seek_matches_replay_on_split_panes_and_kitty_graphics() {
    // Recorded by tap-model's tests: two panes, and a Kitty animation.
    for name in ["split-capture", "kitty-animation"] {
        let session = fixture(here(&format!("../tap-model/tests/fixtures/{name}.tap")));
        let seen = check(&session, IndexOptions::default(), 50, 0x5eed, true);
        assert!(seen.resumed > 0, "{name}");
        assert_eq!(seen.history_differs, 0, "{name}");
        assert_eq!(seen.snapshot_differs, 0, "{name}");
    }
}

/// A long synthetic session: 1.5 MiB of styled output, one 600 KiB event,
/// characters and sequences split across events, resizes, the alternate
/// screen, clears and a reset, over two minutes. With `hyperlinks`, every
/// line starts with an OSC 8 link of its own.
fn synthetic(hyperlinks: bool) -> Session {
    let mut rng = Rng(42);
    let mut events = Vec::new();
    let mut at = 0u64;
    let mut push = |at: u64, value: serde_json::Value| {
        let mut value = value;
        value["atUs"] = at.into();
        events.push(value);
    };
    let output = |bytes: &[u8]| {
        serde_json::json!({
            "type": "output",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "encoding": "base64",
        })
    };
    let mut pending: Vec<u8> = Vec::new();
    for line in 0..20_000u32 {
        let colour = 31 + line % 7;
        // With hyperlinks, the line's first word is a link of its own.
        let (open, close) = if hyperlinks {
            (
                format!("\x1b]8;;https://example.com/{line}\x1b\\"),
                "\x1b]8;;\x1b\\",
            )
        } else {
            (String::new(), "")
        };
        pending.extend_from_slice(
            format!(
                "\x1b[{colour}m{open}line{close} {line} ✓ 日本語 {}\x1b[0m\r\n",
                "x".repeat((line % 90) as usize)
            )
            .as_bytes(),
        );
        match line % 2_500 {
            700 => pending.extend_from_slice(b"\x1b[?1049h\x1b[Halt screen\x1b[5;5Hhere"),
            900 => pending.extend_from_slice(b"\x1b[?1049l"),
            1_200 => pending.extend_from_slice(b"\x1b]2;title here\x07\x1b[H\x1b[2J"),
            2_400 if line > 10_000 => pending.extend_from_slice(b"\x1bc"),
            _ => {}
        }
        if line == 9_000 {
            // One big event, flushed as is.
            let big: Vec<u8> = (0..600 * 1024 / 16)
                .flat_map(|i| format!("{i:>14}\r\n").into_bytes())
                .collect();
            push(at, output(&big));
            at += 50_000;
        }
        if pending.len() > 2_000 + rng.below(6_000) as usize {
            // Cut at an arbitrary byte, so UTF-8 characters and escape
            // sequences are split between events.
            let cut = 1 + rng.below(pending.len() as u64 - 1) as usize;
            let rest = pending.split_off(cut);
            push(at, output(&pending));
            pending = rest;
            at += rng.below(400_000);
            if rng.below(40) == 0 {
                push(
                    at,
                    serde_json::json!({"type": "resize", "cols": 60 + rng.below(80), "rows": 10 + rng.below(30)}),
                );
            }
            if rng.below(10) == 0 {
                push(
                    at,
                    serde_json::json!({"type": "input", "data": "x", "origin": "keyboard"}),
                );
            }
        }
    }
    push(at, output(&pending));
    let project = serde_json::json!({
        "version": 1,
        "title": "synthetic",
        "createdAt": "2026-11-25T00:00:00.000Z",
        "durationUs": at,
        "canvas": {"width": 1280, "height": 720, "background": "#000000", "fps": 30},
        "panes": [{
            "id": "main", "cols": 100, "rows": 30, "fontSize": 14,
            "layout": {"x": 0, "y": 0, "width": 1, "height": 1},
            "events": events,
        }],
        "overlays": [],
        "metadata": {"terminal": "ghostty", "terminalEngine": "test", "timebase": "microseconds"},
    });
    tap_model::legacy::import_json(&project.to_string()).unwrap()
}

#[test]
fn seek_matches_replay_on_a_long_session_with_periodic_checkpoints() {
    let session = synthetic(false);
    assert!(session.manifest.duration_us > 120_000_000);
    let index = SessionIndex::build(&session).unwrap();
    let pane = &index.panes[0];
    assert!(pane.stream_len > 1_500_000);

    // Checkpoints at least every 256 KiB or 10 s (they are taken at the
    // first ground point after either budget runs out).
    let checkpoints: Vec<_> = index.checkpoints().pane(&pane.pane).collect();
    for pair in checkpoints.windows(2) {
        let bytes = pair[1].key.byte - pair[0].key.byte;
        let time = pair[1].at_us - pair[0].at_us;
        assert!(
            bytes <= 256 * 1024 + 64 * 1024 || time <= 10_000_000 + 400_000,
            "{} bytes and {time}µs between checkpoints at {} and {}",
            bytes,
            pair[0].key.byte,
            pair[1].key.byte
        );
    }
    // Some were taken inside the 600 KiB event.
    assert!(checkpoints.iter().filter(|c| c.position.intra > 0).count() >= 2);
    for checkpoint in &checkpoints {
        assert_eq!(checkpoint.key.engine, index.engine);
        assert_eq!(index.checkpoints().get(&checkpoint.key), Some(*checkpoint));
    }

    // Snapshot bytes may differ here (resizes reflow history into pages laid
    // out differently after a restore); the screen, scrollback and VT state
    // may not.
    let seen = check(&session, IndexOptions::default(), 60, 0xfeed, false);
    assert!(
        seen.resumed > seen.seeks / 2,
        "only {} of {} seeks used a checkpoint",
        seen.resumed,
        seen.seeks
    );
    assert_eq!(seen.history_differs, 0);
}

#[test]
fn seek_matches_replay_when_history_is_pruned() {
    // The same session under a 2 MB limit, so most of it scrolls out: about
    // five pages of history at a time, with resizes reflowing it.
    let options = IndexOptions {
        scrollback: Scrollback::Bytes(2_000_000),
    };
    for hyperlinks in [false, true] {
        let session = synthetic(hyperlinks);
        let seen = check(&session, options, 200, 0xfeed, false);
        assert!(
            seen.resumed > seen.seeks / 2,
            "only {} of {} seeks used a checkpoint",
            seen.resumed,
            seen.seeks
        );
        if !hyperlinks {
            // Pages of plain styled text start out the same size after a
            // restore, so the history is the same too.
            assert_eq!(seen.history_differs, 0, "{seen:?}");
        }
    }
}
