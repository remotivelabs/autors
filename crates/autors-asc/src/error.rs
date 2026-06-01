//! Error types for ASC parsing and writing.

/// Error returned by ASC operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A line could not be interpreted as a valid ASC construct.
    #[error("parse error on line {line}: {message}")]
    Parse {
        /// One-based input line number.
        line: usize,
        /// Description of the invalid input.
        message: String,
    },

    /// A value cannot be represented in an ASC output file.
    #[error("write error: {0}")]
    Write(String),

    /// Underlying file-system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient ASC result alias.
pub type Result<T> = std::result::Result<T, Error>;
