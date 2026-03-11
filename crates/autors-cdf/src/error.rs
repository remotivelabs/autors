//! Error types.

/// Errors of the CDF layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// XML (de)serialization error.
    #[error("xml error: {0}")]
    Xml(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
