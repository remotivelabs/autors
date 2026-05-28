//! autors-lin: hardware-independent LIN bus support.
//! The [`device`] module provides the [`device::LinDevice`] abstraction,
//! frame types, configuration, baudrates,
//! classic/enhanced checksums, and a logging frame queue. The [`vendors`]
//! module contains Windows vendor driver adapters (Kvaser/Vector/Peak), each
//! enabled on demand via the `vendor-kvaser`/`vendor-vector`/`vendor-peak`
//! cargo features (all off by default, `all-vendors` enables all three).

pub mod device;
pub mod error;
pub mod vendors;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use error::{Error, Result};
