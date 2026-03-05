//! Node abstraction shared by all A2L node types. Each node type implements its
//! own `parse` / `write_body`; there is no inheritance hierarchy.

use crate::block::Block;
use crate::error::Result;
use crate::writer::Writer;

/// An A2L node type (one implementor per A2L keyword).
pub trait Node: Sized {
    /// The A2L keyword of this node (e.g. `ANNOTATION`).
    const KEYWORD: &'static str;

    /// Parses a node from a block whose keyword has already been verified to
    /// match `Self::KEYWORD`.
    fn parse(block: &Block) -> Result<Self>;

    /// Writes the block contents, without the `/begin` `/end` wrapper (the
    /// wrapper is emitted by `Writer::block`).
    fn write_body(&self, w: &mut Writer) -> Result<()>;

    /// Writes the node as a full block, including `/begin` and `/end`.
    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}
