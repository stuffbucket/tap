//! The index of real zsh recordings (see scripts/record-fixture.ts).

use std::path::Path;

use tap_index::{
    Block, BoundarySource, CommandSource, MarkKind, PaneIndex, SegmentKind, SessionIndex,
};
use tap_model::PaneId;

fn index(name: &str) -> SessionIndex {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.tap"));
    let session = tap_index::open(&path).unwrap();
    SessionIndex::build(&session).unwrap()
}

fn main_pane(index: &SessionIndex) -> &PaneIndex {
    index.pane(&PaneId::new("main").unwrap()).unwrap()
}

use BoundarySource::{AltScreen, Clear, EnterKey, Idle, Osc133, Start};
use SegmentKind::{Command, Output, Paragraph, Tui};

/// Kinds and boundary sources of the top-level segments and their children.
type Shape = Vec<(
    SegmentKind,
    BoundarySource,
    Vec<(SegmentKind, BoundarySource)>,
)>;

fn shape(pane: &PaneIndex) -> Shape {
    pane.segments
        .iter()
        .map(|s| {
            let children = s
                .children
                .iter()
                .map(|c| (c.kind, c.boundary_source))
                .collect();
            (s.kind, s.boundary_source, children)
        })
        .collect()
}

/// Command text, exit code and output of each command block.
fn commands(pane: &PaneIndex) -> Vec<(String, Option<i32>, String)> {
    pane.blocks()
        .iter()
        .filter(|b| b.kind == Command)
        .map(|b| {
            (
                b.command
                    .as_ref()
                    .map(|c| c.text.clone())
                    .unwrap_or_default(),
                b.exit_code,
                b.output.clone().unwrap_or_default(),
            )
        })
        .collect()
}

fn owned(rows: &[(&str, Option<i32>, &str)]) -> Vec<(String, Option<i32>, String)> {
    rows.iter()
        .map(|(c, e, o)| (c.to_string(), *e, o.to_string()))
        .collect()
}

/// The segments every fixture's session produces, with `source` for the
/// command boundaries.
fn session_shape(source: BoundarySource) -> Shape {
    let para = |s| (Paragraph, s);
    vec![
        // zsh's partial-line marker before the first prompt.
        (Output, Start, vec![para(Start)]),
        (Command, source, vec![para(source)]), // echo hello world
        (Command, source, vec![para(source)]), // cat (fails)
        (Command, source, vec![para(source)]), // UTF-8
        (Command, source, vec![para(source)]), // printf, three lines
        // Waited at the prompt, then "start", a 2.5 s sleep, "done".
        (
            Command,
            source,
            vec![
                (SegmentKind::Idle, Idle),
                para(source),
                (SegmentKind::Idle, Idle),
                para(Idle),
            ],
        ),
        // less: the pager, then zsh's partial-line marker after it.
        (Command, source, vec![(Tui, AltScreen), para(AltScreen)]),
        // echo before clear; clear
        (Command, source, vec![para(source), para(Clear)]),
        (Command, source, vec![para(source)]), // echo after clear
    ]
}

const SESSION_COMMANDS: &[(&str, Option<i32>, &str)] = &[
    ("echo hello world", Some(0), "hello world"),
    (
        "cat /nonexistent-tap-fixture",
        Some(1),
        "cat: /nonexistent-tap-fixture: No such file or directory",
    ),
    (
        "echo 'héllo wörld ✓ 日本語'",
        Some(0),
        "héllo wörld ✓ 日本語",
    ),
    ("printf 'one\\ntwo\\nthree\\n'", Some(0), "one\ntwo\nthree"),
    ("echo start; sleep 2.5; echo done", Some(0), "start\ndone"),
    ("less notes.txt", Some(0), ""),
    // The screen was cleared, so nothing of the output is left on it.
    ("echo before clear; clear", Some(0), ""),
    ("echo after clear", Some(0), "after clear"),
];

#[test]
fn osc133_segments_commands_and_exit_codes() {
    let index = index("osc133");
    let pane = main_pane(&index);

    let mut expected = session_shape(Osc133);
    // `exit` started but the recording ended before it did.
    expected.push((Command, Osc133, vec![]));
    assert_eq!(shape(pane), expected);

    let mut expected = owned(SESSION_COMMANDS);
    expected.push(("exit".into(), None, String::new()));
    assert_eq!(commands(pane), expected);

    for block in pane.blocks().iter().filter(|b| b.kind == Command) {
        assert_eq!(block.prompt.as_deref(), Some("tap$"));
        let command = block.command.as_ref().unwrap();
        assert_eq!(command.source, CommandSource::Osc133);
        assert!(!command.approximate && !command.redacted);
    }
}

#[test]
fn segments_tile_the_stream_and_start_at_prompt_marks() {
    let index = index("osc133");
    let pane = main_pane(&index);
    let mut at = 0;
    for segment in &pane.segments {
        assert_eq!(segment.start_byte, at);
        assert!(segment.end_byte >= segment.start_byte);
        at = segment.end_byte;
    }
    assert_eq!(at, pane.stream_len);
    let prompts: Vec<u64> = pane
        .marks
        .iter()
        .filter(|m| m.kind == MarkKind::Prompt)
        .map(|m| m.byte)
        .collect();
    let starts: Vec<u64> = pane.segments[1..].iter().map(|s| s.start_byte).collect();
    assert_eq!(starts, prompts);
}

#[test]
fn mark_offsets_point_at_the_esc_of_each_sequence() {
    let index = index("osc133");
    let pane = main_pane(&index);
    let bytes = pane.stream_bytes();
    let at = |m: &tap_index::Mark| &bytes[m.byte as usize..m.end as usize];
    let mut osc = 0;
    for mark in &pane.marks {
        let sequence = at(mark);
        match &mark.kind {
            MarkKind::Prompt => assert_eq!(sequence, b"\x1b]133;P\x07"),
            MarkKind::CommandStart { command } => {
                assert!(sequence.starts_with(b"\x1b]133;C;cmdline_url="));
                assert!(command.is_some());
                assert_eq!(sequence.last(), Some(&0x07));
            }
            MarkKind::CommandEnd { exit_code } => {
                assert_eq!(
                    sequence,
                    format!("\x1b]133;D;{}\x07", exit_code.unwrap()).as_bytes()
                );
            }
            MarkKind::AltScreenEnter => assert_eq!(sequence, b"\x1b[?1049h"),
            MarkKind::AltScreenExit => assert_eq!(sequence, b"\x1b[?1049l"),
            MarkKind::Clear { scrollback: true } => assert_eq!(sequence, b"\x1b[3J"),
            MarkKind::Clear { scrollback: false } => assert_eq!(sequence, b"\x1b[2J"),
            other => panic!("unexpected mark {other:?}"),
        }
        if matches!(
            mark.kind,
            MarkKind::Prompt | MarkKind::CommandStart { .. } | MarkKind::CommandEnd { .. }
        ) {
            osc += 1;
        }
        // The mark's event holds its first byte, and the mark has its time.
        let chunk = pane.chunks.iter().find(|c| c.event == mark.event).unwrap();
        assert!(chunk.start_byte <= mark.byte && mark.byte < chunk.end_byte);
        assert_eq!(chunk.at_us, mark.at_us);
    }
    // Nine prompts, nine command starts (the last is exit) and eight ends.
    assert_eq!(osc, 26);
    // Every OSC 133 in the raw stream was found.
    let raw = bytes.windows(6).filter(|w| w == b"\x1b]133;").count();
    assert_eq!(osc, raw);
}

#[test]
fn the_pager_is_a_tui_span_named_from_the_command() {
    let index = index("osc133");
    let pane = main_pane(&index);
    let less: &Block = pane
        .blocks()
        .iter()
        .find(|b| {
            b.command
                .as_ref()
                .is_some_and(|c| c.text == "less notes.txt")
        })
        .unwrap();
    assert_eq!(less.tui.len(), 1);
    let span = &less.tui[0];
    assert_eq!(span.program.as_deref(), Some("less"));
    // The pager was open for a little over a second.
    assert!((900_000..2_000_000).contains(&span.duration_us), "{span:?}");
    let segment = pane
        .segments
        .iter()
        .flat_map(|s| &s.children)
        .find(|c| c.id == span.segment_id)
        .unwrap();
    let bytes = &pane.stream_bytes()[segment.start_byte as usize..segment.end_byte as usize];
    assert!(bytes.starts_with(b"\x1b[?1049h") && bytes.ends_with(b"\x1b[?1049l"));
}

#[test]
fn durations_add_up() {
    let index = index("osc133");
    let pane = main_pane(&index);
    let sleep = pane
        .blocks()
        .iter()
        .find(|b| {
            b.command
                .as_ref()
                .is_some_and(|c| c.text.contains("sleep 2.5"))
        })
        .unwrap();
    // The fixture waits about 3 s at this prompt, then types for about 1.7 s.
    assert!(sleep.durations.idle_us >= 2_500_000, "{sleep:?}");
    assert!(
        (2_500_000..3_500_000).contains(&sleep.durations.running_us),
        "{sleep:?}"
    );
    for block in pane.blocks().iter().filter(|b| b.kind == Command) {
        let d = block.durations;
        assert!(d.idle_us + d.typing_us + d.running_us <= block.end_us - block.start_us);
        assert!(d.typing_us > 0, "{block:?}");
    }
}

#[test]
fn without_shell_integration_enter_keys_split_commands() {
    let index = index("no-integration");
    let pane = main_pane(&index);

    let mut expected = session_shape(EnterKey);
    expected.extend([
        (Command, EnterKey, vec![(Paragraph, EnterKey)]), // history recall
        (Command, EnterKey, vec![(Paragraph, EnterKey)]), // ^U, echo killed
        (Command, EnterKey, vec![]),                      // exit
    ]);
    assert_eq!(shape(pane), expected);

    // No exit codes without OSC 133;D; the Enter pressed inside less made no
    // command of its own.
    let mut expected: Vec<_> = SESSION_COMMANDS
        .iter()
        .map(|&(c, _, o)| (c, None, o))
        .collect();
    expected.extend([
        ("", None, "after clear"),
        ("echo killed", None, "killed"),
        ("exit", None, ""),
    ]);
    assert_eq!(commands(pane), owned(&expected));

    let texts: Vec<_> = pane
        .blocks()
        .iter()
        .filter_map(|b| b.command.as_ref())
        .map(|c| (c.source, c.approximate, c.redacted))
        .collect();
    assert!(
        texts
            .iter()
            .all(|&(s, _, r)| s == CommandSource::InputLane && !r)
    );
    // Only the command recalled with the up arrow is approximate.
    let approximate: Vec<usize> = texts
        .iter()
        .enumerate()
        .filter(|(_, t)| t.1)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(approximate, vec![8]);
    let less = pane.blocks().iter().find(|b| !b.tui.is_empty()).unwrap();
    assert_eq!(less.tui[0].program.as_deref(), Some("less"));
}

#[test]
fn redacted_input_shows_as_a_placeholder() {
    let index = index("redacted");
    let pane = main_pane(&index);
    let blocks: Vec<_> = pane
        .blocks()
        .iter()
        .filter(|b| b.kind == Command)
        .map(|b| {
            let c = b.command.as_ref().unwrap();
            (
                c.text.as_str(),
                c.source,
                c.redacted,
                b.exit_code,
                b.output.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        blocks,
        vec![
            // The command line is withheld; its output is not (Tap warns about
            // this when sharing).
            (
                "•••",
                CommandSource::InputLane,
                true,
                Some(0),
                "secret-value"
            ),
            ("•••", CommandSource::InputLane, true, Some(1), ""),
            ("•••", CommandSource::InputLane, true, None, ""),
        ]
    );
}

#[test]
fn segment_ids_are_stable_and_unique() {
    let a = index("osc133");
    let b = index("osc133");
    let ids = |i: &SessionIndex| -> Vec<String> {
        main_pane(i)
            .segments
            .iter()
            .flat_map(|s| std::iter::once(s).chain(&s.children))
            .map(|s| s.id.clone())
            .collect()
    };
    let first = ids(&a);
    assert_eq!(first, ids(&b));
    let mut unique = first.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), first.len());
}

#[test]
fn a_package_indexes_like_the_json_it_came_from() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/osc133.tap");
    let session = tap_index::open(&path).unwrap();
    let dir = std::env::temp_dir().join(format!("tap-index-test-{}", std::process::id()));
    let package = dir.join("Session.tap");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    tap_model::package::create(&package, &session).unwrap();
    let from_package = SessionIndex::build(&tap_index::open(&package).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(from_package.transcript(), index("osc133").transcript());
}
