//! ASAM ODX (ISO 22901) diagnostic data model and ODX file reading/writing.
//! Serialization is built on `serde` + `quick-xml`; XML tag names follow the ODX
//! schema exactly.
//! ## Base building blocks (also referenced by sibling modules such as odx_flash)
//! - Identity bases: `IdRef` (with the concrete references `ProtocolRef`/
//!   `EcuSharedDataRef`/`FunctionalGroupRef`/`BaseVariantRef` and the `LayerRef`
//!   polymorphic enum), `SnRef`, `NotInheritedDiagComm`;
//! - Naming bases: the `Id`/`NamedId`/`NamedDescId` inheritance chain is flattened
//!   into each type by the macros `named_id_struct!`/`named_desc_id_struct!`/
//!   `base_doc_info_struct!`/`ecu_shared_data_struct!`; the standalone concrete
//!   base structs are `NamedIdData`/`NamedDescIdData`;
//! - Parameter bases: the `ParBase`/`ParBaseDef`/`ParIdBase` chains are flattened
//!   in by the macros `par_base_struct!`/`par_base_def_struct!`/`par_id_base_struct!`;
//! - Limit bases: `InternalConstr`/`ScaleConstr`/`CompuScale` (the limit base is
//!   flattened into `desc` plus `lower_limit`/`upper_limit` f64 fields);
//! - Identification/description: `IdentDesc`, `SessionDesc`, `Sdg`, etc.
//! ## Serialization semantics
//! - Members in a wrapper element (e.g. `PARAMS`, `STRUCTURES`, `UNITS`,
//!   `MODIFICATIONS`) serialize as a wrapped array; flat members serialize as
//!   repeated elements;
//! - Missing lists deserialize as empty and write back as `<X />`; optional
//!   members are omitted entirely (Rust: `Option<wrapper>`);
//! - Members equal to their default value are not written (`skip_serializing_if`);
//! - Abstract base members (`DIAG-CODED-TYPE`/`PARAM`/`PARENT-REF`/`LOGICAL-LINK`
//!   etc.) are polymorphic via `xsi:type` (Rust: enums with hand-written
//!   `Serialize`/`Deserialize` plus raw union structs);
//! - The XML declaration is written as `<?xml version="1.0" encoding="utf-8"?>`
//!   with tab indentation; output is UTF-8 without BOM (cross-platform convention,
//!   same as the CDF module).
//! ## Intentional deviations (see the per-type comments)
//! - Date/time members (`DocRevision.DATE`, `XDoc.DATE`) are kept as raw strings
//!   and not parsed;
//! - The inner XML of `DESC` elements is kept as plain text; embedded XHTML markup
//!   is not preserved;
//! - The HTML report (`getOverviewAsHtml`) and the reflection-based `changeId`
//!   are not provided (see the `OdxFile` comments); cross-file `DOCREF` reference
//!   resolution and the PDX container live in [`crate::odx_multifile`];
//! - Runtime back-references (`UnitRef`/`DopRef`/`Parent` etc., non-serialized
//!   reverse pointers) are replaced by `OdxIndex` lookups (Rust ownership forbids
//!   back-pointers).

use std::fmt;
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

pub(crate) fn byte_len_from_bit_len(bit_length: i64) -> i64 {
    bit_length / 8 + i64::from(bit_length % 8 > 0)
}

/// Bitmask with the low `length` bits set.
/// A zero or negative length yields 0 instead of panicking; call sites never produce a zero length.
pub(crate) fn build_bitmask(length: i64) -> u64 {
    if length <= 0 {
        return 0;
    }
    if length >= 64 {
        return u64::MAX;
    }
    (1u64 << length) - 1
}

/// Changes the endianness of a 16-bit value.
pub(crate) fn swap16(v: u16) -> u16 {
    v.swap_bytes()
}

/// Changes the endianness of a 32-bit value.
pub(crate) fn swap32(v: u32) -> u32 {
    v.swap_bytes()
}

/// Changes the endianness of a 64-bit value.
pub(crate) fn swap64(v: u64) -> u64 {
    v.swap_bytes()
}

fn has_hex_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && (b[1] & 0x5F) == b'X'
}

fn from_hex_str(s: &str, offset: usize, len: usize) -> i64 {
    let b = s.as_bytes();
    let end = if len == 0 { s.len() } else { offset + len };
    let mut num: i64 = 0;
    let mut i = offset;
    while i < end && i < b.len() {
        num <<= 4;
        let c = b[i];
        num += if c < b'A' {
            i64::from(c.wrapping_sub(b'0'))
        } else {
            i64::from((c & 0x5F).wrapping_sub(b'A') + 10)
        };
        i += 1;
    }
    num
}

pub(crate) fn hex_byte_at(s: &str, offset: usize) -> u8 {
    from_hex_str(s, offset, 2) as u8
}

pub(crate) fn parse_odx_double(s: &str) -> f64 {
    if has_hex_prefix(s) {
        return from_hex_str(s, 2, 0) as f64;
    }
    if let Ok(v) = s.trim().parse::<f64>() {
        return v;
    }
    let t = s.trim();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = i64::from_str_radix(digits, 16).unwrap_or(0);
    (if neg { -v } else { v }) as f64
}

pub(crate) fn parse_uint_dec_or_hex(s: &str, hex: bool) -> u32 {
    let t = s.trim();
    if !hex {
        if let Ok(v) = t.parse::<u32>() {
            return v;
        }
    }
    u32::from_str_radix(t.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap_or(0)
}

pub(crate) fn bytes_to_hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02X}")).collect()
}

pub(crate) fn hex_to_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < s.len() {
        out.push(hex_byte_at(s, i));
        i += 2;
    }
    if i < s.len() {
        out.push(from_hex_str(s, i, 1) as u8);
    }
    out
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_zero<T: PartialEq + Default>(v: &T) -> bool {
    *v == T::default()
}

fn default_eight() -> u64 {
    8
}

fn is_eight(v: &u64) -> bool {
    *v == 8
}

fn is_physical(a: &AddressingType) -> bool {
    *a == AddressingType::Physical
}

macro_rules! named_id_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        $vis struct $name {
            #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
            pub id: Option<String>,
            $($attrs)*
            #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
            pub short_name: Option<String>,
            #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
            pub long_name: Option<String>,
            #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
            pub sdgs: Option<Sdgs>,
            $($body)*
        }

        impl $name {
            pub fn display_name(&self) -> &str {
                match &self.long_name {
                    Some(l) if !l.is_empty() => l,
                    _ => self.short_name.as_deref().unwrap_or_default(),
                }
            }
        }
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        named_id_struct!($(#[$meta])* $vis struct $name { attrs {} $($body)* });
    };
}

macro_rules! named_desc_id_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        named_id_struct!($(#[$meta])* $vis struct $name {
            attrs { $($attrs)* }
            #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
            pub desc: Option<String>,
            $($body)*
        });
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        named_id_struct!($(#[$meta])* $vis struct $name {
            #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
            pub desc: Option<String>,
            $($body)*
        });
    };
}

macro_rules! par_base_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        $vis struct $name {
            $($attrs)*
            #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
            pub desc: Option<String>,
            #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
            pub short_name: Option<String>,
            #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
            pub long_name: Option<String>,
            $($body)*
        }

        impl $name {
            pub fn display_name(&self) -> &str {
                match &self.long_name {
                    Some(l) if !l.is_empty() => l,
                    _ => self.short_name.as_deref().unwrap_or_default(),
                }
            }
        }
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        par_base_struct!($(#[$meta])* $vis struct $name { attrs {} $($body)* });
    };
}

macro_rules! par_base_def_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        par_base_struct!($(#[$meta])* $vis struct $name {
            attrs {
                #[serde(rename = "@SEMANTIC", default, skip_serializing_if = "Option::is_none")]
                pub semantic: Option<String>,
                $($attrs)*
            }
            #[serde(rename = "BYTE-POSITION", default)]
            pub byte_position: i64,
            #[serde(rename = "BIT-POSITION", default, skip_serializing_if = "is_zero")]
            pub bit_position: i64,
            #[serde(
                rename = "PHYSICAL-DEFAULT-VALUE",
                default,
                skip_serializing_if = "Option::is_none"
            )]
            pub physical_default_value: Option<String>,
            $($body)*
        });
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        par_base_def_struct!($(#[$meta])* $vis struct $name { attrs {} $($body)* });
    };
}

macro_rules! par_id_base_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        par_base_def_struct!($(#[$meta])* $vis struct $name {
            attrs {
                #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
                pub id: Option<String>,
            }
            $($body)*
        });
    };
}

mod limit_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() {
            return Ok(0.0);
        }
        Ok(super::parse_odx_double(&s))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BaseDataType {
    /// 8 Bit, signed
    #[default]
    #[serde(rename = "A_INT8")]
    AInt8,
    /// 8 Bit, unsigned
    #[serde(rename = "A_UINT8")]
    AUint8,
    /// 16 Bit, signed
    #[serde(rename = "A_INT16")]
    AInt16,
    /// 16 Bit, unsigned
    #[serde(rename = "A_UINT16")]
    AUint16,
    /// 32 Bit, signed
    #[serde(rename = "A_INT32")]
    AInt32,
    /// 32 Bit, unsigned
    #[serde(rename = "A_UINT32")]
    AUint32,
    /// 32 Bit, float
    #[serde(rename = "A_FLOAT32")]
    AFloat32,
    /// 64 Bit, float
    #[serde(rename = "A_FLOAT64")]
    AFloat64,
    /// ASCII
    #[serde(rename = "A_ASCIISTRING")]
    AAsciistring,
    /// Unicode
    #[serde(rename = "A_UNICODE2STRING")]
    AUnicode2string,
    /// UTF8
    #[serde(rename = "A_UTF8STRING")]
    AUtf8string,
    /// BYTEFIELD
    #[serde(rename = "A_BYTEFIELD")]
    ABytefield,
}

impl BaseDataType {
    pub fn as_str(self) -> &'static str {
        match self {
            BaseDataType::AInt8 => "A_INT8",
            BaseDataType::AUint8 => "A_UINT8",
            BaseDataType::AInt16 => "A_INT16",
            BaseDataType::AUint16 => "A_UINT16",
            BaseDataType::AInt32 => "A_INT32",
            BaseDataType::AUint32 => "A_UINT32",
            BaseDataType::AFloat32 => "A_FLOAT32",
            BaseDataType::AFloat64 => "A_FLOAT64",
            BaseDataType::AAsciistring => "A_ASCIISTRING",
            BaseDataType::AUnicode2string => "A_UNICODE2STRING",
            BaseDataType::AUtf8string => "A_UTF8STRING",
            BaseDataType::ABytefield => "A_BYTEFIELD",
        }
    }
}

impl fmt::Display for BaseDataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AddressingType {
    /// Physical
    #[default]
    Physical,
    /// Functional
    Functional,
    /// Functional or Physical
    FunctionalOrPhysical,
}

impl AddressingType {
    pub fn as_str(self) -> &'static str {
        match self {
            AddressingType::Physical => "PHYSICAL",
            AddressingType::Functional => "FUNCTIONAL",
            AddressingType::FunctionalOrPhysical => "FUNCTIONAL-OR-PHYSICAL",
        }
    }
}

impl fmt::Display for AddressingType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AddressingType {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AddressingType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        Ok(match v.as_str() {
            "FUNCTIONAL" => AddressingType::Functional,
            "FUNCTIONAL-OR-PHYSICAL" => AddressingType::FunctionalOrPhysical,
            _ => AddressingType::Physical,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CmCategory {
    /// IDENTICAL
    #[default]
    Identical,
    /// TEXTTABLE
    Texttable,
    /// LINEAR
    Linear,
    /// SCALE_LINEAR
    ScaleLinear,
    /// RAT_FUNC
    RatFunc,
    /// SCALE_RAT_FUNC
    ScaleRatFunc,
    /// TAB_INTP
    TabIntp,
    /// COMPUCODE
    Compucode,
}

impl CmCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            CmCategory::Identical => "IDENTICAL",
            CmCategory::Texttable => "TEXTTABLE",
            CmCategory::Linear => "LINEAR",
            CmCategory::ScaleLinear => "SCALE-LINEAR",
            CmCategory::RatFunc => "RAT-FUNC",
            CmCategory::ScaleRatFunc => "SCALE-RAT-FUNC",
            CmCategory::TabIntp => "TAB-INTP",
            CmCategory::Compucode => "COMPUCODE",
        }
    }

    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "TEXTTABLE" => CmCategory::Texttable,
            "LINEAR" => CmCategory::Linear,
            "SCALE-LINEAR" => CmCategory::ScaleLinear,
            "RAT-FUNC" => CmCategory::RatFunc,
            "SCALE-RAT-FUNC" => CmCategory::ScaleRatFunc,
            "TAB-INTP" => CmCategory::TabIntp,
            "COMPUCODE" => CmCategory::Compucode,
            _ => CmCategory::Identical,
        }
    }
}

impl fmt::Display for CmCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ValidType {
    /// VALID
    #[default]
    Valid,
    /// NOT_VALID
    NotValid,
    /// NOT_DEFINED
    NotDefined,
    /// NOT_AVAILABLE
    NotAvailable,
}

impl ValidType {
    pub fn as_str(self) -> &'static str {
        match self {
            ValidType::Valid => "VALID",
            ValidType::NotValid => "NOT-VALID",
            ValidType::NotDefined => "NOT-DEFINED",
            ValidType::NotAvailable => "NOT-AVAILABLE",
        }
    }

    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "NOT-VALID" => ValidType::NotValid,
            "NOT-DEFINED" => ValidType::NotDefined,
            "NOT-AVAILABLE" => ValidType::NotAvailable,
            _ => ValidType::Valid,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminationType {
    /// ZERO (=0)
    #[default]
    Zero,
    /// END_OF_PDU (=1)
    EndOfPdu,
    /// HEX_FF (=0xFF)
    HexFf,
}

impl TerminationType {
    pub fn as_str(self) -> &'static str {
        match self {
            TerminationType::Zero => "ZERO",
            TerminationType::EndOfPdu => "END-OF-PDU",
            TerminationType::HexFf => "HEX-FF",
        }
    }

    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "END-OF-PDU" => TerminationType::EndOfPdu,
            "HEX-FF" => TerminationType::HexFf,
            _ => TerminationType::Zero,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RowFragment {
    /// KEY
    #[default]
    Key,
    /// STRUCT
    Struct,
    /// KEY_AND_STRUCT
    KeyAndStruct,
}

impl RowFragment {
    pub fn as_str(self) -> &'static str {
        match self {
            RowFragment::Key => "KEY",
            RowFragment::Struct => "STRUCT",
            RowFragment::KeyAndStruct => "KEY-AND-STRUCT",
        }
    }

    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "STRUCT" => RowFragment::Struct,
            "KEY-AND-STRUCT" => RowFragment::KeyAndStruct,
            _ => RowFragment::Key,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DataFormatType {
    /// BINARY
    #[default]
    Binary,
    /// INTEL_HEX
    IntelHex,
    /// MOTOROLA_S
    MotorolaS,
}

impl DataFormatType {
    pub fn as_str(self) -> &'static str {
        match self {
            DataFormatType::Binary => "BINARY",
            DataFormatType::IntelHex => "INTEL-HEX",
            DataFormatType::MotorolaS => "MOTOROLA-S",
        }
    }

    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "INTEL-HEX" => DataFormatType::IntelHex,
            "MOTOROLA-S" => DataFormatType::MotorolaS,
            _ => DataFormatType::Binary,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SysParmType {
    /// TIMEZONE
    #[default]
    #[serde(rename = "TIMEZONE")]
    Timezone,
    /// YEAR
    #[serde(rename = "YEAR")]
    Year,
    /// MONTH
    #[serde(rename = "MONTH")]
    Month,
    /// DAY
    #[serde(rename = "DAY")]
    Day,
    /// HOUR
    #[serde(rename = "HOUR")]
    Hour,
    /// MINUTE
    #[serde(rename = "MINUTE")]
    Minute,
    /// SECOND
    #[serde(rename = "SECOND")]
    Second,
    /// TESTERID
    #[serde(rename = "TESTERID")]
    Testerid,
    /// USERID
    #[serde(rename = "USERID")]
    Userid,
    /// CENTURY
    #[serde(rename = "CENTURY")]
    Century,
    /// WEEK
    #[serde(rename = "WEEK")]
    Week,
    /// TIMESTAMP
    #[serde(rename = "TIMESTAMP")]
    Timestamp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinType {
    /// HI
    #[default]
    #[serde(rename = "HI")]
    Hi,
    /// LOW
    #[serde(rename = "LOW")]
    Low,
    /// K
    #[serde(rename = "K")]
    K,
    /// L
    #[serde(rename = "L")]
    L,
    /// TX
    #[serde(rename = "TX")]
    Tx,
    /// RX
    #[serde(rename = "RX")]
    Rx,
    /// PLUS
    #[serde(rename = "PLUS")]
    Plus,
    /// MINUS
    #[serde(rename = "MINUS")]
    Minus,
    /// SINGLE
    #[serde(rename = "SINGLE")]
    Single,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DataBlockType {
    /// UNKNOWN
    #[default]
    Unknown,
    /// BOOT
    Boot,
    /// CODE
    Code,
    /// DATA
    Data,
}

impl DataBlockType {
    pub fn from_odx_name(s: &str) -> Self {
        match s {
            "BOOT" => DataBlockType::Boot,
            "CODE" => DataBlockType::Code,
            "DATA" => DataBlockType::Data,
            _ => DataBlockType::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DataBlockType::Unknown => "UNKNOWN",
            DataBlockType::Boot => "BOOT",
            DataBlockType::Code => "CODE",
            DataBlockType::Data => "DATA",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanBaudrate(pub u32);

impl CanBaudrate {
    /// Not set
    pub const NOT_SET: CanBaudrate = CanBaudrate(0);
    /// 10 kBit/s
    pub const KBIT_10: CanBaudrate = CanBaudrate(10_000);
    /// 20 kBit/s
    pub const KBIT_20: CanBaudrate = CanBaudrate(20_000);
    /// 50 kBit/s
    pub const KBIT_50: CanBaudrate = CanBaudrate(50_000);
    /// 100 kBit/s
    pub const KBIT_100: CanBaudrate = CanBaudrate(100_000);
    /// 125 kBit/s
    pub const KBIT_125: CanBaudrate = CanBaudrate(125_000);
    /// 250 kBit/s
    pub const KBIT_250: CanBaudrate = CanBaudrate(250_000);
    /// 500 kBit/s
    pub const KBIT_500: CanBaudrate = CanBaudrate(500_000);
    /// 800 kBit/s
    pub const KBIT_800: CanBaudrate = CanBaudrate(800_000);
    /// 1 MBit/s
    pub const KBIT_1M: CanBaudrate = CanBaudrate(1_000_000);
    /// 2 MBit/s
    pub const KBIT_2M: CanBaudrate = CanBaudrate(2_000_000);
    /// 4 MBit/s
    pub const KBIT_4M: CanBaudrate = CanBaudrate(4_000_000);
    /// 5 MBit/s
    pub const KBIT_5M: CanBaudrate = CanBaudrate(5_000_000);
    /// 8 MBit/s
    pub const KBIT_8M: CanBaudrate = CanBaudrate(8_000_000);
    /// 10 MBit/s
    pub const KBIT_10M: CanBaudrate = CanBaudrate(10_000_000);

    pub fn parse(s: &str) -> Option<CanBaudrate> {
        let v = match s {
            "NotSet" => 0,
            "_10kBit" => 10_000,
            "_20kBit" => 20_000,
            "_50kBit" => 50_000,
            "_100kBit" => 100_000,
            "_125kBit" => 125_000,
            "_250kBit" => 250_000,
            "_500kBit" => 500_000,
            "_800kBit" => 800_000,
            "_1MBit" => 1_000_000,
            "_2MBit" => 2_000_000,
            "_4MBit" => 4_000_000,
            "_5MBit" => 5_000_000,
            "_8MBit" => 8_000_000,
            "_10MBit" => 10_000_000,
            other => other.parse::<u32>().ok()?,
        };
        Some(CanBaudrate(v))
    }

    pub fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "NotSet",
            10_000 => "_10kBit",
            20_000 => "_20kBit",
            50_000 => "_50kBit",
            100_000 => "_100kBit",
            125_000 => "_125kBit",
            250_000 => "_250kBit",
            500_000 => "_500kBit",
            800_000 => "_800kBit",
            1_000_000 => "_1MBit",
            2_000_000 => "_2MBit",
            4_000_000 => "_4MBit",
            5_000_000 => "_5MBit",
            8_000_000 => "_8MBit",
            10_000_000 => "_10MBit",
            _ => return None,
        })
    }
}

impl fmt::Display for CanBaudrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(n) => f.write_str(n),
            None => write!(f, "{}", self.0),
        }
    }
}

// ============================================================================
// Reference base types: IdRef / SnRef / concrete references / LayerRef polymorphic enum
// ============================================================================

/// Reference by `SHORT-NAME` attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnRef {
    /// The `SHORT-NAME` attribute.
    #[serde(
        rename = "@SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
}

/// Reference by `ID-REF` / `DOCREF` / `DOCTYPE` attributes.
/// A cross-file load failure cannot be written back into a resolved tree;
/// failed loads are tracked in the failure set of
/// [`crate::odx_multifile::OdxDocumentSet`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdRef {
    /// The `ID-REF` attribute.
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    /// The `DOCREF` attribute.
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    /// The `DOCTYPE` attribute.
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
}

impl IdRef {
    /// Creates a reference carrying only an `ID-REF` attribute.
    pub fn new(id_ref: impl Into<String>) -> Self {
        IdRef {
            id_ref: Some(id_ref.into()),
            docref: None,
            doctype: None,
        }
    }
}

impl fmt::Display for IdRef {
    /// Renders the plain type name, matching the untyped `ToString()` form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdRef")
    }
}

/// A `NOT-INHERITED-DIAG-COMM` entry (value type).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotInheritedDiagComm {
    /// The `DIAG-COMM-SNREF` element.
    #[serde(
        rename = "DIAG-COMM-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diag_comm_snref: Option<SnRef>,
}

/// `NOT-INHERITED-DIAG-COMMS` wrapper element. The list is instantiated as
/// soon as the parent is deserialized and is always written back
/// (empty list → `<NOT-INHERITED-DIAG-COMMS />`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotInheritedDiagComms {
    /// The list of entries.
    #[serde(rename = "NOT-INHERITED-DIAG-COMM", default)]
    pub items: Vec<NotInheritedDiagComm>,
}

/// A `BASE-VARIANT-REF` element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseVariantRef {
    /// The `ID-REF` attribute.
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    /// The `DOCREF` attribute.
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    /// The `DOCTYPE` attribute.
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
    /// The list wrapped in `NOT-INHERITED-DIAG-COMMS`.
    #[serde(rename = "NOT-INHERITED-DIAG-COMMS", default)]
    pub not_inherited_diag_comms: NotInheritedDiagComms,
}

impl BaseVariantRef {
    /// Converts to a generic `IdRef` view.
    pub fn as_id_ref(&self) -> IdRef {
        IdRef {
            id_ref: self.id_ref.clone(),
            docref: self.docref.clone(),
            doctype: self.doctype.clone(),
        }
    }
}

/// A `PROTOCOL-REF` element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRef {
    /// The `ID-REF` attribute.
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    /// The `DOCREF` attribute.
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    /// The `DOCTYPE` attribute.
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
}

/// An `ECU-SHARED-DATA-REF` element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcuSharedDataRef {
    /// The `ID-REF` attribute.
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    /// The `DOCREF` attribute.
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    /// The `DOCTYPE` attribute.
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
}

/// A `FUNCTIONAL-GROUP-REF` element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionalGroupRef {
    /// The `ID-REF` attribute.
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    /// The `DOCREF` attribute.
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    /// The `DOCTYPE` attribute.
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
}

/// Polymorphic reference for `PARENT-REF` / `IMPORT-REF`: the declared slot
/// type is a plain `IdRef`, but concrete subtypes are distinguished via
/// `xsi:type`; without `xsi:type` it is a plain `IdRef`.
/// Used for entries of `EcuVariant.PARENT_REFS`, `EcuSharedData.IMPORT_REFS`,
/// and `FunctionalGroup.PARENT_REFS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerRef {
    /// Plain `IdRef` (no `xsi:type`).
    Id(IdRef),
    /// `xsi:type="ECU-SHARED-DATA-REF"`.
    EcuSharedData(IdRef),
    /// `xsi:type="PROTOCOL-REF"`.
    Protocol(IdRef),
    /// `xsi:type="FUNCTIONAL-GROUP-REF"`.
    FunctionalGroup(IdRef),
    /// `xsi:type="BASE-VARIANT-REF"` (may carry `NOT-INHERITED-DIAG-COMMS`).
    BaseVariant(BaseVariantRef),
}

impl Default for LayerRef {
    fn default() -> Self {
        LayerRef::Id(IdRef::default())
    }
}

impl LayerRef {
    pub fn id_ref(&self) -> Option<&str> {
        match self {
            LayerRef::Id(r)
            | LayerRef::EcuSharedData(r)
            | LayerRef::Protocol(r)
            | LayerRef::FunctionalGroup(r) => r.id_ref.as_deref(),
            LayerRef::BaseVariant(r) => r.id_ref.as_deref(),
        }
    }

    pub fn docref(&self) -> Option<&str> {
        match self {
            LayerRef::Id(r)
            | LayerRef::EcuSharedData(r)
            | LayerRef::Protocol(r)
            | LayerRef::FunctionalGroup(r) => r.docref.as_deref(),
            LayerRef::BaseVariant(r) => r.docref.as_deref(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawLayerRef {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    id_ref: Option<String>,
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    docref: Option<String>,
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    doctype: Option<String>,
    #[serde(
        rename = "NOT-INHERITED-DIAG-COMMS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    not_inherited: Option<NotInheritedDiagComms>,
}

impl Serialize for LayerRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let (xsi, id_ref, docref, doctype, nih) = match self {
            LayerRef::Id(r) => (None, &r.id_ref, &r.docref, &r.doctype, None),
            LayerRef::EcuSharedData(r) => (
                Some("ECU-SHARED-DATA-REF"),
                &r.id_ref,
                &r.docref,
                &r.doctype,
                None,
            ),
            LayerRef::Protocol(r) => (Some("PROTOCOL-REF"), &r.id_ref, &r.docref, &r.doctype, None),
            LayerRef::FunctionalGroup(r) => (
                Some("FUNCTIONAL-GROUP-REF"),
                &r.id_ref,
                &r.docref,
                &r.doctype,
                None,
            ),
            LayerRef::BaseVariant(r) => (
                Some("BASE-VARIANT-REF"),
                &r.id_ref,
                &r.docref,
                &r.doctype,
                Some(&r.not_inherited_diag_comms),
            ),
        };
        RawLayerRef {
            xsi_type: xsi.map(str::to_owned),
            id_ref: id_ref.clone(),
            docref: docref.clone(),
            doctype: doctype.clone(),
            not_inherited: nih.filter(|_| xsi == Some("BASE-VARIANT-REF")).cloned(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LayerRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = RawLayerRef::deserialize(d)?;
        let idr = IdRef {
            id_ref: raw.id_ref,
            docref: raw.docref,
            doctype: raw.doctype,
        };
        Ok(match raw.xsi_type.as_deref() {
            Some("ECU-SHARED-DATA-REF") => LayerRef::EcuSharedData(idr),
            Some("PROTOCOL-REF") => LayerRef::Protocol(idr),
            Some("FUNCTIONAL-GROUP-REF") => LayerRef::FunctionalGroup(idr),
            Some("BASE-VARIANT-REF") => LayerRef::BaseVariant(BaseVariantRef {
                id_ref: idr.id_ref,
                docref: idr.docref,
                doctype: idr.doctype,
                not_inherited_diag_comms: raw.not_inherited.unwrap_or_default(),
            }),
            _ => LayerRef::Id(idr),
        })
    }
}

// ============================================================================
// SDG
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sdgs {
    #[serde(rename = "SDG", default)]
    pub items: Vec<Sdg>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sdg {
    #[serde(
        rename = "SDG-CAPTION-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sdg_caption_ref: Option<IdRef>,
    #[serde(
        rename = "SDG-CAPTION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sdg_caption: Option<NamedDescIdData>,
    #[serde(rename = "SD", default, skip_serializing_if = "Vec::is_empty")]
    pub sds: Vec<String>,
    #[serde(rename = "SDG", default, skip_serializing_if = "Vec::is_empty")]
    pub sdgs: Vec<Sdg>,
}

// ============================================================================
// Concrete naming bases (for polymorphic slots where the declared type is the final type)
// ============================================================================

named_id_struct! {
    /// Concrete instance form of the `NamedId` naming base
    /// (used for polymorphic slots such as `FLASHDATA` and `INFO-COMPONENT`
    /// whose declared type is `NamedId`).
    pub struct NamedIdData {}
}

impl fmt::Display for NamedIdData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

named_desc_id_struct! {
    /// Concrete instance form of the `NamedDescId` naming base
    /// (used for slots such as `STATE`, `FLASH-CLASS`, and `SDG-CAPTION`
    /// whose declared type is `NamedDescId`).
    pub struct NamedDescIdData {}
}

impl fmt::Display for NamedDescIdData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

// ============================================================================
// ADMIN-DATA family: AdminData / DocRevision / Modification / CompanyData /
// TeamMember / CompanySpecificInfo / RelatedDoc / XDoc / Audience
// ============================================================================

/// `DOC-REVISIONS` wrapper element (a list member: always written, even when empty).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocRevisions {
    /// `DOC-REVISION` entries.
    #[serde(rename = "DOC-REVISION", default)]
    pub items: Vec<DocRevision>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminData {
    #[serde(rename = "LANGUAGE", default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "DOC-REVISIONS", default)]
    pub doc_revisions: DocRevisions,
}

/// `MODIFICATIONS` wrapper element (an array member: omitted entirely when missing).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Modifications {
    /// `MODIFICATION` entries.
    #[serde(rename = "MODIFICATION", default)]
    pub items: Vec<Modification>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Modification {
    #[serde(rename = "CHANGE", default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(rename = "REASON", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Modification {
    pub fn new(change: impl Into<String>, reason: impl Into<String>) -> Self {
        Modification {
            change: Some(change.into()),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocRevision {
    #[serde(
        rename = "TEAM-MEMBER-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub team_member_ref: Option<IdRef>,
    #[serde(
        rename = "REVISION-LABEL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub revision_label: Option<String>,
    #[serde(rename = "STATE", default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(rename = "DATE", default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(rename = "TOOL", default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(
        rename = "MODIFICATIONS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub modifications: Option<Modifications>,
}

impl DocRevision {
    pub fn new_revision(
        rev_label: impl Into<String>,
        tm: &TeamMember,
        state: impl Into<String>,
    ) -> Self {
        DocRevision {
            team_member_ref: Some(IdRef::new(tm.id.clone().unwrap_or_default())),
            revision_label: Some(rev_label.into()),
            state: Some(state.into()),
            date: None,
            tool: Some(format!(
                "{}  v{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            )),
            modifications: None,
        }
    }
}

impl fmt::Display for DocRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.revision_label.as_deref().unwrap_or_default())
    }
}

named_id_struct! {
    pub struct TeamMember {
        #[serde(rename = "ADDRESS", default, skip_serializing_if = "Option::is_none")]
        pub address: Option<String>,
        #[serde(rename = "ZIP", default, skip_serializing_if = "Option::is_none")]
        pub zip: Option<String>,
        #[serde(rename = "CITY", default, skip_serializing_if = "Option::is_none")]
        pub city: Option<String>,
        #[serde(rename = "DEPARTMENT", default, skip_serializing_if = "Option::is_none")]
        pub department: Option<String>,
        #[serde(rename = "PHONE", default, skip_serializing_if = "Option::is_none")]
        pub phone: Option<String>,
        #[serde(rename = "FAX", default, skip_serializing_if = "Option::is_none")]
        pub fax: Option<String>,
        #[serde(rename = "EMAIL", default, skip_serializing_if = "Option::is_none")]
        pub email: Option<String>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamMembers {
    #[serde(rename = "TEAM-MEMBER", default)]
    pub items: Vec<TeamMember>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelatedDocs {
    #[serde(rename = "RELATED-DOC", default)]
    pub items: Vec<RelatedDoc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanySpecificInfo {
    #[serde(
        rename = "RELATED-DOCS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub related_docs: Option<RelatedDocs>,
}

named_id_struct! {
    pub struct CompanyData {
        #[serde(rename = "TEAM-MEMBERS", default)]
        pub team_members: TeamMembers,
        #[serde(
            rename = "COMPANY-SPECIFIC-INFO",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub company_specific_info: Option<CompanySpecificInfo>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct XDoc {
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    #[serde(rename = "NUMBER", default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(rename = "STATE", default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(rename = "DATE", default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(rename = "PUBLISHER", default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}

impl XDoc {
    pub fn display_name(&self) -> &str {
        match &self.long_name {
            Some(l) if !l.is_empty() => l,
            _ => self.short_name.as_deref().unwrap_or_default(),
        }
    }
}

impl fmt::Display for XDoc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelatedDoc {
    #[serde(rename = "XDOC", default, skip_serializing_if = "Option::is_none")]
    pub xdoc: Option<XDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audience {
    #[serde(
        rename = "@IS-MANUFACTURING",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_manufacturing: bool,
    #[serde(
        rename = "@IS-DEVELOPMENT",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_development: bool,
    #[serde(
        rename = "@IS-SUPPLIER",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_supplier: bool,
    #[serde(
        rename = "@IS-AFTERSALES",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_aftersales: bool,
    #[serde(
        rename = "@IS-AFTERMARKET",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_aftermarket: bool,
}

impl Default for Audience {
    fn default() -> Self {
        Audience {
            is_manufacturing: true,
            is_development: true,
            is_supplier: true,
            is_aftersales: true,
            is_aftermarket: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuScales {
    #[serde(rename = "COMPU-SCALE", default)]
    pub items: Vec<CompuScale>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuInternalToPhys {
    #[serde(
        rename = "COMPU-SCALES",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compu_scales: Option<CompuScales>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CompuValue {
    #[serde(rename = "V")]
    V(f64),
    #[serde(rename = "VT")]
    Vt(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuValues {
    #[serde(rename = "$value", default)]
    pub items: Vec<CompuValue>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuScale {
    #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(rename = "LOWER-LIMIT", default, with = "limit_serde")]
    pub lower_limit: f64,
    #[serde(rename = "UPPER-LIMIT", default, with = "limit_serde")]
    pub upper_limit: f64,
    #[serde(
        rename = "SHORT-LABEL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_label: Option<String>,
    #[serde(
        rename = "COMPU-INVERSE-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compu_inverse_value: Option<CompuValues>,
    #[serde(
        rename = "COMPU-CONST",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compu_const: Option<CompuValues>,
    #[serde(
        rename = "COMPU-RATIONAL-COEFFS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compu_rational_coeffs: Option<CompuRationalCoeffs>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuNumerator {
    #[serde(rename = "V", default)]
    pub items: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuDenominator {
    #[serde(rename = "V", default)]
    pub items: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuRationalCoeffs {
    #[serde(
        rename = "SHORT-LABEL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_label: Option<String>,
    #[serde(
        rename = "COMPU-NUMERATOR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub numerator: Option<CompuNumerator>,
    #[serde(
        rename = "COMPU-DENOMINATOR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub denominator: Option<CompuDenominator>,
}

impl CompuRationalCoeffs {
    pub fn to_physical(&self, raw_value: f64) -> f64 {
        let num = self.numerator.as_ref().map(|n| n.items.as_slice());
        if let Some(n) = num {
            if n.len() == 2 {
                return (raw_value + n[0]) * n[1];
            }
            if n.len() == 3 {
                let den = self.denominator.as_ref().map(|d| d.items.as_slice());
                if let Some(d) = den {
                    if d.len() == 3 {
                        let a = d[0] * raw_value - n[0];
                        let b = d[1] * raw_value - n[1];
                        let c = d[2] * raw_value - n[2];
                        if a != 0.0 && !a.is_nan() && a.is_finite() {
                            let disc = (b * b - 4.0 * a * c).sqrt();
                            let two_a = 2.0 * a;
                            return (-b + disc) / two_a;
                        }
                        return -c / b;
                    }
                }
            }
        }
        raw_value
    }
}

mod cm_category_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &super::CmCategory, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<super::CmCategory, D::Error> {
        let s = String::deserialize(d)?;
        Ok(super::CmCategory::from_odx_name(&s))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompuMethod {
    #[serde(rename = "CATEGORY", default, with = "cm_category_serde")]
    pub category: CmCategory,
    #[serde(
        rename = "COMPU-INTERNAL-TO-PHYS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub compu_internal_to_phys: Option<CompuInternalToPhys>,
}

impl CompuMethod {
    pub fn text_table(&self) -> Option<IndexMap<i64, String>> {
        if self.category != CmCategory::Texttable {
            return None;
        }
        let scales = self
            .compu_internal_to_phys
            .as_ref()?
            .compu_scales
            .as_ref()?;
        let mut map = IndexMap::new();
        for scale in &scales.items {
            let value = match scale.compu_const.as_ref()?.items.first() {
                Some(CompuValue::Vt(s)) => s.clone(),
                Some(CompuValue::V(v)) => format!("{v}"),
                None => continue,
            };
            map.insert(scale.lower_limit.round_ties_even() as i64, value);
        }
        Some(map)
    }
}

impl fmt::Display for CompuMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.category.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScaleConstrs {
    #[serde(rename = "SCALE-CONSTR", default)]
    pub items: Vec<ScaleConstr>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InternalConstr {
    #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(rename = "LOWER-LIMIT", default, with = "limit_serde")]
    pub lower_limit: f64,
    #[serde(rename = "UPPER-LIMIT", default, with = "limit_serde")]
    pub upper_limit: f64,
    #[serde(
        rename = "SCALE-CONSTRS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub scale_constrs: Option<ScaleConstrs>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScaleConstr {
    pub desc: Option<String>,
    pub lower_limit: f64,
    pub upper_limit: f64,
    pub validity: ValidType,
    pub short_label: Option<String>,
}

#[derive(Serialize, Deserialize)]
enum ValidTypeEnumName {
    #[serde(rename = "VALID")]
    Valid,
    #[serde(rename = "NOT_VALID")]
    NotValid,
    #[serde(rename = "NOT_DEFINED")]
    NotDefined,
    #[serde(rename = "NOT_AVAILABLE")]
    NotAvailable,
}

impl From<ValidType> for ValidTypeEnumName {
    fn from(v: ValidType) -> Self {
        match v {
            ValidType::Valid => ValidTypeEnumName::Valid,
            ValidType::NotValid => ValidTypeEnumName::NotValid,
            ValidType::NotDefined => ValidTypeEnumName::NotDefined,
            ValidType::NotAvailable => ValidTypeEnumName::NotAvailable,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawScaleConstr {
    #[serde(rename = "@VALIDITY", default, skip_serializing_if = "Option::is_none")]
    validity_attr: Option<String>,
    #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
    desc: Option<String>,
    #[serde(rename = "LOWER-LIMIT", default, with = "limit_serde")]
    lower_limit: f64,
    #[serde(rename = "UPPER-LIMIT", default, with = "limit_serde")]
    upper_limit: f64,
    #[serde(rename = "mValidity", default, skip_serializing_if = "Option::is_none")]
    m_validity: Option<ValidTypeEnumName>,
    #[serde(
        rename = "SHORT-LABEL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    short_label: Option<String>,
}

impl Serialize for ScaleConstr {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        RawScaleConstr {
            validity_attr: Some(self.validity.as_str().to_owned()),
            desc: self.desc.clone(),
            lower_limit: self.lower_limit,
            upper_limit: self.upper_limit,
            m_validity: Some(self.validity.into()),
            short_label: self.short_label.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ScaleConstr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = RawScaleConstr::deserialize(d)?;
        let mut validity = raw
            .validity_attr
            .as_deref()
            .map_or(ValidType::default(), ValidType::from_odx_name);
        if let Some(m) = raw.m_validity {
            validity = match m {
                ValidTypeEnumName::Valid => ValidType::Valid,
                ValidTypeEnumName::NotValid => ValidType::NotValid,
                ValidTypeEnumName::NotDefined => ValidType::NotDefined,
                ValidTypeEnumName::NotAvailable => ValidType::NotAvailable,
            };
        }
        Ok(ScaleConstr {
            desc: raw.desc,
            lower_limit: raw.lower_limit,
            upper_limit: raw.upper_limit,
            validity,
            short_label: raw.short_label,
        })
    }
}

// ============================================================================
// Unit family: Unit / UnitSpec / UnitSpecs / PhysicalDimension / PhysicalType
// ============================================================================

named_desc_id_struct! {
    /// Physical dimension of a unit, expressed as SI base-quantity exponents.
    pub struct PhysicalDimension {
        /// `TIME-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "TIME-EXP", default, skip_serializing_if = "is_zero")]
        pub time_exp: i32,
        /// `LENGTH-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "LENGTH-EXP", default, skip_serializing_if = "is_zero")]
        pub length_exp: i32,
        /// `MOLAR-AMOUNT-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "MOLAR-AMOUNT-EXP", default, skip_serializing_if = "is_zero")]
        pub molar_amount_exp: i32,
        /// `MASS-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "MASS-EXP", default, skip_serializing_if = "is_zero")]
        pub mass_exp: i32,
        /// `CURRENT-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "CURRENT-EXP", default, skip_serializing_if = "is_zero")]
        pub current_exp: i32,
        /// `LUMINOUS-INTENSITY-EXP` element (defaults to 0; not written when 0).
        #[serde(
            rename = "LUMINOUS-INTENSITY-EXP",
            default,
            skip_serializing_if = "is_zero"
        )]
        pub luminous_intensity_exp: i32,
        /// `TEMPERATURE-EXP` element (defaults to 0; not written when 0).
        #[serde(rename = "TEMPERATURE-EXP", default, skip_serializing_if = "is_zero")]
        pub temperature_exp: i32,
    }
}

named_desc_id_struct! {
    /// A unit of measurement (the `UNIT` element).
    pub struct Unit {
        /// `PHYSICAL-DIMENSION-REF` element.
        #[serde(
            rename = "PHYSICAL-DIMENSION-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub physical_dimension_ref: Option<IdRef>,
        /// `DISPLAY-NAME` element.
        #[serde(rename = "DISPLAY-NAME", default, skip_serializing_if = "Option::is_none")]
        pub display_name_attr: Option<String>,
        /// `FACTOR-SI-TO-UNIT` element (defaults to 0.0; not written when 0).
        #[serde(rename = "FACTOR-SI-TO-UNIT", default, skip_serializing_if = "is_zero")]
        pub factor: f64,
        /// `OFFSET-SI-TO-UNIT` element (defaults to 0.0; not written when 0).
        #[serde(rename = "OFFSET-SI-TO-UNIT", default, skip_serializing_if = "is_zero")]
        pub offset: f64,
    }
}

impl fmt::Display for Unit {
    /// Returns `DISPLAY-NAME` when non-empty, otherwise falls back to the `NamedId` display name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.display_name_attr {
            Some(d) if !d.is_empty() => f.write_str(d),
            _ => f.write_str(self.display_name()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Units {
    #[serde(rename = "UNIT", default)]
    pub items: Vec<Unit>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysicalDimensions {
    #[serde(rename = "PHYSICAL-DIMENSION", default)]
    pub items: Vec<PhysicalDimension>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnitSpec {
    #[serde(rename = "UNITS", default, skip_serializing_if = "Option::is_none")]
    pub units: Option<Units>,
    #[serde(
        rename = "PHYSICAL-DIMENSIONS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub physical_dimensions: Option<PhysicalDimensions>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnitSpecs {
    pub units: Vec<Unit>,
    pub physical_dimensions: Vec<PhysicalDimension>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalType {
    #[serde(rename = "@BASE-DATA-TYPE", default)]
    pub base_data_type: BaseDataType,
    #[serde(
        rename = "@DISPLAY-RADIX",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_radix: Option<String>,
    #[serde(rename = "PRECISION", default, skip_serializing_if = "is_zero")]
    pub precision: i32,
}

impl fmt::Display for PhysicalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.base_data_type)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagCodedTypeStd {
    #[serde(
        rename = "@BASE-TYPE-ENCODING",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub base_type_encoding: Option<String>,
    #[serde(rename = "@BASE-DATA-TYPE", default)]
    pub base_data_type: BaseDataType,
    #[serde(
        rename = "@IS-HIGHLOW-BYTE-ORDER",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_highlow_byte_order: bool,
    #[serde(rename = "BIT-LENGTH", default, skip_serializing_if = "is_zero")]
    pub bit_length: i64,
}

impl Default for DiagCodedTypeStd {
    fn default() -> Self {
        DiagCodedTypeStd {
            base_type_encoding: None,
            base_data_type: BaseDataType::default(),
            is_highlow_byte_order: true,
            bit_length: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagCodedTypeLl {
    #[serde(
        rename = "@BASE-TYPE-ENCODING",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub base_type_encoding: Option<String>,
    #[serde(rename = "@BASE-DATA-TYPE", default)]
    pub base_data_type: BaseDataType,
    #[serde(
        rename = "@IS-HIGHLOW-BYTE-ORDER",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_highlow_byte_order: bool,
    #[serde(rename = "BIT-LENGTH", default, skip_serializing_if = "is_zero")]
    pub bit_length: u64,
}

impl Default for DiagCodedTypeLl {
    fn default() -> Self {
        DiagCodedTypeLl {
            base_type_encoding: None,
            base_data_type: BaseDataType::default(),
            is_highlow_byte_order: true,
            bit_length: 0,
        }
    }
}

mod termination_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &super::TerminationType, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<super::TerminationType, D::Error> {
        let s = String::deserialize(d)?;
        Ok(super::TerminationType::from_odx_name(&s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagCodedTypeMinMax {
    #[serde(rename = "@TERMINATION", default, with = "termination_serde")]
    pub termination: TerminationType,
    #[serde(
        rename = "@BASE-TYPE-ENCODING",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub base_type_encoding: Option<String>,
    #[serde(rename = "@BASE-DATA-TYPE", default)]
    pub base_data_type: BaseDataType,
    #[serde(
        rename = "@IS-HIGHLOW-BYTE-ORDER",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    pub is_highlow_byte_order: bool,
    #[serde(rename = "MAX-LENGTH", default)]
    pub max_length: i64,
    #[serde(rename = "MIN-LENGTH", default)]
    pub min_length: i64,
}

impl Default for DiagCodedTypeMinMax {
    fn default() -> Self {
        DiagCodedTypeMinMax {
            termination: TerminationType::default(),
            base_type_encoding: None,
            base_data_type: BaseDataType::default(),
            is_highlow_byte_order: true,
            max_length: 0,
            min_length: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagCodedTypePl {
    #[serde(
        rename = "@BASE-TYPE-ENCODING",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub base_type_encoding: Option<String>,
    #[serde(rename = "@BASE-DATA-TYPE", default)]
    pub base_data_type: BaseDataType,
    #[serde(
        rename = "LENGTH-KEY-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub length_key_ref: Option<IdRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagCodedType {
    /// `xsi:type="STANDARD-LENGTH-TYPE"`.
    StandardLength(DiagCodedTypeStd),
    /// `xsi:type="LEADING-LENGTH-INFO-TYPE"`.
    LeadingLengthInfo(DiagCodedTypeLl),
    /// `xsi:type="MIN-MAX-LENGTH-TYPE"`.
    MinMaxLength(DiagCodedTypeMinMax),
    /// `xsi:type="PARAM-LENGTH-INFO-TYPE"`.
    ParamLengthInfo(DiagCodedTypePl),
}

impl DiagCodedType {
    pub fn type_name(&self) -> &'static str {
        match self {
            DiagCodedType::StandardLength(_) => "STANDARD-LENGTH-TYPE",
            DiagCodedType::LeadingLengthInfo(_) => "LEADING-LENGTH-INFO-TYPE",
            DiagCodedType::MinMaxLength(_) => "MIN-MAX-LENGTH-TYPE",
            DiagCodedType::ParamLengthInfo(_) => "PARAM-LENGTH-INFO-TYPE",
        }
    }

    pub fn base_data_type(&self) -> BaseDataType {
        match self {
            DiagCodedType::StandardLength(v) => v.base_data_type,
            DiagCodedType::LeadingLengthInfo(v) => v.base_data_type,
            DiagCodedType::MinMaxLength(v) => v.base_data_type,
            DiagCodedType::ParamLengthInfo(v) => v.base_data_type,
        }
    }

    pub fn is_highlow_byte_order(&self) -> bool {
        match self {
            DiagCodedType::StandardLength(v) => v.is_highlow_byte_order,
            DiagCodedType::LeadingLengthInfo(v) => v.is_highlow_byte_order,
            DiagCodedType::MinMaxLength(v) => v.is_highlow_byte_order,
            DiagCodedType::ParamLengthInfo(_) => true,
        }
    }

    pub fn as_std(&self) -> Option<&DiagCodedTypeStd> {
        match self {
            DiagCodedType::StandardLength(v) => Some(v),
            _ => None,
        }
    }
}

impl fmt::Display for DiagCodedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.type_name())
    }
}

#[derive(Serialize, Deserialize)]
struct RawDiagCodedType {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(
        rename = "@TERMINATION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    termination: Option<String>,
    #[serde(
        rename = "@BASE-TYPE-ENCODING",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    base_type_encoding: Option<String>,
    #[serde(
        rename = "@BASE-DATA-TYPE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    base_data_type: Option<BaseDataType>,
    #[serde(
        rename = "@IS-HIGHLOW-BYTE-ORDER",
        default = "default_true",
        skip_serializing_if = "is_true"
    )]
    is_highlow: bool,
    #[serde(rename = "BIT-LENGTH", default, skip_serializing_if = "is_zero")]
    bit_length: i64,
    #[serde(
        rename = "MAX-LENGTH",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    max_length: Option<i64>,
    #[serde(
        rename = "MIN-LENGTH",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    min_length: Option<i64>,
    #[serde(
        rename = "LENGTH-KEY-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    length_key_ref: Option<IdRef>,
}

impl From<&DiagCodedType> for RawDiagCodedType {
    fn from(v: &DiagCodedType) -> Self {
        match v {
            DiagCodedType::StandardLength(v) => RawDiagCodedType {
                xsi_type: Some("STANDARD-LENGTH-TYPE".into()),
                termination: None,
                base_type_encoding: v.base_type_encoding.clone(),
                base_data_type: Some(v.base_data_type),
                is_highlow: v.is_highlow_byte_order,
                bit_length: v.bit_length,
                max_length: None,
                min_length: None,
                length_key_ref: None,
            },
            DiagCodedType::LeadingLengthInfo(v) => RawDiagCodedType {
                xsi_type: Some("LEADING-LENGTH-INFO-TYPE".into()),
                termination: None,
                base_type_encoding: v.base_type_encoding.clone(),
                base_data_type: Some(v.base_data_type),
                is_highlow: v.is_highlow_byte_order,
                bit_length: v.bit_length as i64,
                max_length: None,
                min_length: None,
                length_key_ref: None,
            },
            DiagCodedType::MinMaxLength(v) => RawDiagCodedType {
                xsi_type: Some("MIN-MAX-LENGTH-TYPE".into()),
                termination: Some(v.termination.as_str().into()),
                base_type_encoding: v.base_type_encoding.clone(),
                base_data_type: Some(v.base_data_type),
                is_highlow: v.is_highlow_byte_order,
                bit_length: 0,
                max_length: Some(v.max_length),
                min_length: Some(v.min_length),
                length_key_ref: None,
            },
            DiagCodedType::ParamLengthInfo(v) => RawDiagCodedType {
                xsi_type: Some("PARAM-LENGTH-INFO-TYPE".into()),
                termination: None,
                base_type_encoding: v.base_type_encoding.clone(),
                base_data_type: Some(v.base_data_type),
                is_highlow: true,
                bit_length: 0,
                max_length: None,
                min_length: None,
                length_key_ref: v.length_key_ref.clone(),
            },
        }
    }
}

impl RawDiagCodedType {
    fn into_typed(self) -> std::result::Result<DiagCodedType, String> {
        let bdt = self.base_data_type.unwrap_or_default();
        let bte = self.base_type_encoding;
        match self.xsi_type.as_deref() {
            Some("STANDARD-LENGTH-TYPE") => Ok(DiagCodedType::StandardLength(DiagCodedTypeStd {
                base_type_encoding: bte,
                base_data_type: bdt,
                is_highlow_byte_order: self.is_highlow,
                bit_length: self.bit_length,
            })),
            Some("LEADING-LENGTH-INFO-TYPE") => {
                Ok(DiagCodedType::LeadingLengthInfo(DiagCodedTypeLl {
                    base_type_encoding: bte,
                    base_data_type: bdt,
                    is_highlow_byte_order: self.is_highlow,
                    bit_length: self.bit_length as u64,
                }))
            }
            Some("MIN-MAX-LENGTH-TYPE") => Ok(DiagCodedType::MinMaxLength(DiagCodedTypeMinMax {
                termination: self
                    .termination
                    .as_deref()
                    .map_or(TerminationType::default(), TerminationType::from_odx_name),
                base_type_encoding: bte,
                base_data_type: bdt,
                is_highlow_byte_order: self.is_highlow,
                max_length: self.max_length.unwrap_or(0),
                min_length: self.min_length.unwrap_or(0),
            })),
            Some("PARAM-LENGTH-INFO-TYPE") => Ok(DiagCodedType::ParamLengthInfo(DiagCodedTypePl {
                base_type_encoding: bte,
                base_data_type: bdt,
                length_key_ref: self.length_key_ref,
            })),
            other => Err(format!(
                "DIAG-CODED-TYPE: unsupported or missing xsi:type {other:?}"
            )),
        }
    }

    fn into_std(self) -> DiagCodedTypeStd {
        DiagCodedTypeStd {
            base_type_encoding: self.base_type_encoding,
            base_data_type: self.base_data_type.unwrap_or_default(),
            is_highlow_byte_order: self.is_highlow,
            bit_length: self.bit_length,
        }
    }

    fn no_xsi(mut self) -> Self {
        self.xsi_type = None;
        self
    }
}

impl Serialize for DiagCodedType {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        RawDiagCodedType::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DiagCodedType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        RawDiagCodedType::deserialize(d)?
            .into_typed()
            .map_err(serde::de::Error::custom)
    }
}

par_base_def_struct! {
    pub struct ParValue {
        #[serde(rename = "DOP-REF", default, skip_serializing_if = "Option::is_none")]
        pub dop_ref: Option<IdRef>,
        #[serde(rename = "DOP-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub dop_snref: Option<SnRef>,
    }
}

par_base_def_struct! {
    pub struct ParCodedConst {
        #[serde(rename = "CODED-VALUE", default)]
        pub coded_value: i64,
        #[serde(
            rename = "DIAG-CODED-TYPE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub diag_coded_type: Option<DiagCodedType>,
    }
}

par_base_def_struct! {
    pub struct ParPhysConst {
        #[serde(rename = "DOP-REF", default, skip_serializing_if = "Option::is_none")]
        pub dop_ref: Option<IdRef>,
        #[serde(rename = "DOP-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub dop_snref: Option<SnRef>,
        #[serde(
            rename = "PHYS-CONSTANT-VALUE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub phys_constant_value: Option<String>,
    }
}

par_base_def_struct! {
    pub struct ParReserved {
        #[serde(
            rename = "BIT-LENGTH",
            default = "default_eight",
            skip_serializing_if = "is_eight"
        )]
        pub bit_length: u64,
    }
}

par_base_struct! {
    /// `DYNAMIC` parameter (`xsi:type="DYNAMIC"`).
    pub struct ParDynamic {}
}

par_base_struct! {
    /// `SYSTEM` parameter (`xsi:type="SYSTEM"`); derives directly from the parameter
    /// base and carries its own `BYTE-POSITION`/`BIT-POSITION`.
    pub struct ParSystem {
        attrs {
            /// `SYSPARAM` attribute.
            #[serde(rename = "@SYSPARAM", default, skip_serializing_if = "Option::is_none")]
            pub sysparam: Option<SysParmType>,
        }
        /// `BYTE-POSITION` element.
        #[serde(rename = "BYTE-POSITION", default)]
        pub byte_position: i64,
        /// `BIT-POSITION` element (unsigned; omitted when zero).
        #[serde(rename = "BIT-POSITION", default, skip_serializing_if = "is_zero")]
        pub bit_position: u32,
        /// `DOP-REF` element.
        #[serde(rename = "DOP-REF", default, skip_serializing_if = "Option::is_none")]
        pub dop_ref: Option<IdRef>,
        /// `DOP-SNREF` element.
        #[serde(rename = "DOP-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub dop_snref: Option<SnRef>,
    }
}

par_id_base_struct! {
    /// `LENGTH-KEY` parameter (`xsi:type="LENGTH-KEY"`).
    pub struct ParLength {
        /// `DOP-REF` element.
        #[serde(rename = "DOP-REF", default, skip_serializing_if = "Option::is_none")]
        pub dop_ref: Option<IdRef>,
        /// `DOP-SNREF` element.
        #[serde(rename = "DOP-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub dop_snref: Option<SnRef>,
    }
}

par_id_base_struct! {
    /// `TABLE-KEY` parameter (`xsi:type="TABLE-KEY"`).
    pub struct ParTableKey {
        /// `TABLE-REF` element.
        #[serde(rename = "TABLE-REF", default, skip_serializing_if = "Option::is_none")]
        pub table_ref: Option<IdRef>,
        /// `TABLE-ROW-REF` element.
        #[serde(rename = "TABLE-ROW-REF", default, skip_serializing_if = "Option::is_none")]
        pub table_row_ref: Option<IdRef>,
    }
}

/// Serde conversion of `RowFragment` for the `ROW_FRAGMENT_` element.
/// Note: the element name is the underscored `ROW_FRAGMENT_` exactly as declared
/// (unlike ISO 22901's `ROW-FRAGMENT`); no renaming attribute is applied.
mod row_fragment_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &super::RowFragment, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<super::RowFragment, D::Error> {
        let s = String::deserialize(d)?;
        Ok(super::RowFragment::from_odx_name(&s))
    }
}

par_id_base_struct! {
    /// `TABLE-ENTRY` parameter (`xsi:type="TABLE-ENTRY"`).
    pub struct ParTableEntry {
        /// `TABLE-ROW-REF` element.
        #[serde(rename = "TABLE-ROW-REF", default, skip_serializing_if = "Option::is_none")]
        pub table_row_ref: Option<IdRef>,
        /// `ROW_FRAGMENT_` element (underscored name serialized as-is, always written).
        #[serde(rename = "ROW_FRAGMENT_", default, with = "row_fragment_serde")]
        pub row_fragment: RowFragment,
    }
}

par_base_def_struct! {
    /// `TABLE-STRUCT` parameter (`xsi:type="TABLE-STRUCT"`).
    pub struct ParTableStruct {
        /// `TABLE-KEY-REF` element.
        #[serde(rename = "TABLE-KEY-REF", default, skip_serializing_if = "Option::is_none")]
        pub table_key_ref: Option<IdRef>,
    }
}

/// `CODED-VALUES` wrapper element (a plain list of values, always written).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodedValues {
    /// `CODED-VALUE` entries.
    #[serde(rename = "CODED-VALUE", default)]
    pub items: Vec<i64>,
}

par_base_def_struct! {
    pub struct ParNrcConst {
        #[serde(rename = "CODED-VALUES", default)]
        pub coded_values: CodedValues,
        #[serde(
            rename = "DIAG-CODED-TYPE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub diag_coded_type: Option<DiagCodedTypeStd>,
    }
}

impl ParNrcConst {
    pub fn error_code(&self) -> u8 {
        self.coded_values.items.first().map_or(0, |v| *v as u8)
    }
}

par_base_def_struct! {
    pub struct ParMatchRequestParam {
        #[serde(rename = "REQUEST-BYTE-POS", default)]
        pub request_byte_pos: u8,
        #[serde(rename = "BYTE-LENGTH", default)]
        pub byte_length: u8,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// `xsi:type="VALUE"`.
    Value(ParValue),
    /// `xsi:type="CODED-CONST"`.
    CodedConst(ParCodedConst),
    /// `xsi:type="PHYS-CONST"`.
    PhysConst(ParPhysConst),
    /// `xsi:type="RESERVED"`.
    Reserved(ParReserved),
    /// `xsi:type="TABLE-KEY"`.
    TableKey(ParTableKey),
    /// `xsi:type="TABLE-STRUCT"`.
    TableStruct(ParTableStruct),
    /// `xsi:type="TABLE-ENTRY"`.
    TableEntry(ParTableEntry),
    /// `xsi:type="LENGTH-KEY"`.
    LengthKey(ParLength),
    /// `xsi:type="NRC-CONST"`.
    NrcConst(ParNrcConst),
    /// `xsi:type="MATCHING-REQUEST-PARAM"`.
    MatchingRequestParam(ParMatchRequestParam),
    /// `xsi:type="SYSTEM"`.
    System(ParSystem),
    /// `xsi:type="DYNAMIC"`.
    Dynamic(ParDynamic),
}

impl Param {
    pub fn type_name(&self) -> &'static str {
        match self {
            Param::Value(_) => "VALUE",
            Param::CodedConst(_) => "CODED-CONST",
            Param::PhysConst(_) => "PHYS-CONST",
            Param::Reserved(_) => "RESERVED",
            Param::TableKey(_) => "TABLE-KEY",
            Param::TableStruct(_) => "TABLE-STRUCT",
            Param::TableEntry(_) => "TABLE-ENTRY",
            Param::LengthKey(_) => "LENGTH-KEY",
            Param::NrcConst(_) => "NRC-CONST",
            Param::MatchingRequestParam(_) => "MATCHING-REQUEST-PARAM",
            Param::System(_) => "SYSTEM",
            Param::Dynamic(_) => "DYNAMIC",
        }
    }

    /// `SHORT-NAME`.
    pub fn short_name(&self) -> Option<&str> {
        match self {
            Param::Value(v) => v.short_name.as_deref(),
            Param::CodedConst(v) => v.short_name.as_deref(),
            Param::PhysConst(v) => v.short_name.as_deref(),
            Param::Reserved(v) => v.short_name.as_deref(),
            Param::TableKey(v) => v.short_name.as_deref(),
            Param::TableStruct(v) => v.short_name.as_deref(),
            Param::TableEntry(v) => v.short_name.as_deref(),
            Param::LengthKey(v) => v.short_name.as_deref(),
            Param::NrcConst(v) => v.short_name.as_deref(),
            Param::MatchingRequestParam(v) => v.short_name.as_deref(),
            Param::System(v) => v.short_name.as_deref(),
            Param::Dynamic(v) => v.short_name.as_deref(),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Param::Value(v) => v.display_name(),
            Param::CodedConst(v) => v.display_name(),
            Param::PhysConst(v) => v.display_name(),
            Param::Reserved(v) => v.display_name(),
            Param::TableKey(v) => v.display_name(),
            Param::TableStruct(v) => v.display_name(),
            Param::TableEntry(v) => v.display_name(),
            Param::LengthKey(v) => v.display_name(),
            Param::NrcConst(v) => v.display_name(),
            Param::MatchingRequestParam(v) => v.display_name(),
            Param::System(v) => v.display_name(),
            Param::Dynamic(v) => v.display_name(),
        }
    }

    pub fn is_base_def(&self) -> bool {
        !matches!(self, Param::Dynamic(_) | Param::System(_))
    }

    pub fn byte_position(&self) -> i64 {
        match self {
            Param::Value(v) => v.byte_position,
            Param::CodedConst(v) => v.byte_position,
            Param::PhysConst(v) => v.byte_position,
            Param::Reserved(v) => v.byte_position,
            Param::TableKey(v) => v.byte_position,
            Param::TableStruct(v) => v.byte_position,
            Param::TableEntry(v) => v.byte_position,
            Param::LengthKey(v) => v.byte_position,
            Param::NrcConst(v) => v.byte_position,
            Param::MatchingRequestParam(v) => v.byte_position,
            Param::System(v) => v.byte_position,
            Param::Dynamic(_) => 0,
        }
    }

    /// `BIT-POSITION`.
    pub fn bit_position(&self) -> i64 {
        match self {
            Param::Value(v) => v.bit_position,
            Param::CodedConst(v) => v.bit_position,
            Param::PhysConst(v) => v.bit_position,
            Param::Reserved(v) => v.bit_position,
            Param::TableKey(v) => v.bit_position,
            Param::TableStruct(v) => v.bit_position,
            Param::TableEntry(v) => v.bit_position,
            Param::LengthKey(v) => v.bit_position,
            Param::NrcConst(v) => v.bit_position,
            Param::MatchingRequestParam(v) => v.bit_position,
            Param::System(v) => i64::from(v.bit_position),
            Param::Dynamic(_) => 0,
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Param::TableKey(v) => v.id.as_deref(),
            Param::TableEntry(v) => v.id.as_deref(),
            Param::LengthKey(v) => v.id.as_deref(),
            _ => None,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RawParam {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "@SEMANTIC", default, skip_serializing_if = "Option::is_none")]
    semantic: Option<String>,
    #[serde(rename = "@SYSPARAM", default, skip_serializing_if = "Option::is_none")]
    sysparam: Option<SysParmType>,
    #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
    desc: Option<String>,
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    long_name: Option<String>,
    #[serde(
        rename = "BYTE-POSITION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    byte_position: Option<i64>,
    #[serde(
        rename = "BIT-POSITION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    bit_position: Option<i64>,
    #[serde(
        rename = "PHYSICAL-DEFAULT-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    physical_default_value: Option<String>,
    #[serde(
        rename = "CODED-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    coded_value: Option<i64>,
    #[serde(
        rename = "DIAG-CODED-TYPE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    diag_coded_type: Option<RawDiagCodedType>,
    #[serde(
        rename = "PHYS-CONSTANT-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    phys_constant_value: Option<String>,
    #[serde(
        rename = "BIT-LENGTH",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    bit_length: Option<u64>,
    #[serde(rename = "DOP-REF", default, skip_serializing_if = "Option::is_none")]
    dop_ref: Option<IdRef>,
    #[serde(rename = "DOP-SNREF", default, skip_serializing_if = "Option::is_none")]
    dop_snref: Option<SnRef>,
    #[serde(rename = "TABLE-REF", default, skip_serializing_if = "Option::is_none")]
    table_ref: Option<IdRef>,
    #[serde(
        rename = "TABLE-ROW-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    table_row_ref: Option<IdRef>,
    #[serde(
        rename = "TABLE-KEY-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    table_key_ref: Option<IdRef>,
    #[serde(
        rename = "ROW_FRAGMENT_",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    row_fragment: Option<String>,
    #[serde(
        rename = "CODED-VALUES",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    coded_values: Option<CodedValues>,
    #[serde(
        rename = "REQUEST-BYTE-POS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    request_byte_pos: Option<u8>,
    #[serde(
        rename = "BYTE-LENGTH",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    byte_length: Option<u8>,
}

struct RawParamBase {
    desc: Option<String>,
    short_name: Option<String>,
    long_name: Option<String>,
    semantic: Option<String>,
    byte_position: Option<i64>,
    bit_position: Option<i64>,
    physical_default_value: Option<String>,
}

impl RawParam {
    fn with_base(mut self, xsi: &str, b: RawParamBase) -> Self {
        self.xsi_type = Some(xsi.to_owned());
        self.desc = b.desc;
        self.short_name = b.short_name;
        self.long_name = b.long_name;
        self.semantic = b.semantic;
        self.byte_position = b.byte_position;
        self.bit_position = b.bit_position;
        self.physical_default_value = b.physical_default_value;
        self
    }

    fn base_of_def(
        semantic: &Option<String>,
        byte_position: i64,
        bit_position: i64,
        physical_default_value: &Option<String>,
        desc: &Option<String>,
        short_name: &Option<String>,
        long_name: &Option<String>,
    ) -> RawParamBase {
        RawParamBase {
            desc: desc.clone(),
            short_name: short_name.clone(),
            long_name: long_name.clone(),
            semantic: semantic.clone(),
            byte_position: Some(byte_position),
            bit_position: if bit_position == 0 {
                None
            } else {
                Some(bit_position)
            },
            physical_default_value: physical_default_value.clone(),
        }
    }
}

impl From<&Param> for RawParam {
    fn from(p: &Param) -> Self {
        match p {
            Param::Value(v) => RawParam {
                dop_ref: v.dop_ref.clone(),
                dop_snref: v.dop_snref.clone(),
                ..Default::default()
            }
            .with_base(
                "VALUE",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::CodedConst(v) => RawParam {
                coded_value: Some(v.coded_value),
                diag_coded_type: v.diag_coded_type.as_ref().map(RawDiagCodedType::from),
                ..Default::default()
            }
            .with_base(
                "CODED-CONST",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::PhysConst(v) => RawParam {
                dop_ref: v.dop_ref.clone(),
                dop_snref: v.dop_snref.clone(),
                phys_constant_value: v.phys_constant_value.clone(),
                ..Default::default()
            }
            .with_base(
                "PHYS-CONST",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::Reserved(v) => RawParam {
                bit_length: if v.bit_length == 8 {
                    None
                } else {
                    Some(v.bit_length)
                },
                ..Default::default()
            }
            .with_base(
                "RESERVED",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::TableKey(v) => RawParam {
                id: v.id.clone(),
                table_ref: v.table_ref.clone(),
                table_row_ref: v.table_row_ref.clone(),
                ..Default::default()
            }
            .with_base(
                "TABLE-KEY",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::TableStruct(v) => RawParam {
                table_key_ref: v.table_key_ref.clone(),
                ..Default::default()
            }
            .with_base(
                "TABLE-STRUCT",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::TableEntry(v) => RawParam {
                id: v.id.clone(),
                table_row_ref: v.table_row_ref.clone(),
                row_fragment: Some(v.row_fragment.as_str().to_owned()),
                ..Default::default()
            }
            .with_base(
                "TABLE-ENTRY",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::LengthKey(v) => RawParam {
                id: v.id.clone(),
                dop_ref: v.dop_ref.clone(),
                dop_snref: v.dop_snref.clone(),
                ..Default::default()
            }
            .with_base(
                "LENGTH-KEY",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::NrcConst(v) => RawParam {
                coded_values: Some(v.coded_values.clone()),
                diag_coded_type: v.diag_coded_type.as_ref().map(|d| {
                    RawDiagCodedType::from(&DiagCodedType::StandardLength(d.clone())).no_xsi()
                }),
                ..Default::default()
            }
            .with_base(
                "NRC-CONST",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::MatchingRequestParam(v) => RawParam {
                request_byte_pos: Some(v.request_byte_pos),
                byte_length: Some(v.byte_length),
                ..Default::default()
            }
            .with_base(
                "MATCHING-REQUEST-PARAM",
                RawParam::base_of_def(
                    &v.semantic,
                    v.byte_position,
                    v.bit_position,
                    &v.physical_default_value,
                    &v.desc,
                    &v.short_name,
                    &v.long_name,
                ),
            ),
            Param::System(v) => RawParam {
                sysparam: v.sysparam,
                dop_ref: v.dop_ref.clone(),
                dop_snref: v.dop_snref.clone(),
                ..Default::default()
            }
            .with_base(
                "SYSTEM",
                RawParamBase {
                    desc: v.desc.clone(),
                    short_name: v.short_name.clone(),
                    long_name: v.long_name.clone(),
                    semantic: None,
                    byte_position: Some(v.byte_position),
                    bit_position: if v.bit_position == 0 {
                        None
                    } else {
                        Some(i64::from(v.bit_position))
                    },
                    physical_default_value: None,
                },
            ),
            Param::Dynamic(v) => RawParam {
                ..Default::default()
            }
            .with_base(
                "DYNAMIC",
                RawParamBase {
                    desc: v.desc.clone(),
                    short_name: v.short_name.clone(),
                    long_name: v.long_name.clone(),
                    semantic: None,
                    byte_position: None,
                    bit_position: None,
                    physical_default_value: None,
                },
            ),
        }
    }
}

impl Serialize for Param {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        RawParam::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Param {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = RawParam::deserialize(d)?;
        macro_rules! base {
            () => {
                (
                    raw.semantic.clone(),
                    raw.byte_position.unwrap_or(0),
                    raw.bit_position.unwrap_or(0),
                    raw.physical_default_value.clone(),
                    raw.desc.clone(),
                    raw.short_name.clone(),
                    raw.long_name.clone(),
                )
            };
        }
        match raw.xsi_type.as_deref() {
            Some("VALUE") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::Value(ParValue {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    dop_ref: raw.dop_ref,
                    dop_snref: raw.dop_snref,
                }))
            }
            Some("CODED-CONST") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::CodedConst(ParCodedConst {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    coded_value: raw.coded_value.unwrap_or(0),
                    diag_coded_type: raw
                        .diag_coded_type
                        .map(RawDiagCodedType::into_typed)
                        .transpose()
                        .map_err(serde::de::Error::custom)?,
                }))
            }
            Some("PHYS-CONST") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::PhysConst(ParPhysConst {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    dop_ref: raw.dop_ref,
                    dop_snref: raw.dop_snref,
                    phys_constant_value: raw.phys_constant_value,
                }))
            }
            Some("RESERVED") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::Reserved(ParReserved {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    bit_length: raw.bit_length.unwrap_or(8),
                }))
            }
            Some("TABLE-KEY") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::TableKey(ParTableKey {
                    id: raw.id,
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    table_ref: raw.table_ref,
                    table_row_ref: raw.table_row_ref,
                }))
            }
            Some("TABLE-STRUCT") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::TableStruct(ParTableStruct {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    table_key_ref: raw.table_key_ref,
                }))
            }
            Some("TABLE-ENTRY") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::TableEntry(ParTableEntry {
                    id: raw.id,
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    table_row_ref: raw.table_row_ref,
                    row_fragment: raw
                        .row_fragment
                        .as_deref()
                        .map_or(RowFragment::default(), RowFragment::from_odx_name),
                }))
            }
            Some("LENGTH-KEY") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::LengthKey(ParLength {
                    id: raw.id,
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    dop_ref: raw.dop_ref,
                    dop_snref: raw.dop_snref,
                }))
            }
            Some("NRC-CONST") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::NrcConst(ParNrcConst {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    coded_values: raw.coded_values.unwrap_or_default(),
                    diag_coded_type: raw.diag_coded_type.map(RawDiagCodedType::into_std),
                }))
            }
            Some("MATCHING-REQUEST-PARAM") => {
                let (semantic, bp, bit, pdv, desc, sn, ln) = base!();
                Ok(Param::MatchingRequestParam(ParMatchRequestParam {
                    semantic,
                    byte_position: bp,
                    bit_position: bit,
                    physical_default_value: pdv,
                    desc,
                    short_name: sn,
                    long_name: ln,
                    request_byte_pos: raw.request_byte_pos.unwrap_or(0),
                    byte_length: raw.byte_length.unwrap_or(0),
                }))
            }
            Some("SYSTEM") => Ok(Param::System(ParSystem {
                sysparam: raw.sysparam,
                byte_position: raw.byte_position.unwrap_or(0),
                bit_position: raw.bit_position.unwrap_or(0) as u32,
                dop_ref: raw.dop_ref,
                dop_snref: raw.dop_snref,
                desc: raw.desc,
                short_name: raw.short_name,
                long_name: raw.long_name,
            })),
            Some("DYNAMIC") => Ok(Param::Dynamic(ParDynamic {
                desc: raw.desc,
                short_name: raw.short_name,
                long_name: raw.long_name,
            })),
            other => Err(serde::de::Error::custom(format!(
                "PARAM: unsupported or missing xsi:type {other:?}"
            ))),
        }
    }
}

// ============================================================================
// Parameter containers: ParamContainer / Structure / EnvDataParamContainer
// ============================================================================

/// `PARAMS` wrapper element (a polymorphic `PARAM` item list, always written
/// wrapped under the member name `PARAMS`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// `PARAM` entries (xsi:type polymorphism).
    #[serde(rename = "PARAM", default)]
    pub items: Vec<Param>,
}

named_desc_id_struct! {
    pub struct ParamContainer {
        #[serde(rename = "PARAMS", default)]
        pub params: Params,
    }
}

impl ParamContainer {
    /// Sorts the parameters by (`BYTE-POSITION`, `BIT-POSITION`); parameters that
    /// are not a `ParBaseDef` (DYNAMIC/SYSTEM) sort last.
    /// A stable sort is used, so non-def parameters keep their original relative
    /// order (real-world files hardly ever contain more than one non-def parameter).
    pub fn sort_params(&mut self) {
        self.params
            .items
            .sort_by_key(|p| (!p.is_base_def(), p.byte_position(), p.bit_position()));
    }
}

named_desc_id_struct! {
    pub struct Structure {
        attrs {
            #[serde(rename = "@IS-VISIBLE", default)]
            pub is_visible: bool,
        }
        #[serde(rename = "BYTE-SIZE", default, skip_serializing_if = "is_zero")]
        pub byte_size: i64,
        #[serde(rename = "PARAMS", default)]
        pub params: Params,
    }
}

impl Structure {
    pub fn sort_params(&mut self) {
        self.params
            .items
            .sort_by_key(|p| (!p.is_base_def(), p.byte_position(), p.bit_position()));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtcValues {
    #[serde(rename = "DTC-VALUE", default)]
    pub items: Vec<u32>,
}

named_desc_id_struct! {
    pub struct EnvDataParamContainer {
        #[serde(rename = "PARAMS", default)]
        pub params: Params,
        #[serde(rename = "DTC-VALUES", default)]
        pub dtcs: DtcValues,
    }
}

named_desc_id_struct! {
    pub struct DataObjectProp {
        #[serde(rename = "UNIT-REF", default, skip_serializing_if = "Option::is_none")]
        pub unit_ref: Option<IdRef>,
        #[serde(rename = "COMPU-METHOD", default, skip_serializing_if = "Option::is_none")]
        pub compu_method: Option<CompuMethod>,
        #[serde(
            rename = "DIAG-CODED-TYPE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub diag_coded_type: Option<DiagCodedType>,
        #[serde(rename = "PHYSICAL-TYPE", default)]
        pub physical_type: PhysicalType,
        #[serde(
            rename = "INTERNAL-CONSTR",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub internal_constr: Option<InternalConstr>,
    }
}

named_id_struct! {
    pub struct Dtc {
        #[serde(rename = "TROUBLE-CODE", default)]
        pub trouble_code: i32,
        #[serde(
            rename = "DISPLAY-TROUBLE-CODE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub display_trouble_code: Option<String>,
        #[serde(rename = "TEXT", default, skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DtcItem {
    #[serde(rename = "DTC")]
    Dtc(Dtc),
    #[serde(rename = "DTC-REF")]
    DtcRef(IdRef),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DtcItems {
    #[serde(rename = "$value", default)]
    pub items: Vec<DtcItem>,
}

named_desc_id_struct! {
    pub struct DtcDop {
        #[serde(rename = "UNIT-REF", default, skip_serializing_if = "Option::is_none")]
        pub unit_ref: Option<IdRef>,
        #[serde(rename = "COMPU-METHOD", default, skip_serializing_if = "Option::is_none")]
        pub compu_method: Option<CompuMethod>,
        #[serde(
            rename = "DIAG-CODED-TYPE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub diag_coded_type: Option<DiagCodedType>,
        #[serde(rename = "PHYSICAL-TYPE", default)]
        pub physical_type: PhysicalType,
        #[serde(
            rename = "INTERNAL-CONSTR",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub internal_constr: Option<InternalConstr>,
        #[serde(rename = "DTCS", default)]
        pub dtcs: DtcItems,
    }
}

named_id_struct! {
    pub struct EnvDataDesc {
        #[serde(rename = "PARAM-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub param_snref: Option<SnRef>,
        #[serde(rename = "ENV-DATA-REFS", default)]
        pub env_data_refs: EnvDataRefs,
        #[serde(rename = "ENV-DATAS", default)]
        pub env_datas: EnvDatas,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvDataRefs {
    #[serde(rename = "ENV-DATA-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvDatas {
    #[serde(rename = "ENV-DATA", default)]
    pub items: Vec<EnvDataParamContainer>,
}

named_id_struct! {
    pub struct EndOfPduField {
        #[serde(
            rename = "BASIC-STRUCTURE-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub basic_structure_ref: Option<IdRef>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetermineNumberOfItems {
    #[serde(
        rename = "DATA-OBJECT-PROP-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub data_object_prop_ref: Option<IdRef>,
    #[serde(rename = "BYTE-POSITION", default)]
    pub byte_position: i64,
}

named_id_struct! {
    pub struct DynamicLengthField {
        #[serde(
            rename = "BASIC-STRUCTURE-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub basic_structure_ref: Option<IdRef>,
        #[serde(rename = "OFFSET", default)]
        pub offset: i64,
        #[serde(
            rename = "DETERMINE-NUMBER-OF-ITEMS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub determine_number_of_items: Option<DetermineNumberOfItems>,
    }
}

named_desc_id_struct! {
    pub struct StaticField {
        attrs {
            #[serde(rename = "@IS-VISIBLE", default)]
            pub is_visible: bool,
        }
        #[serde(
            rename = "BASIC-STRUCTURE-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub basic_structure_ref: Option<IdRef>,
        #[serde(rename = "FIXED-NUMBER-OF-ITEMS", default)]
        pub fixed_number_of_items: i64,
        #[serde(rename = "ITEM-BYTE-SIZE", default)]
        pub item_byte_size: i64,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchKey {
    #[serde(
        rename = "DATA-OBJECT-PROP-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub data_object_prop_ref: Option<IdRef>,
    #[serde(rename = "BYTE-POSITION", default)]
    pub byte_position: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultCase {
    #[serde(
        rename = "STRUCTURE-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub structure_ref: Option<IdRef>,
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    #[serde(
        rename = "STRUCTURE-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub structure_ref: Option<IdRef>,
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    #[serde(
        rename = "LOWER-LIMIT",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub lower_limit: Option<String>,
    #[serde(
        rename = "UPPER-LIMIT",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub upper_limit: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cases {
    #[serde(rename = "CASE", default)]
    pub items: Vec<Case>,
}

named_desc_id_struct! {
    pub struct Mux {
        #[serde(rename = "BYTE-POSITION", default)]
        pub byte_position: i64,
        #[serde(rename = "SWITCH-KEY", default, skip_serializing_if = "Option::is_none")]
        pub switch_key: Option<SwitchKey>,
        #[serde(rename = "DEFAULT-CASE", default, skip_serializing_if = "Option::is_none")]
        pub default_case: Option<DefaultCase>,
        #[serde(rename = "CASES", default, skip_serializing_if = "Option::is_none")]
        pub cases: Option<Cases>,
    }
}

named_id_struct! {
    pub struct Table {
        #[serde(rename = "KEY-DOP-REF", default, skip_serializing_if = "Option::is_none")]
        pub key_dop_ref: Option<IdRef>,
        #[serde(rename = "TABLE-ROW-REF", default, skip_serializing_if = "Vec::is_empty")]
        pub table_row_refs: Vec<IdRef>,
        #[serde(rename = "TABLE-ROW", default, skip_serializing_if = "Vec::is_empty")]
        pub rows: Vec<TableRow>,
    }
}

named_id_struct! {
    pub struct TableRow {
        #[serde(rename = "STRUCTURE-REF", default, skip_serializing_if = "Option::is_none")]
        pub structure_ref: Option<IdRef>,
        #[serde(rename = "KEY", default, skip_serializing_if = "Option::is_none")]
        pub key: Option<String>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DtcDops {
    #[serde(rename = "DTC-DOP", default)]
    pub items: Vec<DtcDop>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvDataDescs {
    #[serde(rename = "ENV-DATA-DESC", default)]
    pub items: Vec<EnvDataDesc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataObjectProps {
    #[serde(rename = "DATA-OBJECT-PROP", default)]
    pub items: Vec<DataObjectProp>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Structures {
    #[serde(rename = "STRUCTURE", default)]
    pub items: Vec<Structure>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StaticFields {
    #[serde(rename = "STATIC-FIELD", default)]
    pub items: Vec<StaticField>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndOfPduFields {
    #[serde(rename = "END-OF-PDU-FIELD", default)]
    pub items: Vec<EndOfPduField>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Muxs {
    #[serde(rename = "MUX", default)]
    pub items: Vec<Mux>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DynamicLengthFields {
    #[serde(rename = "DYNAMIC-LENGTH-FIELD", default)]
    pub items: Vec<DynamicLengthField>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tables {
    #[serde(rename = "TABLE", default)]
    pub items: Vec<Table>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagDataDictionarySpec {
    #[serde(rename = "DTC-DOPS", default)]
    pub dtc_dops: DtcDops,
    #[serde(rename = "ENV-DATA-DESCS", default)]
    pub env_data_descs: EnvDataDescs,
    #[serde(rename = "ENV-DATAS", default)]
    pub env_datas: EnvDatas,
    #[serde(rename = "DATA-OBJECT-PROPS", default)]
    pub data_object_props: DataObjectProps,
    #[serde(rename = "STRUCTURES", default)]
    pub structures: Structures,
    #[serde(rename = "STATIC-FIELDS", default)]
    pub static_fields: StaticFields,
    #[serde(rename = "END-OF-PDU-FIELDS", default)]
    pub end_of_pdu_fields: EndOfPduFields,
    #[serde(rename = "MUXS", default)]
    pub muxs: Muxs,
    #[serde(rename = "DYNAMIC-LENGTH-FIELDS", default)]
    pub dynamic_length_fields: DynamicLengthFields,
    #[serde(rename = "UNIT-SPEC", default, skip_serializing_if = "Option::is_none")]
    pub unit_spec: Option<UnitSpec>,
    #[serde(rename = "TABLES", default)]
    pub tables: Tables,
}

#[derive(Debug, Clone, Default)]
pub struct DiagDataDictSpec<'a> {
    pub dtc_dops: Vec<&'a DtcDop>,
    pub env_data_descs: Vec<&'a EnvDataDesc>,
    pub data_object_props: Vec<&'a DataObjectProp>,
    pub structures: Vec<&'a Structure>,
    pub static_fields: Vec<&'a StaticField>,
    pub end_of_pdu_fields: Vec<&'a EndOfPduField>,
    pub muxs: Vec<&'a Mux>,
    pub dynamic_length_fields: Vec<&'a DynamicLengthField>,
    pub tables: Vec<&'a Table>,
    pub units: Vec<&'a Unit>,
    pub physical_dimensions: Vec<&'a PhysicalDimension>,
}

impl<'a> DiagDataDictSpec<'a> {
    pub fn add_spec(&mut self, spec: Option<&'a DiagDataDictionarySpec>) {
        let Some(spec) = spec else { return };
        self.dtc_dops.extend(spec.dtc_dops.items.iter());
        self.env_data_descs.extend(spec.env_data_descs.items.iter());
        self.data_object_props
            .extend(spec.data_object_props.items.iter());
        self.structures.extend(spec.structures.items.iter());
        self.static_fields.extend(spec.static_fields.items.iter());
        self.end_of_pdu_fields
            .extend(spec.end_of_pdu_fields.items.iter());
        self.muxs.extend(spec.muxs.items.iter());
        self.dynamic_length_fields
            .extend(spec.dynamic_length_fields.items.iter());
        self.tables.extend(spec.tables.items.iter());
        if let Some(us) = &spec.unit_spec {
            if let Some(units) = &us.units {
                self.units.extend(units.items.iter());
            }
            if let Some(pds) = &us.physical_dimensions {
                self.physical_dimensions.extend(pds.items.iter());
            }
        }
    }

    pub fn add_view(&mut self, other: &DiagDataDictSpec<'a>) {
        self.dtc_dops.extend(other.dtc_dops.iter().copied());
        self.env_data_descs
            .extend(other.env_data_descs.iter().copied());
        self.data_object_props
            .extend(other.data_object_props.iter().copied());
        self.structures.extend(other.structures.iter().copied());
        self.static_fields
            .extend(other.static_fields.iter().copied());
        self.end_of_pdu_fields
            .extend(other.end_of_pdu_fields.iter().copied());
        self.muxs.extend(other.muxs.iter().copied());
        self.dynamic_length_fields
            .extend(other.dynamic_length_fields.iter().copied());
        self.tables.extend(other.tables.iter().copied());
        self.units.extend(other.units.iter().copied());
        self.physical_dimensions
            .extend(other.physical_dimensions.iter().copied());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyDatas {
    #[serde(rename = "COMPANY-DATA", default)]
    pub items: Vec<CompanyData>,
}

macro_rules! base_doc_info_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        named_desc_id_struct!($(#[$meta])* $vis struct $name {
            attrs { $($attrs)* }
            #[serde(rename = "ADMIN-DATA", default, skip_serializing_if = "Option::is_none")]
            pub admin_data: Option<AdminData>,
            #[serde(rename = "COMPANY-DATAS", default)]
            pub company_datas: CompanyDatas,
            $($body)*
        });
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        base_doc_info_struct!($(#[$meta])* $vis struct $name { attrs {} $($body)* });
    };
}

named_id_struct! {
    pub struct StateTransitionContainer {
        #[serde(rename = "SOURCE-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub source_snref: Option<SnRef>,
        #[serde(rename = "TARGET-SNREF", default, skip_serializing_if = "Option::is_none")]
        pub target_snref: Option<SnRef>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateTransitions {
    #[serde(rename = "STATE-TRANSITION", default)]
    pub items: Vec<StateTransitionContainer>,
}

/// `STATES` wrapper element; the list is always written out (possibly empty).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct States {
    /// `STATE` entries.
    #[serde(rename = "STATE", default)]
    pub items: Vec<NamedDescIdData>,
}

named_desc_id_struct! {
    pub struct ChartContainer {
        #[serde(rename = "STATE-TRANSITIONS", default)]
        pub state_transitions: StateTransitions,
        #[serde(rename = "STATES", default)]
        pub states: States,
    }
}

impl PartialOrd for FunctClass {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FunctClass {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.display_name().cmp(other.display_name())
    }
}

impl Eq for FunctClass {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctClassRefs {
    #[serde(rename = "FUNCT-CLASS-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosResponseRefs {
    #[serde(rename = "POS-RESPONSE-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegResponseRefs {
    #[serde(rename = "NEG-RESPONSE-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreConditionStateRefs {
    #[serde(rename = "PRE-CONDITION-STATE-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTransitionRefs {
    #[serde(rename = "STATE-TRANSITION-REF", default)]
    pub items: Vec<IdRef>,
}

named_id_struct! {
    pub struct DiagService {
        attrs {
            #[serde(rename = "@SEMANTIC", default, skip_serializing_if = "Option::is_none")]
            pub semantic: Option<String>,
            #[serde(
                rename = "@ADDRESSING",
                default,
                skip_serializing_if = "is_physical"
            )]
            pub addressing: AddressingType,
        }
        #[serde(rename = "FUNCT-CLASS-REFS", default)]
        pub funct_class_refs: FunctClassRefs,
        #[serde(rename = "REQUEST-REF", default, skip_serializing_if = "Option::is_none")]
        pub request_ref: Option<IdRef>,
        #[serde(rename = "POS-RESPONSE-REFS", default)]
        pub pos_response_refs: PosResponseRefs,
        #[serde(rename = "NEG-RESPONSE-REFS", default)]
        pub neg_response_refs: NegResponseRefs,
        #[serde(rename = "PRE-CONDITION-STATE-REFS", default)]
        pub pre_condition_state_refs: PreConditionStateRefs,
        #[serde(rename = "STATE-TRANSITION-REFS", default)]
        pub state_transition_refs: StateTransitionRefs,
        #[serde(rename = "AUDIENCE", default, skip_serializing_if = "Option::is_none")]
        pub audience: Option<Audience>,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdentifierRecord {
    pub name: String,
    pub identifier: u16,
    pub response_param_info: Vec<ParamDataInfo>,
}

impl IdentifierRecord {
    pub fn to_physical(&self, data: &[u8]) -> Vec<ParamDataInfo> {
        let mut infos = self.response_param_info.clone();
        for info in &mut infos {
            info.to_physical(data);
        }
        infos
    }
}

impl fmt::Display for IdentifierRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}", self.identifier)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    None,
    Number(f64),
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamDataInfo {
    pub name: String,
    pub data_type: BaseDataType,
    pub par_value: ParValue,
    pub dop: DataObjectProp,
    pub bit_length: i64,
    pub byte_offset: i64,
    pub bitmask: u64,
    pub change_endianess: bool,
    pub unit: String,
    pub value: ParamValue,
}

impl ParamDataInfo {
    pub fn new(
        name: String,
        par_value: ParValue,
        byte_offset: i64,
        dop: DataObjectProp,
        unit: String,
    ) -> Self {
        let dct = dop.diag_coded_type.clone();
        let mut base_data_type = dct
            .as_ref()
            .map_or(BaseDataType::default(), DiagCodedType::base_data_type);
        let mut bit_length = 0;
        let mut bitmask = 0;
        let mut change_endianess = false;
        match &dct {
            Some(DiagCodedType::StandardLength(std)) => {
                change_endianess = cfg!(target_endian = "little") == std.is_highlow_byte_order;
                bit_length = std.bit_length;
                bitmask = build_bitmask(bit_length);
                match byte_len_from_bit_len(bit_length) {
                    1 => {
                        base_data_type = if !(base_data_type as u8).is_multiple_of(2) {
                            BaseDataType::AUint8
                        } else {
                            BaseDataType::AInt8
                        };
                    }
                    2 => {
                        base_data_type = if (base_data_type as u8).is_multiple_of(2) {
                            BaseDataType::AInt16
                        } else {
                            BaseDataType::AUint16
                        };
                    }
                    _ => {}
                }
            }
            Some(DiagCodedType::MinMaxLength(mm)) => {
                bit_length = mm.max_length * 8;
            }
            _ => {}
        }
        ParamDataInfo {
            name,
            data_type: base_data_type,
            par_value,
            dop,
            bit_length,
            byte_offset,
            bitmask,
            change_endianess,
            unit,
            value: ParamValue::None,
        }
    }

    pub fn byte_length(&self) -> i64 {
        byte_len_from_bit_len(self.bit_length)
    }

    pub fn to_physical(&mut self, data: &[u8]) {
        let start = self.byte_offset + self.par_value.byte_position - 1;
        let byte_len = byte_len_from_bit_len(self.bit_length);
        if start < 0 || (data.len() as i64 - start) < byte_len {
            return;
        }
        let start = start as usize;
        let byte_len = byte_len as usize;
        let bytes = &data[start..start + byte_len];
        match self.data_type {
            BaseDataType::AAsciistring => {
                self.value = ParamValue::Text(
                    bytes
                        .iter()
                        .map(|b| if *b > 0x7F { '?' } else { *b as char })
                        .collect(),
                );
                return;
            }
            BaseDataType::AUtf8string => {
                self.value = ParamValue::Text(String::from_utf8_lossy(bytes).into_owned());
                return;
            }
            BaseDataType::AUnicode2string => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                self.value = ParamValue::Text(String::from_utf16_lossy(&units));
                return;
            }
            _ => {}
        }
        let mut raw: u128 = 0;
        for b in bytes {
            raw = (raw << 8) | u128::from(*b);
        }
        let shift = bytes.len() as i64 * 8 - self.par_value.bit_position - self.bit_length;
        let extracted = if shift > 0 { raw >> shift } else { raw };
        if self.data_type == BaseDataType::ABytefield {
            let bl = byte_len_from_bit_len(self.bit_length).max(0) as usize;
            let mut out = Vec::with_capacity(bl);
            let mut v = extracted;
            for _ in 0..bl.max(1) {
                out.push(v as u8);
                v >>= 8;
                if v == 0 {
                    break;
                }
            }
            out.reverse();
            self.value = ParamValue::Bytes(out);
            return;
        }
        let extracted = extracted as u64;
        let num: f64 = match self.data_type {
            BaseDataType::AUint8 => f64::from(extracted as u8),
            BaseDataType::AInt8 => f64::from(extracted as u8 as i8),
            BaseDataType::AInt16 | BaseDataType::AUint16 => {
                let mut v = extracted as u16;
                if self.change_endianess {
                    v = swap16(v);
                }
                if self.data_type == BaseDataType::AInt16 {
                    f64::from(v as i16)
                } else {
                    f64::from(v)
                }
            }
            BaseDataType::AInt32 | BaseDataType::AUint32 => {
                let mut v = extracted as u32;
                if self.change_endianess {
                    v = swap32(v);
                }
                if self.data_type == BaseDataType::AInt16 {
                    f64::from(v as i32)
                } else {
                    f64::from(v)
                }
            }
            BaseDataType::AFloat32 => {
                let mut v = extracted as u32;
                if self.change_endianess {
                    v = swap32(v);
                }
                f64::from(f32::from_bits(v))
            }
            BaseDataType::AFloat64 => {
                let mut v = extracted;
                if self.change_endianess {
                    v = swap64(v);
                }
                f64::from_bits(v)
            }
            other => {
                self.value = ParamValue::None;
                let _ = other;
                return;
            }
        };
        let compu = self.dop.compu_method.clone();
        let Some(compu) = compu else {
            self.value = ParamValue::Number(num);
            return;
        };
        match compu.category {
            CmCategory::Linear | CmCategory::RatFunc => {
                let scale = compu
                    .compu_internal_to_phys
                    .as_ref()
                    .and_then(|p| p.compu_scales.as_ref())
                    .and_then(|s| s.items.first());
                self.value = match scale.and_then(|s| s.compu_rational_coeffs.as_ref()) {
                    Some(coeffs) => ParamValue::Number(coeffs.to_physical(num)),
                    None => ParamValue::None,
                };
            }
            CmCategory::Texttable => {
                let key = num.round_ties_even() as i64;
                match compu.text_table().and_then(|t| t.get(&key).cloned()) {
                    Some(text) => self.value = ParamValue::Text(text),
                    None => self.value = ParamValue::Number(num),
                }
            }
            _ => {
                self.value = ParamValue::Number(num);
            }
        }
    }

    pub fn to_string_value(&self) -> String {
        match &self.value {
            ParamValue::None => String::new(),
            ParamValue::Bytes(bs) => bs
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join("-"),
            ParamValue::Number(n) => format!("{n}"),
            ParamValue::Text(s) => s.clone(),
        }
    }
}

named_desc_id_struct! {
    pub struct FunctClass {}
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctClasss {
    #[serde(rename = "FUNCT-CLASS", default)]
    pub items: Vec<FunctClass>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagComms {
    #[serde(rename = "DIAG-SERVICE", default)]
    pub items: Vec<DiagService>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Requests {
    #[serde(rename = "REQUEST", default)]
    pub items: Vec<ParamContainer>,
}

/// `POS-RESPONSES` wrapper element; always written even when empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PosResponses {
    /// `POS-RESPONSE` entries.
    #[serde(rename = "POS-RESPONSE", default)]
    pub items: Vec<ParamContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NegResponses {
    #[serde(rename = "NEG-RESPONSE", default)]
    pub items: Vec<ParamContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalNegResponses {
    #[serde(rename = "GLOBAL-NEG-RESPONSE", default)]
    pub items: Vec<ParamContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRefs {
    #[serde(rename = "IMPORT-REF", default)]
    pub items: Vec<LayerRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateCharts {
    #[serde(rename = "STATE-CHART", default)]
    pub items: Vec<ChartContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComparamRefs {
    #[serde(rename = "COMPARAM-REF", default)]
    pub items: Vec<ComParamRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRefs {
    #[serde(rename = "PARENT-REF", default)]
    pub items: Vec<LayerRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuVariantPatterns {
    #[serde(rename = "ECU-VARIANT-PATTERN", default)]
    pub items: Vec<EcuVariantPatternContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MatchingParameters {
    #[serde(rename = "MATCHING-PARAMETER", default)]
    pub items: Vec<MatchingParameterContainer>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchingParameterContainer {
    #[serde(
        rename = "DIAG-COMM-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diag_comm_snref: Option<SnRef>,
    #[serde(
        rename = "OUT-PARAM-IF-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub out_param_if_snref: Option<SnRef>,
    #[serde(
        rename = "EXPECTED-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuVariantPatternContainer {
    #[serde(rename = "MATCHING-PARAMETERS", default)]
    pub matching_parameters: MatchingParameters,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EcuVariantPatternsField {
    #[serde(rename = "EcuVariantPatternContainer", default)]
    pub items: Vec<EcuVariantPatternContainer>,
}

macro_rules! ecu_shared_data_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { attrs { $($attrs:tt)* } $($body:tt)* }) => {
        named_desc_id_struct!($(#[$meta])* $vis struct $name {
            attrs { $($attrs)* }
            #[serde(rename = "FUNCT-CLASSS", default)]
            pub funct_classs: FunctClasss,
            #[serde(
                rename = "DIAG-DATA-DICTIONARY-SPEC",
                default,
                skip_serializing_if = "Option::is_none"
            )]
            pub diag_data_dictionary_spec: Option<DiagDataDictionarySpec>,
            #[serde(rename = "DIAG-COMMS", default)]
            pub diag_comms: DiagComms,
            #[serde(rename = "REQUESTS", default)]
            pub requests: Requests,
            #[serde(rename = "POS-RESPONSES", default)]
            pub pos_responses: PosResponses,
            #[serde(rename = "NEG-RESPONSES", default)]
            pub neg_responses: NegResponses,
            #[serde(rename = "GLOBAL-NEG-RESPONSES", default)]
            pub global_neg_responses: GlobalNegResponses,
            #[serde(rename = "IMPORT-REFS", default)]
            pub import_refs: ImportRefs,
            #[serde(rename = "STATE-CHARTS", default)]
            pub state_charts: StateCharts,
            #[serde(rename = "COMPARAM-REFS", default)]
            pub comparam_refs: ComparamRefs,
            $($body)*
        });
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($body:tt)* }) => {
        ecu_shared_data_struct!($(#[$meta])* $vis struct $name { attrs {} $($body)* });
    };
}

ecu_shared_data_struct! {
    pub struct EcuSharedData {}
}

pub trait EcuSharedDataAccess {
    /// `SHORT-NAME`.
    fn esd_short_name(&self) -> Option<&str>;
    /// `FUNCT-CLASSS`.
    fn esd_funct_classs(&self) -> &[FunctClass];
    /// `DIAG-DATA-DICTIONARY-SPEC`.
    fn esd_dds(&self) -> Option<&DiagDataDictionarySpec>;
    /// `DIAG-COMMS`.
    fn esd_diag_comms(&self) -> &[DiagService];
    /// `REQUESTS`.
    fn esd_requests(&self) -> &[ParamContainer];
    /// `POS-RESPONSES`.
    fn esd_pos_responses(&self) -> &[ParamContainer];
    /// `NEG-RESPONSES`.
    fn esd_neg_responses(&self) -> &[ParamContainer];
    /// `GLOBAL-NEG-RESPONSES`.
    fn esd_global_neg_responses(&self) -> &[ParamContainer];
    /// `IMPORT-REFS`.
    fn esd_import_refs(&self) -> &[LayerRef];
    /// `STATE-CHARTS`.
    fn esd_state_charts(&self) -> &[ChartContainer];
    /// `COMPARAM-REFS`.
    fn esd_comparam_refs(&self) -> &[ComParamRef];
    fn esd_parent_refs(&self) -> Option<&[LayerRef]> {
        None
    }
}

macro_rules! impl_esd_access {
    ($t:ty) => {
        impl EcuSharedDataAccess for $t {
            fn esd_short_name(&self) -> Option<&str> {
                self.short_name.as_deref()
            }
            fn esd_funct_classs(&self) -> &[FunctClass] {
                &self.funct_classs.items
            }
            fn esd_dds(&self) -> Option<&DiagDataDictionarySpec> {
                self.diag_data_dictionary_spec.as_ref()
            }
            fn esd_diag_comms(&self) -> &[DiagService] {
                &self.diag_comms.items
            }
            fn esd_requests(&self) -> &[ParamContainer] {
                &self.requests.items
            }
            fn esd_pos_responses(&self) -> &[ParamContainer] {
                &self.pos_responses.items
            }
            fn esd_neg_responses(&self) -> &[ParamContainer] {
                &self.neg_responses.items
            }
            fn esd_global_neg_responses(&self) -> &[ParamContainer] {
                &self.global_neg_responses.items
            }
            fn esd_import_refs(&self) -> &[LayerRef] {
                &self.import_refs.items
            }
            fn esd_state_charts(&self) -> &[ChartContainer] {
                &self.state_charts.items
            }
            fn esd_comparam_refs(&self) -> &[ComParamRef] {
                &self.comparam_refs.items
            }
        }
    };
    ($t:ty, $parent:expr) => {
        impl EcuSharedDataAccess for $t {
            fn esd_short_name(&self) -> Option<&str> {
                self.short_name.as_deref()
            }
            fn esd_funct_classs(&self) -> &[FunctClass] {
                &self.funct_classs.items
            }
            fn esd_dds(&self) -> Option<&DiagDataDictionarySpec> {
                self.diag_data_dictionary_spec.as_ref()
            }
            fn esd_diag_comms(&self) -> &[DiagService] {
                &self.diag_comms.items
            }
            fn esd_requests(&self) -> &[ParamContainer] {
                &self.requests.items
            }
            fn esd_pos_responses(&self) -> &[ParamContainer] {
                &self.pos_responses.items
            }
            fn esd_neg_responses(&self) -> &[ParamContainer] {
                &self.neg_responses.items
            }
            fn esd_global_neg_responses(&self) -> &[ParamContainer] {
                &self.global_neg_responses.items
            }
            fn esd_import_refs(&self) -> &[LayerRef] {
                &self.import_refs.items
            }
            fn esd_state_charts(&self) -> &[ChartContainer] {
                &self.state_charts.items
            }
            fn esd_comparam_refs(&self) -> &[ComParamRef] {
                &self.comparam_refs.items
            }
            fn esd_parent_refs(&self) -> Option<&[LayerRef]> {
                #[allow(clippy::redundant_closure_call)]
                let f: fn(&Self) -> Option<&[LayerRef]> = $parent;
                f(self)
            }
        }
    };
}

impl_esd_access!(EcuSharedData);

ecu_shared_data_struct! {
    pub struct EcuVariant {
        #[serde(rename = "EcuVariantPatterns", default, skip_deserializing)]
        pub ecu_variant_patterns_field: EcuVariantPatternsField,
        #[serde(rename = "PARENT-REFS", default)]
        pub parent_refs: ParentRefs,
        #[serde(rename = "ECU-VARIANT-PATTERNS", default)]
        pub ecu_variant_patterns: EcuVariantPatterns,
    }
}

impl_esd_access!(EcuVariant, |s| Some(s.parent_refs.items.as_slice()));

ecu_shared_data_struct! {
    pub struct BaseVariant {
        #[serde(rename = "EcuVariantPatterns", default, skip_deserializing)]
        pub ecu_variant_patterns_field: EcuVariantPatternsField,
        #[serde(rename = "PARENT-REFS", default)]
        pub parent_refs: ParentRefs,
        #[serde(rename = "ECU-VARIANT-PATTERNS", default)]
        pub ecu_variant_patterns: EcuVariantPatterns,
    }
}

impl_esd_access!(BaseVariant, |s| Some(s.parent_refs.items.as_slice()));

ecu_shared_data_struct! {
    pub struct Protocol {
        attrs {
            #[serde(rename = "@TYPE", default, skip_serializing_if = "Option::is_none")]
            pub type_: Option<String>,
        }
        #[serde(
            rename = "COMPARAM-SPEC-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub comparam_spec_ref: Option<IdRef>,
        #[serde(
            rename = "PROT-STACK-SNREF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub prot_stack_snref: Option<SnRef>,
    }
}

impl_esd_access!(Protocol);

ecu_shared_data_struct! {
    pub struct FunctionalGroup {
        #[serde(rename = "PARENT-REFS", default, skip_serializing_if = "Option::is_none")]
        pub parent_refs: Option<ParentRefs>,
    }
}

impl_esd_access!(FunctionalGroup, |s| s
    .parent_refs
    .as_ref()
    .map(|p| p.items.as_slice()));

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Protocols {
    #[serde(rename = "PROTOCOL", default)]
    pub items: Vec<Protocol>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuSharedDatas {
    #[serde(rename = "ECU-SHARED-DATA", default)]
    pub items: Vec<EcuSharedData>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BaseVariants {
    #[serde(rename = "BASE-VARIANT", default)]
    pub items: Vec<BaseVariant>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuVariants {
    #[serde(rename = "ECU-VARIANT", default)]
    pub items: Vec<EcuVariant>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionalGroups {
    #[serde(rename = "FUNCTIONAL-GROUP", default)]
    pub items: Vec<FunctionalGroup>,
}

base_doc_info_struct! {
    pub struct DiagLayerContainer {
        #[serde(rename = "PROTOCOLS", default)]
        pub protocols: Protocols,
        #[serde(rename = "ECU-SHARED-DATAS", default)]
        pub ecu_shared_datas: EcuSharedDatas,
        #[serde(rename = "BASE-VARIANTS", default)]
        pub base_variants: BaseVariants,
        #[serde(rename = "ECU-VARIANTS", default)]
        pub ecu_variants: EcuVariants,
        #[serde(rename = "FUNCTIONAL-GROUPS", default)]
        pub functional_groups: FunctionalGroups,
    }
}

named_desc_id_struct! {
    pub struct ComParam {
        attrs {
            #[serde(rename = "@CPTYPE", default, skip_serializing_if = "Option::is_none")]
            pub cptype: Option<String>,
            #[serde(rename = "@CPUSAGE", default, skip_serializing_if = "Option::is_none")]
            pub cpusage: Option<String>,
            #[serde(rename = "@DISPLAY-LEVEL", default, skip_serializing_if = "is_zero")]
            pub display_level: i32,
            #[serde(rename = "@PARAM-CLASS", default, skip_serializing_if = "Option::is_none")]
            pub param_class: Option<String>,
        }
        #[serde(
            rename = "DATA-OBJECT-PROP-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub data_object_prop_ref: Option<IdRef>,
        #[serde(
            rename = "PHYSICAL-DEFAULT-VALUE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub physical_default_value: Option<String>,
    }
}

named_desc_id_struct! {
    pub struct ComplexComParam {
        attrs {
            #[serde(rename = "@ALLOW-MULTIPLE-VALUES", default)]
            pub allow_multiple_values: bool,
            #[serde(rename = "@CPTYPE", default, skip_serializing_if = "Option::is_none")]
            pub cptype: Option<String>,
            #[serde(rename = "@CPUSAGE", default, skip_serializing_if = "Option::is_none")]
            pub cpusage: Option<String>,
            #[serde(rename = "@DISPLAY-LEVEL", default, skip_serializing_if = "is_zero")]
            pub display_level: i32,
            #[serde(rename = "@PARAM-CLASS", default, skip_serializing_if = "Option::is_none")]
            pub param_class: Option<String>,
        }
        #[serde(rename = "COMPARAM", default, skip_serializing_if = "Vec::is_empty")]
        pub comparams: Vec<ComParam>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplexValue {
    #[serde(rename = "SIMPLE-VALUE", default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComParamRef {
    #[serde(rename = "@ID-REF", default, skip_serializing_if = "Option::is_none")]
    pub id_ref: Option<String>,
    #[serde(rename = "@DOCREF", default, skip_serializing_if = "Option::is_none")]
    pub docref: Option<String>,
    #[serde(rename = "@DOCTYPE", default, skip_serializing_if = "Option::is_none")]
    pub doctype: Option<String>,
    #[serde(rename = "DESC", default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(
        rename = "PROTOCOL-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub protocol_snref: Option<SnRef>,
    #[serde(rename = "VALUE", default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(
        rename = "SIMPLE-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub simple_value: Option<String>,
    #[serde(
        rename = "COMPLEX-VALUE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub complex_value: Option<ComplexValue>,
}

impl fmt::Display for ComParamRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .id_ref
            .as_deref()
            .and_then(|id| id.rsplit('.').next())
            .unwrap_or_default();
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProtStacks {
    #[serde(rename = "PROT-STACK", default)]
    pub items: Vec<ProtStack>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComParams {
    #[serde(rename = "COMPARAM", default)]
    pub items: Vec<ComParam>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComplexComParams {
    #[serde(rename = "COMPLEX-COMPARAM", default)]
    pub items: Vec<ComplexComParam>,
}

base_doc_info_struct! {
    pub struct ComParamSpec {
        #[serde(rename = "PROT-STACKS", default, skip_serializing_if = "Option::is_none")]
        pub prot_stacks: Option<ProtStacks>,
        #[serde(rename = "COMPARAMS", default, skip_serializing_if = "Option::is_none")]
        pub comparams: Option<ComParams>,
        #[serde(
            rename = "DATA-OBJECT-PROPS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub data_object_props: Option<DataObjectProps>,
        #[serde(rename = "UNIT-SPEC", default, skip_serializing_if = "Option::is_none")]
        pub unit_spec: Option<UnitSpec>,
    }
}

base_doc_info_struct! {
    pub struct ComParamSubset {
        attrs {
            #[serde(rename = "@CATEGORY", default, skip_serializing_if = "Option::is_none")]
            pub category: Option<String>,
        }
        #[serde(rename = "COMPARAMS", default, skip_serializing_if = "Option::is_none")]
        pub comparams: Option<ComParams>,
        #[serde(
            rename = "COMPLEX-COMPARAMS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub complex_comparam: Option<ComplexComParams>,
        #[serde(
            rename = "DATA-OBJECT-PROPS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub data_object_props: Option<DataObjectProps>,
        #[serde(rename = "UNIT-SPEC", default, skip_serializing_if = "Option::is_none")]
        pub unit_spec: Option<UnitSpec>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparamSubsetRefs {
    #[serde(rename = "COMPARAM-SUBSET-REF", default)]
    pub items: Vec<IdRef>,
}

named_desc_id_struct! {
    pub struct ProtStack {
        #[serde(
            rename = "COMPARAM-SUBSET-REFS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub comparam_subset_refs: Option<ComparamSubsetRefs>,
        #[serde(
            rename = "PDU-PROTOCOL-TYPE",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub pdu_protocol_type: Option<String>,
    }
}

// ============================================================================
// Vehicle information family: VehicleInfoSpec / VehicleInformation / VehicleConnector /
// VehicleConnectorPin / Ic* (INFO-COMPONENT polymorphism) / LogicalLink family /
// PkysicalVehicleLink
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum InfoComponent {
    /// A plain `NamedId` (no `xsi:type`).
    Plain(NamedIdData),
    /// `xsi:type="ECU-PROXY"`.
    EcuProxy(NamedIdData),
    /// `xsi:type="MODEL-YEAR"`.
    ModelYear(NamedIdData),
    /// `xsi:type="OEM"`.
    Oem(NamedIdData),
    /// `xsi:type="VEHICLE-MODEL"`.
    VehicleModel(NamedIdData),
    /// `xsi:type="VEHICLE-TYPE"`.
    VehicleType(NamedIdData),
}

impl InfoComponent {
    pub fn data(&self) -> &NamedIdData {
        match self {
            InfoComponent::Plain(d)
            | InfoComponent::EcuProxy(d)
            | InfoComponent::ModelYear(d)
            | InfoComponent::Oem(d)
            | InfoComponent::VehicleModel(d)
            | InfoComponent::VehicleType(d) => d,
        }
    }

    pub fn xsi_type(&self) -> Option<&'static str> {
        match self {
            InfoComponent::Plain(_) => None,
            InfoComponent::EcuProxy(_) => Some("ECU-PROXY"),
            InfoComponent::ModelYear(_) => Some("MODEL-YEAR"),
            InfoComponent::Oem(_) => Some("OEM"),
            InfoComponent::VehicleModel(_) => Some("VEHICLE-MODEL"),
            InfoComponent::VehicleType(_) => Some("VEHICLE-TYPE"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RawInfoComponent {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    long_name: Option<String>,
    #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
    sdgs: Option<Sdgs>,
}

impl Serialize for InfoComponent {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let d = self.data();
        RawInfoComponent {
            xsi_type: self.xsi_type().map(str::to_owned),
            id: d.id.clone(),
            short_name: d.short_name.clone(),
            long_name: d.long_name.clone(),
            sdgs: d.sdgs.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InfoComponent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = RawInfoComponent::deserialize(d)?;
        let data = NamedIdData {
            id: raw.id,
            short_name: raw.short_name,
            long_name: raw.long_name,
            sdgs: raw.sdgs,
        };
        Ok(match raw.xsi_type.as_deref() {
            Some("ECU-PROXY") => InfoComponent::EcuProxy(data),
            Some("MODEL-YEAR") => InfoComponent::ModelYear(data),
            Some("OEM") => InfoComponent::Oem(data),
            Some("VEHICLE-MODEL") => InfoComponent::VehicleModel(data),
            Some("VEHICLE-TYPE") => InfoComponent::VehicleType(data),
            _ => InfoComponent::Plain(data),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InfoComponents {
    #[serde(rename = "INFO-COMPONENT", default)]
    pub items: Vec<InfoComponent>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleInformations {
    #[serde(rename = "VEHICLE-INFORMATION", default)]
    pub items: Vec<VehicleInformation>,
}

base_doc_info_struct! {
    pub struct VehicleInfoSpec {
        #[serde(
            rename = "INFO-COMPONENTS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub info_components: Option<InfoComponents>,
        #[serde(
            rename = "VEHICLE-INFORMATIONS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub vehicle_informations: Option<VehicleInformations>,
    }
}

include!("odx_tail.rs");
