//! The optional label and origin fields of an A2L `ANNOTATION` block.

use crate::block::Block;
use crate::error::Result;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Annotation {
    /// `ANNOTATION_LABEL "..."`.
    pub label: Option<String>,
    /// `ANNOTATION_ORIGIN "..."`.
    pub origin: Option<String>,
}

impl Node for Annotation {
    const KEYWORD: &'static str = "ANNOTATION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut ann = Annotation::default();
        while !cur.is_empty() {
            if cur.take_if("ANNOTATION_LABEL") {
                ann.label = Some(cur.string()?);
            } else if cur.take_if("ANNOTATION_ORIGIN") {
                ann.origin = Some(cur.string()?);
            } else {
                let t = cur.next_token()?;
                return Err(crate::error::Error::parse(
                    t.line,
                    format!("ANNOTATION: unexpected parameter {:?}", t.text),
                ));
            }
        }
        Ok(ann)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(origin) = &self.origin {
            w.tag_value(Some("ANNOTATION_ORIGIN"), Some(origin), true);
        }
        if let Some(label) = &self.label {
            w.tag_value(Some("ANNOTATION_LABEL"), Some(label), true);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotationText {
    pub text: String,
}

impl Node for AnnotationText {
    const KEYWORD: &'static str = "ANNOTATION_TEXT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut parts = Vec::new();
        while !cur.is_empty() {
            parts.push(cur.string()?);
        }
        Ok(AnnotationText {
            text: parts.join(" "),
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if !self.text.is_empty() {
            w.tag_value(None, Some(&self.text), true);
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
        let block = root.child("ANNOTATION").unwrap();
        let ann = Annotation::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        ann.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn annotation_roundtrip() {
        let out = roundtrip(
            "/begin ANNOTATION ANNOTATION_LABEL \"lbl\" ANNOTATION_ORIGIN \"org\" /end ANNOTATION",
        );
        assert_eq!(
            out,
            "/begin ANNOTATION\n  ANNOTATION_ORIGIN \"org\"\n  ANNOTATION_LABEL \"lbl\"\n/end ANNOTATION\n"
        );
    }

    #[test]
    fn annotation_escapes() {
        let out = roundtrip("/begin ANNOTATION ANNOTATION_LABEL \"a\\nb\" /end ANNOTATION");
        assert!(out.contains("\"a\\nb\""));
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let ann = Annotation::parse(root.child("ANNOTATION").unwrap()).unwrap();
        assert_eq!(ann.label.as_deref(), Some("a\nb"));
    }
}
