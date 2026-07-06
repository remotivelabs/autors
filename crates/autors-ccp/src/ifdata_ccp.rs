//! Typed parsing of the A2L `IF_DATA ASAP1B_CCP` block of a CCP node:
//! the transport blob (`TP_BLOB`), DAQ list blobs (`QP_BLOB`), seed & key
//! (`SEED_KEY`), rasters, memory pages, and related sub-blocks.
//! Structure:
//! - [`CcpIfData`]: the typed content of `/begin IF_DATA ASAP1B_CCP ... /end IF_DATA`.
//!   Dispatch depends on the content token sequence: when it has the form
//!   `ASAP1B_CCP ADDRESS_MAPPING|DP_BLOB|KP_BLOB ...` (≥3 tokens), the whole
//!   IF_DATA is replaced by an inline NAMEDNODE ([`CcpNamedNode`]); otherwise the
//!   IF_DATA is named `ASAP1B_CCP` and its sub-blocks are dispatched by `A2LType`
//!   keyword ([`CcpNode`]).
//! - [`CcpNode`]: polymorphic enum of sub-blocks; unknown sub-blocks pass through
//!   verbatim via [`UnsupportedNode`].
//! - The NAMEDNODE form is flattened into [`CcpNamedNode`] (its `Name` is always
//!   `"ASAP1B_CCP " + SubType` and its `Type` is always `IF_DATA`; neither is
//!   stored explicitly).
//!
//! Text conventions: the protocol name is `ASAP1B_CCP`; hex values are written
//! uppercase with no leading zeros (`0x{0:X}`); booleans are written as `"1"`/`"0"`.
//!
//! Write-out conventions (all intentional):
//! - Indentation uses the `autors_a2l::writer::Writer` default (two spaces).
//! - `OptionalCmds` is a `BTreeSet<u8>` written in ascending order, so the
//!   output is deterministic.
//! - In the block form ([`CcpIfData::Nodes`]) the protocol name `ASAP1B_CCP` is
//!   written on the line following `/begin IF_DATA` (matching the pass-through
//!   layout of the core `UnsupportedNode`); the token sequence is equivalent.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use autors_a2l::block::{Block, Item};
use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::{A2lKeyword, ChecksumType, EcuPage};
use autors_a2l::model::unsupported::UnsupportedNode;
use autors_a2l::params::ParamCursor;
use autors_a2l::token::Token;
use autors_a2l::writer::Writer;

use crate::error::{Error, Result};
use autors_xcp::ifdata_xcp::XcpDaqListCanType;

/// Protocol name of the CCP IF_DATA block.
pub const PROTOCOL_CCP: &str = "ASAP1B_CCP";

// ============================================================================
// Helper functions
// ============================================================================

/// Formats a value as uppercase hex with no leading zeros (`0x{0:X}`).
fn to_hex<T: fmt::UpperHex>(v: T) -> String {
    format!("0x{v:X}")
}

fn kw_enum<T: A2lKeyword + Default>(s: &str) -> T {
    T::from_keyword(s).unwrap_or_default()
}

fn u8_val(cur: &mut ParamCursor) -> Result<u8> {
    Ok(cur.uint::<u64>()?.min(255) as u8)
}

fn u16_val(cur: &mut ParamCursor) -> Result<u16> {
    Ok(cur.uint::<u64>()?.min(65535) as u16)
}

fn u32_val(cur: &mut ParamCursor) -> Result<u32> {
    Ok(cur.uint::<u64>()? as u32)
}

fn raw_text(cur: &mut ParamCursor) -> Result<String> {
    Ok(cur.next_token()?.text.clone())
}

fn can_id_str(can_id: u32) -> String {
    let mut s = format!("{:X}", can_id & 0x1FFF_FFFF);
    if can_id & 0x8000_0000 != 0 {
        s.push('x');
    }
    s
}

fn byte_order_as_u8(bo: ByteOrder) -> u8 {
    bo as u8
}

fn byte_order_from_u8(v: u8) -> ByteOrder {
    match v {
        1 => ByteOrder::MSB_FIRST,
        2 => ByteOrder::MSB_LAST,
        _ => ByteOrder::NotSet,
    }
}

// ============================================================================
// ============================================================================

macro_rules! ccp_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            #[default]
            NotSet,
            $($variant),*
        }

        impl A2lKeyword for $name {
            fn as_keyword(self) -> Option<&'static str> {
                match self {
                    $name::NotSet => None,
                    $($name::$variant => Some(stringify!($variant))),*
                }
            }

            fn from_keyword(kw: &str) -> Option<Self> {
                $(if kw.eq_ignore_ascii_case(stringify!($variant)) {
                    return Some($name::$variant);
                })*
                None
            }
        }
    };
}

ccp_enum! {
    CcpAddressMode { DAQ, ODT }
}

ccp_enum! {
    CcpChecksumCalcType { ACTIVE_PAGE, BIT_OR_WITH_OPT_PAGE }
}

ccp_enum! {
    CcpDaqMode { ALTERNATING, BURST }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CcpScalingUnit(pub u16);

impl CcpScalingUnit {
    /// 1 µs.
    pub const _1US: Self = Self(0);
    /// 10 µs.
    pub const _10US: Self = Self(1);
    /// 100 µs.
    pub const _100US: Self = Self(2);
    /// 1 ms.
    pub const _1MS: Self = Self(3);
    /// 10 ms.
    pub const _10MS: Self = Self(4);
    /// 100 ms.
    pub const _100MS: Self = Self(5);
    /// 1 s.
    pub const _1S: Self = Self(6);
    /// 10 s.
    pub const _10S: Self = Self(7);
    /// 1 min.
    pub const _1MIN: Self = Self(8);
    /// 1 h.
    pub const _1HOUR: Self = Self(9);
    /// 1 day.
    pub const _1DAY: Self = Self(10);
    pub const ANGULAR_DEGREES: Self = Self(100);
    pub const REVOLUTIONS_360_DEGREES: Self = Self(101);
    pub const CYCLE_720_DEGREES: Self = Self(102);
    pub const CYLINDER_SEGMENT: Self = Self(103);
    pub const EVENT: Self = Self(998);
    pub const ON_VALUE_CHANGED: Self = Self(999);
    pub const NON_DETERMINISTIC: Self = Self(1000);

    pub fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "_1US",
            1 => "_10US",
            2 => "_100US",
            3 => "_1MS",
            4 => "_10MS",
            5 => "_100MS",
            6 => "_1S",
            7 => "_10S",
            8 => "_1MIN",
            9 => "_1HOUR",
            10 => "_1DAY",
            100 => "AngularDegrees",
            101 => "Revolutions360Degrees",
            102 => "Cycle720Degrees",
            103 => "CylinderSegment",
            998 => "Event",
            999 => "OnValueChanged",
            1000 => "NonDeterministic",
            _ => return None,
        })
    }

    fn to_string_cs(self) -> String {
        match self.name() {
            Some(n) => n.to_string(),
            None => self.0.to_string(),
        }
    }
}

pub fn get_cycle_time(unit: CcpScalingUnit, rate: u32) -> String {
    if rate == 0 {
        return "non cyclic".to_string();
    }
    let name = unit.to_string_cs();
    if unit.0 >= 100 {
        if rate <= 1 {
            return name;
        }
        return format!("{rate}*{name}");
    }
    let s = &name[1..];
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let letters: String = s.chars().skip_while(|c| c.is_ascii_digit()).collect();
    let num: u32 = digits.parse().unwrap_or(0);
    format!("{}{}", num * rate, letters.to_lowercase())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CcpMemoryPageType(pub u16);

impl CcpMemoryPageType {
    pub const NOT_SET: u16 = 0x0;
    /// RAM.
    pub const RAM: u16 = 0x1;
    /// ROM.
    pub const ROM: u16 = 0x2;
    /// FLASH.
    pub const FLASH: u16 = 0x4;
    /// EEPROM.
    pub const EEPROM: u16 = 0x8;
    pub const RAM_INIT_BY_ECU: u16 = 0x10;
    pub const RAM_INIT_BY_TOOL: u16 = 0x20;
    pub const AUTO_FLASH_BACK: u16 = 0x40;
    pub const FLASH_BACK: u16 = 0x80;
    pub const DEFAULT: u16 = 0x100;

    pub fn contains(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    const FLAGS: [(u16, &'static str); 9] = [
        (Self::RAM, "RAM"),
        (Self::ROM, "ROM"),
        (Self::FLASH, "FLASH"),
        (Self::EEPROM, "EEPROM"),
        (Self::RAM_INIT_BY_ECU, "RAM_INIT_BY_ECU"),
        (Self::RAM_INIT_BY_TOOL, "RAM_INIT_BY_TOOL"),
        (Self::AUTO_FLASH_BACK, "AUTO_FLASH_BACK"),
        (Self::FLASH_BACK, "FLASH_BACK"),
        (Self::DEFAULT, "DEFAULT"),
    ];

    fn from_flag_keyword(kw: &str) -> Self {
        for (bit, name) in Self::FLAGS {
            if kw.eq_ignore_ascii_case(name) {
                return Self(bit);
            }
        }
        Self::default()
    }
}

impl fmt::Display for CcpMemoryPageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("NotSet");
        }
        let mut names = Vec::new();
        let mut rest = self.0;
        for (bit, name) in Self::FLAGS {
            if self.contains(bit) {
                names.push(name);
                rest &= !bit;
            }
        }
        if rest != 0 {
            return write!(f, "{}", self.0);
        }
        f.write_str(&names.join(", "))
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CcpTpBlob {
    pub version: u16,
    pub blob_version: u16,
    pub can_id_cmd: u32,
    pub can_id_resp: u32,
    pub station_address: u16,
    pub byte_order: ByteOrder,
    pub daq_mode: CcpDaqMode,
    pub consistency: CcpAddressMode,
    pub address_ext: CcpAddressMode,
    pub optional_cmds: BTreeSet<u8>,
    pub baudrate: u32,
    pub children: Vec<CcpNode>,
}

impl CcpTpBlob {
    pub const KEYWORD: &'static str = "TP_BLOB";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut tp = CcpTpBlob {
            version: cur.uint::<u64>()? as u16,
            blob_version: cur.uint::<u64>()? as u16,
            can_id_cmd: u32_val(&mut cur)?,
            can_id_resp: u32_val(&mut cur)?,
            station_address: cur.uint::<u64>()? as u16,
            byte_order: byte_order_from_u8(u8_val(&mut cur)?),
            ..CcpTpBlob::default()
        };
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("DAQ_MODE") {
                tp.daq_mode = kw_enum(&cur.next_token()?.text);
            } else if t.text.eq_ignore_ascii_case("CONSISTENCY") {
                tp.consistency = kw_enum(&cur.next_token()?.text);
            } else if t.text.eq_ignore_ascii_case("ADDRESS_EXTENSION") {
                tp.address_ext = kw_enum(&cur.next_token()?.text);
            } else if t.text.eq_ignore_ascii_case("OPTIONAL_CMD") {
                tp.optional_cmds.insert(u8_val(&mut cur)?);
            } else if t.text.eq_ignore_ascii_case("BAUDRATE") {
                tp.baudrate = u32_val(&mut cur)?;
            }
        }
        for child in block.children() {
            tp.children.push(CcpNode::parse(child)?);
        }
        Ok(tp)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.version));
        w.value_line(None, &to_hex(self.blob_version));
        w.value_line(None, &to_hex(self.can_id_cmd));
        w.value_line(None, &to_hex(self.can_id_resp));
        w.value_line(None, &to_hex(self.station_address));
        w.value_line(None, &byte_order_as_u8(self.byte_order).to_string());
        if self.baudrate != 0 {
            w.value_line(Some("BAUDRATE"), &self.baudrate.to_string());
        }
        if let Some(kw) = self.daq_mode.as_keyword() {
            w.value_line(Some("DAQ_MODE"), kw);
        }
        if let Some(kw) = self.consistency.as_keyword() {
            w.value_line(Some("CONSISTENCY"), kw);
        }
        if let Some(kw) = self.address_ext.as_keyword() {
            w.value_line(Some("ADDRESS_EXTENSION"), kw);
        }
        for cmd in &self.optional_cmds {
            w.value_line(Some("OPTIONAL_CMD"), &to_hex(*cmd));
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        for child in &self.children {
            child.write_block(w)?;
        }
        w.end_block(Self::KEYWORD);
        Ok(())
    }

    pub fn get_pages(&self, page_type: CcpMemoryPageType) -> Vec<&CcpDefinedPages> {
        self.defined_pages()
            .filter(|p| (page_type.0 & p.page_type.0) != 0)
            .collect()
    }

    pub fn find_page(&self, address_ext: u8, address: u32) -> Option<&CcpDefinedPages> {
        self.defined_pages()
            .find(|p| p.address_ext == address_ext && p.contains(address))
    }

    pub fn find_cal_page(
        &self,
        address_ext: u8,
        address: u32,
        target_page: EcuPage,
    ) -> Option<(&CcpDefinedPages, u32)> {
        let mut page = self.find_page(address_ext, address)?;
        let offset = address.wrapping_sub(page.address);
        match target_page {
            EcuPage::Flash => {
                if (page.page_type.0 & (CcpMemoryPageType::ROM | CcpMemoryPageType::FLASH)) == 0 {
                    page = self.find_page_by_offset(offset, target_page)?;
                }
            }
            EcuPage::RAM => {
                if (page.page_type.0 & CcpMemoryPageType::RAM) == 0 {
                    page = self.find_page_by_offset(offset, target_page)?;
                }
            }
        }
        Some((page, offset))
    }

    pub fn compute_address(&self, address_ext: u8, address: u32, target_page: EcuPage) -> u32 {
        if address == u32::MAX {
            return address;
        }
        match self.find_cal_page(address_ext, address, target_page) {
            Some((page, offset)) => page.address.wrapping_add(offset),
            None => u32::MAX,
        }
    }

    fn find_page_by_offset(&self, offset: u32, target: EcuPage) -> Option<&CcpDefinedPages> {
        self.defined_pages().find(|p| {
            let in_range = p.contains(p.address.wrapping_add(offset));
            match target {
                EcuPage::Flash => (p.page_type.0 & CcpMemoryPageType::RAM) == 0 && in_range,
                EcuPage::RAM => (p.page_type.0 & CcpMemoryPageType::RAM) != 0 && in_range,
            }
        })
    }

    fn defined_pages(&self) -> impl Iterator<Item = &CcpDefinedPages> {
        self.children.iter().filter_map(|n| match n {
            CcpNode::DefinedPages(p) => Some(p),
            _ => None,
        })
    }
}

impl fmt::Display for CcpTpBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let baud = if self.baudrate != 0 {
            format!(", {} Bit/s", self.baudrate)
        } else {
            String::new()
        };
        write!(
            f,
            "CCP Station {:04X}, IDs[{}/{}]{}",
            self.station_address,
            can_id_str(self.can_id_cmd),
            can_id_str(self.can_id_resp),
            baud
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpCanParam {
    pub frequency: u16,
    pub btr0: u8,
    pub btr1: u8,
}

impl CcpCanParam {
    pub const KEYWORD: &'static str = "CAN_PARAM";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let p = CcpCanParam {
            frequency: u16_val(&mut cur)?,
            btr0: u8_val(&mut cur)?,
            btr1: u8_val(&mut cur)?,
        };
        Ok(p)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(
            None,
            &format!(
                "{} {} {}",
                to_hex(self.frequency),
                to_hex(self.btr0),
                to_hex(self.btr1)
            ),
        );
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpChecksum {
    pub dll: Option<String>,
}

impl CcpChecksum {
    pub const KEYWORD: &'static str = "CHECKSUM";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let dll = if cur.is_empty() {
            None
        } else {
            Some(raw_text(&mut cur)?)
        };
        Ok(CcpChecksum { dll })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(dll) = &self.dll {
            w.tag_value(None, Some(dll), true);
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpChecksumParam {
    pub procedure: u16,
    pub limit: u32,
    pub calc_type: CcpChecksumCalcType,
}

impl CcpChecksumParam {
    pub const KEYWORD: &'static str = "CHECKSUM_PARAM";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut p = CcpChecksumParam {
            procedure: cur.uint::<u64>()? as u16,
            limit: u32_val(&mut cur)?,
            ..CcpChecksumParam::default()
        };
        if cur.remaining() > 1 {
            let _tag = cur.next_token()?;
            p.calc_type = kw_enum(&cur.next_token()?.text);
        }
        Ok(p)
    }

    pub fn checksum_type(&self) -> ChecksumType {
        if self.procedure == 0x9001 {
            ChecksumType::CRC_16_CITT
        } else {
            ChecksumType::ADD_12
        }
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.procedure));
        w.value_line(None, &to_hex(self.limit));
        if let Some(kw) = self.calc_type.as_keyword() {
            w.value_line(Some("CHECKSUM_CALCULATION"), kw);
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpDefinedPages {
    pub no: u16,
    pub page_name: String,
    pub address_ext: u8,
    pub address: u32,
    pub length: u32,
    pub page_type: CcpMemoryPageType,
}

impl CcpDefinedPages {
    pub const KEYWORD: &'static str = "DEFINED_PAGES";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut p = CcpDefinedPages {
            no: u16_val(&mut cur)?,
            page_name: raw_text(&mut cur)?,
            address_ext: u8_val(&mut cur)?,
            address: u32_val(&mut cur)?,
            length: u32_val(&mut cur)?,
            ..CcpDefinedPages::default()
        };
        while !cur.is_empty() {
            let t = cur.next_token()?;
            p.page_type.0 |= CcpMemoryPageType::from_flag_keyword(&t.text).0;
        }
        Ok(p)
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.address && address < self.address.wrapping_add(self.length)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.no));
        w.tag_value(None, Some(&self.page_name), true);
        w.value_line(None, &to_hex(self.address_ext));
        w.value_line(None, &to_hex(self.address));
        w.value_line(None, &to_hex(self.length));
        for (bit, name) in CcpMemoryPageType::FLAGS {
            if self.page_type.contains(bit) {
                w.value_line(None, name);
            }
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

impl fmt::Display for CcpDefinedPages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {:08X}({:08X})",
            self.page_type, self.address, self.length
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpEventGroup {
    pub name_long: String,
    pub name_short: String,
}

impl CcpEventGroup {
    pub const KEYWORD: &'static str = "EVENT_GROUP";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(CcpEventGroup {
            name_long: raw_text(&mut cur)?,
            name_short: raw_text(&mut cur)?,
        })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.name_long), true);
        w.tag_value(None, Some(&self.name_short), true);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcpQpBlob {
    pub daq_no: u16,
    pub length: u16,
    pub first_pid: u8,
    pub raster: u8,
    /// CAN ID(`CAN_ID_VARIABLE`/`CAN_ID_FIXED`,hex).
    pub can_id: u32,
    pub daq_list_type: XcpDaqListCanType,
}

impl Default for CcpQpBlob {
    fn default() -> Self {
        CcpQpBlob {
            daq_no: 0,
            length: 0,
            first_pid: 0xFF,
            raster: 0,
            can_id: 0,
            daq_list_type: XcpDaqListCanType::default(),
        }
    }
}

impl CcpQpBlob {
    pub const KEYWORD: &'static str = "QP_BLOB";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut q = CcpQpBlob {
            daq_no: cur.uint::<u64>()? as u16,
            ..CcpQpBlob::default()
        };
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("LENGTH") {
                q.length = cur.uint::<u64>()? as u16;
            } else if t.text.eq_ignore_ascii_case("RASTER") {
                q.raster = u8_val(&mut cur)?;
            } else if t.text.eq_ignore_ascii_case("CAN_ID_VARIABLE") {
                q.daq_list_type = XcpDaqListCanType::VARIABLE;
                q.can_id = u32_val(&mut cur)?;
            } else if t.text.eq_ignore_ascii_case("CAN_ID_FIXED") {
                q.daq_list_type = XcpDaqListCanType::FIXED;
                q.can_id = u32_val(&mut cur)?;
            } else if t.text.eq_ignore_ascii_case("FIRST_PID") {
                q.first_pid = u8_val(&mut cur)?;
            }
        }
        Ok(q)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &self.daq_no.to_string());
        w.value_line(Some("LENGTH"), &self.length.to_string());
        w.value_line(Some("RASTER"), &self.raster.to_string());
        match self.daq_list_type {
            XcpDaqListCanType::VARIABLE => {
                w.value_line(Some("CAN_ID_VARIABLE"), &to_hex(self.can_id))
            }
            XcpDaqListCanType::FIXED => w.value_line(Some("CAN_ID_FIXED"), &to_hex(self.can_id)),
            _ => {}
        }
        if self.first_pid < 0xFF {
            w.value_line(Some("FIRST_PID"), &to_hex(self.first_pid));
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpRaster {
    pub name_long: String,
    pub name_short: String,
    pub evt_chn_no: u8,
    pub scaling_unit: CcpScalingUnit,
    pub rate: u32,
}

impl CcpRaster {
    pub const KEYWORD: &'static str = "RASTER";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(CcpRaster {
            name_long: raw_text(&mut cur)?,
            name_short: raw_text(&mut cur)?,
            evt_chn_no: u8_val(&mut cur)?,
            scaling_unit: CcpScalingUnit(cur.uint::<u64>()? as u16),
            rate: u32_val(&mut cur)?,
        })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.name_long), true);
        w.tag_value(None, Some(&self.name_short), true);
        w.value_line(None, &to_hex(self.evt_chn_no));
        w.value_line(None, &self.scaling_unit.0.to_string());
        w.value_line(None, &self.rate.to_string());
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpSeedKey {
    pub cal_dll: Option<String>,
    pub daq_dll: Option<String>,
    pub pgm_dll: Option<String>,
}

impl CcpSeedKey {
    pub const KEYWORD: &'static str = "SEED_KEY";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut k = CcpSeedKey::default();
        if !cur.is_empty() {
            k.cal_dll = Some(raw_text(&mut cur)?);
        }
        if !cur.is_empty() {
            k.daq_dll = Some(raw_text(&mut cur)?);
        }
        if !cur.is_empty() {
            k.pgm_dll = Some(raw_text(&mut cur)?);
        }
        Ok(k)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        if let Some(dll) = &self.cal_dll {
            w.tag_value(None, Some(dll), true);
        }
        if let Some(dll) = &self.daq_dll {
            w.tag_value(None, Some(dll), true);
        }
        if let Some(dll) = &self.pgm_dll {
            w.tag_value(None, Some(dll), true);
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcpSource {
    pub name_long: String,
    pub scaling_unit: CcpScalingUnit,
    pub rate: u32,
    pub active: bool,
    pub qp_blob: Option<CcpQpBlob>,
}

impl Default for CcpSource {
    fn default() -> Self {
        CcpSource {
            name_long: String::new(),
            scaling_unit: CcpScalingUnit::default(),
            rate: 0,
            active: true,
            qp_blob: None,
        }
    }
}

impl CcpSource {
    pub const KEYWORD: &'static str = "SOURCE";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut s = CcpSource {
            name_long: raw_text(&mut cur)?,
            scaling_unit: CcpScalingUnit(cur.uint::<u64>()? as u16),
            rate: u32_val(&mut cur)?,
            ..CcpSource::default()
        };
        if let Some(qp) = block.child(CcpQpBlob::KEYWORD) {
            s.qp_blob = Some(CcpQpBlob::parse(qp)?);
        }
        Ok(s)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, Some(&self.name_long), true);
        w.value_line(None, &self.scaling_unit.0.to_string());
        w.value_line(None, &self.rate.to_string());
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        if let Some(qp) = &self.qp_blob {
            qp.write_block(w)?;
        }
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpAddressMapping {
    pub base_address: u32,
    pub map_address: u32,
    pub length: u32,
}

impl CcpAddressMapping {
    pub const KEYWORD: &'static str = "ADDRESS_MAPPING";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        Ok(CcpAddressMapping {
            base_address: u32_val(&mut cur)?,
            map_address: u32_val(&mut cur)?,
            length: u32_val(&mut cur)?,
        })
    }

    fn inline_params(&self) -> String {
        format!(
            " {} {} {} ",
            to_hex(self.base_address),
            to_hex(self.map_address),
            to_hex(self.length)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpDpBlob {
    pub address_ext: u16,
    pub address: u32,
    pub size: u32,
}

impl CcpDpBlob {
    pub const KEYWORD: &'static str = "DP_BLOB";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        Ok(CcpDpBlob {
            address_ext: cur.uint::<u64>()? as u16,
            address: u32_val(&mut cur)?,
            size: u32_val(&mut cur)?,
        })
    }

    fn inline_params(&self) -> String {
        format!(
            " {} {} {} ",
            to_hex(self.address_ext),
            to_hex(self.address),
            to_hex(self.size)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcpKpBlob {
    pub address_ext: u16,
    pub address: u32,
    pub size: u32,
    pub rasters: Vec<u16>,
}

impl CcpKpBlob {
    pub const KEYWORD: &'static str = "KP_BLOB";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        let mut k = CcpKpBlob {
            address_ext: cur.uint::<u64>()? as u16,
            address: u32_val(&mut cur)?,
            size: u32_val(&mut cur)?,
            ..CcpKpBlob::default()
        };
        let mut seen = BTreeSet::new();
        while cur.remaining() >= 2 {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("RASTER") {
                let v = cur.uint::<u64>()? as u16;
                if seen.insert(v) {
                    k.rasters.push(v);
                }
            }
        }
        Ok(k)
    }

    fn inline_params(&self) -> String {
        let mut s = format!(
            " {} {} {}",
            to_hex(self.address_ext),
            to_hex(self.address),
            self.size
        );
        for r in &self.rasters {
            s.push_str(&format!(" RASTER {r}"));
        }
        s.push(' ');
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcpNamedNode {
    /// `ADDRESS_MAPPING`.
    AddressMapping(CcpAddressMapping),
    /// `DP_BLOB`.
    DpBlob(CcpDpBlob),
    /// `KP_BLOB`.
    KpBlob(CcpKpBlob),
}

impl CcpNamedNode {
    pub fn sub_type_keyword(&self) -> &'static str {
        match self {
            CcpNamedNode::AddressMapping(_) => CcpAddressMapping::KEYWORD,
            CcpNamedNode::DpBlob(_) => CcpDpBlob::KEYWORD,
            CcpNamedNode::KpBlob(_) => CcpKpBlob::KEYWORD,
        }
    }

    pub fn name(&self) -> String {
        format!("{PROTOCOL_CCP} {}", self.sub_type_keyword())
    }

    pub fn write_inline(&self, w: &mut Writer) -> Result<()> {
        let params = match self {
            CcpNamedNode::AddressMapping(n) => n.inline_params(),
            CcpNamedNode::DpBlob(n) => n.inline_params(),
            CcpNamedNode::KpBlob(n) => n.inline_params(),
        };
        w.value_line(
            None,
            &format!(
                "/begin IF_DATA {} {}{}/end IF_DATA",
                PROTOCOL_CCP,
                self.sub_type_keyword(),
                params
            ),
        );
        Ok(())
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum CcpNode {
    /// `TP_BLOB`.
    TpBlob(CcpTpBlob),
    /// `DEFINED_PAGES`.
    DefinedPages(CcpDefinedPages),
    /// `CAN_PARAM`.
    CanParam(CcpCanParam),
    /// `CHECKSUM`.
    Checksum(CcpChecksum),
    /// `CHECKSUM_PARAM`.
    ChecksumParam(CcpChecksumParam),
    /// `SEED_KEY`.
    SeedKey(CcpSeedKey),
    /// `SOURCE`.
    Source(CcpSource),
    /// `RASTER`.
    Raster(CcpRaster),
    /// `EVENT_GROUP`.
    EventGroup(CcpEventGroup),
    QpBlob(CcpQpBlob),
    Unsupported(UnsupportedNode),
}

impl CcpNode {
    pub fn parse(block: &Block) -> Result<Self> {
        Ok(match block.keyword.to_ascii_uppercase().as_str() {
            CcpTpBlob::KEYWORD => CcpNode::TpBlob(CcpTpBlob::parse(block)?),
            CcpDefinedPages::KEYWORD => CcpNode::DefinedPages(CcpDefinedPages::parse(block)?),
            CcpCanParam::KEYWORD => CcpNode::CanParam(CcpCanParam::parse(block)?),
            CcpChecksum::KEYWORD => CcpNode::Checksum(CcpChecksum::parse(block)?),
            CcpChecksumParam::KEYWORD => CcpNode::ChecksumParam(CcpChecksumParam::parse(block)?),
            CcpSeedKey::KEYWORD => CcpNode::SeedKey(CcpSeedKey::parse(block)?),
            CcpSource::KEYWORD => CcpNode::Source(CcpSource::parse(block)?),
            CcpRaster::KEYWORD => CcpNode::Raster(CcpRaster::parse(block)?),
            CcpEventGroup::KEYWORD => CcpNode::EventGroup(CcpEventGroup::parse(block)?),
            CcpQpBlob::KEYWORD => CcpNode::QpBlob(CcpQpBlob::parse(block)?),
            _ => CcpNode::Unsupported(UnsupportedNode::from_block(block)),
        })
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            CcpNode::TpBlob(n) => n.write_block(w),
            CcpNode::DefinedPages(n) => n.write_block(w),
            CcpNode::CanParam(n) => n.write_block(w),
            CcpNode::Checksum(n) => n.write_block(w),
            CcpNode::ChecksumParam(n) => n.write_block(w),
            CcpNode::SeedKey(n) => n.write_block(w),
            CcpNode::Source(n) => n.write_block(w),
            CcpNode::Raster(n) => n.write_block(w),
            CcpNode::EventGroup(n) => n.write_block(w),
            CcpNode::QpBlob(n) => n.write_block(w),
            CcpNode::Unsupported(n) => Ok(n.write_block(w)?),
        }
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum CcpIfData {
    Named(CcpNamedNode),
    Nodes {
        params: Vec<String>,
        nodes: Vec<CcpNode>,
    },
    Unsupported(UnsupportedNode),
}

impl CcpIfData {
    pub fn parse(block: &Block) -> Result<Self> {
        if !block.keyword.eq_ignore_ascii_case("IF_DATA") {
            return Err(Error::Parse(format!(
                "expected IF_DATA block, got {}",
                block.keyword
            )));
        }
        let params: Vec<&Token> = block.params().collect();
        if params.len() > 2 && params[0].text == PROTOCOL_CCP {
            let toks = &params[2..];
            return Ok(match params[1].text.as_str() {
                CcpAddressMapping::KEYWORD => CcpIfData::Named(CcpNamedNode::AddressMapping(
                    CcpAddressMapping::parse_tokens(toks)?,
                )),
                CcpDpBlob::KEYWORD => {
                    CcpIfData::Named(CcpNamedNode::DpBlob(CcpDpBlob::parse_tokens(toks)?))
                }
                CcpKpBlob::KEYWORD => {
                    CcpIfData::Named(CcpNamedNode::KpBlob(CcpKpBlob::parse_tokens(toks)?))
                }
                _ => Self::parse_nodes(block, &params)?,
            });
        }
        if params.first().map(|t| t.text.as_str()) == Some(PROTOCOL_CCP) {
            return Self::parse_nodes(block, &params);
        }
        Ok(CcpIfData::Unsupported(UnsupportedNode::from_block(block)))
    }

    fn parse_nodes(block: &Block, params: &[&Token]) -> Result<Self> {
        let mut nodes = Vec::new();
        for child in block.children() {
            nodes.push(CcpNode::parse(child)?);
        }
        Ok(CcpIfData::Nodes {
            params: params.iter().map(|t| t.text.clone()).collect(),
            nodes,
        })
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            CcpIfData::Named(n) => n.write_inline(w),
            CcpIfData::Nodes { params, nodes } => {
                w.begin_block("IF_DATA");
                if !params.is_empty() {
                    w.value_line(None, &params.join(" "));
                }
                for node in nodes {
                    node.write_block(w)?;
                }
                w.end_block("IF_DATA");
                Ok(())
            }
            CcpIfData::Unsupported(n) => Ok(n.write_block(w)?),
        }
    }

    pub fn nodes(&self) -> &[CcpNode] {
        match self {
            CcpIfData::Nodes { nodes, .. } => nodes,
            _ => &[],
        }
    }

    pub fn build_source_raster_map(&self) -> BTreeMap<u16, SourceAndRaster> {
        let mut map = BTreeMap::new();
        let sources: Vec<&CcpSource> = self
            .nodes()
            .iter()
            .filter_map(|n| match n {
                CcpNode::Source(s) => Some(s),
                _ => None,
            })
            .collect();
        let rasters: Vec<&CcpRaster> = self
            .nodes()
            .iter()
            .filter_map(|n| match n {
                CcpNode::Raster(r) => Some(r),
                _ => None,
            })
            .collect();
        for src in sources {
            let Some(qp) = &src.qp_blob else { continue };
            let matched = rasters.iter().find(|r| r.evt_chn_no == qp.raster);
            let raster = match matched {
                Some(r) => Some(*r),
                None if !rasters.is_empty() => {
                    Some(rasters[(qp.daq_no as usize).min(rasters.len() - 1)])
                }
                None => None,
            };
            if let Some(r) = raster {
                map.insert(
                    qp.daq_no,
                    SourceAndRaster {
                        source: src.clone(),
                        raster: r.clone(),
                    },
                );
            }
        }
        map
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAndRaster {
    pub source: CcpSource,
    pub raster: CcpRaster,
}

fn tokens_block(toks: &[&Token]) -> Block {
    Block {
        keyword: "IF_DATA".to_string(),
        line: toks.first().map_or(0, |t| t.line),
        items: toks.iter().map(|t| Item::Param((*t).clone())).collect(),
    }
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::block::build_block_tree;
    use autors_a2l::token::tokenize;
    use autors_a2l::writer::WriterOptions;

    fn first_block(src: &str) -> Block {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let b = root.children().next().unwrap().clone();
        b
    }

    fn node_roundtrip(kw_check: &str, src: &str) -> String {
        let block = first_block(src);
        assert_eq!(block.keyword, kw_check);
        let node = CcpNode::parse(&block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    fn ifdata_roundtrip(src: &str) -> String {
        let block = first_block(src);
        let ifd = CcpIfData::parse(&block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        ifd.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn scaling_unit_get_cycle_time() {
        assert_eq!(get_cycle_time(CcpScalingUnit::_1MS, 0), "non cyclic");
        assert_eq!(get_cycle_time(CcpScalingUnit::_1MS, 1), "1ms");
        assert_eq!(get_cycle_time(CcpScalingUnit::_1MS, 10), "10ms");
        assert_eq!(get_cycle_time(CcpScalingUnit::_10US, 1), "10us");
        assert_eq!(get_cycle_time(CcpScalingUnit::_10US, 10), "100us");
        assert_eq!(
            get_cycle_time(CcpScalingUnit::ANGULAR_DEGREES, 1),
            "AngularDegrees"
        );
        assert_eq!(
            get_cycle_time(CcpScalingUnit::ANGULAR_DEGREES, 10),
            "10*AngularDegrees"
        );
        assert_eq!(get_cycle_time(CcpScalingUnit::EVENT, 1), "Event");
        assert_eq!(get_cycle_time(CcpScalingUnit::EVENT, 10), "10*Event");
        assert_eq!(get_cycle_time(CcpScalingUnit::_1DAY, 2), "2day");
    }

    #[test]
    fn memory_page_type_flags() {
        let pt = CcpMemoryPageType(CcpMemoryPageType::RAM | CcpMemoryPageType::FLASH);
        assert_eq!(pt.to_string(), "RAM, FLASH");
        assert_eq!(CcpMemoryPageType(0).to_string(), "NotSet");
        assert!(pt.contains(CcpMemoryPageType::RAM));
        assert!(!pt.contains(CcpMemoryPageType::EEPROM));
        assert_eq!(
            CcpMemoryPageType::from_flag_keyword("flash").0,
            CcpMemoryPageType::FLASH
        );
        assert_eq!(CcpMemoryPageType::from_flag_keyword("junk").0, 0);
    }

    #[test]
    fn enum_keywords() {
        assert_eq!(CcpDaqMode::ALTERNATING.as_keyword(), Some("ALTERNATING"));
        assert_eq!(
            CcpAddressMode::from_keyword("odt"),
            Some(CcpAddressMode::ODT)
        );
        assert_eq!(
            CcpChecksumCalcType::from_keyword("BIT_OR_WITH_OPT_PAGE"),
            Some(CcpChecksumCalcType::BIT_OR_WITH_OPT_PAGE)
        );
        assert_eq!(CcpDaqMode::NotSet.as_keyword(), None);
    }

    #[test]
    fn tp_blob_full_roundtrip() {
        let src = "/begin TP_BLOB 0x1 0x2 0x100 0x101 0x5 2 \
                   BAUDRATE 500000 DAQ_MODE ALTERNATING CONSISTENCY DAQ \
                   ADDRESS_EXTENSION ODT OPTIONAL_CMD 0x9 OPTIONAL_CMD 0xB /end TP_BLOB";
        let out = node_roundtrip("TP_BLOB", src);
        assert_eq!(
            out,
            "/begin TP_BLOB\n  0x1\n  0x2\n  0x100\n  0x101\n  0x5\n  2\n  BAUDRATE 500000\n  DAQ_MODE ALTERNATING\n  CONSISTENCY DAQ\n  ADDRESS_EXTENSION ODT\n  OPTIONAL_CMD 0x9\n  OPTIONAL_CMD 0xB\n/end TP_BLOB\n"
        );
    }

    #[test]
    fn tp_blob_minimal_and_display() {
        let src = "/begin TP_BLOB 0x11 0x22 0x7E0 0x7E8 0x0 1 /end TP_BLOB";
        let block = first_block(src);
        let tp = match CcpNode::parse(&block).unwrap() {
            CcpNode::TpBlob(tp) => tp,
            n => panic!("unexpected {n:?}"),
        };
        assert_eq!(tp.byte_order, ByteOrder::MSB_FIRST);
        assert!(tp.optional_cmds.is_empty());
        assert_eq!(tp.to_string(), "CCP Station 0000, IDs[7E0/7E8]");
        let tp2 = CcpTpBlob {
            station_address: 5,
            can_id_cmd: 0x100,
            can_id_resp: 0x101,
            baudrate: 500000,
            ..CcpTpBlob::default()
        };
        assert_eq!(
            tp2.to_string(),
            "CCP Station 0005, IDs[100/101], 500000 Bit/s"
        );
        let mut w = Writer::new(WriterOptions::default());
        tp.write_block(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin TP_BLOB\n  0x11\n  0x22\n  0x7E0\n  0x7E8\n  0x0\n  1\n/end TP_BLOB\n"
        );
    }

    #[test]
    fn tp_blob_pages_helpers() {
        let src = "/begin TP_BLOB 0x1 0x2 0x100 0x101 0x5 2 \
                   /begin DEFINED_PAGES 0x1 \"Cal\" 0x3 0x80000000 0x1000 RAM /end DEFINED_PAGES \
                   /begin DEFINED_PAGES 0x2 \"Ref\" 0x3 0x90000000 0x1000 ROM FLASH /end DEFINED_PAGES \
                   /end TP_BLOB";
        let block = first_block(src);
        let tp = match CcpNode::parse(&block).unwrap() {
            CcpNode::TpBlob(tp) => tp,
            n => panic!("unexpected {n:?}"),
        };
        assert_eq!(tp.children.len(), 2);
        assert_eq!(
            tp.get_pages(CcpMemoryPageType(CcpMemoryPageType::FLASH))
                .len(),
            1
        );
        assert_eq!(
            tp.get_pages(CcpMemoryPageType(CcpMemoryPageType::RAM))
                .len(),
            1
        );
        let (page, offset) = tp
            .find_cal_page(3, 0x8000_0100, EcuPage::Flash)
            .expect("cal page");
        assert_eq!(page.page_name, "Ref");
        assert_eq!(offset, 0x100);
        assert_eq!(
            tp.compute_address(3, 0x8000_0100, EcuPage::Flash),
            0x9000_0100
        );
        assert_eq!(tp.compute_address(3, u32::MAX, EcuPage::Flash), u32::MAX);
        assert_eq!(tp.compute_address(9, 0x1234, EcuPage::Flash), u32::MAX);
        let mut w = Writer::new(WriterOptions::default());
        tp.write_block(&mut w).unwrap();
        let out = w.into_string();
        assert!(out.contains("/begin DEFINED_PAGES\n    0x1\n    \"Cal\"\n    0x3\n    0x80000000\n    0x1000\n    RAM\n  /end DEFINED_PAGES\n"));
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let tp2 = CcpTpBlob::parse(root.child("TP_BLOB").unwrap()).unwrap();
        assert_eq!(tp, tp2);
    }

    #[test]
    fn can_param_roundtrip() {
        let out = node_roundtrip("CAN_PARAM", "/begin CAN_PARAM 0x10 0x1 0x2 /end CAN_PARAM");
        assert_eq!(out, "/begin CAN_PARAM\n  0x10 0x1 0x2\n/end CAN_PARAM\n");
        let block = first_block("/begin CAN_PARAM 70000 300 0x1FF /end CAN_PARAM");
        let p = CcpCanParam::parse(&block).unwrap();
        assert_eq!((p.frequency, p.btr0, p.btr1), (65535, 255, 255));
    }

    #[test]
    fn checksum_roundtrip() {
        let out = node_roundtrip("CHECKSUM", "/begin CHECKSUM \"chk.dll\" /end CHECKSUM");
        assert_eq!(out, "/begin CHECKSUM\n  \"chk.dll\"\n/end CHECKSUM\n");
        let out = node_roundtrip("CHECKSUM", "/begin CHECKSUM /end CHECKSUM");
        assert_eq!(out, "/begin CHECKSUM\n/end CHECKSUM\n");
    }

    #[test]
    fn checksum_param_roundtrip() {
        let out = node_roundtrip(
            "CHECKSUM_PARAM",
            "/begin CHECKSUM_PARAM 0x9001 0xFFFF CHECKSUM_CALCULATION BIT_OR_WITH_OPT_PAGE /end CHECKSUM_PARAM",
        );
        assert_eq!(
            out,
            "/begin CHECKSUM_PARAM\n  0x9001\n  0xFFFF\n  CHECKSUM_CALCULATION BIT_OR_WITH_OPT_PAGE\n/end CHECKSUM_PARAM\n"
        );
        let block = first_block("/begin CHECKSUM_PARAM 0x1234 0x100 /end CHECKSUM_PARAM");
        let p = CcpChecksumParam::parse(&block).unwrap();
        assert_eq!(p.calc_type, CcpChecksumCalcType::NotSet);
        assert_eq!(p.checksum_type(), ChecksumType::ADD_12);
        let block = first_block("/begin CHECKSUM_PARAM 0x9001 0x1 /end CHECKSUM_PARAM");
        assert_eq!(
            CcpChecksumParam::parse(&block).unwrap().checksum_type(),
            ChecksumType::CRC_16_CITT
        );
    }

    #[test]
    fn defined_pages_roundtrip_and_display() {
        let out = node_roundtrip(
            "DEFINED_PAGES",
            "/begin DEFINED_PAGES 0x1 \"Page1\" 0x3 0x80000000 0x1000 RAM FLASH /end DEFINED_PAGES",
        );
        assert_eq!(
            out,
            "/begin DEFINED_PAGES\n  0x1\n  \"Page1\"\n  0x3\n  0x80000000\n  0x1000\n  RAM\n  FLASH\n/end DEFINED_PAGES\n"
        );
        let block = first_block(
            "/begin DEFINED_PAGES 0x1 \"Page1\" 0x3 0x80000000 0x1000 RAM FLASH /end DEFINED_PAGES",
        );
        let p = CcpDefinedPages::parse(&block).unwrap();
        assert_eq!(p.to_string(), "RAM, FLASH: 80000000(00001000)");
        assert!(p.contains(0x8000_0100));
        assert!(!p.contains(0x7000));
        assert!(!p.contains(0x8000_1000));
    }

    #[test]
    fn event_group_roundtrip() {
        let out = node_roundtrip(
            "EVENT_GROUP",
            "/begin EVENT_GROUP \"eg long\" \"egshort\" /end EVENT_GROUP",
        );
        assert_eq!(
            out,
            "/begin EVENT_GROUP\n  \"eg long\"\n  \"egshort\"\n/end EVENT_GROUP\n"
        );
    }

    #[test]
    fn qp_blob_variable_roundtrip() {
        let out = node_roundtrip(
            "QP_BLOB",
            "/begin QP_BLOB 7 LENGTH 8 RASTER 2 CAN_ID_VARIABLE 0x1FF FIRST_PID 0x0 /end QP_BLOB",
        );
        assert_eq!(
            out,
            "/begin QP_BLOB\n  7\n  LENGTH 8\n  RASTER 2\n  CAN_ID_VARIABLE 0x1FF\n  FIRST_PID 0x0\n/end QP_BLOB\n"
        );
    }

    #[test]
    fn qp_blob_fixed_default_pid() {
        let out = node_roundtrip(
            "QP_BLOB",
            "/begin QP_BLOB 9 LENGTH 4 RASTER 0 CAN_ID_FIXED 0x2FF /end QP_BLOB",
        );
        assert_eq!(
            out,
            "/begin QP_BLOB\n  9\n  LENGTH 4\n  RASTER 0\n  CAN_ID_FIXED 0x2FF\n/end QP_BLOB\n"
        );
        let block = first_block("/begin QP_BLOB 1 /end QP_BLOB");
        let q = CcpQpBlob::parse(&block).unwrap();
        assert_eq!(q.first_pid, 0xFF);
        assert_eq!(q.daq_list_type, XcpDaqListCanType::NotSet);
    }

    #[test]
    fn raster_roundtrip() {
        let out = node_roundtrip(
            "RASTER",
            "/begin RASTER \"raster long\" \"rshort\" 0x2 4 5 /end RASTER",
        );
        assert_eq!(
            out,
            "/begin RASTER\n  \"raster long\"\n  \"rshort\"\n  0x2\n  4\n  5\n/end RASTER\n"
        );
    }

    #[test]
    fn seed_key_roundtrip() {
        let out = node_roundtrip(
            "SEED_KEY",
            "/begin SEED_KEY \"cal.dll\" \"daq.dll\" \"pgm.dll\" /end SEED_KEY",
        );
        assert_eq!(
            out,
            "/begin SEED_KEY\n  \"cal.dll\"\n  \"daq.dll\"\n  \"pgm.dll\"\n/end SEED_KEY\n"
        );
        let block = first_block("/begin SEED_KEY \"cal.dll\" /end SEED_KEY");
        let k = CcpSeedKey::parse(&block).unwrap();
        assert_eq!(k.cal_dll.as_deref(), Some("cal.dll"));
        assert!(k.daq_dll.is_none());
        assert!(k.pgm_dll.is_none());
    }

    #[test]
    fn source_with_qp_blob_roundtrip() {
        let src = "/begin SOURCE \"src long\" 3 10 \
                   /begin QP_BLOB 7 LENGTH 8 RASTER 2 CAN_ID_VARIABLE 0x1FF FIRST_PID 0x0 /end QP_BLOB \
                   /end SOURCE";
        let out = node_roundtrip("SOURCE", src);
        assert_eq!(
            out,
            "/begin SOURCE\n  \"src long\"\n  3\n  10\n  /begin QP_BLOB\n    7\n    LENGTH 8\n    RASTER 2\n    CAN_ID_VARIABLE 0x1FF\n    FIRST_PID 0x0\n  /end QP_BLOB\n/end SOURCE\n"
        );
        let block = first_block(src);
        let s = CcpSource::parse(&block).unwrap();
        assert!(s.active);
        assert_eq!(s.qp_blob.unwrap().daq_no, 7);
    }

    #[test]
    fn named_node_inline_roundtrip() {
        let out = ifdata_roundtrip(
            "/begin IF_DATA ASAP1B_CCP ADDRESS_MAPPING 0x1000 0x2000 0x100 /end IF_DATA",
        );
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_CCP ADDRESS_MAPPING 0x1000 0x2000 0x100 /end IF_DATA\n"
        );
        let out =
            ifdata_roundtrip("/begin IF_DATA ASAP1B_CCP DP_BLOB 0x0 0x80000000 0x100 /end IF_DATA");
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_CCP DP_BLOB 0x0 0x80000000 0x100 /end IF_DATA\n"
        );
        let out = ifdata_roundtrip(
            "/begin IF_DATA ASAP1B_CCP KP_BLOB 0x1 0x80001000 64 RASTER 1 RASTER 2 RASTER 1 /end IF_DATA",
        );
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_CCP KP_BLOB 0x1 0x80001000 64 RASTER 1 RASTER 2 /end IF_DATA\n"
        );
    }

    #[test]
    fn named_node_name() {
        let block = first_block("/begin IF_DATA ASAP1B_CCP DP_BLOB 0x0 0x100 0x10 /end IF_DATA");
        let ifd = CcpIfData::parse(&block).unwrap();
        match ifd {
            CcpIfData::Named(n) => assert_eq!(n.name(), "ASAP1B_CCP DP_BLOB"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ifdata_nodes_roundtrip() {
        let src = "/begin IF_DATA ASAP1B_CCP \
                   /begin TP_BLOB 0x1 0x2 0x100 0x101 0x5 2 BAUDRATE 500000 /end TP_BLOB \
                   /begin CAN_PARAM 0x10 0x1 0x2 /end CAN_PARAM \
                   /begin SEED_KEY \"cal.dll\" /end SEED_KEY \
                   /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert_eq!(
            out,
            "/begin IF_DATA\n  ASAP1B_CCP\n  /begin TP_BLOB\n    0x1\n    0x2\n    0x100\n    0x101\n    0x5\n    2\n    BAUDRATE 500000\n  /end TP_BLOB\n  /begin CAN_PARAM\n    0x10 0x1 0x2\n  /end CAN_PARAM\n  /begin SEED_KEY\n    \"cal.dll\"\n  /end SEED_KEY\n/end IF_DATA\n"
        );
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let ifd2 = CcpIfData::parse(root.child("IF_DATA").unwrap()).unwrap();
        let ifd1 = CcpIfData::parse(&first_block(src)).unwrap();
        assert_eq!(ifd1, ifd2);
    }

    #[test]
    fn ifdata_unknown_protocol_passthrough() {
        let src = "/begin IF_DATA SOMETHING_ELSE 1 2 3 /end IF_DATA";
        let ifd = CcpIfData::parse(&first_block(src)).unwrap();
        match ifd {
            CcpIfData::Unsupported(u) => assert_eq!(u.keyword, "IF_DATA"),
            other => panic!("unexpected {other:?}"),
        }
        let out = ifdata_roundtrip(src);
        assert_eq!(
            out,
            "/begin IF_DATA\n  SOMETHING_ELSE 1 2 3\n/end IF_DATA\n"
        );
    }

    #[test]
    fn ifdata_unknown_child_passthrough() {
        let src = "/begin IF_DATA ASAP1B_CCP /begin FOO_BAR 1 \"x\" /end FOO_BAR /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert_eq!(
            out,
            "/begin IF_DATA\n  ASAP1B_CCP\n  /begin FOO_BAR\n    1 \"x\"\n  /end FOO_BAR\n/end IF_DATA\n"
        );
    }

    #[test]
    fn source_raster_map() {
        let src = "/begin IF_DATA ASAP1B_CCP \
                   /begin RASTER \"r1\" \"r1\" 0x2 4 5 /end RASTER \
                   /begin SOURCE \"s1\" 3 10 /begin QP_BLOB 7 RASTER 2 /end QP_BLOB /end SOURCE \
                   /begin SOURCE \"s2\" 3 25 /begin QP_BLOB 9 RASTER 99 /end QP_BLOB /end SOURCE \
                   /end IF_DATA";
        let ifd = CcpIfData::parse(&first_block(src)).unwrap();
        let map = ifd.build_source_raster_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&7].raster.evt_chn_no, 2);
        assert_eq!(map[&7].source.name_long, "s1");
        assert_eq!(map[&9].raster.evt_chn_no, 2);
        assert_eq!(map[&9].source.name_long, "s2");
    }

    #[test]
    fn parse_errors() {
        assert!(CcpTpBlob::parse(&first_block("/begin TP_BLOB 0x1 0x2 /end TP_BLOB")).is_err());
        assert!(CcpDefinedPages::parse(&first_block(
            "/begin DEFINED_PAGES 0x1 /end DEFINED_PAGES"
        ))
        .is_err());
        assert!(CcpIfData::parse(&first_block("/begin FOO 1 /end FOO")).is_err());
        let err = format!(
            "{}",
            CcpTpBlob::parse(&first_block("/begin TP_BLOB 0x1 0x2 /end TP_BLOB")).unwrap_err()
        );
        assert!(err.contains("unexpected end of parameters"), "{err}");
    }
}
