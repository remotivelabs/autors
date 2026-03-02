//! `A2LCOMPU_VTAB` / `A2LCOMPU_VTAB_RANGE` / `A2LFORMULA` / `A2LBIT_OPERATION` /
//! `A2LFIX_AXIS_PAR_LIST` / `RationalCoeffs`.
//!   `DEFAULT_VALUE`/`DEFAULT_VALUE_NUMERIC`/`FORMULA_INV`/`SIGN_EXTEND`).

use std::collections::BTreeMap;

use indexmap::IndexMap;

use crate::block::Block;
use crate::error::{Error, Result};
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::{escape_str, Writer};

use super::enums::{A2lKeyword, BitOperationType, ConversionType};

fn to_dec(v: f64) -> String {
    format!("{v}")
}

fn to_dec_f32(v: f32) -> String {
    format!("{v}")
}

fn parse_conversion_type(cur: &mut ParamCursor) -> Result<ConversionType> {
    let t = cur.next_token()?;
    ConversionType::from_keyword(&t.text)
        .ok_or_else(|| Error::parse(t.line, format!("unknown conversion type {:?}", t.text)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct RationalCoeffs {
    pub coeffs: [f64; 6],
}

impl Default for RationalCoeffs {
    fn default() -> Self {
        RationalCoeffs {
            coeffs: [0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl RationalCoeffs {
    pub fn factor(&self) -> f64 {
        1.0 / self.coeffs[1]
    }

    pub fn offset(&self) -> f64 {
        -self.factor() * self.coeffs[2]
    }

    fn set_linear(&mut self, factor: f64, offset: f64) {
        if factor != 0.0 {
            self.coeffs[1] = 1.0 / factor;
            self.coeffs[2] = -offset / factor;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Formula {
    pub formula: String,
    pub formula_inv: Option<String>,
}

impl Node for Formula {
    const KEYWORD: &'static str = "FORMULA";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let formula = cur.next_token()?.text.clone();
        let mut formula_inv = None;
        if !cur.is_empty() {
            cur.next_token()?;
            formula_inv = Some(cur.next_token()?.text.clone());
        }
        Ok(Formula {
            formula,
            formula_inv,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if !self.formula.is_empty() {
            w.value_line(None, &format!("\"{}\"", self.formula));
        }
        if let Some(inv) = &self.formula_inv {
            if !inv.is_empty() {
                w.value_line(Some("FORMULA_INV"), &format!("\"{}\"", inv));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompuMethod {
    pub name: String,
    pub description: Option<String>,
    pub conversion_type: ConversionType,
    pub format: String,
    pub unit: String,
    pub coeffs: RationalCoeffs,
    pub compu_tab_ref: Option<String>,
    /// `STATUS_STRING_REF`.
    pub status_string_ref: Option<String>,
    pub ref_unit: Option<String>,
    pub inline_formula: Option<Formula>,
}

impl Default for CompuMethod {
    fn default() -> Self {
        CompuMethod {
            name: String::new(),
            description: None,
            conversion_type: ConversionType::IDENTICAL,
            format: String::new(),
            unit: String::new(),
            coeffs: RationalCoeffs::default(),
            compu_tab_ref: None,
            status_string_ref: None,
            ref_unit: None,
            inline_formula: None,
        }
    }
}

impl Node for CompuMethod {
    const KEYWORD: &'static str = "COMPU_METHOD";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut cm = CompuMethod {
            name: cur.ident()?,
            description: Some(cur.string()?),
            conversion_type: parse_conversion_type(&mut cur)?,
            format: cur.ident()?,
            unit: cur.ident()?,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("COEFFS") {
                for c in &mut cm.coeffs.coeffs {
                    *c = cur.float()?;
                }
            } else if cur.take_if("COEFFS_LINEAR") {
                let factor = cur.float()?;
                let offset = cur.float()?;
                cm.coeffs.set_linear(factor, offset);
            } else if cur.take_if("COMPU_TAB_REF") {
                cm.compu_tab_ref = Some(cur.ident()?);
            } else if cur.take_if("STATUS_STRING_REF") {
                cm.status_string_ref = Some(cur.ident()?);
            } else if cur.take_if("REF_UNIT") {
                cm.ref_unit = Some(cur.ident()?);
            } else if cur.take_if("FORMULA") {
                cm.inline_formula = Some(Formula {
                    formula: cur.next_token()?.text.clone(),
                    formula_inv: None,
                });
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("FORMULA") && cm.inline_formula.is_none() {
                cm.inline_formula = Some(Formula::parse(child)?);
            }
        }
        Ok(cm)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        if let Some(kw) = self.conversion_type.as_keyword() {
            w.value_line(None, kw);
        }
        w.value_line(None, &format!("\"{}\" \"{}\"", self.format, self.unit));
        match self.conversion_type {
            ConversionType::LINEAR => {
                w.value_line(
                    Some("COEFFS_LINEAR"),
                    &format!(
                        "{} {}",
                        to_dec(self.coeffs.factor()),
                        to_dec(self.coeffs.offset())
                    ),
                );
            }
            ConversionType::RAT_FUNC => {
                let c = &self.coeffs.coeffs;
                w.value_line(
                    Some("COEFFS"),
                    &format!(
                        "{} {} {} {} {} {}",
                        to_dec(c[0]),
                        to_dec(c[1]),
                        to_dec(c[2]),
                        to_dec(c[3]),
                        to_dec(c[4]),
                        to_dec(c[5])
                    ),
                );
            }
            ConversionType::TAB_INTP | ConversionType::TAB_NOINTP | ConversionType::TAB_VERB => {
                if let Some(r) = &self.compu_tab_ref {
                    w.value_line(Some("COMPU_TAB_REF"), r);
                }
            }
            _ => {}
        }
        if let Some(s) = &self.status_string_ref {
            if !s.is_empty() {
                w.value_line(Some("STATUS_STRING_REF"), s);
            }
        }
        if let Some(u) = &self.ref_unit {
            if !u.is_empty() {
                w.value_line(Some("REF_UNIT"), u);
            }
        }
        if let Some(f) = &self.inline_formula {
            f.write_block(w)?;
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

#[derive(Debug, Clone, PartialEq)]
pub struct CompuTabBase {
    pub name: String,
    pub description: Option<String>,
    pub conversion_type: ConversionType,
    pub default_value: Option<String>,
}

impl Default for CompuTabBase {
    fn default() -> Self {
        CompuTabBase {
            name: String::new(),
            description: None,
            conversion_type: ConversionType::IDENTICAL,
            default_value: None,
        }
    }
}

impl CompuTabBase {
    fn parse_header(cur: &mut ParamCursor) -> Result<Self> {
        Ok(CompuTabBase {
            name: cur.ident()?,
            description: Some(cur.string()?),
            conversion_type: parse_conversion_type(cur)?,
            default_value: None,
        })
    }

    fn write_base(&self, w: &mut Writer, with_conversion_type: bool) {
        w.tag_value(None, self.description.as_deref(), true);
        if with_conversion_type {
            if let Some(kw) = self.conversion_type.as_keyword() {
                w.value_line(None, kw);
            }
        }
    }

    fn write_default_value(&self, w: &mut Writer) {
        if let Some(dv) = &self.default_value {
            if !dv.is_empty() {
                w.tag_value(Some("DEFAULT_VALUE"), Some(dv), true);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompuTab {
    pub base: CompuTabBase,
    pub values: Vec<(f32, f64)>,
    pub default_value_numeric: Option<f64>,
}

impl Node for CompuTab {
    const KEYWORD: &'static str = "COMPU_TAB";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let base = CompuTabBase::parse_header(&mut cur)?;
        let _declared_count = cur.uint::<u64>()?;
        let mut tab = CompuTab {
            base,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("DEFAULT_VALUE_NUMERIC") {
                tab.default_value_numeric = Some(cur.float()?);
            } else if cur.take_if("DEFAULT_VALUE") {
                tab.base.default_value = Some(cur.next_token()?.text.clone());
            } else {
                let t = cur.next_token()?;
                let key = t.text.parse::<f64>().map_err(|_| {
                    Error::parse(
                        t.line,
                        format!("COMPU_TAB: expected number, got {:?}", t.text),
                    )
                })? as f32;
                let value = cur.float()?;
                if !tab.values.iter().any(|(k, _)| *k == key) {
                    tab.values.push((key, value));
                }
            }
        }
        tab.values
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(tab)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.base.write_base(w, true);
        w.value_line(None, &self.values.len().to_string());
        for (k, v) in &self.values {
            w.value_line(Some(&to_dec_f32(*k)), &to_dec(*v));
        }
        if let Some(dvn) = self.default_value_numeric {
            w.value_line(Some("DEFAULT_VALUE_NUMERIC"), &to_dec(dvn));
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.base.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompuVtab {
    pub base: CompuTabBase,
    pub verbs: BTreeMap<i64, String>,
}

impl Node for CompuVtab {
    const KEYWORD: &'static str = "COMPU_VTAB";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let base = CompuTabBase::parse_header(&mut cur)?;
        let _declared_count = cur.uint::<u64>()?;
        let mut vtab = CompuVtab {
            base,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("DEFAULT_VALUE") {
                vtab.base.default_value = Some(cur.string()?);
            } else {
                let t = cur.next_token()?;
                let key = t
                    .text
                    .parse::<f64>()
                    .map_err(|_| {
                        Error::parse(
                            t.line,
                            format!("COMPU_VTAB: expected number, got {:?}", t.text),
                        )
                    })?
                    .round() as i64;
                let value = cur.string()?;
                vtab.verbs.entry(key).or_insert(value);
            }
        }
        Ok(vtab)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.base.write_base(w, true);
        w.value_line(None, &self.verbs.len().to_string());
        for (k, v) in &self.verbs {
            w.tag_value(Some(&k.to_string()), Some(v), true);
        }
        self.base.write_default_value(w);
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.base.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompuVtabRange {
    pub base: CompuTabBase,
    pub verbs: IndexMap<String, Vec<(f64, f64)>>,
}

impl Node for CompuVtabRange {
    const KEYWORD: &'static str = "COMPU_VTAB_RANGE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let base = CompuTabBase {
            name: cur.ident()?,
            description: Some(cur.string()?),
            ..Default::default()
        };
        let _declared_count = cur.uint::<u64>()?;
        let mut vtr = CompuVtabRange {
            base,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("DEFAULT_VALUE") {
                vtr.base.default_value = Some(cur.string()?);
            } else {
                let t = cur.next_token()?;
                let min = t.text.parse::<f64>().map_err(|_| {
                    Error::parse(
                        t.line,
                        format!("COMPU_VTAB_RANGE: expected number, got {:?}", t.text),
                    )
                })?;
                let max = cur.float()?;
                let text = cur.string()?;
                vtr.verbs.entry(text).or_default().push((min, max));
            }
        }
        Ok(vtr)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.base.write_base(w, false);
        let count: usize = self.verbs.values().map(Vec::len).sum();
        w.value_line(None, &count.to_string());
        for (text, ranges) in &self.verbs {
            for (min, max) in ranges {
                w.value_line(
                    None,
                    &format!("{} {} \"{}\"", to_dec(*min), to_dec(*max), escape_str(text)),
                );
            }
        }
        self.base.write_default_value(w);
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(&format!("{} {}", Self::KEYWORD, self.base.name));
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitOperation {
    pub bit_operation: BitOperationType,
    pub count: i32,
    /// `SIGN_EXTEND`.
    pub sign_extend: bool,
}

impl Node for BitOperation {
    const KEYWORD: &'static str = "BIT_OPERATION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut bo = BitOperation::default();
        while !cur.is_empty() {
            if cur.take_if("LEFT_SHIFT") {
                bo.bit_operation = BitOperationType::LEFT_SHIFT;
                bo.count = cur.int()?;
            } else if cur.take_if("RIGHT_SHIFT") {
                bo.bit_operation = BitOperationType::RIGHT_SHIFT;
                bo.count = cur.int()?;
            } else if cur.take_if("SIGN_EXTEND") {
                bo.sign_extend = true;
            } else {
                cur.next_token()?;
            }
        }
        Ok(bo)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(kw) = self.bit_operation.as_keyword() {
            w.value_line(Some(kw), &self.count.to_string());
        }
        if self.sign_extend {
            w.value_line(None, "SIGN_EXTEND");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FixAxisParList {
    pub values: Vec<f64>,
}

impl Node for FixAxisParList {
    const KEYWORD: &'static str = "FIX_AXIS_PAR_LIST";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut values = Vec::new();
        while !cur.is_empty() {
            values.push(cur.float()?);
        }
        Ok(FixAxisParList { values })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        for v in &self.values {
            w.value_line(None, &to_dec(*v));
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

    fn roundtrip<N: Node>(src: &str, keyword: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child(keyword).unwrap();
        let node = N::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn compu_method_linear_roundtrip() {
        let out = roundtrip::<CompuMethod>(
            "/begin COMPU_METHOD CM \"desc\" LINEAR \"%4.2\" \"rpm\" \
             COEFFS_LINEAR 0.1 5 STATUS_STRING_REF SSR REF_UNIT U1 /end COMPU_METHOD",
            "COMPU_METHOD",
        );
        assert_eq!(
            out,
            "/begin COMPU_METHOD CM\n  \"desc\"\n  LINEAR\n  \"%4.2\" \"rpm\"\n  \
             COEFFS_LINEAR 0.1 5\n  STATUS_STRING_REF SSR\n  REF_UNIT U1\n/end COMPU_METHOD\n"
        );
    }

    #[test]
    fn compu_method_rat_func_roundtrip() {
        let out = roundtrip::<CompuMethod>(
            "/begin COMPU_METHOD CM2 \"\" RAT_FUNC \"%1.0\" \"\" COEFFS 1 2 3 4 5 6 \
             /end COMPU_METHOD",
            "COMPU_METHOD",
        );
        assert_eq!(
            out,
            "/begin COMPU_METHOD CM2\n  \"\"\n  RAT_FUNC\n  \"%1.0\" \"\"\n  \
             COEFFS 1 2 3 4 5 6\n/end COMPU_METHOD\n"
        );
    }

    #[test]
    fn compu_method_tab_ref_and_inline_formula() {
        let out = roundtrip::<CompuMethod>(
            "/begin COMPU_METHOD CM3 \"d\" TAB_NOINTP \"%1.0\" \"-\" COMPU_TAB_REF T1 \
             FORMULA \"x*2\" /end COMPU_METHOD",
            "COMPU_METHOD",
        );
        assert_eq!(
            out,
            "/begin COMPU_METHOD CM3\n  \"d\"\n  TAB_NOINTP\n  \"%1.0\" \"-\"\n  \
             COMPU_TAB_REF T1\n  /begin FORMULA\n    \"x*2\"\n  /end FORMULA\n/end COMPU_METHOD\n"
        );
    }

    #[test]
    fn compu_method_identical_minimal() {
        let out = roundtrip::<CompuMethod>(
            "/begin COMPU_METHOD CM4 \"\" IDENTICAL \"%1.0\" \"\" /end COMPU_METHOD",
            "COMPU_METHOD",
        );
        assert_eq!(
            out,
            "/begin COMPU_METHOD CM4\n  \"\"\n  IDENTICAL\n  \"%1.0\" \"\"\n/end COMPU_METHOD\n"
        );
    }

    #[test]
    fn compu_method_linear_zero_factor_keeps_identity() {
        let toks = tokenize(
            "/begin COMPU_METHOD Z \"\" LINEAR \"%1.0\" \"\" COEFFS_LINEAR 0 7 /end COMPU_METHOD",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        let cm = CompuMethod::parse(root.child("COMPU_METHOD").unwrap()).unwrap();
        assert_eq!(cm.coeffs, RationalCoeffs::default());
    }

    #[test]
    fn compu_tab_roundtrip_sorted_by_key() {
        let out = roundtrip::<CompuTab>(
            "/begin COMPU_TAB T \"table\" TAB_NOINTP 3 2 20 0 0 1 10 \
             DEFAULT_VALUE_NUMERIC -1 /end COMPU_TAB",
            "COMPU_TAB",
        );
        assert_eq!(
            out,
            "/begin COMPU_TAB T\n  \"table\"\n  TAB_NOINTP\n  3\n  0 0\n  1 10\n  2 20\n  \
             DEFAULT_VALUE_NUMERIC -1\n/end COMPU_TAB\n"
        );
    }

    #[test]
    fn compu_tab_tab_intp_roundtrip() {
        let out = roundtrip::<CompuTab>(
            "/begin COMPU_TAB TI \"\" TAB_INTP 2 0 0 10 100.5 /end COMPU_TAB",
            "COMPU_TAB",
        );
        assert_eq!(
            out,
            "/begin COMPU_TAB TI\n  \"\"\n  TAB_INTP\n  2\n  0 0\n  10 100.5\n/end COMPU_TAB\n"
        );
    }

    #[test]
    fn compu_tab_duplicate_key_keeps_first() {
        let toks =
            tokenize("/begin COMPU_TAB D \"\" TAB_NOINTP 2 0 1 0 99 /end COMPU_TAB").unwrap();
        let root = build_block_tree(&toks).unwrap();
        let tab = CompuTab::parse(root.child("COMPU_TAB").unwrap()).unwrap();
        assert_eq!(tab.values, vec![(0.0f32, 1.0f64)]);
    }

    #[test]
    fn compu_vtab_roundtrip() {
        let out = roundtrip::<CompuVtab>(
            "/begin COMPU_VTAB VT \"verb\" TAB_VERB 2 1 \"on\" 0 \"off\" \
             DEFAULT_VALUE \"unknown\" /end COMPU_VTAB",
            "COMPU_VTAB",
        );
        assert_eq!(
            out,
            "/begin COMPU_VTAB VT\n  \"verb\"\n  TAB_VERB\n  2\n  0 \"off\"\n  1 \"on\"\n  \
             DEFAULT_VALUE \"unknown\"\n/end COMPU_VTAB\n"
        );
    }

    #[test]
    fn compu_vtab_escapes_values() {
        let out = roundtrip::<CompuVtab>(
            "/begin COMPU_VTAB VE \"\" TAB_VERB 1 0 \"a\\\"b\" /end COMPU_VTAB",
            "COMPU_VTAB",
        );
        assert_eq!(
            out,
            "/begin COMPU_VTAB VE\n  \"\"\n  TAB_VERB\n  1\n  0 \"a\\\"b\"\n/end COMPU_VTAB\n"
        );
    }

    #[test]
    fn compu_vtab_range_roundtrip() {
        let out = roundtrip::<CompuVtabRange>(
            "/begin COMPU_VTAB_RANGE VTR \"ranges\" 2 0 10 \"low\" 10.5 20 \"high\" \
             DEFAULT_VALUE \"?\" /end COMPU_VTAB_RANGE",
            "COMPU_VTAB_RANGE",
        );
        assert_eq!(
            out,
            "/begin COMPU_VTAB_RANGE VTR\n  \"ranges\"\n  2\n  0 10 \"low\"\n  \
             10.5 20 \"high\"\n  DEFAULT_VALUE \"?\"\n/end COMPU_VTAB_RANGE\n"
        );
    }

    #[test]
    fn compu_vtab_range_groups_by_key() {
        let out = roundtrip::<CompuVtabRange>(
            "/begin COMPU_VTAB_RANGE VR2 \"\" 3 0 1 \"a\" 2 3 \"b\" 4 5 \"a\" \
             /end COMPU_VTAB_RANGE",
            "COMPU_VTAB_RANGE",
        );
        assert_eq!(
            out,
            "/begin COMPU_VTAB_RANGE VR2\n  \"\"\n  3\n  0 1 \"a\"\n  4 5 \"a\"\n  \
             2 3 \"b\"\n/end COMPU_VTAB_RANGE\n"
        );
    }

    #[test]
    fn formula_roundtrip() {
        let out = roundtrip::<Formula>(
            "/begin FORMULA \"x*2\" FORMULA_INV \"x/2\" /end FORMULA",
            "FORMULA",
        );
        assert_eq!(
            out,
            "/begin FORMULA\n  \"x*2\"\n  FORMULA_INV \"x/2\"\n/end FORMULA\n"
        );
    }

    #[test]
    fn formula_without_inverse() {
        let out = roundtrip::<Formula>("/begin FORMULA \"sqrt(x)\" /end FORMULA", "FORMULA");
        assert_eq!(out, "/begin FORMULA\n  \"sqrt(x)\"\n/end FORMULA\n");
    }

    #[test]
    fn bit_operation_roundtrip() {
        let out = roundtrip::<BitOperation>(
            "/begin BIT_OPERATION LEFT_SHIFT 3 SIGN_EXTEND /end BIT_OPERATION",
            "BIT_OPERATION",
        );
        assert_eq!(
            out,
            "/begin BIT_OPERATION\n  LEFT_SHIFT 3\n  SIGN_EXTEND\n/end BIT_OPERATION\n"
        );
    }

    #[test]
    fn bit_operation_empty() {
        let out =
            roundtrip::<BitOperation>("/begin BIT_OPERATION /end BIT_OPERATION", "BIT_OPERATION");
        assert_eq!(out, "/begin BIT_OPERATION\n/end BIT_OPERATION\n");
    }

    #[test]
    fn fix_axis_par_list_roundtrip() {
        let out = roundtrip::<FixAxisParList>(
            "/begin FIX_AXIS_PAR_LIST 0 10 20.5 30 /end FIX_AXIS_PAR_LIST",
            "FIX_AXIS_PAR_LIST",
        );
        assert_eq!(
            out,
            "/begin FIX_AXIS_PAR_LIST\n  0\n  10\n  20.5\n  30\n/end FIX_AXIS_PAR_LIST\n"
        );
    }
}
