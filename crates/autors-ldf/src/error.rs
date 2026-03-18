//! Error types.

/// LDF parsing, validation, encoding, and writing error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Syntax error with source location.
    #[error("parse error at line {line}, column {column}: {message}")]
    Parse {
        /// One-based source line.
        line: usize,
        /// One-based source column.
        column: usize,
        /// Error description.
        message: String,
    },

    /// The parsed or manually constructed model violates an LDF constraint.
    #[error("invalid LDF: {0}")]
    Invalid(String),

    /// A named or numbered LDF object was not found.
    #[error("LDF object not found: {0}")]
    NotFound(String),

    /// A value cannot be encoded or decoded as requested.
    #[error("codec error: {0}")]
    Codec(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;
