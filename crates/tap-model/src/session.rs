//! A whole recording in memory: manifest, raw events and edits.

use std::collections::{BTreeMap, HashSet};

use crate::Error;
use crate::edits::{EDITS_FORMAT_VERSION, Edits};
use crate::event::RawEvent;
use crate::jsnum::MAX_SAFE_INTEGER;
use crate::manifest::{Manifest, PACKAGE_FORMAT_VERSION};
use crate::pane_id::PaneId;
use crate::timemap::{EvalError, SourceTimeline, TimeMap};

/// Largest canvas side in pixels.
pub const MAX_CANVAS_SIDE: u32 = 8192;
/// Largest canvas area in pixels.
pub const MAX_CANVAS_PIXELS: u64 = 64 * 1024 * 1024;

/// A recording: what [`crate::package`] reads and writes and what the legacy
/// JSON form converts to.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `manifest.json`.
    pub manifest: Manifest,
    /// Each pane's raw events, keyed by pane id. Exactly the panes in the
    /// manifest.
    pub events: BTreeMap<PaneId, Vec<RawEvent>>,
    /// `edits.json`.
    pub edits: Edits,
}

impl Session {
    /// A pane's events, or an empty slice for an unknown pane.
    pub fn pane_events(&self, pane: &PaneId) -> &[RawEvent] {
        self.events.get(pane).map_or(&[], Vec::as_slice)
    }

    /// The source facts the time map needs.
    pub fn timeline(&self) -> SourceTimeline {
        let mut timeline = SourceTimeline::new(self.manifest.duration_us);
        for (pane, events) in &self.events {
            timeline.add_pane(pane, events);
        }
        timeline
    }

    /// Evaluates the edit list's time-only ops.
    pub fn time_map(&self) -> Result<TimeMap, EvalError> {
        TimeMap::build(&self.edits.edits, &self.timeline())
    }

    /// Checks everything `validateProject` in `src/model.ts` checks, plus the
    /// package's own rules: format versions, pane ids that are safe and
    /// distinct ignoring case, events for exactly the manifest's panes in
    /// `(at_us, seq)` order, durations representable in JavaScript, and edit
    /// ops that refer only to existing panes.
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = |m: String| Err(Error::Invalid(m));
        let m = &self.manifest;
        if m.format_version != PACKAGE_FORMAT_VERSION {
            return invalid(format!(
                "package format version {} is not supported (this build reads version {PACKAGE_FORMAT_VERSION})",
                m.format_version
            ));
        }
        if m.duration_us > MAX_SAFE_INTEGER {
            return invalid("durationUs must be a non-negative safe integer".into());
        }
        let c = &m.canvas;
        for (field, value) in [("canvas.width", c.width), ("canvas.height", c.height)] {
            if value == 0 || value > MAX_CANVAS_SIDE {
                return invalid(format!(
                    "{field} must be an integer from 1 through {MAX_CANVAS_SIDE}"
                ));
            }
        }
        if u64::from(c.width) * u64::from(c.height) > MAX_CANVAS_PIXELS {
            return invalid("canvas exceeds the 64-megapixel ceiling".into());
        }
        if !c.fps.is_finite() || c.fps <= 0.0 || c.fps > 240.0 {
            return invalid(
                "canvas.fps must be a positive finite number no greater than 240".into(),
            );
        }
        if m.panes.is_empty() {
            return invalid("project needs at least one pane".into());
        }
        let mut folded = HashSet::new();
        for (index, pane) in m.panes.iter().enumerate() {
            if !folded.insert(pane.id.as_str().to_ascii_lowercase()) {
                return invalid(format!(
                    "pane {index} needs a unique id: {:?} clashes with another pane (ids must differ ignoring case)",
                    pane.id.as_str()
                ));
            }
            if pane.cols == 0 || pane.rows == 0 {
                return invalid(format!(
                    "panes[{index}].cols and rows must be integers from 1 through 65535"
                ));
            }
            if !pane.font_size.is_finite() || pane.font_size <= 0.0 || pane.font_size > 512.0 {
                return invalid(format!(
                    "panes[{index}].fontSize must be a positive finite number no greater than 512"
                ));
            }
            pane.layout
                .validate(&format!("panes[{index}].layout"))
                .map_err(Error::Invalid)?;
            let Some(events) = self.events.get(&pane.id) else {
                return invalid(format!("pane {:?} has no event log", pane.id.as_str()));
            };
            validate_events(&format!("panes[{index}]"), events, m.duration_us)?;
        }
        if let Some(extra) = self
            .events
            .keys()
            .find(|id| !m.panes.iter().any(|p| &p.id == *id))
        {
            return invalid(format!(
                "events for pane {:?}, which the manifest does not list",
                extra.as_str()
            ));
        }
        validate_edits(m, &self.edits)
    }
}

/// Checks `edits.json` against the manifest: format version, presentation,
/// overlay times within the recording, and edit ops that refer only to the
/// manifest's panes.
pub fn validate_edits(manifest: &Manifest, edits: &Edits) -> Result<(), Error> {
    let invalid = |m: String| Err(Error::Invalid(m));
    if edits.format_version != EDITS_FORMAT_VERSION {
        return invalid(format!(
            "edits format version {} is not supported (this build reads version {EDITS_FORMAT_VERSION})",
            edits.format_version
        ));
    }
    if let Some(p) = &edits.presentation {
        p.validate().map_err(Error::Invalid)?;
    }
    for (index, overlay) in edits.overlays.iter().enumerate() {
        if overlay.id().is_empty() {
            return invalid(format!("overlays[{index}] has an invalid id or type"));
        }
        let (start, end) = overlay.span();
        if end < start || end > manifest.duration_us {
            return invalid(format!("overlays[{index}] has an invalid time range"));
        }
    }
    for (index, entry) in edits.edits.iter().enumerate() {
        for pane in entry.op.panes() {
            if !manifest.panes.iter().any(|p| &p.id == pane) {
                return invalid(format!(
                    "edit #{index} ({}) refers to unknown pane {:?}",
                    entry.op.name(),
                    pane.as_str()
                ));
            }
        }
    }
    Ok(())
}

/// Checks one pane's events: order, bounds and per-event rules.
pub(crate) fn validate_events(
    field: &str,
    events: &[RawEvent],
    duration_us: u64,
) -> Result<(), Error> {
    let mut previous: Option<&RawEvent> = None;
    for (i, event) in events.iter().enumerate() {
        let at = format!("{field}.events[{i}]");
        if let Some(p) = previous {
            if event.at_us < p.at_us {
                return Err(Error::Invalid(format!(
                    "{field}.events must use sorted, non-negative integer microseconds ({at} is at {}µs, before {}µs)",
                    event.at_us, p.at_us
                )));
            }
            if event.seq <= p.seq {
                return Err(Error::Invalid(format!(
                    "{at}: seq {} does not follow {}",
                    event.seq, p.seq
                )));
            }
        }
        if event.at_us > duration_us {
            return Err(Error::Invalid(format!(
                "{at} is beyond project duration ({}µs > {duration_us}µs)",
                event.at_us
            )));
        }
        event.validate(&at).map_err(Error::Invalid)?;
        previous = Some(event);
    }
    Ok(())
}
