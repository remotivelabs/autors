//! Cooperative remaining-bus simulation for CAN and LIN.
//!
//! [`CanScheduler`] derives cyclic transmissions and transmitting nodes from a
//! DBC. [`LinScheduler`] executes LDF schedule tables, publishing frames for
//! simulated nodes and issuing header-only requests for nodes that remain on
//! the physical bus. Both schedulers are advanced explicitly through `poll`;
//! they never create an implicit background task.

pub mod can;
pub mod error;
pub mod hook;
pub mod lin;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use can::CanScheduler;
pub use error::{Error, Result};
pub use hook::{HookId, HookResult, SendOutcome, TransmissionCause};
pub use lin::LinScheduler;
