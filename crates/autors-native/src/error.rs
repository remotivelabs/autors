#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Load(#[from] libloading::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
