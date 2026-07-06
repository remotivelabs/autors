//! autors-ccp: a hardware-agnostic CCP (CAN Calibration Protocol) master.
//! - [`ccp`]: command/response codecs and the master-side protocol state
//!   machine.
//! - [`ifdata_ccp`]: typed parsing of the A2L `IF_DATA ASAP1B_CCP` block.
//! - [`blocking`] (feature `blocking`): a synchronous facade over the async
//!   master core.
//!
//! The shared protocol base (`CommMaster`, DAQ structures, ...) lives in
//! autors-comm; the CAN transport goes through the autors-can abstraction.
//! The CAN DAQ list types of [`ifdata_ccp`] reuse the XCP-side definitions
//! ([`autors_xcp::ifdata_xcp::XcpDaqListCanType`]), whose layout the CCP
//! `IF_DATA` block shares.

pub mod ccp;
pub mod error;
pub mod ifdata_ccp;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use error::{Error, Result};
