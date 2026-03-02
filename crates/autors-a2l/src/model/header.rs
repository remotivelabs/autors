//! The project comment, version, and project number in an A2L `HEADER` block.

use crate::block::Block;
use crate::error::Result;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub comment: String,
    pub version: Option<String>,
    pub project_no: Option<String>,
}

impl Node for Header {
    const KEYWORD: &'static str = "HEADER";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut header = Header::default();
        if !cur.is_empty() {
            header.comment = cur.string()?;
        }
        while cur.remaining() >= 2 {
            if cur.take_if("VERSION") {
                header.version = Some(cur.string()?);
            } else if cur.take_if("PROJECT_NO") {
                header.project_no = Some(cur.string()?);
            } else {
                cur.next_token()?;
            }
        }
        Ok(header)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.comment), true);
        if let Some(version) = self.version.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("VERSION"), Some(version), true);
        }
        if let Some(project_no) = self.project_no.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("PROJECT_NO"), Some(project_no), false);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn roundtrip(src: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child("HEADER").unwrap();
        let header = Header::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        header.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn header_full() {
        let out = roundtrip(
            "/begin HEADER \"the comment\" VERSION \"1.0\" PROJECT_NO PRJ123 /end HEADER",
        );
        assert_eq!(
            out,
            "/begin HEADER\n  \"the comment\"\n  VERSION \"1.0\"\n  PROJECT_NO PRJ123\n/end HEADER\n"
        );
    }

    #[test]
    fn header_empty_comment_only() {
        let out = roundtrip("/begin HEADER /end HEADER");
        assert_eq!(out, "/begin HEADER\n  \"\"\n/end HEADER\n");
    }

    #[test]
    fn header_skips_unknown_and_empty_values() {
        let out = roundtrip("/begin HEADER \"c\" FOO BAR VERSION \"\" /end HEADER");
        assert_eq!(out, "/begin HEADER\n  \"c\"\n/end HEADER\n");
    }
}
