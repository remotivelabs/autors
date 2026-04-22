//! autors-symbols: symbol-to-A2L address update integration layer.
//! This crate provides the shared update types and helpers used when matching
//! symbols against A2L objects and writing updated addresses back: symbol path
//! parsing, `UpdaterSymbols` / `UpdateData` / `UpdateType`, and the A2L
//! write-back entry point [`update::update_and_write_a2l`].
//! Symbol file parsing lives in separate crates: ELF/DWARF in `autors-elf` and
//! MAP in `autors-map`. Both parser crates build on the shared types defined
//! here.

pub mod error;
pub mod update;

pub use error::{Error, Result};
