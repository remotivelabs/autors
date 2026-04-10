//! Calibration data conservation formats: DCM (Konservierungsformat/DAMOS), Matlab `.m`,
//! and CANape PAR.
//! [`DataConservation`] holds shared state and helpers (`values` and
//! `skipped_values`), while DCM, Matlab, and PAR remain independent format types.
//! # Value representation (overlap with the autors-values crate)
//! A full value-object family (single values, value blocks, curves, maps, cuboids,
//! ASCII values, etc.) is implemented by the value module of the autors-values crate.
//! This module uses a lightweight, format-local representation,
//! [`ConservationValue`], without cross-crate references. Overlapping concerns
//! include value/axis storage, `ValueFormat` (a
//! subset of the full value-object format enum), unit and decimal-count parsing, and
//! the single-value/function-value enumeration semantics.
//! # Format constants
//! The format keywords and templates used here (`FESTWERT`, `ST/X`,
//! `CANape PAR V3.1`, the various `" {0} "` templates, etc.) reproduce the exact
//! byte-for-byte output expected by the tools that consume these files.
//! # Behavioral notes (summary; see the per-site comments for details)
//! - Files are read and written as UTF-8 (the DCM header `* encoding="utf-8"` is
//!   fixed accordingly).
//! - Header timestamps use UTC in `yyyy/M/d H:mm:ss` format.
//! - Numeric formatting always uses a `.` decimal separator, never a
//!   locale-dependent decimal comma.
//! - Extreme-precision values may differ in the last digit from 15-digit (G15-style)
//!   formatting; this code uses the shortest round-trip representation (same
//!   convention as autors-a2l).
//! - Several compatibility quirks of the established tool ecosystem are reproduced
//!   exactly and annotated at each site (Matlab axis write-out upper bound, no
//!   separator between CUBOID slices, CUBE_5 using the 4th axis point count for the
//!   5th axis, PAR reporting a Y-axis check failure as XAxisInvalid, Matlab comment
//!   stripping removing only a leading `%`, etc.).

use std::collections::HashMap;
use std::fmt;
use std::time::SystemTime;

use indexmap::IndexMap;

use autors_a2l::model::characteristic::{AxisPts, Characteristic, CharacteristicChild};
use autors_a2l::model::compu::{CompuMethod, CompuTab, CompuVtab, CompuVtabRange};
use autors_a2l::model::enums::{AxisType, CharacteristicType, ConversionType, DataType};
use autors_a2l::model::function::{Function, FunctionChild};
use autors_a2l::model::measurement::AxisDescr;
use autors_a2l::model::module::{Module, ModuleChild};
use autors_a2l::model::record_layout::RecordLayout;

use autors_formula::formula::{RationalCoeffsEval, TabCoeffs};

use crate::error::{Error, Result};

/// Line-ending sequence. These files conventionally use CRLF (Windows-style).
const EOL: &str = "\r\n";

/// Axis index → letter (`{'X','Y','Z','4','5'}` in Matlab and PAR output).
const AXIS_LETTERS: [char; 5] = ['X', 'Y', 'Z', '4', '5'];

// ============================================================================
// Basic helpers
// ============================================================================

/// Formats a `double` with the invariant culture.
/// Uses the shortest round-trip representation; ordinary values match 15-digit
/// (G15-style) formatting, with possible last-digit differences at extreme
/// precision or in exponential notation (same convention as autors-a2l).
fn to_dec(v: f64) -> String {
    format!("{v}")
}

/// Parses a floating-point number, allowing leading/trailing whitespace, a sign,
/// a decimal point, and an exponent.
fn parse_double(s: &str) -> std::result::Result<f64, String> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| format!("invalid floating point number {s:?}"))
}

/// Lenient integer parsing: supports `0x` hexadecimal, returns 0 on failure.
fn parse_int_val(s: &str) -> i64 {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).unwrap_or(0);
    }
    t.parse::<i64>().unwrap_or(0)
}

/// Rounds to the given number of decimals using midpoint-to-even (banker's rounding).
fn round_bankers(v: f64, decimals: i32) -> f64 {
    let p = 10f64.powi(decimals);
    (v * p).round_ties_even() / p
}

/// Formats with a fixed number of decimal places, invariant culture.
fn format_fixed(v: f64, decimals: i32) -> String {
    if decimals <= 0 {
        format!("{v:.0}")
    } else {
        format!("{:.1$}", v, decimals as usize)
    }
}

/// Extracts the decimal count from a conversion format string: drops the first
/// character (`%`) and splits on `'.'/'d'/'f'`; the second segment is the decimal
/// count. Falls back to 0 when it cannot be determined (a strict parse would
/// throw; the lenient fallback is intentional).
fn decimal_count_of_format(format: &str) -> i32 {
    if format.len() < 2 {
        return 0;
    }
    let lower = format[1..].to_lowercase();
    let parts: Vec<&str> = lower.split(['.', 'd', 'f']).collect();
    if parts.len() >= 2 {
        return parts[1].parse::<i32>().unwrap_or(0);
    }
    0
}

/// Returns the size of a data type in bits.
fn size_in_bit(data_type: DataType) -> u32 {
    match data_type {
        DataType::UByte | DataType::SByte => 8,
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => 16,
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => 32,
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => 64,
        DataType::Unsupported => 0,
    }
}

/// Converts a value to a decimal string according to the data type: integer types
/// are truncated toward zero before formatting, floating-point types are formatted
/// as invariant-culture decimals.
fn to_decimal_string(v: f64, data_type: DataType) -> String {
    match data_type {
        DataType::UByte | DataType::UWord | DataType::ULong | DataType::AUInt64 => {
            format!("{}", v as u64)
        }
        DataType::SByte | DataType::SWord | DataType::SLong | DataType::AInt64 => {
            format!("{}", v as i64)
        }
        _ => to_dec(v),
    }
}

/// days-from-epoch → year/month/day (Howard Hinnant's civil_from_days algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Returns the file-header timestamp in two parts (short date and long time).
/// Always UTC: `yyyy/M/d` and `H:mm:ss`.
fn utc_now_parts() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    (
        format!("{y}/{m}/{d}"),
        format!("{:02}:{:02}:{:02}", rem / 3600, rem / 60 % 60, rem % 60),
    )
}

// ============================================================================
// ErrorType / ValueFormat
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorType {
    NotFound,
    NotSupported,
    Format,
    TypeDiffers,
    NoOfElementsDiffers,
    XAxisInvalid,
    YAxisInvalid,
    ZAxisInvalid,
    Axis4Invalid,
    Axis5Invalid,
}

impl ErrorType {
    pub fn description(self) -> &'static str {
        match self {
            ErrorType::NotFound => "Not found",
            ErrorType::NotSupported => "Not supported",
            ErrorType::Format => "Format error",
            ErrorType::TypeDiffers => "Type incompatible",
            ErrorType::NoOfElementsDiffers => "No of elements differ",
            ErrorType::XAxisInvalid => "X Axis invalid",
            ErrorType::YAxisInvalid => "Y Axis invalid",
            ErrorType::ZAxisInvalid => "Z Axis invalid",
            ErrorType::Axis4Invalid => "4. Axis invalid",
            ErrorType::Axis5Invalid => "5. Axis invalid",
        }
    }
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueFormat {
    #[default]
    Physical,
    Raw,
}

// ============================================================================
// ============================================================================

/// `data[x + nx*(y + ny*(z + …))]`).
#[derive(Debug, Clone, PartialEq)]
pub enum ValueData {
    /// `CHAR_TYPE.VALUE`.
    Scalar(f64),
    /// `CHAR_TYPE.ASCII`.
    Text(String),
    Array {
        dims: Vec<usize>,
        data: Vec<f64>,
    },
}

impl ValueData {
    fn nan_array(dims: &[usize]) -> Self {
        ValueData::Array {
            dims: dims.to_vec(),
            data: vec![f64::NAN; dims.iter().product()],
        }
    }

    fn zero_array(dims: &[usize]) -> Self {
        ValueData::Array {
            dims: dims.to_vec(),
            data: vec![0.0; dims.iter().product()],
        }
    }

    pub fn len(&self) -> usize {
        match self {
            ValueData::Scalar(_) | ValueData::Text(_) => 1,
            ValueData::Array { data, .. } => data.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_nan_marker(&self) -> bool {
        match self {
            ValueData::Scalar(v) => v.is_nan(),
            ValueData::Text(_) => false,
            ValueData::Array { data, .. } => data.first().is_some_and(|v| v.is_nan()),
        }
    }

    fn as_slice(&self) -> Option<&[f64]> {
        match self {
            ValueData::Array { data, .. } => Some(data),
            _ => None,
        }
    }

    fn offset(&self, idx: &[usize]) -> Option<usize> {
        let ValueData::Array { dims, data } = self else {
            return None;
        };
        if idx.len() > dims.len() {
            return None;
        }
        let mut off = 0;
        let mut stride = 1;
        for (d, &i) in idx.iter().enumerate() {
            if i >= dims[d] {
                return None;
            }
            off += i * stride;
            stride *= dims[d];
        }
        if off >= data.len() {
            return None;
        }
        Some(off)
    }

    fn get(&self, idx: &[usize]) -> Option<f64> {
        self.offset(idx).map(|o| match self {
            ValueData::Array { data, .. } => data[o],
            _ => f64::NAN,
        })
    }

    fn set(&mut self, idx: &[usize], v: f64) -> std::result::Result<(), String> {
        let Some(off) = self.offset(idx) else {
            return Err(format!("index {idx:?} out of range"));
        };
        if let ValueData::Array { data, .. } = self {
            data[off] = v;
        }
        Ok(())
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub enum CharTarget<'m> {
    /// `/begin CHARACTERISTIC`.
    Characteristic(&'m Characteristic),
    /// `/begin AXIS_PTS`.
    AxisPts(&'m AxisPts),
}

impl<'m> CharTarget<'m> {
    fn name(&self) -> &'m str {
        match self {
            CharTarget::Characteristic(c) => &c.named.name,
            CharTarget::AxisPts(a) => &a.named.name,
        }
    }

    fn description(&self) -> &'m str {
        match self {
            CharTarget::Characteristic(c) => c.named.description.as_deref().unwrap_or(""),
            CharTarget::AxisPts(a) => a.named.description.as_deref().unwrap_or(""),
        }
    }

    fn conversion(&self) -> &'m str {
        match self {
            CharTarget::Characteristic(c) => &c.conv.conversion,
            CharTarget::AxisPts(a) => &a.conv.conversion,
        }
    }

    fn record_layout_name(&self) -> &'m str {
        match self {
            CharTarget::Characteristic(c) => &c.rec.record_layout,
            CharTarget::AxisPts(a) => &a.rec.record_layout,
        }
    }

    fn phys_unit(&self) -> Option<&'m str> {
        match self {
            CharTarget::Characteristic(c) => c.conv.phys_unit.as_deref(),
            CharTarget::AxisPts(a) => a.conv.phys_unit.as_deref(),
        }
    }

    fn format(&self) -> Option<&'m str> {
        match self {
            CharTarget::Characteristic(c) => c.conv.format.as_deref(),
            CharTarget::AxisPts(a) => a.conv.format.as_deref(),
        }
    }

    fn char_type(&self) -> CharacteristicType {
        match self {
            CharTarget::Characteristic(c) => c.char_type,
            CharTarget::AxisPts(_) => CharacteristicType::VAL_BLK,
        }
    }

    fn characteristic(&self) -> Option<&'m Characteristic> {
        match self {
            CharTarget::Characteristic(c) => Some(c),
            CharTarget::AxisPts(_) => None,
        }
    }
}

pub struct ModuleRefs<'m> {
    module: &'m Module,
    characteristics: HashMap<&'m str, &'m Characteristic>,
    axis_pts: HashMap<&'m str, &'m AxisPts>,
    record_layouts: HashMap<&'m str, &'m RecordLayout>,
    compu_methods: HashMap<&'m str, &'m CompuMethod>,
    compu_tabs: HashMap<&'m str, &'m CompuTab>,
    compu_vtabs: HashMap<&'m str, &'m CompuVtab>,
    compu_vtab_ranges: HashMap<&'m str, &'m CompuVtabRange>,
    units: HashMap<&'m str, &'m autors_a2l::model::module::Unit>,
    functions: Vec<&'m Function>,
    epk: Option<&'m str>,
}

impl<'m> ModuleRefs<'m> {
    pub fn new(module: &'m Module) -> Self {
        let mut refs = ModuleRefs {
            module,
            characteristics: HashMap::new(),
            axis_pts: HashMap::new(),
            record_layouts: HashMap::new(),
            compu_methods: HashMap::new(),
            compu_tabs: HashMap::new(),
            compu_vtabs: HashMap::new(),
            compu_vtab_ranges: HashMap::new(),
            units: HashMap::new(),
            functions: Vec::new(),
            epk: None,
        };
        for child in &module.children {
            match child {
                ModuleChild::Characteristic(c) => {
                    refs.characteristics.insert(c.named.name.as_str(), c);
                }
                ModuleChild::AxisPts(a) => {
                    refs.axis_pts.insert(a.named.name.as_str(), a);
                }
                ModuleChild::RecordLayout(r) => {
                    refs.record_layouts.insert(r.name.as_str(), r);
                }
                ModuleChild::CompuMethod(c) => {
                    refs.compu_methods.insert(c.name.as_str(), c);
                }
                ModuleChild::CompuTab(t) => {
                    refs.compu_tabs.insert(t.base.name.as_str(), t);
                }
                ModuleChild::CompuVtab(t) => {
                    refs.compu_vtabs.insert(t.base.name.as_str(), t);
                }
                ModuleChild::CompuVtabRange(t) => {
                    refs.compu_vtab_ranges.insert(t.base.name.as_str(), t);
                }
                ModuleChild::Unit(u) => {
                    refs.units.insert(u.name.as_str(), u);
                }
                ModuleChild::Function(f) => refs.functions.push(f),
                ModuleChild::ModPar(mp) => refs.epk = mp.epk.as_deref(),
                _ => {}
            }
        }
        refs
    }

    pub fn module(&self) -> &'m Module {
        self.module
    }

    pub fn epk(&self) -> Option<&'m str> {
        self.epk
    }

    pub fn char_target(&self, name: &str) -> Option<CharTarget<'m>> {
        if let Some(c) = self.characteristics.get(name) {
            return Some(CharTarget::Characteristic(c));
        }
        self.axis_pts.get(name).map(|a| CharTarget::AxisPts(a))
    }

    pub fn record_layout(&self, name: &str) -> Option<&'m RecordLayout> {
        self.record_layouts.get(name).copied()
    }

    pub fn compu_method(&self, name: &str) -> Option<&'m CompuMethod> {
        self.compu_methods.get(name).copied()
    }

    pub fn characteristic(&self, name: &str) -> Option<&'m Characteristic> {
        self.characteristics.get(name).copied()
    }

    pub fn axis_pts(&self, name: &str) -> Option<&'m AxisPts> {
        self.axis_pts.get(name).copied()
    }

    pub fn functions(&self) -> &[&'m Function] {
        &self.functions
    }

    pub fn def_characteristic_function(&self, name: &str) -> Option<&'m Function> {
        self.functions.iter().copied().find(|f| {
            f.children.iter().any(|ch| match ch {
                FunctionChild::DefCharacteristic(d) => {
                    d.references.references.iter().any(|r| r == name)
                }
                _ => false,
            })
        })
    }

    fn vtab_of(&self, compu: &CompuMethod) -> Option<&'m CompuVtab> {
        compu
            .compu_tab_ref
            .as_deref()
            .and_then(|r| self.compu_vtabs.get(r).copied())
    }

    fn vtab_range_of(&self, compu: &CompuMethod) -> Option<&'m CompuVtabRange> {
        compu
            .compu_tab_ref
            .as_deref()
            .and_then(|r| self.compu_vtab_ranges.get(r).copied())
    }

    fn compu_tab_of(&self, compu: &CompuMethod) -> Option<&'m CompuTab> {
        compu
            .compu_tab_ref
            .as_deref()
            .and_then(|r| self.compu_tabs.get(r).copied())
    }

    fn is_string_compu(&self, compu: Option<&CompuMethod>) -> bool {
        let Some(cm) = compu else { return false };
        if cm.compu_tab_ref.as_deref().is_none_or(str::is_empty) {
            return false;
        }
        self.vtab_of(cm).is_some() || self.vtab_range_of(cm).is_some()
    }

    fn compu_unit(&self, compu: Option<&CompuMethod>) -> String {
        let Some(cm) = compu else {
            return String::new();
        };
        if let Some(ru) = cm.ref_unit.as_deref().filter(|s| !s.is_empty()) {
            if let Some(u) = self.units.get(ru) {
                return u.display.clone().unwrap_or_default();
            }
        }
        cm.unit.clone()
    }

    fn unit_of(&self, phys_unit: Option<&str>, conversion: &str) -> String {
        match phys_unit {
            Some(u) => u.to_string(),
            None => self.compu_unit(self.compu_method(conversion)),
        }
    }

    fn format_of(&self, format: Option<&str>, conversion: &str) -> Option<String> {
        if let Some(f) = format.filter(|s| !s.is_empty()) {
            return Some(f.to_string());
        }
        self.compu_method(conversion).map(|cm| cm.format.clone())
    }
}

// ============================================================================
// ============================================================================

/// (`SingleValue`/`ASCIIValue`/`ValBlkValue`/`CurveValue`/`MapValue`/`CuboidValue`).
pub struct ConservationValue<'m> {
    target: CharTarget<'m>,
    refs: &'m ModuleRefs<'m>,
    pub value: ValueData,
    pub axis_values: Vec<Vec<f64>>,
    pub unit: String,
    pub unit_axis: Vec<String>,
    pub value_format: ValueFormat,
}

impl<'m> fmt::Debug for ConservationValue<'m> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConservationValue")
            .field("name", &self.name())
            .field("char_type", &self.char_type())
            .field("value", &self.value)
            .field("axis_values", &self.axis_values)
            .field("unit", &self.unit)
            .field("unit_axis", &self.unit_axis)
            .field("value_format", &self.value_format)
            .finish()
    }
}

impl<'m> ConservationValue<'m> {
    pub fn new(refs: &'m ModuleRefs<'m>, name: &str) -> Option<Self> {
        let target = refs.char_target(name)?;
        let value = default_value_data(refs, target);
        let descrs = axis_descrs_of(target);
        let axis_values = descrs
            .iter()
            .map(|d| vec![0.0; d.max_axis_points.max(0) as usize])
            .collect();
        let unit = refs.unit_of(target.phys_unit(), target.conversion());
        let unit_axis = descrs
            .iter()
            .map(|d| refs.unit_of(d.phys_unit.as_deref(), &d.conversion))
            .collect();
        Some(ConservationValue {
            target,
            refs,
            value,
            axis_values,
            unit,
            unit_axis,
            value_format: ValueFormat::Physical,
        })
    }

    fn pending(refs: &'m ModuleRefs<'m>, target: CharTarget<'m>) -> Self {
        let value = default_value_data(refs, target);
        ConservationValue {
            target,
            refs,
            value,
            axis_values: Vec::new(),
            unit: String::new(),
            unit_axis: Vec::new(),
            value_format: ValueFormat::Physical,
        }
    }

    pub fn name(&self) -> &'m str {
        self.target.name()
    }

    pub fn description(&self) -> &'m str {
        self.target.description()
    }

    pub fn char_type(&self) -> CharacteristicType {
        self.target.char_type()
    }

    pub fn is_axis_pts(&self) -> bool {
        matches!(self.target, CharTarget::AxisPts(_))
    }

    pub fn characteristic(&self) -> Option<&'m Characteristic> {
        self.target.characteristic()
    }

    pub fn axis_descrs(&self) -> Vec<&'m AxisDescr> {
        axis_descrs_of(self.target)
    }

    pub fn main_compu(&self) -> Option<&'m CompuMethod> {
        self.refs.compu_method(self.target.conversion())
    }

    fn axis_compu(&self, axis: usize) -> Option<&'m CompuMethod> {
        let descrs = self.axis_descrs();
        let d = descrs.get(axis)?;
        self.refs.compu_method(&d.conversion)
    }

    pub fn record_layout(&self) -> Option<&'m RecordLayout> {
        self.refs.record_layout(self.target.record_layout_name())
    }

    pub fn number_of_elements(&self) -> Option<usize> {
        let c = self.target.characteristic()?;
        match c.char_type {
            CharacteristicType::VALUE => Some(1),
            CharacteristicType::ASCII | CharacteristicType::VAL_BLK => {
                if let Some(dims) = &c.matrix_dim {
                    Some(dims.iter().map(|&d| d.max(0) as usize).product())
                } else {
                    Some(c.number.max(0) as usize)
                }
            }
            _ => None,
        }
    }

    fn max_axis_points_of_target(&self) -> Option<usize> {
        match self.target {
            CharTarget::AxisPts(a) => Some(a.max_axis_points.max(0) as usize),
            CharTarget::Characteristic(_) => None,
        }
    }

    fn matrix_dim_xy(&self) -> (usize, usize) {
        let mut x = self.number_of_elements().unwrap_or(0);
        let mut y = 1;
        if let Some(c) = self.target.characteristic() {
            if let Some(md) = &c.matrix_dim {
                if (md.len() <= 2 || md.get(2).is_some_and(|&d| d <= 1)) && md.len() > 1 {
                    x = md[0].max(0) as usize;
                    y = md[1].max(0) as usize;
                }
            }
        }
        (x, y)
    }

    fn decimal_count(&self) -> i32 {
        if self.value_format == ValueFormat::Raw {
            return 0;
        }
        self.refs
            .format_of(self.target.format(), self.target.conversion())
            .as_deref()
            .map_or(0, decimal_count_of_format)
    }

    fn decimal_count_axis(&self, axis: usize) -> i32 {
        if self.value_format == ValueFormat::Raw {
            return 0;
        }
        let descrs = self.axis_descrs();
        let Some(d) = descrs.get(axis) else { return 0 };
        self.refs
            .format_of(d.format.as_deref(), &d.conversion)
            .as_deref()
            .map_or(0, decimal_count_of_format)
    }

    fn is_string(&self) -> bool {
        self.refs.is_string_compu(self.main_compu())
    }

    fn axis_is_string(&self, axis: usize) -> bool {
        self.refs.is_string_compu(self.axis_compu(axis))
    }

    fn shared_axis_ref(&self, axis: usize) -> Option<&'m str> {
        let descrs = self.axis_descrs();
        descrs.get(axis)?.axis_pts_ref.as_deref()
    }

    fn fnc_data_type(&self) -> DataType {
        self.record_layout()
            .and_then(|rl| {
                rl.fnc_values
                    .as_ref()
                    .map(|f| f.data_type)
                    .or_else(|| rl.axis_pts[0].as_ref().map(|a| a.data_type))
            })
            .unwrap_or_default()
    }

    fn axis_compu_data_type(&self, axis: usize) -> (Option<&'m CompuMethod>, DataType) {
        let descrs = self.axis_descrs();
        let Some(d) = descrs.get(axis) else {
            return (None, DataType::default());
        };
        if let Some(ref_name) = d.curve_axis_ref.as_deref() {
            if let Some(rc) = self.refs.characteristic(ref_name) {
                let compu = rc
                    .children
                    .iter()
                    .find_map(|ch| match ch {
                        CharacteristicChild::AxisDescr(a) => Some(a),
                        _ => None,
                    })
                    .and_then(|a| self.refs.compu_method(&a.conversion));
                let dt = self
                    .refs
                    .record_layout(&rc.rec.record_layout)
                    .and_then(|rl| {
                        rl.axis_pts[0]
                            .as_ref()
                            .map(|a| a.data_type)
                            .or_else(|| rl.axis_rescale_x.as_ref().map(|a| a.data_type))
                    })
                    .unwrap_or_default();
                return (compu, dt);
            }
        }
        if let Some(ref_name) = d.axis_pts_ref.as_deref() {
            if let Some(ap) = self.refs.axis_pts(ref_name) {
                let compu = self.refs.compu_method(&ap.conv.conversion);
                let dt = self
                    .refs
                    .record_layout(&ap.rec.record_layout)
                    .and_then(|rl| {
                        rl.axis_pts[0]
                            .as_ref()
                            .map(|a| a.data_type)
                            .or_else(|| rl.axis_rescale_x.as_ref().map(|a| a.data_type))
                    })
                    .unwrap_or_default();
                return (compu, dt);
            }
        }
        let compu = self.refs.compu_method(&d.conversion);
        let dt = self
            .record_layout()
            .and_then(|rl| {
                rl.axis_pts
                    .get(axis)
                    .and_then(|a| a.as_ref())
                    .map(|a| a.data_type)
                    .or_else(|| rl.axis_rescale_x.as_ref().map(|a| a.data_type))
            })
            .unwrap_or_default();
        (compu, dt)
    }

    fn to_string_value(
        &self,
        value: f64,
        data_type: DataType,
        compu: Option<&CompuMethod>,
        decimal_count: i32,
    ) -> String {
        let dc = decimal_count.min(15);
        if self.value_format == ValueFormat::Physical {
            if let Some(cm) = compu {
                if cm.conversion_type == ConversionType::TAB_VERB {
                    if let Some(vt) = self.refs.vtab_of(cm) {
                        return vtab_to_physical(vt, value);
                    }
                    if let Some(vtr) = self.refs.vtab_range_of(cm) {
                        return vtab_range_to_physical(vtr, value);
                    }
                }
            }
        }
        match self.value_format {
            ValueFormat::Physical => {
                let n = if dc < 0 {
                    value
                } else {
                    round_bankers(value, dc)
                };
                if dc >= 0 {
                    format_fixed(n, dc)
                } else {
                    to_dec(n)
                }
            }
            ValueFormat::Raw => to_decimal_string(value, data_type),
        }
    }

    pub fn to_single_value(&self, force_fmt: bool) -> String {
        match self.char_type() {
            CharacteristicType::VALUE => {
                let v = match self.value {
                    ValueData::Scalar(v) => v,
                    _ => f64::NAN,
                };
                let dc = if force_fmt { self.decimal_count() } else { -1 };
                self.to_string_value(v, self.fnc_data_type(), self.main_compu(), dc)
            }
            CharacteristicType::ASCII => match &self.value {
                ValueData::Text(t) => t.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    pub fn to_single_value_at(&self, x: i64, y: i64, _z: i64, force_fmt: bool) -> String {
        let ct = self.char_type();
        if y == -1
            && x >= 0
            && matches!(
                ct,
                CharacteristicType::VAL_BLK | CharacteristicType::CURVE | CharacteristicType::MAP
            )
        {
            if ct == CharacteristicType::VAL_BLK && self.is_axis_pts() {
            } else {
                let Some(axis0) = self.axis_values.first() else {
                    return String::new();
                };
                if x as usize >= axis0.len() {
                    return String::new();
                }
                let (compu, dt) = self.axis_compu_data_type(0);
                let dc = if force_fmt {
                    self.decimal_count_axis(0)
                } else {
                    -1
                };
                return self.to_string_value(axis0[x as usize], dt, compu, dc);
            }
        }
        if x == -1 && y >= 0 && ct == CharacteristicType::MAP {
            let Some(axis1) = self.axis_values.get(1) else {
                return String::new();
            };
            if y as usize >= axis1.len() {
                return String::new();
            }
            let (compu, dt) = self.axis_compu_data_type(1);
            let dc = if force_fmt {
                self.decimal_count_axis(1)
            } else {
                -1
            };
            return self.to_string_value(axis1[y as usize], dt, compu, dc);
        }
        let dc = if force_fmt { self.decimal_count() } else { -1 };
        let v = match ct {
            CharacteristicType::VAL_BLK | CharacteristicType::CURVE if x >= 0 => self
                .value
                .as_slice()
                .and_then(|d| d.get(x as usize))
                .copied()
                .unwrap_or(f64::NAN),
            CharacteristicType::MAP if x >= 0 && y >= 0 => self
                .value
                .get(&[x as usize, y as usize])
                .unwrap_or(f64::NAN),
            _ => f64::NAN,
        };
        self.to_string_value(v, self.fnc_data_type(), self.main_compu(), dc)
    }

    pub fn fnc_value(&self) -> f64 {
        match self.value {
            ValueData::Scalar(v) => v,
            _ => f64::NAN,
        }
    }

    pub fn fnc_values(&self) -> Vec<f64> {
        match &self.value {
            ValueData::Scalar(v) => vec![*v],
            ValueData::Array { data, .. } => data.clone(),
            ValueData::Text(_) => Vec::new(),
        }
    }

    pub fn to_physical(&self, compu: Option<&CompuMethod>, raw: f64) -> f64 {
        let Some(cm) = compu else { return raw };
        match cm.conversion_type {
            ConversionType::IDENTICAL => raw,
            ConversionType::LINEAR | ConversionType::RAT_FUNC => cm.coeffs.coeffs_to_physical(raw),
            ConversionType::TAB_VERB => raw,
            ConversionType::TAB_INTP | ConversionType::TAB_NOINTP => self
                .refs
                .compu_tab_of(cm)
                .map_or(raw, |t| TabCoeffs::to_physical(cm.conversion_type, t, raw)),
            ConversionType::FORM => raw,
        }
    }
}

fn axis_descrs_of<'m>(target: CharTarget<'m>) -> Vec<&'m AxisDescr> {
    match target {
        CharTarget::Characteristic(c) => c
            .children
            .iter()
            .filter_map(|ch| match ch {
                CharacteristicChild::AxisDescr(a) => Some(a),
                _ => None,
            })
            .collect(),
        CharTarget::AxisPts(_) => Vec::new(),
    }
}

fn default_value_data(refs: &ModuleRefs, target: CharTarget) -> ValueData {
    match target {
        CharTarget::AxisPts(a) => ValueData::nan_array(&[a.max_axis_points.max(0) as usize]),
        CharTarget::Characteristic(c) => match c.char_type {
            CharacteristicType::VALUE => ValueData::Scalar(f64::NAN),
            CharacteristicType::ASCII => ValueData::Text(String::new()),
            CharacteristicType::VAL_BLK => {
                let n = number_of_elements_of(refs, c).unwrap_or(0);
                ValueData::nan_array(&[n])
            }
            _ => {
                let dims: Vec<usize> = axis_descrs_of(CharTarget::Characteristic(c))
                    .iter()
                    .map(|d| d.max_axis_points.max(0) as usize)
                    .collect();
                ValueData::nan_array(&dims)
            }
        },
    }
}

fn number_of_elements_of(_refs: &ModuleRefs, c: &Characteristic) -> Option<usize> {
    match c.char_type {
        CharacteristicType::VALUE => Some(1),
        CharacteristicType::ASCII | CharacteristicType::VAL_BLK => {
            if let Some(dims) = &c.matrix_dim {
                Some(dims.iter().map(|&d| d.max(0) as usize).product())
            } else {
                Some(c.number.max(0) as usize)
            }
        }
        _ => None,
    }
}

// ============================================================================
// ============================================================================

fn to_int64(v: f64) -> i64 {
    v.round_ties_even() as i64
}

fn vtab_to_physical(vt: &CompuVtab, raw: f64) -> String {
    if !raw.is_nan() {
        if let Some(v) = vt.verbs.get(&to_int64(raw)) {
            return v.clone();
        }
    }
    if let Some(dv) = vt.base.default_value.as_deref().filter(|s| !s.is_empty()) {
        return dv.to_string();
    }
    to_dec(raw)
}

fn vtab_to_raw(vt: &CompuVtab, s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    for (k, v) in &vt.verbs {
        if v == s {
            return *k as f64;
        }
    }
    0.0
}

fn vtab_range_to_physical(vtr: &CompuVtabRange, raw: f64) -> String {
    for (text, ranges) in &vtr.verbs {
        for (min, max) in ranges {
            if raw >= *min && raw <= *max {
                return text.clone();
            }
        }
    }
    vtr.base.default_value.clone().unwrap_or_default()
}

fn vtab_range_to_raw(vtr: &CompuVtabRange, s: &str) -> f64 {
    match vtr.verbs.get(s) {
        Some(ranges) if !ranges.is_empty() => ranges[0].0,
        _ => 0.0,
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Default)]
pub struct DataConservation<'m> {
    pub values: Vec<ConservationValue<'m>>,
    pub skipped_values: Vec<(String, ErrorType)>,
}

impl<'m> DataConservation<'m> {
    fn check_axis(descrs: &[&AxisDescr], var_flags: &[bool], axis: usize, count: usize) -> usize {
        let max = descrs[axis].max_axis_points.max(0) as usize;
        if (var_flags[axis] && count > max) || (!var_flags[axis] && count != max) {
            return 0;
        }
        count
    }

    fn axis_var_flags(
        refs: &ModuleRefs<'m>,
        target: CharTarget<'m>,
    ) -> (Vec<&'m AxisDescr>, Vec<bool>) {
        if let CharTarget::Characteristic(c) = target {
            if matches!(
                c.char_type,
                CharacteristicType::VALUE
                    | CharacteristicType::ASCII
                    | CharacteristicType::VAL_BLK
                    | CharacteristicType::NotSet
            ) {
                return (Vec::new(), Vec::new());
            }
        }
        let descrs = axis_descrs_of(target);
        let flags = descrs
            .iter()
            .enumerate()
            .map(|(i, d)| match d.axis_type {
                AxisType::STD_AXIS => refs
                    .record_layout(target.record_layout_name())
                    .and_then(|rl| rl.no_axis_pts.get(i).and_then(|e| e.as_ref()))
                    .is_some(),
                AxisType::COM_AXIS => d
                    .axis_pts_ref
                    .as_deref()
                    .and_then(|n| refs.axis_pts(n))
                    .and_then(|ap| refs.record_layout(&ap.rec.record_layout))
                    .and_then(|rl| rl.no_axis_pts[0].as_ref())
                    .is_some(),
                AxisType::RES_AXIS => d
                    .axis_pts_ref
                    .as_deref()
                    .and_then(|n| refs.axis_pts(n))
                    .and_then(|ap| refs.record_layout(&ap.rec.record_layout))
                    .and_then(|rl| rl.no_rescale_x.as_ref())
                    .is_some(),
                AxisType::CURVE_AXIS => d
                    .curve_axis_ref
                    .as_deref()
                    .and_then(|n| refs.characteristic(n))
                    .and_then(|rc| refs.record_layout(&rc.rec.record_layout))
                    .and_then(|rl| rl.no_axis_pts[0].as_ref())
                    .is_some(),
                AxisType::FIX_AXIS => false,
            })
            .collect();
        (descrs, flags)
    }

    pub fn epk_description(refs: &ModuleRefs<'m>, description: &str) -> String {
        match refs.epk() {
            Some(epk) if !description.is_empty() => format!("EPK: {epk}{EOL}{EOL}{description}"),
            Some(epk) => format!("EPK: {epk}"),
            None => description.to_string(),
        }
    }
}

// ============================================================================
// DCM(Konservierungsformat / DAMOS)
// ============================================================================

/// `KENNLINIE`/`FESTKENNLINIE`/`GRUPPENKENNLINIE`/`KENNFELD`/`FESTKENNFELD`/
/// `GRUPPENKENNFELD`/`STUETZSTELLENVERTEILUNG`/`WERT`/`TEXT`/`ST/X`/`ST_TX/X`/
/// `ST/Y`/`ST_TX/Y`/`EINHEIT_W`/`EINHEIT_X`/`EINHEIT_Y`/`LANGNAME`/`FUNKTION`/
/// `FKT`/`FUNKTIONEN`/`END`/`KONSERVIERUNG_FORMAT`.
pub struct DcmFile;

#[derive(Clone, Copy)]
enum TabRef<'m> {
    /// COMPU_VTAB.
    Vtab(&'m CompuVtab),
    /// COMPU_VTAB_RANGE.
    VtabRange(&'m CompuVtabRange),
}

impl<'m> TabRef<'m> {
    fn to_raw(self, s: &str) -> f64 {
        match self {
            TabRef::Vtab(vt) => vtab_to_raw(vt, s),
            TabRef::VtabRange(vtr) => vtab_range_to_raw(vtr, s),
        }
    }

    fn of(refs: &ModuleRefs<'m>, compu: Option<&'m CompuMethod>) -> Option<TabRef<'m>> {
        let cm = compu?;
        if let Some(vt) = refs.vtab_of(cm) {
            return Some(TabRef::Vtab(vt));
        }
        refs.vtab_range_of(cm).map(TabRef::VtabRange)
    }
}

struct DcmState<'m> {
    pending: Option<ConservationValue<'m>>,
    tab_w: Option<TabRef<'m>>,
    tab_x: Option<TabRef<'m>>,
    tab_y: Option<TabRef<'m>>,
    num: usize,
    num2: usize,
    num3: usize,
    num4: usize,
    nx: usize,
    ny: usize,
}

impl<'m> DcmState<'m> {
    fn new() -> Self {
        DcmState {
            pending: None,
            tab_w: None,
            tab_x: None,
            tab_y: None,
            num: 0,
            num2: 0,
            num3: 0,
            num4: 0,
            nx: 0,
            ny: 0,
        }
    }

    fn reset(&mut self) {
        *self = DcmState::new();
    }
}

impl DcmFile {
    pub fn open<'a>(
        path: impl AsRef<std::path::Path>,
        refs: &'a ModuleRefs<'a>,
    ) -> Result<DataConservation<'a>> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        Self::open_str(&text, refs)
    }

    pub fn open_str<'m>(text: &str, refs: &'m ModuleRefs<'m>) -> Result<DataConservation<'m>> {
        let mut file = DataConservation::default();
        let mut format_ok = false;
        let mut st = DcmState::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with(['*', '!', '.']) {
                continue;
            }
            let _ = Self::process_line(line, refs, &mut file, &mut st, &mut format_ok);
        }
        Ok(file)
    }

    pub fn save(
        path: impl AsRef<std::path::Path>,
        description: &str,
        values: &[ConservationValue<'_>],
        functions: Option<&[&Function]>,
        force_fmt: bool,
    ) -> Result<usize> {
        let (text, count) = Self::save_string(description, values, functions, force_fmt)?;
        std::fs::write(path, text)?;
        Ok(count)
    }

    /// `* encoding="utf-8"` / `* DAMOS Format` / `* Created by {app} ({ver})` /
    pub fn save_string(
        description: &str,
        values: &[ConservationValue<'_>],
        functions: Option<&[&Function]>,
        force_fmt: bool,
    ) -> Result<(String, usize)> {
        let mut out = String::new();
        out.push_str("* encoding=\"utf-8\"");
        out.push_str(EOL);
        out.push_str("* DAMOS Format");
        out.push_str(EOL);
        out.push_str("* Created by  ()");
        out.push_str(EOL);
        let (date, time) = utc_now_parts();
        out.push_str(&format!("* Created at {date} {time}"));
        out.push_str(EOL);
        if !description.is_empty() {
            for line in description.split(EOL) {
                out.push_str("* ");
                out.push_str(line.trim_matches(' '));
                out.push_str(EOL);
            }
        }
        out.push_str(EOL);
        out.push_str("KONSERVIERUNG_FORMAT 2.0");
        out.push_str(EOL);
        out.push_str(EOL);
        if let Some(fns) = functions {
            if !fns.is_empty() {
                out.push_str("FUNKTIONEN");
                out.push_str(EOL);
                for f in fns {
                    out.push_str(&format!(
                        "FKT {} \"{}\" \"{}\"",
                        f.named.name,
                        f.version.as_deref().unwrap_or(""),
                        f.named.description.as_deref().unwrap_or("")
                    ));
                    out.push_str(EOL);
                }
                out.push_str("END");
                out.push_str(EOL);
                out.push_str(EOL);
            }
        }
        let count = Self::write_values(&mut out, values, force_fmt)?;
        Ok((out, count))
    }

    fn write_values(
        out: &mut String,
        values: &[ConservationValue<'_>],
        force_fmt: bool,
    ) -> Result<usize> {
        let mut count = 0;
        for v in values {
            let def_fn = v.refs.def_characteristic_function(v.name());
            match v.char_type() {
                CharacteristicType::VALUE => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    out.push_str(&format!("FESTWERT {}", v.name()));
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!("EINHEIT_W \"{}\"", v.unit));
                    out.push_str(EOL);
                    let is_string = v.is_string();
                    out.push_str(if is_string { " TEXT " } else { " WERT " });
                    out.push_str(&Self::fmt_single(v, is_string, force_fmt));
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
                CharacteristicType::ASCII => {
                    let ValueData::Text(text) = &v.value else {
                        return Err(data_err(format!("{}: ASCII value is not text", v.name())));
                    };
                    if text.is_empty() {
                        continue;
                    }
                    out.push_str(&format!("TEXTSTRING {}", v.name()));
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!(" TEXT \"{text}\""));
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
                CharacteristicType::VAL_BLK if !v.is_axis_pts() => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let (nx, ny) = v.matrix_dim_xy();
                    out.push_str(&format!("FESTWERTEBLOCK {} {nx}", v.name()));
                    if ny > 1 {
                        out.push_str(&format!(" @ {ny}"));
                    }
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!("EINHEIT_W \"{}\"", v.unit));
                    out.push_str(EOL);
                    let is_string = v.is_string();
                    out.push_str(if is_string { " TEXT " } else { " WERT " });
                    for m in 0..ny {
                        for n in 0..nx {
                            let idx = m * nx + n;
                            out.push_str(&Self::fmt_at(v, idx as i64, 0, 0, is_string, force_fmt)?);
                        }
                        if m < ny - 1 {
                            out.push_str(EOL);
                        }
                    }
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
                CharacteristicType::CURVE => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let axis0 = v.axis_values.first().ok_or_else(|| {
                        data_err(format!("{}: CURVE without x axis values", v.name()))
                    })?;
                    let n = axis0.len();
                    if v.value.as_slice().is_some_and(|d| d.len() != n) {
                        return Err(data_err(format!(
                            "{}: CURVE value/axis length mismatch",
                            v.name()
                        )));
                    }
                    let shared = v.shared_axis_ref(0);
                    out.push_str(&format!(
                        "{} {} {n}",
                        if shared.is_some() {
                            "GRUPPENKENNLINIE"
                        } else {
                            "KENNLINIE"
                        },
                        v.name()
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!(
                        "EINHEIT_X \"{}\"",
                        v.unit_axis.first().map_or("", String::as_str)
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!("EINHEIT_W \"{}\"", v.unit));
                    out.push_str(EOL);
                    if let Some(sst) = shared {
                        out.push_str(&format!("*SSTX {sst}"));
                        out.push_str(EOL);
                    }
                    let axis_is_string = v.axis_is_string(0);
                    out.push_str(if axis_is_string { "ST_TX/X " } else { "ST/X " });
                    for i in 0..n {
                        out.push_str(&Self::fmt_at(
                            v,
                            i as i64,
                            -1,
                            0,
                            axis_is_string,
                            force_fmt,
                        )?);
                    }
                    out.push_str(EOL);
                    let is_string = v.is_string();
                    out.push_str(if is_string { " TEXT " } else { " WERT " });
                    for i in 0..n {
                        out.push_str(&Self::fmt_at(v, i as i64, 0, 0, is_string, force_fmt)?);
                    }
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
                CharacteristicType::MAP => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let axis0 = v.axis_values.first().ok_or_else(|| {
                        data_err(format!("{}: MAP without x axis values", v.name()))
                    })?;
                    let axis1 = v.axis_values.get(1).ok_or_else(|| {
                        data_err(format!("{}: MAP without y axis values", v.name()))
                    })?;
                    let (nx, ny) = (axis0.len(), axis1.len());
                    if v.value.as_slice().is_some_and(|d| d.len() != nx * ny) {
                        return Err(data_err(format!(
                            "{}: MAP value/axis length mismatch",
                            v.name()
                        )));
                    }
                    let shared_x = v.shared_axis_ref(0);
                    let shared_y = v.shared_axis_ref(1);
                    out.push_str(&format!(
                        "{} {} {nx} {ny}",
                        if shared_x.is_some() || shared_y.is_some() {
                            "GRUPPENKENNFELD"
                        } else {
                            "KENNFELD"
                        },
                        v.name()
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!(
                        "EINHEIT_X \"{}\"",
                        v.unit_axis.first().map_or("", String::as_str)
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!(
                        "EINHEIT_Y \"{}\"",
                        v.unit_axis.get(1).map_or("", String::as_str)
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!("EINHEIT_W \"{}\"", v.unit));
                    out.push_str(EOL);
                    if let Some(sst) = shared_x {
                        out.push_str(&format!("*SSTX {sst}"));
                        out.push_str(EOL);
                    }
                    if let Some(sst) = shared_y {
                        out.push_str(&format!("*SSTY {sst}"));
                        out.push_str(EOL);
                    }
                    let axis_x_is_string = v.axis_is_string(0);
                    let axis_y_is_string = v.axis_is_string(1);
                    out.push_str(if axis_x_is_string {
                        "ST_TX/X "
                    } else {
                        "ST/X "
                    });
                    for l in 0..nx {
                        out.push_str(&Self::fmt_at(
                            v,
                            l as i64,
                            -1,
                            0,
                            axis_x_is_string,
                            force_fmt,
                        )?);
                    }
                    let is_string = v.is_string();
                    for k in 0..ny {
                        out.push_str(EOL);
                        out.push_str(if axis_y_is_string {
                            "ST_TX/Y "
                        } else {
                            "ST/Y "
                        });
                        out.push_str(&Self::fmt_at(
                            v,
                            -1,
                            k as i64,
                            0,
                            axis_y_is_string,
                            force_fmt,
                        )?);
                        out.push_str(EOL);
                        out.push_str(if is_string { " TEXT " } else { " WERT " });
                        for l in 0..nx {
                            out.push_str(&Self::fmt_at(
                                v, l as i64, k as i64, 0, is_string, force_fmt,
                            )?);
                        }
                    }
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
                _ => {
                    let data = v.value.as_slice().ok_or_else(|| {
                        data_err(format!("{}: unsupported value kind for DCM", v.name()))
                    })?;
                    if data.is_empty() || v.value.is_nan_marker() {
                        continue;
                    }
                    out.push_str(&format!(
                        "STUETZSTELLENVERTEILUNG {} {}",
                        v.name(),
                        data.len()
                    ));
                    out.push_str(EOL);
                    out.push_str(&format!("LANGNAME \"{}\"", v.description()));
                    out.push_str(EOL);
                    if let Some(f) = def_fn {
                        out.push_str(&format!("FUNKTION {}", f.named.name));
                        out.push_str(EOL);
                    }
                    out.push_str(&format!("EINHEIT_X \"{}\"", v.unit));
                    out.push_str(EOL);
                    let is_string = v.is_string();
                    out.push_str(if is_string { "ST_TX/X " } else { "ST/X " });
                    for i in 0..data.len() {
                        out.push_str(&Self::fmt_at(v, i as i64, 0, 0, is_string, force_fmt)?);
                    }
                    out.push_str(EOL);
                    out.push_str("END");
                    out.push_str(EOL);
                    out.push_str(EOL);
                }
            }
            count += 1;
        }
        Ok(count)
    }

    fn fmt_single(v: &ConservationValue<'_>, is_string: bool, force_fmt: bool) -> String {
        let text = v.to_single_value(force_fmt);
        if !is_string {
            text
        } else {
            format!("\"{text}\"")
        }
    }

    fn fmt_at(
        v: &ConservationValue<'_>,
        x: i64,
        y: i64,
        z: i64,
        is_string: bool,
        force_fmt: bool,
    ) -> Result<String> {
        let text = v.to_single_value_at(x, y, z, force_fmt);
        Ok(if !is_string {
            format!("{text} ")
        } else {
            format!("\"{text}\" ")
        })
    }

    fn process_line<'m>(
        line: &str,
        refs: &'m ModuleRefs<'m>,
        file: &mut DataConservation<'m>,
        st: &mut DcmState<'m>,
        format_ok: &mut bool,
    ) -> std::result::Result<(), String> {
        let tokens = tokenize_dcm(line);
        let Some(kw) = tokens.first().map(String::as_str) else {
            return Ok(());
        };
        let arg1 = tokens.get(1).map(String::as_str).unwrap_or("");
        match kw {
            "FESTWERT" => {
                if let Some(target) = Self::begin_value(refs, file, *format_ok, arg1, kw) {
                    let mut v = ConservationValue::pending(refs, target);
                    v.value = ValueData::Scalar(f64::NAN);
                    st.tab_w = TabRef::of(refs, v.main_compu());
                    st.pending = Some(v);
                }
            }
            "TEXTSTRING" => {
                if let Some(target) = Self::begin_value(refs, file, *format_ok, arg1, kw) {
                    let mut v = ConservationValue::pending(refs, target);
                    v.value = ValueData::Text(String::new());
                    st.pending = Some(v);
                }
            }
            "FESTWERTEBLOCK" | "STUETZSTELLENVERTEILUNG" => {
                if let Some(target) = Self::begin_value(refs, file, *format_ok, arg1, kw) {
                    let v = ConservationValue::pending(refs, target);
                    let expected = match v.max_axis_points_of_target() {
                        Some(n) => n,
                        None => v.number_of_elements().ok_or("no element count")?,
                    };
                    let nx =
                        parse_int_val(tokens.get(2).map(String::as_str).unwrap_or("")) as usize;
                    let ny = if tokens.len() <= 4 {
                        1
                    } else {
                        parse_int_val(tokens.get(4).map(String::as_str).unwrap_or("")) as usize
                    };
                    if nx * ny == expected {
                        let mut v = v;
                        v.value = ValueData::zero_array(&[expected]);
                        st.tab_w = TabRef::of(refs, v.main_compu());
                        st.pending = Some(v);
                    }
                }
            }
            "KENNLINIE" | "FESTKENNLINIE" | "GRUPPENKENNLINIE" => {
                if let Some(target) = Self::begin_value(refs, file, *format_ok, arg1, kw) {
                    let (descrs, flags) = DataConservation::axis_var_flags(refs, target);
                    if descrs.is_empty() {
                        return Ok(());
                    }
                    let count =
                        parse_int_val(tokens.get(2).map(String::as_str).unwrap_or("")) as usize;
                    let nx = DataConservation::check_axis(&descrs, &flags, 0, count);
                    if nx == 0 {
                        file.skipped_values
                            .push((arg1.to_string(), ErrorType::XAxisInvalid));
                        return Ok(());
                    }
                    let mut v = ConservationValue::pending(refs, target);
                    v.value = ValueData::zero_array(&[nx]);
                    v.axis_values = vec![vec![0.0; nx]];
                    st.tab_w = TabRef::of(refs, v.main_compu());
                    st.tab_x = TabRef::of(refs, v.axis_compu(0));
                    st.nx = nx;
                    st.pending = Some(v);
                }
            }
            "KENNFELD" | "FESTKENNFELD" | "GRUPPENKENNFELD" => {
                if let Some(target) = Self::begin_value(refs, file, *format_ok, arg1, kw) {
                    let (descrs, flags) = DataConservation::axis_var_flags(refs, target);
                    if descrs.len() < 2 {
                        return Ok(());
                    }
                    let cx =
                        parse_int_val(tokens.get(2).map(String::as_str).unwrap_or("")) as usize;
                    let nx = DataConservation::check_axis(&descrs, &flags, 0, cx);
                    if nx == 0 {
                        file.skipped_values
                            .push((arg1.to_string(), ErrorType::XAxisInvalid));
                        return Ok(());
                    }
                    let cy =
                        parse_int_val(tokens.get(3).map(String::as_str).unwrap_or("")) as usize;
                    let ny = DataConservation::check_axis(&descrs, &flags, 1, cy);
                    if ny == 0 {
                        file.skipped_values
                            .push((arg1.to_string(), ErrorType::XAxisInvalid));
                        return Ok(());
                    }
                    let mut v = ConservationValue::pending(refs, target);
                    v.value = ValueData::zero_array(&[nx, ny]);
                    v.axis_values = vec![vec![0.0; nx], vec![0.0; ny]];
                    st.tab_w = TabRef::of(refs, v.main_compu());
                    st.tab_x = TabRef::of(refs, v.axis_compu(0));
                    st.tab_y = TabRef::of(refs, v.axis_compu(1));
                    st.nx = nx;
                    st.ny = ny;
                    st.pending = Some(v);
                }
            }
            "EINHEIT_W" => {
                if let Some(p) = st.pending.as_mut() {
                    p.unit = arg1.to_string();
                }
            }
            "EINHEIT_X" => {
                if let Some(p) = st.pending.as_mut() {
                    let size = if p.char_type() == CharacteristicType::MAP {
                        2
                    } else {
                        1
                    };
                    p.unit_axis.resize(size, String::new());
                    p.unit_axis[0] = arg1.to_string();
                }
            }
            "EINHEIT_Y" => {
                if let Some(p) = st.pending.as_mut() {
                    let size = if p.char_type() == CharacteristicType::MAP {
                        2
                    } else {
                        1
                    };
                    p.unit_axis.resize(size, String::new());
                    if p.unit_axis.len() < 2 {
                        return Err("EINHEIT_Y without MAP".to_string());
                    }
                    p.unit_axis[1] = arg1.to_string();
                }
            }
            "WERT" | "TEXT" => {
                let Some(p) = st.pending.as_mut() else {
                    return Ok(());
                };
                match p.char_type() {
                    CharacteristicType::VALUE => {
                        p.value = ValueData::Scalar(Self::parse_value(arg1, st.tab_w)?);
                    }
                    CharacteristicType::ASCII => {
                        p.value = ValueData::Text(arg1.to_string());
                    }
                    CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                        for t in &tokens[1..] {
                            let val = Self::parse_value(t, st.tab_w)?;
                            p.value.set(&[st.num3], val)?;
                            st.num3 += 1;
                        }
                    }
                    CharacteristicType::MAP => {
                        for t in &tokens[1..] {
                            let val = Self::parse_value(t, st.tab_w)?;
                            p.value.set(&[st.num3, st.num4], val)?;
                            st.num3 += 1;
                            if st.num3 == st.nx {
                                st.num3 = 0;
                                st.num4 += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "ST/X" | "ST_TX/X" => {
                let Some(p) = st.pending.as_mut() else {
                    return Ok(());
                };
                match p.char_type() {
                    CharacteristicType::VAL_BLK => {
                        for t in &tokens[1..] {
                            let val = Self::parse_value(t, st.tab_w)?;
                            p.value.set(&[st.num], val)?;
                            st.num += 1;
                        }
                    }
                    CharacteristicType::CURVE | CharacteristicType::MAP => {
                        for t in &tokens[1..] {
                            let val = Self::parse_value(t, st.tab_x)?;
                            let slot = p.axis_values[0].get_mut(st.num).ok_or("axis overflow")?;
                            *slot = val;
                            st.num += 1;
                        }
                    }
                    _ => {}
                }
            }
            "ST/Y" | "ST_TX/Y" => {
                let Some(p) = st.pending.as_mut() else {
                    return Ok(());
                };
                if p.char_type() == CharacteristicType::MAP {
                    for t in &tokens[1..] {
                        let val = Self::parse_value(t, st.tab_y)?;
                        let slot = p.axis_values[1].get_mut(st.num2).ok_or("axis overflow")?;
                        *slot = val;
                        st.num2 += 1;
                    }
                }
            }
            "END" => {
                if let Some(v) = st.pending.take() {
                    file.values.push(v);
                }
                st.reset();
            }
            "KONSERVIERUNG_FORMAT" => {
                if let Some(ver) = tokens.get(1) {
                    if let Ok(major) = ver.split('.').next().unwrap_or("").parse::<i32>() {
                        if major >= 2 {
                            *format_ok = true;
                        }
                    }
                }
            }
            "FKT" | "LANGNAME" | "FUNKTION" | "FUNKTIONEN" | "DISPLAYNAME" => {}
            _ => {}
        }
        Ok(())
    }

    fn begin_value<'m>(
        refs: &'m ModuleRefs<'m>,
        file: &mut DataConservation<'m>,
        format_ok: bool,
        name: &str,
        keyword: &str,
    ) -> Option<CharTarget<'m>> {
        if !format_ok {
            file.skipped_values
                .push((name.to_string(), ErrorType::Format));
            return None;
        }
        let Some(target) = refs.char_target(name) else {
            file.skipped_values
                .push((name.to_string(), ErrorType::NotFound));
            return None;
        };
        if !Self::keyword_type_matches(target, keyword) {
            file.skipped_values
                .push((name.to_string(), ErrorType::TypeDiffers));
            return None;
        }
        Some(target)
    }

    fn keyword_type_matches(target: CharTarget<'_>, keyword: &str) -> bool {
        match keyword {
            "FESTWERT" => target.char_type() == CharacteristicType::VALUE,
            "TEXTSTRING" => target.char_type() == CharacteristicType::ASCII,
            "STUETZSTELLENVERTEILUNG" => matches!(target, CharTarget::AxisPts(_)),
            "FESTWERTEBLOCK" => target
                .characteristic()
                .is_some_and(|c| c.char_type == CharacteristicType::VAL_BLK),
            "GRUPPENKENNLINIE" | "FESTKENNLINIE" | "KENNLINIE" => {
                target.char_type() == CharacteristicType::CURVE
            }
            "GRUPPENKENNFELD" | "KENNFELD" | "FESTKENNFELD" => {
                target.char_type() == CharacteristicType::MAP
            }
            _ => false,
        }
    }

    fn parse_value(token: &str, tab: Option<TabRef<'_>>) -> std::result::Result<f64, String> {
        match tab {
            Some(t) => Ok(t.to_raw(token)),
            None => parse_double(token),
        }
    }
}

fn data_err(message: String) -> Error {
    Error::DataFile { line: 0, message }
}

/// `("[^"\\]*(?:\\.[^"\\]*)*")|(?:[^\s\t]+)` + `Match.Value.Trim('"')`:
fn tokenize_dcm(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i] == b'"' {
            i += 1;
            let mut tok = String::new();
            while i < b.len() {
                if b[i] == b'\\' && i + 1 < b.len() {
                    tok.push(b[i] as char);
                    tok.push(b[i + 1] as char);
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                tok.push(b[i] as char);
                i += 1;
            }
            out.push(tok.trim_matches('"').to_string());
        } else {
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(line[start..i].trim_matches('"').to_string());
        }
    }
    out
}

// ============================================================================
// Matlab .m
// ============================================================================

pub struct MatlabFile;

impl MatlabFile {
    pub fn open<'a>(
        path: impl AsRef<std::path::Path>,
        refs: &'a ModuleRefs<'a>,
    ) -> Result<DataConservation<'a>> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        Self::open_str(&text, refs)
    }

    pub fn open_str<'m>(text: &str, refs: &'m ModuleRefs<'m>) -> Result<DataConservation<'m>> {
        let mut buf = String::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('%') {
                continue;
            }
            buf.push(' ');
            buf.push_str(line);
        }
        let mut values: IndexMap<String, ConservationValue<'m>> = IndexMap::new();
        let mut axes: HashMap<String, Vec<f64>> = HashMap::new();
        let mut skipped: Vec<(String, ErrorType)> = Vec::new();
        for segment in buf.split(';').filter(|s| !s.is_empty()) {
            for (name, rhs) in scan_assignments(segment) {
                let Some(target) = refs.char_target(&name) else {
                    if name.ends_with("_AXIS") && name.len() >= 7 {
                        let base = &name[..name.len() - 7];
                        if refs.char_target(base).is_some() {
                            let vals = Self::parse_number_list(&rhs)?;
                            axes.insert(name.clone(), vals);
                        }
                    }
                    continue;
                };
                let char_type = target.char_type();
                let (descrs, flags) = DataConservation::axis_var_flags(refs, target);
                let mut v = ConservationValue::pending(refs, target);
                match char_type {
                    CharacteristicType::VALUE => {
                        v.value = ValueData::Scalar(parse_double(&rhs).map_err(data_err)?);
                    }
                    CharacteristicType::ASCII => {
                        let mut s = String::new();
                        for piece in Self::split_bracket_list(&rhs) {
                            let b = piece.trim().parse::<u8>().map_err(|_| {
                                data_err(format!("{name}: invalid ASCII byte {piece:?}"))
                            })?;
                            s.push(b as char);
                        }
                        v.value = ValueData::Text(s);
                    }
                    CharacteristicType::VAL_BLK => {
                        let parts = Self::split_bracket_list(&rhs);
                        let mut data = Vec::with_capacity(parts.len());
                        for p in &parts {
                            data.push(parse_double(p).map_err(data_err)?);
                        }
                        v.value = ValueData::Array {
                            dims: vec![data.len()],
                            data,
                        };
                    }
                    CharacteristicType::CURVE => {
                        let parts = Self::split_bracket_list(&rhs);
                        if descrs.is_empty() {
                            return Err(data_err(format!("{name}: CURVE without AXIS_DESCR")));
                        }
                        let max = DataConservation::check_axis(&descrs, &flags, 0, parts.len());
                        if max == 0 {
                            skipped.push((name.clone(), ErrorType::XAxisInvalid));
                            continue;
                        }
                        if parts.len() > max {
                            continue;
                        }
                        let mut data = vec![0.0; max];
                        for (i, p) in parts.iter().enumerate() {
                            data[i] = parse_double(p).map_err(data_err)?;
                        }
                        v.value = ValueData::Array {
                            dims: vec![max],
                            data,
                        };
                    }
                    CharacteristicType::MAP => {
                        let parts = Self::split_bracket_list(&rhs);
                        if descrs.len() < 2 {
                            return Err(data_err(format!("{name}: MAP without 2 AXIS_DESCR")));
                        }
                        let mx = descrs[0].max_axis_points.max(0) as usize;
                        let my = descrs[1].max_axis_points.max(0) as usize;
                        if parts.len() > mx * my {
                            continue;
                        }
                        let mut arr = ValueData::zero_array(&[mx, my]);
                        if parts.len() < mx * my {
                            return Err(data_err(format!(
                                "{name}: MAP value count {} < capacity {}",
                                parts.len(),
                                mx * my
                            )));
                        }
                        let mut num = 0;
                        for m in 0..my {
                            for n in 0..mx {
                                let val = parse_double(&parts[num]).map_err(data_err)?;
                                arr.set(&[n, m], val).map_err(data_err)?;
                                num += 1;
                            }
                        }
                        v.value = arr;
                    }
                    CharacteristicType::CUBOID => {
                        let parts = Self::split_bracket_list(&rhs);
                        let dims = Self::axis_dims(&descrs, 3, &name)?;
                        let cap: usize = dims.iter().product();
                        if parts.len() > cap {
                            continue;
                        }
                        if parts.len() < cap {
                            return Err(data_err(format!(
                                "{name}: CUBOID value count {} < capacity {cap}",
                                parts.len()
                            )));
                        }
                        let mut arr = ValueData::zero_array(&dims);
                        let mut num = 0;
                        for z in 0..dims[2] {
                            for y in 0..dims[1] {
                                for x in 0..dims[0] {
                                    let val = parse_double(&parts[num]).map_err(data_err)?;
                                    arr.set(&[x, y, z], val).map_err(data_err)?;
                                    num += 1;
                                }
                            }
                        }
                        v.value = arr;
                    }
                    CharacteristicType::CUBE_4 => {
                        let parts = Self::split_bracket_list(&rhs);
                        let dims = Self::axis_dims(&descrs, 4, &name)?;
                        let cap: usize = dims.iter().product();
                        if parts.len() > cap {
                            continue;
                        }
                        if parts.len() < cap {
                            return Err(data_err(format!(
                                "{name}: CUBE_4 value count {} < capacity {cap}",
                                parts.len()
                            )));
                        }
                        let mut arr = ValueData::zero_array(&dims);
                        let mut num = 0;
                        for w in 0..dims[3] {
                            for z in 0..dims[2] {
                                for y in 0..dims[1] {
                                    for x in 0..dims[0] {
                                        let val = parse_double(&parts[num]).map_err(data_err)?;
                                        arr.set(&[x, y, z, w], val).map_err(data_err)?;
                                        num += 1;
                                    }
                                }
                            }
                        }
                        v.value = arr;
                    }
                    CharacteristicType::CUBE_5 => {
                        let parts = Self::split_bracket_list(&rhs);
                        let dims = Self::axis_dims(&descrs, 5, &name)?;
                        let cap: usize = dims.iter().product();
                        if parts.len() > cap {
                            continue;
                        }
                        let mut arr = ValueData::zero_array(&dims);
                        let mut num = 0;
                        for v5 in 0..dims[3] {
                            for w in 0..dims[3] {
                                for z in 0..dims[2] {
                                    for y in 0..dims[1] {
                                        for x in 0..dims[0] {
                                            if num >= parts.len() {
                                                return Err(data_err(format!(
                                                    "{name}: CUBE_5 value underflow"
                                                )));
                                            }
                                            let val =
                                                parse_double(&parts[num]).map_err(data_err)?;
                                            arr.set(&[x, y, z, w, v5], val).map_err(data_err)?;
                                            num += 1;
                                        }
                                    }
                                }
                            }
                        }
                        v.value = arr;
                    }
                    _ => {
                        let parts = Self::split_bracket_list(&rhs);
                        let mut data = Vec::with_capacity(parts.len());
                        for p in &parts {
                            data.push(parse_double(p).map_err(data_err)?);
                        }
                        v.value = ValueData::Array {
                            dims: vec![data.len()],
                            data,
                        };
                    }
                }
                values.insert(name.clone(), v);
            }
        }
        for (name, v) in values.iter_mut() {
            let count = v.axis_descrs().len();
            if count >= 1 {
                let mut axis_values = Vec::with_capacity(count);
                for i in 0..count {
                    let key = format!("{name}_{}_AXIS", AXIS_LETTERS[i.min(4)]);
                    axis_values.push(axes.get(&key).cloned().unwrap_or_default());
                }
                v.axis_values = axis_values;
            }
        }
        let file = DataConservation {
            values: values.into_values().collect(),
            skipped_values: skipped,
        };
        Ok(file)
    }

    pub fn save(
        path: impl AsRef<std::path::Path>,
        description: &str,
        values: &[ConservationValue<'_>],
        add_parameter_desc: bool,
    ) -> Result<usize> {
        let (text, count) = Self::save_string(description, values, add_parameter_desc)?;
        std::fs::write(path, text)?;
        Ok(count)
    }

    pub fn save_string(
        description: &str,
        values: &[ConservationValue<'_>],
        add_parameter_desc: bool,
    ) -> Result<(String, usize)> {
        let mut out = String::new();
        out.push_str("% Created by  ()");
        out.push_str(EOL);
        let (date, time) = utc_now_parts();
        out.push_str(&format!("% Created at {date} {time}"));
        out.push_str(EOL);
        if !description.is_empty() {
            for line in description.split(EOL) {
                out.push_str("% ");
                out.push_str(line.trim_matches(' '));
                out.push_str(EOL);
            }
        }
        out.push_str(EOL);
        let mut count = 0;
        for v in values {
            if add_parameter_desc {
                let s = Self::parameter_desc(v);
                if !s.is_empty() {
                    out.push_str(&s);
                }
            }
            let name = v.name();
            match v.char_type() {
                CharacteristicType::VALUE => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let writes_raw = v.main_compu().and_then(|cm| v.refs.vtab_of(cm)).is_some();
                    if writes_raw {
                        out.push_str(&format!("{name} = {};", to_dec(v.fnc_value())));
                    } else {
                        out.push_str(&format!("{name} = {};", v.to_single_value(true)));
                    }
                    out.push_str(EOL);
                }
                CharacteristicType::ASCII => {
                    let ValueData::Text(text) = &v.value else {
                        return Err(data_err(format!("{name}: ASCII value is not text")));
                    };
                    out.push_str(&format!("{name} = ["));
                    let bytes: Vec<String> = text
                        .chars()
                        .map(|c| ((c as u32) & 0xFF).to_string())
                        .collect();
                    out.push_str(&bytes.join(" "));
                    out.push_str("];");
                    out.push_str(EOL);
                }
                CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let data = v
                        .value
                        .as_slice()
                        .ok_or_else(|| data_err(format!("{name}: not an array")))?;
                    out.push_str(&format!("{name} = ["));
                    out.push_str(&Self::join_dec(data));
                    out.push_str("];");
                    out.push_str(EOL);
                    if v.char_type() == CharacteristicType::CURVE {
                        Self::write_axes(&mut out, v, add_parameter_desc)?;
                    }
                }
                CharacteristicType::MAP => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let (dims, data) = Self::dims_data(v)?;
                    let (nx, ny) = (dims[0], dims[1]);
                    out.push_str(&format!("{name} = ["));
                    for y in 0..ny {
                        for x in 0..nx {
                            out.push_str(&to_dec(data[x + nx * y]));
                            if x < nx - 1 {
                                out.push(' ');
                            }
                        }
                        if y < ny - 1 {
                            out.push_str(EOL);
                        }
                    }
                    out.push_str("];");
                    out.push_str(EOL);
                    Self::write_axes(&mut out, v, add_parameter_desc)?;
                }
                CharacteristicType::CUBOID
                | CharacteristicType::CUBE_4
                | CharacteristicType::CUBE_5 => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let (dims, data) = Self::dims_data(v)?;
                    out.push_str(&format!("{name} = ["));
                    Self::write_ndim(&mut out, &dims, data);
                    out.push_str("];");
                    out.push_str(EOL);
                    Self::write_axes(&mut out, v, add_parameter_desc)?;
                }
                _ => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let data = v
                        .value
                        .as_slice()
                        .ok_or_else(|| data_err(format!("{name}: not an array")))?;
                    out.push_str(&format!("{name} = ["));
                    out.push_str(&Self::join_dec(data));
                    out.push_str("];");
                    out.push_str(EOL);
                }
            }
            count += 1;
        }
        Ok((out, count))
    }

    fn write_axes(
        out: &mut String,
        v: &ConservationValue<'_>,
        add_parameter_desc: bool,
    ) -> Result<()> {
        let descrs = v.axis_descrs();
        for (i, d) in descrs.iter().enumerate() {
            if add_parameter_desc {
                let unit = v.refs.unit_of(d.phys_unit.as_deref(), &d.conversion);
                if !unit.is_empty() {
                    out.push_str(&format!("% Unit: {unit}"));
                    out.push_str(EOL);
                }
            }
            let letter = AXIS_LETTERS[i.min(4)];
            out.push_str(&format!("{}_{letter}_AXIS = [", v.name()));
            let axis_len = v.axis_values.len();
            for j in 0..axis_len {
                let val = v.axis_values.get(i).and_then(|a| a.get(j)).ok_or_else(|| {
                    data_err(format!("{}: axis {i} index {j} out of range", v.name()))
                })?;
                out.push_str(&to_dec(*val));
                if j < axis_len - 1 {
                    out.push(' ');
                }
            }
            out.push_str("];");
            out.push_str(EOL);
        }
        Ok(())
    }

    fn parameter_desc(v: &ConservationValue<'_>) -> String {
        let desc = v.description();
        if desc.is_empty() {
            return String::new();
        }
        let unit = &v.unit;
        let text = if unit.is_empty() {
            desc.to_string()
        } else {
            format!("{desc}\nUnit: {unit}")
        };
        let mut out = String::from(EOL);
        for line in text.split(['\r', '\n']).filter(|s| !s.is_empty()) {
            out.push_str("% ");
            out.push_str(line);
            out.push_str(EOL);
        }
        out
    }

    fn write_ndim(out: &mut String, dims: &[usize], data: &[f64]) {
        let (nx, ny) = (dims[0], dims[1]);
        let slice: usize = dims[..2].iter().product();
        let slices: usize = dims[2..].iter().product();
        for s in 0..slices {
            for y in 0..ny {
                for x in 0..nx {
                    out.push_str(&to_dec(data[s * slice + y * nx + x]));
                    if x < nx - 1 {
                        out.push(' ');
                    }
                }
                if y < ny - 1 {
                    out.push_str(EOL);
                }
            }
        }
    }

    fn dims_data<'a>(v: &'a ConservationValue<'a>) -> Result<(Vec<usize>, &'a [f64])> {
        match &v.value {
            ValueData::Array { dims, data } => Ok((dims.clone(), data)),
            _ => Err(data_err(format!("{}: not an array", v.name()))),
        }
    }

    fn join_dec(data: &[f64]) -> String {
        data.iter()
            .map(|v| to_dec(*v))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn split_bracket_list(rhs: &str) -> Vec<String> {
        rhs.trim_matches(['[', ']'])
            .split([' ', '\t'])
            .map(str::to_string)
            .collect()
    }

    fn parse_number_list(rhs: &str) -> Result<Vec<f64>> {
        Self::split_bracket_list(rhs)
            .iter()
            .map(|p| parse_double(p).map_err(data_err))
            .collect()
    }

    fn axis_dims(descrs: &[&AxisDescr], n: usize, name: &str) -> Result<Vec<usize>> {
        if descrs.len() < n {
            return Err(data_err(format!("{name}: expected {n} AXIS_DESCR")));
        }
        Ok(descrs
            .iter()
            .take(n)
            .map(|d| d.max_axis_points.max(0) as usize)
            .collect())
    }
}

fn scan_assignments(segment: &str) -> Vec<(String, String)> {
    let b = segment.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !is_word_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_word_byte(b[i]) {
            i += 1;
        }
        let name = &segment[start..i];
        let mut j = i;
        // \s+ = \s+
        let Some(j2) = skip_ws(b, j).filter(|&k| k > j) else {
            continue;
        };
        j = j2;
        if j >= b.len() || b[j] != b'=' {
            continue;
        }
        j += 1;
        let Some(j3) = skip_ws(b, j).filter(|&k| k > j) else {
            continue;
        };
        j = j3;
        if j < b.len() && b[j] == b'[' {
            // \[[-\d\.\s]+\]
            let content_start = j + 1;
            let mut k = content_start;
            while k < b.len() && is_num_list_byte(b[k]) {
                k += 1;
            }
            if k > content_start && k < b.len() && b[k] == b']' {
                out.push((name.to_string(), segment[j..=k].to_string()));
                i = k + 1;
            } else {
                i = start + 1;
            }
        } else {
            // [-\d\.]+
            let mut k = j;
            while k < b.len() && (b[k] == b'-' || b[k] == b'.' || b[k].is_ascii_digit()) {
                k += 1;
            }
            if k > j {
                out.push((name.to_string(), segment[j..k].to_string()));
                i = k;
            } else {
                i = start + 1;
            }
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_num_list_byte(b: u8) -> bool {
    b == b'-' || b == b'.' || b.is_ascii_digit() || b.is_ascii_whitespace()
}

fn skip_ws(b: &[u8], mut i: usize) -> Option<usize> {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    Some(i)
}

// ============================================================================
// CANape PAR
// ============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParType {
    #[default]
    Unknown,
    /// `CANape PAR V3.1`.
    CANapeV3_1,
    /// `CANape PAR V3.2`.
    CANapeV3_2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParDataType {
    String,
    Int,
    UInt,
    Float,
    Double,
}

impl ParDataType {
    fn as_str(self) -> &'static str {
        match self {
            ParDataType::String => "STRING",
            ParDataType::Int => "INT",
            ParDataType::UInt => "UINT",
            ParDataType::Float => "FLOAT",
            ParDataType::Double => "DOUBLE",
        }
    }
}

pub struct ParFile;

impl ParFile {
    pub fn open<'a>(
        path: impl AsRef<std::path::Path>,
        refs: &'a ModuleRefs<'a>,
    ) -> Result<DataConservation<'a>> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        Self::open_str(&text, refs)
    }

    pub fn open_str<'m>(text: &str, refs: &'m ModuleRefs<'m>) -> Result<DataConservation<'m>> {
        let mut file = DataConservation::default();
        let mut par_type = ParType::Unknown;
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            if par_type == ParType::Unknown {
                if line.starts_with("CANape PAR V3.1") {
                    par_type = ParType::CANapeV3_1;
                }
                if line.starts_with("CANape PAR V3.2") {
                    par_type = ParType::CANapeV3_2;
                }
                continue;
            }
            let Some((name, param, value_text)) = parse_par_line(line) else {
                continue;
            };
            let text2 = value_text
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let Some((pdt, _bits, dims)) = parse_par_param(&param) else {
                continue;
            };
            let Some(target) = refs.char_target(&name) else {
                continue;
            };
            let char_type = target.char_type();
            let Some(mut v) = Self::create_value(refs, target, char_type, pdt, &dims) else {
                file.skipped_values
                    .push((name.clone(), ErrorType::NotFound));
                continue;
            };
            match char_type {
                CharacteristicType::VALUE => {
                    v.value = ValueData::Scalar(parse_double(&text2).map_err(data_err)?);
                }
                CharacteristicType::ASCII => {
                    let mut s = String::new();
                    for piece in text2.split([' ', '\t']).filter(|p| !p.is_empty()) {
                        let code = piece.parse::<u32>().map_err(|_| {
                            data_err(format!("{name}: invalid ASCII code {piece:?}"))
                        })?;
                        s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    v.value = ValueData::Text(s);
                }
                CharacteristicType::VAL_BLK | CharacteristicType::CURVE => {
                    let n = dims.get(1).copied().unwrap_or(0).max(0) as usize;
                    let mut arr = ValueData::zero_array(&[n]);
                    let mut num = 0;
                    if !text2.is_empty() {
                        arr.set(&[0], parse_double(&text2).map_err(data_err)?)
                            .map_err(data_err)?;
                        num = 1;
                    }
                    for l in num..n {
                        let l2 = Self::read_non_comment(&mut lines)
                            .ok_or_else(|| data_err(format!("{name}: unexpected end of file")))?;
                        let parts: Vec<&str> = l2.split([':', ';']).collect();
                        let val =
                            parse_double(parts.get(1).unwrap_or(&"").trim()).map_err(data_err)?;
                        arr.set(&[l], val).map_err(data_err)?;
                    }
                    v.value = arr;
                }
                CharacteristicType::MAP => {
                    let ny = dims.first().copied().unwrap_or(0).max(0) as usize;
                    let nx = dims.get(1).copied().unwrap_or(0).max(0) as usize;
                    let mut arr = ValueData::zero_array(&[nx, ny]);
                    let mut num = 0;
                    if !text2.is_empty() {
                        arr.set(&[0, 0], parse_double(&text2).map_err(data_err)?)
                            .map_err(data_err)?;
                        num = 1;
                    }
                    for m in 0..ny {
                        for n in num..nx {
                            let l2 = Self::read_non_comment(&mut lines).ok_or_else(|| {
                                data_err(format!("{name}: unexpected end of file"))
                            })?;
                            let parts: Vec<&str> = l2.split([':', ';']).collect();
                            let val = parse_double(parts.get(1).unwrap_or(&"").trim())
                                .map_err(data_err)?;
                            arr.set(&[n, m], val).map_err(data_err)?;
                        }
                        num = 0;
                    }
                    v.value = arr;
                }
                CharacteristicType::CUBOID => {
                    let nz = dims.first().copied().unwrap_or(0).max(0) as usize;
                    let ny = dims.get(1).copied().unwrap_or(0).max(0) as usize;
                    let nx = dims.get(2).copied().unwrap_or(0).max(0) as usize;
                    let mut arr = ValueData::zero_array(&[nx, ny, nz]);
                    let mut num = 0;
                    if !text2.is_empty() {
                        arr.set(&[0, 0, 0], parse_double(&text2).map_err(data_err)?)
                            .map_err(data_err)?;
                        num = 1;
                    }
                    for i in 0..nz {
                        for j in 0..ny {
                            for k in num..nx {
                                let l2 = Self::read_non_comment(&mut lines).ok_or_else(|| {
                                    data_err(format!("{name}: unexpected end of file"))
                                })?;
                                let parts: Vec<&str> = l2.split([':', ';']).collect();
                                let val = parse_double(parts.get(1).unwrap_or(&"").trim())
                                    .map_err(data_err)?;
                                arr.set(&[k, j, i], val).map_err(data_err)?;
                            }
                            num = 0;
                        }
                    }
                    v.value = arr;
                }
                _ => {}
            }
            let descrs = v.axis_descrs();
            let rl = v.record_layout();
            let mut dim_idx = dims.len() as i64 - 1;
            for (j, _d) in descrs.iter().enumerate() {
                let has_axis_pts = rl
                    .and_then(|rl| rl.axis_pts.get(j).and_then(|a| a.as_ref()))
                    .is_some();
                if !has_axis_pts {
                    continue;
                }
                let letter = AXIS_LETTERS[j.min(4)];
                if dim_idx < 0 {
                    return Err(data_err(format!("{name}: axis dimension underflow")));
                }
                let count = dims[dim_idx as usize].max(0) as usize;
                dim_idx -= 1;
                for i in 0..count {
                    let l2 = Self::read_non_comment(&mut lines)
                        .ok_or_else(|| data_err(format!("{name}: unexpected end of file")))?;
                    let parts: Vec<&str> = l2.split([':', ';']).collect();
                    if parts.first().map(|p| p.trim()) == Some(letter.to_string().as_str()) {
                        let val =
                            parse_double(parts.get(1).unwrap_or(&"").trim()).map_err(data_err)?;
                        let slot = v
                            .axis_values
                            .get_mut(j)
                            .and_then(|a| a.get_mut(i))
                            .ok_or_else(|| {
                                data_err(format!("{name}: axis {j} index {i} out of range"))
                            })?;
                        *slot = val;
                    }
                }
            }
            file.values.push(v);
        }
        Ok(file)
    }

    /// values, encoding, type, deviceNumber, onlyCurrentVariant, user, department,
    /// project, subject)`).
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        path: impl AsRef<std::path::Path>,
        description: Option<&str>,
        a2l_source: Option<&str>,
        data_source: Option<&str>,
        values: &[ConservationValue<'_>],
        par_type: ParType,
        device_number: bool,
        only_current_variant: bool,
        user: &str,
        department: &str,
        project: &str,
        subject: &str,
    ) -> Result<usize> {
        let (text, count) = Self::save_string(
            description,
            a2l_source,
            data_source,
            values,
            par_type,
            device_number,
            only_current_variant,
            user,
            department,
            project,
            subject,
        )?;
        std::fs::write(path, text)?;
        Ok(count)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_string(
        description: Option<&str>,
        a2l_source: Option<&str>,
        data_source: Option<&str>,
        values: &[ConservationValue<'_>],
        par_type: ParType,
        device_number: bool,
        only_current_variant: bool,
        user: &str,
        department: &str,
        project: &str,
        subject: &str,
    ) -> Result<(String, usize)> {
        let mut out = String::new();
        out.push_str(if par_type == ParType::CANapeV3_1 {
            "CANape PAR V3.1"
        } else {
            "CANape PAR V3.2"
        });
        out.push_str(": ");
        let a2l_name = a2l_source.map(file_name).unwrap_or_default();
        if !a2l_name.is_empty() {
            if a2l_name.contains([' ', '\t']) {
                out.push_str(&format!("\"{a2l_name}\" "));
            } else {
                out.push_str(&format!("{a2l_name} "));
            }
        } else {
            out.push_str("~ ");
        }
        out.push_str(if device_number { "1 " } else { "0 " });
        out.push_str(if only_current_variant { "1 " } else { "0 " });
        let data_name = data_source.map(file_name).unwrap_or_default();
        out.push_str(&data_name);
        out.push_str(EOL);
        for s in [user, department, project, subject] {
            out.push_str(&format!("; {s}"));
            out.push_str(EOL);
        }
        if let Some(desc) = description {
            for line in desc.split(['\r', '\n']).filter(|s| !s.is_empty()) {
                out.push_str(&format!("; {line}"));
                out.push_str(EOL);
            }
        }
        out.push_str(EOL);
        let mut count = 0;
        for v in values {
            let name = v.name();
            let rl = v
                .record_layout()
                .ok_or_else(|| data_err(format!("{name}: record layout not found")))?;
            match v.char_type() {
                CharacteristicType::VALUE => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let (bits, pdt) =
                        Self::par_data_type(rl.fnc_values.as_ref().map(|f| f.data_type));
                    out.push_str(&format!(
                        "{name} [{}({bits})] {} ; {}",
                        pdt.as_str(),
                        to_dec(v.fnc_value()),
                        v.to_single_value(true)
                    ));
                    out.push_str(EOL);
                }
                CharacteristicType::ASCII => {
                    let ValueData::Text(text) = &v.value else {
                        return Err(data_err(format!("{name}: ASCII value is not text")));
                    };
                    if text.is_empty() {
                        continue;
                    }
                    out.push_str(&format!("{name} [STRING({})] ", text.chars().count()));
                    for c in text.chars() {
                        out.push_str(&format!("{} ", c as u32));
                    }
                    out.push_str(&format!("; {text}"));
                    out.push_str(EOL);
                }
                CharacteristicType::VAL_BLK | CharacteristicType::CURVE if !v.is_axis_pts() => {
                    let data = v
                        .value
                        .as_slice()
                        .ok_or_else(|| data_err(format!("{name}: not an array")))?;
                    if data.is_empty() || v.value.is_nan_marker() {
                        continue;
                    }
                    let (bits, pdt) =
                        Self::par_data_type(rl.fnc_values.as_ref().map(|f| f.data_type));
                    out.push_str(&format!(
                        "{name} [{}({bits}),(1,{})] ",
                        pdt.as_str(),
                        data.len()
                    ));
                    Self::write_value_lines(&mut out, v, par_type);
                }
                CharacteristicType::MAP => {
                    if v.value.is_nan_marker() {
                        continue;
                    }
                    let (dims, _data) = MatlabFile::dims_data(v)?;
                    let (bits, pdt) =
                        Self::par_data_type(rl.fnc_values.as_ref().map(|f| f.data_type));
                    out.push_str(&format!(
                        "{name} [{}({bits}),({},{})] ",
                        pdt.as_str(),
                        dims[1],
                        dims[0]
                    ));
                    Self::write_value_lines(&mut out, v, par_type);
                }
                _ => {
                    let data = v
                        .value
                        .as_slice()
                        .ok_or_else(|| data_err(format!("{name}: not an array")))?;
                    if data.is_empty() || v.value.is_nan_marker() {
                        continue;
                    }
                    let (bits, pdt) =
                        Self::par_data_type(rl.axis_pts[0].as_ref().map(|a| a.data_type));
                    out.push_str(&format!(
                        "{name} [{}({bits}),(1,{})] ",
                        pdt.as_str(),
                        data.len()
                    ));
                    Self::write_value_lines(&mut out, v, par_type);
                }
            }
            let descrs = v.axis_descrs();
            for (j, d) in descrs.iter().enumerate() {
                if rl.axis_pts.get(j).and_then(|a| a.as_ref()).is_none() {
                    continue;
                }
                let letter = AXIS_LETTERS[match j {
                    0 => 0,
                    1 => 1,
                    _ => 2,
                }];
                let axis_compu = v.refs.compu_method(&d.conversion);
                let axis_vtab = axis_compu.and_then(|cm| v.refs.vtab_of(cm));
                let raw_values = v.axis_values.get(j).cloned().unwrap_or_default();
                for raw in raw_values {
                    let phys = Self::phys_text(v, axis_compu, axis_vtab, raw);
                    out.push_str(&format!("{letter} : {} ; {phys}", to_dec(raw)));
                    out.push_str(EOL);
                }
            }
            count += 1;
        }
        Ok((out, count))
    }

    fn par_data_type(data_type: Option<DataType>) -> (u32, ParDataType) {
        let pdt = match data_type.unwrap_or_default() {
            DataType::Float32Ieee | DataType::Float16Ieee => ParDataType::Float,
            DataType::SByte | DataType::SWord | DataType::SLong => ParDataType::Int,
            DataType::UByte | DataType::UWord | DataType::ULong => ParDataType::UInt,
            _ => ParDataType::Double,
        };
        (size_in_bit(data_type.unwrap_or_default()), pdt)
    }

    fn write_value_lines(out: &mut String, v: &ConservationValue<'_>, par_type: ParType) {
        let mut first = true;
        if par_type == ParType::CANapeV3_2 {
            first = false;
            out.push_str(EOL);
        }
        let compu = v.main_compu();
        let vtab = compu.and_then(|cm| v.refs.vtab_of(cm));
        for raw in v.fnc_values() {
            let phys = Self::phys_text(v, compu, vtab, raw);
            if first {
                out.push_str(&format!("{} ; {phys}", to_dec(raw)));
                out.push_str(EOL);
                first = false;
            } else {
                out.push_str(&format!(": {} ; {phys}", to_dec(raw)));
                out.push_str(EOL);
            }
        }
    }

    fn phys_text(
        v: &ConservationValue<'_>,
        compu: Option<&CompuMethod>,
        vtab: Option<&CompuVtab>,
        raw: f64,
    ) -> String {
        if let Some(vt) = vtab {
            return vt
                .verbs
                .get(&to_int64(raw))
                .cloned()
                .or_else(|| vt.base.default_value.clone())
                .unwrap_or_default();
        }
        to_dec(v.to_physical(compu, raw))
    }

    fn create_value<'m>(
        refs: &'m ModuleRefs<'m>,
        target: CharTarget<'m>,
        char_type: CharacteristicType,
        pdt: ParDataType,
        dims: &[i64],
    ) -> Option<ConservationValue<'m>> {
        let mut v = match dims.len() {
            1 => {
                if char_type != CharacteristicType::VALUE && char_type != CharacteristicType::ASCII
                {
                    return None;
                }
                let mut v = ConservationValue::pending(refs, target);
                v.value = if pdt != ParDataType::String {
                    ValueData::Scalar(f64::NAN)
                } else {
                    ValueData::Text(String::new())
                };
                v
            }
            2 => {
                let is_blk = char_type == CharacteristicType::VAL_BLK && dims[0] == 1;
                let is_curve = char_type == CharacteristicType::CURVE && dims[0] == 1;
                let is_map = char_type == CharacteristicType::MAP;
                if !is_blk && !is_curve && !is_map {
                    return None;
                }
                let mut v = ConservationValue::pending(refs, target);
                let descrs = v.axis_descrs();
                v.axis_values = descrs
                    .iter()
                    .map(|d| {
                        if d.axis_type == AxisType::STD_AXIS {
                            vec![0.0; d.max_axis_points.max(0) as usize]
                        } else {
                            Vec::new()
                        }
                    })
                    .collect();
                v
            }
            3 => {
                let mut v = ConservationValue::pending(refs, target);
                let descrs = v.axis_descrs();
                v.axis_values = descrs
                    .iter()
                    .map(|d| vec![0.0; d.max_axis_points.max(0) as usize])
                    .collect();
                v
            }
            _ => return None,
        };
        v.value_format = ValueFormat::Raw;
        Some(v)
    }

    fn read_non_comment<'a, I>(lines: &mut I) -> Option<String>
    where
        I: Iterator<Item = &'a str>,
    {
        loop {
            let line = lines.next()?;
            if !line.starts_with(';') {
                return Some(line.to_string());
            }
        }
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// `(?<name>[\w\.\[\]]+)\s+(?<device>.*)\s*\[(?<param>.*)\]\s*(?<value>.*)`
fn parse_par_line(line: &str) -> Option<(String, String, String)> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && (is_word_byte(b[i]) || b[i] == b'.' || b[i] == b'[' || b[i] == b']') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let name = &line[..i];
    // \s+
    let j = skip_ws(b, i)?;
    if j == i {
        return None;
    }
    // device.* \s* \[ param.* \] \s* value.*
    let rest = &line[j..];
    let rb = rest.rfind(']')?;
    let lb = rest[..rb].rfind('[')?;
    let param = &rest[lb + 1..rb];
    let value = rest[rb + 1..].trim_start();
    Some((name.to_string(), param.to_string(), value.to_string()))
}

/// `(?<type>STRING|INT|UINT|FLOAT|DOUBLE)(?:\s*\((?<size>\d+)\)(?:\s*,\s*\((?<dim>.*)\))*)*`:
fn parse_par_param(param: &str) -> Option<(ParDataType, Option<u32>, Vec<i64>)> {
    let keywords = [
        ("STRING", ParDataType::String),
        ("INT", ParDataType::Int),
        ("UINT", ParDataType::UInt),
        ("FLOAT", ParDataType::Float),
        ("DOUBLE", ParDataType::Double),
    ];
    let mut found: Option<(usize, &str, ParDataType)> = None;
    for (kw, pdt) in keywords {
        if let Some(pos) = param.find(kw) {
            if found.as_ref().is_none_or(|(p, _, _)| pos < *p) {
                found = Some((pos, kw, pdt));
            }
        }
    }
    let (pos, kw, pdt) = found?;
    let rest = &param[pos + kw.len()..];
    let b = rest.as_bytes();
    // \s*\((?<size>\d+)\)
    let mut i = skip_ws(b, 0)?;
    let mut size = None;
    let mut dim: Option<String> = None;
    if i < b.len() && b[i] == b'(' {
        let mut k = i + 1;
        let s = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > s && k < b.len() && b[k] == b')' {
            size = rest[s..k].parse::<u32>().ok();
            i = k + 1;
        }
    }
    let mut j = skip_ws(b, i)?;
    if j < b.len() && b[j] == b',' {
        j = skip_ws(b, j + 1)?;
        if j < b.len() && b[j] == b'(' {
            let rb = rest.rfind(')')?;
            if rb > j {
                dim = Some(rest[j + 1..rb].to_string());
            }
        }
    }
    let dims = match dim.as_deref().filter(|s| !s.is_empty()) {
        Some(d) => d
            .split(',')
            .map(|p| p.trim().parse::<i64>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?,
        None => vec![1],
    };
    Some((pdt, size, dims))
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::model::project::Project;

    const A2L: &str = "\
ASAP2_VERSION 1 71
/begin PROJECT Proj \"\"
/begin MODULE Mod \"\"
/begin MOD_PAR \"\"
EPK \"EPK-123\"
/end MOD_PAR
/begin RECORD_LAYOUT RL_U8
FNC_VALUES 1 UBYTE ROW_DIR DIRECT
/end RECORD_LAYOUT
/begin RECORD_LAYOUT RL_S16
FNC_VALUES 1 SWORD ROW_DIR DIRECT
/end RECORD_LAYOUT
/begin RECORD_LAYOUT RL_F32
FNC_VALUES 1 FLOAT32_IEEE ROW_DIR DIRECT
/end RECORD_LAYOUT
/begin RECORD_LAYOUT RL_CURVE
AXIS_PTS_X 1 UWORD INDEX_INCR DIRECT
FNC_VALUES 2 SWORD ROW_DIR DIRECT
/end RECORD_LAYOUT
/begin RECORD_LAYOUT RL_MAP
AXIS_PTS_X 1 UWORD INDEX_INCR DIRECT
AXIS_PTS_Y 2 UWORD INDEX_INCR DIRECT
FNC_VALUES 3 FLOAT32_IEEE ROW_DIR DIRECT
/end RECORD_LAYOUT
/begin RECORD_LAYOUT RL_AXIS
AXIS_PTS_X 1 UWORD INDEX_INCR DIRECT
/end RECORD_LAYOUT
/begin COMPU_METHOD CM_IDENT \"identical\" IDENTICAL \"%6.2\" \"Nm\"
/end COMPU_METHOD
/begin COMPU_METHOD CM_AXIS \"axis conv\" IDENTICAL \"%4.0\" \"rpm\"
/end COMPU_METHOD
/begin COMPU_METHOD CM_LIN \"linear\" LINEAR \"%5.1\" \"kPa\"
COEFFS_LINEAR 2 10
/end COMPU_METHOD
/begin COMPU_METHOD CM_BOOL \"verb\" TAB_VERB \"%4.0\" \"-\"
COMPU_TAB_REF VT_BOOL
/end COMPU_METHOD
/begin COMPU_VTAB VT_BOOL \"bool tab\" TAB_VERB 2
0 \"OFF\"
1 \"ON\"
DEFAULT_VALUE \"?\"
/end COMPU_VTAB
/begin CHARACTERISTIC Scalar1 \"a scalar\" VALUE 0x1000 RL_U8 0 CM_IDENT 0 100
/end CHARACTERISTIC
/begin CHARACTERISTIC Text1 \"a text\" ASCII 0x1004 RL_U8 0 CM_IDENT 0 100 NUMBER 8
/end CHARACTERISTIC
/begin CHARACTERISTIC Bool1 \"a bool\" VALUE 0x1010 RL_U8 0 CM_BOOL 0 1
/end CHARACTERISTIC
/begin CHARACTERISTIC Block1 \"a block\" VAL_BLK 0x1020 RL_S16 0 CM_IDENT 0 100 NUMBER 4
/end CHARACTERISTIC
/begin CHARACTERISTIC Block2 \"matrix block\" VAL_BLK 0x1030 RL_S16 0 CM_IDENT 0 100 MATRIX_DIM 2 2
/end CHARACTERISTIC
/begin CHARACTERISTIC Curve1 \"a curve\" CURVE 0x1040 RL_CURVE 0 CM_LIN 0 100
/begin AXIS_DESCR STD_AXIS InpX CM_AXIS 4 0 4000
/end AXIS_DESCR
/end CHARACTERISTIC
/begin CHARACTERISTIC Map1 \"a map\" MAP 0x1050 RL_MAP 0 CM_IDENT 0 100
/begin AXIS_DESCR STD_AXIS InpX CM_AXIS 3 0 4000
/end AXIS_DESCR
/begin AXIS_DESCR STD_AXIS InpY CM_AXIS 2 0 10
/end AXIS_DESCR
/end CHARACTERISTIC
/begin AXIS_PTS AxisPts1 \"shared axis\" 0x1060 InpX RL_AXIS 0 CM_AXIS 4 0 4000
/end AXIS_PTS
/begin CHARACTERISTIC Curve2 \"grouped curve\" CURVE 0x1070 RL_CURVE 0 CM_LIN 0 100
/begin AXIS_DESCR COM_AXIS InpX CM_AXIS 4 0 4000 AXIS_PTS_REF AxisPts1
/end AXIS_DESCR
/end CHARACTERISTIC
/begin FUNCTION F1 \"func one\"
FUNCTION_VERSION \"1.0\"
/begin DEF_CHARACTERISTIC Scalar1 Curve1
/end DEF_CHARACTERISTIC
/end FUNCTION
/end MODULE
/end PROJECT";

    fn parse_project() -> Project {
        Project::parse_str(A2L).expect("fixture A2L must parse")
    }

    fn module(project: &Project) -> &Module {
        project.modules().next().expect("one module")
    }

    fn strip_date_line(text: &str) -> String {
        text.lines()
            .filter(|l| !l.contains("Created at"))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    #[test]
    fn error_type_descriptions() {
        assert_eq!(ErrorType::NotFound.description(), "Not found");
        assert_eq!(
            ErrorType::NoOfElementsDiffers.description(),
            "No of elements differ"
        );
        assert_eq!(ErrorType::Axis4Invalid.description(), "4. Axis invalid");
        assert_eq!(ErrorType::Axis5Invalid.to_string(), "5. Axis invalid");
    }

    #[test]
    fn decimal_count_parsing() {
        assert_eq!(decimal_count_of_format("%6.2"), 2);
        assert_eq!(decimal_count_of_format("%4.0"), 0);
        assert_eq!(decimal_count_of_format("%5.3f"), 3);
        assert_eq!(decimal_count_of_format(""), 0);
        assert_eq!(decimal_count_of_format("%"), 0);
        assert_eq!(decimal_count_of_format("%6"), 0);
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_int_val(" 42 "), 42);
        assert_eq!(parse_int_val("0x10"), 16);
        assert_eq!(parse_int_val("junk"), 0);
        assert!((parse_double(" 1.5 ").unwrap() - 1.5).abs() < 1e-12);
        assert!(parse_double("").is_err());
        assert_eq!(round_bankers(0.5, 0), 0.0);
        assert_eq!(round_bankers(1.5, 0), 2.0);
        assert_eq!(round_bankers(2.675, 2), 2.68);
        assert_eq!(format_fixed(1.5, 2), "1.50");
        assert_eq!(format_fixed(2.0, 0), "2");
        assert_eq!(to_decimal_string(250.7, DataType::UByte), "250");
        assert_eq!(to_decimal_string(-3.7, DataType::SWord), "-3");
        assert_eq!(to_decimal_string(1.5, DataType::Float32Ieee), "1.5");
        assert_eq!(size_in_bit(DataType::UByte), 8);
        assert_eq!(size_in_bit(DataType::Float64Ieee), 64);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20351), (2025, 9, 20));
    }

    #[test]
    fn dcm_tokenizer_basics() {
        assert_eq!(tokenize_dcm("WERT 1 2 3"), vec!["WERT", "1", "2", "3"]);
        assert_eq!(
            tokenize_dcm("EINHEIT_W \"Nm kg\""),
            vec!["EINHEIT_W", "Nm kg"]
        );
        assert_eq!(tokenize_dcm("TEXT \"a\\\"b\""), vec!["TEXT", "a\\\"b"]);
        assert_eq!(tokenize_dcm("   "), Vec::<String>::new());
        assert_eq!(
            tokenize_dcm("FESTWERTEBLOCK B 2 @ 2"),
            vec!["FESTWERTEBLOCK", "B", "2", "@", "2"]
        );
    }

    #[test]
    fn value_data_indexing() {
        let mut a = ValueData::zero_array(&[3, 2]);
        a.set(&[2, 1], 7.0).unwrap();
        assert_eq!(a.get(&[2, 1]), Some(7.0));
        assert_eq!(a.get(&[0, 0]), Some(0.0));
        assert_eq!(a.get(&[3, 0]), None);
        assert_eq!(a.as_slice().unwrap()[5], 7.0);
        let mut c = ValueData::zero_array(&[2, 2, 2]);
        c.set(&[1, 1, 1], 9.0).unwrap();
        assert_eq!(c.as_slice().unwrap()[7], 9.0);
        assert!(ValueData::Scalar(f64::NAN).is_nan_marker());
        assert!(!ValueData::zero_array(&[2]).is_nan_marker());
    }

    #[test]
    fn scan_assignments_basics() {
        let r = scan_assignments(" Scalar1 = 1.5");
        assert_eq!(r, vec![("Scalar1".to_string(), "1.5".to_string())]);
        let r = scan_assignments(" Curve1 = [1 2 3]");
        assert_eq!(r, vec![("Curve1".to_string(), "[1 2 3]".to_string())]);
        let r = scan_assignments(" A = 1 B=2 C = [3 4]");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "A");
        assert_eq!(r[1].0, "C");
    }

    #[test]
    fn par_line_and_param_parsing() {
        let (name, param, value) = parse_par_line("Scalar1 [UINT(8)] 2 ; 2.00").unwrap();
        assert_eq!(name, "Scalar1");
        assert_eq!(param, "UINT(8)");
        assert_eq!(value, "2 ; 2.00");
        let (name, param, _) = parse_par_line("Map1 [FLOAT(32),(2,3)] ").unwrap();
        assert_eq!(name, "Map1");
        let (pdt, bits, dims) = parse_par_param(&param).unwrap();
        assert_eq!(pdt, ParDataType::Float);
        assert_eq!(bits, Some(32));
        assert_eq!(dims, vec![2, 3]);
        let (pdt, _, dims) = parse_par_param("STRING(5)").unwrap();
        assert_eq!(pdt, ParDataType::String);
        assert_eq!(dims, vec![1]);
        let (name, _, _) = parse_par_line("arr[0] [INT(8),(1,2)] 5").unwrap();
        assert_eq!(name, "arr[0]");
        assert!(parse_par_line("; comment").is_none());
        assert!(parse_par_line("nobracket").is_none());
    }

    // ---------- ModuleRefs / ConservationValue ----------

    #[test]
    fn module_refs_lookup() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        assert!(refs.char_target("Scalar1").is_some());
        assert!(matches!(
            refs.char_target("AxisPts1"),
            Some(CharTarget::AxisPts(_))
        ));
        assert!(refs.char_target("Nope").is_none());
        assert_eq!(refs.epk(), Some("EPK-123"));
        let f = refs.def_characteristic_function("Curve1").unwrap();
        assert_eq!(f.named.name, "F1");
        assert_eq!(f.version.as_deref(), Some("1.0"));
        assert!(refs.def_characteristic_function("Block1").is_none());
        assert_eq!(
            DataConservation::epk_description(&refs, "desc"),
            "EPK: EPK-123\r\n\r\ndesc"
        );
        assert_eq!(DataConservation::epk_description(&refs, ""), "EPK: EPK-123");
    }

    #[test]
    fn conservation_value_basics() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let scalar = ConservationValue::new(&refs, "Scalar1").unwrap();
        assert_eq!(scalar.char_type(), CharacteristicType::VALUE);
        assert_eq!(scalar.unit, "Nm");
        assert_eq!(scalar.decimal_count(), 2);
        assert_eq!(scalar.number_of_elements(), Some(1));
        assert!(!scalar.is_string());

        let bool1 = ConservationValue::new(&refs, "Bool1").unwrap();
        assert!(bool1.is_string());

        let blk2 = ConservationValue::new(&refs, "Block2").unwrap();
        assert_eq!(blk2.number_of_elements(), Some(4));
        assert_eq!(blk2.matrix_dim_xy(), (2, 2));

        let blk1 = ConservationValue::new(&refs, "Block1").unwrap();
        assert_eq!(blk1.matrix_dim_xy(), (4, 1));

        let curve = ConservationValue::new(&refs, "Curve1").unwrap();
        assert_eq!(curve.axis_values.len(), 1);
        assert_eq!(curve.axis_values[0].len(), 4);
        assert_eq!(curve.unit, "kPa");
        assert_eq!(curve.decimal_count(), 1);
        assert_eq!(curve.unit_axis[0], "rpm");
        assert_eq!(curve.decimal_count_axis(0), 0);

        let ap = ConservationValue::new(&refs, "AxisPts1").unwrap();
        assert!(ap.is_axis_pts());
        assert_eq!(ap.char_type(), CharacteristicType::VAL_BLK);

        let curve2 = ConservationValue::new(&refs, "Curve2").unwrap();
        assert_eq!(curve2.shared_axis_ref(0), Some("AxisPts1"));
    }

    #[test]
    fn to_single_value_formats() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let mut scalar = ConservationValue::new(&refs, "Scalar1").unwrap();
        scalar.value = ValueData::Scalar(1.5);
        assert_eq!(scalar.to_single_value(true), "1.50");
        assert_eq!(scalar.to_single_value(false), "1.5");
        scalar.value_format = ValueFormat::Raw;
        scalar.value = ValueData::Scalar(250.7);
        assert_eq!(scalar.to_single_value(true), "250");
        let mut bool1 = ConservationValue::new(&refs, "Bool1").unwrap();
        bool1.value = ValueData::Scalar(1.0);
        assert_eq!(bool1.to_single_value(true), "ON");
        bool1.value = ValueData::Scalar(5.0);
        assert_eq!(bool1.to_single_value(true), "?"); // DEFAULT_VALUE
        let mut curve = ConservationValue::new(&refs, "Curve1").unwrap();
        curve.axis_values = vec![vec![100.0, 200.0, 300.0, 400.0]];
        curve.value = ValueData::Array {
            dims: vec![4],
            data: vec![0.0, 1.0, 2.0, 3.0],
        };
        assert_eq!(curve.to_single_value_at(2, -1, 0, true), "300");
        assert_eq!(curve.to_single_value_at(2, 0, 0, true), "2.0");
        assert_eq!(curve.to_single_value_at(9, -1, 0, true), "");
        // to_physical:LINEAR phys = 2*raw+10
        let cm = curve.main_compu();
        assert_eq!(curve.to_physical(cm, 1.0), 12.0);
    }

    // ---------- DCM ----------

    fn sample_values<'m>(refs: &'m ModuleRefs<'m>) -> Vec<ConservationValue<'m>> {
        let mut scalar = ConservationValue::new(refs, "Scalar1").unwrap();
        scalar.value = ValueData::Scalar(1.5);
        let mut text = ConservationValue::new(refs, "Text1").unwrap();
        text.value = ValueData::Text("Hello".to_string());
        let mut bool1 = ConservationValue::new(refs, "Bool1").unwrap();
        bool1.value = ValueData::Scalar(1.0);
        let mut blk = ConservationValue::new(refs, "Block1").unwrap();
        blk.value = ValueData::Array {
            dims: vec![4],
            data: vec![1.0, 2.0, 3.0, 4.0],
        };
        let mut blk2 = ConservationValue::new(refs, "Block2").unwrap();
        blk2.value = ValueData::Array {
            dims: vec![2, 2],
            data: vec![5.0, 6.0, 7.0, 8.0],
        };
        let mut curve = ConservationValue::new(refs, "Curve1").unwrap();
        curve.axis_values = vec![vec![100.0, 200.0, 300.0, 400.0]];
        curve.value = ValueData::Array {
            dims: vec![4],
            data: vec![0.0, 1.0, 2.0, 3.0],
        };
        let mut map = ConservationValue::new(refs, "Map1").unwrap();
        map.axis_values = vec![vec![1.0, 2.0, 3.0], vec![10.0, 20.0]];
        map.value = ValueData::Array {
            dims: vec![3, 2],
            data: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        };
        let mut ap = ConservationValue::new(refs, "AxisPts1").unwrap();
        ap.value = ValueData::Array {
            dims: vec![4],
            data: vec![100.0, 200.0, 300.0, 400.0],
        };
        let mut curve2 = ConservationValue::new(refs, "Curve2").unwrap();
        curve2.axis_values = vec![vec![100.0, 200.0, 300.0, 400.0]];
        curve2.value = ValueData::Array {
            dims: vec![4],
            data: vec![4.0, 3.0, 2.0, 1.0],
        };
        vec![scalar, text, bool1, blk, blk2, curve, map, ap, curve2]
    }

    #[test]
    fn dcm_save_layout() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = sample_values(&refs);
        let fns: Vec<&Function> = refs.functions().to_vec();
        let (text, count) = DcmFile::save_string("desc one", &values, Some(&fns), true).unwrap();
        assert_eq!(count, 9);
        assert!(text.starts_with("* encoding=\"utf-8\"\r\n* DAMOS Format\r\n"));
        assert!(text.contains("* desc one\r\n\r\nKONSERVIERUNG_FORMAT 2.0\r\n\r\n"));
        assert!(text.contains("FUNKTIONEN\r\nFKT F1 \"1.0\" \"func one\"\r\nEND\r\n\r\n"));
        assert!(text.contains(
            "FESTWERT Scalar1\r\nLANGNAME \"a scalar\"\r\nFUNKTION F1\r\nEINHEIT_W \"Nm\"\r\n WERT 1.50\r\nEND\r\n\r\n"
        ));
        // TEXTSTRING
        assert!(text
            .contains("TEXTSTRING Text1\r\nLANGNAME \"a text\"\r\n TEXT \"Hello\"\r\nEND\r\n\r\n"));
        assert!(text.contains("FESTWERT Bool1\r\nLANGNAME \"a bool\"\r\nEINHEIT_W \"-\"\r\n TEXT \"ON\"\r\nEND\r\n\r\n"));
        assert!(text.contains("FESTWERTEBLOCK Block1 4\r\n"));
        assert!(text.contains(" WERT 1.00 2.00 3.00 4.00 \r\nEND\r\n\r\n"));
        assert!(text.contains("FESTWERTEBLOCK Block2 2 @ 2\r\n"));
        assert!(text.contains(" WERT 5.00 6.00 \r\n7.00 8.00 \r\nEND\r\n\r\n"));
        assert!(text.contains("KENNLINIE Curve1 4\r\n"));
        assert!(text.contains("EINHEIT_X \"rpm\"\r\nEINHEIT_W \"kPa\"\r\n"));
        assert!(text.contains("ST/X 100 200 300 400 \r\n"));
        assert!(text.contains(" WERT 0.0 1.0 2.0 3.0 \r\nEND\r\n\r\n"));
        assert!(text.contains("KENNFELD Map1 3 2\r\n"));
        assert!(text.contains("EINHEIT_Y \"rpm\"\r\n"));
        assert!(text.contains(
            "ST/Y 10 \r\n WERT 1.00 2.00 3.00 \r\nST/Y 20 \r\n WERT 4.00 5.00 6.00 \r\nEND\r\n\r\n"
        ));
        assert!(text.contains("STUETZSTELLENVERTEILUNG AxisPts1 4\r\n"));
        assert!(text.contains("EINHEIT_X \"rpm\"\r\nST/X 100 200 300 400 \r\nEND\r\n\r\n"));
        // GRUPPENKENNLINIE + *SSTX
        assert!(text.contains("GRUPPENKENNLINIE Curve2 4\r\n"));
        assert!(text.contains("*SSTX AxisPts1\r\n"));
    }

    #[test]
    fn dcm_roundtrip() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = sample_values(&refs);
        let (text, _) = DcmFile::save_string("", &values, None, true).unwrap();
        let parsed = DcmFile::open_str(&text, &refs).unwrap();
        assert_eq!(parsed.values.len(), 9);
        assert!(parsed.skipped_values.is_empty());
        let scalar = &parsed.values[0];
        assert_eq!(scalar.name(), "Scalar1");
        assert_eq!(scalar.value, ValueData::Scalar(1.5));
        assert_eq!(scalar.unit, "Nm");
        let text1 = &parsed.values[1];
        assert_eq!(text1.value, ValueData::Text("Hello".to_string()));
        let bool1 = &parsed.values[2];
        assert_eq!(bool1.value, ValueData::Scalar(1.0)); // TEXT "ON" → VTAB toRaw → 1
        assert_eq!(bool1.unit, "-");
        let blk2 = &parsed.values[4];
        assert_eq!(blk2.value.len(), 4);
        let curve = &parsed.values[5];
        assert_eq!(curve.axis_values[0], vec![100.0, 200.0, 300.0, 400.0]);
        assert_eq!(curve.unit_axis[0], "rpm");
        let map = &parsed.values[6];
        assert_eq!(map.axis_values[1], vec![10.0, 20.0]);
        assert_eq!(map.value.get(&[2, 1]), Some(6.0));
        let ap = &parsed.values[7];
        assert!(ap.is_axis_pts());
        assert_eq!(ap.value.len(), 4);
        let curve2 = &parsed.values[8];
        assert_eq!(curve2.name(), "Curve2");
        let (text2, _) = DcmFile::save_string("", &parsed.values, None, true).unwrap();
        let parsed2 = DcmFile::open_str(&text2, &refs).unwrap();
        let (text3, _) = DcmFile::save_string("", &parsed2.values, None, true).unwrap();
        assert_eq!(strip_date_line(&text2), strip_date_line(&text3));
    }

    #[test]
    fn dcm_parse_skips() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let f = DcmFile::open_str("FESTWERT Scalar1\n WERT 1\nEND\n", &refs).unwrap();
        assert_eq!(f.values.len(), 0);
        assert_eq!(
            f.skipped_values,
            vec![("Scalar1".to_string(), ErrorType::Format)]
        );
        let f = DcmFile::open_str(
            "KONSERVIERUNG_FORMAT 2.0\nFESTWERT Unknown1\nEND\nFESTWERT Curve1\nEND\n",
            &refs,
        )
        .unwrap();
        assert_eq!(f.skipped_values.len(), 2);
        assert_eq!(
            f.skipped_values[0],
            ("Unknown1".to_string(), ErrorType::NotFound)
        );
        assert_eq!(
            f.skipped_values[1],
            ("Curve1".to_string(), ErrorType::TypeDiffers)
        );
        let f = DcmFile::open_str("KONSERVIERUNG_FORMAT 2.0\nKENNLINIE Curve1 5\nEND\n", &refs)
            .unwrap();
        assert_eq!(
            f.skipped_values,
            vec![("Curve1".to_string(), ErrorType::XAxisInvalid)]
        );
        let f = DcmFile::open_str(
            "* comment\n! comment\n. comment\nKONSERVIERUNG_FORMAT 1.0\nFESTWERT Scalar1\nEND\n",
            &refs,
        )
        .unwrap();
        assert_eq!(
            f.skipped_values,
            vec![("Scalar1".to_string(), ErrorType::Format)]
        );
    }

    #[test]
    fn dcm_parse_multiline_values_and_strings() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let text = "KONSERVIERUNG_FORMAT 2.0\n\
KENNFELD Map1 3 2\nEINHEIT_X \"rpm\"\nEINHEIT_Y \"g\"\nEINHEIT_W \"Nm\"\n\
ST/X 1\nST/X 2 3\nST/Y 10\nWERT 1 2\nWERT 3\nST/Y 20\nWERT 4 5 6\nEND\n";
        let f = DcmFile::open_str(text, &refs).unwrap();
        assert_eq!(f.values.len(), 1);
        let map = &f.values[0];
        assert_eq!(map.axis_values[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(map.axis_values[1], vec![10.0, 20.0]);
        assert_eq!(
            map.value.as_slice().unwrap(),
            &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(map.unit_axis, vec!["rpm".to_string(), "g".to_string()]);
    }

    // ---------- Matlab ----------

    #[test]
    fn matlab_save_layout() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = sample_values(&refs);
        let (text, count) = MatlabFile::save_string("mdesc", &values, true).unwrap();
        assert_eq!(count, 9);
        assert!(text.starts_with("% Created by  ()\r\n"));
        assert!(text.contains("% mdesc\r\n\r\n"));
        assert!(text.contains("Scalar1 = 1.50;\r\n"));
        assert!(text.contains("Text1 = [72 101 108 108 111];\r\n"));
        assert!(text.contains("Bool1 = 1;\r\n"));
        assert!(text.contains("Block1 = [1 2 3 4];\r\n"));
        assert!(text.contains("Map1 = [1 2 3\r\n4 5 6];\r\n"));
        assert!(text.contains("Curve1_X_AXIS = [100];\r\n"));
        assert!(text.contains("Map1_X_AXIS = [1 2];\r\n"));
        assert!(text.contains("Map1_Y_AXIS = [10 20];\r\n"));
        assert!(text.contains("% Unit: rpm\r\nCurve1_X_AXIS"));
        // AXIS_PTS
        assert!(text.contains("AxisPts1 = [100 200 300 400];\r\n"));
        assert!(text.contains("Curve1 = [0 1 2 3];\r\n"));
    }

    #[test]
    fn matlab_roundtrip() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = sample_values(&refs);
        let (text, _) = MatlabFile::save_string("", &values, false).unwrap();
        let parsed = MatlabFile::open_str(&text, &refs).unwrap();
        assert!(parsed.skipped_values.is_empty());
        assert_eq!(parsed.values.len(), 9);
        let scalar = &parsed.values[0];
        assert_eq!(scalar.value, ValueData::Scalar(1.5));
        let text1 = &parsed.values[1];
        assert_eq!(text1.value, ValueData::Text("Hello".to_string()));
        let bool1 = &parsed.values[2];
        assert_eq!(bool1.value, ValueData::Scalar(1.0));
        let curve = &parsed.values[5];
        assert_eq!(curve.axis_values[0], vec![100.0]);
        let map = &parsed.values[6];
        assert_eq!(map.axis_values[0], vec![1.0, 2.0]);
        assert_eq!(map.axis_values[1], vec![10.0, 20.0]);
        assert_eq!(map.value.get(&[2, 1]), Some(6.0));
        let ap = &parsed.values[7];
        assert_eq!(ap.value.len(), 4);
    }

    #[test]
    fn matlab_parse_comment_and_axis_rules() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let src = "% comment\nScalar1 = 1.5;\nMap1 = [1 2 3];\n";
        assert!(MatlabFile::open_str(src, &refs).is_err());
        let src = "Curve1_X_AXIS = [9 8 7 6];\nCurve1 = [1 2 3 4];\n";
        let f = MatlabFile::open_str(src, &refs).unwrap();
        assert_eq!(f.values.len(), 1);
        assert_eq!(f.values[0].axis_values[0], vec![9.0, 8.0, 7.0, 6.0]);
        let src = "Unknown1 = 1;\nScalar1 = 2;\n";
        let f = MatlabFile::open_str(src, &refs).unwrap();
        assert_eq!(f.values.len(), 1);
        assert_eq!(f.values[0].value, ValueData::Scalar(2.0));
        let src = "Curve1 = [1 2 3];\n";
        let f = MatlabFile::open_str(src, &refs).unwrap();
        assert_eq!(f.values.len(), 0);
        assert_eq!(
            f.skipped_values,
            vec![("Curve1".to_string(), ErrorType::XAxisInvalid)]
        );
    }

    // ---------- PAR ----------

    fn par_values<'m>(refs: &'m ModuleRefs<'m>) -> Vec<ConservationValue<'m>> {
        let mut values = sample_values(refs);
        for v in values.iter_mut() {
            v.value_format = ValueFormat::Raw;
        }
        values
    }

    #[test]
    fn par_save_layout_v31_v32() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = par_values(&refs);
        let (text, count) = ParFile::save_string(
            Some("pdesc"),
            Some("C:\\work\\my proj.a2l"),
            Some("C:\\work\\data.hex"),
            &values,
            ParType::CANapeV3_1,
            true,
            false,
            "u",
            "d",
            "pr",
            "s",
        )
        .unwrap();
        assert_eq!(count, 9);
        assert!(text.starts_with("CANape PAR V3.1: \"my proj.a2l\" 1 0 data.hex\r\n"));
        assert!(text.contains("; u\r\n; d\r\n; pr\r\n; s\r\n; pdesc\r\n\r\n"));
        assert!(text.contains("Scalar1 [UINT(8)] 1.5 ; 1\r\n"));
        assert!(text.contains("Bool1 [UINT(8)] 1 ; 1\r\n"));
        // ASCII
        assert!(text.contains("Text1 [STRING(5)] 72 101 108 108 111 ; Hello\r\n"));
        assert!(text.contains("Block1 [INT(16),(1,4)] 1 ; 1\r\n: 2 ; 2\r\n: 3 ; 3\r\n: 4 ; 4\r\n"));
        // CURVE(SWORD → INT(16));CM_LIN phys = 2*raw+10
        assert!(
            text.contains("Curve1 [INT(16),(1,4)] 0 ; 10\r\n: 1 ; 12\r\n: 2 ; 14\r\n: 3 ; 16\r\n")
        );
        assert!(text.contains("X : 100 ; 100\r\nX : 200 ; 200\r\n"));
        // MAP:(ny,nx)
        assert!(text.contains("Map1 [FLOAT(32),(2,3)] 1 ; 1\r\n"));
        assert!(text.contains("AxisPts1 [UINT(16),(1,4)] 100 ; 100\r\n"));
        let (text32, _) = ParFile::save_string(
            None,
            None,
            None,
            &values,
            ParType::CANapeV3_2,
            false,
            true,
            "",
            "",
            "",
            "",
        )
        .unwrap();
        assert!(text32.starts_with("CANape PAR V3.2: ~ 0 1 \r\n"));
        assert!(text32.contains("Block1 [INT(16),(1,4)] \r\n: 1 ; 1\r\n"));
    }

    #[test]
    fn par_roundtrip_v31_v32() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = par_values(&refs);
        assert!(ParFile::open_str(
            &ParFile::save_string(
                None,
                None,
                None,
                &values,
                ParType::CANapeV3_1,
                true,
                false,
                "",
                "",
                "",
                ""
            )
            .unwrap()
            .0,
            &refs
        )
        .is_err());
        let values: Vec<_> = values
            .into_iter()
            .filter(|v| v.name() != "Curve2")
            .collect();
        for ty in [ParType::CANapeV3_1, ParType::CANapeV3_2] {
            let (text, _) =
                ParFile::save_string(None, None, None, &values, ty, true, false, "", "", "", "")
                    .unwrap();
            let parsed = ParFile::open_str(&text, &refs).unwrap();
            assert!(parsed.skipped_values.is_empty(), "{text}");
            assert_eq!(parsed.values.len(), 8, "{text}");
            let scalar = &parsed.values[0];
            assert_eq!(scalar.value, ValueData::Scalar(1.5));
            assert_eq!(scalar.value_format, ValueFormat::Raw);
            let text1 = &parsed.values[1];
            assert_eq!(text1.value, ValueData::Text("Hello".to_string()));
            let blk = &parsed.values[3];
            assert_eq!(blk.value.as_slice().unwrap(), &[1.0, 2.0, 3.0, 4.0]);
            let curve = &parsed.values[5];
            assert_eq!(curve.value.as_slice().unwrap(), &[0.0, 1.0, 2.0, 3.0]);
            assert_eq!(curve.axis_values[0], vec![100.0, 200.0, 300.0, 400.0]);
            let map = &parsed.values[6];
            assert_eq!(map.value.get(&[2, 1]), Some(6.0));
            assert_eq!(map.axis_values[0], vec![1.0, 2.0, 3.0]);
            assert_eq!(map.axis_values[1], vec![10.0, 20.0]);
            let ap = &parsed.values[7];
            assert_eq!(ap.value.as_slice().unwrap(), &[100.0, 200.0, 300.0, 400.0]);
        }
    }

    #[test]
    fn par_parse_skips() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let f = ParFile::open_str("Scalar1 [UINT(8)] 2 ; 2\r\n", &refs).unwrap();
        assert_eq!(f.values.len(), 0);
        let src = "CANape PAR V3.1: ~ 1 0 \r\n\
Unknown1 [UINT(8)] 2 ; 2\r\n\
Curve1 [INT(16),(2,4)] 1 ; 1\r\n";
        let f = ParFile::open_str(src, &refs).unwrap();
        assert_eq!(f.values.len(), 0);
        assert_eq!(
            f.skipped_values,
            vec![("Curve1".to_string(), ErrorType::NotFound)]
        );
    }

    #[test]
    fn par_ascii_multiline_and_comments() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let src = "CANape PAR V3.1: ~ 1 0 \r\n\
; comment\r\n\
Block1 [INT(16),(1,4)] 1 ; 1\r\n\
; skip me\r\n\
: 2 ; 2\r\n\
: 3 ; 3\r\n\
: 4 ; 4\r\n";
        let f = ParFile::open_str(src, &refs).unwrap();
        assert_eq!(f.values.len(), 1);
        assert_eq!(f.values[0].value.as_slice().unwrap(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn nan_values_are_skipped() {
        let p = parse_project();
        let refs = ModuleRefs::new(module(&p));
        let values = vec![
            ConservationValue::new(&refs, "Scalar1").unwrap(),
            ConservationValue::new(&refs, "Curve1").unwrap(),
        ];
        let (dcm, n) = DcmFile::save_string("", &values, None, true).unwrap();
        assert_eq!(n, 0);
        assert!(!dcm.contains("FESTWERT"));
        let (_, n) = MatlabFile::save_string("", &values, false).unwrap();
        assert_eq!(n, 0);
        let (_, n) = ParFile::save_string(
            None,
            None,
            None,
            &values,
            ParType::CANapeV3_1,
            true,
            false,
            "",
            "",
            "",
            "",
        )
        .unwrap();
        assert_eq!(n, 0);
    }
}
