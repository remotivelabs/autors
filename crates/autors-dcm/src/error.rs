//! Error types for the conservation-format layer.

/// Error raised while reading or writing a DCM/conservation-format file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A data (conservation format) file could not be parsed.
    #[error("data file error at line {line}: {message}")]
    DataFile {
        /// Line number where the error occurred (0 means no line context).
        line: u32,
        /// Description of the parse failure.
        message: String,
    },

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient `Result` alias for this crate's error type.
pub type Result<T> = std::result::Result<T, Error>;
