//! Scheduler errors.

/// Error returned while configuring or advancing a scheduler.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration value or runtime selection is invalid.
    #[error("invalid scheduler configuration: {0}")]
    Invalid(String),

    /// A requested DBC/LDF object does not exist.
    #[error("scheduler object not found: {0}")]
    NotFound(String),

    /// A user hook rejected a transmission.
    #[error("{phase} hook {hook_id} failed for {message}: {reason}")]
    Hook {
        /// Hook phase (`before-send` or `after-send`).
        phase: &'static str,
        /// Registration token.
        hook_id: u64,
        /// DBC/LDF message name.
        message: String,
        /// Error reported by the hook.
        reason: String,
    },

    /// An adapter returned success without accepting the complete operation.
    #[error("{bus} adapter accepted {actual} of {expected} units for {message}")]
    IncompleteTransmission {
        /// Bus kind (`CAN` or `LIN`).
        bus: &'static str,
        /// DBC/LDF message name.
        message: String,
        /// Expected payload bytes, or one unit for a header request.
        expected: usize,
        /// Accepted payload bytes, or zero for a rejected header request.
        actual: usize,
    },

    /// CAN adapter operation failed.
    #[error(transparent)]
    Can(#[from] autors_can::Error),

    /// LIN adapter operation failed.
    #[error(transparent)]
    Lin(#[from] autors_lin::Error),

    /// LDF payload construction failed.
    #[error(transparent)]
    Ldf(#[from] autors_ldf::Error),
}

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;
