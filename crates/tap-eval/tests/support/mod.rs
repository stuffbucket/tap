//! Shared helpers for tap-eval's tests.
#![allow(dead_code)]

use std::path::Path;

use tap_eval::state::TermState;
use tap_eval::{Evaluation, evaluate_entries};
use tap_index::stream::{ItemKind, PaneStream};
use tap_index::vt::{self, Scrollback};
use tap_index::{IndexOptions, Position, Replay, SessionIndex};
use tap_model::edl::{EditEntry, EditOp, SegId};
use tap_model::event::{EventKind, Output, TextEncoding};
use tap_model::{PaneId, Session};

pub fn fixture(name: &str) -> Session {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tap-index/tests/fixtures")
        .join(format!("{name}.tap"));
    tap_index::open(&path).unwrap()
}

pub fn main_pane() -> PaneId {
    PaneId::new("main").unwrap()
}

pub fn evaluate(session: &Session, raw: &SessionIndex, ops: Vec<EditOp>) -> Evaluation {
    let entries: Vec<EditEntry> = ops.into_iter().map(EditEntry::new).collect();
    evaluate_entries(session, raw, &entries).unwrap()
}

pub fn excise(start: u64, end: u64) -> EditOp {
    EditOp::Excise {
        segment: SegId {
            pane: main_pane(),
            start,
            end,
        },
    }
}

/// Everything observable about a terminal, as tap-index's seek tests
/// compare it, plus the state an excision must carry.
#[derive(Debug, PartialEq, Eq)]
pub struct Observed {
    pub text: String,
    pub vt: Vec<u8>,
    pub cursor: (u16, u16),
    pub size: (u16, u16),
    pub alternate: bool,
    pub title: String,
    pub state: TermState,
}

pub fn observe(term: &vt::Terminal) -> Observed {
    Observed {
        text: term.screen_text(),
        vt: term.vt_dump(),
        cursor: term.cursor(),
        size: term.size(),
        alternate: term.on_alternate_screen(),
        title: term.title(),
        state: TermState::read(term),
    }
}

/// A one-pane session from output chunks, one event each, 0.1 s apart,
/// with an 80×24 screen.
pub fn synthetic(chunks: &[&[u8]]) -> Session {
    let mut events = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let data = base64(chunk);
        events.push(format!(
            r#"{{"atUs":{},"type":"output","data":"{data}","encoding":"base64"}}"#,
            (i as u64 + 1) * 100_000
        ));
    }
    let json = format!(
        r##"{{"version":1,"title":"synthetic","createdAt":"2026-11-26T00:00:00.000Z","durationUs":{},
        "canvas":{{"width":1280,"height":720,"background":"#101014","fps":60}},
        "panes":[{{"id":"main","cols":80,"rows":24,"fontSize":16,
        "layout":{{"x":0,"y":0,"width":1,"height":1}},"events":[{}]}}],"overlays":[],
        "metadata":{{"terminal":"ghostty","terminalEngine":"libghostty-vt 1.0.1","timebase":"microseconds","source":"interactive-pty"}}}}"##,
        (chunks.len() as u64 + 1) * 100_000,
        events.join(",")
    );
    tap_model::legacy::import_json(&json).unwrap()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The reference for an excision, built without tap-eval's byte program:
/// the session with each output event's bytes in `[a, b)` removed and
/// `fixups` played just before byte `b` (at the end of the last output
/// event when `b` is the end of the stream).
pub fn spliced(session: &Session, pane: &PaneId, a: u64, b: u64, fixups: &[u8]) -> Session {
    let mut out = session.clone();
    let events = out.events.get_mut(pane).unwrap();
    let total: u64 = events
        .iter()
        .map(|e| match &e.kind {
            EventKind::Output(o) => o.data.len() as u64,
            _ => 0,
        })
        .sum();
    let last = events
        .iter()
        .rposition(|e| matches!(e.kind, EventKind::Output(_)))
        .unwrap();
    let mut offset = 0u64;
    for (index, event) in events.iter_mut().enumerate() {
        let EventKind::Output(output) = &mut event.kind else {
            continue;
        };
        let (s0, s1) = (offset, offset + output.data.len() as u64);
        offset = s1;
        let mut data = Vec::new();
        for (i, &byte) in output.data.iter().enumerate() {
            let s = s0 + i as u64;
            if s == b {
                data.extend_from_slice(fixups);
            }
            if s < a || s >= b {
                data.push(byte);
            }
        }
        if b == total && index == last {
            data.extend_from_slice(fixups);
        }
        *output = Output {
            data,
            data_encoding: TextEncoding::Base64,
            playback: None,
        };
    }
    out
}

/// The terminal after exactly the first `byte` bytes of a stream (and the
/// events before the one holding that byte), all history kept.
pub fn at_byte(stream: &PaneStream, byte: u64) -> vt::Terminal {
    let mut replay = Replay::start(stream, Scrollback::Unlimited).unwrap();
    let position = tap_eval::byte_position(stream, byte);
    replay.run_to_position(position).unwrap();
    replay.term
}

/// The terminal at the end of a pane: every event applied.
pub fn at_end(stream: &PaneStream) -> vt::Terminal {
    let mut replay = Replay::start(stream, Scrollback::Unlimited).unwrap();
    replay
        .run_to_position(Position {
            item: stream.items.len(),
            intra: 0,
        })
        .unwrap();
    replay.term
}

pub fn edited_index(session: &Session, evaluation: &Evaluation) -> SessionIndex {
    evaluation.index(session, IndexOptions::default()).unwrap()
}

/// Output items of a stream with their bytes.
pub fn outputs(stream: &PaneStream) -> Vec<&[u8]> {
    stream
        .items
        .iter()
        .filter_map(|i| match i.kind {
            ItemKind::Output { len } => {
                Some(&stream.bytes[i.offset as usize..(i.offset + len) as usize])
            }
            _ => None,
        })
        .collect()
}

/// Deterministic pseudo-random numbers (xorshift64*).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}
