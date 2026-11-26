//! Tap's recording index: OSC 133 marks, command segments, the transcript
//! and checkpointed seek.
//!
//! Building the index replays each pane twice through libghostty-vt:
//!
//! 1. **Marks** ([`scan`]): the stream is fed one escape sequence at a time
//!    so every OSC 133 mark, alternate-screen switch, clear and reset gets
//!    the exact byte offset of its sequence.
//! 2. **Segments** ([`segment`]): command blocks from the OSC 133 marks, or
//!    from Enter keystrokes when the pane has none, with TUI spans, output
//!    paragraphs and idle gaps inside them.
//! 3. **Transcript and checkpoints**: a second replay stops at each segment's
//!    boundaries to read the prompt and output text off the screen and to
//!    snapshot the terminal ([`replay`]).
//!
//! [`SessionIndex::seek`] then restores the nearest checkpoint and replays
//! only what follows it.
//!
//! All use of libghostty-vt is inside [`vt`].
//!
//! Nothing here is published (`publish = false`).

mod id;
pub mod line_editor;
pub mod replay;
pub mod scan;
pub mod segment;
pub mod stream;
pub mod transcript;
pub mod vt;

use std::path::Path;

use serde::Serialize;
use tap_model::{PaneId, Session};

pub use replay::{Checkpoint, CheckpointKey, Checkpoints, Position};
pub use scan::{Mark, MarkKind};
pub use segment::{BoundarySource, Segment, SegmentKind};
pub use transcript::{Block, CommandSource, CommandText, Durations, Transcript, TuiSpan};
pub use vt::VtError;

use replay::{Checkpointer, Replay};
use segment::Segmentation;
use stream::{ItemKind, PaneStream};
use transcript::Captured;

/// Something that stopped the index from being built or used.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The recording could not be read.
    #[error(transparent)]
    Model(#[from] tap_model::Error),
    /// The terminal engine failed.
    #[error(transparent)]
    Vt(#[from] VtError),
    /// No such pane.
    #[error("no pane {0:?} in this recording")]
    UnknownPane(String),
}

/// A stream offset and the event and time it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    /// Index of the output event in the pane.
    pub event: usize,
    /// Its first stream byte.
    pub start_byte: u64,
    /// One past its last stream byte.
    pub end_byte: u64,
    /// Its source time.
    pub at_us: u64,
}

/// A keystroke (or paste, or reply) and where the output stood when it was
/// sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRef {
    /// Index of the input event in the pane.
    pub event: usize,
    /// Its source time.
    pub at_us: u64,
    /// Stream offset of all output written before it.
    pub byte: u64,
}

/// One pane's index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneIndex {
    /// The pane.
    pub pane: PaneId,
    /// Length of the pane's stream.
    pub stream_len: u64,
    /// Output events → stream bytes, sorted.
    pub chunks: Vec<Chunk>,
    /// Marks, sorted by offset.
    pub marks: Vec<Mark>,
    /// Input events, sorted.
    pub inputs: Vec<InputRef>,
    /// Top-level segments, tiling the stream.
    pub segments: Vec<Segment>,
    #[serde(skip)]
    blocks: Vec<Block>,
    #[serde(skip)]
    stream: PaneStream,
}

impl PaneIndex {
    /// The pane's stream: every output event's playback bytes, concatenated.
    /// Offsets in marks, chunks and segments index into it.
    pub fn stream_bytes(&self) -> &[u8] {
        &self.stream.bytes
    }

    /// The transcript blocks of this pane.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}

/// A session's index.
#[derive(Debug, Clone, Serialize)]
pub struct SessionIndex {
    /// [`vt::engine_fingerprint`] of the engine that built it.
    pub engine: String,
    /// One per pane, in manifest order.
    pub panes: Vec<PaneIndex>,
    #[serde(skip)]
    checkpoints: Checkpoints,
}

/// Opens a recording: a `Session.tap/` package directory or a single-file
/// `.tap` JSON project.
pub fn open(path: &Path) -> Result<Session, IndexError> {
    if path.is_dir() {
        Ok(tap_model::package::open(path)?)
    } else {
        let json = std::fs::read_to_string(path).map_err(|source| tap_model::Error::Io {
            path: path.to_owned(),
            source,
        })?;
        Ok(tap_model::legacy::import_json(&json)?)
    }
}

impl SessionIndex {
    /// Indexes every pane of a session.
    pub fn build(session: &Session) -> Result<Self, IndexError> {
        let mut checkpoints = Checkpoints::default();
        let mut panes = Vec::new();
        for info in &session.manifest.panes {
            let stream = PaneStream::new(
                info.id.clone(),
                (info.cols, info.rows),
                session.pane_events(&info.id),
                session.manifest.duration_us,
            );
            panes.push(index_pane(stream, &mut checkpoints)?);
        }
        Ok(Self {
            engine: vt::engine_fingerprint(),
            panes,
            checkpoints,
        })
    }

    /// A pane's index.
    pub fn pane(&self, pane: &PaneId) -> Result<&PaneIndex, IndexError> {
        self.panes
            .iter()
            .find(|p| &p.pane == pane)
            .ok_or_else(|| IndexError::UnknownPane(pane.as_str().to_string()))
    }

    /// The transcript of every pane.
    pub fn transcript(&self) -> Transcript {
        Transcript {
            panes: self
                .panes
                .iter()
                .map(|p| transcript::PaneTranscript {
                    pane: p.pane.as_str().to_string(),
                    blocks: p.blocks.clone(),
                })
                .collect(),
        }
    }

    /// The checkpoints taken while indexing.
    pub fn checkpoints(&self) -> &Checkpoints {
        &self.checkpoints
    }

    /// The pane's terminal as it was at `source_us` (every event at or
    /// before it applied): the nearest checkpoint, restored, plus the events
    /// after it.
    pub fn seek(&self, pane: &PaneId, source_us: u64) -> Result<vt::Terminal, IndexError> {
        let index = self.pane(pane)?;
        let target = Position {
            item: index.stream.items_through(source_us),
            intra: 0,
        };
        let mut replay = match self.checkpoints.at_or_before(pane, target) {
            Some(checkpoint) => Replay::resume(&index.stream, checkpoint)?,
            None => Replay::start(&index.stream)?,
        };
        replay.run_to_item(target.item, &mut |_| {})?;
        Ok(replay.term)
    }

    /// The same terminal as [`SessionIndex::seek`], replayed from the start
    /// of the pane without checkpoints. Slow; for checking seek.
    pub fn replay_from_start(
        &self,
        pane: &PaneId,
        source_us: u64,
    ) -> Result<vt::Terminal, IndexError> {
        let index = self.pane(pane)?;
        let mut replay = Replay::start(&index.stream)?;
        replay.run_to_item(index.stream.items_through(source_us), &mut |_| {})?;
        Ok(replay.term)
    }
}

/// What the second replay does at a stream offset. Variants are in the
/// order they run when several share an offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    /// Read a segment's output (it ended here).
    OutputText(usize),
    /// Snapshot the terminal (a segment boundary).
    Checkpoint,
    /// Pin the row a segment's prompt is drawn on.
    PinPrompt(usize),
    /// Read a segment's prompt rows (its first key is about to be typed).
    PromptText(usize),
    /// Pin where a segment's output starts.
    PinOutput(usize),
}

fn index_pane(stream: PaneStream, checkpoints: &mut Checkpoints) -> Result<PaneIndex, IndexError> {
    let marks = scan::scan(&stream)?;
    let segmentation = segment::segment(&stream, &marks);
    let captured = replay_for_transcript(&stream, &segmentation, checkpoints)?;
    let blocks = segmentation
        .segments
        .iter()
        .zip(&segmentation.plans)
        .zip(captured)
        .map(|((segment, plan), captured)| {
            transcript::block(&stream, &marks, segment, plan, captured)
        })
        .collect();
    let mut chunks = Vec::new();
    let mut inputs = Vec::new();
    for (event, item) in stream.items.iter().enumerate() {
        match item.kind {
            ItemKind::Output { len } => chunks.push(Chunk {
                event,
                start_byte: item.offset,
                end_byte: item.offset + len,
                at_us: item.at_us,
            }),
            ItemKind::Input { .. } => inputs.push(InputRef {
                event,
                at_us: item.at_us,
                byte: item.offset,
            }),
            ItemKind::Resize { .. } | ItemKind::Other => {}
        }
    }
    Ok(PaneIndex {
        pane: stream.pane.clone(),
        stream_len: stream.len(),
        chunks,
        marks,
        inputs,
        segments: segmentation.segments,
        blocks,
        stream,
    })
}

/// Replays the pane once, reading prompt and output text at each segment's
/// boundaries and taking checkpoints.
fn replay_for_transcript(
    stream: &PaneStream,
    segmentation: &Segmentation,
    checkpoints: &mut Checkpoints,
) -> Result<Vec<Captured>, IndexError> {
    let mut stops: Vec<(u64, Action)> = Vec::new();
    for (index, (segment, plan)) in segmentation
        .segments
        .iter()
        .zip(&segmentation.plans)
        .enumerate()
    {
        stops.push((segment.start_byte, Action::Checkpoint));
        for child in &segment.children {
            stops.push((child.start_byte, Action::Checkpoint));
            stops.push((child.end_byte, Action::Checkpoint));
        }
        if let Some(byte) = plan.prompt_byte {
            stops.push((byte, Action::PinPrompt(index)));
            let typed_at = plan
                .first_input
                .map(|(item, _)| stream.items[item].offset)
                .or(plan.output_start.map(|(b, _)| b))
                .unwrap_or(segment.end_byte);
            stops.push((typed_at.max(byte), Action::PromptText(index)));
        }
        if let Some((byte, _)) = plan.output_start {
            stops.push((byte, Action::PinOutput(index)));
            let end = plan.command_end.map_or(segment.end_byte, |(e, _)| e);
            stops.push((end.max(byte), Action::OutputText(index)));
        }
    }
    stops.sort();

    let mut captured = vec![Captured::default(); segmentation.segments.len()];
    let mut prompt_pins = Vec::new();
    prompt_pins.resize_with(captured.len(), || None);
    let mut output_pins = Vec::new();
    output_pins.resize_with(captured.len(), || None);

    let mut checkpointer = Checkpointer::new(stream.pane.clone(), checkpoints);
    let mut replay = Replay::start(stream)?;
    for (byte, action) in stops {
        replay.run_to_byte(byte, &mut |r| checkpointer.periodic(r))?;
        let term = &replay.term;
        match action {
            Action::Checkpoint => checkpointer.take(&replay),
            Action::PinPrompt(i) => prompt_pins[i] = term.pin_cursor(),
            Action::PromptText(i) => {
                captured[i].prompt = Some(
                    prompt_pins[i]
                        .as_ref()
                        .map(|pin| term.rows_text(pin))
                        .unwrap_or_default(),
                );
            }
            Action::PinOutput(i) => output_pins[i] = term.pin_cursor(),
            Action::OutputText(i) => {
                let semantic =
                    segmentation.semantic && segmentation.segments[i].kind == SegmentKind::Command;
                captured[i].output = Some(
                    output_pins[i]
                        .as_ref()
                        .map(|pin| term.output_text(pin, semantic))
                        .unwrap_or_default(),
                );
            }
        }
    }
    replay.run_to_item(stream.items.len(), &mut |r| checkpointer.periodic(r))?;
    if let Some(error) = checkpointer.error.take() {
        return Err(error.into());
    }
    Ok(captured)
}
