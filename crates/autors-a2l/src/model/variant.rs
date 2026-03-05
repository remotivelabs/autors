//! `A2LVAR_CHARACTERISTIC`, `A2LVAR_CRITERION`, `A2LVAR_FORBIDDEN_COMB`.

use indexmap::IndexMap;

use crate::block::Block;
use crate::error::{Error, Result};
use crate::model::enums::VarNamingType;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

fn var_naming_keyword(naming: VarNamingType) -> Option<&'static str> {
    match naming {
        VarNamingType::NUMERIC => Some("NUMERIC"),
        VarNamingType::NotSet => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariantCoding {
    pub var_separator: Option<String>,
    pub var_naming: VarNamingType,
    pub children: Vec<VariantCodingChild>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantCodingChild {
    /// `/begin VAR_CRITERION`.
    VarCriterion(VarCriterion),
    /// `/begin VAR_CHARACTERISTIC`.
    VarCharacteristic(VarCharacteristic),
    /// `/begin VAR_FORBIDDEN_COMB`.
    VarForbiddenComb(VarForbiddenComb),
    Unsupported(crate::model::unsupported::UnsupportedNode),
}

impl VariantCodingChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            VariantCodingChild::VarCriterion(n) => n.write_block(w),
            VariantCodingChild::VarCharacteristic(n) => n.write_block(w),
            VariantCodingChild::VarForbiddenComb(n) => n.write_block(w),
            VariantCodingChild::Unsupported(n) => n.write_block(w),
        }
    }
}

impl Node for VariantCoding {
    const KEYWORD: &'static str = "VARIANT_CODING";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut vc = VariantCoding::default();
        while !cur.is_empty() {
            if cur.take_if("VAR_SEPARATOR") {
                vc.var_separator = Some(cur.string()?);
            } else if cur.take_if("VAR_NAMING") {
                let tok = cur.next_token()?;
                vc.var_naming = match tok.text.to_ascii_uppercase().as_str() {
                    "NUMERIC" => VarNamingType::NUMERIC,
                    _ => VarNamingType::NotSet,
                };
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            vc.children.push(match child.keyword.as_str() {
                "VAR_CRITERION" => VariantCodingChild::VarCriterion(VarCriterion::parse(child)?),
                "VAR_CHARACTERISTIC" => {
                    VariantCodingChild::VarCharacteristic(VarCharacteristic::parse(child)?)
                }
                "VAR_FORBIDDEN_COMB" => {
                    VariantCodingChild::VarForbiddenComb(VarForbiddenComb::parse(child)?)
                }
                _ => VariantCodingChild::Unsupported(
                    crate::model::unsupported::UnsupportedNode::from_block(child),
                ),
            });
        }
        Ok(vc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(sep) = self.var_separator.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("VAR_SEPARATOR"), Some(sep), true);
        }
        if let Some(kw) = var_naming_keyword(self.var_naming) {
            w.value_line(Some("VAR_NAMING"), kw);
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarAddress {
    pub addresses: Vec<u32>,
}

impl Node for VarAddress {
    const KEYWORD: &'static str = "VAR_ADDRESS";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut va = VarAddress::default();
        while !cur.is_empty() {
            va.addresses.push(cur.uint::<u32>()?);
        }
        Ok(va)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        for addr in &self.addresses {
            w.value_line(None, &format!("0x{:X}", addr));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarCharacteristic {
    pub name: String,
    pub criterion_values: Vec<String>,
}

impl Node for VarCharacteristic {
    const KEYWORD: &'static str = "VAR_CHARACTERISTIC";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut vc = VarCharacteristic::default();
        if !cur.is_empty() {
            vc.name = cur.ident()?;
        }
        while !cur.is_empty() {
            vc.criterion_values.push(cur.ident()?);
        }
        Ok(vc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        for v in &self.criterion_values {
            w.value_line(None, v);
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarCriterion {
    pub name: String,
    pub description: String,
    pub var_measurement: Option<String>,
    pub var_selection_characteristic: Option<String>,
    pub criterion_values: Vec<String>,
    pub children: Vec<VarCriterionChild>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarCriterionChild {
    /// `/begin VAR_ADDRESS`.
    VarAddress(VarAddress),
    Unsupported(crate::model::unsupported::UnsupportedNode),
}

impl VarCriterionChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            VarCriterionChild::VarAddress(n) => n.write_block(w),
            VarCriterionChild::Unsupported(n) => n.write_block(w),
        }
    }
}

impl Node for VarCriterion {
    const KEYWORD: &'static str = "VAR_CRITERION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut vc = VarCriterion {
            name: cur.ident()?,
            description: cur.string()?,
            ..VarCriterion::default()
        };
        while !cur.is_empty() {
            if cur.take_if("VAR_MEASUREMENT") {
                vc.var_measurement = Some(cur.ident()?);
            } else if cur.take_if("VAR_SELECTION_CHARACTERISTIC") {
                vc.var_selection_characteristic = Some(cur.ident()?);
            } else {
                vc.criterion_values.push(cur.ident()?);
            }
        }
        for child in block.children() {
            vc.children.push(match child.keyword.as_str() {
                "VAR_ADDRESS" => VarCriterionChild::VarAddress(VarAddress::parse(child)?),
                _ => VarCriterionChild::Unsupported(
                    crate::model::unsupported::UnsupportedNode::from_block(child),
                ),
            });
        }
        Ok(vc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.description), true);
        if let Some(m) = self.var_measurement.as_deref().filter(|s| !s.is_empty()) {
            w.value_line(Some("VAR_MEASUREMENT"), m);
        }
        if let Some(c) = self
            .var_selection_characteristic
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            w.value_line(Some("VAR_SELECTION_CHARACTERISTIC"), c);
        }
        w.references(&self.criterion_values);
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarForbiddenComb {
    pub forbidden: IndexMap<String, String>,
}

impl Node for VarForbiddenComb {
    const KEYWORD: &'static str = "VAR_FORBIDDEN_COMB";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut vfc = VarForbiddenComb::default();
        while cur.remaining() >= 2 {
            let key = cur.ident()?;
            let value = cur.ident()?;
            vfc.forbidden.insert(key, value);
        }
        if !cur.is_empty() {
            let t = cur.next_token()?;
            return Err(Error::parse(
                t.line,
                format!("VAR_FORBIDDEN_COMB: dangling key {:?}", t.text),
            ));
        }
        Ok(vfc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        for (k, v) in &self.forbidden {
            w.value_line(Some(k), v);
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
    fn variant_coding_full() {
        let out = roundtrip::<VariantCoding>(
            "/begin VARIANT_CODING VAR_SEPARATOR \".\" VAR_NAMING NUMERIC /end VARIANT_CODING",
        );
        assert_eq!(
            out,
            "/begin VARIANT_CODING\n  VAR_SEPARATOR \".\"\n  VAR_NAMING NUMERIC\n/end VARIANT_CODING\n"
        );
    }

    #[test]
    fn variant_coding_defaults_write_nothing() {
        let out = roundtrip::<VariantCoding>("/begin VARIANT_CODING /end VARIANT_CODING");
        assert_eq!(out, "/begin VARIANT_CODING\n/end VARIANT_CODING\n");
    }

    #[test]
    fn var_address_hex_lines() {
        let out = roundtrip::<VarAddress>("/begin VAR_ADDRESS 0x1000 8192 /end VAR_ADDRESS");
        assert_eq!(
            out,
            "/begin VAR_ADDRESS\n  0x1000\n  0x2000\n/end VAR_ADDRESS\n"
        );
    }

    #[test]
    fn var_characteristic_value_per_line() {
        let out = roundtrip::<VarCharacteristic>(
            "/begin VAR_CHARACTERISTIC Char1 Val1 Val2 /end VAR_CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin VAR_CHARACTERISTIC Char1\n  Val1\n  Val2\n/end VAR_CHARACTERISTIC\n"
        );
    }

    #[test]
    fn var_criterion_full() {
        let out = roundtrip::<VarCriterion>(
            "/begin VAR_CRITERION Crit1 \"desc\" VAR_MEASUREMENT Meas1 VAR_SELECTION_CHARACTERISTIC Char1 V1 V2 V3 V4 /end VAR_CRITERION",
        );
        assert_eq!(
            out,
            "/begin VAR_CRITERION Crit1\n  \"desc\"\n  VAR_MEASUREMENT Meas1\n  VAR_SELECTION_CHARACTERISTIC Char1\n  V1 V2 V3\n  V4\n/end VAR_CRITERION\n"
        );
    }

    #[test]
    fn var_forbidden_comb_pairs() {
        let out = roundtrip::<VarForbiddenComb>(
            "/begin VAR_FORBIDDEN_COMB Crit1 Val1 Crit2 Val2 /end VAR_FORBIDDEN_COMB",
        );
        assert_eq!(
            out,
            "/begin VAR_FORBIDDEN_COMB\n  Crit1 Val1\n  Crit2 Val2\n/end VAR_FORBIDDEN_COMB\n"
        );
    }

    #[test]
    fn var_forbidden_comb_rejects_dangling_key() {
        let toks = tokenize("/begin VAR_FORBIDDEN_COMB OnlyKey /end VAR_FORBIDDEN_COMB").unwrap();
        let root = build_block_tree(&toks).unwrap();
        assert!(VarForbiddenComb::parse(root.child("VAR_FORBIDDEN_COMB").unwrap()).is_err());
    }
}
