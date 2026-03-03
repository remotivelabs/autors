//! `A2LTRANSFORMER_OUT_OBJECTS`.

use crate::block::Block;
use crate::error::Result;
use crate::model::enums::{A2lKeyword, TriggerType};
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

const NO_INVERSE_TRANSFORMER: &str = "NO_INVERSE_TRANSFORMER";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transformer {
    pub name: String,
    pub version: String,
    pub executable32: String,
    pub executable64: String,
    pub timeout: u32,
    pub trigger: TriggerType,
    pub inverse_transformer: Option<String>,
}

impl Node for Transformer {
    const KEYWORD: &'static str = "TRANSFORMER";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let name = cur.ident()?;
        let version = cur.string()?;
        let executable32 = cur.string()?;
        let executable64 = cur.string()?;
        let timeout = cur.uint::<u32>()?;
        let tok = cur.next_token()?;
        let trigger = TriggerType::from_keyword(&tok.text).unwrap_or(TriggerType::ON_CHANGE);
        let inverse_transformer = Some(cur.ident()?);
        Ok(Transformer {
            name,
            version,
            executable32,
            executable64,
            timeout,
            trigger,
            inverse_transformer,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.version), true);
        w.tag_value(None, Some(&self.executable32), true);
        w.tag_value(None, Some(&self.executable64), true);
        w.value_line(None, &self.timeout.to_string());
        if let Some(kw) = self.trigger.as_keyword() {
            w.value_line(None, kw);
        }
        let inverse = self
            .inverse_transformer
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(NO_INVERSE_TRANSFORMER);
        w.value_line(None, inverse);
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

macro_rules! transformer_objects {
    ($(#[$meta:meta])* $name:ident, $kw:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name {
            pub references: Vec<String>,
        }

        impl Node for $name {
            const KEYWORD: &'static str = $kw;

            fn parse(block: &Block) -> Result<Self> {
                let mut cur = ParamCursor::new(block);
                let mut references = Vec::new();
                while !cur.is_empty() {
                    references.push(cur.ident()?);
                }
                Ok(Self { references })
            }

            fn write_body(&self, w: &mut Writer) -> Result<()> {
                w.references(&self.references);
                Ok(())
            }
        }
    };
}

transformer_objects!(TransformerInObjects, "TRANSFORMER_IN_OBJECTS");

transformer_objects!(TransformerOutObjects, "TRANSFORMER_OUT_OBJECTS");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn roundtrip<T: Node>(src: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child(T::KEYWORD).unwrap();
        let node = T::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn transformer_full() {
        let out = roundtrip::<Transformer>(
            "/begin TRANSFORMER Tr1 \"1.0\" \"tr32.dll\" \"tr64.dll\" 100 ON_CHANGE Tr2 /end TRANSFORMER",
        );
        assert_eq!(
            out,
            "/begin TRANSFORMER Tr1\n  \"1.0\"\n  \"tr32.dll\"\n  \"tr64.dll\"\n  100\n  ON_CHANGE\n  Tr2\n/end TRANSFORMER\n"
        );
    }

    #[test]
    fn transformer_default_inverse() {
        let toks = tokenize(
            "/begin TRANSFORMER Tr1 \"1.0\" \"a.dll\" \"b.dll\" 0 ON_USER_REQUEST NO_INVERSE_TRANSFORMER /end TRANSFORMER",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        let mut t = Transformer::parse(root.child("TRANSFORMER").unwrap()).unwrap();
        t.inverse_transformer = None;
        let mut w = Writer::new(WriterOptions::default());
        t.write_block(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin TRANSFORMER Tr1\n  \"1.0\"\n  \"a.dll\"\n  \"b.dll\"\n  0\n  ON_USER_REQUEST\n  NO_INVERSE_TRANSFORMER\n/end TRANSFORMER\n"
        );
    }

    #[test]
    fn transformer_in_objects_folds_by_columns() {
        let out = roundtrip::<TransformerInObjects>(
            "/begin TRANSFORMER_IN_OBJECTS M1 M2 M3 M4 /end TRANSFORMER_IN_OBJECTS",
        );
        assert_eq!(
            out,
            "/begin TRANSFORMER_IN_OBJECTS\n  M1 M2 M3\n  M4\n/end TRANSFORMER_IN_OBJECTS\n"
        );
    }

    #[test]
    fn transformer_out_objects_empty() {
        let out = roundtrip::<TransformerOutObjects>(
            "/begin TRANSFORMER_OUT_OBJECTS /end TRANSFORMER_OUT_OBJECTS",
        );
        assert_eq!(
            out,
            "/begin TRANSFORMER_OUT_OBJECTS\n/end TRANSFORMER_OUT_OBJECTS\n"
        );
    }
}
