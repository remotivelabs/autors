//! CAN frame and configuration types.
//! Core types: `CanFrame` (a timestamped CAN frame with bus ID, 11/29-bit ID,
//! frame type, payload and direction flag), `FrameType` (classic/FD/BRS bit
//! flags), `CanBaudrate`/`CanFdBaudrate` (nominal and CAN FD data-phase bit
//! rates), `CanStandard`, `CanConfiguration` (channel configuration),
//! `CanFilterIds`, and `J1939Frame` (a J1939 view over a `CanFrame`).
//! Frames encode to/from little-endian `u64`/byte slices and render as CSV or
//! clipboard (tab-separated) text; the exact output formats are part of the
//! behavioral contract and are pinned down by the unit tests.
//! Behavioral notes (details at each item):
//! - Frame timestamps (`elapsed`) are process-relative: a process-wide static
//!   start [`std::time::Instant`] is captured on first use.
//! - `time_str` always renders three decimal places.

use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::device::{to_can_id_string, CAN_EXT_FLAG};
use crate::error::{Error, Result};

/// Process-relative timestamp source: time since process start.
pub(crate) fn now_elapsed() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

// ---------------------------------------------------------------------------
// Baud rate enumerations
// ---------------------------------------------------------------------------

/// Nominal CAN bit rate.
/// The enum value is the nominal bit rate in bit/s; `NotSet` = 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum CanBaudrate {
    /// Not set.
    #[default]
    NotSet = 0,
    /// 10 kBit/s.
    B10Kbit = 10_000,
    /// 20 kBit/s.
    B20Kbit = 20_000,
    /// 50 kBit/s.
    B50Kbit = 50_000,
    /// 100 kBit/s.
    B100Kbit = 100_000,
    /// 125 kBit/s.
    B125Kbit = 125_000,
    /// 250 kBit/s.
    B250Kbit = 250_000,
    /// 500 kBit/s.
    B500Kbit = 500_000,
    /// 800 kBit/s.
    B800Kbit = 800_000,
    /// 1 MBit/s.
    B1Mbit = 1_000_000,
    /// 2 MBit/s.
    B2Mbit = 2_000_000,
    /// 4 MBit/s.
    B4Mbit = 4_000_000,
    /// 5 MBit/s.
    B5Mbit = 5_000_000,
    /// 8 MBit/s.
    B8Mbit = 8_000_000,
    /// 10 MBit/s.
    B10Mbit = 10_000_000,
}

impl CanBaudrate {
    /// The bit rate in bit/s (the enum's underlying integer value).
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Maps a bit/s value to a variant; unknown values return `None` (only
    /// known values are accepted).
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::NotSet),
            10_000 => Some(Self::B10Kbit),
            20_000 => Some(Self::B20Kbit),
            50_000 => Some(Self::B50Kbit),
            100_000 => Some(Self::B100Kbit),
            125_000 => Some(Self::B125Kbit),
            250_000 => Some(Self::B250Kbit),
            500_000 => Some(Self::B500Kbit),
            800_000 => Some(Self::B800Kbit),
            1_000_000 => Some(Self::B1Mbit),
            2_000_000 => Some(Self::B2Mbit),
            4_000_000 => Some(Self::B4Mbit),
            5_000_000 => Some(Self::B5Mbit),
            8_000_000 => Some(Self::B8Mbit),
            10_000_000 => Some(Self::B10Mbit),
            _ => None,
        }
    }

    /// Legacy-style display name (e.g. `_500kBit`), used for textual bit-rate
    /// output.
    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::NotSet => "NotSet",
            Self::B10Kbit => "_10kBit",
            Self::B20Kbit => "_20kBit",
            Self::B50Kbit => "_50kBit",
            Self::B100Kbit => "_100kBit",
            Self::B125Kbit => "_125kBit",
            Self::B250Kbit => "_250kBit",
            Self::B500Kbit => "_500kBit",
            Self::B800Kbit => "_800kBit",
            Self::B1Mbit => "_1MBit",
            Self::B2Mbit => "_2MBit",
            Self::B4Mbit => "_4MBit",
            Self::B5Mbit => "_5MBit",
            Self::B8Mbit => "_8MBit",
            Self::B10Mbit => "_10MBit",
        }
    }
}

impl fmt::Display for CanBaudrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

/// CAN FD data-phase bit rate.
/// The enum value is the data-phase bit rate in bit/s; `NotUsed` = 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum CanFdBaudrate {
    /// CAN FD not used.
    #[default]
    NotUsed = 0,
    /// 500 kBit/s.
    B500Kbit = 500_000,
    /// 1 MBit/s.
    B1Mbit = 1_000_000,
    /// 2 MBit/s.
    B2Mbit = 2_000_000,
    /// 4 MBit/s.
    B4Mbit = 4_000_000,
    /// 5 MBit/s.
    B5Mbit = 5_000_000,
    /// 8 MBit/s.
    B8Mbit = 8_000_000,
    /// 10 MBit/s.
    B10Mbit = 10_000_000,
}

impl CanFdBaudrate {
    /// The bit rate in bit/s.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Maps a bit/s value to a variant; unknown values return `None`.
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::NotUsed),
            500_000 => Some(Self::B500Kbit),
            1_000_000 => Some(Self::B1Mbit),
            2_000_000 => Some(Self::B2Mbit),
            4_000_000 => Some(Self::B4Mbit),
            5_000_000 => Some(Self::B5Mbit),
            8_000_000 => Some(Self::B8Mbit),
            10_000_000 => Some(Self::B10Mbit),
            _ => None,
        }
    }

    /// Legacy-style display name (e.g. `_2MBit`).
    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::NotUsed => "NotUsed",
            Self::B500Kbit => "_500kBit",
            Self::B1Mbit => "_1MBit",
            Self::B2Mbit => "_2MBit",
            Self::B4Mbit => "_4MBit",
            Self::B5Mbit => "_5MBit",
            Self::B8Mbit => "_8MBit",
            Self::B10Mbit => "_10MBit",
        }
    }
}

impl fmt::Display for CanFdBaudrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

/// CAN standard mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CanStandard {
    /// CAN 2.0A+B (default).
    #[default]
    Can20AB,
    /// Standard (11-bit ID) frames only.
    Can20A,
    /// Extended (29-bit ID) frames only.
    Can20B,
}

/// Frame type as a set of bit flags.
/// Although the constants look like a plain enum (`CAN20B` = 0, `FD` = 1,
/// `BRS` = 2), they are combined bitwise in practice: the default frame type
/// for transmission is 3 = FD|BRS, and implementations test individual bits
/// with `type & FD`. Hence the bit-flag newtype; `FrameType::FD_BRS` is the
/// combined value 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameType(pub u8);

impl FrameType {
    /// Classic CAN 2.0B frame.
    pub const CAN20B: Self = Self(0);
    /// CAN FD frame.
    pub const FD: Self = Self(1);
    /// CAN FD frame with bit rate switching (BRS) in the data phase.
    pub const BRS: Self = Self(2);
    /// FD + BRS (combined value 3, the default for transmission).
    pub const FD_BRS: Self = Self(3);

    /// The raw bit value.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether all bits of `other` are set in `self` (i.e. `(self & other) == other`).
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether this is a classic CAN frame (no FD bits set).
    pub const fn is_classic(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for FrameType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for FrameType {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Set of CAN IDs accepted by a receive filter.
pub type CanFilterIds = std::collections::HashSet<u32>;

// ---------------------------------------------------------------------------
// CANConfiguration
// ---------------------------------------------------------------------------

/// CAN (FD) bit timing parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BitRatePar {
    /// Baud rate prescaler (BRP).
    pub brp: i32,
    /// Time segment 1 (propagation segment + phase segment 1).
    pub tseg1: i32,
    /// Time segment 2 (phase segment 2).
    pub tseg2: i32,
    /// Synchronization jump width.
    pub sjw: i32,
}

/// Full CAN FD bit timing configuration (nominal + data phase).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BitRateConfig {
    /// Controller clock (Hz); 0 = not set.
    pub clock: i32,
    /// Nominal (arbitration phase) bit timing.
    pub nominal: BitRatePar,
    /// Data-phase bit timing.
    pub data: BitRatePar,
    /// Whether to use non-ISO CAN FD.
    pub non_iso: bool,
}

/// Channel configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct CanConfiguration {
    /// Channel number (0-based).
    pub channel: i32,
    /// CAN standard (2.0A/2.0B).
    pub mode: CanStandard,
    /// Nominal bit rate.
    pub baudrate: CanBaudrate,
    /// CAN FD data-phase bit rate.
    pub baudrate_fd: CanFdBaudrate,
    /// Hardware type (vendor-specific number).
    pub hardware_type: i32,
    /// Full CAN FD bit timing configuration; takes precedence over
    /// `baudrate`/`baudrate_fd` when set.
    pub fd_bit_rate_config: Option<BitRateConfig>,
    /// Bus identifier (assigned by the device on `open`, e.g. `Kvaser/CAN1`).
    pub bus_id: Option<String>,
    /// Serial port name (ELM327/Lawicel-style devices).
    pub com_port: Option<String>,
    /// Serial port parameters, default `9600,8,N,1`.
    pub com_port_config: String,
    /// ELM327 protocol number (6 = ISO 15765-4 CAN 11/500; set by the
    /// caller's constructor).
    pub elm327_protocol: i32,
    /// ELM327 monitor mode.
    pub elm327_monitor: bool,
    /// ELM327 command timeout (ms), default 200.
    pub elm327_cmd_timeout: i32,
    /// Transmit timeout (ms).
    pub tx_timeout: i32,
}

impl Default for CanConfiguration {
    fn default() -> Self {
        Self {
            channel: 0,
            mode: CanStandard::default(),
            baudrate: CanBaudrate::default(),
            baudrate_fd: CanFdBaudrate::default(),
            hardware_type: 0,
            fd_bit_rate_config: None,
            bus_id: None,
            com_port: None,
            com_port_config: "9600,8,N,1".to_string(),
            elm327_protocol: 0,
            elm327_monitor: false,
            elm327_cmd_timeout: 200,
            tx_timeout: 0,
        }
    }
}

impl CanConfiguration {
    /// Creates a configuration from a channel number and bit rates.
    pub fn new(channel: i32, baudrate: CanBaudrate, baudrate_fd: CanFdBaudrate) -> Self {
        Self {
            channel,
            baudrate,
            baudrate_fd,
            ..Self::default()
        }
    }

    /// Creates a configuration from a full bit timing configuration.
    pub fn with_bit_rate_config(channel: i32, cfg: BitRateConfig) -> Self {
        Self {
            channel,
            fd_bit_rate_config: Some(cfg),
            ..Self::default()
        }
    }

    /// Creates a serial-port (ELM327) configuration.
    pub fn with_com_port(
        com_port: impl Into<String>,
        monitor: bool,
        cmd_timeout: i32,
        elm327_protocol: i32,
    ) -> Self {
        Self {
            com_port: Some(com_port.into()),
            elm327_monitor: monitor,
            elm327_cmd_timeout: cmd_timeout,
            elm327_protocol,
            ..Self::default()
        }
    }

    /// Whether CAN FD is requested (data-phase bit rate or full bit timing
    /// configuration).
    pub fn is_fd(&self) -> bool {
        self.baudrate_fd != CanFdBaudrate::NotUsed || self.fd_bit_rate_config.is_some()
    }

    /// Renders the bit rate as text.
    /// Without FD the output is just the nominal bit rate name; with FD it is
    /// `"{data} ({nominal} Arbitration)"`; with a full bit timing
    /// configuration the same format is filled with `data.brp` /
    /// `nominal.brp`.
    pub fn bit_rate_str(&self) -> String {
        match &self.fd_bit_rate_config {
            None => {
                if self.baudrate_fd == CanFdBaudrate::NotUsed {
                    self.baudrate.cs_name().to_string()
                } else {
                    format!(
                        "{} ({} Arbitration)",
                        self.baudrate_fd.cs_name(),
                        self.baudrate.cs_name()
                    )
                }
            }
            Some(cfg) => format!("{} ({} Arbitration)", cfg.data.brp, cfg.nominal.brp),
        }
    }
}

// ---------------------------------------------------------------------------
// CANFrame
// ---------------------------------------------------------------------------

/// A timestamped CAN frame.
/// The top bit of `id` (`CAN_EXT_FLAG`) marks a 29-bit extended ID.
#[derive(Debug, Clone)]
pub struct CanFrame {
    /// Bus identifier.
    pub bus_id: String,
    /// CAN ID (including the `CAN_EXT_FLAG` extended-ID bit).
    pub id: u32,
    /// Frame type (classic/FD/BRS).
    pub frame_type: FrameType,
    /// Payload.
    pub data: Vec<u8>,
    /// Frame timestamp: time since process start, captured at construction.
    pub elapsed: Duration,
    /// Whether this frame was sent by the master (false = received from the bus).
    pub is_master_frame: bool,
}

impl CanFrame {
    /// Creates a frame; the timestamp is captured now.
    pub fn new(
        bus_id: impl Into<String>,
        can_id: u32,
        data: Vec<u8>,
        is_master_frame: bool,
        frame_type: FrameType,
    ) -> Self {
        Self {
            bus_id: bus_id.into(),
            id: can_id,
            frame_type,
            data,
            elapsed: now_elapsed(),
            is_master_frame,
        }
    }

    /// Creates a frame, truncating `data` to `len` bytes.
    /// Returns `Error::Invalid` when `len > data.len()`.
    pub fn with_len(
        bus_id: impl Into<String>,
        can_id: u32,
        mut data: Vec<u8>,
        len: usize,
        is_master_frame: bool,
        frame_type: FrameType,
    ) -> Result<Self> {
        if len > data.len() {
            return Err(Error::Invalid(format!(
                "len {len} exceeds data length {}",
                data.len()
            )));
        }
        data.truncate(len);
        Ok(Self::new(bus_id, can_id, data, is_master_frame, frame_type))
    }

    /// Creates a classic CAN 2.0B frame from `data`, split little-endian into
    /// `len` bytes.
    pub fn from_u64_le(
        bus_id: impl Into<String>,
        can_id: u32,
        data: u64,
        len: usize,
        is_master_frame: bool,
    ) -> Self {
        let bytes = data.to_le_bytes();
        Self::new(
            bus_id,
            can_id,
            bytes[..len.min(8)].to_vec(),
            is_master_frame,
            FrameType::CAN20B,
        )
    }

    /// The CAN ID without the extended-ID flag bit.
    pub fn raw_id(&self) -> u32 {
        self.id & 0x7FFF_FFFF
    }

    /// Whether the frame carries a 29-bit extended ID.
    pub fn is_extended_id(&self) -> bool {
        self.id & CAN_EXT_FLAG != 0
    }

    /// Direction indicator: `→` for master frames, `←` for frames received
    /// from the bus.
    pub fn rw_indicator(&self) -> char {
        if self.is_master_frame {
            '\u{2192}'
        } else {
            '\u{2190}'
        }
    }

    /// `"{rw} {id}"` (e.g. `→ 123(X)`).
    pub fn address(&self) -> String {
        format!("{} {}", self.rw_indicator(), to_can_id_string(self.id))
    }

    /// Approximate number of bits of the frame on the wire.
    pub fn raw_frame_length(&self) -> usize {
        Self::raw_frame_length_for(self.id, self.data.len())
    }

    /// Approximate wire length for a given ID/payload: extended frames are
    /// 64 + data bits, standard frames 44 + data bits.
    pub fn raw_frame_length_for(can_id: u32, data_len: usize) -> usize {
        (if can_id & CAN_EXT_FLAG != 0 { 64 } else { 44 }) + data_len * 8
    }

    /// Uppercase hex payload, space-separated (e.g. `11 22 33`).
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

    /// Printable ASCII (0x20–0x7F, not a control character, not
    /// `not_allowed_char`) rendered as-is; everything else becomes `.`.
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

    /// Timestamp text: `elapsed - offset` in seconds, always with three
    /// decimal places.
    pub fn time_str(&self, offset: f64) -> String {
        format!("{:.3}", self.elapsed.as_secs_f64() - offset)
    }

    /// CSV row: `{time};"{bus}";"{addr}";{len};"{data}";"{ascii}"`.
    pub fn to_csv(&self) -> String {
        format!(
            "{};\"{}\";\"{}\";{};\"{}\";\"{}\"",
            self.time_str(0.0),
            self.bus_id,
            self.address(),
            self.data.len(),
            self.data_str(),
            self.data_ascii_str('"')
        )
    }

    /// Tab-separated row without quotes.
    pub fn to_clipboard(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            self.time_str(0.0),
            self.bus_id,
            self.address(),
            self.data.len(),
            self.data_str(),
            self.data_ascii_str('\t')
        )
    }
}

// ---------------------------------------------------------------------------
// J1939
// ---------------------------------------------------------------------------

/// J1939 PDU format (derived from the PF field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum J1939PduFormat {
    /// PDU1 (PF < 240, point-to-point).
    #[default]
    Pdu1,
    /// PDU2 (PF >= 240, broadcast).
    Pdu2,
}

impl fmt::Display for J1939PduFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pdu1 => "PDU1",
            Self::Pdu2 => "PDU2",
        })
    }
}

/// J1939 view over a `CanFrame`.
/// Note: construction resets the timestamp — the embedded frame's `elapsed`
/// is set to the current time, so the source frame's timestamp is not kept.
#[derive(Debug, Clone)]
pub struct J1939Frame {
    /// The embedded CAN frame.
    pub frame: CanFrame,
}

impl J1939Frame {
    /// Copies the frame's fields and resets the timestamp to now.
    pub fn new(frame: &CanFrame) -> Self {
        let mut frame = frame.clone();
        frame.elapsed = now_elapsed();
        Self { frame }
    }

    /// J1939 priority field.
    pub fn priority(&self) -> u8 {
        ((self.frame.id >> 26) & 7) as u8
    }

    /// Extended data page bit.
    pub fn ex_data_page(&self) -> u8 {
        ((self.frame.id >> 25) & 1) as u8
    }

    /// Data page bit.
    pub fn data_page(&self) -> u8 {
        ((self.frame.id >> 24) & 1) as u8
    }

    /// PDU format: PF >= 240 means PDU2.
    pub fn pdu_format(&self) -> J1939PduFormat {
        if ((self.frame.id >> 16) & 0xFF) >= 240 {
            J1939PduFormat::Pdu2
        } else {
            J1939PduFormat::Pdu1
        }
    }

    /// PDU specific field.
    pub fn pdu_specific(&self) -> u8 {
        ((self.frame.id >> 8) & 0xFF) as u8
    }

    /// Parameter Group Number (PGN).
    pub fn pgn(&self) -> u32 {
        (self.frame.id >> 8) & 0x3FFFF
    }

    /// Source address.
    pub fn source_address(&self) -> u8 {
        (self.frame.id & 0xFF) as u8
    }

    /// CSV row: the ID is rendered as uppercase hex masked to 29 bits (no
    /// `(X)` marker), `PDUFormat` is quoted, PGN/source address are decimal.
    pub fn to_csv(&self) -> String {
        format!(
            "{};\"{}\";\"{:X}\";{};{};\"{}\";{};{};{};{};\"{}\";\"{}\"",
            self.frame.time_str(0.0),
            self.frame.bus_id,
            self.frame.id & 0x1FFF_FFFF,
            self.priority(),
            self.data_page(),
            self.pdu_format(),
            self.pdu_specific(),
            self.pgn(),
            self.source_address(),
            self.frame.data.len(),
            self.frame.data_str(),
            self.frame.data_ascii_str('"')
        )
    }

    /// Tab-separated row without quotes.
    /// Intentional quirk, kept as part of the behavioral contract: the format
    /// emits `source_address` twice and skips the data length entirely
    /// (e.g. `...|35|35|<data>|<ascii>`).
    pub fn to_clipboard(&self) -> String {
        format!(
            "{}\t{}\t{:X}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.frame.time_str(0.0),
            self.frame.bus_id,
            self.frame.id & 0x1FFF_FFFF,
            self.priority(),
            self.data_page(),
            self.pdu_format(),
            self.pdu_specific(),
            self.pgn(),
            self.source_address(),
            self.source_address(), // deliberate: the data-length slot repeats the source address
            self.frame.data_str(),
            self.frame.data_ascii_str('\t')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample payload used across the format tests (includes '"' and
    /// non-printable bytes).
    const SAMPLE: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x41, 0x42, 0x08];

    fn sample_frame() -> CanFrame {
        let mut f = CanFrame::new("TESTBUS", 0x123, SAMPLE.to_vec(), true, FrameType::CAN20B);
        f.elapsed = Duration::from_millis(250);
        f
    }

    #[test]
    fn baudrate_enum_mapping() {
        assert_eq!(CanBaudrate::B500Kbit.as_u32(), 500_000);
        assert_eq!(CanBaudrate::from_u32(500_000), Some(CanBaudrate::B500Kbit));
        assert_eq!(CanBaudrate::from_u32(125_000), Some(CanBaudrate::B125Kbit));
        assert_eq!(CanBaudrate::from_u32(0), Some(CanBaudrate::NotSet));
        assert_eq!(CanBaudrate::from_u32(42_000), None);
        assert_eq!(CanBaudrate::default(), CanBaudrate::NotSet);
        assert_eq!(CanBaudrate::B500Kbit.cs_name(), "_500kBit");
        assert_eq!(CanBaudrate::B500Kbit.to_string(), "_500kBit");

        assert_eq!(CanFdBaudrate::B2Mbit.as_u32(), 2_000_000);
        assert_eq!(
            CanFdBaudrate::from_u32(2_000_000),
            Some(CanFdBaudrate::B2Mbit)
        );
        assert_eq!(CanFdBaudrate::from_u32(125_000), None);
        assert_eq!(CanFdBaudrate::default(), CanFdBaudrate::NotUsed);
        assert_eq!(CanFdBaudrate::B2Mbit.cs_name(), "_2MBit");
    }

    #[test]
    fn frame_type_flags() {
        assert!(FrameType::CAN20B.is_classic());
        assert!(!FrameType::FD.is_classic());
        // Combined default for transmission: FD|BRS = 3
        assert_eq!(FrameType::FD_BRS.bits(), 3);
        assert!(FrameType::FD_BRS.contains(FrameType::FD));
        assert!(FrameType::FD_BRS.contains(FrameType::BRS));
        assert!(!FrameType::CAN20B.contains(FrameType::FD));
        assert!(FrameType::FD_BRS > FrameType::CAN20B);
        assert_eq!((FrameType::FD | FrameType::BRS), FrameType::FD_BRS);
        assert_eq!((FrameType::FD_BRS & FrameType::FD), FrameType::FD);
    }

    #[test]
    fn configuration_defaults() {
        let c = CanConfiguration::default();
        assert_eq!(c.com_port_config, "9600,8,N,1"); // documented default value
        assert_eq!(c.elm327_cmd_timeout, 200);
        assert_eq!(c.channel, 0);
        assert!(!c.is_fd());

        let fd = CanConfiguration::new(1, CanBaudrate::B500Kbit, CanFdBaudrate::B2Mbit);
        assert!(fd.is_fd());
        let com = CanConfiguration::with_com_port("COM3", true, 200, 6);
        assert_eq!(com.com_port.as_deref(), Some("COM3"));
        assert_eq!(com.elm327_protocol, 6);
    }

    #[test]
    fn bit_rate_str_matches_expected_contract() {
        // The expected values below pin the exact output format of bit_rate_str.
        let c = CanConfiguration {
            baudrate: CanBaudrate::B500Kbit,
            ..CanConfiguration::default()
        };
        assert_eq!(c.bit_rate_str(), "_500kBit");

        let c = CanConfiguration {
            baudrate: CanBaudrate::B500Kbit,
            baudrate_fd: CanFdBaudrate::B2Mbit,
            ..CanConfiguration::default()
        };
        assert_eq!(c.bit_rate_str(), "_2MBit (_500kBit Arbitration)");

        let c2 = CanConfiguration::with_bit_rate_config(
            0,
            BitRateConfig {
                clock: 0,
                nominal: BitRatePar {
                    brp: 2,
                    tseg1: 63,
                    tseg2: 16,
                    sjw: 16,
                },
                data: BitRatePar {
                    brp: 1,
                    tseg1: 29,
                    tseg2: 10,
                    sjw: 10,
                },
                non_iso: false,
            },
        );
        assert_eq!(c2.bit_rate_str(), "1 (2 Arbitration)");
    }

    #[test]
    fn frame_u64_codec_roundtrip() {
        // u64 little-endian codec: 0x1122334455667788 -> [88 77 66 55 44 33 22 11]
        let f = CanFrame::from_u64_le("B", 0x456, 0x1122_3344_5566_7788, 8, true);
        assert_eq!(f.data, vec![0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
        assert_eq!(f.frame_type, FrameType::CAN20B);
        assert_eq!(
            crate::device::data_from_array(&f.data),
            0x1122_3344_5566_7788
        );

        // data_from_array(SAMPLE) = 0x0842415544332211
        assert_eq!(
            crate::device::data_from_array(&SAMPLE),
            0x0842_4155_4433_2211
        );

        let short = CanFrame::from_u64_le("B", 1, 0xABCD, 2, false);
        assert_eq!(short.data, vec![0xCD, 0xAB]);
    }

    #[test]
    fn with_len_truncates_or_errors() {
        let f =
            CanFrame::with_len("B", 0x456, SAMPLE.to_vec(), 5, true, FrameType::CAN20B).unwrap();
        assert_eq!(f.data, SAMPLE[..5]);
        let same =
            CanFrame::with_len("B", 0x456, SAMPLE.to_vec(), 8, true, FrameType::CAN20B).unwrap();
        assert_eq!(same.data, SAMPLE);
        assert!(
            CanFrame::with_len("B", 0x456, SAMPLE.to_vec(), 9, true, FrameType::CAN20B).is_err()
        );
    }

    #[test]
    fn frame_properties_match_expected_contract() {
        let f = sample_frame();
        assert!(f.is_master_frame);
        assert!(!f.is_extended_id());
        assert_eq!(f.raw_id(), 0x123);
        assert_eq!(f.rw_indicator(), '\u{2192}');
        // address() = "→ 123"
        assert_eq!(f.address(), "\u{2192} 123");
        // raw_frame_length(std, 8) = 108
        assert_eq!(f.raw_frame_length(), 108);
        // raw_frame_length(ext, 8) = 128
        assert_eq!(CanFrame::raw_frame_length_for(0x8000_0123, 8), 128);

        let ext = CanFrame::new(
            "TESTBUS",
            0x8000_0123,
            SAMPLE.to_vec(),
            false,
            FrameType::FD,
        );
        assert!(ext.is_extended_id());
        assert_eq!(ext.raw_id(), 0x123);
        assert_eq!(ext.rw_indicator(), '\u{2190}');
        assert_eq!(ext.address(), "\u{2190} 123(X)");
    }

    #[test]
    fn frame_text_formats_match_expected_contract() {
        // All expected values pin the exact output format.
        let f = sample_frame();
        assert_eq!(f.data_str(), "11 22 33 44 55 41 42 08");
        assert_eq!(f.data_ascii_str('"'), "..3DUAB.");
        assert_eq!(f.time_str(0.0), "0.250");
        assert_eq!(
            f.to_csv(),
            "0.250;\"TESTBUS\";\"\u{2192} 123\";8;\"11 22 33 44 55 41 42 08\";\"..3DUAB.\""
        );

        let mut ext = CanFrame::new(
            "TESTBUS",
            0x8000_0123,
            SAMPLE.to_vec(),
            false,
            FrameType::FD,
        );
        ext.elapsed = Duration::from_millis(1500);
        assert_eq!(
            ext.to_csv(),
            "1.500;\"TESTBUS\";\"\u{2190} 123(X)\";8;\"11 22 33 44 55 41 42 08\";\"..3DUAB.\""
        );
        assert_eq!(
            ext.to_clipboard(),
            "1.500\tTESTBUS\t\u{2190} 123(X)\t8\t11 22 33 44 55 41 42 08\t.\"3DUAB."
        );
    }

    #[test]
    fn ascii_str_rules() {
        let f = CanFrame::new(
            "B",
            1,
            vec![0x00, 0x1F, 0x20, 0x7E, 0x7F, 0x80, b'X'],
            false,
            FrameType::CAN20B,
        );
        // control characters -> '.'; 0x20/0x7E printable; 0x7F (DEL, a control char) -> '.'; >0x7F -> '.'
        assert_eq!(f.data_ascii_str('\0'), ".. ~..X");
        // the not_allowed_char itself is also replaced
        let g = CanFrame::new("B", 1, b"aXb".to_vec(), false, FrameType::CAN20B);
        assert_eq!(g.data_ascii_str('X'), "a.b");
        assert_eq!(
            CanFrame::new("B", 1, vec![], false, FrameType::CAN20B).data_str(),
            ""
        );
    }

    #[test]
    fn j1939_properties_match_expected_contract() {
        // For ID 0x80000123: Priority=0, EDP=0, DP=0, PDU1, PS=1, PGN=1, SA=0x23
        let frame = CanFrame::new(
            "TESTBUS",
            0x8000_0123,
            SAMPLE.to_vec(),
            false,
            FrameType::CAN20B,
        );
        let jf = J1939Frame::new(&frame);
        assert_eq!(jf.priority(), 0);
        assert_eq!(jf.ex_data_page(), 0);
        assert_eq!(jf.data_page(), 0);
        assert_eq!(jf.pdu_format(), J1939PduFormat::Pdu1);
        assert_eq!(jf.pdu_specific(), 1);
        assert_eq!(jf.pgn(), 1);
        assert_eq!(jf.source_address(), 0x23);

        // PDU2 boundary: PF (bits 16-23) = 0xF0 = 240
        let pdu2 = CanFrame::new("B", 0x80F0_0123, vec![], false, FrameType::CAN20B);
        assert_eq!(J1939Frame::new(&pdu2).pdu_format(), J1939PduFormat::Pdu2);
        // PF = 0xEF = 239 < 240 -> PDU1
        let pdu1 = CanFrame::new("B", 0x80EF_0123, vec![], false, FrameType::CAN20B);
        assert_eq!(J1939Frame::new(&pdu1).pdu_format(), J1939PduFormat::Pdu1);
    }

    #[test]
    fn j1939_text_formats_match_expected_contract() {
        // The J1939Frame constructor resets the timestamp; here it is assigned
        // directly to make the output deterministic.
        let frame = CanFrame::new(
            "TESTBUS",
            0x8000_0123,
            SAMPLE.to_vec(),
            false,
            FrameType::CAN20B,
        );
        let mut jf = J1939Frame::new(&frame);
        jf.frame.elapsed = Duration::ZERO;
        assert_eq!(
            jf.to_csv(),
            "0.000;\"TESTBUS\";\"123\";0;0;\"PDU1\";1;1;35;8;\"11 22 33 44 55 41 42 08\";\"..3DUAB.\""
        );
        assert_eq!(
            jf.to_clipboard(),
            "0.000\tTESTBUS\t123\t0\t0\tPDU1\t1\t1\t35\t35\t11 22 33 44 55 41 42 08\t.\"3DUAB."
        );
    }
}
