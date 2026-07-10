#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Core(#[from] autors_a2l::Error),

    #[error(transparent)]
    IsoTp(#[from] autors_isotp::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
