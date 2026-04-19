//! MDF V4 (ASAM MDF 4.10) block model with byte-level reading and writing.
//! This module models MDF v4 blocks (ID/HD/FH/CH/AT/EV/DG/CG/SI/CN/CC/CA/DT/DZ/DL/HL/SR/RD/RI/MD)
//! and parses/serializes them at the byte level. The binary layout is written out
//! explicitly field by field (little-endian, 8-byte block alignment, link pointers
//! stored as absolute file offsets). All blocks are flattened structs of fields;
//! block polymorphism is expressed with the [`DataBlockV4`] enum.
//! Well-known constant strings used by the format are fixed here (block IDs "CC"/"CG"/…,
//! FileID `"MDF     "`, FormatId `"4.10    "`, the FH/HD comment templates, and the
//! comment XML serialization shape `<CNcomment><TX />…`).
//! Intentional behaviors and known quirks (each item):
//! - The enums ConversionType, ChannelType, SyncType, SignalType, RecordIdType,
//!   TimeFlagsType, TimeQualityType, UnfinalizedFlagsType, CustomFlagsType, and
//!   DependencyType are shared with the v3 code and referenced from `crate::base`;
//!   the MDF4-specific difference (the cc_type byte mapping of ConversionType) is kept
//!   in this file as a same-crate inherent impl extension.
//! - Parsing a `CcBlockV4` with an unknown cc_type byte returns a parse error rather
//!   than silently falling back to the enum default (ParametricLinear = 0).
//! - Writing a `CcBlockV4` of type TextRange is rejected with `Error::Write`, because
//!   the legacy block-size formula for it (`TabSize*20` with no data written) produces
//!   a corrupt block.
//! - For TextTable conversions the written BlockSize formula (`TabSize*16+8`) does not
//!   match the actually written content (`TabSize*8` keys plus string links); this
//!   mismatch is reproduced deliberately.
//! - For TextTable conversions the written RefCount is the number of additional string
//!   links, not the value stored when the block was parsed; reproduced deliberately.
//! - Block size is recomputed from content whenever the table data is non-empty, not
//!   only when `TabSize == 0`, so re-writing a parsed block still yields a correctly
//!   sized block.
//! - `ChBlockV4` parsing reads the second and third dependency links both from
//!   `links[i+2]` (where i+1 would be expected); this quirk is reproduced deliberately
//!   and noted at the site.
//! - AT/CA/CH write paths are implemented from their read layouts (CH per ASAM MDF
//!   4.10); writing EV blocks returns `Error::Write`.
//! - The FH comment's tool_id/tool_version are modeled as `Option` and set to `None`
//!   by the constructor; when absent the corresponding XML elements are omitted.
//! - `DgBlockV4` writing pads to 8-byte alignment after DT/DZ *and* after HL endings
//!   (the only difference is padding zero bytes).
//! - Data groups spanning multiple channel groups are re-sorted by record ID during
//!   parsing via [`DgBlockV4::resort`] (invoked from `HdBlockV4` parsing). The
//!   re-sorting buffers per-CG record data in memory (`Vec<u8>` buffers joined by
//!   `read_data`); compressed data is materialized by in-memory concatenation in
//!   `read_data` instead of temporary files plus memory mapping. See the resort
//!   comments for the exact semantics.
//! - `CnBlockV4` writing does not write attachment links (the link count is always 8;
//!   only the attachment count field is written); reproduced deliberately.
//! - `HlBlockV4` writing unconditionally writes the DL link as `position + BlockSize`
//!   (even when there is no DL block); reproduced deliberately.
//! - Timestamps are kept as raw u64 values (100 ns units since 1970-01-01);
//!   `RecordingTime`/`TimeStamp` accessors document the conversion.
//! - Comment XML is serialized with the miniature XML reader/writer built into this
//!   file (this crate deliberately has no serde/quick-xml dependency). The
//!   serialization shape is: no xmlns, no XML declaration, empty elements self-closing.

use crate::error::{Error, Result};

/// Unix epoch (1970-01-01) time base; timestamps are 100 ns units since this instant.
pub const MDF4_EPOCH_UNIX_SECS: u64 = 0;
/// zlib header bytes used by DZ blocks.
pub const ZLIB_HEADER: [u8; 2] = [120, 1];

fn parse_err<T>(offset: u64, message: impl Into<String>) -> Result<T> {
    Err(Error::Parse {
        offset,
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------
// Little-endian raw read/write helpers (cursor tracks absolute position; error
// messages use absolute offsets)
// ---------------------------------------------------------------------------

fn rd_u8(buf: &[u8], pos: &mut usize) -> Result<u8> {
    if buf.len() - *pos < 1 {
        return parse_err(*pos as u64, "unexpected end of data (u8)");
    }
    let v = buf[*pos];
    *pos += 1;
    Ok(v)
}

fn rd_bytes<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    if buf.len() - *pos < n {
        return parse_err(
            *pos as u64,
            format!(
                "unexpected end of data: need {n} bytes, have {}",
                buf.len() - *pos
            ),
        );
    }
    let s = &buf[*pos..*pos + n];
    *pos += n;
    Ok(s)
}

fn rd_u16(buf: &[u8], pos: &mut usize) -> Result<u16> {
    let mut a = [0u8; 2];
    a.copy_from_slice(rd_bytes(buf, pos, 2)?);
    Ok(u16::from_le_bytes(a))
}

fn rd_i16(buf: &[u8], pos: &mut usize) -> Result<i16> {
    let mut a = [0u8; 2];
    a.copy_from_slice(rd_bytes(buf, pos, 2)?);
    Ok(i16::from_le_bytes(a))
}

fn rd_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let mut a = [0u8; 4];
    a.copy_from_slice(rd_bytes(buf, pos, 4)?);
    Ok(u32::from_le_bytes(a))
}

fn rd_i64(buf: &[u8], pos: &mut usize) -> Result<i64> {
    let mut a = [0u8; 8];
    a.copy_from_slice(rd_bytes(buf, pos, 8)?);
    Ok(i64::from_le_bytes(a))
}

fn rd_u64(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut a = [0u8; 8];
    a.copy_from_slice(rd_bytes(buf, pos, 8)?);
    Ok(u64::from_le_bytes(a))
}

fn rd_f64(buf: &[u8], pos: &mut usize) -> Result<f64> {
    let mut a = [0u8; 8];
    a.copy_from_slice(rd_bytes(buf, pos, 8)?);
    Ok(f64::from_le_bytes(a))
}

fn wr_u8(w: &mut Vec<u8>, v: u8) {
    w.push(v);
}
fn wr_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn wr_i16(w: &mut Vec<u8>, v: i16) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn wr_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn wr_i64(w: &mut Vec<u8>, v: i64) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn wr_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn wr_f64(w: &mut Vec<u8>, v: f64) {
    w.extend_from_slice(&v.to_le_bytes());
}
/// Write `n` zero bytes (reserving/skipping a region).
fn wr_zeros(w: &mut Vec<u8>, n: usize) {
    w.resize(w.len() + n, 0);
}

/// Back-patch link number `idx` in the link area (block header is 24 bytes plus
/// 8 bytes per link).
fn patch_link(w: &mut [u8], block_start: usize, idx: usize, value: i64) {
    let off = block_start + 24 + idx * 8;
    w[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

// ---------------------------------------------------------------------------
// Bit flags / enums
// ---------------------------------------------------------------------------

macro_rules! flags_newtype {
    ($(#[$meta:meta])* $name:ident($ty:ty), $($(#[$cmeta:meta])* $cname:ident = $cval:expr),* $(,)?) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name(pub $ty);

        impl $name {
            $($(#[$cmeta])* pub const $cname: Self = Self($cval);)*

            /// Raw bit value.
            pub fn bits(self) -> $ty {
                self.0
            }

            /// Whether all bits of `other` are set in `self`.
            pub fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// Construct from a raw bit value.
            pub fn from_bits(bits: $ty) -> Self {
                Self(bits)
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }

        impl std::ops::BitAnd for $name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self {
                Self(self.0 & rhs.0)
            }
        }
    };
}

macro_rules! plain_enum {
    ($(#[$meta:meta])* $name:ident($ty:ty) { $($(#[$vmeta:meta])* $vname:ident = $vval:expr),* $(,)? }) => {
        $(#[$meta])*
        // Variant names intentionally keep their spec-style spellings (e.g. UINT_LE, K_LINE).
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        #[repr($ty)]
        pub enum $name {
            $($(#[$vmeta])* $vname = $vval,)*
        }

        impl $name {
            /// Construct from a raw value; unknown values return None (so callers can
            /// report an error instead of silently accepting an invalid value).
            pub fn from_raw(v: $ty) -> Option<Self> {
                match v {
                    $($vval => Some(Self::$vname),)*
                    _ => None,
                }
            }

            /// Raw value.
            pub fn raw(self) -> $ty {
                self as $ty
            }
        }
    };
}

plain_enum! {
    /// Bus type of a channel (MDF4 CN block `bus_type`).
    BusType(u8) {
        #[default] NONE = 0,
        OTHER = 1,
        CAN = 2,
        LIN = 3,
        MOST = 4,
        FLEXRAY = 5,
        K_LINE = 6,
        ETHERNET = 7,
        USB = 8,
    }
}

flags_newtype! {
    /// Flags of an AT (attachment) block (u16 bit flags).
    AttachmentFlags(u16),
    /// Attachment data is embedded in the AT block.
    EMBEDDED_DATA = 1,
    /// The embedded data is compressed.
    COMPRESSED_EMBEDDED_DATA = 2,
    /// Contains an MD5 checksum.
    MD5_CHECKSUM_VALID = 4,
}

flags_newtype! {
    /// Flags of a CA (channel array) block (u32 bit flags).
    CaFlags(u32),
    NONE = 0,
    DYNAMIC_SIZE = 1,
    INPUT_QUANTITY = 2,
    OUTPUT_QUANTITY = 4,
    COMPARISON_QUANTITY = 8,
    AXIS = 0x10,
    FIXED_AXIS = 0x20,
    INVERSE_LAYOUT = 0x40,
}

plain_enum! {
    /// Kind of template block referenced by a CA block.
    CaTemplate(u8) {
        #[default] CN = 0,
        CG = 1,
        DG = 2,
    }
}

plain_enum! {
    /// Channel array type of a CA block.
    CaType(u8) {
        #[default] Array = 0,
        Axis = 1,
        LookUp = 2,
    }
}

plain_enum! {
    /// Cause of an event (EV block).
    CauseType(u8) {
        #[default] Other = 0,
        Error = 1,
        Tool = 2,
        Script = 3,
        User = 4,
    }
}

flags_newtype! {
    /// Flags of a channel (CN block, u32 bit flags).
    ChannelFlags(u32),
    NONE = 0,
    INVALID = 1,
    INVAL_BYTES_VALID = 2,
    PRECISION_VALID = 4,
    VALUE_RANGE_VALID = 8,
    LIMIT_RANGE_VALID = 0x10,
    EXTENDED_LIMIT_RANGE_VALID = 0x20,
    DISCRETE_VALUE = 0x40,
    CALIBRATION = 0x80,
    CALCULATED = 0x100,
    VIRTUAL = 0x200,
    BUS_EVENT = 0x400,
    MONOTONOUS = 0x800,
    DEFAULT_X_AXIS = 0x1000,
}

flags_newtype! {
    /// Flags of a channel group (CG block, u16 bit flags).
    ChannelGroupFlags(u16),
    NONE = 0,
    VLSD = 1,
    BUS_EVENT = 2,
    PLAIN_BUS_EVENT = 4,
}

flags_newtype! {
    /// Flags of a conversion (CC block, u16 bit flags).
    ConversionFlags(u16),
    NONE = 0,
    PRECISION_VALID = 1,
    LIMIT_RANGE_VALID = 2,
    STATUS_STRING = 4,
}

flags_newtype! {
    /// Data list flags (u8 in DL blocks, u16 in HL blocks).
    DataBlockFlags(u16),
    NONE = 0,
    EQUAL_LENGTH = 1,
}

flags_newtype! {
    /// Flags of an event (EV block, u8 bit flags).
    EventFlags(u8),
    NONE = 0,
    POST_PROCESSING = 1,
}

plain_enum! {
    /// Event type of an EV block.
    EventType(u8) {
        #[default] Recording = 0,
        RecordingInterrupt = 1,
        AcquisitionInterrupt = 2,
        StartRecordingTrigger = 3,
        StopRecordingTrigger = 4,
        Trigger = 5,
        Marker = 6,
    }
}

plain_enum! {
    /// Hierarchy type of a CH block.
    HierarchyType(u8) {
        #[default] Group = 0,
        Function = 1,
        Structure = 2,
        MapList = 3,
        InMeasurement = 4,
        OutMeasurement = 5,
        LocMeasurement = 6,
        DefCharacteristic = 7,
        RefCharacteristic = 8,
    }
}

plain_enum! {
    /// Range type of an event's range.
    RangeType(u8) {
        #[default] Point = 0,
        BeginRange = 1,
        EndRange = 2,
    }
}

plain_enum! {
    /// Compression type of a DZ block.
    ZipType(u8) {
        #[default] Deflate = 0,
        TransposeAndDeflate = 1,
        None = 2,
    }
}

plain_enum! {
    /// Source type of an SI block.
    SourceType(u8) {
        #[default] OTHER = 0,
        ECU = 1,
        BUS = 2,
        IO = 3,
        TOOL = 4,
        USER = 5,
    }
}

flags_newtype! {
    /// Flags of an SI (source information) block (u8 bit flags).
    SourceFlags(u8),
    NONE = 0,
    SIMULATED_SOURCE = 1,
}

flags_newtype! {
    /// Flags of an SR (sample reduction) block (u8 bit flags).
    SrFlags(u8),
    NONE = 0,
    INVALIDATION_BYTES = 1,
}

// --- The common enums are shared with the v3 code and referenced from
// --- crate::base. MDF4-specific differences (the cc_type byte mapping of
// --- ConversionType etc.) are kept in this file as same-crate inherent impl
// --- extensions, without changing the public API shape of base.rs.
use crate::base::{
    ChannelType, ConversionType, CustomFlagsType, DependencyType, RecordIdType, SignalType,
    SyncType, TimeFlagsType, TimeQualityType, UnfinalizedFlagsType, STR_RESORT_MSG,
};

/// MDF4 extension of ConversionType: the file's cc_type bytes (0..=10) and the
/// enum values use different encodings; the base.rs version carries no such
/// mapping, so it is extended here.
impl ConversionType {
    /// Construct from an MDF4 file cc_type byte. Unknown bytes return None so
    /// the caller can report a parse error instead of silently falling back to
    /// the enum default (0 = ParametricLinear).
    pub fn from_cc_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::None,
            1 => Self::ParametricLinear,
            2 => Self::Rational,
            3 => Self::TextFormula,
            4 => Self::TabInt,
            5 => Self::Tab,
            6 => Self::TabRange,
            7 => Self::TextTable,
            8 => Self::TextRange,
            9 => Self::TextToValue,
            10 => Self::TextToText,
            _ => return None,
        })
    }

    /// Map to the MDF4 file cc_type byte. Types not listed map to 0; this is
    /// intentional.
    pub fn to_cc_byte(self) -> u8 {
        match self {
            Self::None => 0,
            Self::ParametricLinear => 1,
            Self::Rational => 2,
            Self::TextFormula => 3,
            Self::TabInt => 4,
            Self::Tab => 5,
            Self::TabRange => 6,
            Self::TextTable => 7,
            Self::TextRange => 8,
            Self::TextToValue => 9,
            Self::TextToText => 10,
            _ => 0,
        }
    }
}

/// Monotony type of a channel axis (re-exported from the A2L model).
pub use autors_a2l::model::enums::MonotonyType;

/// XML element text of a MonotonyType (serialized by enum name).
fn monotony_to_xml(m: MonotonyType) -> Option<&'static str> {
    match m {
        MonotonyType::NOT_MON => Some("NOT_MON"),
        MonotonyType::MON_DECREASE => Some("MON_DECREASE"),
        MonotonyType::MON_INCREASE => Some("MON_INCREASE"),
        MonotonyType::STRICT_DECREASE => Some("STRICT_DECREASE"),
        MonotonyType::STRICT_INCREASE => Some("STRICT_INCREASE"),
        MonotonyType::MONOTONOUS => Some("MONOTONOUS"),
        MonotonyType::STRICT_MON => Some("STRICT_MON"),
        MonotonyType::NotSet => None,
    }
}

fn monotony_from_xml(s: &str) -> Option<MonotonyType> {
    Some(match s {
        "NOT_MON" => MonotonyType::NOT_MON,
        "MON_DECREASE" => MonotonyType::MON_DECREASE,
        "MON_INCREASE" => MonotonyType::MON_INCREASE,
        "STRICT_DECREASE" => MonotonyType::STRICT_DECREASE,
        "STRICT_INCREASE" => MonotonyType::STRICT_INCREASE,
        "MONOTONOUS" => MonotonyType::MONOTONOUS,
        "STRICT_MON" => MonotonyType::STRICT_MON,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// BlockBaseV4: the common header of all "##XX" blocks (flattened into a field
// group embedded in each block struct)
// ---------------------------------------------------------------------------

/// Common header of every MDF4 block (flattened form).
/// Layout: `##`, 2-character ID, u32 reserved, i64 block_size, u64 link_count,
/// then link_count i64 links (absolute file offsets, 0 means none).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockBaseV4 {
    /// 2-character block ID (e.g. `*b"CC"`; without the `##` prefix).
    pub id: [u8; 2],
    /// Total block length (header and link area included, trailing alignment
    /// padding excluded).
    pub block_size: i64,
    /// Link array (length is link_count).
    pub links: Vec<i64>,
}

/// Fixed block header size: `##` + ID + u32 + i64 + u64.
pub const BLOCK_HEADER_SIZE: usize = 24;

impl BlockBaseV4 {
    /// Create a new block header with the given ID, block size, and link count.
    pub fn new(id: [u8; 2], block_size: i64, link_count: usize) -> Self {
        BlockBaseV4 {
            id,
            block_size,
            links: vec![0; link_count],
        }
    }

    /// Number of links.
    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    /// Parse a block header, returning (base, offset where the data area starts).
    pub fn parse_header(buf: &[u8], pos: u64) -> Result<(Self, usize)> {
        let mut p = pos as usize;
        let b0 = rd_u8(buf, &mut p)?;
        let b1 = rd_u8(buf, &mut p)?;
        if b0 != b'#' || b1 != b'#' {
            return parse_err(pos, "block does not start with \"##\"");
        }
        let idb = rd_bytes(buf, &mut p, 2)?;
        let id = [idb[0], idb[1]];
        let _reserved = rd_u32(buf, &mut p)?;
        let block_size = rd_i64(buf, &mut p)?;
        let link_count = rd_u64(buf, &mut p)?;
        if block_size < (BLOCK_HEADER_SIZE + link_count as usize * 8) as i64 {
            return parse_err(
                pos,
                format!("block size {block_size} too small for {link_count} links"),
            );
        }
        let mut links = Vec::with_capacity(link_count as usize);
        for _ in 0..link_count {
            links.push(rd_i64(buf, &mut p)?);
        }
        Ok((
            BlockBaseV4 {
                id,
                block_size,
                links,
            },
            p,
        ))
    }

    /// Sniff the block header at the given offset (used to dispatch on the block ID).
    pub fn peek_id(buf: &[u8], pos: u64) -> Result<[u8; 2]> {
        let (base, _) = Self::parse_header(buf, pos)?;
        Ok(base.id)
    }

    /// Write the block header (the link area is zeroed first; callers back-patch
    /// it later with `patch_link`). Returns the block start offset.
    pub fn write_header(&self, w: &mut Vec<u8>) -> u64 {
        let start = w.len() as u64;
        w.extend_from_slice(b"##");
        w.extend_from_slice(&self.id);
        wr_u32(w, 0);
        wr_i64(w, self.block_size);
        wr_u64(w, self.links.len() as u64);
        wr_zeros(w, self.links.len() * 8);
        start
    }
}

// ---------------------------------------------------------------------------
// RdSdBlockV4 / text blocks (TX/MD/RD/SD)
// ---------------------------------------------------------------------------

/// String cleanup for MDF strings: UTF-8 decode, strip leading/trailing '\0',
/// then truncate at the first interior '\0'.
fn clean_mdf_string(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_matches('\0');
    match s.find('\0') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Read the content of the TX/MD text block at `link`; returns an empty string
/// when link <= 0 (no block is read in that case).
fn read_text_block(buf: &[u8], link: i64) -> Result<String> {
    if link <= 0 {
        return Ok(String::new());
    }
    let (base, data_off) = BlockBaseV4::parse_header(buf, link as u64)?;
    let len = (base.block_size - BLOCK_HEADER_SIZE as i64).max(0) as usize;
    let mut p = data_off;
    let raw = rd_bytes(buf, &mut p, len)?;
    Ok(clean_mdf_string(raw))
}

/// Write a TX/MD text block and return its start link; an empty string writes
/// nothing and returns 0. After the data plus one NUL the block is **always**
/// padded to 8 bytes (8 more bytes are appended even when already aligned;
/// deliberate).
fn write_text_block(w: &mut Vec<u8>, text: &str, as_md: bool) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let data = text.as_bytes();
    let base = BlockBaseV4::new(
        if as_md { *b"MD" } else { *b"TX" },
        24 + data.len() as i64 + 1,
        0,
    );
    let start = base.write_header(w);
    w.extend_from_slice(data);
    wr_u8(w, 0);
    let rem = w.len() % 8;
    wr_zeros(w, 8 - rem);
    start as i64
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RdSdBlockV4 {
    pub base: BlockBaseV4,
    pub data: Vec<u8>,
}

impl RdSdBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let len = (base.block_size - BLOCK_HEADER_SIZE as i64).max(0) as usize;
        let mut p = data_off;
        let data = rd_bytes(buf, &mut p, len)?.to_vec();
        Ok(RdSdBlockV4 { base, data })
    }

    pub fn new_text(text: &str, as_md: bool) -> Self {
        let data = text.as_bytes().to_vec();
        let base = BlockBaseV4::new(
            if as_md { *b"MD" } else { *b"TX" },
            24 + data.len() as i64 + 1,
            0,
        );
        RdSdBlockV4 { base, data }
    }

    pub fn new_data(data: Vec<u8>, as_sd: bool) -> Self {
        let base = BlockBaseV4::new(
            if as_sd { *b"SD" } else { *b"RD" },
            24 + data.len() as i64,
            0,
        );
        RdSdBlockV4 { base, data }
    }

    pub fn text(&self) -> String {
        clean_mdf_string(&self.data)
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let is_text = &self.base.id == b"MD" || &self.base.id == b"TX";
        let start = self.base.write_header(w);
        w.extend_from_slice(&self.data);
        if is_text {
            wr_u8(w, 0);
        }
        let rem = w.len() % 8;
        wr_zeros(w, 8 - rem);
        Ok(start)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in data {
        a = (a + x as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn transpose(src: &[u8], a: usize, b: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    if a > 0 && b > 0 {
        for i in 0..a {
            for j in 0..b {
                let idx = j * a + i;
                if idx < src.len() {
                    out.push(src[idx]);
                }
            }
        }
    }
    let used = a * b;
    if src.len() > used {
        out.extend_from_slice(&src[used..]);
    }
    out
}

pub fn dz_decompress(
    zip_type: ZipType,
    zip_parameter: u32,
    uncompressed_size: u64,
    compressed: &[u8],
) -> Result<Vec<u8>> {
    use std::io::Read;
    if compressed.len() < ZLIB_HEADER.len() {
        return Err(Error::Compression(
            "DZ payload shorter than zlib header".into(),
        ));
    }
    let mut dec = flate2::read::DeflateDecoder::new(&compressed[ZLIB_HEADER.len()..]);
    let mut raw = Vec::new();
    dec.read_to_end(&mut raw)
        .map_err(|e| Error::Compression(format!("deflate decode failed: {e}")))?;
    match zip_type {
        ZipType::Deflate => Ok(raw),
        ZipType::TransposeAndDeflate => {
            if zip_parameter == 0 {
                return Err(Error::Compression(
                    "zip parameter is 0 for transposed DZ block".into(),
                ));
            }
            let rows = uncompressed_size as usize / zip_parameter as usize;
            Ok(transpose(&raw, rows, zip_parameter as usize))
        }
        ZipType::None => Err(Error::Compression(
            "ZipType.None carries no compressed payload".into(),
        )),
    }
}

pub fn dz_compress(zip_type: ZipType, zip_parameter: u32, data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let payload = match zip_type {
        ZipType::Deflate => data.to_vec(),
        ZipType::TransposeAndDeflate => {
            if zip_parameter == 0 {
                return Err(Error::Compression(
                    "zip parameter is 0 for transposed DZ block".into(),
                ));
            }
            transpose(
                data,
                zip_parameter as usize,
                data.len() / zip_parameter as usize,
            )
        }
        ZipType::None => return Err(Error::Compression("ZipType.None carries no payload".into())),
    };
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&payload)
        .map_err(|e| Error::Compression(format!("deflate encode failed: {e}")))?;
    let deflated = enc
        .finish()
        .map_err(|e| Error::Compression(format!("deflate finish failed: {e}")))?;
    let mut out = Vec::with_capacity(deflated.len() + 6);
    out.extend_from_slice(&ZLIB_HEADER);
    out.extend_from_slice(&deflated);
    out.extend_from_slice(&adler32(&payload).to_be_bytes());
    Ok(out)
}

// ---------------------------------------------------------------------------
//
// ---------------------------------------------------------------------------

fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[derive(Default)]
struct XmlWriter {
    s: String,
}

impl XmlWriter {
    fn new() -> Self {
        XmlWriter { s: String::new() }
    }

    fn start(&mut self, name: &str) {
        self.s.push('<');
        self.s.push_str(name);
    }

    fn attr(&mut self, name: &str, value: &str) {
        self.s.push(' ');
        self.s.push_str(name);
        self.s.push_str("=\"");
        self.s.push_str(&xml_escape_attr(value));
        self.s.push('"');
    }

    fn open_end(&mut self) {
        self.s.push('>');
    }

    fn empty_end(&mut self) {
        self.s.push_str(" />");
    }

    fn text(&mut self, value: &str) {
        self.s.push_str(&xml_escape_text(value));
    }

    fn close(&mut self, name: &str) {
        self.s.push_str("</");
        self.s.push_str(name);
        self.s.push('>');
    }

    fn text_elem(&mut self, name: &str, value: &str) {
        self.start(name);
        if value.is_empty() {
            self.empty_end();
        } else {
            self.open_end();
            self.text(value);
            self.close(name);
        }
    }
}

#[derive(Debug, Default)]
struct XmlElement {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XmlElement>,
    text: String,
}

impl XmlElement {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&XmlElement> {
        self.children.iter().find(|c| c.name == name)
    }
}

struct XmlParser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> XmlParser<'a> {
    fn new(s: &'a str) -> Self {
        XmlParser {
            s: s.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn starts_with(&self, pat: &str) -> bool {
        self.s[self.pos..].starts_with(pat.as_bytes())
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Option<()> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn name(&mut self) -> Option<String> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'=') {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let raw = std::str::from_utf8(&self.s[start..self.pos]).ok()?;
        let local = raw.rsplit(':').next().unwrap_or(raw);
        Some(local.to_string())
    }

    fn unescape(&mut self, raw: &str) -> Option<String> {
        if !raw.contains('&') {
            return Some(raw.to_string());
        }
        let mut out = String::with_capacity(raw.len());
        let mut it = raw.chars();
        while let Some(c) = it.next() {
            if c != '&' {
                out.push(c);
                continue;
            }
            let mut ent = String::new();
            for ec in it.by_ref() {
                if ec == ';' {
                    break;
                }
                ent.push(ec);
            }
            match ent.as_str() {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    let cp = u32::from_str_radix(&ent[2..], 16).ok()?;
                    out.push(char::from_u32(cp)?);
                }
                _ if ent.starts_with('#') => {
                    let cp = ent[1..].parse::<u32>().ok()?;
                    out.push(char::from_u32(cp)?);
                }
                _ => return None,
            }
        }
        Some(out)
    }

    fn quoted(&mut self) -> Option<String> {
        let q = self.peek()?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        self.pos += 1;
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == q {
                break;
            }
            self.pos += 1;
        }
        if self.peek() != Some(q) {
            return None;
        }
        let raw = std::str::from_utf8(&self.s[start..self.pos]).ok()?;
        self.pos += 1;
        self.unescape(raw)
    }

    fn element(&mut self) -> Option<XmlElement> {
        self.expect(b'<')?;
        if self.peek() == Some(b'?') || self.peek() == Some(b'!') {
            return None;
        }
        let name = self.name()?;
        let mut el = XmlElement {
            name,
            ..Default::default()
        };
        loop {
            self.skip_ws();
            match self.peek()? {
                b'/' => {
                    self.pos += 1;
                    self.expect(b'>')?;
                    return Some(el);
                }
                b'>' => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    let aname = self.name()?;
                    self.skip_ws();
                    self.expect(b'=')?;
                    self.skip_ws();
                    let aval = self.quoted()?;
                    el.attrs.push((aname, aval));
                }
            }
        }
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'<' {
                    break;
                }
                self.pos += 1;
            }
            if self.pos > start {
                let raw = std::str::from_utf8(&self.s[start..self.pos]).ok()?;
                el.text.push_str(&self.unescape(raw)?);
            }
            self.peek()?;
            if self.starts_with("</") {
                self.pos += 2;
                let cname = self.name()?;
                self.skip_ws();
                self.expect(b'>')?;
                if cname != el.name {
                    return None;
                }
                return Some(el);
            }
            if self.starts_with("<!--") {
                let end = b"-->";
                let tail = &self.s[self.pos..];
                let idx = tail
                    .windows(end.len())
                    .position(|w| w == end)
                    .map(|i| i + end.len())?;
                self.pos += idx;
                continue;
            }
            if self.starts_with("<![CDATA[") {
                let end = b"]]>";
                self.pos += 9;
                let tail = &self.s[self.pos..];
                let idx = tail.windows(end.len()).position(|w| w == end)?;
                let raw = std::str::from_utf8(&tail[..idx]).ok()?;
                el.text.push_str(raw);
                self.pos += idx + end.len();
                continue;
            }
            let child = self.element()?;
            el.children.push(child);
        }
    }

    fn document(&mut self) -> Option<XmlElement> {
        loop {
            self.skip_ws();
            if self.starts_with("<?") {
                let tail = &self.s[self.pos..];
                let idx = tail.windows(2).position(|w| w == b"?>").map(|i| i + 2)?;
                self.pos += idx;
                continue;
            }
            if self.starts_with("<!--") {
                let tail = &self.s[self.pos..];
                let idx = tail.windows(3).position(|w| w == b"-->").map(|i| i + 3)?;
                self.pos += idx;
                continue;
            }
            break;
        }
        let root = self.element()?;
        self.skip_ws();
        if self.pos != self.s.len() {
            return None;
        }
        Some(root)
    }
}

fn parse_xml(xml: &str) -> Option<XmlElement> {
    XmlParser::new(xml).document()
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamesBase {
    pub text: Option<String>,
    pub ci: i32,
}

impl NamesBase {
    fn write_elem(&self, w: &mut XmlWriter, name: &str) {
        w.start(name);
        if self.ci != 0 {
            w.attr("ci", &self.ci.to_string());
        }
        match &self.text {
            Some(t) if !t.is_empty() => {
                w.open_end();
                w.text(t);
                w.close(name);
            }
            _ => w.empty_end(),
        }
    }

    fn parse_elem(el: &XmlElement) -> Self {
        NamesBase {
            text: if el.text.is_empty() {
                None
            } else {
                Some(el.text.clone())
            },
            ci: el.attr("ci").and_then(|v| v.parse().ok()).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NamesItem {
    Name(NamesBase),
    Display(NamesBase),
    Vendor(NamesBase),
    Description(NamesBase),
}

impl NamesItem {
    fn write(&self, w: &mut XmlWriter) {
        match self {
            NamesItem::Name(b) => b.write_elem(w, "name"),
            NamesItem::Display(b) => b.write_elem(w, "display"),
            NamesItem::Vendor(b) => b.write_elem(w, "vendor"),
            NamesItem::Description(b) => b.write_elem(w, "description"),
        }
    }

    fn parse(el: &XmlElement) -> Option<Self> {
        Some(match el.name.as_str() {
            "name" => NamesItem::Name(NamesBase::parse_elem(el)),
            "display" => NamesItem::Display(NamesBase::parse_elem(el)),
            "vendor" => NamesItem::Vendor(NamesBase::parse_elem(el)),
            "description" => NamesItem::Description(NamesBase::parse_elem(el)),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EBase {
    pub name: Option<String>,
    pub desc: Option<String>,
}

impl EBase {
    fn write_attrs(&self, w: &mut XmlWriter) {
        if let Some(n) = &self.name {
            w.attr("name", n);
        }
        if let Some(d) = &self.desc {
            w.attr("desc", d);
        }
    }

    fn parse_attrs(el: &XmlElement) -> Self {
        EBase {
            name: el.attr("name").map(str::to_string),
            desc: el.attr("desc").map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EType {
    pub base: EBase,
    pub text: Option<String>,
    pub ci: i32,
    pub type_attr: Option<String>,
    pub ro: bool,
    pub unit: Option<String>,
    pub unit_ref: Option<String>,
}

impl EType {
    fn write_elem(&self, w: &mut XmlWriter) {
        w.start("e");
        self.base.write_attrs(w);
        if self.ci != 0 {
            w.attr("ci", &self.ci.to_string());
        }
        if let Some(t) = &self.type_attr {
            if t != "string" {
                w.attr("type", t);
            }
        }
        if self.ro {
            w.attr("ro", "true");
        }
        if let Some(u) = &self.unit {
            w.attr("unit", u);
        }
        if let Some(u) = &self.unit_ref {
            w.attr("unit_ref", u);
        }
        match &self.text {
            Some(t) if !t.is_empty() => {
                w.open_end();
                w.text(t);
                w.close("e");
            }
            _ => w.empty_end(),
        }
    }

    fn parse_elem(el: &XmlElement) -> Self {
        EType {
            base: EBase::parse_attrs(el),
            text: if el.text.is_empty() {
                None
            } else {
                Some(el.text.clone())
            },
            ci: el.attr("ci").and_then(|v| v.parse().ok()).unwrap_or(0),
            type_attr: el.attr("type").map(str::to_string),
            ro: el.attr("ro") == Some("true") || el.attr("ro") == Some("1"),
            unit: el.attr("unit").map(str::to_string),
            unit_ref: el.attr("unit_ref").map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeType {
    pub base: EBase,
    pub sub_nodes: Vec<ENode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ENode {
    E(EType),
    Tree(TreeType),
}

impl ENode {
    fn write(&self, w: &mut XmlWriter) {
        match self {
            ENode::E(e) => e.write_elem(w),
            ENode::Tree(t) => {
                w.start("tree");
                t.base.write_attrs(w);
                if t.sub_nodes.is_empty() {
                    w.empty_end();
                } else {
                    w.open_end();
                    for n in &t.sub_nodes {
                        n.write(w);
                    }
                    w.close("tree");
                }
            }
        }
    }

    fn parse(el: &XmlElement) -> Option<Self> {
        Some(match el.name.as_str() {
            "e" => ENode::E(EType::parse_elem(el)),
            "tree" => ENode::Tree(TreeType {
                base: EBase::parse_attrs(el),
                sub_nodes: el
                    .children
                    .iter()
                    .map(ENode::parse)
                    .collect::<Option<Vec<_>>>()?,
            }),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommentBase {
    pub tx: String,
    pub common_properties: Option<Vec<ENode>>,
}

impl CommentBase {
    fn write(&self, w: &mut XmlWriter) {
        w.text_elem("TX", &self.tx);
        if let Some(props) = &self.common_properties {
            w.start("common_properties");
            if props.is_empty() {
                w.empty_end();
            } else {
                w.open_end();
                for n in props {
                    n.write(w);
                }
                w.close("common_properties");
            }
        }
    }

    fn parse(root: &XmlElement) -> Option<Self> {
        let tx = root.child("TX").map(|e| e.text.clone()).unwrap_or_default();
        let common_properties = root.child("common_properties").map(|el| {
            el.children
                .iter()
                .map(ENode::parse)
                .collect::<Option<Vec<_>>>()
        });
        let common_properties = match common_properties {
            Some(v) => Some(v?),
            None => None,
        };
        Some(CommentBase {
            tx,
            common_properties,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BaseNames {
    pub comment: CommentBase,
    pub names: Option<Vec<NamesItem>>,
}

impl BaseNames {
    fn write(&self, w: &mut XmlWriter) {
        self.comment.write(w);
        if let Some(names) = &self.names {
            w.start("names");
            if names.is_empty() {
                w.empty_end();
            } else {
                w.open_end();
                for n in names {
                    n.write(w);
                }
                w.close("names");
            }
        }
    }

    fn parse(root: &XmlElement) -> Option<Self> {
        let names = match root.child("names") {
            Some(el) => Some(
                el.children
                    .iter()
                    .map(NamesItem::parse)
                    .collect::<Option<Vec<_>>>()?,
            ),
            None => None,
        };
        Some(BaseNames {
            comment: CommentBase::parse(root)?,
            names,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RasterType {
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub unit: Option<String>,
    pub unit_ref: Option<String>,
}

fn f64_to_xml(v: f64) -> String {
    format!("{v}")
}

fn f64_from_xml(s: &str) -> Option<f64> {
    s.parse().ok()
}

impl RasterType {
    fn write_elem(&self, w: &mut XmlWriter) {
        w.start("raster");
        if self.min != 0.0 {
            w.attr("min", &f64_to_xml(self.min));
        }
        if self.max != 0.0 {
            w.attr("max", &f64_to_xml(self.max));
        }
        if let Some(u) = &self.unit {
            w.attr("unit", u);
        }
        if let Some(u) = &self.unit_ref {
            w.attr("unit_ref", u);
        }
        w.open_end();
        w.text(&f64_to_xml(self.value));
        w.close("raster");
    }

    fn parse_elem(el: &XmlElement) -> Option<Self> {
        Some(RasterType {
            value: f64_from_xml(el.text.trim())?,
            min: el.attr("min").and_then(f64_from_xml).unwrap_or(0.0),
            max: el.attr("max").and_then(f64_from_xml).unwrap_or(0.0),
            unit: el.attr("unit").map(str::to_string),
            unit_ref: el.attr("unit_ref").map(str::to_string),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddressType {
    pub address: Option<String>,
    pub byte_count: i32,
}

impl AddressType {
    fn write_elem(&self, w: &mut XmlWriter) {
        w.start("linker_address");
        if self.byte_count != 0 {
            w.attr("byte_count", &self.byte_count.to_string());
        }
        match &self.address {
            Some(t) if !t.is_empty() => {
                w.open_end();
                w.text(t);
                w.close("linker_address");
            }
            _ => w.empty_end(),
        }
    }

    fn parse_elem(el: &XmlElement) -> Self {
        AddressType {
            address: if el.text.is_empty() {
                None
            } else {
                Some(el.text.clone())
            },
            byte_count: el
                .attr("byte_count")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CnComment {
    pub base: BaseNames,
    pub raster: Option<RasterType>,
    /// `<linker_name>`.
    pub linker_name: Option<String>,
    /// `<linker_address>`.
    pub linker_address: Option<AddressType>,
    pub axis_monotony: MonotonyType,
}

impl CnComment {
    pub fn serialize(&self) -> String {
        let mut w = XmlWriter::new();
        w.start("CNcomment");
        w.open_end();
        self.base.write(&mut w);
        if let Some(r) = &self.raster {
            r.write_elem(&mut w);
        }
        if let Some(n) = &self.linker_name {
            w.text_elem("linker_name", n);
        }
        if let Some(a) = &self.linker_address {
            a.write_elem(&mut w);
        }
        if let Some(m) = monotony_to_xml(self.axis_monotony) {
            w.text_elem("axis_monotony", m);
        }
        w.close("CNcomment");
        w.s
    }

    pub fn parse(xml: &str) -> Option<Self> {
        let root = parse_xml(xml)?;
        if root.name != "CNcomment" {
            return None;
        }
        Some(CnComment {
            base: BaseNames::parse(&root)?,
            raster: match root.child("raster") {
                Some(el) => Some(RasterType::parse_elem(el)?),
                None => None,
            },
            linker_name: root.child("linker_name").map(|e| e.text.clone()),
            linker_address: root.child("linker_address").map(AddressType::parse_elem),
            axis_monotony: match root.child("axis_monotony") {
                Some(e) => monotony_from_xml(&e.text)?,
                None => MonotonyType::NotSet,
            },
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FhComment {
    pub comment: CommentBase,
    /// `<tool_id>`.
    pub tool_id: Option<String>,
    /// `<tool_vendor>`.
    pub tool_vendor: Option<String>,
    /// `<tool_version>`.
    pub tool_version: Option<String>,
    /// `<user_name>`.
    pub user_name: Option<String>,
}

impl FhComment {
    pub fn serialize(&self) -> String {
        let mut w = XmlWriter::new();
        w.start("FHcomment");
        w.open_end();
        self.comment.write(&mut w);
        if let Some(v) = &self.tool_id {
            w.text_elem("tool_id", v);
        }
        if let Some(v) = &self.tool_vendor {
            w.text_elem("tool_vendor", v);
        }
        if let Some(v) = &self.tool_version {
            w.text_elem("tool_version", v);
        }
        if let Some(v) = &self.user_name {
            w.text_elem("user_name", v);
        }
        w.close("FHcomment");
        w.s
    }

    pub fn parse(xml: &str) -> Option<Self> {
        let root = parse_xml(xml)?;
        if root.name != "FHcomment" {
            return None;
        }
        Some(FhComment {
            comment: CommentBase::parse(&root)?,
            tool_id: root.child("tool_id").map(|e| e.text.clone()),
            tool_vendor: root.child("tool_vendor").map(|e| e.text.clone()),
            tool_version: root.child("tool_version").map(|e| e.text.clone()),
            user_name: root.child("user_name").map(|e| e.text.clone()),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HdComment {
    pub comment: CommentBase,
    /// `<time_source>`.
    pub time_source: Option<String>,
}

impl HdComment {
    pub fn serialize(&self) -> String {
        let mut w = XmlWriter::new();
        w.start("HDcomment");
        w.open_end();
        self.comment.write(&mut w);
        if let Some(t) = &self.time_source {
            w.text_elem("time_source", t);
        }
        w.close("HDcomment");
        w.s
    }

    pub fn parse(xml: &str) -> Option<Self> {
        let root = parse_xml(xml)?;
        if root.name != "HDcomment" {
            return None;
        }
        Some(HdComment {
            comment: CommentBase::parse(&root)?,
            time_source: root.child("time_source").map(|e| e.text.clone()),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SiComment {
    pub base: BaseNames,
    pub path: Option<NamesBase>,
    pub bus: Option<NamesBase>,
    /// `<protocol>`.
    pub protocol: Option<String>,
}

impl SiComment {
    pub fn serialize(&self) -> String {
        let mut w = XmlWriter::new();
        w.start("SIcomment");
        w.open_end();
        self.base.write(&mut w);
        if let Some(p) = &self.path {
            p.write_elem(&mut w, "path");
        }
        if let Some(b) = &self.bus {
            b.write_elem(&mut w, "bus");
        }
        if let Some(p) = &self.protocol {
            w.text_elem("protocol", p);
        }
        w.close("SIcomment");
        w.s
    }

    pub fn parse(xml: &str) -> Option<Self> {
        let root = parse_xml(xml)?;
        if root.name != "SIcomment" {
            return None;
        }
        Some(SiComment {
            base: BaseNames::parse(&root)?,
            path: root.child("path").map(NamesBase::parse_elem),
            bus: root.child("bus").map(NamesBase::parse_elem),
            protocol: root.child("protocol").map(|e| e.text.clone()),
        })
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct RationalCoeffs {
    pub coeffs: [f64; 6],
}

pub const RATIONAL_IDENTITY: [f64; 6] = [0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

impl RationalCoeffs {
    pub fn identity() -> Self {
        RationalCoeffs {
            coeffs: RATIONAL_IDENTITY,
        }
    }

    pub fn linear(factor: f64, offset: f64) -> Self {
        let mut c = RATIONAL_IDENTITY;
        c[1] = 1.0 / factor;
        c[2] = -offset / factor;
        RationalCoeffs { coeffs: c }
    }

    pub fn new(coeffs: [f64; 6]) -> Self {
        RationalCoeffs { coeffs }
    }

    pub fn from_params(params: &[f64]) -> Option<Self> {
        match params.len() {
            2 => Some(Self::linear(params[1], params[0])),
            6 => {
                let mut c = [0.0; 6];
                c.copy_from_slice(params);
                Some(Self::new(c))
            }
            _ => None,
        }
    }

    fn is_identity(&self) -> bool {
        self.coeffs == RATIONAL_IDENTITY
    }

    fn has_denom(&self) -> bool {
        let c = &self.coeffs;
        c[3] != 0.0 || c[4] != 0.0 || c[5] != 1.0
    }

    pub fn to_physical(&self, raw_value: f64) -> f64 {
        if self.is_identity() || raw_value.is_nan() {
            return raw_value;
        }
        let c = &self.coeffs;
        let (a, b, cc) = if self.has_denom() {
            (
                c[3] * raw_value - c[0],
                c[4] * raw_value - c[1],
                c[5] * raw_value - c[2],
            )
        } else {
            (-c[0], -c[1], raw_value - c[2])
        };
        if a == 0.0 {
            return -cc / b;
        }
        let disc = (b * b - 4.0 * a * cc).sqrt();
        let x1 = (-b + disc) / (2.0 * a);
        let x2 = (-b - disc) / (2.0 * a);
        if x1 != x2 {
            return raw_value;
        }
        x1
    }

    pub fn to_raw(&self, x: f64) -> f64 {
        if self.is_identity() || x.is_nan() {
            return x;
        }
        let c = &self.coeffs;
        let mut num = if c[0] != 0.0 {
            c[0] * x * x + c[1] * x + c[2]
        } else {
            c[1] * x + c[2]
        };
        if self.has_denom() {
            num /= if c[3] != 0.0 {
                c[3] * x * x + c[4] * x + c[5]
            } else {
                c[4] * x + c[5]
            };
        }
        num
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct IdBlockV4 {
    pub file_id: String,
    pub format_id: String,
    pub program_id: String,
    pub version: u16,
    pub unfinalized_flags: UnfinalizedFlagsType,
    pub custom_flags: CustomFlagsType,
}

pub const ID_BLOCK_SIZE: usize = 64;

impl IdBlockV4 {
    pub fn new(program_id: &str, version: u16) -> Self {
        IdBlockV4 {
            file_id: "MDF     ".to_string(),
            format_id: "4.10    ".to_string(),
            program_id: program_id.to_string(),
            version,
            unfinalized_flags: UnfinalizedFlagsType::NONE,
            custom_flags: CustomFlagsType::NONE,
        }
    }

    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < ID_BLOCK_SIZE {
            return parse_err(0, "file too small for ID block");
        }
        let mut p = 0usize;
        let file_id = clean_mdf_string(rd_bytes(buf, &mut p, 8)?);
        let format_id = clean_mdf_string(rd_bytes(buf, &mut p, 8)?);
        let program_id = clean_mdf_string(rd_bytes(buf, &mut p, 8)?);
        let _reserved = rd_u32(buf, &mut p)?;
        let version = rd_u16(buf, &mut p)?;
        let _ = rd_bytes(buf, &mut p, 30)?;
        let unfinalized_flags = UnfinalizedFlagsType::from_bits(rd_u16(buf, &mut p)?);
        let custom_flags = CustomFlagsType::from_bits(rd_u16(buf, &mut p)?);
        Ok(IdBlockV4 {
            file_id,
            format_id,
            program_id,
            version,
            unfinalized_flags,
            custom_flags,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let start = w.len() as u64;
        let mut field = |s: &str| {
            let mut b = [0u8; 8];
            let n = s.len().min(8);
            b[..n].copy_from_slice(&s.as_bytes()[..n]);
            w.extend_from_slice(&b);
        };
        field(&self.file_id);
        field(&self.format_id);
        field(&self.program_id);
        wr_u32(w, 0);
        wr_u16(w, self.version);
        for _ in 0..17 {
            wr_u16(w, 0);
        }
        Ok(start)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum DataBlockV4 {
    Dt(DtBlockV4),
    Dz(DzBlockV4),
    Dl(DlBlockV4),
    Hl(HlBlockV4),
    RdSd(RdSdBlockV4),
    At(AtBlockV4),
}

impl DataBlockV4 {
    fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        match self {
            DataBlockV4::Dt(b) => b.write_block(w),
            DataBlockV4::Dz(b) => b.write_block(w),
            DataBlockV4::Dl(b) => b.write_block(w, true),
            DataBlockV4::Hl(b) => b.write_block(w),
            DataBlockV4::RdSd(b) => b.write_block(w),
            DataBlockV4::At(b) => b.write_block(w, true),
        }
    }

    fn append_data(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            DataBlockV4::Dt(b) => {
                out.extend_from_slice(&b.data);
                Ok(())
            }
            DataBlockV4::Dz(b) => {
                out.extend_from_slice(&b.decompress()?);
                Ok(())
            }
            DataBlockV4::Dl(b) => {
                for d in &b.data_blocks {
                    d.append_data(out)?;
                }
                Ok(())
            }
            DataBlockV4::Hl(b) => {
                for dl in &b.dl_blocks {
                    for d in &dl.data_blocks {
                        d.append_data(out)?;
                    }
                }
                Ok(())
            }
            DataBlockV4::RdSd(_) | DataBlockV4::At(_) => Ok(()),
        }
    }
}

fn parse_data_chain(buf: &[u8], mut link: i64) -> Result<Vec<DataBlockV4>> {
    let mut blocks = Vec::new();
    while link > 0 {
        let id = BlockBaseV4::peek_id(buf, link as u64)?;
        match &id {
            b"DT" => {
                blocks.push(DataBlockV4::Dt(DtBlockV4::parse(buf, link as u64)?));
                break;
            }
            b"HL" => {
                blocks.push(DataBlockV4::Hl(HlBlockV4::parse(buf, link as u64)?));
                break;
            }
            b"DL" => {
                let dl = DlBlockV4::parse(buf, link as u64)?;
                link = dl.base.links[0];
                blocks.push(DataBlockV4::Dl(dl));
            }
            b"RD" => {
                blocks.push(DataBlockV4::RdSd(RdSdBlockV4::parse(buf, link as u64)?));
                break;
            }
            b"DZ" => {
                blocks.push(DataBlockV4::Dz(DzBlockV4::parse(buf, link as u64)?));
                break;
            }
            _ => break,
        }
    }
    Ok(blocks)
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DgBlockV4 {
    pub base: BlockBaseV4,
    pub record_id_type: RecordIdType,
    pub comment: String,
    pub cg_blocks: Vec<CgBlockV4>,
    pub data_blocks: Vec<DataBlockV4>,
}

impl DgBlockV4 {
    pub fn new() -> Self {
        DgBlockV4 {
            base: BlockBaseV4::new(*b"DG", 64, 4),
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let record_id_type =
            RecordIdType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
                offset: pos,
                message: "unknown record id type".into(),
            })?;
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let data_blocks = parse_data_chain(buf, link(2))?;
        let mut cg_blocks = Vec::new();
        let mut l = link(1);
        while l > 0 {
            let cg = CgBlockV4::parse(buf, l as u64)?;
            l = cg.base.links[0];
            cg_blocks.push(cg);
        }
        let comment = read_text_block(buf, link(3))?;
        Ok(DgBlockV4 {
            base,
            record_id_type,
            comment,
            cg_blocks,
            data_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"DG", 64, 4);
        let start = base.write_header(w) as usize;
        wr_u8(w, self.record_id_type.raw());
        wr_zeros(w, 7);
        let first_cg = if self.cg_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.cg_blocks.len();
        for (i, cg) in self.cg_blocks.iter().enumerate() {
            cg.write_block(w, i + 1 == n)?;
        }
        let data_start = w.len() as i64;
        for b in &self.data_blocks {
            b.write_block(w)?;
        }
        let rem = w.len() % 8;
        if rem > 0 {
            wr_zeros(w, 8 - rem);
        }
        let comment = write_text_block(w, &self.comment, false);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, first_cg);
        patch_link(w, start, 2, data_start);
        patch_link(w, start, 3, comment);
        Ok(start as u64)
    }

    pub fn read_data(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for b in &self.data_blocks {
            b.append_data(&mut out)?;
        }
        Ok(out)
    }

    pub fn resort(&self) -> Result<Vec<DgBlockV4>> {
        let id_type = self.record_id_type;
        if self.cg_blocks.len() < 2 || id_type == RecordIdType::None {
            return Ok(vec![self.clone()]);
        }
        let data = self.read_data()?;
        let total = data.len() as u64;
        let id_width = u64::from(id_type.raw());
        let mut cgs = self.cg_blocks.clone();
        for cg in &mut cgs {
            cg.record_count = 0;
        }
        let mut bufs: Vec<Option<Vec<u8>>> = vec![None; cgs.len()];
        let mut num2: u64 = 0;
        let mut cursor: usize = 0;
        while total.saturating_sub(id_width) > num2 {
            let position = cursor;
            let key: u64 = match id_type {
                RecordIdType::Before8Bit => match data.get(cursor) {
                    Some(&b) => {
                        cursor += 1;
                        u64::from(b)
                    }
                    None => break,
                },
                RecordIdType::Before16Bit => match data.get(cursor..cursor + 2) {
                    Some(s) => {
                        cursor += 2;
                        u64::from(u16::from_le_bytes([s[0], s[1]]))
                    }
                    None => break,
                },
                RecordIdType::Before32Bit => match data.get(cursor..cursor + 4) {
                    Some(s) => {
                        cursor += 4;
                        u64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                    }
                    None => break,
                },
                RecordIdType::Before64Bit => match data.get(cursor..cursor + 8) {
                    Some(s) => {
                        cursor += 8;
                        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
                    }
                    None => break,
                },
                _ => 0,
            };
            let Some(i) = cgs.iter().rposition(|cg| cg.record_id == key) else {
                break;
            };
            if cgs[i].flags.contains(ChannelGroupFlags::VLSD) {
                let Some(s) = data.get(cursor..cursor + 4) else {
                    break;
                };
                let n3 = u64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                num2 += 4;
                cursor += 4;
                cursor += n3 as usize;
                num2 += n3;
                continue;
            }
            let record_size = cgs[i].record_size_with_id(id_type);
            let end = position + record_size as usize;
            let Some(bytes) = data.get(position..end) else {
                break;
            };
            bufs[i]
                .get_or_insert_with(Vec::new)
                .extend_from_slice(bytes);
            cgs[i].record_count += 1;
            num2 += record_size;
            cursor = end;
        }
        let mut out = Vec::new();
        for (i, buf) in bufs.into_iter().enumerate() {
            let Some(buf) = buf else {
                continue;
            };
            let cg = cgs[i].clone();
            let mut dg = DgBlockV4::new();
            dg.record_id_type = id_type;
            dg.comment = STR_RESORT_MSG.replace("{0}", &cg.record_id.to_string());
            dg.cg_blocks = vec![cg];
            dg.data_blocks = vec![DataBlockV4::Dt(DtBlockV4::new(buf))];
            out.push(dg);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// CGBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CgBlockV4 {
    pub base: BlockBaseV4,
    pub record_id: u64,
    pub record_count: u64,
    pub flags: ChannelGroupFlags,
    pub path_separator: u16,
    pub record_size: u32,
    pub inval_size: u32,
    pub acquisition_name: String,
    pub comment: String,
    pub acquisition_source: Option<Box<SiBlockV4>>,
    pub cn_blocks: Vec<CnBlockV4>,
    pub sr_blocks: Vec<SrBlockV4>,
}

impl CgBlockV4 {
    pub fn new(comment: Option<&str>) -> Self {
        CgBlockV4 {
            base: BlockBaseV4::new(*b"CG", 104, 6),
            comment: comment.unwrap_or_default().to_string(),
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let record_id = rd_u64(buf, &mut p)?;
        let record_count = rd_u64(buf, &mut p)?;
        let flags = ChannelGroupFlags::from_bits(rd_u16(buf, &mut p)?);
        let path_separator = rd_u16(buf, &mut p)?;
        let _reserved = rd_u32(buf, &mut p)?;
        let record_size = rd_u32(buf, &mut p)?;
        let inval_size = rd_u32(buf, &mut p)?;
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let mut cn_blocks = Vec::new();
        let mut l = link(1);
        while l > 0 {
            let cn = CnBlockV4::parse(buf, l as u64)?;
            l = cn.base.links[0];
            cn_blocks.push(cn);
        }
        cn_blocks.sort_by_key(|c| c.add_offset + (c.bit_offset as u32) / 8);
        let acquisition_name = read_text_block(buf, link(2))?;
        let acquisition_source = if link(3) > 0 {
            Some(Box::new(SiBlockV4::parse(buf, link(3) as u64)?))
        } else {
            None
        };
        let mut sr_blocks = Vec::new();
        let mut l = link(4);
        while l > 0 {
            let sr = SrBlockV4::parse(buf, l as u64)?;
            l = sr.base.links[0];
            sr_blocks.push(sr);
        }
        let comment = read_text_block(buf, link(5))?;
        Ok(CgBlockV4 {
            base,
            record_id,
            record_count,
            flags,
            path_separator,
            record_size,
            inval_size,
            acquisition_name,
            comment,
            acquisition_source,
            cn_blocks,
            sr_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"CG", 104, 6);
        let start = base.write_header(w) as usize;
        wr_u64(
            w,
            if self.record_count != 0 {
                1
            } else {
                self.record_id
            },
        );
        wr_u64(w, self.record_count);
        wr_u16(w, self.flags.bits());
        wr_u16(w, self.path_separator);
        wr_u32(w, 0);
        wr_u32(w, self.record_size);
        wr_u32(w, self.inval_size);
        let first_cn = if self.cn_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.cn_blocks.len();
        for (i, cn) in self.cn_blocks.iter().enumerate() {
            cn.write_block(w, i + 1 == n)?;
        }
        let acq_name = write_text_block(w, &self.acquisition_name, false);
        let acq_source = match &self.acquisition_source {
            Some(si) => si.write_block(w)? as i64,
            None => 0,
        };
        let first_sr = if self.sr_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.sr_blocks.len();
        for (i, sr) in self.sr_blocks.iter().enumerate() {
            sr.write_block(w, i + 1 == n)?;
        }
        let comment = write_text_block(w, &self.comment, false);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, first_cn);
        patch_link(w, start, 2, acq_name);
        patch_link(w, start, 3, acq_source);
        patch_link(w, start, 4, first_sr);
        patch_link(w, start, 5, comment);
        Ok(start as u64)
    }

    pub fn record_size_with_id(&self, record_id_type: RecordIdType) -> u64 {
        let mut n = u64::from(self.record_size) + u64::from(self.inval_size);
        n += match record_id_type {
            RecordIdType::BeforeAndAfter8Bit => 2,
            RecordIdType::Before8Bit => 1,
            RecordIdType::Before16Bit => 2,
            RecordIdType::Before32Bit => 4,
            RecordIdType::Before64Bit => 8,
            RecordIdType::None => 0,
        };
        n
    }
}

// ---------------------------------------------------------------------------
// CNBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CnBlockV4 {
    pub base: BlockBaseV4,
    pub name: String,
    pub unit: String,
    pub comment: String,
    pub cn_comment: Option<CnComment>,
    pub channel_type: ChannelType,
    pub sync_type: SyncType,
    pub signal_type: SignalType,
    pub bit_offset: u8,
    pub add_offset: u32,
    pub no_of_bits: u32,
    pub flags: ChannelFlags,
    pub inval_bit_pos: u32,
    pub precision: u8,
    pub min_raw: f64,
    pub max_raw: f64,
    pub min: f64,
    pub max: f64,
    pub min_ex: f64,
    pub max_ex: f64,
    pub cc_block: Option<Box<CcBlockV4>>,
    pub ca_block: Option<Box<CaBlockV4>>,
    pub composition: Vec<CnBlockV4>,
    pub si_block: Option<Box<SiBlockV4>>,
    pub data_block: Option<Box<DataBlockV4>>,
    pub attachment_links: Vec<i64>,
}

impl CnBlockV4 {
    /// uint byteOffset, uint noOfBits, ICCBLOCK, SyncType, ChannelFlags, uint invalBitPos, byte precision)`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signal_type: SignalType,
        channel_type: ChannelType,
        name: &str,
        description: Option<&str>,
        bit_offset: u32,
        byte_offset: u32,
        no_of_bits: u32,
        cc_block: Option<Box<CcBlockV4>>,
        sync_type: SyncType,
        flags: ChannelFlags,
        inval_bit_pos: u32,
        precision: u8,
    ) -> Self {
        let cn_comment = CnComment {
            base: BaseNames {
                comment: CommentBase {
                    tx: description.unwrap_or_default().to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        CnBlockV4 {
            base: BlockBaseV4::new(*b"CN", 160, 8),
            name: name.to_string(),
            comment: cn_comment.serialize(),
            cn_comment: Some(cn_comment),
            channel_type,
            sync_type,
            signal_type,
            bit_offset: (bit_offset % 8) as u8,
            add_offset: (byte_offset + bit_offset) / 8,
            no_of_bits,
            flags,
            inval_bit_pos,
            precision,
            min_raw: f64::NAN,
            max_raw: f64::NAN,
            min: f64::NAN,
            max: f64::NAN,
            min_ex: f64::NAN,
            max_ex: f64::NAN,
            cc_block,
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let channel_type =
            ChannelType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
                offset: pos,
                message: "unknown channel type".into(),
            })?;
        let sync_type = SyncType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown sync type".into(),
        })?;
        let signal_type =
            SignalType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
                offset: pos,
                message: "unknown signal type".into(),
            })?;
        let bit_offset = rd_u8(buf, &mut p)?;
        let add_offset = rd_u32(buf, &mut p)?;
        let no_of_bits = rd_u32(buf, &mut p)?;
        let flags = ChannelFlags::from_bits(rd_u32(buf, &mut p)?);
        let inval_bit_pos = rd_u32(buf, &mut p)?;
        let precision = rd_u8(buf, &mut p)?;
        let _ = rd_u8(buf, &mut p)?;
        let _ = rd_u16(buf, &mut p)?;
        let mut range = [f64::NAN; 6];
        for (i, flag) in [
            ChannelFlags::VALUE_RANGE_VALID,
            ChannelFlags::LIMIT_RANGE_VALID,
            ChannelFlags::EXTENDED_LIMIT_RANGE_VALID,
        ]
        .iter()
        .enumerate()
        {
            if flags.contains(*flag) {
                range[i * 2] = rd_f64(buf, &mut p)?;
                range[i * 2 + 1] = rd_f64(buf, &mut p)?;
            } else {
                let _ = rd_bytes(buf, &mut p, 16)?;
            }
        }
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let mut ca_block = None;
        let mut composition = Vec::new();
        if link(1) > 0 {
            match &BlockBaseV4::peek_id(buf, link(1) as u64)? {
                b"CA" => ca_block = Some(Box::new(CaBlockV4::parse(buf, link(1) as u64)?)),
                b"CN" => {
                    let mut l = link(1);
                    while l > 0 {
                        let cn = CnBlockV4::parse(buf, l as u64)?;
                        l = cn.base.links[0];
                        composition.push(cn);
                    }
                    composition.sort_by_key(|c| c.add_offset + (c.bit_offset as u32) / 8);
                }
                _ => {}
            }
        }
        let name = read_text_block(buf, link(2))?;
        let unit = read_text_block(buf, link(6))?;
        let comment = read_text_block(buf, link(7))?;
        let cn_comment = if comment.is_empty() {
            None
        } else {
            Some(CnComment::parse(&comment).unwrap_or_else(|| CnComment {
                base: BaseNames {
                    comment: CommentBase {
                        tx: comment.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            }))
        };
        let si_block = if link(3) > 0 {
            Some(Box::new(SiBlockV4::parse(buf, link(3) as u64)?))
        } else {
            None
        };
        let mut min = range[2];
        let mut max = range[3];
        let cc_block = if link(4) > 0 {
            let cc = CcBlockV4::parse(buf, link(4) as u64)?;
            if min.is_nan() && !cc.min.is_nan() {
                min = cc.min;
            }
            if max.is_nan() && !cc.max.is_nan() {
                max = cc.max;
            }
            Some(Box::new(cc))
        } else {
            None
        };
        let data_block = if link(5) > 0 {
            let id = BlockBaseV4::peek_id(buf, link(5) as u64)?;
            let b = match &id {
                b"AT" => Some(DataBlockV4::At(AtBlockV4::parse(buf, link(5) as u64)?)),
                b"HL" => Some(DataBlockV4::Hl(HlBlockV4::parse(buf, link(5) as u64)?)),
                b"DL" => Some(DataBlockV4::Dl(DlBlockV4::parse(buf, link(5) as u64)?)),
                b"DZ" => Some(DataBlockV4::Dz(DzBlockV4::parse(buf, link(5) as u64)?)),
                _ => None,
            };
            b.map(Box::new)
        } else {
            None
        };
        let attachment_links = base.links.iter().skip(8).copied().collect();
        Ok(CnBlockV4 {
            base,
            name,
            unit,
            comment,
            cn_comment,
            channel_type,
            sync_type,
            signal_type,
            bit_offset,
            add_offset,
            no_of_bits,
            flags,
            inval_bit_pos,
            precision,
            min_raw: range[0],
            max_raw: range[1],
            min,
            max,
            min_ex: range[4],
            max_ex: range[5],
            cc_block,
            ca_block,
            composition,
            si_block,
            data_block,
            attachment_links,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"CN", 160, 8);
        let start = base.write_header(w) as usize;
        wr_u8(w, self.channel_type.raw());
        wr_u8(w, self.sync_type.raw());
        wr_u8(w, self.signal_type.raw());
        wr_u8(w, self.bit_offset);
        wr_u32(w, self.add_offset);
        wr_u32(w, self.no_of_bits);
        let (mut min, mut max) = (self.min, self.max);
        if let Some(cc) = &self.cc_block {
            min = cc.min;
            max = cc.max;
        }
        let mut flags = self.flags;
        if !min.is_nan() && !max.is_nan() {
            flags |= ChannelFlags::LIMIT_RANGE_VALID;
        }
        wr_u32(w, flags.bits());
        wr_u32(w, self.inval_bit_pos);
        wr_u8(w, self.precision);
        wr_u8(w, 0);
        wr_u16(w, self.attachment_links.len() as u16);
        wr_f64(w, self.min_raw);
        wr_f64(w, self.max_raw);
        wr_f64(w, min);
        wr_f64(w, max);
        wr_f64(w, self.min_ex);
        wr_f64(w, self.max_ex);
        let composite = if let Some(ca) = &self.ca_block {
            ca.write_block(w)? as i64
        } else if !self.composition.is_empty() {
            let l = w.len() as i64;
            let n = self.composition.len();
            for (i, cn) in self.composition.iter().enumerate() {
                cn.write_block(w, i + 1 == n)?;
            }
            l
        } else {
            0
        };
        let sd = match &self.data_block {
            Some(b) => b.write_block(w)? as i64,
            None => 0,
        };
        let si = match &self.si_block {
            Some(b) => b.write_block(w)? as i64,
            None => 0,
        };
        let cc = match &self.cc_block {
            Some(b) => b.write_block(w)? as i64,
            None => 0,
        };
        let name = write_text_block(w, &self.name, false);
        let unit = write_text_block(w, &self.unit, false);
        let comment = write_text_block(w, &self.comment, true);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, composite);
        patch_link(w, start, 2, name);
        patch_link(w, start, 3, si);
        patch_link(w, start, 4, cc);
        patch_link(w, start, 5, sd);
        patch_link(w, start, 6, unit);
        patch_link(w, start, 7, comment);
        Ok(start as u64)
    }

    pub fn description(&self) -> &str {
        self.cn_comment
            .as_ref()
            .map(|c| c.base.comment.tx.as_str())
            .unwrap_or("")
    }

    pub fn rate(&self) -> Option<f64> {
        self.cn_comment
            .as_ref()
            .and_then(|c| c.raster.as_ref())
            .map(|r| r.value)
    }
}

// ---------------------------------------------------------------------------
// CCBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CcBlockV4 {
    pub base: BlockBaseV4,
    pub name: String,
    pub unit: String,
    pub comment: String,
    pub conversion_type: ConversionType,
    pub precision: u8,
    pub flags: ConversionFlags,
    pub ref_count: u16,
    pub tab_size: u16,
    pub min: f64,
    pub max: f64,
    pub params: Vec<f64>,
    pub formula: String,
    pub num_pairs: Vec<(f64, f64)>,
    pub text_table: Vec<(f64, String)>,
    pub text_range: Vec<(String, f64, f64)>,
    pub default_text: String,
    pub inv_cc_block: Option<Box<CcBlockV4>>,
}

impl CcBlockV4 {
    pub fn new(conversion_type: ConversionType, unit: &str, min: f64, max: f64) -> Self {
        let mut cc = CcBlockV4 {
            base: BlockBaseV4::new(*b"CC", 80, 4),
            conversion_type,
            unit: unit.to_string(),
            min: f64::NAN,
            max: f64::NAN,
            ..Default::default()
        };
        if !min.is_nan() && !max.is_nan() {
            cc.flags |= ConversionFlags::LIMIT_RANGE_VALID;
            cc.min = min;
            cc.max = max;
        }
        cc
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let conv_byte = rd_u8(buf, &mut p)?;
        let conversion_type =
            ConversionType::from_cc_byte(conv_byte).ok_or_else(|| Error::Parse {
                offset: pos,
                message: format!("unknown cc conversion type byte {conv_byte}"),
            })?;
        let precision = rd_u8(buf, &mut p)?;
        let flags = ConversionFlags::from_bits(rd_u16(buf, &mut p)?);
        let ref_count = rd_u16(buf, &mut p)?;
        let tab_size = rd_u16(buf, &mut p)?;
        let (mut min, mut max) = (f64::NAN, f64::NAN);
        if flags.contains(ConversionFlags::LIMIT_RANGE_VALID) {
            min = rd_f64(buf, &mut p)?;
            max = rd_f64(buf, &mut p)?;
        } else {
            let _ = rd_bytes(buf, &mut p, 16)?;
        }
        let extra_links: Vec<i64> = base.links.iter().skip(4).copied().collect();
        let mut params = Vec::new();
        let mut formula = String::new();
        let mut num_pairs = Vec::new();
        let mut text_table = Vec::new();
        let mut text_range = Vec::new();
        let mut default_text = String::new();
        match conversion_type {
            ConversionType::ParametricLinear | ConversionType::Rational => {
                for _ in 0..tab_size {
                    params.push(rd_f64(buf, &mut p)?);
                }
            }
            ConversionType::TextFormula => {
                if let Some(l) = extra_links.first() {
                    formula = read_text_block(buf, *l)?;
                }
            }
            ConversionType::TabInt | ConversionType::Tab => {
                for _ in 0..(tab_size as usize) / 2 {
                    let k = rd_f64(buf, &mut p)?;
                    let v = rd_f64(buf, &mut p)?;
                    num_pairs.push((k, v));
                }
            }
            ConversionType::TextTable => {
                let mut keys = Vec::with_capacity(tab_size as usize);
                for _ in 0..tab_size {
                    keys.push(rd_f64(buf, &mut p)?);
                }
                let mut texts = Vec::with_capacity(extra_links.len());
                for l in &extra_links {
                    texts.push(read_text_block(buf, *l)?);
                }
                for (i, k) in keys.iter().enumerate() {
                    if let Some(t) = texts.get(i) {
                        text_table.push((*k, t.clone()));
                    }
                }
                if let Some(last) = texts.last() {
                    default_text = last.clone();
                }
            }
            ConversionType::TextRange => {
                let mut ranges = Vec::with_capacity(tab_size as usize / 2);
                let mut i = 0;
                while i < tab_size {
                    let lo = rd_f64(buf, &mut p)?;
                    let hi = rd_f64(buf, &mut p)?;
                    ranges.push((lo, hi));
                    i += 2;
                }
                let mut texts = Vec::with_capacity(extra_links.len());
                for l in &extra_links {
                    texts.push(read_text_block(buf, *l)?);
                }
                for (t, (lo, hi)) in texts.iter().zip(ranges.iter()) {
                    text_range.push((t.clone(), *lo, *hi));
                }
                if let Some(last) = texts.last() {
                    default_text = last.clone();
                }
            }
            _ => {}
        }
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let name = read_text_block(buf, link(0))?;
        let unit = read_text_block(buf, link(1))?;
        let comment = read_text_block(buf, link(2))?;
        let inv_cc_block = if link(3) > 0 {
            Some(Box::new(CcBlockV4::parse(buf, link(3) as u64)?))
        } else {
            None
        };
        let ord = |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
        num_pairs.sort_by(|a, b| ord(&a.0, &b.0));
        text_table.sort_by(|a, b| ord(&a.0, &b.0));
        text_range.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(CcBlockV4 {
            base,
            name,
            unit,
            comment,
            conversion_type,
            precision,
            flags,
            ref_count,
            tab_size,
            min,
            max,
            params,
            formula,
            num_pairs,
            text_table,
            text_range,
            default_text,
            inv_cc_block,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let mut tab_size = self.tab_size;
        let mut block_size: i64 = 80;
        let mut extra_texts: Vec<String> = Vec::new();
        match self.conversion_type {
            ConversionType::ParametricLinear | ConversionType::Rational
                if !self.params.is_empty() =>
            {
                tab_size = self.params.len() as u16;
                block_size += i64::from(tab_size) * 8;
            }
            ConversionType::TextFormula if tab_size == 0 => {
                block_size += 8;
                extra_texts.push(self.formula.clone());
            }
            ConversionType::TabInt | ConversionType::Tab if !self.num_pairs.is_empty() => {
                tab_size = (self.num_pairs.len() * 2) as u16;
                block_size += i64::from(tab_size) * 8;
            }
            ConversionType::TextTable if !self.text_table.is_empty() => {
                tab_size = self.text_table.len() as u16;
                block_size += i64::from(tab_size) * 2 * 8 + 8;
                extra_texts = self.text_table.iter().map(|(_, t)| t.clone()).collect();
                extra_texts.push(self.default_text.clone());
            }
            ConversionType::TextRange if !self.text_range.is_empty() => {
                return Err(Error::Write(
                    "TextRange CC block writing is not supported".into(),
                ));
            }
            _ => {}
        }
        let base = BlockBaseV4::new(*b"CC", block_size, 4 + extra_texts.len());
        let start = base.write_header(w) as usize;
        wr_u8(w, self.conversion_type.to_cc_byte());
        wr_u8(w, self.precision);
        let mut flags = self.flags;
        if !self.min.is_nan() && !self.max.is_nan() {
            flags |= ConversionFlags::LIMIT_RANGE_VALID;
        }
        wr_u16(w, flags.bits());
        wr_u16(w, extra_texts.len() as u16);
        wr_u16(w, tab_size);
        if flags.contains(ConversionFlags::LIMIT_RANGE_VALID) {
            wr_f64(w, self.min);
            wr_f64(w, self.max);
        } else {
            wr_zeros(w, 16);
        }
        match self.conversion_type {
            ConversionType::ParametricLinear | ConversionType::Rational => {
                for v in &self.params {
                    wr_f64(w, *v);
                }
            }
            ConversionType::TabInt | ConversionType::Tab => {
                for (k, v) in &self.num_pairs {
                    wr_f64(w, *k);
                    wr_f64(w, *v);
                }
            }
            ConversionType::TextTable => {
                for (k, _) in &self.text_table {
                    wr_f64(w, *k);
                }
            }
            _ => {}
        }
        let mut extra_links = Vec::with_capacity(extra_texts.len());
        for t in &extra_texts {
            extra_links.push(write_text_block(w, t, false));
        }
        let name = write_text_block(w, &self.name, false);
        let unit = write_text_block(w, &self.unit, false);
        let comment = write_text_block(w, &self.comment, false);
        let inv = match &self.inv_cc_block {
            Some(cc) => cc.write_block(w)? as i64,
            None => 0,
        };
        patch_link(w, start, 0, name);
        patch_link(w, start, 1, unit);
        patch_link(w, start, 2, comment);
        patch_link(w, start, 3, inv);
        for (i, l) in extra_links.iter().enumerate() {
            patch_link(w, start, 4 + i, *l);
        }
        Ok(start as u64)
    }

    pub fn to_physical(&self, raw_value: f64, use_inversed: bool) -> f64 {
        let num = raw_value;
        match self.conversion_type {
            ConversionType::ParametricLinear => match RationalCoeffs::from_params(&self.params) {
                Some(rc) => {
                    if use_inversed {
                        rc.to_raw(num)
                    } else {
                        rc.to_physical(num)
                    }
                }
                None => num,
            },
            ConversionType::Rational => match RationalCoeffs::from_params(&self.params) {
                Some(rc) => {
                    if !use_inversed {
                        rc.to_raw(num)
                    } else {
                        rc.to_physical(num)
                    }
                }
                None => num,
            },
            ConversionType::Tab => self
                .num_pairs
                .iter()
                .find(|(k, _)| *k == raw_value)
                .map(|(_, v)| *v)
                .unwrap_or(num),
            ConversionType::TabInt => {
                let mut result = num;
                if !self.num_pairs.is_empty() {
                    let (mut pk, mut pv) = self.num_pairs[0];
                    result = pv;
                    for &(k, v) in self.num_pairs.iter().skip(1) {
                        if raw_value < pk {
                            return result;
                        }
                        if raw_value <= k {
                            return (raw_value - pk) / (k - pk) * (v - pv) + pv;
                        }
                        pk = k;
                        pv = v;
                    }
                    result = pv;
                }
                result
            }
            _ => num,
        }
    }
}

// ---------------------------------------------------------------------------
// DTBLOCKV4 / DZBLOCKV4 / DLBLOCKV4 / HLBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DtBlockV4 {
    pub base: BlockBaseV4,
    pub data: Vec<u8>,
}

impl DtBlockV4 {
    pub fn new(data: Vec<u8>) -> Self {
        let base = BlockBaseV4::new(*b"DT", 24 + data.len() as i64, 0);
        DtBlockV4 { base, data }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let len = (base.block_size - BLOCK_HEADER_SIZE as i64).max(0) as usize;
        let mut p = data_off;
        let data = rd_bytes(buf, &mut p, len)?.to_vec();
        Ok(DtBlockV4 { base, data })
    }

    pub fn size(&self) -> i64 {
        self.base.block_size - BLOCK_HEADER_SIZE as i64
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let base = BlockBaseV4::new(*b"DT", 24 + self.data.len() as i64, 0);
        let start = base.write_header(w);
        w.extend_from_slice(&self.data);
        Ok(start)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DzBlockV4 {
    pub base: BlockBaseV4,
    pub block_type: [u8; 2],
    pub zip_type: ZipType,
    pub zip_parameter: u32,
    pub size: i64,
    pub length: u64,
    pub data: Vec<u8>,
}

impl DzBlockV4 {
    pub fn new(
        data: Vec<u8>,
        uncompressed_size: i64,
        zip_type: ZipType,
        zip_parameter: u32,
        block_type: [u8; 2],
    ) -> Self {
        let base = BlockBaseV4::new(*b"DZ", 48 + data.len() as i64, 0);
        DzBlockV4 {
            base,
            block_type,
            zip_type,
            zip_parameter,
            size: uncompressed_size,
            length: data.len() as u64,
            data,
        }
    }

    pub fn from_uncompressed(
        data: &[u8],
        zip_type: ZipType,
        zip_parameter: u32,
        block_type: [u8; 2],
    ) -> Result<Self> {
        let payload = dz_compress(zip_type, zip_parameter, data)?;
        Ok(Self::new(
            payload,
            data.len() as i64,
            zip_type,
            zip_parameter,
            block_type,
        ))
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let bt = rd_bytes(buf, &mut p, 2)?;
        let block_type = [bt[0], bt[1]];
        let zip_type = ZipType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown zip type".into(),
        })?;
        let _ = rd_u8(buf, &mut p)?;
        let zip_parameter = rd_u32(buf, &mut p)?;
        let size = rd_i64(buf, &mut p)?;
        let length = rd_u64(buf, &mut p)?;
        if zip_type == ZipType::None {
            return parse_err(pos, "ZipType.None DZ block not supported");
        }
        let data = rd_bytes(buf, &mut p, length as usize)?.to_vec();
        Ok(DzBlockV4 {
            base,
            block_type,
            zip_type,
            zip_parameter,
            size,
            length,
            data,
        })
    }

    pub fn decompress(&self) -> Result<Vec<u8>> {
        dz_decompress(
            self.zip_type,
            self.zip_parameter,
            self.size as u64,
            &self.data,
        )
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let base = BlockBaseV4::new(*b"DZ", 48 + self.data.len() as i64, 0);
        let start = base.write_header(w);
        w.extend_from_slice(&self.block_type);
        wr_u8(w, self.zip_type.raw());
        wr_u8(w, 0);
        wr_u32(w, self.zip_parameter);
        wr_i64(w, self.size);
        wr_u64(w, self.length);
        w.extend_from_slice(&self.data);
        Ok(start)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DlBlockV4 {
    pub base: BlockBaseV4,
    pub flags: DataBlockFlags,
    pub count: u32,
    pub equal_length: u64,
    pub data_blocks: Vec<DataBlockV4>,
}

impl DlBlockV4 {
    pub fn new(flags: DataBlockFlags, count: usize) -> Self {
        let base = BlockBaseV4::new(*b"DL", 48 + 8 * count as i64, 1 + count);
        DlBlockV4 {
            base,
            flags,
            count: count as u32,
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let flags = DataBlockFlags::from_bits(u16::from(rd_u8(buf, &mut p)?));
        let _ = rd_u16(buf, &mut p)?;
        let _ = rd_u8(buf, &mut p)?;
        let count = rd_u32(buf, &mut p)?;
        let equal_length = rd_u64(buf, &mut p)?;
        let mut data_blocks = Vec::new();
        for l in base.links.iter().skip(1) {
            if *l <= 0 {
                continue;
            }
            match &BlockBaseV4::peek_id(buf, *l as u64)? {
                b"DT" => data_blocks.push(DataBlockV4::Dt(DtBlockV4::parse(buf, *l as u64)?)),
                b"DZ" => data_blocks.push(DataBlockV4::Dz(DzBlockV4::parse(buf, *l as u64)?)),
                _ => {}
            }
        }
        Ok(DlBlockV4 {
            base,
            flags,
            count,
            equal_length,
            data_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let n = self.data_blocks.len();
        let base = BlockBaseV4::new(*b"DL", 48 + 8 * n as i64, 1 + n);
        let start = base.write_header(w) as usize;
        wr_u8(w, self.flags.bits() as u8);
        wr_u16(w, 0);
        wr_u8(w, 0);
        wr_u32(w, n as u32);
        wr_u64(w, self.equal_length);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        for (i, b) in self.data_blocks.iter().enumerate() {
            let l = b.write_block(w)? as i64;
            patch_link(w, start, 1 + i, l);
        }
        Ok(start as u64)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HlBlockV4 {
    pub base: BlockBaseV4,
    pub flags: DataBlockFlags,
    pub zip_type: ZipType,
    pub dl_blocks: Vec<DlBlockV4>,
}

impl HlBlockV4 {
    pub fn new(zip_type: ZipType, flags: DataBlockFlags) -> Self {
        HlBlockV4 {
            base: BlockBaseV4::new(*b"HL", 40, 1),
            flags,
            zip_type,
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let flags = DataBlockFlags::from_bits(rd_u16(buf, &mut p)?);
        let zip_type = ZipType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown zip type".into(),
        })?;
        let mut dl_blocks = Vec::new();
        let mut l = base.links.first().copied().unwrap_or(0);
        while l > 0 {
            let dl = DlBlockV4::parse(buf, l as u64)?;
            l = dl.base.links[0];
            dl_blocks.push(dl);
        }
        Ok(HlBlockV4 {
            base,
            flags,
            zip_type,
            dl_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let base = BlockBaseV4::new(*b"HL", 40, 1);
        let start = base.write_header(w) as usize;
        patch_link(w, start, 0, start as i64 + 40);
        wr_u16(w, self.flags.bits());
        wr_u8(w, self.zip_type.raw());
        wr_u8(w, 0);
        wr_u32(w, 0);
        let n = self.dl_blocks.len();
        for (i, dl) in self.dl_blocks.iter().enumerate() {
            dl.write_block(w, i + 1 == n)?;
        }
        Ok(start as u64)
    }
}

// ---------------------------------------------------------------------------
// SIBLOCKV4 / SRBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SiBlockV4 {
    pub base: BlockBaseV4,
    pub name: String,
    pub path: String,
    pub comment: String,
    pub source: SourceType,
    pub bus: BusType,
    pub flags: SourceFlags,
}

impl SiBlockV4 {
    pub fn new(
        source: SourceType,
        bus: BusType,
        name: &str,
        path: &str,
        comment: &str,
        flags: SourceFlags,
    ) -> Self {
        SiBlockV4 {
            base: BlockBaseV4::new(*b"SI", 56, 3),
            name: name.to_string(),
            path: path.to_string(),
            comment: comment.to_string(),
            source,
            bus,
            flags,
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let source = SourceType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown source type".into(),
        })?;
        let bus = BusType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown bus type".into(),
        })?;
        let flags = SourceFlags::from_bits(rd_u8(buf, &mut p)?);
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let name = read_text_block(buf, link(0))?;
        let path = read_text_block(buf, link(1))?;
        let comment = read_text_block(buf, link(2))?;
        Ok(SiBlockV4 {
            base,
            name,
            path,
            comment,
            source,
            bus,
            flags,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let base = BlockBaseV4::new(*b"SI", 56, 3);
        let start = base.write_header(w) as usize;
        wr_u8(w, self.source.raw());
        wr_u8(w, self.bus.raw());
        wr_u8(w, self.flags.bits());
        wr_u8(w, 0);
        wr_u32(w, 0);
        let name = write_text_block(w, &self.name, false);
        let path = write_text_block(w, &self.path, false);
        let comment = write_text_block(w, &self.comment, false);
        patch_link(w, start, 0, name);
        patch_link(w, start, 1, path);
        patch_link(w, start, 2, comment);
        Ok(start as u64)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SrBlockV4 {
    pub base: BlockBaseV4,
    pub nr_of_red_samples: u64,
    pub len_of_time_int: f64,
    pub flags: SrFlags,
    pub data_blocks: Vec<DataBlockV4>,
}

impl SrBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let nr_of_red_samples = rd_u64(buf, &mut p)?;
        let len_of_time_int = rd_f64(buf, &mut p)?;
        let flags = SrFlags::from_bits(rd_u8(buf, &mut p)?);
        let data_blocks = parse_data_chain(buf, base.links.get(1).copied().unwrap_or(0))?;
        Ok(SrBlockV4 {
            base,
            nr_of_red_samples,
            len_of_time_int,
            flags,
            data_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"SR", 57, 2);
        let start = base.write_header(w) as usize;
        wr_u64(w, self.nr_of_red_samples);
        wr_f64(w, self.len_of_time_int);
        wr_u8(w, self.flags.bits());
        let data_start = w.len() as i64;
        for b in &self.data_blocks {
            b.write_block(w)?;
        }
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, data_start);
        Ok(start as u64)
    }
}

// ---------------------------------------------------------------------------
// ATBLOCKV4 / CABLOCKV4 / CHBLOCKV4 / EVBLOCKV4 / FHBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AtBlockV4 {
    pub base: BlockBaseV4,
    pub filename: String,
    pub mime_type: String,
    pub comment: String,
    pub flags: AttachmentFlags,
    pub creator_index: u16,
}

impl AtBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let flags = AttachmentFlags::from_bits(rd_u16(buf, &mut p)?);
        let creator_index = rd_u16(buf, &mut p)?;
        let _ = rd_u32(buf, &mut p)?;
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let filename = read_text_block(buf, link(1))?;
        let mime_type = read_text_block(buf, link(2))?;
        let comment = read_text_block(buf, link(3))?;
        Ok(AtBlockV4 {
            base,
            filename,
            mime_type,
            comment,
            flags,
            creator_index,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"AT", 64, 4);
        let start = base.write_header(w) as usize;
        wr_u16(w, self.flags.bits());
        wr_u16(w, self.creator_index);
        wr_u32(w, 0);
        let filename = write_text_block(w, &self.filename, false);
        let mime = write_text_block(w, &self.mime_type, false);
        let comment = write_text_block(w, &self.comment, false);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, filename);
        patch_link(w, start, 2, mime);
        patch_link(w, start, 3, comment);
        Ok(start as u64)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaBlockV4 {
    pub base: BlockBaseV4,
    pub ca_type: CaType,
    pub template_type: CaTemplate,
    pub n_dim: u16,
    pub flags: CaFlags,
    pub byte_offset_base: u32,
    pub inval_bit_pos_base: u32,
    pub dim_size: u64,
}

impl CaBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let ca_type = CaType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown CA type".into(),
        })?;
        let template_type =
            CaTemplate::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
                offset: pos,
                message: "unknown CA template".into(),
            })?;
        let n_dim = rd_u16(buf, &mut p)?;
        let flags = CaFlags::from_bits(rd_u32(buf, &mut p)?);
        let byte_offset_base = rd_u32(buf, &mut p)?;
        let inval_bit_pos_base = rd_u32(buf, &mut p)?;
        let dim_size = rd_u64(buf, &mut p)?;
        Ok(CaBlockV4 {
            base,
            ca_type,
            template_type,
            n_dim,
            flags,
            byte_offset_base,
            inval_bit_pos_base,
            dim_size,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        let base = BlockBaseV4::new(*b"CA", 48, 0);
        let start = base.write_header(w);
        wr_u8(w, self.ca_type.raw());
        wr_u8(w, self.template_type.raw());
        wr_u16(w, self.n_dim);
        wr_u32(w, self.flags.bits());
        wr_u32(w, self.byte_offset_base);
        wr_u32(w, self.inval_bit_pos_base);
        wr_u64(w, self.dim_size);
        Ok(start)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChBlockV4 {
    pub base: BlockBaseV4,
    pub name: String,
    pub comment: String,
    pub hierarchy_type: HierarchyType,
    pub dependencies: Vec<DependencyType>,
    pub ch_blocks: Vec<ChBlockV4>,
}

impl ChBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let dep_count = rd_u32(buf, &mut p)?;
        let hierarchy_type =
            HierarchyType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
                offset: pos,
                message: "unknown hierarchy type".into(),
            })?;
        let _ = rd_bytes(buf, &mut p, 3)?;
        let ref_links: Vec<i64> = base
            .links
            .iter()
            .skip(4)
            .copied()
            .filter(|l| *l > 0)
            .collect();
        let mut dependencies = Vec::new();
        let (mut n, mut i) = (0u32, 0usize);
        while n < dep_count && i < ref_links.len() {
            dependencies.push(DependencyType::new(
                ref_links[i],
                ref_links.get(i + 2).copied().unwrap_or(0),
                ref_links.get(i + 2).copied().unwrap_or(0),
            ));
            n += 1;
            i += 3;
        }
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let name = read_text_block(buf, link(2))?;
        let comment = read_text_block(buf, link(3))?;
        let mut ch_blocks = Vec::new();
        let mut l = link(1);
        while l > 0 {
            let ch = ChBlockV4::parse(buf, l as u64)?;
            l = ch.base.links[0];
            ch_blocks.push(ch);
        }
        Ok(ChBlockV4 {
            base,
            name,
            comment,
            hierarchy_type,
            dependencies,
            ch_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let n_dep = self.dependencies.len();
        let base = BlockBaseV4::new(*b"CH", 24 + (4 + 3 * n_dep) as i64 * 8 + 8, 4 + 3 * n_dep);
        let start = base.write_header(w) as usize;
        wr_u32(w, n_dep as u32);
        wr_u8(w, self.hierarchy_type.raw());
        wr_zeros(w, 3);
        let child = if self.ch_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.ch_blocks.len();
        for (i, ch) in self.ch_blocks.iter().enumerate() {
            ch.write_block(w, i + 1 == n)?;
        }
        let name = write_text_block(w, &self.name, false);
        let comment = write_text_block(w, &self.comment, false);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, child);
        patch_link(w, start, 2, name);
        patch_link(w, start, 3, comment);
        for (i, dep) in self.dependencies.iter().enumerate() {
            patch_link(w, start, 4 + 3 * i, dep.link_dg);
            patch_link(w, start, 5 + 3 * i, dep.link_cg);
            patch_link(w, start, 6 + 3 * i, dep.link_cn);
        }
        Ok(start as u64)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvBlockV4 {
    pub base: BlockBaseV4,
    pub event_type: EventType,
    pub sync_type: SyncType,
    pub range_type: RangeType,
    pub cause_type: CauseType,
    pub flags: EventFlags,
    pub creator_index: u16,
    pub sync_base_value: i64,
    pub sync_factor: f64,
    pub name: String,
    pub comment: String,
    pub ref_links: Vec<i64>,
}

impl EvBlockV4 {
    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let event_type = EventType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown event type".into(),
        })?;
        let sync_type = SyncType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown sync type".into(),
        })?;
        let range_type = RangeType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown range type".into(),
        })?;
        let cause_type = CauseType::from_raw(rd_u8(buf, &mut p)?).ok_or_else(|| Error::Parse {
            offset: pos,
            message: "unknown cause type".into(),
        })?;
        let flags = EventFlags::from_bits(rd_u8(buf, &mut p)?);
        let _ = rd_bytes(buf, &mut p, 3)?;
        let _ = rd_u32(buf, &mut p)?;
        let _ = rd_u16(buf, &mut p)?;
        let creator_index = rd_u16(buf, &mut p)?;
        let sync_base_value = rd_i64(buf, &mut p)?;
        let sync_factor = rd_f64(buf, &mut p)?;
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let name = read_text_block(buf, link(3))?;
        let comment = read_text_block(buf, link(4))?;
        let ref_links = base.links.iter().skip(5).copied().collect();
        Ok(EvBlockV4 {
            base,
            event_type,
            sync_type,
            range_type,
            cause_type,
            flags,
            creator_index,
            sync_base_value,
            sync_factor,
            name,
            comment,
            ref_links,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FhBlockV4 {
    pub base: BlockBaseV4,
    pub time_stamp: u64,
    pub utc_offset: i16,
    pub dst_offset: i16,
    pub flags: TimeFlagsType,
    pub comment: String,
}

impl FhBlockV4 {
    pub fn new(
        user_name: &str,
        time_stamp: u64,
        utc_offset: i16,
        dst_offset: i16,
        flags: TimeFlagsType,
    ) -> Self {
        let comment = FhComment {
            comment: CommentBase {
                tx: "File created".to_string(),
                ..Default::default()
            },
            tool_vendor: Some("https://jnachbur.de".to_string()),
            user_name: Some(user_name.to_string()),
            ..Default::default()
        };
        FhBlockV4 {
            base: BlockBaseV4::new(*b"FH", 56, 2),
            time_stamp,
            utc_offset,
            dst_offset,
            flags,
            comment: comment.serialize(),
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let time_stamp = rd_u64(buf, &mut p)?;
        let utc_offset = rd_i16(buf, &mut p)?;
        let dst_offset = rd_i16(buf, &mut p)?;
        let flags = TimeFlagsType::from_bits(rd_u8(buf, &mut p)?);
        let _ = rd_u16(buf, &mut p)?;
        let _ = rd_u8(buf, &mut p)?;
        let comment = read_text_block(buf, base.links.get(1).copied().unwrap_or(0))?;
        Ok(FhBlockV4 {
            base,
            time_stamp,
            utc_offset,
            dst_offset,
            flags,
            comment,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>, is_last: bool) -> Result<u64> {
        let base = BlockBaseV4::new(*b"FH", 56, 2);
        let start = base.write_header(w) as usize;
        wr_u64(w, self.time_stamp);
        wr_i16(w, self.utc_offset);
        wr_i16(w, self.dst_offset);
        wr_u8(w, self.flags.bits());
        wr_u16(w, 0);
        wr_u8(w, 0);
        let comment = write_text_block(w, &self.comment, false);
        let next = if is_last { 0 } else { w.len() as i64 };
        patch_link(w, start, 0, next);
        patch_link(w, start, 1, comment);
        Ok(start as u64)
    }
}

// ---------------------------------------------------------------------------
// HDBLOCKV4
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HdBlockV4 {
    pub base: BlockBaseV4,
    pub start_time: u64,
    pub utc_offset: i16,
    pub dst_offset: i16,
    pub flags: TimeFlagsType,
    pub time_quality: TimeQualityType,
    pub start_angle: f64,
    pub start_distance: f64,
    pub comment: String,
    pub dg_blocks: Vec<DgBlockV4>,
    pub fh_blocks: Vec<FhBlockV4>,
    pub ch_blocks: Vec<ChBlockV4>,
    pub at_blocks: Vec<AtBlockV4>,
    pub ev_blocks: Vec<EvBlockV4>,
}

impl HdBlockV4 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        start_time_100ns: u64,
        utc_offset: i16,
        dst_offset: i16,
        time_quality: TimeQualityType,
        author: &str,
        organization: &str,
        project: &str,
        subject: &str,
        text: &str,
    ) -> Self {
        let comment = HdComment {
            comment: CommentBase {
                tx: text.to_string(),
                common_properties: Some(vec![
                    ENode::E(EType {
                        base: EBase {
                            name: Some("author".to_string()),
                            ..Default::default()
                        },
                        text: Some(author.to_string()),
                        ..Default::default()
                    }),
                    ENode::E(EType {
                        base: EBase {
                            name: Some("organization".to_string()),
                            ..Default::default()
                        },
                        text: Some(organization.to_string()),
                        ..Default::default()
                    }),
                    ENode::E(EType {
                        base: EBase {
                            name: Some("project".to_string()),
                            ..Default::default()
                        },
                        text: Some(project.to_string()),
                        ..Default::default()
                    }),
                    ENode::E(EType {
                        base: EBase {
                            name: Some("subject".to_string()),
                            ..Default::default()
                        },
                        text: Some(subject.to_string()),
                        ..Default::default()
                    }),
                ]),
            },
            ..Default::default()
        };
        let flags = TimeFlagsType::OFFSETS_VALID;
        let fh = FhBlockV4::new(author, start_time_100ns, utc_offset, dst_offset, flags);
        HdBlockV4 {
            base: BlockBaseV4::new(*b"HD", 104, 6),
            start_time: start_time_100ns,
            utc_offset,
            dst_offset,
            flags,
            time_quality,
            start_angle: f64::NAN,
            start_distance: f64::NAN,
            comment: comment.serialize(),
            fh_blocks: vec![fh],
            ..Default::default()
        }
    }

    pub fn parse(buf: &[u8], pos: u64) -> Result<Self> {
        let (base, data_off) = BlockBaseV4::parse_header(buf, pos)?;
        let mut p = data_off;
        let start_time = rd_u64(buf, &mut p)?;
        let utc_offset = rd_i16(buf, &mut p)?;
        let dst_offset = rd_i16(buf, &mut p)?;
        let flags = TimeFlagsType::from_bits(rd_u8(buf, &mut p)?);
        let time_quality =
            TimeQualityType::from_raw(u16::from(rd_u8(buf, &mut p)?)).ok_or_else(|| {
                Error::Parse {
                    offset: pos,
                    message: "unknown time quality".into(),
                }
            })?;
        let start_flags = rd_u8(buf, &mut p)?;
        let mut start_angle = f64::NAN;
        let mut start_distance = f64::NAN;
        let v1 = rd_f64(buf, &mut p)?;
        if start_flags & 1 > 0 {
            start_angle = v1;
        }
        let v2 = rd_f64(buf, &mut p)?;
        if start_flags & 2 > 0 {
            start_distance = v2;
        }
        let link = |i: usize| base.links.get(i).copied().unwrap_or(0);
        let mut dg_blocks = Vec::new();
        let mut l = link(0);
        while l > 0 {
            let dg = DgBlockV4::parse(buf, l as u64)?;
            l = dg.base.links[0];
            dg_blocks.extend(dg.resort()?);
        }
        let mut fh_blocks = Vec::new();
        let mut l = link(1);
        while l > 0 {
            let fh = FhBlockV4::parse(buf, l as u64)?;
            l = fh.base.links[0];
            fh_blocks.push(fh);
        }
        let mut ch_blocks = Vec::new();
        let mut l = link(2);
        while l > 0 {
            let ch = ChBlockV4::parse(buf, l as u64)?;
            l = ch.base.links[0];
            ch_blocks.push(ch);
        }
        let mut at_blocks = Vec::new();
        let mut l = link(3);
        while l > 0 {
            let at = AtBlockV4::parse(buf, l as u64)?;
            l = at.base.links[0];
            at_blocks.push(at);
        }
        let mut ev_blocks = Vec::new();
        let mut l = link(4);
        while l > 0 {
            let ev = EvBlockV4::parse(buf, l as u64)?;
            l = ev.base.links[0];
            ev_blocks.push(ev);
        }
        let comment = read_text_block(buf, link(5))?;
        Ok(HdBlockV4 {
            base,
            start_time,
            utc_offset,
            dst_offset,
            flags,
            time_quality,
            start_angle,
            start_distance,
            comment,
            dg_blocks,
            fh_blocks,
            ch_blocks,
            at_blocks,
            ev_blocks,
        })
    }

    pub fn write_block(&self, w: &mut Vec<u8>) -> Result<u64> {
        if !self.ev_blocks.is_empty() {
            return Err(Error::Write("EV block writing is not supported".into()));
        }
        let base = BlockBaseV4::new(*b"HD", 104, 6);
        let start = base.write_header(w) as usize;
        wr_u64(w, self.start_time);
        wr_i16(w, self.utc_offset);
        wr_i16(w, self.dst_offset);
        wr_u8(w, self.flags.bits());
        wr_u8(w, self.time_quality.raw() as u8);
        let mut start_flags = 0u8;
        if !self.start_angle.is_nan() {
            start_flags |= 1;
        }
        if !self.start_distance.is_nan() {
            start_flags |= 2;
        }
        wr_u8(w, start_flags);
        wr_f64(
            w,
            if self.start_angle.is_nan() {
                0.0
            } else {
                self.start_angle
            },
        );
        wr_f64(
            w,
            if self.start_distance.is_nan() {
                0.0
            } else {
                self.start_distance
            },
        );
        let comment = write_text_block(w, &self.comment, false);
        let first_dg = if self.dg_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.dg_blocks.len();
        for (i, dg) in self.dg_blocks.iter().enumerate() {
            dg.write_block(w, i + 1 == n)?;
        }
        let first_fh = if self.fh_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.fh_blocks.len();
        for (i, fh) in self.fh_blocks.iter().enumerate() {
            fh.write_block(w, i + 1 == n)?;
        }
        let first_ch = if self.ch_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.ch_blocks.len();
        for (i, ch) in self.ch_blocks.iter().enumerate() {
            ch.write_block(w, i + 1 == n)?;
        }
        let first_at = if self.at_blocks.is_empty() {
            0
        } else {
            w.len() as i64
        };
        let n = self.at_blocks.len();
        for (i, at) in self.at_blocks.iter().enumerate() {
            at.write_block(w, i + 1 == n)?;
        }
        let first_ev = 0i64;
        patch_link(w, start, 0, first_dg);
        patch_link(w, start, 1, first_fh);
        patch_link(w, start, 2, first_ch);
        patch_link(w, start, 3, first_at);
        patch_link(w, start, 4, first_ev);
        patch_link(w, start, 5, comment);
        Ok(start as u64)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn feq(a: f64, b: f64) -> bool {
        a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
    }

    fn clear_links(b: &mut BlockBaseV4) {
        for l in &mut b.links {
            *l = 0;
        }
    }

    #[test]
    fn id_block_roundtrip() {
        let id = IdBlockV4::new("autors", 410);
        let mut w = Vec::new();
        assert_eq!(id.write_block(&mut w).unwrap(), 0);
        assert_eq!(w.len(), 64);
        assert_eq!(&w[0..8], b"MDF     ");
        assert_eq!(&w[8..16], b"4.10    ");
        assert_eq!(&w[16..22], b"autors");
        assert_eq!(u16::from_le_bytes([w[28], w[29]]), 410);
        let back = IdBlockV4::parse(&w).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn text_block_write_read() {
        let mut w = vec![0u8; 8];
        let link = write_text_block(&mut w, "hello", false);
        assert_eq!(link, 8);
        assert_eq!(w.len(), 40);
        assert_eq!(&w[8..12], b"##TX");
        assert_eq!(i64::from_le_bytes(w[16..24].try_into().unwrap()), 30);
        assert_eq!(read_text_block(&w, link).unwrap(), "hello");

        let mut w = vec![0u8; 8];
        let link = write_text_block(&mut w, "1234567", true);
        assert_eq!(w.len(), 48);
        assert_eq!(&w[8..12], b"##MD");
        assert_eq!(read_text_block(&w, link).unwrap(), "1234567");

        let mut w = Vec::new();
        assert_eq!(write_text_block(&mut w, "", false), 0);
        assert!(w.is_empty());
        assert_eq!(read_text_block(&w, 0).unwrap(), "");
    }

    #[test]
    fn rdsd_block_roundtrip() {
        let rd = RdSdBlockV4::new_data(vec![1, 2, 3, 4], false);
        let mut w = Vec::new();
        rd.write_block(&mut w).unwrap();
        assert_eq!(&w[0..4], b"##RD");
        assert_eq!(w.len(), 32);
        let back = RdSdBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.data, vec![1, 2, 3, 4]);
        assert_eq!(back.base.id, *b"RD");

        let sd = RdSdBlockV4::new_data(vec![9, 9], true);
        let mut w = Vec::new();
        sd.write_block(&mut w).unwrap();
        assert_eq!(&w[0..4], b"##SD");

        let tx = RdSdBlockV4::new_text("text", false);
        let mut w = Vec::new();
        tx.write_block(&mut w).unwrap();
        assert_eq!(RdSdBlockV4::parse(&w, 0).unwrap().text(), "text");
    }

    #[test]
    fn bad_magic_rejected() {
        let buf = vec![0u8; 64];
        assert!(BlockBaseV4::parse_header(&buf, 0).is_err());
    }

    #[test]
    fn transpose_unit() {
        let src = b"abcdef";
        assert_eq!(transpose(src, 3, 2), b"adbecf");
        assert_eq!(transpose(b"adbecf", 2, 3), b"abcdef");
        assert_eq!(transpose(b"abcdefg", 3, 2), b"adbecfg");
    }

    #[test]
    fn dz_deflate_roundtrip() {
        let data: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
        let dz = DzBlockV4::from_uncompressed(&data, ZipType::Deflate, 0, *b"DT").unwrap();
        assert_eq!(dz.decompress().unwrap(), data);
        let mut w = Vec::new();
        dz.write_block(&mut w).unwrap();
        assert_eq!(&w[0..4], b"##DZ");
        assert_eq!(&w[24..26], b"DT");
        let back = DzBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back, dz);
        assert_eq!(back.decompress().unwrap(), data);
    }

    #[test]
    fn dz_transpose_roundtrip() {
        let data: Vec<u8> = (0..16u8).collect();
        let dz =
            DzBlockV4::from_uncompressed(&data, ZipType::TransposeAndDeflate, 4, *b"DT").unwrap();
        assert_eq!(dz.decompress().unwrap(), data);
        let mut w = Vec::new();
        dz.write_block(&mut w).unwrap();
        let back = DzBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.decompress().unwrap(), data);
        assert_eq!(back.zip_parameter, 4);
    }

    #[test]
    fn cc_linear_roundtrip() {
        let mut cc = CcBlockV4::new(ConversionType::ParametricLinear, "rpm", 0.0, 100.0);
        cc.params = vec![5.0, 2.0]; // offset 5, factor 2 → phys = raw*2 + 5
        let mut w = Vec::new();
        cc.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.conversion_type, ConversionType::ParametricLinear);
        assert_eq!(back.unit, "rpm");
        assert_eq!(back.params, vec![5.0, 2.0]);
        assert!(feq(back.min, 0.0) && feq(back.max, 100.0));
        assert!(back.flags.contains(ConversionFlags::LIMIT_RANGE_VALID));
        let mut w2 = Vec::new();
        back.write_block(&mut w2).unwrap();
        assert_eq!(w, w2);
        assert!(feq(back.to_physical(10.0, false), 25.0));
        assert!(feq(back.to_physical(25.0, true), 10.0));
    }

    #[test]
    fn cc_rational_roundtrip() {
        let mut cc = CcBlockV4::new(ConversionType::Rational, "", f64::NAN, f64::NAN);
        // phys = (a x² + b x + c) / (d x² + e x + f)
        cc.params = vec![0.0, 2.0, 0.0, 0.0, 0.0, 1.0]; // phys = 2x
        let mut w = Vec::new();
        cc.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.params.len(), 6);
        let rc = RationalCoeffs::from_params(&back.params).unwrap();
        assert!(feq(rc.to_raw(10.0), 20.0));
        assert!(feq(rc.to_physical(20.0), 10.0));
        assert!(feq(back.to_physical(20.0, false), 40.0));
        assert!(feq(back.to_physical(10.0, true), 5.0));
    }

    #[test]
    fn cc_tab_and_tab_int() {
        let mut cc = CcBlockV4::new(ConversionType::Tab, "", f64::NAN, f64::NAN);
        cc.num_pairs = vec![(1.0, 10.0), (2.0, 20.0)];
        let mut w = Vec::new();
        cc.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.tab_size, 4);
        assert_eq!(back.num_pairs, vec![(1.0, 10.0), (2.0, 20.0)]);
        assert!(feq(back.to_physical(2.0, false), 20.0));
        assert!(feq(back.to_physical(99.0, false), 99.0));

        let mut cci = CcBlockV4::new(ConversionType::TabInt, "", f64::NAN, f64::NAN);
        cci.num_pairs = vec![(0.0, 0.0), (10.0, 100.0)];
        let mut w = Vec::new();
        cci.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert!(feq(back.to_physical(5.0, false), 50.0));
        assert!(feq(back.to_physical(-1.0, false), 0.0));
        assert!(feq(back.to_physical(99.0, false), 100.0));
    }

    #[test]
    fn cc_text_table_roundtrip() {
        let mut cc = CcBlockV4::new(ConversionType::TextTable, "", f64::NAN, f64::NAN);
        cc.text_table = vec![(1.0, "one".to_string()), (2.0, "two".to_string())];
        cc.default_text = "other".to_string();
        let mut w = Vec::new();
        cc.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.text_table, cc.text_table);
        assert_eq!(back.default_text, "other");
        assert_eq!(back.tab_size, 2);
    }

    #[test]
    fn cc_text_formula_roundtrip() {
        let mut cc = CcBlockV4::new(ConversionType::TextFormula, "", f64::NAN, f64::NAN);
        cc.formula = "X*2+1".to_string();
        let mut w = Vec::new();
        cc.write_block(&mut w).unwrap();
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.formula, "X*2+1");
        assert!(feq(back.to_physical(3.0, false), 3.0));
    }

    #[test]
    fn cc_text_range_parse() {
        let mut w = Vec::new();
        let base = BlockBaseV4::new(*b"CC", 80 + 8 + 16, 6);
        let start = base.write_header(&mut w) as usize;
        wr_u8(&mut w, 8); // TextRange
        wr_u8(&mut w, 0);
        wr_u16(&mut w, 0);
        wr_u16(&mut w, 0);
        wr_u16(&mut w, 2);
        wr_zeros(&mut w, 16);
        wr_f64(&mut w, 1.5);
        wr_f64(&mut w, 9.5);
        let l0 = write_text_block(&mut w, "low", false);
        let l1 = write_text_block(&mut w, "def", false);
        patch_link(&mut w, start, 4, l0);
        patch_link(&mut w, start, 5, l1);
        let back = CcBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.conversion_type, ConversionType::TextRange);
        assert_eq!(back.text_range, vec![("low".to_string(), 1.5, 9.5)]);
        assert_eq!(back.default_text, "def");
    }

    #[test]
    fn cn_comment_xml() {
        let c = CnComment {
            base: BaseNames {
                comment: CommentBase {
                    tx: "desc text".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            raster: Some(RasterType {
                value: 0.01,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            c.serialize(),
            "<CNcomment><TX>desc text</TX><raster>0.01</raster></CNcomment>"
        );
        let back = CnComment::parse(&c.serialize()).unwrap();
        assert_eq!(back, c);
        assert!(feq(back.raster.as_ref().unwrap().value, 0.01));

        assert_eq!(
            CnComment::default().serialize(),
            "<CNcomment><TX /></CNcomment>"
        );
        assert_eq!(
            CnComment::parse("<CNcomment><TX /></CNcomment>").unwrap(),
            CnComment::default()
        );
    }

    #[test]
    fn cn_comment_xml_full() {
        let c = CnComment {
            base: BaseNames {
                comment: CommentBase {
                    tx: "d".to_string(),
                    common_properties: Some(vec![
                        ENode::E(EType {
                            base: EBase {
                                name: Some("author".to_string()),
                                ..Default::default()
                            },
                            text: Some("me & you".to_string()),
                            unit: Some("rpm".to_string()),
                            ..Default::default()
                        }),
                        ENode::Tree(TreeType {
                            base: EBase {
                                name: Some("grp".to_string()),
                                desc: Some("g".to_string()),
                            },
                            sub_nodes: vec![ENode::E(EType {
                                base: EBase {
                                    name: Some("leaf".to_string()),
                                    ..Default::default()
                                },
                                ci: 2,
                                ro: true,
                                type_attr: Some("int".to_string()),
                                text: Some("42".to_string()),
                                ..Default::default()
                            })],
                        }),
                    ]),
                },
                names: Some(vec![
                    NamesItem::Name(NamesBase {
                        text: Some("n".to_string()),
                        ci: 1,
                    }),
                    NamesItem::Display(NamesBase {
                        text: Some("disp".to_string()),
                        ci: 0,
                    }),
                ]),
            },
            linker_name: Some("sym".to_string()),
            linker_address: Some(AddressType {
                address: Some("0x8000".to_string()),
                byte_count: 4,
            }),
            axis_monotony: MonotonyType::MON_INCREASE,
            ..Default::default()
        };
        let xml = c.serialize();
        let back = CnComment::parse(&xml).unwrap();
        assert_eq!(back, c);
        assert!(xml.contains("me &amp; you"));
        assert!(xml.contains("type=\"int\""));
        assert!(xml.contains("ro=\"true\""));
        assert!(xml.contains("<axis_monotony>MON_INCREASE</axis_monotony>"));
        assert!(CnComment::parse("not xml").is_none());
        assert!(CnComment::parse("<HDcomment />").is_none());
    }

    #[test]
    fn fh_comment_xml() {
        let c = FhComment {
            comment: CommentBase {
                tx: "File created".to_string(),
                ..Default::default()
            },
            tool_vendor: Some("https://jnachbur.de".to_string()),
            user_name: Some("tester".to_string()),
            ..Default::default()
        };
        assert_eq!(
            c.serialize(),
            "<FHcomment><TX>File created</TX><tool_vendor>https://jnachbur.de</tool_vendor><user_name>tester</user_name></FHcomment>"
        );
        assert_eq!(FhComment::parse(&c.serialize()).unwrap(), c);
    }

    #[test]
    fn hd_comment_xml() {
        let hd = HdBlockV4::new(
            0,
            0,
            0,
            TimeQualityType::LocalPc,
            "s1",
            "s2",
            "s3",
            "s4",
            "tx",
        );
        assert_eq!(
            hd.comment,
            "<HDcomment><TX>tx</TX><common_properties><e name=\"author\">s1</e><e name=\"organization\">s2</e><e name=\"project\">s3</e><e name=\"subject\">s4</e></common_properties></HDcomment>"
        );
        let c = HdComment::parse(&hd.comment).unwrap();
        assert_eq!(c.comment.tx, "tx");
        let props = c.comment.common_properties.unwrap();
        assert_eq!(props.len(), 4);
        match &props[0] {
            ENode::E(e) => {
                assert_eq!(e.base.name.as_deref(), Some("author"));
                assert_eq!(e.text.as_deref(), Some("s1"));
            }
            ENode::Tree(_) => panic!("expected <e>"),
        }
    }

    #[test]
    fn si_comment_xml() {
        assert_eq!(
            SiComment::default().serialize(),
            "<SIcomment><TX /></SIcomment>"
        );
        let c = SiComment {
            base: BaseNames {
                comment: CommentBase {
                    tx: "src".to_string(),
                    ..Default::default()
                },
                names: Some(vec![NamesItem::Vendor(NamesBase {
                    text: Some("v".to_string()),
                    ci: 0,
                })]),
            },
            path: Some(NamesBase {
                text: Some("p".to_string()),
                ci: 0,
            }),
            bus: Some(NamesBase {
                text: Some("CAN".to_string()),
                ci: 3,
            }),
            protocol: Some("XCP".to_string()),
        };
        let xml = c.serialize();
        assert_eq!(SiComment::parse(&xml).unwrap(), c);
        assert!(xml.contains("<names><vendor>v</vendor></names>"));
    }

    #[test]
    fn si_block_roundtrip() {
        let si = SiBlockV4::new(
            SourceType::ECU,
            BusType::CAN,
            "ecu1",
            "p1",
            "cmt",
            SourceFlags::SIMULATED_SOURCE,
        );
        let mut w = Vec::new();
        si.write_block(&mut w).unwrap();
        let mut back = SiBlockV4::parse(&w, 0).unwrap();
        clear_links(&mut back.base);
        assert_eq!(back, si);
        let mut w2 = Vec::new();
        back.write_block(&mut w2).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn fh_block_roundtrip() {
        let fh = FhBlockV4::new("tester", 1000, 60, 30, TimeFlagsType::OFFSETS_VALID);
        assert_eq!(
            fh.comment,
            "<FHcomment><TX>File created</TX><tool_vendor>https://jnachbur.de</tool_vendor><user_name>tester</user_name></FHcomment>"
        );
        let mut w = Vec::new();
        fh.write_block(&mut w, true).unwrap();
        let mut back = FhBlockV4::parse(&w, 0).unwrap();
        clear_links(&mut back.base);
        assert_eq!(back, fh);
        let mut w2 = Vec::new();
        back.write_block(&mut w2, true).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn sr_block_roundtrip() {
        let sr = SrBlockV4 {
            nr_of_red_samples: 7,
            len_of_time_int: 0.25,
            flags: SrFlags::INVALIDATION_BYTES,
            data_blocks: vec![DataBlockV4::Dt(DtBlockV4::new(vec![1, 2, 3, 4]))],
            ..Default::default()
        };
        let mut w = Vec::new();
        sr.write_block(&mut w, true).unwrap();
        let back = SrBlockV4::parse(&w, 0).unwrap();
        assert!(feq(back.len_of_time_int, 0.25));
        assert_eq!(back.nr_of_red_samples, 7);
        assert_eq!(back.data_blocks.len(), 1);
        match &back.data_blocks[0] {
            DataBlockV4::Dt(dt) => assert_eq!(dt.data, vec![1, 2, 3, 4]),
            other => panic!("expected DT, got {other:?}"),
        }
    }

    #[test]
    fn ca_block_roundtrip() {
        let ca = CaBlockV4 {
            base: BlockBaseV4::new(*b"CA", 48, 0),
            ca_type: CaType::LookUp,
            template_type: CaTemplate::CG,
            n_dim: 2,
            flags: CaFlags::FIXED_AXIS | CaFlags::AXIS,
            byte_offset_base: 8,
            inval_bit_pos_base: 3,
            dim_size: 16,
        };
        let mut w = Vec::new();
        ca.write_block(&mut w).unwrap();
        let back = CaBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back, ca);
    }

    #[test]
    fn at_block_roundtrip() {
        let at = AtBlockV4 {
            base: BlockBaseV4::new(*b"AT", 64, 4),
            filename: "a.csv".to_string(),
            mime_type: "text/csv".to_string(),
            comment: "note".to_string(),
            flags: AttachmentFlags::EMBEDDED_DATA | AttachmentFlags::MD5_CHECKSUM_VALID,
            creator_index: 2,
        };
        let mut w = Vec::new();
        at.write_block(&mut w, true).unwrap();
        let mut back = AtBlockV4::parse(&w, 0).unwrap();
        clear_links(&mut back.base);
        assert_eq!(back, at);
    }

    #[test]
    fn ch_block_roundtrip() {
        let ch = ChBlockV4 {
            name: "grp".to_string(),
            comment: "c".to_string(),
            hierarchy_type: HierarchyType::Function,
            dependencies: vec![DependencyType::new(1000, 2000, 3000)],
            ..Default::default()
        };
        let mut w = Vec::new();
        ch.write_block(&mut w, true).unwrap();
        let back = ChBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.name, "grp");
        assert_eq!(back.hierarchy_type, HierarchyType::Function);
        assert_eq!(back.dependencies.len(), 1);
        assert_eq!(back.dependencies[0].link_dg, 1000);
        assert_eq!(back.dependencies[0].link_cg, 3000);
        assert_eq!(back.dependencies[0].link_cn, 3000);
    }

    #[test]
    fn ev_block_parse() {
        let mut w = Vec::new();
        let base = BlockBaseV4::new(*b"EV", 96, 5);
        let start = base.write_header(&mut w) as usize;
        wr_u8(&mut w, EventType::Marker.raw());
        wr_u8(&mut w, SyncType::Time.raw());
        wr_u8(&mut w, RangeType::Point.raw());
        wr_u8(&mut w, CauseType::User.raw());
        wr_u8(&mut w, EventFlags::POST_PROCESSING.bits());
        wr_zeros(&mut w, 3);
        wr_u32(&mut w, 0);
        wr_u16(&mut w, 0);
        wr_u16(&mut w, 7); // creator index
        wr_i64(&mut w, 12345);
        wr_f64(&mut w, 1.5);
        let ln = write_text_block(&mut w, "evt", false);
        let lc = write_text_block(&mut w, "cmt", false);
        patch_link(&mut w, start, 3, ln);
        patch_link(&mut w, start, 4, lc);
        let back = EvBlockV4::parse(&w, 0).unwrap();
        assert_eq!(back.event_type, EventType::Marker);
        assert_eq!(back.sync_type, SyncType::Time);
        assert_eq!(back.cause_type, CauseType::User);
        assert!(back.flags.contains(EventFlags::POST_PROCESSING));
        assert_eq!(back.creator_index, 7);
        assert_eq!(back.sync_base_value, 12345);
        assert!(feq(back.sync_factor, 1.5));
        assert_eq!(back.name, "evt");
        assert_eq!(back.comment, "cmt");
    }

    #[test]
    fn cg_record_size_with_id() {
        let cg = CgBlockV4 {
            record_size: 10,
            inval_size: 1,
            ..Default::default()
        };
        assert_eq!(cg.record_size_with_id(RecordIdType::None), 11);
        assert_eq!(cg.record_size_with_id(RecordIdType::Before8Bit), 12);
        assert_eq!(cg.record_size_with_id(RecordIdType::Before16Bit), 13);
        assert_eq!(cg.record_size_with_id(RecordIdType::Before32Bit), 15);
        assert_eq!(cg.record_size_with_id(RecordIdType::Before64Bit), 19);
        assert_eq!(cg.record_size_with_id(RecordIdType::BeforeAndAfter8Bit), 13);
    }

    #[test]
    fn hl_dl_dt_chain_read_data() {
        let d1: Vec<u8> = (0..8u8).collect();
        let d2: Vec<u8> = (8..16u8).collect();
        let dz_data = DzBlockV4::from_uncompressed(&d2, ZipType::Deflate, 0, *b"DT").unwrap();
        let hl = HlBlockV4 {
            flags: DataBlockFlags::EQUAL_LENGTH,
            zip_type: ZipType::Deflate,
            dl_blocks: vec![DlBlockV4 {
                flags: DataBlockFlags::EQUAL_LENGTH,
                count: 2,
                equal_length: 8,
                data_blocks: vec![
                    DataBlockV4::Dt(DtBlockV4::new(d1.clone())),
                    DataBlockV4::Dz(dz_data),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let dg = DgBlockV4 {
            data_blocks: vec![DataBlockV4::Hl(hl)],
            ..Default::default()
        };
        let mut w = Vec::new();
        dg.write_block(&mut w, true).unwrap();
        let back = DgBlockV4::parse(&w, 0).unwrap();
        let mut expected = d1.clone();
        expected.extend_from_slice(&d2);
        assert_eq!(back.read_data().unwrap(), expected);
        let mut w2 = Vec::new();
        back.write_block(&mut w2, true).unwrap();
        assert_eq!(w, w2);
    }

    fn synth_small_file() -> (Vec<u8>, Vec<u8>) {
        let mut w = Vec::new();
        IdBlockV4::new("autors", 410).write_block(&mut w).unwrap();
        let mut hd = HdBlockV4::new(
            0,
            60,
            0,
            TimeQualityType::LocalPc,
            "author",
            "org",
            "proj",
            "subj",
            "note",
        );
        let mut dg = DgBlockV4::new();
        dg.record_id_type = RecordIdType::None;
        let mut cg = CgBlockV4::new(None);
        cg.record_id = 1;
        cg.record_count = 3;
        cg.record_size = 12;
        let cn_time = CnBlockV4::new(
            SignalType::FloatLe,
            ChannelType::Master,
            "time",
            Some("time channel"),
            0,
            0,
            64,
            None,
            SyncType::Time,
            ChannelFlags::MONOTONOUS,
            0,
            0,
        );
        let mut cc = CcBlockV4::new(ConversionType::ParametricLinear, "rpm", 0.0, 100.0);
        cc.params = vec![0.0, 2.0];
        let cn_val = CnBlockV4::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "rpm",
            Some("engine speed"),
            0,
            64,
            32,
            Some(Box::new(cc)),
            SyncType::None,
            ChannelFlags::NONE,
            0,
            0,
        );
        cg.cn_blocks = vec![cn_time, cn_val];
        dg.cg_blocks = vec![cg];
        let mut data = Vec::new();
        for i in 0..3u32 {
            data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
            data.extend_from_slice(&(1000u32 + i).to_le_bytes());
        }
        dg.data_blocks = vec![DataBlockV4::Dt(DtBlockV4::new(data.clone()))];
        hd.dg_blocks = vec![dg];
        hd.write_block(&mut w).unwrap();
        (w, data)
    }

    #[test]
    fn small_mdf4_roundtrip() {
        let (w, data) = synth_small_file();
        assert_eq!(w.len() % 8, 0);
        let id = IdBlockV4::parse(&w).unwrap();
        assert_eq!(id.version, 410);
        let hd = HdBlockV4::parse(&w, 64).unwrap();
        assert_eq!(hd.utc_offset, 60);
        assert!(hd.comment.contains("<HDcomment>"));
        assert_eq!(hd.fh_blocks.len(), 1);
        assert_eq!(hd.dg_blocks.len(), 1);
        let dg = &hd.dg_blocks[0];
        assert_eq!(dg.record_id_type, RecordIdType::None);
        assert_eq!(dg.read_data().unwrap(), data);
        let cg = &dg.cg_blocks[0];
        assert_eq!(cg.record_count, 3);
        assert_eq!(cg.record_size, 12);
        assert_eq!(cg.cn_blocks.len(), 2);
        let t = &cg.cn_blocks[0];
        assert_eq!(t.name, "time");
        assert_eq!(t.channel_type, ChannelType::Master);
        assert_eq!(t.no_of_bits, 64);
        assert_eq!(t.description(), "time channel");
        assert_eq!(t.sync_type, SyncType::Time);
        let v = &cg.cn_blocks[1];
        assert_eq!(v.name, "rpm");
        assert_eq!(v.add_offset, 8);
        let cc = v.cc_block.as_ref().unwrap();
        assert_eq!(cc.unit, "rpm");
        assert_eq!(cc.params, vec![0.0, 2.0]);
        assert!(feq(v.min, 0.0) && feq(v.max, 100.0));
        assert!(v.flags.contains(ChannelFlags::LIMIT_RANGE_VALID));
        assert!(feq(cc.to_physical(1000.0, false), 2000.0));
        let mut w2 = Vec::new();
        id.write_block(&mut w2).unwrap();
        hd.write_block(&mut w2).unwrap();
        assert_eq!(w.len(), w2.len());
        assert_eq!(w, w2);
    }

    #[test]
    fn small_mdf4_dz_data_roundtrip() {
        let mut w = Vec::new();
        IdBlockV4::new("autors", 410).write_block(&mut w).unwrap();
        let mut hd = HdBlockV4::new(0, 0, 0, TimeQualityType::LocalPc, "a", "o", "p", "s", "t");
        let mut dg = DgBlockV4::new();
        let mut cg = CgBlockV4::new(None);
        cg.record_id = 1;
        cg.record_count = 2;
        cg.record_size = 8;
        let cn = CnBlockV4::new(
            SignalType::FloatLe,
            ChannelType::Master,
            "time",
            None,
            0,
            0,
            64,
            None,
            SyncType::Time,
            ChannelFlags::NONE,
            0,
            0,
        );
        cg.cn_blocks = vec![cn];
        dg.cg_blocks = vec![cg];
        let data: Vec<u8> = (0..16u8).collect();
        let dz = DzBlockV4::from_uncompressed(&data, ZipType::Deflate, 0, *b"DT").unwrap();
        dg.data_blocks = vec![DataBlockV4::Dz(dz)];
        hd.dg_blocks = vec![dg];
        hd.write_block(&mut w).unwrap();

        let hd2 = HdBlockV4::parse(&w, 64).unwrap();
        assert_eq!(hd2.dg_blocks[0].read_data().unwrap(), data);
        let cn2 = &hd2.dg_blocks[0].cg_blocks[0].cn_blocks[0];
        assert_eq!(cn2.comment, "<CNcomment><TX /></CNcomment>");
        let mut w2 = Vec::new();
        IdBlockV4::parse(&w).unwrap().write_block(&mut w2).unwrap();
        hd2.write_block(&mut w2).unwrap();
        assert_eq!(w, w2);
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    fn resort_sample() -> (DgBlockV4, Vec<u8>) {
        let mut dg = DgBlockV4::new();
        dg.record_id_type = RecordIdType::Before8Bit;
        let mut cg1 = CgBlockV4::new(None);
        cg1.record_id = 1;
        cg1.record_size = 4;
        let mut cg2 = CgBlockV4::new(None);
        cg2.record_id = 2;
        cg2.record_size = 2;
        dg.cg_blocks = vec![cg1, cg2];
        let data: Vec<u8> = vec![
            1, 0xA1, 0xA2, 0xA3, 0xA4, //
            2, 0xB1, 0xB2, //
            1, 0xC1, 0xC2, 0xC3, 0xC4, //
            1, 0xD1, 0xD2, 0xD3, 0xD4, //
            2, 0xE1, 0xE2,
        ];
        dg.data_blocks = vec![DataBlockV4::Dt(DtBlockV4::new(data.clone()))];
        (dg, data)
    }

    #[test]
    fn resort_splits_scrambled_records() {
        let (dg, _) = resort_sample();
        let out = dg.resort().unwrap();
        assert_eq!(out.len(), 2);
        let dg1 = &out[0];
        assert_eq!(dg1.record_id_type, RecordIdType::Before8Bit);
        assert_eq!(dg1.comment, "Resorted Data to Channel Group (Record ID 1)");
        assert_eq!(dg1.cg_blocks.len(), 1);
        assert_eq!(dg1.cg_blocks[0].record_id, 1);
        assert_eq!(dg1.cg_blocks[0].record_count, 3);
        assert_eq!(
            dg1.read_data().unwrap(),
            vec![1, 0xA1, 0xA2, 0xA3, 0xA4, 1, 0xC1, 0xC2, 0xC3, 0xC4, 1, 0xD1, 0xD2, 0xD3, 0xD4]
        );
        let dg2 = &out[1];
        assert_eq!(dg2.comment, "Resorted Data to Channel Group (Record ID 2)");
        assert_eq!(dg2.cg_blocks[0].record_id, 2);
        assert_eq!(dg2.cg_blocks[0].record_count, 2);
        assert_eq!(dg2.read_data().unwrap(), vec![2, 0xB1, 0xB2, 2, 0xE1, 0xE2]);
        assert_eq!(dg.cg_blocks.len(), 2);
        assert_eq!(dg.comment, "");
    }

    #[test]
    fn resort_no_split_single_cg_or_no_record_id() {
        let (mut dg, _) = resort_sample();
        dg.cg_blocks.truncate(1);
        let out = dg.resort().unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cg_blocks[0].record_id, 1);
        let (mut dg, _) = resort_sample();
        dg.record_id_type = RecordIdType::None;
        let out = dg.resort().unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cg_blocks.len(), 2);
    }

    #[test]
    fn resort_unknown_record_id_stops() {
        let (mut dg, _) = resort_sample();
        let mut data = dg.read_data().unwrap();
        data.extend_from_slice(&[9, 0xFF, 0xFF, 0xFF, 0xFF]);
        data.extend_from_slice(&[1, 0xF1, 0xF2, 0xF3, 0xF4]);
        dg.data_blocks = vec![DataBlockV4::Dt(DtBlockV4::new(data))];
        let out = dg.resort().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].cg_blocks[0].record_count, 3,
            "records after an unknown ID are discarded"
        );
        assert_eq!(out[1].cg_blocks[0].record_count, 2);
    }

    #[test]
    fn resort_vlsd_records_skipped() {
        let (mut dg, _) = resort_sample();
        dg.cg_blocks[1].flags = ChannelGroupFlags::VLSD;
        let data: Vec<u8> = vec![
            1, 0xA1, 0xA2, 0xA3, 0xA4, //
            2, 3, 0, 0, 0, 0xB1, 0xB2, 0xB3, // VLSD:len=3
            1, 0xC1, 0xC2, 0xC3, 0xC4,
        ];
        dg.data_blocks = vec![DataBlockV4::Dt(DtBlockV4::new(data))];
        let out = dg.resort().unwrap();
        assert_eq!(
            out.len(),
            1,
            "a VLSD channel group does not create another DG"
        );
        assert_eq!(out[0].cg_blocks[0].record_id, 1);
        assert_eq!(
            out[0].cg_blocks[0].record_count, 2,
            "records after a VLSD record are still read"
        );
        assert_eq!(
            out[0].read_data().unwrap(),
            vec![1, 0xA1, 0xA2, 0xA3, 0xA4, 1, 0xC1, 0xC2, 0xC3, 0xC4]
        );
    }

    #[test]
    fn resort_over_dz_compressed_data() {
        let (mut dg, _) = resort_sample();
        let data = dg.read_data().unwrap();
        let dz = DzBlockV4::from_uncompressed(&data, ZipType::Deflate, 0, *b"DT").unwrap();
        dg.data_blocks = vec![DataBlockV4::Dz(dz)];
        let out = dg.resort().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].cg_blocks[0].record_count, 3);
        assert_eq!(
            out[1].read_data().unwrap(),
            vec![2, 0xB1, 0xB2, 2, 0xE1, 0xE2]
        );
    }

    #[test]
    fn hd_parse_resorts_multi_cg_dg() {
        let mut w = Vec::new();
        IdBlockV4::new("autors", 410).write_block(&mut w).unwrap();
        let mut hd = HdBlockV4::new(0, 0, 0, TimeQualityType::LocalPc, "a", "o", "p", "s", "t");
        let (mut dg, _) = resort_sample();
        assert_eq!(dg.cg_blocks[0].record_count, 0);
        for cg in &mut dg.cg_blocks {
            cg.cn_blocks = vec![CnBlockV4::new(
                SignalType::FloatLe,
                ChannelType::Master,
                "time",
                None,
                0,
                0,
                64,
                None,
                SyncType::Time,
                ChannelFlags::NONE,
                0,
                0,
            )];
        }
        hd.dg_blocks = vec![std::mem::take(&mut dg)];
        hd.write_block(&mut w).unwrap();

        let hd2 = HdBlockV4::parse(&w, 64).unwrap();
        assert_eq!(hd2.dg_blocks.len(), 2, "parsing splits groups by record ID");
        assert_eq!(hd2.dg_blocks[0].cg_blocks[0].record_id, 1);
        assert_eq!(hd2.dg_blocks[0].cg_blocks[0].record_count, 3);
        assert_eq!(
            hd2.dg_blocks[0].comment,
            "Resorted Data to Channel Group (Record ID 1)"
        );
        assert_eq!(hd2.dg_blocks[1].cg_blocks[0].record_id, 2);
        assert_eq!(hd2.dg_blocks[1].cg_blocks[0].record_count, 2);
        assert_eq!(
            hd2.dg_blocks[0].read_data().unwrap(),
            vec![1, 0xA1, 0xA2, 0xA3, 0xA4, 1, 0xC1, 0xC2, 0xC3, 0xC4, 1, 0xD1, 0xD2, 0xD3, 0xD4]
        );
    }
}
