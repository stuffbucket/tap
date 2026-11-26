//! Errors.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error as ThisError;

use crate::tlog::TlogError;

/// Anything that can go wrong reading or writing a recording.
#[derive(Debug, ThisError)]
pub enum Error {
    /// A file-system operation failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// An event log is damaged or cannot be written.
    #[error("{}: {source}", path.display())]
    Tlog {
        /// The event log.
        path: PathBuf,
        /// What is wrong and where.
        #[source]
        source: TlogError,
    },
    /// The data parses but breaks a rule; the message names the field.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }

    pub(crate) fn tlog(path: &Path, source: TlogError) -> Self {
        Self::Tlog {
            path: path.to_owned(),
            source,
        }
    }
}
