//! Error types for the data file layer.

/// Error type of the data file layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Value/address error (out-of-bounds access, etc.).
    #[error("value error: {0}")]
    Value(String),

    /// Data file parsing error.
    #[error("data file error at line {line}: {message}")]
    DataFile {
        /// Line number where the error occurred (0 means no line context).
        line: u32,
        /// Error description.
        message: String,
    },

    /// A recognized container that does not yet have a parser or writer.
    #[error("unsupported data file format: {0}")]
    UnsupportedFormat(String),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias for the data file layer.
pub type Result<T> = std::result::Result<T, Error>;
