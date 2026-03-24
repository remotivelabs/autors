//! Error types.

/// Errors of the ODX data layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Data parsing error.
    #[error("parse error: {0}")]
    Parse(String),

    /// XML (de)serialization error.
    #[error("xml error: {0}")]
    Xml(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// PDX (zip container) error.
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
