//! Error types.

/// ISO-TP transport layer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Protocol error (frame too short, malformed format, etc.).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// CAN transport error (device send/receive failure).
    #[error(transparent)]
    Can(#[from] autors_can::Error),

    /// LIN transport error (device send/receive failure).
    #[error(transparent)]
    Lin(#[from] autors_lin::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
