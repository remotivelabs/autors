//! Error types.

/// ELF/DWARF parsing error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binary parsing error.
    #[error("parse error at offset {offset:#x}: {message}")]
    Parse {
        /// Offset at which the error occurred.
        offset: u64,
        /// Error description.
        message: String,
    },

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Error from the autors-symbols layer (A2L node view / symbol path
    /// resolution).
    #[error(transparent)]
    Symbols(#[from] autors_symbols::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
