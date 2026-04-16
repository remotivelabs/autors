//! MDF base layer: shared enums and flag types, annotations, the read entry
//! point [`MdfReader`], and the write entry point [`MdfWriter`].
//! ## Contents
//! - Shared enums and flag types: `ChannelType`, `ConversionType`,
//!   `DependencyType`, `CustomFlagsType`, `MdfFileExtension`, `MdfType`,
//!   `RecordIdType`, `SignalType`, `SyncType`, `TimeFlagsType`,
//!   `TimeQualityType`, `UnfinalizedFlagsType`.
//! - [`Annotation`], [`AnnotationList`], and [`RawDataRecord`].
//! - [`MdfReader`] dispatches on the format version; [`MdfWriter`] builds
//!   channel groups, channels, and records and serializes the file.
//!
//! The per-version format blocks are concrete types in [`crate::v3`] /
//! `crate::v4`; the read entry point uses an enum for version dispatch.
//! File identifiers, program identifiers, master-channel labels, annotation
//! labels, date/time formats, and the CSV separator are defined by the
//! constants in [`crate::v3`].
//!
//! Design notes:
//! - All parsing and writing is done in memory (`Vec<u8>`); the file IO
//!   (`open`/`save`) is a thin wrapper over `fs::read`/`fs::write`. This keeps
//!   the semantics simple at the cost of higher memory usage for large files.
//! - `DataPoint`/`DataPointF`/`DataLimits`/`ValueObjectFormat`/`SRange` are
//!   defined in autors-util (`helpers` module) and re-exported here so
//!   `base::DataPoint` etc. remain usable at their familiar path.
//! - Text encoding uses a Latin-1 mapping (bytes 0x00–0xFF map one-to-one; on
//!   the write side characters beyond Latin-1 are replaced with `?`). Pure
//!   ASCII text is unaffected.
//! - On write, the recording time, UTC offset, and code page are passed in
//!   explicitly by the caller rather than being taken from the system
//!   clock/locale.
//! - Write errors are propagated through `Result`; they are never swallowed.
//! - `MdfReader::save` performs a full serialization rewrite rather than
//!   patching only the ID/HD header in place (see
//!   [`crate::v3::Mdf3File::save`]).

use std::path::Path;

use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::DataType;
use autors_formula::formula::FormulaDict;
use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::v3;

// ---------------------------------------------------------------------------
// Macros and basic utilities
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

            /// Whether all bits of `other` are set.
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
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        #[repr($ty)]
        pub enum $name {
            $($(#[$vmeta])* $vname = $vval,)*
        }

        impl $name {
            /// Construct from a raw value; unknown values return None (unknown
            /// values are rejected rather than blindly cast).
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

/// Construct a parse error (with file offset).
pub(crate) fn parse_err<T>(offset: u64, message: impl Into<String>) -> Result<T> {
    Err(Error::Parse {
        offset,
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------
// Text encoding (Latin-1 mapping; see the design notes in the module docs)
// ---------------------------------------------------------------------------

/// Decode a fixed-length byte string: truncate at the first NUL, then map each
/// byte via Latin-1.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end].iter().map(|&b| b as char).collect()
}

pub(crate) fn encode_text(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| {
            let v = c as u32;
            if v <= 0xFF {
                v as u8
            } else {
                b'?'
            }
        })
        .collect()
}

pub(crate) fn fixed_bytes(s: &str, n: usize, reserve_terminator: bool) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if !s.is_empty() {
        let bytes = encode_text(s);
        let limit = if reserve_terminator {
            n.saturating_sub(1)
        } else {
            n
        };
        let copy = bytes.len().min(limit);
        buf[..copy].copy_from_slice(&bytes[..copy]);
    }
    buf
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

plain_enum! {
    ChannelType(u8) {
        #[default] Data = 0,
        VariableData = 1,
        Master = 2,
        VirtualMaster = 3,
        Sync = 4,
        MaxLenData = 5,
        VirtualData = 6,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum ConversionType {
    ParametricLinear = 0,
    TabInt = 1,
    Tab = 2,
    Polynomial = 6,
    Exponential = 7,
    Logarithmic = 8,
    Rational = 9,
    TextFormula = 10,
    TextTable = 11,
    TextRange = 12,
    Date = 132,
    Time = 133,
    TabRange = 134,
    TextToValue = 135,
    TextToText = 136,
    #[default]
    None = 0xFFFF,
}

impl ConversionType {
    pub fn from_raw(v: u16) -> Option<Self> {
        Some(match v {
            0 => Self::ParametricLinear,
            1 => Self::TabInt,
            2 => Self::Tab,
            6 => Self::Polynomial,
            7 => Self::Exponential,
            8 => Self::Logarithmic,
            9 => Self::Rational,
            10 => Self::TextFormula,
            11 => Self::TextTable,
            12 => Self::TextRange,
            132 => Self::Date,
            133 => Self::Time,
            134 => Self::TabRange,
            135 => Self::TextToValue,
            136 => Self::TextToText,
            0xFFFF => Self::None,
            _ => return None,
        })
    }

    pub fn raw(self) -> u16 {
        self as u16
    }
}

flags_newtype! {
    CustomFlagsType(u16),
    NONE = 0,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyType {
    pub link_dg: i64,
    pub link_cg: i64,
    pub link_cn: i64,
    pub target: (usize, usize, usize),
}

impl DependencyType {
    pub fn new(link_dg: i64, link_cg: i64, link_cn: i64) -> Self {
        DependencyType {
            link_dg,
            link_cg,
            link_cn,
            target: (0, 0, 0),
        }
    }
}

plain_enum! {
    MdfFileExtension(u8) {
        #[default] Dat = 0,
        Mdf = 1,
        Mf3 = 2,
        Mf4 = 3,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MdfType {
    /// V3.30.
    #[default]
    V3,
    /// V4.10.
    V4,
}

impl MdfType {
    pub fn description(self) -> &'static str {
        match self {
            MdfType::V3 => "V3.30",
            MdfType::V4 => "V4.10",
        }
    }
}

plain_enum! {
    RecordIdType(u8) {
        #[default] None = 0,
        Before8Bit = 1,
        Before16Bit = 2,
        Before32Bit = 4,
        Before64Bit = 8,
        BeforeAndAfter8Bit = 0xFF,
    }
}

plain_enum! {
    SignalType(u8) {
        #[default] UIntLe = 0,
        UIntBe = 1,
        SIntLe = 2,
        SIntBe = 3,
        FloatLe = 4,
        FloatBe = 5,
        String = 6,
        StringUtf8 = 7,
        StringUtf16Le = 8,
        StringUtf16Be = 9,
        ByteArray = 10,
        MimeSample = 11,
        MimeStream = 12,
        CanOpenData = 13,
        CanOpenTime = 14,
    }
}

impl SignalType {
    pub(crate) fn is_big_endian(self) -> bool {
        matches!(
            self,
            SignalType::UIntBe
                | SignalType::SIntBe
                | SignalType::FloatBe
                | SignalType::StringUtf16Be
        )
    }
}

plain_enum! {
    SyncType(u8) {
        #[default] None = 0,
        Time = 1,
        Angle = 2,
        Distance = 3,
        Index = 4,
    }
}

flags_newtype! {
    TimeFlagsType(u8),
    LOCAL_TIME = 1,
    OFFSETS_VALID = 2,
}

plain_enum! {
    TimeQualityType(u16) {
        #[default] LocalPc = 0,
        ExternalSource = 10,
        ExternalAbsolute = 0x10,
    }
}

flags_newtype! {
    UnfinalizedFlagsType(u16),
    NONE = 0,
    UPDATE_CG_BLOCK_REQUIRED = 1,
    UPDATE_SR_BLOCK_REQUIRED = 2,
    INVAL_LENGTH_LAST_DT = 4,
    INVAL_VLSD_CG_BLOCK_SD_LENGTH = 0x20,
}

// ---------------------------------------------------------------------------
// Annotation / RawDataRecord
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Annotation {
    pub timestamp: f64,
    pub text: String,
}

impl Annotation {
    pub fn new(timestamp: f64, text: impl Into<String>) -> Self {
        Annotation {
            timestamp,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnnotationList {
    pub items: Vec<Annotation>,
    pub max_length: usize,
}

impl AnnotationList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, annotation: Annotation) {
        self.max_length = self.max_length.max(encode_text(&annotation.text).len() + 1);
        self.items.push(annotation);
    }

    pub(crate) fn write_records(&self) -> Vec<u8> {
        let mut sorted = self.items.clone();
        sorted.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out = Vec::with_capacity(sorted.len() * (8 + self.max_length));
        for a in &sorted {
            out.extend_from_slice(&a.timestamp.to_le_bytes());
            out.extend_from_slice(&fixed_bytes(&a.text, self.max_length, true));
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawDataRecord {
    pub timestamp: f64,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub use autors_util::helpers::{DataLimits, DataPoint, DataPointF, SRange, ValueObjectFormat};

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum MdfReader {
    /// MDF V3.x.
    V3(Box<v3::Mdf3File>),
}

impl MdfReader {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let id = v3::IdBlock::parse(data)?;
        if id.file_id != v3::FILE_ID_MDF && id.file_id != v3::FILE_ID_UNFINALIZED {
            return parse_err(0, format!("not an MDF file (file id {:?})", id.file_id));
        }
        if id.version >= 400 {
            return parse_err(
                0,
                format!("MDF version {} not supported yet (V4 pending)", id.version),
            );
        }
        Ok(MdfReader::V3(Box::new(v3::Mdf3File::parse(data)?)))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let data = std::fs::read(path)?;
        Self::parse(&data)
    }

    pub fn mdf_type(&self) -> MdfType {
        match self {
            MdfReader::V3(_) => MdfType::V3,
        }
    }

    pub fn as_v3(&self) -> Option<&v3::Mdf3File> {
        match self {
            MdfReader::V3(f) => Some(f),
        }
    }

    pub fn as_v3_mut(&mut self) -> Option<&mut v3::Mdf3File> {
        match self {
            MdfReader::V3(f) => Some(f),
        }
    }

    pub fn write(&self) -> Result<Vec<u8>> {
        match self {
            MdfReader::V3(f) => f.write(),
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.write()?)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

const STR_COMPRESS_FAILS: &str = "Compressing data is only supported by the V4 format.";
const STR_COMPUTATION_MISMATCH: &str = "Computation parameter count must be 2 or 6!";
const STR_RECORD_SIZE_EXCEEDED: &str =
    "Recordsize exceeded, {0} allows a maximum recordsize of {1} bytes.";
const STR_HANDLE_NOT_ALLOCATED: &str =
    "Specified handle {handle} doesn't exist. Create a new one from createChannelGroup first!";
const STR_HANDLE_RUNNING: &str = "Specified handle ({0}) already contains recording data.";
pub(crate) const STR_INVALID_TS_DESC: &str =
    "Timestamp is invalid, maybe by a connection loss...timestamps will be adjusted.";
pub(crate) const STR_RESORT_MSG: &str = "Resorted Data to Channel Group (Record ID {0})";

#[derive(Debug)]
struct GroupState {
    dg: usize,
    last_rel_ts: f64,
}

#[derive(Debug, Clone)]
enum CcSpec {
    None,
    Params(Vec<f64>),
    /// TextTable.
    TextTable(Vec<(f64, String)>),
    TabRange(Vec<(f64, SRange)>, String),
}

///   ([`v3::DgBlock::data`]).
#[derive(Debug)]
pub struct MdfWriter {
    mdf_type: MdfType,
    compress: bool,
    file: v3::Mdf3File,
    groups: IndexMap<i32, GroupState>,
    group_counter: i32,
    annotations: AnnotationList,
    annotation_dg: Option<usize>,
    annotation_last_ts: f64,
    base_ts: f64,
    data_received: bool,
    saved: bool,
}

impl MdfWriter {
    /// headerComment, MDFType.V3, false)`).
    pub fn new_v3(
        author: &str,
        organization: &str,
        project: &str,
        subject: &str,
        header_comment: &str,
        recording: v3::HdRecordingInfo,
        code_page: u16,
    ) -> Self {
        let id_block = v3::IdBlock::new_v3(v3::PROGRAM_ID, code_page);
        let hd_block = v3::HdBlock::new(
            recording,
            author,
            organization,
            project,
            subject,
            header_comment,
            "",
        );
        MdfWriter {
            mdf_type: MdfType::V3,
            compress: false,
            file: v3::Mdf3File::new(id_block, hd_block),
            groups: IndexMap::new(),
            group_counter: i32::MIN,
            annotations: AnnotationList::new(),
            annotation_dg: None,
            annotation_last_ts: f64::NAN,
            base_ts: f64::NAN,
            data_received: false,
            saved: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mdf_type: MdfType,
        compress: bool,
        author: &str,
        organization: &str,
        project: &str,
        subject: &str,
        header_comment: &str,
        recording: v3::HdRecordingInfo,
        code_page: u16,
    ) -> Result<Self> {
        match mdf_type {
            MdfType::V3 => {
                if compress {
                    return Err(Error::Write(format!(
                        "{STR_COMPRESS_FAILS} (parameter 'compress')"
                    )));
                }
                Ok(Self::new_v3(
                    author,
                    organization,
                    project,
                    subject,
                    header_comment,
                    recording,
                    code_page,
                ))
            }
            MdfType::V4 => Err(Error::Write(
                "MDF V4 writer is implemented by the parallel v4 task (not wired yet)".into(),
            )),
        }
    }

    pub fn mdf_type(&self) -> MdfType {
        self.mdf_type
    }

    pub fn compress(&self) -> bool {
        self.compress
    }

    pub fn data_received(&self) -> bool {
        self.data_received
    }

    pub fn file(&self) -> &v3::Mdf3File {
        &self.file
    }

    pub fn channel_group(&self, handle: i32) -> Option<&v3::CgBlock> {
        let key = Self::key_of(handle);
        let g = self.groups.get(&key)?;
        self.file.hd_block.dg_blocks.get(g.dg)?.cg_blocks.first()
    }

    fn key_of(handle: i32) -> i32 {
        if handle >= 0 {
            handle.wrapping_add(1)
        } else {
            handle
        }
    }

    pub fn create_channel_group(&mut self, cg_comment: Option<&str>) -> i32 {
        self.group_counter = self.group_counter.wrapping_add(1);
        let handle = self.group_counter;
        let mut dg = v3::DgBlock::default();
        let mut cg = v3::CgBlock::new(cg_comment.unwrap_or_default());
        cg.cn_blocks.push(Self::master_channel());
        cg.record_size += 8;
        dg.cg_blocks.push(cg);
        self.file.hd_block.dg_blocks.push(dg);
        let dg_idx = self.file.hd_block.dg_blocks.len() - 1;
        self.groups.insert(
            handle,
            GroupState {
                dg: dg_idx,
                last_rel_ts: f64::NAN,
            },
        );
        handle
    }

    fn master_channel() -> v3::CnBlock {
        v3::CnBlock::new(
            SignalType::FloatLe,
            ChannelType::Master,
            v3::MASTER_CHANNEL_NAME,
            v3::MASTER_CHANNEL_DESC,
            0,
            0,
            64,
            Some(v3::CcBlock::new(
                ConversionType::None,
                v3::MASTER_CHANNEL_UNIT,
                f64::NAN,
                f64::NAN,
            )),
            "",
            "",
            "",
        )
    }

    /// upperLimit, dt, bo, description, para, bitOffset)`).
    #[allow(clippy::too_many_arguments)]
    pub fn add_measurement(
        &mut self,
        handle: i32,
        name: &str,
        unit: &str,
        lower_limit: f64,
        upper_limit: f64,
        dt: DataType,
        bo: ByteOrder,
        description: Option<&str>,
        para: Option<&[f64]>,
        bit_offset: u32,
    ) -> Result<()> {
        let spec = match para {
            None => CcSpec::None,
            Some(p) if p.len() == 2 || p.len() == 6 => CcSpec::Params(p.to_vec()),
            Some(_) => {
                return Err(Error::Write(format!(
                    "{STR_COMPUTATION_MISMATCH} (parameter 'para')"
                )));
            }
        };
        self.add_measurement_impl(
            handle,
            name,
            unit,
            lower_limit,
            upper_limit,
            dt,
            bo,
            description,
            bit_offset,
            spec,
        )
    }

    /// string> textTable, ..)` → ConversionType.TextTable).
    #[allow(clippy::too_many_arguments)]
    pub fn add_measurement_text_table(
        &mut self,
        handle: i32,
        name: &str,
        unit: &str,
        lower_limit: f64,
        upper_limit: f64,
        dt: DataType,
        bo: ByteOrder,
        text_table: &[(f64, String)],
        description: Option<&str>,
        bit_offset: u32,
    ) -> Result<()> {
        self.add_measurement_impl(
            handle,
            name,
            unit,
            lower_limit,
            upper_limit,
            dt,
            bo,
            description,
            bit_offset,
            CcSpec::TextTable(text_table.to_vec()),
        )
    }

    /// sRange> rangeTable, string defaultText, ..)` → ConversionType.TabRange).
    #[allow(clippy::too_many_arguments)]
    pub fn add_measurement_range_table(
        &mut self,
        handle: i32,
        name: &str,
        unit: &str,
        lower_limit: f64,
        upper_limit: f64,
        dt: DataType,
        bo: ByteOrder,
        range_table: &[(f64, SRange)],
        default_text: &str,
        description: Option<&str>,
        bit_offset: u32,
    ) -> Result<()> {
        self.add_measurement_impl(
            handle,
            name,
            unit,
            lower_limit,
            upper_limit,
            dt,
            bo,
            description,
            bit_offset,
            CcSpec::TabRange(range_table.to_vec(), default_text.to_string()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_measurement_impl(
        &mut self,
        handle: i32,
        name: &str,
        unit: &str,
        lower_limit: f64,
        upper_limit: f64,
        dt: DataType,
        bo: ByteOrder,
        description: Option<&str>,
        bit_offset: u32,
        spec: CcSpec,
    ) -> Result<()> {
        if self.saved {
            return Ok(());
        }
        if !self.groups.contains_key(&handle) {
            return Err(Error::Write(STR_HANDLE_NOT_ALLOCATED.to_string()));
        }
        let Some(dg_idx) = self.groups.get(&handle).map(|g| g.dg) else {
            return Err(Error::Write(STR_HANDLE_NOT_ALLOCATED.to_string()));
        };
        let dg = &mut self.file.hd_block.dg_blocks[dg_idx];
        if !dg.data.is_empty() {
            return Err(Error::Write(
                STR_HANDLE_RUNNING.replace("{0}", &handle.to_string()),
            ));
        }
        let cg = &mut dg.cg_blocks[0];
        let record_size = u64::from(cg.record_size);
        if record_size > 65535 {
            return Err(Error::Write(
                STR_RECORD_SIZE_EXCEEDED
                    .replace("{0}", "V3")
                    .replace("{1}", "65534"),
            ));
        }
        let bits = data_type_bits(dt)
            .ok_or_else(|| Error::Write(format!("data type {dt:?} not supported")))?;
        let signal = signal_type_of(bo, dt)?;
        let cc = Self::create_cc_block(unit, lower_limit, upper_limit, spec);
        let bit_pos = if bit_offset == 0 {
            (record_size * 8) as u32
        } else {
            bit_offset + 64
        };
        let cn = v3::CnBlock::new(
            signal,
            ChannelType::Data,
            name,
            description.unwrap_or_default(),
            bit_pos,
            0,
            bits,
            Some(cc),
            "",
            "",
            "",
        );
        cg.cn_blocks.push(cn);
        let byte_len = u64::from(bits / 8).max(1);
        let new_size = if bit_offset == 0 {
            (record_size + byte_len).max(record_size)
        } else {
            (u64::from(bit_offset) + byte_len + 8).max(record_size)
        };
        cg.record_size = new_size as u32;
        if new_size > 65535 {
            return Err(Error::Write(
                STR_RECORD_SIZE_EXCEEDED
                    .replace("{0}", "V3")
                    .replace("{1}", "65534"),
            ));
        }
        Ok(())
    }

    fn create_cc_block(unit: &str, min: f64, max: f64, spec: CcSpec) -> v3::CcBlock {
        match spec {
            CcSpec::None => v3::CcBlock::new(ConversionType::None, unit, min, max),
            CcSpec::Params(params) => {
                let conv = if params.len() == 2 {
                    ConversionType::ParametricLinear
                } else {
                    ConversionType::Rational
                };
                let mut cc = v3::CcBlock::new(conv, unit, min, max);
                cc.params = params;
                cc.recalc_coeffs();
                if conv == ConversionType::Rational && cc.params != v3::RATIONAL_IDENTITY {
                    let orig = cc.params.clone();
                    cc.params[1] = orig[5];
                    cc.params[5] = orig[1];
                    cc.params[2] *= -1.0;
                    cc.params[4] *= -1.0;
                    cc.recalc_coeffs();
                }
                cc
            }
            CcSpec::TextTable(pairs) => {
                let mut cc = v3::CcBlock::new(ConversionType::TextTable, unit, min, max);
                for (k, v) in pairs {
                    cc.insert_number_text(k, v);
                }
                cc
            }
            CcSpec::TabRange(pairs, default_text) => {
                let mut cc = v3::CcBlock::new(ConversionType::TabRange, unit, min, max);
                cc.default_text = default_text;
                for (k, v) in pairs {
                    cc.insert_number_range(k, v);
                }
                cc
            }
        }
    }

    /// string name, string description, SignalType signalType)`).
    pub fn add_raw_data(
        &mut self,
        cg_comment: Option<&str>,
        record_size: u32,
        name: &str,
        description: Option<&str>,
        signal_type: SignalType,
    ) -> Result<i32> {
        if self.saved {
            return Ok(-1);
        }
        let handle = self.create_channel_group(cg_comment);
        let Some(g) = self.groups.get(&handle) else {
            return Err(Error::Write(STR_HANDLE_NOT_ALLOCATED.to_string()));
        };
        let cg = &mut self.file.hd_block.dg_blocks[g.dg].cg_blocks[0];
        let bit_pos = cg.record_size * 8;
        let bits = record_size * 8;
        let cn = v3::CnBlock::new(
            signal_type,
            ChannelType::Data,
            name,
            description.unwrap_or_default(),
            bit_pos,
            0,
            bits,
            None,
            "",
            "",
            "",
        );
        cg.cn_blocks.push(cn);
        cg.record_size += record_size;
        if u64::from(cg.record_size) > 65535 {
            return Err(Error::Write(
                STR_RECORD_SIZE_EXCEEDED
                    .replace("{0}", "V3")
                    .replace("{1}", "65534"),
            ));
        }
        Ok(handle)
    }

    pub fn add_data_entry(&mut self, handle: i32, timestamp: f64, data: &[u8]) -> Result<bool> {
        self.add_data_entry_impl(handle, timestamp, data, None)
    }

    pub fn add_data_entry_read_back(
        &mut self,
        handle: i32,
        timestamp: f64,
        data: &[u8],
        formulas: Option<&FormulaDict>,
    ) -> Result<(bool, Vec<f64>)> {
        let mut values = Vec::new();
        let result =
            self.add_data_entry_impl(handle, timestamp, data, Some((&mut values, formulas)))?;
        Ok((result, values))
    }

    fn add_data_entry_impl(
        &mut self,
        handle: i32,
        timestamp: f64,
        data: &[u8],
        mut read_back: Option<(&mut Vec<f64>, Option<&FormulaDict>)>,
    ) -> Result<bool> {
        if self.saved {
            return Ok(false);
        }
        let key = Self::key_of(handle);
        if !self.groups.contains_key(&key) {
            return Err(Error::Write(
                STR_HANDLE_NOT_ALLOCATED.replace("{handle}", &handle.to_string()),
            ));
        }
        let (dg_idx, prev_rel) = {
            let Some(g) = self.groups.get(&key) else {
                return Err(Error::Write(STR_HANDLE_NOT_ALLOCATED.to_string()));
            };
            (g.dg, g.last_rel_ts)
        };
        let (result, data_len) = {
            let cg = &self.file.hd_block.dg_blocks[dg_idx].cg_blocks[0];
            if cg.record_count == u64::from(u32::MAX) || cg.record_count == u64::MAX {
                return Ok(false);
            }
            let data_len = (cg.record_size - 8) as usize;
            let result = data_len == data.len();
            if data_len > data.len() {
                return Ok(result);
            }
            (result, data_len)
        };
        let rel = self.rebase_timestamp(timestamp, prev_rel, false);
        let Some(g) = self.groups.get_mut(&key) else {
            return Err(Error::Write(STR_HANDLE_NOT_ALLOCATED.to_string()));
        };
        g.last_rel_ts = rel;
        let dg_idx = g.dg;
        let dg = &mut self.file.hd_block.dg_blocks[dg_idx];
        dg.data.extend_from_slice(&rel.to_le_bytes());
        dg.data.extend_from_slice(&data[..data_len]);
        if let Some((values, formulas)) = read_back.as_mut() {
            **values = Self::read_back_values(&dg.cg_blocks[0], &data[..data_len], *formulas);
        }
        let cg = &mut dg.cg_blocks[0];
        if let Some(cc) = cg.cn_blocks[0].cc_block.as_mut() {
            if cg.record_count == 0 {
                cc.min = rel;
            }
            cc.max = rel;
        }
        cg.record_count += 1;
        self.data_received = true;
        Ok(result)
    }

    fn read_back_values(cg: &v3::CgBlock, data: &[u8], formulas: Option<&FormulaDict>) -> Vec<f64> {
        let mut values = Vec::with_capacity(cg.cn_blocks.len());
        let mut extra_ofs: i64 = 0;
        for cn in &cg.cn_blocks {
            if cn.channel_type == ChannelType::Master {
                extra_ofs -= i64::from(cn.no_of_bits / 8);
                continue;
            }
            values.push(match cn.signal_type {
                SignalType::UIntLe
                | SignalType::UIntBe
                | SignalType::SIntLe
                | SignalType::SIntBe
                | SignalType::FloatLe
                | SignalType::FloatBe => cn.decode_read_back(data, extra_ofs, formulas),
                _ => f64::NAN,
            });
        }
        values
    }

    fn rebase_timestamp(&mut self, timestamp: f64, prev_rel: f64, is_annotation: bool) -> f64 {
        if self.base_ts.is_nan() {
            self.base_ts = timestamp;
        }
        let mut rel = timestamp - self.base_ts;
        if !is_annotation && !prev_rel.is_nan() && rel < prev_rel {
            self.base_ts = prev_rel;
            rel = timestamp - self.base_ts;
            self.add_annotation(rel, STR_INVALID_TS_DESC);
        }
        rel
    }

    pub fn add_annotation(&mut self, timestamp: f64, text: &str) {
        if self.saved {
            return;
        }
        if self.annotation_dg.is_none() {
            let mut dg = v3::DgBlock::default();
            let mut cg = v3::CgBlock::new(v3::ANNOTATION_CG_COMMENT);
            cg.cn_blocks.push(Self::master_channel());
            cg.cn_blocks.push(v3::CnBlock::new(
                SignalType::String,
                ChannelType::Data,
                v3::ANNOTATION_CHANNEL_NAME,
                "",
                64,
                0,
                0,
                Some(v3::CcBlock::new(
                    ConversionType::None,
                    "",
                    f64::NAN,
                    f64::NAN,
                )),
                "",
                "",
                "",
            ));
            dg.cg_blocks.push(cg);
            self.file.hd_block.dg_blocks.insert(0, dg);
            for g in self.groups.values_mut() {
                g.dg += 1;
            }
            self.annotation_dg = Some(0);
        }
        let Some(dg_idx) = self.annotation_dg else {
            return;
        };
        let rel = self.rebase_timestamp(timestamp, self.annotation_last_ts, true);
        self.annotation_last_ts = rel;
        self.annotations.add(Annotation::new(rel, text));
        let cg = &mut self.file.hd_block.dg_blocks[dg_idx].cg_blocks[0];
        cg.record_count += 1;
        if let Some(cc) = cg.cn_blocks[0].cc_block.as_mut() {
            if self.annotations.items.len() == 1 {
                cc.min = rel;
            }
            cc.max = rel;
        }
        let bits = (self.annotations.max_length * 8) as u32;
        cg.cn_blocks[1].no_of_bits = bits;
        cg.record_size = 8 + bits / 8;
    }

    /// "Method not supported for V3").
    pub fn add_source_information(&mut self, _handle: i32) -> Result<()> {
        match self.mdf_type {
            MdfType::V3 => Err(Error::Write("Method not supported for V3".into())),
            MdfType::V4 => Err(Error::Write(
                "MDF V4 writer is implemented by the parallel v4 task".into(),
            )),
        }
    }

    pub fn write(&mut self) -> Result<Vec<u8>> {
        if let Some(dg_idx) = self.annotation_dg {
            self.file.hd_block.dg_blocks[dg_idx].data = self.annotations.write_records();
        }
        self.file.write()
    }

    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<bool> {
        if self.saved || !self.data_received {
            return Ok(true);
        }
        self.saved = true;
        let bytes = self.write()?;
        std::fs::write(path, bytes)?;
        Ok(true)
    }

    pub fn close(&mut self) {
        self.saved = true;
        self.groups.clear();
    }
}

fn data_type_bits(dt: DataType) -> Option<u32> {
    Some(match dt {
        DataType::UByte | DataType::SByte => 8,
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => 16,
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => 32,
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => 64,
        DataType::Unsupported => return None,
    })
}

fn signal_type_of(bo: ByteOrder, dt: DataType) -> Result<SignalType> {
    let be = match bo {
        ByteOrder::MSB_FIRST => true,
        ByteOrder::MSB_LAST => false,
        ByteOrder::NotSet => return Err(Error::Write(format!("byte order {bo:?} not supported"))),
    };
    Ok(match dt {
        DataType::UByte | DataType::UWord | DataType::ULong | DataType::AUInt64 => {
            if be {
                SignalType::UIntBe
            } else {
                SignalType::UIntLe
            }
        }
        DataType::SByte | DataType::SWord | DataType::SLong | DataType::AInt64 => {
            if be {
                SignalType::SIntBe
            } else {
                SignalType::SIntLe
            }
        }
        DataType::Float16Ieee | DataType::Float32Ieee | DataType::Float64Ieee => {
            if be {
                SignalType::FloatBe
            } else {
                SignalType::FloatLe
            }
        }
        DataType::Unsupported => {
            return Err(Error::Write(format!("data type {dt:?} not supported")))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_recording() -> v3::HdRecordingInfo {
        v3::HdRecordingInfo {
            date: "30:07:2026".to_string(),
            time: "08:59:16".to_string(),
            timestamp_ns: 0x18C7_059E_D266_E044,
            utc_offset: 8,
            time_quality: TimeQualityType::LocalPc,
            timer_id: v3::TIMER_ID_LOCAL_PC.to_string(),
        }
    }

    fn build_golden_writer() -> MdfWriter {
        let mut w = MdfWriter::new_v3(
            "Max Mustermann",
            "autors",
            "DemoProject",
            "DemoSubject",
            "header comment",
            golden_recording(),
            936,
        );
        let h = w.create_channel_group(Some("cg comment"));
        w.add_measurement(
            h,
            "speed",
            "km/h",
            0.0,
            250.0,
            DataType::UWord,
            ByteOrder::MSB_LAST,
            Some("vehicle speed"),
            Some(&[2.0, 3.0]),
            0,
        )
        .unwrap();
        w.add_measurement(
            h,
            "temp",
            "degC",
            -40.0,
            150.0,
            DataType::Float32Ieee,
            ByteOrder::MSB_LAST,
            None,
            None,
            0,
        )
        .unwrap();
        for i in 0..3u16 {
            let mut rec = Vec::new();
            rec.extend_from_slice(&(100 + i).to_le_bytes());
            rec.extend_from_slice(&(20.5f32 + f32::from(i)).to_le_bytes());
            assert!(w.add_data_entry(h, 0.1 * f64::from(i), &rec).unwrap());
        }
        w.add_annotation(0.05, "first note");
        w.add_annotation(0.15, "second longer note");
        w
    }

    const GOLDEN_HEX: &str = "\
4d 44 46 20 20 20 20 20 33 2e 33 30 20 20 20 20
41 55 54 4f 52 53 20 20 00 00 00 00 4a 01 a8 03
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
48 44 d0 00 23 01 00 00 10 01 00 00 00 00 00 00
02 00 33 30 3a 30 37 3a 32 30 32 36 30 38 3a 35
39 3a 31 36 4d 61 78 20 4d 75 73 74 65 72 6d 61
6e 6e 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 61 75 74 6f 72 73 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 44 65 6d 6f 50 72 6f 6a 65 63 74 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 44 65 6d 6f 53 75 62 6a 65 63 74 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 44 e0 66 d2 9e 05 c7 18 08 00 00 00
4c 6f 63 61 6c 20 50 43 20 52 65 66 65 72 65 6e
63 65 20 54 69 6d 65 00 00 00 00 00 00 00 00 00
54 58 13 00 68 65 61 64 65 72 20 63 6f 6d 6d 65
6e 74 00 44 47 1c 00 c7 03 00 00 3f 01 00 00 00
00 00 00 91 03 00 00 01 00 00 00 00 00 00 00 43
47 1e 00 00 00 00 00 5d 01 00 00 81 03 00 00 00
00 02 00 1b 00 02 00 00 00 00 00 00 00 43 4e e4
00 6f 02 00 00 41 02 00 00 00 00 00 00 00 00 00
00 00 00 00 00 01 00 74 69 6d 65 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 74 69 6d 65 73 74 61 6d 70
20 63 68 61 6e 6e 65 6c 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 40 00 10 00 00 00 00
00 00 00 00 00 f8 ff 00 00 00 00 00 00 f8 ff 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 43 43 2e 00 01 00 9a 99 99 99 99 99 a9 3f 33
33 33 33 33 33 c3 3f 73 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 ff ff 00 00 43
4e e4 00 00 00 00 00 53 03 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 41 6e 6e 6f 74 61 74
69 6f 6e 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 98 00 07 00 00
00 00 00 00 00 00 00 f8 ff 00 00 00 00 00 00 f8
ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 08 00 43 43 2e 00 00 00 00 00 00 00 00 00 f8
ff 00 00 00 00 00 00 f8 ff 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 ff ff 00
00 54 58 10 00 41 6e 6e 6f 74 61 74 69 6f 6e 73
00 9a 99 99 99 99 99 a9 3f 66 69 72 73 74 20 6e
6f 74 65 00 00 00 00 00 00 00 00 00 33 33 33 33
33 33 c3 3f 73 65 63 6f 6e 64 20 6c 6f 6e 67 65
72 20 6e 6f 74 65 00 44 47 1c 00 00 00 00 00 e3
03 00 00 00 00 00 00 56 07 00 00 01 00 00 00 00
00 00 00 43 47 1e 00 00 00 00 00 01 04 00 00 47
07 00 00 00 00 03 00 0e 00 03 00 00 00 00 00 00
00 43 4e e4 00 13 05 00 00 e5 04 00 00 00 00 00
00 00 00 00 00 00 00 00 00 01 00 74 69 6d 65 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 74 69 6d 65 73
74 61 6d 70 20 63 68 61 6e 6e 65 6c 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 40 00 10
00 00 00 00 00 00 00 00 00 f8 ff 00 00 00 00 00
00 f8 ff 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 43 43 2e 00 01 00 00 00 00 00 00
00 00 00 9a 99 99 99 99 99 c9 3f 73 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ff
ff 00 00 43 4e e4 00 35 06 00 00 f7 05 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 73 70 65
65 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 76 65 68
69 63 6c 65 20 73 70 65 65 64 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 10
00 0d 00 00 00 00 00 00 00 00 00 f8 ff 00 00 00
00 00 00 f8 ff 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 08 00 43 43 3e 00 01 00 00 00 00
00 00 00 00 00 00 00 00 00 00 40 6f 40 6b 6d 2f
68 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 02 00 00 00 00 00 00 00 00 40 00 00 00
00 00 00 08 40 43 4e e4 00 00 00 00 00 19 07 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 74
65 6d 70 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 20 00 0f 00 00 00 00 00 00 00 00 00 f8 ff 00
00 00 00 00 00 f8 ff 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 0a 00 43 43 2e 00 01 00 00
00 00 00 00 00 44 c0 00 00 00 00 00 c0 62 40 64
65 67 43 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 ff ff 00 00 54 58 0f 00 63 67 20 63 6f
6d 6d 65 6e 74 00 00 00 00 00 00 00 00 00 64 00
00 00 a4 41 9a 99 99 99 99 99 b9 3f 65 00 00 00
ac 41 9a 99 99 99 99 99 c9 3f 66 00 00 00 b4 41";

    fn golden_bytes() -> Vec<u8> {
        GOLDEN_HEX
            .split_whitespace()
            .map(|s| u8::from_str_radix(s, 16).unwrap())
            .collect()
    }

    #[test]
    fn writer_output_matches_golden_bytes() {
        let mut w = build_golden_writer();
        let bytes = w.write().unwrap();
        let golden = golden_bytes();
        assert_eq!(golden.len(), 1920, "golden size");
        assert_eq!(bytes.len(), golden.len(), "file length");
        assert_eq!(
            bytes, golden,
            "byte-identical with the checked-in golden data"
        );
    }

    #[test]
    fn writer_file_id_constants() {
        assert_eq!(v3::FILE_ID_MDF, "MDF     ");
        assert_eq!(v3::FORMAT_ID_V330, "3.30    ");
        assert_eq!(v3::PROGRAM_ID, "AUTORS  ");
        assert_eq!(v3::FILE_ID_UNFINALIZED, "UnFinMF ");
    }

    #[test]
    fn annotation_list_max_length_and_sort() {
        let mut list = AnnotationList::new();
        list.add(Annotation::new(0.15, "second longer note"));
        list.add(Annotation::new(0.05, "first note"));
        assert_eq!(list.max_length, 19);
        let rec = list.write_records();
        assert_eq!(rec.len(), 2 * (8 + 19));
        assert_eq!(
            f64::from_le_bytes(rec[0..8].try_into().unwrap()),
            0.05,
            "sorted ascending"
        );
        assert_eq!(&rec[8..8 + 10], b"first note");
        assert_eq!(f64::from_le_bytes(rec[27..35].try_into().unwrap()), 0.15);
        assert_eq!(&rec[35..35 + 18], b"second longer note");
    }

    #[test]
    fn handles_are_negative_incrementing() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h1 = w.create_channel_group(None);
        let h2 = w.create_channel_group(Some("x"));
        assert_eq!(h1, i32::MIN + 1);
        assert_eq!(h2, i32::MIN + 2);
        assert!(w.channel_group(h1).is_some());
        assert!(w.channel_group(h2).is_some());
        assert!(w.channel_group(0).is_none());
    }

    #[test]
    fn new_rejects_v3_compress_and_v4() {
        let r = golden_recording();
        assert!(MdfWriter::new(MdfType::V3, true, "", "", "", "", "", r.clone(), 1252).is_err());
        assert!(MdfWriter::new(MdfType::V4, false, "", "", "", "", "", r, 1252).is_err());
    }

    #[test]
    fn add_measurement_param_count_check() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h = w.create_channel_group(None);
        let err = w
            .add_measurement(
                h,
                "x",
                "V",
                0.0,
                1.0,
                DataType::UWord,
                ByteOrder::MSB_LAST,
                None,
                Some(&[1.0, 2.0, 3.0]),
                0,
            )
            .unwrap_err();
        assert!(err.to_string().contains("2 or 6"));
        assert!(w
            .add_measurement(
                12345,
                "x",
                "V",
                0.0,
                1.0,
                DataType::UWord,
                ByteOrder::MSB_LAST,
                None,
                None,
                0
            )
            .is_err());
    }

    #[test]
    fn data_entry_short_data_rejected_long_truncated() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h = w.create_channel_group(None);
        w.add_measurement(
            h,
            "x",
            "V",
            0.0,
            1.0,
            DataType::UWord,
            ByteOrder::MSB_LAST,
            None,
            None,
            0,
        )
        .unwrap();
        assert!(
            !w.add_data_entry(h, 0.0, &[1]).unwrap(),
            "too short returns false without writing"
        );
        assert!(
            !w.add_data_entry(h, 0.1, &[1, 2, 3]).unwrap(),
            "overlong data returns false and is truncated"
        );
        let cg = w.channel_group(h).unwrap();
        assert_eq!(cg.record_count, 1);
        let bytes = w.write().unwrap();
        let back = MdfReader::parse(&bytes).unwrap();
        let f = back.as_v3().unwrap();
        assert_eq!(f.hd_block.dg_blocks[0].cg_blocks[0].record_count, 1);
    }

    #[test]
    fn timestamp_rebase_on_jump() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h = w.create_channel_group(None);
        w.add_measurement(
            h,
            "x",
            "V",
            0.0,
            1.0,
            DataType::UWord,
            ByteOrder::MSB_LAST,
            None,
            None,
            0,
        )
        .unwrap();
        assert!(w.add_data_entry(h, 10.0, &[1, 2]).unwrap());
        assert!(
            w.add_data_entry(h, 9.0, &[3, 4]).unwrap(),
            "a backward time jump resets the baseline and adds an annotation"
        );
        assert_eq!(w.annotations.items.len(), 1);
        assert!(w.annotations.items[0]
            .text
            .contains("timestamps will be adjusted"));
        let cg = w.channel_group(h).unwrap();
        let cc = cg.cn_blocks[0].cc_block.as_ref().unwrap();
        assert_eq!(cc.min, 0.0);
        assert_eq!(cc.max, 9.0);
    }

    #[test]
    fn read_back_values_with_conversion() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h = w.create_channel_group(None);
        w.add_measurement(
            h,
            "speed",
            "km/h",
            0.0,
            250.0,
            DataType::UWord,
            ByteOrder::MSB_LAST,
            None,
            Some(&[2.0, 3.0]),
            0,
        )
        .unwrap();
        w.add_measurement(
            h,
            "temp",
            "degC",
            -40.0,
            150.0,
            DataType::Float32Ieee,
            ByteOrder::MSB_LAST,
            None,
            None,
            0,
        )
        .unwrap();
        let mut rec = Vec::new();
        rec.extend_from_slice(&302u16.to_le_bytes());
        rec.extend_from_slice(&20.5f32.to_le_bytes());
        let (ok, values) = w.add_data_entry_read_back(h, 0.0, &rec, None).unwrap();
        assert!(ok);
        assert_eq!(
            values.len(),
            2,
            "the time master channel does not produce a returned value"
        );
        assert!(
            (values[0] - 100.0).abs() < 1e-9,
            "speed read-back = {}",
            values[0]
        );
        assert_eq!(
            values[1],
            f64::from(20.5f32),
            "a channel without conversion is decoded directly"
        );
        let (ok, values) = w.add_data_entry_read_back(h, 0.1, &[1], None).unwrap();
        assert!(!ok);
        assert!(values.is_empty());
    }

    #[test]
    fn read_back_values_non_numeric_nan() {
        let mut w = MdfWriter::new_v3("", "", "", "", "", golden_recording(), 1252);
        let h = w
            .add_raw_data(None, 4, "raw", None, SignalType::ByteArray)
            .unwrap();
        let (ok, values) = w
            .add_data_entry_read_back(h, 0.0, &[1, 2, 3, 4], None)
            .unwrap();
        assert!(ok);
        assert_eq!(values.len(), 1);
        assert!(values[0].is_nan());
    }
}
