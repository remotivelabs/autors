//! autors-a2l: object model, parser, and writer for A2L (ASAM MCD-2 MC) files.
//! Entry points: [`Project::parse_str`] / [`Project::parse_file`] parse a document;
//! [`Project::write_string`] / [`Project::save`] write one out. The processing
//! pipeline runs from [`token`] through [`block`] to the per-node [`node::Node`]
//! implementations.

pub mod block;
pub mod error;
pub mod model;
pub mod node;
pub mod params;
pub mod token;
pub mod writer;

pub use error::{Error, Result};
pub use model::module::{Module, ModuleChild};
pub use model::project::{Project, ProjectChild};
pub use node::Node;
pub use writer::{escape_str, unescape_str, Writer, WriterOptions};
