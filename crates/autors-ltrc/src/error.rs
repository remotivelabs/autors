//! Error types for LTRC parsing.

/// Error returned while reading a PLIN-View trace.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A line does not conform to the selected LTRC version.
    #[error("parse error on line {line}: {message}")]
    Parse {
        /// One-based input line number.
        line: usize,
        /// Description of the invalid input.
        message: String,
    },

    /// Underlying file-system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient LTRC result alias.
pub type Result<T> = std::result::Result<T, Error>;
