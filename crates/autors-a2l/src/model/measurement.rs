//! A2L measurement, axis description, blob, and instance model nodes.
//! Parsing keeps child order and unsupported blocks intact, while writing emits
//! canonical parameter values and preserves optional metadata required for
//! reliable read/write round trips.

use crate::block::Block;
use crate::error::{Error, Result};
use crate::model::annotation::Annotation;
use crate::model::base::{AddressFields, ByteOrder, ConversionRefFields, NamedFields};
use crate::model::enums::{
    A2lKeyword, AddrType, AxisType, DataType, DepositType, IndexMode, MonotonyType,
};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::{A2lInt, ParamCursor};
use crate::writer::Writer;

fn to_hex(v: u64) -> String {
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

fn read_matrix_dim(cur: &mut ParamCursor) -> Vec<i32> {
    let mut dims: Vec<i32> = Vec::new();
    while dims.len() < 5 {
        match cur.peek().and_then(|t| A2lInt::parse_a2l(&t.text)) {
            Some(v) => {
                dims.push(v);
                let _ = cur.next_token();
            }
            None => break,
        }
    }
    dims
}

fn write_matrix_dim(w: &mut Writer, dims: &[i32]) {
    let s = dims
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    w.tag_value(Some("MATRIX_DIM"), Some(&s), false);
}

fn write_named_block<T: Node>(node: &T, name: &str, w: &mut Writer) -> Result<()> {
    w.begin_block(&format!("{} {name}", T::KEYWORD));
    node.write_body(w)?;
    w.end_block(T::KEYWORD);
    Ok(())
}

/// Accuracy, LowerLimit, UpperLimit.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub named: NamedFields,
    pub data_type: DataType,
    pub conv: ConversionRefFields,
    pub resolution: i32,
    pub accuracy: f64,
    pub addr: AddressFields,
    pub layout: IndexMode,
    pub bit_mask: Option<u64>,
    pub error_mask: Option<u64>,
    pub discrete: bool,
    pub read_write: bool,
    pub matrix_dim: Option<Vec<i32>>,
    pub addr_type: AddrType,
    pub children: Vec<MeasurementChild>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MeasurementChild {
    /// `/begin ANNOTATION ...`.
    Annotation(Annotation),
    Unsupported(UnsupportedNode),
}

impl Default for Measurement {
    fn default() -> Self {
        Measurement {
            named: NamedFields::default(),
            data_type: DataType::default(),
            conv: ConversionRefFields::default(),
            resolution: 1,
            accuracy: 0.0,
            addr: AddressFields::default(),
            layout: IndexMode::default(),
            bit_mask: None,
            error_mask: None,
            discrete: false,
            read_write: false,
            matrix_dim: None,
            addr_type: AddrType::DIRECT,
            children: Vec::new(),
        }
    }
}

impl Node for Measurement {
    const KEYWORD: &'static str = "MEASUREMENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let data_type = enum_value::<DataType>(&mut cur, "MEASUREMENT")?;
        let conversion = cur.ident()?;
        let resolution = cur.int()?;
        let accuracy = cur.float()?;
        let lower_limit = cur.float()?;
        let upper_limit = cur.float()?;
        let mut conv = ConversionRefFields {
            conversion,
            lower_limit,
            upper_limit,
            ..Default::default()
        };
        let mut node = Measurement {
            named,
            data_type,
            resolution,
            accuracy,
            ..Default::default()
        };
        let mut array_size = 0i32;
        while !cur.is_empty() {
            if cur.take_if("ECU_ADDRESS") {
                node.addr.address = Some(cur.uint()?);
            } else if cur.take_if("LAYOUT") {
                node.layout = enum_value(&mut cur, "MEASUREMENT")?;
            } else if cur.take_if("BIT_MASK") {
                let v = cur.uint::<u64>()?;
                node.bit_mask = (v != u64::MAX).then_some(v);
            } else if cur.take_if("ERROR_MASK") {
                let v = cur.uint::<u64>()?;
                node.error_mask = (v != 0).then_some(v);
            } else if cur.take_if("DISCRETE") {
                node.discrete = true;
            } else if cur.take_if("READ_WRITE") {
                node.read_write = true;
            } else if cur.take_if("ARRAY_SIZE") {
                array_size = cur.int()?;
            } else if cur.take_if("MATRIX_DIM") {
                node.matrix_dim = Some(read_matrix_dim(&mut cur));
            } else if cur.take_if("ADDRESS_TYPE") {
                node.addr_type = enum_value(&mut cur, "MEASUREMENT")?;
            } else if node.addr.take(&mut cur)? || conv.take(&mut cur)? {
            } else {
                cur.next_token()?;
            }
        }
        if array_size > 1 && node.matrix_dim.is_none() {
            node.matrix_dim = Some(vec![array_size, 1, 1]);
        }
        node.conv = conv;
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                node.children
                    .push(MeasurementChild::Annotation(Annotation::parse(child)?));
            } else {
                node.children
                    .push(MeasurementChild::Unsupported(UnsupportedNode::from_block(
                        child,
                    )));
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, self.data_type.as_keyword().unwrap_or("Unsupported"));
        w.value_line(None, &self.conv.conversion);
        w.value_line(
            None,
            &format!("{} {}", self.resolution, to_dec(self.accuracy)),
        );
        // FORMAT / PHYS_UNIT / BYTE_ORDER / REF_MEMORY_SEGMENT
        self.conv.write_limits(w)?;
        self.addr.write(w)?;
        self.conv.write(w)?;
        w.tag_value(Some("LAYOUT"), self.layout.as_keyword(), false);
        if let Some(a) = self.addr.address {
            w.tag_value(Some("ECU_ADDRESS"), Some(&to_hex(u64::from(a))), false);
        }
        if let Some(bm) = self.bit_mask {
            w.tag_value(Some("BIT_MASK"), Some(&to_hex(bm)), false);
        }
        if let Some(em) = self.error_mask {
            w.tag_value(Some("ERROR_MASK"), Some(&to_hex(em)), false);
        }
        if self.discrete {
            w.value_line(None, "DISCRETE");
        }
        if self.read_write {
            w.value_line(None, "READ_WRITE");
        }
        if let Some(dims) = &self.matrix_dim {
            write_matrix_dim(w, dims);
        }
        if self.addr_type != AddrType::DIRECT {
            w.tag_value(Some("ADDRESS_TYPE"), self.addr_type.as_keyword(), false);
        }
        for child in &self.children {
            match child {
                MeasurementChild::Annotation(a) => a.write_block(w)?,
                MeasurementChild::Unsupported(u) => u.write_block(w)?,
            }
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixAxisPar {
    pub offset: f64,
    pub shift: f64,
    pub numberapo: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixAxisParDist {
    pub offset: f64,
    pub distance: f64,
    pub numberapo: i32,
}

/// LowerLimit, UpperLimit.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisDescr {
    pub axis_type: AxisType,
    pub input_quantity: String,
    pub conversion: String,
    pub max_axis_points: i32,
    pub lower_limit: f64,
    pub upper_limit: f64,
    pub axis_pts_ref: Option<String>,
    pub curve_axis_ref: Option<String>,
    pub read_only: bool,
    pub extended_limits: Option<(f64, f64)>,
    pub max_grad: Option<f64>,
    pub step_size: Option<f64>,
    pub format: Option<String>,
    pub phys_unit: Option<String>,
    pub byte_order: ByteOrder,
    pub monotony: MonotonyType,
    pub deposit: DepositType,
    pub fix_axis_par: Option<FixAxisPar>,
    pub fix_axis_par_dist: Option<FixAxisParDist>,
    pub children: Vec<AxisDescrChild>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AxisDescrChild {
    /// `/begin ANNOTATION ...`.
    Annotation(Annotation),
    Unsupported(UnsupportedNode),
}

impl Default for AxisDescr {
    fn default() -> Self {
        AxisDescr {
            axis_type: AxisType::STD_AXIS,
            input_quantity: String::new(),
            conversion: String::new(),
            max_axis_points: 0,
            lower_limit: 0.0,
            upper_limit: 0.0,
            axis_pts_ref: None,
            curve_axis_ref: None,
            read_only: false,
            extended_limits: None,
            max_grad: None,
            step_size: None,
            format: None,
            phys_unit: None,
            byte_order: ByteOrder::default(),
            monotony: MonotonyType::default(),
            deposit: DepositType::default(),
            fix_axis_par: None,
            fix_axis_par_dist: None,
            children: Vec::new(),
        }
    }
}

impl Node for AxisDescr {
    const KEYWORD: &'static str = "AXIS_DESCR";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = AxisDescr {
            axis_type: enum_value::<AxisType>(&mut cur, "AXIS_DESCR")?,
            input_quantity: cur.ident()?,
            conversion: cur.ident()?,
            max_axis_points: cur.int()?,
            lower_limit: cur.float()?,
            upper_limit: cur.float()?,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("AXIS_PTS_REF") {
                node.axis_pts_ref = Some(cur.ident()?);
            } else if cur.take_if("CURVE_AXIS_REF") {
                node.curve_axis_ref = Some(cur.ident()?);
            } else if cur.take_if("READ_ONLY") {
                node.read_only = true;
            } else if cur.take_if("EXTENDED_LIMITS") {
                node.extended_limits = Some((cur.float()?, cur.float()?));
            } else if cur.take_if("MAX_GRAD") {
                node.max_grad = Some(cur.float()?);
            } else if cur.take_if("STEP_SIZE") {
                node.step_size = Some(cur.float()?);
            } else if cur.take_if("FORMAT") {
                node.format = Some(cur.string()?);
            } else if cur.take_if("PHYS_UNIT") {
                node.phys_unit = Some(cur.string()?);
            } else if cur.take_if("BYTE_ORDER") {
                node.byte_order = enum_value(&mut cur, "AXIS_DESCR")?;
            } else if cur.take_if("MONOTONY") {
                node.monotony = enum_value(&mut cur, "AXIS_DESCR")?;
            } else if cur.take_if("DEPOSIT") {
                node.deposit = enum_value(&mut cur, "AXIS_DESCR")?;
            } else if cur.take_if("FIX_AXIS_PAR") {
                node.fix_axis_par = Some(FixAxisPar {
                    offset: cur.float()?,
                    shift: cur.float()?,
                    numberapo: cur.int()?,
                });
            } else if cur.take_if("FIX_AXIS_PAR_DIST") {
                node.fix_axis_par_dist = Some(FixAxisParDist {
                    offset: cur.float()?,
                    distance: cur.float()?,
                    numberapo: cur.int()?,
                });
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                node.children
                    .push(AxisDescrChild::Annotation(Annotation::parse(child)?));
            } else {
                node.children
                    .push(AxisDescrChild::Unsupported(UnsupportedNode::from_block(
                        child,
                    )));
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, self.axis_type.as_keyword().unwrap_or("STD_AXIS"));
        w.value_line(None, &self.input_quantity);
        w.value_line(None, &self.conversion);
        w.value_line(None, &self.max_axis_points.to_string());
        w.value_line(
            None,
            &format!("{} {}", to_dec(self.lower_limit), to_dec(self.upper_limit)),
        );
        if let Some(r) = self.axis_pts_ref.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("AXIS_PTS_REF"), Some(r), false);
        }
        if let Some(r) = self.curve_axis_ref.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("CURVE_AXIS_REF"), Some(r), false);
        }
        if self.read_only {
            w.value_line(None, "READ_ONLY");
        }
        if let Some((lo, hi)) = self.extended_limits {
            w.value_line(
                None,
                &format!("EXTENDED_LIMITS {} {}", to_dec(lo), to_dec(hi)),
            );
        }
        if let Some(mg) = self.max_grad {
            w.tag_value(Some("MAX_GRAD"), Some(&to_dec(mg)), false);
        }
        if let Some(ss) = self.step_size {
            w.tag_value(Some("STEP_SIZE"), Some(&to_dec(ss)), false);
        }
        if let Some(f) = self.format.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("FORMAT"), Some(f), true);
        }
        if let Some(u) = self.phys_unit.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("PHYS_UNIT"), Some(u), true);
        }
        if let Some(kw) = self.byte_order.as_keyword() {
            w.tag_value(Some("BYTE_ORDER"), Some(kw), false);
        }
        if let Some(kw) = self.monotony.as_keyword() {
            w.tag_value(Some("MONOTONY"), Some(kw), false);
        }
        if let Some(kw) = self.deposit.as_keyword() {
            w.tag_value(Some("DEPOSIT"), Some(kw), false);
        }
        if let Some(p) = &self.fix_axis_par {
            let v = format!("{} {} {}", to_dec(p.offset), to_dec(p.shift), p.numberapo);
            w.tag_value(Some("FIX_AXIS_PAR"), Some(&v), false);
        }
        if let Some(p) = &self.fix_axis_par_dist {
            let v = format!(
                "{} {} {}",
                to_dec(p.offset),
                to_dec(p.distance),
                p.numberapo
            );
            w.tag_value(Some("FIX_AXIS_PAR_DIST"), Some(&v), false);
        }
        for child in &self.children {
            match child {
                AxisDescrChild::Annotation(a) => a.write_block(w)?,
                AxisDescrChild::Unsupported(u) => u.write_block(w)?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub named: NamedFields,
    pub addr: AddressFields,
    pub size: u32,
    pub addr_type: AddrType,
    pub children: Vec<BlobChild>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlobChild {
    /// `/begin ANNOTATION ...`.
    Annotation(Annotation),
    Unsupported(UnsupportedNode),
}

impl Default for Blob {
    fn default() -> Self {
        Blob {
            named: NamedFields::default(),
            addr: AddressFields::default(),
            size: 0,
            addr_type: AddrType::DIRECT,
            children: Vec::new(),
        }
    }
}

impl Node for Blob {
    const KEYWORD: &'static str = "BLOB";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = Blob {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        node.addr.address = Some(cur.uint()?);
        node.size = cur.uint()?;
        while !cur.is_empty() {
            if cur.take_if("ADDRESS_TYPE") {
                node.addr_type = enum_value(&mut cur, "BLOB")?;
            } else if node.addr.take(&mut cur)? {
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                node.children
                    .push(BlobChild::Annotation(Annotation::parse(child)?));
            } else {
                node.children
                    .push(BlobChild::Unsupported(UnsupportedNode::from_block(child)));
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        if let Some(a) = self.addr.address {
            w.value_line(None, &to_hex(u64::from(a)));
        }
        w.value_line(None, &to_hex(u64::from(self.size)));
        self.addr.write(w)?;
        if self.addr_type != AddrType::DIRECT {
            w.tag_value(Some("ADDRESS_TYPE"), self.addr_type.as_keyword(), false);
        }
        for child in &self.children {
            match child {
                BlobChild::Annotation(a) => a.write_block(w)?,
                BlobChild::Unsupported(u) => u.write_block(w)?,
            }
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Instance {
    pub named: NamedFields,
    pub typedef_name: String,
    pub addr: AddressFields,
    pub layout: IndexMode,
    pub read_write: bool,
    pub matrix_dim: Option<Vec<i32>>,
    pub children: Vec<InstanceChild>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstanceChild {
    /// `/begin ANNOTATION ...`.
    Annotation(Annotation),
    Unsupported(UnsupportedNode),
}

impl Node for Instance {
    const KEYWORD: &'static str = "INSTANCE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = Instance {
            named: NamedFields::read_header(&mut cur)?,
            typedef_name: cur.ident()?,
            ..Default::default()
        };
        node.addr.address = Some(cur.uint()?);
        while !cur.is_empty() {
            if cur.take_if("LAYOUT") {
                node.layout = enum_value(&mut cur, "INSTANCE")?;
            } else if cur.take_if("READ_WRITE") {
                node.read_write = true;
            } else if cur.take_if("MATRIX_DIM") {
                node.matrix_dim = Some(read_matrix_dim(&mut cur));
            } else if node.addr.take(&mut cur)? {
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("ANNOTATION") {
                node.children
                    .push(InstanceChild::Annotation(Annotation::parse(child)?));
            } else {
                node.children
                    .push(InstanceChild::Unsupported(UnsupportedNode::from_block(
                        child,
                    )));
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, &self.typedef_name);
        if let Some(a) = self.addr.address {
            w.value_line(None, &to_hex(u64::from(a)));
        }
        self.addr.write(w)?;
        w.tag_value(Some("LAYOUT"), self.layout.as_keyword(), false);
        if self.read_write {
            w.value_line(None, "READ_WRITE");
        }
        if let Some(dims) = &self.matrix_dim {
            write_matrix_dim(w, dims);
        }
        for child in &self.children {
            match child {
                InstanceChild::Annotation(a) => a.write_block(w)?,
                InstanceChild::Unsupported(u) => u.write_block(w)?,
            }
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
    use crate::block::build_block_tree;
    use crate::model::enums::{CalibrationAccess, ScalingUnits};
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn block_of(src: &str, keyword: &str) -> Block {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        root.child(keyword).unwrap().clone()
    }

    fn measurement_roundtrip(src: &str) -> String {
        let m = Measurement::parse(&block_of(src, "MEASUREMENT")).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        m.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn measurement_full_roundtrip() {
        let out = measurement_roundtrip(
            "/begin MEASUREMENT MyMeas \"desc\" UWORD MyConv 4 0.1 0 255 \
             ECU_ADDRESS 0x1000 BIT_MASK 0xFF ERROR_MASK 0x8000 DISCRETE READ_WRITE \
             MATRIX_DIM 2 3 ADDRESS_TYPE PWORD LAYOUT ROW_DIR FORMAT \"%6.2\" \
             PHYS_UNIT \"bar\" BYTE_ORDER MSB_FIRST REF_MEMORY_SEGMENT Seg1 \
             DISPLAY_IDENTIFIER disp CALIBRATION_ACCESS CALIBRATION MAX_REFRESH 3 10 \
             SYMBOL_LINK \"sym\" -4 MODEL_LINK \"mdl\" ECU_ADDRESS_EXTENSION 2 \
             ARRAY_SIZE 6 \
             /begin ANNOTATION ANNOTATION_LABEL \"lbl\" /end ANNOTATION \
             /begin IF_DATA XCP 1 2 3 /end IF_DATA \
             /end MEASUREMENT",
        );
        assert_eq!(
            out,
            "/begin MEASUREMENT MyMeas\n\
             \x20 \"desc\"\n\
             \x20 UWORD\n\
             \x20 MyConv\n\
             \x20 4 0.1\n\
             \x20 0 255\n\
             \x20 ECU_ADDRESS_EXTENSION 2\n\
             \x20 DISPLAY_IDENTIFIER disp\n\
             \x20 CALIBRATION_ACCESS CALIBRATION\n\
             \x20 MAX_REFRESH 3 10\n\
             \x20 SYMBOL_LINK \"sym\" -4\n\
             \x20 MODEL_LINK \"mdl\"\n\
             \x20 FORMAT \"%6.2\"\n\
             \x20 PHYS_UNIT \"bar\"\n\
             \x20 BYTE_ORDER MSB_FIRST\n\
             \x20 REF_MEMORY_SEGMENT Seg1\n\
             \x20 LAYOUT ROW_DIR\n\
             \x20 ECU_ADDRESS 0x1000\n\
             \x20 BIT_MASK 0xFF\n\
             \x20 ERROR_MASK 0x8000\n\
             \x20 DISCRETE\n\
             \x20 READ_WRITE\n\
             \x20 MATRIX_DIM 2 3\n\
             \x20 ADDRESS_TYPE PWORD\n\
             \x20 /begin ANNOTATION\n\
             \x20   ANNOTATION_LABEL \"lbl\"\n\
             \x20 /end ANNOTATION\n\
             \x20 /begin IF_DATA\n\
             \x20   XCP 1 2 3\n\
             \x20 /end IF_DATA\n\
             /end MEASUREMENT\n"
        );
    }

    #[test]
    fn measurement_parse_fields() {
        let m = Measurement::parse(&block_of(
            "/begin MEASUREMENT M \"\" SWORD C 8 0.5 -10 200 \
             ECU_ADDRESS 0x1F MAX_REFRESH 6 5 MATRIX_DIM 2 2 READ_WRITE \
             /end MEASUREMENT",
            "MEASUREMENT",
        ))
        .unwrap();
        assert_eq!(m.named.name, "M");
        assert_eq!(m.named.description.as_deref(), Some(""));
        assert_eq!(m.data_type, DataType::SWord);
        assert_eq!(m.conv.conversion, "C");
        assert_eq!(m.resolution, 8);
        assert_eq!(m.accuracy, 0.5);
        assert_eq!(m.conv.lower_limit, -10.0);
        assert_eq!(m.conv.upper_limit, 200.0);
        assert_eq!(m.addr.address, Some(0x1F));
        assert_eq!(m.addr.max_refresh_unit, Some(ScalingUnits::Time_1Sec));
        assert_eq!(m.addr.max_refresh_rate, 5);
        assert_eq!(m.matrix_dim.as_deref(), Some(&[2, 2][..]));
        assert!(m.read_write);
        assert!(!m.discrete);
        assert_eq!(m.addr_type, AddrType::DIRECT);
    }

    #[test]
    fn measurement_array_size_becomes_matrix_dim() {
        let out = measurement_roundtrip(
            "/begin MEASUREMENT M \"\" SWORD C 1 0 0 100 ARRAY_SIZE 5 /end MEASUREMENT",
        );
        assert_eq!(
            out,
            "/begin MEASUREMENT M\n\
             \x20 \"\"\n\
             \x20 SWORD\n\
             \x20 C\n\
             \x20 1 0\n\
             \x20 0 100\n\
             \x20 MATRIX_DIM 5 1 1\n\
             /end MEASUREMENT\n"
        );
        let m = Measurement::parse(&block_of(
            "/begin MEASUREMENT M \"\" SWORD C 1 0 0 100 ARRAY_SIZE 1 /end MEASUREMENT",
            "MEASUREMENT",
        ))
        .unwrap();
        assert_eq!(m.matrix_dim, None);
    }

    #[test]
    fn measurement_unset_masks_not_written() {
        let out = measurement_roundtrip(
            "/begin MEASUREMENT M \"\" UWORD C 1 0 0 100 \
             BIT_MASK 0xFFFFFFFFFFFFFFFF ERROR_MASK 0 ADDRESS_TYPE DIRECT \
             /end MEASUREMENT",
        );
        assert_eq!(
            out,
            "/begin MEASUREMENT M\n\
             \x20 \"\"\n\
             \x20 UWORD\n\
             \x20 C\n\
             \x20 1 0\n\
             \x20 0 100\n\
             /end MEASUREMENT\n"
        );
    }

    #[test]
    fn measurement_unknown_tokens_skipped() {
        let m = Measurement::parse(&block_of(
            "/begin MEASUREMENT M \"\" UWORD C 1 0 0 100 FOO_BAR 123 DISCRETE /end MEASUREMENT",
            "MEASUREMENT",
        ))
        .unwrap();
        assert!(m.discrete);
        let mut w = Writer::new(WriterOptions::default());
        m.write_block(&mut w).unwrap();
        assert!(!w.into_string().contains("FOO_BAR"));
    }

    #[test]
    fn measurement_matrix_dim_stops_at_keyword() {
        let m = Measurement::parse(&block_of(
            "/begin MEASUREMENT M \"\" UWORD C 1 0 0 100 MATRIX_DIM 2 3 DISCRETE /end MEASUREMENT",
            "MEASUREMENT",
        ))
        .unwrap();
        assert_eq!(m.matrix_dim.as_deref(), Some(&[2, 3][..]));
        assert!(m.discrete);
    }

    fn axis_descr_roundtrip(src: &str) -> String {
        let a = AxisDescr::parse(&block_of(src, "AXIS_DESCR")).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        a.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn axis_descr_full_roundtrip() {
        let out = axis_descr_roundtrip(
            "/begin AXIS_DESCR STD_AXIS NO_INPUT_QUANTITY MyConv 16 0 100 \
             AXIS_PTS_REF AxisPts1 CURVE_AXIS_REF Curve1 READ_ONLY EXTENDED_LIMITS -10 200 \
             MAX_GRAD 1.5 STEP_SIZE 0.5 FORMAT \"%4.1\" PHYS_UNIT \"rpm\" \
             BYTE_ORDER MSB_LAST MONOTONY MON_INCREASE DEPOSIT ABSOLUTE \
             FIX_AXIS_PAR 0 1 16 FIX_AXIS_PAR_DIST 0 2.5 16 \
             /end AXIS_DESCR",
        );
        assert_eq!(
            out,
            "/begin AXIS_DESCR\n\
             \x20 STD_AXIS\n\
             \x20 NO_INPUT_QUANTITY\n\
             \x20 MyConv\n\
             \x20 16\n\
             \x20 0 100\n\
             \x20 AXIS_PTS_REF AxisPts1\n\
             \x20 CURVE_AXIS_REF Curve1\n\
             \x20 READ_ONLY\n\
             \x20 EXTENDED_LIMITS -10 200\n\
             \x20 MAX_GRAD 1.5\n\
             \x20 STEP_SIZE 0.5\n\
             \x20 FORMAT \"%4.1\"\n\
             \x20 PHYS_UNIT \"rpm\"\n\
             \x20 BYTE_ORDER MSB_LAST\n\
             \x20 MONOTONY MON_INCREASE\n\
             \x20 DEPOSIT ABSOLUTE\n\
             \x20 FIX_AXIS_PAR 0 1 16\n\
             \x20 FIX_AXIS_PAR_DIST 0 2.5 16\n\
             /end AXIS_DESCR\n"
        );
    }

    #[test]
    fn axis_descr_minimal() {
        let out =
            axis_descr_roundtrip("/begin AXIS_DESCR COM_AXIS Meas1 Conv1 8 -1 1 /end AXIS_DESCR");
        assert_eq!(
            out,
            "/begin AXIS_DESCR\n\
             \x20 COM_AXIS\n\
             \x20 Meas1\n\
             \x20 Conv1\n\
             \x20 8\n\
             \x20 -1 1\n\
             /end AXIS_DESCR\n"
        );
    }

    #[test]
    fn axis_descr_parse_fields_and_annotation() {
        let a = AxisDescr::parse(&block_of(
            "/begin AXIS_DESCR FIX_AXIS NO_INPUT_QUANTITY Conv 4 0 10 \
             BYTE_ORDER MSB_FIRST FIX_AXIS_PAR 1.5 0.5 4 \
             /begin ANNOTATION ANNOTATION_ORIGIN \"org\" /end ANNOTATION \
             /end AXIS_DESCR",
            "AXIS_DESCR",
        ))
        .unwrap();
        assert_eq!(a.axis_type, AxisType::FIX_AXIS);
        assert_eq!(a.input_quantity, "NO_INPUT_QUANTITY");
        assert_eq!(a.max_axis_points, 4);
        assert_eq!(a.byte_order, ByteOrder::MSB_FIRST);
        assert_eq!(
            a.fix_axis_par,
            Some(FixAxisPar {
                offset: 1.5,
                shift: 0.5,
                numberapo: 4
            })
        );
        assert!(matches!(
            a.children.as_slice(),
            [AxisDescrChild::Annotation(_)]
        ));
        let mut w = Writer::new(WriterOptions::default());
        a.write_block(&mut w).unwrap();
        assert!(w.into_string().contains("ANNOTATION_ORIGIN \"org\""));
    }

    #[test]
    fn blob_roundtrip() {
        let b = Blob::parse(&block_of(
            "/begin BLOB MyBlob \"blob desc\" 0x4000 0x100 \
             DISPLAY_IDENTIFIER disp MAX_REFRESH 6 5 ADDRESS_TYPE PLONG \
             /end BLOB",
            "BLOB",
        ))
        .unwrap();
        assert_eq!(b.named.name, "MyBlob");
        assert_eq!(b.addr.address, Some(0x4000));
        assert_eq!(b.size, 0x100);
        assert_eq!(b.addr_type, AddrType::PLONG);
        let mut w = Writer::new(WriterOptions::default());
        b.write_block(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin BLOB MyBlob\n\
             \x20 \"blob desc\"\n\
             \x20 0x4000\n\
             \x20 0x100\n\
             \x20 DISPLAY_IDENTIFIER disp\n\
             \x20 MAX_REFRESH 6 5\n\
             \x20 ADDRESS_TYPE PLONG\n\
             /end BLOB\n"
        );
    }

    #[test]
    fn blob_minimal_skips_defaults() {
        let b = Blob::parse(&block_of(
            "/begin BLOB B \"\" 0x0 0x10 CALIBRATION_ACCESS OFFLINE_CALIBRATION /end BLOB",
            "BLOB",
        ))
        .unwrap();
        assert_eq!(b.addr.calib_access, CalibrationAccess::OFFLINE_CALIBRATION);
        let mut w = Writer::new(WriterOptions::default());
        b.write_block(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin BLOB B\n\
             \x20 \"\"\n\
             \x20 0x0\n\
             \x20 0x10\n\
             \x20 CALIBRATION_ACCESS OFFLINE_CALIBRATION\n\
             /end BLOB\n"
        );
    }

    #[test]
    fn instance_roundtrip() {
        let i = Instance::parse(&block_of(
            "/begin INSTANCE MyInst \"inst desc\" MyTypedef 0x5000 \
             SYMBOL_LINK \"sym\" 4 LAYOUT COLUMN_DIR MATRIX_DIM 2 2 READ_WRITE \
             /begin OVERWRITE X 0 /end OVERWRITE \
             /end INSTANCE",
            "INSTANCE",
        ))
        .unwrap();
        assert_eq!(i.named.name, "MyInst");
        assert_eq!(i.typedef_name, "MyTypedef");
        assert_eq!(i.addr.address, Some(0x5000));
        assert_eq!(i.layout, IndexMode::COLUMN_DIR);
        assert_eq!(i.matrix_dim.as_deref(), Some(&[2, 2][..]));
        assert!(i.read_write);
        let mut w = Writer::new(WriterOptions::default());
        i.write_block(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin INSTANCE MyInst\n\
             \x20 \"inst desc\"\n\
             \x20 MyTypedef\n\
             \x20 0x5000\n\
             \x20 SYMBOL_LINK \"sym\" 4\n\
             \x20 LAYOUT COLUMN_DIR\n\
             \x20 READ_WRITE\n\
             \x20 MATRIX_DIM 2 2\n\
             \x20 /begin OVERWRITE\n\
             \x20   X 0\n\
             \x20 /end OVERWRITE\n\
             /end INSTANCE\n"
        );
    }

    #[test]
    fn unknown_enum_value_is_error() {
        assert!(Measurement::parse(&block_of(
            "/begin MEASUREMENT M \"\" BADTYPE C 1 0 0 100 /end MEASUREMENT",
            "MEASUREMENT",
        ))
        .is_err());
        assert!(AxisDescr::parse(&block_of(
            "/begin AXIS_DESCR STD_AXIS Q C 1 0 1 MONOTONY BAD /end AXIS_DESCR",
            "AXIS_DESCR",
        ))
        .is_err());
    }
}
