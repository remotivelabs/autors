#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("formula error: {0}")]
    Formula(String),

    #[error("value error: {0}")]
    Value(String),
}

pub type Result<T> = std::result::Result<T, Error>;
