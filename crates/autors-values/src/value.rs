//! Runtime representations of calibration and measurement values.
//! The value family covers scalar, ASCII, block, curve, map, cuboid, and
//! higher-dimensional data. It applies A2L conversion rules, axis metadata,
//! limits, display formatting, and paste compatibility checks. Context needed
//! for scalar conversion is collected in [`SingleValueContext`].

use std::cmp::Ordering;
use std::fmt;

use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::characteristic::{AxisPts, Characteristic, CharacteristicChild};
use autors_a2l::model::compu::{CompuMethod, CompuTab, CompuVtab, CompuVtabRange, RationalCoeffs};
use autors_a2l::model::enums::{AxisValueType, CharacteristicType, ConversionType, DataType};
use autors_a2l::model::measurement::Measurement;

use crate::error::{Error, Result};

// ===========================================================================
//
// ===========================================================================

pub use autors_util::helpers::ValueObjectFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PasteResult {
    Ok,
    Warn,
    WarnDstLimitViolated,
    WarnDoesNotFit,
    Err,
    ErrSrcFormatNotAvailable,
    ErrNotSupportedByTargetValue,
    ErrSrcWrongValueFormat,
    ErrSrcNonQuadratic,
}

pub use autors_util::helpers::BitOperation;

fn apply_bit_operation(value: i64, dt: DataType, bits: &BitOperation) -> i64 {
    if bits.bit_mask == u64::MAX {
        return value;
    }
    let mut num = match dt {
        DataType::UByte | DataType::SByte => {
            value & (-256 + (value as u8 & bits.bit_mask as u8) as i64)
        }
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => {
            value & (-65536 + (value as u16 & bits.bit_mask as u16) as i64)
        }
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => {
            value & (-4294967296 + (value as u32 & bits.bit_mask as u32) as i64)
        }
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => {
            value & bits.bit_mask as i64
        }
        DataType::Unsupported => value,
    };
    if bits.shift_count > 0 {
        num >>= bits.shift_count;
    } else if bits.shift_count < 0 {
        num <<= -bits.shift_count;
    }
    num
}

pub fn size_in_byte(dt: DataType) -> Result<usize> {
    match dt {
        DataType::UByte | DataType::SByte => Ok(1),
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => Ok(2),
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => Ok(4),
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => Ok(8),
        DataType::Unsupported => Err(Error::Value(format!(
            "getSizeInByte: unsupported data type {dt:?}"
        ))),
    }
}

pub fn size_in_bit(dt: DataType) -> Result<usize> {
    Ok(size_in_byte(dt)? * 8)
}

fn needs_swap(bo: ByteOrder) -> bool {
    bo == ByteOrder::MSB_FIRST
}

pub fn get_single_raw_value(
    buffer: &[u8],
    offset: usize,
    dt: DataType,
    bo: ByteOrder,
    bits: Option<&BitOperation>,
) -> Result<f64> {
    let swap = needs_swap(bo);
    let take = |n: usize| -> Result<&[u8]> {
        buffer.get(offset..offset + n).ok_or_else(|| {
            Error::Value(format!(
                "getSingleRawValue: buffer too small (offset {offset}, need {n}, len {})",
                buffer.len()
            ))
        })
    };
    let num: i64 = match dt {
        DataType::UByte => take(1)?[0] as i64,
        DataType::SByte => take(1)?[0] as i8 as i64,
        DataType::UWord => {
            let mut b = [0u8; 2];
            b.copy_from_slice(take(2)?);
            let v = u16::from_le_bytes(b);
            (if swap { v.swap_bytes() } else { v }) as i64
        }
        DataType::SWord => {
            let mut b = [0u8; 2];
            b.copy_from_slice(take(2)?);
            let v = u16::from_le_bytes(b);
            (if swap { v.swap_bytes() } else { v }) as i16 as i64
        }
        DataType::ULong => {
            let mut b = [0u8; 4];
            b.copy_from_slice(take(4)?);
            let v = u32::from_le_bytes(b);
            (if swap { v.swap_bytes() } else { v }) as i64
        }
        DataType::SLong => {
            let mut b = [0u8; 4];
            b.copy_from_slice(take(4)?);
            let v = u32::from_le_bytes(b);
            (if swap { v.swap_bytes() } else { v }) as i32 as i64
        }
        DataType::AUInt64 | DataType::AInt64 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(take(8)?);
            let v = u64::from_le_bytes(b);
            (if swap { v.swap_bytes() } else { v }) as i64
        }
        DataType::Float32Ieee => {
            let mut b = [0u8; 4];
            b.copy_from_slice(take(4)?);
            let v = u32::from_le_bytes(b);
            return Ok(f32::from_bits(if swap { v.swap_bytes() } else { v }) as f64);
        }
        DataType::Float64Ieee => {
            let mut b = [0u8; 8];
            b.copy_from_slice(take(8)?);
            let v = u64::from_le_bytes(b);
            return Ok(f64::from_bits(if swap { v.swap_bytes() } else { v }));
        }
        DataType::Unsupported | DataType::Float16Ieee => {
            return Err(Error::Value(format!(
                "getSingleRawValue: unsupported data type {dt:?}"
            )));
        }
    };
    Ok(match bits {
        Some(b) => apply_bit_operation(num, dt, b) as f64,
        None => num as f64,
    })
}

fn double_to_bits(value: f64, dt: DataType) -> Result<u64> {
    let i = value as i64;
    Ok(match dt {
        DataType::UByte | DataType::SByte => (i as u8) as u64,
        DataType::UWord | DataType::SWord => (i as u16) as u64,
        DataType::ULong | DataType::SLong => (i as u32) as u64,
        DataType::AUInt64 | DataType::AInt64 => i as u64,
        DataType::Float32Ieee => (value as f32).to_bits() as u64,
        DataType::Float64Ieee => value.to_bits(),
        DataType::Unsupported | DataType::Float16Ieee => {
            return Err(Error::Value(format!(
                "raw value bits: unsupported data type {dt:?}"
            )));
        }
    })
}

pub fn get_single_raw_value_buffer(raw_value: f64, dt: DataType, bo: ByteOrder) -> Result<Vec<u8>> {
    let mut bytes = match dt {
        DataType::UByte | DataType::SByte => vec![(raw_value as i64) as u8],
        DataType::UWord => ((raw_value as i64) as u16).to_le_bytes().to_vec(),
        DataType::SWord => ((raw_value as i64) as i16).to_le_bytes().to_vec(),
        DataType::ULong => ((raw_value as i64) as u32).to_le_bytes().to_vec(),
        DataType::SLong => ((raw_value as i64) as i32).to_le_bytes().to_vec(),
        DataType::AUInt64 => ((raw_value as i64) as u64).to_le_bytes().to_vec(),
        DataType::AInt64 => (raw_value as i64).to_le_bytes().to_vec(),
        DataType::Float32Ieee => (raw_value as f32).to_bits().to_le_bytes().to_vec(),
        DataType::Float64Ieee => raw_value.to_bits().to_le_bytes().to_vec(),
        DataType::Unsupported | DataType::Float16Ieee => {
            return Err(Error::Value(format!(
                "getSingleRawValueBuffer: unsupported data type {dt:?}"
            )));
        }
    };
    if bytes.len() > 1 && needs_swap(bo) {
        bytes.reverse();
    }
    Ok(bytes)
}

// ===========================================================================
// ===========================================================================

pub fn round_bankers(value: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (value * m).round_ties_even() / m
}

/// `2.5 F0→"3"`, `0.1 F17→"0.10000000000000000"`, `1/3 F20→"0.33333333333333300000"`.
pub fn format_fixed(value: f64, decimals: i32) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-∞" } else { "∞" }.to_string();
    }
    let decimals = decimals.max(0) as usize;
    let sci = format!("{value:.14e}");
    let (mant, exp_str) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let mut exp: i32 = exp_str.parse().unwrap_or(0);
    let negative = mant.starts_with('-');
    let mut digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    debug_assert_eq!(digits.len(), 15);
    let keep = exp + decimals as i32 + 1;
    if keep <= 0 {
        if keep == 0 && digits[0] >= b'5' {
            digits.clear();
            digits.push(b'1');
            exp = -(decimals as i32);
        } else {
            return zero_fixed(decimals);
        }
    } else if (keep as usize) < digits.len() {
        let k = keep as usize;
        if digits[k] >= b'5' {
            let mut i = k;
            loop {
                if i == 0 {
                    digits.insert(0, b'1');
                    exp += 1;
                    break;
                }
                i -= 1;
                if digits[i] == b'9' {
                    digits[i] = b'0';
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
        digits.truncate(keep.max(0) as usize);
    }
    let point = exp + 1;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if point <= 0 {
        out.push('0');
    } else {
        for i in 0..point as usize {
            out.push(if i < digits.len() {
                digits[i] as char
            } else {
                '0'
            });
        }
    }
    if decimals > 0 {
        out.push('.');
        for i in 0..decimals {
            let idx = point + i as i32;
            out.push(if idx >= 0 && (idx as usize) < digits.len() {
                digits[idx as usize] as char
            } else {
                '0'
            });
        }
    }
    out
}

fn zero_fixed(decimals: usize) -> String {
    if decimals == 0 {
        "0".to_string()
    } else {
        format!("0.{}", "0".repeat(decimals))
    }
}

pub fn to_decimal_string(raw_value: f64, dt: DataType) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    Ok(double_to_bits(raw_value, dt)?.to_string())
}

pub fn to_hex_string(raw_value: f64, dt: DataType) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    Ok(format!("0x{:X}", double_to_bits(raw_value, dt)?))
}

pub fn to_binary_string(raw_value: f64, dt: DataType) -> Result<String> {
    if raw_value.is_nan() {
        return Ok("NaN".to_string());
    }
    let bits = double_to_bits(raw_value, dt)?;
    let width = size_in_bit(dt)?;
    Ok(format!("{bits:0width$b}"))
}

pub fn get_decimal_count(format: &str) -> Result<i32> {
    if format.len() < 2 {
        return Ok(0);
    }
    let body = format[1..].to_ascii_lowercase();
    let parts: Vec<&str> = body.split(['.', 'd', 'f']).collect();
    if parts.len() >= 2 {
        parts[1]
            .parse::<i32>()
            .map_err(|_| Error::Value(format!("getDecimalCount: invalid format {format:?}")))
    } else {
        Ok(0)
    }
}

pub fn parse2_double_val(s: &str) -> Result<f64> {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() > 2 && b[0] == b'0' && (b[1] & 0x5F) == b'X' {
        return i64::from_str_radix(&t[2..], 16)
            .map(|v| v as f64)
            .map_err(|_| Error::Value(format!("parse2DoubleVal: invalid hex {s:?}")));
    }
    t.parse::<f64>()
        .map_err(|_| Error::Value(format!("parse2DoubleVal: invalid number {s:?}")))
}

// ===========================================================================
// ===========================================================================

fn is_identity_coeffs(c: &[f64; 6]) -> bool {
    *c == [0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
}

fn has_denominator(c: &[f64; 6]) -> bool {
    c[3] != 0.0 || c[4] != 0.0 || c[5] != 1.0
}

pub fn rational_to_physical(coeffs: &RationalCoeffs, raw_value: f64) -> f64 {
    let c = &coeffs.coeffs;
    if is_identity_coeffs(c) || raw_value.is_nan() {
        return raw_value;
    }
    let (num, num2, num3) = if has_denominator(c) {
        (
            c[3] * raw_value - c[0],
            c[4] * raw_value - c[1],
            c[5] * raw_value - c[2],
        )
    } else {
        (-c[0], -c[1], raw_value - c[2])
    };
    if num == 0.0 {
        return -num3 / num2;
    }
    let disc = num2 * num2 - 4.0 * num * num3;
    if disc < 0.0 {
        return raw_value;
    }
    let root = disc.sqrt();
    let num6 = (-num2 + root) / (2.0 * num);
    let num7 = (-num2 - root) / (2.0 * num);
    if num6 != num7 {
        return raw_value;
    }
    num6
}

pub fn rational_to_raw(coeffs: &RationalCoeffs, data_type: DataType, x: f64) -> f64 {
    let c = &coeffs.coeffs;
    if is_identity_coeffs(c) || x.is_nan() {
        return x;
    }
    let mut num = if c[0] != 0.0 {
        c[0] * x * x + c[1] * x + c[2]
    } else {
        c[1] * x + c[2]
    };
    if has_denominator(c) {
        num /= if c[3] != 0.0 {
            c[3] * x * x + c[4] * x + c[5]
        } else {
            c[4] * x + c[5]
        };
    }
    if !matches!(
        data_type,
        DataType::Float16Ieee | DataType::Float32Ieee | DataType::Float64Ieee
    ) {
        num = round_bankers(num, 0);
    }
    num
}

fn convert_to_i64(v: f64) -> i64 {
    v.round_ties_even() as i64
}

pub fn tab_to_physical(conv_type: ConversionType, tab: &CompuTab, raw_value: f64) -> f64 {
    let values = &tab.values;
    match conv_type {
        ConversionType::TAB_INTP => {
            if values.is_empty() {
                return raw_value;
            }
            let (mut num2, mut num3) = (f64::from(values[0].0), values[0].1);
            for &(k, v) in &values[1..] {
                if raw_value < num2 {
                    return num3;
                }
                let (num4, num5) = (f64::from(k), v);
                if raw_value <= num4 {
                    return (raw_value - num2) / (num4 - num2) * (num5 - num3) + num3;
                }
                num2 = num4;
                num3 = num5;
            }
            num3
        }
        ConversionType::TAB_NOINTP => {
            let key = convert_to_i64(raw_value) as f32;
            for &(k, v) in values {
                if k == key {
                    return v;
                }
            }
            tab.default_value_numeric.unwrap_or(f64::NAN)
        }
        _ => 0.0,
    }
}

pub fn tab_to_raw(
    conv_type: ConversionType,
    tab: &CompuTab,
    _data_type: DataType,
    physical_value: f64,
) -> f64 {
    let values = &tab.values;
    match conv_type {
        ConversionType::TAB_INTP => {
            if values.is_empty() {
                return 0.0;
            }
            let (mut num, mut num2) = (f64::from(values[0].0), values[0].1);
            for &(k, v) in &values[1..] {
                if physical_value < num2 {
                    return num;
                }
                let (num3, num4) = (f64::from(k), v);
                if physical_value <= num4 {
                    return (physical_value - num2) / (num4 - num2) * (num3 - num) + num;
                }
                num = num3;
                num2 = num4;
            }
            num
        }
        ConversionType::TAB_NOINTP => {
            for &(k, v) in values {
                if v == physical_value {
                    return f64::from(k);
                }
            }
            0.0
        }
        _ => 0.0,
    }
}

pub fn vtab_to_physical(tab: &CompuVtab, raw_value: f64) -> String {
    if !raw_value.is_nan() {
        if let Some(v) = tab.verbs.get(&convert_to_i64(raw_value)) {
            return v.clone();
        }
    }
    match tab.base.default_value.as_deref().filter(|s| !s.is_empty()) {
        Some(dv) => dv.to_string(),
        None => format!("{raw_value}"),
    }
}

pub fn vtab_to_raw(tab: &CompuVtab, str_value: &str) -> f64 {
    if str_value.is_empty() {
        return 0.0;
    }
    for (k, v) in &tab.verbs {
        if v == str_value {
            return *k as f64;
        }
    }
    0.0
}

pub fn vtab_range_to_physical(tab: &CompuVtabRange, raw_value: f64) -> String {
    for (text, ranges) in &tab.verbs {
        for &(min, max) in ranges {
            if raw_value >= min && raw_value <= max {
                return text.clone();
            }
        }
    }
    tab.base.default_value.clone().unwrap_or_default()
}

pub fn vtab_range_to_raw(tab: &CompuVtabRange, phys_value: &str) -> f64 {
    match tab.verbs.get(phys_value) {
        Some(ranges) if !ranges.is_empty() => ranges[0].0,
        _ => 0.0,
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CompuTabRef<'a> {
    Tab(&'a CompuTab),
    Vtab(&'a CompuVtab),
    VtabRange(&'a CompuVtabRange),
}

/// `A2LCOMPU_METHOD.mDefaultCompuMethod`).
#[derive(Debug, Clone, Copy)]
pub struct Conversion<'a> {
    pub method: &'a CompuMethod,
    pub tab: Option<CompuTabRef<'a>>,
}

impl<'a> Conversion<'a> {
    pub fn new(method: &'a CompuMethod) -> Self {
        Conversion { method, tab: None }
    }

    pub fn with_tab(method: &'a CompuMethod, tab: CompuTabRef<'a>) -> Self {
        Conversion {
            method,
            tab: Some(tab),
        }
    }

    pub fn to_physical(&self, raw_value: f64) -> Result<f64> {
        match self.method.conversion_type {
            ConversionType::IDENTICAL => Ok(raw_value),
            ConversionType::LINEAR | ConversionType::RAT_FUNC => {
                Ok(rational_to_physical(&self.method.coeffs, raw_value))
            }
            ConversionType::TAB_VERB => Ok(raw_value),
            ConversionType::TAB_INTP | ConversionType::TAB_NOINTP => match self.tab {
                Some(CompuTabRef::Tab(tab)) => {
                    Ok(tab_to_physical(self.method.conversion_type, tab, raw_value))
                }
                _ => Err(Error::Value(format!(
                    "toPhysical: {:?} requires a COMPU_TAB reference",
                    self.method.conversion_type
                ))),
            },
            ConversionType::FORM => {
                let f = self.method.inline_formula.as_ref().ok_or_else(|| {
                    Error::Formula(format!(
                        "toPhysical: FORM of {:?} has no inline FORMULA",
                        self.method.name
                    ))
                })?;
                let ev =
                    autors_formula::formula::A2LFormula::new(&f.formula, f.formula_inv.as_deref())?;
                Ok(ev.to_physical(raw_value))
            }
        }
    }

    pub fn to_raw(&self, data_type: DataType, physical_value: f64) -> Result<f64> {
        match self.method.conversion_type {
            ConversionType::IDENTICAL => Ok(physical_value),
            ConversionType::LINEAR | ConversionType::RAT_FUNC => Ok(rational_to_raw(
                &self.method.coeffs,
                data_type,
                physical_value,
            )),
            ConversionType::TAB_VERB => Ok(physical_value),
            ConversionType::TAB_INTP | ConversionType::TAB_NOINTP => match self.tab {
                Some(CompuTabRef::Tab(tab)) => Ok(tab_to_raw(
                    self.method.conversion_type,
                    tab,
                    data_type,
                    physical_value,
                )),
                _ => Err(Error::Value(format!(
                    "toRaw: {:?} requires a COMPU_TAB reference",
                    self.method.conversion_type
                ))),
            },
            ConversionType::FORM => {
                let f = self.method.inline_formula.as_ref().ok_or_else(|| {
                    Error::Formula(format!(
                        "toRaw: FORM of {:?} has no inline FORMULA",
                        self.method.name
                    ))
                })?;
                let ev =
                    autors_formula::formula::A2LFormula::new(&f.formula, f.formula_inv.as_deref())?;
                Ok(ev.to_raw(data_type, physical_value))
            }
        }
    }

    pub fn get_min_increment(&self, decimal_count: i32, data_type: DataType) -> Result<f64> {
        match self.method.conversion_type {
            ConversionType::TAB_NOINTP | ConversionType::TAB_VERB => {
                return Err(Error::Value(format!(
                    "getMinIncrement: not supported for {:?}",
                    self.method.conversion_type
                )));
            }
            _ => {}
        }
        let mut num = 1.0;
        for _ in (0..decimal_count).rev() {
            num /= 10.0;
        }
        if !matches!(
            data_type,
            DataType::Float16Ieee | DataType::Float32Ieee | DataType::Float64Ieee
        ) {
            num = (self.to_physical(1.0)? - self.to_physical(0.0)?).abs();
        }
        Ok(num)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn to_string_value(
        &self,
        value: f64,
        format: ValueObjectFormat,
        data_type: DataType,
        decimal_count: i32,
        lower_limit: f64,
        upper_limit: f64,
    ) -> Result<(String, bool, bool)> {
        let decimal_count = decimal_count.min(15);
        if self.method.conversion_type == ConversionType::TAB_VERB
            && format == ValueObjectFormat::Physical
        {
            match self.tab {
                Some(CompuTabRef::Vtab(tab)) => {
                    return Ok((vtab_to_physical(tab, value), false, false));
                }
                Some(CompuTabRef::VtabRange(tab)) => {
                    return Ok((vtab_range_to_physical(tab, value), false, false));
                }
                _ => {}
            }
        }
        match format {
            ValueObjectFormat::Physical => {
                let mut num = if decimal_count < 0 {
                    value
                } else {
                    round_bankers(value, decimal_count)
                };
                let lower_violated = num < lower_limit;
                let upper_violated = num > upper_limit;
                if num == 0.0 {
                    num = 0.0;
                }
                let text = if decimal_count >= 0 {
                    format_fixed(num, decimal_count)
                } else {
                    format!("{num}")
                };
                Ok((text, lower_violated, upper_violated))
            }
            ValueObjectFormat::Raw => Ok((to_decimal_string(value, data_type)?, false, false)),
            ValueObjectFormat::RawHex => Ok((to_hex_string(value, data_type)?, false, false)),
            ValueObjectFormat::RawBin => Ok((to_binary_string(value, data_type)?, false, false)),
        }
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ValueData {
    None,
    Scalar(f64),
    Text(String),
    Texts(Vec<String>),
    Array { dims: Vec<usize>, data: Vec<f64> },
}

impl ValueData {
    pub fn array(dims: Vec<usize>, data: Vec<f64>) -> Result<Self> {
        if dims.is_empty() || dims.len() > 5 {
            return Err(Error::Value(format!(
                "ValueData::array: unsupported rank {}",
                dims.len()
            )));
        }
        let total: usize = dims.iter().product();
        if total != data.len() {
            return Err(Error::Value(format!(
                "ValueData::array: dims {dims:?} imply {total} elements, got {}",
                data.len()
            )));
        }
        Ok(ValueData::Array { dims, data })
    }

    pub fn len(&self) -> usize {
        match self {
            ValueData::None => 0,
            ValueData::Scalar(_) | ValueData::Text(_) => 1,
            ValueData::Texts(t) => t.len(),
            ValueData::Array { data, .. } => data.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_array(&self) -> Option<(&[usize], &[f64])> {
        match self {
            ValueData::Array { dims, data } => Some((dims, data)),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<(&[usize], &mut [f64])> {
        match self {
            ValueData::Array { dims, data } => Some((dims, data)),
            _ => None,
        }
    }

    pub fn as_scalar(&self) -> Option<f64> {
        match self {
            ValueData::Scalar(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ValueData::Text(s) => Some(s),
            _ => None,
        }
    }

    fn flat_index(dims: &[usize], idx: &[usize]) -> Result<usize> {
        if idx.len() != dims.len() {
            return Err(Error::Value(format!(
                "index rank {} does not match dims {dims:?}",
                idx.len()
            )));
        }
        let mut flat = 0usize;
        for (i, &ix) in idx.iter().enumerate() {
            if ix >= dims[i] {
                return Err(Error::Value(format!(
                    "index {idx:?} out of bounds for dims {dims:?}"
                )));
            }
            flat = flat * dims[i] + ix;
        }
        Ok(flat)
    }

    pub fn get(&self, idx: &[usize]) -> Result<f64> {
        match self {
            ValueData::Array { dims, data } => {
                let flat = Self::flat_index(dims, idx)?;
                Ok(data[flat])
            }
            _ => Err(Error::Value("get: value is not a numeric array".into())),
        }
    }

    pub fn set(&mut self, idx: &[usize], value: f64) -> Result<()> {
        match self {
            ValueData::Array { dims, data } => {
                let flat = Self::flat_index(dims, idx)?;
                data[flat] = value;
                Ok(())
            }
            _ => Err(Error::Value("set: value is not a numeric array".into())),
        }
    }

    pub fn dim_x(&self) -> usize {
        match self {
            ValueData::Array { dims, .. } => dims[0],
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CharacteristicRef {
    /// `/begin CHARACTERISTIC`.
    Char(Characteristic),
    AxisPts(AxisPts),
}

impl CharacteristicRef {
    pub fn char_type(&self) -> CharacteristicType {
        match self {
            CharacteristicRef::Char(ch) => ch.char_type,
            CharacteristicRef::AxisPts(_) => CharacteristicType::VAL_BLK,
        }
    }

    pub fn conversion(&self) -> &str {
        match self {
            CharacteristicRef::Char(ch) => &ch.conv.conversion,
            CharacteristicRef::AxisPts(ap) => &ap.conv.conversion,
        }
    }

    pub fn lower_limit(&self) -> f64 {
        match self {
            CharacteristicRef::Char(ch) => ch.conv.lower_limit,
            CharacteristicRef::AxisPts(ap) => ap.conv.lower_limit,
        }
    }

    pub fn upper_limit(&self) -> f64 {
        match self {
            CharacteristicRef::Char(ch) => ch.conv.upper_limit,
            CharacteristicRef::AxisPts(ap) => ap.conv.upper_limit,
        }
    }

    pub fn record_layout(&self) -> &str {
        match self {
            CharacteristicRef::Char(ch) => &ch.rec.record_layout,
            CharacteristicRef::AxisPts(ap) => &ap.rec.record_layout,
        }
    }

    pub fn axis_descrs(&self) -> Vec<&autors_a2l::model::measurement::AxisDescr> {
        match self {
            CharacteristicRef::Char(ch) => ch
                .children
                .iter()
                .filter_map(|c| match c {
                    CharacteristicChild::AxisDescr(a) => Some(a),
                    _ => None,
                })
                .collect(),
            CharacteristicRef::AxisPts(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IncrementContext<'a> {
    pub conversion: &'a Conversion<'a>,
    /// `RefRecordLayout.AxisPts[axis].DataType`).
    pub data_type: DataType,
    pub lower_limit: f64,
    pub upper_limit: f64,
}

pub struct SingleValueContext<'a> {
    pub conversion: &'a Conversion<'a>,
    pub data_type: DataType,
    pub axes: Vec<AxisContext<'a>>,
}

pub struct AxisContext<'a> {
    pub conversion: &'a Conversion<'a>,
    pub data_type: DataType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BaseValue {
    pub characteristic: Option<CharacteristicRef>,
    pub value_format: ValueObjectFormat,
    pub value: ValueData,
    pub axis_value: Vec<Vec<f64>>,
    pub decimal_count_axis: Vec<i32>,
    pub unit_axis: Vec<String>,
    pub decimal_count: i32,
    pub unit: String,
}

impl Default for BaseValue {
    fn default() -> Self {
        BaseValue {
            characteristic: None,
            value_format: ValueObjectFormat::default(),
            value: ValueData::None,
            axis_value: Vec::new(),
            decimal_count_axis: Vec::new(),
            unit_axis: Vec::new(),
            decimal_count: 0,
            unit: String::new(),
        }
    }
}

impl BaseValue {
    pub fn with_characteristic(characteristic: CharacteristicRef) -> Self {
        BaseValue {
            characteristic: Some(characteristic),
            ..Default::default()
        }
    }

    pub fn char_type(&self) -> CharacteristicType {
        match &self.characteristic {
            Some(r) => r.char_type(),
            None => CharacteristicType::VAL_BLK,
        }
    }

    fn limits(&self) -> (f64, f64) {
        match &self.characteristic {
            Some(r) => (r.lower_limit(), r.upper_limit()),
            None => (f64::NEG_INFINITY, f64::INFINITY),
        }
    }

    fn fmt_axis_dec(&self, axis: usize) -> i32 {
        self.decimal_count_axis.get(axis).copied().unwrap_or(0)
    }

    fn fmt_axis_unit(&self, axis: usize) -> &str {
        self.unit_axis.get(axis).map_or("", String::as_str)
    }

    fn fmt_axis_values(&self, axis: usize) -> &[f64] {
        self.axis_value.get(axis).map_or(&[], Vec::as_slice)
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    pub fn base_to_string(&self) -> String {
        match &self.value {
            ValueData::Scalar(v) => {
                let num = format_fixed(*v, self.decimal_count);
                if !self.unit.is_empty() {
                    format!("Value[{}]={num}", self.unit)
                } else {
                    format!("Value={num}")
                }
            }
            ValueData::Text(s) => s.clone(),
            ValueData::Texts(_) => "System.String[]".to_string(),
            ValueData::Array { dims, .. } => match dims.len() {
                1 => "System.Double[]".to_string(),
                2 => "System.Double[,]".to_string(),
                3 => "System.Double[,,]".to_string(),
                4 => "System.Double[,,,]".to_string(),
                _ => "System.Double[,,,,]".to_string(),
            },
            ValueData::None => String::new(),
        }
    }

    // -----------------------------------------------------------------------
    // toSingleValue
    // -----------------------------------------------------------------------

    pub fn to_single_value(&self, force_fmt: bool, ctx: &SingleValueContext) -> Result<String> {
        match self.char_type() {
            CharacteristicType::VALUE => {
                let v = self
                    .value
                    .as_scalar()
                    .ok_or_else(|| Error::Value("toSingleValue: scalar value not set".into()))?;
                let (lo, hi) = self.limits();
                let dc = if force_fmt { self.decimal_count } else { -1 };
                let (text, _, _) = ctx.conversion.to_string_value(
                    v,
                    self.value_format,
                    ctx.data_type,
                    dc,
                    lo,
                    hi,
                )?;
                Ok(text)
            }
            CharacteristicType::ASCII => Ok(self.value.as_text().unwrap_or("").to_string()),
            CharacteristicType::VAL_BLK
            | CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5 => Ok(String::new()),
            other => Err(Error::Value(format!(
                "toSingleValue: unsupported {other:?}"
            ))),
        }
    }

    pub fn to_single_value_at(
        &self,
        x: i32,
        y: i32,
        z: i32,
        force_fmt: bool,
        ctx: &SingleValueContext,
    ) -> Result<String> {
        let char_type = self.char_type();
        let axis_ctx = |idx: usize| -> (&Conversion, DataType) {
            match ctx.axes.get(idx) {
                Some(a) => (a.conversion, a.data_type),
                None => (ctx.conversion, ctx.data_type),
            }
        };
        let fmt_axis = |v: f64, idx: usize| -> Result<String> {
            let (conv, dt) = axis_ctx(idx);
            let dc = if force_fmt {
                self.fmt_axis_dec(idx)
            } else {
                -1
            };
            let (text, _, _) =
                conv.to_string_value(v, self.value_format, dt, dc, f64::MIN, f64::MAX)?;
            Ok(text)
        };
        let fmt_value = |v: f64| -> Result<String> {
            let dc = if force_fmt { self.decimal_count } else { -1 };
            let (text, _, _) = ctx.conversion.to_string_value(
                v,
                self.value_format,
                ctx.data_type,
                dc,
                f64::MIN,
                f64::MAX,
            )?;
            Ok(text)
        };
        match char_type {
            CharacteristicType::VALUE => self.to_single_value(force_fmt, ctx),
            CharacteristicType::CUBOID => {
                if y == -1 && x == -1 {
                    return fmt_axis(
                        self.fmt_axis_values(2)
                            .get(z.max(0) as usize)
                            .copied()
                            .ok_or_else(|| {
                                Error::Value(format!("toSingleValueAt: z index {z} out of range"))
                            })?,
                        2,
                    );
                }
                if y == -1 && x >= 0 {
                    let x = x as usize;
                    if x >= self.fmt_axis_values(0).len() {
                        return Ok(String::new());
                    }
                    return fmt_axis(self.fmt_axis_values(0)[x], 0);
                }
                if x == -1 {
                    if y == -1 {
                        return Ok(format!(
                            "[{}/{}]",
                            self.fmt_axis_unit(0),
                            self.fmt_axis_unit(1)
                        ));
                    }
                    let y = y as usize;
                    if y >= self.fmt_axis_values(1).len() {
                        return Ok(String::new());
                    }
                    return fmt_axis(self.fmt_axis_values(1)[y], 1);
                }
                fmt_value(self.value.get(&[x as usize, y as usize, z as usize])?)
            }
            CharacteristicType::MAP => {
                if y == -1 && x >= 0 {
                    let x = x as usize;
                    if x >= self.fmt_axis_values(0).len() {
                        return Ok(String::new());
                    }
                    return fmt_axis(self.fmt_axis_values(0)[x], 0);
                }
                if x == -1 {
                    if y == -1 {
                        return Ok(format!(
                            "[{}/{}]",
                            self.fmt_axis_unit(0),
                            self.fmt_axis_unit(1)
                        ));
                    }
                    let y = y as usize;
                    if y >= self.fmt_axis_values(1).len() {
                        return Ok(String::new());
                    }
                    return fmt_axis(self.fmt_axis_values(1)[y], 1);
                }
                fmt_value(self.value.get(&[x as usize, y as usize])?)
            }
            CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                if x == -1 {
                    if y == -1 {
                        if !self.axis_value.is_empty() && !self.fmt_axis_unit(0).is_empty() {
                            return Ok(self.fmt_axis_unit(0).to_string());
                        }
                        return Ok(" ".to_string());
                    }
                    return Ok(self.unit.clone());
                }
                if y == -1 {
                    if !self.axis_value.is_empty() {
                        let x = x as usize;
                        if x >= self.fmt_axis_values(0).len() {
                            return Ok(String::new());
                        }
                        return fmt_axis(self.fmt_axis_values(0)[x], 0);
                    }
                    return Ok(x.to_string());
                }
                fmt_value(self.value.get(&[x as usize])?)
            }
            other => Err(Error::Value(format!(
                "toSingleValueAt: unsupported {other:?}"
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    pub fn get_fnc_scalar(&self) -> Result<f64> {
        self.value
            .as_scalar()
            .ok_or_else(|| Error::Value("getFncValue: scalar value not set".into()))
    }

    pub fn get_fnc_value(&self, x: i32, y: i32, z: i32) -> Result<f64> {
        match self.char_type() {
            CharacteristicType::VALUE => self.get_fnc_scalar(),
            CharacteristicType::ASCII => {
                let s = self
                    .value
                    .as_text()
                    .ok_or_else(|| Error::Value("getFncValue: text value not set".into()))?;
                let unit = s.encode_utf16().nth(x.max(0) as usize).ok_or_else(|| {
                    Error::Value(format!("getFncValue: char index {x} out of range"))
                })?;
                Ok(f64::from(unit))
            }
            CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                self.value.get(&[x.max(0) as usize])
            }
            CharacteristicType::MAP => self.value.get(&[x.max(0) as usize, y.max(0) as usize]),
            CharacteristicType::CUBOID => {
                self.value
                    .get(&[x.max(0) as usize, y.max(0) as usize, z.max(0) as usize])
            }
            other => Err(Error::Value(format!("getFncValue: unsupported {other:?}"))),
        }
    }

    pub fn set_fnc_scalar(&mut self, value: f64) -> Result<()> {
        self.set_fnc_value(value, 0, 0, 0)
    }

    pub fn set_fnc_value(&mut self, value: f64, x: i32, y: i32, z: i32) -> Result<()> {
        match self.char_type() {
            CharacteristicType::VALUE => {
                self.value = ValueData::Scalar(value);
                Ok(())
            }
            CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                self.value.set(&[x.max(0) as usize], value)
            }
            CharacteristicType::MAP => self
                .value
                .set(&[x.max(0) as usize, y.max(0) as usize], value),
            CharacteristicType::CUBOID => self.value.set(
                &[x.max(0) as usize, y.max(0) as usize, z.max(0) as usize],
                value,
            ),
            other => Err(Error::Value(format!("setFncValue: unsupported {other:?}"))),
        }
    }

    pub fn fnc_values(&self) -> Result<Vec<f64>> {
        match self.char_type() {
            CharacteristicType::VALUE => Ok(vec![self.get_fnc_scalar()?]),
            CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                let (_, data) = self
                    .value
                    .as_array()
                    .ok_or_else(|| Error::Value("fnc_values: array value not set".into()))?;
                Ok(data.to_vec())
            }
            CharacteristicType::MAP | CharacteristicType::CUBOID => {
                let (dims, data) = self
                    .value
                    .as_array()
                    .ok_or_else(|| Error::Value("fnc_values: array value not set".into()))?;
                let mut idx = vec![0usize; dims.len()];
                let mut out = Vec::with_capacity(data.len());
                for _ in 0..data.len() {
                    out.push(data[ValueData::flat_index(dims, &idx)?]);
                    for d in 0..dims.len() {
                        idx[d] += 1;
                        if idx[d] < dims[d] {
                            break;
                        }
                        idx[d] = 0;
                    }
                }
                Ok(out)
            }
            other => Err(Error::Value(format!("fnc_values: unsupported {other:?}"))),
        }
    }

    pub fn compare_to(&self, other: &BaseValue) -> Ordering {
        let ct = self.char_type();
        let ct_other = other.char_type();
        if ct != CharacteristicType::VALUE && ct_other != CharacteristicType::VALUE {
            return Ordering::Equal;
        }
        if ct != CharacteristicType::VALUE {
            return Ordering::Less;
        }
        if ct_other != CharacteristicType::VALUE {
            return Ordering::Greater;
        }
        let a = self.value.as_scalar().unwrap_or(f64::NAN);
        let b = other.value.as_scalar().unwrap_or(f64::NAN);
        match (a.is_nan(), b.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        }
    }
}

// ===========================================================================
// ===========================================================================

impl BaseValue {
    pub fn get_next_value(
        &self,
        increment: bool,
        value: f64,
        ctx: &IncrementContext,
    ) -> Result<f64> {
        let conv = ctx.conversion;
        match conv.method.conversion_type {
            ConversionType::TAB_NOINTP => {
                let tab = match conv.tab {
                    Some(CompuTabRef::Tab(t)) => t,
                    _ => {
                        return Err(Error::Value(
                            "getNextValue: TAB_NOINTP requires a COMPU_TAB reference".into(),
                        ));
                    }
                };
                let mut v = value;
                if self.value_format == ValueObjectFormat::Physical {
                    v = conv.to_raw(ctx.data_type, v)?;
                }
                let values = &tab.values;
                if values.is_empty() {
                    return Err(Error::Value("getNextValue: empty COMPU_TAB".into()));
                }
                let mut num6 = v as f32;
                let num4 = values[0].0;
                let num5 = values[values.len() - 1].0;
                let num7;
                if num6 < num4 || num6 > num5 {
                    num7 = if increment { num4 } else { num5 };
                } else {
                    loop {
                        num6 += if increment { 1.0 } else { -1.0 };
                        if num6 >= num5 {
                            num7 = num5;
                            break;
                        }
                        if num6 <= num4 {
                            num7 = num4;
                            break;
                        }
                        if values.iter().any(|(k, _)| *k == num6) {
                            num7 = num6;
                            break;
                        }
                    }
                }
                if self.value_format != ValueObjectFormat::Physical {
                    return Ok(f64::from(num7));
                }
                Ok(values
                    .iter()
                    .find(|(k, _)| *k == num7)
                    .map(|(_, v)| *v)
                    .unwrap_or(f64::NAN))
            }
            ConversionType::TAB_VERB => match conv.tab {
                Some(CompuTabRef::Vtab(vtab)) => {
                    let mut num3 = convert_to_i64(value);
                    let keys: Vec<i64> = vtab.verbs.keys().copied().collect();
                    if keys.is_empty() {
                        return Err(Error::Value("getNextValue: empty COMPU_VTAB".into()));
                    }
                    let num4 = keys[0];
                    let num5 = keys[keys.len() - 1];
                    if num3 < num4 || num3 > num5 {
                        return Ok(if increment { num4 } else { num5 } as f64);
                    }
                    loop {
                        num3 += if increment { 1 } else { -1 };
                        if num3 >= num5 {
                            return Ok(num5 as f64);
                        }
                        if num3 <= num4 {
                            return Ok(num4 as f64);
                        }
                        if vtab.verbs.contains_key(&num3) {
                            return Ok(num3 as f64);
                        }
                    }
                }
                Some(CompuTabRef::VtabRange(vtr)) => {
                    let mut v = value;
                    if self.value_format == ValueObjectFormat::Raw {
                        v = conv.to_physical(v)?;
                    }
                    let mut num = 0.0;
                    if let Some((_, ranges)) = vtr.verbs.iter().next() {
                        if !ranges.is_empty() {
                            num = ranges[0].0;
                        }
                    }
                    let keys: Vec<&String> = vtr.verbs.keys().collect();
                    let current = vtab_range_to_physical(vtr, v);
                    if let Some(idx) = keys.iter().position(|k| **k == current) {
                        let idx = (idx as i32 + if increment { 1 } else { -1 })
                            .clamp(0, vtr.verbs.len() as i32 - 1)
                            as usize;
                        if let Some((_, ranges)) = vtr.verbs.get_index(idx) {
                            if !ranges.is_empty() {
                                num = ranges[0].0;
                            }
                        }
                    }
                    if self.value_format != ValueObjectFormat::Raw {
                        return Ok(num);
                    }
                    conv.to_raw(ctx.data_type, num)
                }
                _ => Err(Error::Value(
                    "getNextValue: TAB_VERB requires a COMPU_VTAB/_RANGE reference".into(),
                )),
            },
            other => Err(Error::Value(format!(
                "getNextValue: unsupported conversion type {other:?}"
            ))),
        }
    }

    pub fn increment_or_decrement_single(
        &mut self,
        increment: bool,
        count: i32,
        ctx: &IncrementContext,
    ) -> Result<bool> {
        let old = self
            .value
            .as_scalar()
            .ok_or_else(|| Error::Value("incrementOrDecrement: scalar value not set".into()))?;
        let mut num = old;
        if matches!(
            ctx.conversion.method.conversion_type,
            ConversionType::TAB_NOINTP | ConversionType::TAB_VERB
        ) {
            num = self.get_next_value(increment, num, ctx)?;
        } else {
            let (lo, hi) = self.limits();
            let min_incr =
                ctx.conversion
                    .get_min_increment_ctx(self.decimal_count, ctx.data_type, lo, hi)?;
            for _ in 0..count {
                num = if increment {
                    num + min_incr
                } else {
                    num - min_incr
                };
            }
        }
        if self.value_format == ValueObjectFormat::Physical {
            let (lo, hi) = self.limits();
            num = num.clamp(lo, hi);
        }
        if num == old {
            return Ok(false);
        }
        self.value = ValueData::Scalar(num);
        Ok(true)
    }

    pub fn increment_or_decrement_at(
        &mut self,
        indexes: &[Vec<usize>],
        increment: bool,
        count: i32,
        ctx: &IncrementContext,
    ) -> Result<bool> {
        if matches!(
            self.char_type(),
            CharacteristicType::CUBE_4 | CharacteristicType::CUBE_5
        ) {
            return Err(Error::Value(format!(
                "incrementOrDecrement: not implemented for {:?}",
                self.char_type()
            )));
        }
        let (lo, hi) = self.limits();
        let min_incr = match ctx.conversion.method.conversion_type {
            ConversionType::TAB_NOINTP | ConversionType::TAB_VERB => f64::NAN,
            _ => ctx
                .conversion
                .get_min_increment_ctx(self.decimal_count, ctx.data_type, lo, hi)?,
        };
        for idx in indexes {
            let mut num2 = self.value.get(idx)?;
            if !min_incr.is_nan() {
                for _ in 0..count {
                    num2 = if increment {
                        num2 + min_incr
                    } else {
                        num2 - min_incr
                    };
                }
            } else {
                num2 = self.get_next_value(increment, num2, ctx)?;
            }
            if self.value_format == ValueObjectFormat::Physical {
                num2 = num2.clamp(lo, hi);
            }
            self.value.set(idx, num2)?;
        }
        Ok(true)
    }

    pub fn increment_or_decrement_axis(
        &mut self,
        axis: AxisValueType,
        indexes: &[usize],
        increment: bool,
        count: i32,
        ctx: &IncrementContext,
    ) -> Result<bool> {
        let axis_idx = axis as usize;
        let ct = self.char_type();
        let invalid = matches!(
            ct,
            CharacteristicType::NotSet
                | CharacteristicType::VALUE
                | CharacteristicType::ASCII
                | CharacteristicType::VAL_BLK
        ) || (axis_idx > 0 && ct == CharacteristicType::CURVE)
            || (axis_idx > 1 && ct == CharacteristicType::MAP)
            || (axis_idx > 2 && ct == CharacteristicType::CUBOID)
            || (axis_idx > 3 && ct == CharacteristicType::CUBE_4);
        if invalid {
            return Err(Error::Value(format!(
                "incrementOrDecrementAxis: axis {axis:?} invalid for {ct:?}"
            )));
        }
        let min_incr = match ctx.conversion.method.conversion_type {
            ConversionType::TAB_NOINTP | ConversionType::TAB_VERB => f64::NAN,
            _ => ctx.conversion.get_min_increment_ctx(
                self.fmt_axis_dec(axis_idx),
                ctx.data_type,
                ctx.lower_limit,
                ctx.upper_limit,
            )?,
        };
        let mut array =
            self.axis_value.get(axis_idx).cloned().ok_or_else(|| {
                Error::Value(format!("incrementOrDecrementAxis: no axis {axis_idx}"))
            })?;
        let ascending = array.len() <= 1 || array[0] <= array[array.len() - 1];
        for &item in indexes {
            let mut num2 = *array
                .get(item)
                .ok_or_else(|| Error::Value(format!("axis index {item} out of range")))?;
            if !min_incr.is_nan() {
                for _ in 0..count {
                    num2 = if increment {
                        num2 + min_incr
                    } else {
                        num2 - min_incr
                    };
                }
            } else {
                num2 = self.get_next_value(increment, num2, ctx)?;
            }
            if self.value_format == ValueObjectFormat::Physical {
                num2 = num2.clamp(ctx.lower_limit, ctx.upper_limit);
            }
            array[item] = num2;
        }
        fn not_ge(a: f64, b: f64) -> bool {
            !matches!(a.partial_cmp(&b), Some(Ordering::Greater | Ordering::Equal))
        }
        fn not_le(a: f64, b: f64) -> bool {
            !matches!(a.partial_cmp(&b), Some(Ordering::Less | Ordering::Equal))
        }
        for j in 0..array.len() {
            let num3 = array[j];
            let num4 = if j > 0 {
                array[j - 1]
            } else if ascending {
                ctx.lower_limit
            } else {
                ctx.upper_limit
            };
            let num5 = if j < array.len() - 1 {
                array[j + 1]
            } else if ascending {
                ctx.upper_limit
            } else {
                ctx.lower_limit
            };
            if (!ascending || not_ge(num3, num4) || not_le(num3, num5))
                && (ascending || not_le(num3, num4) || not_ge(num3, num5))
            {
                return Ok(false);
            }
        }
        self.axis_value[axis_idx] = array;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    pub fn modify_value_by(
        &mut self,
        new_value: f64,
        treat_value_as_percent: bool,
        func: fn(f64, f64, bool) -> f64,
    ) -> Result<()> {
        match self.char_type() {
            CharacteristicType::VALUE => {
                let v = self
                    .value
                    .as_scalar()
                    .ok_or_else(|| Error::Value("modifyValue: scalar value not set".into()))?;
                self.value = ValueData::Scalar(func(v, new_value, treat_value_as_percent));
                Ok(())
            }
            CharacteristicType::VAL_BLK
            | CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5 => {
                let (_, data) = self
                    .value
                    .as_array_mut()
                    .ok_or_else(|| Error::Value("modifyValue: array value not set".into()))?;
                for v in data.iter_mut() {
                    *v = func(*v, new_value, treat_value_as_percent);
                }
                Ok(())
            }
            other => Err(Error::Value(format!("modifyValue: unsupported {other:?}"))),
        }
    }

    pub fn modify_value_by_at(
        &mut self,
        new_value: f64,
        treat_value_as_percent: bool,
        func: fn(f64, f64, bool) -> f64,
        indexes: &[Vec<usize>],
    ) -> Result<()> {
        match self.char_type() {
            CharacteristicType::VAL_BLK
            | CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5 => {
                for idx in indexes {
                    let v = self.value.get(idx)?;
                    self.value
                        .set(idx, func(v, new_value, treat_value_as_percent))?;
                }
                Ok(())
            }
            other => Err(Error::Value(format!(
                "modifyValueAt: unsupported {other:?}"
            ))),
        }
    }

    pub fn modify_value_with(
        &mut self,
        func: &mut dyn FnMut(f64, Option<&CharacteristicRef>) -> f64,
    ) -> Result<()> {
        match self.char_type() {
            CharacteristicType::VALUE => {
                let v = self
                    .value
                    .as_scalar()
                    .ok_or_else(|| Error::Value("modifyValue: scalar value not set".into()))?;
                self.value = ValueData::Scalar(func(v, self.characteristic.as_ref()));
                Ok(())
            }
            CharacteristicType::VAL_BLK
            | CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5 => {
                let ch = self.characteristic.clone();
                let (_, data) = self
                    .value
                    .as_array_mut()
                    .ok_or_else(|| Error::Value("modifyValue: array value not set".into()))?;
                for v in data.iter_mut() {
                    *v = func(*v, ch.as_ref());
                }
                Ok(())
            }
            other => Err(Error::Value(format!("modifyValue: unsupported {other:?}"))),
        }
    }

    pub fn modify_value_with_at(
        &mut self,
        func: &mut dyn FnMut(f64, Option<&CharacteristicRef>) -> f64,
        indexes: &[Vec<usize>],
    ) -> Result<()> {
        match self.char_type() {
            CharacteristicType::VAL_BLK
            | CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5 => {
                let ch = self.characteristic.clone();
                for idx in indexes {
                    let v = self.value.get(idx)?;
                    self.value.set(idx, func(v, ch.as_ref()))?;
                }
                Ok(())
            }
            other => Err(Error::Value(format!(
                "modifyValueAt: unsupported {other:?}"
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    pub fn can_paste_from_clipboard(
        &self,
        text: Option<&str>,
        origin: (usize, usize),
        conv_is_table: bool,
    ) -> (PasteResult, Option<Vec<Vec<f64>>>) {
        let char_type = self.char_type();
        if !matches!(
            char_type,
            CharacteristicType::VALUE
                | CharacteristicType::VAL_BLK
                | CharacteristicType::CURVE
                | CharacteristicType::MAP
        ) {
            return (PasteResult::ErrNotSupportedByTargetValue, None);
        }
        if conv_is_table {
            return (PasteResult::ErrNotSupportedByTargetValue, None);
        }
        let text = match text {
            Some(t) if !t.is_empty() => t,
            _ => return (PasteResult::ErrSrcFormatNotAvailable, None),
        };
        let lines: Vec<&str> = text.split(['\r', '\n']).filter(|l| !l.is_empty()).collect();
        if lines.is_empty() {
            return (PasteResult::ErrSrcFormatNotAvailable, None);
        }
        let cols = lines[0].split('\t').count();
        let mut grid: Vec<Vec<&str>> = Vec::with_capacity(cols);
        for (i, line) in lines.iter().enumerate() {
            let cells: Vec<&str> = line.split('\t').collect();
            if i > 0 && cells.len() != cols {
                return (PasteResult::ErrSrcNonQuadratic, None);
            }
            for (j, cell) in cells.into_iter().enumerate() {
                if grid.len() <= j {
                    grid.push(Vec::new());
                }
                grid[j].push(cell);
            }
        }
        let rows = lines.len();
        let has_x_header = !self.axis_value.is_empty() && self.fmt_axis_values(0).len() + 1 == cols;
        let has_y_header = !self.axis_value.is_empty()
            && (self.axis_value.len() + 1 == rows
                || (self.axis_value.len() > 1 && self.fmt_axis_values(1).len() + 1 == rows));
        let (nx, ny) = (has_x_header as usize, has_y_header as usize);
        let (w, h) = (cols - nx, rows - ny);
        let fits = match char_type {
            CharacteristicType::VAL_BLK => {
                let cap = if origin == (0, 0) {
                    self.value.len()
                } else {
                    self.value.len().saturating_sub(origin.0)
                };
                w <= cap
            }
            CharacteristicType::CURVE => {
                let cap = if origin == (0, 0) {
                    self.fmt_axis_values(0).len()
                } else {
                    self.fmt_axis_values(0).len().saturating_sub(origin.0)
                };
                w <= cap
            }
            CharacteristicType::MAP => {
                let cap_x = if origin == (0, 0) {
                    self.fmt_axis_values(0).len()
                } else {
                    self.fmt_axis_values(0).len().saturating_sub(origin.0)
                };
                let cap_y = if origin == (0, 0) {
                    self.fmt_axis_values(1).len()
                } else {
                    self.fmt_axis_values(1).len().saturating_sub(origin.1)
                };
                w <= cap_x && h <= cap_y
            }
            _ => true,
        };
        if !fits {
            return (PasteResult::WarnDoesNotFit, None);
        }
        let mut values = vec![vec![0.0; h]; w];
        for k in ny..rows {
            for l in nx..cols {
                match parse2_double_val(grid[l][k]) {
                    Ok(v) => values[l - nx][k - ny] = v,
                    Err(_) => return (PasteResult::ErrSrcWrongValueFormat, None),
                }
            }
        }
        (PasteResult::Ok, Some(values))
    }

    pub fn paste_single(&mut self, text: Option<&str>, conv_is_table: bool) -> PasteResult {
        let (result, values) = self.can_paste_from_clipboard(text, (0, 0), conv_is_table);
        if result != PasteResult::Ok {
            return result;
        }
        if let Some(values) = values {
            if let Some(&v) = values.first().and_then(|r| r.first()) {
                self.value = ValueData::Scalar(v);
            }
        }
        PasteResult::Ok
    }

    pub fn paste_row(
        &mut self,
        text: Option<&str>,
        origin: (usize, usize),
        conv_is_table: bool,
    ) -> PasteResult {
        let (result, values) = self.can_paste_from_clipboard(text, origin, conv_is_table);
        if result != PasteResult::Ok {
            return result;
        }
        if let Some(values) = values {
            for (i, row) in values.iter().enumerate() {
                if let Some(&v) = row.first() {
                    if self.value.set(&[i + origin.0], v).is_err() {
                        return PasteResult::Err;
                    }
                }
            }
        }
        PasteResult::Ok
    }

    pub fn paste_block(
        &mut self,
        text: Option<&str>,
        origin: (usize, usize),
        conv_is_table: bool,
    ) -> PasteResult {
        let (result, values) = self.can_paste_from_clipboard(text, origin, conv_is_table);
        if result != PasteResult::Ok {
            return result;
        }
        if let Some(values) = values {
            for (i, row) in values.iter().enumerate() {
                for (j, &v) in row.iter().enumerate() {
                    if self.value.set(&[i + origin.0, j + origin.1], v).is_err() {
                        return PasteResult::Err;
                    }
                }
            }
        }
        PasteResult::Ok
    }
}

impl Conversion<'_> {
    fn get_min_increment_ctx(
        &self,
        decimal_count: i32,
        data_type: DataType,
        _lower: f64,
        _upper: f64,
    ) -> Result<f64> {
        self.get_min_increment(decimal_count, data_type)
    }
}

// ===========================================================================
// ===========================================================================

pub fn modify_set_func(original_value: f64, value: f64, treat_value_as_percent: bool) -> f64 {
    let _ = original_value;
    if !treat_value_as_percent {
        return value;
    }
    original_value * (value / 100.0)
}

pub fn modify_add_func(original_value: f64, value: f64, treat_value_as_percent: bool) -> f64 {
    if !treat_value_as_percent {
        return original_value + value;
    }
    original_value + original_value * (value / 100.0)
}

pub fn modify_sub_func(original_value: f64, value: f64, treat_value_as_percent: bool) -> f64 {
    if !treat_value_as_percent {
        return original_value - value;
    }
    original_value - original_value * (value / 100.0)
}

pub fn modify_mul_func(original_value: f64, value: f64, treat_value_as_percent: bool) -> f64 {
    if !treat_value_as_percent {
        return original_value * value;
    }
    original_value * original_value * (value / 100.0)
}

#[allow(clippy::eq_op)]
pub fn modify_div_func(original_value: f64, value: f64, treat_value_as_percent: bool) -> f64 {
    if !treat_value_as_percent {
        return original_value / value;
    }
    original_value / original_value * (value / 100.0)
}

pub fn modify_inv_func(original_value: f64, lower_limit: f64, upper_limit: f64) -> f64 {
    let num = upper_limit - original_value;
    if num >= 0.0 {
        return lower_limit + num;
    }
    upper_limit - (original_value - lower_limit)
}

// ===========================================================================
// ===========================================================================

macro_rules! value_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name {
            pub base: BaseValue,
        }

        impl $name {
            pub fn new() -> Self {
                $name::default()
            }

            pub fn with_characteristic(characteristic: CharacteristicRef) -> Self {
                $name {
                    base: BaseValue::with_characteristic(characteristic),
                }
            }
        }
    };
}

value_type!(SingleValue);
value_type!(AsciiValue);
value_type!(ValBlkValue);
value_type!(CurveValue);
value_type!(MapValue);
value_type!(CuboidValue);
value_type!(Cube4Value);
value_type!(Cube5Value);

impl fmt::Display for SingleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.base.base_to_string())
    }
}

impl fmt::Display for AsciiValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.base.base_to_string())
    }
}

impl fmt::Display for ValBlkValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = if self.base.unit.is_empty() {
            "Values=\n".to_string()
        } else {
            format!("Values[{}]=\n", self.base.unit)
        };
        let mut count = 0usize;
        let mut push_item = |s: &mut String, text: String| {
            s.push_str(&text);
            s.push(',');
            count += 1;
            if count.is_multiple_of(8) {
                s.pop();
                s.push('\n');
            }
        };
        match &self.base.value {
            ValueData::Texts(texts) => {
                for t in texts {
                    push_item(&mut s, t.clone());
                }
            }
            ValueData::Array { data, .. } => {
                for v in data {
                    push_item(&mut s, format_fixed(*v, self.base.decimal_count));
                }
            }
            _ => {}
        }
        s.pop();
        f.write_str(&s)
    }
}

impl fmt::Display for CurveValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let u0 = self.base.fmt_axis_unit(0);
        let mut s = if u0.is_empty() {
            "Axis\t".to_string()
        } else {
            format!("Axis[{u0}]\t")
        };
        let xs = self.base.fmt_axis_values(0);
        let dc0 = self.base.fmt_axis_dec(0);
        for (i, x) in xs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format_fixed(*x, dc0));
        }
        if self.base.unit.is_empty() {
            s.push_str("\nValues\t");
        } else {
            s.push_str(&format!("\nValues[{}]\t", self.base.unit));
        }
        if let Some((_, data)) = self.base.value.as_array() {
            for (i, v) in data.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format_fixed(*v, self.base.decimal_count));
            }
        }
        f.write_str(&s)
    }
}

impl fmt::Display for MapValue {
    /// `\n{y:F$dc1}\t\t{row…}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let u0 = self.base.fmt_axis_unit(0);
        let u1 = self.base.fmt_axis_unit(1);
        let mut s = if u0.is_empty() {
            "X\t\t".to_string()
        } else {
            format!("X[{u0}]\t\t")
        };
        let xs = self.base.fmt_axis_values(0);
        let ys = self.base.fmt_axis_values(1);
        let dc0 = self.base.fmt_axis_dec(0);
        let dc1 = self.base.fmt_axis_dec(1);
        for (i, x) in xs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format_fixed(*x, dc0));
        }
        match (self.base.unit.is_empty(), u1.is_empty()) {
            (true, true) => s.push_str("\nY"),
            (true, false) => s.push_str(&format!("\nY[{u1}]")),
            (false, true) => s.push_str(&format!("\nY\t\t[{}]", self.base.unit)),
            (false, false) => s.push_str(&format!("\nY[{u1}]\t\t[{}]", self.base.unit)),
        }
        if let Some((dims, data)) = self.base.value.as_array() {
            let (nx, ny) = (dims[0], *dims.get(1).unwrap_or(&1));
            for j in 0..ny {
                s.push('\n');
                s.push_str(&format_fixed(*ys.get(j).unwrap_or(&0.0), dc1));
                s.push_str("\t\t");
                for k in 0..nx {
                    if k > 0 {
                        s.push(',');
                    }
                    s.push_str(&format_fixed(data[k * ny + j], self.base.decimal_count));
                }
            }
        }
        f.write_str(&s)
    }
}

impl fmt::Display for CuboidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("")
    }
}

impl fmt::Display for Cube4Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("")
    }
}

impl fmt::Display for Cube5Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CharValue {
    /// CHAR_TYPE.VALUE.
    Single(SingleValue),
    /// CHAR_TYPE.ASCII.
    Ascii(AsciiValue),
    ValBlk(ValBlkValue),
    /// CHAR_TYPE.CURVE.
    Curve(CurveValue),
    /// CHAR_TYPE.MAP.
    Map(MapValue),
    /// CHAR_TYPE.CUBOID.
    Cuboid(CuboidValue),
    /// CHAR_TYPE.CUBE_4.
    Cube4(Cube4Value),
    /// CHAR_TYPE.CUBE_5.
    Cube5(Cube5Value),
}

impl CharValue {
    pub fn base(&self) -> &BaseValue {
        match self {
            CharValue::Single(v) => &v.base,
            CharValue::Ascii(v) => &v.base,
            CharValue::ValBlk(v) => &v.base,
            CharValue::Curve(v) => &v.base,
            CharValue::Map(v) => &v.base,
            CharValue::Cuboid(v) => &v.base,
            CharValue::Cube4(v) => &v.base,
            CharValue::Cube5(v) => &v.base,
        }
    }

    pub fn base_mut(&mut self) -> &mut BaseValue {
        match self {
            CharValue::Single(v) => &mut v.base,
            CharValue::Ascii(v) => &mut v.base,
            CharValue::ValBlk(v) => &mut v.base,
            CharValue::Curve(v) => &mut v.base,
            CharValue::Map(v) => &mut v.base,
            CharValue::Cuboid(v) => &mut v.base,
            CharValue::Cube4(v) => &mut v.base,
            CharValue::Cube5(v) => &mut v.base,
        }
    }

    pub fn char_type(&self) -> CharacteristicType {
        self.base().char_type()
    }

    pub fn compare_to(&self, other: &CharValue) -> Ordering {
        self.base().compare_to(other.base())
    }

    pub fn for_char_type(
        char_type: CharacteristicType,
        characteristic: CharacteristicRef,
    ) -> Result<Self> {
        Ok(match char_type {
            CharacteristicType::VALUE => {
                CharValue::Single(SingleValue::with_characteristic(characteristic))
            }
            CharacteristicType::ASCII => {
                CharValue::Ascii(AsciiValue::with_characteristic(characteristic))
            }
            CharacteristicType::VAL_BLK => {
                CharValue::ValBlk(ValBlkValue::with_characteristic(characteristic))
            }
            CharacteristicType::CURVE => {
                CharValue::Curve(CurveValue::with_characteristic(characteristic))
            }
            CharacteristicType::MAP => {
                CharValue::Map(MapValue::with_characteristic(characteristic))
            }
            CharacteristicType::CUBOID => {
                CharValue::Cuboid(CuboidValue::with_characteristic(characteristic))
            }
            CharacteristicType::CUBE_4 => {
                CharValue::Cube4(Cube4Value::with_characteristic(characteristic))
            }
            CharacteristicType::CUBE_5 => {
                CharValue::Cube5(Cube5Value::with_characteristic(characteristic))
            }
            other => {
                return Err(Error::Value(format!(
                    "createValue: unsupported char type {other:?}"
                )));
            }
        })
    }
}

impl fmt::Display for CharValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CharValue::Single(v) => fmt::Display::fmt(v, f),
            CharValue::Ascii(v) => fmt::Display::fmt(v, f),
            CharValue::ValBlk(v) => fmt::Display::fmt(v, f),
            CharValue::Curve(v) => fmt::Display::fmt(v, f),
            CharValue::Map(v) => fmt::Display::fmt(v, f),
            CharValue::Cuboid(v) => fmt::Display::fmt(v, f),
            CharValue::Cube4(v) => fmt::Display::fmt(v, f),
            CharValue::Cube5(v) => fmt::Display::fmt(v, f),
        }
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRange {
    pub start: u32,
    pub next: u32,
}

impl MemoryRange {
    pub fn size(&self) -> usize {
        (self.next - self.start) as usize
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRangeList {
    ranges: Vec<MemoryRange>,
    reduced: bool,
}

impl MemoryRangeList {
    pub fn new() -> Self {
        MemoryRangeList::default()
    }

    pub fn add(&mut self, range: MemoryRange) {
        self.ranges.push(range);
        self.reduced = false;
    }

    pub fn size(&self) -> usize {
        self.ranges.iter().map(|r| r.size()).sum()
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn ranges(&self) -> &[MemoryRange] {
        &self.ranges
    }

    pub fn reduce(&mut self, min_gap_size: u32, max_block_size: usize) {
        if self.reduced {
            return;
        }
        self.ranges.sort_by_key(|r| r.start);
        let mut output_len = 0;
        for input_index in 0..self.ranges.len() {
            let current = self.ranges[input_index];
            if output_len > 0
                && self.ranges[output_len - 1]
                    .next
                    .saturating_add(min_gap_size)
                    >= current.start
            {
                self.ranges[output_len - 1].next =
                    self.ranges[output_len - 1].next.max(current.next);
            } else {
                self.ranges[output_len] = current;
                output_len += 1;
            }
        }
        self.ranges.truncate(output_len);
        self.reduced = true;
        if max_block_size == 0 {
            return;
        }
        let ranges = std::mem::take(&mut self.ranges);
        self.ranges = Vec::with_capacity(ranges.len());
        for range in ranges {
            let mut start = range.start;
            while (range.next - start) as usize > max_block_size {
                let next = start + max_block_size as u32;
                self.ranges.push(MemoryRange { start, next });
                start = next;
            }
            self.ranges.push(MemoryRange {
                start,
                next: range.next,
            });
        }
    }

    pub fn find_range_index(&mut self, address: u32, size: usize) -> i32 {
        self.reduce(0, 0);
        if self.ranges.is_empty() || address < self.ranges[0].start {
            return -1;
        }
        if address >= self.ranges[self.ranges.len() - 1].next {
            return -1;
        }
        let mut num = 0usize;
        let mut num2 = self.ranges.len();
        while num < num2 {
            let mid = num + (num2 - num) / 2;
            if self.ranges[mid].start <= address {
                num = mid + 1;
            } else {
                num2 = mid;
            }
        }
        let idx = num.saturating_sub(1);
        if self.ranges[idx].next as usize >= address as usize + size {
            idx as i32
        } else {
            -1
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeasurementAccessData {
    pub memory_ranges: MemoryRangeList,
    pub data_chunks: Vec<Vec<u8>>,
}

impl MeasurementAccessData {
    pub fn new(measurements: &[Measurement]) -> Result<Self> {
        let mut memory_ranges = MemoryRangeList::new();
        for m in measurements {
            let address = m.addr.address.unwrap_or(u32::MAX);
            let next = address.wrapping_add(memory_size_of(m)? as u32);
            memory_ranges.add(MemoryRange {
                start: address,
                next,
            });
        }
        memory_ranges.reduce(0, 0);
        let data_chunks = memory_ranges
            .ranges()
            .iter()
            .map(|r| vec![0u8; r.size()])
            .collect();
        Ok(MeasurementAccessData {
            memory_ranges,
            data_chunks,
        })
    }

    pub fn get_data(&mut self, address: u32, size: usize) -> Option<Vec<u8>> {
        let (data, offset) = self.get_data_ref(address, size)?;
        Some(data[offset..offset + size].to_vec())
    }

    pub fn get_data_ref(&mut self, address: u32, size: usize) -> Option<(&[u8], usize)> {
        let num = self.memory_ranges.find_range_index(address, size);
        if num < 0 {
            return None;
        }
        let range = self.memory_ranges.ranges[num as usize];
        if (range.next as usize) < address as usize + size {
            return None;
        }
        Some((
            &self.data_chunks[num as usize],
            (address - range.start) as usize,
        ))
    }

    pub fn set_data(&mut self, address: u32, data: &[u8]) -> bool {
        let num = self.memory_ranges.find_range_index(address, data.len());
        if num < 0 {
            return false;
        }
        let range = self.memory_ranges.ranges[num as usize];
        if (range.next as usize) < address as usize + data.len() {
            return false;
        }
        let offset = (address - range.start) as usize;
        self.data_chunks[num as usize][offset..offset + data.len()].copy_from_slice(data);
        true
    }

    /// `sizeof(DataType) * (y * MatrixDim[0] + x)`.
    pub fn address_offset_of(m: &Measurement, x: i32, y: i32) -> Result<u32> {
        if x < 0 || m.matrix_dim.is_none() {
            return Ok(0);
        }
        let dims = m.matrix_dim.as_deref().unwrap_or(&[]);
        let num = *dims.first().unwrap_or(&0);
        let offset = size_in_byte(m.data_type)? as i64 * (y as i64 * num as i64 + x as i64);
        Ok(offset as u32)
    }

    /// `SIGN_EXTEND`).
    pub fn bit_operation_of(m: &Measurement) -> BitOperation {
        let mask = m.bit_mask.unwrap_or(u64::MAX);
        for child in &m.children {
            let u = match child {
                autors_a2l::model::measurement::MeasurementChild::Unsupported(u)
                    if u.keyword.eq_ignore_ascii_case("BIT_OPERATION") =>
                {
                    u
                }
                _ => continue,
            };
            let mut shift = 0i32;
            let mut sign_extend = false;
            let texts: Vec<&str> = unsupported_param_texts(u);
            let mut i = 0;
            while i < texts.len() {
                match texts[i] {
                    "RIGHT_SHIFT" => {
                        shift = texts.get(i + 1).and_then(|t| t.parse().ok()).unwrap_or(0);
                        i += 1;
                    }
                    "LEFT_SHIFT" => {
                        shift = -texts.get(i + 1).and_then(|t| t.parse().ok()).unwrap_or(0);
                        i += 1;
                    }
                    "SIGN_EXTEND" => sign_extend = true,
                    _ => {}
                }
                i += 1;
            }
            return BitOperation {
                bit_mask: mask,
                shift_count: shift,
                sign_extend,
            };
        }
        BitOperation::new(mask)
    }

    /// [`MeasurementAccessData::get_data`]).
    pub fn get_raw_value(&mut self, m: &Measurement, x: i32, y: i32) -> Option<f64> {
        let size = size_in_byte(m.data_type).ok()?;
        let address = m
            .addr
            .address
            .unwrap_or(u32::MAX)
            .wrapping_add(Self::address_offset_of(m, x, y).ok()?);
        let num = self.memory_ranges.find_range_index(address, size);
        if num < 0 {
            return None;
        }
        let range = self.memory_ranges.ranges()[num as usize];
        if (range.next as usize) < address as usize + size {
            return None;
        }
        let offset = (address - range.start) as usize;
        let bits = Self::bit_operation_of(m);
        get_single_raw_value(
            &self.data_chunks[num as usize],
            offset,
            m.data_type,
            m.conv.byte_order,
            Some(&bits),
        )
        .ok()
    }

    pub fn get_phys_value(
        &mut self,
        m: &Measurement,
        conv: &Conversion,
        x: i32,
        y: i32,
    ) -> Option<f64> {
        let raw = self.get_raw_value(m, x, y)?;
        conv.to_physical(raw).ok()
    }

    pub fn set_phys_value(
        &mut self,
        m: &Measurement,
        conv: &Conversion,
        value: f64,
        x: i32,
        y: i32,
    ) -> bool {
        let Ok(raw) = conv.to_raw(m.data_type, value) else {
            return false;
        };
        let Ok(buffer) = get_single_raw_value_buffer(raw, m.data_type, m.conv.byte_order) else {
            return false;
        };
        let Ok(offset) = Self::address_offset_of(m, x, y) else {
            return false;
        };
        let address = m.addr.address.unwrap_or(u32::MAX).wrapping_add(offset);
        self.set_data(address, &buffer)
    }
}

fn memory_size_of(m: &Measurement) -> Result<usize> {
    let array_size: i64 = m
        .matrix_dim
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|&d| i64::from(d))
        .product();
    Ok(size_in_byte(m.data_type)? * array_size as usize)
}

fn unsupported_param_texts(u: &autors_a2l::model::unsupported::UnsupportedNode) -> Vec<&str> {
    u.items
        .iter()
        .filter_map(|item| match item {
            autors_a2l::block::Item::Param(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::model::compu::{CompuTabBase, RationalCoeffs};

    fn coeffs(c: [f64; 6]) -> RationalCoeffs {
        RationalCoeffs { coeffs: c }
    }

    fn linear_cm() -> CompuMethod {
        // COEFFS_LINEAR 0.5 2.0 → phys = 0.5 * raw + 2
        CompuMethod {
            conversion_type: ConversionType::LINEAR,
            coeffs: coeffs([0.0, 2.0, -4.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        }
    }

    fn tab3() -> CompuTab {
        CompuTab {
            base: CompuTabBase {
                conversion_type: ConversionType::TAB_NOINTP,
                ..Default::default()
            },
            values: vec![(0.0, 0.0), (1.0, 10.0), (2.0, 20.0)],
            default_value_numeric: None,
        }
    }

    #[test]
    #[allow(clippy::excessive_precision)]
    fn format_fixed_golden() {
        assert_eq!(format_fixed(2.675, 2), "2.68");
        assert_eq!(format_fixed(0.125, 2), "0.13");
        assert_eq!(format_fixed(0.375, 2), "0.38");
        assert_eq!(format_fixed(1.005, 2), "1.01");
        assert_eq!(format_fixed(0.145, 2), "0.15");
        assert_eq!(format_fixed(0.155, 2), "0.16");
        assert_eq!(format_fixed(-0.125, 2), "-0.13");
        assert_eq!(format_fixed(5.05, 2), "5.05");
        assert_eq!(format_fixed(2.5, 0), "3");
        assert_eq!(format_fixed(0.5, 0), "1");
        assert_eq!(format_fixed(-2.5, 0), "-3");
        assert_eq!(format_fixed(5.5, 0), "6");
        assert_eq!(format_fixed(0.15, 1), "0.2");
        assert_eq!(format_fixed(0.25, 1), "0.3");
        assert_eq!(format_fixed(5.55, 1), "5.6");
        assert_eq!(format_fixed(0.1, 17), "0.10000000000000000");
        assert_eq!(format_fixed(1.0 / 3.0, 20), "0.33333333333333300000");
        assert_eq!(format_fixed(1.123456789012345678, 15), "1.123456789012350");
        assert_eq!(format_fixed(1e20, 2), "100000000000000000000.00");
        assert_eq!(format_fixed(0.0, 2), "0.00");
        assert_eq!(format_fixed(0.0, 0), "0");
        assert_eq!(format_fixed(5.0, 2), "5.00");
        assert_eq!(format_fixed(-1.5e-10, 3), "0.000");
        assert_eq!(format_fixed(f64::NAN, 2), "NaN");
        assert_eq!(format_fixed(f64::INFINITY, 2), "∞");
        assert_eq!(format_fixed(f64::NEG_INFINITY, 0), "-∞");
    }

    // ---- round_bankers(golden:Math.Round) ----

    #[test]
    fn round_bankers_golden() {
        assert_eq!(round_bankers(0.5, 0), 0.0);
        assert_eq!(round_bankers(1.5, 0), 2.0);
        assert_eq!(round_bankers(2.5, 0), 2.0);
        assert_eq!(round_bankers(3.5, 0), 4.0);
        assert_eq!(round_bankers(-0.5, 0), 0.0);
        assert!(round_bankers(-0.5, 0).is_sign_negative());
        assert_eq!(round_bankers(0.125, 2), 0.12);
        assert_eq!(round_bankers(0.375, 2), 0.38);
        assert_eq!(round_bankers(5.5, 0), 6.0);
        assert_eq!(round_bankers(4.5, 0), 4.0);
    }

    #[test]
    fn rational_golden() {
        let id = CompuMethod::default();
        let conv = Conversion::new(&id);
        assert_eq!(conv.to_physical(42.0).unwrap(), 42.0);
        assert_eq!(conv.to_raw(DataType::UByte, 42.0).unwrap(), 42.0);

        let cm = linear_cm();
        let conv = Conversion::new(&cm);
        assert_eq!(conv.to_physical(10.0).unwrap(), 7.0);
        assert_eq!(conv.to_raw(DataType::UByte, 7.0).unwrap(), 10.0);
        assert_eq!(conv.to_raw(DataType::Float32Ieee, 6.9).unwrap(), 9.8);
        assert_eq!(conv.get_min_increment(2, DataType::UByte).unwrap(), 0.5);
        assert_eq!(
            conv.get_min_increment(2, DataType::Float32Ieee).unwrap(),
            0.01
        );
        let idc = Conversion::new(&id);
        assert_eq!(
            idc.get_min_increment(3, DataType::Float32Ieee).unwrap(),
            0.001
        );

        let rat = CompuMethod {
            conversion_type: ConversionType::RAT_FUNC,
            coeffs: coeffs([0.0, 2.0, 1.0, 0.0, 1.0, 3.0]),
            ..Default::default()
        };
        let conv = Conversion::new(&rat);
        assert_eq!(conv.to_physical(1.0).unwrap(), 2.0);
        assert_eq!(conv.to_physical(4.0).unwrap(), -5.5);
        assert_eq!(conv.to_raw(DataType::Float64Ieee, 1.0).unwrap(), 0.75);
        assert_eq!(conv.to_raw(DataType::UByte, 1.5).unwrap(), 1.0);

        let rat2 = CompuMethod {
            conversion_type: ConversionType::RAT_FUNC,
            coeffs: coeffs([0.0, 2.0, 1.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        };
        let conv = Conversion::new(&rat2);
        assert_eq!(conv.to_physical(1.0).unwrap(), 0.0);
        assert_eq!(conv.to_raw(DataType::UByte, 3.0).unwrap(), 7.0);
        assert_eq!(conv.to_raw(DataType::UByte, 2.25).unwrap(), 6.0);
        assert_eq!(conv.to_raw(DataType::UByte, 1.75).unwrap(), 4.0);
    }

    #[test]
    fn rational_edge_cases() {
        let weird = CompuMethod {
            conversion_type: ConversionType::RAT_FUNC,
            coeffs: coeffs([1.0, 0.0, 1.0, 1.0, 0.0, 1.0]),
            ..Default::default()
        };
        let conv = Conversion::new(&weird);
        let v = conv.to_physical(3.0).unwrap();
        assert_eq!(v, 3.0);
        let cm = linear_cm();
        assert!(Conversion::new(&cm).to_physical(f64::NAN).unwrap().is_nan());
        assert!(Conversion::new(&cm)
            .to_raw(DataType::UByte, f64::NAN)
            .unwrap()
            .is_nan());
        let form = CompuMethod {
            conversion_type: ConversionType::FORM,
            ..Default::default()
        };
        assert!(Conversion::new(&form).to_physical(1.0).is_err());
        assert!(Conversion::new(&form).to_raw(DataType::UByte, 1.0).is_err());
        let form_ok = CompuMethod {
            conversion_type: ConversionType::FORM,
            inline_formula: Some(autors_a2l::model::compu::Formula {
                formula: "X1*2+1".to_string(),
                formula_inv: Some("(X1-1)/2".to_string()),
            }),
            ..Default::default()
        };
        let conv = Conversion::new(&form_ok);
        assert_eq!(conv.to_physical(3.0).unwrap(), 7.0);
        assert_eq!(conv.to_raw(DataType::UByte, 7.0).unwrap(), 3.0);
    }

    // ---- COMPU_TAB(golden:TabCoeffs.toPhysical/toRaw) ----

    #[test]
    fn tab_golden() {
        let tab = tab3();
        assert_eq!(tab_to_physical(ConversionType::TAB_INTP, &tab, 0.5), 5.0);
        assert_eq!(tab_to_physical(ConversionType::TAB_INTP, &tab, 3.0), 20.0);
        assert_eq!(tab_to_physical(ConversionType::TAB_INTP, &tab, -1.0), 0.0);
        assert_eq!(
            tab_to_raw(ConversionType::TAB_INTP, &tab, DataType::UByte, 15.0),
            1.5
        );
        assert_eq!(
            tab_to_raw(ConversionType::TAB_INTP, &tab, DataType::UByte, 99.0),
            2.0
        );
        assert_eq!(tab_to_physical(ConversionType::TAB_NOINTP, &tab, 1.0), 10.0);
        assert_eq!(tab_to_physical(ConversionType::TAB_NOINTP, &tab, 1.4), 10.0);
        assert!(tab_to_physical(ConversionType::TAB_NOINTP, &tab, 9.0).is_nan());
        let tab_def = CompuTab {
            default_value_numeric: Some(-1.0),
            ..tab3()
        };
        assert_eq!(
            tab_to_physical(ConversionType::TAB_NOINTP, &tab_def, 9.0),
            -1.0
        );
        assert_eq!(
            tab_to_raw(ConversionType::TAB_NOINTP, &tab, DataType::UByte, 10.0),
            1.0
        );
        assert_eq!(
            tab_to_raw(ConversionType::TAB_NOINTP, &tab, DataType::UByte, 11.0),
            0.0
        );
        assert_eq!(tab_to_physical(ConversionType::TAB_NOINTP, &tab, 1.5), 20.0);
        assert_eq!(tab_to_physical(ConversionType::TAB_NOINTP, &tab, 2.5), 20.0);
    }

    // ---- COMPU_VTAB / COMPU_VTAB_RANGE(golden) ----

    #[test]
    fn vtab_golden() {
        let mut vt = CompuVtab::default();
        vt.verbs.insert(0, "off".into());
        vt.verbs.insert(1, "on".into());
        assert_eq!(vtab_to_physical(&vt, 1.0), "on");
        assert_eq!(vtab_to_physical(&vt, 5.0), "5");
        vt.base.default_value = Some("n/a".into());
        assert_eq!(vtab_to_physical(&vt, 5.0), "n/a");
        assert_eq!(vtab_to_physical(&vt, f64::NAN), "n/a");
        assert_eq!(vtab_to_raw(&vt, "on"), 1.0);
        assert_eq!(vtab_to_raw(&vt, "x"), 0.0);
        assert_eq!(vtab_to_raw(&vt, ""), 0.0);

        let mut vtr = CompuVtabRange::default();
        vtr.verbs.insert("low".into(), vec![(0.0, 10.0)]);
        vtr.verbs.insert("high".into(), vec![(10.5, 20.0)]);
        assert_eq!(vtab_range_to_physical(&vtr, 5.0), "low");
        assert_eq!(vtab_range_to_physical(&vtr, 10.2), "");
        assert_eq!(vtab_range_to_physical(&vtr, 99.0), "");
        vtr.base.default_value = Some("def".into());
        assert_eq!(vtab_range_to_physical(&vtr, 99.0), "def");
        assert_eq!(vtab_range_to_raw(&vtr, "high"), 10.5);
        assert_eq!(vtab_range_to_raw(&vtr, "x"), 0.0);
    }

    // ---- to_string_value(golden:A2LCONVERSION_REF.toStringValue) ----

    #[test]
    #[allow(clippy::approx_constant)]
    fn to_string_value_golden() {
        let cm = CompuMethod::default();
        let conv = Conversion::new(&cm);
        let tsv = |v: f64, f: ValueObjectFormat, dt: DataType, dc: i32| {
            conv.to_string_value(v, f, dt, dc, f64::MIN, f64::MAX)
                .unwrap()
                .0
        };
        assert_eq!(
            tsv(
                3.14159,
                ValueObjectFormat::Physical,
                DataType::Float32Ieee,
                2
            ),
            "3.14"
        );
        assert_eq!(
            tsv(255.0, ValueObjectFormat::Raw, DataType::UWord, 2),
            "255"
        );
        assert_eq!(
            tsv(255.0, ValueObjectFormat::RawHex, DataType::UWord, 2),
            "0xFF"
        );
        assert_eq!(
            tsv(5.0, ValueObjectFormat::RawBin, DataType::UByte, 2),
            "00000101"
        );
        assert_eq!(tsv(-3.0, ValueObjectFormat::Raw, DataType::SByte, 2), "253");
        assert_eq!(
            tsv(f64::NAN, ValueObjectFormat::Raw, DataType::UWord, 2),
            "NaN"
        );
        assert_eq!(
            tsv(
                3.14159,
                ValueObjectFormat::Physical,
                DataType::Float32Ieee,
                -1
            ),
            "3.14159"
        );
        assert_eq!(
            tsv(1.0, ValueObjectFormat::Raw, DataType::Float32Ieee, 0),
            "1065353216"
        );
        assert_eq!(
            tsv(1.0, ValueObjectFormat::RawHex, DataType::Float32Ieee, 0),
            "0x3F800000"
        );
        assert_eq!(
            tsv(1.0, ValueObjectFormat::RawBin, DataType::Float32Ieee, 0),
            "00111111100000000000000000000000"
        );
        assert_eq!(
            tsv(1.0, ValueObjectFormat::Raw, DataType::Float64Ieee, 0),
            "4607182418800017408"
        );
        assert_eq!(
            tsv(-1.0, ValueObjectFormat::Raw, DataType::ULong, 0),
            "4294967295"
        );
        assert_eq!(
            tsv(-1.0, ValueObjectFormat::Raw, DataType::UWord, 0),
            "65535"
        );
        assert_eq!(
            tsv(255.7, ValueObjectFormat::Raw, DataType::UByte, 0),
            "255"
        );
        assert_eq!(
            tsv(0.0, ValueObjectFormat::RawHex, DataType::UByte, 0),
            "0x0"
        );
        assert_eq!(
            tsv(3.0, ValueObjectFormat::RawBin, DataType::UWord, 0),
            "0000000000000011"
        );
        assert_eq!(
            tsv(0.125, ValueObjectFormat::Physical, DataType::UByte, 2),
            "0.12"
        );
        assert_eq!(
            tsv(2.5, ValueObjectFormat::Physical, DataType::UByte, 0),
            "2"
        );
        assert_eq!(
            tsv(-2.5, ValueObjectFormat::Physical, DataType::UByte, 0),
            "-2"
        );
        assert_eq!(
            tsv(0.375, ValueObjectFormat::Physical, DataType::UByte, 2),
            "0.38"
        );
        assert_eq!(
            tsv(1e20, ValueObjectFormat::Physical, DataType::UByte, 2),
            "100000000000000000000.00"
        );
        assert_eq!(
            tsv(-1.5e-10, ValueObjectFormat::Physical, DataType::UByte, 3),
            "0.000"
        );
        let (_, lo, hi) = conv
            .to_string_value(
                5.0,
                ValueObjectFormat::Physical,
                DataType::UByte,
                0,
                10.0,
                20.0,
            )
            .unwrap();
        assert!(lo && !hi);
        let (_, lo, hi) = conv
            .to_string_value(
                25.0,
                ValueObjectFormat::Physical,
                DataType::UByte,
                0,
                10.0,
                20.0,
            )
            .unwrap();
        assert!(!lo && hi);
        let mut vt = CompuVtab::default();
        vt.verbs.insert(1, "on".into());
        let cmv = CompuMethod {
            conversion_type: ConversionType::TAB_VERB,
            ..Default::default()
        };
        let conv = Conversion::with_tab(&cmv, CompuTabRef::Vtab(&vt));
        assert_eq!(
            conv.to_string_value(
                1.0,
                ValueObjectFormat::Physical,
                DataType::UByte,
                0,
                f64::MIN,
                f64::MAX
            )
            .unwrap()
            .0,
            "on"
        );
    }

    #[test]
    fn decimal_count_and_parse() {
        assert_eq!(get_decimal_count("%6.3").unwrap(), 3);
        assert_eq!(get_decimal_count("%").unwrap(), 0);
        assert_eq!(get_decimal_count("").unwrap(), 0);
        assert!(get_decimal_count("%6.x").is_err());
        assert_eq!(parse2_double_val("0x1F").unwrap(), 31.0);
        assert_eq!(parse2_double_val("0XFF").unwrap(), 255.0);
        assert_eq!(parse2_double_val("3.5").unwrap(), 3.5);
        assert_eq!(parse2_double_val(" -2.5e2 ").unwrap(), -250.0);
        assert!(parse2_double_val("abc").is_err());
    }

    #[test]
    fn raw_value_io() {
        let buf = [0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x80, 0x3F];
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::UWord, ByteOrder::MSB_LAST, None).unwrap(),
            513.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::UWord, ByteOrder::MSB_FIRST, None).unwrap(),
            258.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::SWord, ByteOrder::MSB_LAST, None).unwrap(),
            513.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 4, DataType::Float32Ieee, ByteOrder::MSB_LAST, None)
                .unwrap(),
            1.0
        );
        assert!(get_single_raw_value(&buf, 7, DataType::ULong, ByteOrder::MSB_LAST, None).is_err());
        let b = get_single_raw_value_buffer(513.0, DataType::UWord, ByteOrder::MSB_LAST).unwrap();
        assert_eq!(b, [0x01, 0x02]);
        let b = get_single_raw_value_buffer(513.0, DataType::UWord, ByteOrder::MSB_FIRST).unwrap();
        assert_eq!(b, [0x02, 0x01]);
        let b = get_single_raw_value_buffer(-2.0, DataType::SByte, ByteOrder::MSB_LAST).unwrap();
        assert_eq!(b, [0xFE]);
        let b =
            get_single_raw_value_buffer(1.0, DataType::Float32Ieee, ByteOrder::MSB_FIRST).unwrap();
        assert_eq!(b, [0x3F, 0x80, 0x00, 0x00]);
        assert!(
            get_single_raw_value_buffer(1.0, DataType::Float16Ieee, ByteOrder::MSB_LAST).is_err()
        );
    }

    #[test]
    fn bit_operation_mask_shift() {
        let bo = BitOperation::new(0xFF00);
        assert_eq!(bo.shift_count, 8);
        assert!(!bo.sign_extend);
        let buf = [0x34, 0x12];
        let v =
            get_single_raw_value(&buf, 0, DataType::UWord, ByteOrder::MSB_LAST, Some(&bo)).unwrap();
        assert_eq!(v, 0x12 as f64);
        let bo_full = BitOperation::new(u64::MAX);
        assert_eq!(bo_full.shift_count, 0);
        let v = get_single_raw_value(
            &buf,
            0,
            DataType::UWord,
            ByteOrder::MSB_LAST,
            Some(&bo_full),
        )
        .unwrap();
        assert_eq!(v, 0x1234 as f64);
    }
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests_values {
    use super::*;
    use autors_a2l::model::enums::AxisValueType;

    fn linear_cm() -> CompuMethod {
        CompuMethod {
            conversion_type: ConversionType::LINEAR,
            coeffs: RationalCoeffs {
                coeffs: [0.0, 2.0, -4.0, 0.0, 0.0, 1.0],
            },
            ..Default::default()
        }
    }

    fn ident_ctx<'a>(conv: &'a Conversion<'a>) -> SingleValueContext<'a> {
        SingleValueContext {
            conversion: conv,
            data_type: DataType::UByte,
            axes: Vec::new(),
        }
    }

    fn char_ref(char_type: CharacteristicType, lo: f64, hi: f64) -> CharacteristicRef {
        let mut ch = Characteristic {
            char_type,
            ..Default::default()
        };
        ch.conv.lower_limit = lo;
        ch.conv.upper_limit = hi;
        CharacteristicRef::Char(ch)
    }

    fn inc_ctx<'a>(conv: &'a Conversion<'a>) -> IncrementContext<'a> {
        IncrementContext {
            conversion: conv,
            data_type: DataType::UByte,
            lower_limit: 0.0,
            upper_limit: 100.0,
        }
    }

    #[test]
    fn single_value_to_string_golden() {
        let mut sv = SingleValue::new();
        sv.base.value = ValueData::Scalar(5.0);
        sv.base.decimal_count = 2;
        sv.base.unit = "V".into();
        assert_eq!(sv.to_string(), "Value[V]=5.00");
        sv.base.unit.clear();
        assert_eq!(sv.to_string(), "Value=5.00");
        sv.base.value = ValueData::Scalar(5.5);
        sv.base.decimal_count = 0;
        assert_eq!(sv.to_string(), "Value=6");
    }

    #[test]
    fn ascii_value_to_string() {
        let mut av = AsciiValue::new();
        av.base.value = ValueData::Text("hello".into());
        assert_eq!(av.to_string(), "hello");
    }

    #[test]
    fn valblk_to_string_golden() {
        let mut vb = ValBlkValue::new();
        vb.base.value =
            ValueData::array(vec![9], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]).unwrap();
        vb.base.decimal_count = 1;
        vb.base.unit = "Nm".into();
        assert_eq!(
            vb.to_string(),
            "Values[Nm]=\n1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0\n9.0"
        );
        vb.base.unit.clear();
        assert_eq!(
            vb.to_string(),
            "Values=\n1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0\n9.0"
        );
        vb.base.value = ValueData::Texts(vec!["off".into(), "on".into()]);
        assert_eq!(vb.to_string(), "Values=\noff,on");
    }

    #[test]
    fn curve_to_string_golden() {
        let mut cu = CurveValue::new();
        cu.base.value = ValueData::array(vec![3], vec![10.0, 20.0, 30.0]).unwrap();
        cu.base.axis_value = vec![vec![100.0, 200.0, 300.0]];
        cu.base.decimal_count = 1;
        cu.base.decimal_count_axis = vec![0];
        cu.base.unit = "Nm".into();
        cu.base.unit_axis = vec!["rpm".into()];
        assert_eq!(
            cu.to_string(),
            "Axis[rpm]\t100,200,300\nValues[Nm]\t10.0,20.0,30.0"
        );
        cu.base.unit.clear();
        cu.base.unit_axis = vec![String::new()];
        assert_eq!(cu.to_string(), "Axis\t100,200,300\nValues\t10.0,20.0,30.0");
    }

    #[test]
    fn map_to_string_golden() {
        let mut mv = MapValue::new();
        mv.base.value = ValueData::array(vec![2, 2], vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        mv.base.axis_value = vec![vec![0.5, 1.5], vec![7.0, 8.0]];
        mv.base.decimal_count = 1;
        mv.base.decimal_count_axis = vec![0, 2];
        mv.base.unit = "K".into();
        mv.base.unit_axis = vec!["x-u".into(), "y-u".into()];
        assert_eq!(
            mv.to_string(),
            "X[x-u]\t\t1,2\nY[y-u]\t\t[K]\n7.00\t\t1.0,3.0\n8.00\t\t2.0,4.0"
        );
        mv.base.unit.clear();
        mv.base.unit_axis = vec![String::new(), "y-u".into()];
        assert_eq!(
            mv.to_string(),
            "X\t\t1,2\nY[y-u]\n7.00\t\t1.0,3.0\n8.00\t\t2.0,4.0"
        );
        mv.base.unit = "K".into();
        mv.base.unit_axis = vec!["x-u".into(), String::new()];
        assert_eq!(
            mv.to_string(),
            "X[x-u]\t\t1,2\nY\t\t[K]\n7.00\t\t1.0,3.0\n8.00\t\t2.0,4.0"
        );
    }

    #[test]
    fn cube_to_string_empty() {
        assert_eq!(CuboidValue::new().to_string(), "");
        assert_eq!(Cube4Value::new().to_string(), "");
        assert_eq!(Cube5Value::new().to_string(), "");
    }

    // ---- toSingleValue ----

    #[test]
    fn to_single_value_scalar() {
        let cm = linear_cm();
        let conv = Conversion::new(&cm);
        let ctx = SingleValueContext {
            conversion: &conv,
            data_type: DataType::UByte,
            axes: Vec::new(),
        };
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(10.0);
        sv.base.decimal_count = 2;
        assert_eq!(sv.base.to_single_value(true, &ctx).unwrap(), "10.00");
        assert_eq!(sv.base.to_single_value(false, &ctx).unwrap(), "10");
        sv.base.value_format = ValueObjectFormat::Raw;
        assert_eq!(sv.base.to_single_value(true, &ctx).unwrap(), "10");
        let mut av = AsciiValue::with_characteristic(char_ref(CharacteristicType::ASCII, 0.0, 0.0));
        av.base.value = ValueData::Text("abc".into());
        let idm = CompuMethod::default();
        let iconv = Conversion::new(&idm);
        let ictx = ident_ctx(&iconv);
        assert_eq!(av.base.to_single_value(true, &ictx).unwrap(), "abc");
        let mut vb =
            ValBlkValue::with_characteristic(char_ref(CharacteristicType::VAL_BLK, 0.0, 100.0));
        vb.base.value = ValueData::array(vec![2], vec![1.0, 2.0]).unwrap();
        assert_eq!(vb.base.to_single_value(true, &ictx).unwrap(), "");
    }

    #[test]
    fn to_single_value_at_labels() {
        let idm = CompuMethod::default();
        let iconv = Conversion::new(&idm);
        let ctx = SingleValueContext {
            conversion: &iconv,
            data_type: DataType::UByte,
            axes: vec![
                AxisContext {
                    conversion: &iconv,
                    data_type: DataType::UByte,
                },
                AxisContext {
                    conversion: &iconv,
                    data_type: DataType::UWord,
                },
            ],
        };
        let mut cu =
            CurveValue::with_characteristic(char_ref(CharacteristicType::CURVE, 0.0, 100.0));
        cu.base.value = ValueData::array(vec![3], vec![10.0, 20.0, 30.0]).unwrap();
        cu.base.axis_value = vec![vec![100.0, 200.0, 300.0]];
        cu.base.decimal_count = 1;
        cu.base.decimal_count_axis = vec![0];
        cu.base.unit = "Nm".into();
        cu.base.unit_axis = vec!["rpm".into()];
        assert_eq!(
            cu.base.to_single_value_at(-1, -1, 0, true, &ctx).unwrap(),
            "rpm"
        );
        cu.base.unit_axis = vec![String::new()];
        assert_eq!(
            cu.base.to_single_value_at(-1, -1, 0, true, &ctx).unwrap(),
            " "
        );
        assert_eq!(
            cu.base.to_single_value_at(-1, 0, 0, true, &ctx).unwrap(),
            "Nm"
        );
        assert_eq!(
            cu.base.to_single_value_at(1, -1, 0, true, &ctx).unwrap(),
            "200"
        );
        assert_eq!(
            cu.base.to_single_value_at(5, -1, 0, true, &ctx).unwrap(),
            ""
        );
        assert_eq!(
            cu.base.to_single_value_at(1, 0, 0, true, &ctx).unwrap(),
            "20.0"
        );
        let mut mv = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        mv.base.value = ValueData::array(vec![2, 2], vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        mv.base.axis_value = vec![vec![0.5, 1.5], vec![7.0, 8.0]];
        mv.base.decimal_count = 1;
        mv.base.decimal_count_axis = vec![0, 2];
        mv.base.unit_axis = vec!["x-u".into(), "y-u".into()];
        assert_eq!(
            mv.base.to_single_value_at(-1, -1, 0, true, &ctx).unwrap(),
            "[x-u/y-u]"
        );
        assert_eq!(
            mv.base.to_single_value_at(0, -1, 0, true, &ctx).unwrap(),
            "0"
        );
        assert_eq!(
            mv.base.to_single_value_at(-1, 1, 0, true, &ctx).unwrap(),
            "8.00"
        );
        assert_eq!(
            mv.base.to_single_value_at(1, 0, 0, true, &ctx).unwrap(),
            "3.0"
        );
        assert_eq!(
            mv.base.to_single_value_at(9, -1, 0, true, &ctx).unwrap(),
            ""
        );
        assert_eq!(
            mv.base.to_single_value_at(-1, 9, 0, true, &ctx).unwrap(),
            ""
        );
        assert_eq!(
            mv.base.to_single_value_at(-1, 1, 0, false, &ctx).unwrap(),
            "8"
        );
    }

    #[test]
    fn fnc_value_access() {
        let mut mv = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        mv.base.value = ValueData::array(vec![2, 2], vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(mv.base.get_fnc_value(1, 0, 0).unwrap(), 3.0);
        assert_eq!(mv.base.get_fnc_value(0, 1, 0).unwrap(), 2.0);
        mv.base.set_fnc_value(9.0, 1, 1, 0).unwrap();
        assert_eq!(mv.base.get_fnc_value(1, 1, 0).unwrap(), 9.0);
        assert_eq!(mv.base.fnc_values().unwrap(), vec![1.0, 3.0, 2.0, 9.0]);

        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.set_fnc_scalar(3.5).unwrap();
        assert_eq!(sv.base.get_fnc_value(0, 0, 0).unwrap(), 3.5);
        assert_eq!(sv.base.fnc_values().unwrap(), vec![3.5]);

        let mut av = AsciiValue::with_characteristic(char_ref(CharacteristicType::ASCII, 0.0, 0.0));
        av.base.value = ValueData::Text("AB".into());
        assert_eq!(av.base.get_fnc_value(1, 0, 0).unwrap(), 66.0);

        assert!(mv.base.get_fnc_value(5, 0, 0).is_err());
        // CUBOID
        let mut cb =
            CuboidValue::with_characteristic(char_ref(CharacteristicType::CUBOID, 0.0, 100.0));
        cb.base.value = ValueData::array(vec![2, 1, 2], vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(cb.base.get_fnc_value(1, 0, 1).unwrap(), 4.0);
        assert_eq!(cb.base.fnc_values().unwrap(), vec![1.0, 3.0, 2.0, 4.0]);
        let mut c4 =
            Cube4Value::with_characteristic(char_ref(CharacteristicType::CUBE_4, 0.0, 100.0));
        c4.base.value = ValueData::array(vec![1, 1, 1, 1], vec![1.0]).unwrap();
        assert!(c4.base.fnc_values().is_err());
        assert!(c4.base.get_fnc_value(0, 0, 0).is_err());
    }

    #[test]
    fn compare_to_semantics() {
        let mut a =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        a.base.value = ValueData::Scalar(1.0);
        let mut b =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        b.base.value = ValueData::Scalar(2.0);
        let mut c = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        c.base.value = ValueData::array(vec![1, 1], vec![1.0]).unwrap();
        assert_eq!(a.base.compare_to(&b.base), Ordering::Less);
        assert_eq!(b.base.compare_to(&a.base), Ordering::Greater);
        assert_eq!(a.base.compare_to(&a.base.clone()), Ordering::Equal);
        assert_eq!(c.base.compare_to(&b.base), Ordering::Less);
        assert_eq!(b.base.compare_to(&c.base), Ordering::Greater);
        let d = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        assert_eq!(c.base.compare_to(&d.base), Ordering::Equal);
    }

    // ---- modify ----

    #[test]
    fn modify_funcs() {
        assert_eq!(modify_set_func(5.0, 3.0, false), 3.0);
        assert_eq!(modify_set_func(5.0, 50.0, true), 2.5);
        assert_eq!(modify_add_func(5.0, 3.0, false), 8.0);
        assert_eq!(modify_add_func(5.0, 50.0, true), 7.5);
        assert_eq!(modify_sub_func(5.0, 3.0, false), 2.0);
        assert_eq!(modify_sub_func(5.0, 50.0, true), 2.5);
        assert_eq!(modify_mul_func(5.0, 3.0, false), 15.0);
        assert_eq!(modify_mul_func(5.0, 50.0, true), 12.5);
        assert_eq!(modify_div_func(6.0, 3.0, false), 2.0);
        assert_eq!(modify_inv_func(3.0, 0.0, 10.0), 7.0);
        assert_eq!(modify_inv_func(12.0, 0.0, 10.0), -2.0);

        let mut mv = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        mv.base.value = ValueData::array(vec![2, 1], vec![1.0, 2.0]).unwrap();
        mv.base
            .modify_value_by(1.5, false, modify_add_func)
            .unwrap();
        assert_eq!(mv.base.value.as_array().unwrap().1, &[2.5, 3.5]);
        mv.base
            .modify_value_by_at(1.0, false, modify_mul_func, &[vec![1, 0]])
            .unwrap();
        assert_eq!(mv.base.value.as_array().unwrap().1, &[2.5, 3.5]);
        mv.base.modify_value_with(&mut |v, _| v * 10.0).unwrap();
        assert_eq!(mv.base.value.as_array().unwrap().1, &[25.0, 35.0]);
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(1.0);
        assert!(sv
            .base
            .modify_value_by_at(1.0, false, modify_add_func, &[vec![0]])
            .is_err());
        sv.base
            .modify_value_by(2.0, false, modify_add_func)
            .unwrap();
        assert_eq!(sv.base.value.as_scalar().unwrap(), 3.0);
    }

    // ---- increment / decrement ----

    #[test]
    fn increment_single_linear() {
        let cm = linear_cm(); // phys = 0.5 raw + 2 → min incr (UBYTE) = 0.5
        let conv = Conversion::new(&cm);
        let ctx = inc_ctx(&conv);
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(10.0);
        sv.base.decimal_count = 2;
        assert!(sv
            .base
            .increment_or_decrement_single(true, 2, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 11.0);
        sv.base.value = ValueData::Scalar(99.9);
        assert!(sv
            .base
            .increment_or_decrement_single(true, 5, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 100.0);
        assert!(!sv
            .base
            .increment_or_decrement_single(true, 5, &ctx)
            .unwrap());
        // decrement
        assert!(sv
            .base
            .increment_or_decrement_single(false, 2, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 99.0);
    }

    #[test]
    fn increment_single_tab_nointp() {
        let tab = CompuTab {
            values: vec![(0.0, 0.0), (5.0, 10.0), (9.0, 20.0)],
            ..Default::default()
        };
        let cm = CompuMethod {
            conversion_type: ConversionType::TAB_NOINTP,
            ..Default::default()
        };
        let conv = Conversion::with_tab(&cm, CompuTabRef::Tab(&tab));
        let ctx = inc_ctx(&conv);
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(10.0);
        assert!(sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 20.0);
        assert!(!sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 20.0);
        assert!(sv
            .base
            .increment_or_decrement_single(false, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 10.0);
        sv.base.value_format = ValueObjectFormat::Raw;
        sv.base.value = ValueData::Scalar(0.0);
        assert!(sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 5.0);
        sv.base.value = ValueData::Scalar(12.0);
        assert!(sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 0.0);
        sv.base.value = ValueData::Scalar(-1.0);
        assert!(sv
            .base
            .increment_or_decrement_single(false, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 9.0);
        assert!(conv.get_min_increment(0, DataType::UByte).is_err());
    }

    #[test]
    fn increment_tab_verb() {
        let mut vt = CompuVtab::default();
        vt.verbs.insert(0, "a".into());
        vt.verbs.insert(1, "b".into());
        vt.verbs.insert(3, "c".into());
        let cm = CompuMethod {
            conversion_type: ConversionType::TAB_VERB,
            ..Default::default()
        };
        let conv = Conversion::with_tab(&cm, CompuTabRef::Vtab(&vt));
        let ctx = inc_ctx(&conv);
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(1.0);
        sv.base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap();
        assert_eq!(sv.base.value.as_scalar().unwrap(), 3.0);
        assert!(!sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 3.0);
        sv.base.value = ValueData::Scalar(9.0);
        assert!(sv
            .base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(sv.base.value.as_scalar().unwrap(), 0.0);
        let mut vtr = CompuVtabRange::default();
        vtr.verbs.insert("low".into(), vec![(0.0, 10.0)]);
        vtr.verbs.insert("high".into(), vec![(10.5, 20.0)]);
        let conv = Conversion::with_tab(&cm, CompuTabRef::VtabRange(&vtr));
        let ctx = inc_ctx(&conv);
        sv.base.value = ValueData::Scalar(5.0); // → "low"
        sv.base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap();
        assert_eq!(sv.base.value.as_scalar().unwrap(), 10.5);
        sv.base
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap();
        assert_eq!(sv.base.value.as_scalar().unwrap(), 10.5);
    }

    #[test]
    fn increment_at_indexes() {
        let cm = linear_cm();
        let conv = Conversion::new(&cm);
        let ctx = inc_ctx(&conv);
        let mut vb =
            ValBlkValue::with_characteristic(char_ref(CharacteristicType::VAL_BLK, 0.0, 100.0));
        vb.base.value = ValueData::array(vec![3], vec![10.0, 20.0, 30.0]).unwrap();
        vb.base
            .increment_or_decrement_at(&[vec![0], vec![2]], true, 2, &ctx)
            .unwrap();
        assert_eq!(vb.base.value.as_array().unwrap().1, &[11.0, 20.0, 31.0]);
        let tab = CompuTab {
            values: vec![(0.0, 0.0), (1.0, 10.0), (2.0, 20.0)],
            ..Default::default()
        };
        let cm = CompuMethod {
            conversion_type: ConversionType::TAB_NOINTP,
            ..Default::default()
        };
        let conv = Conversion::with_tab(&cm, CompuTabRef::Tab(&tab));
        let ctx = inc_ctx(&conv);
        vb.base.value = ValueData::array(vec![2], vec![0.0, 10.0]).unwrap();
        vb.base
            .increment_or_decrement_at(&[vec![1]], true, 5, &ctx)
            .unwrap();
        assert_eq!(vb.base.value.as_array().unwrap().1, &[0.0, 20.0]);
    }

    #[test]
    fn increment_axis_monotony() {
        let cm = linear_cm();
        let conv = Conversion::new(&cm);
        let ctx = IncrementContext {
            conversion: &conv,
            data_type: DataType::UByte,
            lower_limit: 0.0,
            upper_limit: 1000.0,
        };
        let mut cu =
            CurveValue::with_characteristic(char_ref(CharacteristicType::CURVE, 0.0, 100.0));
        cu.base.value = ValueData::array(vec![3], vec![1.0, 2.0, 3.0]).unwrap();
        cu.base.axis_value = vec![vec![10.0, 20.0, 30.0]];
        assert!(cu
            .base
            .increment_or_decrement_axis(AxisValueType::XAxis, &[0, 2], true, 2, &ctx)
            .unwrap());
        assert_eq!(cu.base.axis_value[0], vec![11.0, 20.0, 31.0]);
        let mut cu2 =
            CurveValue::with_characteristic(char_ref(CharacteristicType::CURVE, 0.0, 100.0));
        cu2.base.value = ValueData::array(vec![3], vec![1.0, 2.0, 3.0]).unwrap();
        cu2.base.axis_value = vec![vec![10.0, 20.0, 30.0]];
        assert!(!cu2
            .base
            .increment_or_decrement_axis(AxisValueType::XAxis, &[1], false, 40, &ctx)
            .unwrap());
        assert_eq!(cu2.base.axis_value[0], vec![10.0, 20.0, 30.0]);
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(1.0);
        assert!(sv
            .base
            .increment_or_decrement_axis(AxisValueType::XAxis, &[0], true, 1, &ctx)
            .is_err());
    }

    #[test]
    fn paste_parse_ok_and_headers() {
        let mut mv = MapValue::with_characteristic(char_ref(CharacteristicType::MAP, 0.0, 100.0));
        mv.base.value = ValueData::array(vec![2, 2], vec![0.0; 4]).unwrap();
        mv.base.axis_value = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let text = "\t1\t2\r\n3\t11\t12\r\n4\t21\t22";
        let (res, values) = mv.base.can_paste_from_clipboard(Some(text), (0, 0), false);
        assert_eq!(res, PasteResult::Ok);
        let values = values.unwrap();
        assert_eq!(values, vec![vec![11.0, 21.0], vec![12.0, 22.0]]);
        assert_eq!(
            mv.base.paste_block(Some(text), (0, 0), false),
            PasteResult::Ok
        );
        assert_eq!(
            mv.base.value.as_array().unwrap().1,
            &[11.0, 21.0, 12.0, 22.0]
        );
    }

    #[test]
    fn paste_errors() {
        let mut vb =
            ValBlkValue::with_characteristic(char_ref(CharacteristicType::VAL_BLK, 0.0, 100.0));
        vb.base.value = ValueData::array(vec![2], vec![0.0, 0.0]).unwrap();
        assert_eq!(
            vb.base.paste_row(None, (0, 0), false),
            PasteResult::ErrSrcFormatNotAvailable
        );
        assert_eq!(
            vb.base.paste_row(Some("1\t2"), (0, 0), true),
            PasteResult::ErrNotSupportedByTargetValue
        );
        assert_eq!(
            vb.base.paste_row(Some("1\t2\r\n3"), (0, 0), false),
            PasteResult::ErrSrcNonQuadratic
        );
        assert_eq!(
            vb.base.paste_row(Some("1\t2\t3"), (0, 0), false),
            PasteResult::WarnDoesNotFit
        );
        assert_eq!(
            vb.base.paste_row(Some("1\tzz"), (0, 0), false),
            PasteResult::ErrSrcWrongValueFormat
        );
        assert_eq!(vb.base.paste_row(Some("7"), (1, 0), false), PasteResult::Ok);
        assert_eq!(vb.base.value.as_array().unwrap().1, &[0.0, 7.0]);
        assert_eq!(
            vb.base.paste_row(Some("0xA"), (0, 0), false),
            PasteResult::Ok
        );
        assert_eq!(vb.base.value.as_array().unwrap().1, &[10.0, 7.0]);
        let mut av = AsciiValue::with_characteristic(char_ref(CharacteristicType::ASCII, 0.0, 0.0));
        av.base.value = ValueData::Text(String::new());
        assert_eq!(
            av.base.can_paste_from_clipboard(Some("1"), (0, 0), false).0,
            PasteResult::ErrNotSupportedByTargetValue
        );
        // SingleValue
        let mut sv =
            SingleValue::with_characteristic(char_ref(CharacteristicType::VALUE, 0.0, 100.0));
        sv.base.value = ValueData::Scalar(0.0);
        assert_eq!(sv.base.paste_single(Some("42.5"), false), PasteResult::Ok);
        assert_eq!(sv.base.value.as_scalar().unwrap(), 42.5);
    }

    // ---- ValueData ----

    #[test]
    fn value_data_basics() {
        assert!(ValueData::array(vec![2, 2], vec![1.0]).is_err());
        assert!(ValueData::array(vec![], vec![]).is_err());
        let mut a = ValueData::array(vec![2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        assert_eq!(a.get(&[1, 2]).unwrap(), 6.0);
        a.set(&[0, 2], 9.0).unwrap();
        assert_eq!(a.get(&[0, 2]).unwrap(), 9.0);
        assert!(a.get(&[2, 0]).is_err());
        assert!(a.get(&[0]).is_err());
        assert_eq!(a.len(), 6);
    }

    // ---- CharValue ----

    #[test]
    fn char_value_dispatch() {
        let cv = CharValue::for_char_type(
            CharacteristicType::MAP,
            char_ref(CharacteristicType::MAP, 0.0, 100.0),
        )
        .unwrap();
        assert_eq!(cv.char_type(), CharacteristicType::MAP);
        assert!(matches!(cv, CharValue::Map(_)));
        assert!(CharValue::for_char_type(
            CharacteristicType::NotSet,
            char_ref(CharacteristicType::VALUE, 0.0, 100.0)
        )
        .is_err());
        let mut cv = CharValue::for_char_type(
            CharacteristicType::VALUE,
            char_ref(CharacteristicType::VALUE, 0.0, 100.0),
        )
        .unwrap();
        cv.base_mut().value = ValueData::Scalar(1.5);
        cv.base_mut().decimal_count = 1;
        assert_eq!(cv.to_string(), "Value=1.5");
    }
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests_access {
    use super::*;
    use autors_a2l::block::Item;
    use autors_a2l::model::measurement::MeasurementChild;
    use autors_a2l::model::unsupported::UnsupportedNode;
    use autors_a2l::token::Token;

    fn meas(address: u32, dt: DataType, dims: Option<Vec<i32>>) -> Measurement {
        let mut m = Measurement {
            data_type: dt,
            matrix_dim: dims,
            ..Default::default()
        };
        m.addr.address = Some(address);
        m
    }

    fn param(text: &str) -> Item {
        Item::Param(Token {
            text: text.to_string(),
            line: 0,
            quoted: false,
        })
    }

    #[test]
    fn memory_range_list_reduce() {
        let mut list = MemoryRangeList::new();
        list.add(MemoryRange {
            start: 100,
            next: 110,
        });
        list.add(MemoryRange {
            start: 50,
            next: 60,
        });
        list.add(MemoryRange {
            start: 105,
            next: 120,
        });
        list.add(MemoryRange {
            start: 200,
            next: 210,
        });
        list.reduce(0, 0);
        assert_eq!(
            list.ranges(),
            &[
                MemoryRange {
                    start: 50,
                    next: 60
                },
                MemoryRange {
                    start: 100,
                    next: 120
                },
                MemoryRange {
                    start: 200,
                    next: 210
                },
            ]
        );
        assert_eq!(list.size(), 40);
        let mut list2 = MemoryRangeList::new();
        list2.add(MemoryRange {
            start: 100,
            next: 110,
        });
        list2.add(MemoryRange {
            start: 115,
            next: 120,
        });
        list2.reduce(5, 0);
        assert_eq!(
            list2.ranges(),
            &[MemoryRange {
                start: 100,
                next: 120
            }]
        );
        let mut list3 = MemoryRangeList::new();
        list3.add(MemoryRange {
            start: 0,
            next: 250,
        });
        list3.reduce(0, 100);
        assert_eq!(
            list3.ranges(),
            &[
                MemoryRange {
                    start: 0,
                    next: 100
                },
                MemoryRange {
                    start: 100,
                    next: 200
                },
                MemoryRange {
                    start: 200,
                    next: 250
                },
            ]
        );
    }

    #[test]
    fn find_range_index_binary_search() {
        let mut list = MemoryRangeList::new();
        for i in 0..5u32 {
            list.add(MemoryRange {
                start: i * 10,
                next: i * 10 + 5,
            });
        }
        assert_eq!(list.find_range_index(0, 1), 0);
        assert_eq!(list.find_range_index(4, 1), 0);
        assert_eq!(list.find_range_index(5, 1), -1);
        assert_eq!(list.find_range_index(41, 4), 4);
        assert_eq!(list.find_range_index(41, 5), -1);
        assert_eq!(list.find_range_index(100, 1), -1);
        assert_eq!(list.find_range_index(0, 0), 0);
    }

    #[test]
    fn access_data_new_and_get_set() {
        let ms = [
            meas(0x1000, DataType::UWord, None),
            meas(0x1002, DataType::UByte, None),
        ];
        let mut mad = MeasurementAccessData::new(&ms).unwrap();
        assert_eq!(mad.memory_ranges.ranges().len(), 1);
        assert_eq!(mad.data_chunks.len(), 1);
        assert_eq!(mad.data_chunks[0].len(), 3);
        assert!(mad.set_data(0x1000, &[0x34, 0x12, 0xFF]));
        assert_eq!(mad.get_data(0x1000, 3).unwrap(), vec![0x34, 0x12, 0xFF]);
        assert_eq!(mad.get_data(0x1001, 2).unwrap(), vec![0x12, 0xFF]);
        assert!(mad.get_data(0x1002, 2).is_none());
        assert!(!mad.set_data(0x1002, &[1, 2]));
        assert!(mad.get_data(0x2000, 1).is_none());
    }

    #[test]
    fn access_data_raw_phys_values() {
        let mut m = meas(0x100, DataType::UWord, None);
        m.conv.byte_order = ByteOrder::MSB_FIRST;
        let cm = CompuMethod {
            conversion_type: ConversionType::LINEAR,
            coeffs: RationalCoeffs {
                coeffs: [0.0, 2.0, -4.0, 0.0, 0.0, 1.0],
            },
            ..Default::default()
        };
        let conv = Conversion::new(&cm);
        let mut mad = MeasurementAccessData::new(std::slice::from_ref(&m)).unwrap();
        assert!(mad.set_phys_value(&m, &conv, 7.0, -1, 0));
        assert_eq!(mad.get_data(0x100, 2).unwrap(), vec![0x00, 0x0A]);
        assert_eq!(mad.get_raw_value(&m, -1, 0).unwrap(), 10.0);
        assert_eq!(mad.get_phys_value(&m, &conv, -1, 0).unwrap(), 7.0);
        m.conv.byte_order = ByteOrder::MSB_LAST;
        assert!(mad.set_phys_value(&m, &conv, 9.0, -1, 0));
        assert_eq!(mad.get_data(0x100, 2).unwrap(), vec![0x0E, 0x00]);
        let m2 = meas(0x999, DataType::UByte, None);
        assert!(mad.get_raw_value(&m2, -1, 0).is_none());
        assert!(!mad.set_phys_value(&m2, &Conversion::new(&CompuMethod::default()), 1.0, -1, 0));
    }

    #[test]
    fn access_data_matrix_and_bitmask() {
        let mut m = meas(0x200, DataType::UWord, Some(vec![2, 2]));
        m.conv.byte_order = ByteOrder::MSB_LAST;
        assert_eq!(
            MeasurementAccessData::address_offset_of(&m, -1, 0).unwrap(),
            0
        );
        assert_eq!(
            MeasurementAccessData::address_offset_of(&m, 1, 1).unwrap(),
            6
        ); // 2*(1*2+1)
        let idm = CompuMethod::default();
        let conv = Conversion::new(&idm);
        let mut mad = MeasurementAccessData::new(std::slice::from_ref(&m)).unwrap();
        assert_eq!(mad.data_chunks[0].len(), 8);
        assert!(mad.set_phys_value(&m, &conv, 42.0, 1, 1));
        assert_eq!(mad.get_raw_value(&m, 1, 1).unwrap(), 42.0);
        assert_eq!(mad.get_data(0x206, 2).unwrap(), vec![42, 0]);

        // BIT_MASK 0xF0 + BIT_OPERATION RIGHT_SHIFT 4
        let mut mm = meas(0x300, DataType::UByte, None);
        mm.bit_mask = Some(0xF0);
        mm.children
            .push(MeasurementChild::Unsupported(UnsupportedNode {
                keyword: "BIT_OPERATION".into(),
                items: vec![param("RIGHT_SHIFT"), param("4"), param("SIGN_EXTEND")],
            }));
        let mut mad2 = MeasurementAccessData::new(std::slice::from_ref(&mm)).unwrap();
        assert!(mad2.set_data(0x300, &[0xA5]));
        // (0xA5 & 0xF0) >> 4 = 0x0A
        assert_eq!(mad2.get_raw_value(&mm, -1, 0).unwrap(), 10.0);
        let bo = MeasurementAccessData::bit_operation_of(&mm);
        assert_eq!(bo.bit_mask, 0xF0);
        assert_eq!(bo.shift_count, 4);
        assert!(bo.sign_extend);
        let mm2 = {
            let mut x = meas(0x300, DataType::UByte, None);
            x.bit_mask = Some(0xF0);
            x
        };
        let bo2 = MeasurementAccessData::bit_operation_of(&mm2);
        assert_eq!(bo2.shift_count, 4);
        assert!(!bo2.sign_extend);
    }
}
