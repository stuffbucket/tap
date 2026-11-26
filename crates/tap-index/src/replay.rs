//! Replaying a pane's stream into a terminal.
//!
//! **Positions.** A replay position is `(item, intra)`: every event before
//! `items[item]` has been applied, and the first `intra` bytes of that event
//! (when it is output). Positions order lexicographically, so "the state at
//! source time t" is the position `(first event after t, 0)`.

use serde::Serialize;

use crate::stream::{ItemKind, PaneStream};
use crate::vt::{self, VtError};

/// Largest slice written in one call.
const SLICE: u64 = 64 * 1024;

/// A replay position: events before `item` applied, plus `intra` bytes of
/// `item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Position {
    /// Index of the next event to apply.
    pub item: usize,
    /// Bytes of that event already applied.
    pub intra: u64,
}

/// A terminal being driven through a pane's events.
pub(crate) struct Replay<'s> {
    stream: &'s PaneStream,
    pub term: vt::Terminal,
    position: Position,
}

impl<'s> Replay<'s> {
    /// A fresh terminal at the start of the pane.
    pub fn start(stream: &'s PaneStream) -> Result<Self, VtError> {
        let (cols, rows) = stream.initial_size;
        Ok(Self {
            stream,
            term: vt::Terminal::new(cols, rows)?,
            position: Position { item: 0, intra: 0 },
        })
    }

    /// Stream offset of everything applied so far.
    pub fn byte(&self) -> u64 {
        self.stream
            .items
            .get(self.position.item)
            .map_or(self.stream.len(), |item| item.offset + self.position.intra)
    }

    fn done(&self) -> bool {
        self.position.item >= self.stream.items.len()
    }

    /// Applies the next piece of the current event: up to `limit` stream
    /// bytes of output, or the whole of any other event.
    fn step(&mut self, limit: Option<u64>) -> Result<(), VtError> {
        let item = &self.stream.items[self.position.item];
        match item.kind {
            ItemKind::Output { len } => {
                let mut n = (len - self.position.intra).min(SLICE);
                if let Some(limit) = limit {
                    n = n.min(limit.saturating_sub(self.byte()).max(1));
                }
                if n > 0 {
                    let from = (item.offset + self.position.intra) as usize;
                    self.term.write(&self.stream.bytes[from..from + n as usize]);
                    self.term.take_signals();
                }
                self.position.intra += n;
                if self.position.intra >= len {
                    self.position = Position {
                        item: self.position.item + 1,
                        intra: 0,
                    };
                }
            }
            ItemKind::Resize { cols, rows } => {
                self.term.resize(cols, rows)?;
                self.position.item += 1;
            }
            ItemKind::Input { .. } | ItemKind::Other => self.position.item += 1,
        }
        Ok(())
    }

    /// Finishes a sequence left open, so the parser is at ground. Only
    /// output bytes are consumed; events in between are applied in order.
    fn settle(&mut self) -> Result<(), VtError> {
        while !self.term.at_ground() && !self.done() {
            let item = &self.stream.items[self.position.item];
            let ItemKind::Output { len } = item.kind else {
                self.step(None)?;
                continue;
            };
            let from = (item.offset + self.position.intra) as usize;
            let to = (item.offset + len) as usize;
            let consumed = match self.term.write_sequence(&self.stream.bytes[from..to])? {
                Some(step) => step.consumed as u64,
                None => (to - from) as u64,
            };
            self.term.take_signals();
            self.position.intra += consumed;
            if self.position.intra >= len {
                self.position = Position {
                    item: self.position.item + 1,
                    intra: 0,
                };
            }
        }
        Ok(())
    }

    /// Applies output until at least `byte` stream bytes have been applied
    /// and the parser is at ground. Events after the last byte needed are
    /// not applied. `at_ground` runs whenever a piece ends at ground.
    pub fn run_to_byte(
        &mut self,
        byte: u64,
        at_ground: &mut dyn FnMut(&Self),
    ) -> Result<(), VtError> {
        while self.byte() < byte && !self.done() {
            self.step(Some(byte))?;
            if self.term.at_ground() {
                at_ground(self);
            }
        }
        self.settle()
    }
}
