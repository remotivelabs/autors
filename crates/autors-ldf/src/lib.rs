//! LIN Description File support.
//!
//! The crate exposes an editable LDF network model in [`model`], payload
//! encoding and decoding in [`codec`], and standard LIN diagnostic request
//! helpers in [`diagnostic`]. Parsing and writing are available through
//! [`model::Ldf`]. Parser implementation details are intentionally private.

pub mod codec;
pub mod diagnostic;
pub mod error;
pub mod model;

mod parser;
mod writer;

pub use error::{Error, Result};
