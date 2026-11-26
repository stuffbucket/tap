//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh, empty temp directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tap-model-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every legacy `.tap` project the round-trip tests cover: this crate's
/// fixtures plus any `.tap` file under the repository's `examples/` and
/// `test/` directories.
///
/// The fixtures `split-capture.tap` and `kitty-animation.tap` were recorded
/// from the projects in `examples/` with `tap capture-spec
/// examples/split-capture.json` and `tap tape examples/kitty-animation.tape`;
/// `every-event.tap` is hand-written to cover every event type and field.
pub fn legacy_projects() -> Vec<PathBuf> {
    let root = repo_root();
    let mut found = Vec::new();
    for dir in [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        root.join("examples"),
        root.join("test"),
    ] {
        collect(&dir, &mut found);
    }
    found.sort();
    found
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "tap") {
            out.push(path);
        }
    }
}
