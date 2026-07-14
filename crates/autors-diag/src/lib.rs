//! autors-diag: UDS/KWP2000/DoIP diagnostic protocols (session/message layer).
//! CAN ISO-TP and LIN diagnostic transport live in the autors-isotp crate, and ODX/PRM
//! file parsing lives in autors-odx / autors-prm. Typed parsing of the
//! KWP-domain A2L `IF_DATA` block is provided by the [`ifdata_kwp`] module.
//! Offline pcapng extraction is provided by [`doip_capture`]. The live
//! request-response flow is asynchronous at the core
//! ([`uds::UdsTransport`], [`doip_client::DoIpClient`], and the send/receive
//! paths of [`uds::UdsClient`] are all async, waiting via autors-runtime
//! primitives); synchronous callers use the [`blocking`] facade from the
//! `blocking` feature.

pub mod doip;
pub mod doip_capture;
pub mod doip_client;

pub mod error;
pub mod ifdata_kwp;
pub mod kwp;
pub mod uds;
pub mod uds_transport;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use error::{Error, Result};
