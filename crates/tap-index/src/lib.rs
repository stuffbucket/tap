//! Tap's recording index: OSC 133 marks, command segments, the transcript
//! and checkpointed seek.
//!
//! Each pane's output is one byte stream ([`stream`]). [`scan`] feeds it
//! through libghostty-vt one escape sequence at a time, so every OSC 133
//! mark, alternate-screen switch, clear and reset gets the exact byte
//! offset of its sequence.
//!
//! All use of libghostty-vt is inside [`vt`].
//!
//! Nothing here is published (`publish = false`).

pub mod scan;
pub mod stream;
pub mod vt;

pub use scan::{Mark, MarkKind};
pub use vt::VtError;
