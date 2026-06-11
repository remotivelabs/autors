//! Types shared by CAN and LIN transmission hooks.

use std::error::Error as StdError;

/// Stable token returned when a hook is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookId(pub(crate) u64);

impl HookId {
    /// Numeric registration token.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Error type returned by a hook.
pub type HookError = Box<dyn StdError + Send + Sync + 'static>;

/// Result returned by a hook.
pub type HookResult = std::result::Result<(), HookError>;

/// Why a transmission was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmissionCause {
    /// A DBC cycle timer expired.
    Cyclic,
    /// The application requested a one-shot transmission.
    Triggered,
    /// An LDF schedule slot became due.
    Schedule,
}

/// Result visible to after-send hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// A payload frame was accepted by the adapter.
    Sent {
        /// Payload bytes reported as sent by the adapter.
        bytes: usize,
    },
    /// A LIN header-only request was accepted by the adapter.
    Requested,
    /// The adapter returned an error.
    Failed {
        /// Display form of the adapter error.
        error: String,
    },
}
