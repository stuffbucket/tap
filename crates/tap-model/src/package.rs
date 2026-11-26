//! `Session.tap/`: a recording as a directory.
//!
//! ```text
//! Session.tap/
//!   manifest.json         Manifest: format version, panes, canvas, metadata
//!   events/<pane>.tlog    each pane's raw events (see crate::tlog); never edited
//!   edits.json            Edits: the edit list, presentation, overlays
//!   index/                derived, disposable (not written by this crate)
//!   checkpoints/          derived, disposable (not written by this crate)
//!   thumbs/               derived, disposable (not written by this crate)
//! ```
//!
//! The raw event logs are authoritative and immutable once the package is
//! created. `edits.json` is replaced atomically on every save. Everything in
//! [`DERIVED_DIRS`] can be deleted at any time and rebuilt from the rest;
//! [`open`] ignores those directories.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::edits::{EDITS_FORMAT_VERSION, Edits};
use crate::manifest::{Manifest, PACKAGE_FORMAT_VERSION};
use crate::session::{Session, validate_edits};
use crate::{Error, fsutil, tlog};

/// The manifest's file name.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The edits document's file name.
pub const EDITS_FILE: &str = "edits.json";
/// The directory holding one `.tlog` per pane.
pub const EVENTS_DIR: &str = "events";
/// Directories of derived data: rebuildable caches, safe to delete.
pub const DERIVED_DIRS: [&str; 3] = ["index", "checkpoints", "thumbs"];

/// Writes `session` as a new package at `dir`, which must not exist.
///
/// The package is assembled in a hidden sibling directory and renamed into
/// place, so `dir` either does not exist or is complete. Each file inside is
/// itself written to a temp file, synced and renamed.
pub fn create(dir: &Path, session: &Session) -> Result<(), Error> {
    session.validate()?;
    if dir.exists() {
        return Err(Error::io(
            dir,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a package already exists here",
            ),
        ));
    }
    let staging = fsutil::temp_sibling(dir);
    let result = (|| {
        let events_dir = staging.join(EVENTS_DIR);
        fs::create_dir_all(&events_dir).map_err(|e| Error::io(&events_dir, e))?;
        for pane in &session.manifest.panes {
            tlog::write_file(
                &events_dir.join(pane.id.tlog_file_name()),
                session.pane_events(&pane.id),
            )?;
        }
        fsutil::sync_dir(&events_dir)?;
        write_json(&staging.join(EDITS_FILE), &session.edits)?;
        write_json(&staging.join(MANIFEST_FILE), &session.manifest)?;
        fsutil::sync_dir(&staging)?;
        fs::rename(&staging, dir).map_err(|e| Error::io(dir, e))?;
        fsutil::sync_parent(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Reads and validates the package at `dir`. Any damage to an event log is
/// an error naming the file, record and byte offset; use
/// [`tlog::recover`] to salvage a damaged log.
pub fn open(dir: &Path) -> Result<Session, Error> {
    let manifest: Manifest = read_json(&dir.join(MANIFEST_FILE), PACKAGE_FORMAT_VERSION)?;
    let edits: Edits = read_json(&dir.join(EDITS_FILE), EDITS_FORMAT_VERSION)?;
    let events_dir = dir.join(EVENTS_DIR);
    let mut events = BTreeMap::new();
    for pane in &manifest.panes {
        events.insert(
            pane.id.clone(),
            tlog::read_file(&events_dir.join(pane.id.tlog_file_name()))?,
        );
    }
    let listed: HashSet<String> = manifest
        .panes
        .iter()
        .map(|p| p.id.tlog_file_name())
        .collect();
    for entry in fs::read_dir(&events_dir).map_err(|e| Error::io(&events_dir, e))? {
        let path = entry.map_err(|e| Error::io(&events_dir, e))?.path();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if path.extension() == Some(OsStr::new("tlog")) && !listed.contains(name) {
            return Err(Error::Invalid(format!(
                "{}: event log for a pane the manifest does not list",
                path.display()
            )));
        }
    }
    let session = Session {
        manifest,
        events,
        edits,
    };
    session.validate()?;
    Ok(session)
}

/// Replaces the package's `edits.json` atomically, after checking `edits`
/// against the package's manifest.
pub fn save_edits(dir: &Path, edits: &Edits) -> Result<(), Error> {
    let manifest: Manifest = read_json(&dir.join(MANIFEST_FILE), PACKAGE_FORMAT_VERSION)?;
    validate_edits(&manifest, edits)?;
    write_json(&dir.join(EDITS_FILE), edits)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| Error::json(path.display().to_string(), e))?;
    bytes.push(b'\n');
    fsutil::write_atomic(path, &bytes)
}

/// Parses a versioned JSON document, checking `formatVersion` before the
/// schema so a newer file gets a version error rather than a field error.
fn read_json<T: DeserializeOwned>(path: &Path, supported: u32) -> Result<T, Error> {
    let context = path.display().to_string();
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::json(&context, e))?;
    match value
        .get("formatVersion")
        .and_then(serde_json::Value::as_u64)
    {
        Some(v) if v == u64::from(supported) => {}
        Some(v) => {
            return Err(Error::Invalid(format!(
                "{context}: format version {v} is not supported (this build reads version {supported})"
            )));
        }
        None => {
            return Err(Error::Invalid(format!(
                "{context}: missing or invalid formatVersion"
            )));
        }
    }
    serde_json::from_value(value).map_err(|e| Error::json(context, e))
}
