//! Lossless storage and writing of A2L blocks without a typed model.
//! Parameters and child blocks retain their token order for reliable round trips.

use crate::block::{Block, Item};
use crate::error::Result;
use crate::writer::{escape_str, Writer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedNode {
    pub keyword: String,
    pub items: Vec<Item>,
}

impl UnsupportedNode {
    pub fn from_block(block: &Block) -> Self {
        UnsupportedNode {
            keyword: block.keyword.clone(),
            items: block.items.clone(),
        }
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&self.keyword);
        write_items(&self.items, w);
        w.end_block(&self.keyword);
        Ok(())
    }
}

fn write_items(items: &[Item], w: &mut Writer) {
    let mut params = String::new();
    for item in items {
        match item {
            Item::Param(t) => {
                if !params.is_empty() {
                    params.push(' ');
                }
                if t.quoted {
                    params.push('"');
                    params.push_str(&escape_str(&t.text));
                    params.push('"');
                } else {
                    params.push_str(&t.text);
                }
            }
            Item::Child(b) => {
                if !params.is_empty() {
                    w.value_line(None, &params);
                    params.clear();
                }
                UnsupportedNode::from_block(b).write_block(w).ok();
            }
        }
    }
    if !params.is_empty() {
        w.value_line(None, &params);
    }
}
