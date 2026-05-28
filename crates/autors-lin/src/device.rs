//! LIN device abstraction and bus support: frame types, configuration, baudrates, classic/enhanced
//! checksums, and a logging frame queue.
//! Design notes:
//! - Checksums and PID parity bits are implemented directly per the LIN
//!   specification (LIN 2.x §2.3.1.5 / §2.8.3), so they can be used without
//!   vendor hardware and in tests; hardware backends may instead delegate
//!   checksum computation to their drivers.
//! - [`LinFrame`] carries the log timestamp, data bytes, and master/slave
//!   direction as plain fields. The timestamp is taken from a process-wide
//!   monotonic clock based on [`std::time::Instant`].
//! - Log formatting is parameterized: the timestamp decimal count is an
//!   argument of the formatting methods (with [`DEFAULT_LOG_TIME_DECIMALS`]
//!   as the default), and master/slave/ID log filtering is configured
//!   explicitly via [`LinLogFilter`] on [`LinFrameQueue`] (defaults: log both
//!   directions, no ID filter).
//! - File logging writes one line synchronously per enqueued frame; there is
//!   no background writer task, disk-space check, or frame-enqueued event.
//! - Transport is abstracted as the small trait [`LinDevice`], with
//!   receiving driven by polling from the implementer; there is no
//!   background receive/dispatch thread or listener registration facility.
//! - [`LinConfiguration`] is a plain struct; there is no serialization
//!   support (this crate has no serde dependency).
//! - Canonical CSV log line example:
//!   `0.000;"TESTBUS";"→ 22";4;"48 65 6C 22";"Hel."`.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// LIN sync byte (0x55, following the Break in the frame header). The sync
/// Break is a physical-layer signal with no byte representation.
pub const SYNC_BYTE: u8 = 0x55;

/// Maximum length of the LIN data field, in bytes.
pub const MAX_DATA_LEN: usize = 8;

/// Default number of decimal places for log timestamps.
pub const DEFAULT_LOG_TIME_DECIMALS: usize = 3;

/// Frame count limit of the queue: once the queue exceeds 110000 frames it is
/// trimmed back to [`FRAME_CAP_KEEP`].
const FRAME_CAP: usize = 110_000;
/// Number of frames kept after the cap triggers (see [`FRAME_CAP`]).
const FRAME_CAP_KEEP: usize = 100_000;

// ---------------------------------------------------------------------------
// Time base
// ---------------------------------------------------------------------------

/// Process-wide monotonic clock: duration since (roughly) process start.
/// The starting [`Instant`] is initialized lazily on first access.
fn time_base_elapsed() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Checksum algorithm for LIN frames: the classic checksum or the enhanced
/// checksum defined by the LIN specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChecksumType {
    /// Classic checksum: inverted sum of the data bytes only (LIN 1.3 slaves,
    /// LIN 2.x diagnostic frames 0x3C/0x3D). Numeric value 0.
    #[default]
    CalcChecksum = 0,
    /// Enhanced checksum: inverted sum of the PID and data bytes (LIN 2.x
    /// normal frames). Numeric value 1.
    CalcChecksumEnhanced = 1,
}

impl ChecksumType {
    /// Numeric value of the enum (0/1).
    pub fn to_num(self) -> u32 {
        self as u32
    }

    /// Converts a numeric value back to the enum; unknown values return
    /// `None`.
    pub fn from_num(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::CalcChecksum),
            1 => Some(Self::CalcChecksumEnhanced),
            _ => None,
        }
    }
}

/// LIN protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LinVersion {
    /// LIN 1.3 (numeric value 0; the default version).
    #[default]
    V1_3 = 0,
    /// LIN 2.0 (numeric value 1).
    V2_0 = 1,
    /// LIN 2.1 (numeric value 2; the version used by `LinConfiguration::new`).
    V2_1 = 2,
}

impl LinVersion {
    /// Numeric value of the enum.
    pub fn to_num(self) -> u32 {
        self as u32
    }

    /// Converts a numeric value back to the enum; unknown values return
    /// `None`.
    pub fn from_num(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::V1_3),
            1 => Some(Self::V2_0),
            2 => Some(Self::V2_1),
            _ => None,
        }
    }
}

/// Standard LIN baudrates (numeric values are the baudrate itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LinBaudrate {
    /// Not set (numeric value 0).
    #[default]
    NotSet = 0,
    /// 1200 baud.
    B1200 = 1200,
    /// 2400 baud.
    B2400 = 2400,
    /// 4800 baud.
    B4800 = 4800,
    /// 9600 baud.
    B9600 = 9600,
    /// 19200 baud.
    B19200 = 19200,
}

impl LinBaudrate {
    /// Numeric value of the enum (the baudrate itself).
    pub fn to_num(self) -> u16 {
        self as u16
    }

    /// Converts a raw value back to the enum; non-standard values return
    /// `None`.
    pub fn from_num(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::NotSet),
            1200 => Some(Self::B1200),
            2400 => Some(Self::B2400),
            4800 => Some(Self::B4800),
            9600 => Some(Self::B9600),
            19200 => Some(Self::B19200),
            _ => None,
        }
    }

    /// Human-readable description of the baudrate.
    pub fn description(self) -> &'static str {
        match self {
            Self::NotSet => "Not set",
            Self::B1200 => "1200 Baud",
            Self::B2400 => "2400 Baud",
            Self::B4800 => "4800 Baud",
            Self::B9600 => "9600 Baud",
            Self::B19200 => "19200 Baud",
        }
    }
}

// ---------------------------------------------------------------------------
// LINConfiguration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinConfiguration {
    pub master_id: u8,
    pub channel: i32,
    pub baudrate: u16,
    pub version: LinVersion,
    pub dlc: u8,
    pub checksum_type: ChecksumType,
    pub hardware_type: i32,
    pub bus_id: Option<String>,
}

impl LinConfiguration {
    /// (version = 2.1, dlc = 8, checksum = classic, channel = 0).
    pub fn new(master_id: u8, baudrate: u16) -> Self {
        Self::with_options(
            master_id,
            baudrate,
            LinVersion::V2_1,
            8,
            ChecksumType::CalcChecksum,
            0,
        )
    }

    pub fn with_options(
        master_id: u8,
        baudrate: u16,
        version: LinVersion,
        dlc: u8,
        checksum_type: ChecksumType,
        channel: i32,
    ) -> Self {
        LinConfiguration {
            master_id,
            channel,
            baudrate,
            version,
            dlc,
            checksum_type,
            hardware_type: 0,
            bus_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// P0 = ID0⊕ID1⊕ID2⊕ID4(bit6);P1 = ¬(ID1⊕ID3⊕ID4⊕ID5)(bit7).
pub fn protected_id(id: u8) -> u8 {
    let id = id & 0x3F;
    let b = |n: u8| (id >> n) & 1;
    let p0 = b(0) ^ b(1) ^ b(2) ^ b(4);
    let p1 = !(b(1) ^ b(3) ^ b(4) ^ b(5)) & 1;
    id | (p0 << 6) | (p1 << 7)
}

fn lin_sum<'a>(bytes: impl IntoIterator<Item = &'a u8>) -> u8 {
    let mut sum: u16 = 0;
    for &b in bytes {
        sum += u16::from(b);
        if sum > 0xFF {
            sum -= 0xFF;
        }
    }
    sum as u8
}

pub fn classic_checksum(data: &[u8]) -> u8 {
    !lin_sum(data)
}

pub fn enhanced_checksum(pid: u8, data: &[u8]) -> u8 {
    !lin_sum(std::iter::once(&pid).chain(data))
}

pub fn compute_checksum(checksum_type: ChecksumType, id: u8, data: &[u8]) -> u8 {
    match checksum_type {
        ChecksumType::CalcChecksum => classic_checksum(data),
        ChecksumType::CalcChecksumEnhanced => enhanced_checksum(protected_id(id), data),
    }
}

// ---------------------------------------------------------------------------
// LINFrame
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinFrame {
    pub bus_id: String,
    pub id: u8,
    pub data: Vec<u8>,
    pub is_master_frame: bool,
    pub elapsed: Duration,
}

impl LinFrame {
    pub fn new(bus_id: &str, id: u8, data: Vec<u8>, is_master_frame: bool) -> Self {
        LinFrame {
            bus_id: bus_id.to_owned(),
            id,
            data,
            is_master_frame,
            elapsed: time_base_elapsed(),
        }
    }

    pub fn with_len(
        bus_id: &str,
        id: u8,
        data: Vec<u8>,
        len: u8,
        is_master_frame: bool,
    ) -> Result<Self> {
        let len = usize::from(len);
        if len > data.len() {
            return Err(Error::Parse(format!(
                "LINFrame: len {len} exceeds data length {}",
                data.len()
            )));
        }
        let mut frame = Self::new(bus_id, id, data, is_master_frame);
        frame.data.truncate(len);
        Ok(frame)
    }

    pub fn from_u64(bus_id: &str, id: u8, data: u64, len: u8, is_master_frame: bool) -> Self {
        let len = usize::from(len);
        let bytes = (0..len)
            .map(|i| if i < 8 { (data >> (8 * i)) as u8 } else { 0 })
            .collect();
        Self::new(bus_id, id, bytes, is_master_frame)
    }

    pub fn rw_indicator(&self) -> char {
        if self.is_master_frame {
            '\u{2192}'
        } else {
            '\u{2190}'
        }
    }

    pub fn address(&self) -> String {
        format!("{} {}", self.rw_indicator(), to_lin_id_string(self.id))
    }

    pub fn data_str(&self) -> String {
        let mut s = String::with_capacity(self.data.len() * 3);
        for b in &self.data {
            let _ = write!(s, "{b:02X} ");
        }
        s.pop();
        s
    }

    pub fn data_ascii_str(&self, not_allowed: char) -> String {
        self.data
            .iter()
            .map(|&b| {
                let c = b as char;
                if c.is_control() || b > 0x7F || c == not_allowed {
                    '.'
                } else {
                    c
                }
            })
            .collect()
    }

    pub fn time_str(&self) -> String {
        self.time_str_with(DEFAULT_LOG_TIME_DECIMALS, 0.0)
    }

    /// `Settings.Default.LogTimeDecimalCount`).
    pub fn time_str_with(&self, decimals: usize, offset_secs: f64) -> String {
        format!(
            "{:.decimals$}",
            self.elapsed.as_secs_f64() - offset_secs,
            decimals = decimals
        )
    }

    pub fn to_csv(&self) -> String {
        self.to_csv_with(DEFAULT_LOG_TIME_DECIMALS)
    }

    pub fn to_csv_with(&self, decimals: usize) -> String {
        self.format_line(';', true, '"', decimals)
    }

    pub fn to_clipboard(&self) -> String {
        self.to_clipboard_with(DEFAULT_LOG_TIME_DECIMALS)
    }

    pub fn to_clipboard_with(&self, decimals: usize) -> String {
        self.format_line('\t', false, '\t', decimals)
    }

    fn format_line(&self, sep: char, quote: bool, not_allowed: char, decimals: usize) -> String {
        let time = self.time_str_with(decimals, 0.0);
        let addr = self.address();
        let data = self.data_str();
        let ascii = self.data_ascii_str(not_allowed);
        if quote {
            format!(
                "{time}{sep}\"{}\"{sep}\"{addr}\"{sep}{}{sep}\"{data}\"{sep}\"{ascii}\"",
                self.bus_id,
                self.data.len()
            )
        } else {
            format!(
                "{time}{sep}{}{sep}{addr}{sep}{}{sep}{data}{sep}{ascii}",
                self.bus_id,
                self.data.len()
            )
        }
    }

    pub fn pid(&self) -> u8 {
        protected_id(self.id)
    }

    pub fn checksum(&self, checksum_type: ChecksumType) -> u8 {
        compute_checksum(checksum_type, self.id, &self.data)
    }

    pub fn encode_header(&self) -> [u8; 2] {
        [SYNC_BYTE, self.pid()]
    }

    pub fn encode_response(&self, checksum_type: ChecksumType) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 2);
        out.push(self.pid());
        out.extend_from_slice(&self.data);
        out.push(self.checksum(checksum_type));
        out
    }

    pub fn decode_response(bytes: &[u8], checksum_type: ChecksumType) -> Result<(u8, Vec<u8>)> {
        if bytes.len() < 2 {
            return Err(Error::Parse(format!(
                "LIN response too short: {} bytes",
                bytes.len()
            )));
        }
        if bytes.len() > MAX_DATA_LEN + 2 {
            return Err(Error::Parse(format!(
                "LIN response too long: {} bytes",
                bytes.len()
            )));
        }
        let pid = bytes[0];
        let id = pid & 0x3F;
        if protected_id(id) != pid {
            return Err(Error::Parse(format!(
                "LIN PID parity mismatch: 0x{pid:02X}"
            )));
        }
        let data = &bytes[1..bytes.len() - 1];
        let expect = compute_checksum(checksum_type, id, data);
        let actual = bytes[bytes.len() - 1];
        if expect != actual {
            return Err(Error::Parse(format!(
                "LIN checksum mismatch: expected 0x{expect:02X}, got 0x{actual:02X}"
            )));
        }
        Ok((id, data.to_vec()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinEventArgs {
    pub frame: LinFrame,
}

impl LinEventArgs {
    pub fn new(frame: LinFrame) -> Self {
        LinEventArgs { frame }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinFilterIds(HashSet<u8>);

impl LinFilterIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_ids(ids: &[u8]) -> Self {
        LinFilterIds(ids.iter().copied().collect())
    }

    pub fn insert(&mut self, id: u8) -> bool {
        self.0.insert(id)
    }

    pub fn contains(&self, id: u8) -> bool {
        self.0.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<u8> for LinFilterIds {
    fn from_iter<I: IntoIterator<Item = u8>>(iter: I) -> Self {
        LinFilterIds(iter.into_iter().collect())
    }
}

pub type LinFrameDict = BTreeMap<u64, LinFrame>;

// ---------------------------------------------------------------------------
// LINFrameQueue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinLogFilter {
    pub show_master: bool,
    pub show_slave: bool,
    pub id_filter: Option<LinFilterIds>,
}

impl Default for LinLogFilter {
    fn default() -> Self {
        LinLogFilter {
            show_master: true,
            show_slave: true,
            id_filter: None,
        }
    }
}

#[derive(Default)]
struct QueueInner {
    frames: Vec<LinFrame>,
    read_cursor: usize,
    dict: LinFrameDict,
    filter: LinLogFilter,
    log_dir: Option<PathBuf>,
    writer: Option<BufWriter<File>>,
}

pub struct LinFrameQueue {
    inner: Mutex<QueueInner>,
}

impl LinFrameQueue {
    pub const LOG_FILE_NAME: &'static str = "LINLog.csv";

    pub const LOG_FILE_CSV_HEADER: &'static str = "Time[s];Source;ID;Len;Data;ASCII";

    pub fn new() -> Self {
        LinFrameQueue {
            inner: Mutex::new(QueueInner::default()),
        }
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<LinFrameQueue> = OnceLock::new();
        INSTANCE.get_or_init(LinFrameQueue::new)
    }

    fn lock(&self) -> MutexGuard<'_, QueueInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn filter(&self) -> LinLogFilter {
        self.lock().filter.clone()
    }

    pub fn set_filter(&self, filter: LinLogFilter) {
        self.lock().filter = filter;
    }

    pub fn enqueue_frame(&self, frame: LinFrame, unique_bus_id: Option<i32>) -> bool {
        let mut inner = self.lock();
        let f = &inner.filter;
        if (!f.show_master && frame.is_master_frame) || (!f.show_slave && !frame.is_master_frame) {
            return false;
        }
        if let Some(ids) = &f.id_filter {
            if !ids.is_empty() && !ids.contains(frame.id) {
                return false;
            }
        }
        let key = match unique_bus_id {
            Some(bus) => ((bus as u64) << 32) | u64::from(frame.id),
            None => u64::from(frame.id),
        };
        inner.dict.insert(key, frame.clone());
        if let Some(w) = inner.writer.as_mut() {
            let _ = writeln!(w, "{}", frame.to_csv());
            let _ = w.flush();
        }
        inner.frames.push(frame);
        if inner.frames.len() > FRAME_CAP {
            let drop = inner.frames.len() - FRAME_CAP_KEEP;
            inner.frames.drain(..drop);
            inner.read_cursor = inner.read_cursor.saturating_sub(drop);
        }
        true
    }

    pub fn frame_dict(&self) -> LinFrameDict {
        self.lock().dict.clone()
    }

    pub fn get_frames(&self) -> (Vec<LinFrame>, usize) {
        let mut inner = self.lock();
        inner.read_cursor = inner.frames.len();
        (inner.frames.clone(), inner.frames.len())
    }

    pub fn get_unread_frames(&self) -> (Vec<LinFrame>, usize) {
        let mut inner = self.lock();
        let total = inner.frames.len();
        let unread = inner.frames[inner.read_cursor.min(total)..].to_vec();
        inner.read_cursor = total;
        (unread, total)
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.frames.clear();
        inner.read_cursor = 0;
        inner.dict.clear();
    }

    pub fn update_file_log_setting(&self, log_to_file: bool, log_dir: &Path) -> Result<()> {
        let mut inner = self.lock();
        if log_to_file && inner.writer.is_some() && inner.log_dir.as_deref() == Some(log_dir) {
            return Ok(());
        }
        inner.writer = None;
        inner.log_dir = None;
        if !log_to_file {
            return Ok(());
        }
        let path = log_dir.join(Self::LOG_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        let mut w = BufWriter::new(file);
        writeln!(w, "{}", Self::LOG_FILE_CSV_HEADER)?;
        w.flush()?;
        inner.writer = Some(w);
        inner.log_dir = Some(log_dir.to_path_buf());
        Ok(())
    }
}

impl Default for LinFrameQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub fn to_lin_id_string(id: u8) -> String {
    format!("{id:X}")
}

pub fn make_bus_id(device_name: &str, channel: i32) -> String {
    format!("{device_name}/LIN{}", channel + 1)
}

#[async_trait]
pub trait LinDevice {
    fn unique_bus_id(&self) -> i32;

    fn is_available(&self) -> bool;

    async fn open(&mut self, config: &LinConfiguration) -> Result<bool>;

    async fn send(&mut self, id: u8, data: &[u8]) -> Result<usize>;

    /// Sends only a LIN header for `id`, requesting a subscriber response.
    ///
    /// Devices that cannot issue header-only requests retain source
    /// compatibility through this default, but diagnostic transports require
    /// a backend that overrides it.
    async fn request(&mut self, id: u8) -> Result<bool> {
        Err(Error::Protocol(format!(
            "LIN device does not support header-only request for ID 0x{id:02X}"
        )))
    }

    async fn on_receive(&mut self) -> Result<Option<LinFrame>>;

    async fn close(&mut self);

    fn has_sent(&self, frame: LinFrame) -> usize {
        let len = frame.data.len();
        LinFrameQueue::instance().enqueue_frame(frame, Some(self.unique_bus_id()));
        len
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn master_frame() -> LinFrame {
        let mut f = LinFrame::new("TESTBUS", 0x22, vec![0x48, 0x65, 0x6C, 0x22], true);
        f.elapsed = Duration::ZERO;
        f
    }

    // ---- ChecksumType / LinVersion / LinBaudrate ----

    #[test]
    fn checksum_type_values() {
        assert_eq!(ChecksumType::CalcChecksum.to_num(), 0);
        assert_eq!(ChecksumType::CalcChecksumEnhanced.to_num(), 1);
        assert_eq!(ChecksumType::from_num(0), Some(ChecksumType::CalcChecksum));
        assert_eq!(
            ChecksumType::from_num(1),
            Some(ChecksumType::CalcChecksumEnhanced)
        );
        assert_eq!(ChecksumType::from_num(2), None);
        assert_eq!(ChecksumType::default(), ChecksumType::CalcChecksum);
    }

    #[test]
    fn lin_version_values() {
        assert_eq!(LinVersion::V1_3.to_num(), 0);
        assert_eq!(LinVersion::V2_0.to_num(), 1);
        assert_eq!(LinVersion::V2_1.to_num(), 2);
        assert_eq!(LinVersion::from_num(2), Some(LinVersion::V2_1));
        assert_eq!(LinVersion::from_num(3), None);
        assert_eq!(LinVersion::default(), LinVersion::V1_3);
        assert!(LinVersion::V1_3 < LinVersion::V2_1);
    }

    #[test]
    fn lin_baudrate_values() {
        let cases = [
            (LinBaudrate::NotSet, 0u16, "Not set"),
            (LinBaudrate::B1200, 1200, "1200 Baud"),
            (LinBaudrate::B2400, 2400, "2400 Baud"),
            (LinBaudrate::B4800, 4800, "4800 Baud"),
            (LinBaudrate::B9600, 9600, "9600 Baud"),
            (LinBaudrate::B19200, 19200, "19200 Baud"),
        ];
        for (br, num, desc) in cases {
            assert_eq!(br.to_num(), num);
            assert_eq!(LinBaudrate::from_num(num), Some(br));
            assert_eq!(br.description(), desc);
        }
        assert_eq!(LinBaudrate::from_num(1000), None);
    }

    // ---- LinConfiguration ----

    #[test]
    fn lin_configuration_default_matches_parameterless_defaults() {
        let c = LinConfiguration::default();
        assert_eq!(c.master_id, 0);
        assert_eq!(c.channel, 0);
        assert_eq!(c.baudrate, 0);
        assert_eq!(c.version, LinVersion::V1_3);
        assert_eq!(c.dlc, 0);
        assert_eq!(c.checksum_type, ChecksumType::CalcChecksum);
        assert_eq!(c.hardware_type, 0);
        assert_eq!(c.bus_id, None);
    }

    #[test]
    fn lin_configuration_new_matches_default_arguments() {
        let c = LinConfiguration::new(0x3C, 19200);
        assert_eq!(c.master_id, 0x3C);
        assert_eq!(c.baudrate, 19200);
        assert_eq!(c.version, LinVersion::V2_1);
        assert_eq!(c.dlc, 8);
        assert_eq!(c.checksum_type, ChecksumType::CalcChecksum);
        assert_eq!(c.channel, 0);
        let full = LinConfiguration::with_options(
            0x3C,
            19200,
            LinVersion::V2_1,
            8,
            ChecksumType::CalcChecksumEnhanced,
            2,
        );
        assert_eq!(full.checksum_type, ChecksumType::CalcChecksumEnhanced);
        assert_eq!(full.channel, 2);
    }

    #[test]
    fn protected_id_parity() {
        assert_eq!(protected_id(0x22), 0xE2);
        assert_eq!(protected_id(0x3D), 0x7D);
        assert_eq!(protected_id(0x00), 0x80);
        assert_eq!(protected_id(0x3C), 0x3C);
        assert_eq!(protected_id(0x3F), 0xBF);
        assert_eq!(protected_id(0xFF), protected_id(0x3F));
    }

    #[test]
    fn classic_checksum_vectors() {
        assert_eq!(classic_checksum(&[0x12, 0x34]), 0xB9);
        assert_eq!(classic_checksum(&[]), 0xFF);
        assert_eq!(classic_checksum(&[0xFF, 0xFF]), 0x00);
        assert_eq!(
            classic_checksum(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]),
            0xDB
        );
    }

    #[test]
    fn enhanced_checksum_includes_pid() {
        assert_eq!(enhanced_checksum(0xE2, &[0x12, 0x34]), 0xD6);
        assert_eq!(
            compute_checksum(ChecksumType::CalcChecksumEnhanced, 0x22, &[0x12, 0x34]),
            enhanced_checksum(0xE2, &[0x12, 0x34])
        );
        assert_eq!(
            compute_checksum(ChecksumType::CalcChecksum, 0x22, &[0x12, 0x34]),
            classic_checksum(&[0x12, 0x34])
        );
    }

    #[test]
    fn frame_new_and_accessors() {
        let f = master_frame();
        assert_eq!(f.bus_id, "TESTBUS");
        assert_eq!(f.id, 0x22);
        assert_eq!(f.data, vec![0x48, 0x65, 0x6C, 0x22]);
        assert!(f.is_master_frame);
        assert_eq!(f.rw_indicator(), '\u{2192}');
        assert_eq!(f.address(), "→ 22");
        assert_eq!(f.data_str(), "48 65 6C 22");
        assert_eq!(f.data_ascii_str('"'), "Hel.");
        assert_eq!(f.time_str(), "0.000");
    }

    #[test]
    fn frame_with_len_truncates() {
        let mut f = LinFrame::with_len(
            "TESTBUS",
            0x0A,
            vec![0x09, 0x7F, 0x41, 0x00, 0xFF, 0x20],
            4,
            false,
        )
        .unwrap();
        f.elapsed = Duration::ZERO;
        assert_eq!(f.data.len(), 4);
        assert!(!f.is_master_frame);
        assert_eq!(f.rw_indicator(), '\u{2190}');
        assert_eq!(f.address(), "← A");
        assert_eq!(f.data_str(), "09 7F 41 00");
        assert_eq!(f.data_ascii_str('\t'), "..A.");
        let g = LinFrame::with_len("B", 1, vec![1, 2], 2, true).unwrap();
        assert_eq!(g.data, vec![1, 2]);
        assert!(LinFrame::with_len("B", 1, vec![1, 2], 3, true).is_err());
    }

    #[test]
    fn frame_from_u64_little_endian() {
        let f = LinFrame::from_u64("TESTBUS", 0x3D, 0x0102_0304_0506_0708, 8, true);
        assert_eq!(f.data_str(), "08 07 06 05 04 03 02 01");
        let f2 = LinFrame::from_u64("B", 1, 0xAABB, 2, true);
        assert_eq!(f2.data, vec![0xBB, 0xAA]);
        let f3 = LinFrame::from_u64("B", 1, 0x01, 10, true);
        assert_eq!(f3.data.len(), 10);
        assert_eq!(f3.data[8..], [0, 0]);
    }

    #[test]
    fn frame_to_csv_matches_expected_contract() {
        let f = master_frame();
        assert_eq!(
            f.to_csv(),
            "0.000;\"TESTBUS\";\"→ 22\";4;\"48 65 6C 22\";\"Hel.\""
        );
    }

    #[test]
    fn frame_to_clipboard_matches_expected_contract() {
        let f = master_frame();
        assert_eq!(
            f.to_clipboard(),
            "0.000\tTESTBUS\t→ 22\t4\t48 65 6C 22\tHel\""
        );
    }

    #[test]
    fn frame_ascii_replacement_rules() {
        let f = LinFrame::new("B", 1, vec![0x41, 0x00, 0x80, 0x22, 0x09], true);
        assert_eq!(f.data_ascii_str('"'), "A....");
        assert_eq!(f.data_ascii_str('\t'), "A..\".");
    }

    #[test]
    fn frame_time_str_with_offset_and_decimals() {
        let mut f = master_frame();
        f.elapsed = Duration::new(1, 234_567_000);
        assert_eq!(f.time_str_with(2, 0.0), "1.23");
        assert_eq!(f.time_str_with(3, 1.0), "0.235");
        assert_eq!(f.time_str_with(0, 0.0), "1");
    }

    #[test]
    fn encode_header_and_response() {
        let f = master_frame();
        assert_eq!(f.pid(), 0xE2);
        assert_eq!(f.encode_header(), [0x55, 0xE2]);
        let enc = f.encode_response(ChecksumType::CalcChecksumEnhanced);
        assert_eq!(enc.len(), 6);
        assert_eq!(enc[0], 0xE2);
        assert_eq!(&enc[1..5], &[0x48, 0x65, 0x6C, 0x22]);
        assert_eq!(enc[5], f.checksum(ChecksumType::CalcChecksumEnhanced));
    }

    #[test]
    fn decode_response_roundtrip() {
        for ct in [
            ChecksumType::CalcChecksum,
            ChecksumType::CalcChecksumEnhanced,
        ] {
            let f = LinFrame::new("B", 0x11, vec![1, 2, 3, 4, 5, 6, 7, 8], false);
            let enc = f.encode_response(ct);
            let (id, data) = LinFrame::decode_response(&enc, ct).unwrap();
            assert_eq!(id, 0x11);
            assert_eq!(data, f.data);
        }
        let f = LinFrame::new("B", 0x22, vec![], true);
        let (id, data) = LinFrame::decode_response(
            &f.encode_response(ChecksumType::CalcChecksum),
            ChecksumType::CalcChecksum,
        )
        .unwrap();
        assert_eq!(id, 0x22);
        assert!(data.is_empty());
    }

    #[test]
    fn decode_response_errors() {
        assert!(LinFrame::decode_response(&[0xE2], ChecksumType::CalcChecksum).is_err());
        assert!(LinFrame::decode_response(&[0u8; 11], ChecksumType::CalcChecksum).is_err());
        let bad_pid = [0x22, 0x00, 0x00];
        assert!(LinFrame::decode_response(&bad_pid, ChecksumType::CalcChecksum).is_err());
        let f = master_frame();
        let mut enc = f.encode_response(ChecksumType::CalcChecksumEnhanced);
        *enc.last_mut().unwrap() ^= 0xFF;
        assert!(LinFrame::decode_response(&enc, ChecksumType::CalcChecksumEnhanced).is_err());
    }

    #[test]
    fn lin_id_string_matches_expected_contract() {
        let cases = [
            (0u8, "0"),
            (1, "1"),
            (15, "F"),
            (16, "10"),
            (60, "3C"),
            (63, "3F"),
            (255, "FF"),
        ];
        for (id, s) in cases {
            assert_eq!(to_lin_id_string(id), s);
        }
    }

    #[test]
    fn bus_id_rule_matches_expected_contract() {
        assert_eq!(make_bus_id("PROBEASM", 2), "PROBEASM/LIN3");
        assert_eq!(make_bus_id("autors-can", 0), "autors-can/LIN1");
    }

    // ---- LinEventArgs / LinFilterIds ----

    #[test]
    fn event_args_holds_frame() {
        let f = master_frame();
        let e = LinEventArgs::new(f.clone());
        assert_eq!(e.frame, f);
    }

    #[test]
    fn filter_ids_set_semantics() {
        let mut ids = LinFilterIds::new();
        assert!(ids.is_empty());
        assert!(ids.insert(0x22));
        assert!(!ids.insert(0x22));
        assert!(ids.contains(0x22));
        assert!(!ids.contains(0x23));
        assert_eq!(ids.len(), 1);
        let from: LinFilterIds = [0x01, 0x02, 0x02].into_iter().collect();
        assert_eq!(from.len(), 2);
        assert!(LinFilterIds::from_ids(&[0x0A]).contains(0x0A));
    }

    // ---- LinFrameQueue ----

    #[test]
    fn queue_enqueue_and_read_cursors() {
        let q = LinFrameQueue::new();
        assert!(q.enqueue_frame(master_frame(), None));
        assert!(q.enqueue_frame(LinFrame::new("B", 0x23, vec![1], false), None));
        let (unread, total) = q.get_unread_frames();
        assert_eq!(unread.len(), 2);
        assert_eq!(total, 2);
        let (unread, _) = q.get_unread_frames();
        assert!(unread.is_empty());
        q.enqueue_frame(master_frame(), None);
        let (all, cnt) = q.get_frames();
        assert_eq!(all.len(), 3);
        assert_eq!(cnt, 3);
        q.clear();
        let (all, cnt) = q.get_frames();
        assert!(all.is_empty());
        assert_eq!(cnt, 0);
    }

    #[test]
    fn queue_filter_rules() {
        let q = LinFrameQueue::new();
        assert!(q.enqueue_frame(master_frame(), None));
        q.set_filter(LinLogFilter {
            show_master: false,
            ..LinLogFilter::default()
        });
        assert!(!q.enqueue_frame(master_frame(), None));
        assert!(q.enqueue_frame(LinFrame::new("B", 0x22, vec![1], false), None));
        q.set_filter(LinLogFilter {
            show_slave: false,
            id_filter: Some(LinFilterIds::from_ids(&[0x22])),
            ..LinLogFilter::default()
        });
        assert!(q.enqueue_frame(master_frame(), None));
        let mut other = master_frame();
        other.id = 0x23;
        assert!(!q.enqueue_frame(other, None));
        let (all, _) = q.get_frames();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn queue_frame_dict_keying() {
        let q = LinFrameQueue::new();
        q.enqueue_frame(master_frame(), None);
        q.enqueue_frame(master_frame(), Some(7));
        let dict = q.frame_dict();
        assert_eq!(dict.len(), 2);
        assert!(dict.contains_key(&0x22));
        assert!(dict.contains_key(&((7u64 << 32) | 0x22)));
        let mut newer = master_frame();
        newer.data = vec![9];
        q.enqueue_frame(newer.clone(), Some(7));
        assert_eq!(q.frame_dict()[&((7u64 << 32) | 0x22)].data, vec![9]);
        q.clear();
        assert!(q.frame_dict().is_empty());
    }

    #[test]
    fn queue_cap_trims_to_keep() {
        let q = LinFrameQueue::new();
        for i in 0..(FRAME_CAP + 1) {
            q.enqueue_frame(LinFrame::new("B", (i % 64) as u8, vec![], true), None);
        }
        let (all, cnt) = q.get_frames();
        assert_eq!(all.len(), FRAME_CAP_KEEP);
        assert_eq!(cnt, FRAME_CAP_KEEP);
    }

    #[test]
    fn queue_file_logging() {
        let dir = std::env::temp_dir().join(format!("autors_lin_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let q = LinFrameQueue::new();
        q.update_file_log_setting(true, &dir).unwrap();
        q.update_file_log_setting(true, &dir).unwrap();
        q.enqueue_frame(master_frame(), None);
        q.update_file_log_setting(false, &dir).unwrap();
        let content = std::fs::read_to_string(dir.join(LinFrameQueue::LOG_FILE_NAME)).unwrap();
        let mut lines = content.lines();
        assert_eq!(lines.next(), Some(LinFrameQueue::LOG_FILE_CSV_HEADER));
        assert_eq!(lines.next(), Some(master_frame().to_csv().as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- LinDevice trait ----

    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingDevice;

    fn _assert_object_safe(_: &dyn LinDevice) {}

    struct MockDevice {
        bus_id: i32,
        opened: Option<LinConfiguration>,
        rx: VecDeque<LinFrame>,
        closed: bool,
    }

    impl MockDevice {
        fn new() -> Self {
            Self {
                bus_id: 3,
                opened: None,
                rx: VecDeque::new(),
                closed: false,
            }
        }
    }

    #[async_trait]
    impl LinDevice for MockDevice {
        fn unique_bus_id(&self) -> i32 {
            self.bus_id
        }
        fn is_available(&self) -> bool {
            true
        }
        async fn open(&mut self, config: &LinConfiguration) -> Result<bool> {
            let mut cfg = config.clone();
            cfg.bus_id = Some(make_bus_id("MOCK", config.channel));
            self.opened = Some(cfg);
            Ok(true)
        }
        async fn send(&mut self, _id: u8, data: &[u8]) -> Result<usize> {
            Ok(data.len())
        }
        async fn on_receive(&mut self) -> Result<Option<LinFrame>> {
            Ok(self.rx.pop_front())
        }
        async fn close(&mut self) {
            self.closed = true;
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn device_trait_contract() {
        let mut p = BlockingDevice::new(MockDevice::new());
        _assert_object_safe(&p.0);
        assert!(p.is_available());
        assert_eq!(p.unique_bus_id(), 3);
        let cfg = LinConfiguration::new(0x3C, 19200);
        assert!(p.open(&cfg).unwrap());
        let opened = p.0.opened.as_ref().unwrap();
        assert_eq!(opened.bus_id.as_deref(), Some("MOCK/LIN1"));
        assert_eq!(opened.checksum_type, ChecksumType::CalcChecksum);
        assert_eq!(p.send(0x22, &[1, 2, 3]).unwrap(), 3);
        assert!(p.on_receive().unwrap().is_none());
        p.0.rx.push_back(master_frame());
        let got = p.on_receive().unwrap().unwrap();
        assert_eq!(got.id, 0x22);
        assert_eq!(p.has_sent(master_frame()), 4);
        p.close();
        assert!(p.0.closed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_send_receive_loopback() {
        let mut p = MockDevice::new();
        let cfg = LinConfiguration::new(0x3C, 19200);
        assert!(p.open(&cfg).await.unwrap());
        assert_eq!(p.send(0x22, &[1, 2, 3]).await.unwrap(), 3);

        p.rx.push_back(master_frame());
        let frame = p.on_receive().await.unwrap().expect("expected a frame");
        assert_eq!(frame.id, 0x22);
        assert!(p.on_receive().await.unwrap().is_none());

        p.close().await;
        assert!(p.closed);

        let mut boxed: Box<dyn LinDevice + Send> = Box::new(MockDevice::new());
        _assert_object_safe(&*boxed);
        assert!(boxed.open(&cfg).await.unwrap());
        assert_eq!(boxed.send(0x22, &[1]).await.unwrap(), 1);
        assert!(boxed.on_receive().await.unwrap().is_none());
        boxed.close().await;
    }
}
