//! Error types.

/// DBC parse/write error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Parse error with line number.
    #[error("parse error at line {line}: {message}")]
    Parse {
        /// Line where the error occurred (0 when no line context is available).
        line: u32,
        /// Error description.
        message: String,
    },

    /// Write error.
    #[error("write error: {0}")]
    Write(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient Result alias.
pub type Result<T> = std::result::Result<T, Error>;
