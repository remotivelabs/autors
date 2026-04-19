//! MDF v3 (ASAM MDF 3.30) block model with byte-level reading and writing.
//! Covers the block types IDBLOCK, HDBLOCK, DGBLOCK, CGBLOCK, CNBLOCK,
//! CCBLOCK, CDBLOCK, CEBLOCK, SRBLOCK, TRBLOCK and TXBLOCK, plus the v3
//! read-side implementation (record decoding, value access, annotations,
//! CSV export).
//! Binary layout: little-endian throughout; a block header is a 2-character
//! ID plus a u16 block length; inter-block links are u32 absolute file
//! offsets (0 means none). On the write side the block header is reserved
//! first and the links are back-patched after the child blocks are written.
//! Each block carries its Id/BlockSize in its own read/write code;
//! Parent/Tag/Name navigation properties are not modeled.
//! Known quirks and deliberate behavior (see also the `crate::base` module
//! documentation):
//! - File strings use a Latin-1 mapping (see the base.rs module docs).
//! - The read side keeps everything in memory (`Vec<u8>`); a DG block's raw
//!   record area is stored in [`DgBlock::data`].
//! - The CCBLOCK TextFormula write declares a block length of 46+len+1 yet
//!   always writes a 256-byte formula buffer, and the TextRange branch
//!   declares 46+20×(n+1) yet writes an inline TXBLOCK, so the declared
//!   block length does not match the actual byte count. This is intentional:
//!   the read side does not rely on the declared length and links are
//!   back-patched with the actual byte count, so the file remains valid.
//! - The CCBLOCK read side reads TextRange texts as absolute links
//!   (asymmetric with the inline write layout, so reading back loses the
//!   texts). This read-side behavior is deliberate.
//! - The CNBLOCK read position skips the 1-byte record ID only for
//!   Before8Bit/BeforeAndAfter8Bit; for Before16/32/64Bit it does not skip
//!   (the record ID bytes are read as data). This quirk is preserved
//!   deliberately.
//! - The DGBLOCK read side maps record-ID type codes 0/1/2 to the enum;
//!   other values fall back to None by design.
//! - Record decoding takes `NoOfBits/8` (floor) bytes for channels whose
//!   bit count is not a multiple of 8 — intentional.
//! - `RationalCoeffs` is replicated locally (autors-mdf does not depend on
//!   autors-values); only the toPhysical/toRaw paths used by CC conversion
//!   are implemented.
//! - TextFormula conversion requires a formula dictionary: the read side
//!   `decode`/`to_physical` has none and returns raw values without
//!   evaluating formulas (noted at the call sites); the write-side
//!   read-back ([`CnBlock::decode_read_back`]) accepts a `FormulaDict` to
//!   evaluate formulas by channel name (see that function's notes).
//! - CSV export always uses \r\n line endings.

use std::collections::HashMap;
use std::path::Path;

use autors_a2l::model::base::ByteOrder;
use autors_formula::formula::FormulaDict;

use crate::base::{
    decode_text, encode_text, fixed_bytes, parse_err, Annotation, ChannelType, ConversionType,
    CustomFlagsType, DataLimits, DataPoint, DependencyType, RawDataRecord, RecordIdType, SRange,
    SignalType, TimeQualityType, UnfinalizedFlagsType, ValueObjectFormat, STR_RESORT_MSG,
};
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const FILE_ID_MDF: &str = "MDF     ";
/// File identifier used while an MDF file remains unfinalized.
pub const FILE_ID_UNFINALIZED: &str = "UnFinMF ";
pub const FORMAT_ID_V330: &str = "3.30    ";
/// Program identifier written by this crate.
pub const PROGRAM_ID: &str = "AUTORS  ";
pub const MASTER_CHANNEL_NAME: &str = "time";
pub const MASTER_CHANNEL_DESC: &str = "timestamp channel";
pub const MASTER_CHANNEL_UNIT: &str = "s";
pub const ANNOTATION_CG_COMMENT: &str = "Annotations";
pub const ANNOTATION_CHANNEL_NAME: &str = "Annotation";
pub const TIMER_ID_LOCAL_PC: &str = "Local PC Reference Time";

pub const ID_BLOCK_SIZE: usize = 64;
pub const HD_BLOCK_SIZE: usize = 208;
pub const DG_BLOCK_SIZE: usize = 28;
pub const CG_BLOCK_SIZE: usize = 30;
pub const CN_BLOCK_SIZE: usize = 228;
pub const CC_BLOCK_BASE_SIZE: usize = 46;
pub const SR_BLOCK_SIZE: usize = 24;

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len().saturating_sub(self.pos) < n {
            return parse_err(
                self.pos as u64,
                format!(
                    "unexpected end of data: need {n} bytes at {:#x}, file len {:#x}",
                    self.pos,
                    self.data.len()
                ),
            );
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let mut a = [0u8; 2];
        a.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(a))
    }

    fn i16(&mut self) -> Result<i16> {
        let mut a = [0u8; 2];
        a.copy_from_slice(self.take(2)?);
        Ok(i16::from_le_bytes(a))
    }

    fn u32(&mut self) -> Result<u32> {
        let mut a = [0u8; 4];
        a.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(a))
    }

    fn u64(&mut self) -> Result<u64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }

    fn f64(&mut self) -> Result<f64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(f64::from_le_bytes(a))
    }

    fn text(&mut self, n: usize) -> Result<String> {
        Ok(decode_text(self.take(n)?))
    }
}

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_f64(out: &mut Vec<u8>, v: f64) {
    let v = if v.is_nan() {
        f64::from_bits(0xFFF8_0000_0000_0000)
    } else {
        v
    };
    out.extend_from_slice(&v.to_le_bytes());
}

fn patch(out: &mut [u8], pos: usize, hdr: &[u8]) {
    out[pos..pos + hdr.len()].copy_from_slice(hdr);
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn write_tx_block(out: &mut Vec<u8>, text: &str) -> u32 {
    let pos = out.len() as u32;
    let bytes = encode_text(text);
    out.extend_from_slice(b"TX");
    put_u16(out, (4 + bytes.len() + 1) as u16);
    out.extend_from_slice(&bytes);
    out.push(0);
    pos
}

fn read_tx_block(data: &[u8], link: u32) -> String {
    if link == 0 {
        return String::new();
    }
    let mut r = Reader::new(data, link as usize);
    let Ok(_id) = r.take(2) else {
        return String::new();
    };
    let Ok(size) = r.u16() else {
        return String::new();
    };
    let n = size as i64 - 4;
    if n <= 0 {
        return String::new();
    }
    let avail = data.len().saturating_sub(r.pos);
    let n = (n as usize).min(avail);
    decode_text(&data[r.pos..r.pos + n])
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

macro_rules! plain_enum {
    ($(#[$meta:meta])* $name:ident($ty:ty) { $($(#[$vmeta:meta])* $vname:ident = $vval:expr),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        #[repr($ty)]
        pub enum $name {
            $($(#[$vmeta])* $vname = $vval,)*
        }

        impl $name {
            pub fn from_raw(v: $ty) -> Option<Self> {
                match v {
                    $($vval => Some(Self::$vname),)*
                    _ => None,
                }
            }

            pub fn raw(self) -> $ty {
                self as $ty
            }
        }
    };
}

plain_enum! {
    FpFormatType(u16) {
        #[default] Ieee754 = 0,
        GFloat = 1,
        DFloat = 2,
    }
}

plain_enum! {
    ExtensionType(u16) {
        #[default] Dim = 2,
        VectorCan = 19,
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct IdBlock {
    pub file_id: String,
    pub format_id: String,
    pub program_id: String,
    pub byte_order: ByteOrder,
    pub fp_format: FpFormatType,
    pub version: u16,
    pub code_page: u16,
    pub unfinalized_flags: UnfinalizedFlagsType,
    pub custom_flags: CustomFlagsType,
}

impl Default for IdBlock {
    fn default() -> Self {
        Self::new_v3("", 0)
    }
}

impl IdBlock {
    /// FPFormatType fpFormat = IEEE_754)`.
    pub fn new_v3(program_id: &str, code_page: u16) -> Self {
        IdBlock {
            file_id: FILE_ID_MDF.to_string(),
            format_id: FORMAT_ID_V330.to_string(),
            program_id: program_id.to_string(),
            byte_order: ByteOrder::MSB_LAST,
            fp_format: FpFormatType::Ieee754,
            version: 330,
            code_page,
            unfinalized_flags: UnfinalizedFlagsType::NONE,
            custom_flags: CustomFlagsType::NONE,
        }
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < ID_BLOCK_SIZE {
            return parse_err(
                0,
                format!("file too small for MDF id block: {} bytes", data.len()),
            );
        }
        let mut r = Reader::new(data, 0);
        let file_id = r.text(8)?;
        let format_id = r.text(8)?;
        let program_id = r.text(8)?;
        let byte_order = if r.u16()? > 0 {
            ByteOrder::MSB_FIRST
        } else {
            ByteOrder::MSB_LAST
        };
        let fp_raw = r.u16()?;
        let fp_format = FpFormatType::from_raw(fp_raw).ok_or_else(|| Error::Parse {
            offset: 26,
            message: format!("unknown floating point format {fp_raw}"),
        })?;
        let version = r.u16()?;
        let code_page = r.u16()?;
        r.skip(7 * 4)?;
        let unfinalized_flags = UnfinalizedFlagsType::from_bits(r.u16()?);
        let custom_flags = CustomFlagsType::from_bits(r.u16()?);
        Ok(IdBlock {
            file_id,
            format_id,
            program_id,
            byte_order,
            fp_format,
            version,
            code_page,
            unfinalized_flags,
            custom_flags,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&fixed_bytes(&self.file_id, 8, false));
        out.extend_from_slice(&fixed_bytes(&self.format_id, 8, false));
        out.extend_from_slice(&fixed_bytes(&self.program_id, 8, false));
        put_u16(out, 0);
        put_u16(out, self.fp_format.raw());
        put_u16(out, self.version);
        put_u16(out, self.code_page);
        out.extend_from_slice(&[0u8; 32]);
    }

    pub fn block_size(&self) -> u32 {
        ID_BLOCK_SIZE as u32
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub const RATIONAL_IDENTITY: [f64; 6] = [0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

#[derive(Debug, Clone, PartialEq)]
pub struct RationalCoeffs {
    pub coeffs: [f64; 6],
    identity: bool,
    full: bool,
}

impl RationalCoeffs {
    pub fn from_params(params: &[f64]) -> Option<Self> {
        match params.len() {
            2 => Some(Self::factor_offset(params[1], params[0])),
            6 => Some(Self::from_array(params)),
            _ => None,
        }
    }

    fn factor_offset(factor: f64, offset: f64) -> Self {
        let mut coeffs = RATIONAL_IDENTITY;
        coeffs[1] = 1.0 / factor;
        coeffs[2] = -offset / factor;
        let identity = coeffs == RATIONAL_IDENTITY;
        RationalCoeffs {
            coeffs,
            identity,
            full: false,
        }
    }

    fn from_array(c: &[f64]) -> Self {
        let mut coeffs = [0.0; 6];
        coeffs.copy_from_slice(&c[..6]);
        let identity = coeffs == RATIONAL_IDENTITY;
        let full = coeffs[3] != 0.0 || coeffs[4] != 0.0 || coeffs[5] != 1.0;
        RationalCoeffs {
            coeffs,
            identity,
            full,
        }
    }

    pub fn factor(&self) -> f64 {
        1.0 / self.coeffs[1]
    }

    pub fn offset(&self) -> f64 {
        -self.factor() * self.coeffs[2]
    }

    pub fn to_physical(&self, raw: f64) -> f64 {
        if self.identity || raw.is_nan() {
            return raw;
        }
        let [a, b, c, d, e, f] = self.coeffs;
        let (num, num2, num3) = if self.full {
            (d * raw - a, e * raw - b, f * raw - c)
        } else {
            (-a, -b, raw - c)
        };
        if num == 0.0 {
            return -num3 / num2;
        }
        let sq = (num2 * num2 - 4.0 * num * num3).sqrt();
        let denom = 2.0 * num;
        let x1 = (-num2 + sq) / denom;
        let x2 = (-num2 - sq) / denom;
        if x1 != x2 {
            return raw;
        }
        x1
    }

    pub fn to_raw(&self, x: f64) -> f64 {
        if self.identity || x.is_nan() {
            return x;
        }
        let [a, b, c, d, e, f] = self.coeffs;
        let mut num = if a != 0.0 {
            a * x * x + b * x + c
        } else {
            b * x + c
        };
        if self.full {
            num /= if d != 0.0 {
                d * x * x + e * x + f
            } else {
                e * x + f
            };
        }
        num
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum CcKey {
    Number(f64),
    Text(String),
}

impl PartialOrd for CcKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp_total(other))
    }
}

impl CcKey {
    fn cmp_total(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (CcKey::Number(a), CcKey::Number(b)) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            (CcKey::Text(a), CcKey::Text(b)) => a.cmp(b),
            (CcKey::Number(_), CcKey::Text(_)) => std::cmp::Ordering::Less,
            (CcKey::Text(_), CcKey::Number(_)) => std::cmp::Ordering::Greater,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CcValue {
    Number(f64),
    Text(String),
    Range(SRange),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CcDate {
    pub ms: u16,
    pub minute: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CcTime {
    pub ms: u32,
    pub days: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CcBlock {
    pub conversion_type: ConversionType,
    pub unit: String,
    pub min: f64,
    pub max: f64,
    pub tab_size: u16,
    pub params: Vec<f64>,
    pub pairs: Vec<(CcKey, CcValue)>,
    pub formula: String,
    pub default_text: String,
    pub date: Option<CcDate>,
    pub time: Option<CcTime>,
    coeffs: Option<RationalCoeffs>,
}

impl Default for CcBlock {
    fn default() -> Self {
        Self::new(ConversionType::None, "", f64::NAN, f64::NAN)
    }
}

impl CcBlock {
    pub fn new(conversion_type: ConversionType, unit: &str, min: f64, max: f64) -> Self {
        CcBlock {
            conversion_type,
            unit: unit.to_string(),
            min,
            max,
            tab_size: 0,
            params: Vec::new(),
            pairs: Vec::new(),
            formula: String::new(),
            default_text: String::new(),
            date: None,
            time: None,
            coeffs: None,
        }
    }

    pub fn recalc_coeffs(&mut self) {
        self.coeffs = RationalCoeffs::from_params(&self.params);
    }

    pub fn insert_number_number(&mut self, key: f64, value: f64) {
        self.insert(CcKey::Number(key), CcValue::Number(value));
    }

    pub fn insert_number_text(&mut self, key: f64, value: String) {
        self.insert(CcKey::Number(key), CcValue::Text(value));
    }

    pub fn insert_text_range(&mut self, key: String, value: SRange) {
        self.insert(CcKey::Text(key), CcValue::Range(value));
    }

    pub fn insert_number_range(&mut self, key: f64, value: SRange) {
        self.insert(CcKey::Number(key), CcValue::Range(value));
    }

    fn insert(&mut self, key: CcKey, value: CcValue) {
        if let Some(existing) = self.pairs.iter_mut().find(|(k, _)| *k == key) {
            existing.1 = value;
            return;
        }
        self.pairs.push((key, value));
        self.pairs.sort_by(|a, b| a.0.cmp_total(&b.0));
    }

    fn parse(data: &[u8], pos: usize) -> Result<Self> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let mut min = f64::NAN;
        let mut max = f64::NAN;
        if r.u16()? > 0 {
            min = r.f64()?;
            max = r.f64()?;
        } else {
            r.skip(16)?;
        }
        let unit = r.text(20)?;
        let conv_raw = r.u16()?;
        let conversion_type = ConversionType::from_raw(conv_raw).ok_or_else(|| Error::Parse {
            offset: pos as u64,
            message: format!("unknown conversion type {conv_raw}"),
        })?;
        let tab_size = r.u16()?;
        let mut cc = CcBlock::new(conversion_type, &unit, min, max);
        cc.tab_size = tab_size;
        let mut param_count = 0usize;
        match conversion_type {
            ConversionType::ParametricLinear
            | ConversionType::Polynomial
            | ConversionType::Exponential
            | ConversionType::Logarithmic
            | ConversionType::Rational => {
                param_count = usize::from(tab_size);
            }
            ConversionType::TextFormula => {
                cc.formula = r.text(usize::from(tab_size))?;
            }
            ConversionType::TabInt | ConversionType::Tab => {
                for _ in 0..tab_size {
                    let k = r.f64()?;
                    let v = r.f64()?;
                    cc.insert_number_number(k, v);
                }
            }
            ConversionType::TextTable => {
                for _ in 0..tab_size {
                    let k = r.f64()?;
                    let v = r.text(32)?;
                    cc.insert_number_text(k, v);
                }
            }
            ConversionType::TextRange => {
                r.f64()?;
                r.f64()?;
                let link = r.u32()?;
                cc.default_text = read_tx_block(data, link);
                for _ in 0..tab_size.saturating_sub(1) {
                    let range = SRange::new(r.f64()?, r.f64()?);
                    let text = read_tx_block(data, r.u32()?);
                    cc.insert_text_range(text, range);
                }
            }
            ConversionType::Date => {
                cc.date = Some(CcDate {
                    ms: r.u16()?,
                    minute: r.u8()?,
                    hour: r.u8()?,
                    day: r.u8()?,
                    month: r.u8()?,
                    year: r.u8()?,
                });
            }
            ConversionType::Time => {
                cc.time = Some(CcTime {
                    ms: r.u32()?,
                    days: r.u8()?,
                });
            }
            ConversionType::None => {}
            other => {
                return parse_err(
                    pos as u64,
                    format!("conversion type {other:?} not supported"),
                );
            }
        }
        if param_count > 0 {
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                params.push(r.f64()?);
            }
            cc.params = params;
            cc.recalc_coeffs();
        }
        Ok(cc)
    }

    fn write_to(&self, out: &mut Vec<u8>) -> Result<()> {
        let formula_bytes = encode_text(&self.formula);
        let (tab_size, block_size) = match self.conversion_type {
            ConversionType::ParametricLinear
            | ConversionType::Polynomial
            | ConversionType::Exponential
            | ConversionType::Logarithmic
            | ConversionType::Rational => (
                self.params.len(),
                CC_BLOCK_BASE_SIZE + self.params.len() * 8,
            ),
            ConversionType::TextFormula => (
                formula_bytes.len(),
                CC_BLOCK_BASE_SIZE + formula_bytes.len() + 1,
            ),
            ConversionType::TabInt | ConversionType::Tab => {
                (self.pairs.len(), CC_BLOCK_BASE_SIZE + self.pairs.len() * 16)
            }
            ConversionType::TextTable => {
                (self.pairs.len(), CC_BLOCK_BASE_SIZE + self.pairs.len() * 40)
            }
            ConversionType::TextRange => (
                self.pairs.len() + 1,
                CC_BLOCK_BASE_SIZE + (self.pairs.len() + 1) * 20,
            ),
            ConversionType::Date => (6, CC_BLOCK_BASE_SIZE + 7),
            ConversionType::Time => (2, CC_BLOCK_BASE_SIZE + 5),
            ConversionType::None => (0, CC_BLOCK_BASE_SIZE),
            other => {
                return Err(Error::Write(format!(
                    "conversion type {other:?} not supported for writing"
                )));
            }
        };
        out.extend_from_slice(b"CC");
        put_u16(out, block_size as u16);
        put_u16(out, u16::from(!self.min.is_nan() && !self.max.is_nan()));
        put_f64(out, self.min);
        put_f64(out, self.max);
        out.extend_from_slice(&fixed_bytes(&self.unit, 20, true));
        put_u16(out, self.conversion_type.raw());
        put_u16(out, tab_size as u16);
        match self.conversion_type {
            ConversionType::ParametricLinear
            | ConversionType::Polynomial
            | ConversionType::Exponential
            | ConversionType::Logarithmic
            | ConversionType::Rational => {
                for &p in &self.params {
                    put_f64(out, p);
                }
            }
            ConversionType::TextFormula => {
                out.extend_from_slice(&fixed_bytes(&self.formula, 256, true));
            }
            ConversionType::TabInt | ConversionType::Tab => {
                for (k, v) in &self.pairs {
                    let (CcKey::Number(key), CcValue::Number(value)) = (k, v) else {
                        return Err(Error::Write("Tab/TabInt pairs must be numeric".into()));
                    };
                    put_f64(out, *key);
                    put_f64(out, *value);
                }
            }
            ConversionType::TextTable => {
                for (k, v) in &self.pairs {
                    let (CcKey::Number(key), CcValue::Text(text)) = (k, v) else {
                        return Err(Error::Write("TextTable pairs must be number→text".into()));
                    };
                    put_f64(out, *key);
                    out.extend_from_slice(&fixed_bytes(text, 32, true));
                }
            }
            ConversionType::TextRange => {
                put_f64(out, 0.0);
                put_f64(out, 0.0);
                write_tx_block(out, &self.default_text);
                for (k, v) in &self.pairs {
                    let (CcKey::Text(text), CcValue::Range(range)) = (k, v) else {
                        return Err(Error::Write("TextRange pairs must be text→range".into()));
                    };
                    put_f64(out, range.min);
                    put_f64(out, range.max);
                    write_tx_block(out, text);
                }
            }
            ConversionType::Date => {
                let d = self
                    .date
                    .ok_or_else(|| Error::Write("Date conversion without date".into()))?;
                put_u16(out, d.ms);
                put_u8(out, d.minute);
                put_u8(out, d.hour);
                put_u8(out, d.day);
                put_u8(out, d.month);
                put_u8(out, d.year);
            }
            ConversionType::Time => {
                let t = self
                    .time
                    .ok_or_else(|| Error::Write("Time conversion without time".into()))?;
                put_u32(out, t.ms);
                put_u8(out, t.days);
            }
            ConversionType::None => {}
            other => {
                return Err(Error::Write(format!(
                    "conversion type {other:?} not supported for writing"
                )))
            }
        }
        Ok(())
    }

    pub fn to_physical(&self, raw: f64, inverse: bool) -> f64 {
        match self.conversion_type {
            ConversionType::ParametricLinear => match &self.coeffs {
                Some(c) => {
                    if inverse {
                        c.to_raw(raw)
                    } else {
                        c.to_physical(raw)
                    }
                }
                None => raw,
            },
            ConversionType::Rational => match &self.coeffs {
                Some(c) => {
                    if inverse {
                        c.to_physical(raw)
                    } else {
                        c.to_raw(raw)
                    }
                }
                None => raw,
            },
            ConversionType::Tab => {
                for (k, v) in &self.pairs {
                    if let (CcKey::Number(key), CcValue::Number(value)) = (k, v) {
                        if *key == raw {
                            return *value;
                        }
                    }
                }
                raw
            }
            ConversionType::TabInt => {
                let mut prev: Option<(f64, f64)> = None;
                for (k, v) in &self.pairs {
                    let (CcKey::Number(key), CcValue::Number(value)) = (k, v) else {
                        continue;
                    };
                    match prev {
                        None => {
                            if raw < *key {
                                return *value;
                            }
                            if raw == *key {
                                return *value;
                            }
                            prev = Some((*key, *value));
                        }
                        Some((pk, pv)) => {
                            if raw <= *key {
                                return (raw - pk) / (*key - pk) * (*value - pv) + pv;
                            }
                            prev = Some((*key, *value));
                        }
                    }
                }
                prev.map_or(raw, |(_, v)| v)
            }
            ConversionType::Polynomial => {
                if self.params.len() < 6 {
                    return raw;
                }
                let p = &self.params;
                let x2 = raw - p[4] - p[5];
                let x3 = p[2] * x2 - p[0];
                (p[1] - p[3] * x2) / x3
            }
            ConversionType::Exponential => {
                if self.params.len() < 7 {
                    return raw;
                }
                let p = &self.params;
                if p[0] == 0.0 {
                    ((p[2] / (raw - p[6]) - p[5]) / p[3]).exp() / p[4]
                } else if p[3] == 0.0 {
                    (((raw - p[6]) * p[5] - p[2]) / p[0]).exp() / p[1]
                } else {
                    raw
                }
            }
            ConversionType::Logarithmic => {
                if self.params.len() < 7 {
                    return raw;
                }
                let p = &self.params;
                if p[0] == 0.0 {
                    ((p[2] / (raw - p[6]) - p[5]) / p[3]).exp() / p[4]
                } else if p[4] == 0.0 {
                    (((raw - p[6]) * p[5] - p[2]) / p[0]).exp() / p[1]
                } else {
                    raw
                }
            }
            _ => raw,
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CdBlock {
    pub dependency_type: u16,
    pub dependencies: Vec<DependencyType>,
}

impl CdBlock {
    fn parse(data: &[u8], pos: usize) -> Result<Self> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let dependency_type = r.u16()?;
        let count = r.u16()?;
        let mut dependencies = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            dependencies.push(DependencyType::new(
                i64::from(r.u32()?),
                i64::from(r.u32()?),
                i64::from(r.u32()?),
            ));
        }
        Ok(CdBlock {
            dependency_type,
            dependencies,
        })
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"CD");
        put_u16(out, (8 + self.dependencies.len() * 12) as u16);
        put_u16(out, self.dependency_type);
        put_u16(out, self.dependencies.len() as u16);
        for dep in &self.dependencies {
            put_u32(out, dep.link_dg as u32);
            put_u32(out, dep.link_cg as u32);
            put_u32(out, dep.link_cn as u32);
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DimType {
    pub module: u16,
    pub address: u32,
    pub description: String,
    pub ecu_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VectorCanType {
    pub id: u32,
    pub index: u32,
    pub message: String,
    pub sender: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CeBlock {
    pub extension_type: ExtensionType,
    pub dim: Option<DimType>,
    pub vector_can: Option<VectorCanType>,
}

impl CeBlock {
    pub fn new_dim(dim: DimType) -> Self {
        CeBlock {
            extension_type: ExtensionType::Dim,
            dim: Some(dim),
            vector_can: None,
        }
    }

    pub fn new_vector_can(vector_can: VectorCanType) -> Self {
        CeBlock {
            extension_type: ExtensionType::VectorCan,
            dim: None,
            vector_can: Some(vector_can),
        }
    }

    fn parse(data: &[u8], pos: usize) -> Result<Self> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let ext_raw = r.u16()?;
        let extension_type = ExtensionType::from_raw(ext_raw).ok_or_else(|| Error::Parse {
            offset: pos as u64,
            message: format!("unknown extension type {ext_raw}"),
        })?;
        let ce = match extension_type {
            ExtensionType::Dim => CeBlock::new_dim(DimType {
                module: r.u16()?,
                address: r.u32()?,
                description: r.text(80)?,
                ecu_id: r.text(32)?,
            }),
            ExtensionType::VectorCan => CeBlock::new_vector_can(VectorCanType {
                id: r.u32()?,
                index: r.u32()?,
                message: r.text(36)?,
                sender: r.text(36)?,
            }),
        };
        Ok(ce)
    }

    fn write_to(&self, out: &mut Vec<u8>) -> Result<()> {
        let payload = match self.extension_type {
            ExtensionType::Dim => 118,
            ExtensionType::VectorCan => 80,
        };
        out.extend_from_slice(b"CE");
        put_u16(out, (6 + payload) as u16);
        put_u16(out, self.extension_type.raw());
        match self.extension_type {
            ExtensionType::Dim => {
                let dim = self
                    .dim
                    .as_ref()
                    .ok_or_else(|| Error::Write("DIM extension without data".into()))?;
                put_u16(out, dim.module);
                put_u32(out, dim.address);
                out.extend_from_slice(&fixed_bytes(&dim.description, 80, true));
                out.extend_from_slice(&fixed_bytes(&dim.ecu_id, 32, true));
            }
            ExtensionType::VectorCan => {
                let vc = self
                    .vector_can
                    .as_ref()
                    .ok_or_else(|| Error::Write("VectorCAN extension without data".into()))?;
                put_u32(out, vc.id);
                put_u32(out, vc.index);
                out.extend_from_slice(&fixed_bytes(&vc.message, 36, true));
                out.extend_from_slice(&fixed_bytes(&vc.sender, 36, true));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SrBlock {
    pub data_link: u32,
    pub nr_of_red_samples: u64,
    pub len_of_time_int: f64,
    pub file_offset: u32,
}

impl SrBlock {
    fn parse(data: &[u8], pos: usize) -> Result<(Self, u32)> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let next = r.u32()?;
        let data_link = r.u32()?;
        let nr_of_red_samples = u64::from(r.u32()?);
        let len_of_time_int = r.f64()?;
        Ok((
            SrBlock {
                data_link,
                nr_of_red_samples,
                len_of_time_int,
                file_offset: pos as u32,
            },
            next,
        ))
    }

    fn write_to(&self, out: &mut Vec<u8>, is_last: bool) {
        let pos = out.len();
        let next = if is_last {
            0
        } else {
            (pos + SR_BLOCK_SIZE) as u32
        };
        out.extend_from_slice(b"SR");
        put_u16(out, SR_BLOCK_SIZE as u16);
        put_u32(out, next);
        put_u32(out, self.data_link);
        put_u32(out, self.nr_of_red_samples as u32);
        put_f64(out, self.len_of_time_int);
        debug_assert_eq!(out.len(), pos + SR_BLOCK_SIZE);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TriggerEvent {
    pub time: f64,
    pub pre_time: f64,
    pub post_time: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrBlock {
    pub trigger_events: Vec<TriggerEvent>,
    pub comment: String,
}

impl TrBlock {
    fn parse(data: &[u8], pos: usize) -> Result<Self> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let comment_link = r.u32()?;
        let count = r.u16()?;
        let mut trigger_events = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            trigger_events.push(TriggerEvent {
                time: r.f64()?,
                pre_time: r.f64()?,
                post_time: r.f64()?,
            });
        }
        Ok(TrBlock {
            trigger_events,
            comment: read_tx_block(data, comment_link),
        })
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        let pos = out.len();
        let block_size = 10 + self.trigger_events.len() * 24;
        out.resize(pos + block_size, 0);
        let comment_link = if self.comment.is_empty() {
            0
        } else {
            write_tx_block(out, &self.comment)
        };
        let mut hdr = Vec::with_capacity(block_size);
        hdr.extend_from_slice(b"TR");
        put_u16(&mut hdr, block_size as u16);
        put_u32(&mut hdr, comment_link);
        put_u16(&mut hdr, self.trigger_events.len() as u16);
        for e in &self.trigger_events {
            put_f64(&mut hdr, e.time);
            put_f64(&mut hdr, e.pre_time);
            put_f64(&mut hdr, e.post_time);
        }
        patch(out, pos, &hdr);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct CnBlock {
    pub name: String,
    pub description: String,
    pub bit_offset: u16,
    pub no_of_bits: u32,
    pub channel_type: ChannelType,
    pub signal_type: SignalType,
    pub add_offset: u32,
    pub min_raw: f64,
    pub max_raw: f64,
    pub rate: f64,
    pub comment: String,
    pub long_name: String,
    pub display_name: String,
    pub cc_block: Option<CcBlock>,
    pub ce_block: Option<CeBlock>,
    pub cd_block: Option<CdBlock>,
    pub file_offset: u32,
}

impl Default for CnBlock {
    fn default() -> Self {
        Self::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "",
            "",
            0,
            0,
            0,
            None,
            "",
            "",
            "",
        )
    }
}

impl CnBlock {
    /// `bit_offset = bit_pos % 8`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signal_type: SignalType,
        channel_type: ChannelType,
        name: &str,
        description: &str,
        bit_pos: u32,
        bit_in_byte: u32,
        no_of_bits: u32,
        cc_block: Option<CcBlock>,
        comment: &str,
        long_name: &str,
        display_name: &str,
    ) -> Self {
        debug_assert!(
            matches!(channel_type, ChannelType::Data | ChannelType::Master),
            "CNBLOCK construction supports only Data and Master channel types"
        );
        CnBlock {
            name: name.to_string(),
            description: description.to_string(),
            bit_offset: (bit_pos % 8) as u16,
            no_of_bits,
            channel_type,
            signal_type,
            add_offset: (bit_in_byte + bit_pos) / 8,
            min_raw: f64::NAN,
            max_raw: f64::NAN,
            rate: 0.0,
            comment: comment.to_string(),
            long_name: long_name.to_string(),
            display_name: display_name.to_string(),
            cc_block,
            ce_block: None,
            cd_block: None,
            file_offset: 0,
        }
    }

    pub fn unit(&self) -> &str {
        self.cc_block.as_ref().map_or("", |cc| cc.unit.as_str())
    }

    pub fn min(&self) -> f64 {
        self.cc_block.as_ref().map_or(f64::NAN, |cc| cc.min)
    }

    pub fn max(&self) -> f64 {
        self.cc_block.as_ref().map_or(f64::NAN, |cc| cc.max)
    }

    fn bitmask(&self) -> u64 {
        if self.no_of_bits.is_multiple_of(8) {
            u64::MAX
        } else {
            build_bitmask(self.no_of_bits)
        }
    }

    fn parse(data: &[u8], pos: usize, byte_order: ByteOrder) -> Result<(Self, u32)> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let block_size = usize::from(r.u16()?);
        let next = r.u32()?;
        let cc_link = r.u32()?;
        let ce_link = r.u32()?;
        let cd_link = r.u32()?;
        let comment_link = r.u32()?;
        let type_raw = r.u16()?;
        let channel_type = match type_raw {
            0 => ChannelType::Data,
            1 => ChannelType::Master,
            _ => ChannelType::Data,
        };
        let name = r.text(32)?;
        let description = r.text(128)?;
        let bit_offset = r.u16()?;
        let no_of_bits = u32::from(r.u16()?);
        let signal_raw = r.u16()?;
        let signal_type =
            signal_type_of_code(signal_raw, byte_order).ok_or_else(|| Error::Parse {
                offset: pos as u64,
                message: format!("unknown signal type code {signal_raw}"),
            })?;
        let mut min_raw = f64::NAN;
        let mut max_raw = f64::NAN;
        if r.u16()? > 0 {
            min_raw = r.f64()?;
            max_raw = r.f64()?;
        } else {
            r.skip(16)?;
        }
        let rate = r.f64()?;
        let mut long_name_link = 0;
        if r.pos - pos < block_size {
            long_name_link = r.u32()?;
        }
        let mut display_name_link = 0;
        if r.pos - pos < block_size {
            display_name_link = r.u32()?;
        }
        let mut add_offset = 0;
        if r.pos - pos < block_size {
            add_offset = u32::from(r.u16()?);
        }
        let cc_block = if cc_link != 0 {
            Some(CcBlock::parse(data, cc_link as usize)?)
        } else {
            None
        };
        let ce_block = if ce_link != 0 {
            Some(CeBlock::parse(data, ce_link as usize)?)
        } else {
            None
        };
        let cd_block = if cd_link != 0 {
            Some(CdBlock::parse(data, cd_link as usize)?)
        } else {
            None
        };
        let cn = CnBlock {
            name,
            description,
            bit_offset,
            no_of_bits,
            channel_type,
            signal_type,
            add_offset,
            min_raw,
            max_raw,
            rate,
            comment: read_tx_block(data, comment_link),
            long_name: read_tx_block(data, long_name_link),
            display_name: read_tx_block(data, display_name_link),
            cc_block,
            ce_block,
            cd_block,
            file_offset: pos as u32,
        };
        Ok((cn, next))
    }

    fn write_to(&self, out: &mut Vec<u8>, is_last: bool) -> Result<()> {
        if !matches!(self.channel_type, ChannelType::Data | ChannelType::Master) {
            return Err(Error::Write(format!(
                "channel type {:?} not supported for writing",
                self.channel_type
            )));
        }
        let pos = out.len();
        out.resize(pos + CN_BLOCK_SIZE, 0);
        let long_name_link = if self.long_name.is_empty() {
            0
        } else {
            write_tx_block(out, &self.long_name)
        };
        let display_name_link = if self.display_name.is_empty() {
            0
        } else {
            write_tx_block(out, &self.display_name)
        };
        let comment_link = if self.comment.is_empty() {
            0
        } else {
            write_tx_block(out, &self.comment)
        };
        let cc_link = match &self.cc_block {
            Some(cc) => {
                let p = out.len() as u32;
                cc.write_to(out)?;
                p
            }
            None => 0,
        };
        let ce_link = match &self.ce_block {
            Some(ce) => {
                let p = out.len() as u32;
                ce.write_to(out)?;
                p
            }
            None => 0,
        };
        let cd_link = match &self.cd_block {
            Some(cd) => {
                let p = out.len() as u32;
                cd.write_to(out);
                p
            }
            None => 0,
        };
        let next = if is_last { 0 } else { out.len() as u32 };
        let signal_code =
            signal_code_of_type(self.signal_type, self.no_of_bits).ok_or_else(|| {
                Error::Write(format!(
                    "signal type {:?} not supported for writing",
                    self.signal_type
                ))
            })?;
        let mut hdr = Vec::with_capacity(CN_BLOCK_SIZE);
        hdr.extend_from_slice(b"CN");
        put_u16(&mut hdr, CN_BLOCK_SIZE as u16);
        put_u32(&mut hdr, next);
        put_u32(&mut hdr, cc_link);
        put_u32(&mut hdr, ce_link);
        put_u32(&mut hdr, cd_link);
        put_u32(&mut hdr, comment_link);
        put_u16(
            &mut hdr,
            u16::from(self.channel_type == ChannelType::Master),
        );
        hdr.extend_from_slice(&fixed_bytes(&self.name, 32, true));
        hdr.extend_from_slice(&fixed_bytes(&self.description, 128, true));
        put_u16(&mut hdr, self.bit_offset);
        put_u16(&mut hdr, self.no_of_bits as u16);
        put_u16(&mut hdr, signal_code);
        put_u16(
            &mut hdr,
            u16::from(!self.min_raw.is_nan() && !self.max_raw.is_nan()),
        );
        put_f64(&mut hdr, self.min_raw);
        put_f64(&mut hdr, self.max_raw);
        put_f64(&mut hdr, self.rate);
        put_u32(&mut hdr, long_name_link);
        put_u32(&mut hdr, display_name_link);
        put_u16(&mut hdr, self.add_offset as u16);
        patch(out, pos, &hdr);
        Ok(())
    }

    pub fn decode(
        &self,
        format: ValueObjectFormat,
        record: &[u8],
        add_ofs: usize,
        inverse: bool,
    ) -> f64 {
        let byte_ofs = i64::from(self.add_offset) + i64::from(self.bit_offset / 8) + add_ofs as i64;
        self.decode_at(format, record, byte_ofs, inverse, None)
    }

    /// KeyNotFoundException).
    pub fn decode_read_back(
        &self,
        record: &[u8],
        extra_ofs: i64,
        formulas: Option<&FormulaDict>,
    ) -> f64 {
        let byte_ofs = i64::from(self.add_offset) + i64::from(self.bit_offset / 8) + extra_ofs;
        self.decode_at(
            ValueObjectFormat::Physical,
            record,
            byte_ofs,
            true,
            formulas,
        )
    }

    fn decode_at(
        &self,
        format: ValueObjectFormat,
        record: &[u8],
        byte_ofs: i64,
        inverse: bool,
        formulas: Option<&FormulaDict>,
    ) -> f64 {
        if byte_ofs < 0 {
            return f64::NAN;
        }
        let bit_in_byte = usize::from(self.bit_offset % 8);
        let byte_ofs = byte_ofs as usize;
        let num_bytes = (self.no_of_bits / 8).max(1) as usize;
        let signal = if self.channel_type == ChannelType::VirtualData {
            SignalType::UIntLe
        } else {
            self.signal_type
        };
        let swap = num_bytes > 1 && signal.is_big_endian();
        let Some(slice) = record.get(byte_ofs..byte_ofs + num_bytes) else {
            return f64::NAN;
        };
        let raw = match self.signal_type {
            SignalType::SIntLe | SignalType::SIntBe => {
                let mut a = slice.to_vec();
                if num_bytes > 1 && !self.signal_type.raw().is_multiple_of(2) {
                    a.reverse();
                }
                let v = le_bytes_to_i64(&a);
                let v = if bit_in_byte > 0 { v >> bit_in_byte } else { v };
                v as f64
            }
            SignalType::FloatLe | SignalType::FloatBe => {
                let mut a = slice.to_vec();
                if swap {
                    a.reverse();
                }
                match self.no_of_bits {
                    32 => {
                        let mut a4 = [0u8; 4];
                        a4.copy_from_slice(&a[..4]);
                        f64::from(f32::from_le_bytes(a4))
                    }
                    64 => {
                        let mut a8 = [0u8; 8];
                        a8.copy_from_slice(&a[..8]);
                        f64::from_le_bytes(a8)
                    }
                    _ => return f64::NAN,
                }
            }
            _ => {
                let mut a = slice.to_vec();
                if swap {
                    a.reverse();
                }
                let n = a.len().min(8);
                let mut buf = [0u8; 8];
                buf[..n].copy_from_slice(&a[..n]);
                let mut u = u64::from_le_bytes(buf);
                if bit_in_byte > 0 {
                    u >>= bit_in_byte;
                }
                u &= self.bitmask();
                u as f64
            }
        };
        if format == ValueObjectFormat::Physical {
            self.to_physical_impl(raw, inverse, formulas)
        } else {
            raw
        }
    }

    pub fn to_physical(&self, raw: f64, inverse: bool) -> f64 {
        self.to_physical_impl(raw, inverse, None)
    }

    fn to_physical_impl(&self, raw: f64, inverse: bool, formulas: Option<&FormulaDict>) -> f64 {
        match &self.cc_block {
            None => raw,
            Some(cc) => {
                if cc.conversion_type == ConversionType::TextFormula {
                    match formulas {
                        Some(d) => d.get(&self.name).map_or(f64::NAN, |f| f.to_physical(raw)),
                        None => raw,
                    }
                } else {
                    cc.to_physical(raw, inverse)
                }
            }
        }
    }
}

fn build_bitmask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn le_bytes_to_i64(bytes: &[u8]) -> i64 {
    let mut buf = [0u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    if n > 0 && n < 8 && bytes[n - 1] & 0x80 != 0 {
        for b in &mut buf[n..] {
            *b = 0xFF;
        }
    }
    i64::from_le_bytes(buf)
}

fn signal_type_of_code(code: u16, byte_order: ByteOrder) -> Option<SignalType> {
    Some(match code {
        9 => SignalType::UIntBe,
        13 => SignalType::UIntLe,
        10 => SignalType::SIntBe,
        14 => SignalType::SIntLe,
        11 | 12 => SignalType::FloatBe,
        15 | 16 => SignalType::FloatLe,
        7 => SignalType::String,
        8 => SignalType::ByteArray,
        0 => {
            if byte_order == ByteOrder::MSB_FIRST {
                SignalType::UIntBe
            } else {
                SignalType::UIntLe
            }
        }
        1 => {
            if byte_order == ByteOrder::MSB_FIRST {
                SignalType::SIntBe
            } else {
                SignalType::SIntLe
            }
        }
        2 | 3 => {
            if byte_order == ByteOrder::MSB_FIRST {
                SignalType::FloatBe
            } else {
                SignalType::FloatLe
            }
        }
        _ => return None,
    })
}

fn signal_code_of_type(signal_type: SignalType, no_of_bits: u32) -> Option<u16> {
    Some(match signal_type {
        SignalType::ByteArray => 8,
        SignalType::String => 7,
        SignalType::UIntBe => 9,
        SignalType::UIntLe => 13,
        SignalType::SIntBe => 10,
        SignalType::SIntLe => 14,
        SignalType::FloatBe => {
            if no_of_bits < 64 {
                11
            } else {
                12
            }
        }
        SignalType::FloatLe => {
            if no_of_bits < 64 {
                15
            } else {
                16
            }
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CgBlock {
    pub record_id: u64,
    pub record_size: u32,
    pub record_count: u64,
    pub comment: String,
    pub cn_blocks: Vec<CnBlock>,
    pub sr_blocks: Vec<SrBlock>,
    pub file_offset: u32,
}

impl CgBlock {
    pub fn new(comment: &str) -> Self {
        CgBlock {
            comment: comment.to_string(),
            ..Default::default()
        }
    }

    pub fn record_size_total(&self, record_id_type: RecordIdType) -> u64 {
        let extra = match record_id_type {
            RecordIdType::None => 0,
            RecordIdType::Before8Bit => 1,
            RecordIdType::Before16Bit => 2,
            RecordIdType::Before32Bit => 4,
            RecordIdType::Before64Bit => 8,
            RecordIdType::BeforeAndAfter8Bit => 2,
        };
        u64::from(self.record_size) + extra
    }

    pub fn read_position(&self, record_index: u64, record_id_type: RecordIdType) -> u64 {
        let mut pos = record_index * self.record_size_total(record_id_type);
        if matches!(
            record_id_type,
            RecordIdType::Before8Bit | RecordIdType::BeforeAndAfter8Bit
        ) {
            pos += 1;
        }
        pos
    }

    fn parse(data: &[u8], pos: usize, byte_order: ByteOrder) -> Result<(Self, u32)> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let block_size = usize::from(r.u16()?);
        let next = r.u32()?;
        let first_cn = r.u32()?;
        let comment_link = r.u32()?;
        let record_id = u64::from(r.u16()?);
        let _n_cn = r.u16()?;
        let record_size = u32::from(r.u16()?);
        let record_count = u64::from(r.u32()?);
        let mut sr_link = 0;
        if r.pos - pos < block_size {
            sr_link = r.u32()?;
        }
        let mut cn_blocks = Vec::new();
        let mut link = first_cn;
        while link != 0 {
            let (cn, next_cn) = CnBlock::parse(data, link as usize, byte_order)?;
            cn_blocks.push(cn);
            link = next_cn;
        }
        cn_blocks.sort_by_key(|cn| cn.add_offset + u32::from(cn.bit_offset / 8));
        let mut sr_blocks = Vec::new();
        let mut link = sr_link;
        while link != 0 {
            let (sr, next_sr) = SrBlock::parse(data, link as usize)?;
            sr_blocks.push(sr);
            link = next_sr;
        }
        Ok((
            CgBlock {
                record_id,
                record_size,
                record_count,
                comment: read_tx_block(data, comment_link),
                cn_blocks,
                sr_blocks,
                file_offset: pos as u32,
            },
            next,
        ))
    }

    fn write_to(&self, out: &mut Vec<u8>, is_last: bool) -> Result<()> {
        let pos = out.len();
        out.resize(pos + CG_BLOCK_SIZE, 0);
        let cn_link = if self.cn_blocks.is_empty() {
            0
        } else {
            out.len() as u32
        };
        for (i, cn) in self.cn_blocks.iter().enumerate() {
            cn.write_to(out, i + 1 == self.cn_blocks.len())?;
        }
        let sr_link = if self.sr_blocks.is_empty() {
            0
        } else {
            out.len() as u32
        };
        for (i, sr) in self.sr_blocks.iter().enumerate() {
            sr.write_to(out, i + 1 == self.sr_blocks.len());
        }
        let comment_link = if self.comment.is_empty() {
            0
        } else {
            write_tx_block(out, &self.comment)
        };
        let next = if is_last { 0 } else { out.len() as u32 };
        let mut hdr = Vec::with_capacity(CG_BLOCK_SIZE);
        hdr.extend_from_slice(b"CG");
        put_u16(&mut hdr, CG_BLOCK_SIZE as u16);
        put_u32(&mut hdr, next);
        put_u32(&mut hdr, cn_link);
        put_u32(&mut hdr, comment_link);
        put_u16(&mut hdr, self.record_id as u16);
        put_u16(&mut hdr, self.cn_blocks.len() as u16);
        put_u16(&mut hdr, self.record_size as u16);
        put_u32(&mut hdr, self.record_count as u32);
        put_u32(&mut hdr, sr_link);
        patch(out, pos, &hdr);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DgBlock {
    pub record_id_type: RecordIdType,
    pub cg_blocks: Vec<CgBlock>,
    pub tr_block: Option<TrBlock>,
    pub comment: String,
    pub data: Vec<u8>,
    pub file_offset: u32,
}

impl DgBlock {
    pub fn record_id_size(&self) -> usize {
        match self.record_id_type {
            RecordIdType::BeforeAndAfter8Bit => 1,
            t => usize::from(t.raw()),
        }
    }

    fn parse(data: &[u8], pos: usize, byte_order: ByteOrder) -> Result<(Self, u32)> {
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let _block_size = r.u16()?;
        let next = r.u32()?;
        let first_cg = r.u32()?;
        let tr_link = r.u32()?;
        let data_link = r.u32()?;
        let _n_cg = r.u16()?;
        let record_id_type = match r.u16()? {
            0 => RecordIdType::None,
            1 => RecordIdType::Before8Bit,
            2 => RecordIdType::BeforeAndAfter8Bit,
            _ => RecordIdType::None,
        };
        let mut cg_blocks = Vec::new();
        let mut link = first_cg;
        while link != 0 {
            let (cg, next_cg) = CgBlock::parse(data, link as usize, byte_order)?;
            cg_blocks.push(cg);
            link = next_cg;
        }
        let tr_block = if tr_link != 0 {
            Some(TrBlock::parse(data, tr_link as usize)?)
        } else {
            None
        };
        let data = if data_link == 0 || data_link as usize >= data.len() {
            Vec::new()
        } else {
            let start = data_link as usize;
            let end = if next != 0 {
                (next as usize).min(data.len())
            } else {
                data.len()
            };
            data[start..end.max(start)].to_vec()
        };
        Ok((
            DgBlock {
                record_id_type,
                cg_blocks,
                tr_block,
                comment: String::new(),
                data,
                file_offset: pos as u32,
            },
            next,
        ))
    }

    fn write_to(&self, out: &mut Vec<u8>, is_last: bool) -> Result<()> {
        let pos = out.len();
        out.resize(pos + DG_BLOCK_SIZE, 0);
        let tr_link = match &self.tr_block {
            Some(tr) => {
                let p = out.len() as u32;
                tr.write_to(out);
                p
            }
            None => 0,
        };
        let cg_link = if self.cg_blocks.is_empty() {
            0
        } else {
            out.len() as u32
        };
        for (i, cg) in self.cg_blocks.iter().enumerate() {
            cg.write_to(out, i + 1 == self.cg_blocks.len())?;
        }
        let data_link = out.len() as u32;
        out.extend_from_slice(&self.data);
        let next = if is_last { 0 } else { out.len() as u32 };
        let mut hdr = Vec::with_capacity(DG_BLOCK_SIZE);
        hdr.extend_from_slice(b"DG");
        put_u16(&mut hdr, DG_BLOCK_SIZE as u16);
        put_u32(&mut hdr, next);
        put_u32(&mut hdr, cg_link);
        put_u32(&mut hdr, tr_link);
        put_u32(&mut hdr, data_link);
        put_u16(&mut hdr, self.cg_blocks.len() as u16);
        put_u16(
            &mut hdr,
            match self.record_id_type {
                RecordIdType::Before8Bit => 1,
                RecordIdType::BeforeAndAfter8Bit => 2,
                _ => 0,
            },
        );
        put_u32(&mut hdr, 0);
        patch(out, pos, &hdr);
        Ok(())
    }

    fn resort(self, next_link: u32) -> Vec<DgBlock> {
        let id_size = self.record_id_size();
        if self.cg_blocks.len() < 2 || id_size == 0 {
            return vec![self];
        }
        let mut buffers: indexmap::IndexMap<u64, (CgBlock, Vec<u8>, u64)> =
            indexmap::IndexMap::new();
        for cg in self.cg_blocks {
            buffers.insert(cg.record_id, (cg, Vec::new(), 0));
        }
        let record_id_type = self.record_id_type;
        let data = self.data;
        let mut pos = 0usize;
        while pos + id_size <= data.len() {
            if next_link != 0 && pos + id_size >= data.len() {
                break;
            }
            let id = read_le_uint(&data[pos..], id_size);
            let Some((cg, buf, count)) = buffers.get_mut(&id) else {
                break;
            };
            let rec_size = cg.record_size_total(record_id_type) as usize;
            if pos + rec_size > data.len() {
                break;
            }
            buf.extend_from_slice(&data[pos..pos + rec_size]);
            *count += 1;
            pos += rec_size;
        }
        let mut out = Vec::new();
        for (cg, buf, count) in buffers.into_values() {
            if count > 0 {
                let comment = STR_RESORT_MSG.replace("{0}", &cg.record_id.to_string());
                let mut cg = cg;
                cg.record_count = count;
                out.push(DgBlock {
                    record_id_type,
                    cg_blocks: vec![cg],
                    tr_block: None,
                    comment,
                    data: buf,
                    file_offset: self.file_offset,
                });
            }
        }
        out
    }
}

fn read_le_uint(bytes: &[u8], n: usize) -> u64 {
    let mut v = 0u64;
    for (i, &b) in bytes.iter().take(n).enumerate() {
        v |= u64::from(b) << (i * 8);
    }
    v
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct HdRecordingInfo {
    pub date: String,
    pub time: String,
    pub timestamp_ns: u64,
    pub utc_offset: i16,
    pub time_quality: TimeQualityType,
    pub timer_id: String,
}

impl HdRecordingInfo {
    pub fn from_unix_nanos(ns: u64, utc_offset_hours: i16, time_quality: TimeQualityType) -> Self {
        let secs = ns / 1_000_000_000;
        let days = (secs / 86_400) as i64;
        let tod = secs % 86_400;
        let (y, m, d) = civil_from_days(days);
        HdRecordingInfo {
            date: format!("{d:02}:{m:02}:{y:04}"),
            time: format!("{:02}:{:02}:{:02}", tod / 3600, (tod % 3600) / 60, tod % 60),
            timestamp_ns: ns,
            utc_offset: utc_offset_hours,
            time_quality,
            timer_id: TIMER_ID_LOCAL_PC.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HdBlock {
    pub author: String,
    pub organization: String,
    pub project: String,
    pub subject: String,
    pub date: String,
    pub time: String,
    pub timestamp_ns: u64,
    pub utc_offset: i16,
    pub time_quality: TimeQualityType,
    pub timer_id: String,
    pub comment: String,
    pub program_specific_data: String,
    pub dg_blocks: Vec<DgBlock>,
}

impl HdBlock {
    pub fn new(
        recording: HdRecordingInfo,
        author: &str,
        organization: &str,
        project: &str,
        subject: &str,
        comment: &str,
        program_specific_data: &str,
    ) -> Self {
        HdBlock {
            author: author.to_string(),
            organization: organization.to_string(),
            project: project.to_string(),
            subject: subject.to_string(),
            date: recording.date,
            time: recording.time,
            timestamp_ns: recording.timestamp_ns,
            utc_offset: recording.utc_offset,
            time_quality: recording.time_quality,
            timer_id: recording.timer_id,
            comment: comment.to_string(),
            program_specific_data: program_specific_data.to_string(),
            dg_blocks: Vec::new(),
        }
    }

    pub fn recording_time_unix_nanos(&self) -> u64 {
        if self.timestamp_ns != 0 {
            return self.timestamp_ns;
        }
        parse_hd_date_time(&self.date, &self.time).unwrap_or(0)
    }

    fn parse(data: &[u8], byte_order: ByteOrder) -> Result<Self> {
        let pos = ID_BLOCK_SIZE;
        let mut r = Reader::new(data, pos);
        r.take(2)?;
        let block_size = usize::from(r.u16()?);
        let dg_link = r.u32()?;
        let comment_link = r.u32()?;
        let pr_link = r.u32()?;
        let _n_dg = r.u16()?;
        let date = r.text(10)?;
        let time = r.text(8)?;
        let author = r.text(32)?;
        let organization = r.text(32)?;
        let project = r.text(32)?;
        let subject = r.text(32)?;
        let mut timestamp_ns = 0;
        if r.pos - pos < block_size {
            timestamp_ns = r.u64()?;
        }
        let mut utc_offset = 0;
        if r.pos - pos < block_size {
            utc_offset = r.i16()?;
        }
        let mut time_quality = TimeQualityType::LocalPc;
        if r.pos - pos < block_size {
            let q = r.u16()?;
            time_quality = TimeQualityType::from_raw(q).ok_or_else(|| Error::Parse {
                offset: pos as u64,
                message: format!("unknown time quality {q}"),
            })?;
        }
        let mut timer_id = String::new();
        if r.pos - pos < block_size {
            timer_id = r.text(32)?;
        }
        let mut dg_blocks = Vec::new();
        let mut link = dg_link;
        while link > 0 {
            let (dg, next) = DgBlock::parse(data, link as usize, byte_order)?;
            dg_blocks.extend(dg.resort(next));
            link = next;
        }
        Ok(HdBlock {
            author,
            organization,
            project,
            subject,
            date,
            time,
            timestamp_ns,
            utc_offset,
            time_quality,
            timer_id,
            comment: read_tx_block(data, comment_link),
            program_specific_data: read_tx_block(data, pr_link),
            dg_blocks,
        })
    }

    fn write_to(&self, out: &mut Vec<u8>) -> Result<()> {
        if out.len() < ID_BLOCK_SIZE {
            out.resize(ID_BLOCK_SIZE, 0);
        }
        let pos = ID_BLOCK_SIZE;
        out.resize(pos + HD_BLOCK_SIZE, 0);
        let comment_link = if self.comment.is_empty() {
            0
        } else {
            write_tx_block(out, &self.comment)
        };
        let pr_link = if self.program_specific_data.is_empty() {
            0
        } else {
            write_tx_block(out, &self.program_specific_data)
        };
        let dg_link = if self.dg_blocks.is_empty() {
            0
        } else {
            out.len() as u32
        };
        for (i, dg) in self.dg_blocks.iter().enumerate() {
            dg.write_to(out, i + 1 == self.dg_blocks.len())?;
        }
        let mut hdr = Vec::with_capacity(HD_BLOCK_SIZE);
        hdr.extend_from_slice(b"HD");
        put_u16(&mut hdr, HD_BLOCK_SIZE as u16);
        put_u32(&mut hdr, dg_link);
        put_u32(&mut hdr, comment_link);
        put_u32(&mut hdr, pr_link);
        put_u16(&mut hdr, self.dg_blocks.len() as u16);
        hdr.extend_from_slice(&fixed_bytes(&self.date, 10, false));
        hdr.extend_from_slice(&fixed_bytes(&self.time, 8, false));
        hdr.extend_from_slice(&fixed_bytes(&self.author, 32, true));
        hdr.extend_from_slice(&fixed_bytes(&self.organization, 32, true));
        hdr.extend_from_slice(&fixed_bytes(&self.project, 32, true));
        hdr.extend_from_slice(&fixed_bytes(&self.subject, 32, true));
        put_u64(&mut hdr, self.timestamp_ns);
        put_i16(&mut hdr, self.utc_offset);
        put_u16(&mut hdr, self.time_quality.raw());
        hdr.extend_from_slice(&fixed_bytes(&self.timer_id, 32, true));
        patch(out, pos, &hdr);
        Ok(())
    }
}

fn parse_hd_date_time(date: &str, time: &str) -> Option<u64> {
    let date_disp = date.replace(':', ".");
    let mut dp = date_disp.split('.');
    let day: u32 = dp.next()?.parse().ok()?;
    let month: u32 = dp.next()?.parse().ok()?;
    let year: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hour: u64 = tp.next()?.parse().ok()?;
    let min: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some((days as u64) * 86_400_000_000_000 + (hour * 3600 + min * 60 + sec) * 1_000_000_000)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) → days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * u64::from(if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum BlockRef {
    Dg(usize),
    Cg(usize, usize),
    Cn(usize, usize, usize),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mdf3File {
    pub id_block: IdBlock,
    pub hd_block: HdBlock,
}

impl Mdf3File {
    pub fn new(id_block: IdBlock, hd_block: HdBlock) -> Self {
        Mdf3File { id_block, hd_block }
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let id_block = IdBlock::parse(data)?;
        if id_block.file_id != FILE_ID_MDF && id_block.file_id != FILE_ID_UNFINALIZED {
            return parse_err(
                0,
                format!("not an MDF file (file id {:?})", id_block.file_id),
            );
        }
        let hd_block = HdBlock::parse(data, id_block.byte_order)?;
        let mut file = Mdf3File { id_block, hd_block };
        file.resolve_dependencies();
        Ok(file)
    }

    pub fn write(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.id_block.write_to(&mut out);
        self.hd_block.write_to(&mut out)?;
        Ok(out)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let data = std::fs::read(path)?;
        Self::parse(&data)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.write()?)?;
        Ok(())
    }

    fn resolve_dependencies(&mut self) {
        let mut map: HashMap<u32, BlockRef> = HashMap::new();
        for (d, dg) in self.hd_block.dg_blocks.iter().enumerate() {
            if dg.file_offset != 0 {
                map.insert(dg.file_offset, BlockRef::Dg(d));
            }
            for (c, cg) in dg.cg_blocks.iter().enumerate() {
                if cg.file_offset != 0 {
                    map.insert(cg.file_offset, BlockRef::Cg(d, c));
                }
                for (n, cn) in cg.cn_blocks.iter().enumerate() {
                    if cn.file_offset != 0 {
                        map.insert(cn.file_offset, BlockRef::Cn(d, c, n));
                    }
                }
            }
        }
        for dg in &mut self.hd_block.dg_blocks {
            for cg in &mut dg.cg_blocks {
                for cn in &mut cg.cn_blocks {
                    let Some(cd) = &mut cn.cd_block else { continue };
                    cd.dependencies.retain_mut(|dep| {
                        let dg_ref = map.get(&(dep.link_dg as u32));
                        let cg_ref = map.get(&(dep.link_cg as u32));
                        let cn_ref = map.get(&(dep.link_cn as u32));
                        match (dg_ref, cg_ref, cn_ref) {
                            (
                                Some(BlockRef::Dg(d)),
                                Some(BlockRef::Cg(cd_d, c)),
                                Some(BlockRef::Cn(cn_d, cn_c, n)),
                            ) if d == cd_d && cd_d == cn_d && c == cn_c => {
                                dep.target = (*d, *c, *n);
                                true
                            }
                            _ => false,
                        }
                    });
                }
            }
        }
    }

    fn master_channel_idx(cg: &CgBlock) -> Option<usize> {
        cg.cn_blocks.iter().position(|cn| {
            matches!(
                cn.channel_type,
                ChannelType::Master | ChannelType::VirtualMaster
            )
        })
    }

    fn record_bytes(&self, dgi: usize, cgi: usize, record_index: u64) -> Vec<u8> {
        let dg = &self.hd_block.dg_blocks[dgi];
        let cg = &dg.cg_blocks[cgi];
        let total = cg.record_size_total(dg.record_id_type) as usize;
        let pos = cg.read_position(record_index, dg.record_id_type) as usize;
        let mut buf = vec![0u8; total];
        if pos < dg.data.len() {
            let n = total.min(dg.data.len() - pos);
            buf[..n].copy_from_slice(&dg.data[pos..pos + n]);
        }
        buf
    }

    pub fn master_value(&self, dgi: usize, cgi: usize, cni: usize, record_index: u64) -> f64 {
        let Some(cn) = self
            .hd_block
            .dg_blocks
            .get(dgi)
            .and_then(|d| d.cg_blocks.get(cgi))
            .and_then(|c| c.cn_blocks.get(cni))
        else {
            return f64::NAN;
        };
        if cn.no_of_bits == 0 {
            cn.rate * record_index as f64
        } else {
            self.get_data(ValueObjectFormat::Physical, dgi, cgi, cni, record_index)
        }
    }

    pub fn get_data(
        &self,
        format: ValueObjectFormat,
        dgi: usize,
        cgi: usize,
        cni: usize,
        record_index: u64,
    ) -> f64 {
        let Some(dg) = self.hd_block.dg_blocks.get(dgi) else {
            return f64::NAN;
        };
        let Some(cg) = dg.cg_blocks.get(cgi) else {
            return f64::NAN;
        };
        let Some(cn) = cg.cn_blocks.get(cni) else {
            return f64::NAN;
        };
        if cg.record_size == 0 {
            return f64::NAN;
        }
        let rec = self.record_bytes(dgi, cgi, record_index);
        cn.decode(format, &rec, 0, false)
    }

    fn record_range(
        &self,
        dgi: usize,
        cgi: usize,
        master: usize,
        start: f64,
        end: f64,
    ) -> (u64, u64) {
        let count = self.hd_block.dg_blocks[dgi].cg_blocks[cgi].record_count;
        let last = self.find_last_le(dgi, cgi, master, end, count);
        let first = self.find_last_le(dgi, cgi, master, start, last + 1);
        (first, last)
    }

    fn find_last_le(&self, dgi: usize, cgi: usize, master: usize, cap: f64, hi: u64) -> u64 {
        let (mut lo, mut hi) = (0u64, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.master_value(dgi, cgi, master, mid) <= cap {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo > 0 {
            lo - 1
        } else {
            0
        }
    }

    pub fn enum_samples(
        &self,
        dgi: usize,
        cgi: usize,
        cni: usize,
        format: ValueObjectFormat,
        start: f64,
        end: f64,
    ) -> Vec<DataPoint> {
        let Some(dg) = self.hd_block.dg_blocks.get(dgi) else {
            return Vec::new();
        };
        let Some(cg) = dg.cg_blocks.get(cgi) else {
            return Vec::new();
        };
        if cg.record_count == 0 {
            return Vec::new();
        }
        let Some(master) = Self::master_channel_idx(cg) else {
            return Vec::new();
        };
        let (first, last) = self.record_range(dgi, cgi, master, start, end);
        if last <= first {
            return Vec::new();
        }
        let last = last.min(cg.record_count - 1);
        let mut out = Vec::with_capacity((last - first + 1) as usize);
        for idx in first..=last {
            let rec = self.record_bytes(dgi, cgi, idx);
            let master_cn = &cg.cn_blocks[master];
            let x = if master_cn.no_of_bits == 0 {
                self.master_value(dgi, cgi, master, idx)
            } else {
                master_cn.decode(ValueObjectFormat::Physical, &rec, 0, false)
            };
            let y = cg.cn_blocks[cni].decode(format, &rec, 0, false);
            out.push(DataPoint::new(x, y));
        }
        out
    }

    fn read_string_record(&self, dgi: usize, cgi: usize, cni: usize, record_index: u64) -> String {
        let dg = &self.hd_block.dg_blocks[dgi];
        let cg = &dg.cg_blocks[cgi];
        let cn = &cg.cn_blocks[cni];
        let pos = cg.read_position(record_index, dg.record_id_type) as usize
            + cn.add_offset as usize
            + usize::from(cn.bit_offset / 8);
        let len = (cn.no_of_bits / 8) as usize;
        if pos >= dg.data.len() {
            return String::new();
        }
        let n = len.min(dg.data.len() - pos);
        decode_text(&dg.data[pos..pos + n])
    }

    pub fn get_annotations(&self) -> Vec<Annotation> {
        let mut out = Vec::new();
        for (dgi, dg) in self.hd_block.dg_blocks.iter().enumerate() {
            for (cgi, cg) in dg.cg_blocks.iter().enumerate() {
                let Some(master) = Self::master_channel_idx(cg) else {
                    continue;
                };
                for (cni, cn) in cg.cn_blocks.iter().enumerate() {
                    let sig = cn.signal_type.raw();
                    if !(6..=9).contains(&sig) {
                        continue;
                    }
                    if cn.channel_type != ChannelType::Data {
                        continue;
                    }
                    for idx in 0..cg.record_count {
                        let text = self.read_string_record(dgi, cgi, cni, idx);
                        let t = self.master_value(dgi, cgi, master, idx);
                        if !t.is_nan() {
                            out.push(Annotation::new(t, text));
                        }
                    }
                }
            }
        }
        out
    }

    pub fn get_sample_point_count(&self) -> u64 {
        self.hd_block
            .dg_blocks
            .iter()
            .flat_map(|dg| &dg.cg_blocks)
            .map(|cg| cg.cn_blocks.len() as u64 * cg.record_count)
            .sum()
    }

    pub fn get_length(&self) -> f64 {
        let mut min_v = f64::MAX;
        let mut max_v = f64::MIN;
        for (dgi, dg) in self.hd_block.dg_blocks.iter().enumerate() {
            for (cgi, cg) in dg.cg_blocks.iter().enumerate() {
                if cg.record_count < 1 {
                    continue;
                }
                let Some(master) = Self::master_channel_idx(cg) else {
                    continue;
                };
                let cn = &cg.cn_blocks[master];
                let (first_v, last_v) = if cn.no_of_bits == 0 {
                    (0.0, cn.rate * cg.record_count as f64)
                } else {
                    let v0 = self.get_data(ValueObjectFormat::Physical, dgi, cgi, master, 0);
                    if v0.is_nan() {
                        continue;
                    }
                    let v1 = self.get_data(
                        ValueObjectFormat::Physical,
                        dgi,
                        cgi,
                        master,
                        cg.record_count - 1,
                    );
                    if v1.is_nan() {
                        continue;
                    }
                    (v0, v1)
                };
                if last_v >= first_v {
                    min_v = min_v.min(first_v);
                    max_v = max_v.max(last_v);
                }
            }
        }
        if max_v == f64::MIN {
            return f64::NAN;
        }
        let limits = DataLimits::new(min_v, max_v);
        if limits.min != limits.max {
            limits.max - limits.min
        } else {
            f64::NAN
        }
    }

    pub fn get_raw_data(
        &self,
        dgi: usize,
        cgi: usize,
        cni: usize,
        start: f64,
        end: f64,
    ) -> Option<(Vec<RawDataRecord>, DataLimits)> {
        let dg = self.hd_block.dg_blocks.get(dgi)?;
        let cg = dg.cg_blocks.get(cgi)?;
        let cn = cg.cn_blocks.get(cni)?;
        let master = Self::master_channel_idx(cg)?;
        let (first, last) = self.record_range(dgi, cgi, master, start, end);
        let total = cg.record_size_total(dg.record_id_type) as usize;
        let mut limits_x = DataLimits::default();
        let mut out = Vec::new();
        let mut idx = first;
        while idx < cg.record_count && idx <= last {
            let rec = self.record_bytes(dgi, cgi, idx);
            let ts = self.master_value(dgi, cgi, master, idx);
            limits_x.max = ts;
            if idx == 0 {
                limits_x.min = ts;
            }
            let byte_start = cn.add_offset as usize + usize::from(cn.bit_offset / 8);
            let mut len = total.saturating_sub(byte_start);
            if cni + 1 < cg.cn_blocks.len().saturating_sub(1) {
                let next = &cg.cn_blocks[cni + 1];
                let next_start = next.add_offset as usize + usize::from(next.bit_offset / 8);
                len = (next_start as i64 - byte_start as i64).max(1) as usize;
            }
            let avail = rec.len().saturating_sub(byte_start);
            let data = rec
                .get(byte_start..byte_start + len.min(avail))
                .unwrap_or(&[])
                .to_vec();
            out.push(RawDataRecord {
                timestamp: ts,
                data,
            });
            idx += 1;
        }
        Some((out, limits_x))
    }

    pub fn to_csv(
        &self,
        master_decimal_places: u8,
        channels: Option<&[(usize, usize, usize)]>,
    ) -> Result<String> {
        let mut out = String::new();
        for (dgi, dg) in self.hd_block.dg_blocks.iter().enumerate() {
            for (cgi, cg) in dg.cg_blocks.iter().enumerate() {
                let mut chans: Vec<usize> = (0..cg.cn_blocks.len())
                    .filter(|&k| {
                        let cn = &cg.cn_blocks[k];
                        cn.channel_type != ChannelType::Data
                            || channels.is_none()
                            || channels.is_some_and(|f| f.contains(&(dgi, cgi, k)))
                    })
                    .collect();
                if chans.len() < 2 {
                    continue;
                }
                chans.sort_by(|&a, &b| cn_compare(&cg.cn_blocks[a], &cg.cn_blocks[b]));
                let mut line = String::new();
                for &k in &chans {
                    let cn = &cg.cn_blocks[k];
                    line.push('"');
                    line.push_str(&cn.name);
                    line.push('[');
                    line.push_str(cn.unit());
                    line.push_str("]\";");
                }
                line.pop();
                out.push_str(&line);
                out.push_str("\r\n");
                for idx in 0..cg.record_count {
                    let rec = self.record_bytes(dgi, cgi, idx);
                    let mut line = String::new();
                    for &k in &chans {
                        let cn = &cg.cn_blocks[k];
                        if matches!(
                            cn.channel_type,
                            ChannelType::Master | ChannelType::VirtualMaster
                        ) {
                            let v = self.master_value(dgi, cgi, k, idx);
                            if v.is_nan() {
                                return Err(Error::Write(
                                    "NaN master value encountered during CSV export".into(),
                                ));
                            }
                            line.push_str(&format_f64(round_half_even(v, master_decimal_places)));
                            line.push(';');
                        } else if cn.signal_type == SignalType::String {
                            let text = self.read_string_record(dgi, cgi, k, idx);
                            line.push('"');
                            line.push_str(&text);
                            line.push_str("\";");
                        } else {
                            let v = cn.decode(ValueObjectFormat::Physical, &rec, 0, false);
                            if v.is_nan() {
                                return Err(Error::Write(
                                    "NaN value encountered during CSV export".into(),
                                ));
                            }
                            line.push_str(&format_f64(v));
                            line.push(';');
                        }
                    }
                    line.pop();
                    out.push_str(&line);
                    out.push_str("\r\n");
                }
            }
        }
        Ok(out)
    }
}

fn cn_compare(a: &CnBlock, b: &CnBlock) -> std::cmp::Ordering {
    if std::ptr::eq(a, b) {
        return std::cmp::Ordering::Equal;
    }
    if matches!(
        a.channel_type,
        ChannelType::Master | ChannelType::VirtualMaster
    ) {
        return std::cmp::Ordering::Less;
    }
    if matches!(
        b.channel_type,
        ChannelType::Master | ChannelType::VirtualMaster
    ) {
        return std::cmp::Ordering::Greater;
    }
    a.name.cmp(&b.name)
}

fn round_half_even(v: f64, digits: u8) -> f64 {
    if !v.is_finite() {
        return v;
    }
    let m = 10f64.powi(i32::from(digits));
    (v * m).round_ties_even() / m
}

fn format_f64(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let neg = v < 0.0;
    let s = format!("{:.14e}", v.abs());
    let Some((mant, exp)) = s.split_once('e') else {
        return s;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return s;
    };
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..15).contains(&exp) {
        if exp >= 0 {
            let exp = exp as usize;
            if digits.len() > exp + 1 {
                out.push_str(&digits[..exp + 1]);
                out.push('.');
                out.push_str(&digits[exp + 1..]);
            } else {
                out.push_str(digits);
                for _ in 0..exp + 1 - digits.len() {
                    out.push('0');
                }
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if exp >= 0 { '+' } else { '-' });
        out.push_str(&format!("{:02}", exp.abs()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::{AnnotationList, MdfReader, MdfType, MdfWriter};
    use autors_a2l::model::enums::DataType;

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn id_block_write_parse_roundtrip() {
        let id = IdBlock::new_v3(PROGRAM_ID, 936);
        let mut buf = Vec::new();
        id.write_to(&mut buf);
        assert_eq!(buf.len(), 64);
        assert_eq!(&buf[0..8], b"MDF     ");
        assert_eq!(&buf[8..16], b"3.30    ");
        assert_eq!(&buf[16..24], b"AUTORS  ");
        assert_eq!(
            u16::from_le_bytes(buf[24..26].try_into().unwrap()),
            0,
            "byte order is always written as zero"
        );
        assert_eq!(u16::from_le_bytes(buf[28..30].try_into().unwrap()), 330);
        assert_eq!(u16::from_le_bytes(buf[30..32].try_into().unwrap()), 936);
        let back = IdBlock::parse(&buf).unwrap();
        assert_eq!(back, id);
        assert!(IdBlock::parse(&buf[..32]).is_err());
    }

    #[test]
    fn tx_block_helper() {
        let mut buf = vec![0u8; 64];
        let link = write_tx_block(&mut buf, "hello");
        assert_eq!(link, 64);
        assert_eq!(&buf[64..66], b"TX");
        assert_eq!(u16::from_le_bytes(buf[66..68].try_into().unwrap()), 10);
        assert_eq!(read_tx_block(&buf, link), "hello");
        assert_eq!(read_tx_block(&buf, 0), "");
        assert_eq!(read_tx_block(&buf, 9999), "");
        assert_eq!(
            read_tx_block(&buf, 2),
            "",
            "a block shorter than four bytes yields an empty string"
        );
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn rational_coeffs_linear_and_rational() {
        let c = RationalCoeffs::from_params(&[2.0, 3.0]).unwrap();
        assert!((c.to_physical(100.0) - 302.0).abs() < 1e-9);
        assert!((c.to_raw(302.0) - 100.0).abs() < 1e-9);
        let id = RationalCoeffs::from_params(&RATIONAL_IDENTITY).unwrap();
        assert_eq!(id.to_physical(42.0), 42.0);
        let r = RationalCoeffs::from_params(&[0.0, 1.0, 1.0, 0.0, 1.0, 2.0]).unwrap();
        let phys = r.to_raw(2.0);
        assert!((phys - 0.75).abs() < 1e-12, "(2+1)/(2+2) = 0.75");
        let back = r.to_physical(phys);
        assert!(
            (back - 2.0).abs() < 1e-9,
            "inverse conversion recovers the raw value"
        );
        assert!(RationalCoeffs::from_params(&[1.0, 2.0, 3.0]).is_none());
    }

    #[test]
    fn cc_block_tab_and_tabint() {
        let mut cc = CcBlock::new(ConversionType::Tab, "rpm", f64::NAN, f64::NAN);
        cc.insert_number_number(3.0, 300.0);
        cc.insert_number_number(1.0, 100.0);
        assert_eq!(cc.to_physical(1.0, false), 100.0);
        assert_eq!(
            cc.to_physical(2.0, false),
            2.0,
            "an unmatched value remains unchanged"
        );
        assert_eq!(cc.pairs[0].0, CcKey::Number(1.0));

        let mut cc = CcBlock::new(ConversionType::TabInt, "", f64::NAN, f64::NAN);
        cc.insert_number_number(0.0, 0.0);
        cc.insert_number_number(10.0, 100.0);
        assert_eq!(cc.to_physical(5.0, false), 50.0, "linear interpolation");
        assert_eq!(
            cc.to_physical(-1.0, false),
            0.0,
            "values below the first key use the first value"
        );
        assert_eq!(
            cc.to_physical(20.0, false),
            100.0,
            "values above the last key use the last value"
        );
    }

    #[test]
    fn cc_block_polynomial_and_none() {
        let mut cc = CcBlock::new(ConversionType::Polynomial, "", f64::NAN, f64::NAN);
        // (p1 - p3 x2) / (p2 x2 - p0),x2 = raw - p4 - p5
        cc.params = vec![0.0, 4.0, 1.0, 0.0, 0.0, 0.0];
        cc.recalc_coeffs();
        assert_eq!(cc.to_physical(2.0, false), 2.0, "4 / 2 = 2");
        let cc = CcBlock::new(ConversionType::None, "s", 0.0, 1.0);
        assert_eq!(cc.to_physical(0.5, false), 0.5);
    }

    #[test]
    fn decode_read_back_text_formula() {
        let mut cc = CcBlock::new(ConversionType::TextFormula, "km/h", f64::NAN, f64::NAN);
        cc.formula = "X1 * 2 + 1".to_string();
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "speed",
            "",
            64,
            0,
            16,
            Some(cc),
            "",
            "",
            "",
        );
        let data = 5u16.to_le_bytes();
        assert_eq!(cn.decode_read_back(&data, -8, None), 5.0);
        let mut dict = FormulaDict::new(true);
        dict.add_formula("speed", "X1 * 2 + 1", None).unwrap();
        dict.build(None);
        assert_eq!(cn.decode_read_back(&data, -8, Some(&dict)), 11.0);
        let mut dict2 = FormulaDict::new(true);
        dict2.add_formula("other", "X1 + 1", None).unwrap();
        dict2.build(None);
        assert!(cn.decode_read_back(&data, -8, Some(&dict2)).is_nan());
        assert!(cn.decode_read_back(&data, 0, None).is_nan());
    }

    #[test]
    fn cc_block_write_parse_roundtrip() {
        let mut cc = CcBlock::new(ConversionType::TextTable, "V", 0.0, 5.0);
        cc.insert_number_text(1.0, "on".to_string());
        cc.insert_number_text(0.0, "off".to_string());
        let mut buf = Vec::new();
        cc.write_to(&mut buf).unwrap();
        // 46 + 2*40 = 126
        assert_eq!(u16::from_le_bytes(buf[2..4].try_into().unwrap()), 126);
        let back = CcBlock::parse(&buf, 0).unwrap();
        assert_eq!(back.conversion_type, ConversionType::TextTable);
        assert_eq!(back.pairs, cc.pairs);
        assert_eq!(back.unit, "V");
        assert_eq!(back.min, 0.0);
        assert_eq!(back.max, 5.0);

        let mut cc = CcBlock::new(ConversionType::ParametricLinear, "km/h", 0.0, 250.0);
        cc.params = vec![2.0, 3.0];
        cc.recalc_coeffs();
        let mut buf = Vec::new();
        cc.write_to(&mut buf).unwrap();
        assert_eq!(u16::from_le_bytes(buf[2..4].try_into().unwrap()), 62);
        let back = CcBlock::parse(&buf, 0).unwrap();
        assert_eq!(back.params, vec![2.0, 3.0]);
        assert!((back.to_physical(100.0, false) - 302.0).abs() < 1e-9);

        let cc = CcBlock::new(ConversionType::TabRange, "", f64::NAN, f64::NAN);
        let mut buf = Vec::new();
        assert!(cc.write_to(&mut buf).is_err());
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn signal_type_code_mapping() {
        assert_eq!(
            signal_type_of_code(0, ByteOrder::MSB_LAST),
            Some(SignalType::UIntLe)
        );
        assert_eq!(
            signal_type_of_code(0, ByteOrder::MSB_FIRST),
            Some(SignalType::UIntBe)
        );
        assert_eq!(
            signal_type_of_code(1, ByteOrder::MSB_FIRST),
            Some(SignalType::SIntBe)
        );
        assert_eq!(
            signal_type_of_code(3, ByteOrder::MSB_LAST),
            Some(SignalType::FloatLe)
        );
        assert_eq!(
            signal_type_of_code(9, ByteOrder::MSB_LAST),
            Some(SignalType::UIntBe)
        );
        assert_eq!(
            signal_type_of_code(16, ByteOrder::MSB_LAST),
            Some(SignalType::FloatLe)
        );
        assert_eq!(
            signal_type_of_code(7, ByteOrder::MSB_LAST),
            Some(SignalType::String)
        );
        assert_eq!(signal_type_of_code(99, ByteOrder::MSB_LAST), None);
        assert_eq!(signal_code_of_type(SignalType::FloatLe, 32), Some(15));
        assert_eq!(signal_code_of_type(SignalType::FloatLe, 64), Some(16));
        assert_eq!(signal_code_of_type(SignalType::FloatBe, 32), Some(11));
        assert_eq!(signal_code_of_type(SignalType::String, 8), Some(7));
        assert_eq!(signal_code_of_type(SignalType::StringUtf8, 8), None);
    }

    #[test]
    fn decode_uint_sint_float() {
        // UINT_LE 16
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&100u16.to_le_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 100.0);
        // UINT_BE 16
        let cn = CnBlock::new(
            SignalType::UIntBe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&100u16.to_be_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 100.0);
        let cn = CnBlock::new(
            SignalType::SIntLe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&(-2i16).to_le_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), -2.0);
        // SINT_BE 16
        let cn = CnBlock::new(
            SignalType::SIntBe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&(-2i16).to_be_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), -2.0);
        // FLOAT_LE 32 / FLOAT_BE 64
        let cn = CnBlock::new(
            SignalType::FloatLe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            32,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&20.5f32.to_le_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 20.5);
        let cn = CnBlock::new(
            SignalType::FloatBe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            64,
            None,
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&20.5f64.to_be_bytes());
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 20.5);
    }

    #[test]
    fn decode_bit_offset_masking() {
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "a",
            "",
            68,
            0,
            12,
            None,
            "",
            "",
            "",
        );
        assert_eq!(cn.add_offset, 8);
        assert_eq!(cn.bit_offset, 4);
        let mut rec = vec![0u8; 8];
        rec.push(0xF0);
        assert_eq!(
            cn.decode(ValueObjectFormat::Raw, &rec, 0, false),
            15.0,
            "0xF0 >> 4 = 15"
        );
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "a",
            "",
            68,
            0,
            8,
            None,
            "",
            "",
            "",
        );
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 15.0);
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "a",
            "",
            64,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        assert_eq!(cn.bitmask(), u64::MAX);
    }

    #[test]
    fn decode_with_linear_conversion() {
        let mut cc = CcBlock::new(ConversionType::ParametricLinear, "km/h", 0.0, 250.0);
        cc.params = vec![2.0, 3.0];
        cc.recalc_coeffs();
        let cn = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "speed",
            "",
            64,
            0,
            16,
            Some(cc),
            "",
            "",
            "",
        );
        let mut rec = vec![0u8; 8];
        rec.extend_from_slice(&100u16.to_le_bytes());
        assert!((cn.decode(ValueObjectFormat::Physical, &rec, 0, false) - 302.0).abs() < 1e-9);
        assert_eq!(cn.decode(ValueObjectFormat::Raw, &rec, 0, false), 100.0);
        assert!(
            (cn.decode(ValueObjectFormat::Physical, &rec, 0, true) - (100.0 / 3.0 - 2.0 / 3.0))
                .abs()
                < 1e-9
        );
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    fn build_manual_file() -> Mdf3File {
        let id = IdBlock::new_v3("TEST", 1252);
        let recording = HdRecordingInfo {
            date: "01:02:2024".to_string(),
            time: "03:04:05".to_string(),
            timestamp_ns: 1_700_000_000_000_000_000,
            utc_offset: 1,
            time_quality: TimeQualityType::LocalPc,
            timer_id: TIMER_ID_LOCAL_PC.to_string(),
        };
        let mut hd = HdBlock::new(recording, "me", "org", "proj", "subj", "hd comment", "psd");
        let mut dg = DgBlock::default();
        let mut cg = CgBlock::new("cg1");
        cg.record_id = 0;
        cg.record_size = 8 + 2 + 2;
        cg.record_count = 2;
        let master = CnBlock::new(
            SignalType::FloatLe,
            ChannelType::Master,
            "time",
            "master",
            0,
            0,
            64,
            None,
            "",
            "",
            "",
        );
        let ch1 = CnBlock::new(
            SignalType::UIntLe,
            ChannelType::Data,
            "u16ch",
            "d1",
            64,
            0,
            16,
            None,
            "cn comment",
            "long name",
            "disp",
        );
        let mut ch2 = CnBlock::new(
            SignalType::SIntBe,
            ChannelType::Data,
            "s16be",
            "",
            80,
            0,
            16,
            None,
            "",
            "",
            "",
        );
        ch2.ce_block = Some(CeBlock::new_dim(DimType {
            module: 1,
            address: 0x8000,
            description: "dim desc".to_string(),
            ecu_id: "ECU1".to_string(),
        }));
        ch2.cd_block = Some(CdBlock {
            dependency_type: 2,
            dependencies: vec![DependencyType::new(1, 2, 3)],
        });
        cg.cn_blocks = vec![master, ch1, ch2];
        cg.sr_blocks.push(SrBlock {
            data_link: 0,
            nr_of_red_samples: 7,
            len_of_time_int: 0.5,
            file_offset: 0,
        });
        dg.cg_blocks.push(cg);
        dg.tr_block = Some(TrBlock {
            trigger_events: vec![TriggerEvent {
                time: 1.0,
                pre_time: -0.5,
                post_time: 0.5,
            }],
            comment: "trig".to_string(),
        });
        for i in 0..2u16 {
            dg.data
                .extend_from_slice(&(0.5f64 * f64::from(i)).to_le_bytes());
            dg.data.extend_from_slice(&(10 + i).to_le_bytes());
            dg.data.extend_from_slice(&(-(i as i16)).to_be_bytes());
        }
        hd.dg_blocks.push(dg);
        Mdf3File::new(id, hd)
    }

    #[test]
    fn manual_file_layout_and_roundtrip() {
        let mut file = build_manual_file();
        let bytes = file.write().unwrap();
        // ID + HD
        assert_eq!(&bytes[0..8], b"MDF     ");
        assert_eq!(&bytes[64..66], b"HD");
        assert_eq!(u16::from_le_bytes(bytes[66..68].try_into().unwrap()), 208);
        // HD: dg_link/comment_link/pr_link
        let dg_link = u32::from_le_bytes(bytes[68..72].try_into().unwrap()) as usize;
        let comment_link = u32::from_le_bytes(bytes[72..76].try_into().unwrap()) as usize;
        let pr_link = u32::from_le_bytes(bytes[76..80].try_into().unwrap()) as usize;
        assert_eq!(&bytes[comment_link..comment_link + 2], b"TX");
        assert_eq!(&bytes[pr_link..pr_link + 2], b"TX");
        assert_eq!(
            u16::from_le_bytes(bytes[80..82].try_into().unwrap()),
            1,
            "n_dg"
        );
        assert_eq!(&bytes[82..92], b"01:02:2024");
        assert_eq!(&bytes[92..100], b"03:04:05");
        assert_eq!(
            i16::from_le_bytes(bytes[236..238].try_into().unwrap()),
            1,
            "utc offset"
        );
        // DG
        assert_eq!(&bytes[dg_link..dg_link + 2], b"DG");
        let cg_link =
            u32::from_le_bytes(bytes[dg_link + 8..dg_link + 12].try_into().unwrap()) as usize;
        let tr_link =
            u32::from_le_bytes(bytes[dg_link + 12..dg_link + 16].try_into().unwrap()) as usize;
        let data_link =
            u32::from_le_bytes(bytes[dg_link + 16..dg_link + 20].try_into().unwrap()) as usize;
        assert_eq!(&bytes[tr_link..tr_link + 2], b"TR");
        assert_eq!(&bytes[cg_link..cg_link + 2], b"CG");
        assert_eq!(bytes.len(), data_link + 24);
        let back = Mdf3File::parse(&bytes).unwrap();
        assert!(back.hd_block.dg_blocks[0].cg_blocks[0].cn_blocks[2]
            .cd_block
            .as_ref()
            .unwrap()
            .dependencies
            .is_empty());

        let first_cn =
            u32::from_le_bytes(bytes[cg_link + 8..cg_link + 12].try_into().unwrap()) as usize;
        let ch1_off =
            u32::from_le_bytes(bytes[first_cn + 4..first_cn + 8].try_into().unwrap()) as usize;
        let ch2_off =
            u32::from_le_bytes(bytes[ch1_off + 4..ch1_off + 8].try_into().unwrap()) as usize;
        let cd = file.hd_block.dg_blocks[0].cg_blocks[0].cn_blocks[2]
            .cd_block
            .as_mut()
            .unwrap();
        cd.dependencies[0] = DependencyType::new(dg_link as i64, cg_link as i64, ch2_off as i64);
        let bytes = file.write().unwrap();
        assert_eq!(bytes.len(), data_link + 24, "layout remains unchanged");
        let back = Mdf3File::parse(&bytes).unwrap();
        assert_eq!(back.id_block.program_id, "TEST");
        assert_eq!(back.id_block.code_page, 1252);
        let hd = &back.hd_block;
        assert_eq!(hd.author, "me");
        assert_eq!(hd.comment, "hd comment");
        assert_eq!(hd.program_specific_data, "psd");
        assert_eq!(hd.date, "01:02:2024");
        assert_eq!(hd.timestamp_ns, 1_700_000_000_000_000_000);
        assert_eq!(hd.dg_blocks.len(), 1);
        let dg = &hd.dg_blocks[0];
        assert_eq!(dg.data.len(), 24);
        let tr = dg.tr_block.as_ref().unwrap();
        assert_eq!(tr.comment, "trig");
        assert_eq!(tr.trigger_events[0].pre_time, -0.5);
        let cg = &dg.cg_blocks[0];
        assert_eq!(cg.record_size, 12);
        assert_eq!(cg.record_count, 2);
        assert_eq!(cg.comment, "cg1");
        assert_eq!(cg.sr_blocks.len(), 1);
        assert_eq!(cg.sr_blocks[0].nr_of_red_samples, 7);
        assert_eq!(cg.cn_blocks[0].name, "time");
        assert_eq!(cg.cn_blocks[1].name, "u16ch");
        assert_eq!(cg.cn_blocks[1].comment, "cn comment");
        assert_eq!(cg.cn_blocks[1].long_name, "long name");
        assert_eq!(cg.cn_blocks[1].display_name, "disp");
        assert_eq!(cg.cn_blocks[2].name, "s16be");
        let ce = cg.cn_blocks[2].ce_block.as_ref().unwrap();
        assert_eq!(ce.dim.as_ref().unwrap().address, 0x8000);
        let deps = &cg.cn_blocks[2].cd_block.as_ref().unwrap().dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].target, (0, 0, 2));
        assert_eq!(back.get_data(ValueObjectFormat::Physical, 0, 0, 0, 1), 0.5);
        assert_eq!(back.get_data(ValueObjectFormat::Raw, 0, 0, 1, 1), 11.0);
        assert_eq!(back.get_data(ValueObjectFormat::Raw, 0, 0, 2, 1), -1.0);
        let bytes2 = back.write().unwrap();
        assert_eq!(bytes2, bytes);
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    fn golden_file() -> Mdf3File {
        let mut w = MdfWriter::new_v3(
            "Max Mustermann",
            "autors",
            "DemoProject",
            "DemoSubject",
            "header comment",
            HdRecordingInfo {
                date: "30:07:2026".to_string(),
                time: "08:59:16".to_string(),
                timestamp_ns: 0x18C7_059E_D266_E044,
                utc_offset: 8,
                time_quality: TimeQualityType::LocalPc,
                timer_id: TIMER_ID_LOCAL_PC.to_string(),
            },
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
            w.add_data_entry(h, 0.1 * f64::from(i), &rec).unwrap();
        }
        w.add_annotation(0.05, "first note");
        w.add_annotation(0.15, "second longer note");
        Mdf3File::parse(&w.write().unwrap()).unwrap()
    }

    #[test]
    fn golden_read_values_and_annotations() {
        let f = golden_file();
        assert_eq!(f.id_block.file_id, FILE_ID_MDF);
        assert_eq!(f.id_block.format_id, FORMAT_ID_V330);
        assert_eq!(f.id_block.program_id, PROGRAM_ID);
        assert_eq!(f.id_block.version, 330);
        let hd = &f.hd_block;
        assert_eq!(hd.author, "Max Mustermann");
        assert_eq!(hd.organization, "autors");
        assert_eq!(hd.project, "DemoProject");
        assert_eq!(hd.subject, "DemoSubject");
        assert_eq!(hd.comment, "header comment");
        assert_eq!(hd.timer_id, TIMER_ID_LOCAL_PC);
        assert_eq!(hd.dg_blocks.len(), 2, "one annotation DG and one data DG");
        let dg = &hd.dg_blocks[1];
        let cg = &dg.cg_blocks[0];
        assert_eq!(cg.record_size, 14);
        assert_eq!(cg.record_count, 3);
        assert_eq!(cg.comment, "cg comment");
        assert_eq!(cg.cn_blocks.len(), 3);
        assert_eq!(cg.cn_blocks[1].name, "speed");
        assert_eq!(cg.cn_blocks[1].signal_type, SignalType::UIntLe);
        assert_eq!(cg.cn_blocks[1].no_of_bits, 16);
        assert_eq!(cg.cn_blocks[1].add_offset, 8);
        assert_eq!(cg.cn_blocks[1].unit(), "km/h");
        assert_eq!(cg.cn_blocks[1].description, "vehicle speed");
        assert_eq!(cg.cn_blocks[2].name, "temp");
        assert_eq!(cg.cn_blocks[2].signal_type, SignalType::FloatLe);
        assert_eq!(cg.cn_blocks[2].add_offset, 10);
        assert_eq!(f.get_data(ValueObjectFormat::Physical, 1, 0, 0, 0), 0.0);
        assert_eq!(f.get_data(ValueObjectFormat::Physical, 1, 0, 0, 2), 0.2);
        assert!((f.get_data(ValueObjectFormat::Physical, 1, 0, 1, 0) - 302.0).abs() < 1e-9);
        assert!((f.get_data(ValueObjectFormat::Physical, 1, 0, 1, 2) - 308.0).abs() < 1e-9);
        assert_eq!(f.get_data(ValueObjectFormat::Physical, 1, 0, 2, 1), 21.5);
        assert_eq!(f.get_data(ValueObjectFormat::Raw, 1, 0, 1, 0), 100.0);
        let master_cc = cg.cn_blocks[0].cc_block.as_ref().unwrap();
        assert_eq!(master_cc.min, 0.0);
        assert_eq!(master_cc.max, 0.2);
        let ann = f.get_annotations();
        assert_eq!(ann.len(), 2);
        assert_eq!(ann[0], Annotation::new(0.05, "first note"));
        assert_eq!(ann[1], Annotation::new(0.15, "second longer note"));
        let acg = &hd.dg_blocks[0].cg_blocks[0];
        assert_eq!(acg.comment, ANNOTATION_CG_COMMENT);
        assert_eq!(acg.record_size, 27);
        assert_eq!(acg.record_count, 2);
        assert_eq!(acg.cn_blocks[1].name, ANNOTATION_CHANNEL_NAME);
        assert_eq!(acg.cn_blocks[1].signal_type, SignalType::String);
        assert_eq!(acg.cn_blocks[1].no_of_bits, 152);
        assert_eq!(f.get_length(), 0.2);
        assert_eq!(f.get_sample_point_count(), 2 * 2 + 3 * 3);
        // RecordingTime
        assert_eq!(hd.recording_time_unix_nanos(), 0x18C7_059E_D266_E044);
    }

    #[test]
    fn golden_enum_samples_and_csv() {
        let f = golden_file();
        let samples = f.enum_samples(1, 0, 1, ValueObjectFormat::Physical, f64::MIN, f64::MAX);
        assert_eq!(samples.len(), 3);
        assert!(samples[0].x == 0.0 && (samples[0].y - 302.0).abs() < 1e-9);
        assert!(samples[2].x == 0.2 && (samples[2].y - 308.0).abs() < 1e-9);
        let samples = f.enum_samples(1, 0, 1, ValueObjectFormat::Physical, 0.1, 0.2);
        assert_eq!(samples.len(), 2);
        assert!(samples[0].x == 0.1 && (samples[0].y - 305.0).abs() < 1e-9);
        assert!(samples[1].x == 0.2 && (samples[1].y - 308.0).abs() < 1e-9);
        assert!(f
            .enum_samples(1, 0, 1, ValueObjectFormat::Physical, 0.1, 0.15)
            .is_empty());
        let csv = f.to_csv(3, None).unwrap();
        let expected = "\"time[s]\";\"Annotation[]\"\r\n0.05;\"first note\"\r\n0.15;\"second longer note\"\r\n\
            \"time[s]\";\"speed[km/h]\";\"temp[degC]\"\r\n0;302;20.5\r\n0.1;305;21.5\r\n0.2;308;22.5\r\n";
        assert_eq!(csv, expected);
        let csv = f.to_csv(2, Some(&[(1usize, 0usize, 2usize)])).unwrap();
        assert!(csv.contains("\"time[s]\";\"temp[degC]\""));
        assert!(!csv.contains("speed"));
        let (raw, limits) = f.get_raw_data(1, 0, 1, 0.0, 1.0).unwrap();
        assert_eq!(raw.len(), 3);
        assert_eq!(raw[0].timestamp, 0.0);
        let mut expected = 100u16.to_le_bytes().to_vec();
        expected.extend_from_slice(&20.5f32.to_le_bytes());
        assert_eq!(raw[0].data, expected);
        assert_eq!(limits.min, 0.0);
        assert_eq!(limits.max, 0.2);
    }

    #[test]
    fn golden_parse_write_idempotent() {
        let f = golden_file();
        let b1 = f.write().unwrap();
        let f2 = Mdf3File::parse(&b1).unwrap();
        let b2 = f2.write().unwrap();
        assert_eq!(b2, b1, "parse-write cycles are idempotent");
    }

    #[test]
    fn reader_dispatch_and_bad_files() {
        let f = golden_file();
        let bytes = f.write().unwrap();
        let r = MdfReader::parse(&bytes).unwrap();
        assert_eq!(r.mdf_type(), MdfType::V3);
        assert!(r.as_v3().is_some());
        assert!(MdfReader::parse(&[]).is_err());
        assert!(MdfReader::parse(&[0u8; 100]).is_err());
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert!(MdfReader::parse(&bad).is_err());
        let mut v4 = bytes.clone();
        v4[28] = 0x9A; // version = 410
        v4[29] = 0x01;
        let err = MdfReader::parse(&v4).unwrap_err();
        assert!(err.to_string().contains("V4"), "{err}");
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn resort_splits_multi_cg_dg() {
        let id = IdBlock::new_v3("T", 1252);
        let recording = HdRecordingInfo {
            date: "01:01:2024".to_string(),
            time: "00:00:00".to_string(),
            timestamp_ns: 0,
            utc_offset: 0,
            time_quality: TimeQualityType::LocalPc,
            timer_id: String::new(),
        };
        let mut hd = HdBlock::new(recording, "", "", "", "", "", "");
        let mut dg = DgBlock {
            record_id_type: RecordIdType::Before8Bit,
            ..Default::default()
        };
        for rid in 0..2u64 {
            let mut cg = CgBlock::new("");
            cg.record_id = rid;
            cg.record_size = 1;
            cg.record_count = 0;
            let mut master = CnBlock::new(
                SignalType::UIntLe,
                ChannelType::Master,
                "t",
                "",
                0,
                0,
                8,
                None,
                "",
                "",
                "",
            );
            master.rate = 1.0;
            cg.cn_blocks.push(master);
            dg.cg_blocks.push(cg);
        }
        dg.data = vec![0, 10, 1, 20, 0, 11, 1, 21, 0, 12];
        hd.dg_blocks.push(dg);
        let bytes = Mdf3File::new(id, hd).write().unwrap();
        let back = Mdf3File::parse(&bytes).unwrap();
        assert_eq!(
            back.hd_block.dg_blocks.len(),
            2,
            "record IDs split the input into two DG blocks"
        );
        for (i, dg) in back.hd_block.dg_blocks.iter().enumerate() {
            assert_eq!(dg.cg_blocks.len(), 1);
            assert_eq!(dg.cg_blocks[0].record_id, i as u64);
            assert_eq!(dg.cg_blocks[0].record_count, if i == 0 { 3 } else { 2 });
            assert!(dg.comment.contains("Resorted"), "{}", dg.comment);
            assert!(dg.comment.contains(&format!("(Record ID {i})")));
        }
        assert_eq!(back.hd_block.dg_blocks[0].data, vec![0, 10, 0, 11, 0, 12]);
        assert_eq!(back.hd_block.dg_blocks[1].data, vec![1, 20, 1, 21]);
        assert_eq!(back.get_data(ValueObjectFormat::Raw, 0, 0, 0, 1), 11.0);
        assert_eq!(back.get_data(ValueObjectFormat::Raw, 1, 0, 0, 1), 21.0);
    }

    #[test]
    fn civil_date_math() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 7, 30)), (2026, 7, 30));
        assert_eq!(
            days_from_civil(2000, 2, 29) - days_from_civil(1999, 12, 31),
            60
        );
        let info =
            HdRecordingInfo::from_unix_nanos(0x18C7_059E_D266_E044, 8, TimeQualityType::LocalPc);
        assert!(info.date.ends_with(":2026"), "{}", info.date);
        assert_eq!(info.time.len(), 8);
    }

    #[test]
    fn recording_time_fallback_parse() {
        let mut hd = HdBlock::new(
            HdRecordingInfo {
                date: "30:07:2026".to_string(),
                time: "08:59:16".to_string(),
                timestamp_ns: 0,
                utc_offset: 0,
                time_quality: TimeQualityType::LocalPc,
                timer_id: String::new(),
            },
            "",
            "",
            "",
            "",
            "",
            "",
        );
        let days = days_from_civil(2026, 7, 30) as u64;
        assert_eq!(
            hd.recording_time_unix_nanos(),
            days * 86_400_000_000_000 + (8 * 3600 + 59 * 60 + 16) * 1_000_000_000
        );
        hd.date = "garbage".to_string();
        assert_eq!(
            hd.recording_time_unix_nanos(),
            0,
            "parse failures fall back to the Unix epoch"
        );
    }

    #[test]
    fn annotation_list_in_base_used_for_writer_records() {
        let mut list = AnnotationList::new();
        list.add(Annotation::new(0.5, "ab"));
        let rec = list.write_records();
        assert_eq!(rec.len(), 8 + 3);
        assert_eq!(f64::from_le_bytes(rec[0..8].try_into().unwrap()), 0.5);
        assert_eq!(&rec[8..11], b"ab\0");
    }
}
