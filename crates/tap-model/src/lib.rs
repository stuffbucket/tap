//! Tap's recording model.
//!
//! This crate holds everything about a recording that needs no terminal
//! emulator:
//!
//! - the raw event model ([`event`]), mirroring `TapEvent` in `src/model.ts`
//!   with integer microseconds and a per-pane sequence number;
//! - the `Session.tap/` package ([`package`]): `manifest.json`
//!   ([`manifest`]), one checksummed event log per pane ([`tlog`]) and
//!   `edits.json` ([`edits`]);
//! - import from and export to the single-file `.tap` JSON form
//!   ([`legacy`]);
//! - the edit decision list ([`edl`]) and the time map its time-only ops
//!   produce ([`timemap`]).
//!
//! The on-disk format is described precisely in this crate's `README.md`.

mod error;
mod fsutil;
mod jsnum;
mod varint;

pub mod edits;
pub mod edl;
pub mod event;
pub mod legacy;
pub mod manifest;
pub mod package;
pub mod pane_id;
pub mod session;
pub mod timemap;
pub mod tlog;

pub use edits::Edits;
pub use edl::{EditEntry, EditOp, Src, SrcRange};
pub use error::Error;
pub use event::{EventKind, RawEvent};
pub use manifest::Manifest;
pub use pane_id::PaneId;
pub use session::Session;
pub use timemap::{EvalError, SourceTimeline, TimeMap};
pub use tlog::TlogError;
