//! `manifest.json`: what a package contains.

use serde::{Deserialize, Serialize};

use crate::event::PaneLayout;
use crate::jsnum;
use crate::pane_id::PaneId;

/// The package format version this build reads and writes.
pub const PACKAGE_FORMAT_VERSION: u32 = 1;

/// `manifest.json`.
///
/// Within one format version the document is closed: unknown fields are
/// rejected rather than dropped, so that a newer writer's data is never
/// silently lost by an older reader re-saving the package. Adding a field
/// therefore means bumping [`PACKAGE_FORMAT_VERSION`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// Package format version; must be [`PACKAGE_FORMAT_VERSION`].
    pub format_version: u32,
    /// The recording's title.
    pub title: String,
    /// When the recording was made, as the capture wrote it (ISO 8601).
    pub created_at: String,
    /// Length of the source recording in microseconds. Every event's
    /// `at_us` is at most this.
    pub duration_us: u64,
    /// The output canvas.
    pub canvas: Canvas,
    /// The panes, in drawing order. Each has `events/<id>.tlog`.
    pub panes: Vec<PaneInfo>,
    /// Capture metadata.
    pub metadata: Metadata,
    /// Identifies the terminal engine that recorded the session (for example
    /// `libghostty-vt 1.0.1`). Informational: derived caches such as
    /// `checkpoints/<fingerprint>/` are keyed by the engine that *builds*
    /// them, not by this value.
    pub engine_fingerprint: String,
}

/// The output canvas (`TapCanvas`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    /// Width in pixels, 1..=8192.
    pub width: u32,
    /// Height in pixels, 1..=8192.
    pub height: u32,
    /// CSS background colour.
    pub background: String,
    /// Frames per second, in (0, 240].
    #[serde(serialize_with = "jsnum::serialize")]
    pub fps: f64,
}

/// A pane's static description (`TapPane` without its events).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaneInfo {
    /// The pane id; also the name of its event log.
    pub id: PaneId,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Initial columns, at least 1.
    pub cols: u16,
    /// Initial rows, at least 1.
    pub rows: u16,
    /// Font size in points, in (0, 512].
    #[serde(serialize_with = "jsnum::serialize")]
    pub font_size: f64,
    /// Initial position on the canvas.
    pub layout: PaneLayout,
}

/// The terminal family that recorded the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Terminal {
    /// Ghostty (libghostty-vt).
    Ghostty,
}

/// The unit of every `at_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timebase {
    /// Microseconds.
    Microseconds,
}

/// Capture metadata (`TapProject.metadata`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Metadata {
    /// Always `ghostty`.
    pub terminal: Terminal,
    /// Free-form description of the engine(s) used.
    pub terminal_engine: String,
    /// Always `microseconds`.
    pub timebase: Timebase,
    /// Where the recording came from (`pty`, `vhs-tape`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Shells that were set up to write OSC 133 prompt and command marks
    /// during capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration: Option<Vec<String>>,
}
