//! FORMAT / PHYS_UNIT / BYTE_ORDER / REF_MEMORY_SEGMENT / ECU_ADDRESS_EXTENSION /
//! DISPLAY_IDENTIFIER / CALIBRATION_ACCESS / MAX_REFRESH / SYMBOL_LINK / MODEL_LINK),

use crate::block::Block;
use crate::error::{Error, Result};
use crate::model::annotation::Annotation;
use crate::model::base::{AddressFields, ConversionRefFields, NamedFields, RecordLayoutRefFields};
use crate::model::enums::CharacteristicType;
use crate::model::enums::{A2lKeyword, DepositType, EncodingType, MonotonyType};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::{A2lInt, ParamCursor};
use crate::writer::Writer;

fn to_hex<T: std::fmt::UpperHex>(v: T) -> String {
    format!("0x{v:X}")
}

fn to_dec(v: f64) -> String {
    format!("{v}")
}

fn enum_value<T: A2lKeyword>(cur: &mut ParamCursor, owner: &str) -> Result<T> {
    let t = cur.next_token()?;
    T::from_keyword(&t.text).ok_or_else(|| {
        Error::parse(
            t.line,
            format!("{owner}: unknown enum keyword {:?}", t.text),
        )
    })
}

fn write_named_block<T: Node>(node: &T, name: &str, w: &mut Writer) -> Result<()> {
    w.begin_block(&format!("{} {name}", T::KEYWORD));
    node.write_body(w)?;
    w.end_block(T::KEYWORD);
    Ok(())
}

fn encoding_as_keyword(e: EncodingType) -> &'static str {
    match e {
        EncodingType::ASCII => "ASCII",
        EncodingType::UTF8 => "UTF8",
        EncodingType::UTF16 => "UTF16",
        EncodingType::UTF32 => "UTF32",
    }
}

fn encoding_from_keyword(kw: &str) -> Option<EncodingType> {
    match kw.to_ascii_uppercase().as_str() {
        "ASCII" => Some(EncodingType::ASCII),
        "UTF8" => Some(EncodingType::UTF8),
        "UTF16" => Some(EncodingType::UTF16),
        "UTF32" => Some(EncodingType::UTF32),
        _ => None,
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum CharacteristicChild {
    /// `/begin ANNOTATION`.
    Annotation(Annotation),
    /// `/begin AXIS_DESCR`.
    AxisDescr(crate::model::measurement::AxisDescr),
    /// `/begin DEPENDENT_CHARACTERISTIC`.
    DependentCharacteristic(crate::model::function::DependentCharacteristic),
    /// `/begin VIRTUAL_CHARACTERISTIC`.
    VirtualCharacteristic(crate::model::function::VirtualCharacteristic),
    Unsupported(UnsupportedNode),
}

impl CharacteristicChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            CharacteristicChild::Annotation(n) => n.write_block(w),
            CharacteristicChild::AxisDescr(n) => n.write_block(w),
            CharacteristicChild::DependentCharacteristic(n) => n.write_block(w),
            CharacteristicChild::VirtualCharacteristic(n) => n.write_block(w),
            CharacteristicChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Characteristic {
    pub named: NamedFields,
    pub char_type: CharacteristicType,
    pub addr: AddressFields,
    pub conv: ConversionRefFields,
    pub rec: RecordLayoutRefFields,
    pub bitmask: Option<u64>,
    pub number: i32,
    pub matrix_dim: Option<Vec<i32>>,
    pub comparison_quantity: Option<String>,
    pub discrete: bool,
    pub encoding: EncodingType,
    pub children: Vec<CharacteristicChild>,
}

impl Default for Characteristic {
    fn default() -> Self {
        Characteristic {
            named: NamedFields::default(),
            char_type: CharacteristicType::default(),
            addr: AddressFields::default(),
            conv: ConversionRefFields::default(),
            rec: RecordLayoutRefFields::default(),
            bitmask: None,
            number: 0,
            matrix_dim: None,
            comparison_quantity: None,
            discrete: false,
            encoding: EncodingType::ASCII,
            children: Vec::new(),
        }
    }
}

impl Node for Characteristic {
    const KEYWORD: &'static str = "CHARACTERISTIC";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let char_type = enum_value(&mut cur, "CHARACTERISTIC")?;
        let mut ch = Characteristic {
            named,
            char_type,
            ..Characteristic::default()
        };
        ch.addr.address = Some(cur.uint::<u32>()?);
        ch.rec.record_layout = cur.ident()?;
        ch.rec.max_diff = cur.float()?;
        ch.conv.conversion = cur.ident()?;
        ch.conv.lower_limit = cur.float()?;
        ch.conv.upper_limit = cur.float()?;
        while !cur.is_empty() {
            if ch.addr.take(&mut cur)? || ch.conv.take(&mut cur)? || ch.rec.take(&mut cur)? {
                continue;
            }
            if cur.take_if("BIT_MASK") {
                ch.bitmask = Some(cur.uint::<u64>()?);
            } else if cur.take_if("NUMBER") {
                ch.number = cur.int::<i32>()?;
            } else if cur.take_if("MATRIX_DIM") {
                let mut dims = Vec::new();
                while dims.len() < 5 {
                    match cur.peek() {
                        Some(t) if <i32 as A2lInt>::parse_a2l(&t.text).is_some() => {
                            dims.push(cur.int::<i32>()?);
                        }
                        _ => break,
                    }
                }
                ch.matrix_dim = Some(dims);
            } else if cur.take_if("COMPARISON_QUANTITY") {
                ch.comparison_quantity = Some(cur.ident()?);
            } else if cur.take_if("DISCRETE") {
                ch.discrete = true;
            } else if cur.take_if("ENCODING") {
                let t = cur.next_token()?;
                ch.encoding = encoding_from_keyword(&t.text).ok_or_else(|| {
                    Error::parse(
                        t.line,
                        format!("CHARACTERISTIC: unknown enum keyword {:?}", t.text),
                    )
                })?;
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            ch.children
                .push(if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                    CharacteristicChild::Annotation(Annotation::parse(child)?)
                } else if child.keyword.eq_ignore_ascii_case("AXIS_DESCR") {
                    CharacteristicChild::AxisDescr(crate::model::measurement::AxisDescr::parse(
                        child,
                    )?)
                } else if child
                    .keyword
                    .eq_ignore_ascii_case("DEPENDENT_CHARACTERISTIC")
                {
                    CharacteristicChild::DependentCharacteristic(
                        crate::model::function::DependentCharacteristic::parse(child)?,
                    )
                } else if child.keyword.eq_ignore_ascii_case("VIRTUAL_CHARACTERISTIC") {
                    CharacteristicChild::VirtualCharacteristic(
                        crate::model::function::VirtualCharacteristic::parse(child)?,
                    )
                } else {
                    CharacteristicChild::Unsupported(UnsupportedNode::from_block(child))
                });
        }
        Ok(ch)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?; // "<LONG_IDENT>"
        if let Some(kw) = self.char_type.as_keyword() {
            w.value_line(None, kw);
        }
        w.value_line(None, &to_hex(self.addr.address.unwrap_or(u32::MAX)));
        w.value_line(None, &self.rec.record_layout);
        w.value_line(None, &to_dec(self.rec.max_diff));
        w.value_line(None, &self.conv.conversion);
        self.conv.write_limits(w)?;
        self.addr.write(w)?;
        self.conv.write(w)?;
        self.rec.write(w)?;
        if let Some(bm) = self.bitmask {
            w.value_line(Some("BIT_MASK"), &to_hex(bm));
        }
        if self.number != 0 {
            w.value_line(Some("NUMBER"), &self.number.to_string());
        }
        if let Some(dims) = &self.matrix_dim {
            let s = dims
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            w.value_line(Some("MATRIX_DIM"), &s);
        }
        if let Some(cq) = self
            .comparison_quantity
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            w.value_line(None, cq);
        }
        if self.discrete {
            w.value_line(None, "DISCRETE");
        }
        if self.encoding != EncodingType::ASCII {
            w.value_line(Some("ENCODING"), encoding_as_keyword(self.encoding));
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisPtsChild {
    /// `/begin ANNOTATION`.
    Annotation(Annotation),
    Unsupported(UnsupportedNode),
}

impl AxisPtsChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            AxisPtsChild::Annotation(n) => n.write_block(w),
            AxisPtsChild::Unsupported(n) => n.write_block(w),
        }
    }
}

/// RecordLayout MaxDiff Conversion MaxAxisPoints LowerLimit UpperLimit`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxisPts {
    pub named: NamedFields,
    pub addr: AddressFields,
    pub conv: ConversionRefFields,
    pub rec: RecordLayoutRefFields,
    pub input_quantity: Option<String>,
    pub max_axis_points: i32,
    pub monotony: MonotonyType,
    pub deposit: DepositType,
    pub children: Vec<AxisPtsChild>,
}

impl Node for AxisPts {
    const KEYWORD: &'static str = "AXIS_PTS";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let mut ap = AxisPts {
            named,
            ..AxisPts::default()
        };
        ap.addr.address = Some(cur.uint::<u32>()?);
        ap.input_quantity = Some(cur.ident()?);
        ap.rec.record_layout = cur.ident()?;
        ap.rec.max_diff = cur.float()?;
        ap.conv.conversion = cur.ident()?;
        ap.max_axis_points = cur.int::<i32>()?;
        ap.conv.lower_limit = cur.float()?;
        ap.conv.upper_limit = cur.float()?;
        while !cur.is_empty() {
            if ap.addr.take(&mut cur)? || ap.conv.take(&mut cur)? || ap.rec.take(&mut cur)? {
                continue;
            }
            if cur.take_if("MONOTONY") {
                ap.monotony = enum_value(&mut cur, "AXIS_PTS")?;
            } else if cur.take_if("DEPOSIT") {
                ap.deposit = enum_value(&mut cur, "AXIS_PTS")?;
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            ap.children
                .push(if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                    AxisPtsChild::Annotation(Annotation::parse(child)?)
                } else {
                    AxisPtsChild::Unsupported(UnsupportedNode::from_block(child))
                });
        }
        Ok(ap)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?; // "<LONG_IDENT>"
        w.value_line(None, &to_hex(self.addr.address.unwrap_or(u32::MAX)));
        if let Some(iq) = &self.input_quantity {
            w.value_line(None, iq);
        }
        w.value_line(None, &self.rec.record_layout);
        w.value_line(None, &to_dec(self.rec.max_diff));
        w.value_line(None, &self.conv.conversion);
        w.value_line(None, &self.max_axis_points.to_string());
        self.conv.write_limits(w)?;
        self.addr.write(w)?;
        self.conv.write(w)?;
        self.rec.write(w)?;
        if let Some(kw) = self.monotony.as_keyword() {
            w.value_line(Some("MONOTONY"), kw);
        }
        if let Some(kw) = self.deposit.as_keyword() {
            w.value_line(Some("DEPOSIT"), kw);
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_block_tree, Item};
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn parse_and_write<T: Node>(src: &str, keyword: &str) -> (T, String) {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child(keyword).unwrap();
        let node = T::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        (node, w.into_string())
    }

    fn reparse<T: Node>(out: &str, keyword: &str) -> T {
        let toks = tokenize(out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        T::parse(root.child(keyword).unwrap()).unwrap()
    }

    fn assert_reparse_eq<T: Node + PartialEq + std::fmt::Debug>(
        first: &T,
        out: &str,
        keyword: &str,
    ) {
        assert_eq!(*first, reparse::<T>(out, keyword));
    }

    fn unsupported_sig(u: &UnsupportedNode) -> String {
        let mut s = u.keyword.clone();
        for it in &u.items {
            match it {
                Item::Param(t) => {
                    s.push(' ');
                    s.push_str(&t.text);
                }
                Item::Child(b) => {
                    s.push(' ');
                    s.push_str(&b.keyword);
                }
            }
        }
        s
    }

    #[test]
    fn characteristic_full_roundtrip() {
        let src = "/begin CHARACTERISTIC KFRLMN \"Friction map\" MAP 0x803DAC RL_MAP 0 CONV_KF 0 100 \
                   GUARD_RAILS STEP_SIZE 0.5 EXTENDED_LIMITS -10 200 READ_ONLY \
                   REF_MEMORY_SEGMENT Seg1 BYTE_ORDER MSB_FIRST PHYS_UNIT \"Nm\" FORMAT \"%5.2\" \
                   MODEL_LINK \"mdl\" SYMBOL_LINK \"sym\" -4 MAX_REFRESH 3 10 \
                   CALIBRATION_ACCESS CALIBRATION DISPLAY_IDENTIFIER disp ECU_ADDRESS_EXTENSION 2 \
                   ENCODING UTF8 DISCRETE MATRIX_DIM 2 3 NUMBER 4 BIT_MASK 0xFF \
                   /begin ANNOTATION ANNOTATION_LABEL \"lbl\" ANNOTATION_ORIGIN \"org\" /end ANNOTATION \
                   /begin AXIS_DESCR STD_AXIS Input1 CONV_AX 0 10 0 100 /end AXIS_DESCR \
                   /begin MAP_LIST M1 M2 /end MAP_LIST \
                   /end CHARACTERISTIC";
        let (ch, out) = parse_and_write::<Characteristic>(src, "CHARACTERISTIC");
        assert_eq!(
            out,
            "/begin CHARACTERISTIC KFRLMN\n  \"Friction map\"\n  MAP\n  0x803DAC\n  RL_MAP\n  \
             0\n  CONV_KF\n  0 100\n  ECU_ADDRESS_EXTENSION 2\n  DISPLAY_IDENTIFIER disp\n  \
             CALIBRATION_ACCESS CALIBRATION\n  MAX_REFRESH 3 10\n  SYMBOL_LINK \"sym\" -4\n  \
             MODEL_LINK \"mdl\"\n  FORMAT \"%5.2\"\n  PHYS_UNIT \"Nm\"\n  BYTE_ORDER MSB_FIRST\n  \
             REF_MEMORY_SEGMENT Seg1\n  READ_ONLY\n  EXTENDED_LIMITS -10 200\n  STEP_SIZE 0.5\n  \
             GUARD_RAILS\n  BIT_MASK 0xFF\n  NUMBER 4\n  MATRIX_DIM 2 3\n  DISCRETE\n  \
             ENCODING UTF8\n  \
             /begin ANNOTATION\n    ANNOTATION_ORIGIN \"org\"\n    ANNOTATION_LABEL \"lbl\"\n  \
             /end ANNOTATION\n  \
             /begin AXIS_DESCR\n    STD_AXIS\n    Input1\n    CONV_AX\n    0\n    10 0\n  /end AXIS_DESCR\n  \
             /begin MAP_LIST\n    M1 M2\n  /end MAP_LIST\n\
             /end CHARACTERISTIC\n"
        );
        assert_eq!(ch.named.name, "KFRLMN");
        assert_eq!(ch.char_type, CharacteristicType::MAP);
        assert_eq!(ch.addr.address, Some(0x803DAC));
        assert_eq!(ch.rec.record_layout, "RL_MAP");
        assert_eq!(ch.conv.conversion, "CONV_KF");
        assert_eq!(ch.bitmask, Some(0xFF));
        assert_eq!(ch.number, 4);
        assert_eq!(ch.matrix_dim, Some(vec![2, 3]));
        assert!(ch.discrete);
        assert_eq!(ch.encoding, EncodingType::UTF8);
        assert_eq!(ch.children.len(), 3);
        let again = reparse::<Characteristic>(&out, "CHARACTERISTIC");
        let mut ch_flat = ch.clone();
        ch_flat.children.clear();
        let mut again_flat = again.clone();
        again_flat.children.clear();
        assert_eq!(ch_flat, again_flat);
        assert_eq!(again.children.len(), ch.children.len());
        for (a, b) in ch.children.iter().zip(&again.children) {
            match (a, b) {
                (CharacteristicChild::Annotation(x), CharacteristicChild::Annotation(y)) => {
                    assert_eq!(x, y)
                }
                (CharacteristicChild::AxisDescr(x), CharacteristicChild::AxisDescr(y)) => {
                    assert_eq!(x, y)
                }
                (CharacteristicChild::Unsupported(x), CharacteristicChild::Unsupported(y)) => {
                    assert_eq!(unsupported_sig(x), unsupported_sig(y))
                }
                _ => panic!("child kind mismatch after reparse"),
            }
        }
    }

    #[test]
    fn characteristic_minimal() {
        let (ch, out) = parse_and_write::<Characteristic>(
            "/begin CHARACTERISTIC C1 \"\" VALUE 0x1000 RL 0 CV -1.5 250.5 /end CHARACTERISTIC",
            "CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin CHARACTERISTIC C1\n  \"\"\n  VALUE\n  0x1000\n  RL\n  0\n  CV\n  \
             -1.5 250.5\n/end CHARACTERISTIC\n"
        );
        assert_eq!(ch.bitmask, None);
        assert_eq!(ch.matrix_dim, None);
        assert_eq!(ch.comparison_quantity, None);
        assert!(!ch.discrete);
        assert_reparse_eq(&ch, &out, "CHARACTERISTIC");
    }

    #[test]
    fn characteristic_matrix_dim_capped_at_five() {
        let (ch, out) = parse_and_write::<Characteristic>(
            "/begin CHARACTERISTIC C \"d\" VAL_BLK 0x10 RL 0 CV 0 1 MATRIX_DIM 1 2 3 4 5 6 \
             /end CHARACTERISTIC",
            "CHARACTERISTIC",
        );
        assert_eq!(ch.matrix_dim, Some(vec![1, 2, 3, 4, 5]));
        assert!(out.contains("MATRIX_DIM 1 2 3 4 5\n"));
        assert!(!out.contains('6'));
    }

    #[test]
    fn characteristic_encoding_ascii_not_written() {
        let (ch, out) = parse_and_write::<Characteristic>(
            "/begin CHARACTERISTIC C \"d\" ASCII 0x10 RL 0 CV 0 1 NUMBER 8 ENCODING ASCII \
             /end CHARACTERISTIC",
            "CHARACTERISTIC",
        );
        assert_eq!(ch.encoding, EncodingType::ASCII);
        assert!(out.contains("NUMBER 8\n"));
        assert!(!out.contains("ENCODING"));
    }

    #[test]
    fn characteristic_comparison_quantity_written_bare() {
        let (ch, out) = parse_and_write::<Characteristic>(
            "/begin CHARACTERISTIC C \"d\" VALUE 0x10 RL 0 CV 0 1 COMPARISON_QUANTITY Meas1 \
             /end CHARACTERISTIC",
            "CHARACTERISTIC",
        );
        assert_eq!(ch.comparison_quantity.as_deref(), Some("Meas1"));
        assert!(out.contains("\n  Meas1\n"));
        assert!(!out.contains("COMPARISON_QUANTITY"));
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let again = Characteristic::parse(root.child("CHARACTERISTIC").unwrap()).unwrap();
        assert_eq!(again.comparison_quantity, None);
    }

    #[test]
    fn characteristic_skips_unknown_params() {
        let (ch, out) = parse_and_write::<Characteristic>(
            "/begin CHARACTERISTIC C \"d\" CURVE 0x10 RL 0 CV 0 1 FOO BAR READ_ONLY \
             /end CHARACTERISTIC",
            "CHARACTERISTIC",
        );
        assert!(ch.rec.read_only);
        assert!(!out.contains("FOO"));
        assert!(!out.contains("BAR"));
    }

    #[test]
    fn characteristic_rejects_unknown_char_type() {
        let toks =
            tokenize("/begin CHARACTERISTIC C \"d\" BOGUS 0x10 RL 0 CV 0 1 /end CHARACTERISTIC")
                .unwrap();
        let root = build_block_tree(&toks).unwrap();
        assert!(Characteristic::parse(root.child("CHARACTERISTIC").unwrap()).is_err());
    }

    #[test]
    fn characteristic_rejects_unknown_encoding() {
        let toks = tokenize(
            "/begin CHARACTERISTIC C \"d\" ASCII 0x10 RL 0 CV 0 1 ENCODING LATIN1 \
             /end CHARACTERISTIC",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        assert!(Characteristic::parse(root.child("CHARACTERISTIC").unwrap()).is_err());
    }

    #[test]
    fn char_type_keywords() {
        for (kw, ct) in [
            ("VALUE", CharacteristicType::VALUE),
            ("ASCII", CharacteristicType::ASCII),
            ("VAL_BLK", CharacteristicType::VAL_BLK),
            ("CURVE", CharacteristicType::CURVE),
            ("MAP", CharacteristicType::MAP),
            ("CUBOID", CharacteristicType::CUBOID),
            ("CUBE_4", CharacteristicType::CUBE_4),
            ("CUBE_5", CharacteristicType::CUBE_5),
        ] {
            assert_eq!(CharacteristicType::from_keyword(kw), Some(ct));
            assert_eq!(ct.as_keyword(), Some(kw));
        }
        assert_eq!(
            CharacteristicType::from_keyword("val_blk"),
            Some(CharacteristicType::VAL_BLK)
        );
        assert_eq!(CharacteristicType::from_keyword("CUBE_6"), None);
        assert_eq!(CharacteristicType::NotSet.as_keyword(), None);
    }

    #[test]
    fn axis_pts_full_roundtrip() {
        let src = "/begin AXIS_PTS AX1 \"axis pts\" 0x810000 Meas_X RL_AX 0 CONV_AX 16 0 255 \
                   GUARD_RAILS STEP_SIZE 1 READ_ONLY BYTE_ORDER MSB_LAST FORMAT \"%4.1\" \
                   DEPOSIT DIFFERENCE MONOTONY MON_INCREASE \
                   /begin ANNOTATION ANNOTATION_LABEL \"a\" /end ANNOTATION \
                   /end AXIS_PTS";
        let (ap, out) = parse_and_write::<AxisPts>(src, "AXIS_PTS");
        assert_eq!(
            out,
            "/begin AXIS_PTS AX1\n  \"axis pts\"\n  0x810000\n  Meas_X\n  RL_AX\n  0\n  \
             CONV_AX\n  16\n  0 255\n  FORMAT \"%4.1\"\n  BYTE_ORDER MSB_LAST\n  READ_ONLY\n  \
             STEP_SIZE 1\n  GUARD_RAILS\n  MONOTONY MON_INCREASE\n  DEPOSIT DIFFERENCE\n  \
             /begin ANNOTATION\n    ANNOTATION_LABEL \"a\"\n  /end ANNOTATION\n\
             /end AXIS_PTS\n"
        );
        assert_eq!(ap.named.name, "AX1");
        assert_eq!(ap.addr.address, Some(0x810000));
        assert_eq!(ap.input_quantity.as_deref(), Some("Meas_X"));
        assert_eq!(ap.max_axis_points, 16);
        assert_eq!(ap.monotony, MonotonyType::MON_INCREASE);
        assert_eq!(ap.deposit, DepositType::DIFFERENCE);
        assert_reparse_eq(&ap, &out, "AXIS_PTS");
    }

    #[test]
    fn axis_pts_minimal() {
        let (ap, out) = parse_and_write::<AxisPts>(
            "/begin AXIS_PTS A \"d\" 0x2000 Inp RL 0.5 Conv 8 -1 1 /end AXIS_PTS",
            "AXIS_PTS",
        );
        assert_eq!(
            out,
            "/begin AXIS_PTS A\n  \"d\"\n  0x2000\n  Inp\n  RL\n  0.5\n  Conv\n  8\n  \
             -1 1\n/end AXIS_PTS\n"
        );
        assert_eq!(ap.monotony, MonotonyType::NotSet);
        assert_eq!(ap.deposit, DepositType::NotSet);
        assert_reparse_eq(&ap, &out, "AXIS_PTS");
    }

    #[test]
    fn axis_pts_skips_unknown_params() {
        let (ap, out) = parse_and_write::<AxisPts>(
            "/begin AXIS_PTS A \"d\" 0x2000 Inp RL 0 Conv 8 0 1 FOO MONOTONY STRICT_INCREASE \
             /end AXIS_PTS",
            "AXIS_PTS",
        );
        assert_eq!(ap.monotony, MonotonyType::STRICT_INCREASE);
        assert!(!out.contains("FOO"));
        assert!(out.contains("MONOTONY STRICT_INCREASE\n"));
    }

    #[test]
    fn axis_pts_rejects_unknown_monotony() {
        let toks = tokenize(
            "/begin AXIS_PTS A \"d\" 0x2000 Inp RL 0 Conv 8 0 1 MONOTONY BOGUS /end AXIS_PTS",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        assert!(AxisPts::parse(root.child("AXIS_PTS").unwrap()).is_err());
    }
}
