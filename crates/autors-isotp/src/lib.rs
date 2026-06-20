//! Automotive diagnostic transport protocols.
//! [`transport::IsoTp`] implements ISO 15765-2 over CAN. [`lin::LinTpFsm`] and
//! [`lin_transport::LinTp`] implement the related ISO 17987-2 transport over
//! LIN diagnostic frames without exposing either bus device to service-layer
//! clients.

pub mod error;
pub mod isotp;
pub mod lin;
pub mod lin_transport;
pub mod transport;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use error::{Error, Result};
