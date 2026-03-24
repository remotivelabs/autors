//! autors-odx: ODX diagnostic data (file parsing layer).
//! This crate provides parsing and data access for ODX (Open Diagnostic Data
//! Exchange) files: [`odx`] is the core ODX parsing foundation, [`odx_flash`]
//! covers ODX flash data (the flash-family facade), and [`odx_multifile`]
//! handles PDX containers and joint loading of multiple files linked through
//! cross-file `DOCREF` references.

pub mod error;
pub mod odx;
pub mod odx_flash;
pub mod odx_multifile;

pub use error::{Error, Result};
