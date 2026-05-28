//! Error types.

/// LIN protocol layer error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Protocol error.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Frame parsing error.
    #[error("parse error: {0}")]
    Parse(String),

    /// Driver/adapter layer error (vendor DLL loading or call failure).
    #[error("driver error: {0}")]
    Driver(String),

    /// Invalid frame or parameter format.
    #[error("invalid frame or parameter: {0}")]
    Invalid(String),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
