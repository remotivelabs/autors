//! autors-mdf: reading and writing of MDF (ASAM Measurement Data Format)
//! V3/V4 measurement files.
//! The crate is organized by format version:
//! - `base`: block structures and helpers shared by both format versions.
//! - `v3`: reader/writer for MDF 3.x files (`.dat`/`.mdf`).
//! - `v4`: reader/writer for MDF 4.x files (`.mf4`).
//! - `error`: the error type used by all readers and writers.

pub mod base;
pub mod error;
pub mod v3;
pub mod v4;

pub use error::{Error, Result};
