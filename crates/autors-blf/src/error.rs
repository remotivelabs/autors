//! Error types.

/// Error type for BLF reading and writing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binary structure parse error.
    #[error("parse error at offset {offset:#x}: {message}")]
    Parse {
        /// Offset at which the error occurred.
        offset: u64,
        /// Error description.
        message: String,
    },

    /// Write-out error.
    #[error("write error: {0}")]
    Write(String),

    /// Compression/decompression error.
    #[error("compression error: {0}")]
    Compression(String),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
