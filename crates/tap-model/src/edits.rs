//! `edits.json`: everything the user changes. Rewritten atomically on every
//! save; the raw events are never touched.

use serde::{Deserialize, Serialize};

use crate::edl::EditEntry;
use crate::jsnum;

/// The `edits.json` format version this build reads and writes.
pub const EDITS_FORMAT_VERSION: u32 = 1;

/// `edits.json`.
///
/// Times in overlays are **source** microseconds, like every other anchor;
/// they go through the time map when rendered. Like the manifest, the
/// document is closed within a format version (unknown fields are rejected).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Edits {
    /// Format version; must be [`EDITS_FORMAT_VERSION`].
    pub format_version: u32,
    /// The edit decision list, in the order the user made the edits. The
    /// time-only ops' effect does not depend on this order.
    pub edits: Vec<EditEntry>,
    /// How the panes are drawn, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
    /// Text and image overlays.
    pub overlays: Vec<Overlay>,
}

impl Default for Edits {
    fn default() -> Self {
        Self {
            format_version: EDITS_FORMAT_VERSION,
            edits: Vec::new(),
            presentation: None,
            overlays: Vec::new(),
        }
    }
}

/// Window bar style (`TapPresentation.windowBar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowBar {
    /// No window bar.
    None,
    /// Grey rings on the left.
    Rings,
    /// Grey rings on the right.
    RingsRight,
    /// Traffic lights on the left.
    Colorful,
    /// Traffic lights on the right.
    ColorfulRight,
}

/// A colour theme (`TapPresentation.theme`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Background colour.
    pub background: String,
    /// Foreground colour.
    pub foreground: String,
    /// Cursor colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// The 16 (or more) palette colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
}

/// The look of the output (`TapPresentation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Presentation {
    /// Space between the window edge and the terminal grid, in pixels.
    #[serde(serialize_with = "jsnum::serialize")]
    pub padding: f64,
    /// Space around the window, in pixels.
    #[serde(serialize_with = "jsnum::serialize")]
    pub margin: f64,
    /// CSS colour of the margin.
    pub margin_fill: String,
    /// Window corner radius, in pixels.
    #[serde(serialize_with = "jsnum::serialize")]
    pub border_radius: f64,
    /// Window bar style.
    pub window_bar: WindowBar,
    /// Window bar height, in pixels.
    #[serde(serialize_with = "jsnum::serialize")]
    pub window_bar_size: f64,
    /// Title shown in the window bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// Drop-shadow size, in pixels.
    #[serde(serialize_with = "jsnum::serialize")]
    pub shadow: f64,
    /// Terminal font family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// Extra space between cells, in pixels.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "jsnum::option::serialize"
    )]
    pub letter_spacing: Option<f64>,
    /// Line height as a multiple of the font size.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "jsnum::option::serialize"
    )]
    pub line_height: Option<f64>,
    /// Whether the cursor blinks.
    pub cursor_blink: bool,
    /// Where a looping export starts, as a percentage of its length.
    #[serde(serialize_with = "jsnum::serialize")]
    pub loop_offset_percent: f64,
    /// Name of the colour theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_name: Option<String>,
    /// The colour theme itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
}

impl Presentation {
    /// The checks `validateProject` applies.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("padding", self.padding),
            ("margin", self.margin),
            ("borderRadius", self.border_radius),
            ("windowBarSize", self.window_bar_size),
            ("shadow", self.shadow),
        ] {
            if !value.is_finite() || !(0.0..=4096.0).contains(&value) {
                return Err(format!("presentation.{field} is invalid"));
            }
        }
        if !self.loop_offset_percent.is_finite()
            || !(0.0..=100.0).contains(&self.loop_offset_percent)
        {
            return Err("presentation.loopOffsetPercent must be from 0 through 100".into());
        }
        Ok(())
    }
}

/// A text or image drawn over the canvas (`TapOverlay`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Overlay {
    /// Text.
    Text {
        /// Unique, non-empty id.
        id: String,
        /// First source microsecond it is shown.
        start_us: u64,
        /// Last source microsecond it is shown.
        end_us: u64,
        /// Left edge in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        x: f64,
        /// Top edge in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        y: f64,
        /// The text.
        text: String,
        /// CSS colour.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        /// CSS font.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
    },
    /// An image.
    Image {
        /// Unique, non-empty id.
        id: String,
        /// First source microsecond it is shown.
        start_us: u64,
        /// Last source microsecond it is shown.
        end_us: u64,
        /// Left edge in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        x: f64,
        /// Top edge in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        y: f64,
        /// Width in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        width: f64,
        /// Height in canvas pixels.
        #[serde(serialize_with = "jsnum::serialize")]
        height: f64,
        /// Image path or URL.
        source: String,
        /// Opacity, 0..=1.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "jsnum::option::serialize"
        )]
        opacity: Option<f64>,
    },
}

impl Overlay {
    /// The overlay's id.
    pub fn id(&self) -> &str {
        match self {
            Self::Text { id, .. } | Self::Image { id, .. } => id,
        }
    }

    /// The overlay's `[start_us, end_us]`.
    pub fn span(&self) -> (u64, u64) {
        match *self {
            Self::Text {
                start_us, end_us, ..
            }
            | Self::Image {
                start_us, end_us, ..
            } => (start_us, end_us),
        }
    }
}
