//! autors-map: symbol parsing for linker MAP files.
//! This crate reads linker MAP files into a symbol table and computes which
//! A2L nodes need their addresses synchronized from those symbols.
//! The shared types needed for symbol-driven A2L address updates
//! (`UpdaterSymbols`, `UpdateData`, symbol path parsing, etc.) live in the
//! `autors-symbols` crate, which this crate depends on.

pub mod error;
pub mod map;

pub use error::{Error, Result};
