//! Errors and warnings.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why an operation on a store failed.
#[derive(Debug)]
pub enum Error {
    /// The store has no part (§20.2.2).
    Missing(PathBuf),
    /// Reading or writing a file failed.
    Io { path: PathBuf, source: io::Error },
    /// A part is compressed with a codec this build does not include (§16.5).
    UnsupportedCodec { path: PathBuf, codec: &'static str },
    /// The path names no TSVZ variant, or is not valid UTF-8.
    BadPath(PathBuf),
    /// The operation was refused; nothing was written. The text says why.
    Refused(String),
    /// No handler can be used for the store: none runs, or its pointer file or socket cannot be
    /// trusted (§21.2.4). The text says why.
    NoHandler(String),
    /// The handler answered a request with status 1 (failed) or 2 (usage error) (§20.6, §21.6).
    Handler { status: i32, message: String },
    /// The connection to the handler broke during a request, which may or may not have been
    /// applied.
    HandlerLost(String),
}

impl Error {
    pub fn io(path: &Path, source: io::Error) -> Error {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing(path) => write!(f, "{}: no such store", path.display()),
            Error::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            Error::UnsupportedCodec { path, codec } => {
                write!(
                    f,
                    "{}: this tsvz cannot read {} parts",
                    path.display(),
                    codec
                )
            }
            Error::BadPath(path) => write!(
                f,
                "{}: not a TSVZ store path (it needs a .tsvz, .csvz, .nsvz, .psvz, .tsv, .csv, .nsv or .psv extension, and must be UTF-8)",
                path.display()
            ),
            Error::Refused(why) => f.write_str(why),
            Error::NoHandler(why) => write!(f, "no handler: {why}"),
            Error::Handler { status, message } => {
                write!(f, "the handler answered status {status}: {message}")
            }
            Error::HandlerLost(why) => write!(
                f,
                "lost the handler ({why}); the request may or may not have been applied"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A problem a reader or writer tolerated, and what it did about it (§4.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub part: PathBuf,
    pub offset: Option<u64>,
    pub message: String,
}

/// Where warnings go.
pub type WarnFn = Arc<dyn Fn(&Warning) + Send + Sync>;
