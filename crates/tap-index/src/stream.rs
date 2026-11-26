//! A pane's events as one playback byte stream.
//!
//! Output events are concatenated in event order into one byte string, the
//! pane's *stream*. Byte offsets everywhere in the index are offsets into it.
//! Each output event contributes its `playbackData` when it has one (the
//! self-contained bytes replay must feed) and its `data` otherwise.

use tap_model::PaneId;
use tap_model::event::{EventKind, InputOrigin, RawEvent};

use crate::program::{InputEdit, PaneProgram};

/// One event, as the index sees it. `items[i]` is the pane's event `i`.
#[derive(Debug, Clone)]
pub struct Item {
    /// Source time in microseconds.
    pub at_us: u64,
    /// What the event does to the stream.
    pub kind: ItemKind,
    /// Stream offset at this event: every output byte of earlier events.
    pub offset: u64,
}

/// The parts of an event the index uses.
#[derive(Debug, Clone)]
pub enum ItemKind {
    /// Output bytes `offset..offset + len` of the stream.
    Output {
        /// Number of bytes.
        len: u64,
    },
    /// The terminal was resized.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Bytes written to the PTY.
    Input {
        /// The bytes (empty when redacted).
        data: Vec<u8>,
        /// Where they came from.
        origin: Option<InputOrigin>,
        /// Whether the bytes were withheld.
        redacted: bool,
    },
    /// Anything that does not affect the terminal (scroll, marker, layout,
    /// visibility).
    Other,
}

/// A pane's stream and its events.
#[derive(Debug, Clone)]
pub struct PaneStream {
    /// The pane.
    pub pane: PaneId,
    /// Columns and rows at the start.
    pub initial_size: (u16, u16),
    /// One item per event, in event order.
    pub items: Vec<Item>,
    /// The concatenated playback bytes.
    pub bytes: Vec<u8>,
    /// End of the recording in microseconds.
    pub end_us: u64,
    /// The edits that turn the raw stream into [`PaneStream::bytes`]; the
    /// identity for an unedited pane.
    pub program: PaneProgram,
}

impl PaneStream {
    /// Builds the stream from a pane's events.
    pub fn new(pane: PaneId, initial_size: (u16, u16), events: &[RawEvent], end_us: u64) -> Self {
        let mut bytes = Vec::new();
        let items = events
            .iter()
            .map(|event| {
                let offset = bytes.len() as u64;
                let kind = match &event.kind {
                    EventKind::Output(output) => {
                        let data = output.playback.as_ref().map_or(&output.data, |p| &p.data);
                        bytes.extend_from_slice(data);
                        ItemKind::Output {
                            len: data.len() as u64,
                        }
                    }
                    EventKind::Resize { cols, rows } => ItemKind::Resize {
                        cols: *cols,
                        rows: *rows,
                    },
                    EventKind::Input(input) => ItemKind::Input {
                        data: input.data.clone(),
                        origin: input.origin,
                        redacted: input.redacted == Some(true),
                    },
                    EventKind::Scroll { .. }
                    | EventKind::Marker { .. }
                    | EventKind::Layout(_)
                    | EventKind::Visibility { .. } => ItemKind::Other,
                };
                Item {
                    at_us: event.at_us,
                    kind,
                    offset,
                }
            })
            .collect();
        let end_us = events.last().map_or(end_us, |e| end_us.max(e.at_us));
        let program = PaneProgram::identity(bytes.len() as u64);
        Self {
            pane,
            initial_size,
            items,
            bytes,
            end_us,
            program,
        }
    }

    /// The raw stream's length, for building a [`PaneProgram`] over it.
    pub fn raw_len(events: &[RawEvent]) -> u64 {
        events
            .iter()
            .map(|event| match &event.kind {
                EventKind::Output(output) => output
                    .playback
                    .as_ref()
                    .map_or(&output.data, |p| &p.data)
                    .len() as u64,
                _ => 0,
            })
            .sum()
    }

    /// The stream an edited pane plays: `program` applied to the raw stream
    /// of `events`. Every event keeps its item; each output event plays the
    /// program bytes derived from its source bytes
    /// ([`PaneProgram::boundary`]), so an event can play nothing. Withheld
    /// input events lose their bytes, or (when their command was excised)
    /// become [`ItemKind::Other`]. With the identity program this is exactly
    /// [`PaneStream::new`].
    pub fn with_program(
        pane: PaneId,
        initial_size: (u16, u16),
        events: &[RawEvent],
        end_us: u64,
        program: PaneProgram,
    ) -> Self {
        let raw = Self::new(pane, initial_size, events, end_us);
        if program.is_identity() {
            return raw;
        }
        assert_eq!(
            program.source_len(),
            raw.len(),
            "a program must be built over the pane's raw stream"
        );
        let bytes = program.render(&raw.bytes);
        let items = raw
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let offset = program.boundary(item.offset);
                let kind = match &item.kind {
                    ItemKind::Output { len } => ItemKind::Output {
                        len: program.boundary(item.offset + len) - offset,
                    },
                    ItemKind::Input {
                        data,
                        origin,
                        redacted,
                    } => match program.inputs().get(&index) {
                        None => ItemKind::Input {
                            data: data.clone(),
                            origin: *origin,
                            redacted: *redacted,
                        },
                        Some(InputEdit::Redacted { .. }) => ItemKind::Input {
                            data: Vec::new(),
                            origin: *origin,
                            redacted: true,
                        },
                        Some(InputEdit::Removed { .. }) => ItemKind::Other,
                    },
                    other => other.clone(),
                };
                Item {
                    at_us: item.at_us,
                    kind,
                    offset,
                }
            })
            .collect();
        Self {
            pane: raw.pane,
            initial_size,
            items,
            bytes,
            end_us: raw.end_us,
            program,
        }
    }

    /// Total stream length.
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether the pane wrote no output.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The output item that holds stream byte `byte`, if any.
    pub fn output_item_at(&self, byte: u64) -> Option<usize> {
        // The last item starting at or before `byte` that has output covering it.
        let mut index = self.items.partition_point(|item| item.offset <= byte);
        while index > 0 {
            index -= 1;
            let item = &self.items[index];
            if let ItemKind::Output { len } = item.kind
                && byte < item.offset + len
            {
                return Some(index);
            }
            if item.offset < byte {
                break;
            }
        }
        None
    }

    /// Source time of stream byte `byte`: the time of the output event that
    /// wrote it, or the end of the recording for the end of the stream.
    pub fn time_of_byte(&self, byte: u64) -> u64 {
        self.output_item_at(byte)
            .map_or(self.end_us, |index| self.items[index].at_us)
    }

    /// Index of the first item recorded after `source_us`: replaying items
    /// before it gives the screen at `source_us`.
    pub fn items_through(&self, source_us: u64) -> usize {
        self.items.partition_point(|item| item.at_us <= source_us)
    }
}
