//! autors-can: CAN hardware abstraction layer.
//! Provides the shared frame and error types ([`frame`], [`error`]), the
//! `CanDevice` trait ([`device`]), and pluggable hardware backends:
//! - Windows: vendor-native driver DLLs are loaded dynamically at runtime via
//!   libloading (Kvaser canlib32, Vector vxlapi, Peak PCANBasic); a missing
//!   driver is reported as an error. Each vendor is enabled on demand by a
//!   `vendor-*` cargo feature (all off by default, `all-vendors` enables all),
//!   see [`vendors`];
//! - Linux: the socketcan crate (feature `socketcan`, enabled by default);
//! - macOS: a stub backend (compiles, reports unsupported at runtime).

pub mod device;
pub mod error;
pub mod frame;
pub mod vendors;

#[cfg(feature = "blocking")]
pub mod blocking;

#[cfg(all(target_os = "linux", feature = "socketcan"))]
pub mod socketcan;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub mod stub;

pub use error::{Error, Result};
