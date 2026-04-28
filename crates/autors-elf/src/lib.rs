//! autors-elf: parsing of ELF files and their DWARF debug information.
//! The `elf` module covers the ELF header, section headers and symbol tables;
//! the `dwarf` module (a submodule here because DWARF data lives inside ELF
//! files) covers compilation units, variable address resolution and the
//! symbol tree.
//! Shared types needed for symbol→A2L address updating (`UpdaterSymbols` /
//! `UpdateData` / symbol path resolution, etc.) live in the `autors-symbols`
//! crate, which this crate depends on.

pub mod dwarf;
pub mod elf;
pub mod error;

pub use error::{Error, Result};
