//! General-purpose algorithms and value-conversion helpers.
//! The module contains checksum and bit operations, byte-order-aware numeric
//! conversion, display formatting, progress data, and lightweight timing and
//! stream helpers. It deliberately contains no platform or device I/O.

use std::fmt;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Adler32Computer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Adler32Computer {
    a: i32,
    b: i32,
}

impl Adler32Computer {
    pub fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    pub fn checksum(&self) -> i32 {
        self.b * 65536 + self.a
    }

    pub fn update(&mut self, data: &[u8], offset: usize, length: usize) {
        let end = length.saturating_sub(offset).min(data.len());
        for &byte in &data[offset.min(end)..end] {
            self.a = (self.a + i32::from(byte)) % 65521;
            self.b = (self.b + self.a) % 65521;
        }
    }
}

// ---------------------------------------------------------------------------
// BitOperation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitOperation {
    pub bit_mask: u64,
    pub shift_count: i32,
    pub sign_extend: bool,
}

impl BitOperation {
    pub fn new(bitmask: u64) -> Self {
        Self {
            bit_mask: bitmask,
            shift_count: get_shift_count(bitmask),
            sign_extend: false,
        }
    }
}

impl Default for BitOperation {
    fn default() -> Self {
        Self::new(u64::MAX)
    }
}

// ---------------------------------------------------------------------------
// BYTEORDER_TYPE
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ByteOrderType {
    #[default]
    NotSet = 0,
    MsbFirst = 1,
    MsbLast = 2,
}

impl ByteOrderType {
    pub const BIG_ENDIAN: Self = Self::MsbFirst;
    pub const LITTLE_ENDIAN: Self = Self::MsbLast;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub mod constants {
    pub const CAT_DATA: &str = "Data";
    pub const CAT_DATA_XCP_PLUS: &str = "Data.XCPpLus";
    pub const CAT_DATA_LIMIT: &str = "Data.Limit";
    pub const CAT_DATA_LINK: &str = "Data.Link";
    pub const CAT_NAVIGATION: &str = "Navigation";
    pub const DESC_TAG_PROPERTY: &str =
        "This property may be used to bind any user reference to this instance.";
    pub const SHORT_NAME: &str = "SHORT-NAME";
    pub const LONG_NAME: &str = "LONG-NAME";
    pub const DESC: &str = "DESC";
    pub const FILE_CACHE: usize = 1_048_576;
    pub const MAX_POINTS_IN_MEMORY: usize = 100_000;
    pub const MAX_FRAMES_IN_MEMORY: usize = 100_000;
    pub const MAX_UNCOMPRESSED_ZIP: usize = 4_194_304;
    pub const ZLIB_HEADER: [u8; 2] = [120, 1];
    pub const CRLF_DELIMITER: [char; 2] = ['\r', '\n'];
    pub const WS_DELIMITER: [char; 2] = [' ', '\t'];
}

// ---------------------------------------------------------------------------
// DATA_TYPE
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DataType {
    #[default]
    UByte = 0,
    SByte = 1,
    UWord = 2,
    SWord = 3,
    ULong = 4,
    SLong = 5,
    Float32Ieee = 6,
    AUInt64 = 7,
    AInt64 = 8,
    Float64Ieee = 9,
    Float16Ieee = 10,
    Bitfield = 11,
}

impl DataType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::UByte => "UBYTE",
            Self::SByte => "SBYTE",
            Self::UWord => "UWORD",
            Self::SWord => "SWORD",
            Self::ULong => "ULONG",
            Self::SLong => "SLONG",
            Self::Float32Ieee => "FLOAT32_IEEE",
            Self::AUInt64 => "A_UINT64",
            Self::AInt64 => "A_INT64",
            Self::Float64Ieee => "FLOAT64_IEEE",
            Self::Float16Ieee => "FLOAT16_IEEE",
            Self::Bitfield => "BITFIELD",
        }
    }

    pub const fn size_in_byte(self) -> i32 {
        match self {
            Self::UByte | Self::SByte => 1,
            Self::UWord | Self::SWord | Self::Float16Ieee => 2,
            Self::ULong | Self::SLong | Self::Float32Ieee => 4,
            Self::AUInt64 | Self::AInt64 | Self::Float64Ieee | Self::Bitfield => 8,
        }
    }

    pub const fn size_in_bit(self) -> i32 {
        self.size_in_byte() * 8
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

// ---------------------------------------------------------------------------
// DataLimits / DataPoint / DataPointF / sRange
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DataLimits {
    pub min: f64,
    pub max: f64,
}

impl DataLimits {
    pub fn new(min: f64, max: f64) -> Self {
        if min == max {
            Self {
                min: min - 1.0,
                max: max + 1.0,
            }
        } else {
            Self { min, max }
        }
    }

    pub fn absolute_diff(&self) -> f64 {
        (self.max - self.min).abs()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DataPoint {
    pub x: f64,
    pub y: f64,
}

impl DataPoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl fmt::Display for DataPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}/{}]", self.x, self.y)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DataPointF {
    pub x: f32,
    pub y: f32,
}

impl DataPointF {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl fmt::Display for DataPointF {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}/{}]", self.x, self.y)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SRange {
    pub min: f64,
    pub max: f64,
}

impl SRange {
    pub const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }
}

impl fmt::Display for SRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Range: {}..{:.2}", self.min, self.max)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub const IDENTIFIER_REGEX: &str = "[A-Z_]+[A-Z0-9_]*(\\[\\d+\\])*";

pub fn is_valid_designator(name: &str) -> bool {
    fn segment_valid(seg: &str) -> bool {
        let chars: Vec<char> = seg.chars().collect();
        let n = chars.len();
        let mut i = 0;
        // [A-Z_]+
        let start = i;
        while i < n && (chars[i].is_ascii_alphabetic() || chars[i] == '_') {
            i += 1;
        }
        if i == start {
            return false;
        }
        // [A-Z0-9_]*
        while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        // (\[\d+\])*
        while i < n && chars[i] == '[' {
            i += 1;
            let dstart = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i == dstart || i >= n || chars[i] != ']' {
                return false;
            }
            i += 1;
        }
        i == n
    }
    !name.is_empty() && name.split('.').all(segment_valid)
}

pub const fn change_endianness_u16(value: u16) -> u16 {
    value.swap_bytes()
}

pub const fn change_endianness_u32(value: u32) -> u32 {
    value.swap_bytes()
}

pub const fn change_endianness_u64(value: u64) -> u64 {
    value.swap_bytes()
}

pub const fn change_endianness_f32(value: f32) -> f32 {
    f32::from_bits(value.to_bits().swap_bytes())
}

pub const fn change_endianness_f64(value: f64) -> f64 {
    f64::from_bits(value.to_bits().swap_bytes())
}

pub fn change_endianness_bytes(data: &[u8], offset: usize, len: usize) -> Result<Vec<u8>> {
    let at = |n: usize| -> Result<&[u8]> {
        data.get(offset..offset + n)
            .ok_or_else(|| Error::General("change_endianness_bytes: buffer out of bounds".into()))
    };
    match len {
        4 => {
            let v = u32::from_le_bytes(
                at(4)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            Ok(v.swap_bytes().to_le_bytes().to_vec())
        }
        8 => {
            let v = u64::from_le_bytes(
                at(8)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            Ok(v.swap_bytes().to_le_bytes().to_vec())
        }
        _ => Err(Error::General(format!(
            "change_endianness_bytes: unsupported length {len}"
        ))),
    }
}

pub const fn align_up(address: i32, data_size: i32) -> i32 {
    (address + (data_size - 1)) & !(data_size - 1)
}

pub const fn align_down(address: i32, data_size: i32) -> i32 {
    address & !(data_size - 1)
}

pub const fn get_byte_len_from_bit_len(bit_length: i64) -> i64 {
    bit_length / 8 + if bit_length % 8 > 0 { 1 } else { 0 }
}

pub fn from_hex_str(s: &str, offset: &mut usize, len: usize) -> i64 {
    let bytes = s.as_bytes();
    let end = *offset + if len == 0 { s.len() - *offset } else { len };
    let mut num: i64 = 0;
    while *offset < end {
        num <<= 4;
        let c = bytes[*offset];
        *offset += 1;
        num += if c < b'A' {
            i64::from(c) - 48
        } else {
            i64::from(c & 0x5F) - 65 + 10
        };
    }
    num
}

pub fn to_byte(s: &str, offset: usize) -> u8 {
    let mut off = offset;
    from_hex_str(s, &mut off, 2) as u8
}

pub fn to_hex_str(s: &mut String, value: i64, len: usize) {
    let mut digits = Vec::with_capacity(len);
    let mut v = value as u64;
    for _ in 0..len {
        let nib = v & 0xF;
        digits.push(char::from(if nib < 10 {
            48 + nib as u8
        } else {
            65 + (nib as u8 - 10)
        }));
        v >>= 4;
    }
    for c in digits.iter().rev() {
        s.push(*c);
    }
}

fn double_to_raw_bits(raw: f64, dt: DataType) -> Result<(u64, i32)> {
    let t = raw as i64;
    Ok(match dt {
        DataType::UByte | DataType::SByte => (u64::from(t as u8), 8),
        DataType::UWord | DataType::SWord => (u64::from(t as u16), 16),
        DataType::ULong | DataType::SLong => (u64::from(t as u32), 32),
        DataType::AUInt64 | DataType::AInt64 | DataType::Bitfield => (t as u64, 64),
        DataType::Float32Ieee => (u64::from((raw as f32).to_bits()), 32),
        DataType::Float64Ieee => (raw.to_bits(), 64),
        DataType::Float16Ieee => {
            return Err(Error::General(format!("unsupported DATA_TYPE: {dt}")));
        }
    })
}

pub fn to_decimal_string_f64(raw_value: f64, data_type: DataType) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    Ok(double_to_raw_bits(raw_value, data_type)?.0.to_string())
}

pub fn to_decimal_string_u64(raw_value: u64) -> String {
    raw_value.to_string()
}

/// `0`→`0x0`).
pub fn to_hex_string_u64(raw_value: u64, remove_leading_zeros: bool) -> String {
    let mut s = format!("0x{raw_value:X}");
    if remove_leading_zeros {
        while s.len() > 3 && s.as_bytes()[2] == b'0' {
            s.remove(2);
        }
    }
    s
}

pub fn to_hex_string_f64(
    raw_value: f64,
    data_type: DataType,
    remove_leading_zeros: bool,
) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    Ok(to_hex_string_u64(
        double_to_raw_bits(raw_value, data_type)?.0,
        remove_leading_zeros,
    ))
}

pub fn to_binary_string_u64(
    raw_value: u64,
    data_type: DataType,
    remove_leading_zeros: bool,
) -> String {
    let size_in_bit = data_type.size_in_bit();
    let mut s = String::with_capacity(size_in_bit as usize);
    let mut bit = 1u64;
    let mut digits = Vec::with_capacity(size_in_bit as usize);
    for _ in 0..size_in_bit {
        digits.push(if bit & raw_value != 0 { '1' } else { '0' });
        bit <<= 1;
    }
    for c in digits.iter().rev() {
        s.push(*c);
    }
    if remove_leading_zeros {
        while s.len() > 1 && !s.starts_with('1') {
            s.remove(0);
        }
    }
    s
}

pub fn to_binary_string_f64(
    raw_value: f64,
    data_type: DataType,
    remove_leading_zeros: bool,
) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    Ok(to_binary_string_u64(
        double_to_raw_bits(raw_value, data_type)?.0,
        data_type,
        remove_leading_zeros,
    ))
}

fn has_hex_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && (b[1] & 0x5F) == b'X'
}

pub fn parse2_single_val(par: &str) -> Result<f32> {
    if has_hex_prefix(par) {
        let mut off = 2;
        return Ok(from_hex_str(par, &mut off, 0) as f32);
    }
    par.trim()
        .parse::<f32>()
        .map_err(|e| Error::General(format!("parse2_single_val({par:?}): {e}")))
}

pub fn parse2_double_val(par: &str, odx: bool) -> Result<f64> {
    if has_hex_prefix(par) {
        let mut off = 2;
        return Ok(from_hex_str(par, &mut off, 0) as f64);
    }
    if odx {
        if let Ok(v) = par.trim().parse::<f64>() {
            return Ok(v);
        }
        return i64::from_str_radix(par.trim(), 16)
            .map(|v| v as f64)
            .map_err(|e| Error::General(format!("parse2_double_val({par:?}, odx): {e}")));
    }
    par.trim()
        .parse::<f64>()
        .map_err(|e| Error::General(format!("parse2_double_val({par:?}): {e}")))
}

pub fn try_parse2_double_val(par: &str) -> Option<f64> {
    if has_hex_prefix(par) {
        return i64::from_str_radix(&par[2..], 16).ok().map(|v| v as f64);
    }
    par.trim().parse::<f64>().ok()
}

pub fn parse2_int_val(par: &str) -> Result<i64> {
    if has_hex_prefix(par) {
        let mut off = 2;
        return Ok(from_hex_str(par, &mut off, 0));
    }
    par.trim()
        .parse::<i64>()
        .map_err(|e| Error::General(format!("parse2_int_val({par:?}): {e}")))
}

pub fn try_parse2_int_val(par: &str) -> Option<i64> {
    if has_hex_prefix(par) {
        return i64::from_str_radix(&par[2..], 16).ok();
    }
    par.trim().parse::<i64>().ok()
}

pub const fn get_shift_count(mut bitmask: u64) -> i32 {
    let mut i = 0;
    while i < 64 {
        if bitmask & 1 != 0 {
            break;
        }
        bitmask >>= 1;
        i += 1;
    }
    i
}

pub fn build_bitmask(length: i64, left_shift: i32) -> Result<u64> {
    if length == 0 {
        return Err(Error::General(
            "build_bitmask: length must be greater than zero".into(),
        ));
    }
    let mut num = 1u64;
    let mut length = length;
    while length > 1 {
        num <<= 1;
        num |= 1;
        length -= 1;
    }
    if left_shift > 0 {
        num = num.wrapping_shl(left_shift as u32);
    }
    Ok(num)
}

pub fn get_trimmed_text(text: &str, max_columns: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut sb = String::new();
    let mut count = 0usize;
    for word in text.split(' ') {
        sb.push_str(word);
        count += word.chars().count();
        if !word.ends_with('\\') && count > max_columns {
            sb.push('\n');
            count = 0;
        } else {
            sb.push(' ');
        }
    }
    sb.trim().to_string()
}

pub fn build_html_table_line(args: &[&str], id: Option<&str>) -> String {
    let mut s = String::new();
    match id {
        Some(id) if !id.is_empty() => {
            s.push_str("<tr id='");
            s.push_str(id);
            s.push_str("'>");
        }
        _ => s.push_str("<tr>"),
    }
    for a in args {
        s.push_str("<td>");
        s.push_str(a);
        s.push_str("</td>");
    }
    s.push_str("</tr>");
    s
}

pub fn num_to_array(num: i64, size: usize) -> Vec<u8> {
    let mut array = vec![0u8; size];
    if cfg!(target_endian = "little") {
        for i in (0..size).rev() {
            array[size - 1 - i] = num.checked_shr((i * 8) as u32).unwrap_or(0) as u8;
        }
    } else {
        for (i, b) in array.iter_mut().enumerate() {
            *b = num.checked_shr((i * 8) as u32).unwrap_or(0) as u8;
        }
    }
    array
}

pub fn array_to_num(data: &[u8], offset: &mut usize, size: usize) -> i64 {
    let mut num = 0u64;
    let mut n2 = size as i64 - 1;
    while n2 >= 0 {
        n2 -= 1;
        num |= u64::from(data[*offset]) << ((n2 + 1) * 8);
        *offset += 1;
    }
    num as i64
}

fn apply_bit_operation(value: i64, dt: DataType, bits: &BitOperation) -> Result<i64> {
    if bits.bit_mask == u64::MAX {
        return Ok(value);
    }
    let mut num = match dt {
        DataType::UByte | DataType::SByte => {
            value & (-256 + i64::from(value as u8 & bits.bit_mask as u8))
        }
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => {
            value & (-65536 + i64::from(value as u16 & bits.bit_mask as u16))
        }
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => {
            value & (-4_294_967_296 + i64::from((value as i32 & bits.bit_mask as i32) as u32))
        }
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => {
            value & bits.bit_mask as i64
        }
        DataType::Bitfield => {
            return Err(Error::General(format!("unsupported DATA_TYPE: {dt}")));
        }
    };
    if bits.shift_count > 0 {
        num >>= bits.shift_count;
    } else if bits.shift_count < 0 {
        num <<= -bits.shift_count;
    }
    Ok(num)
}

pub fn set_bitmask(i64_value: i64, dt: DataType, shiftdown: bool, bm: u64) -> Result<i64> {
    let mut bits = BitOperation::new(u64::MAX);
    bits.bit_mask = bm;
    bits.shift_count = if shiftdown && bm != u64::MAX {
        get_shift_count(bm)
    } else {
        0
    };
    apply_bit_operation(i64_value, dt, &bits)
}

pub fn get_single_raw_value(
    buffer: &[u8],
    offset: usize,
    dt: DataType,
    bo: ByteOrderType,
    bits: Option<&BitOperation>,
    bitfield_len: usize,
) -> Result<f64> {
    let swap = cfg!(target_endian = "little") && bo == ByteOrderType::MsbFirst;
    let dt = if dt == DataType::Bitfield {
        match bitfield_len {
            1..=8 => DataType::UByte,
            9..=16 => DataType::UWord,
            17..=32 => DataType::ULong,
            33..=64 => DataType::AUInt64,
            _ => {
                return Err(Error::General(format!(
                    "get_single_raw_value: invalid bitfieldLen {bitfield_len}"
                )));
            }
        }
    } else {
        dt
    };
    let at = |n: usize| -> Result<&[u8]> {
        buffer
            .get(offset..offset + n)
            .ok_or_else(|| Error::General("get_single_raw_value: buffer out of bounds".into()))
    };
    let num: i64 = match dt {
        DataType::UByte => i64::from(at(1)?[0]),
        DataType::SByte => i64::from(at(1)?[0] as i8),
        DataType::UWord => {
            let v = u16::from_le_bytes(
                at(2)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            i64::from(if swap { v.swap_bytes() } else { v })
        }
        DataType::SWord => {
            let v = u16::from_le_bytes(
                at(2)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            i64::from((if swap { v.swap_bytes() } else { v }) as i16)
        }
        DataType::ULong => {
            let v = u32::from_le_bytes(
                at(4)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            i64::from(if swap { v.swap_bytes() } else { v })
        }
        DataType::SLong => {
            let v = u32::from_le_bytes(
                at(4)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            i64::from((if swap { v.swap_bytes() } else { v }) as i32)
        }
        DataType::AUInt64 | DataType::AInt64 => {
            let v = u64::from_le_bytes(
                at(8)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            (if swap { v.swap_bytes() } else { v }) as i64
        }
        DataType::Float32Ieee => {
            let v = u32::from_le_bytes(
                at(4)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            return Ok(f64::from(f32::from_bits(if swap {
                v.swap_bytes()
            } else {
                v
            })));
        }
        DataType::Float64Ieee => {
            let v = u64::from_le_bytes(
                at(8)?
                    .try_into()
                    .map_err(|_| Error::General("invalid slice length".into()))?,
            );
            return Ok(f64::from_bits(if swap { v.swap_bytes() } else { v }));
        }
        DataType::Float16Ieee | DataType::Bitfield => {
            return Err(Error::General(format!("unsupported DATA_TYPE: {dt}")));
        }
    };
    match bits {
        None => Ok(num as f64),
        Some(b) => Ok(apply_bit_operation(num, dt, b)? as f64),
    }
}

pub fn get_single_raw_value_buffer(
    raw_value: f64,
    dt: DataType,
    bo: ByteOrderType,
    bitfield_len: usize,
) -> Result<Vec<u8>> {
    let dt = if dt == DataType::Bitfield {
        match bitfield_len {
            1..=8 => DataType::UByte,
            9..=16 => DataType::UWord,
            17..=32 => DataType::ULong,
            33..=64 => DataType::AUInt64,
            _ => {
                return Err(Error::General(format!(
                    "get_single_raw_value_buffer: invalid bitfieldLen {bitfield_len}"
                )));
            }
        }
    } else {
        dt
    };
    let t = raw_value as i64;
    let mut array = match dt {
        DataType::UByte | DataType::SByte => vec![t as u8],
        DataType::UWord => (t as u16).to_le_bytes().to_vec(),
        DataType::SWord => (t as i16).to_le_bytes().to_vec(),
        DataType::ULong => (t as u32).to_le_bytes().to_vec(),
        DataType::SLong => (t as i32).to_le_bytes().to_vec(),
        DataType::AUInt64 => (t as u64).to_le_bytes().to_vec(),
        DataType::AInt64 => t.to_le_bytes().to_vec(),
        DataType::Float32Ieee => (raw_value as f32).to_bits().to_le_bytes().to_vec(),
        DataType::Float64Ieee => raw_value.to_bits().to_le_bytes().to_vec(),
        DataType::Float16Ieee | DataType::Bitfield => {
            return Err(Error::General(format!("unsupported DATA_TYPE: {dt}")));
        }
    };
    if array.len() > 1
        && ((cfg!(target_endian = "little") && bo == ByteOrderType::MsbFirst)
            || (cfg!(target_endian = "big") && bo == ByteOrderType::MsbLast))
    {
        array.reverse();
    }
    Ok(array)
}

pub const fn reverse(b: u8) -> u8 {
    b.reverse_bits()
}

pub fn to_u16(value: &[u8], idx: usize) -> u16 {
    u16::from_ne_bytes([value[idx], value[idx + 1]])
}

pub fn write_u16(value: u16, buffer: &mut [u8], idx: usize) {
    buffer[idx..idx + 2].copy_from_slice(&value.to_ne_bytes());
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub fn strip_xml_namespaces(xml: &str) -> String {
    fn strip_prefix(name: &str) -> &str {
        match name.rfind(':') {
            Some(i) => &name[i + 1..],
            None => name,
        }
    }
    let bytes = xml.as_bytes();
    let mut out = String::with_capacity(xml.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            out.push_str(&xml[start..i]);
            continue;
        }
        if xml[i..].starts_with("<!--") {
            let end = xml[i..].find("-->").map(|p| i + p + 3).unwrap_or(xml.len());
            out.push_str(&xml[i..end]);
            i = end;
            continue;
        }
        if xml[i..].starts_with("<![CDATA[") {
            let end = xml[i..].find("]]>").map(|p| i + p + 3).unwrap_or(xml.len());
            out.push_str(&xml[i..end]);
            i = end;
            continue;
        }
        if xml[i..].starts_with("<?") {
            let end = xml[i..].find("?>").map(|p| i + p + 2).unwrap_or(xml.len());
            out.push_str(&xml[i..end]);
            i = end;
            continue;
        }
        if xml[i..].starts_with("<!") {
            let end = xml[i..].find('>').map(|p| i + p + 1).unwrap_or(xml.len());
            out.push_str(&xml[i..end]);
            i = end;
            continue;
        }
        i += 1;
        if i < bytes.len() && bytes[i] == b'/' {
            out.push_str("</");
            i += 1;
        } else {
            out.push('<');
        }
        let nstart = i;
        while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') {
            i += 1;
        }
        out.push_str(strip_prefix(&xml[nstart..i]));
        let mut pending_ws = String::new();
        while i < bytes.len() && bytes[i] != b'>' {
            if bytes[i] == b'/' {
                out.push_str(&pending_ws);
                pending_ws.clear();
                out.push('/');
                i += 1;
                continue;
            }
            if bytes[i].is_ascii_whitespace() {
                pending_ws.push(bytes[i] as char);
                i += 1;
                continue;
            }
            let astart = i;
            while i < bytes.len()
                && !matches!(bytes[i], b'=' | b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
            {
                i += 1;
            }
            let aname = &xml[astart..i];
            let mut value: Option<String> = None;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'=' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                    let q = bytes[i];
                    let vstart = i + 1;
                    let vend = xml[vstart..]
                        .find(q as char)
                        .map(|p| vstart + p)
                        .unwrap_or(xml.len());
                    value = Some(xml[vstart..vend].to_string());
                    i = (vend + 1).min(xml.len());
                }
            }
            if aname == "xmlns" || aname.starts_with("xmlns:") {
                pending_ws.clear();
                continue;
            }
            out.push_str(&pending_ws);
            pending_ws.clear();
            out.push_str(strip_prefix(aname));
            if let Some(v) = value {
                out.push_str("=\"");
                out.push_str(&v);
                out.push('"');
            }
        }
        pending_ws.clear();
        if i < bytes.len() && bytes[i] == b'>' {
            out.push('>');
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// InvariantStreamWriter
// ---------------------------------------------------------------------------

pub struct InvariantStreamWriter {
    inner: BufWriter<std::fs::File>,
}

impl InvariantStreamWriter {
    pub fn new(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            inner: BufWriter::with_capacity(constants::FILE_CACHE, file),
        })
    }

    pub fn write_line(&mut self, s: &str) -> Result<()> {
        self.inner.write_all(s.as_bytes())?;
        self.inner.write_all(b"\n")?;
        Ok(())
    }
}

impl Write for InvariantStreamWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// MemoryRange / MemoryRangeList
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRange {
    pub start: u32,
    pub next: u32,
}

impl MemoryRange {
    pub const fn new(start: u32, next: u32) -> Self {
        Self { start, next }
    }

    pub const fn size(&self) -> i32 {
        (self.next - self.start) as i32
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryRangeList {
    ranges: Vec<MemoryRange>,
    reduced: bool,
}

impl MemoryRangeList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, range: MemoryRange) {
        self.ranges.push(range);
        self.reduced = false;
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn size(&self) -> i32 {
        self.ranges.iter().map(MemoryRange::size).sum()
    }

    pub fn ranges(&self) -> &[MemoryRange] {
        &self.ranges
    }

    pub fn reduce(&mut self, min_gap_size: u32, max_block_size: u32) {
        if self.reduced {
            return;
        }
        self.ranges.sort_by_key(|r| r.start);
        let mut i = self.ranges.len();
        while i > 1 {
            i -= 1;
            let (prev, cur) = (self.ranges[i - 1], self.ranges[i]);
            if prev.next.saturating_add(min_gap_size) >= cur.start {
                self.ranges[i - 1] =
                    MemoryRange::new(prev.start.min(cur.start), prev.next.max(cur.next));
                self.ranges.remove(i);
            }
        }
        self.reduced = true;
        if max_block_size == 0 {
            return;
        }
        let mut i = 0;
        while i < self.ranges.len() {
            let r = self.ranges[i];
            if r.size() > max_block_size as i32 {
                let n2 = r.start + max_block_size;
                let size = r.size();
                self.ranges[i].next = n2;
                self.ranges.insert(
                    i + 1,
                    MemoryRange::new(n2, n2 + (size - max_block_size as i32) as u32),
                );
            }
            i += 1;
        }
    }

    pub fn find_range_index(&mut self, address: u32, size: u32) -> i32 {
        self.reduce(0, 0);
        if self.ranges.is_empty() || address < self.ranges[0].start {
            return -1;
        }
        if address >= self.ranges[self.ranges.len() - 1].next {
            return -1;
        }
        let (mut lo, mut hi) = (0usize, self.ranges.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.ranges[mid].start <= address {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let idx = lo.saturating_sub(1);
        if u64::from(self.ranges[idx].next) >= u64::from(address) + u64::from(size) {
            idx as i32
        } else {
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// MessageType / ParserEventArgs / ProgressArgs / ValueObjectFormat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MessageType {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ParserEvent {
    pub timestamp: std::time::SystemTime,
    pub source: Option<String>,
    pub msg_type: MessageType,
    pub message: String,
}

impl ParserEvent {
    pub fn new(source: Option<String>, msg_type: MessageType, message: impl Into<String>) -> Self {
        Self {
            timestamp: std::time::SystemTime::now(),
            source,
            msg_type,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressArgs {
    pub percent: i32,
    pub cancel: bool,
}

impl ProgressArgs {
    pub const fn new(percent: i32) -> Self {
        Self {
            percent,
            cancel: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ValueObjectFormat {
    #[default]
    Physical,
    Raw,
    RawHex,
    RawBin,
}

// ---------------------------------------------------------------------------
// TcpSocketWithTimeout
// ---------------------------------------------------------------------------

pub struct TcpSocketWithTimeout;

impl TcpSocketWithTimeout {
    pub fn connect(
        addr: &std::net::SocketAddr,
        timeout: Duration,
        ttl: u32,
    ) -> std::io::Result<std::net::TcpStream> {
        let stream = std::net::TcpStream::connect_timeout(addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_ttl(ttl)?;
        Ok(stream)
    }

    pub fn connect_default(addr: &std::net::SocketAddr) -> std::io::Result<std::net::TcpStream> {
        Self::connect(addr, Duration::from_millis(200), 32)
    }
}

// ---------------------------------------------------------------------------
// TimeBase
// ---------------------------------------------------------------------------

pub struct TimeBase;

struct DaqState {
    daq_last_timestamp: f64,
    stopped: f64,
    overridden: f64,
    reset_elapsed: Duration,
}

impl TimeBase {
    fn start() -> &'static Instant {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now)
    }

    fn daq() -> &'static Mutex<DaqState> {
        static DAQ: OnceLock<Mutex<DaqState>> = OnceLock::new();
        DAQ.get_or_init(|| {
            Mutex::new(DaqState {
                daq_last_timestamp: 0.0,
                stopped: f64::NAN,
                overridden: f64::NAN,
                reset_elapsed: Duration::ZERO,
            })
        })
    }

    pub fn elapsed() -> Duration {
        Self::start().elapsed()
    }

    pub fn elapsed_nanos() -> u128 {
        Self::start().elapsed().as_nanos()
    }

    pub fn set_daq_last_timestamp(value: f64) {
        let mut s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        s.daq_last_timestamp = value.max(s.daq_last_timestamp);
    }

    pub fn elapsed_daq_seconds() -> f64 {
        let s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        ((Self::elapsed() - s.reset_elapsed).as_secs_f64()).max(s.daq_last_timestamp)
    }

    pub fn reset_daq_time() {
        let mut s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        if !s.stopped.is_nan() {
            s.stopped = f64::NAN;
            s.overridden = f64::NAN;
            s.reset_elapsed = Self::elapsed();
            s.daq_last_timestamp = 0.0;
        }
    }

    pub fn stop_daq_time() {
        let mut s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        if s.stopped.is_nan() {
            s.stopped = s.daq_last_timestamp;
            s.overridden = s.daq_last_timestamp;
        }
    }

    pub fn get_daq_time() -> f64 {
        let s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        if s.overridden.is_nan() {
            if s.stopped.is_nan() {
                s.daq_last_timestamp
            } else {
                s.stopped
            }
        } else {
            s.overridden
        }
    }

    pub fn set_current_daq_time_from_seconds(time: f64) {
        let mut s = Self::daq().lock().unwrap_or_else(|e| e.into_inner());
        s.overridden = time;
    }

    pub fn block_for_micro_secs(micro_secs: u64, cancel: Option<&AtomicBool>) {
        let deadline = Self::elapsed() + Duration::from_micros(micro_secs);
        loop {
            if Self::elapsed() >= deadline {
                break;
            }
            if let Some(c) = cancel {
                if c.load(AtomicOrdering::Relaxed) {
                    break;
                }
            }
            std::hint::spin_loop();
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler32_golden() {
        let mut c = Adler32Computer::new();
        c.update(b"Wikipedia", 0, 9);
        assert_eq!(c.checksum(), 0x11E6_0398);
    }

    #[test]
    fn enums_reflected_values() {
        assert_eq!(MessageType::Info as i32, 0);
        assert_eq!(ValueObjectFormat::RawBin as i32, 3);
    }

    #[test]
    fn adler32_offset_quirk() {
        let mut c = Adler32Computer::new();
        c.update(b"Wikipedia", 2, 9);
        let mut expect_a = 1i32;
        let mut expect_b = 0i32;
        for &b in &b"Wikipedia"[2..7] {
            expect_a = (expect_a + i32::from(b)) % 65521;
            expect_b = (expect_b + expect_a) % 65521;
        }
        assert_eq!(c.checksum(), expect_b * 65536 + expect_a);
    }

    #[test]
    fn bit_operation_new() {
        let b = BitOperation::new(0xF0);
        assert_eq!(b.bit_mask, 0xF0);
        assert_eq!(b.shift_count, 4);
        assert!(!b.sign_extend);
        assert_eq!(BitOperation::default().bit_mask, u64::MAX);
    }

    #[test]
    fn byte_order_type_values() {
        assert_eq!(ByteOrderType::NotSet as u8, 0);
        assert_eq!(ByteOrderType::MsbFirst as u8, 1);
        assert_eq!(ByteOrderType::MsbLast as u8, 2);
        assert_eq!(ByteOrderType::BIG_ENDIAN, ByteOrderType::MsbFirst);
        assert_eq!(ByteOrderType::LITTLE_ENDIAN, ByteOrderType::MsbLast);
    }

    #[test]
    fn data_type_values_match_dll() {
        assert_eq!(DataType::UByte.as_u8(), 0);
        assert_eq!(DataType::SByte.as_u8(), 1);
        assert_eq!(DataType::UWord.as_u8(), 2);
        assert_eq!(DataType::SWord.as_u8(), 3);
        assert_eq!(DataType::ULong.as_u8(), 4);
        assert_eq!(DataType::SLong.as_u8(), 5);
        assert_eq!(DataType::Float32Ieee.as_u8(), 6);
        assert_eq!(DataType::AUInt64.as_u8(), 7);
        assert_eq!(DataType::AInt64.as_u8(), 8);
        assert_eq!(DataType::Float64Ieee.as_u8(), 9);
        assert_eq!(DataType::Float16Ieee.as_u8(), 10);
        assert_eq!(DataType::Bitfield.as_u8(), 11);
    }

    #[test]
    fn data_type_sizes() {
        assert_eq!(DataType::UByte.size_in_byte(), 1);
        assert_eq!(DataType::SByte.size_in_bit(), 8);
        assert_eq!(DataType::UWord.size_in_byte(), 2);
        assert_eq!(DataType::Float16Ieee.size_in_byte(), 2);
        assert_eq!(DataType::ULong.size_in_byte(), 4);
        assert_eq!(DataType::Float32Ieee.size_in_bit(), 32);
        assert_eq!(DataType::AUInt64.size_in_byte(), 8);
        assert_eq!(DataType::Float64Ieee.size_in_byte(), 8);
        assert_eq!(DataType::Bitfield.size_in_byte(), 8);
    }

    #[test]
    fn data_limits_behavior() {
        let d = DataLimits::new(2.0, 5.0);
        assert_eq!((d.min, d.max), (2.0, 5.0));
        assert_eq!(d.absolute_diff(), 3.0);
        let eq = DataLimits::new(3.0, 3.0);
        assert_eq!((eq.min, eq.max), (2.0, 4.0));
        assert_eq!(eq.absolute_diff(), 2.0);
    }

    #[test]
    fn data_point_display_golden() {
        assert_eq!(DataPoint::new(1.5, 2.5).to_string(), "[1.5/2.5]");
        assert_eq!(DataPointF::new(1.5, 2.5).to_string(), "[1.5/2.5]");
    }

    #[test]
    fn s_range_display_golden() {
        assert_eq!(SRange::new(1.25, 3.5).to_string(), "Range: 1.25..3.50");
    }

    #[test]
    fn is_valid_designator_golden() {
        assert!(is_valid_designator("Abc.Def_1[2]"));
        assert!(!is_valid_designator("1Abc"));
        assert!(!is_valid_designator("A..B"));
        assert!(!is_valid_designator(""));
        assert!(is_valid_designator("_X9[10][2]"));
        assert!(!is_valid_designator("A[]"));
    }

    #[test]
    fn change_endianness_golden() {
        assert_eq!(
            change_endianness_u64(0x1122_3344_5566_7788),
            0x8877_6655_4433_2211
        );
        assert_eq!(change_endianness_u32(0x1122_3344), 0x4433_2211);
        assert_eq!(change_endianness_u16(0x1122), 0x2211);
        assert_eq!(
            change_endianness_f64(1.5),
            f64::from_bits(1.5f64.to_bits().swap_bytes())
        );
        assert_eq!(
            change_endianness_bytes(&[0x44, 0x33, 0x22, 0x11], 0, 4).unwrap(),
            vec![0x11, 0x22, 0x33, 0x44]
        );
        assert!(change_endianness_bytes(&[0; 8], 0, 3).is_err());
    }

    #[test]
    fn align_golden() {
        assert_eq!(align_up(13, 4), 16);
        assert_eq!(align_down(13, 4), 12);
        assert_eq!(align_up(16, 4), 16);
    }

    #[test]
    fn bit_len_golden() {
        assert_eq!(get_byte_len_from_bit_len(9), 2);
        assert_eq!(get_byte_len_from_bit_len(8), 1);
        assert_eq!(get_byte_len_from_bit_len(0), 0);
    }

    #[test]
    fn hex_str_roundtrip() {
        let mut off = 0;
        assert_eq!(from_hex_str("FF", &mut off, 0), 255);
        assert_eq!(off, 2);
        let mut off = 2;
        assert_eq!(from_hex_str("0x1A", &mut off, 0), 26);
        assert_eq!(to_byte("AB", 0), 0xAB);
        let mut s = String::from("0x");
        to_hex_str(&mut s, 0xAB, 2);
        assert_eq!(s, "0xAB");
        to_hex_str(&mut s, 0x5, 4);
        assert_eq!(s, "0xAB0005");
    }

    #[test]
    fn decimal_string_golden() {
        assert_eq!(
            to_decimal_string_f64(f64::NAN, DataType::UByte).unwrap(),
            "NaN"
        );
        assert_eq!(
            to_decimal_string_f64(255.9, DataType::UByte).unwrap(),
            "255"
        );
        assert_eq!(to_decimal_string_f64(-1.0, DataType::SByte).unwrap(), "255");
        assert_eq!(
            to_decimal_string_f64(1.5, DataType::Float32Ieee).unwrap(),
            "1069547520"
        );
        assert_eq!(to_decimal_string_u64(42), "42");
    }

    #[test]
    fn hex_string_golden() {
        assert_eq!(to_hex_string_u64(0x1234, false), "0x1234");
        assert_eq!(to_hex_string_u64(0x12, true), "0x12");
        assert_eq!(to_hex_string_u64(0x100, true), "0x100");
        assert_eq!(to_hex_string_u64(0, true), "0x0");
        assert_eq!(
            to_hex_string_f64(f64::NAN, DataType::UByte, true).unwrap(),
            "NaN"
        );
        assert_eq!(
            to_hex_string_f64(255.9, DataType::UWord, true).unwrap(),
            "0xFF"
        );
        assert_eq!(
            to_hex_string_f64(1.5, DataType::Float32Ieee, false).unwrap(),
            "0x3FC00000"
        );
    }

    #[test]
    fn binary_string_golden() {
        assert_eq!(
            to_binary_string_u64(5, DataType::UWord, false),
            "0000000000000101"
        );
        assert_eq!(to_binary_string_u64(5, DataType::UWord, true), "101");
        assert_eq!(to_binary_string_u64(0, DataType::UByte, true), "0");
        assert_eq!(
            to_binary_string_f64(f64::NAN, DataType::UByte, true).unwrap(),
            "NaN"
        );
    }

    #[test]
    fn parse_values_golden() {
        assert_eq!(parse2_double_val("0x1A", false).unwrap(), 26.0);
        assert_eq!(parse2_double_val("0X1a", false).unwrap(), 26.0);
        assert_eq!(parse2_double_val("1.5e2", false).unwrap(), 150.0);
        assert_eq!(parse2_double_val("1.5e2", true).unwrap(), 150.0);
        assert_eq!(parse2_double_val("1A", true).unwrap(), 26.0);
        assert!(parse2_double_val("ab", false).is_err());
        assert_eq!(parse2_int_val("0xFF").unwrap(), 255);
        assert_eq!(parse2_int_val("-42").unwrap(), -42);
        assert_eq!(try_parse2_double_val("0xFF"), Some(255.0));
        assert_eq!(try_parse2_double_val("12.5"), Some(12.5));
        assert_eq!(try_parse2_double_val("zz"), None);
        assert_eq!(try_parse2_int_val("0x10"), Some(16));
        assert_eq!(try_parse2_int_val("-7"), Some(-7));
        assert_eq!(try_parse2_int_val("x"), None);
        assert_eq!(parse2_single_val("0x10").unwrap(), 16.0);
        assert_eq!(parse2_single_val("2.5").unwrap(), 2.5);
    }

    #[test]
    fn shift_and_bitmask_golden() {
        assert_eq!(get_shift_count(0xF0), 4);
        assert_eq!(get_shift_count(0), 64);
        assert_eq!(build_bitmask(4, 0).unwrap(), 15);
        assert_eq!(build_bitmask(4, 4).unwrap(), 240);
        assert_eq!(build_bitmask(64, 0).unwrap(), u64::MAX);
        assert_eq!(build_bitmask(1, 0).unwrap(), 1);
        assert!(build_bitmask(0, 0).is_err());
    }

    #[test]
    fn set_bitmask_golden() {
        assert_eq!(set_bitmask(0xFF, DataType::UByte, true, 0xF0).unwrap(), 15);
        assert_eq!(
            set_bitmask(0xFF, DataType::UByte, false, 0xF0).unwrap(),
            240
        );
    }

    #[test]
    fn trimmed_text_golden() {
        assert_eq!(
            get_trimmed_text("hello world this is long", 10),
            "hello world this\nis long"
        );
        assert_eq!(get_trimmed_text("", 10), "");
    }

    #[test]
    fn html_table_line_golden() {
        assert_eq!(
            build_html_table_line(&["A", "B"], Some("row1")),
            "<tr id='row1'><td>A</td><td>B</td></tr>"
        );
        assert_eq!(build_html_table_line(&["A"], None), "<tr><td>A</td></tr>");
    }

    #[test]
    fn num_array_golden() {
        assert_eq!(num_to_array(0x1122_3344, 4), vec![0x11, 0x22, 0x33, 0x44]);
        assert_eq!(num_to_array(0x1122, 2), vec![0x11, 0x22]);
        let mut off = 0;
        assert_eq!(
            array_to_num(&[0x11, 0x22, 0x33, 0x44], &mut off, 4),
            0x1122_3344
        );
        assert_eq!(off, 4);
    }

    #[test]
    fn single_raw_value_golden() {
        let buf = [0x44, 0x33, 0x22, 0x11];
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::ULong, ByteOrderType::MsbFirst, None, 1)
                .unwrap(),
            1144201745.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::ULong, ByteOrderType::MsbLast, None, 1)
                .unwrap(),
            287454020.0
        );
        let sbuf = [0x01, 0x80];
        assert_eq!(
            get_single_raw_value(&sbuf, 0, DataType::SWord, ByteOrderType::MsbFirst, None, 1)
                .unwrap(),
            384.0
        );
        assert_eq!(
            get_single_raw_value(&sbuf, 0, DataType::SWord, ByteOrderType::MsbLast, None, 1)
                .unwrap(),
            -32767.0
        );
    }

    #[test]
    fn single_raw_value_bitfield() {
        let bits = BitOperation::new(0xF0);
        let buf = [0xAB];
        assert_eq!(
            get_single_raw_value(
                &buf,
                0,
                DataType::Bitfield,
                ByteOrderType::MsbLast,
                Some(&bits),
                8
            )
            .unwrap(),
            10.0
        );
        assert!(get_single_raw_value(
            &buf,
            0,
            DataType::Bitfield,
            ByteOrderType::MsbLast,
            None,
            65
        )
        .is_err());
    }

    #[test]
    fn single_raw_value_buffer_golden() {
        assert_eq!(
            get_single_raw_value_buffer(1.5, DataType::Float32Ieee, ByteOrderType::MsbFirst, 1)
                .unwrap(),
            vec![0x3F, 0xC0, 0x00, 0x00]
        );
        // 4386.0 == 0x1122
        assert_eq!(
            get_single_raw_value_buffer(4386.0, DataType::UWord, ByteOrderType::MsbFirst, 1)
                .unwrap(),
            vec![0x11, 0x22]
        );
        assert_eq!(
            get_single_raw_value_buffer(4386.0, DataType::UWord, ByteOrderType::MsbLast, 1)
                .unwrap(),
            vec![0x22, 0x11]
        );
    }

    #[test]
    fn reverse_and_u16_golden() {
        assert_eq!(reverse(0x96), 0x69);
        let mut buf = [0u8; 2];
        write_u16(0x1122, &mut buf, 0);
        assert_eq!(to_u16(&buf, 0), 0x1122);
    }

    #[test]
    fn strip_namespaces() {
        let xml = r#"<?xml version="1.0"?><a:root xmlns:a="urn:x" a:attr="1"><a:child>t</a:child><!-- c --></a:root>"#;
        assert_eq!(
            strip_xml_namespaces(xml),
            r#"<?xml version="1.0"?><root attr="1"><child>t</child><!-- c --></root>"#
        );
        assert_eq!(strip_xml_namespaces("<x>plain</x>"), "<x>plain</x>");
    }

    #[test]
    fn invariant_writer_writes_file() {
        let dir = std::env::temp_dir().join(format!("autors_util_isw_{}.txt", std::process::id()));
        {
            let mut w = InvariantStreamWriter::new(&dir).unwrap();
            w.write_line("1.5").unwrap();
            w.write_all(b"2.5\n").unwrap();
            w.flush().unwrap();
        }
        let content = std::fs::read_to_string(&dir).unwrap();
        assert_eq!(content, "1.5\n2.5\n");
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn memory_range_reduce_merge() {
        let mut l = MemoryRangeList::new();
        l.add(MemoryRange::new(0x100, 0x200));
        l.add(MemoryRange::new(0x180, 0x300));
        l.add(MemoryRange::new(0x400, 0x500));
        l.reduce(0, 0);
        assert_eq!(l.len(), 2);
        assert_eq!(l.ranges()[0], MemoryRange::new(0x100, 0x300));
        assert_eq!(l.size(), 0x200 + 0x100);
        let mut g = MemoryRangeList::new();
        g.add(MemoryRange::new(0x100, 0x200));
        g.add(MemoryRange::new(0x210, 0x300));
        g.reduce(0x10, 0);
        assert_eq!(g.len(), 1);
        assert_eq!(g.ranges()[0], MemoryRange::new(0x100, 0x300));
    }

    #[test]
    fn memory_range_reduce_split() {
        let mut l = MemoryRangeList::new();
        l.add(MemoryRange::new(0x1000, 0x1250)); // 0x250
        l.reduce(0, 0x100);
        assert_eq!(l.len(), 3);
        assert_eq!(l.ranges()[0], MemoryRange::new(0x1000, 0x1100));
        assert_eq!(l.ranges()[1], MemoryRange::new(0x1100, 0x1200));
        assert_eq!(l.ranges()[2], MemoryRange::new(0x1200, 0x1250));
    }

    #[test]
    fn memory_range_find() {
        let mut l = MemoryRangeList::new();
        l.add(MemoryRange::new(0x100, 0x200));
        l.add(MemoryRange::new(0x400, 0x500));
        assert_eq!(l.find_range_index(0x110, 0x20), 0);
        assert_eq!(l.find_range_index(0x1F0, 0x20), -1);
        assert_eq!(l.find_range_index(0x450, 0x10), 1);
        assert_eq!(l.find_range_index(0x50, 0x10), -1);
        assert_eq!(l.find_range_index(0x500, 0x10), -1);
        assert!(MemoryRangeList::new().find_range_index(0, 1) == -1);
    }

    #[test]
    fn parser_event_and_progress() {
        let e = ParserEvent::new(Some("A2LParser".into()), MessageType::Warning, "msg");
        assert_eq!(e.msg_type, MessageType::Warning);
        assert_eq!(e.message, "msg");
        assert_eq!(e.source.as_deref(), Some("A2LParser"));
        let p = ProgressArgs::new(50);
        assert_eq!(p.percent, 50);
        assert!(!p.cancel);
    }

    #[test]
    fn timebase_basics() {
        let e1 = TimeBase::elapsed();
        TimeBase::block_for_micro_secs(10, None);
        let e2 = TimeBase::elapsed();
        assert!(e2 >= e1);
        let flag = AtomicBool::new(true);
        TimeBase::block_for_micro_secs(60_000_000, Some(&flag));
        TimeBase::set_daq_last_timestamp(1.5);
        assert!(TimeBase::elapsed_daq_seconds() >= 1.5);
        TimeBase::set_daq_last_timestamp(0.5);
        assert_eq!(TimeBase::get_daq_time(), 1.5);
        TimeBase::stop_daq_time();
        assert_eq!(TimeBase::get_daq_time(), 1.5);
        TimeBase::reset_daq_time();
        TimeBase::set_current_daq_time_from_seconds(7.25);
        assert_eq!(TimeBase::get_daq_time(), 7.25);
    }

    #[test]
    fn tcp_socket_timeout_refused() {
        let addr: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(TcpSocketWithTimeout::connect(&addr, Duration::from_millis(100), 32).is_err());
    }

    #[test]
    fn tcp_socket_connect_ok() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let s = TcpSocketWithTimeout::connect(&addr, Duration::from_millis(500), 32).unwrap();
        assert!(s.nodelay().unwrap());
        assert_eq!(s.ttl().unwrap(), 32);
    }
}
