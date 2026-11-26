//! Small file-system helpers: atomic replace and directory sync.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Error;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique sibling path for staging `target` before a rename.
pub(crate) fn temp_sibling(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "tap".into(), |n| n.to_string_lossy().into_owned());
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    target.with_file_name(format!(
        ".{name}.{}-{nanos}-{unique}.tmp",
        std::process::id()
    ))
}

/// Writes `bytes` to `target` so that readers see either the old file or the
/// complete new one: write a sibling temp file, flush it to disk, rename it
/// over `target`, then flush the directory entry.
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), Error> {
    let temp = temp_sibling(target);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| Error::io(&temp, e))?;
        file.write_all(bytes).map_err(|e| Error::io(&temp, e))?;
        file.sync_all().map_err(|e| Error::io(&temp, e))?;
        drop(file);
        fs::rename(&temp, target).map_err(|e| Error::io(target, e))?;
        sync_parent(target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Flushes the directory containing `path`, so a rename into it survives a
/// crash. A no-op where directories cannot be opened (Windows).
pub(crate) fn sync_parent(path: &Path) -> Result<(), Error> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    sync_dir(parent)
}

/// Flushes a directory's entries to disk (Unix only).
pub(crate) fn sync_dir(dir: &Path) -> Result<(), Error> {
    if cfg!(unix) {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| Error::io(dir, e))?;
    }
    Ok(())
}
