//! Error types.

/// Error type for MDF reading and writing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binary structure parse error.
    #[error("parse error at offset {offset:#x}: {message}")]
    Parse {
        /// Offset at which the error occurred.
        offset: u64,
        /// Description of the error.
        message: String,
    },

    /// Write error.
    #[error("write error: {0}")]
    Write(String),

    /// Compression/decompression error.
    #[error("compression error: {0}")]
    Compression(String),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Error from the autors-a2l layer.
    #[error(transparent)]
    Core(#[from] autors_a2l::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
