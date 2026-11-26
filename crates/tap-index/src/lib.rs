//! Tap's recording index: OSC 133 marks, command segments, the transcript
//! and checkpointed seek.
//!
//! All use of libghostty-vt is inside [`vt`].
//!
//! Nothing here is published (`publish = false`).

pub mod vt;

pub use vt::VtError;
