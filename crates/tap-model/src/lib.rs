//! Tap's recording model.
//!
//! This crate holds everything about a recording that needs no terminal
//! emulator. So far:
//!
//! - the raw event model ([`event`]), mirroring `TapEvent` in `src/model.ts`
//!   with integer microseconds and a per-pane sequence number;
//! - the per-pane event log ([`tlog`]), an append-safe, checksummed record
//!   log;
//! - the edit decision list ([`edl`]).

mod error;
mod fsutil;
mod jsnum;
mod varint;

pub mod edl;
pub mod event;
pub mod pane_id;
pub mod tlog;

pub use edl::{EditEntry, EditOp, Src, SrcRange};
pub use error::Error;
pub use event::{EventKind, RawEvent};
pub use pane_id::PaneId;
pub use tlog::TlogError;
