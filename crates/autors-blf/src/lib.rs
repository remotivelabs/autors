//! Read, inspect, modify, and write Vector BLF (Binary Logging Format) files.
//!
//! The crate supports buffered and streaming I/O, compressed and uncompressed
//! log containers, version-1 and version-2 object headers, all registered BLF
//! object IDs, and lossless fallback for future object types. Frequently used
//! CAN, LIN, Ethernet, metadata, and variable-length events have typed models;
//! every other registered object remains available through
//! [`objects::BlfObject::Raw`].

pub mod error;
pub mod ethernet;
pub mod file;
pub mod general;
pub mod objects;
pub mod stream;

pub use error::{Error, Result};
pub use file::BlfFile;
pub use objects::{BlfObject, ObjectRepresentation, ObjectType};
pub use stream::{BlfReader, BlfReaderOptions, BlfWriter};
