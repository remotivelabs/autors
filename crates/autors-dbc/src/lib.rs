//! CAN DBC database file reading and writing.
//! `dbc::DBCFile` parses DBC files into an editable object model and writes
//! them back byte-stably; `error` defines the crate's error type.

pub mod dbc;
pub mod error;

pub use error::{Error, Result};
