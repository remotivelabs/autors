//! Error types.

/// MAP file parsing error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Text parsing error.
    #[error("parse error at offset {offset:#x}: {message}")]
    Parse {
        /// Offset of the failure (always 0 for this text format).
        offset: u64,
        /// Error description.
        message: String,
    },

    /// A2L update error.
    #[error("a2l update error: {0}")]
    Update(String),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Error from the autors-symbols layer (A2L node view / symbol path parsing).
    #[error(transparent)]
    Symbols(#[from] autors_symbols::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
