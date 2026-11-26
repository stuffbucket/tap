//! The single-file `.tap` JSON form (`TapProject` in `src/model.ts`):
//! import into a [`Session`] and export back.
//!
//! Import accepts what `validateProject` accepts, with two deliberate
//! differences: unknown fields are errors (so nothing is silently dropped),
//! and pane ids must be safe file names (see [`PaneId`]). Base64 must use
//! the standard alphabet; padding is optional.
//!
//! Export reproduces the imported JSON value: byte strings keep the
//! `encoding` they were written with, and absent optional fields stay
//! absent. (Text is pretty-printed with two spaces like the TypeScript
//! writer; key order may differ.)

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Error;
use crate::edits::{EDITS_FORMAT_VERSION, Edits, Overlay, Presentation};
use crate::event::{
    EventKind, Input, InputOrigin, Output, PaneLayout, Playback, RawEvent, TextEncoding,
};
use crate::jsnum::{self, MAX_SAFE_INTEGER};
use crate::manifest::{Canvas, Manifest, Metadata, PACKAGE_FORMAT_VERSION, PaneInfo};
use crate::pane_id::PaneId;
use crate::session::Session;

/// The legacy project version this module reads and writes.
pub const LEGACY_VERSION: u64 = 1;

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Encoding {
    Utf8,
    Base64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum LegacyEvent {
    Output {
        at_us: u64,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<Encoding>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playback_data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playback_encoding: Option<Encoding>,
    },
    Input {
        at_us: u64,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<Encoding>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<InputOrigin>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    Resize {
        at_us: u64,
        cols: u64,
        rows: u64,
    },
    Scroll {
        at_us: u64,
        delta: i64,
    },
    Marker {
        at_us: u64,
        label: String,
    },
    Layout {
        at_us: u64,
        layout: PaneLayout,
    },
    Visibility {
        at_us: u64,
        visible: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyPane<E> {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    cols: u64,
    rows: u64,
    #[serde(serialize_with = "jsnum::serialize")]
    font_size: f64,
    layout: PaneLayout,
    events: Vec<E>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyProject {
    version: u64,
    title: String,
    created_at: String,
    duration_us: u64,
    canvas: Canvas,
    #[serde(skip_serializing_if = "Option::is_none")]
    presentation: Option<Presentation>,
    panes: Vec<LegacyPane<LegacyEvent>>,
    overlays: Vec<Overlay>,
    metadata: Metadata,
}

const PROJECT_FIELDS: [&str; 9] = [
    "version",
    "title",
    "createdAt",
    "durationUs",
    "canvas",
    "presentation",
    "panes",
    "overlays",
    "metadata",
];

fn field<T: DeserializeOwned>(obj: &mut Map<String, Value>, name: &str) -> Result<T, Error> {
    let value = obj
        .remove(name)
        .ok_or_else(|| Error::Invalid(format!("project is missing field `{name}`")))?;
    serde_json::from_value(value).map_err(|e| Error::json(name, e))
}

fn optional_field<T: DeserializeOwned>(
    obj: &mut Map<String, Value>,
    name: &str,
) -> Result<Option<T>, Error> {
    obj.remove(name)
        .map(|value| serde_json::from_value(value).map_err(|e| Error::json(name, e)))
        .transpose()
}

fn decode(
    field: &str,
    data: &str,
    encoding: Option<Encoding>,
) -> Result<(Vec<u8>, TextEncoding), Error> {
    match encoding {
        None => Ok((data.as_bytes().to_vec(), TextEncoding::Unspecified)),
        Some(Encoding::Utf8) => Ok((data.as_bytes().to_vec(), TextEncoding::Utf8)),
        Some(Encoding::Base64) => BASE64
            .decode(data)
            .map(|bytes| (bytes, TextEncoding::Base64))
            .map_err(|e| Error::Invalid(format!("{field} is not valid base64: {e}"))),
    }
}

fn encode(bytes: &[u8], hint: TextEncoding) -> (String, Option<Encoding>) {
    let as_text = |e| std::str::from_utf8(bytes).ok().map(|s| (s.to_owned(), e));
    match hint {
        TextEncoding::Unspecified => as_text(None),
        TextEncoding::Utf8 => as_text(Some(Encoding::Utf8)),
        TextEncoding::Base64 => None,
    }
    .unwrap_or_else(|| (BASE64.encode(bytes), Some(Encoding::Base64)))
}

fn grid(field: &str, value: u64) -> Result<u16, Error> {
    u16::try_from(value)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| Error::Invalid(format!("{field} must be an integer from 1 through 65535")))
}

fn to_raw(field: &str, seq: u64, event: LegacyEvent) -> Result<RawEvent, Error> {
    let (at_us, kind) = match event {
        LegacyEvent::Output {
            at_us,
            data,
            encoding,
            playback_data,
            playback_encoding,
        } => {
            let (data, data_encoding) = decode(&format!("{field}.data"), &data, encoding)?;
            let playback = match (playback_data, playback_encoding) {
                (Some(p), enc) => {
                    let (data, encoding) = decode(&format!("{field}.playbackData"), &p, enc)?;
                    Some(Playback { data, encoding })
                }
                (None, Some(_)) => {
                    return Err(Error::Invalid(format!(
                        "{field} has playbackEncoding but no playbackData"
                    )));
                }
                (None, None) => None,
            };
            (
                at_us,
                EventKind::Output(Output {
                    data,
                    data_encoding,
                    playback,
                }),
            )
        }
        LegacyEvent::Input {
            at_us,
            data,
            encoding,
            origin,
            redacted,
        } => {
            let (data, data_encoding) = decode(&format!("{field}.data"), &data, encoding)?;
            (
                at_us,
                EventKind::Input(Input {
                    data,
                    data_encoding,
                    origin,
                    redacted,
                }),
            )
        }
        LegacyEvent::Resize { at_us, cols, rows } => (
            at_us,
            EventKind::Resize {
                cols: grid(&format!("{field}.cols"), cols)?,
                rows: grid(&format!("{field}.rows"), rows)?,
            },
        ),
        LegacyEvent::Scroll { at_us, delta } => (at_us, EventKind::Scroll { delta }),
        LegacyEvent::Marker { at_us, label } => (at_us, EventKind::Marker { label }),
        LegacyEvent::Layout { at_us, layout } => (at_us, EventKind::Layout(layout)),
        LegacyEvent::Visibility { at_us, visible } => (at_us, EventKind::Visibility { visible }),
    };
    if at_us > MAX_SAFE_INTEGER {
        return Err(Error::Invalid(format!(
            "{field}.atUs must be a non-negative safe integer"
        )));
    }
    Ok(RawEvent { at_us, seq, kind })
}

fn from_raw(event: &RawEvent) -> LegacyEvent {
    let at_us = event.at_us;
    match &event.kind {
        EventKind::Output(o) => {
            let (data, encoding) = encode(&o.data, o.data_encoding);
            let (playback_data, playback_encoding) = match &o.playback {
                Some(p) => {
                    let (d, e) = encode(&p.data, p.encoding);
                    (Some(d), e)
                }
                None => (None, None),
            };
            LegacyEvent::Output {
                at_us,
                data,
                encoding,
                playback_data,
                playback_encoding,
            }
        }
        EventKind::Input(i) => {
            let (data, encoding) = encode(&i.data, i.data_encoding);
            LegacyEvent::Input {
                at_us,
                data,
                encoding,
                origin: i.origin,
                redacted: i.redacted,
            }
        }
        &EventKind::Resize { cols, rows } => LegacyEvent::Resize {
            at_us,
            cols: u64::from(cols),
            rows: u64::from(rows),
        },
        &EventKind::Scroll { delta } => LegacyEvent::Scroll { at_us, delta },
        EventKind::Marker { label } => LegacyEvent::Marker {
            at_us,
            label: label.clone(),
        },
        &EventKind::Layout(layout) => LegacyEvent::Layout { at_us, layout },
        &EventKind::Visibility { visible } => LegacyEvent::Visibility { at_us, visible },
    }
}

/// Imports a legacy `.tap` JSON document. The result has an empty edit list;
/// the project's presentation and overlays move to [`Edits`], and each
/// pane's events are numbered `0, 1, 2, …` in array order.
pub fn import_json(json: &str) -> Result<Session, Error> {
    let value: Value = serde_json::from_str(json).map_err(|e| Error::json("project", e))?;
    let Value::Object(mut obj) = value else {
        return Err(Error::Invalid("project must be an object".into()));
    };
    let version = obj.get("version").cloned().unwrap_or(Value::Null);
    if version.as_u64() != Some(LEGACY_VERSION) {
        return Err(Error::Invalid(format!(
            "unsupported tap project version: {version}"
        )));
    }
    if let Some(unknown) = obj.keys().find(|k| !PROJECT_FIELDS.contains(&k.as_str())) {
        return Err(Error::Invalid(format!(
            "project has unknown field `{unknown}`"
        )));
    }
    let title: String = field(&mut obj, "title")?;
    let created_at: String = field(&mut obj, "createdAt")?;
    let duration_us: u64 = field(&mut obj, "durationUs")?;
    if duration_us > MAX_SAFE_INTEGER {
        return Err(Error::Invalid(
            "durationUs must be a non-negative safe integer".into(),
        ));
    }
    let canvas: Canvas = field(&mut obj, "canvas")?;
    let presentation: Option<Presentation> = optional_field(&mut obj, "presentation")?;
    let overlays: Vec<Overlay> = field(&mut obj, "overlays")?;
    let metadata: Metadata = field(&mut obj, "metadata")?;
    let panes: Vec<Value> = field(&mut obj, "panes")?;

    let mut infos = Vec::with_capacity(panes.len());
    let mut events = std::collections::BTreeMap::new();
    for (index, pane) in panes.into_iter().enumerate() {
        let name = format!("panes[{index}]");
        let pane: LegacyPane<Value> =
            serde_json::from_value(pane).map_err(|e| Error::json(&name, e))?;
        let id = PaneId::new(pane.id).map_err(|e| Error::Invalid(format!("{name}.id: {e}")))?;
        let mut raw = Vec::with_capacity(pane.events.len());
        for (i, event) in pane.events.into_iter().enumerate() {
            let field = format!("{name}.events[{i}]");
            let event: LegacyEvent =
                serde_json::from_value(event).map_err(|e| Error::json(&field, e))?;
            raw.push(to_raw(&field, i as u64, event)?);
        }
        if events.insert(id.clone(), raw).is_some() {
            return Err(Error::Invalid(format!("pane {index} needs a unique id")));
        }
        infos.push(PaneInfo {
            id,
            title: pane.title,
            cols: grid(&format!("{name}.cols"), pane.cols)?,
            rows: grid(&format!("{name}.rows"), pane.rows)?,
            font_size: pane.font_size,
            layout: pane.layout,
        });
    }

    let session = Session {
        manifest: Manifest {
            format_version: PACKAGE_FORMAT_VERSION,
            title,
            created_at,
            duration_us,
            canvas,
            panes: infos,
            engine_fingerprint: metadata.terminal_engine.clone(),
            metadata,
        },
        events,
        edits: Edits {
            format_version: EDITS_FORMAT_VERSION,
            edits: Vec::new(),
            presentation,
            overlays,
        },
    };
    session.validate()?;
    Ok(session)
}

/// Exports a session as a legacy `.tap` JSON document (pretty-printed, with
/// a trailing newline).
///
/// The legacy form has no edit list, so a session with any edit entries is
/// refused rather than exported without them; clear `edits.edits` first to
/// export the raw recording.
pub fn export_json(session: &Session) -> Result<String, Error> {
    session.validate()?;
    if !session.edits.edits.is_empty() {
        return Err(Error::Invalid(format!(
            "the legacy .tap format cannot hold edits, and this session has {}; clear them to export the raw recording",
            session.edits.edits.len()
        )));
    }
    let m = &session.manifest;
    let project = LegacyProject {
        version: LEGACY_VERSION,
        title: m.title.clone(),
        created_at: m.created_at.clone(),
        duration_us: m.duration_us,
        canvas: m.canvas.clone(),
        presentation: session.edits.presentation.clone(),
        panes: m
            .panes
            .iter()
            .map(|p| LegacyPane {
                id: p.id.as_str().to_owned(),
                title: p.title.clone(),
                cols: u64::from(p.cols),
                rows: u64::from(p.rows),
                font_size: p.font_size,
                layout: p.layout,
                events: session.pane_events(&p.id).iter().map(from_raw).collect(),
            })
            .collect(),
        overlays: session.edits.overlays.clone(),
        metadata: m.metadata.clone(),
    };
    let mut text = serde_json::to_string_pretty(&project).map_err(|e| Error::json("project", e))?;
    text.push('\n');
    Ok(text)
}
