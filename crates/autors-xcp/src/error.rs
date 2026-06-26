//! Error types.

/// XCP protocol-layer error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Protocol error (command failure, malformed response, timeout, etc.).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Message parsing error.
    #[error("parse error: {0}")]
    Parse(String),

    /// autors-comm protocol base error.
    #[error(transparent)]
    Comm(#[from] autors_comm::Error),

    /// autors-can transport-layer error.
    #[error(transparent)]
    Can(#[from] autors_can::Error),

    /// autors-a2l layer error.
    #[error(transparent)]
    Core(#[from] autors_a2l::Error),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient Result alias.
pub type Result<T> = std::result::Result<T, Error>;
