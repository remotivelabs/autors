//! Error types.

/// Symbol parsing and A2L update errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binary/text parsing error.
    #[error("parse error at offset {offset:#x}: {message}")]
    Parse {
        /// Offset of the failure (0 for text formats).
        offset: u64,
        /// Error description.
        message: String,
    },

    /// A2L update error.
    #[error("a2l update error: {0}")]
    Update(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Error from the autors-a2l layer.
    #[error(transparent)]
    Core(#[from] autors_a2l::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
