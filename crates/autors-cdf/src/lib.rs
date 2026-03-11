//! autors-cdf: object model and read/write support for ASAM CDF
//! (Calibration Data Format) XML files.
//! Provides the CDF XML document object model plus serialization and
//! deserialization entry points; see the [`cdf`] module.

pub mod cdf;
pub mod error;

pub use error::{Error, Result};
