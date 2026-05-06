//! Reading and writing of calibration values in ECU data file images.
//! Given an A2L CHARACTERISTIC / AXIS_PTS (including its RECORD_LAYOUT and
//! resolved project-level references) and a data file image
//! (`MemorySegmentList`), the RECORD_LAYOUT entries are walked to read out a
//! value object ([`CharValue`]) or to write a value object back into the
//! image, including read-out and write-back of COM_AXIS / RES_AXIS /
//! CURVE_AXIS referenced axes and the Conversion scaling chain.
//! Design notes and quirks:
//! - Project-level reference resolution (`RefRecordLayout` / `RefCompuMethod`
//!   / `RefAxisPtsRef` / `RefCurveAxisRef` / MOD_COMMON fallback) follows the
//!   existing conventions of [`crate::value`]: the caller resolves the
//!   references and injects them via [`CharacteristicIo`] / [`AxisPtsIo`] /
//!   [`AxisIo`]; the identity conversion (a default placeholder COMPU_METHOD)
//!   is provided internally by this module.
//! - Variant-coding address resolution (`getAddress(criterionValues)`) is not
//!   implemented: addresses (including referenced-axis addresses) are supplied
//!   by the caller as `u64` parameters/fields (the base address when there is
//!   no criterion).
//! - Segment lookup by address uses `find_mem_seg(address, 1)`.
//! - Buffer-overrun and similar errors on the read path are all surfaced as
//!   [`Error`] rather than silently swallowed; an AXIS_PTS whose layout has no
//!   AXIS_PTS value entry still reads out as a `None` value.
//! - On write-back, skipped entries **reset** the cursor to the
//!   layout-relative offset via [`record_layout_offset`], discarding the
//!   in-segment base address (a deliberate compatibility quirk, reproduced
//!   exactly; the FIX_AXIS OFFSET/DIST_OP/SHIFT_OP reads likewise read
//!   segment data at layout-relative offsets). This is only observable for
//!   static layouts containing pointer-type or skipped entries.
//! - When writing back referenced axes (COM_AXIS/RES_AXIS), an axis whose
//!   value array is an empty `Vec` is treated as unset and skipped.
//! - The `FIX_AXIS_PAR_LIST` sub-block of `AXIS_DESCR` is not typed in the
//!   autors-a2l model (it is passed through as UnsupportedNode), so that
//!   FIX_AXIS value source cannot be implemented; the axis values are left
//!   unchanged in that case (all other FIX_AXIS sources are implemented).

use std::sync::OnceLock;

use autors_a2l::model::base::{ByteOrder, RecordLayoutRefFields};
use autors_a2l::model::characteristic::{AxisPts, Characteristic};
use autors_a2l::model::compu::CompuMethod;
use autors_a2l::model::enums::{
    AddrType, AxisType, CharacteristicType, DataType, DepositType, EncodingType, IndexMode,
    IndexOrder,
};
use autors_a2l::model::measurement::AxisDescr;
use autors_a2l::model::record_layout::{
    AxisPtsLayoutDesc, AxisRescaleLayoutDesc, FncValuesLayoutDesc, NoAxisPtsLayoutDesc,
    RecordLayout,
};
use autors_datafile::datafile::{DataFileBase, MemorySegment, MemorySegmentList};

use crate::error::{Error, Result};
use crate::value::{
    get_decimal_count, get_single_raw_value, get_single_raw_value_buffer, size_in_byte,
    BitOperation, CharValue, CharacteristicRef, Conversion, ValueData, ValueObjectFormat,
};

// ===========================================================================
// Injected context (resolved project-level references)
// ===========================================================================

/// Resolved references of an AXIS_PTS (record layout and conversion).
#[derive(Debug, Clone, Copy)]
pub struct AxisPtsIo<'a> {
    /// The AXIS_PTS node.
    pub axis_pts: &'a AxisPts,
    /// `RECORD_LAYOUT` reference (resolved by name).
    pub record_layout: &'a RecordLayout,
    /// Conversion (COMPU_METHOD plus resolved conversion tables).
    pub conversion: Conversion<'a>,
    /// Fallback when the layout does not carry its own alignment table (the
    /// MOD_COMMON alignments; pass [`DEFAULT_ALIGNMENTS`] when unknown).
    pub default_alignments: [i32; 7],
    /// Fallback when the node does not set a byte order (the MOD_COMMON
    /// BYTE_ORDER; `NotSet` is treated as little-endian, matching the
    /// existing convention of [`crate::value::get_single_raw_value`]).
    pub default_byte_order: ByteOrder,
}

/// Resolved target of a referenced axis (axis-points reference or curve-axis
/// reference).
#[derive(Debug, Clone)]
pub enum AxisRefIo<'a> {
    /// The AXIS_PTS referenced by a COM_AXIS / RES_AXIS, together with its
    /// (caller-resolved) address.
    AxisPts {
        /// The referenced target.
        io: AxisPtsIo<'a>,
        /// Address of the referenced target.
        address: u64,
    },
    /// The CURVE-type CHARACTERISTIC referenced by a CURVE_AXIS, together
    /// with its address.
    CurveAxis {
        /// The referenced target (`char_type` should be CURVE).
        io: Box<CharacteristicIo<'a>>,
        /// Address of the referenced target.
        address: u64,
    },
}

/// Resolved context of a single AXIS_DESCR.
#[derive(Debug, Clone)]
pub struct AxisIo<'a> {
    /// The AXIS_DESCR node.
    pub descr: &'a AxisDescr,
    /// Axis conversion (resolved per `descr.conversion`).
    pub conversion: Conversion<'a>,
    /// Referenced-axis target (resolved by the caller for COM_AXIS /
    /// RES_AXIS / CURVE_AXIS; `None` for STD_AXIS / FIX_AXIS).
    pub reference: Option<AxisRefIo<'a>>,
}

#[derive(Debug, Clone)]
pub struct CharacteristicIo<'a> {
    pub characteristic: &'a Characteristic,
    pub record_layout: &'a RecordLayout,
    pub conversion: Conversion<'a>,
    pub axes: Vec<AxisIo<'a>>,
    pub default_alignments: [i32; 7],
    pub default_byte_order: ByteOrder,
}

#[derive(Debug, Clone)]
pub enum RecordLayoutRefIo<'a> {
    /// CHARACTERISTIC.
    Characteristic(CharacteristicIo<'a>),
    AxisPts(AxisPtsIo<'a>),
}

pub const DEFAULT_ALIGNMENTS: [i32; 7] = [1, 2, 4, 4, 4, 8, 2];

fn identity_conversion() -> Conversion<'static> {
    static IDENTITY: OnceLock<CompuMethod> = OnceLock::new();
    Conversion::new(IDENTITY.get_or_init(CompuMethod::default))
}

fn alignments_of(layout: &RecordLayout, fallback: [i32; 7]) -> [i32; 7] {
    layout.alignments.unwrap_or(fallback)
}

fn alignment_of(alignments: [i32; 7], dt: DataType) -> Result<i32> {
    let idx = match dt {
        DataType::UByte | DataType::SByte => 0,
        DataType::UWord | DataType::SWord => 1,
        DataType::ULong | DataType::SLong => 2,
        DataType::Float32Ieee => 3,
        DataType::Float64Ieee => 4,
        DataType::AUInt64 | DataType::AInt64 => 5,
        DataType::Float16Ieee => 6,
        DataType::Unsupported => {
            return Err(Error::Value(format!(
                "getAlignment: unsupported data type {dt:?}"
            )))
        }
    };
    Ok(alignments[idx])
}

fn align_up(offset: usize, alignment: i32) -> usize {
    if alignment <= 1 {
        return offset;
    }
    let a = alignment as usize;
    (offset + (a - 1)) & !(a - 1)
}

fn resolve_byte_order(bo: ByteOrder, default: ByteOrder) -> ByteOrder {
    if bo == ByteOrder::NotSet {
        default
    } else {
        bo
    }
}

fn deposit_of(d: DepositType) -> DepositType {
    if d == DepositType::NotSet {
        DepositType::ABSOLUTE
    } else {
        d
    }
}

fn is_alternate(mode: IndexMode) -> bool {
    matches!(
        mode,
        IndexMode::ALTERNATE_WITH_X | IndexMode::ALTERNATE_WITH_Y | IndexMode::ALTERNATE_CURVES
    )
}

fn is_alternate_fnc(layout: &RecordLayout) -> bool {
    layout
        .fnc_values
        .as_ref()
        .is_some_and(|f| is_alternate(f.index_mode))
}

pub(crate) fn number_of_elements(ch: &Characteristic) -> Result<usize> {
    match ch.char_type {
        CharacteristicType::VALUE => Ok(1),
        CharacteristicType::ASCII | CharacteristicType::VAL_BLK => {
            if let Some(dims) = &ch.matrix_dim {
                Ok(dims.iter().map(|&d| d.max(0) as usize).product())
            } else {
                Ok(ch.number.max(0) as usize)
            }
        }
        other => Err(Error::Value(format!(
            "getNumberOfElements: not supported for {other:?}"
        ))),
    }
}

fn decimal_count_of(format: Option<&str>) -> Result<i32> {
    match format {
        Some(f) => get_decimal_count(f),
        None => Ok(0),
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Clone, Copy)]
enum LayoutEntry<'a> {
    /// SHIFT_OP/NO_RESCALE_X/RESERVED/IDENTIFICATION/RIP_ADDR_W).
    NoAxisPts(&'a NoAxisPtsLayoutDesc),
    /// `AXIS_PTS_X..5`.
    AxisPts(&'a AxisPtsLayoutDesc),
    /// `AXIS_RESCALE_X`.
    AxisRescale(&'a AxisRescaleLayoutDesc),
    /// `FNC_VALUES`.
    FncValues(&'a FncValuesLayoutDesc),
}

impl LayoutEntry<'_> {
    fn position(&self) -> i32 {
        match self {
            LayoutEntry::NoAxisPts(e) => e.position,
            LayoutEntry::AxisPts(e) => e.position,
            LayoutEntry::AxisRescale(e) => e.position,
            LayoutEntry::FncValues(e) => e.position,
        }
    }

    fn data_type(&self) -> DataType {
        match self {
            LayoutEntry::NoAxisPts(e) => e.data_type,
            LayoutEntry::AxisPts(e) => e.data_type,
            LayoutEntry::AxisRescale(e) => e.data_type,
            LayoutEntry::FncValues(e) => e.data_type,
        }
    }

    fn axis_idx(&self) -> i32 {
        match self {
            LayoutEntry::NoAxisPts(e) => e.axis_idx,
            LayoutEntry::AxisPts(e) => e.axis_idx,
            LayoutEntry::AxisRescale(e) => e.axis_idx,
            LayoutEntry::FncValues(_) => -1,
        }
    }
}

fn layout_entries(layout: &RecordLayout) -> Vec<LayoutEntry<'_>> {
    let mut entries = Vec::new();
    for e in layout.no_axis_pts.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in layout.axis_pts.iter().flatten() {
        entries.push(LayoutEntry::AxisPts(e));
    }
    for e in layout.src_address.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in layout.offset.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in layout.dist_op.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in layout.rip_addr.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in layout.shift_op.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in &layout.reserved {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &layout.axis_rescale_x {
        entries.push(LayoutEntry::AxisRescale(e));
    }
    if let Some(e) = &layout.no_rescale_x {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &layout.identification {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &layout.rip_addr_w {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &layout.fnc_values {
        entries.push(LayoutEntry::FncValues(e));
    }
    entries.sort_by_key(|e| e.position());
    entries
}

pub fn record_layout_offset(
    layout: &RecordLayout,
    default_alignments: [i32; 7],
    position: i32,
    axis_max_points: Option<&[i32]>,
    fallback_max_points: i32,
) -> Result<usize> {
    let alignments = alignments_of(layout, default_alignments);
    let ptr_size =
        |alignments: [i32; 7], offset: usize, dt: DataType, size: usize| -> Result<usize> {
            Ok(align_up(offset, alignment_of(alignments, dt)?) + size)
        };
    let mut offset = 0usize;
    for entry in layout_entries(layout) {
        if entry.position() >= position {
            continue;
        }
        let size = size_in_byte(entry.data_type())?;
        let align = alignment_of(alignments, entry.data_type())?;
        match entry {
            LayoutEntry::NoAxisPts(_) => {
                offset = align_up(offset, align);
                offset += size;
            }
            LayoutEntry::AxisPts(e) => match e.address_type {
                AddrType::DIRECT => {
                    offset = align_up(offset, align);
                    let count = axis_max_points
                        .and_then(|m| m.get(e.axis_idx.max(0) as usize).copied())
                        .unwrap_or(fallback_max_points);
                    offset += size * count.max(0) as usize;
                }
                AddrType::PBYTE => offset += 1,
                AddrType::PWORD => offset = ptr_size(alignments, offset, DataType::UWord, 2)?,
                AddrType::PLONG => offset = ptr_size(alignments, offset, DataType::ULong, 4)?,
                AddrType::PLONGLONG => offset = ptr_size(alignments, offset, DataType::AUInt64, 8)?,
            },
            LayoutEntry::AxisRescale(e) => {
                if e.address_type == AddrType::DIRECT {
                    offset = align_up(offset, align);
                    offset += size * 2 * e.max_no_rescale_pairs.max(0) as usize;
                }
            }
            LayoutEntry::FncValues(e) => match e.address_type {
                AddrType::DIRECT => {
                    offset = align_up(offset, align);
                    let mut total = size;
                    if let Some(max_pts) = axis_max_points {
                        for i in (0..max_pts.len()).rev() {
                            let mut count = max_pts[i].max(0) as usize;
                            if (e.index_mode == IndexMode::ALTERNATE_WITH_X && i == 0)
                                || (e.index_mode == IndexMode::ALTERNATE_WITH_Y && i == 1)
                            {
                                count *= 2;
                            }
                            total *= count;
                        }
                    }
                    offset += total;
                }
                AddrType::PBYTE => offset += 1,
                AddrType::PWORD => offset = ptr_size(alignments, offset, DataType::UWord, 2)?,
                AddrType::PLONG => offset = ptr_size(alignments, offset, DataType::ULong, 4)?,
                AddrType::PLONGLONG => offset = ptr_size(alignments, offset, DataType::AUInt64, 8)?,
            },
        }
    }
    Ok(offset)
}

// ===========================================================================
// ===========================================================================

struct Cursor<'a> {
    layout: &'a RecordLayout,
    alignments: [i32; 7],
    offset: usize,
    conversion: Conversion<'a>,
    data_type: DataType,
    byte_order: ByteOrder,
    bit_op: Option<BitOperation>,
}

impl<'a> Cursor<'a> {
    fn new(layout: &'a RecordLayout, alignments: [i32; 7], offset: usize) -> Self {
        Cursor {
            layout,
            alignments,
            offset,
            conversion: identity_conversion(),
            data_type: DataType::Unsupported,
            byte_order: ByteOrder::NotSet,
            bit_op: None,
        }
    }

    fn align(&mut self) -> Result<()> {
        self.offset = align_up(self.offset, alignment_of(self.alignments, self.data_type)?);
        Ok(())
    }

    fn read_raw(&self, data: &[u8], skip_bit_op: bool) -> Result<f64> {
        get_single_raw_value(
            data,
            self.offset,
            self.data_type,
            self.byte_order,
            if skip_bit_op {
                None
            } else {
                self.bit_op.as_ref()
            },
        )
    }

    fn write_raw(&self, seg: &mut MemorySegment, value: f64) -> Result<()> {
        let mut value = value;
        if matches!(
            self.data_type,
            DataType::UByte | DataType::UWord | DataType::ULong | DataType::AUInt64
        ) {
            if let Some(bits) = &self.bit_op {
                if bits.bit_mask != u64::MAX {
                    let old = self.read_raw(seg.data(), true)?;
                    value = ((old as u64 & !bits.bit_mask) | ((value as u64) << bits.shift_count))
                        as f64;
                }
            }
        }
        let buf = get_single_raw_value_buffer(value, self.data_type, self.byte_order)?;
        seg.set_data_bytes(self.offset, &buf);
        Ok(())
    }

    fn static_size_adjust(&mut self, actual_count: usize, max_points: &[i32]) -> Result<()> {
        if !self.layout.static_record_layout {
            return Ok(());
        }
        let expected: usize = max_points.iter().map(|&m| m.max(0) as usize).product();
        let size = size_in_byte(self.data_type)?;
        self.offset += expected.saturating_sub(actual_count) * size;
        Ok(())
    }
}

fn read_seq(
    cur: &mut Cursor,
    data: &[u8],
    count: usize,
    order: IndexOrder,
    deposit: DepositType,
) -> Result<Vec<f64>> {
    cur.align()?;
    let size = size_in_byte(cur.data_type)?;
    let mut out = vec![0.0; count];
    for i in 0..count {
        let mut raw = cur.read_raw(data, false)?;
        if deposit == DepositType::DIFFERENCE && i > 0 {
            let prev = if order != IndexOrder::INDEX_DECR {
                i - 1
            } else {
                count - i
            };
            raw += out[prev];
        }
        let idx = if order != IndexOrder::INDEX_DECR {
            i
        } else {
            count - 1 - i
        };
        out[idx] = cur.conversion.to_physical(raw)?;
        cur.offset += size;
    }
    Ok(out)
}

fn write_seq(
    cur: &mut Cursor,
    seg: &mut MemorySegment,
    values: &[f64],
    order: IndexOrder,
    deposit: DepositType,
) -> Result<()> {
    cur.align()?;
    let size = size_in_byte(cur.data_type)?;
    let mut prev = f64::NAN;
    for i in 0..values.len() {
        let idx = if order != IndexOrder::INDEX_DECR {
            i
        } else {
            values.len() - 1 - i
        };
        let mut raw = cur.conversion.to_raw(cur.data_type, values[idx])?;
        if deposit == DepositType::DIFFERENCE && i > 0 {
            raw -= prev;
        }
        cur.write_raw(seg, raw)?;
        prev = raw;
        cur.offset += size;
    }
    Ok(())
}

// ===========================================================================
// ===========================================================================

fn nest_order(mode: IndexMode, ndim: usize) -> Vec<usize> {
    match mode {
        IndexMode::COLUMN_DIR if ndim > 2 => {
            let mut v: Vec<usize> = (2..ndim).rev().collect();
            v.push(0);
            v.push(1);
            v
        }
        _ => (0..ndim).collect(),
    }
}

fn flat_order(dims: &[usize], nest: &[usize]) -> Vec<usize> {
    let mut order = Vec::new();
    if dims.contains(&0) {
        return order;
    }
    let mut idx = vec![0usize; dims.len()];
    loop {
        let mut flat = 0usize;
        for (d, &ix) in idx.iter().enumerate() {
            flat = flat * dims[d] + ix;
        }
        order.push(flat);
        let mut k = nest.len();
        loop {
            if k == 0 {
                return order;
            }
            k -= 1;
            let d = nest[k];
            idx[d] += 1;
            if idx[d] < dims[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn fnc_values(layout: &RecordLayout) -> Result<&FncValuesLayoutDesc> {
    layout
        .fnc_values
        .as_ref()
        .ok_or_else(|| Error::Value("record layout has no FNC_VALUES".into()))
}

fn fnc_values_read(
    cur: &mut Cursor,
    data: &[u8],
    axis_convs: &[Conversion],
    axis_values: &mut [Vec<f64>],
    dims: &[usize],
    out: &mut [f64],
) -> Result<()> {
    let fnc = fnc_values(cur.layout)?;
    let size = size_in_byte(cur.data_type)?;
    cur.align()?;
    match fnc.index_mode {
        IndexMode::ROW_DIR | IndexMode::COLUMN_DIR | IndexMode::NotSet => {
            for flat in flat_order(dims, &nest_order(fnc.index_mode, dims.len())) {
                out[flat] = cur.conversion.to_physical(cur.read_raw(data, false)?)?;
                cur.offset += size;
            }
        }
        IndexMode::ALTERNATE_WITH_X => match dims.len() {
            1 => {
                for i in 0..dims[0] {
                    axis_values[0][i] = axis_convs[0].to_physical(cur.read_raw(data, true)?)?;
                    cur.offset += size;
                    out[i] = cur.conversion.to_physical(cur.read_raw(data, false)?)?;
                    cur.offset += size;
                }
            }
            2 => {
                for x in 0..dims[0] {
                    axis_values[0][x] = axis_convs[0].to_physical(cur.read_raw(data, true)?)?;
                    cur.offset += size;
                    for y in 0..dims[1] {
                        out[x + y * dims[0]] =
                            cur.conversion.to_physical(cur.read_raw(data, false)?)?;
                        cur.offset += size;
                    }
                }
            }
            _ => {
                return Err(Error::Value(format!(
                    "getValue: {:?} not supported for {} dims",
                    fnc.index_mode,
                    dims.len()
                )))
            }
        },
        IndexMode::ALTERNATE_WITH_Y => {
            if dims.len() != 2 {
                return Err(Error::Value(format!(
                    "getValue: {:?} not supported for {} dims",
                    fnc.index_mode,
                    dims.len()
                )));
            }
            for y in 0..dims[1] {
                axis_values[1][y] = axis_convs[1].to_physical(cur.read_raw(data, true)?)?;
                cur.offset += size;
                for x in 0..dims[0] {
                    out[x + y * dims[0]] =
                        cur.conversion.to_physical(cur.read_raw(data, false)?)?;
                    cur.offset += size;
                }
            }
        }
        other => {
            return Err(Error::Value(format!(
                "getValue: index mode {other:?} not supported"
            )))
        }
    }
    Ok(())
}

fn write_checked(
    cur: &mut Cursor,
    seg: &mut MemorySegment,
    size: usize,
    conv: Conversion,
    v: f64,
) -> Result<()> {
    let raw = conv.to_raw(cur.data_type, v)?;
    if raw.is_nan() {
        return Err(Error::Value("setValue: toRaw returned NaN".into()));
    }
    cur.write_raw(seg, raw)?;
    cur.offset += size;
    Ok(())
}

fn fnc_values_write(
    cur: &mut Cursor,
    seg: &mut MemorySegment,
    axis_convs: &[Conversion],
    axis_values: &[Vec<f64>],
    dims: &[usize],
    values: &[f64],
) -> Result<()> {
    let fnc = fnc_values(cur.layout)?;
    let size = size_in_byte(cur.data_type)?;
    cur.align()?;
    match fnc.index_mode {
        IndexMode::ROW_DIR | IndexMode::COLUMN_DIR | IndexMode::NotSet => {
            for flat in flat_order(dims, &nest_order(fnc.index_mode, dims.len())) {
                write_checked(cur, seg, size, cur.conversion, values[flat])?;
            }
        }
        IndexMode::ALTERNATE_WITH_X => match dims.len() {
            1 => {
                for i in 0..dims[0] {
                    write_checked(cur, seg, size, axis_convs[0], axis_values[0][i])?;
                    write_checked(cur, seg, size, cur.conversion, values[i])?;
                }
            }
            2 => {
                for x in 0..dims[0] {
                    write_checked(cur, seg, size, axis_convs[0], axis_values[0][x])?;
                    for y in 0..dims[1] {
                        write_checked(cur, seg, size, cur.conversion, values[x + y * dims[0]])?;
                    }
                }
            }
            _ => {
                return Err(Error::Value(format!(
                    "setValue: {:?} not supported for {} dims",
                    fnc.index_mode,
                    dims.len()
                )))
            }
        },
        IndexMode::ALTERNATE_WITH_Y => {
            if dims.len() != 2 {
                return Err(Error::Value(format!(
                    "setValue: {:?} not supported for {} dims",
                    fnc.index_mode,
                    dims.len()
                )));
            }
            for y in 0..dims[1] {
                write_checked(cur, seg, size, axis_convs[1], axis_values[1][y])?;
                for x in 0..dims[0] {
                    write_checked(cur, seg, size, cur.conversion, values[x + y * dims[0]])?;
                }
            }
        }
        other => {
            return Err(Error::Value(format!(
                "setValue: index mode {other:?} not supported"
            )))
        }
    }
    Ok(())
}

// ===========================================================================
// ===========================================================================

fn read_axis_pts(
    seg: &MemorySegment,
    io: &AxisPtsIo,
    format: ValueObjectFormat,
    address: u64,
) -> Result<Option<Vec<f64>>> {
    let alignments = alignments_of(io.record_layout, io.default_alignments);
    let mut cur = Cursor::new(
        io.record_layout,
        alignments,
        (address - seg.address) as usize,
    );
    cur.byte_order = resolve_byte_order(io.axis_pts.conv.byte_order, io.default_byte_order);
    let max = io.axis_pts.max_axis_points.max(0) as usize;
    let mut count = max;
    let mut values = None;
    for entry in layout_entries(io.record_layout) {
        cur.data_type = entry.data_type();
        match entry {
            LayoutEntry::NoAxisPts(e)
                if io.record_layout.no_axis_pts[0]
                    .as_ref()
                    .is_some_and(|x| std::ptr::eq(x, e)) =>
            {
                cur.conversion = identity_conversion();
                let v = read_seq(
                    &mut cur,
                    seg.data(),
                    1,
                    IndexOrder::INDEX_INCR,
                    DepositType::ABSOLUTE,
                )?;
                count = if v[0] < 0.0 || v[0] > max as f64 {
                    max
                } else {
                    v[0] as usize
                };
            }
            LayoutEntry::AxisPts(e)
                if io.record_layout.axis_pts[0]
                    .as_ref()
                    .is_some_and(|x| std::ptr::eq(x, e)) =>
            {
                cur.conversion = if format == ValueObjectFormat::Physical {
                    io.conversion
                } else {
                    identity_conversion()
                };
                values = Some(read_seq(
                    &mut cur,
                    seg.data(),
                    count,
                    e.index_order,
                    deposit_of(io.axis_pts.deposit),
                )?);
            }
            LayoutEntry::NoAxisPts(e)
                if io
                    .record_layout
                    .no_rescale_x
                    .as_ref()
                    .is_some_and(|x| std::ptr::eq(x, e)) =>
            {
                cur.conversion = identity_conversion();
                let v = read_seq(
                    &mut cur,
                    seg.data(),
                    1,
                    IndexOrder::INDEX_INCR,
                    DepositType::ABSOLUTE,
                )?;
                count = (v[0].max(0.0) as usize).min(
                    io.record_layout
                        .axis_rescale_x
                        .as_ref()
                        .map(|ar| ar.max_no_rescale_pairs.max(0) as usize)
                        .unwrap_or(0),
                );
            }
            LayoutEntry::AxisRescale(e) => {
                cur.conversion = if format == ValueObjectFormat::Physical {
                    io.conversion
                } else {
                    identity_conversion()
                };
                values = Some(read_seq(
                    &mut cur,
                    seg.data(),
                    count * 2,
                    e.index_order,
                    deposit_of(io.axis_pts.deposit),
                )?);
            }
            _ => {}
        }
    }
    Ok(values)
}

fn write_axis_pts(
    seg: &mut MemorySegment,
    io: &AxisPtsIo,
    values: &[f64],
    format: ValueObjectFormat,
    address: u64,
) -> Result<()> {
    let alignments = alignments_of(io.record_layout, io.default_alignments);
    let mut cur = Cursor::new(
        io.record_layout,
        alignments,
        (address - seg.address) as usize,
    );
    cur.byte_order = resolve_byte_order(io.axis_pts.conv.byte_order, io.default_byte_order);
    let count = (io.axis_pts.max_axis_points.max(0) as usize).min(values.len());
    for entry in layout_entries(io.record_layout) {
        cur.data_type = entry.data_type();
        match entry {
            LayoutEntry::NoAxisPts(e)
                if io.record_layout.no_axis_pts[0]
                    .as_ref()
                    .is_some_and(|x| std::ptr::eq(x, e)) =>
            {
                cur.conversion = identity_conversion();
                write_seq(
                    &mut cur,
                    seg,
                    &[count as f64],
                    IndexOrder::INDEX_INCR,
                    DepositType::ABSOLUTE,
                )?;
            }
            LayoutEntry::AxisPts(e)
                if io.record_layout.axis_pts[0]
                    .as_ref()
                    .is_some_and(|x| std::ptr::eq(x, e)) =>
            {
                cur.conversion = if format == ValueObjectFormat::Physical {
                    io.conversion
                } else {
                    identity_conversion()
                };
                write_seq(
                    &mut cur,
                    seg,
                    &values[..count],
                    e.index_order,
                    deposit_of(io.axis_pts.deposit),
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

// ===========================================================================
// ===========================================================================

pub fn get_value(
    segments: &MemorySegmentList,
    io: &CharacteristicIo,
    format: ValueObjectFormat,
    address: u64,
) -> Result<Option<CharValue>> {
    let ch = io.characteristic;
    let Some(seg) = segments.find_mem_seg(address, 1) else {
        return Ok(None);
    };
    let physical = format == ValueObjectFormat::Physical;
    let mut value = CharValue::for_char_type(ch.char_type, CharacteristicRef::Char(ch.clone()))?;
    {
        let base = value.base_mut();
        base.value_format = format;
        base.unit = if physical {
            ch.conv.phys_unit.clone().unwrap_or_default()
        } else {
            String::new()
        };
        base.decimal_count = if physical {
            decimal_count_of(ch.conv.format.as_deref())?
        } else {
            0
        };
    }
    let alignments = alignments_of(io.record_layout, io.default_alignments);
    let mut cur = Cursor::new(
        io.record_layout,
        alignments,
        (address - seg.address) as usize,
    );
    cur.conversion = if physical {
        io.conversion
    } else {
        identity_conversion()
    };
    cur.data_type = fnc_values(io.record_layout)?.data_type;
    cur.byte_order = resolve_byte_order(ch.conv.byte_order, io.default_byte_order);
    cur.bit_op = ch.bitmask.map(BitOperation::new);
    match ch.char_type {
        CharacteristicType::VALUE => {
            let raw = cur.read_raw(seg.data(), false)?;
            value.base_mut().value = ValueData::Scalar(cur.conversion.to_physical(raw)?);
        }
        CharacteristicType::ASCII => {
            let n = number_of_elements(ch)?;
            let bytes = seg.get_data_bytes(address, n)?;
            value.base_mut().value_format = ValueObjectFormat::Physical;
            value.base_mut().value = ValueData::Text(decode_text(bytes, ch.encoding));
        }
        CharacteristicType::VAL_BLK => {
            let n = number_of_elements(ch)?;
            let vals = read_seq(
                &mut cur,
                seg.data(),
                n,
                IndexOrder::INDEX_INCR,
                DepositType::ABSOLUTE,
            )?;
            value.base_mut().value = ValueData::array(vec![n], vals)?;
        }
        CharacteristicType::CURVE
        | CharacteristicType::MAP
        | CharacteristicType::CUBOID
        | CharacteristicType::CUBE_4
        | CharacteristicType::CUBE_5 => {
            read_axis_value(segments, seg, io, format, &mut cur, &mut value)?;
        }
        other => {
            return Err(Error::Value(format!(
                "getValue: unsupported char type {other:?}"
            )))
        }
    }
    Ok(Some(value))
}

pub fn get_axis_pts_value(
    segments: &MemorySegmentList,
    io: &AxisPtsIo,
    format: ValueObjectFormat,
    address: u64,
) -> Result<Option<CharValue>> {
    let Some(seg) = segments.find_mem_seg(address, 1) else {
        return Ok(None);
    };
    let mut value = CharValue::for_char_type(
        CharacteristicType::VAL_BLK,
        CharacteristicRef::AxisPts(io.axis_pts.clone()),
    )?;
    let physical = format == ValueObjectFormat::Physical;
    {
        let base = value.base_mut();
        base.value_format = format;
        base.unit = if physical {
            io.axis_pts.conv.phys_unit.clone().unwrap_or_default()
        } else {
            String::new()
        };
        base.decimal_count = if physical {
            decimal_count_of(io.axis_pts.conv.format.as_deref())?
        } else {
            0
        };
    }
    let vals = read_axis_pts(seg, io, format, address)?;
    value.base_mut().value = match vals {
        Some(v) => {
            let n = v.len();
            ValueData::array(vec![n], v)?
        }
        None => ValueData::None,
    };
    Ok(Some(value))
}

pub fn create_value(
    segments: &MemorySegmentList,
    io: &RecordLayoutRefIo,
    format: ValueObjectFormat,
    address: u64,
) -> Result<Option<CharValue>> {
    match io {
        RecordLayoutRefIo::Characteristic(c) => get_value(segments, c, format, address),
        RecordLayoutRefIo::AxisPts(a) => get_axis_pts_value(segments, a, format, address),
    }
}

fn read_axis_value<'a>(
    segments: &MemorySegmentList,
    seg: &MemorySegment,
    io: &CharacteristicIo<'a>,
    format: ValueObjectFormat,
    cur: &mut Cursor<'a>,
    value: &mut CharValue,
) -> Result<()> {
    let physical = format == ValueObjectFormat::Physical;
    let n_axes = io.axes.len();
    let mut axis_values: Vec<Vec<f64>> = Vec::with_capacity(n_axes);
    let mut sizes = vec![0usize; n_axes];
    let mut unit_axis = Vec::with_capacity(n_axes);
    let mut decimal_count_axis = Vec::with_capacity(n_axes);
    for (i, axis) in io.axes.iter().enumerate() {
        unit_axis.push(if physical {
            axis.descr.phys_unit.clone().unwrap_or_default()
        } else {
            String::new()
        });
        decimal_count_axis.push(if physical {
            decimal_count_of(axis.descr.format.as_deref())?
        } else {
            0
        });
        match axis.descr.axis_type {
            AxisType::COM_AXIS | AxisType::RES_AXIS => {
                let Some(AxisRefIo::AxisPts { io: ap_io, address }) = &axis.reference else {
                    return Err(Error::Value(format!(
                        "getValue: axis {i} ({:?}) has no resolved AXIS_PTS reference",
                        axis.descr.axis_type
                    )));
                };
                let ref_seg = segments.find_mem_seg(*address, 1).ok_or_else(|| {
                    Error::Value(format!(
                        "getValue: no memory segment for referenced AXIS_PTS at {address:#x}"
                    ))
                })?;
                let vals = read_axis_pts(ref_seg, ap_io, format, *address)?.ok_or_else(|| {
                    Error::Value(format!(
                        "getValue: referenced AXIS_PTS at {address:#x} has no AXIS_PTS_X entry"
                    ))
                })?;
                sizes[i] = vals.len();
                axis_values.push(vals);
            }
            AxisType::CURVE_AXIS => {
                let Some(AxisRefIo::CurveAxis { io: c_io, address }) = &axis.reference else {
                    return Err(Error::Value(format!(
                        "getValue: axis {i} (CURVE_AXIS) has no resolved CURVE_AXIS reference"
                    )));
                };
                let cv = get_value(segments, c_io, format, *address)?.ok_or_else(|| {
                    Error::Value(format!(
                        "getValue: no memory segment for referenced curve at {address:#x}"
                    ))
                })?;
                let vals = cv.base().axis_value.first().cloned().unwrap_or_default();
                sizes[i] = vals.len();
                axis_values.push(vals);
            }
            _ => {
                let max = axis.descr.max_axis_points.max(0) as usize;
                sizes[i] = max;
                axis_values.push(vec![0.0; max]);
            }
        }
    }
    // int[], double[][], ref object),35124–35311)
    let axis_convs: Vec<Conversion> = io
        .axes
        .iter()
        .map(|a| {
            if physical {
                a.conversion
            } else {
                identity_conversion()
            }
        })
        .collect();
    let max_points: Vec<i32> = io.axes.iter().map(|a| a.descr.max_axis_points).collect();
    let mut dims = sizes.clone();
    let mut data = vec![0.0; dims.iter().product()];
    for entry in layout_entries(io.record_layout) {
        let axis_idx = entry.axis_idx();
        cur.data_type = entry.data_type();
        match entry {
            LayoutEntry::FncValues(_) => {
                fnc_values_read(
                    cur,
                    seg.data(),
                    &axis_convs,
                    &mut axis_values,
                    &dims,
                    &mut data,
                )?;
                cur.static_size_adjust(data.len(), &max_points)?;
            }
            LayoutEntry::AxisRescale(e) => {
                let saved = cur.conversion;
                cur.conversion = identity_conversion();
                read_seq(
                    cur,
                    seg.data(),
                    e.max_no_rescale_pairs.max(0) as usize * 2,
                    IndexOrder::INDEX_INCR,
                    DepositType::ABSOLUTE,
                )?;
                cur.conversion = saved;
            }
            LayoutEntry::AxisPts(e) => {
                if axis_idx >= 2 || !is_alternate_fnc(io.record_layout) {
                    let ai = axis_idx.max(0) as usize;
                    let descr = io.axes.get(ai).map(|a| a.descr).ok_or_else(|| {
                        Error::Value(format!(
                            "getValue: AXIS_PTS entry axis index {axis_idx} out of range"
                        ))
                    })?;
                    let saved = cur.conversion;
                    cur.conversion = axis_convs[ai];
                    axis_values[ai] = read_seq(
                        cur,
                        seg.data(),
                        sizes[ai],
                        e.index_order,
                        deposit_of(descr.deposit),
                    )?;
                    cur.static_size_adjust(axis_values[ai].len(), &max_points[ai..ai + 1])?;
                    cur.conversion = saved;
                }
            }
            LayoutEntry::NoAxisPts(e) => {
                if axis_idx < 2 && is_alternate_fnc(io.record_layout) {
                    continue;
                }
                cur.align()?;
                let num = cur.read_raw(seg.data(), true)?;
                cur.offset += size_in_byte(cur.data_type)?;
                let fnc_pos = io.record_layout.fnc_values.as_ref().map(|f| f.position);
                if axis_idx >= 0 && (axis_idx as usize) < io.axes.len() {
                    let max = io.axes[axis_idx as usize].descr.max_axis_points;
                    if num > 0.0 && num < max as f64 && fnc_pos.is_some_and(|p| e.position < p) {
                        let ai = axis_idx as usize;
                        sizes[ai] = num as usize;
                        dims[ai] = num as usize;
                        data = vec![0.0; dims.iter().product()];
                        axis_values[ai] = vec![0.0; num as usize];
                    }
                }
            }
        }
    }
    fix_axis_pass(seg, cur, io, &axis_convs, &dims, &mut axis_values)?;
    let base = value.base_mut();
    base.value = ValueData::array(dims, data)?;
    base.axis_value = axis_values;
    base.unit_axis = unit_axis;
    base.decimal_count_axis = decimal_count_axis;
    Ok(())
}

fn fix_axis_pass(
    seg: &MemorySegment,
    cur: &Cursor,
    io: &CharacteristicIo,
    axis_convs: &[Conversion],
    dims: &[usize],
    axis_values: &mut [Vec<f64>],
) -> Result<()> {
    let max_points: Vec<i32> = io.axes.iter().map(|a| a.descr.max_axis_points).collect();
    for (i, axis) in io.axes.iter().enumerate() {
        let descr = axis.descr;
        if descr.axis_type != AxisType::FIX_AXIS {
            continue;
        }
        let conv = axis_convs[i];
        if let Some(par) = &descr.fix_axis_par {
            let step = 2f64.powf(par.shift);
            let n = (par.numberapo.max(0) as usize).min(axis_values[i].len());
            for (j, v) in axis_values[i].iter_mut().take(n).enumerate() {
                *v = conv.to_physical(par.offset + j as f64 * step)?;
            }
            continue;
        }
        if let (Some(offset_desc), Some(shift_desc)) =
            (&io.record_layout.offset[i], &io.record_layout.shift_op[i])
        {
            let off = record_layout_offset(
                io.record_layout,
                io.default_alignments,
                offset_desc.position,
                Some(&max_points),
                0,
            )?;
            let base =
                get_single_raw_value(seg.data(), off, offset_desc.data_type, cur.byte_order, None)?;
            let soff = record_layout_offset(
                io.record_layout,
                io.default_alignments,
                shift_desc.position,
                Some(&max_points),
                0,
            )?;
            let shift =
                get_single_raw_value(seg.data(), soff, shift_desc.data_type, cur.byte_order, None)?;
            let step = 2f64.powf(shift);
            for (k, v) in axis_values[i].iter_mut().enumerate().take(dims[i]) {
                *v = conv.to_physical(base + k as f64 * step)?;
            }
            continue;
        }
        if let Some(dist) = &descr.fix_axis_par_dist {
            let n = (dist.numberapo.max(0) as usize).min(axis_values[i].len());
            for (j, v) in axis_values[i].iter_mut().take(n).enumerate() {
                *v = conv.to_physical(dist.offset + j as f64 * dist.distance)?;
            }
            continue;
        }
        if let (Some(offset_desc), Some(dist_desc)) =
            (&io.record_layout.offset[i], &io.record_layout.dist_op[i])
        {
            let off = record_layout_offset(
                io.record_layout,
                io.default_alignments,
                offset_desc.position,
                Some(&max_points),
                0,
            )?;
            let base =
                get_single_raw_value(seg.data(), off, offset_desc.data_type, cur.byte_order, None)?;
            let doff = record_layout_offset(
                io.record_layout,
                io.default_alignments,
                dist_desc.position,
                Some(&max_points),
                0,
            )?;
            let dist_v =
                get_single_raw_value(seg.data(), doff, dist_desc.data_type, cur.byte_order, None)?;
            for (k, v) in axis_values[i].iter_mut().enumerate().take(dims[i]) {
                *v = conv.to_physical(base + k as f64 * dist_v)?;
            }
            continue;
        }
    }
    Ok(())
}

fn decode_text(bytes: &[u8], encoding: EncodingType) -> String {
    let text = match encoding {
        EncodingType::UTF8 | EncodingType::ASCII => String::from_utf8_lossy(bytes).into_owned(),
        EncodingType::UTF16 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        EncodingType::UTF32 => bytes
            .chunks_exact(4)
            .map(|c| {
                char::from_u32(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .unwrap_or(char::REPLACEMENT_CHARACTER)
            })
            .collect(),
    };
    text.trim_matches('\0').to_string()
}

fn encode_text(text: &str, encoding: EncodingType, number_of_elements: usize) -> Vec<u8> {
    let mut padded = text.to_string();
    while padded.chars().count() < number_of_elements {
        padded.push(' ');
    }
    let mut bytes = match encoding {
        EncodingType::UTF8 | EncodingType::ASCII => padded.into_bytes(),
        EncodingType::UTF16 => padded
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect(),
        EncodingType::UTF32 => padded
            .chars()
            .flat_map(|c| (c as u32).to_le_bytes())
            .collect(),
    };
    bytes.truncate(number_of_elements);
    bytes
}

// ===========================================================================
// ===========================================================================

pub fn set_value(
    base: &mut DataFileBase,
    io: &RecordLayoutRefIo,
    value: &CharValue,
    address: u64,
) -> Result<bool> {
    match io {
        RecordLayoutRefIo::AxisPts(ap_io) => set_axis_pts_value(base, ap_io, value, address),
        RecordLayoutRefIo::Characteristic(c_io) => {
            set_characteristic_value(base, c_io, value, address)
        }
    }
}

/// (35963–35977).
fn set_axis_pts_value(
    base: &mut DataFileBase,
    io: &AxisPtsIo,
    value: &CharValue,
    address: u64,
) -> Result<bool> {
    if !matches!(value, CharValue::ValBlk(_)) {
        return Err(Error::Value(
            "setValue: AXIS_PTS requires a ValBlkValue".into(),
        ));
    }
    let Some(seg) = base.segment_list.find_mem_seg_mut(address, 1) else {
        return Ok(false);
    };
    let vals = match value.base().value.as_array() {
        Some((_, data)) => data.to_vec(),
        None => Vec::new(),
    };
    let format = value.base().value_format;
    write_axis_pts(seg, io, &vals, format, address)?;
    base.mark_changed(io.axis_pts.rec.clone());
    Ok(true)
}

/// (36005–36205).
fn set_characteristic_value(
    base: &mut DataFileBase,
    io: &CharacteristicIo,
    value: &CharValue,
    address: u64,
) -> Result<bool> {
    let ch = io.characteristic;
    let type_ok = matches!(
        (ch.char_type, value),
        (CharacteristicType::VALUE, CharValue::Single(_))
            | (CharacteristicType::ASCII, CharValue::Ascii(_))
            | (CharacteristicType::VAL_BLK, CharValue::ValBlk(_))
            | (CharacteristicType::CURVE, CharValue::Curve(_))
            | (CharacteristicType::MAP, CharValue::Map(_))
            | (CharacteristicType::CUBOID, CharValue::Cuboid(_))
            | (CharacteristicType::CUBE_4, CharValue::Cube4(_))
            | (CharacteristicType::CUBE_5, CharValue::Cube5(_))
    );
    if !type_ok {
        return Err(Error::Value(format!(
            "setValue: value type does not match char type {:?}",
            ch.char_type
        )));
    }
    let physical = value.base().value_format == ValueObjectFormat::Physical;
    let alignments = alignments_of(io.record_layout, io.default_alignments);
    let mut changed: Vec<RecordLayoutRefFields> = vec![ch.rec.clone()];
    let mut result = true;
    let mut ref_unresolved = false;
    let mut ref_writebacks: Vec<(AxisPtsIo, u64, Vec<f64>)> = Vec::new();
    {
        let Some(seg) = base.segment_list.find_mem_seg_mut(address, 1) else {
            return Ok(false);
        };
        let mut cur = Cursor::new(
            io.record_layout,
            alignments,
            (address - seg.address) as usize,
        );
        cur.conversion = if physical {
            io.conversion
        } else {
            identity_conversion()
        };
        cur.data_type = fnc_values(io.record_layout)?.data_type;
        cur.byte_order = resolve_byte_order(ch.conv.byte_order, io.default_byte_order);
        cur.bit_op = ch.bitmask.map(BitOperation::new);
        match ch.char_type {
            CharacteristicType::VALUE => {
                let v = value
                    .base()
                    .value
                    .as_scalar()
                    .ok_or_else(|| Error::Value("setValue: scalar value not set".into()))?;
                let raw = cur.conversion.to_raw(cur.data_type, v)?;
                cur.write_raw(seg, raw)?;
            }
            CharacteristicType::ASCII => {
                let text = value
                    .base()
                    .value
                    .as_text()
                    .ok_or_else(|| Error::Value("setValue: text value not set".into()))?;
                let bytes = encode_text(text, ch.encoding, number_of_elements(ch)?);
                seg.set_data_bytes(cur.offset, &bytes);
            }
            CharacteristicType::VAL_BLK => {
                let n = number_of_elements(ch)?;
                let data = value
                    .base()
                    .value
                    .as_array()
                    .map(|(_, d)| d)
                    .ok_or_else(|| Error::Value("setValue: array value not set".into()))?;
                if data.len() < n {
                    return Err(Error::Value(format!(
                        "setValue: value has {} elements, expected {n}",
                        data.len()
                    )));
                }
                write_seq(
                    &mut cur,
                    seg,
                    &data[..n],
                    IndexOrder::INDEX_INCR,
                    DepositType::ABSOLUTE,
                )?;
            }
            _ => {
                write_axis_value(
                    seg,
                    io,
                    value,
                    &mut cur,
                    &mut ref_writebacks,
                    &mut ref_unresolved,
                )?;
            }
        }
    }
    if ref_unresolved {
        result = false;
    }
    for (ap_io, ref_address, vals) in &ref_writebacks {
        let Some(ref_seg) = base.segment_list.find_mem_seg_mut(*ref_address, 1) else {
            result = false;
            continue;
        };
        write_axis_pts(
            ref_seg,
            ap_io,
            vals,
            value.base().value_format,
            *ref_address,
        )?;
        changed.push(ap_io.axis_pts.rec.clone());
    }
    for r in changed {
        base.mark_changed(r);
    }
    Ok(result)
}

fn write_axis_value<'a>(
    seg: &mut MemorySegment,
    io: &CharacteristicIo<'a>,
    value: &CharValue,
    cur: &mut Cursor<'a>,
    ref_writebacks: &mut Vec<(AxisPtsIo<'a>, u64, Vec<f64>)>,
    ref_unresolved: &mut bool,
) -> Result<()> {
    let ch = io.characteristic;
    let physical = value.base().value_format == ValueObjectFormat::Physical;
    let axis_convs: Vec<Conversion> = io
        .axes
        .iter()
        .map(|a| {
            if physical {
                a.conversion
            } else {
                identity_conversion()
            }
        })
        .collect();
    let max_points: Vec<i32> = io.axes.iter().map(|a| a.descr.max_axis_points).collect();
    let axis_values = &value.base().axis_value;
    if axis_values.len() < io.axes.len() {
        return Err(Error::Value(format!(
            "setValue: axis_value has {} entries, expected {}",
            axis_values.len(),
            io.axes.len()
        )));
    }
    let (dims, data) = value
        .base()
        .value
        .as_array()
        .ok_or_else(|| Error::Value("setValue: array value not set".into()))?;
    let dims = dims.to_vec();
    let data = data.to_vec();
    for entry in layout_entries(io.record_layout) {
        let axis_idx = entry.axis_idx();
        cur.data_type = entry.data_type();
        match entry {
            LayoutEntry::FncValues(_) => {
                cur.conversion = if physical {
                    io.conversion
                } else {
                    identity_conversion()
                };
                cur.byte_order = resolve_byte_order(ch.conv.byte_order, io.default_byte_order);
                cur.bit_op = ch.bitmask.map(BitOperation::new);
                fnc_values_write(cur, seg, &axis_convs, axis_values, &dims, &data)?;
                cur.static_size_adjust(data.len(), &max_points)?;
            }
            LayoutEntry::AxisRescale(_) => {}
            LayoutEntry::AxisPts(e) => {
                let matches_slot = io
                    .record_layout
                    .axis_pts
                    .get(axis_idx.max(0) as usize)
                    .and_then(|s| s.as_ref())
                    .is_some_and(|x| std::ptr::eq(x, e));
                if matches_slot {
                    if e.address_type == AddrType::DIRECT {
                        let ai = axis_idx.max(0) as usize;
                        let descr = io.axes[ai].descr;
                        let vals = &axis_values[ai];
                        let n = (descr.max_axis_points.max(0) as usize).min(vals.len());
                        cur.conversion = axis_convs[ai];
                        cur.byte_order =
                            resolve_byte_order(descr.byte_order, io.default_byte_order);
                        cur.bit_op = None;
                        write_seq(
                            cur,
                            seg,
                            &vals[..n],
                            e.index_order,
                            deposit_of(descr.deposit),
                        )?;
                        cur.static_size_adjust(n, &max_points[ai..ai + 1])?;
                    }
                } else {
                    cur.offset = record_layout_offset(
                        io.record_layout,
                        io.default_alignments,
                        e.position + 1,
                        Some(&max_points),
                        0,
                    )?;
                }
            }
            LayoutEntry::NoAxisPts(e) => {
                let matches_slot = io
                    .record_layout
                    .no_axis_pts
                    .get(axis_idx.max(0) as usize)
                    .and_then(|s| s.as_ref())
                    .is_some_and(|x| std::ptr::eq(x, e));
                if matches_slot {
                    let ai = axis_idx.max(0) as usize;
                    let descr = io.axes[ai].descr;
                    let n = (descr.max_axis_points.max(0) as usize).min(axis_values[ai].len());
                    cur.conversion = identity_conversion();
                    cur.byte_order = resolve_byte_order(descr.byte_order, io.default_byte_order);
                    cur.bit_op = None;
                    write_seq(
                        cur,
                        seg,
                        &[n as f64],
                        IndexOrder::INDEX_INCR,
                        DepositType::ABSOLUTE,
                    )?;
                } else {
                    cur.offset = record_layout_offset(
                        io.record_layout,
                        io.default_alignments,
                        e.position + 1,
                        Some(&max_points),
                        0,
                    )?;
                }
            }
        }
    }
    for (i, axis) in io.axes.iter().enumerate() {
        if !matches!(
            axis.descr.axis_type,
            AxisType::COM_AXIS | AxisType::RES_AXIS
        ) {
            continue;
        }
        let vals = &axis_values[i];
        if vals.is_empty() {
            continue;
        }
        if let Some(AxisRefIo::AxisPts { io: ap_io, address }) = &axis.reference {
            ref_writebacks.push((*ap_io, *address, vals.clone()));
        } else {
            *ref_unresolved = true;
        }
    }
    Ok(())
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::model::base::{AddressFields, NamedFields};
    use autors_a2l::model::compu::RationalCoeffs;
    use autors_a2l::model::enums::{MemoryPrgType, MonotonyType};
    use autors_a2l::model::measurement::AxisDescr;
    use autors_datafile::datafile::MemorySegment;

    use crate::value::IncrementContext;

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    fn cm_identity() -> CompuMethod {
        CompuMethod {
            name: "CM_ID".into(),
            ..Default::default()
        }
    }

    /// LINEAR:`raw = 2*phys + 10`(`phys = (raw-10)/2`).
    fn cm_linear() -> CompuMethod {
        CompuMethod {
            name: "CM_LIN".into(),
            conversion_type: autors_a2l::model::enums::ConversionType::LINEAR,
            coeffs: RationalCoeffs {
                coeffs: [0.0, 2.0, 10.0, 0.0, 0.0, 1.0],
            },
            ..Default::default()
        }
    }

    fn no_axis_pts(
        name: &str,
        position: i32,
        data_type: DataType,
        axis_idx: i32,
    ) -> NoAxisPtsLayoutDesc {
        NoAxisPtsLayoutDesc {
            name: name.into(),
            position,
            data_type,
            axis_idx,
        }
    }

    fn axis_pts_desc(position: i32, data_type: DataType, axis_idx: i32) -> AxisPtsLayoutDesc {
        AxisPtsLayoutDesc {
            name: "AXIS_PTS_X".into(),
            position,
            data_type,
            axis_idx,
            index_order: IndexOrder::INDEX_INCR,
            address_type: AddrType::DIRECT,
        }
    }

    fn fnc(position: i32, data_type: DataType, index_mode: IndexMode) -> FncValuesLayoutDesc {
        FncValuesLayoutDesc {
            name: "FNC_VALUES".into(),
            position,
            data_type,
            index_mode,
            address_type: AddrType::DIRECT,
        }
    }

    fn descr(axis_type: AxisType, max: i32) -> AxisDescr {
        AxisDescr {
            axis_type,
            input_quantity: String::new(),
            conversion: "CM_ID".into(),
            max_axis_points: max,
            lower_limit: 0.0,
            upper_limit: 0.0,
            axis_pts_ref: None,
            curve_axis_ref: None,
            read_only: false,
            extended_limits: None,
            max_grad: None,
            step_size: None,
            format: None,
            phys_unit: None,
            byte_order: ByteOrder::NotSet,
            monotony: MonotonyType::NotSet,
            deposit: DepositType::NotSet,
            fix_axis_par: None,
            fix_axis_par_dist: None,
            children: Vec::new(),
        }
    }

    fn char_of(char_type: CharacteristicType, name: &str, address: u32) -> Characteristic {
        let mut ch = Characteristic {
            char_type,
            ..Default::default()
        };
        ch.named = NamedFields {
            name: name.into(),
            description: None,
        };
        ch.addr = AddressFields {
            address: Some(address),
            ..Default::default()
        };
        ch.conv.conversion = "CM_ID".into();
        ch.rec.record_layout = "RL".into();
        ch
    }

    fn base_of(segs: Vec<(u64, Vec<u8>)>) -> DataFileBase {
        let list = MemorySegmentList::from_vec(
            segs.into_iter()
                .map(|(a, d)| MemorySegment::from_data(a, d, MemoryPrgType::DATA, true))
                .collect(),
        );
        DataFileBase::new(None, list)
    }

    fn io_of<'a>(
        ch: &'a Characteristic,
        rl: &'a RecordLayout,
        conv: Conversion<'a>,
        axes: Vec<AxisIo<'a>>,
    ) -> CharacteristicIo<'a> {
        CharacteristicIo {
            characteristic: ch,
            record_layout: rl,
            conversion: conv,
            axes,
            default_alignments: DEFAULT_ALIGNMENTS,
            default_byte_order: ByteOrder::MSB_LAST,
        }
    }

    // -----------------------------------------------------------------------
    // SingleValue(CHAR_TYPE.VALUE)
    // -----------------------------------------------------------------------

    #[test]
    fn single_value_get_set_physical() {
        let cm = cm_linear();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UWord, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let mut ch = char_of(CharacteristicType::VALUE, "K1", 0x1000);
        ch.conv.phys_unit = Some("rpm".into());
        ch.conv.format = Some("%6.2".into());
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        // raw = 210(UWORD LE)→ phys = (210-10)/2 = 100
        let mut base = base_of(vec![(0x1000, vec![210, 0])]);

        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x1000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().value.as_scalar(), Some(100.0));
        assert_eq!(v.base().unit, "rpm");
        assert_eq!(v.base().decimal_count, 2);

        // setValue:phys 50 → raw = 2*50+10 = 110
        let mut v = v;
        v.base_mut().value = ValueData::Scalar(50.0);
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), &[110, 0]);
        assert!(base.is_dirty);
        assert_eq!(base.changed_values, vec![ch.rec.clone()]);
    }

    #[test]
    fn single_value_raw_format_skips_conversion() {
        let cm = cm_linear();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UWord, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::VALUE, "K1", 0x1000);
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let base = base_of(vec![(0x1000, vec![210, 0])]);
        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Raw, 0x1000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().value.as_scalar(), Some(210.0));
        assert_eq!(v.base().unit, "");
        assert_eq!(v.base().decimal_count, 0);
    }

    #[test]
    fn single_value_bitmask_read_modify_write() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UByte, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let mut ch = char_of(CharacteristicType::VALUE, "K1", 0x1000);
        ch.bitmask = Some(0xF0);
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let mut base = base_of(vec![(0x1000, vec![0xAB])]);
        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Raw, 0x1000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().value.as_scalar(), Some(10.0));
        let mut v = v;
        v.base_mut().value = ValueData::Scalar(5.0);
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), &[0x5B]);
    }

    #[test]
    fn segment_miss_returns_none_and_false() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UByte, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::VALUE, "K1", 0x1000);
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let mut base = base_of(vec![(0x2000, vec![0; 4])]);
        assert!(
            get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x1000)
                .unwrap()
                .is_none()
        );
        let v = CharValue::for_char_type(
            CharacteristicType::VALUE,
            CharacteristicRef::Char(ch.clone()),
        )
        .unwrap();
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(!set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert!(!base.is_dirty);
    }

    // -----------------------------------------------------------------------
    // ASCII / VAL_BLK
    // -----------------------------------------------------------------------

    #[test]
    fn ascii_get_set() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UByte, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let mut ch = char_of(CharacteristicType::ASCII, "TXT", 0x1000);
        ch.number = 8;
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let mut base = base_of(vec![(0x1000, b"EPK\0\0\0\0\0".to_vec())]);
        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Raw, 0x1000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().value.as_text(), Some("EPK"));
        assert_eq!(v.base().value_format, ValueObjectFormat::Physical);

        let mut v = v;
        v.base_mut().value = ValueData::Text("AB".into());
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), b"AB      ");
    }

    #[test]
    fn val_blk_round_trip() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let mut ch = char_of(CharacteristicType::VAL_BLK, "VB", 0x1000);
        ch.number = 3;
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let mut base = base_of(vec![(0x1000, vec![1, 0, 2, 0, 3, 0])]);
        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x1000)
            .unwrap()
            .unwrap();
        let (dims, data) = v.base().value.as_array().unwrap();
        assert_eq!(dims, &[3]);
        assert_eq!(data, &[1.0, 2.0, 3.0]);

        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), &[1, 0, 2, 0, 3, 0]);
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    fn curve_fixture() -> (Characteristic, RecordLayout, Vec<u8>) {
        let rl = RecordLayout {
            name: "RL".into(),
            no_axis_pts: [
                Some(no_axis_pts("NO_AXIS_PTS_X", 1, DataType::UByte, 0)),
                None,
                None,
                None,
                None,
            ],
            axis_pts: [
                Some(axis_pts_desc(2, DataType::UWord, 0)),
                None,
                None,
                None,
                None,
            ],
            fnc_values: Some(fnc(3, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::CURVE, "KL", 0x2000);
        #[rustfmt::skip]
        let bytes = vec![
            3, 0,
            10, 0, 20, 0, 30, 0,
            100, 0, 200, 0, 44, 1,
        ];
        (ch, rl, bytes)
    }

    #[test]
    fn curve_get_set_round_trip() {
        let cm = cm_identity();
        let (ch, rl, bytes) = curve_fixture();
        let conv = Conversion::new(&cm);
        let d = descr(AxisType::STD_AXIS, 4);
        let axes = vec![AxisIo {
            descr: &d,
            conversion: conv,
            reference: None,
        }];
        let io = io_of(&ch, &rl, conv, axes);
        let mut base = base_of(vec![(0x2000, bytes.clone())]);

        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x2000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().axis_value, vec![vec![10.0, 20.0, 30.0]]);
        let (dims, data) = v.base().value.as_array().unwrap();
        assert_eq!(dims, &[3]);
        assert_eq!(data, &[100.0, 200.0, 300.0]);

        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x2000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), bytes.as_slice());

        let mut v = v;
        v.base_mut().value.set(&[1], 250.0).unwrap();
        assert!(set_value(&mut base, &io_ref, &v, 0x2000).unwrap());
        let mut expect = bytes;
        expect[10] = 250;
        expect[11] = 0;
        assert_eq!(base.segment_list.segments[0].data(), expect.as_slice());
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    /// AXIS_PTS_Y(4,UW) FNC_VALUES(5,UW,ROW_DIR);2×3 MAP.
    fn map_fixture() -> (Characteristic, RecordLayout, Vec<u8>) {
        let rl = RecordLayout {
            name: "RL".into(),
            no_axis_pts: [
                Some(no_axis_pts("NO_AXIS_PTS_X", 1, DataType::UByte, 0)),
                Some(no_axis_pts("NO_AXIS_PTS_Y", 3, DataType::UByte, 1)),
                None,
                None,
                None,
            ],
            axis_pts: [
                Some(AxisPtsLayoutDesc {
                    name: "AXIS_PTS_X".into(),
                    ..axis_pts_desc(2, DataType::UByte, 0)
                }),
                Some(AxisPtsLayoutDesc {
                    name: "AXIS_PTS_Y".into(),
                    ..axis_pts_desc(4, DataType::UWord, 1)
                }),
                None,
                None,
                None,
            ],
            fnc_values: Some(fnc(5, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::MAP, "KF", 0x4000);
        #[rustfmt::skip]
        let bytes = vec![
            2, 1, 2,
            3,                    // NY=3
            10, 0, 20, 0, 30, 0,
        ];
        let _ = bytes;
        let bytes = vec![
            2, 1, 2, 3, 10, 0, 20, 0, 30, 0, 0, 0, 1, 0, 10, 0, 11, 0, 20, 0, 21, 0,
        ];
        (ch, rl, bytes)
    }

    #[test]
    fn map_get_set() {
        let cm = cm_identity();
        let (ch, rl, bytes) = map_fixture();
        let conv = Conversion::new(&cm);
        let dx = descr(AxisType::STD_AXIS, 2);
        let dy = descr(AxisType::STD_AXIS, 3);
        let axes = vec![
            AxisIo {
                descr: &dx,
                conversion: conv,
                reference: None,
            },
            AxisIo {
                descr: &dy,
                conversion: conv,
                reference: None,
            },
        ];
        let io = io_of(&ch, &rl, conv, axes);
        let mut base = base_of(vec![(0x4000, bytes.clone())]);

        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x4000)
            .unwrap()
            .unwrap();
        assert_eq!(
            v.base().axis_value,
            vec![vec![1.0, 2.0], vec![10.0, 20.0, 30.0]]
        );
        let (dims, data) = v.base().value.as_array().unwrap();
        assert_eq!(dims, &[2, 3]);
        assert_eq!(data, &[0.0, 1.0, 10.0, 11.0, 20.0, 21.0]);

        let mut v = v;
        v.base_mut().value.set(&[1, 2], 99.0).unwrap();
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x4000).unwrap());
        let mut expect = bytes;
        expect[20] = 99;
        expect[21] = 0;
        assert_eq!(base.segment_list.segments[0].data(), expect.as_slice());
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn curve_com_axis_referenced() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::CURVE, "KL", 0x5000);
        let ap_rl = RecordLayout {
            name: "RL_AX".into(),
            no_axis_pts: [
                Some(no_axis_pts("NO_AXIS_PTS_X", 1, DataType::UByte, 0)),
                None,
                None,
                None,
                None,
            ],
            axis_pts: [
                Some(axis_pts_desc(2, DataType::UWord, 0)),
                None,
                None,
                None,
                None,
            ],
            ..Default::default()
        };
        let mut ap = AxisPts {
            max_axis_points: 3,
            ..Default::default()
        };
        ap.named.name = "AX1".into();
        ap.addr.address = Some(0x5100);
        ap.rec.record_layout = "RL_AX".into();
        let conv = Conversion::new(&cm);
        let ap_io = AxisPtsIo {
            axis_pts: &ap,
            record_layout: &ap_rl,
            conversion: conv,
            default_alignments: DEFAULT_ALIGNMENTS,
            default_byte_order: ByteOrder::MSB_LAST,
        };
        let mut d = descr(AxisType::COM_AXIS, 3);
        d.axis_pts_ref = Some("AX1".into());
        let axes = vec![AxisIo {
            descr: &d,
            conversion: conv,
            reference: Some(AxisRefIo::AxisPts {
                io: ap_io,
                address: 0x5100,
            }),
        }];
        let io = io_of(&ch, &rl, conv, axes);
        let mut base = base_of(vec![
            (0x5000, vec![1, 0, 2, 0, 3, 0]),
            (0x5100, vec![3, 0, 10, 0, 20, 0, 30, 0]),
        ]);

        let v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x5000)
            .unwrap()
            .unwrap();
        assert_eq!(v.base().axis_value, vec![vec![10.0, 20.0, 30.0]]);
        let (dims, data) = v.base().value.as_array().unwrap();
        assert_eq!(dims, &[3]);
        assert_eq!(data, &[1.0, 2.0, 3.0]);

        let mut v = v;
        v.base_mut().axis_value[0][1] = 25.0;
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x5000).unwrap());
        assert_eq!(
            base.segment_list.segments[1].data(),
            &[3, 0, 10, 0, 25, 0, 30, 0]
        );
        assert_eq!(base.changed_values.len(), 2);
        assert!(base.changed_values.contains(&ch.rec));
        assert!(base.changed_values.contains(&ap.rec));
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[test]
    fn increment_then_write_back() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            fnc_values: Some(fnc(1, DataType::UByte, IndexMode::COLUMN_DIR)),
            ..Default::default()
        };
        let mut ch = char_of(CharacteristicType::VALUE, "K1", 0x1000);
        ch.conv.lower_limit = 0.0;
        ch.conv.upper_limit = 255.0;
        let conv = Conversion::new(&cm);
        let io = io_of(&ch, &rl, conv, vec![]);
        let mut base = base_of(vec![(0x1000, vec![100])]);

        let mut v = get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x1000)
            .unwrap()
            .unwrap();
        let ctx = IncrementContext {
            conversion: &conv,
            data_type: DataType::UByte,
            lower_limit: 0.0,
            upper_limit: 255.0,
        };
        assert!(v
            .base_mut()
            .increment_or_decrement_single(true, 1, &ctx)
            .unwrap());
        assert_eq!(v.base().value.as_scalar(), Some(101.0));
        let io_ref = RecordLayoutRefIo::Characteristic(io);
        assert!(set_value(&mut base, &io_ref, &v, 0x1000).unwrap());
        assert_eq!(base.segment_list.segments[0].data(), &[101]);
    }
}
