//! `A2LMEMORY_SEGMENT` / `A2LCALIBRATION_HANDLE` / `A2LCALIBRATION_METHOD` / `A2LUNIT` /
//! `A2LUSER_RIGHTS` / `A2LREF_GROUP`.

use indexmap::IndexMap;

use crate::block::Block;
use crate::error::{Error, Result};
use crate::model::base::ByteOrder;
use crate::model::enums::{
    A2lKeyword, DepositType, MemoryAttribute, MemoryPrgType, MemoryType, PrgType, UnitType,
};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::{escape_str, Writer};

fn to_hex(v: u32) -> String {
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

fn write_quoted_if_not_empty(w: &mut Writer, tag: &str, value: &Option<String>) {
    if let Some(v) = value {
        if !v.is_empty() {
            w.tag_value(Some(tag), Some(v), true);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Module {
    pub name: String,
    pub description: Option<String>,
    pub children: Vec<ModuleChild>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleChild {
    /// `/begin MOD_PAR`.
    ModPar(ModPar),
    /// `/begin MOD_COMMON`.
    ModCommon(ModCommon),
    /// `/begin MEASUREMENT`.
    Measurement(crate::model::measurement::Measurement),
    /// `/begin CHARACTERISTIC`.
    Characteristic(crate::model::characteristic::Characteristic),
    /// `/begin AXIS_PTS`.
    AxisPts(crate::model::characteristic::AxisPts),
    /// `/begin BLOB`.
    Blob(crate::model::measurement::Blob),
    /// `/begin INSTANCE`.
    Instance(crate::model::measurement::Instance),
    /// `/begin COMPU_METHOD`.
    CompuMethod(crate::model::compu::CompuMethod),
    /// `/begin COMPU_TAB`.
    CompuTab(crate::model::compu::CompuTab),
    /// `/begin COMPU_VTAB`.
    CompuVtab(crate::model::compu::CompuVtab),
    /// `/begin COMPU_VTAB_RANGE`.
    CompuVtabRange(crate::model::compu::CompuVtabRange),
    /// `/begin FORMULA`.
    Formula(crate::model::compu::Formula),
    /// `/begin RECORD_LAYOUT`.
    RecordLayout(crate::model::record_layout::RecordLayout),
    /// `/begin FUNCTION`.
    Function(crate::model::function::Function),
    /// `/begin GROUP`.
    Group(crate::model::function::Group),
    /// `/begin FRAME`.
    Frame(crate::model::function::Frame),
    /// `/begin UNIT`.
    Unit(Unit),
    /// `/begin USER_RIGHTS`.
    UserRights(UserRights),
    /// `/begin VARIANT_CODING`.
    VariantCoding(crate::model::variant::VariantCoding),
    /// `/begin TYPEDEF_AXIS`.
    TypedefAxis(crate::model::typedef::TypedefAxis),
    /// `/begin TYPEDEF_BLOB`.
    TypedefBlob(crate::model::typedef::TypedefBlob),
    /// `/begin TYPEDEF_CHARACTERISTIC`.
    TypedefCharacteristic(crate::model::typedef::TypedefCharacteristic),
    /// `/begin TYPEDEF_MEASUREMENT`.
    TypedefMeasurement(crate::model::typedef::TypedefMeasurement),
    /// `/begin TYPEDEF_STRUCTURE`.
    TypedefStructure(crate::model::typedef::TypedefStructure),
    Unsupported(UnsupportedNode),
}

impl ModuleChild {
    fn parse(block: &Block) -> Result<Self> {
        use crate::model::{
            characteristic as ch, compu, function as f, measurement as m, typedef as t, variant,
        };
        Ok(match block.keyword.as_str() {
            "MOD_PAR" => ModuleChild::ModPar(ModPar::parse(block)?),
            "MOD_COMMON" => ModuleChild::ModCommon(ModCommon::parse(block)?),
            "MEASUREMENT" => ModuleChild::Measurement(m::Measurement::parse(block)?),
            "CHARACTERISTIC" => ModuleChild::Characteristic(ch::Characteristic::parse(block)?),
            "AXIS_PTS" => ModuleChild::AxisPts(ch::AxisPts::parse(block)?),
            "BLOB" => ModuleChild::Blob(m::Blob::parse(block)?),
            "INSTANCE" => ModuleChild::Instance(m::Instance::parse(block)?),
            "COMPU_METHOD" => ModuleChild::CompuMethod(compu::CompuMethod::parse(block)?),
            "COMPU_TAB" => ModuleChild::CompuTab(compu::CompuTab::parse(block)?),
            "COMPU_VTAB" => ModuleChild::CompuVtab(compu::CompuVtab::parse(block)?),
            "COMPU_VTAB_RANGE" => ModuleChild::CompuVtabRange(compu::CompuVtabRange::parse(block)?),
            "FORMULA" => ModuleChild::Formula(compu::Formula::parse(block)?),
            "RECORD_LAYOUT" => {
                ModuleChild::RecordLayout(crate::model::record_layout::RecordLayout::parse(block)?)
            }
            "FUNCTION" => ModuleChild::Function(f::Function::parse(block)?),
            "GROUP" => ModuleChild::Group(f::Group::parse(block)?),
            "FRAME" => ModuleChild::Frame(f::Frame::parse(block)?),
            "UNIT" => ModuleChild::Unit(Unit::parse(block)?),
            "USER_RIGHTS" => ModuleChild::UserRights(UserRights::parse(block)?),
            "VARIANT_CODING" => ModuleChild::VariantCoding(variant::VariantCoding::parse(block)?),
            "TYPEDEF_AXIS" => ModuleChild::TypedefAxis(t::TypedefAxis::parse(block)?),
            "TYPEDEF_BLOB" => ModuleChild::TypedefBlob(t::TypedefBlob::parse(block)?),
            "TYPEDEF_CHARACTERISTIC" => {
                ModuleChild::TypedefCharacteristic(t::TypedefCharacteristic::parse(block)?)
            }
            "TYPEDEF_MEASUREMENT" => {
                ModuleChild::TypedefMeasurement(t::TypedefMeasurement::parse(block)?)
            }
            "TYPEDEF_STRUCTURE" => {
                ModuleChild::TypedefStructure(t::TypedefStructure::parse(block)?)
            }
            _ => ModuleChild::Unsupported(UnsupportedNode::from_block(block)),
        })
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            ModuleChild::ModPar(n) => n.write_block(w),
            ModuleChild::ModCommon(n) => n.write_block(w),
            ModuleChild::Measurement(n) => n.write_block(w),
            ModuleChild::Characteristic(n) => n.write_block(w),
            ModuleChild::AxisPts(n) => n.write_block(w),
            ModuleChild::Blob(n) => n.write_block(w),
            ModuleChild::Instance(n) => n.write_block(w),
            ModuleChild::CompuMethod(n) => n.write_block(w),
            ModuleChild::CompuTab(n) => n.write_block(w),
            ModuleChild::CompuVtab(n) => n.write_block(w),
            ModuleChild::CompuVtabRange(n) => n.write_block(w),
            ModuleChild::Formula(n) => n.write_block(w),
            ModuleChild::RecordLayout(n) => n.write_block(w),
            ModuleChild::Function(n) => n.write_block(w),
            ModuleChild::Group(n) => n.write_block(w),
            ModuleChild::Frame(n) => n.write_block(w),
            ModuleChild::Unit(n) => n.write_block(w),
            ModuleChild::UserRights(n) => n.write_block(w),
            ModuleChild::VariantCoding(n) => n.write_block(w),
            ModuleChild::TypedefAxis(n) => n.write_block(w),
            ModuleChild::TypedefBlob(n) => n.write_block(w),
            ModuleChild::TypedefCharacteristic(n) => n.write_block(w),
            ModuleChild::TypedefMeasurement(n) => n.write_block(w),
            ModuleChild::TypedefStructure(n) => n.write_block(w),
            ModuleChild::Unsupported(n) => n.write_block(w),
        }
    }
}

impl Node for Module {
    const KEYWORD: &'static str = "MODULE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let name = cur.ident()?;
        let description = if cur.is_empty() {
            None
        } else {
            Some(cur.string()?)
        };
        let children = block
            .children()
            .map(ModuleChild::parse)
            .collect::<Result<Vec<_>>>()?;
        Ok(Module {
            name,
            description,
            children,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        for child in &self.children {
            w.blank_line();
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.name, w)
    }
}

/// (BYTE=0, WORD=1, LONG=2, FLOAT32_IEEE=3, FLOAT64_IEEE=4, INT64=5, FLOAT16_IEEE=6;
const ALIGNMENT_TAGS: [&str; 7] = [
    "ALIGNMENT_BYTE",
    "ALIGNMENT_WORD",
    "ALIGNMENT_LONG",
    "ALIGNMENT_FLOAT32_IEEE",
    "ALIGNMENT_FLOAT64_IEEE",
    "ALIGNMENT_INT64",
    "ALIGNMENT_FLOAT16_IEEE",
];

const DEFAULT_ALIGNMENTS: [i32; 7] = [1, 2, 4, 4, 4, 8, 2];

fn write_alignments(w: &mut Writer, alignments: &[i32; 7]) {
    for (i, tag) in ALIGNMENT_TAGS.iter().enumerate() {
        w.value_line(Some(tag), &alignments[i].to_string());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModCommon {
    pub comment: Option<String>,
    pub byte_order: ByteOrder,
    pub deposit: DepositType,
    pub data_size: Option<i32>,
    pub alignments: [i32; 7],
}

impl Default for ModCommon {
    fn default() -> Self {
        ModCommon {
            comment: None,
            byte_order: ByteOrder::MSB_LAST,
            deposit: DepositType::ABSOLUTE,
            data_size: None,
            alignments: DEFAULT_ALIGNMENTS,
        }
    }
}

impl Node for ModCommon {
    const KEYWORD: &'static str = "MOD_COMMON";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut mc = ModCommon::default();
        if !cur.is_empty() {
            mc.comment = Some(cur.string()?);
        }
        while cur.remaining() >= 2 {
            if cur.take_if("BYTE_ORDER") {
                mc.byte_order = enum_value(&mut cur, "MOD_COMMON")?;
            } else if cur.take_if("DEPOSIT") {
                mc.deposit = enum_value(&mut cur, "MOD_COMMON")?;
            } else if cur.take_if("DATA_SIZE") {
                mc.data_size = Some(cur.int::<i32>()?);
            } else {
                let mut matched = false;
                for (i, tag) in ALIGNMENT_TAGS.iter().enumerate() {
                    if cur.take_if(tag) {
                        mc.alignments[i] = cur.int::<i32>()?;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    cur.next_token()?;
                }
            }
        }
        Ok(mc)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.comment.as_deref(), true);
        if let Some(kw) = self.byte_order.as_keyword() {
            w.value_line(Some("BYTE_ORDER"), kw);
        }
        if let Some(kw) = self.deposit.as_keyword() {
            w.value_line(Some("DEPOSIT"), kw);
        }
        if let Some(ds) = self.data_size {
            if ds > 0 {
                w.value_line(Some("DATA_SIZE"), &ds.to_string());
            }
        }
        write_alignments(w, &self.alignments);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModParChild {
    /// `/begin CALIBRATION_HANDLE`.
    CalibrationHandle(CalibrationHandle),
    /// `/begin CALIBRATION_METHOD`.
    CalibrationMethod(CalibrationMethod),
    /// `/begin MEMORY_LAYOUT`.
    MemoryLayout(MemoryLayout),
    /// `/begin MEMORY_SEGMENT`.
    MemorySegment(MemorySegment),
    Unsupported(UnsupportedNode),
}

impl ModParChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            ModParChild::CalibrationHandle(n) => n.write_block(w),
            ModParChild::CalibrationMethod(n) => n.write_block(w),
            ModParChild::MemoryLayout(n) => n.write_block(w),
            ModParChild::MemorySegment(n) => n.write_block(w),
            ModParChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModPar {
    pub description: Option<String>,
    /// `VERSION "..."`.
    pub version: Option<String>,
    /// `SUPPLIER "..."`.
    pub supplier: Option<String>,
    /// `CUSTOMER "..."`.
    pub customer: Option<String>,
    /// `CUSTOMER_NO "..."`.
    pub customer_no: Option<String>,
    /// `USER "..."`.
    pub user: Option<String>,
    /// `PHONE_NO "..."`.
    pub phone_no: Option<String>,
    /// `ECU "..."`.
    pub ecu: Option<String>,
    /// `CPU "..."`.
    pub cpu: Option<String>,
    /// `EPK "..."`.
    pub epk: Option<String>,
    pub epk_address: Option<u32>,
    pub no_of_interfaces: Option<i32>,
    pub ecu_calibration_offset: Option<u32>,
    pub system_constants: IndexMap<String, String>,
    pub children: Vec<ModParChild>,
}

impl Node for ModPar {
    const KEYWORD: &'static str = "MOD_PAR";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut mp = ModPar::default();
        if !cur.is_empty() {
            mp.description = Some(cur.string()?);
        }
        while cur.remaining() >= 2 {
            if cur.take_if("VERSION") {
                mp.version = Some(cur.string()?);
            } else if cur.take_if("SUPPLIER") {
                mp.supplier = Some(cur.string()?);
            } else if cur.take_if("CUSTOMER") {
                mp.customer = Some(cur.string()?);
            } else if cur.take_if("CUSTOMER_NO") {
                mp.customer_no = Some(cur.string()?);
            } else if cur.take_if("USER") {
                mp.user = Some(cur.string()?);
            } else if cur.take_if("PHONE_NO") {
                mp.phone_no = Some(cur.string()?);
            } else if cur.take_if("ECU") {
                mp.ecu = Some(cur.string()?);
            } else if cur.take_if("CPU") {
                mp.cpu = Some(cur.string()?);
            } else if cur.take_if("EPK") {
                mp.epk = Some(cur.string()?);
            } else if cur.take_if("ADDR_EPK") {
                mp.epk_address = Some(cur.uint::<u32>()?);
            } else if cur.take_if("NO_OF_INTERFACES") {
                mp.no_of_interfaces = Some(cur.int::<i32>()?);
            } else if cur.take_if("ECU_CALIBRATION_OFFSET") {
                mp.ecu_calibration_offset = Some(cur.uint::<u32>()?);
            } else if cur.take_if("SYSTEM_CONSTANT") {
                let name = cur.string()?;
                let value = cur.string()?;
                mp.system_constants.insert(name, value);
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            mp.children
                .push(match child.keyword.to_ascii_uppercase().as_str() {
                    "CALIBRATION_HANDLE" => {
                        ModParChild::CalibrationHandle(CalibrationHandle::parse(child)?)
                    }
                    "CALIBRATION_METHOD" => {
                        ModParChild::CalibrationMethod(CalibrationMethod::parse(child)?)
                    }
                    "MEMORY_LAYOUT" => ModParChild::MemoryLayout(MemoryLayout::parse(child)?),
                    "MEMORY_SEGMENT" => ModParChild::MemorySegment(MemorySegment::parse(child)?),
                    _ => ModParChild::Unsupported(UnsupportedNode::from_block(child)),
                });
        }
        Ok(mp)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        write_quoted_if_not_empty(w, "VERSION", &self.version);
        write_quoted_if_not_empty(w, "SUPPLIER", &self.supplier);
        write_quoted_if_not_empty(w, "CUSTOMER", &self.customer);
        write_quoted_if_not_empty(w, "CUSTOMER_NO", &self.customer_no);
        write_quoted_if_not_empty(w, "USER", &self.user);
        write_quoted_if_not_empty(w, "PHONE_NO", &self.phone_no);
        write_quoted_if_not_empty(w, "ECU", &self.ecu);
        write_quoted_if_not_empty(w, "CPU", &self.cpu);
        write_quoted_if_not_empty(w, "EPK", &self.epk);
        if let Some(addr) = self.epk_address {
            w.value_line(Some("ADDR_EPK"), &to_hex(addr));
        }
        if let Some(n) = self.no_of_interfaces {
            if n > -1 {
                w.value_line(Some("NO_OF_INTERFACES"), &n.to_string());
            }
        }
        if let Some(off) = self.ecu_calibration_offset {
            if off != 0 {
                w.value_line(Some("ECU_CALIBRATION_OFFSET"), &off.to_string());
            }
        }
        for (name, value) in &self.system_constants {
            w.value_line(
                None,
                &format!(
                    "SYSTEM_CONSTANT \"{}\" \"{}\"",
                    escape_str(name),
                    escape_str(value)
                ),
            );
        }
        for child in &self.children {
            w.blank_line();
            child.write_block(w)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    pub prg_type: PrgType,
    pub address: u32,
    pub size: u32,
    pub offsets: [i32; 5],
}

impl Default for MemoryLayout {
    fn default() -> Self {
        MemoryLayout {
            prg_type: PrgType::PRG_CODE,
            address: 0,
            size: 0,
            offsets: [-1; 5],
        }
    }
}

impl Node for MemoryLayout {
    const KEYWORD: &'static str = "MEMORY_LAYOUT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let prg_type = enum_value(&mut cur, "MEMORY_LAYOUT")?;
        let address = cur.uint::<u32>()?;
        let size = cur.uint::<u32>()?;
        let mut offsets = [-1; 5];
        for o in &mut offsets {
            *o = cur.int::<i32>()?;
        }
        Ok(MemoryLayout {
            prg_type,
            address,
            size,
            offsets,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let kw = self.prg_type.as_keyword().unwrap_or("PRG_CODE");
        let line = format!(
            "{} {} {} {}",
            kw,
            to_hex(self.address),
            to_hex(self.size),
            self.offsets
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        w.value_line(None, &line);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySegment {
    pub name: String,
    pub description: Option<String>,
    /// `MEMORYPRG_TYPE`.
    pub prg_type: MemoryPrgType,
    /// `MEMORY_TYPE`.
    pub memory_type: MemoryType,
    /// `MEMORY_ATTRIBUTE`.
    pub memory_attribute: MemoryAttribute,
    pub address: u32,
    pub size: u32,
    pub offsets: [i32; 5],
}

impl Default for MemorySegment {
    fn default() -> Self {
        MemorySegment {
            name: String::new(),
            description: None,
            prg_type: MemoryPrgType::DATA,
            memory_type: MemoryType::FLASH,
            memory_attribute: MemoryAttribute::INTERN,
            address: 0,
            size: 0,
            offsets: [-1; 5],
        }
    }
}

impl Node for MemorySegment {
    const KEYWORD: &'static str = "MEMORY_SEGMENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let name = cur.ident()?;
        let description = Some(cur.string()?);
        let prg_type = enum_value(&mut cur, "MEMORY_SEGMENT")?;
        let memory_type = enum_value(&mut cur, "MEMORY_SEGMENT")?;
        let memory_attribute = enum_value(&mut cur, "MEMORY_SEGMENT")?;
        let address = cur.uint::<u32>()?;
        let size = cur.uint::<u32>()?;
        let mut offsets = [-1; 5];
        for o in &mut offsets {
            *o = cur.int::<i32>()?;
        }
        Ok(MemorySegment {
            name,
            description,
            prg_type,
            memory_type,
            memory_attribute,
            address,
            size,
            offsets,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        let line = format!(
            "{} {} {} {} {} {}",
            self.prg_type.as_keyword().unwrap_or("DATA"),
            self.memory_type.as_keyword().unwrap_or("FLASH"),
            self.memory_attribute.as_keyword().unwrap_or("INTERN"),
            to_hex(self.address),
            to_hex(self.size),
            self.offsets
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        w.value_line(None, &line);
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.name, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalibrationHandle {
    pub text: Option<String>,
    pub handles: Vec<u32>,
}

impl Node for CalibrationHandle {
    const KEYWORD: &'static str = "CALIBRATION_HANDLE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut ch = CalibrationHandle::default();
        while !cur.is_empty() {
            if cur.take_if("CALIBRATION_HANDLE_TEXT") {
                ch.text = Some(cur.string()?);
            } else {
                ch.handles.push(cur.uint::<u32>()?);
            }
        }
        Ok(ch)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(text) = &self.text {
            if !text.is_empty() {
                w.tag_value(Some("CALIBRATION_HANDLE_TEXT"), Some(text), true);
            }
        }
        if !self.handles.is_empty() {
            let line = self
                .handles
                .iter()
                .map(|h| to_hex(*h))
                .collect::<Vec<_>>()
                .join(" ");
            w.value_line(None, &line);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalibrationMethod {
    pub method: String,
    pub version: i32,
}

impl Node for CalibrationMethod {
    const KEYWORD: &'static str = "CALIBRATION_METHOD";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let method = cur.string()?;
        let version = cur.int::<i32>()?;
        Ok(CalibrationMethod { method, version })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.method), true);
        w.value_line(None, &self.version.to_string());
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub name: String,
    pub description: Option<String>,
    pub display: Option<String>,
    pub unit_type: UnitType,
    pub ref_unit: Option<String>,
    pub unit_conversion: Option<[f64; 2]>,
    pub si_exponents: Option<[i32; 7]>,
}

impl Default for Unit {
    fn default() -> Self {
        Unit {
            name: String::new(),
            description: None,
            display: None,
            unit_type: UnitType::DERIVED,
            ref_unit: None,
            unit_conversion: None,
            si_exponents: None,
        }
    }
}

impl Node for Unit {
    const KEYWORD: &'static str = "UNIT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let name = cur.ident()?;
        let description = Some(cur.string()?);
        let display = Some(cur.string()?);
        let unit_type = enum_value(&mut cur, "UNIT")?;
        let mut unit = Unit {
            name,
            description,
            display,
            unit_type,
            ..Unit::default()
        };
        while !cur.is_empty() {
            if cur.take_if("REF_UNIT") {
                unit.ref_unit = Some(cur.ident()?);
            } else if cur.take_if("UNIT_CONVERSION") {
                unit.unit_conversion = Some([cur.float()?, cur.float()?]);
            } else if cur.take_if("SI_EXPONENTS") {
                let mut exps = [0; 7];
                for e in &mut exps {
                    *e = cur.int::<i32>()?;
                }
                unit.si_exponents = Some(exps);
            } else {
                cur.next_token()?;
            }
        }
        Ok(unit)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        w.tag_value(None, self.display.as_deref(), true);
        if let Some(kw) = self.unit_type.as_keyword() {
            w.value_line(None, kw);
        }
        if let Some(ru) = &self.ref_unit {
            if !ru.is_empty() {
                w.value_line(Some("REF_UNIT"), ru);
            }
        }
        if let Some([factor, offset]) = self.unit_conversion {
            w.value_line(
                Some("UNIT_CONVERSION"),
                &format!("{} {}", to_dec(factor), to_dec(offset)),
            );
        }
        if let Some(exps) = self.si_exponents {
            let line = exps
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            w.value_line(Some("SI_EXPONENTS"), &line);
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.name, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefGroup {
    pub references: Vec<String>,
}

impl Node for RefGroup {
    const KEYWORD: &'static str = "REF_GROUP";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut references = Vec::new();
        while !cur.is_empty() {
            references.push(cur.ident()?);
        }
        Ok(RefGroup { references })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.references(&self.references);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRightsChild {
    /// `/begin REF_GROUP`.
    RefGroup(RefGroup),
    Unsupported(UnsupportedNode),
}

impl UserRightsChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            UserRightsChild::RefGroup(n) => n.write_block(w),
            UserRightsChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserRights {
    pub name: String,
    pub read_only: bool,
    pub children: Vec<UserRightsChild>,
}

impl Node for UserRights {
    const KEYWORD: &'static str = "USER_RIGHTS";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let name = cur.ident()?;
        let mut read_only = false;
        while !cur.is_empty() {
            if cur.take_if("READ_ONLY") {
                read_only = true;
            } else {
                cur.next_token()?;
            }
        }
        let mut children = Vec::new();
        for child in block.children() {
            children.push(if child.keyword.eq_ignore_ascii_case("REF_GROUP") {
                UserRightsChild::RefGroup(RefGroup::parse(child)?)
            } else {
                UserRightsChild::Unsupported(UnsupportedNode::from_block(child))
            });
        }
        Ok(UserRights {
            name,
            read_only,
            children,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        if self.read_only {
            w.value_line(None, "READ_ONLY");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn roundtrip<T: Node>(src: &str, keyword: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child(keyword).unwrap();
        let node = T::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn mod_common_full() {
        let out = roundtrip::<ModCommon>(
            "/begin MOD_COMMON \"cmt\" BYTE_ORDER MSB_FIRST DEPOSIT DIFFERENCE DATA_SIZE 64 \
             ALIGNMENT_BYTE 1 ALIGNMENT_WORD 2 ALIGNMENT_LONG 4 ALIGNMENT_FLOAT32_IEEE 4 \
             ALIGNMENT_FLOAT64_IEEE 8 ALIGNMENT_INT64 8 ALIGNMENT_FLOAT16_IEEE 2 /end MOD_COMMON",
            "MOD_COMMON",
        );
        assert_eq!(
            out,
            "/begin MOD_COMMON\n  \"cmt\"\n  BYTE_ORDER MSB_FIRST\n  DEPOSIT DIFFERENCE\n  \
             DATA_SIZE 64\n  ALIGNMENT_BYTE 1\n  ALIGNMENT_WORD 2\n  ALIGNMENT_LONG 4\n  \
             ALIGNMENT_FLOAT32_IEEE 4\n  ALIGNMENT_FLOAT64_IEEE 8\n  ALIGNMENT_INT64 8\n  \
             ALIGNMENT_FLOAT16_IEEE 2\n/end MOD_COMMON\n"
        );
    }

    #[test]
    fn mod_common_defaults() {
        let out = roundtrip::<ModCommon>("/begin MOD_COMMON \"d\" /end MOD_COMMON", "MOD_COMMON");
        assert_eq!(
            out,
            "/begin MOD_COMMON\n  \"d\"\n  BYTE_ORDER MSB_LAST\n  DEPOSIT ABSOLUTE\n  \
             ALIGNMENT_BYTE 1\n  ALIGNMENT_WORD 2\n  ALIGNMENT_LONG 4\n  ALIGNMENT_FLOAT32_IEEE 4\n  \
             ALIGNMENT_FLOAT64_IEEE 4\n  ALIGNMENT_INT64 8\n  ALIGNMENT_FLOAT16_IEEE 2\n/end MOD_COMMON\n"
        );
    }

    #[test]
    fn mod_common_skips_unknown() {
        let out = roundtrip::<ModCommon>(
            "/begin MOD_COMMON \"d\" FOO 1 BYTE_ORDER MSB_LAST /end MOD_COMMON",
            "MOD_COMMON",
        );
        assert!(out.contains("BYTE_ORDER MSB_LAST"));
        assert!(!out.contains("FOO"));
    }

    #[test]
    fn mod_par_full() {
        let src = "/begin MOD_PAR \"mod desc\" VERSION \"1.0\" SUPPLIER \"sup\" CUSTOMER \"cust\" \
                   CUSTOMER_NO \"42\" USER \"user1\" PHONE_NO \"123\" ECU \"ecu1\" CPU \"cpu1\" \
                   EPK \"epk1\" ADDR_EPK 0x8000 NO_OF_INTERFACES 2 ECU_CALIBRATION_OFFSET 16 \
                   SYSTEM_CONSTANT \"a\" \"1\" SYSTEM_CONSTANT \"b\" \"2\" \
                   /begin MEMORY_LAYOUT PRG_CODE 0xC000 0x4000 -1 -1 -1 -1 -1 /end MEMORY_LAYOUT \
                   /begin MEMORY_SEGMENT Seg1 \"seg desc\" DATA FLASH INTERN 0x8000 0x1000 -1 -1 -1 -1 -1 /end MEMORY_SEGMENT \
                   /begin CALIBRATION_HANDLE 0x100 0x101 CALIBRATION_HANDLE_TEXT \"handle text\" /end CALIBRATION_HANDLE \
                   /begin CALIBRATION_METHOD \"InCircuit\" 1 /end CALIBRATION_METHOD \
                   /begin IF_DATA XCP_TEST /end IF_DATA \
                   /end MOD_PAR";
        let out = roundtrip::<ModPar>(src, "MOD_PAR");
        let expected = "/begin MOD_PAR\n  \"mod desc\"\n  VERSION \"1.0\"\n  SUPPLIER \"sup\"\n  \
            CUSTOMER \"cust\"\n  CUSTOMER_NO \"42\"\n  USER \"user1\"\n  PHONE_NO \"123\"\n  \
            ECU \"ecu1\"\n  CPU \"cpu1\"\n  EPK \"epk1\"\n  ADDR_EPK 0x8000\n  \
            NO_OF_INTERFACES 2\n  ECU_CALIBRATION_OFFSET 16\n  SYSTEM_CONSTANT \"a\" \"1\"\n  \
            SYSTEM_CONSTANT \"b\" \"2\"\n\n  \
            /begin MEMORY_LAYOUT\n    PRG_CODE 0xC000 0x4000 -1 -1 -1 -1 -1\n  /end MEMORY_LAYOUT\n\n  \
            /begin MEMORY_SEGMENT Seg1\n    \"seg desc\"\n    \
            DATA FLASH INTERN 0x8000 0x1000 -1 -1 -1 -1 -1\n  /end MEMORY_SEGMENT\n\n  \
            /begin CALIBRATION_HANDLE\n    CALIBRATION_HANDLE_TEXT \"handle text\"\n    \
            0x100 0x101\n  /end CALIBRATION_HANDLE\n\n  \
            /begin CALIBRATION_METHOD\n    \"InCircuit\"\n    1\n  /end CALIBRATION_METHOD\n\n  \
            /begin IF_DATA\n    XCP_TEST\n  /end IF_DATA\n/end MOD_PAR\n";
        assert_eq!(out, expected);
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let mp2 = ModPar::parse(root.child("MOD_PAR").unwrap()).unwrap();
        assert_eq!(mp2.system_constants.len(), 2);
        assert_eq!(mp2.children.len(), 5);
        assert_eq!(mp2.epk_address, Some(0x8000));
    }

    #[test]
    fn mod_par_epk_defaults() {
        let out = roundtrip::<ModPar>("/begin MOD_PAR \"d\" /end MOD_PAR", "MOD_PAR");
        assert_eq!(out, "/begin MOD_PAR\n  \"d\"\n/end MOD_PAR\n");
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let mp = ModPar::parse(root.child("MOD_PAR").unwrap()).unwrap();
        assert_eq!(mp.epk, None);
        assert_eq!(mp.epk_address, None);
    }

    #[test]
    fn mod_par_skips_unknown_flat() {
        let out = roundtrip::<ModPar>(
            "/begin MOD_PAR \"d\" FOO bar VERSION \"2\" /end MOD_PAR",
            "MOD_PAR",
        );
        assert_eq!(
            out,
            "/begin MOD_PAR\n  \"d\"\n  VERSION \"2\"\n/end MOD_PAR\n"
        );
    }

    #[test]
    fn memory_layout_roundtrip() {
        let out = roundtrip::<MemoryLayout>(
            "/begin MEMORY_LAYOUT PRG_DATA 0x8000 0x100 -1 -1 -1 -1 -1 /end MEMORY_LAYOUT",
            "MEMORY_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin MEMORY_LAYOUT\n  PRG_DATA 0x8000 0x100 -1 -1 -1 -1 -1\n/end MEMORY_LAYOUT\n"
        );
    }

    #[test]
    fn memory_segment_roundtrip() {
        let out = roundtrip::<MemorySegment>(
            "/begin MEMORY_SEGMENT Seg1 \"desc\" CODE ROM EXTERN 0xC000 0x2000 0 1 2 3 4 /end MEMORY_SEGMENT",
            "MEMORY_SEGMENT",
        );
        assert_eq!(
            out,
            "/begin MEMORY_SEGMENT Seg1\n  \"desc\"\n  CODE ROM EXTERN 0xC000 0x2000 0 1 2 3 4\n/end MEMORY_SEGMENT\n"
        );
    }

    #[test]
    fn calibration_handle_roundtrip() {
        let out = roundtrip::<CalibrationHandle>(
            "/begin CALIBRATION_HANDLE 0x1 0xA CALIBRATION_HANDLE_TEXT \"note\" /end CALIBRATION_HANDLE",
            "CALIBRATION_HANDLE",
        );
        assert_eq!(
            out,
            "/begin CALIBRATION_HANDLE\n  CALIBRATION_HANDLE_TEXT \"note\"\n  0x1 0xA\n/end CALIBRATION_HANDLE\n"
        );
    }

    #[test]
    fn calibration_handle_no_text() {
        let out = roundtrip::<CalibrationHandle>(
            "/begin CALIBRATION_HANDLE 0x10 /end CALIBRATION_HANDLE",
            "CALIBRATION_HANDLE",
        );
        assert_eq!(
            out,
            "/begin CALIBRATION_HANDLE\n  0x10\n/end CALIBRATION_HANDLE\n"
        );
    }

    #[test]
    fn calibration_method_roundtrip() {
        let out = roundtrip::<CalibrationMethod>(
            "/begin CALIBRATION_METHOD \"InCircuit\" 0x100 /end CALIBRATION_METHOD",
            "CALIBRATION_METHOD",
        );
        assert_eq!(
            out,
            "/begin CALIBRATION_METHOD\n  \"InCircuit\"\n  256\n/end CALIBRATION_METHOD\n"
        );
    }

    #[test]
    fn unit_roundtrip() {
        let out = roundtrip::<Unit>(
            "/begin UNIT Kelvin \"temperature\" \"K\" DERIVED REF_UNIT base \
             UNIT_CONVERSION 1.5 2 SI_EXPONENTS 1 0 0 0 0 0 0 /end UNIT",
            "UNIT",
        );
        assert_eq!(
            out,
            "/begin UNIT Kelvin\n  \"temperature\"\n  \"K\"\n  DERIVED\n  REF_UNIT base\n  \
             UNIT_CONVERSION 1.5 2\n  SI_EXPONENTS 1 0 0 0 0 0 0\n/end UNIT\n"
        );
    }

    #[test]
    fn unit_minimal() {
        let out = roundtrip::<Unit>("/begin UNIT U \"d\" \"%6.3\" EXTENDED_SI /end UNIT", "UNIT");
        assert_eq!(
            out,
            "/begin UNIT U\n  \"d\"\n  \"%6.3\"\n  EXTENDED_SI\n/end UNIT\n"
        );
    }

    #[test]
    fn user_rights_roundtrip() {
        let out = roundtrip::<UserRights>(
            "/begin USER_RIGHTS admin READ_ONLY /begin REF_GROUP g1 g2 g3 g4 /end REF_GROUP \
             /begin FOO x /end FOO /end USER_RIGHTS",
            "USER_RIGHTS",
        );
        assert_eq!(
            out,
            "/begin USER_RIGHTS admin\n  READ_ONLY\n  /begin REF_GROUP\n    g1 g2 g3\n    g4\n  \
             /end REF_GROUP\n  /begin FOO\n    x\n  /end FOO\n/end USER_RIGHTS\n"
        );
    }

    #[test]
    fn user_rights_writable() {
        let out = roundtrip::<UserRights>(
            "/begin USER_RIGHTS guest /begin REF_GROUP g1 /end REF_GROUP /end USER_RIGHTS",
            "USER_RIGHTS",
        );
        assert_eq!(
            out,
            "/begin USER_RIGHTS guest\n  /begin REF_GROUP\n    g1\n  /end REF_GROUP\n/end USER_RIGHTS\n"
        );
    }

    #[test]
    fn module_roundtrip() {
        let out = roundtrip::<Module>(
            "/begin MODULE Mod1 \"desc\" /begin MOD_COMMON \"c\" /end MOD_COMMON \
             /begin MEASUREMENT M \"d\" UBYTE NO_COMPU_METHOD 0 0 0 1 ECU_ADDRESS 0x0 /end MEASUREMENT \
             /end MODULE",
            "MODULE",
        );
        assert_eq!(
            out,
            "/begin MODULE Mod1\n  \"desc\"\n\n  /begin MOD_COMMON\n    \"c\"\n    \
             BYTE_ORDER MSB_LAST\n    DEPOSIT ABSOLUTE\n    \
             ALIGNMENT_BYTE 1\n    ALIGNMENT_WORD 2\n    ALIGNMENT_LONG 4\n    \
             ALIGNMENT_FLOAT32_IEEE 4\n    ALIGNMENT_FLOAT64_IEEE 4\n    ALIGNMENT_INT64 8\n    \
             ALIGNMENT_FLOAT16_IEEE 2\n  /end MOD_COMMON\n\n  \
             /begin MEASUREMENT M\n    \"d\"\n    UBYTE\n    NO_COMPU_METHOD\n    0 0\n    0 1\n    ECU_ADDRESS 0x0\n  \
             /end MEASUREMENT\n/end MODULE\n"
        );
    }

    #[test]
    fn mod_par_description_escapes() {
        let out = roundtrip::<ModPar>("/begin MOD_PAR \"a\\nb\" /end MOD_PAR", "MOD_PAR");
        assert!(out.contains("\"a\\nb\""));
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let mp = ModPar::parse(root.child("MOD_PAR").unwrap()).unwrap();
        assert_eq!(mp.description.as_deref(), Some("a\nb"));
    }
}
