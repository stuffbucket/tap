//! Excise: a command removed as if it never happened, or refused with
//! every reason.

mod support;

use tap_eval::{Outcome, Refusal, Side, Unreadable, state::TermState};
use tap_index::{SegmentKind, SessionIndex};
use tap_model::Session;

use support::*;

/// Trailing blanks of every line and trailing empty lines dropped.
fn normalise(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    lines[..end].join("\n")
}

/// Excises `[a, b)` and checks the result two ways:
///
/// 1. the edited session (its index's seek and replay) ends in exactly the
///    state of a reference built by splicing the recording's bytes by hand
///    (the range removed, the fixups played before byte `b`): screen text
///    with scrollback, VT dump with every extra, cursor, size, screen,
///    title, and every mode, colour, keyboard flag, character set and the
///    rest of [`TermState`];
/// 2. it ends in the state the original recording ends in, except for what
///    the command drew: the same [`TermState`], and the original's screen
///    text without the command's lines (unless a later clear erased them
///    anyway).
///
/// Returns the fixups' parts.
fn check_excision(session: &Session, raw: &SessionIndex, a: u64, b: u64) -> Vec<String> {
    let evaluation = evaluate(session, raw, vec![excise(a, b)]);
    let Outcome::Excised { fixups, .. } = &evaluation.outcomes[0] else {
        panic!("{a}..{b} was not excised: {:?}", evaluation.outcomes[0]);
    };
    let pane = raw.pane(&main_pane()).unwrap();
    let edited = edited_index(session, &evaluation);
    let edited_pane = edited.pane(&main_pane()).unwrap();
    let end = session.manifest.duration_us;

    // 1. Against bytes spliced by hand.
    let reference_session = spliced(session, &main_pane(), a, b, fixups.bytes());
    let reference = SessionIndex::build(&reference_session).unwrap();
    let reference_pane = reference.pane(&main_pane()).unwrap();
    assert_eq!(
        reference_pane.stream_bytes(),
        edited_pane.stream_bytes(),
        "{a}..{b}: program bytes"
    );
    let want = observe(&at_end(reference_pane.stream()));
    let got = observe(&at_end(edited_pane.stream()));
    assert_eq!(
        got, want,
        "{a}..{b}: end state against the spliced reference"
    );
    let seek = edited.seek(&main_pane(), end).unwrap();
    let replay = reference.replay_from_start(&main_pane(), end).unwrap();
    assert_eq!(seek.vt_dump(), replay.vt_dump(), "{a}..{b}: seek");
    assert_eq!(TermState::read(&seek), TermState::read(&replay));

    // 2. Against the original.
    let original = at_end(pane.stream());
    assert_eq!(
        TermState::read(&original),
        got.state,
        "{a}..{b}: terminal state against the original"
    );
    let before = normalise(&at_byte(pane.stream(), a).screen_text());
    let after = normalise(&at_byte(pane.stream(), b).screen_text());
    let full = normalise(&original.screen_text());
    // What the original drew after the range: everything past the text at
    // `b`, unless a later clear erased that too.
    let expected = match full.strip_prefix(&after) {
        Some(rest) if before.is_empty() => rest.trim_start_matches('\n').to_string(),
        Some(rest) if rest.starts_with('\n') || rest.is_empty() => format!("{before}{rest}"),
        Some(rest) => format!("{before}\n{rest}"),
        None => full.clone(),
    };
    assert_eq!(
        normalise(&got.text),
        normalise(&expected),
        "{a}..{b}: screen text is the original's without the command"
    );
    fixups.parts.iter().map(|p| p.state.clone()).collect()
}

fn refusals(session: &Session, raw: &SessionIndex, a: u64, b: u64) -> Vec<Refusal> {
    let evaluation = evaluate(session, raw, vec![excise(a, b)]);
    match &evaluation.outcomes[0] {
        Outcome::Refused { reasons } => reasons.clone(),
        other => panic!("{a}..{b} was not refused: {other:?}"),
    }
}

#[test]
fn excising_a_command_carries_the_state_it_set() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    // printf sets the title, palette entry 1, cursor keys mode, G1, the
    // working directory, Kitty keyboard flags and the cursor shape.
    let parts = check_excision(&session, &raw, 104, 623);
    assert_eq!(
        parts,
        [
            "cursor shape",
            "modes",
            "character sets",
            "kitty keyboard flags",
            "palette",
            "title",
            "working directory"
        ]
    );
    let evaluation = evaluate(&session, &raw, vec![excise(104, 623)]);
    let Outcome::Excised {
        fixups,
        inputs_removed,
        ..
    } = &evaluation.outcomes[0]
    else {
        unreachable!()
    };
    assert_eq!(
        fixups.text,
        "\\e[4 q\\e[?1h\\e)0\\e[=5;1u\\e]4;1;rgb:12/34/56\\e\\\\\\e]2;tap title\\e\\\\\\e]7;file://fixture/tmp\\e\\\\"
    );
    // The keys typed for it go too.
    assert_eq!(*inputs_removed, 96);
    // Its time is cut: from its prompt to the next.
    let pane = raw.pane(&main_pane()).unwrap();
    let cut = pane.stream().time_of_byte(623) - pane.stream().time_of_byte(104);
    assert_eq!(
        evaluation.time_map.edited_duration(),
        session.manifest.duration_us - cut
    );
    // The transcript no longer has it.
    let edited = edited_index(&session, &evaluation);
    let commands: Vec<String> = edited.transcript().panes[0]
        .blocks
        .iter()
        .filter_map(|b| b.command.as_ref().map(|c| c.text.clone()))
        .collect();
    assert_eq!(commands[0], "echo next");
    assert_eq!(commands.len(), 9);
}

#[test]
fn every_command_excises_or_says_why() {
    use Refusal::*;
    for name in ["osc133", "redacted", "edits", "no-integration"] {
        let session = fixture(name);
        let raw = SessionIndex::build(&session).unwrap();
        let pane = raw.pane(&main_pane()).unwrap();
        let mut table = Vec::new();
        for segment in &pane.segments {
            let (a, b) = (segment.start_byte, segment.end_byte);
            let evaluation = evaluate(&session, &raw, vec![excise(a, b)]);
            let row = match &evaluation.outcomes[0] {
                Outcome::Excised { .. } => {
                    check_excision(&session, &raw, a, b);
                    "excised".to_string()
                }
                Outcome::Refused { reasons } => reasons
                    .iter()
                    .map(|r| match r {
                        NotAtPrompt { side, .. } => format!("not-at-prompt:{side:?}"),
                        AlternateScreen { .. } => "alternate-screen".into(),
                        KittyImageUsedLater { .. } => "kitty-image".into(),
                        AbsolutePositioning { .. } => "absolute".into(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                other => panic!("{name} {a}..{b}: {other:?}"),
            };
            table.push((segment.kind, row));
        }
        let expected: Vec<(SegmentKind, &str)> = match name {
            "osc133" => vec![
                (SegmentKind::Output, "not-at-prompt:Start"),
                (SegmentKind::Command, "excised"), // echo hello world
                (SegmentKind::Command, "excised"), // cat (fails)
                (SegmentKind::Command, "excised"), // UTF-8
                (SegmentKind::Command, "excised"), // printf
                (SegmentKind::Command, "excised"), // sleep
                (SegmentKind::Command, "alternate-screen"), // less
                (SegmentKind::Command, "excised"), // echo before clear; clear
                (SegmentKind::Command, "excised"), // echo after clear
                (SegmentKind::Command, "excised"), // exit
            ],
            "redacted" => vec![
                (SegmentKind::Output, "not-at-prompt:Start"),
                (SegmentKind::Command, "excised"),
                (SegmentKind::Command, "excised"),
                (SegmentKind::Command, "excised"),
            ],
            "edits" => vec![
                (SegmentKind::Output, "not-at-prompt:Start"),
                (SegmentKind::Command, "excised"), // printf state
                (SegmentKind::Command, "excised"), // echo next
                (SegmentKind::Command, "excised"), // echo token
                (SegmentKind::Command, "excised"), // key=
                (SegmentKind::Command, "excised"), // wide
                (SegmentKind::Command, "alternate-screen"), // full screen
                (SegmentKind::Command, "absolute"), // clear
                (SegmentKind::Command, "kitty-image absolute"), // transmit
                (SegmentKind::Command, "excised"), // place, positioned
                (SegmentKind::Command, "excised"), // exit
            ],
            // No OSC 133 marks: no command starts or ends at a prompt mark.
            _ => {
                let both = "not-at-prompt:Start not-at-prompt:End";
                let mut rows = vec![(SegmentKind::Output, both)];
                rows.extend([(SegmentKind::Command, both); 5]);
                rows.push((
                    SegmentKind::Command,
                    "not-at-prompt:Start not-at-prompt:End alternate-screen",
                ));
                rows.extend([(SegmentKind::Command, both); 4]);
                rows.push((SegmentKind::Command, "not-at-prompt:Start"));
                rows
            }
        };
        let table: Vec<(SegmentKind, &str)> = table.iter().map(|(k, r)| (*k, r.as_str())).collect();
        assert_eq!(table, expected, "{name}");
    }
}

#[test]
fn refusals_list_exactly_the_failed_preconditions() {
    let session = fixture("edits");
    let raw = SessionIndex::build(&session).unwrap();
    assert_eq!(
        refusals(&session, &raw, 2371, 2743),
        vec![
            Refusal::KittyImageUsedLater {
                image: "i=31".into(),
                transmitted: 2587,
                used: 2935
            },
            Refusal::AbsolutePositioning {
                byte: 2929,
                sequence: "\\e[3;1H".into()
            },
        ]
    );
    assert_eq!(
        refusals(&session, &raw, 1718, 2160),
        vec![Refusal::AlternateScreen { byte: 2003 }]
    );
    // Without the clear, the later CUP lands on different rows.
    assert_eq!(
        refusals(&session, &raw, 2160, 2371),
        vec![Refusal::AbsolutePositioning {
            byte: 2929,
            sequence: "\\e[3;1H".into()
        }]
    );
    assert_eq!(
        refusals(&session, &raw, 0, 104),
        vec![Refusal::NotAtPrompt {
            side: Side::Start,
            byte: 0
        }]
    );
    // Two prompts' worth at once is fine; overlapping excisions are not.
    let evaluation = evaluate(&session, &raw, vec![excise(623, 1105), excise(839, 1420)]);
    assert!(matches!(evaluation.outcomes[0], Outcome::Excised { .. }));
    assert_eq!(
        evaluation.outcomes[1],
        Outcome::Refused {
            reasons: vec![Refusal::Overlaps { op: 0 }]
        }
    );
}

const P: &[u8] = b"\x1b]133;P\x07$ ";

/// Where the command after the first prompt starts printing.
const AT: u64 = P.len() as u64;

/// The outcome of excising the first command of a session that prints
/// `middle` in it and `after` in the next.
fn synthetic_excise(middle: &[u8], after: &[u8]) -> Outcome {
    let session = synthetic(&[P, middle, P, after, P]);
    let raw = SessionIndex::build(&session).unwrap();
    let b = P.len() as u64 + middle.len() as u64;
    evaluate(&session, &raw, vec![excise(0, b)]).outcomes[0].clone()
}

fn synthetic_refusals(middle: &[u8], after: &[u8]) -> Vec<Refusal> {
    match synthetic_excise(middle, after) {
        Outcome::Refused { reasons } => reasons,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn unreadable_state_is_refused_when_it_matters() {
    // A saved cursor restored later. Restoring it positions the cursor
    // absolutely, so after a clear the two terminals agree again, except
    // for the saved cursor.
    assert_eq!(
        synthetic_refusals(b"\x1b7x\r\n", b"\x1b8y"),
        vec![
            Refusal::AbsolutePositioning {
                byte: 2 * AT + 5,
                sequence: "\\e8".into()
            },
            Refusal::UnreadableState {
                state: Unreadable::SavedCursor,
                byte: AT
            }
        ]
    );
    assert_eq!(
        synthetic_refusals(b"\x1b7x\r\n", b"\x1b[H\x1b[2J\x1b8y"),
        vec![Refusal::UnreadableState {
            state: Unreadable::SavedCursor,
            byte: AT
        }]
    );
    // …but not when something saves again first.
    let outcome = synthetic_excise(b"\x1b7x\r\n", b"\x1b[H\x1b[2J\x1b7\x1b8y");
    assert!(matches!(outcome, Outcome::Excised { .. }), "{outcome:?}");
    // Saved private modes restored later.
    assert_eq!(
        synthetic_refusals(b"\x1b[?1s\x1b[?1h", b"\x1b[?1r"),
        vec![Refusal::UnreadableState {
            state: Unreadable::SavedModes,
            byte: AT
        }]
    );
    // Kitty keyboard flags pushed and not popped: the stack below the top
    // cannot be read back.
    assert_eq!(
        synthetic_refusals(b"\x1b[>1u", b""),
        vec![Refusal::UnreadableState {
            state: Unreadable::KittyKeyboardStack,
            byte: AT
        }]
    );
    // Pushed and popped: fine.
    let outcome = synthetic_excise(b"\x1b[>1ux\x1b[<u", b"");
    assert!(matches!(outcome, Outcome::Excised { .. }), "{outcome:?}");
    // 132-column mode cannot be set back without clearing the screen.
    assert_eq!(
        synthetic_refusals(b"\x1b[?40h\x1b[?3h", b""),
        vec![Refusal::Unfixable {
            state: "132-column mode (DECCOLM)".into()
        }]
    );
}

#[test]
fn an_end_inside_a_sequence_is_not_at_ground() {
    // The next prompt mark cuts a CSI short: its ESC is where the mark is,
    // but the parser is not at ground just before it.
    let session = synthetic(&[P, b"x\x1b[1", P, b"y", P]);
    let raw = SessionIndex::build(&session).unwrap();
    let marks: Vec<u64> = raw
        .pane(&main_pane())
        .unwrap()
        .marks
        .iter()
        .map(|m| m.byte)
        .collect();
    let end = AT + 4;
    assert_eq!(marks, vec![0, end, 2 * AT + 5]);
    assert_eq!(
        refusals(&session, &raw, 0, end),
        vec![Refusal::NotAtGround {
            side: Side::End,
            byte: end
        }]
    );
}

#[test]
fn identical_programs_index_identically() {
    // Excising and then indexing twice gives the same checkpoints, keyed by
    // the program's prefix hash; before the splice they are the raw
    // stream's.
    let session = fixture("osc133");
    let raw = SessionIndex::build(&session).unwrap();
    let evaluation = evaluate(&session, &raw, vec![excise(984, 1255)]);
    let a = edited_index(&session, &evaluation);
    let b = edited_index(&session, &evaluation);
    let keys = |i: &SessionIndex| -> Vec<_> {
        i.checkpoints()
            .pane(&main_pane())
            .map(|c| c.key.clone())
            .collect()
    };
    assert_eq!(keys(&a), keys(&b));
    let raw_keys = keys(&raw);
    for key in keys(&a).iter().filter(|k| k.byte <= 984) {
        assert!(raw_keys.contains(key), "{key:?} before the splice");
    }
    for key in keys(&a).iter().filter(|k| k.byte > 984) {
        assert!(!raw_keys.contains(key), "{key:?} after the splice");
    }
}
