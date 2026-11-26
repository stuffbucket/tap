//! Byte programs: the source ↔ program mapping round-trips, events tile
//! the program, and the identity program indexes exactly as the raw stream.

use std::collections::BTreeMap;
use std::path::Path;

use proptest::prelude::*;
use tap_index::stream::{ItemKind, PaneStream};
use tap_index::{
    Attach, IndexOptions, InputEdit, Location, Origin, PaneProgram, SessionIndex, Splice,
};
use tap_model::PaneId;
use tap_model::event::{EventKind, Input, Output, RawEvent, TextEncoding};

/// Random non-overlapping splices over `len` bytes.
fn splices(len: u64) -> impl Strategy<Value = Vec<Splice>> {
    proptest::collection::vec(
        (
            0..len.max(1),
            1..8u64,
            proptest::collection::vec(any::<u8>(), 0..6),
            any::<bool>(),
        ),
        0..6,
    )
    .prop_map(move |raw| {
        let mut out: Vec<Splice> = Vec::new();
        let mut taken: Vec<(u64, u64)> = Vec::new();
        for (op, (start, width, bytes, next)) in raw.into_iter().enumerate() {
            let end = (start + width).min(len);
            if start >= end || taken.iter().any(|&(a, b)| start < b && a < end) {
                continue;
            }
            taken.push((start, end));
            out.push(Splice {
                source: start..end,
                bytes,
                attach: if next { Attach::Next } else { Attach::Last },
                op,
            });
        }
        out
    })
}

fn stream_and_program() -> impl Strategy<Value = (Vec<u8>, Vec<u64>, Vec<Splice>)> {
    (1..60u64).prop_flat_map(|len| {
        (
            proptest::collection::vec(any::<u8>(), len as usize),
            // Event boundaries.
            proptest::collection::vec(0..=len, 0..8),
            splices(len),
        )
    })
}

proptest! {
    #[test]
    fn source_and_program_offsets_round_trip((raw, _, splices) in stream_and_program()) {
        let program = PaneProgram::new(raw.len() as u64, splices.clone(), BTreeMap::new()).unwrap();
        let played = program.render(&raw);
        prop_assert_eq!(played.len() as u64, program.len());
        for s in 0..raw.len() as u64 {
            match program.locate(s) {
                Location::Kept(p) => {
                    prop_assert_eq!(program.to_source(p), Origin::Source(s));
                    prop_assert_eq!(played[p as usize], raw[s as usize]);
                }
                Location::Removed { splice, program: range } => {
                    let sp = &program.splices()[splice];
                    prop_assert!(sp.source.contains(&s));
                    prop_assert_eq!(range.end - range.start, sp.bytes.len() as u64);
                }
            }
        }
        for p in 0..program.len() {
            match program.to_source(p) {
                Origin::Source(s) => {
                    prop_assert_eq!(program.locate(s), Location::Kept(p));
                }
                Origin::Inserted { splice, offset } => {
                    prop_assert_eq!(played[p as usize], program.splices()[splice].bytes[offset as usize]);
                    prop_assert!(program.splice_range(splice).contains(&p));
                }
            }
        }
        prop_assert_eq!(program.locate(raw.len() as u64), Location::Kept(program.len()));
    }

    #[test]
    fn events_tile_the_program((raw, cuts, splices) in stream_and_program()) {
        let mut cuts = cuts;
        cuts.push(0);
        cuts.push(raw.len() as u64);
        cuts.sort_unstable();
        cuts.dedup();
        let mut events = Vec::new();
        for (i, w) in cuts.windows(2).enumerate() {
            events.push(RawEvent {
                at_us: i as u64,
                seq: 2 * i as u64,
                kind: EventKind::Output(Output {
                    data: raw[w[0] as usize..w[1] as usize].to_vec(),
                    data_encoding: TextEncoding::Unspecified,
                    playback: None,
                }),
            });
            events.push(RawEvent {
                at_us: i as u64,
                seq: 2 * i as u64 + 1,
                kind: EventKind::Input(Input {
                    data: b"k".to_vec(),
                    data_encoding: TextEncoding::Unspecified,
                    origin: None,
                    redacted: None,
                }),
            });
        }
        let inputs = BTreeMap::from([(1, InputEdit::Redacted { op: 99 })]);
        let program = PaneProgram::new(raw.len() as u64, splices, inputs).unwrap();
        let played = program.render(&raw);
        let pane = PaneId::new("p").unwrap();
        let stream = PaneStream::with_program(pane, (80, 24), &events, 1000, program.clone());
        prop_assert_eq!(&stream.bytes, &played);
        let (mut next, mut previous) = (0, 0);
        for item in &stream.items {
            prop_assert!(item.offset >= previous);
            previous = item.offset;
            if let ItemKind::Output { len } = item.kind {
                prop_assert_eq!(item.offset, next);
                next = item.offset + len;
            }
        }
        prop_assert_eq!(next, program.len());
        let redacted = matches!(&stream.items[1].kind, ItemKind::Input { data, redacted: true, .. } if data.is_empty());
        prop_assert!(redacted);
    }
}

fn fixture(name: &str) -> tap_model::Session {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.tap"));
    tap_index::open(&path).unwrap()
}

#[test]
fn identity_programs_index_exactly_as_the_raw_stream() {
    for name in ["osc133", "no-integration", "redacted", "edits"] {
        let session = fixture(name);
        let raw = SessionIndex::build(&session).unwrap();
        let programs = raw
            .panes
            .iter()
            .map(|p| (p.pane.clone(), PaneProgram::identity(p.stream_len)))
            .collect();
        let edited =
            SessionIndex::build_programs(&session, &programs, IndexOptions::default()).unwrap();
        assert_eq!(
            serde_json::to_string(&raw).unwrap(),
            serde_json::to_string(&edited).unwrap(),
            "{name}: index"
        );
        assert_eq!(raw.transcript(), edited.transcript(), "{name}: transcript");
        assert_eq!(raw.transcript().to_text(), edited.transcript().to_text());
        for pane in &raw.panes {
            let a: Vec<_> = raw.checkpoints().pane(&pane.pane).collect();
            let b: Vec<_> = edited.checkpoints().pane(&pane.pane).collect();
            assert_eq!(a, b, "{name}: checkpoints");
            for t in [
                0,
                session.manifest.duration_us / 2,
                session.manifest.duration_us,
            ] {
                let x = raw.seek(&pane.pane, t).unwrap();
                let y = edited.seek(&pane.pane, t).unwrap();
                assert_eq!(x.vt_dump(), y.vt_dump(), "{name}: seek to {t}");
            }
        }
    }
}
