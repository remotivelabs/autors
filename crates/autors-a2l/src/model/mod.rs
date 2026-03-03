//! A2L object model: typed representations of the A2L node families
//! (measurements, characteristics, conversion methods, record layouts, and so on).

pub mod annotation;
pub mod base;
pub mod canape;
pub mod characteristic;
pub mod compu;
pub mod enums;
pub mod function;
pub mod header;
pub mod measurement;
pub mod module;
pub mod project;
pub mod record_layout;
pub mod transformer;
pub mod typedef;
pub mod unsupported;
pub mod variant;

pub use annotation::{Annotation, AnnotationText};
pub use unsupported::UnsupportedNode;
