//! autors-xcp: hardware-agnostic XCP master (with SxI framing).
//! The crate provides byte-level command/response codecs, transports and the
//! master state machine in the [`xcp`] module, plus typed parsing of the A2L
//! `IF_DATA XCP` block in the [`ifdata_xcp`] module. The shared protocol base
//! (`CommMaster`/DAQ structures etc.) lives in autors-comm; CAN transport goes
//! through the autors-can abstraction; UDP/TCP transport ([`eth_transport`],
//! `std::net`) and SxI serial transport ([`sxi_serial`], serialport adapter +
//! XCP transport bridge) live in this crate.

pub mod error;
pub mod eth_transport;
pub mod ifdata_xcp;
pub mod sxi_serial;
pub mod xcp;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use error::{Error, Result};
