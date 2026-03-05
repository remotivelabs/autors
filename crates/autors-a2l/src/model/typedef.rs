//! A2L type definitions for axes, blobs, characteristics, measurements, and
//! structures.
//!
//! The parser collects the common name, description, conversion, record-layout,
//! limits, and dimension fields while preserving unsupported nested blocks for
//! round-trip output.

use crate::block::Block;
use crate::error::Result;
use crate::model::base::{ByteOrder, ConversionRefFields, NamedFields, RecordLayoutRefFields};
use crate::model::enums::CharacteristicType;
use crate::model::enums::{
    A2lKeyword, AddrType, DataType, DepositType, EncodingType, IndexMode, MonotonyType,
};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::{A2lInt, ParamCursor};
use crate::writer::Writer;

fn to_dec(v: f64) -> String {
    format!("{v}")
}

fn to_hex(v: u64) -> String {
    format!("0x{v:X}")
}

fn enum_kw<T: A2lKeyword + std::fmt::Debug + Copy>(v: T) -> String {
    match v.as_keyword() {
        Some(kw) => kw.to_string(),
        None => format!("{v:?}"),
    }
}

fn take_enum_or<T: A2lKeyword>(cur: &mut ParamCursor, default: T) -> Result<T> {
    let t = cur.next_token()?;
    Ok(T::from_keyword(&t.text).unwrap_or(default))
}

fn read_limits(cur: &mut ParamCursor) -> Result<(f64, f64)> {
    let a = cur.float()?;
    let b = cur.float()?;
    Ok((a.min(b), a.max(b)))
}

fn parse_matrix_dim(cur: &mut ParamCursor) -> Vec<i32> {
    let mut dims = Vec::new();
    while dims.len() < 5 {
        let Some(t) = cur.peek() else { break };
        let Some(v) = i32::parse_a2l(&t.text) else {
            break;
        };
        dims.push(v);
        let _ = cur.next_token();
    }
    dims
}

fn write_matrix_dim(w: &mut Writer, dims: &[i32]) {
    let line = dims
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    w.tag_value(Some("MATRIX_DIM"), Some(&line), false);
}

fn write_named_block<T: Node>(node: &T, name: &str, w: &mut Writer) -> Result<()> {
    w.begin_block(&format!("{} {}", T::KEYWORD, name));
    node.write_body(w)?;
    w.end_block(T::KEYWORD);
    Ok(())
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

fn encoding_as_keyword(e: EncodingType) -> &'static str {
    match e {
        EncodingType::ASCII => "ASCII",
        EncodingType::UTF8 => "UTF8",
        EncodingType::UTF16 => "UTF16",
        EncodingType::UTF32 => "UTF32",
    }
}

/// `/begin TYPEDEF_AXIS Name "LONG_IDENT" InputQuantity RecordLayout MaxDiff Conversion
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypedefAxis {
    pub named: NamedFields,
    pub input_quantity: Option<String>,
    pub rec: RecordLayoutRefFields,
    pub conv: ConversionRefFields,
    pub max_axis_points: i32,
    pub monotony: MonotonyType,
    pub deposit: DepositType,
}

impl Node for TypedefAxis {
    const KEYWORD: &'static str = "TYPEDEF_AXIS";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut td = TypedefAxis {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        td.input_quantity = Some(cur.ident()?);
        td.rec.record_layout = cur.ident()?;
        td.rec.max_diff = cur.float()?;
        td.conv.conversion = cur.ident()?;
        td.max_axis_points = cur.int()?;
        let (lo, hi) = read_limits(&mut cur)?;
        td.conv.lower_limit = lo;
        td.conv.upper_limit = hi;
        while !cur.is_empty() {
            if cur.take_if("PHYS_UNIT") {
                td.conv.phys_unit = Some(cur.string()?);
            } else if cur.take_if("STEP_SIZE") {
                td.rec.step_size = Some(cur.float()?);
            } else if cur.take_if("FORMAT") {
                td.conv.format = Some(cur.string()?);
            } else if cur.take_if("DEPOSIT") {
                td.deposit = take_enum_or(&mut cur, DepositType::NotSet)?;
            } else if cur.take_if("MONOTONY") {
                td.monotony = take_enum_or(&mut cur, MonotonyType::NotSet)?;
            } else if cur.take_if("BYTE_ORDER") {
                td.conv.byte_order = take_enum_or(&mut cur, ByteOrder::NotSet)?;
            } else if cur.take_if("EXTENDED_LIMITS") {
                td.rec.extended_limits = Some(read_limits(&mut cur)?);
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(td)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        if let Some(iq) = &self.input_quantity {
            w.value_line(None, iq);
        }
        if !self.rec.record_layout.is_empty() {
            w.value_line(None, &self.rec.record_layout);
        }
        w.value_line(None, &to_dec(self.rec.max_diff));
        if !self.conv.conversion.is_empty() {
            w.value_line(None, &self.conv.conversion);
        }
        w.value_line(None, &self.max_axis_points.to_string());
        self.conv.write_limits(w)?;
        self.conv.write(w)?;
        self.rec.write(w)?;
        if let Some(kw) = self.monotony.as_keyword() {
            w.tag_value(Some("MONOTONY"), Some(kw), false);
        }
        if let Some(kw) = self.deposit.as_keyword() {
            w.tag_value(Some("DEPOSIT"), Some(kw), false);
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedefBlob {
    pub named: NamedFields,
    pub size: u32,
    pub addr_type: AddrType,
}

impl Default for TypedefBlob {
    fn default() -> Self {
        TypedefBlob {
            named: NamedFields::default(),
            size: 0,
            addr_type: AddrType::DIRECT,
        }
    }
}

impl Node for TypedefBlob {
    const KEYWORD: &'static str = "TYPEDEF_BLOB";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut td = TypedefBlob {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        td.size = cur.uint::<u32>()?;
        while !cur.is_empty() {
            if cur.take_if("ADDRESS_TYPE") {
                td.addr_type = take_enum_or(&mut cur, AddrType::DIRECT)?;
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(td)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, &to_hex(u64::from(self.size)));
        if self.addr_type != AddrType::DIRECT {
            w.tag_value(Some("ADDRESS_TYPE"), self.addr_type.as_keyword(), false);
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

/// `/begin TYPEDEF_CHARACTERISTIC Name "LONG_IDENT" CharType RecordLayout MaxDiff
#[derive(Debug, Clone, PartialEq)]
pub struct TypedefCharacteristic {
    pub named: NamedFields,
    pub char_type: CharacteristicType,
    pub rec: RecordLayoutRefFields,
    pub conv: ConversionRefFields,
    pub bitmask: Option<u64>,
    pub number: i32,
    pub matrix_dim: Option<Vec<i32>>,
    pub discrete: bool,
    pub encoding: EncodingType,
}

impl Default for TypedefCharacteristic {
    fn default() -> Self {
        TypedefCharacteristic {
            named: NamedFields::default(),
            char_type: CharacteristicType::NotSet,
            rec: RecordLayoutRefFields::default(),
            conv: ConversionRefFields::default(),
            bitmask: None,
            number: 0,
            matrix_dim: None,
            discrete: false,
            encoding: EncodingType::ASCII,
        }
    }
}

impl Node for TypedefCharacteristic {
    const KEYWORD: &'static str = "TYPEDEF_CHARACTERISTIC";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut td = TypedefCharacteristic {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        td.char_type = take_enum_or(&mut cur, CharacteristicType::NotSet)?;
        td.rec.record_layout = cur.ident()?;
        td.rec.max_diff = cur.float()?;
        td.conv.conversion = cur.ident()?;
        let (lo, hi) = read_limits(&mut cur)?;
        td.conv.lower_limit = lo;
        td.conv.upper_limit = hi;
        while !cur.is_empty() {
            if cur.take_if("DISCRETE") {
                td.discrete = true;
            } else if cur.take_if("ENCODING") {
                let t = cur.next_token()?;
                td.encoding = encoding_from_keyword(&t.text).unwrap_or(EncodingType::ASCII);
            } else if cur.take_if("BIT_MASK") {
                td.bitmask = Some(cur.uint::<u64>()?);
            } else if cur.take_if("FORMAT") {
                td.conv.format = Some(cur.string()?);
            } else if cur.take_if("NUMBER") {
                td.number = cur.int()?;
            } else if cur.take_if("PHYS_UNIT") {
                td.conv.phys_unit = Some(cur.string()?);
            } else if cur.take_if("STEP_SIZE") {
                td.rec.step_size = Some(cur.float()?);
            } else if cur.take_if("BYTE_ORDER") {
                td.conv.byte_order = take_enum_or(&mut cur, ByteOrder::NotSet)?;
            } else if cur.take_if("MATRIX_DIM") {
                td.matrix_dim = Some(parse_matrix_dim(&mut cur));
            } else if cur.take_if("EXTENDED_LIMITS") {
                td.rec.extended_limits = Some(read_limits(&mut cur)?);
            } else if cur.take_if("DISPLAY_IDENTIFIER") {
                let _ = cur.ident()?;
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(td)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, &enum_kw(self.char_type));
        if !self.rec.record_layout.is_empty() {
            w.value_line(None, &self.rec.record_layout);
        }
        w.value_line(None, &to_dec(self.rec.max_diff));
        if !self.conv.conversion.is_empty() {
            w.value_line(None, &self.conv.conversion);
        }
        self.conv.write_limits(w)?;
        self.conv.write(w)?;
        self.rec.write(w)?;
        if let Some(bm) = self.bitmask {
            w.tag_value(Some("BIT_MASK"), Some(&to_hex(bm)), false);
        }
        if self.number != 0 {
            w.tag_value(Some("NUMBER"), Some(&self.number.to_string()), false);
        }
        if let Some(dims) = &self.matrix_dim {
            write_matrix_dim(w, dims);
        }
        if self.discrete {
            w.value_line(None, "DISCRETE");
        }
        if self.encoding != EncodingType::ASCII {
            w.tag_value(
                Some("ENCODING"),
                Some(encoding_as_keyword(self.encoding)),
                false,
            );
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

/// `/begin TYPEDEF_MEASUREMENT Name "LONG_IDENT" DataType Conversion Resolution
#[derive(Debug, Clone, PartialEq)]
pub struct TypedefMeasurement {
    pub named: NamedFields,
    pub data_type: DataType,
    pub conv: ConversionRefFields,
    pub resolution: i32,
    pub accuracy: f64,
    pub layout: IndexMode,
    pub bit_mask: Option<u64>,
    pub error_mask: u64,
    pub discrete: bool,
    pub matrix_dim: Option<Vec<i32>>,
    pub addr_type: AddrType,
}

impl Default for TypedefMeasurement {
    fn default() -> Self {
        TypedefMeasurement {
            named: NamedFields::default(),
            data_type: DataType::default(),
            conv: ConversionRefFields::default(),
            resolution: 1,
            accuracy: 0.0,
            layout: IndexMode::NotSet,
            bit_mask: None,
            error_mask: 0,
            discrete: false,
            matrix_dim: None,
            addr_type: AddrType::DIRECT,
        }
    }
}

impl Node for TypedefMeasurement {
    const KEYWORD: &'static str = "TYPEDEF_MEASUREMENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut td = TypedefMeasurement {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        td.data_type = take_enum_or(&mut cur, DataType::default())?;
        td.conv.conversion = cur.ident()?;
        td.resolution = cur.int()?;
        td.accuracy = cur.float()?;
        let (lo, hi) = read_limits(&mut cur)?;
        td.conv.lower_limit = lo;
        td.conv.upper_limit = hi;
        while !cur.is_empty() {
            if cur.take_if("FORMAT") {
                td.conv.format = Some(cur.string()?);
            } else if cur.take_if("LAYOUT") {
                td.layout = take_enum_or(&mut cur, IndexMode::NotSet)?;
            } else if cur.take_if("BYTE_ORDER") {
                td.conv.byte_order = take_enum_or(&mut cur, ByteOrder::NotSet)?;
            } else if cur.take_if("ERROR_MASK") {
                td.error_mask = cur.uint::<u64>()?;
            } else if cur.take_if("MATRIX_DIM") {
                td.matrix_dim = Some(parse_matrix_dim(&mut cur));
            } else if cur.take_if("BIT_MASK") {
                td.bit_mask = Some(cur.uint::<u64>()?);
            } else if cur.take_if("DISCRETE") {
                td.discrete = true;
            } else if cur.take_if("PHYS_UNIT") {
                td.conv.phys_unit = Some(cur.string()?);
            } else if cur.take_if("ADDRESS_TYPE") {
                td.addr_type = take_enum_or(&mut cur, AddrType::DIRECT)?;
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(td)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, &enum_kw(self.data_type));
        if !self.conv.conversion.is_empty() {
            w.value_line(None, &self.conv.conversion);
        }
        w.value_line(
            None,
            &format!("{} {}", self.resolution, to_dec(self.accuracy)),
        );
        self.conv.write_limits(w)?;
        self.conv.write(w)?;
        if let Some(kw) = self.layout.as_keyword() {
            w.tag_value(Some("LAYOUT"), Some(kw), false);
        }
        if let Some(bm) = self.bit_mask {
            w.tag_value(Some("BIT_MASK"), Some(&to_hex(bm)), false);
        }
        if self.error_mask != 0 {
            w.tag_value(Some("ERROR_MASK"), Some(&to_hex(self.error_mask)), false);
        }
        if self.discrete {
            w.value_line(None, "DISCRETE");
        }
        if let Some(dims) = &self.matrix_dim {
            write_matrix_dim(w, dims);
        }
        if self.addr_type != AddrType::DIRECT {
            w.tag_value(Some("ADDRESS_TYPE"), self.addr_type.as_keyword(), false);
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedefStructureChild {
    /// `/begin STRUCTURE_COMPONENT`.
    Component(StructureComponent),
    Unsupported(UnsupportedNode),
}

impl TypedefStructureChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            TypedefStructureChild::Component(n) => n.write_block(w),
            TypedefStructureChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedefStructure {
    pub named: NamedFields,
    pub size: u32,
    pub addr_type: AddrType,
    pub consistent_exchange: bool,
    pub symbol_type_link: Option<String>,
    pub children: Vec<TypedefStructureChild>,
}

impl Default for TypedefStructure {
    fn default() -> Self {
        TypedefStructure {
            named: NamedFields::default(),
            size: 0,
            addr_type: AddrType::DIRECT,
            consistent_exchange: false,
            symbol_type_link: None,
            children: Vec::new(),
        }
    }
}

impl Node for TypedefStructure {
    const KEYWORD: &'static str = "TYPEDEF_STRUCTURE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut td = TypedefStructure {
            named: NamedFields::read_header(&mut cur)?,
            ..Default::default()
        };
        td.size = cur.uint::<u32>()?;
        while !cur.is_empty() {
            if cur.take_if("CONSISTENT_EXCHANGE") {
                td.consistent_exchange = true;
            } else if cur.take_if("SYMBOL_TYPE_LINK") {
                td.symbol_type_link = Some(cur.ident()?);
            } else if cur.take_if("ADDRESS_TYPE") {
                td.addr_type = take_enum_or(&mut cur, AddrType::DIRECT)?;
            } else {
                let _ = cur.next_token();
            }
        }
        for child in block.children() {
            td.children.push(
                if child.keyword.eq_ignore_ascii_case("STRUCTURE_COMPONENT") {
                    TypedefStructureChild::Component(StructureComponent::parse(child)?)
                } else {
                    TypedefStructureChild::Unsupported(UnsupportedNode::from_block(child))
                },
            );
        }
        Ok(td)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        if self.consistent_exchange {
            w.value_line(None, "CONSISTENT_EXCHANGE");
        }
        if let Some(link) = self.symbol_type_link.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("SYMBOL_TYPE_LINK"), Some(link), false);
        }
        w.value_line(None, &to_hex(u64::from(self.size)));
        if self.addr_type != AddrType::DIRECT {
            w.tag_value(Some("ADDRESS_TYPE"), self.addr_type.as_keyword(), false);
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
pub enum StructureComponentChild {
    /// `/begin AR_COMPONENT`.
    ArComponent(ArComponent),
    Unsupported(UnsupportedNode),
}

impl StructureComponentChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            StructureComponentChild::ArComponent(n) => n.write_block(w),
            StructureComponentChild::Unsupported(n) => n.write_block(w),
        }
    }
}

/// `/begin STRUCTURE_COMPONENT Name TypedefName AddressOffset ... /end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureComponent {
    pub name: String,
    pub typedef_name: String,
    pub address_offset: u32,
    pub layout: IndexMode,
    pub matrix_dim: Option<Vec<i32>>,
    pub symbol_type_link: Option<String>,
    pub children: Vec<StructureComponentChild>,
}

impl Default for StructureComponent {
    fn default() -> Self {
        StructureComponent {
            name: String::new(),
            typedef_name: String::new(),
            address_offset: u32::MAX,
            layout: IndexMode::NotSet,
            matrix_dim: None,
            symbol_type_link: None,
            children: Vec::new(),
        }
    }
}

impl Node for StructureComponent {
    const KEYWORD: &'static str = "STRUCTURE_COMPONENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut sc = StructureComponent {
            name: cur.ident()?,
            typedef_name: cur.ident()?,
            address_offset: cur.uint::<u32>()?,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("LAYOUT") {
                sc.layout = take_enum_or(&mut cur, IndexMode::NotSet)?;
            } else if cur.take_if("MATRIX_DIM") {
                sc.matrix_dim = Some(parse_matrix_dim(&mut cur));
            } else if cur.take_if("SYMBOL_TYPE_LINK") {
                sc.symbol_type_link = Some(cur.ident()?);
            } else {
                let _ = cur.next_token();
            }
        }
        for child in block.children() {
            sc.children
                .push(if child.keyword.eq_ignore_ascii_case("AR_COMPONENT") {
                    StructureComponentChild::ArComponent(ArComponent::parse(child)?)
                } else {
                    StructureComponentChild::Unsupported(UnsupportedNode::from_block(child))
                });
        }
        Ok(sc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if !self.typedef_name.is_empty() {
            w.value_line(None, &self.typedef_name);
        }
        w.value_line(None, &to_hex(u64::from(self.address_offset)));
        if let Some(kw) = self.layout.as_keyword() {
            w.tag_value(Some("LAYOUT"), Some(kw), false);
        }
        if let Some(dims) = &self.matrix_dim {
            write_matrix_dim(w, dims);
        }
        if let Some(link) = self.symbol_type_link.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("SYMBOL_TYPE_LINK"), Some(link), false);
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.name, w)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Overwrite {
    pub name: String,
    pub axis_no: u16,
    pub conversion: Option<String>,
    pub format: Option<String>,
    pub phys_unit: Option<String>,
    pub input_quantity: Option<String>,
    pub monotony: MonotonyType,
    pub lower_limit: Option<f64>,
    pub upper_limit: Option<f64>,
    pub lower_limit_ex: Option<f64>,
    pub upper_limit_ex: Option<f64>,
}

impl Default for Overwrite {
    fn default() -> Self {
        Overwrite {
            name: String::new(),
            axis_no: 0,
            conversion: None,
            format: None,
            phys_unit: None,
            input_quantity: None,
            monotony: MonotonyType::NotSet,
            lower_limit: None,
            upper_limit: None,
            lower_limit_ex: Some(0.0),
            upper_limit_ex: Some(0.0),
        }
    }
}

impl Node for Overwrite {
    const KEYWORD: &'static str = "OVERWRITE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut ow = Overwrite {
            name: cur.ident()?,
            axis_no: cur.uint::<u16>()?,
            ..Default::default()
        };
        while !cur.is_empty() {
            if cur.take_if("FORMAT") {
                ow.format = Some(cur.ident()?);
            } else if cur.take_if("LIMITS") {
                let (lo, hi) = read_limits(&mut cur)?;
                ow.lower_limit = Some(lo);
                ow.upper_limit = Some(hi);
            } else if cur.take_if("CONVERSION") {
                ow.conversion = Some(cur.ident()?);
            } else if cur.take_if("PHYS_UNIT") {
                ow.phys_unit = Some(cur.ident()?);
            } else if cur.take_if("INPUT_QUANTITY") {
                ow.input_quantity = Some(cur.ident()?);
            } else if cur.take_if("MONOTONY") {
                ow.monotony = take_enum_or(&mut cur, MonotonyType::NotSet)?;
            } else if cur.take_if("EXTENDED_LIMITS") {
                let (lo, hi) = read_limits(&mut cur)?;
                ow.lower_limit_ex = Some(lo);
                ow.upper_limit_ex = Some(hi);
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(ow)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &self.axis_no.to_string());
        if let Some(cv) = self.conversion.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("CONVERSION"), Some(cv), false);
        }
        if let Some(f) = self.format.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("FORMAT"), Some(f), false);
        }
        if let Some(u) = self.phys_unit.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("PHYS_UNIT"), Some(u), false);
        }
        if let Some(iq) = self.input_quantity.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("INPUT_QUANTITY"), Some(iq), false);
        }
        if let Some(kw) = self.monotony.as_keyword() {
            w.tag_value(Some("MONOTONY"), Some(kw), false);
        }
        if let (Some(lo), Some(hi)) = (self.lower_limit, self.upper_limit) {
            w.value_line(None, &format!("LIMITS {} {}", to_dec(lo), to_dec(hi)));
        }
        if let (Some(lo), Some(hi)) = (self.lower_limit_ex, self.upper_limit_ex) {
            w.value_line(
                None,
                &format!("EXTENDED_LIMITS {} {}", to_dec(lo), to_dec(hi)),
            );
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.name, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArComponent {
    pub component_type: String,
    pub prototype_of: Option<String>,
}

impl Node for ArComponent {
    const KEYWORD: &'static str = "AR_COMPONENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut ac = ArComponent {
            component_type: cur.string()?,
            ..Default::default()
        };
        while cur.remaining() >= 2 {
            if cur.take_if("PROTOTYPE_OF") {
                ac.prototype_of = Some(cur.ident()?);
            } else {
                let _ = cur.next_token();
            }
        }
        Ok(ac)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.component_type), true);
        if let Some(p) = self.prototype_of.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("PROTOTYPE_OF"), Some(p), false);
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
    fn typedef_axis_roundtrip() {
        let out = roundtrip::<TypedefAxis>(
            "/begin TYPEDEF_AXIS Ax \"desc\" InQ RL1 0 Conv1 16 0 100 \
             DEPOSIT ABSOLUTE MONOTONY MON_INCREASE FORMAT \"%6.2\" PHYS_UNIT \"rpm\" \
             BYTE_ORDER MSB_FIRST STEP_SIZE 0.5 EXTENDED_LIMITS 200 -10 /end TYPEDEF_AXIS",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_AXIS Ax\n\
             \x20 \"desc\"\n\
             \x20 InQ\n\
             \x20 RL1\n\
             \x20 0\n\
             \x20 Conv1\n\
             \x20 16\n\
             \x20 0 100\n\
             \x20 FORMAT \"%6.2\"\n\
             \x20 PHYS_UNIT \"rpm\"\n\
             \x20 BYTE_ORDER MSB_FIRST\n\
             \x20 EXTENDED_LIMITS -10 200\n\
             \x20 STEP_SIZE 0.5\n\
             \x20 MONOTONY MON_INCREASE\n\
             \x20 DEPOSIT ABSOLUTE\n\
             /end TYPEDEF_AXIS\n"
        );
    }

    #[test]
    fn typedef_axis_minimal_and_limit_normalization() {
        let out = roundtrip::<TypedefAxis>(
            "/begin TYPEDEF_AXIS Ax \"\" InQ RL1 0.5 Conv1 4 100 0 /end TYPEDEF_AXIS",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_AXIS Ax\n\
             \x20 \"\"\n\
             \x20 InQ\n\
             \x20 RL1\n\
             \x20 0.5\n\
             \x20 Conv1\n\
             \x20 4\n\
             \x20 0 100\n\
             /end TYPEDEF_AXIS\n"
        );
    }

    #[test]
    fn typedef_axis_skips_unknown_params_and_bad_enum() {
        let out = roundtrip::<TypedefAxis>(
            "/begin TYPEDEF_AXIS Ax \"\" InQ RL1 0 Conv1 4 0 100 MONOTONY BAD SOMETHING 7 /end TYPEDEF_AXIS",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_AXIS Ax\n\
             \x20 \"\"\n\
             \x20 InQ\n\
             \x20 RL1\n\
             \x20 0\n\
             \x20 Conv1\n\
             \x20 4\n\
             \x20 0 100\n\
             /end TYPEDEF_AXIS\n"
        );
    }

    #[test]
    fn typedef_blob_roundtrip() {
        let out = roundtrip::<TypedefBlob>(
            "/begin TYPEDEF_BLOB Bl \"blob\" 0x40 ADDRESS_TYPE PBYTE /end TYPEDEF_BLOB",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_BLOB Bl\n\
             \x20 \"blob\"\n\
             \x20 0x40\n\
             \x20 ADDRESS_TYPE PBYTE\n\
             /end TYPEDEF_BLOB\n"
        );
    }

    #[test]
    fn typedef_blob_default_addr_type_omitted() {
        let out = roundtrip::<TypedefBlob>("/begin TYPEDEF_BLOB B \"\" 16 /end TYPEDEF_BLOB");
        assert_eq!(
            out,
            "/begin TYPEDEF_BLOB B\n\
             \x20 \"\"\n\
             \x20 0x10\n\
             /end TYPEDEF_BLOB\n"
        );
    }

    #[test]
    fn typedef_characteristic_roundtrip() {
        let out = roundtrip::<TypedefCharacteristic>(
            "/begin TYPEDEF_CHARACTERISTIC Ch \"char\" CURVE RL2 0 Conv2 0 255 \
             DISCRETE ENCODING UTF8 BIT_MASK 0xFF NUMBER 6 MATRIX_DIM 2 3 \
             FORMAT \"%4.1\" PHYS_UNIT \"V\" BYTE_ORDER MSB_LAST STEP_SIZE 1 \
             EXTENDED_LIMITS 300 -5 DISPLAY_IDENTIFIER disp /end TYPEDEF_CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_CHARACTERISTIC Ch\n\
             \x20 \"char\"\n\
             \x20 CURVE\n\
             \x20 RL2\n\
             \x20 0\n\
             \x20 Conv2\n\
             \x20 0 255\n\
             \x20 FORMAT \"%4.1\"\n\
             \x20 PHYS_UNIT \"V\"\n\
             \x20 BYTE_ORDER MSB_LAST\n\
             \x20 EXTENDED_LIMITS -5 300\n\
             \x20 STEP_SIZE 1\n\
             \x20 BIT_MASK 0xFF\n\
             \x20 NUMBER 6\n\
             \x20 MATRIX_DIM 2 3\n\
             \x20 DISCRETE\n\
             \x20 ENCODING UTF8\n\
             /end TYPEDEF_CHARACTERISTIC\n"
        );
    }

    #[test]
    fn typedef_characteristic_minimal() {
        let out = roundtrip::<TypedefCharacteristic>(
            "/begin TYPEDEF_CHARACTERISTIC C \"\" VAL_BLK RL 1.5 Cv 0 10 /end TYPEDEF_CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_CHARACTERISTIC C\n\
             \x20 \"\"\n\
             \x20 VAL_BLK\n\
             \x20 RL\n\
             \x20 1.5\n\
             \x20 Cv\n\
             \x20 0 10\n\
             /end TYPEDEF_CHARACTERISTIC\n"
        );
    }

    #[test]
    fn typedef_measurement_roundtrip() {
        let out = roundtrip::<TypedefMeasurement>(
            "/begin TYPEDEF_MEASUREMENT Me \"meas\" UWORD Conv3 2 0.1 0 65535 \
             ADDRESS_TYPE PLONG BIT_MASK 0xFFFF DISCRETE ERROR_MASK 0x8000 \
             FORMAT \"%5.0\" LAYOUT ROW_DIR MATRIX_DIM 4 PHYS_UNIT \"km/h\" \
             BYTE_ORDER MSB_FIRST /end TYPEDEF_MEASUREMENT",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_MEASUREMENT Me\n\
             \x20 \"meas\"\n\
             \x20 UWORD\n\
             \x20 Conv3\n\
             \x20 2 0.1\n\
             \x20 0 65535\n\
             \x20 FORMAT \"%5.0\"\n\
             \x20 PHYS_UNIT \"km/h\"\n\
             \x20 BYTE_ORDER MSB_FIRST\n\
             \x20 LAYOUT ROW_DIR\n\
             \x20 BIT_MASK 0xFFFF\n\
             \x20 ERROR_MASK 0x8000\n\
             \x20 DISCRETE\n\
             \x20 MATRIX_DIM 4\n\
             \x20 ADDRESS_TYPE PLONG\n\
             /end TYPEDEF_MEASUREMENT\n"
        );
    }

    #[test]
    fn typedef_measurement_minimal() {
        let out = roundtrip::<TypedefMeasurement>(
            "/begin TYPEDEF_MEASUREMENT Me \"\" UBYTE Conv3 1 0 0 255 /end TYPEDEF_MEASUREMENT",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_MEASUREMENT Me\n\
             \x20 \"\"\n\
             \x20 UBYTE\n\
             \x20 Conv3\n\
             \x20 1 0\n\
             \x20 0 255\n\
             /end TYPEDEF_MEASUREMENT\n"
        );
    }

    #[test]
    fn typedef_measurement_matrix_dim_caps_at_five() {
        let toks = tokenize(
            "/begin TYPEDEF_MEASUREMENT M \"\" UBYTE C 1 0 0 255 MATRIX_DIM 1 2 3 4 5 6 /end TYPEDEF_MEASUREMENT",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        let td = TypedefMeasurement::parse(root.child("TYPEDEF_MEASUREMENT").unwrap()).unwrap();
        assert_eq!(td.matrix_dim, Some(vec![1, 2, 3, 4, 5]));
        let mut w = Writer::new(WriterOptions::default());
        td.write_block(&mut w).unwrap();
        assert!(w.into_string().contains("MATRIX_DIM 1 2 3 4 5\n"));
    }

    #[test]
    fn typedef_structure_roundtrip() {
        let out = roundtrip::<TypedefStructure>(
            "/begin TYPEDEF_STRUCTURE St \"struct\" 0x20 CONSISTENT_EXCHANGE \
             SYMBOL_TYPE_LINK my_type_t ADDRESS_TYPE PWORD \
             /begin STRUCTURE_COMPONENT comp1 TypedefA 0x10 LAYOUT COLUMN_DIR \
             MATRIX_DIM 2 2 SYMBOL_TYPE_LINK comp_t \
             /begin AR_COMPONENT \"comp type\" PROTOTYPE_OF proto /end AR_COMPONENT \
             /end STRUCTURE_COMPONENT \
             /begin IF_DATA X 1 2 /end IF_DATA \
             /end TYPEDEF_STRUCTURE",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_STRUCTURE St\n\
             \x20 \"struct\"\n\
             \x20 CONSISTENT_EXCHANGE\n\
             \x20 SYMBOL_TYPE_LINK my_type_t\n\
             \x20 0x20\n\
             \x20 ADDRESS_TYPE PWORD\n\
             \x20 /begin STRUCTURE_COMPONENT comp1\n\
             \x20   TypedefA\n\
             \x20   0x10\n\
             \x20   LAYOUT COLUMN_DIR\n\
             \x20   MATRIX_DIM 2 2\n\
             \x20   SYMBOL_TYPE_LINK comp_t\n\
             \x20   /begin AR_COMPONENT\n\
             \x20     \"comp type\"\n\
             \x20     PROTOTYPE_OF proto\n\
             \x20   /end AR_COMPONENT\n\
             \x20 /end STRUCTURE_COMPONENT\n\
             \x20 /begin IF_DATA\n\
             \x20   X 1 2\n\
             \x20 /end IF_DATA\n\
             /end TYPEDEF_STRUCTURE\n"
        );
    }

    #[test]
    fn typedef_structure_minimal() {
        let out = roundtrip::<TypedefStructure>(
            "/begin TYPEDEF_STRUCTURE S \"\" 8 /end TYPEDEF_STRUCTURE",
        );
        assert_eq!(
            out,
            "/begin TYPEDEF_STRUCTURE S\n\
             \x20 \"\"\n\
             \x20 0x8\n\
             /end TYPEDEF_STRUCTURE\n"
        );
    }

    #[test]
    fn structure_component_minimal() {
        let out = roundtrip::<StructureComponent>(
            "/begin STRUCTURE_COMPONENT c TDef 255 /end STRUCTURE_COMPONENT",
        );
        assert_eq!(
            out,
            "/begin STRUCTURE_COMPONENT c\n\
             \x20 TDef\n\
             \x20 0xFF\n\
             /end STRUCTURE_COMPONENT\n"
        );
    }

    #[test]
    fn overwrite_roundtrip() {
        let out = roundtrip::<Overwrite>(
            "/begin OVERWRITE Ov 3 INPUT_QUANTITY Meas1 FORMAT \"%3.1\" CONVERSION Conv4 \
             PHYS_UNIT deg LIMITS 90 -90 MONOTONY STRICT_INCREASE EXTENDED_LIMITS 100 -100 /end OVERWRITE",
        );
        assert_eq!(
            out,
            "/begin OVERWRITE Ov\n\
             \x20 3\n\
             \x20 CONVERSION Conv4\n\
             \x20 FORMAT %3.1\n\
             \x20 PHYS_UNIT deg\n\
             \x20 INPUT_QUANTITY Meas1\n\
             \x20 MONOTONY STRICT_INCREASE\n\
             \x20 LIMITS -90 90\n\
             \x20 EXTENDED_LIMITS -100 100\n\
             /end OVERWRITE\n"
        );
    }

    #[test]
    fn overwrite_minimal_writes_extended_limits_zero() {
        let out = roundtrip::<Overwrite>("/begin OVERWRITE Ov 0 /end OVERWRITE");
        assert_eq!(
            out,
            "/begin OVERWRITE Ov\n\
             \x20 0\n\
             \x20 EXTENDED_LIMITS 0 0\n\
             /end OVERWRITE\n"
        );
    }

    #[test]
    fn ar_component_roundtrip() {
        let out = roundtrip::<ArComponent>(
            "/begin AR_COMPONENT \"sw-c type\" PROTOTYPE_OF proto1 /end AR_COMPONENT",
        );
        assert_eq!(
            out,
            "/begin AR_COMPONENT\n\
             \x20 \"sw-c type\"\n\
             \x20 PROTOTYPE_OF proto1\n\
             /end AR_COMPONENT\n"
        );
    }

    #[test]
    fn ar_component_without_prototype() {
        let out = roundtrip::<ArComponent>("/begin AR_COMPONENT \"sw-c\" /end AR_COMPONENT");
        assert_eq!(
            out,
            "/begin AR_COMPONENT\n\
             \x20 \"sw-c\"\n\
             /end AR_COMPONENT\n"
        );
    }
}
