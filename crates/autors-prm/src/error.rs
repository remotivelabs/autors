//! Error types.

/// Errors produced by the PRM layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// General execution error (constructed via [`crate::prm_if::prm_error`]).
    #[error("{0}")]
    General(String),

    /// Script parse error.
    #[error("parse error: {0}")]
    Parse(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
