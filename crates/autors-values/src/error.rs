//! Error types.

/// Error type of the values layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Formula-related error (e.g. no inline formula available), carried as a message.
    #[error("formula error: {0}")]
    Formula(String),

    /// Value object error (type mismatch, dimension mismatch, etc.).
    #[error("value error: {0}")]
    Value(String),

    /// Error from the autors-formula layer (FORM expression compilation/evaluation).
    #[error(transparent)]
    FormulaEval(#[from] autors_formula::Error),

    /// Error from the autors-a2l layer.
    #[error(transparent)]
    Core(#[from] autors_a2l::Error),

    /// Error from the autors-datafile layer (e.g. ECU image read/write out of bounds).
    #[error(transparent)]
    Datafile(#[from] autors_datafile::Error),

    /// Error from the autors-cdf layer (CDF document read/write).
    #[error(transparent)]
    Cdf(#[from] autors_cdf::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
