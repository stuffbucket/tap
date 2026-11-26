//! Errors.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error as ThisError;

use crate::timemap::EvalError;
use crate::tlog::TlogError;

/// Anything that can go wrong reading, writing, converting or evaluating a
/// recording.
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
    /// A JSON document does not parse or does not match the schema.
    #[error("{context}: {source}")]
    Json {
        /// Which document or field.
        context: String,
        /// The parser's message (with line and column when known).
        #[source]
        source: serde_json::Error,
    },
    /// The data parses but breaks a rule; the message names the field.
    #[error("{0}")]
    Invalid(String),
    /// The edit list cannot be evaluated.
    #[error(transparent)]
    Eval(#[from] EvalError),
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

    pub(crate) fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json {
            context: context.into(),
            source,
        }
    }
}
