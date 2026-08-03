//! Shared communication and data-acquisition primitives.
//! This module defines protocol-independent frames, DAQ lists, connection
//! policy, progress callbacks, Seed & Key integration, and the common master
//! state used by CCP, XCP, and diagnostic clients.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::DataType;
use autors_util::helpers::ByteOrderType;

use crate::error::{Error, Result};

pub use autors_native::seed_key::SkType;
pub use autors_util::helpers::BitOperation;
pub use autors_util::helpers::ValueObjectFormat;
pub use autors_util::helpers::{DataPoint, ProgressArgs};

pub fn now_elapsed() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Protocol-independent frame data and timing metadata.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub is_master_frame: bool,
    pub elapsed: Duration,
}

impl Frame {
    pub fn new(data: Vec<u8>, is_master_frame: bool) -> Self {
        Self {
            data,
            is_master_frame,
            elapsed: now_elapsed(),
        }
    }

    pub fn rw_indicator(&self) -> char {
        if self.is_master_frame {
            '\u{2192}'
        } else {
            '\u{2190}'
        }
    }

    pub fn data_str(&self) -> String {
        let mut s = String::with_capacity(self.data.len() * 3);
        for (i, b) in self.data.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    pub fn data_ascii_str(&self, not_allowed_char: char) -> String {
        self.data
            .iter()
            .map(|&b| {
                let c = b as char;
                if c.is_control() || b > 0x7F || c == not_allowed_char {
                    '.'
                } else {
                    c
                }
            })
            .collect()
    }

    pub fn time_str(&self, offset: f64) -> String {
        format!("{:.3}", self.elapsed.as_secs_f64() - offset)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MediaType {
    #[default]
    Can,
    Lin,
}

#[derive(Debug, Clone)]
pub struct FrameBase {
    pub data: Vec<u8>,
    pub elapsed: Duration,
    pub is_master_frame: bool,
}

impl FrameBase {
    pub fn new(is_master_frame: bool) -> Self {
        Self {
            data: Vec::new(),
            elapsed: autors_util::helpers::TimeBase::elapsed(),
            is_master_frame,
        }
    }

    pub fn data_str(&self) -> String {
        let mut s = String::with_capacity(self.data.len() * 3);
        for (i, b) in self.data.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    pub fn time_str(&self, offset: f64) -> String {
        format!("{:.3}", self.elapsed.as_secs_f64() - offset)
    }

    pub fn data_ascii_str(&self, not_allowed_char: char) -> String {
        self.data
            .iter()
            .map(|&b| {
                let c = b as char;
                if c.is_control() || b > 0x7F || c == not_allowed_char {
                    '.'
                } else {
                    c
                }
            })
            .collect()
    }

    pub const fn rw_indicator(&self) -> char {
        if self.is_master_frame {
            '\u{2192}'
        } else {
            '\u{2190}'
        }
    }
}

#[derive(Debug, Clone)]
pub struct FrameEventArgs<T> {
    pub frame: T,
    pub channel: u16,
}

impl<T> FrameEventArgs<T> {
    pub const fn new(frame: T, channel: u16) -> Self {
        Self { frame, channel }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectBehaviourType {
    #[default]
    Automatic,
    Manual,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StartStopMode {
    #[default]
    Stop = 0,
    Start = 1,
    Select = 2,
}

impl StartStopMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Stop),
            1 => Some(Self::Start),
            2 => Some(Self::Select),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EpkCheckResult {
    #[default]
    Equal,
    NotEqual,
    Failed,
    NotApplicable,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProgramClearMode {
    #[default]
    AbsoluteAccess = 0,
    FunctionalAccess = 1,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProgramVerifyMode {
    RequestToStartInternalRoutine = 0,
    SendingVerificationValue = 1,
    #[default]
    None = 2,
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub trait Response {
    fn change_endianness(&mut self) {}
}

pub trait SeedKeyProvider: Send {
    fn sk_type(&self) -> SkType {
        SkType::CCP
    }

    /// `InvalidArgument`).
    fn compute_key_from_seed(&self, seed: &[u8]) -> Option<Vec<u8>>;
}

#[async_trait]
pub trait CommMasterHandle: Send {
    async fn poll_alive(&mut self);

    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// [`CommMaster::add_values_received_callback`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValuesReceivedEventArgs {
    pub daq_list_no: i32,
    pub seconds: f64,
    pub data: Vec<u8>,
}

impl ValuesReceivedEventArgs {
    pub fn new(daq_list_no: i32, seconds: f64, data: Vec<u8>) -> Self {
        Self {
            daq_list_no,
            seconds,
            data,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XcpPrgParams {
    pub clear_mode: ProgramClearMode,
    pub verify_mode: ProgramVerifyMode,
    pub verify_type: u16,
    pub verify_value: u32,
}

impl XcpPrgParams {
    pub fn new(
        clear_mode: ProgramClearMode,
        verify_mode: ProgramVerifyMode,
        verify_type: u16,
        verify_value: u32,
    ) -> Self {
        Self {
            clear_mode,
            verify_mode,
            verify_type,
            verify_value,
        }
    }
}

pub type ProgressCallback<'a> = Option<&'a mut dyn FnMut(&mut ProgressArgs)>;

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub fn data_type_size_in_byte(dt: DataType) -> Option<usize> {
    match dt {
        DataType::UByte | DataType::SByte => Some(1),
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => Some(2),
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => Some(4),
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => Some(8),
        DataType::Unsupported => None,
    }
}

pub fn get_single_raw_value(
    buffer: &[u8],
    offset: usize,
    dt: DataType,
    bo: ByteOrder,
    bits: Option<&BitOperation>,
) -> Result<f64> {
    use autors_util::helpers::DataType as UtilDt;
    let dt = match dt {
        DataType::UByte => UtilDt::UByte,
        DataType::SByte => UtilDt::SByte,
        DataType::UWord => UtilDt::UWord,
        DataType::SWord => UtilDt::SWord,
        DataType::ULong => UtilDt::ULong,
        DataType::SLong => UtilDt::SLong,
        DataType::AUInt64 => UtilDt::AUInt64,
        DataType::AInt64 => UtilDt::AInt64,
        DataType::Float16Ieee => UtilDt::Float16Ieee,
        DataType::Float32Ieee => UtilDt::Float32Ieee,
        DataType::Float64Ieee => UtilDt::Float64Ieee,
        DataType::Unsupported => {
            return Err(Error::Parse(format!(
                "getSingleRawValue: unsupported data type {dt:?}"
            )));
        }
    };
    let bo = if bo == ByteOrder::MSB_FIRST {
        ByteOrderType::MsbFirst
    } else {
        ByteOrderType::MsbLast
    };
    autors_util::helpers::get_single_raw_value(buffer, offset, dt, bo, bits, 0)
        .map_err(|e| Error::Parse(e.to_string()))
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MeasurementInfo {
    pub name: String,
    pub address: u32,
    pub address_extension: i32,
    pub data_type: DataType,
    pub byte_order: ByteOrder,
    pub bits: Option<BitOperation>,
    pub matrix_dim: Option<Vec<i32>>,
    pub phys_conv: Option<Arc<dyn Fn(f64) -> f64 + Send + Sync>>,
}

impl fmt::Debug for MeasurementInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeasurementInfo")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("address_extension", &self.address_extension)
            .field("data_type", &self.data_type)
            .field("byte_order", &self.byte_order)
            .field("bits", &self.bits)
            .field("matrix_dim", &self.matrix_dim)
            .field("phys_conv", &self.phys_conv.as_ref().map(|_| "<closure>"))
            .finish()
    }
}

impl PartialEq for MeasurementInfo {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.address == other.address
            && self.address_extension == other.address_extension
            && self.data_type == other.data_type
            && self.byte_order == other.byte_order
            && self.bits == other.bits
            && self.matrix_dim == other.matrix_dim
    }
}

impl MeasurementInfo {
    pub fn size_in_byte(&self) -> Option<usize> {
        data_type_size_in_byte(self.data_type)
    }

    pub fn array_size(&self) -> i32 {
        self.matrix_dim.as_ref().map_or(1, |d| d.iter().product())
    }

    pub fn address_offset(&self, x: i32, y: i32) -> u32 {
        if x < 0 || self.matrix_dim.is_none() {
            return 0;
        }
        let dim0 = self
            .matrix_dim
            .as_ref()
            .map_or(0, |d| d.first().copied().unwrap_or(0));
        let size = self.size_in_byte().unwrap_or(0) as i64;
        (size * (y as i64 * dim0 as i64 + x as i64)) as u32
    }

    pub fn to_physical(&self, raw: f64) -> f64 {
        match &self.phys_conv {
            Some(conv) => conv(raw),
            None => raw,
        }
    }

    pub fn raw_value(&self, data: &[u8], pos: usize) -> Result<f64> {
        get_single_raw_value(
            data,
            pos,
            self.data_type,
            self.byte_order,
            self.bits.as_ref(),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaqMeasurement {
    pub measurement: MeasurementInfo,
    pub index: i32,
    pub desired_event_channels: Option<Vec<u16>>,
}

impl DaqMeasurement {
    pub fn new(
        measurement: MeasurementInfo,
        index: i32,
        desired_event_channels: Option<Vec<u16>>,
    ) -> Self {
        Self {
            measurement,
            index,
            desired_event_channels,
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct DaqValueBuffer {
    entries: Vec<(f64, Vec<u8>)>,
}

impl DaqValueBuffer {
    fn index_at_or_before(&self, t: f64) -> i32 {
        if self.entries.is_empty() {
            return -1;
        }
        let (mut lo, mut hi) = (0usize, self.entries.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entries[mid].0 <= t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            0
        } else {
            (lo - 1) as i32
        }
    }

    fn range_indices(&self, end: f64, start: f64) -> Option<(usize, usize)> {
        let end_idx = self.index_at_or_before(end);
        if end_idx < 0 {
            return None;
        }
        let (mut lo, mut hi) = (0usize, end_idx as usize + 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entries[mid].0 <= start {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let start_idx = if lo == 0 { 0 } else { lo - 1 };
        Some((start_idx, end_idx as usize))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn add(&mut self, time: f64, data: Vec<u8>) {
        self.entries.push((time, data));
        if self.entries.len() > 100_000 {
            let cut = self.entries.len() / 10;
            self.entries.drain(0..cut);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_data(
        &self,
        entry: &OdtEntry,
        format: ValueObjectFormat,
        x_values: &mut Vec<f64>,
        y_values: &mut Vec<f64>,
        mut start: f64,
        end: f64,
        append: bool,
    ) -> Result<()> {
        let pos = entry.daq_position();
        if pos < 0 {
            return Err(Error::Protocol(
                "ODT entry has no DAQ position (not placed in a DAQ list)".to_string(),
            ));
        }
        if !append {
            x_values.clear();
            y_values.clear();
        } else if let Some(last) = x_values.last() {
            start = last + f64::EPSILON;
        }
        if end < start {
            return Ok(());
        }
        let Some((first, last)) = self.range_indices(end, start) else {
            return Ok(());
        };
        for (time, data) in &self.entries[first..=last] {
            x_values.push(*time);
            let mut v = entry.measurement.raw_value(data, pos as usize)?;
            if format == ValueObjectFormat::Physical {
                v = entry.measurement.to_physical(v);
            }
            y_values.push(v);
        }
        Ok(())
    }

    pub fn get_value(
        &self,
        entry: &OdtEntry,
        idx: usize,
        format: ValueObjectFormat,
    ) -> Option<DataPoint> {
        let (time, data) = self.entries.get(idx)?;
        let pos = entry.daq_position();
        if pos < 0 {
            return None;
        }
        let mut v = entry.measurement.raw_value(data, pos as usize).ok()?;
        if format == ValueObjectFormat::Physical {
            v = entry.measurement.to_physical(v);
        }
        Some(DataPoint::new(*time, v))
    }

    pub fn current_value(&self, entry: &OdtEntry, format: ValueObjectFormat, time: f64) -> f64 {
        let idx = if time.is_nan() {
            self.entries.len() as i32 - 1
        } else {
            self.index_at_or_before(time)
        };
        if idx < 0 {
            return f64::NAN;
        }
        self.get_value(entry, idx as usize, format)
            .map_or(f64::NAN, |p| p.y)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CacheEntry {
    data: Vec<u8>,
    offset: usize,
    trailing: usize,
}

impl CacheEntry {
    fn payload_len(&self) -> usize {
        self.data.len() - self.offset - self.trailing
    }
}

#[derive(Debug)]
pub struct DaqCache {
    expected_lists: i32,
    expected_size: i32,
    map: std::collections::BTreeMap<u8, CacheEntry>,
}

impl DaqCache {
    pub fn new(expected_lists: i32, expected_size: i32) -> Self {
        Self {
            expected_lists,
            expected_size,
            map: std::collections::BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, data: Vec<u8>, offset: usize, trailing: usize) {
        let key = data.first().copied().unwrap_or(0);
        self.map.insert(
            key,
            CacheEntry {
                data,
                offset,
                trailing,
            },
        );
    }

    pub fn complete(&mut self) -> Option<Vec<u8>> {
        let result = (|| {
            if self.map.len() as i32 != self.expected_lists
                || self
                    .map
                    .values()
                    .map(CacheEntry::payload_len)
                    .sum::<usize>()
                    != self.expected_size as usize
            {
                return None;
            }
            let mut out = Vec::with_capacity(self.expected_size as usize);
            for e in self.map.values() {
                out.extend_from_slice(&e.data[e.offset..e.offset + e.payload_len()]);
            }
            Some(out)
        })();
        self.map.clear();
        result
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

// ---------------------------------------------------------------------------
// ODTEntry / ODTList
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct OdtEntry {
    pub measurement: MeasurementInfo,
    pub array_offset: i32,
    pub data_offset: i32,
    pub size: u8,
    pub bit_offset: u8,
    daq_position: AtomicI32,
    values: Arc<Mutex<DaqValueBuffer>>,
}

impl OdtEntry {
    fn new(
        values: Arc<Mutex<DaqValueBuffer>>,
        measurement: MeasurementInfo,
        array_offset: i32,
        data_offset: i32,
        size: u8,
    ) -> Self {
        Self {
            measurement,
            array_offset,
            data_offset,
            size,
            bit_offset: u8::MAX,
            daq_position: AtomicI32::new(-1),
            values,
        }
    }

    pub fn address(&self) -> u32 {
        (self.measurement.address as i64 + self.array_offset as i64 + self.data_offset as i64)
            as u32
    }

    pub fn index(&self) -> i32 {
        self.array_offset / self.measurement.size_in_byte().unwrap_or(1) as i32
    }

    pub fn daq_position(&self) -> i32 {
        self.daq_position.load(AtomicOrdering::SeqCst)
    }

    fn set_daq_position(&self, pos: i32) {
        self.daq_position.store(pos, AtomicOrdering::SeqCst);
    }

    fn buffer(&self) -> MutexGuard<'_, DaqValueBuffer> {
        self.values.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn count(&self) -> usize {
        self.buffer().len()
    }

    pub fn value(&self, time: f64) -> f64 {
        self.current_value(ValueObjectFormat::Raw, time)
    }

    pub fn current_value(&self, format: ValueObjectFormat, time: f64) -> f64 {
        self.buffer().current_value(self, format, time)
    }

    pub fn get_data(
        &self,
        format: ValueObjectFormat,
        x_values: &mut Vec<f64>,
        y_values: &mut Vec<f64>,
        start: f64,
        end: f64,
        append: bool,
    ) -> Result<()> {
        self.buffer()
            .get_data(self, format, x_values, y_values, start, end, append)
    }

    pub fn get_value(&self, idx: usize, format: ValueObjectFormat) -> Option<DataPoint> {
        self.buffer().get_value(self, idx, format)
    }
}

#[derive(Debug, Default)]
pub struct OdtList {
    pub pid: u8,
    remaining: i32,
    entries_left: i32,
    pub size: u16,
    pub entries: Vec<Arc<OdtEntry>>,
}

impl OdtList {
    fn new(pid: u8, capacity: i32, max_entries: i32) -> Self {
        let entry_capacity = capacity.max(0).min(max_entries.max(0)) as usize;
        Self {
            pid,
            remaining: capacity,
            entries_left: max_entries,
            size: 0,
            entries: Vec::with_capacity(entry_capacity),
        }
    }

    fn fits(&self, entry: &OdtEntry) -> bool {
        if self.remaining >= entry.size as i32 {
            return self.entries_left > 0;
        }
        false
    }

    fn consume(&mut self, entry: Arc<OdtEntry>) {
        self.remaining -= entry.size as i32;
        self.entries_left -= 1;
        self.size += entry.size as u16;
        self.entries.push(entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// DAQList / DAQDict
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DaqList {
    daq_no: u16,
    first_pid: u8,
    pub can_id: u32,
    pub evt_no: u16,
    max_dto: u16,
    max_odt: u8,
    max_odt_entries: u8,
    data_offset_first_pid: i32,
    data_offset: i32,
    pub time_cycle: String,
    pub odts: std::collections::BTreeMap<u8, OdtList>,
    pub cache: Option<DaqCache>,
    pub values: Arc<Mutex<DaqValueBuffer>>,
    pub last_timestamp: f64,
    position_ctr: i32,
}

impl DaqList {
    pub fn new_inactive(daq_no: u16) -> Self {
        Self {
            daq_no,
            first_pid: 0,
            can_id: 0,
            evt_no: u16::MAX,
            max_dto: 0,
            max_odt: 0,
            max_odt_entries: 0,
            data_offset_first_pid: 0,
            data_offset: 0,
            time_cycle: String::new(),
            odts: std::collections::BTreeMap::new(),
            cache: None,
            values: Arc::new(Mutex::new(DaqValueBuffer::default())),
            last_timestamp: f64::NAN,
            position_ctr: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        daq_no: u16,
        first_pid: u8,
        evt_no: u16,
        max_dto: u16,
        max_odt: u8,
        max_odt_entries: u8,
        data_offset_first_pid: i32,
        data_offset: i32,
        time_cycle: String,
        can_id: u32,
    ) -> Self {
        Self {
            first_pid,
            can_id,
            evt_no,
            max_dto,
            max_odt,
            max_odt_entries,
            data_offset_first_pid,
            data_offset,
            time_cycle,
            ..Self::new_inactive(daq_no)
        }
    }

    pub fn daq_no(&self) -> u16 {
        self.daq_no
    }

    pub fn first_pid(&self) -> u8 {
        self.first_pid
    }

    pub fn clear_data(&mut self) {
        self.last_timestamp = f64::NAN;
        if let Some(c) = &mut self.cache {
            c.clear();
        }
        self.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    fn new_odt_list(
        &mut self,
        mut b2: u8,
        consumed: &mut Vec<usize>,
        odt_entries: &mut HashMap<u32, Vec<Arc<OdtEntry>>>,
    ) -> Option<u8> {
        let last_pid = self.odts.keys().next_back().copied();
        if self.odts.len() == self.max_odt as usize {
            if let Some(pid) = last_pid {
                let ol = self.odts.get_mut(&pid)?;
                while b2 > 0 {
                    let idx = ol.entries.len().wrapping_sub(1);
                    let entry = ol.entries.get(idx).cloned()?;
                    b2 = b2.wrapping_sub(entry.size);
                    if b2 == 0 {
                        consumed.pop();
                        if let Some(list) = odt_entries.get_mut(&entry.address()) {
                            if let Some(pos) =
                                list.iter().position(|e| e.measurement == entry.measurement)
                            {
                                list.remove(pos);
                            }
                        }
                    }
                    ol.entries.remove(idx);
                }
                if ol.entries.is_empty() {
                    self.odts.remove(&pid);
                }
            }
            return None;
        }
        let pid = last_pid.map_or(self.first_pid, |p| p.wrapping_add(1));
        let capacity = self.max_dto as i32
            - if pid == self.first_pid {
                self.data_offset_first_pid
            } else {
                self.data_offset
            };
        self.odts.insert(
            pid,
            OdtList::new(pid, capacity, self.max_odt_entries as i32),
        );
        Some(pid)
    }

    fn fill_daq_list_indexed<'a>(
        &mut self,
        measurements: impl IntoIterator<Item = (usize, &'a DaqMeasurement)>,
        odt_entries: &mut HashMap<u32, Vec<Arc<OdtEntry>>>,
        is_ccp: bool,
        reset: bool,
    ) -> Result<Vec<usize>> {
        self.position_ctr = if reset { 0 } else { self.position_ctr };
        let mut current_pid: Option<u8> = None;
        let mut consumed = Vec::new();
        for (measurement_index, m) in measurements {
            let meas = &m.measurement;
            let index = m.index;
            let array_size = meas.array_size();
            if index < 0 || index >= array_size {
                return Err(Error::Protocol(format!(
                    "index {index} of measurement {} is out of range (array size {array_size})",
                    meas.name
                )));
            }
            let Some(size) = meas.size_in_byte() else {
                return Err(Error::Protocol(format!(
                    "measurement {}: unsupported data type {:?}",
                    meas.name, meas.data_type
                )));
            };
            let b = size as u8;
            let array_offset = b as i32 * index;
            let mut b2: u8 = 0;
            while b2 < b {
                let need_new = match current_pid.and_then(|pid| self.odts.get(&pid)) {
                    None => true,
                    Some(ol) => ol.remaining == 0 || ol.entries_left == 0,
                };
                if need_new {
                    match self.new_odt_list(b2, &mut consumed, odt_entries) {
                        None => return Ok(consumed),
                        Some(pid) => current_pid = Some(pid),
                    }
                }
                let pid = current_pid.unwrap_or(0);
                let Some(ol) = self.odts.get(&pid) else {
                    return Ok(consumed);
                };
                let num = (b - b2) as i32;
                let mut b3 = num.min(ol.remaining) as u8;
                if is_ccp && b2 == 0 && b > 1 && !b3.is_multiple_of(2) {
                    b3 = if ol.remaining >= 2 {
                        b3 - 1
                    } else {
                        num.min(7) as u8
                    };
                }
                let entry = Arc::new(OdtEntry::new(
                    Arc::clone(&self.values),
                    meas.clone(),
                    array_offset,
                    b2 as i32,
                    b3,
                ));
                let fits = ol.fits(&entry);
                if !fits {
                    match self.new_odt_list(b2, &mut consumed, odt_entries) {
                        None => return Ok(consumed),
                        Some(pid) => current_pid = Some(pid),
                    }
                }
                let pid = current_pid.unwrap_or(0);
                let Some(ol) = self.odts.get_mut(&pid) else {
                    return Ok(consumed);
                };
                ol.consume(Arc::clone(&entry));
                if b2 == 0 {
                    entry.set_daq_position(self.position_ctr);
                    self.position_ctr += b as i32;
                    odt_entries
                        .entry(entry.address())
                        .or_default()
                        .push(Arc::clone(&entry));
                    consumed.push(measurement_index);
                }
                b2 = b2.wrapping_add(b3);
            }
        }
        Ok(consumed)
    }

    /// Fills this DAQ list and returns the indices of measurements that fit.
    ///
    /// This is useful to callers that already own the source vector: indices
    /// avoid cloning every consumed measurement merely to remove it later.
    pub fn fill_daq_list_indices(
        &mut self,
        measurements: &[DaqMeasurement],
        odt_entries: &mut HashMap<u32, Vec<Arc<OdtEntry>>>,
        is_ccp: bool,
        reset: bool,
    ) -> Result<Vec<usize>> {
        self.fill_daq_list_indexed(measurements.iter().enumerate(), odt_entries, is_ccp, reset)
    }

    /// Fills this DAQ list from a sorted subset of measurement indices.
    pub fn fill_daq_list_selected_indices(
        &mut self,
        measurements: &[DaqMeasurement],
        selected: &[usize],
        odt_entries: &mut HashMap<u32, Vec<Arc<OdtEntry>>>,
        is_ccp: bool,
        reset: bool,
    ) -> Result<Vec<usize>> {
        self.fill_daq_list_indexed(
            selected
                .iter()
                .filter_map(|&index| measurements.get(index).map(|m| (index, m))),
            odt_entries,
            is_ccp,
            reset,
        )
    }

    pub fn fill_daq_list(
        &mut self,
        measurements: &[DaqMeasurement],
        odt_entries: &mut HashMap<u32, Vec<Arc<OdtEntry>>>,
        is_ccp: bool,
        reset: bool,
    ) -> Result<Vec<DaqMeasurement>> {
        self.fill_daq_list_indices(measurements, odt_entries, is_ccp, reset)
            .map(|indices| {
                indices
                    .into_iter()
                    .map(|index| measurements[index].clone())
                    .collect()
            })
    }
}

fn remove_indices<T>(values: &mut Vec<T>, indices: &[usize]) {
    let mut indices = indices.iter().copied().peekable();
    let mut index = 0;
    values.retain(|_| {
        let remove = indices.peek().is_some_and(|&next| next == index);
        if remove {
            indices.next();
        }
        index += 1;
        !remove
    });
    debug_assert!(indices.next().is_none());
}

#[derive(Debug, Default)]
pub struct DaqDict {
    pub lists: Vec<DaqList>,
    pub odt_entries: HashMap<u32, Vec<Arc<OdtEntry>>>,
}

impl DaqDict {
    pub fn clear(&mut self) {
        self.odt_entries.clear();
        self.clear_data();
        self.lists.clear();
    }

    pub fn clear_data(&mut self) {
        for list in &mut self.lists {
            list.clear_data();
        }
    }

    pub fn get_odt_entry(
        &self,
        measurement: &MeasurementInfo,
        x: i32,
        y: i32,
    ) -> Option<Arc<OdtEntry>> {
        let key = measurement
            .address
            .wrapping_add(measurement.address_offset(x, y));
        self.odt_entries
            .get(&key)?
            .iter()
            .find(|e| e.measurement == *measurement)
            .cloned()
    }

    pub fn fill_daq_lists(
        &mut self,
        measurements: &mut Vec<DaqMeasurement>,
        is_ccp: bool,
    ) -> Result<usize> {
        let DaqDict { lists, odt_entries } = self;
        odt_entries.reserve(measurements.len());
        for list in lists.iter_mut() {
            if list.evt_no == u16::MAX {
                continue;
            }
            let evt = list.evt_no;
            let candidates: Vec<usize> = measurements
                .iter()
                .enumerate()
                .filter(|m| {
                    m.1.desired_event_channels
                        .as_ref()
                        .is_some_and(|ch| ch.contains(&evt))
                })
                .map(|(index, _)| index)
                .collect();
            let consumed = list.fill_daq_list_selected_indices(
                measurements,
                &candidates,
                odt_entries,
                is_ccp,
                true,
            )?;
            remove_indices(measurements, &consumed);
        }
        for list in lists.iter_mut() {
            if list.evt_no == u16::MAX {
                continue;
            }
            let consumed = list.fill_daq_list_indices(measurements, odt_entries, is_ccp, false)?;
            remove_indices(measurements, &consumed);
        }
        for list in lists.iter_mut() {
            if list.evt_no != u16::MAX && !list.odts.is_empty() {
                let total: u16 = list.odts.values().map(|o| o.size).sum();
                list.cache = Some(DaqCache::new(list.odts.len() as i32, total as i32));
            }
        }
        Ok(measurements.len())
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DaqClock {
    epoch: Instant,
    last_timestamp: f64,
    stopped: Option<f64>,
    override_time: Option<f64>,
}

impl Default for DaqClock {
    fn default() -> Self {
        Self::new()
    }
}

impl DaqClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_timestamp: 0.0,
            stopped: None,
            override_time: None,
        }
    }

    pub fn elapsed_daq_seconds(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64().max(self.last_timestamp)
    }

    pub fn set_last_timestamp(&mut self, v: f64) {
        self.last_timestamp = v.max(self.last_timestamp);
    }

    pub fn last_timestamp(&self) -> f64 {
        self.last_timestamp
    }

    pub fn reset(&mut self) {
        if self.stopped.is_some() {
            self.stopped = None;
            self.override_time = None;
            self.epoch = Instant::now();
            self.last_timestamp = 0.0;
        }
    }

    pub fn stop(&mut self) {
        if self.stopped.is_none() {
            self.stopped = Some(self.last_timestamp);
            self.override_time = Some(self.last_timestamp);
        }
    }

    pub fn daq_time(&self) -> f64 {
        if let Some(v) = self.override_time {
            if !v.is_nan() {
                return v;
            }
        }
        if let Some(v) = self.stopped {
            if !v.is_nan() {
                return v;
            }
        }
        self.last_timestamp
    }

    pub fn set_current_daq_time_from_seconds(&mut self, time: f64) {
        self.override_time = Some(time);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub type ValuesReceivedCallback = Box<dyn FnMut(&ValuesReceivedEventArgs) + Send>;

pub type ConnectionStateCallback = Box<dyn FnMut(bool) + Send>;

pub struct CommMaster {
    pub connect_behaviour: ConnectBehaviourType,
    connected: bool,
    pub slave_connected: bool,
    change_endianess: bool,
    last_received_time: Duration,
    last_state_change: Option<Instant>,
    frames_sent: u64,
    errors_received: u64,
    frames_received: u64,
    events_received: u64,
    services_received: u64,
    bits_transferred: i64,
    rate_start: Option<Instant>,
    pub name: String,
    pub seed_and_key: Option<Box<dyn SeedKeyProvider>>,
    pub prevent_default_requests: bool,
    pub daqs: DaqDict,
    pub daq_clock: DaqClock,
    connection_state_callbacks: Vec<ConnectionStateCallback>,
    values_received_callbacks: Vec<ValuesReceivedCallback>,
}

impl Default for CommMaster {
    fn default() -> Self {
        Self {
            connect_behaviour: ConnectBehaviourType::default(),
            connected: false,
            slave_connected: false,
            change_endianess: false,
            last_received_time: Duration::ZERO,
            last_state_change: None,
            frames_sent: 0,
            errors_received: 0,
            frames_received: 0,
            events_received: 0,
            services_received: 0,
            bits_transferred: 0,
            rate_start: None,
            name: String::new(),
            seed_and_key: None,
            prevent_default_requests: false,
            daqs: DaqDict::default(),
            daq_clock: DaqClock::default(),
            connection_state_callbacks: Vec::new(),
            values_received_callbacks: Vec::new(),
        }
    }
}

impl CommMaster {
    pub fn new(connect_behaviour: ConnectBehaviourType) -> Self {
        Self {
            connect_behaviour,
            ..Self::default()
        }
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    pub fn change_endianess(&self) -> bool {
        self.change_endianess
    }

    pub fn set_change_endianess(&mut self, v: bool) {
        self.change_endianess = v;
    }

    pub fn last_received_time(&self) -> Duration {
        self.last_received_time
    }

    pub fn set_last_received_time(&mut self, v: Duration) {
        self.last_received_time = v;
    }

    pub fn last_state_change(&self) -> Option<Instant> {
        self.last_state_change
    }

    pub fn set_last_state_change_now(&mut self) {
        self.last_state_change = Some(Instant::now());
    }

    pub fn frames_sent(&self) -> u64 {
        self.frames_sent
    }

    pub fn errors_received(&self) -> u64 {
        self.errors_received
    }

    pub fn frames_received(&self) -> u64 {
        self.frames_received
    }

    pub fn events_received(&self) -> u64 {
        self.events_received
    }

    pub fn services_received(&self) -> u64 {
        self.services_received
    }

    pub fn reset_counters_on_disconnect(&mut self) {
        self.frames_sent = 0;
        self.errors_received = 0;
        self.frames_received = 0;
        self.events_received = 0;
    }

    pub fn inc_errors_received(&mut self) {
        self.errors_received += 1;
    }

    pub fn inc_events_received(&mut self) {
        self.events_received += 1;
    }

    pub fn increase_ctr(&mut self, bit_length: usize) {
        self.frames_sent += 1;
        self.bits_transferred += bit_length as i64;
    }

    pub fn record_frame_received(&mut self, bit_length: usize) {
        self.frames_received += 1;
        self.bits_transferred += bit_length as i64;
    }

    pub fn transmission_rate(&mut self) -> f64 {
        match self.rate_start {
            None => {
                self.bits_transferred = 0;
                self.rate_start = Some(Instant::now());
                0.0
            }
            Some(start) => {
                let secs = start.elapsed().as_secs_f64();
                let bps = if secs > 0.0 {
                    self.bits_transferred as f64 / secs
                } else {
                    0.0
                };
                self.bits_transferred = 0;
                self.rate_start = Some(Instant::now());
                bps
            }
        }
    }

    pub fn add_connection_state_callback(&mut self, cb: ConnectionStateCallback) {
        self.connection_state_callbacks.push(cb);
    }

    pub fn add_values_received_callback(&mut self, cb: ValuesReceivedCallback) {
        self.values_received_callbacks.push(cb);
    }

    pub fn raise_connection_state_changed(&mut self) {
        let connected = self.connected;
        for cb in &mut self.connection_state_callbacks {
            cb(connected);
        }
    }

    pub fn raise_on_values_received(&mut self, daq_list_no: i32, seconds: f64, data: &[u8]) {
        let args = ValuesReceivedEventArgs::new(daq_list_no, seconds, data.to_vec());
        for cb in &mut self.values_received_callbacks {
            cb(&args);
        }
    }

    pub fn get_daq_raw_value(&self, measurement: &MeasurementInfo, x: i32, y: i32) -> Option<f64> {
        let entry = self.daqs.get_odt_entry(measurement, x, y)?;
        let v = entry.value(self.daq_clock.daq_time());
        if v.is_nan() {
            return None;
        }
        Some(v)
    }

    pub fn get_daq_phys_value(&self, measurement: &MeasurementInfo, x: i32, y: i32) -> Option<f64> {
        let raw = self.get_daq_raw_value(measurement, x, y)?;
        Some(measurement.to_physical(raw))
    }
}

pub const STR_TRIMMER: [char; 3] = ['\0', 'ÿ', ' '];

pub fn check_epk(expected: &str, device_data: Option<&[u8]>) -> (EpkCheckResult, Option<String>) {
    let expected = expected.trim_end_matches(STR_TRIMMER);
    if expected.is_empty() {
        return (EpkCheckResult::NotApplicable, None);
    }
    let Some(data) = device_data else {
        return (EpkCheckResult::Failed, None);
    };
    let device: String = data
        .iter()
        .map(|&b| if b.is_ascii() { b as char } else { '?' })
        .collect();
    let device = device.trim_end_matches('\0').to_string();
    if expected == device {
        (EpkCheckResult::Equal, Some(device))
    } else {
        (EpkCheckResult::NotEqual, Some(device))
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub struct CommKernel;

type ClientRegistry = Mutex<Vec<Weak<Mutex<dyn CommMasterHandle>>>>;

fn clients() -> &'static ClientRegistry {
    static CLIENTS: OnceLock<ClientRegistry> = OnceLock::new();
    CLIENTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_clients() -> MutexGuard<'static, Vec<Weak<Mutex<dyn CommMasterHandle>>>> {
    clients().lock().unwrap_or_else(|p| p.into_inner())
}

impl CommKernel {
    pub fn register_client<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> Result<()> {
        let mut guard = lock_clients();
        guard.retain(|w| w.strong_count() > 0);
        let new_ptr = Arc::as_ptr(master) as *const ();
        if guard.iter().any(|w| {
            w.upgrade()
                .is_some_and(|m| Arc::as_ptr(&m) as *const () == new_ptr)
        }) {
            let name = master
                .lock()
                .map(|m| m.name().to_string())
                .unwrap_or_default();
            return Err(Error::Protocol(format!(
                "{name} is already registered at the CommKernel"
            )));
        }
        let handle: Arc<Mutex<dyn CommMasterHandle>> = master.clone();
        guard.push(Arc::downgrade(&handle));
        Ok(())
    }

    pub fn deregister_client<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> bool {
        let mut guard = lock_clients();
        let ptr = Arc::as_ptr(master) as *const ();
        let before = guard.len();
        guard.retain(|w| {
            w.strong_count() > 0
                && w.upgrade()
                    .is_some_and(|m| Arc::as_ptr(&m) as *const () != ptr)
        });
        guard.len() != before
    }

    pub fn is_registered<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> bool {
        let guard = lock_clients();
        let ptr = Arc::as_ptr(master) as *const ();
        guard.iter().any(|w| {
            w.upgrade()
                .is_some_and(|m| Arc::as_ptr(&m) as *const () == ptr)
        })
    }

    pub fn registered_count() -> usize {
        let mut guard = lock_clients();
        guard.retain(|w| w.strong_count() > 0);
        guard.len()
    }

    /// [`crate::blocking::BlockingCommKernel`].
    #[allow(clippy::await_holding_lock)]
    pub async fn poll_clients() {
        let handles: Vec<Arc<Mutex<dyn CommMasterHandle>>> = {
            let mut guard = lock_clients();
            guard.retain(|w| w.strong_count() > 0);
            guard.iter().filter_map(Weak::upgrade).collect()
        };
        for h in handles {
            if let Ok(mut g) = h.lock() {
                g.poll_alive().await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub use self::{
    DaqCache as DAQCache, DaqClock as TimeBase, DaqDict as DAQDict, DaqList as DAQList,
    DaqMeasurement as DAQMeasurement, OdtEntry as ODTEntry, OdtList as ODTList,
};

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn meas(name: &str, addr: u32, dt: DataType) -> MeasurementInfo {
        MeasurementInfo {
            name: name.to_string(),
            address: addr,
            data_type: dt,
            ..MeasurementInfo::default()
        }
    }

    fn daq_meas(name: &str, addr: u32, dt: DataType, ch: Option<Vec<u16>>) -> DaqMeasurement {
        DaqMeasurement::new(meas(name, addr, dt), 0, ch)
    }

    fn ccp_list(daq_no: u16, evt_no: u16, max_odt: u8) -> DaqList {
        DaqList::new(
            daq_no,
            0,
            evt_no,
            8,
            max_odt,
            7,
            1,
            1,
            "10ms".to_string(),
            0x400 + daq_no as u32,
        )
    }

    #[test]
    fn enum_values_match_expected_contract() {
        assert_eq!(
            ConnectBehaviourType::default(),
            ConnectBehaviourType::Automatic
        );
        assert_eq!(StartStopMode::Stop as u8, 0);
        assert_eq!(StartStopMode::Start as u8, 1);
        assert_eq!(StartStopMode::Select as u8, 2);
        assert_eq!(StartStopMode::from_u8(2), Some(StartStopMode::Select));
        assert_eq!(StartStopMode::from_u8(3), None);
        assert_eq!(ProgramClearMode::AbsoluteAccess as u8, 0);
        assert_eq!(ProgramClearMode::FunctionalAccess as u8, 1);
        assert_eq!(ProgramVerifyMode::RequestToStartInternalRoutine as u8, 0);
        assert_eq!(ProgramVerifyMode::SendingVerificationValue as u8, 1);
        assert_eq!(ProgramVerifyMode::None as u8, 2);
        assert_eq!(EpkCheckResult::default(), EpkCheckResult::Equal);
        assert!(SkType::CCP.contains(SkType::CCP));
        assert!(!SkType::XCP.contains(SkType::CCP));
        assert!((SkType::CCP | SkType::XCP).contains(SkType::XCP));
        assert_eq!((SkType::CCP | SkType::UDS).0, 5);
    }

    #[test]
    fn small_structs() {
        let p = ProgressArgs::new(42);
        assert_eq!(p.percent, 42);
        assert!(!p.cancel);
        let x = XcpPrgParams::default();
        assert_eq!(x.clear_mode, ProgramClearMode::AbsoluteAccess);
        assert_eq!(x.verify_mode, ProgramVerifyMode::None);
        assert_eq!(x.verify_type, 0);
        let v = ValuesReceivedEventArgs::new(2, 1.5, vec![1, 2]);
        assert_eq!(v.daq_list_no, 2);
        assert_eq!(v.data, vec![1, 2]);
        let d = DataPoint::new(1.0, 2.0);
        assert_eq!((d.x, d.y), (1.0, 2.0));
    }

    #[test]
    fn media_type_and_frame_base() {
        assert_eq!(MediaType::default(), MediaType::Can);
        let mut f = FrameBase::new(true);
        f.data = vec![0x11, 0x22, 0x08, b'A'];
        f.elapsed = Duration::from_millis(250);
        assert_eq!(f.data_str(), "11 22 08 41");
        assert_eq!(f.data_ascii_str('"'), "...A");
        assert_eq!(f.time_str(0.0), "0.250");
        assert_eq!(f.rw_indicator(), '\u{2192}');
        assert_eq!(FrameBase::new(false).rw_indicator(), '\u{2190}');
        let ev = FrameEventArgs::new(f, 2);
        assert_eq!(ev.channel, 2);
    }

    #[test]
    fn bit_operation_matches_expected_contract() {
        assert_eq!(BitOperation::new(0xF0).shift_count, 4);
        assert_eq!(BitOperation::new(u64::MAX).shift_count, 0);
        let le = ByteOrder::MSB_LAST;
        let b = BitOperation::new(u64::MAX);
        assert_eq!(
            get_single_raw_value(&[0xAB], 0, DataType::UByte, le, Some(&b)).unwrap(),
            0xAB as f64
        );
        let b = BitOperation::new(0xF0);
        assert_eq!(
            get_single_raw_value(&[0xAB], 0, DataType::UByte, le, Some(&b)).unwrap(),
            0xA as f64
        );
        let b = BitOperation::new(0x0FF0);
        assert_eq!(
            get_single_raw_value(&[0xCD, 0xAB], 0, DataType::UWord, le, Some(&b)).unwrap(),
            0xBC as f64
        );
    }

    #[test]
    fn size_in_byte_mapping() {
        assert_eq!(data_type_size_in_byte(DataType::UByte), Some(1));
        assert_eq!(data_type_size_in_byte(DataType::SWord), Some(2));
        assert_eq!(data_type_size_in_byte(DataType::Float32Ieee), Some(4));
        assert_eq!(data_type_size_in_byte(DataType::AInt64), Some(8));
        assert_eq!(data_type_size_in_byte(DataType::Unsupported), None);
    }

    #[test]
    fn raw_value_extraction() {
        let buf = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let le = ByteOrder::MSB_LAST;
        let be = ByteOrder::MSB_FIRST;
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::UByte, le, None).unwrap(),
            1.0
        );
        assert_eq!(
            get_single_raw_value(&[0xFF], 0, DataType::SByte, le, None).unwrap(),
            -1.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::UWord, le, None).unwrap(),
            0x0201 as f64
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::UWord, be, None).unwrap(),
            0x0102 as f64
        );
        assert_eq!(
            get_single_raw_value(&[0xFF, 0xFF], 0, DataType::SWord, be, None).unwrap(),
            -1.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::ULong, le, None).unwrap(),
            0x04030201 as f64
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::ULong, be, None).unwrap(),
            0x01020304 as f64
        );
        assert_eq!(
            get_single_raw_value(&[0xFF, 0xFF, 0xFF, 0xFF], 0, DataType::SLong, le, None).unwrap(),
            -1.0
        );
        assert_eq!(
            get_single_raw_value(&buf, 0, DataType::AUInt64, be, None).unwrap(),
            0x0102030405060708u64 as f64
        );
        // float32: 0x3F800000 = 1.0
        assert_eq!(
            get_single_raw_value(
                &[0x00, 0x00, 0x80, 0x3F],
                0,
                DataType::Float32Ieee,
                le,
                None
            )
            .unwrap(),
            1.0
        );
        assert_eq!(
            get_single_raw_value(
                &[0x3F, 0x80, 0x00, 0x00],
                0,
                DataType::Float32Ieee,
                be,
                None
            )
            .unwrap(),
            1.0
        );
        assert_eq!(
            get_single_raw_value(
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F],
                0,
                DataType::Float64Ieee,
                le,
                None
            )
            .unwrap(),
            1.0
        );
        let bits = BitOperation::new(0xF0);
        assert_eq!(
            get_single_raw_value(&[0xAB], 0, DataType::UByte, le, Some(&bits)).unwrap(),
            0xA as f64
        );
        assert!(get_single_raw_value(&buf, 7, DataType::UWord, le, None).is_err());
        assert!(get_single_raw_value(&buf, 0, DataType::Unsupported, le, None).is_err());
        assert!(get_single_raw_value(&buf, 0, DataType::Float16Ieee, le, None).is_err());
    }

    #[test]
    fn measurement_info_helpers() {
        let mut m = meas("m1", 0x1000, DataType::UWord);
        assert_eq!(m.array_size(), 1);
        assert_eq!(m.address_offset(2, 0), 0);
        m.matrix_dim = Some(vec![5, 1, 1]);
        assert_eq!(m.array_size(), 5);
        assert_eq!(m.address_offset(-1, 0), 0);
        assert_eq!(m.address_offset(2, 0), 4); // 2 * (0*5 + 2)
        m.matrix_dim = Some(vec![5, 3, 1]);
        assert_eq!(m.array_size(), 15);
        assert_eq!(m.address_offset(1, 1), 12); // 2 * (1*5 + 1)
        assert_eq!(m.to_physical(2.0), 2.0);
        m.phys_conv = Some(Arc::new(|r| r * 2.0 + 1.0));
        assert_eq!(m.to_physical(2.0), 5.0);
        assert_eq!(m.raw_value(&[0x34, 0x12], 0).unwrap(), 0x1234 as f64);
    }

    #[test]
    fn value_buffer_behaviour() {
        let values = Arc::new(Mutex::new(DaqValueBuffer::default()));
        let m = meas("v", 0x100, DataType::UWord);
        let entry = OdtEntry::new(Arc::clone(&values), m, 0, 0, 2);
        assert!(entry
            .get_data(
                ValueObjectFormat::Raw,
                &mut vec![],
                &mut vec![],
                0.0,
                1.0,
                false
            )
            .is_err());
        assert_eq!(entry.get_value(0, ValueObjectFormat::Raw), None);
        entry.set_daq_position(0);
        {
            let buf = values.lock().unwrap();
            assert!(buf.is_empty());
            assert!(buf
                .current_value(&entry, ValueObjectFormat::Raw, f64::NAN)
                .is_nan());
        }
        let mut buf = values.lock().unwrap();
        buf.add(0.1, vec![0x01, 0x00, 0xAA]);
        buf.add(0.2, vec![0x02, 0x00, 0xBB]);
        buf.add(0.5, vec![0x05, 0x00, 0xCC]);
        assert_eq!(buf.len(), 3);
        assert_eq!(
            buf.current_value(&entry, ValueObjectFormat::Raw, f64::NAN),
            5.0
        );
        assert_eq!(buf.current_value(&entry, ValueObjectFormat::Raw, 0.15), 1.0);
        assert_eq!(buf.current_value(&entry, ValueObjectFormat::Raw, 0.05), 1.0);
        assert_eq!(buf.current_value(&entry, ValueObjectFormat::Raw, 0.5), 5.0);
        assert_eq!(
            buf.get_value(&entry, 1, ValueObjectFormat::Raw),
            Some(DataPoint::new(0.2, 2.0))
        );
        assert_eq!(buf.get_value(&entry, 3, ValueObjectFormat::Raw), None);
        let (mut xs, mut ys) = (vec![], vec![]);
        buf.get_data(
            &entry,
            ValueObjectFormat::Raw,
            &mut xs,
            &mut ys,
            0.0,
            0.3,
            false,
        )
        .unwrap();
        assert_eq!(xs, vec![0.1, 0.2]);
        assert_eq!(ys, vec![1.0, 2.0]);
        buf.get_data(
            &entry,
            ValueObjectFormat::Raw,
            &mut xs,
            &mut ys,
            0.0,
            1.0,
            true,
        )
        .unwrap();
        assert_eq!(xs, vec![0.1, 0.2, 0.2, 0.5]);
        let (mut xs2, mut ys2) = (vec![], vec![]);
        buf.get_data(
            &entry,
            ValueObjectFormat::Raw,
            &mut xs2,
            &mut ys2,
            1.0,
            0.0,
            false,
        )
        .unwrap();
        assert!(xs2.is_empty());
        drop(buf);
        let values2 = Arc::new(Mutex::new(DaqValueBuffer::default()));
        let mut m2 = meas("v2", 0x100, DataType::UByte);
        m2.phys_conv = Some(Arc::new(|r| r * 10.0));
        let e2 = OdtEntry::new(Arc::clone(&values2), m2, 0, 0, 1);
        e2.set_daq_position(0);
        values2.lock().unwrap().add(1.0, vec![3]);
        assert_eq!(
            e2.current_value(ValueObjectFormat::Physical, f64::NAN),
            30.0
        );
        assert_eq!(e2.value(1.0), 3.0);
        assert_eq!(e2.count(), 1);
    }

    #[test]
    fn value_buffer_cap_trim() {
        let mut buf = DaqValueBuffer::default();
        for i in 0..100_001 {
            buf.add(i as f64, vec![1]);
        }
        assert_eq!(buf.len(), 100_001 - 10_000);
        assert_eq!(buf.entries.first().unwrap().0, 10_000.0);
    }

    #[test]
    fn daq_cache_completion() {
        let mut c = DaqCache::new(2, 2);
        c.insert(vec![0, 0x11], 1, 0);
        assert_eq!(c.complete(), None);
        assert!(c.map.is_empty());
        c.insert(vec![1, 0x22], 1, 0);
        c.insert(vec![0, 0x11, 0x33], 1, 1);
        assert_eq!(c.complete(), Some(vec![0x11, 0x22]));
        assert!(c.map.is_empty());
    }

    #[test]
    fn odt_list_capacity_accounting() {
        let values = Arc::new(Mutex::new(DaqValueBuffer::default()));
        let mut ol = OdtList::new(0, 7, 7);
        let e1 = Arc::new(OdtEntry::new(
            Arc::clone(&values),
            meas("a", 0, DataType::ULong),
            0,
            0,
            4,
        ));
        assert!(ol.fits(&e1));
        ol.consume(Arc::clone(&e1));
        assert_eq!(ol.remaining, 3);
        assert_eq!(ol.entries_left, 6);
        assert_eq!(ol.size, 4);
        assert_eq!(ol.len(), 1);
        let e2 = Arc::new(OdtEntry::new(
            Arc::clone(&values),
            meas("b", 4, DataType::ULong),
            0,
            0,
            4,
        ));
        assert!(!ol.fits(&e2));
        let e3 = Arc::new(OdtEntry::new(
            Arc::clone(&values),
            meas("c", 8, DataType::UWord),
            0,
            0,
            2,
        ));
        assert!(ol.fits(&e3));
    }

    #[test]
    fn fill_daq_list_basic_and_positions() {
        let mut list = ccp_list(0, 1, 4);
        let mut odt_entries = HashMap::new();
        let measurements = vec![
            daq_meas("a", 0x1000, DataType::UByte, None),
            daq_meas("b", 0x1001, DataType::UWord, None),
        ];
        let consumed = list
            .fill_daq_list(&measurements, &mut odt_entries, true, true)
            .unwrap();
        assert_eq!(consumed.len(), 2);
        assert_eq!(list.odts.len(), 1);
        let ol = &list.odts[&0];
        assert_eq!(ol.len(), 2);
        assert_eq!(ol.size, 3);
        assert_eq!(ol.entries[0].daq_position(), 0);
        assert_eq!(ol.entries[1].daq_position(), 1);
        assert_eq!(ol.entries[0].address(), 0x1000);
        assert_eq!(ol.entries[1].address(), 0x1001);
        assert!(odt_entries.contains_key(&0x1000));
        assert!(odt_entries.contains_key(&0x1001));
    }

    #[test]
    fn fill_daq_list_ccp_even_split_rule() {
        let mut list = ccp_list(0, 1, 4);
        let mut odt_entries = HashMap::new();
        let measurements = vec![
            daq_meas("pad", 0x2000, DataType::ULong, None),
            daq_meas("x", 0x2004, DataType::ULong, None),
        ];
        let consumed = list
            .fill_daq_list(&measurements, &mut odt_entries, true, true)
            .unwrap();
        assert_eq!(consumed.len(), 2);
        assert_eq!(list.odts.len(), 2);
        let first = &list.odts[&0];
        assert_eq!(
            first.entries.iter().map(|e| e.size).collect::<Vec<_>>(),
            vec![4, 2, 1]
        );
        let second = &list.odts[&1];
        assert_eq!(second.pid, 1);
        assert_eq!(
            second.entries.iter().map(|e| e.size).collect::<Vec<_>>(),
            vec![1]
        );
        let mut list2 = ccp_list(0, 1, 4);
        let mut odt2 = HashMap::new();
        let consumed2 = list2
            .fill_daq_list(&measurements, &mut odt2, false, true)
            .unwrap();
        assert_eq!(consumed2.len(), 2);
        assert_eq!(
            list2.odts[&0]
                .entries
                .iter()
                .map(|e| e.size)
                .collect::<Vec<_>>(),
            vec![4, 3]
        );
    }

    #[test]
    fn fill_daq_list_max_odt_eviction() {
        let mut list = ccp_list(0, 1, 1);
        let mut odt_entries = HashMap::new();
        let measurements = vec![daq_meas("big", 0x3000, DataType::AUInt64, None)];
        let consumed = list
            .fill_daq_list(&measurements, &mut odt_entries, true, true)
            .unwrap();
        assert!(consumed.is_empty());
        assert!(list.odts.is_empty());
        assert!(odt_entries.values().all(|v| v.is_empty()));
    }

    #[test]
    fn fill_daq_list_index_validation() {
        let mut list = ccp_list(0, 1, 4);
        let mut odt_entries = HashMap::new();
        let m = DaqMeasurement::new(meas("arr", 0x4000, DataType::UByte), 5, None);
        assert!(list
            .fill_daq_list(&[m], &mut odt_entries, true, true)
            .is_err());
    }

    #[test]
    fn fill_daq_lists_event_channel_preference() {
        let mut dict = DaqDict::default();
        dict.lists.push(ccp_list(0, 1, 4));
        dict.lists.push(ccp_list(1, 2, 4));
        dict.lists.push(DaqList::new_inactive(9));
        let mut measurements = vec![
            daq_meas("preferred", 0x5000, DataType::UByte, Some(vec![2])),
            daq_meas("plain", 0x5001, DataType::UByte, None),
        ];
        let remaining = dict.fill_daq_lists(&mut measurements, true).unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(dict.lists[0].odts[&0].entries.len(), 1);
        assert_eq!(dict.lists[0].odts[&0].entries[0].measurement.name, "plain");
        assert_eq!(
            dict.lists[1].odts[&0].entries[0].measurement.name,
            "preferred"
        );
        assert!(dict.lists[0].cache.is_some());
        assert!(dict.lists[2].cache.is_none());
        let m = meas("preferred", 0x5000, DataType::UByte);
        assert!(dict.get_odt_entry(&m, -1, 0).is_some());
        let other = meas("nope", 0x9999, DataType::UByte);
        assert!(dict.get_odt_entry(&other, -1, 0).is_none());
        // clear / clear_data
        dict.clear_data();
        assert!(dict.lists[0].last_timestamp.is_nan());
        dict.clear();
        assert!(dict.lists.is_empty() && dict.odt_entries.is_empty());
    }

    #[test]
    fn remove_indices_compacts_ordered_subsequence_with_duplicates() {
        let a = daq_meas("a", 0x1000, DataType::UByte, None);
        let b = daq_meas("b", 0x1001, DataType::UByte, None);
        let c = daq_meas("c", 0x1002, DataType::UByte, None);
        let mut measurements = vec![a.clone(), b.clone(), a.clone(), c.clone()];

        remove_indices(&mut measurements, &[0, 2]);

        assert_eq!(measurements, vec![b, c]);
    }

    #[test]
    fn daq_clock_compatibility_edge_cases() {
        let mut clock = DaqClock::new();
        clock.reset();
        assert_eq!(clock.last_timestamp(), 0.0);
        assert!(clock.elapsed_daq_seconds() >= 0.0);
        clock.set_last_timestamp(1.5);
        clock.set_last_timestamp(1.0);
        assert_eq!(clock.last_timestamp(), 1.5);
        assert_eq!(clock.daq_time(), 1.5);
        clock.set_last_timestamp(2.0);
        clock.stop();
        assert_eq!(clock.daq_time(), 2.0);
        clock.set_last_timestamp(3.0);
        clock.stop();
        assert_eq!(clock.daq_time(), 2.0);
        clock.reset();
        assert_eq!(clock.last_timestamp(), 0.0);
        clock.set_current_daq_time_from_seconds(9.0);
        assert_eq!(clock.daq_time(), 9.0);
    }

    #[test]
    fn comm_master_counters_and_rate() {
        let mut m = CommMaster::new(ConnectBehaviourType::Manual);
        assert!(!m.connected());
        m.set_connected(true);
        assert!(m.connected());
        m.increase_ctr(108);
        m.record_frame_received(64);
        m.inc_errors_received();
        m.inc_events_received();
        assert_eq!(m.frames_sent(), 1);
        assert_eq!(m.frames_received(), 1);
        assert_eq!(m.errors_received(), 1);
        assert_eq!(m.events_received(), 1);
        assert_eq!(m.services_received(), 0);
        assert_eq!(m.transmission_rate(), 0.0);
        m.reset_counters_on_disconnect();
        assert_eq!(m.frames_sent(), 0);
        assert_eq!(m.errors_received(), 0);
        assert_eq!(m.frames_received(), 0);
        assert_eq!(m.events_received(), 0);
    }

    #[test]
    fn comm_master_callbacks_and_daq_value_access() {
        let mut m = CommMaster::new(ConnectBehaviourType::Manual);
        let hits = Arc::new(Mutex::new(Vec::new()));
        let h = Arc::clone(&hits);
        m.add_connection_state_callback(Box::new(move |c| h.lock().unwrap().push(c)));
        let vals = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&vals);
        m.add_values_received_callback(Box::new(move |a| v.lock().unwrap().push(a.clone())));
        m.set_connected(true);
        m.raise_connection_state_changed();
        assert_eq!(*hits.lock().unwrap(), vec![true]);
        m.raise_on_values_received(3, 0.5, &[1, 2, 3]);
        assert_eq!(vals.lock().unwrap()[0].daq_list_no, 3);
        let mut dict = DaqDict::default();
        dict.lists.push(ccp_list(0, 1, 4));
        let mut ms = vec![daq_meas("q", 0x6000, DataType::UByte, None)];
        dict.fill_daq_lists(&mut ms, true).unwrap();
        m.daqs = dict;
        let query = meas("q", 0x6000, DataType::UByte);
        assert_eq!(m.get_daq_raw_value(&query, -1, 0), None);
        let entry = m.daqs.get_odt_entry(&query, -1, 0).unwrap();
        entry.values.lock().unwrap().add(1.0, vec![42]);
        assert_eq!(m.get_daq_raw_value(&query, -1, 0), Some(42.0));
        assert_eq!(m.get_daq_phys_value(&query, -1, 0), Some(42.0));
    }

    #[test]
    fn epk_check_matches_expected_contract() {
        assert_eq!(check_epk("", Some(b"x")).0, EpkCheckResult::NotApplicable);
        assert_eq!(
            check_epk("\0ÿ ", Some(b"x")).0,
            EpkCheckResult::NotApplicable
        );
        assert_eq!(check_epk("EPK1", None), (EpkCheckResult::Failed, None));
        assert_eq!(
            check_epk("EPK-123", Some(b"EPK-123\0\0")),
            (EpkCheckResult::Equal, Some("EPK-123".to_string()))
        );
        assert_eq!(
            check_epk("EPK-123 ÿ\0", Some(b"EPK-123")).0,
            EpkCheckResult::Equal
        );
        // NotEqual
        assert_eq!(
            check_epk("EPK-123", Some(b"EPK-999\0")),
            (EpkCheckResult::NotEqual, Some("EPK-999".to_string()))
        );
        assert_eq!(check_epk("A?B", Some(b"A\xFFB")).0, EpkCheckResult::Equal);
    }

    struct FakeHandle {
        label: String,
        polled: usize,
    }

    #[async_trait]
    impl CommMasterHandle for FakeHandle {
        async fn poll_alive(&mut self) {
            self.polled += 1;
        }
        fn name(&self) -> &str {
            &self.label
        }
    }

    static KERNEL_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(feature = "blocking")]
    #[test]
    fn comm_kernel_registry() {
        use crate::blocking::BlockingCommKernel;
        let _guard = KERNEL_TEST_LOCK.lock().unwrap();
        let master = Arc::new(Mutex::new(FakeHandle {
            label: "T1".to_string(),
            polled: 0,
        }));
        let before = BlockingCommKernel::registered_count();
        assert!(!BlockingCommKernel::is_registered(&master));
        BlockingCommKernel::register_client(&master).unwrap();
        assert!(BlockingCommKernel::is_registered(&master));
        assert_eq!(BlockingCommKernel::registered_count(), before + 1);
        assert!(BlockingCommKernel::register_client(&master).is_err());
        BlockingCommKernel::poll_clients();
        assert!(master.lock().unwrap().polled >= 1);
        assert!(BlockingCommKernel::deregister_client(&master));
        assert!(!BlockingCommKernel::is_registered(&master));
        assert!(!BlockingCommKernel::deregister_client(&master));
        let temp = Arc::new(Mutex::new(FakeHandle {
            label: "T2".to_string(),
            polled: 0,
        }));
        BlockingCommKernel::register_client(&temp).unwrap();
        let n = BlockingCommKernel::registered_count();
        drop(temp);
        BlockingCommKernel::poll_clients();
        assert_eq!(BlockingCommKernel::registered_count(), n - 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn async_poll_alive_via_dyn_handle() {
        let shared = Arc::new(Mutex::new(FakeHandle {
            label: "D1".to_string(),
            polled: 0,
        }));
        let obj: Arc<Mutex<dyn CommMasterHandle>> = shared.clone();
        assert_eq!(obj.lock().unwrap().name(), "D1");
        obj.lock().unwrap().poll_alive().await;
        obj.lock().unwrap().poll_alive().await;
        assert_eq!(shared.lock().unwrap().polled, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn poll_clients_async_drives_registered_handles() {
        let _guard = KERNEL_TEST_LOCK.lock().unwrap();
        let master = Arc::new(Mutex::new(FakeHandle {
            label: "A1".to_string(),
            polled: 0,
        }));
        CommKernel::register_client(&master).unwrap();
        assert!(CommKernel::is_registered(&master));
        CommKernel::poll_clients().await;
        CommKernel::poll_clients().await;
        assert!(master.lock().unwrap().polled >= 2);
        assert!(CommKernel::deregister_client(&master));
        let polled = master.lock().unwrap().polled;
        CommKernel::poll_clients().await;
        assert_eq!(master.lock().unwrap().polled, polled);
    }

    #[test]
    fn response_trait_default_noop() {
        struct R;
        impl Response for R {}
        let mut r = R;
        r.change_endianness();
    }
}
