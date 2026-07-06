//! Error types.

/// CCP protocol-layer error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Protocol error (command failed, malformed response, timeout, ...).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Message parsing error.
    #[error("parse error: {0}")]
    Parse(String),

    /// Error from the autors-comm protocol base.
    #[error(transparent)]
    Comm(#[from] autors_comm::Error),

    /// Error from the autors-can transport layer.
    #[error(transparent)]
    Can(#[from] autors_can::Error),

    /// Error from the autors-a2l layer.
    #[error(transparent)]
    Core(#[from] autors_a2l::Error),

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenient `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
