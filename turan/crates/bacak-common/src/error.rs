//! Crate-wide error type.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("malformed {kind} entry: {detail}")]
    Malformed { kind: &'static str, detail: String },

    #[error("ipc protocol error: {0}")]
    Ipc(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub fn malformed(kind: &'static str, detail: impl Into<String>) -> Self {
        Error::Malformed {
            kind,
            detail: detail.into(),
        }
    }
}
