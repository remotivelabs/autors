#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    General(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
