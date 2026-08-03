//! XCP protocol engine: byte-level command/response encoding and decoding, frames and
//! receive buffering, SxI framing, and the XCP master state machine.
//! This module is an XCP-on-CAN master implementation (XCP on CAN 1.x, including some
//! 1.5 commands: DTO_CTR_PROPERTIES/TIME_CORRELATION_PROPERTIES/WRITE_DAQ_MULTIPLE).
//! Structure:
//! - The full set of enums (`CommandCode`/`CmdResult`/`EventCodes` etc.); `[Flags]`-style
//!   enums are modeled as bitflag newtypes, following the autors-can `FrameType` precedent.
//! - Command structs (`XcpCommand` and 40+ implementations) carry a `cmd_code`
//!   field and apply byte order through the `swap` parameter of
//!   `XcpCommand::encode`.
//! - Response/event structs (`XcpResponse` and 30+ implementations) apply byte
//!   order through the `swap` parameter of
//!   `XcpResponse::decode`.
//! - `XcpFrame` and `XcpReceiveBuffer`.
//! - SxI: `SxiCore`/`SxiDevice`/`SerialPortDevice` (serial-port hardware IO is
//!   abstracted behind `SxiSerialIo`).
//! - `XcpTransport` (transport base trait) and `CanXcpTransport` (XCP-on-CAN transport).
//! - UDP/TCP transports live in `crate::eth_transport`; the SxI serial-port adapter and
//!   the SxI-to-XCP transport bridge live in `crate::sxi_serial`.
//! - `XcpMasterBase`/`XcpMaster`, using `autors_comm::base::CommMaster` for
//!   shared communication state.
//!
//! Notable design decisions (see also the per-item comments):
//! - There is no USB transport; `XcpTransport` is an open trait that external code can
//!   implement. The Ethernet constructors only parse IP addresses and do not perform
//!   DNS resolution (std has no DNS).
//! - There is no global frame-logging facility; the `XCPPreventBlockMode`/
//!   `RespectOptionalCmds` options are plain boolean fields on the master struct.
//! - Background receive threads are replaced by polling:
//!   `SerialPortDevice::poll_once` / `XcpTransport::poll`.
//! - Events (event received / error received / service request received /
//!   connection state changed / values received) are exposed as callback fields.
//! - `RespGetCommModeInfo::to_string` reproduces a known quirk in its Interleaved
//!   branch: the format string only has `{0}`, so it outputs MaxBS rather than QueueSize.
//! - The command exchange is handled by `XcpMasterBase::exchange`; event/service/DAQ
//!   frames arriving while a command is awaited are queued and dispatched by
//!   `XcpMaster::drain_pending` after the command returns.
//!
//! Depends on types from `autors_comm::base`: `autors_comm::base::CommMaster`,
//! `Frame`, `DaqList`/`OdtEntry`/`OdtList`/`DaqCache`/`DaqMeasurement`,
//! `DaqClock`, `ProgressArgs`/`ProgressCallback`, `StartStopMode`, `ProgramClearMode`/
//! `ProgramVerifyMode`, `XcpPrgParams`, `ConnectBehaviourType`. The `SeedKeyProvider`
//! trait from base lacks the resource parameter XCP requires, so this file keeps its
//! own `XcpSeedKeyProvider`. The data-file memory-segment model (not covered by base)
//! is temporarily represented by the local type `XcpMemorySegment`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::{ChecksumType, EcuPage};
use autors_can::device::CanDevice;
use autors_can::frame::CanFrame;

use crate::error::{Error, Result};
use crate::eth_transport::{TcpXcpTransport, UdpXcpTransport};
use crate::ifdata_xcp::{
    ChecksumSxi, CommModeBasic, XcpAlignment, XcpBlockMode, XcpDaq, XcpDaqList, XcpDaqListType,
    XcpDaqMode, XcpEvent, XcpHeaderLen, XcpIdFieldType, XcpNode, XcpOnCan, XcpOnSxi,
    XcpProtocolLayer, XcpTimestampResolution, XcpTimestampSize, XcpTimestampSupported,
};
use crate::sxi_serial::SxiXcpTransport;
use autors_comm::base::{
    CommKernel, CommMaster, CommMasterHandle, ConnectBehaviourType, DaqCache, DaqList,
    DaqMeasurement, Frame, OdtEntry, ProgramClearMode, ProgramVerifyMode, ProgressArgs,
    ProgressCallback, StartStopMode, XcpPrgParams,
};

// ============================================================================
// Small utilities
// ============================================================================

/// Process-local static start instant (same as autors-can `now_elapsed`).
pub(crate) fn now_elapsed() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// Async equivalent of a microsecond-scale delay.
/// Sub-millisecond waits are a busy-wait and waits of >=1ms a thread sleep in the
/// blocking design; here everything uniformly yields via [`autors_runtime::sleep`]
/// (same CF-throttling approach as autors-isotp; timing granularity depends on the
/// executor).
pub(crate) async fn block_for_micro_secs(us: u64) {
    autors_runtime::sleep(Duration::from_micros(us)).await;
}

/// Little-endian u16 read (XCP header / protocol inline numeric fields are always LE).
pub(crate) fn u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Little-endian u32 read.
pub(crate) fn u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub(crate) fn swap16(v: u16, swap: bool) -> u16 {
    if swap {
        v.swap_bytes()
    } else {
        v
    }
}

pub(crate) fn swap32(v: u32, swap: bool) -> u32 {
    if swap {
        v.swap_bytes()
    } else {
        v
    }
}

pub(crate) fn align_up(v: usize, align: usize) -> usize {
    debug_assert!(align > 0);
    v.div_ceil(align) * align
}

pub(crate) fn ts_size_bytes(size: XcpTimestampSize) -> usize {
    match size {
        XcpTimestampSize::BYTE => 1,
        XcpTimestampSize::WORD => 2,
        XcpTimestampSize::DWORD => 4,
        XcpTimestampSize::NotSet => 0,
    }
}

pub(crate) fn checksum_sxi_len(checksum: ChecksumSxi) -> usize {
    match checksum {
        ChecksumSxi::CHECKSUM_BYTE => 1,
        ChecksumSxi::CHECKSUM_WORD => 2,
        _ => 0,
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BasicAddressModeType {
    #[default]
    Address = 0,
    Length = 1,
}

impl BasicAddressModeType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CalPageMode(pub u8);

impl CalPageMode {
    pub const ECU: Self = Self(0x1);
    pub const XCP: Self = Self(0x2);
    pub const ALL: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for CalPageMode {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ClockInfo(pub u8);

impl ClockInfo {
    pub const NONE: Self = Self(0);
    /// SLV_CLK_INFO.
    pub const SLV_CLK_INFO: Self = Self(1);
    /// GRANDM_CLK_INFO.
    pub const GRANDM_CLK_INFO: Self = Self(2);
    /// CLK_RELATION.
    pub const CLK_RELATION: Self = Self(4);
    /// ECU_CLK_INFO.
    pub const ECU_CLK_INFO: Self = Self(8);
    /// ECU_GRANDM_CLK_INFO.
    pub const ECU_GRANDM_CLK_INFO: Self = Self(0x10);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CommModeOptional(pub u8);

impl CommModeOptional {
    pub const NONE: Self = Self(0x0);
    pub const MASTER_BLOCK_MODE: Self = Self(0x1);
    pub const INTERLEAVED_MODE: Self = Self(0x2);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CommModeProgram(pub u8);

impl CommModeProgram {
    pub const NONE: Self = Self(0x0);
    pub const MASTER_BLOCK_MODE: Self = Self(0x1);
    pub const INTERLEAVED_MODE: Self = Self(0x2);
    pub const SLAVE_BLOCK_MODE: Self = Self(0x40);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ConnectMode {
    #[default]
    Normal = 0,
    UserDefined = 1,
}

impl ConnectMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqEventProperties(pub u8);

impl DaqEventProperties {
    pub const DAQ: Self = Self(0x4);
    pub const STIM: Self = Self(0x8);
    pub const CONSISTENCY_DAQ: Self = Self(0x40);
    pub const CONSISTENCY_EVENT: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqKeyByte(pub u8);

impl DaqKeyByte {
    /// Optimisation_Type 0.
    pub const OPTIMISATION_TYPE0: Self = Self(0x1);
    /// Optimisation_Type 1.
    pub const OPTIMISATION_TYPE1: Self = Self(0x2);
    /// Optimisation_Type 0+1.
    pub const OPTIMISATION_TYPE01: Self = Self(0x3);
    /// Optimisation_Type 2.
    pub const OPTIMISATION_TYPE2: Self = Self(0x4);
    /// Optimisation_Type 0+2.
    pub const OPTIMISATION_TYPE02: Self = Self(0x5);
    /// Optimisation_Type 3.
    pub const OPTIMISATION_TYPE3: Self = Self(0x8);
    pub const ADR_EXTENSION_ODT: Self = Self(0x10);
    pub const ADR_EXTENSION_DAQ: Self = Self(0x20);
    pub const ADR_EXTENSION_ODTDAQ: Self = Self(0x30);
    /// Identification_Field_Type 0.
    pub const ID_FIELD_TYPE0: Self = Self(0x40);
    /// Identification_Field_Type 1.
    pub const ID_FIELD_TYPE1: Self = Self(0x80);
    /// Identification_Field_Type 0+1.
    pub const ID_FIELD_TYPE11: Self = Self(0xC0);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqListMode(pub u8);

impl DaqListMode {
    pub const NONE: Self = Self(0x0);
    pub const ALTERNATING: Self = Self(0x1);
    pub const SELECTED: Self = Self(0x1);
    pub const DIRECTION: Self = Self(0x2);
    pub const DTO_CTR: Self = Self(0x8);
    pub const TIMESTAMP: Self = Self(0x10);
    pub const PID_OFF: Self = Self(0x20);
    pub const RUNNING: Self = Self(0x40);
    /// RESUME.
    pub const RESUME: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for DaqListMode {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for DaqListMode {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqListProperties(pub u8);

impl DaqListProperties {
    pub const PREDEFINED: Self = Self(0x1);
    pub const EVENT_FIXED: Self = Self(0x2);
    pub const DAQ: Self = Self(0x4);
    pub const STIM: Self = Self(0x8);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqProperties(pub u8);

impl DaqProperties {
    pub const DYNAMIC: Self = Self(0x1);
    pub const PRESCALER_SUPPORTED: Self = Self(0x2);
    pub const RESUME_SUPPORTED: Self = Self(0x4);
    pub const BIT_STIM_SUPPORTED: Self = Self(0x8);
    pub const TIMESTAMP_SUPPORTED: Self = Self(0x10);
    pub const PID_OFF_SUPPORTED: Self = Self(0x20);
    pub const OVERLOAD_MSB: Self = Self(0x40);
    pub const OVERLOAD_EVENT: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DaqTimestampMode(pub u8);

impl DaqTimestampMode {
    pub const SIZE0: Self = Self(0x1);
    pub const SIZE1: Self = Self(0x2);
    pub const SIZE2: Self = Self(0x4);
    pub const TIMESTAMP_FIXED: Self = Self(0x8);
    pub const UINT0: Self = Self(0x10);
    pub const UINT1: Self = Self(0x20);
    pub const UINT2: Self = Self(0x40);
    pub const UINT3: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DtoCtrMode {
    #[default]
    None = 0,
    /// DAQ.
    Daq = 1,
    /// STIM.
    Stim = 2,
}

impl DtoCtrMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Daq,
            2 => Self::Stim,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DtoCtrModifier {
    #[default]
    None = 0,
    /// STIM.
    Stim = 1,
    /// DAQ.
    Daq = 2,
    RelatedEvent = 4,
}

impl DtoCtrModifier {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DtoCtrProperties(pub u8);

impl DtoCtrProperties {
    pub const RELATED_EVENT_FIXED: Self = Self(1);
    pub const DAQ_MODE_FIXED: Self = Self(2);
    pub const STIM_MODE_FIXED: Self = Self(4);
    pub const RELATED_EVENT_PRESENT: Self = Self(8);
    pub const DAQ_MODE_PRESENT: Self = Self(0x10);
    pub const STIM_MODE_PRESENT: Self = Self(0x20);
    pub const STIM_CTR_CPY_PRESENT: Self = Self(0x40);
    pub const ECT_CTR_PRESENT: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventCodes {
    #[default]
    ResumeMode = 0,
    ClearDAQ = 1,
    StoreDAQ = 2,
    StoreCAL = 3,
    CmdPending = 5,
    DaqOverload = 6,
    SessionTerminated = 7,
    TimeSync = 8,
    StimTimeout = 9,
    Sleep = 10,
    WakeUp = 11,
    DaqDataLost = 253,
    User = 254,
    Transport = 255,
}

impl EventCodes {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::ResumeMode,
            1 => Self::ClearDAQ,
            2 => Self::StoreDAQ,
            3 => Self::StoreCAL,
            5 => Self::CmdPending,
            6 => Self::DaqOverload,
            7 => Self::SessionTerminated,
            8 => Self::TimeSync,
            9 => Self::StimTimeout,
            10 => Self::Sleep,
            11 => Self::WakeUp,
            253 => Self::DaqDataLost,
            254 => Self::User,
            255 => Self::Transport,
            _ => return None,
        })
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::ResumeMode => "ResumeMode",
            Self::ClearDAQ => "ClearDAQ",
            Self::StoreDAQ => "StoreDAQ",
            Self::StoreCAL => "StoreCAL",
            Self::CmdPending => "CmdPending",
            Self::DaqOverload => "DAQOverload",
            Self::SessionTerminated => "SessionTerminated",
            Self::TimeSync => "TimeSync",
            Self::StimTimeout => "STIMTimeout",
            Self::Sleep => "Sleep",
            Self::WakeUp => "WakeUp",
            Self::DaqDataLost => "DAQDataLost",
            Self::User => "User",
            Self::Transport => "Transport",
        }
    }
}

impl fmt::Display for EventCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GetIdRespType(pub u8);

impl GetIdRespType {
    pub const TRANSFER_MODE: Self = Self(0x1);
    pub const COMPRESSED_ENCRYPTED: Self = Self(0x2);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GetIdType {
    #[default]
    Ascii = 0,
    Asap2FileNameWithoutExt = 1,
    Asap2FileName = 2,
    /// ASAP2 URL.
    Asap2Url = 3,
    Asap2File2Upload = 4,
}

impl GetIdType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GetSectorInfoMode {
    #[default]
    StartAddress = 0,
    Length = 1,
    NameLength = 2,
}

impl GetSectorInfoMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GetSegmentModeType {
    Address = 0,
    #[default]
    Standard = 1,
    Mapping = 2,
}

impl GetSegmentModeType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GetSlaveIdMode {
    #[default]
    IdentifyByEcho = 0,
    ConfirmByInverseEcho = 1,
}

impl GetSlaveIdMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GranOdtEntrySize {
    #[default]
    Byte = 1,
    Word = 2,
    DWord = 4,
    QWord = 8,
}

impl GranOdtEntrySize {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MappingInfoModeType {
    #[default]
    SourceAddress = 0,
    DestinationAddress = 1,
    LengthAddress = 2,
}

impl MappingInfoModeType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ObservableClocks(pub u8);

impl ObservableClocks {
    pub const NONE: Self = Self(0);
    pub const XCP_SLV_CLK_AVAIL: Self = Self(1);
    pub const XCP_SLV_CLK_NOT_AVAIL: Self = Self(2);
    pub const GRANDM_CLK_AVAIL: Self = Self(4);
    pub const GRANDM_CLK_NOT_AVAIL: Self = Self(8);
    pub const ECU_CLK_CAN_READ_RANDOM: Self = Self(0x10);
    pub const ECU_CLK_CANNOT_READ_RANDOM: Self = Self(0x20);
    pub const ECU_CLK_CANNOT_READ: Self = Self(48);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PageProperties(pub u8);

impl PageProperties {
    pub const ECU_ACCESS_WITHOUT_XCP: Self = Self(0x1);
    pub const ECU_ACCESS_WITH_XCP: Self = Self(0x2);
    pub const XCP_READ_ACCESS_WITHOUT_ECU: Self = Self(0x4);
    pub const XCP_READ_ACCESS_WITH_ECU: Self = Self(0x8);
    pub const XCP_WRITE_ACCESS_WITHOUT_ECU: Self = Self(0x10);
    pub const XCP_WRITE_ACCESS_WITH_ECU: Self = Self(0x20);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PayloadFmt(pub u8);

impl PayloadFmt {
    pub const NONE: Self = Self(0);
    pub const XCP_SLV_DWORD: Self = Self(1);
    pub const XCP_SLV_DLONG: Self = Self(2);
    /// grandmaster DWORD.
    pub const GRANDM_DWORD: Self = Self(4);
    /// grandmaster DLONG.
    pub const GRANDM_DLONG: Self = Self(8);
    /// ECU DWORD.
    pub const ECU_DWORD: Self = Self(0x10);
    /// ECU DLONG.
    pub const ECU_DLONG: Self = Self(0x20);
    pub const CLUSTER_IDENTIFIER: Self = Self(0x40);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PidSlaveMaster {
    Serv = 0xFC,
    Ev = 0xFD,
    Err = 0xFE,
    #[default]
    Res = 0xFF,
}

impl PidSlaveMaster {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0xFC => Self::Serv,
            0xFD => Self::Ev,
            0xFE => Self::Err,
            0xFF => Self::Res,
            _ => return None,
        })
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::Serv => "SERV",
            Self::Ev => "EV",
            Self::Err => "ERR",
            Self::Res => "RES",
        }
    }
}

impl fmt::Display for PidSlaveMaster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ProgramProperties(pub u8);

impl ProgramProperties {
    pub const NONE: Self = Self(0x0);
    pub const ABSOLUTE_MODE: Self = Self(0x1);
    pub const FUNCTIONAL_MODE: Self = Self(0x2);
    pub const COMPRESSION_SUPPORTED: Self = Self(0x4);
    pub const COMPRESSION_REQUIRED: Self = Self(0x8);
    pub const ENCRYPTION_SUPPORTED: Self = Self(0x10);
    pub const ENCRYPTION_REQUIRED: Self = Self(0x20);
    pub const NON_SEQ_PROGRAM_SUPPORTED: Self = Self(0x40);
    pub const NON_SEQ_PROGRAM_REQUIRED: Self = Self(0x80);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ResourceType(pub u8);

impl ResourceType {
    pub const NONE: Self = Self(0x0);
    pub const CAL_PAG: Self = Self(0x1);
    /// DAQ.
    pub const DAQ: Self = Self(0x4);
    /// STIM.
    pub const STIM: Self = Self(0x8);
    pub const PGM: Self = Self(0x10);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for ResourceType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SeedModeType {
    #[default]
    FirstPart = 0,
    RemainingPart = 1,
}

impl SeedModeType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SegmentInfoMode {
    #[default]
    GetBasicAddress = 0,
    GetStandardInfo = 1,
    GetAddressMapping = 2,
}

/// Segment mode flags (`[Flags]` byte).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SegmentMode(pub u8);

impl SegmentMode {
    /// None.
    pub const NONE: Self = Self(0x0);
    /// Freeze.
    pub const FREEZE: Self = Self(0x1);

    /// Raw bit value.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Service request code (byte).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ServiceRequestCode {
    /// The slave requests a reset.
    #[default]
    Reset = 0,
    /// The slave transmits an ASCII text stream.
    Text = 1,
}

impl ServiceRequestCode {
    /// Raw numeric value.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Numeric conversion (unknown values fall back to None).
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Reset),
            1 => Some(Self::Text),
            _ => None,
        }
    }

    /// Enumeration member name.
    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::Reset => "Reset",
            Self::Text => "Text",
        }
    }
}

/// Session state flags (`[Flags]` byte).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionState(pub u8);

impl SessionState {
    /// None.
    pub const NONE: Self = Self(0x0);
    /// STORE_CAL request.
    pub const STORE_CAL_REQUEST: Self = Self(0x1);
    /// STORE_DAQ request.
    pub const STORE_DAQ_REQUEST: Self = Self(0x4);
    /// CLEAR_DAQ request.
    pub const CLEAR_DAQ_REQUEST: Self = Self(0x8);
    /// DAQ running.
    pub const DAQ_RUNNING: Self = Self(0x40);
    /// RESUME.
    pub const RESUME: Self = Self(0x80);

    /// Raw bit value.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether all bits of `other` are set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Set-request mode flags (`[Flags]` byte).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SetRequestMode(pub u8);

impl SetRequestMode {
    /// STORE_CAL request.
    pub const STORE_CAL_REQUEST: Self = Self(0x1);
    /// STORE_DAQ request (without RESUME).
    pub const STORE_DAQ_REQUEST_NO_RESUME: Self = Self(0x2);
    /// STORE_DAQ request (with RESUME).
    pub const STORE_DAQ_REQUEST_RESUME: Self = Self(0x4);
    /// CLEAR_DAQ request.
    pub const CLEAR_DAQ_REQUEST: Self = Self(0x8);

    /// Raw bit value.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SlaveConfig(pub u8);

impl SlaveConfig {
    pub const NONE: Self = Self(0);
    pub const RESPONSE_FMT_SEND_ON_INIT_TRIGGER: Self = Self(1);
    pub const RESPONSE_FMT_SEND_ON_ALL_TRIGGER: Self = Self(2);
    pub const DAQ_TS_RELATION: Self = Self(4);
    pub const TIME_SYNC_BRIDGE_DISABLED: Self = Self(8);
    pub const TIME_SYNC_BRIDGE_ENABLED: Self = Self(0x10);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// STIM timeout mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StimTimeoutMode {
    /// Event channel number.
    #[default]
    EventChannelNo = 0,
    /// DAQ list number.
    DaqListNo = 1,
}

/// Synchronization state (byte bit flags).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SyncState(pub u8);

impl SyncState {
    /// None.
    pub const NONE: Self = Self(0);
    /// Slave clock is synchronized.
    pub const SLV_CLK_IS_SYNCRONIZED: Self = Self(1);
    /// Slave clock is syntonized (the SYNTONIZED spelling is kept verbatim).
    pub const SLV_CLK_IS_SYNTONIZED: Self = Self(3);
    /// Slave clock not supported.
    pub const SLV_CLK_IS_NOT_SUPPORTED: Self = Self(4);
    /// Grandmaster clock.
    pub const GRANDM_CLK: Self = Self(8);
    /// ECU clock.
    pub const ECU_CLK: Self = Self(0x10);
    /// ECU clock unknown.
    pub const ECU_CLK_UNKNOWN: Self = Self(0x20);

    /// Raw bit value.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TimeCorrGetPropsReq {
    #[default]
    None = 0,
    GetClkInfo = 1,
}

impl TimeCorrGetPropsReq {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TimeCorrSetProps(pub u8);

impl TimeCorrSetProps {
    pub const NONE: Self = Self(0);
    pub const RESPONSE_FMT_SEND_ON_INIT_TRIGGER: Self = Self(1);
    pub const RESPONSE_FMT_SEND_ON_ALL_TRIGGER: Self = Self(2);
    pub const TIME_SYNC_BRIDGE_ENABLE: Self = Self(4);
    pub const TIME_SYNC_BRIDGE_DISABLE: Self = Self(8);
    pub const SET_CLUSTER_ID: Self = Self(0x10);

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TimeOfTsSampling {
    #[default]
    DuringCmdProcessing = 0,
    LowJitter = 1,
    PhysicalTransmission = 2,
    PhysicalReception = 3,
}

impl TimeOfTsSampling {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::DuringCmdProcessing,
            1 => Self::LowJitter,
            2 => Self::PhysicalTransmission,
            3 => Self::PhysicalReception,
            _ => return None,
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TriggerInitiator {
    #[default]
    HwTrigger = 0,
    EventDerived = 1,
    MultiCast = 2,
    MultiCastViaTimeSync = 3,
    StateChangeInSynSync = 4,
    LeapSecondOccured = 5,
    ReleaseEcuReset = 6,
    Reserved = 7,
}

impl TriggerInitiator {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::HwTrigger,
            1 => Self::EventDerived,
            2 => Self::MultiCast,
            3 => Self::MultiCastViaTimeSync,
            4 => Self::StateChangeInSynSync,
            5 => Self::LeapSecondOccured,
            6 => Self::ReleaseEcuReset,
            7 => Self::Reserved,
            _ => return None,
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UsbEndpointType {
    #[default]
    Configurable = 0,
    Fixxed = 1,
}

impl UsbEndpointType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Configurable),
            1 => Some(Self::Fixxed),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XcpType {
    #[default]
    Unknown,
    /// UDP.
    Udp,
    /// TCP.
    Tcp,
    /// CAN.
    Can,
    /// USB.
    Usb,
    /// FlexRay.
    FlexRay,
    Sxi,
}

impl XcpType {
    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Udp => "UDP",
            Self::Tcp => "TCP",
            Self::Can => "CAN",
            Self::Usb => "USB",
            Self::FlexRay => "FlexRay",
            Self::Sxi => "SxI",
        }
    }
}

impl fmt::Display for XcpType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum XcpOnCanCmd {
    /// GET_SLAVE_ID.
    #[default]
    GetSlaveId = 0xFF,
    /// GET_DAQ_ID.
    GetDaqId = 0xFE,
    /// SET_DAQ_ID.
    SetDaqId = 0xFD,
    /// GET_DAQ_CLOCK_MULTICAST.
    GetDaqClockMulticast = 0xFA,
}

impl XcpOnCanCmd {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

// ============================================================================
// CommandCode / CmdResult
// ============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CommandCode {
    /// CONNECT.
    Connect = 0xFF,
    /// DISCONNECT.
    Disconnect = 0xFE,
    /// GET_STATUS.
    GetStatus = 0xFD,
    /// SYNCH.
    Synch = 0xFC,
    /// GET_COMM_MODE_INFO.
    GetCommModeInfo = 0xFB,
    /// GET_ID.
    GetId = 0xFA,
    /// SET_REQUEST.
    SetRequest = 0xF9,
    /// GET_SEED.
    GetSeed = 0xF8,
    /// UNLOCK.
    Unlock = 0xF7,
    /// SET_MTA.
    SetMta = 0xF6,
    /// UPLOAD.
    Upload = 0xF5,
    /// SHORT_UPLOAD.
    ShortUpload = 0xF4,
    /// BUILD_CHECKSUM.
    BuildChecksum = 0xF3,
    /// TRANSPORT_LAYER_CMD.
    TransportLayerCmd = 0xF2,
    /// USER_CMD.
    UserCmd = 0xF1,
    /// DOWNLOAD.
    Download = 0xF0,
    /// DOWNLOAD_NEXT.
    DownloadNext = 0xEF,
    /// DOWNLOAD_MAX.
    DownloadMax = 0xEE,
    /// SHORT_DOWNLOAD.
    ShortDownload = 0xED,
    /// MODIFY_BITS.
    ModifyBits = 0xEC,
    /// SET_CAL_PAGE.
    SetCalPage = 0xEB,
    /// GET_CAL_PAGE.
    GetCalPage = 0xEA,
    /// GET_PAG_PROCESSOR_INFO.
    GetPagProcessorInfo = 0xE9,
    /// GET_SEGMENT_INFO.
    GetSegmentInfo = 0xE8,
    /// GET_PAGE_INFO.
    GetPageInfo = 0xE7,
    /// SET_SEGMENT_MODE.
    SetSegmentMode = 0xE6,
    /// GET_SEGMENT_MODE.
    GetSegmentMode = 0xE5,
    /// COPY_CAL_PAGE.
    CopyCalPage = 0xE4,
    /// CLEAR_DAQ_LIST.
    ClearDaqList = 0xE3,
    /// SET_DAQ_PTR.
    SetDaqPtr = 0xE2,
    /// WRITE_DAQ.
    WriteDaq = 0xE1,
    /// SET_DAQ_LIST_MODE.
    SetDaqListMode = 0xE0,
    /// GET_DAQ_LIST_MODE.
    GetDaqListMode = 0xDF,
    /// START_STOP_DAQ_LIST.
    StartStopDaqList = 0xDE,
    /// START_STOP_SYNCH.
    StartStopSynch = 0xDD,
    /// GET_DAQ_CLOCK.
    GetDaqClock = 0xDC,
    /// READ_DAQ.
    ReadDaq = 0xDB,
    /// GET_DAQ_PROCESSOR_INFO.
    GetDaqProcessorInfo = 0xDA,
    /// GET_DAQ_RESOLUTION_INFO.
    GetDaqResolutionInfo = 0xD9,
    /// GET_DAQ_LIST_INFO.
    GetDaqListInfo = 0xD8,
    /// GET_DAQ_EVENT_INFO.
    GetDaqEventInfo = 0xD7,
    /// FREE_DAQ.
    FreeDAQ = 0xD6,
    /// ALLOC_DAQ.
    AllocDAQ = 0xD5,
    /// ALLOC_ODT.
    AllocODT = 0xD4,
    /// ALLOC_ODT_ENTRY.
    AllocODTEntry = 0xD3,
    /// PROGRAM_START.
    ProgramStart = 0xD2,
    /// PROGRAM_CLEAR.
    ProgramClear = 0xD1,
    /// PROGRAM.
    Program = 0xD0,
    /// PROGRAM_RESET.
    ProgramReset = 0xCF,
    /// GET_PGM_PROCESSOR_INFO.
    GetPgmProcessorInfo = 0xCE,
    /// GET_SECTOR_INFO.
    GetSectorInfo = 0xCD,
    /// PROGRAM_PREPARE.
    ProgramPrepare = 0xCC,
    /// PROGRAM_FORMAT.
    ProgramFormat = 0xCB,
    /// PROGRAM_NEXT.
    ProgramNext = 0xCA,
    /// PROGRAM_MAX.
    ProgramMax = 0xC9,
    /// PROGRAM_VERIFY.
    ProgramVerify = 0xC8,
    /// WRITE_DAQ_MULTIPLE.
    WriteDaqMultiple = 0xC7,
    /// TIME_CORRELATION_PROPERTIES.
    TimeCorrelationProperties = 0xC6,
    #[default]
    DtoCtrProperties = 0xC5,
}

impl CommandCode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0xFF => Self::Connect,
            0xFE => Self::Disconnect,
            0xFD => Self::GetStatus,
            0xFC => Self::Synch,
            0xFB => Self::GetCommModeInfo,
            0xFA => Self::GetId,
            0xF9 => Self::SetRequest,
            0xF8 => Self::GetSeed,
            0xF7 => Self::Unlock,
            0xF6 => Self::SetMta,
            0xF5 => Self::Upload,
            0xF4 => Self::ShortUpload,
            0xF3 => Self::BuildChecksum,
            0xF2 => Self::TransportLayerCmd,
            0xF1 => Self::UserCmd,
            0xF0 => Self::Download,
            0xEF => Self::DownloadNext,
            0xEE => Self::DownloadMax,
            0xED => Self::ShortDownload,
            0xEC => Self::ModifyBits,
            0xEB => Self::SetCalPage,
            0xEA => Self::GetCalPage,
            0xE9 => Self::GetPagProcessorInfo,
            0xE8 => Self::GetSegmentInfo,
            0xE7 => Self::GetPageInfo,
            0xE6 => Self::SetSegmentMode,
            0xE5 => Self::GetSegmentMode,
            0xE4 => Self::CopyCalPage,
            0xE3 => Self::ClearDaqList,
            0xE2 => Self::SetDaqPtr,
            0xE1 => Self::WriteDaq,
            0xE0 => Self::SetDaqListMode,
            0xDF => Self::GetDaqListMode,
            0xDE => Self::StartStopDaqList,
            0xDD => Self::StartStopSynch,
            0xDC => Self::GetDaqClock,
            0xDB => Self::ReadDaq,
            0xDA => Self::GetDaqProcessorInfo,
            0xD9 => Self::GetDaqResolutionInfo,
            0xD8 => Self::GetDaqListInfo,
            0xD7 => Self::GetDaqEventInfo,
            0xD6 => Self::FreeDAQ,
            0xD5 => Self::AllocDAQ,
            0xD4 => Self::AllocODT,
            0xD3 => Self::AllocODTEntry,
            0xD2 => Self::ProgramStart,
            0xD1 => Self::ProgramClear,
            0xD0 => Self::Program,
            0xCF => Self::ProgramReset,
            0xCE => Self::GetPgmProcessorInfo,
            0xCD => Self::GetSectorInfo,
            0xCC => Self::ProgramPrepare,
            0xCB => Self::ProgramFormat,
            0xCA => Self::ProgramNext,
            0xC9 => Self::ProgramMax,
            0xC8 => Self::ProgramVerify,
            0xC7 => Self::WriteDaqMultiple,
            0xC6 => Self::TimeCorrelationProperties,
            0xC5 => Self::DtoCtrProperties,
            _ => return None,
        })
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::GetStatus => "GetStatus",
            Self::Synch => "Synch",
            Self::GetCommModeInfo => "GetCommModeInfo",
            Self::GetId => "GetId",
            Self::SetRequest => "SetRequest",
            Self::GetSeed => "GetSeed",
            Self::Unlock => "Unlock",
            Self::SetMta => "SetMTA",
            Self::Upload => "Upload",
            Self::ShortUpload => "ShortUpload",
            Self::BuildChecksum => "BuildChecksum",
            Self::TransportLayerCmd => "TransportLayerCmd",
            Self::UserCmd => "UserCmd",
            Self::Download => "Download",
            Self::DownloadNext => "DownloadNext",
            Self::DownloadMax => "DownloadMax",
            Self::ShortDownload => "ShortDownload",
            Self::ModifyBits => "ModifyBits",
            Self::SetCalPage => "SetCalPage",
            Self::GetCalPage => "GetCalPage",
            Self::GetPagProcessorInfo => "GetPAGProcessorInfo",
            Self::GetSegmentInfo => "GetSegmentInfo",
            Self::GetPageInfo => "GetPageInfo",
            Self::SetSegmentMode => "SetSegmentMode",
            Self::GetSegmentMode => "GetSegmentMode",
            Self::CopyCalPage => "CopyCALPage",
            Self::ClearDaqList => "ClearDaqList",
            Self::SetDaqPtr => "SetDAQPtr",
            Self::WriteDaq => "WriteDAQ",
            Self::SetDaqListMode => "SetDAQListMode",
            Self::GetDaqListMode => "GetDAQListMode",
            Self::StartStopDaqList => "StartStopDAQList",
            Self::StartStopSynch => "StartStopSynch",
            Self::GetDaqClock => "GetDAQClock",
            Self::ReadDaq => "ReadDAQ",
            Self::GetDaqProcessorInfo => "GetDAQProcessorInfo",
            Self::GetDaqResolutionInfo => "GetDAQResolutionInfo",
            Self::GetDaqListInfo => "GetDAQListInfo",
            Self::GetDaqEventInfo => "GetDAQEventInfo",
            Self::FreeDAQ => "FreeDAQ",
            Self::AllocDAQ => "AllocDAQ",
            Self::AllocODT => "AllocODT",
            Self::AllocODTEntry => "AllocODTEntry",
            Self::ProgramStart => "ProgramStart",
            Self::ProgramClear => "ProgramClear",
            Self::Program => "Program",
            Self::ProgramReset => "ProgramReset",
            Self::GetPgmProcessorInfo => "GetPGMProcessorInfo",
            Self::GetSectorInfo => "GetSectorInfo",
            Self::ProgramPrepare => "ProgramPrepare",
            Self::ProgramFormat => "ProgramFormat",
            Self::ProgramNext => "ProgramNext",
            Self::ProgramMax => "ProgramMax",
            Self::ProgramVerify => "ProgramVerify",
            Self::WriteDaqMultiple => "WriteDAQMultiple",
            Self::TimeCorrelationProperties => "TimeCorrelationProperties",
            Self::DtoCtrProperties => "DtoCtrPproperties",
        }
    }
}

impl fmt::Display for CommandCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cs_name())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CmdResult(pub i32);

impl CmdResult {
    pub const ERR_CMD_SYNCH: Self = Self(0);
    pub const ERR_CMD_BUSY: Self = Self(0x10);
    pub const ERR_DAQ_ACTIVE: Self = Self(0x11);
    pub const ERR_PGM_ACTIVE: Self = Self(0x12);
    pub const ERR_CMD_UNKNOWN: Self = Self(0x20);
    pub const ERR_CMD_SYNTAX: Self = Self(0x21);
    pub const ERR_OUT_OF_RANGE: Self = Self(0x22);
    pub const ERR_WRITE_PROTECTED: Self = Self(0x23);
    pub const ERR_ACCESS_DENIED: Self = Self(0x24);
    pub const ERR_ACCESS_LOCKED: Self = Self(0x25);
    pub const ERR_PAGE_NOT_VALID: Self = Self(0x26);
    pub const ERR_MODE_NOT_VALID: Self = Self(0x27);
    pub const ERR_SEGMENT_NOT_VALID: Self = Self(0x28);
    pub const ERR_SEQUENCE: Self = Self(0x29);
    pub const ERR_DAQ_CONFIG: Self = Self(0x2A);
    pub const ERR_MEMORY_OVERFLOW: Self = Self(0x30);
    pub const ERR_GENERIC: Self = Self(0x31);
    pub const ERR_VERIFY: Self = Self(0x32);
    pub const ERR_RESOURCE_TEMPORARY_NOT_ACCESSIBLE: Self = Self(0x33);
    pub const ERR_SUBCMD_UNKNOWN: Self = Self(0x34);
    pub const OK: Self = Self(0xFF);
    pub const ERR_SND_CMD_FAILED: Self = Self(0x100);
    pub const ERR_TIMEOUT: Self = Self(0x101);
    pub const ERR_CANCELLED: Self = Self(0x102);
    pub const ERR_INVALID_ARGUMENT: Self = Self(0x103);
    pub const ERR_PROTOCOL_FAILURE: Self = Self(0x104);

    pub const fn as_i32(self) -> i32 {
        self.0
    }

    pub const fn as_wire_u8(self) -> u8 {
        self.0 as u8
    }

    pub const fn from_wire_u8(v: u8) -> Self {
        Self(v as i32)
    }

    pub const fn description(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "Command processor synchronization",
            0x10 => "Command was not executed",
            0x11 => "Command rejected because DAQ is running",
            0x12 => "Command rejected because PGM is running",
            0x20 => "Unknown command or not implemented optional command",
            0x21 => "Command syntax invalid",
            0x22 => "Command syntax valid but command parameter(s) out of range",
            0x23 => "The memory location is write protected",
            0x24 => "The memory location is not accessible",
            0x25 => "Access denied, Seed & Key is required",
            0x26 => "Selected page not available",
            0x27 => "Selected page mode not available",
            0x28 => "Selected segment not valid",
            0x29 => "Sequence error",
            0x2A => "DAQ configuration not valid",
            0x30 => "Memory overflow error",
            0x31 => "Generic error",
            0x32 => "The slave internal program verify routine detects an error",
            0x33 => "Access to the requested resource is temporary not possible",
            0x34 => "Unknown sub command or not implemented optional sub command",
            0xFF => "Successful",
            0x100 => "Send command failed",
            0x101 => "Timeout",
            0x102 => "Command was canelled by user",
            0x103 => "Invalid argument failure",
            0x104 => "Protocol failure (unexpected response length)",
            _ => return None,
        })
    }

    pub fn cs_name(self) -> String {
        match self.0 {
            0 => "ERR_CMD_SYNCH".to_string(),
            0x10 => "ERR_CMD_BUSY".to_string(),
            0x11 => "ERR_DAQ_ACTIVE".to_string(),
            0x12 => "ERR_PGM_ACTIVE".to_string(),
            0x20 => "ERR_CMD_UNKNOWN".to_string(),
            0x21 => "ERR_CMD_SYNTAX".to_string(),
            0x22 => "ERR_OUT_OF_RANGE".to_string(),
            0x23 => "ERR_WRITE_PROTECTED".to_string(),
            0x24 => "ERR_ACCESS_DENIED".to_string(),
            0x25 => "ERR_ACCESS_LOCKED".to_string(),
            0x26 => "ERR_PAGE_NOT_VALID".to_string(),
            0x27 => "ERR_MODE_NOT_VALID".to_string(),
            0x28 => "ERR_SEGMENT_NOT_VALID".to_string(),
            0x29 => "ERR_SEQUENCE".to_string(),
            0x2A => "ERR_DAQ_CONFIG".to_string(),
            0x30 => "ERR_MEMORY_OVERFLOW".to_string(),
            0x31 => "ERR_GENERIC".to_string(),
            0x32 => "ERR_VERIFY".to_string(),
            0x33 => "ERR_RESOURCE_TEMPORARY_NOT_ACCESSIBLE".to_string(),
            0x34 => "ERR_SUBCMD_UNKNOWN".to_string(),
            0xFF => "OK".to_string(),
            0x100 => "ERR_SND_CMD_FAILED".to_string(),
            0x101 => "ERR_TIMEOUT".to_string(),
            0x102 => "ERR_CANCELLED".to_string(),
            0x103 => "ERR_INVALID_ARGUMENT".to_string(),
            0x104 => "ERR_PROTOCOL_FAILURE".to_string(),
            v => v.to_string(),
        }
    }
}

impl fmt::Display for CmdResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.cs_name())
    }
}

// ============================================================================
// ============================================================================

pub trait XcpCommand {
    fn cmd_code(&self) -> CommandCode;

    fn encode(&self, swap: bool) -> Vec<u8>;

    fn to_bytes(
        &self,
        frame_fmt: XcpHeaderLen,
        ctr: u16,
        swap: bool,
        additional: &[u8],
    ) -> Vec<u8> {
        build_cto(frame_fmt, ctr, &self.encode(swap), additional)
    }
}

/// GET_COMM_MODE_INFO/GET_PAG_PROCESSOR_INFO/FREE_DAQ/GET_DAQ_CLOCK/READ_DAQ/
/// GET_DAQ_PROCESSOR_INFO/GET_DAQ_RESOLUTION_INFO/PROGRAM_START/PROGRAM_RESET/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdBare {
    pub cmd_code: CommandCode,
}

impl CmdBare {
    pub const fn new(cmd_code: CommandCode) -> Self {
        Self { cmd_code }
    }
}

impl XcpCommand for CmdBare {
    fn cmd_code(&self) -> CommandCode {
        self.cmd_code
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![self.cmd_code.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdConnect {
    pub mode: ConnectMode,
}

impl XcpCommand for CmdConnect {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::Connect
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::Connect.as_u8(), self.mode.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdDisconnect;

impl XcpCommand for CmdDisconnect {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::Disconnect
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::Disconnect.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetId {
    pub id_type: GetIdType,
}

impl XcpCommand for CmdGetId {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetId
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::GetId.as_u8(), self.id_type.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetRequest {
    pub mode: SetRequestMode,
    pub session_id: u16,
}

impl XcpCommand for CmdSetRequest {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetRequest
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::SetRequest.as_u8(), self.mode.bits()];
        v.extend_from_slice(&swap16(self.session_id, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetSeed {
    pub mode: SeedModeType,
    pub resource: ResourceType,
}

impl XcpCommand for CmdGetSeed {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetSeed
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::GetSeed.as_u8(),
            self.mode.as_u8(),
            self.resource.bits(),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdUnlock {
    pub remaining_len: u8,
}

impl XcpCommand for CmdUnlock {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::Unlock
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::Unlock.as_u8(), self.remaining_len]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetMta {
    pub address_extension: u8,
    pub address: u32,
}

impl XcpCommand for CmdSetMta {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetMta
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::SetMta.as_u8(), 0, 0, self.address_extension];
        v.extend_from_slice(&swap32(self.address, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdUpload {
    pub number_of_elements: u8,
}

impl XcpCommand for CmdUpload {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::Upload
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::Upload.as_u8(), self.number_of_elements]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdShortUpload {
    pub number_of_elements: u8,
    pub address_extension: u8,
    pub address: u32,
}

impl XcpCommand for CmdShortUpload {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ShortUpload
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![
            CommandCode::ShortUpload.as_u8(),
            self.number_of_elements,
            0,
            self.address_extension,
        ];
        v.extend_from_slice(&swap32(self.address, swap).to_le_bytes());
        v
    }
}

/// BUILD_CHECKSUM command parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdBuildChecksum {
    /// Block size.
    pub block_size: u32,
}

impl XcpCommand for CmdBuildChecksum {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::BuildChecksum
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::BuildChecksum.as_u8(), 0, 0, 0];
        v.extend_from_slice(&swap32(self.block_size, swap).to_le_bytes());
        v
    }
}

/// TRANSPORT_LAYER_CMD / USER_CMD command parameters
/// (additional data bytes are appended via `additional`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdUserTransCmd {
    /// Command code (`TransportLayerCmd` or `UserCmd`).
    pub cmd_code: CommandCode,
    /// Sub-command.
    pub sub_command: u8,
}

impl XcpCommand for CmdUserTransCmd {
    fn cmd_code(&self) -> CommandCode {
        self.cmd_code
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![self.cmd_code.as_u8(), self.sub_command]
    }
}

/// DOWNLOAD / DOWNLOAD_NEXT / PROGRAM / PROGRAM_NEXT command parameters
/// (data bytes are appended via `additional`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdDownload {
    /// Command code.
    pub cmd_code: CommandCode,
    /// Number of elements.
    pub number_of_elements: u8,
}

impl XcpCommand for CmdDownload {
    fn cmd_code(&self) -> CommandCode {
        self.cmd_code
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![self.cmd_code.as_u8(), self.number_of_elements]
    }
}

/// SHORT_DOWNLOAD command parameters (data bytes are appended via `additional`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdShortDownload {
    /// Number of elements.
    pub number_of_elements: u8,
    /// Address extension.
    pub address_extension: u8,
    /// Address.
    pub address: u32,
}

impl XcpCommand for CmdShortDownload {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ShortDownload
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![
            CommandCode::ShortDownload.as_u8(),
            self.number_of_elements,
            0,
            self.address_extension,
        ];
        v.extend_from_slice(&swap32(self.address, swap).to_le_bytes());
        v
    }
}

/// MODIFY_BITS command parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdModifyBits {
    /// Shift value.
    pub shift_value: u8,
    /// AND mask.
    pub and_mask: u16,
    /// XOR mask.
    pub xor_mask: u16,
}

impl XcpCommand for CmdModifyBits {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ModifyBits
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::ModifyBits.as_u8(), self.shift_value];
        v.extend_from_slice(&swap16(self.and_mask, swap).to_le_bytes());
        v.extend_from_slice(&swap16(self.xor_mask, swap).to_le_bytes());
        v
    }
}

/// SET_CAL_PAGE command parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetCalPage {
    /// Mode.
    pub mode: CalPageMode,
    /// Segment number.
    pub segment_no: u8,
    /// Page number.
    pub page_no: u8,
}

impl XcpCommand for CmdSetCalPage {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetCalPage
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::SetCalPage.as_u8(),
            self.mode.bits(),
            self.segment_no,
            self.page_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetCalPage {
    pub mode: CalPageMode,
    pub segment_no: u8,
}

impl XcpCommand for CmdGetCalPage {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetCalPage
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::GetCalPage.as_u8(),
            self.mode.bits(),
            self.segment_no,
        ]
    }
}

/// GET_SEGMENT_INFO command parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetSegmentInfo {
    /// Mode.
    pub mode: GetSegmentModeType,
    /// Segment number.
    pub segment_no: u8,
    /// Segment info selector (`BasicAddressModeType` / `MappingInfoModeType` numeric value).
    pub segment_info: u8,
    /// Mapping index.
    pub mapping_index: u8,
}

impl CmdGetSegmentInfo {
    /// Standard segment info query.
    pub fn standard(segment_no: u8) -> Self {
        Self {
            mode: GetSegmentModeType::Standard,
            segment_no,
            segment_info: 0,
            mapping_index: 0,
        }
    }

    /// Address/length segment info query (`BasicAddressModeType`).
    pub fn address(mode: BasicAddressModeType, segment_no: u8) -> Self {
        Self {
            mode: GetSegmentModeType::Address,
            segment_no,
            segment_info: mode.as_u8(),
            mapping_index: 0,
        }
    }

    /// Address mapping segment info query (`MappingInfoModeType`).
    pub fn mapping(mode: MappingInfoModeType, segment_no: u8, mapping_index: u8) -> Self {
        Self {
            mode: GetSegmentModeType::Mapping,
            segment_no,
            segment_info: mode.as_u8(),
            mapping_index,
        }
    }
}

impl XcpCommand for CmdGetSegmentInfo {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetSegmentInfo
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::GetSegmentInfo.as_u8(),
            self.mode.as_u8(),
            self.segment_no,
            self.segment_info,
            self.mapping_index,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetPageInfo {
    pub segment_no: u8,
    pub page_no: u8,
}

impl XcpCommand for CmdGetPageInfo {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetPageInfo
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::GetPageInfo.as_u8(),
            self.segment_no,
            self.page_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetSegmentMode {
    pub mode: SegmentMode,
    pub segment_no: u8,
}

impl XcpCommand for CmdSetSegmentMode {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetSegmentMode
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::SetSegmentMode.as_u8(),
            self.mode.bits(),
            self.segment_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetSegmentMode {
    pub segment_no: u8,
}

impl XcpCommand for CmdGetSegmentMode {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetSegmentMode
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::GetSegmentMode.as_u8(), 0, self.segment_no]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdCopyCalPage {
    pub src_segment_no: u8,
    pub src_page_no: u8,
    pub dst_segment_no: u8,
    pub dst_page_no: u8,
}

impl XcpCommand for CmdCopyCalPage {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::CopyCalPage
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::CopyCalPage.as_u8(),
            self.src_segment_no,
            self.src_page_no,
            self.dst_segment_no,
            self.dst_page_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdList {
    pub cmd_code: CommandCode,
    pub list_no: u16,
}

impl XcpCommand for CmdList {
    fn cmd_code(&self) -> CommandCode {
        self.cmd_code
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![self.cmd_code.as_u8(), 0];
        v.extend_from_slice(&swap16(self.list_no, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetDaqPtr {
    pub daq_list_no: u16,
    pub odt_no: u8,
    pub odt_entry_no: u8,
}

impl XcpCommand for CmdSetDaqPtr {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetDaqPtr
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::SetDaqPtr.as_u8(), 0];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v.extend_from_slice(&[self.odt_no, self.odt_entry_no]);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdWriteDaq {
    pub bit_offset: u8,
    pub element_size: u8,
    pub address_extension: u8,
    pub address: u32,
}

impl XcpCommand for CmdWriteDaq {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::WriteDaq
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![
            CommandCode::WriteDaq.as_u8(),
            self.bit_offset,
            self.element_size,
            self.address_extension,
        ];
        v.extend_from_slice(&swap32(self.address, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdWriteDaqMultiple {
    pub record_count: u8,
}

impl XcpCommand for CmdWriteDaqMultiple {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::WriteDaqMultiple
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::WriteDaqMultiple.as_u8(), self.record_count]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetDaqListMode {
    pub mode: DaqListMode,
    pub daq_list_no: u16,
    pub event_channel_no: u16,
    pub prescaler: u8,
    pub priority: u8,
}

impl XcpCommand for CmdSetDaqListMode {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::SetDaqListMode
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::SetDaqListMode.as_u8(), self.mode.bits()];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v.extend_from_slice(&swap16(self.event_channel_no, swap).to_le_bytes());
        v.extend_from_slice(&[self.prescaler, self.priority]);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdStartStopDaqList {
    pub mode: StartStopMode,
    pub daq_list_no: u16,
}

impl XcpCommand for CmdStartStopDaqList {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::StartStopDaqList
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::StartStopDaqList.as_u8(), self.mode.as_u8()];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdStartStopSynch {
    pub mode: StartStopMode,
}

impl XcpCommand for CmdStartStopSynch {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::StartStopSynch
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![CommandCode::StartStopSynch.as_u8(), self.mode.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdAllocDaq {
    pub daq_count: u16,
}

impl XcpCommand for CmdAllocDaq {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::AllocDAQ
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::AllocDAQ.as_u8(), 0];
        v.extend_from_slice(&swap16(self.daq_count, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdAllocOdt {
    pub daq_list_no: u16,
    pub odt_count: u8,
}

impl XcpCommand for CmdAllocOdt {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::AllocODT
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::AllocODT.as_u8(), 0];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v.push(self.odt_count);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdAllocOdtEntry {
    pub daq_list_no: u16,
    pub odt_no: u8,
    pub odt_entries_count: u8,
}

impl XcpCommand for CmdAllocOdtEntry {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::AllocODTEntry
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::AllocODTEntry.as_u8(), 0];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v.extend_from_slice(&[self.odt_no, self.odt_entries_count]);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdProgramClear {
    pub mode: ProgramClearMode,
    pub clear_range: u32,
}

impl XcpCommand for CmdProgramClear {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ProgramClear
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::ProgramClear.as_u8(), self.mode as u8, 0, 0];
        v.extend_from_slice(&swap32(self.clear_range, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetSectorInfo {
    pub mode: GetSectorInfoMode,
    pub sector_no: u8,
}

impl XcpCommand for CmdGetSectorInfo {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::GetSectorInfo
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::GetSectorInfo.as_u8(),
            self.mode.as_u8(),
            self.sector_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdProgramPrepare {
    pub code_size: u16,
}

impl XcpCommand for CmdProgramPrepare {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ProgramPrepare
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::ProgramPrepare.as_u8(), 0];
        v.extend_from_slice(&swap16(self.code_size, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdProgramFormat {
    pub compression_method: u8,
    pub encryption_method: u8,
    pub programming_method: u8,
    pub access_method: u8,
}

impl XcpCommand for CmdProgramFormat {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ProgramFormat
    }

    fn encode(&self, _swap: bool) -> Vec<u8> {
        vec![
            CommandCode::ProgramFormat.as_u8(),
            self.compression_method,
            self.encryption_method,
            self.programming_method,
            self.access_method,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdProgramVerify {
    pub mode: ProgramVerifyMode,
    pub verification_type: u16,
    pub verification_value: u32,
}

impl XcpCommand for CmdProgramVerify {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::ProgramVerify
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::ProgramVerify.as_u8(), self.mode as u8];
        v.extend_from_slice(&swap16(self.verification_type, swap).to_le_bytes());
        v.extend_from_slice(&swap32(self.verification_value, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdDtoCtrProperties {
    pub modifier: DtoCtrModifier,
    pub event_channel_no: u16,
    pub related_event_channel_no: u16,
    pub mode: DtoCtrMode,
}

impl XcpCommand for CmdDtoCtrProperties {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::DtoCtrProperties
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::DtoCtrProperties.as_u8(), self.modifier.as_u8()];
        v.extend_from_slice(&swap16(self.event_channel_no, swap).to_le_bytes());
        v.extend_from_slice(&swap16(self.related_event_channel_no, swap).to_le_bytes());
        v.push(self.mode.as_u8());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdTimeCorrelationProperties {
    pub set_properties: TimeCorrSetProps,
    pub get_properties_req: TimeCorrGetPropsReq,
    pub cluster_id: u16,
}

impl XcpCommand for CmdTimeCorrelationProperties {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::TimeCorrelationProperties
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![
            CommandCode::TimeCorrelationProperties.as_u8(),
            self.set_properties.bits(),
            self.get_properties_req.as_u8(),
            0,
        ];
        v.extend_from_slice(&swap16(self.cluster_id, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetDaqListUsbEndpoint {
    pub daq_list_no: u16,
}

impl XcpCommand for CmdGetDaqListUsbEndpoint {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::TransportLayerCmd
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::TransportLayerCmd.as_u8(), 0xFF];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetDaqListUsbEndpoint {
    pub daq_list_no: u16,
    pub endpoint_no: u8,
}

impl XcpCommand for CmdSetDaqListUsbEndpoint {
    fn cmd_code(&self) -> CommandCode {
        CommandCode::TransportLayerCmd
    }

    fn encode(&self, swap: bool) -> Vec<u8> {
        let mut v = vec![CommandCode::TransportLayerCmd.as_u8(), 0xFE];
        v.extend_from_slice(&swap16(self.daq_list_no, swap).to_le_bytes());
        v.push(self.endpoint_no);
        v
    }
}

// ============================================================================
// ============================================================================

/// CTR_BYTE/FILL_BYTE/WORD=2,CTR_WORD/FILL_WORD=4).
pub const fn header_len_size(frame_fmt: XcpHeaderLen) -> usize {
    match frame_fmt {
        XcpHeaderLen::BYTE => 1,
        XcpHeaderLen::CTR_BYTE | XcpHeaderLen::FILL_BYTE | XcpHeaderLen::WORD => 2,
        XcpHeaderLen::CTR_WORD | XcpHeaderLen::FILL_WORD => 4,
        _ => 0,
    }
}

pub fn build_cto(frame_fmt: XcpHeaderLen, ctr: u16, payload: &[u8], additional: &[u8]) -> Vec<u8> {
    let total = (payload.len() + additional.len()) as u16;
    let hdr = header_len_size(frame_fmt);
    let mut out = Vec::with_capacity(hdr + payload.len() + additional.len());
    match frame_fmt {
        XcpHeaderLen::BYTE => out.push(total as u8),
        XcpHeaderLen::FILL_BYTE => out.extend_from_slice(&[total as u8, 0]),
        XcpHeaderLen::CTR_BYTE => {
            out.push(total as u8);
            out.push(ctr as u8);
        }
        XcpHeaderLen::WORD => out.extend_from_slice(&total.to_le_bytes()),
        XcpHeaderLen::FILL_WORD => {
            out.extend_from_slice(&total.to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        }
        XcpHeaderLen::CTR_WORD => {
            out.extend_from_slice(&total.to_le_bytes());
            out.extend_from_slice(&ctr.to_le_bytes());
        }
        _ => {}
    }
    out.extend_from_slice(payload);
    out.extend_from_slice(additional);
    out
}

// ============================================================================
// ============================================================================

pub trait XcpResponse: Sized {
    const SIZE: usize;

    fn decode(data: &[u8], swap: bool) -> Option<Self>;

    fn decode_with_rest(data: &[u8], swap: bool) -> Option<(Self, &[u8])> {
        let resp = Self::decode(data, swap)?;
        Some((resp, &data[Self::SIZE..]))
    }
}

fn decode_pid(data: &[u8], size: usize) -> Option<PidSlaveMaster> {
    if data.len() < size {
        return None;
    }
    PidSlaveMaster::from_u8(data[0])
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespBase {
    pub pid: PidSlaveMaster,
}

impl XcpResponse for RespBase {
    const SIZE: usize = 1;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        Some(Self {
            pid: decode_pid(data, 1)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespError {
    pub pid: PidSlaveMaster,
    pub error_code: CmdResult,
}

impl RespError {
    pub fn new(error_code: CmdResult) -> Self {
        Self {
            pid: PidSlaveMaster::Err,
            error_code,
        }
    }
}

impl XcpResponse for RespError {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            error_code: CmdResult::from_wire_u8(data[1]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespConnect {
    pub pid: PidSlaveMaster,
    pub resource: ResourceType,
    pub comm_mode_basic: CommModeBasic,
    pub max_cto: u8,
    pub max_dto: u16,
    pub version: u16,
}

impl RespConnect {
    pub fn new(
        resource: ResourceType,
        comm_mode_basic: CommModeBasic,
        max_cto: u8,
        max_dto: u16,
        v1: u8,
        v2: u8,
    ) -> Self {
        Self {
            pid: PidSlaveMaster::Res,
            resource,
            comm_mode_basic,
            max_cto,
            max_dto,
            version: ((v1 as u16) << 8) + v2 as u16,
        }
    }

    pub fn version_major(&self) -> u8 {
        (self.version >> 8) as u8
    }

    pub fn version_minor(&self) -> u8 {
        (self.version & 0xFF) as u8
    }

    pub fn address_granularity(&self) -> usize {
        if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_DWORD)
        {
            4
        } else if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_WORD)
        {
            2
        } else {
            1
        }
    }

    pub fn is_big_endian(&self) -> bool {
        self.comm_mode_basic.contains(CommModeBasic::BIG_ENDIAN)
    }
}

impl XcpResponse for RespConnect {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            resource: ResourceType(data[1]),
            comm_mode_basic: CommModeBasic(data[2]),
            max_cto: data[3],
            max_dto: swap16(u16_le(data, 4), swap),
            version: swap16(u16_le(data, 6), swap),
        })
    }
}

impl fmt::Display for RespConnect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endian = if self.is_big_endian() {
            "Big"
        } else {
            "Little"
        };
        let ag = if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_WORD)
        {
            "WORD"
        } else if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_DWORD)
        {
            "DWORD"
        } else {
            "BYTE"
        };
        let mut resources = String::new();
        if self.resource.contains(ResourceType::CAL_PAG) {
            resources.push_str("CAL_PAG,");
        }
        if self.resource.contains(ResourceType::DAQ) {
            resources.push_str("DAQ,");
        }
        if self.resource.contains(ResourceType::PGM) {
            resources.push_str("PGM,");
        }
        if self.resource.contains(ResourceType::STIM) {
            resources.push_str("STIM,");
        }
        if self
            .comm_mode_basic
            .contains(CommModeBasic::SLAVE_BLOCK_MODE)
        {
            resources.push_str("Slave Block mode supported");
        }
        write!(
            f,
            "XCPVersion={}.{} {}Endian MaxCTO={} MaxDTO={} AddressGranularity={} Resources={}",
            self.version_major(),
            self.version_minor(),
            endian,
            self.max_cto,
            self.max_dto,
            ag,
            resources
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetStatus {
    pub pid: PidSlaveMaster,
    pub session_state: SessionState,
    pub resource_protection_state: ResourceType,
    pub session_configuration_id: u16,
}

impl RespGetStatus {
    pub fn is_storing(&self) -> bool {
        self.session_state.contains(SessionState::STORE_CAL_REQUEST)
            || self.session_state.contains(SessionState::STORE_DAQ_REQUEST)
    }
}

impl XcpResponse for RespGetStatus {
    const SIZE: usize = 6;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            session_state: SessionState(data[1]),
            resource_protection_state: ResourceType(data[2]),
            session_configuration_id: swap16(u16_le(data, 4), swap),
        })
    }
}

impl fmt::Display for RespGetStatus {
    /// Formats as `{TypeName}: SessionState={0}\nResourceProtectionState={1}\nSessionID={2}`.
    /// The type name is the fixed string "RespGetStatus"; the flag enums are printed
    /// as their numeric value rather than an "A, B" flag list (log text only).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RespGetStatus: SessionState={}\nResourceProtectionState={}\nSessionID={}",
            self.session_state.bits(),
            self.resource_protection_state.bits(),
            self.session_configuration_id
        )
    }
}

/// Response to the GET_COMM_MODE_INFO command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetCommModeInfo {
    /// Packet identifier.
    pub pid: PidSlaveMaster,
    /// Optional communication mode.
    pub comm_mode: CommModeOptional,
    /// Maximum block size.
    pub max_bs: u8,
    /// Minimum separation time.
    pub min_st: u8,
    /// Queue size.
    pub queue_size: u8,
    /// Driver version.
    pub driver_version: u8,
}

impl XcpResponse for RespGetCommModeInfo {
    const SIZE: usize = 8;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            comm_mode: CommModeOptional(data[2]),
            max_bs: data[4],
            min_st: data[5],
            queue_size: data[6],
            driver_version: data[7],
        })
    }
}

impl fmt::Display for RespGetCommModeInfo {
    /// Formats the response according to the communication mode flags.
    /// Note: the Interleaved-only branch intentionally prints
    /// `"InterLeavedMode (QueueSize={0})"` with MaxBS as the first argument —
    /// the value shown as "QueueSize" is actually MaxBS.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mbm = self.comm_mode.contains(CommModeOptional::MASTER_BLOCK_MODE);
        let ilm = self.comm_mode.contains(CommModeOptional::INTERLEAVED_MODE);
        match (mbm, ilm) {
            (true, true) => write!(
                f,
                "MasterBlockMode (MaxBS={} MinST={}), InterLeavedMode (QueueSize={})",
                self.max_bs, self.min_st, self.queue_size
            ),
            (true, false) => write!(
                f,
                "MasterBlockMode (MaxBS={} MinST={})",
                self.max_bs, self.min_st
            ),
            (false, true) => write!(f, "InterLeavedMode (QueueSize={})", self.max_bs),
            (false, false) => Ok(()),
        }
    }
}

/// Response to the GET_ID command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetId {
    /// Packet identifier.
    pub pid: PidSlaveMaster,
    /// Mode.
    pub mode: GetIdRespType,
    /// Data length.
    pub length: u32,
}

impl XcpResponse for RespGetId {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            mode: GetIdRespType(data[1]),
            length: swap32(u32_le(data, 4), swap),
        })
    }
}

/// Response to the GET_SEED command (the seed bytes follow as additional data).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSeed {
    /// Packet identifier.
    pub pid: PidSlaveMaster,
    /// Total seed length (may be split into multiple parts).
    pub length: u8,
}

impl XcpResponse for RespGetSeed {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            length: data[1],
        })
    }
}

/// Response to the UNLOCK command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespUnlock {
    /// Packet identifier.
    pub pid: PidSlaveMaster,
    /// Protection state after unlocking.
    pub protection_state: ResourceType,
}

impl XcpResponse for RespUnlock {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            protection_state: ResourceType(data[1]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespBuildChecksum {
    pub pid: PidSlaveMaster,
    pub xcp_type: u8,
    pub checksum: u32,
}

impl RespBuildChecksum {
    pub fn checksum_type(&self) -> Option<ChecksumType> {
        xcp_to_checksum_type(self.xcp_type)
    }

    pub fn new(type_: ChecksumType, checksum: u32) -> Option<Self> {
        Some(Self {
            pid: PidSlaveMaster::Res,
            xcp_type: checksum_type_to_xcp(type_)?,
            checksum,
        })
    }
}

fn xcp_to_checksum_type(v: u8) -> Option<ChecksumType> {
    Some(match v {
        1 => ChecksumType::ADD_11,
        2 => ChecksumType::ADD_12,
        3 => ChecksumType::ADD_14,
        4 => ChecksumType::ADD_22,
        5 => ChecksumType::ADD_24,
        6 => ChecksumType::ADD_44,
        7 => ChecksumType::CRC_16,
        8 => ChecksumType::CRC_16_CITT,
        9 => ChecksumType::CRC_32,
        0xFF => ChecksumType::USER_DEFINED,
        _ => return None,
    })
}

fn checksum_type_to_xcp(t: ChecksumType) -> Option<u8> {
    Some(match t {
        ChecksumType::ADD_11 => 1,
        ChecksumType::ADD_12 => 2,
        ChecksumType::ADD_14 => 3,
        ChecksumType::ADD_22 => 4,
        ChecksumType::ADD_24 => 5,
        ChecksumType::ADD_44 => 6,
        ChecksumType::CRC_16 => 7,
        ChecksumType::CRC_16_CITT => 8,
        ChecksumType::CRC_32 => 9,
        ChecksumType::USER_DEFINED => 0xFF,
        _ => return None,
    })
}

impl XcpResponse for RespBuildChecksum {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            xcp_type: data[1],
            checksum: swap32(u32_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetCalPage {
    pub pid: PidSlaveMaster,
    pub page_no: u8,
}

impl XcpResponse for RespGetCalPage {
    const SIZE: usize = 4;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            page_no: data[3],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetPagProcessorInfo {
    pub pid: PidSlaveMaster,
    pub max_segment: u8,
    pub properties: crate::ifdata_xcp::PagProperties,
}

impl XcpResponse for RespGetPagProcessorInfo {
    const SIZE: usize = 3;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            max_segment: data[1],
            properties: crate::ifdata_xcp::PagProperties(data[2]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSegmentInfo {
    pub pid: PidSlaveMaster,
    pub max_pages: u8,
    pub address_extension: u8,
    pub max_mapping: u8,
    pub compression_method: u8,
    pub encryption_method: u8,
}

impl XcpResponse for RespGetSegmentInfo {
    const SIZE: usize = 6;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            max_pages: data[1],
            address_extension: data[2],
            max_mapping: data[3],
            compression_method: data[4],
            encryption_method: data[5],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSegmentInfoAddress {
    pub pid: PidSlaveMaster,
    pub info: u32,
}

impl XcpResponse for RespGetSegmentInfoAddress {
    const SIZE: usize = 7;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            info: swap32(u32_le(data, 3), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetPageInfo {
    pub pid: PidSlaveMaster,
    pub properties: PageProperties,
    pub init_segment: u8,
}

impl XcpResponse for RespGetPageInfo {
    const SIZE: usize = 3;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: PageProperties(data[1]),
            init_segment: data[2],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSegmentMode {
    pub pid: PidSlaveMaster,
    pub mode: SegmentMode,
}

impl XcpResponse for RespGetSegmentMode {
    const SIZE: usize = 3;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            mode: SegmentMode(data[2]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespStartStopDaqList {
    pub pid: PidSlaveMaster,
    pub first_pid: u8,
}

impl XcpResponse for RespStartStopDaqList {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            first_pid: data[1],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqClock {
    pub pid: PidSlaveMaster,
    pub trigger_info: u8,
    pub payload_fmt: PayloadFmt,
    pub timestamp: u32,
}

impl RespGetDaqClock {
    pub fn trigger_initiator(&self) -> Option<TriggerInitiator> {
        TriggerInitiator::from_u8(self.trigger_info & 7)
    }

    pub fn time_of_ts_sampling(&self) -> Option<TimeOfTsSampling> {
        TimeOfTsSampling::from_u8((self.trigger_info >> 3) & 3)
    }
}

impl XcpResponse for RespGetDaqClock {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            trigger_info: data[2],
            payload_fmt: PayloadFmt(data[3]),
            timestamp: swap32(u32_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqProcessorInfo {
    pub pid: PidSlaveMaster,
    pub properties: DaqProperties,
    pub max_daq: u16,
    pub max_event_channel: u16,
    pub min_daq: u8,
    pub daq_key_byte: DaqKeyByte,
}

impl XcpResponse for RespGetDaqProcessorInfo {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: DaqProperties(data[1]),
            max_daq: swap16(u16_le(data, 2), swap),
            max_event_channel: swap16(u16_le(data, 4), swap),
            min_daq: data[6],
            daq_key_byte: DaqKeyByte(data[7]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqResolutionInfo {
    pub pid: PidSlaveMaster,
    pub granularity_odt_entry_size_daq: crate::ifdata_xcp::XcpOdtEntrySize,
    pub max_odt_entry_size_daq: u8,
    pub granularity_odt_entry_size_stim: crate::ifdata_xcp::XcpOdtEntrySize,
    pub max_odt_entry_size_stim: u8,
    pub timestamp_mode: DaqTimestampMode,
    pub timestamp_ticks: u16,
}

impl RespGetDaqResolutionInfo {
    pub fn ts_size(&self) -> u8 {
        self.timestamp_mode.0 & 0x07
    }
}

impl XcpResponse for RespGetDaqResolutionInfo {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            granularity_odt_entry_size_daq: odt_entry_size_from_u8(data[1]),
            max_odt_entry_size_daq: data[2],
            granularity_odt_entry_size_stim: odt_entry_size_from_u8(data[3]),
            max_odt_entry_size_stim: data[4],
            timestamp_mode: DaqTimestampMode(data[5]),
            timestamp_ticks: swap16(u16_le(data, 6), swap),
        })
    }
}

fn odt_entry_size_from_u8(v: u8) -> crate::ifdata_xcp::XcpOdtEntrySize {
    match v {
        2 => crate::ifdata_xcp::XcpOdtEntrySize::WORD,
        4 => crate::ifdata_xcp::XcpOdtEntrySize::DWORD,
        8 => crate::ifdata_xcp::XcpOdtEntrySize::DLONG,
        _ => crate::ifdata_xcp::XcpOdtEntrySize::BYTE,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqListMode {
    pub pid: PidSlaveMaster,
    pub mode: DaqListMode,
    pub event_channel_no: u16,
    pub prescaler: u8,
    pub priority: u8,
}

impl XcpResponse for RespGetDaqListMode {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            mode: DaqListMode(data[1]),
            event_channel_no: swap16(u16_le(data, 4), swap),
            prescaler: data[6],
            priority: data[7],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqListInfo {
    pub pid: PidSlaveMaster,
    pub properties: DaqListProperties,
    pub max_odt: u8,
    pub max_odt_entries: u8,
    pub fixed_event: u16,
}

impl XcpResponse for RespGetDaqListInfo {
    const SIZE: usize = 6;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: DaqListProperties(data[1]),
            max_odt: data[2],
            max_odt_entries: data[3],
            fixed_event: swap16(u16_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqEventInfo {
    pub pid: PidSlaveMaster,
    pub properties: DaqEventProperties,
    pub max_daq_list: u8,
    pub name_length: u8,
    pub time_cycle: u8,
    pub time_unit: XcpTimestampResolution,
    pub priority: u8,
}

impl XcpResponse for RespGetDaqEventInfo {
    const SIZE: usize = 7;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: DaqEventProperties(data[1]),
            max_daq_list: data[2],
            name_length: data[3],
            time_cycle: data[4],
            time_unit: ts_resolution_from_u8(data[5]),
            priority: data[6],
        })
    }
}

fn ts_resolution_from_u8(v: u8) -> XcpTimestampResolution {
    match v {
        0 => XcpTimestampResolution::_1NS,
        1 => XcpTimestampResolution::_10NS,
        2 => XcpTimestampResolution::_100NS,
        3 => XcpTimestampResolution::_1US,
        4 => XcpTimestampResolution::_10US,
        5 => XcpTimestampResolution::_100US,
        6 => XcpTimestampResolution::_1MS,
        7 => XcpTimestampResolution::_10MS,
        8 => XcpTimestampResolution::_100MS,
        9 => XcpTimestampResolution::_1S,
        10 => XcpTimestampResolution::_1PS,
        11 => XcpTimestampResolution::_10PS,
        12 => XcpTimestampResolution::_100PS,
        _ => XcpTimestampResolution::NotSet,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespReadDaq {
    pub pid: PidSlaveMaster,
    pub bit_offset: u8,
    pub element_size: u8,
    pub address_extension: u8,
    pub address: u32,
}

impl XcpResponse for RespReadDaq {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            bit_offset: data[1],
            element_size: data[2],
            address_extension: data[3],
            address: swap32(u32_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespDtoCtrResp {
    pub pid: PidSlaveMaster,
    pub properties: DtoCtrProperties,
    pub current_related_event_channel_no: u16,
    pub mode: DtoCtrMode,
}

impl XcpResponse for RespDtoCtrResp {
    const SIZE: usize = 5;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: DtoCtrProperties(data[1]),
            current_related_event_channel_no: swap16(u16_le(data, 2), swap),
            mode: DtoCtrMode::from_u8(data[4]),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetPgmProcessorInfo {
    pub pid: PidSlaveMaster,
    pub properties: ProgramProperties,
    pub max_sector: u8,
}

impl XcpResponse for RespGetPgmProcessorInfo {
    const SIZE: usize = 3;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            properties: ProgramProperties(data[1]),
            max_sector: data[2],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSectorInfoModeAddressOrLen {
    pub pid: PidSlaveMaster,
    pub clear_sequence_no: u8,
    pub program_sequence_no: u8,
    pub programming_method: u8,
    pub sector_info: u32,
}

impl XcpResponse for RespGetSectorInfoModeAddressOrLen {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            clear_sequence_no: data[1],
            program_sequence_no: data[2],
            programming_method: data[3],
            sector_info: swap32(u32_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetSectorInfoModeSectorNameLen {
    pub pid: PidSlaveMaster,
    pub sector_name_len: u8,
}

impl XcpResponse for RespGetSectorInfoModeSectorNameLen {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            sector_name_len: data[1],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespProgramStart {
    pub pid: PidSlaveMaster,
    pub comm_mode: CommModeProgram,
    pub max_cto: u8,
    pub max_bs: u8,
    pub min_st: u8,
    pub queue_size: u8,
}

impl XcpResponse for RespProgramStart {
    const SIZE: usize = 7;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            comm_mode: CommModeProgram(data[2]),
            max_cto: data[3],
            max_bs: data[4],
            min_st: data[5],
            queue_size: data[6],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespGetDaqListUsbEndpoint {
    pub pid: PidSlaveMaster,
    pub type_: UsbEndpointType,
    pub endpoint_no: u8,
}

impl XcpResponse for RespGetDaqListUsbEndpoint {
    const SIZE: usize = 5;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            type_: UsbEndpointType::from_u8(data[1])?,
            endpoint_no: data[4],
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespTimeCorrelation {
    pub pid: PidSlaveMaster,
    pub slave_config: SlaveConfig,
    pub observable_clocks: ObservableClocks,
    pub sync_state: SyncState,
    pub clock_info: ClockInfo,
    pub cluster_id: u16,
}

impl XcpResponse for RespTimeCorrelation {
    const SIZE: usize = 8;

    fn decode(data: &[u8], swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            slave_config: SlaveConfig(data[1]),
            observable_clocks: ObservableClocks(data[2]),
            sync_state: SyncState(data[3]),
            clock_info: ClockInfo(data[4]),
            cluster_id: swap16(u16_le(data, 6), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespEvent {
    pub pid: PidSlaveMaster,
    pub event_code: EventCodes,
}

impl XcpResponse for RespEvent {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            event_code: EventCodes::from_u8(data[1]).unwrap_or(EventCodes::ResumeMode),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespService {
    pub pid: PidSlaveMaster,
    pub service_request_code: ServiceRequestCode,
}

impl XcpResponse for RespService {
    const SIZE: usize = 2;

    fn decode(data: &[u8], _swap: bool) -> Option<Self> {
        let pid = decode_pid(data, Self::SIZE)?;
        Some(Self {
            pid,
            service_request_code: ServiceRequestCode::from_u8(data[1])?,
        })
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventBase {
    pub pid: PidSlaveMaster,
    pub event_code: EventCodes,
}

impl EventBase {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let pid = decode_pid(data, 2)?;
        Some(Self {
            pid,
            event_code: EventCodes::from_u8(data[1]).unwrap_or(EventCodes::ResumeMode),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventResumeMode {
    pub base: EventBase,
    pub session_id: u16,
}

impl EventResumeMode {
    pub const SIZE: usize = 4;

    pub fn decode(data: &[u8], swap: bool) -> Option<Self> {
        Some(Self {
            base: EventBase::decode(data)?,
            session_id: swap16(u16_le(data, 2), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventResumeModeTs {
    pub base: EventResumeMode,
    pub current_timestamp: u32,
}

impl EventResumeModeTs {
    pub const SIZE: usize = 8;

    pub fn decode(data: &[u8], swap: bool) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            base: EventResumeMode::decode(data, swap)?,
            current_timestamp: swap32(u32_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventStimTimeout {
    pub base: EventBase,
    pub mode: StimTimeoutMode,
    pub list_no: u16,
}

impl EventStimTimeout {
    pub const SIZE: usize = 6;

    pub fn decode(data: &[u8], swap: bool) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            base: EventBase::decode(data)?,
            mode: match data[2] {
                1 => StimTimeoutMode::DaqListNo,
                _ => StimTimeoutMode::EventChannelNo,
            },
            list_no: swap16(u16_le(data, 4), swap),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventTimeSync {
    pub base: EventBase,
    pub flags: u8,
    pub payload_fmt: PayloadFmt,
    pub timestamp: u32,
}

impl EventTimeSync {
    pub const SIZE: usize = 8;

    pub fn decode(data: &[u8], swap: bool) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            base: EventBase::decode(data)?,
            flags: data[2],
            payload_fmt: PayloadFmt(data[3]),
            timestamp: swap32(u32_le(data, 4), swap),
        })
    }

    pub fn trigger_initiator(&self) -> Option<TriggerInitiator> {
        TriggerInitiator::from_u8(self.flags & 7)
    }

    pub fn time_of_ts_sampling(&self) -> Option<TimeOfTsSampling> {
        TimeOfTsSampling::from_u8((self.flags >> 3) & 3)
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone)]
pub struct XcpFrame {
    pub frame: Frame,
    pub type_: XcpType,
    pub source: String,
    pub len: u16,
    pub ctr: u16,
    pub frame_fmt: XcpHeaderLen,
}

impl XcpFrame {
    pub const RESPONSE_INDEX: usize = 0;
    pub const EVENT_CODE_INDEX: usize = 1;

    pub fn new(
        type_: XcpType,
        source: impl Into<String>,
        data: &[u8],
        is_master_frame: bool,
        frame_fmt: XcpHeaderLen,
    ) -> Self {
        let source = source.into();
        let hdr = header_len_size(frame_fmt);
        let mut len = data.len() as u16;
        let mut ctr = 0u16;
        let payload: Vec<u8> = if frame_fmt == XcpHeaderLen::NotSet || data.len() < hdr {
            data.to_vec()
        } else {
            match frame_fmt {
                XcpHeaderLen::BYTE | XcpHeaderLen::FILL_BYTE => len = data[0] as u16,
                XcpHeaderLen::CTR_BYTE => {
                    len = data[0] as u16;
                    ctr = data[1] as u16;
                }
                XcpHeaderLen::WORD | XcpHeaderLen::FILL_WORD => len = u16_le(data, 0),
                XcpHeaderLen::CTR_WORD => {
                    len = u16_le(data, 0);
                    ctr = u16_le(data, 2);
                }
                _ => {}
            }
            let avail = data.len() - hdr;
            let n = (len as usize).min(avail);
            data[hdr..hdr + n].to_vec()
        };
        if frame_fmt == XcpHeaderLen::NotSet || data.len() < hdr {
            len = payload.len() as u16;
        }
        Self {
            frame: Frame::new(payload, is_master_frame),
            type_,
            source,
            len,
            ctr,
            frame_fmt,
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.frame.data
    }

    pub fn address(&self) -> String {
        if !self.source.is_empty() {
            format!(
                "{} {} {}",
                self.frame.rw_indicator(),
                self.type_,
                self.source
            )
        } else {
            self.type_.to_string()
        }
    }

    pub fn is_daq(&self) -> bool {
        !self.frame.is_master_frame && !self.frame.data.is_empty() && self.frame.data[0] < 0xFC
    }

    pub fn is_error(&self) -> bool {
        !self.frame.is_master_frame && !self.frame.data.is_empty() && self.frame.data[0] == 0xFE
    }

    pub fn type_str(&self) -> String {
        if self.frame.is_master_frame {
            return match CommandCode::from_u8(self.frame.data[0]) {
                Some(code) => code.cs_name().to_string(),
                None => (self.frame.data[0]).to_string(),
            };
        }
        if self.is_daq() {
            return "DAQ".to_string();
        }
        if !self.frame.data.is_empty() {
            let pid = PidSlaveMaster::from_u8(self.frame.data[0]);
            match pid {
                Some(PidSlaveMaster::Err) if self.frame.data.len() > 1 => {
                    return format!(
                        "{}({})",
                        PidSlaveMaster::Err,
                        CmdResult::from_wire_u8(self.frame.data[1]).cs_name()
                    );
                }
                Some(PidSlaveMaster::Ev) if self.frame.data.len() > 1 => {
                    let name = EventCodes::from_u8(self.frame.data[1]).map_or_else(
                        || (self.frame.data[1]).to_string(),
                        |e| e.cs_name().to_string(),
                    );
                    return format!("{}({})", PidSlaveMaster::Ev, name);
                }
                Some(p) => return p.cs_name().to_string(),
                None => return "Unknown".to_string(),
            }
        }
        "Unknown".to_string()
    }

    pub fn raw_frame_length(&self) -> usize {
        let mut n = self.frame.data.len();
        if !self.frame.is_master_frame {
            match self.type_ {
                XcpType::Udp | XcpType::Tcp | XcpType::Usb | XcpType::Sxi => {
                    n += header_len_size(self.frame_fmt);
                }
                _ => {}
            }
        }
        n * 8
    }

    pub fn to_csv(&self) -> String {
        self.format_line(';')
    }

    pub fn to_clipboard(&self) -> String {
        self.format_line('\t')
    }

    fn format_line(&self, sep: char) -> String {
        let ctr = if self.type_ != XcpType::Can {
            self.ctr.to_string()
        } else {
            "-".to_string()
        };
        let ascii = self
            .frame
            .data_ascii_str(if sep == ';' { '"' } else { '\t' });
        if sep == ';' {
            format!(
                "{};\"{}\";{};\"{}\";\"{}\";\"{}\";\"{}\"",
                self.frame.time_str(0.0),
                self.address(),
                self.frame.data.len(),
                ctr,
                self.type_str(),
                self.frame.data_str(),
                ascii
            )
        } else {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                self.frame.time_str(0.0),
                self.address(),
                self.frame.data.len(),
                ctr,
                self.type_str(),
                self.frame.data_str(),
                ascii
            )
        }
    }
}

impl fmt::Display for XcpFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.frame.data[0];
        let text = if self.frame.is_master_frame {
            match CommandCode::from_u8(b) {
                Some(code) => code.cs_name().to_string(),
                None => b.to_string(),
            }
        } else if b >= 0xFC {
            PidSlaveMaster::from_u8(b)
                .unwrap_or(PidSlaveMaster::Res)
                .cs_name()
                .to_string()
        } else {
            format!("DAQ {b:02X}")
        };
        write!(
            f,
            "{} - Len:{}, Ctr:{}, {} from {}",
            self.frame.time_str(0.0),
            self.len,
            self.ctr,
            text,
            self.address()
        )
    }
}

// ============================================================================
// ============================================================================

pub(crate) fn build_checksum_add(type_: ChecksumType, data: &[u8]) -> (u32, u8) {
    match type_ {
        ChecksumType::ADD_11 => {
            let mut sum: u8 = 0;
            for &b in data {
                sum = sum.wrapping_add(b);
            }
            (sum as u32, 1)
        }
        ChecksumType::ADD_12 => {
            let mut sum: u16 = 0;
            for &b in data {
                sum = sum.wrapping_add(b as u16);
            }
            (sum as u32, 2)
        }
        _ => unreachable!("SxI checksums only support ADD_11 and ADD_12"),
    }
}

// ============================================================================
// ============================================================================

pub struct XcpReceiveBuffer {
    frame_fmt: XcpHeaderLen,
    alignment: XcpAlignment,
    header_size: usize,
    min_frame: usize,
    checksum_len: usize,
    checksum_sxi: ChecksumSxi,
    checksum_type: ChecksumType,
    framing: Option<(u8, u8)>,
    buf: Vec<u8>,
    raw: Vec<u8>,
    skip: usize,
    state: u8,
}

impl XcpReceiveBuffer {
    /// [`Error::Protocol`].
    pub fn new(frame_fmt: XcpHeaderLen, alignment: XcpAlignment) -> Result<Self> {
        if frame_fmt == XcpHeaderLen::NotSet {
            return Err(Error::Protocol(
                "XCP_HEADER_LEN.NotSet not supported for receive buffer".to_string(),
            ));
        }
        let header_size = header_len_size(frame_fmt);
        Ok(Self {
            frame_fmt,
            alignment,
            header_size,
            min_frame: header_size + 1,
            checksum_len: 0,
            checksum_sxi: ChecksumSxi::NO_CHECKSUM,
            checksum_type: ChecksumType::ADD_11,
            framing: None,
            buf: Vec::new(),
            raw: Vec::new(),
            skip: 0,
            state: 0,
        })
    }

    pub fn new_sxi(sxi: &XcpOnSxi) -> Result<Self> {
        let mut buf = Self::new(sxi.header_len, XcpAlignment::_8_BIT)?;
        buf.checksum_sxi = sxi.checksum;
        buf.framing = sxi.children.iter().find_map(|n| match n {
            XcpNode::Framing(f) => Some((f.sync, f.esc)),
            _ => None,
        });
        if sxi.checksum != ChecksumSxi::NO_CHECKSUM && sxi.checksum != ChecksumSxi::NotSet {
            buf.checksum_len = checksum_sxi_len(sxi.checksum);
            buf.min_frame += buf.checksum_len;
            buf.checksum_type = if sxi.checksum == ChecksumSxi::CHECKSUM_BYTE {
                ChecksumType::ADD_11
            } else {
                ChecksumType::ADD_12
            };
        }
        Ok(buf)
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.raw.clear();
        self.skip = 0;
        self.state = 0;
    }

    fn verify_checksum(&self, data: &[u8], total: usize) -> bool {
        let body = &data[..total - self.checksum_len];
        match self.checksum_sxi {
            ChecksumSxi::CHECKSUM_BYTE => {
                let (sum, _) = build_checksum_add(self.checksum_type, body);
                data[total - 1] == sum as u8
            }
            ChecksumSxi::CHECKSUM_WORD => {
                let (sum, _) = build_checksum_add(self.checksum_type, body);
                u16_le(data, total - 2) == sum as u16
            }
            _ => true,
        }
    }

    fn try_extract(&mut self, type_: XcpType, source: &str) -> Option<XcpFrame> {
        if self.buf.len() < self.min_frame + self.skip {
            return None;
        }
        let avail = self.buf.len() - self.skip;
        let array = &self.buf[self.skip..];
        let len_field = match self.frame_fmt {
            XcpHeaderLen::BYTE | XcpHeaderLen::CTR_BYTE | XcpHeaderLen::FILL_BYTE => {
                array[0] as usize
            }
            XcpHeaderLen::WORD | XcpHeaderLen::CTR_WORD | XcpHeaderLen::FILL_WORD => {
                u16_le(array, 0) as usize
            }
            _ => 0,
        };
        let total = len_field + self.header_size + self.checksum_len;
        if total < self.min_frame {
            self.buf.clear();
            return None;
        }
        if avail < total {
            return None;
        }
        let frame_bytes: Vec<u8> = self.buf[self.skip..self.skip + total].to_vec();
        self.buf.drain(..total);
        if self.checksum_sxi != ChecksumSxi::NO_CHECKSUM
            && self.checksum_sxi != ChecksumSxi::NotSet
            && !self.verify_checksum(&frame_bytes, total)
        {
            return None;
        }
        let payload_end = total - self.checksum_len;
        let frame = XcpFrame::new(
            type_,
            source,
            &frame_bytes[..payload_end],
            false,
            self.frame_fmt,
        );
        match self.alignment {
            XcpAlignment::_16_BIT | XcpAlignment::_32_BIT | XcpAlignment::_64_BIT => {
                self.skip = align_up(total, self.alignment as usize) - total;
            }
            _ => {}
        }
        Some(frame)
    }

    pub fn get_frame_from_data(
        &mut self,
        type_: XcpType,
        source: &str,
        data: &[u8],
    ) -> Option<XcpFrame> {
        let Some((sync, esc)) = self.framing else {
            self.buf.extend_from_slice(data);
            return self.try_extract(type_, source);
        };
        self.raw.extend_from_slice(data);
        let mut frame = None;
        let mut consumed = 0;
        for &b in &self.raw.clone() {
            consumed += 1;
            match self.state {
                0 => {
                    if b == sync {
                        self.state = 2;
                    }
                    continue;
                }
                1 => {
                    if b != sync && b != esc {
                        self.state = 0;
                        continue;
                    }
                    self.state = 2;
                }
                _ => {
                    if b == esc {
                        self.state = 1;
                        continue;
                    }
                }
            }
            self.buf.push(b);
            if let Some(f) = self.try_extract(type_, source) {
                frame = Some(f);
                self.state = 0;
                break;
            }
        }
        self.raw.drain(..consumed);
        frame
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SxiParity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SxiStopBits {
    #[default]
    One,
    Two,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SxiHandshake {
    #[default]
    None,
    XOnXOff,
    RequestToSend,
    /// RTS + XON/XOFF.
    RequestToSendXOnXOff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SxiSerialConfig {
    pub port: String,
    pub baudrate: u32,
    pub parity: SxiParity,
    pub data_bits: u8,
    pub stop_bits: SxiStopBits,
    pub read_buffer_size: usize,
    pub write_buffer_size: usize,
    pub read_timeout_ms: i32,
    pub write_timeout_ms: i32,
    pub handshake: SxiHandshake,
}

impl SxiSerialConfig {
    pub fn new(
        sxi: &XcpOnSxi,
        com_port: impl Into<String>,
        timeout_ms: i32,
        handshake: SxiHandshake,
    ) -> Self {
        let (parity, stop_bits) = match &sxi.duplex_mode {
            Some(dm) => {
                let parity = match dm.parity {
                    crate::ifdata_xcp::ParityType::EVEN => SxiParity::Even,
                    crate::ifdata_xcp::ParityType::ODD => SxiParity::Odd,
                    _ => SxiParity::None,
                };
                let stop_bits = match dm.stop_bits {
                    crate::ifdata_xcp::StopBitsType::ONE_STOP_BIT => SxiStopBits::One,
                    _ => SxiStopBits::Two,
                };
                (parity, stop_bits)
            }
            None => (SxiParity::None, SxiStopBits::One),
        };
        Self {
            port: com_port.into(),
            baudrate: sxi.baudrate,
            parity,
            data_bits: 8,
            stop_bits,
            read_buffer_size: 65536,
            write_buffer_size: 65536,
            read_timeout_ms: timeout_ms,
            write_timeout_ms: timeout_ms,
            handshake,
        }
    }
}

pub type SxiDataCallback = Box<dyn FnMut(&[u8]) + Send>;

pub struct SxiCore {
    pub port: String,
    pub sxi: XcpOnSxi,
    framing: Option<(u8, u8)>,
    recv: XcpReceiveBuffer,
    callback: Option<SxiDataCallback>,
}

impl SxiCore {
    pub fn new(port: impl Into<String>, sxi: XcpOnSxi) -> Result<Self> {
        let recv = XcpReceiveBuffer::new_sxi(&sxi)?;
        let framing = sxi.children.iter().find_map(|n| match n {
            XcpNode::Framing(f) => Some((f.sync, f.esc)),
            _ => None,
        });
        Ok(Self {
            port: port.into(),
            sxi,
            framing,
            recv,
            callback: None,
        })
    }

    pub fn set_data_callback(&mut self, callback: SxiDataCallback) {
        self.callback = Some(callback);
    }

    pub fn reset(&mut self) {
        self.recv.reset();
    }

    fn append_checksum(&self, data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        match self.sxi.checksum {
            ChecksumSxi::CHECKSUM_BYTE => {
                let (sum, _) = build_checksum_add(ChecksumType::ADD_11, data);
                out.push(sum as u8);
            }
            ChecksumSxi::CHECKSUM_WORD => {
                let (sum, _) = build_checksum_add(ChecksumType::ADD_12, data);
                out.extend_from_slice(&(sum as u16).to_le_bytes());
            }
            _ => {}
        }
        out
    }

    pub fn send_msg(&mut self, data: &[u8], mut send: impl FnMut(&[u8]) -> usize) -> usize {
        let payload = self.append_checksum(data);
        let Some((sync, esc)) = self.framing else {
            return send(&payload);
        };
        let mut framed = Vec::with_capacity(payload.len() + 4);
        framed.push(sync);
        for &b in &payload {
            if b == sync || b == esc {
                framed.push(esc);
            }
            framed.push(b);
        }
        if send(&framed) != framed.len() {
            return 0;
        }
        data.len()
    }

    pub fn on_received(&mut self, chunk: &[u8]) {
        if let Some(cb) = self.callback.as_mut() {
            cb(chunk);
        }
    }

    pub fn get_frame_from_data(
        &mut self,
        type_: XcpType,
        source: &str,
        data: &[u8],
    ) -> Option<XcpFrame> {
        self.recv.get_frame_from_data(type_, source, data)
    }
}

pub trait SxiDevice {
    fn core(&self) -> &SxiCore;
    fn core_mut(&mut self) -> &mut SxiCore;

    fn is_available(&mut self) -> bool;
    fn open(&mut self) -> bool;
    fn reset(&mut self);
    fn close(&mut self);
    fn send(&mut self, data: &[u8]) -> usize;

    fn port(&self) -> &str {
        &self.core().port
    }

    fn send_msg(&mut self, data: &[u8]) -> usize {
        let payload = self.core().append_checksum(data);
        let framing = self.core().framing;
        match framing {
            None => self.send(&payload),
            Some((sync, esc)) => {
                let mut framed = Vec::with_capacity(payload.len() + 4);
                framed.push(sync);
                for &b in &payload {
                    if b == sync || b == esc {
                        framed.push(esc);
                    }
                    framed.push(b);
                }
                if self.send(&framed) != framed.len() {
                    return 0;
                }
                data.len()
            }
        }
    }

    fn get_frame_from_data(
        &mut self,
        type_: XcpType,
        source: &str,
        data: &[u8],
    ) -> Option<XcpFrame> {
        self.core_mut().get_frame_from_data(type_, source, data)
    }
}

pub trait SxiSerialIo {
    fn is_open(&self) -> bool;
    fn bytes_to_read(&mut self) -> usize;
    fn read(&mut self, buf: &mut [u8]) -> usize;
    fn bytes_to_write(&mut self) -> usize;
    fn write(&mut self, data: &[u8]);
    fn discard_buffers(&mut self);
}

pub type SxiOpenFn<IO> = Box<dyn Fn(&SxiSerialConfig) -> Option<IO> + Send>;

pub struct SerialPortDevice<IO: SxiSerialIo> {
    core: SxiCore,
    io: Option<IO>,
    pub config: SxiSerialConfig,
    open_fn: SxiOpenFn<IO>,
    timeout_ms: i32,
}

impl<IO: SxiSerialIo> SerialPortDevice<IO> {
    pub fn new(
        port: impl Into<String>,
        sxi: XcpOnSxi,
        handshake: SxiHandshake,
        timeout_ms: i32,
        open_fn: impl Fn(&SxiSerialConfig) -> Option<IO> + Send + 'static,
    ) -> Result<Self> {
        let config = SxiSerialConfig::new(&sxi, port.into(), timeout_ms, handshake);
        let core = SxiCore::new(config.port.clone(), sxi)?;
        Ok(Self {
            core,
            io: None,
            config,
            open_fn: Box::new(open_fn),
            timeout_ms,
        })
    }

    pub fn poll_once(&mut self) -> bool {
        let Some(io) = self.io.as_mut() else {
            return false;
        };
        let mut buf = vec![0u8; 65536];
        let mut n = 0;
        while io.bytes_to_read() > 0 && n < buf.len() {
            let got = io.read(&mut buf[n..]);
            if got == 0 {
                break;
            }
            n += got;
        }
        if n > 0 {
            self.core.on_received(&buf[..n]);
            true
        } else {
            false
        }
    }
}

impl<IO: SxiSerialIo> SxiDevice for SerialPortDevice<IO> {
    fn core(&self) -> &SxiCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut SxiCore {
        &mut self.core
    }

    fn is_available(&mut self) -> bool {
        self.io.as_ref().is_some_and(SxiSerialIo::is_open)
    }

    fn open(&mut self) -> bool {
        self.io = (self.open_fn)(&self.config);
        if self.io.is_some() {
            self.core.reset();
            true
        } else {
            false
        }
    }

    fn reset(&mut self) {
        self.core.reset();
        if let Some(io) = self.io.as_mut() {
            io.discard_buffers();
        }
    }

    fn close(&mut self) {
        self.io = None;
    }

    // TODO(hardware): route serial waits through spawn_blocking if executor stall becomes an issue
    fn send(&mut self, data: &[u8]) -> usize {
        let Some(io) = self.io.as_mut() else {
            return 0;
        };
        if !io.is_open() {
            return 0;
        }
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms.max(0) as u64);
        while io.bytes_to_write() > 0 {
            if Instant::now() >= deadline {
                return 0;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        io.write(data);
        data.len()
    }
}

// ============================================================================
// ============================================================================

#[async_trait]
pub trait XcpTransport {
    fn source(&self) -> &str;

    fn frame_fmt(&self) -> XcpHeaderLen;

    fn reset(&mut self);

    async fn send_bytes(&mut self, data: &[u8]) -> usize;

    fn next_frame(&mut self) -> Option<XcpFrame>;

    async fn poll(&mut self) {}
}

pub struct CanXcpTransport {
    device: Arc<Mutex<dyn CanDevice + Send>>,
    source: String,
    send_id: u32,
    send_as_can20: bool,
    max_dlc: usize,
    listener_token: u64,
    queue: Arc<Mutex<std::collections::VecDeque<XcpFrame>>>,
}

impl CanXcpTransport {
    pub fn new(
        device: Arc<Mutex<dyn CanDevice + Send>>,
        master_source: impl Into<String>,
        xcp_can: &XcpOnCan,
    ) -> Result<Self> {
        let send_id = if xcp_can.can_id_cmd != u32::MAX {
            xcp_can.can_id_cmd
        } else if xcp_can.can_id_broadcast != u32::MAX {
            xcp_can.can_id_broadcast
        } else {
            return Err(Error::Protocol(
                "XCP_ON_CAN Command and Braodcast ID undefined!".to_string(),
            ));
        };
        let can_fd = xcp_can.children.iter().find_map(|n| match n {
            XcpNode::CanFd(fd) => Some(fd),
            _ => None,
        });
        let (send_as_can20, max_dlc) = match can_fd {
            None => (true, if xcp_can.max_dlc_required { 8 } else { 0 }),
            Some(fd) => (
                false,
                if fd.max_dlc_required {
                    fd.max_dlc as usize
                } else {
                    0
                },
            ),
        };
        let resp_masked = xcp_can.can_id_resp & 0x9FFF_FFFF;
        let mut ids: Vec<u32> = Vec::new();
        if xcp_can.can_id_resp != u32::MAX {
            ids.push(resp_masked);
        }
        for n in &xcp_can.children {
            match n {
                XcpNode::DaqListCanId(d)
                    if d.daq_list_type == crate::ifdata_xcp::XcpDaqListCanType::FIXED =>
                {
                    ids.push(d.can_id & 0x9FFF_FFFF);
                }
                XcpNode::EventCanIdList(e) => {
                    for &id in &e.fixed_can_ids {
                        ids.push(id & 0x9FFF_FFFF);
                    }
                }
                _ => {}
            }
        }
        let source = master_source.into();
        let queue: Arc<Mutex<std::collections::VecDeque<XcpFrame>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let cb_queue = Arc::clone(&queue);
        let cb_source = source.clone();
        let token = device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .register_listener(
                Some(&ids),
                Box::new(move |frame: &CanFrame| {
                    let src = if frame.id == resp_masked
                        && frame.data.first().is_some_and(|&b| b >= 0xFC)
                    {
                        cb_source.clone()
                    } else {
                        autors_can::device::to_can_id_string(frame.id)
                    };
                    let xcp_frame =
                        XcpFrame::new(XcpType::Can, src, &frame.data, false, XcpHeaderLen::NotSet);
                    cb_queue
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push_back(xcp_frame);
                }),
            );
        Ok(Self {
            device,
            source,
            send_id,
            send_as_can20,
            max_dlc,
            listener_token: token,
            queue,
        })
    }

    /// Unregisters the CAN listener (the disposal counterpart: `unregisterClient`).
    pub fn shutdown(&mut self) {
        self.device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unregister_listener(self.listener_token);
    }
}

impl Drop for CanXcpTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[async_trait]
impl XcpTransport for CanXcpTransport {
    fn source(&self) -> &str {
        &self.source
    }

    fn frame_fmt(&self) -> XcpHeaderLen {
        XcpHeaderLen::NotSet
    }

    fn reset(&mut self) {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// `sendMsg(sendId, data, sendAsCAN20, maxDLC, 0)`.
    async fn send_bytes(&mut self, data: &[u8]) -> usize {
        let device = Arc::clone(&self.device);
        let data = data.to_vec();
        let (send_id, send_as_can20, max_dlc) = (self.send_id, self.send_as_can20, self.max_dlc);
        autors_runtime::spawn_blocking(move || {
            autors_runtime::block_on(device.lock().unwrap_or_else(|p| p.into_inner()).send_msg(
                send_id,
                &data,
                send_as_can20,
                max_dlc,
                0,
            ))
            .unwrap_or(0)
        })
        .await
    }

    fn next_frame(&mut self) -> Option<XcpFrame> {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
    }

    async fn poll(&mut self) {
        let device = Arc::clone(&self.device);
        autors_runtime::spawn_blocking(move || loop {
            let got = autors_runtime::block_on(
                device.lock().unwrap_or_else(|p| p.into_inner()).poll_once(),
            )
            .unwrap_or(false);
            if !got {
                break;
            }
        })
        .await;
    }
}

// ============================================================================
// ============================================================================

/// BUILD_CHECKSUM→T2(1),PROGRAM_START/PREPARE/VERIFY→T3(2),PROGRAM_CLEAR→T4(3),
fn cmd_timing_index(code: CommandCode) -> usize {
    match code {
        CommandCode::BuildChecksum => 1,
        CommandCode::ProgramStart | CommandCode::ProgramPrepare | CommandCode::ProgramVerify => 2,
        CommandCode::ProgramClear => 3,
        CommandCode::Program
        | CommandCode::ProgramReset
        | CommandCode::ProgramNext
        | CommandCode::ProgramMax => 4,
        _ => 0,
    }
}

#[derive(Debug)]
pub(crate) enum PendingDispatch {
    Event(XcpFrame),
    Service(XcpFrame),
    Daq(XcpFrame),
    Error(CmdResult, CommandCode),
}

pub struct XcpMasterBase {
    pub base: CommMaster,
    pub type_: XcpType,
    pub port: i32,
    pub address: Option<std::net::IpAddr>,
    pub protocol_layer: XcpProtocolLayer,
    frame_fmt: XcpHeaderLen,
    ctr: u16,
    max_cto: u8,
    max_dto: u16,
    last_response: Option<XcpFrame>,
    last_error: CmdResult,
    mag: usize,
    transport: Option<Box<dyn XcpTransport + Send>>,
    pending: std::collections::VecDeque<PendingDispatch>,
}

impl XcpMasterBase {
    fn new_common(
        connect_behaviour: ConnectBehaviourType,
        type_: XcpType,
        protocol_layer: XcpProtocolLayer,
        frame_fmt: XcpHeaderLen,
        name: String,
        transport: Box<dyn XcpTransport + Send>,
    ) -> Self {
        let mut base = CommMaster::new(connect_behaviour);
        base.name = name;
        Self {
            base,
            type_,
            port: 0,
            address: None,
            protocol_layer,
            frame_fmt,
            ctr: 0,
            max_cto: 0,
            max_dto: 0,
            last_response: None,
            last_error: CmdResult::OK,
            mag: 0,
            transport: Some(transport),
            pending: std::collections::VecDeque::new(),
        }
    }

    pub fn new_can(
        connect_behaviour: ConnectBehaviourType,
        xcp_can: &XcpOnCan,
        protocol_layer: XcpProtocolLayer,
        can_device_name: &str,
        transport: Box<dyn XcpTransport + Send>,
    ) -> Self {
        let name = format!(
            "{} {}",
            can_device_name,
            autors_can::device::to_can_id_string(xcp_can.can_id_resp)
        );
        Self::new_common(
            connect_behaviour,
            XcpType::Can,
            protocol_layer,
            XcpHeaderLen::NotSet,
            name,
            transport,
        )
    }

    pub fn new_sxi(
        connect_behaviour: ConnectBehaviourType,
        xcp_sxi: &XcpOnSxi,
        protocol_layer: XcpProtocolLayer,
        com_port: &str,
        transport: Box<dyn XcpTransport + Send>,
    ) -> Self {
        Self::new_common(
            connect_behaviour,
            XcpType::Sxi,
            protocol_layer,
            xcp_sxi.header_len,
            format!("XCP_ON_SxI using ({com_port})"),
            transport,
        )
    }

    /// [`crate::eth_transport::TcpXcpTransport`]),[`XcpMaster::new_udp_tcp`]
    pub fn new_udp_tcp(
        connect_behaviour: ConnectBehaviourType,
        type_: XcpType,
        remote_address: &str,
        remote_port: i32,
        protocol_layer: XcpProtocolLayer,
        transport: Box<dyn XcpTransport + Send>,
    ) -> Result<Self> {
        let address: std::net::IpAddr = remote_address
            .parse()
            .map_err(|_| Error::Protocol(format!("Unknown host {remote_address}:{remote_port}")))?;
        let name = format!("{address}:{remote_port}");
        let mut m = Self::new_common(
            connect_behaviour,
            type_,
            protocol_layer,
            XcpHeaderLen::CTR_WORD,
            name,
            transport,
        );
        m.port = remote_port;
        m.address = Some(address);
        Ok(m)
    }

    pub fn without_transport(
        connect_behaviour: ConnectBehaviourType,
        type_: XcpType,
        protocol_layer: XcpProtocolLayer,
    ) -> Self {
        let mut base = CommMaster::new(connect_behaviour);
        base.name = String::new();
        Self {
            base,
            type_,
            port: 0,
            address: None,
            protocol_layer,
            frame_fmt: XcpHeaderLen::NotSet,
            ctr: 0,
            max_cto: 0,
            max_dto: 0,
            last_response: None,
            last_error: CmdResult::OK,
            mag: 0,
            transport: None,
            pending: std::collections::VecDeque::new(),
        }
    }

    pub fn timings(&self) -> [u16; 7] {
        self.protocol_layer.timings
    }

    pub fn max_cto(&self) -> u8 {
        self.max_cto
    }

    pub fn max_dto(&self) -> u16 {
        self.max_dto
    }

    pub fn address_granularity(&self) -> usize {
        self.mag
    }

    pub fn last_response(&self) -> Option<&XcpFrame> {
        self.last_response.as_ref()
    }

    pub fn last_error_response(&self) -> CmdResult {
        self.last_error
    }

    pub fn last_error_text(&self) -> String {
        self.last_error.to_string()
    }

    pub fn reset_last_error(&mut self) {
        self.last_error = CmdResult::OK;
    }

    pub fn alive_cycle_time(&self) -> i32 {
        self.protocol_layer.timings[0] as i32 + self.protocol_layer.timings[1] as i32
    }

    pub fn padding_size(ag: usize) -> usize {
        match ag {
            4 => 3,
            2 => 1,
            _ => 0,
        }
    }

    pub fn increase_ctr(&mut self, bit_length: usize) {
        self.base.increase_ctr(bit_length);
        self.ctr = self.ctr.wrapping_add(1);
    }

    pub fn frame_fmt(&self) -> XcpHeaderLen {
        self.frame_fmt
    }

    pub fn ctr(&self) -> u16 {
        self.ctr
    }

    pub(crate) fn pop_pending(&mut self) -> Option<PendingDispatch> {
        self.pending.pop_front()
    }

    pub fn set_connection_state(&mut self, resp: Option<&RespConnect>) {
        self.base.set_last_state_change_now();
        match resp {
            Some(r) => {
                self.base.slave_connected = true;
                self.base.set_change_endianess(r.is_big_endian());
                self.mag = r.address_granularity();
                self.max_dto = self.protocol_layer.max_dto.min(r.max_dto).max(8);
                self.max_cto = self.protocol_layer.max_cto.min(r.max_cto).max(8);
                self.base.reset_counters_on_disconnect();
            }
            None => {
                self.base.slave_connected = false;
                self.mag = 0;
                self.max_cto = 0;
                self.max_dto = 0;
                self.base.set_change_endianess(false);
            }
        }
    }

    pub fn on_error_received(&mut self, result: CmdResult, code: CommandCode) {
        self.base.inc_errors_received();
        self.last_error = result;
        self.pending.push_back(PendingDispatch::Error(result, code));
    }

    fn on_response_received(&mut self) {
        self.base.set_last_received_time(now_elapsed());
    }

    fn pack(&self, cmd: &dyn XcpCommand, additional: &[u8]) -> Vec<u8> {
        let payload = cmd.encode(self.base.change_endianess());
        build_cto(self.frame_fmt, self.ctr, &payload, additional)
    }

    pub async fn exchange<T: XcpResponse>(
        &mut self,
        data: &[u8],
        use_timeout_ms: i32,
        expected_payload: u8,
    ) -> (CmdResult, Option<T>, Vec<u8>) {
        if data.is_empty() {
            self.set_connection_state(None);
            return (CmdResult::ERR_TIMEOUT, None, Vec::new());
        }
        let code = CommandCode::from_u8(data[0]).unwrap_or_default();
        let timeout_ms = if use_timeout_ms < 0 {
            self.protocol_layer.timings[cmd_timing_index(code)] as i32
        } else {
            use_timeout_ms
        };
        let Some(transport) = self.transport.as_mut() else {
            self.set_connection_state(None);
            return (CmdResult::ERR_TIMEOUT, None, Vec::new());
        };
        transport.reset();
        let sent = transport.send_bytes(data).await;
        if data.len() > sent {
            self.set_connection_state(None);
            return (CmdResult::ERR_SND_CMD_FAILED, None, Vec::new());
        }
        let master_frame = XcpFrame::new(
            self.type_,
            transport.source(),
            data,
            true,
            transport.frame_fmt(),
        );
        let bits = master_frame.raw_frame_length();
        self.increase_ctr(bits);
        if use_timeout_ms >= 0 {
            if use_timeout_ms > 0 {
                block_for_micro_secs(use_timeout_ms as u64).await;
            }
            return (CmdResult::OK, None, Vec::new());
        }
        match self.wait_collect(timeout_ms, expected_payload).await {
            None => {
                self.set_connection_state(None);
                (CmdResult::ERR_TIMEOUT, None, Vec::new())
            }
            Some(list) => match list[0] {
                0xFE => {
                    let err = CmdResult::from_wire_u8(list.get(1).copied().unwrap_or(0));
                    self.on_error_received(err, code);
                    (err, None, Vec::new())
                }
                0xFF => {
                    if T::SIZE > list.len() {
                        return (CmdResult::ERR_PROTOCOL_FAILURE, None, Vec::new());
                    }
                    match T::decode(&list, self.base.change_endianess()) {
                        Some(resp) => {
                            self.on_response_received();
                            (CmdResult::OK, Some(resp), list[T::SIZE..].to_vec())
                        }
                        None => (CmdResult::ERR_PROTOCOL_FAILURE, None, Vec::new()),
                    }
                }
                _ => {
                    self.set_connection_state(None);
                    (CmdResult::ERR_TIMEOUT, None, Vec::new())
                }
            },
        }
    }

    async fn wait_collect(&mut self, timeout_ms: i32, expected: u8) -> Option<Vec<u8>> {
        let mut collected: Vec<u8> = Vec::new();
        let target = expected as usize + 1;
        let timeout = Duration::from_millis(timeout_ms.max(0) as u64);
        let mut deadline = Instant::now() + timeout;
        loop {
            let frame = {
                let transport = self.transport.as_mut()?;
                let mut f = transport.next_frame();
                if f.is_none() {
                    transport.poll().await;
                    f = transport.next_frame();
                }
                f
            };
            match frame {
                Some(frame) => {
                    self.base.record_frame_received(frame.raw_frame_length());
                    match frame.data()[0] {
                        0xFE | 0xFF => {
                            if collected.is_empty() {
                                collected.extend_from_slice(frame.data());
                            } else {
                                collected.extend_from_slice(&frame.data()[1..]);
                            }
                            if expected == 0 {
                                return Some(collected);
                            }
                            if collected.len() >= target {
                                collected.truncate(target);
                                return Some(collected);
                            }
                        }
                        0xFD => {
                            if frame.data().get(1) == Some(&(EventCodes::CmdPending as u8)) {
                                deadline = Instant::now() + timeout;
                            }
                            self.pending.push_back(PendingDispatch::Event(frame));
                        }
                        0xFC => self.pending.push_back(PendingDispatch::Service(frame)),
                        _ => self.pending.push_back(PendingDispatch::Daq(frame)),
                    }
                }
                None => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    autors_runtime::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn connect(&mut self, mode: ConnectMode) -> (CmdResult, Option<RespConnect>) {
        let bytes = self.pack(&CmdConnect { mode }, &[]);
        let (result, resp, _) = self.exchange::<RespConnect>(&bytes, -1, 0).await;
        self.set_connection_state(resp.as_ref());
        (result, resp)
    }

    pub async fn internal_connect(&mut self) -> bool {
        if let Some(t) = self.transport.as_mut() {
            t.reset();
        }
        self.connect(ConnectMode::Normal).await.0 == CmdResult::OK
    }

    pub async fn internal_get_status(&mut self) -> bool {
        self.get_status().await.0 == CmdResult::OK
    }

    pub async fn disconnect(&mut self, use_timeout_ms: i32) -> bool {
        let mut result = CmdResult::OK;
        if self.base.slave_connected {
            let bytes = self.pack(&CmdDisconnect, &[]);
            let (r, _, _) = self.exchange::<RespBase>(&bytes, use_timeout_ms, 0).await;
            result = r;
            self.set_connection_state(None);
        }
        result == CmdResult::OK
    }

    pub async fn get_status(&mut self) -> (CmdResult, Option<RespGetStatus>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetStatus), &[]);
        let (r, resp, _) = self.exchange::<RespGetStatus>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn synch(&mut self) -> CmdResult {
        let bytes = self.pack(&CmdBare::new(CommandCode::Synch), &[]);
        self.exchange::<RespError>(&bytes, -1, 0).await.0
    }

    pub async fn get_comm_mode_info(&mut self) -> (CmdResult, Option<RespGetCommModeInfo>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetCommModeInfo), &[]);
        let (r, resp, _) = self.exchange::<RespGetCommModeInfo>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_id(&mut self, id_type: GetIdType) -> (CmdResult, Option<RespGetId>, Vec<u8>) {
        let bytes = self.pack(&CmdGetId { id_type }, &[]);
        self.exchange::<RespGetId>(&bytes, -1, 0).await
    }

    pub async fn set_request(&mut self, mode: SetRequestMode, session_id: u16) -> CmdResult {
        let bytes = self.pack(&CmdSetRequest { mode, session_id }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn get_seed(
        &mut self,
        seed_mode: SeedModeType,
        resource: ResourceType,
    ) -> (CmdResult, Option<RespGetSeed>, Vec<u8>) {
        let bytes = self.pack(
            &CmdGetSeed {
                mode: seed_mode,
                resource,
            },
            &[],
        );
        self.exchange::<RespGetSeed>(&bytes, -1, 0).await
    }

    pub async fn unlock(
        &mut self,
        key: &[u8],
        offset: &mut usize,
    ) -> (CmdResult, Option<RespUnlock>) {
        let n = (key.len() - *offset).min(self.max_cto as usize - 2);
        let chunk = &key[*offset..*offset + n];
        let payload = CmdUnlock {
            remaining_len: (key.len() - *offset) as u8,
        };
        *offset += n;
        let bytes = self.pack(&payload, chunk);
        let (r, resp, _) = self.exchange::<RespUnlock>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn set_mta(&mut self, address_extension: u8, address: u32) -> CmdResult {
        let bytes = self.pack(
            &CmdSetMta {
                address_extension,
                address,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn upload(&mut self, number_of_elements: u8) -> (CmdResult, Vec<u8>) {
        if number_of_elements == 0 {
            return (CmdResult::ERR_INVALID_ARGUMENT, Vec::new());
        }
        let bytes = self.pack(&CmdUpload { number_of_elements }, &[]);
        let (result, _, rest) = self
            .exchange::<RespBase>(&bytes, -1, number_of_elements)
            .await;
        if result != CmdResult::OK {
            return (result, Vec::new());
        }
        let pad = Self::padding_size(self.mag);
        if pad > 0 {
            let want = number_of_elements as usize * self.mag;
            if rest.len() < pad + want {
                self.set_connection_state(None);
                return (CmdResult::ERR_TIMEOUT, Vec::new());
            }
            return (result, rest[pad..pad + want].to_vec());
        }
        (result, rest)
    }

    pub async fn short_upload(
        &mut self,
        number_of_elements: u8,
        address_extension: u8,
        address: u32,
    ) -> (CmdResult, Vec<u8>) {
        if number_of_elements == 0 {
            return (CmdResult::ERR_INVALID_ARGUMENT, Vec::new());
        }
        let bytes = self.pack(
            &CmdShortUpload {
                number_of_elements,
                address_extension,
                address,
            },
            &[],
        );
        let (result, _, rest) = self.exchange::<RespBase>(&bytes, -1, 0).await;
        let pad = Self::padding_size(self.mag);
        if result != CmdResult::OK || pad == 0 {
            return (result, rest);
        }
        let want = number_of_elements as usize * self.mag;
        if rest.len() < pad + want {
            self.set_connection_state(None);
            return (CmdResult::ERR_TIMEOUT, Vec::new());
        }
        (result, rest[pad..pad + want].to_vec())
    }

    pub async fn modify_bits(&mut self, s: u8, ma: u16, mx: u16) -> CmdResult {
        let bytes = self.pack(
            &CmdModifyBits {
                shift_value: s,
                and_mask: ma,
                xor_mask: mx,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn build_checksum(
        &mut self,
        block_size: u32,
    ) -> (CmdResult, Option<RespBuildChecksum>) {
        let bytes = self.pack(&CmdBuildChecksum { block_size }, &[]);
        let (r, resp, _) = self.exchange::<RespBuildChecksum>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn download(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::Download, bytes, no_of_data_elements, -1)
            .await
    }

    pub async fn download_next(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::DownloadNext, bytes, no_of_data_elements, -1)
            .await
    }

    pub async fn download_max(&mut self, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::DownloadMax, bytes, 0, -1)
            .await
    }

    pub async fn short_download(
        &mut self,
        bytes: &[u8],
        address_extension: u8,
        address: u32,
    ) -> CmdResult {
        let cmd = CmdShortDownload {
            number_of_elements: bytes.len() as u8,
            address_extension,
            address,
        };
        let data = self.pack(&cmd, bytes);
        self.exchange::<RespBase>(&data, -1, 0).await.0
    }

    pub async fn set_cal_page(
        &mut self,
        mode: CalPageMode,
        segment_no: u8,
        page_no: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdSetCalPage {
                mode,
                segment_no,
                page_no,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn get_cal_page(
        &mut self,
        mode: CalPageMode,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetCalPage>) {
        let bytes = self.pack(&CmdGetCalPage { mode, segment_no }, &[]);
        let (r, resp, _) = self.exchange::<RespGetCalPage>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_pag_processor_info(&mut self) -> (CmdResult, Option<RespGetPagProcessorInfo>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetPagProcessorInfo), &[]);
        let (r, resp, _) = self
            .exchange::<RespGetPagProcessorInfo>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_segment_info_mapping(
        &mut self,
        mapping_mode: MappingInfoModeType,
        segment_no: u8,
        mapping_index: u8,
    ) -> (CmdResult, Option<RespGetSegmentInfoAddress>) {
        let bytes = self.pack(
            &CmdGetSegmentInfo::mapping(mapping_mode, segment_no, mapping_index),
            &[],
        );
        let (r, resp, _) = self
            .exchange::<RespGetSegmentInfoAddress>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_segment_info_address(
        &mut self,
        basic_address_mode: BasicAddressModeType,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetSegmentInfoAddress>) {
        let bytes = self.pack(
            &CmdGetSegmentInfo::address(basic_address_mode, segment_no),
            &[],
        );
        let (r, resp, _) = self
            .exchange::<RespGetSegmentInfoAddress>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_segment_info(
        &mut self,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetSegmentInfo>) {
        let bytes = self.pack(&CmdGetSegmentInfo::standard(segment_no), &[]);
        let (r, resp, _) = self.exchange::<RespGetSegmentInfo>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_page_info(
        &mut self,
        segment_no: u8,
        page_no: u8,
    ) -> (CmdResult, Option<RespGetPageInfo>) {
        let bytes = self.pack(
            &CmdGetPageInfo {
                segment_no,
                page_no,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespGetPageInfo>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn set_segment_mode(&mut self, mode: SegmentMode, segment_no: u8) -> CmdResult {
        let bytes = self.pack(&CmdSetSegmentMode { mode, segment_no }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn get_segment_mode(
        &mut self,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetSegmentMode>) {
        let bytes = self.pack(&CmdGetSegmentMode { segment_no }, &[]);
        let (r, resp, _) = self.exchange::<RespGetSegmentMode>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn copy_cal_page(
        &mut self,
        src_segment_no: u8,
        src_page_no: u8,
        dst_segment_no: u8,
        dst_page_no: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdCopyCalPage {
                src_segment_no,
                src_page_no,
                dst_segment_no,
                dst_page_no,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn set_daq_ptr(
        &mut self,
        daq_list_no: u16,
        odt_no: u8,
        odt_entry_no: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdSetDaqPtr {
                daq_list_no,
                odt_no,
                odt_entry_no,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn write_daq(
        &mut self,
        bit_offset: u8,
        element_size: u8,
        address_extension: u8,
        address: u32,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdWriteDaq {
                bit_offset,
                element_size,
                address_extension,
                address,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn write_daq_multiple(&mut self, entries: &[Arc<OdtEntry>]) -> CmdResult {
        let per_frame = ((self.max_cto as usize - 2) / 8).max(1);
        let mut result = CmdResult::OK;
        let swap = self.base.change_endianess();
        for chunk in entries.chunks(per_frame) {
            let mut records = Vec::with_capacity(chunk.len() * 8);
            for e in chunk {
                records.push(e.bit_offset);
                records.push(e.size);
                records.extend_from_slice(&swap32(e.address(), swap).to_le_bytes());
                records.push(e.measurement.address_extension as u8);
                records.push(0);
            }
            let cmd = CmdWriteDaqMultiple {
                record_count: chunk.len() as u8,
            };
            let bytes = self.pack(&cmd, &records);
            result = self.exchange::<RespBase>(&bytes, -1, 0).await.0;
            if result != CmdResult::OK {
                return result;
            }
        }
        result
    }

    pub async fn set_daq_list_mode(
        &mut self,
        mode: DaqListMode,
        daq_list_no: u16,
        event_channel_no: u16,
        prescaler: u8,
        priority: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdSetDaqListMode {
                mode,
                daq_list_no,
                event_channel_no,
                prescaler,
                priority,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn start_stop_daq_list(
        &mut self,
        mode: StartStopMode,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespStartStopDaqList>) {
        let bytes = self.pack(&CmdStartStopDaqList { mode, daq_list_no }, &[]);
        let (r, resp, _) = self.exchange::<RespStartStopDaqList>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn start_stop_synch(&mut self, mode: StartStopMode) -> CmdResult {
        let bytes = self.pack(&CmdStartStopSynch { mode }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn read_daq(&mut self) -> (CmdResult, Option<RespReadDaq>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::ReadDaq), &[]);
        let (r, resp, _) = self.exchange::<RespReadDaq>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_daq_clock(&mut self) -> (CmdResult, Option<RespGetDaqClock>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetDaqClock), &[]);
        let (r, resp, _) = self.exchange::<RespGetDaqClock>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_daq_processor_info(&mut self) -> (CmdResult, Option<RespGetDaqProcessorInfo>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetDaqProcessorInfo), &[]);
        let (r, resp, _) = self
            .exchange::<RespGetDaqProcessorInfo>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_daq_resolution_info(
        &mut self,
    ) -> (CmdResult, Option<RespGetDaqResolutionInfo>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetDaqResolutionInfo), &[]);
        let (r, resp, _) = self
            .exchange::<RespGetDaqResolutionInfo>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_daq_list_mode(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListMode>) {
        let bytes = self.pack(
            &CmdList {
                cmd_code: CommandCode::GetDaqListMode,
                list_no: daq_list_no,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespGetDaqListMode>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn get_daq_event_info(
        &mut self,
        event_channel_no: u16,
    ) -> (CmdResult, Option<RespGetDaqEventInfo>) {
        let bytes = self.pack(
            &CmdList {
                cmd_code: CommandCode::GetDaqEventInfo,
                list_no: event_channel_no,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespGetDaqEventInfo>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn clear_daq_list(&mut self, daq_list_no: u16) -> CmdResult {
        let bytes = self.pack(
            &CmdList {
                cmd_code: CommandCode::ClearDaqList,
                list_no: daq_list_no,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn get_daq_list_info(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListInfo>) {
        let bytes = self.pack(
            &CmdList {
                cmd_code: CommandCode::GetDaqListInfo,
                list_no: daq_list_no,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespGetDaqListInfo>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn free_daq(&mut self) -> CmdResult {
        let bytes = self.pack(&CmdBare::new(CommandCode::FreeDAQ), &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    /// Sends ALLOC_DAQ for the given number of DAQ lists.
    pub async fn alloc_daq(&mut self, daq_count: u16) -> CmdResult {
        let bytes = self.pack(&CmdAllocDaq { daq_count }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    /// Sends ALLOC_ODT for the given DAQ list.
    pub async fn alloc_odt(&mut self, daq_list_no: u16, odt_count: u8) -> CmdResult {
        let bytes = self.pack(
            &CmdAllocOdt {
                daq_list_no,
                odt_count,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn alloc_odt_entry(
        &mut self,
        daq_list_no: u16,
        odt_no: u8,
        odt_entries_count: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdAllocOdtEntry {
                daq_list_no,
                odt_no,
                odt_entries_count,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn program_start(&mut self) -> (CmdResult, Option<RespProgramStart>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::ProgramStart), &[]);
        let (r, resp, _) = self.exchange::<RespProgramStart>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn program_clear(&mut self, mode: ProgramClearMode, clear_range: u32) -> CmdResult {
        let bytes = self.pack(&CmdProgramClear { mode, clear_range }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn write_bytes(
        &mut self,
        cmd_code: CommandCode,
        bytes: &[u8],
        no_of_data_elements: u8,
        use_timeout_ms: i32,
    ) -> CmdResult {
        let data = match cmd_code {
            CommandCode::ProgramNext
            | CommandCode::Program
            | CommandCode::DownloadNext
            | CommandCode::Download => self.pack(
                &CmdDownload {
                    cmd_code,
                    number_of_elements: no_of_data_elements,
                },
                bytes,
            ),
            CommandCode::ProgramMax | CommandCode::DownloadMax => {
                self.pack(&CmdBare::new(cmd_code), bytes)
            }
            _ => return CmdResult::ERR_INVALID_ARGUMENT,
        };
        self.exchange::<RespBase>(&data, use_timeout_ms, 0).await.0
    }

    pub async fn program(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::Program, bytes, no_of_data_elements, -1)
            .await
    }

    pub async fn program_next(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::ProgramNext, bytes, no_of_data_elements, -1)
            .await
    }

    pub async fn program_max(&mut self, bytes: &[u8]) -> CmdResult {
        self.write_bytes(CommandCode::ProgramMax, bytes, 0, -1)
            .await
    }

    pub async fn program_reset(&mut self) -> CmdResult {
        let bytes = self.pack(&CmdBare::new(CommandCode::ProgramReset), &[]);
        let result = self.exchange::<RespBase>(&bytes, -1, 0).await.0;
        self.set_connection_state(None);
        result
    }

    pub async fn get_pgm_processor_info(&mut self) -> (CmdResult, Option<RespGetPgmProcessorInfo>) {
        let bytes = self.pack(&CmdBare::new(CommandCode::GetPgmProcessorInfo), &[]);
        let (r, resp, _) = self
            .exchange::<RespGetPgmProcessorInfo>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn get_sector_info(
        &mut self,
        mode: GetSectorInfoMode,
        sector_no: u8,
    ) -> std::result::Result<(CmdResult, Option<RespGetSectorInfoModeAddressOrLen>), Error> {
        if mode == GetSectorInfoMode::NameLength {
            return Err(Error::Protocol("Wrong call (response)".to_string()));
        }
        let bytes = self.pack(&CmdGetSectorInfo { mode, sector_no }, &[]);
        let (r, resp, _) = self
            .exchange::<RespGetSectorInfoModeAddressOrLen>(&bytes, -1, 0)
            .await;
        Ok((r, resp))
    }

    pub async fn get_sector_info_name_len(
        &mut self,
        mode: GetSectorInfoMode,
        sector_no: u8,
    ) -> std::result::Result<(CmdResult, Option<RespGetSectorInfoModeSectorNameLen>), Error> {
        if mode != GetSectorInfoMode::NameLength {
            return Err(Error::Protocol("Wrong call (response)".to_string()));
        }
        let bytes = self.pack(&CmdGetSectorInfo { mode, sector_no }, &[]);
        let (r, resp, _) = self
            .exchange::<RespGetSectorInfoModeSectorNameLen>(&bytes, -1, 0)
            .await;
        Ok((r, resp))
    }

    pub async fn program_prepare(&mut self, code_size: u16) -> CmdResult {
        let bytes = self.pack(&CmdProgramPrepare { code_size }, &[]);
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn program_format(
        &mut self,
        compression_method: u8,
        encryption_method: u8,
        programming_method: u8,
        access_method: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdProgramFormat {
                compression_method,
                encryption_method,
                programming_method,
                access_method,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn program_verify(
        &mut self,
        mode: ProgramVerifyMode,
        verification_type: u16,
        verification_value: u32,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdProgramVerify {
                mode,
                verification_type,
                verification_value,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }

    pub async fn user_cmd(&mut self, sub_command: u8, bytes: &[u8]) -> (CmdResult, Vec<u8>) {
        let cmd = CmdUserTransCmd {
            cmd_code: CommandCode::UserCmd,
            sub_command,
        };
        let data = self.pack(&cmd, bytes);
        let (r, _, rest) = self.exchange::<RespBase>(&data, -1, 0).await;
        (r, rest)
    }

    pub async fn dto_ctr_properties(
        &mut self,
        modifier: DtoCtrModifier,
        event_channel_no: u16,
        related_event_channel_no: u16,
        mode: DtoCtrMode,
    ) -> (CmdResult, Option<RespDtoCtrResp>) {
        let bytes = self.pack(
            &CmdDtoCtrProperties {
                modifier,
                event_channel_no,
                related_event_channel_no,
                mode,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespDtoCtrResp>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn time_correlation_properties(
        &mut self,
        set_properties: TimeCorrSetProps,
        get_properties_req: TimeCorrGetPropsReq,
        cluster_id: u16,
    ) -> (CmdResult, Option<RespTimeCorrelation>) {
        let bytes = self.pack(
            &CmdTimeCorrelationProperties {
                set_properties,
                get_properties_req,
                cluster_id,
            },
            &[],
        );
        let (r, resp, _) = self.exchange::<RespTimeCorrelation>(&bytes, -1, 0).await;
        (r, resp)
    }

    pub async fn transport_layer_cmd(
        &mut self,
        sub_command: u8,
        bytes: &[u8],
        use_timeout_ms: i32,
    ) -> (CmdResult, Vec<u8>) {
        let cmd = CmdUserTransCmd {
            cmd_code: CommandCode::TransportLayerCmd,
            sub_command,
        };
        let data = self.pack(&cmd, bytes);
        let (r, _, rest) = self.exchange::<RespBase>(&data, use_timeout_ms, 0).await;
        (r, rest)
    }

    pub async fn get_daq_id(&mut self, daq_list_no: u16) -> (CmdResult, Option<(bool, u32)>) {
        let bytes = daq_list_no.to_le_bytes();
        let (result, rest) = self
            .transport_layer_cmd(XcpOnCanCmd::GetDaqId.as_u8(), &bytes, -1)
            .await;
        if result != CmdResult::OK || rest.len() < 8 {
            return (result, None);
        }
        let is_fixed = rest[1] == 1;
        let mut can_id = u32_le(&rest, 4);
        if self.base.change_endianess() {
            can_id = can_id.swap_bytes();
        }
        (CmdResult::OK, Some((is_fixed, can_id)))
    }

    pub async fn set_daq_id(&mut self, daq_list_no: u16, can_id: u32) -> CmdResult {
        let swap = self.base.change_endianess();
        let mut bytes = Vec::with_capacity(6);
        bytes.extend_from_slice(&swap16(daq_list_no, swap).to_le_bytes());
        bytes.extend_from_slice(&swap32(can_id, swap).to_le_bytes());
        self.transport_layer_cmd(XcpOnCanCmd::SetDaqId.as_u8(), &bytes, -1)
            .await
            .0
    }

    pub async fn get_daq_clock_multicast(&mut self, cluster_id: u16, counter: u8) -> CmdResult {
        let mut bytes = Vec::with_capacity(3);
        bytes.extend_from_slice(&cluster_id.to_le_bytes());
        bytes.push(counter);
        self.transport_layer_cmd(XcpOnCanCmd::GetDaqClockMulticast.as_u8(), &bytes, 0)
            .await
            .0
    }

    pub async fn get_daq_list_usb_endpoint(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListUsbEndpoint>) {
        let bytes = self.pack(&CmdGetDaqListUsbEndpoint { daq_list_no }, &[]);
        let (r, resp, _) = self
            .exchange::<RespGetDaqListUsbEndpoint>(&bytes, -1, 0)
            .await;
        (r, resp)
    }

    pub async fn set_daq_list_usb_endpoint(
        &mut self,
        daq_list_no: u16,
        endpoint_no: u8,
    ) -> CmdResult {
        let bytes = self.pack(
            &CmdSetDaqListUsbEndpoint {
                daq_list_no,
                endpoint_no,
            },
            &[],
        );
        self.exchange::<RespBase>(&bytes, -1, 0).await.0
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct Odt {
    pub pid: u8,
    pub is_first_pid: bool,
    pub entries: Vec<Arc<OdtEntry>>,
}

pub type OdtDict = BTreeMap<u8, Odt>;

pub fn get_cycle_time(res: XcpTimestampResolution, time_cycle: u8) -> String {
    if time_cycle == 0 {
        return "NC".to_string();
    }
    let (n, unit) = match res {
        XcpTimestampResolution::_1NS => (1, "ns"),
        XcpTimestampResolution::_10NS => (10, "ns"),
        XcpTimestampResolution::_100NS => (100, "ns"),
        XcpTimestampResolution::_1US => (1, "us"),
        XcpTimestampResolution::_10US => (10, "us"),
        XcpTimestampResolution::_100US => (100, "us"),
        XcpTimestampResolution::_1MS => (1, "ms"),
        XcpTimestampResolution::_10MS => (10, "ms"),
        XcpTimestampResolution::_100MS => (100, "ms"),
        XcpTimestampResolution::_1S => (1, "s"),
        XcpTimestampResolution::_1PS => (1, "ps"),
        XcpTimestampResolution::_10PS => (10, "ps"),
        XcpTimestampResolution::_100PS => (100, "ps"),
        XcpTimestampResolution::NotSet => (0, ""),
    };
    format!("{}{}", n * time_cycle as u64, unit)
}

pub struct DaqListXcp {
    pub base: DaqList,
    pub daq_no: u16,
    pub first_pid: u8,
    pub evt: XcpEvent,
    pub odt_dict: OdtDict,
    pub state: StartStopMode,
    pub mode: DaqListMode,
    last_ts_raw: u32,
}

impl DaqListXcp {
    pub fn new(daq_no: u16, evt: XcpEvent, first_pid: u8) -> Self {
        Self {
            base: DaqList::new_inactive(daq_no),
            daq_no,
            first_pid,
            evt,
            odt_dict: OdtDict::new(),
            state: StartStopMode::default(),
            mode: DaqListMode::NONE,
            last_ts_raw: 0,
        }
    }

    pub fn with_limits(
        daq_no: u16,
        evt: XcpEvent,
        max_dto: u16,
        max_odt: u8,
        max_odt_entries: u8,
        data_offset_first_pid: usize,
        data_offset: usize,
    ) -> Self {
        let time_cycle = get_cycle_time(evt.time_unit, evt.time_cycle);
        Self {
            base: DaqList::new(
                daq_no,
                0,
                evt.id,
                max_dto,
                max_odt,
                max_odt_entries,
                data_offset_first_pid as i32,
                data_offset as i32,
                time_cycle,
                0,
            ),
            daq_no,
            first_pid: 0,
            evt,
            odt_dict: OdtDict::new(),
            state: StartStopMode::default(),
            mode: DaqListMode::NONE,
            last_ts_raw: 0,
        }
    }

    pub fn decode_timestamp(
        &mut self,
        data: &[u8],
        offset: usize,
        ts_supported: Option<&XcpTimestampSupported>,
        swap: bool,
    ) -> f64 {
        let Some(ts) = ts_supported else {
            return f64::NAN;
        };
        if data.len() < offset + 1 {
            return f64::NAN;
        }
        let last = self.last_ts_raw;
        let (raw, mut delta): (u32, i64) = match ts.size {
            XcpTimestampSize::BYTE => {
                let num = data[offset] as u32;
                let delta = if num < last {
                    num as i64 + 255 - last as i64
                } else {
                    num as i64 - last as i64
                };
                (num, delta)
            }
            XcpTimestampSize::WORD => {
                if data.len() < offset + 2 {
                    return f64::NAN;
                }
                let num = swap16(u16_le(data, offset), swap) as u32;
                let delta = if num < last {
                    num as i64 + 65535 - last as i64
                } else {
                    num as i64 - last as i64
                };
                (num, delta)
            }
            XcpTimestampSize::DWORD => {
                if data.len() < offset + 4 {
                    return f64::NAN;
                }
                let num = swap32(u32_le(data, offset), swap);
                let delta = if num < last {
                    num.wrapping_add(0xFFFF_FFFF).wrapping_sub(last) as i64
                } else {
                    num.wrapping_sub(last) as i64
                };
                (num, delta)
            }
            _ => return f64::NAN,
        };
        let mut result = 0.0;
        if last != 0 {
            delta *= ts.ticks as i64;
            let mult: i64 = match ts.resolution {
                XcpTimestampResolution::_1PS => 1,
                XcpTimestampResolution::_10PS => 10,
                XcpTimestampResolution::_100PS => 100,
                XcpTimestampResolution::_1NS => 1_000,
                XcpTimestampResolution::_10NS => 10_000,
                XcpTimestampResolution::_100NS => 100_000,
                XcpTimestampResolution::_1US => 1_000_000,
                XcpTimestampResolution::_10US => 10_000_000,
                XcpTimestampResolution::_100US => 100_000_000,
                XcpTimestampResolution::_1MS => 1_000_000_000,
                XcpTimestampResolution::_10MS => 10_000_000_000,
                XcpTimestampResolution::_100MS => 100_000_000_000,
                XcpTimestampResolution::_1S => 1_000_000_000_000,
                XcpTimestampResolution::NotSet => 0,
            };
            result = (delta * mult) as f64 / 1_000_000_000_000.0;
        }
        self.last_ts_raw = raw;
        result
    }

    pub fn clear_data(&mut self) {
        self.last_ts_raw = 0;
        self.base.clear_data();
    }
}

#[derive(Debug, Clone)]
pub struct DaqAndEvt {
    pub daq_list: XcpDaqList,
    pub evt: XcpEvent,
    pub dynamic: bool,
}

pub fn build_daq_and_evt_map(daq: &XcpDaq) -> BTreeMap<u16, DaqAndEvt> {
    let mut map = BTreeMap::new();
    let is_dynamic = daq.mode == XcpDaqMode::DYNAMIC;
    let num: u16 = if is_dynamic { u16::MAX } else { daq.max_daq };
    let node_list: Vec<&XcpDaqList> = daq
        .children
        .iter()
        .filter_map(|n| match n {
            XcpNode::DaqList(l) => Some(l),
            _ => None,
        })
        .collect();
    let mut unassigned: Vec<&XcpDaqList> = node_list
        .iter()
        .copied()
        .filter(|l| l.event_fixed == u16::MAX)
        .collect();
    let mut next_no: u16 = node_list
        .iter()
        .map(|l| l.daq_no)
        .max()
        .map_or(0, |m| m + 1);
    for n in &daq.children {
        let XcpNode::Event(evt) = n else { continue };
        if !(evt.daq_list_type == XcpDaqListType::DAQ
            || evt.daq_list_type == XcpDaqListType::DAQ_STIM)
        {
            continue;
        }
        let fixed: Vec<&XcpDaqList> = node_list
            .iter()
            .copied()
            .filter(|l| l.event_fixed == evt.id)
            .collect();
        let mut i = 0usize;
        for item in fixed {
            map.insert(
                item.daq_no,
                DaqAndEvt {
                    daq_list: item.clone(),
                    evt: evt.clone(),
                    dynamic: false,
                },
            );
            i += 1;
            if i >= evt.max_daq_list as usize {
                break;
            }
        }
        while i < evt.max_daq_list as usize {
            let Some(item) = unassigned.pop() else { break };
            map.insert(
                item.daq_no,
                DaqAndEvt {
                    daq_list: item.clone(),
                    evt: evt.clone(),
                    dynamic: false,
                },
            );
            i += 1;
        }
        while i < evt.max_daq_list as usize {
            if next_no > num || !is_dynamic {
                break;
            }
            let item = XcpDaqList {
                active: true,
                daq_list_type: XcpDaqListType::DAQ,
                max_odt: 252,
                max_odt_entries: u8::MAX,
                daq_no: next_no,
                ..XcpDaqList::default()
            };
            next_no += 1;
            map.insert(
                item.daq_no,
                DaqAndEvt {
                    daq_list: item,
                    evt: evt.clone(),
                    dynamic: true,
                },
            );
            i += 1;
        }
    }
    map
}

#[derive(Default)]
pub struct DaqDictXcp {
    pub lists: Vec<DaqListXcp>,
    pub odt_entries: std::collections::HashMap<u32, Vec<Arc<OdtEntry>>>,
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

impl DaqDictXcp {
    pub fn clear(&mut self) {
        self.odt_entries.clear();
        self.clear_data();
        self.lists.clear();
    }

    pub fn clear_data(&mut self) {
        for l in &mut self.lists {
            l.clear_data();
        }
    }

    pub fn get_first_pid(&self, daq_list: &DaqListXcp) -> u8 {
        let mut pid = 0u8;
        for l in &self.lists {
            if l.daq_no == daq_list.daq_no {
                return pid;
            }
            pid = pid.wrapping_add(l.odt_dict.len() as u8);
        }
        pid
    }

    fn find_list(
        &self,
        frame: &XcpFrame,
        id_field: XcpIdFieldType,
        swap: bool,
    ) -> Option<(usize, usize)> {
        let data = frame.data();
        let mut offset = 1usize;
        let idx = match id_field {
            XcpIdFieldType::ABSOLUTE => {
                let pid = *data.first()?;
                self.lists.iter().position(|l| {
                    let first = l.first_pid;
                    pid >= first && pid < first.wrapping_add(l.base.odts.len() as u8)
                })?
            }
            XcpIdFieldType::BYTE => {
                let daq_no = *data.get(1)? as u16;
                offset = 2;
                self.lists.iter().position(|l| l.daq_no == daq_no)?
            }
            XcpIdFieldType::WORD => {
                if data.len() < 3 {
                    return None;
                }
                let daq_no = swap16(u16_le(data, 1), swap);
                offset = 3;
                self.lists.iter().position(|l| l.daq_no == daq_no)?
            }
            XcpIdFieldType::ALIGNED => {
                if data.len() < 4 {
                    return None;
                }
                let daq_no = swap16(u16_le(data, 2), swap);
                offset = 4;
                self.lists.iter().position(|l| l.daq_no == daq_no)?
            }
            _ => return None,
        };
        Some((idx, offset))
    }

    pub fn resolve(
        &self,
        frame: &XcpFrame,
        id_field: XcpIdFieldType,
        swap: bool,
    ) -> Option<(usize, u8, usize)> {
        let (idx, offset) = self.find_list(frame, id_field, swap)?;
        let list = &self.lists[idx];
        let odt_key = frame.data().first()?.wrapping_sub(list.first_pid);
        if !list.base.odts.contains_key(&odt_key) {
            return None;
        }
        Some((idx, odt_key, offset))
    }

    pub fn fill(
        &mut self,
        max_dto: u16,
        data_offset_first_pid: usize,
        data_offset: usize,
        absolute_id: bool,
        map: &BTreeMap<u16, DaqAndEvt>,
        measurements: &mut Vec<DaqMeasurement>,
    ) -> Result<usize> {
        let mut count = 0usize;
        for (&daq_no, dae) in map {
            if absolute_id && count >= 252 {
                break;
            }
            if dae.daq_list.active {
                let list = DaqListXcp::with_limits(
                    daq_no,
                    dae.evt.clone(),
                    max_dto,
                    dae.daq_list.max_odt,
                    dae.daq_list.max_odt_entries,
                    data_offset_first_pid,
                    data_offset,
                );
                count += list.base.odts.len();
                self.lists.push(list);
            }
        }
        let leftover = self.fill_daq_lists(measurements, false)?;
        self.renumber_dynamic(map);
        Ok(leftover)
    }

    pub fn fill_daq_lists(
        &mut self,
        measurements: &mut Vec<DaqMeasurement>,
        is_ccp: bool,
    ) -> Result<usize> {
        self.odt_entries.reserve(measurements.len());
        for list in &mut self.lists {
            let evt_no = list.base.evt_no;
            if evt_no == u16::MAX {
                continue;
            }
            let preferred: Vec<usize> = measurements
                .iter()
                .enumerate()
                .filter(|(_, m)| {
                    m.desired_event_channels
                        .as_ref()
                        .is_some_and(|chs| chs.contains(&evt_no))
                })
                .map(|(index, _)| index)
                .collect();
            let consumed = list.base.fill_daq_list_selected_indices(
                measurements,
                &preferred,
                &mut self.odt_entries,
                is_ccp,
                true,
            )?;
            remove_indices(measurements, &consumed);
        }
        for list in &mut self.lists {
            if list.base.evt_no == u16::MAX {
                continue;
            }
            let consumed = list.base.fill_daq_list_indices(
                measurements,
                &mut self.odt_entries,
                is_ccp,
                false,
            )?;
            remove_indices(measurements, &consumed);
        }
        for list in &mut self.lists {
            if list.base.evt_no != u16::MAX && !list.base.odts.is_empty() {
                let total: usize = list.base.odts.values().map(|o| o.size as usize).sum();
                list.base.cache = Some(DaqCache::new(list.base.odts.len() as i32, total as i32));
            }
        }
        Ok(measurements.len())
    }

    fn renumber_dynamic(&mut self, map: &BTreeMap<u16, DaqAndEvt>) {
        let mut num: u16 = map
            .values()
            .map(|dae| {
                if dae.dynamic {
                    0
                } else {
                    dae.daq_list.daq_no + 1
                }
            })
            .max()
            .unwrap_or(0);
        self.lists.retain(|l| !l.base.odts.is_empty());
        for list in &mut self.lists {
            let old_no = list.daq_no;
            if map.get(&old_no).is_some_and(|dae| dae.dynamic) {
                list.daq_no = num;
                num = num.wrapping_add(1);
            }
        }
    }
}

// ============================================================================
// ============================================================================

pub trait XcpSeedKeyProvider: Send {
    fn supports_xcp(&self) -> bool {
        true
    }

    fn compute_key_from_seed(&self, resource: u8, seed: &[u8])
        -> std::result::Result<Vec<u8>, i32>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XcpMemorySegment {
    pub address: i64,
    pub size: i32,
    pub data: Vec<u8>,
}

impl XcpMemorySegment {
    pub fn new(address: i64, size: i32, data: Vec<u8>) -> Self {
        Self {
            address,
            size,
            data,
        }
    }

    pub fn get_data_bytes(&self, address: i64, len: u32) -> Option<Vec<u8>> {
        let end = address.checked_add(len as i64)? - 1;
        if address < self.address || end >= self.address + self.size as i64 {
            return None;
        }
        let start = (address - self.address) as usize;
        Some(self.data[start..start + len as usize].to_vec())
    }
}

#[derive(Default)]
pub struct XcpCallbacks {
    pub xcp_event: Option<Box<dyn FnMut(EventBase) + Send>>,
    pub service_request: Option<Box<dyn FnMut(RespService) + Send>>,
    pub xcp_error: Option<Box<dyn FnMut(CmdResult, CommandCode) + Send>>,
}

#[derive(Debug, Default, Clone)]
pub struct CachedResponses {
    pub connect: Option<RespConnect>,
    pub status: Option<RespGetStatus>,
    pub comm_mode_info: Option<RespGetCommModeInfo>,
    pub pag_proc_info: Option<RespGetPagProcessorInfo>,
    pub daq_proc_info: Option<RespGetDaqProcessorInfo>,
    pub daq_resolution_info: Option<RespGetDaqResolutionInfo>,
    pub pgm_proc_info: Option<RespGetPgmProcessorInfo>,
    pub cal_page: Option<RespGetCalPage>,
}

pub struct XcpMaster {
    pub base: XcpMasterBase,
    daq: Option<XcpDaq>,
    daq_and_evt_map: BTreeMap<u16, DaqAndEvt>,
    timestamp_supported: Option<XcpTimestampSupported>,
    data_offset_first_pid: usize,
    data_offset: usize,
    is_daq_running: bool,
    daq_config_changed: bool,
    cached: CachedResponses,
    daq_clock: u32,
    seed_and_key: Option<Box<dyn XcpSeedKeyProvider>>,
    pub callbacks: XcpCallbacks,
    daqs_storage: DaqDictXcp,
    address_mapper: Option<Box<dyn Fn(u32) -> u32 + Send>>,
    pub prevent_block_mode: bool,
    pub respect_optional_cmds: bool,
}

impl XcpMaster {
    pub fn with_base(base: XcpMasterBase, daq: Option<&XcpDaq>) -> Self {
        let daq_and_evt_map = daq.map_or_else(BTreeMap::new, build_daq_and_evt_map);
        let mut data_offset = daq.map_or(0, |d| d.id_field_size() as usize);
        let timestamp_supported = daq.and_then(|d| {
            d.children.iter().find_map(|n| match n {
                XcpNode::TimestampSupported(t) => Some(t.clone()),
                _ => None,
            })
        });
        if let Some(ts) = &timestamp_supported {
            data_offset += ts_size_bytes(ts.size);
        }
        Self {
            base,
            daq: daq.cloned(),
            daq_and_evt_map,
            timestamp_supported,
            data_offset_first_pid: data_offset,
            data_offset: daq.map_or(0, |d| d.id_field_size() as usize),
            is_daq_running: false,
            daq_config_changed: false,
            cached: CachedResponses::default(),
            daq_clock: 0,
            seed_and_key: None,
            callbacks: XcpCallbacks::default(),
            daqs_storage: DaqDictXcp::default(),
            address_mapper: None,
            prevent_block_mode: false,
            respect_optional_cmds: true,
        }
    }

    pub fn new_can(
        connect_behaviour: ConnectBehaviourType,
        device: Arc<Mutex<dyn CanDevice + Send>>,
        xcp_can: &XcpOnCan,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
        device_name: &str,
    ) -> Result<Self> {
        let source = format!(
            "{} {}",
            device_name,
            autors_can::device::to_can_id_string(xcp_can.can_id_resp)
        );
        let transport = CanXcpTransport::new(Arc::clone(&device), source, xcp_can)?;
        let base = XcpMasterBase::new_can(
            connect_behaviour,
            xcp_can,
            protocol_layer,
            device_name,
            Box::new(transport),
        );
        Ok(Self::with_base(base, daq))
    }

    /// Creates a UDP/TCP master.
    /// Wires a [`UdpXcpTransport`]/[`TcpXcpTransport`] depending on `type_`
    /// (only UDP/TCP are handled); other types return [`Error::Protocol`].
    /// IP parsing matches [`XcpMasterBase::new_udp_tcp`] (numeric IPs only, no DNS).
    pub fn new_udp_tcp(
        connect_behaviour: ConnectBehaviourType,
        type_: XcpType,
        remote_address: &str,
        remote_port: i32,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
    ) -> Result<Self> {
        let address: std::net::IpAddr = remote_address
            .parse()
            .map_err(|_| Error::Protocol(format!("Unknown host {remote_address}:{remote_port}")))?;
        let port = u16::try_from(remote_port)
            .map_err(|_| Error::Protocol(format!("Unknown host {remote_address}:{remote_port}")))?;
        let remote = std::net::SocketAddr::new(address, port);
        let transport: Box<dyn XcpTransport + Send> = match type_ {
            XcpType::Udp => Box::new(UdpXcpTransport::new(remote)?),
            XcpType::Tcp => Box::new(TcpXcpTransport::new(remote)?),
            _ => {
                return Err(Error::Protocol(format!(
                    "XCP transport type {type_} not supported for UDP/TCP master"
                )))
            }
        };
        let base = XcpMasterBase::new_udp_tcp(
            connect_behaviour,
            type_,
            remote_address,
            remote_port,
            protocol_layer,
            transport,
        )?;
        Ok(Self::with_base(base, daq))
    }

    /// [`XcpMasterBase::new_sxi`]).
    pub fn new_sxi<IO: SxiSerialIo + Send + 'static>(
        connect_behaviour: ConnectBehaviourType,
        device: SerialPortDevice<IO>,
        xcp_sxi: &XcpOnSxi,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
    ) -> Self {
        let com_port = device.core().port.clone();
        let transport = SxiXcpTransport::new(device);
        let base = XcpMasterBase::new_sxi(
            connect_behaviour,
            xcp_sxi,
            protocol_layer,
            &com_port,
            Box::new(transport),
        );
        Self::with_base(base, daq)
    }

    pub fn set_address_mapper(&mut self, mapper: impl Fn(u32) -> u32 + Send + 'static) {
        self.address_mapper = Some(Box::new(mapper));
    }

    pub fn map_address(&self, address: u32) -> u32 {
        self.address_mapper.as_ref().map_or(address, |m| m(address))
    }

    pub fn set_seed_key_provider(&mut self, provider: Box<dyn XcpSeedKeyProvider>) {
        self.seed_and_key = Some(provider);
    }

    pub fn is_daq_running(&self) -> bool {
        self.is_daq_running
    }

    pub fn daq_clock(&self) -> u32 {
        self.daq_clock
    }

    pub fn daq_and_evt_map(&self) -> &BTreeMap<u16, DaqAndEvt> {
        &self.daq_and_evt_map
    }

    pub fn cached(&self) -> &CachedResponses {
        &self.cached
    }

    pub fn active_page(&self) -> EcuPage {
        match self.cached.cal_page.as_ref().map_or(1, |r| r.page_no) {
            0 => EcuPage::Flash,
            _ => EcuPage::RAM,
        }
    }

    pub fn byte_order(&self) -> ByteOrder {
        match &self.cached.connect {
            None => ByteOrder::NotSet,
            Some(r) if r.is_big_endian() => ByteOrder::MSB_FIRST,
            _ => ByteOrder::MSB_LAST,
        }
    }

    pub fn version(&self) -> String {
        self.cached.connect.as_ref().map_or_else(String::new, |r| {
            format!("{}.{}", r.version_major(), r.version_minor())
        })
    }

    pub fn str_address_granularity(&self) -> &'static str {
        match &self.cached.connect {
            None => "",
            Some(r) => {
                if r.comm_mode_basic
                    .contains(CommModeBasic::ADDRESS_GRANULARITY_DWORD)
                {
                    "DWORD"
                } else if r
                    .comm_mode_basic
                    .contains(CommModeBasic::ADDRESS_GRANULARITY_WORD)
                {
                    "WORD"
                } else {
                    "BYTE"
                }
            }
        }
    }

    pub fn can_write(&self) -> bool {
        self.cached
            .connect
            .as_ref()
            .is_some_and(|r| r.resource.contains(ResourceType::CAL_PAG))
    }

    pub fn is_allowed_request(&self, cmd: &str) -> bool {
        if !self.respect_optional_cmds {
            return true;
        }
        self.base
            .protocol_layer
            .optional_cmds
            .iter()
            .any(|c| c == cmd)
    }

    pub fn is_slave_block_mode(&self) -> bool {
        if self.prevent_block_mode || self.base.max_cto() == u8::MAX {
            return false;
        }
        self.base
            .protocol_layer
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::SLAVE)
    }

    fn is_master_block_mode(&self, max_cto: u8) -> bool {
        if self.prevent_block_mode || max_cto == u8::MAX {
            return false;
        }
        self.base
            .protocol_layer
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::MASTER)
    }

    /// (`GET_STATUS` → `GET_COMM_MODE_INFO` → unlock → `GET_CAL_PAGE`/
    /// `GET_PAG_PROCESSOR_INFO` → `GET_PGM_PROCESSOR_INFO` → `GET_DAQ_RESOLUTION_INFO`/
    pub async fn set_connection_state(&mut self, resp: Option<&RespConnect>) {
        let connected = self.base.base.connected();
        self.base.set_connection_state(resp);
        let mut slave_now = self.base.base.slave_connected;
        if slave_now == connected {
            return;
        }
        if slave_now && !self.base.base.prevent_default_requests {
            if let Some(connect_resp) = resp {
                let (status_result, _) = self.get_status().await;
                if status_result == CmdResult::OK {
                    if connect_resp
                        .comm_mode_basic
                        .contains(CommModeBasic::OPTIONAL)
                    {
                        let _ = self.get_comm_mode_info().await;
                    }
                    let protection = self
                        .cached
                        .status
                        .as_ref()
                        .map_or(ResourceType::NONE, |s| s.resource_protection_state);
                    if self.is_allowed_request("GET_SEED")
                        && protection != ResourceType::NONE
                        && self.seed_and_key.is_some()
                    {
                        let _ = self.unlock_ecu_all().await;
                    }
                    if self.is_allowed_request("SET_CAL_PAGE")
                        && connect_resp.resource.contains(ResourceType::CAL_PAG)
                        && self.get_cal_page(CalPageMode::XCP, 0).await.0 == CmdResult::OK
                        && self.is_allowed_request("GET_PAG_PROCESSOR_INFO")
                    {
                        let _ = self.get_pag_processor_info().await;
                    }
                    if self.is_allowed_request("GET_PGM_PROCESSOR_INFO")
                        && connect_resp.resource.contains(ResourceType::PGM)
                    {
                        let _ = self.get_pgm_processor_info().await;
                    }
                    if connect_resp.resource.contains(ResourceType::DAQ) {
                        if self.is_allowed_request("GET_DAQ_RESOLUTION_INFO") {
                            let _ = self.get_daq_resolution_info().await;
                        }
                        if self.is_allowed_request("GET_DAQ_PROCESSOR_INFO") {
                            let (res, info) = self.get_daq_processor_info().await;
                            if res == CmdResult::OK {
                                if let Some(info) = info {
                                    self.trim_daq_map(&info);
                                }
                            }
                        }
                    }
                } else {
                    slave_now = false;
                }
            }
        }
        let changed = self.base.base.connected() != slave_now;
        self.base.base.set_connected(slave_now);
        if !self.base.base.connected() {
            self.cached = CachedResponses::default();
        }
        if changed {
            self.base.base.raise_connection_state_changed();
        }
    }

    fn trim_daq_map(&mut self, info: &RespGetDaqProcessorInfo) {
        if info.max_daq > 0 {
            while self.daq_and_evt_map.len() > info.max_daq as usize {
                let Some(&last) = self.daq_and_evt_map.keys().next_back() else {
                    break;
                };
                self.daq_and_evt_map.remove(&last);
            }
        }
        if info.max_event_channel > 0 {
            let stale: Vec<u16> = self
                .daq_and_evt_map
                .iter()
                .filter(|(_, dae)| dae.evt.id >= info.max_event_channel)
                .map(|(k, _)| *k)
                .collect();
            for k in stale {
                self.daq_and_evt_map.remove(&k);
            }
        }
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn connect(&mut self, mode: ConnectMode) -> (CmdResult, Option<RespConnect>) {
        let bytes = self.base.pack(&CmdConnect { mode }, &[]);
        let (result, resp, _) = self.base.exchange::<RespConnect>(&bytes, -1, 0).await;
        if result == CmdResult::OK {
            self.cached.connect = resp;
        }
        let resp_copy = resp;
        self.set_connection_state(resp_copy.as_ref()).await;
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_status(&mut self) -> (CmdResult, Option<RespGetStatus>) {
        let (result, resp) = self.base.get_status().await;
        if result == CmdResult::OK {
            self.cached.status = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_comm_mode_info(&mut self) -> (CmdResult, Option<RespGetCommModeInfo>) {
        let (result, resp) = self.base.get_comm_mode_info().await;
        if result == CmdResult::OK {
            self.cached.comm_mode_info = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_cal_page(
        &mut self,
        mode: CalPageMode,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetCalPage>) {
        let (result, resp) = self.base.get_cal_page(mode, segment_no).await;
        if result == CmdResult::OK {
            self.cached.cal_page = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_pag_processor_info(&mut self) -> (CmdResult, Option<RespGetPagProcessorInfo>) {
        let (result, resp) = self.base.get_pag_processor_info().await;
        if result == CmdResult::OK {
            self.cached.pag_proc_info = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_daq_processor_info(&mut self) -> (CmdResult, Option<RespGetDaqProcessorInfo>) {
        let (result, resp) = self.base.get_daq_processor_info().await;
        if result == CmdResult::OK {
            self.cached.daq_proc_info = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_daq_resolution_info(
        &mut self,
    ) -> (CmdResult, Option<RespGetDaqResolutionInfo>) {
        let (result, resp) = self.base.get_daq_resolution_info().await;
        if result == CmdResult::OK {
            self.cached.daq_resolution_info = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn get_pgm_processor_info(&mut self) -> (CmdResult, Option<RespGetPgmProcessorInfo>) {
        let (result, resp) = self.base.get_pgm_processor_info().await;
        if result == CmdResult::OK {
            self.cached.pgm_proc_info = resp;
        }
        self.drain_pending().await;
        (result, resp)
    }

    pub async fn disconnect(&mut self, use_timeout_ms: i32) -> bool {
        let _ = self.stop_measurements(true).await;
        self.daqs_mut().clear_data();
        let ok = self.base.disconnect(use_timeout_ms).await;
        self.base.base.set_connected(false);
        self.cached = CachedResponses::default();
        self.drain_pending().await;
        ok
    }

    pub async fn close(&mut self) {
        if self.base.base.connected() || self.base.base.slave_connected {
            let _ = self.disconnect(0).await;
        }
        self.daqs_mut().clear();
    }

    pub fn daqs(&self) -> &DaqDictXcp {
        &self.daqs_storage
    }

    pub fn daqs_mut(&mut self) -> &mut DaqDictXcp {
        &mut self.daqs_storage
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn drain_pending(&mut self) {
        while let Some(p) = self.base.pop_pending() {
            match p {
                PendingDispatch::Event(frame) => {
                    self.base.base.inc_events_received();
                    self.on_event_received(&frame).await;
                }
                PendingDispatch::Service(frame) => {
                    if let Some(serv) = RespService::decode(frame.data(), false) {
                        if let Some(cb) = self.callbacks.service_request.as_mut() {
                            cb(serv);
                        }
                    }
                }
                PendingDispatch::Daq(frame) => self.on_daq_frame_received(&frame),
                PendingDispatch::Error(err, code) => {
                    if let Some(cb) = self.callbacks.xcp_error.as_mut() {
                        cb(err, code);
                    }
                }
            }
        }
    }

    pub async fn poll(&mut self) {
        if let Some(t) = self.base.transport.as_mut() {
            t.poll().await;
            while let Some(frame) = t.next_frame() {
                self.base
                    .base
                    .record_frame_received(frame.raw_frame_length());
                match frame.data()[0] {
                    0xFD => self.base.pending.push_back(PendingDispatch::Event(frame)),
                    0xFC => self.base.pending.push_back(PendingDispatch::Service(frame)),
                    0xFE | 0xFF => self.base.pending.push_back(PendingDispatch::Daq(frame)),
                    _ => self.base.pending.push_back(PendingDispatch::Daq(frame)),
                }
            }
        }
        self.drain_pending().await;
    }

    pub async fn on_event_received(&mut self, frame: &XcpFrame) {
        let Some(ev) = EventBase::decode(frame.data()) else {
            return;
        };
        match ev.event_code {
            EventCodes::TimeSync => {
                if let Some(ts) =
                    EventTimeSync::decode(frame.data(), self.base.base.change_endianess())
                {
                    self.daq_clock = ts.timestamp;
                } else {
                    return;
                }
            }
            EventCodes::SessionTerminated => {
                Box::pin(self.set_connection_state(None)).await;
            }
            _ => {}
        }
        if let Some(cb) = self.callbacks.xcp_event.as_mut() {
            cb(ev);
        }
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn read_sync(
        &mut self,
        len: usize,
        address_extension: u8,
        address: u32,
        mut progress: ProgressCallback<'_>,
    ) -> (bool, Vec<u8>) {
        let mut data: Vec<u8> = Vec::new();
        if !self.base.base.slave_connected {
            return (false, data);
        }
        let address = self.map_address(address);
        let mut num = self.base.max_cto() as usize - 1;
        let mut result = CmdResult::OK;
        if address != u32::MAX && len <= num && self.is_allowed_request("SHORT_UPLOAD") {
            let (r, d) = self
                .base
                .short_upload(len as u8, address_extension, address)
                .await;
            result = r;
            data = d;
        } else {
            if address != u32::MAX
                && self.base.set_mta(address_extension, address).await != CmdResult::OK
            {
                if let Some(cb) = progress.as_mut() {
                    cb(&mut ProgressArgs::new(100));
                }
                return (false, data);
            }
            if self.is_slave_block_mode() {
                num = 255;
            }
            let step = (len / 1024).max(1);
            let mut next = step;
            let mut read = 0usize;
            while read < len {
                let n = (len - read).min(num);
                let (r, chunk) = self.base.upload(n as u8).await;
                result = r;
                if result != CmdResult::OK {
                    break;
                }
                data.extend_from_slice(&chunk);
                read += n;
                if let Some(cb) = progress.as_mut() {
                    if read > next {
                        let mut args = ProgressArgs::new((read as f64 * 100.0 / len as f64) as i32);
                        cb(&mut args);
                        if args.cancel {
                            if let Some(cb2) = progress.as_mut() {
                                cb2(&mut ProgressArgs::new(100));
                            }
                            return (false, data);
                        }
                        next += step;
                    }
                }
            }
        }
        if let Some(cb) = progress.as_mut() {
            cb(&mut ProgressArgs::new(100));
        }
        self.drain_pending().await;
        (result == CmdResult::OK, data)
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_program_loop<P: FnMut(&mut ProgressArgs) + ?Sized>(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        prog_start: Option<&RespProgramStart>,
        mut progress: Option<&mut P>,
    ) -> bool {
        if !self.base.base.slave_connected {
            return false;
        }
        let mut result = CmdResult::OK;
        let address = self.map_address(address);
        let r = async {
            if prog_start.is_none()
                && address != u32::MAX
                && data.len() < self.base.max_cto() as usize - 8
                && self.is_allowed_request("SHORT_DOWNLOAD")
            {
                result = self
                    .base
                    .short_download(data, address_extension, address)
                    .await;
                return result == CmdResult::OK;
            }
            if address != u32::MAX {
                result = self.base.set_mta(address_extension, address).await;
                if result != CmdResult::OK {
                    return false;
                }
            }
            let eff_cto = prog_start.map_or_else(|| self.base.max_cto(), |p| p.max_cto);
            let num = eff_cto as usize - 2;
            let mut val = num;
            let min_st = prog_start
                .map(|p| p.min_st)
                .or_else(|| self.cached.comm_mode_info.as_ref().map(|c| c.min_st))
                .unwrap_or(0);
            let mbm = self.is_master_block_mode(eff_cto);
            let programming = prog_start.is_some();
            let mut code = if programming {
                CommandCode::Program
            } else {
                CommandCode::Download
            };
            let use_max = !mbm
                && data.len() > num
                && if programming {
                    self.is_allowed_request("PROGRAM_MAX")
                } else {
                    self.is_allowed_request("DOWNLOAD_MAX")
                };
            if use_max {
                val = num + 1;
                code = if programming {
                    CommandCode::ProgramMax
                } else {
                    CommandCode::DownloadMax
                };
            }
            if mbm {
                let max_bs = prog_start
                    .map_or(self.base.protocol_layer.comm_modes_supported.max_bs, |p| {
                        p.max_bs
                    });
                val = (max_bs as usize * num).clamp(1, 255);
            }
            let mut num3 = data.len().min(val);
            let step = (data.len() / 100).max(1);
            let mut next = step;
            let mut done = 0usize;
            while done < data.len() {
                let n = num3.min(num);
                let chunk = &data[done..done + n];
                let timeout = if num3 > n { min_st as i32 } else { -1 };
                result = self
                    .base
                    .write_bytes(code, chunk, num3 as u8, timeout)
                    .await;
                if result != CmdResult::OK {
                    break;
                }
                done += n;
                num3 -= n;
                if num3 == 0 {
                    num3 = (data.len() - done).min(val);
                    let use_max2 = use_max && data.len() - done >= num;
                    code = match (programming, use_max2) {
                        (false, false) => CommandCode::Download,
                        (false, true) => CommandCode::DownloadMax,
                        (true, false) => CommandCode::Program,
                        (true, true) => CommandCode::ProgramMax,
                    };
                } else {
                    code = if programming {
                        CommandCode::ProgramNext
                    } else {
                        CommandCode::DownloadNext
                    };
                }
                if let Some(cb) = progress.as_mut() {
                    if done > next {
                        let mut args =
                            ProgressArgs::new((done as f64 * 100.0 / data.len() as f64) as i32);
                        cb(&mut args);
                        if args.cancel {
                            return false;
                        }
                        next += step;
                    }
                }
            }
            result == CmdResult::OK
        }
        .await;
        if let Some(cb) = progress.as_mut() {
            cb(&mut ProgressArgs::new(100));
        }
        self.drain_pending().await;
        r
    }

    pub async fn write_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
    ) -> bool {
        self.write_program_loop(address_extension, address, data, None, progress)
            .await
    }

    pub async fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
        modes: Option<&XcpPrgParams>,
    ) -> bool {
        let mut progress = progress;
        let (mut result, prog_start) = self.base.program_start().await;
        if result != CmdResult::OK {
            return false;
        }
        let Some(prog_start) = prog_start else {
            return false;
        };
        if !self.is_allowed_request("SET_MTA")
            || (self.is_master_block_mode(prog_start.max_cto)
                && !self.is_allowed_request("PROGRAM_NEXT"))
        {
            return false;
        }
        if let Some(m) = modes {
            result = self
                .base
                .program_clear(m.clear_mode, data.len() as u32)
                .await;
            if result != CmdResult::OK {
                return false;
            }
        }
        result = if self
            .write_program_loop(
                address_extension,
                address,
                data,
                Some(&prog_start),
                progress.as_mut(),
            )
            .await
        {
            CmdResult::OK
        } else {
            CmdResult::ERR_GENERIC
        };
        if result == CmdResult::OK {
            if let Some(m) = modes {
                if m.verify_mode != ProgramVerifyMode::None
                    && self.is_allowed_request("PROGRAM_VERIFY")
                {
                    result = self
                        .base
                        .program_verify(m.verify_mode, m.verify_type, m.verify_value)
                        .await;
                }
            }
        }
        if result == CmdResult::OK {
            let _ = self.base.program_reset().await;
        }
        result == CmdResult::OK
    }

    pub async fn program_sync_segments(
        &mut self,
        address_extension: u8,
        segments: &[XcpMemorySegment],
        mut progress: ProgressCallback<'_>,
        connect_mode: ConnectMode,
        modes: Option<&XcpPrgParams>,
    ) -> bool {
        if connect_mode != ConnectMode::Normal {
            if self.base.base.connected() {
                let _ = self.disconnect(-1).await;
            }
            self.base.base.connect_behaviour = ConnectBehaviourType::Manual;
            if self.connect(connect_mode).await.0 != CmdResult::OK {
                return false;
            }
            let (res, status) = self.get_status().await;
            if res != CmdResult::OK {
                return false;
            }
            if self.seed_and_key.is_some() {
                let Some(status) = status else { return false };
                if self.unlock_ecu(&status, ResourceType::PGM).await != CmdResult::OK {
                    return false;
                }
            }
        }
        let (result, prog_start) = self.base.program_start().await;
        if result != CmdResult::OK {
            return false;
        }
        let Some(prog_start) = prog_start else {
            return false;
        };
        for segment in segments.iter() {
            if !self.is_allowed_request("SET_MTA")
                || (self.is_master_block_mode(prog_start.max_cto)
                    && !self.is_allowed_request("PROGRAM_NEXT"))
            {
                return false;
            }
            if let Some(m) = modes {
                if self
                    .base
                    .program_clear(m.clear_mode, segment.size as u32)
                    .await
                    != CmdResult::OK
                {
                    return false;
                }
            }
            let Some(data) = segment.get_data_bytes(segment.address, segment.size as u32) else {
                return false;
            };
            if !self
                .write_program_loop(
                    address_extension,
                    segment.address as u32,
                    &data,
                    Some(&prog_start),
                    progress.as_mut(),
                )
                .await
            {
                return false;
            }
        }
        if let Some(m) = modes {
            if m.verify_mode != ProgramVerifyMode::None
                && self.is_allowed_request("PROGRAM_VERIFY")
                && self
                    .base
                    .program_verify(m.verify_mode, m.verify_type, m.verify_value)
                    .await
                    != CmdResult::OK
            {
                return false;
            }
        }
        let _ = self.base.program_reset().await;
        true
    }

    pub async fn modify_bits_guarded(
        &mut self,
        address_extension: u8,
        address: u32,
        s: u8,
        ma: u16,
        mx: u16,
    ) -> bool {
        if !self.is_allowed_request("SET_MTA") || !self.is_allowed_request("MODIFY_BITS") {
            return false;
        }
        if self.base.set_mta(address_extension, address).await != CmdResult::OK {
            return false;
        }
        self.base.modify_bits(s, ma, mx).await == CmdResult::OK
    }

    pub async fn get_id_data(&mut self, id_type: GetIdType) -> Option<Vec<u8>> {
        if !self.is_allowed_request("GET_ID") {
            return None;
        }
        let (result, resp, mut data) = self.base.get_id(id_type).await;
        if result != CmdResult::OK {
            return None;
        }
        let resp = resp?;
        if !resp.mode.contains(GetIdRespType::TRANSFER_MODE) {
            let (ok, d) = self
                .read_sync(resp.length as usize, 0, u32::MAX, None)
                .await;
            if !ok {
                return None;
            }
            data = d;
        }
        Some(data)
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn unlock_ecu(
        &mut self,
        status: &RespGetStatus,
        resource: ResourceType,
    ) -> CmdResult {
        if !self.base.base.slave_connected {
            return CmdResult::ERR_TIMEOUT;
        }
        if !status.resource_protection_state.contains(resource) {
            return CmdResult::OK;
        }
        let (mut result, seed_resp, seed) =
            self.base.get_seed(SeedModeType::FirstPart, resource).await;
        let Some(seed_resp) = seed_resp else {
            return result;
        };
        if result != CmdResult::OK || seed_resp.length == 0 {
            return result;
        }
        let mut seed_all = seed;
        if seed_resp.length > self.base.max_cto() - 2 {
            loop {
                let (r, resp2, chunk) = self
                    .base
                    .get_seed(SeedModeType::RemainingPart, resource)
                    .await;
                result = r;
                if result != CmdResult::OK {
                    return result;
                }
                let remaining = resp2.map_or(0, |r2| r2.length);
                seed_all.extend_from_slice(&chunk);
                if chunk.len() >= remaining as usize {
                    break;
                }
            }
        }
        let Some(provider) = self.seed_and_key.as_ref() else {
            return CmdResult::ERR_GENERIC;
        };
        let key = match provider.compute_key_from_seed(resource.bits(), &seed_all) {
            Ok(k) => k,
            Err(_) => return CmdResult::ERR_GENERIC,
        };
        if key.is_empty() {
            return CmdResult::ERR_GENERIC;
        }
        let mut offset = 0usize;
        let mut last_resp = None;
        while offset != key.len() {
            let (r, resp2) = self.base.unlock(&key, &mut offset).await;
            result = r;
            if result != CmdResult::OK {
                return result;
            }
            last_resp = resp2;
        }
        match last_resp {
            Some(r2) if r2.protection_state.contains(resource) => CmdResult::ERR_ACCESS_LOCKED,
            _ => CmdResult::OK,
        }
    }

    pub async fn unlock_ecu_all(&mut self) -> CmdResult {
        if self
            .seed_and_key
            .as_ref()
            .is_some_and(|p| !p.supports_xcp())
        {
            return CmdResult::ERR_GENERIC;
        }
        if !self.base.base.slave_connected {
            return CmdResult::ERR_TIMEOUT;
        }
        let Some(status) = self.cached.status else {
            return CmdResult::ERR_PROTOCOL_FAILURE;
        };
        let mut result = CmdResult::OK;
        for resource in [
            ResourceType::CAL_PAG,
            ResourceType::DAQ,
            ResourceType::PGM,
            ResourceType::STIM,
        ] {
            if status.resource_protection_state.contains(resource) {
                let r = self.unlock_ecu(&status, resource).await;
                if r != CmdResult::OK {
                    result = r;
                }
            }
        }
        let _ = self.get_status().await;
        result
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn set_page(&mut self, page: EcuPage) -> bool {
        if !self.base.base.slave_connected {
            return false;
        }
        if self.active_page() == page {
            return true;
        }
        self.cached.cal_page = None;
        if self
            .base
            .set_cal_page(
                CalPageMode::ECU | CalPageMode::XCP | CalPageMode::ALL,
                0,
                page as u8,
            )
            .await
            != CmdResult::OK
        {
            return false;
        }
        self.get_cal_page(CalPageMode::XCP, 0).await.0 == CmdResult::OK
    }

    pub async fn freeze_page(&mut self, segment: u8, timeout_ms: u32) -> CmdResult {
        if !self.base.base.slave_connected {
            return CmdResult::ERR_TIMEOUT;
        }
        let _ = self.stop_measurements(true).await;
        let mut result = self
            .base
            .set_segment_mode(SegmentMode::FREEZE, segment)
            .await;
        if result != CmdResult::OK {
            return result;
        }
        let r = async {
            result = self
                .base
                .set_request(SetRequestMode::STORE_CAL_REQUEST, 0)
                .await;
            if result != CmdResult::OK {
                return result;
            }
            let start = Instant::now();
            loop {
                autors_runtime::sleep(Duration::from_millis(
                    self.base.protocol_layer.timings[0] as u64,
                ))
                .await;
                if start.elapsed() > Duration::from_millis(timeout_ms as u64) {
                    return CmdResult::ERR_TIMEOUT;
                }
                let (res, status) = self.get_status().await;
                result = res;
                if result != CmdResult::OK {
                    return result;
                }
                if let Some(s) = status {
                    if !s.session_state.contains(SessionState::STORE_CAL_REQUEST) {
                        return result;
                    }
                }
            }
        }
        .await;
        if r == CmdResult::OK {
            let _ = self.base.set_segment_mode(SegmentMode::NONE, segment).await;
        }
        r
    }

    pub async fn copy_page2page(&mut self, src_page: EcuPage, dst_page: EcuPage) -> Result<bool> {
        if src_page == dst_page {
            return Err(Error::Protocol(
                "copy must be called with different pages".to_string(),
            ));
        }
        if dst_page == EcuPage::Flash {
            if let Some(info) = &self.cached.pag_proc_info {
                if info
                    .properties
                    .contains(crate::ifdata_xcp::PagProperties::FREEZE_SUPPORTED)
                {
                    return Ok(self.freeze_page(0, 30000).await == CmdResult::OK);
                }
            }
        }
        Ok(self
            .base
            .copy_cal_page(0, src_page as u8, 0, dst_page as u8)
            .await
            == CmdResult::OK)
    }

    /// Computes a checksum over `size` bytes at the given address (SET_MTA + BUILD_CHECKSUM).
    /// Returns `None` if either command fails.
    pub async fn get_checksum(
        &mut self,
        address_extension: u8,
        address: u32,
        size: u32,
    ) -> Option<u32> {
        if self.base.set_mta(address_extension, address).await != CmdResult::OK {
            return None;
        }
        let (result, resp) = self.base.build_checksum(size).await;
        if result != CmdResult::OK {
            return None;
        }
        resp.map(|r| r.checksum)
    }

    // ------------------------------------------------------------------
    // DAQ measurement (configure / start / stop, DAQ frame reception,
    // slave ID scan)
    // ------------------------------------------------------------------

    /// Registers the measurement list; fill errors are returned as `Err`.
    pub fn configure_measurements(&mut self, measurements: &mut Vec<DaqMeasurement>) -> Result<()> {
        self.daqs_mut().clear();
        self.daq_config_changed = true;
        if !self.daq_and_evt_map.is_empty() {
            let absolute = self
                .daq
                .as_ref()
                .is_some_and(|d| d.id_field == XcpIdFieldType::ABSOLUTE);
            let max_dto = self.base.max_dto();
            let off_first = self.data_offset_first_pid;
            let off = self.data_offset;
            let map = std::mem::take(&mut self.daq_and_evt_map);
            self.daqs_mut()
                .fill(max_dto, off_first, off, absolute, &map, measurements)?;
            self.daq_and_evt_map = map;
        }
        Ok(())
    }

    /// Dynamic/static DAQ allocation sequence
    /// (FREE_DAQ/ALLOC_DAQ/ALLOC_ODT/ALLOC_ODT_ENTRY or CLEAR_DAQ_LIST).
    async fn alloc_daq_lists(&mut self, snapshot: &[(usize, u16, u16, u8, usize)]) -> bool {
        let daq = self.daq.clone().unwrap_or_default();
        if daq.mode != XcpDaqMode::DYNAMIC {
            for &(_, daq_no, _, _, _) in snapshot {
                if self.base.clear_daq_list(daq_no).await != CmdResult::OK {
                    return false;
                }
            }
            return true;
        }
        if self.base.free_daq().await != CmdResult::OK {
            return false;
        }
        let num = (daq.max_daq as i32 - daq.min_daq as i32).max(snapshot.len() as i32) as u16;
        if num > 0 && self.base.alloc_daq(num).await != CmdResult::OK {
            return false;
        }
        for &(_, daq_no, _, _, odt_count) in snapshot {
            if daq_no >= daq.min_daq as u16
                && odt_count > 0
                && self.base.alloc_odt(daq_no, odt_count as u8).await != CmdResult::OK
            {
                return false;
            }
        }
        for &(idx, daq_no, _, _, _) in snapshot {
            if daq_no < daq.min_daq as u16 {
                continue;
            }
            let odts: Vec<(u8, usize)> = self.daqs().lists[idx]
                .base
                .odts
                .iter()
                .map(|(&k, o)| (k, o.entries.len()))
                .collect();
            for (odt_no, entry_count) in odts {
                if self
                    .base
                    .alloc_odt_entry(daq_no, odt_no, entry_count as u8)
                    .await
                    != CmdResult::OK
                {
                    return false;
                }
            }
        }
        true
    }

    /// SET_DAQ_PTR + WRITE_DAQ(_MULTIPLE) sequence for one DAQ list.
    async fn write_daq_entries(&mut self, idx: usize) -> bool {
        let daq_no = self.daqs().lists[idx].daq_no;
        let odts: Vec<(u8, Vec<Arc<OdtEntry>>)> = self.daqs().lists[idx]
            .base
            .odts
            .iter()
            .map(|(&k, o)| (k, o.entries.clone()))
            .collect();
        for (odt_no, entries) in odts {
            if self.base.set_daq_ptr(daq_no, odt_no, 0).await != CmdResult::OK {
                return false;
            }
            if entries.len() > 1
                && self.base.max_cto() >= 18
                && self.is_allowed_request("WRITE_DAQ_MULTIPLE")
            {
                if self.base.write_daq_multiple(&entries).await != CmdResult::OK {
                    return false;
                }
                continue;
            }
            for e in &entries {
                // The address extension field of WRITE_DAQ is a single byte.
                if self
                    .base
                    .write_daq(
                        e.bit_offset,
                        e.size,
                        e.measurement.address_extension as u8,
                        e.address(),
                    )
                    .await
                    != CmdResult::OK
                {
                    return false;
                }
            }
        }
        true
    }

    /// Snapshot of the DAQ lists: `(index, daq_no, evt_id, evt_priority, odt count)`.
    fn daq_snapshot(&self) -> Vec<(usize, u16, u16, u8, usize)> {
        self.daqs()
            .lists
            .iter()
            .enumerate()
            .map(|(i, l)| (i, l.daq_no, l.evt.id, l.evt.priority, l.base.odts.len()))
            .collect()
    }

    /// Allocates and starts all configured DAQ lists.
    pub async fn start_measurements(&mut self, do_synchronized: bool) -> bool {
        if !self.base.base.connected() || self.is_daq_running {
            return false;
        }
        self.daqs_mut().clear_data();
        let snapshot = self.daq_snapshot();
        if snapshot.is_empty() {
            return true;
        }
        self.base.base.daq_clock.reset();
        if self.daq_config_changed {
            if !self.alloc_daq_lists(&snapshot).await {
                return false;
            }
            let ts_enabled = self
                .timestamp_supported
                .as_ref()
                .is_some_and(|t| t.size != XcpTimestampSize::NotSet);
            for &(idx, daq_no, evt_id, evt_priority, _) in &snapshot {
                if !self.write_daq_entries(idx).await {
                    return false;
                }
                let mut mode = DaqListMode::NONE;
                if ts_enabled {
                    mode |= DaqListMode::TIMESTAMP;
                }
                if self
                    .base
                    .set_daq_list_mode(mode, daq_no, evt_id, 1, evt_priority)
                    .await
                    != CmdResult::OK
                {
                    return false;
                }
            }
            self.daq_config_changed = false;
        }
        if self
            .base
            .protocol_layer
            .optional_cmds
            .iter()
            .any(|c| c == "GET_DAQ_CLOCK")
            && self
                .cached
                .daq_proc_info
                .as_ref()
                .is_some_and(|i| i.properties.contains(DaqProperties::TIMESTAMP_SUPPORTED))
        {
            let (_, resp) = self.base.get_daq_clock().await;
            self.daq_clock = resp.map_or(0, |r| r.timestamp);
        } else {
            self.daq_clock = 0;
        }
        let mode = if do_synchronized {
            StartStopMode::Select
        } else {
            StartStopMode::Start
        };
        let absolute = self
            .daq
            .as_ref()
            .is_some_and(|d| d.id_field == XcpIdFieldType::ABSOLUTE);
        for &(idx, daq_no, _, _, _) in &snapshot {
            let (res, resp) = self.base.start_stop_daq_list(mode, daq_no).await;
            if res == CmdResult::OK && absolute {
                if let Some(r) = resp {
                    self.daqs_mut().lists[idx].first_pid = r.first_pid;
                }
            }
        }
        if do_synchronized
            && self.base.start_stop_synch(StartStopMode::Start).await != CmdResult::OK
        {
            return false;
        }
        self.is_daq_running = true;
        self.drain_pending().await;
        true
    }

    /// Stops all running DAQ lists; the running flag is always reset afterwards.
    pub async fn stop_measurements(&mut self, do_synchronized: bool) -> bool {
        let r = async {
            let snapshot = self.daq_snapshot();
            if !self.is_daq_running {
                return true;
            }
            let mode = if do_synchronized {
                StartStopMode::Select
            } else {
                StartStopMode::Stop
            };
            for &(_, daq_no, _, _, odt_count) in &snapshot {
                if odt_count > 0 {
                    let _ = self.base.start_stop_daq_list(mode, daq_no).await;
                }
            }
            if do_synchronized
                && self.base.start_stop_synch(StartStopMode::Stop).await != CmdResult::OK
            {
                return false;
            }
            true
        }
        .await;
        self.is_daq_running = false;
        self.drain_pending().await;
        r
    }

    /// Handles a received DAQ frame: PID resolution → timestamp → packet assembly callback → cache.
    pub fn on_daq_frame_received(&mut self, frame: &XcpFrame) {
        if !self.base.base.connected() || !self.is_daq_running {
            return;
        }
        let id_field = self
            .daq
            .as_ref()
            .map_or(XcpIdFieldType::NotSet, |d| d.id_field);
        let swap = self.base.base.change_endianess();
        let Some((idx, odt_key, offset)) = self.daqs().resolve(frame, id_field, swap) else {
            return;
        };
        let odt_pid_and_bytes = {
            let list = &self.daqs().lists[idx];
            if list.base.cache.is_none() {
                return;
            }
            list.base.odts.get(&odt_key).map(|o| (o.pid, o.size))
        };
        let Some((odt_pid, odt_bytes)) = odt_pid_and_bytes else {
            return;
        };
        if odt_pid == 0 {
            let ts_supported = self.timestamp_supported.clone();
            let ts_delta = {
                let list = &mut self.daqs_mut().lists[idx];
                list.decode_timestamp(frame.data(), offset, ts_supported.as_ref(), swap)
            };
            let elapsed = self.base.base.daq_clock.elapsed_daq_seconds();
            let (new_ts, combined) = {
                let list = &mut self.daqs_mut().lists[idx];
                // Fall back to the host DAQ clock when the timestamp delta is invalid
                // (NaN) or no base time exists yet.
                let new_ts = if ts_delta.is_nan() || list.base.last_timestamp.is_nan() {
                    elapsed
                } else {
                    list.base.last_timestamp + ts_delta
                };
                list.base.last_timestamp = new_ts;
                let combined = list.base.cache.as_mut().and_then(|c| c.complete());
                (new_ts, combined)
            };
            self.base.base.daq_clock.set_last_timestamp(new_ts);
            if let Some(array) = combined {
                let daq_no = self.daqs().lists[idx].daq_no;
                // Raise the values-received callback (base callback table).
                self.base
                    .base
                    .raise_on_values_received(daq_no as i32, new_ts, &array);
                self.daqs_mut().lists[idx]
                    .base
                    .values
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .add(new_ts, array);
            }
        }
        if !self.daqs().lists[idx].base.last_timestamp.is_nan() {
            let num3 = if odt_pid == 0 {
                self.data_offset_first_pid
            } else {
                self.data_offset
            };
            let trailing = frame.data().len().saturating_sub(odt_bytes as usize + num3);
            if let Some(c) = self.daqs_mut().lists[idx].base.cache.as_mut() {
                c.insert(frame.data().to_vec(), num3, trailing);
            }
        }
    }

    /// XCP-on-CAN slave identification (two-phase echo; CAN only).
    /// Returns `(result code, [(slave_cmd_id, resp_id)])`; only slaves that respond
    /// to both the IdentifyByEcho and the ConfirmByInverseEcho phase are included
    /// (the per-entry confirmation flag).
    /// The CAN device may be shared with the master transport (locks are acquired in
    /// short scopes, never held across calls).
    pub async fn get_slave_ids(
        &mut self,
        device: &Arc<Mutex<dyn CanDevice + Send>>,
        timeout: u16,
    ) -> Result<(CmdResult, Vec<(u32, u32)>)> {
        if self.base.type_ != XcpType::Can {
            return Err(Error::Protocol(
                "This method is only supported using XCPonCAN".to_string(),
            ));
        }
        const SLAVE_ID_PATTERN: [u8; 3] = *b"XCP";
        struct Shared {
            mode: u8,
            map: std::collections::HashMap<u32, (u32, bool)>,
        }
        let shared = Arc::new(Mutex::new(Shared {
            mode: GetSlaveIdMode::IdentifyByEcho.as_u8(),
            map: std::collections::HashMap::new(),
        }));
        let cb_shared = Arc::clone(&shared);
        let token = device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .register_listener(
                None,
                Box::new(move |frame: &CanFrame| {
                    if frame.is_master_frame || frame.data.len() < 8 {
                        return;
                    }
                    let num = u32_le(&frame.data, 4);
                    let mut echo = SLAVE_ID_PATTERN;
                    let mut guard = cb_shared.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.mode == GetSlaveIdMode::ConfirmByInverseEcho.as_u8() {
                        for b in &mut echo {
                            *b = !*b;
                        }
                    }
                    if frame.data[1] == echo[0]
                        && frame.data[2] == echo[1]
                        && frame.data[3] == echo[2]
                    {
                        let confirmed = guard.mode == GetSlaveIdMode::ConfirmByInverseEcho.as_u8()
                            && guard.map.contains_key(&num);
                        guard.map.insert(num, (frame.id, confirmed));
                    }
                }),
            );
        let timeout = timeout.max(self.base.protocol_layer.timings[0]);
        let mut result = CmdResult::OK;
        let mut pairs = Vec::new();
        'outer: {
            loop {
                let mode = shared.lock().unwrap_or_else(|p| p.into_inner()).mode;
                if mode > GetSlaveIdMode::ConfirmByInverseEcho.as_u8() {
                    break;
                }
                let mut bytes = SLAVE_ID_PATTERN.to_vec();
                bytes.push(mode);
                let (res, _) = self.base.transport_layer_cmd(0xFF, &bytes, 0).await;
                if res != CmdResult::OK {
                    result = res;
                    break 'outer;
                }
                block_for_micro_secs(1000 * timeout as u64).await;
                shared.lock().unwrap_or_else(|p| p.into_inner()).mode += 1;
            }
            let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            for (&num, &(id, confirmed)) in &guard.map {
                if confirmed {
                    pairs.push((num, id));
                }
            }
        }
        device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unregister_listener(token);
        Ok((result, pairs))
    }

    pub fn into_shared(self) -> Result<Arc<Mutex<Self>>> {
        let shared = Arc::new(Mutex::new(self));
        CommKernel::register_client(&shared)?;
        Ok(shared)
    }
}

#[async_trait]
impl CommMasterHandle for XcpMaster {
    async fn poll_alive(&mut self) {
        if self.base.base.connect_behaviour != ConnectBehaviourType::Automatic {
            return;
        }
        let elapsed = now_elapsed().saturating_sub(self.base.base.last_received_time());
        if (elapsed.as_millis() as i32) < self.base.alive_cycle_time() {
            return;
        }
        if !self.base.base.slave_connected {
            let _ = self.connect(ConnectMode::Normal).await;
        } else {
            let _ = self.get_status().await;
        }
    }

    fn name(&self) -> &str {
        &self.base.base.name
    }
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use autors_can::device::DeviceCore;
    use autors_can::frame::{CanConfiguration, FrameType};
    use std::collections::VecDeque;

    #[cfg(feature = "blocking")]
    use crate::blocking::{BlockingXcpMaster, BlockingXcpMasterBase};
    #[cfg(feature = "blocking")]
    use autors_comm::blocking::BlockingCommKernel;

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[test]
    fn command_code_values() {
        assert_eq!(CommandCode::Connect.as_u8(), 0xFF);
        assert_eq!(CommandCode::Disconnect.as_u8(), 0xFE);
        assert_eq!(CommandCode::SetMta.as_u8(), 0xF6);
        assert_eq!(CommandCode::Upload.as_u8(), 0xF5);
        assert_eq!(CommandCode::Download.as_u8(), 0xF0);
        assert_eq!(CommandCode::SetCalPage.as_u8(), 0xEB);
        assert_eq!(CommandCode::ClearDaqList.as_u8(), 0xE3);
        assert_eq!(CommandCode::SetDaqPtr.as_u8(), 0xE2);
        assert_eq!(CommandCode::WriteDaq.as_u8(), 0xE1);
        assert_eq!(CommandCode::FreeDAQ.as_u8(), 0xD6);
        assert_eq!(CommandCode::AllocODTEntry.as_u8(), 0xD3);
        assert_eq!(CommandCode::ProgramStart.as_u8(), 0xD2);
        assert_eq!(CommandCode::ProgramVerify.as_u8(), 0xC8);
        assert_eq!(CommandCode::WriteDaqMultiple.as_u8(), 0xC7);
        assert_eq!(CommandCode::TimeCorrelationProperties.as_u8(), 0xC6);
        assert_eq!(CommandCode::DtoCtrProperties.as_u8(), 0xC5);
        assert_eq!(CommandCode::from_u8(0xF6), Some(CommandCode::SetMta));
        assert_eq!(CommandCode::from_u8(0x00), None);
        assert_eq!(CommandCode::SetMta.cs_name(), "SetMTA");
        assert_eq!(CommandCode::DtoCtrProperties.cs_name(), "DtoCtrPproperties");
        assert_eq!(CommandCode::Connect.to_string(), "Connect");
    }

    #[test]
    fn cmd_result_codes() {
        assert_eq!(CmdResult::OK.as_i32(), 0xFF);
        assert_eq!(CmdResult::ERR_CMD_SYNCH.as_i32(), 0);
        assert_eq!(CmdResult::ERR_OUT_OF_RANGE.as_i32(), 0x22);
        assert_eq!(CmdResult::ERR_MEMORY_OVERFLOW.as_i32(), 0x30);
        assert_eq!(CmdResult::ERR_SUBCMD_UNKNOWN.as_i32(), 0x34);
        assert_eq!(CmdResult::ERR_SND_CMD_FAILED.as_i32(), 0x100);
        assert_eq!(CmdResult::ERR_TIMEOUT.as_i32(), 0x101);
        assert_eq!(CmdResult::ERR_PROTOCOL_FAILURE.as_i32(), 0x104);
        assert_eq!(CmdResult::from_wire_u8(0x22), CmdResult::ERR_OUT_OF_RANGE);
        assert_eq!(CmdResult::ERR_OUT_OF_RANGE.as_wire_u8(), 0x22);
        assert_eq!(CmdResult::OK.description(), Some("Successful"));
        assert_eq!(CmdResult::ERR_TIMEOUT.description(), Some("Timeout"));
        assert_eq!(CmdResult(7).description(), None);
        assert_eq!(CmdResult::ERR_OUT_OF_RANGE.to_string(), "ERR_OUT_OF_RANGE");
        assert_eq!(CmdResult(7).to_string(), "7");
    }

    #[test]
    fn cmd_encode_little_endian() {
        assert_eq!(
            CmdConnect {
                mode: ConnectMode::Normal
            }
            .encode(false),
            [0xFF, 0x00]
        );
        assert_eq!(
            CmdConnect {
                mode: ConnectMode::UserDefined
            }
            .encode(false),
            [0xFF, 0x01]
        );
        assert_eq!(CmdDisconnect.encode(false), [0xFE]);
        assert_eq!(CmdBare::new(CommandCode::GetStatus).encode(false), [0xFD]);
        assert_eq!(CmdBare::new(CommandCode::Synch).encode(false), [0xFC]);
        assert_eq!(
            CmdGetId {
                id_type: GetIdType::Asap2FileName
            }
            .encode(false),
            [0xFA, 0x02]
        );
        assert_eq!(
            CmdSetRequest {
                mode: SetRequestMode::STORE_CAL_REQUEST,
                session_id: 0x1234
            }
            .encode(false),
            [0xF9, 0x01, 0x34, 0x12]
        );
        assert_eq!(
            CmdGetSeed {
                mode: SeedModeType::FirstPart,
                resource: ResourceType::CAL_PAG
            }
            .encode(false),
            [0xF8, 0x00, 0x01]
        );
        assert_eq!(CmdUnlock { remaining_len: 6 }.encode(false), [0xF7, 0x06]);
        assert_eq!(
            CmdSetMta {
                address_extension: 0x55,
                address: 0x11223344
            }
            .encode(false),
            [0xF6, 0x00, 0x00, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(
            CmdUpload {
                number_of_elements: 4
            }
            .encode(false),
            [0xF5, 0x04]
        );
        assert_eq!(
            CmdShortUpload {
                number_of_elements: 2,
                address_extension: 0,
                address: 0x1000
            }
            .encode(false),
            [0xF4, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]
        );
        assert_eq!(
            CmdBuildChecksum {
                block_size: 0x12345678
            }
            .encode(false),
            [0xF3, 0x00, 0x00, 0x00, 0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(
            CmdUserTransCmd {
                cmd_code: CommandCode::TransportLayerCmd,
                sub_command: 0xFE
            }
            .encode(false),
            [0xF2, 0xFE]
        );
        assert_eq!(
            CmdDownload {
                cmd_code: CommandCode::Download,
                number_of_elements: 3
            }
            .encode(false),
            [0xF0, 0x03]
        );
        assert_eq!(
            CmdShortDownload {
                number_of_elements: 2,
                address_extension: 1,
                address: 0x2000
            }
            .encode(false),
            [0xED, 0x02, 0x00, 0x01, 0x00, 0x20, 0x00, 0x00]
        );
        assert_eq!(
            CmdModifyBits {
                shift_value: 1,
                and_mask: 0xFF0F,
                xor_mask: 0x00F0
            }
            .encode(false),
            [0xEC, 0x01, 0x0F, 0xFF, 0xF0, 0x00]
        );
        assert_eq!(
            CmdSetCalPage {
                mode: CalPageMode::ECU | CalPageMode::XCP,
                segment_no: 0,
                page_no: 1
            }
            .encode(false),
            [0xEB, 0x03, 0x00, 0x01]
        );
        assert_eq!(
            CmdGetCalPage {
                mode: CalPageMode::XCP,
                segment_no: 0
            }
            .encode(false),
            [0xEA, 0x02, 0x00]
        );
        assert_eq!(
            CmdGetSegmentInfo::standard(2).encode(false),
            [0xE8, 0x01, 0x02, 0x00, 0x00]
        );
        assert_eq!(
            CmdGetSegmentInfo::address(BasicAddressModeType::Length, 3).encode(false),
            [0xE8, 0x00, 0x03, 0x01, 0x00]
        );
        assert_eq!(
            CmdGetSegmentInfo::mapping(MappingInfoModeType::DestinationAddress, 1, 5).encode(false),
            [0xE8, 0x02, 0x01, 0x01, 0x05]
        );
        assert_eq!(
            CmdGetPageInfo {
                segment_no: 1,
                page_no: 2
            }
            .encode(false),
            [0xE7, 0x01, 0x02]
        );
        assert_eq!(
            CmdSetSegmentMode {
                mode: SegmentMode::FREEZE,
                segment_no: 4
            }
            .encode(false),
            [0xE6, 0x01, 0x04]
        );
        assert_eq!(
            CmdGetSegmentMode { segment_no: 4 }.encode(false),
            [0xE5, 0x00, 0x04]
        );
        assert_eq!(
            CmdCopyCalPage {
                src_segment_no: 0,
                src_page_no: 1,
                dst_segment_no: 0,
                dst_page_no: 0
            }
            .encode(false),
            [0xE4, 0x00, 0x01, 0x00, 0x00]
        );
        assert_eq!(
            CmdList {
                cmd_code: CommandCode::GetDaqListMode,
                list_no: 0x1234
            }
            .encode(false),
            [0xDF, 0x00, 0x34, 0x12]
        );
        assert_eq!(
            CmdSetDaqPtr {
                daq_list_no: 1,
                odt_no: 2,
                odt_entry_no: 3
            }
            .encode(false),
            [0xE2, 0x00, 0x01, 0x00, 0x02, 0x03]
        );
        assert_eq!(
            CmdWriteDaq {
                bit_offset: 0xFF,
                element_size: 2,
                address_extension: 0,
                address: 0x3000
            }
            .encode(false),
            [0xE1, 0xFF, 0x02, 0x00, 0x00, 0x30, 0x00, 0x00]
        );
        assert_eq!(
            CmdWriteDaqMultiple { record_count: 2 }.encode(false),
            [0xC7, 0x02]
        );
        assert_eq!(
            CmdSetDaqListMode {
                mode: DaqListMode::TIMESTAMP,
                daq_list_no: 1,
                event_channel_no: 5,
                prescaler: 1,
                priority: 0,
            }
            .encode(false),
            [0xE0, 0x10, 0x01, 0x00, 0x05, 0x00, 0x01, 0x00]
        );
        assert_eq!(
            CmdStartStopDaqList {
                mode: StartStopMode::Select,
                daq_list_no: 2
            }
            .encode(false),
            [0xDE, 0x02, 0x02, 0x00]
        );
        assert_eq!(
            CmdStartStopSynch {
                mode: StartStopMode::Start
            }
            .encode(false),
            [0xDD, 0x01]
        );
        assert_eq!(
            CmdAllocDaq { daq_count: 3 }.encode(false),
            [0xD5, 0x00, 0x03, 0x00]
        );
        assert_eq!(
            CmdAllocOdt {
                daq_list_no: 1,
                odt_count: 2
            }
            .encode(false),
            [0xD4, 0x00, 0x01, 0x00, 0x02]
        );
        assert_eq!(
            CmdAllocOdtEntry {
                daq_list_no: 1,
                odt_no: 0,
                odt_entries_count: 4
            }
            .encode(false),
            [0xD3, 0x00, 0x01, 0x00, 0x00, 0x04]
        );
        assert_eq!(
            CmdProgramClear {
                mode: ProgramClearMode::AbsoluteAccess,
                clear_range: 0x100
            }
            .encode(false),
            [0xD1, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
        );
        assert_eq!(
            CmdGetSectorInfo {
                mode: GetSectorInfoMode::StartAddress,
                sector_no: 2
            }
            .encode(false),
            [0xCD, 0x00, 0x02]
        );
        assert_eq!(
            CmdProgramPrepare { code_size: 0x40 }.encode(false),
            [0xCC, 0x00, 0x40, 0x00]
        );
        assert_eq!(
            CmdProgramFormat {
                compression_method: 1,
                encryption_method: 2,
                programming_method: 3,
                access_method: 4,
            }
            .encode(false),
            [0xCB, 0x01, 0x02, 0x03, 0x04]
        );
        assert_eq!(
            CmdProgramVerify {
                mode: ProgramVerifyMode::SendingVerificationValue,
                verification_type: 0x1234,
                verification_value: 0xAABBCCDD,
            }
            .encode(false),
            [0xC8, 0x01, 0x34, 0x12, 0xDD, 0xCC, 0xBB, 0xAA]
        );
        assert_eq!(
            CmdDtoCtrProperties {
                modifier: DtoCtrModifier::Daq,
                event_channel_no: 1,
                related_event_channel_no: 2,
                mode: DtoCtrMode::Daq,
            }
            .encode(false),
            [0xC5, 0x02, 0x01, 0x00, 0x02, 0x00, 0x01]
        );
        assert_eq!(
            CmdTimeCorrelationProperties {
                set_properties: TimeCorrSetProps::SET_CLUSTER_ID,
                get_properties_req: TimeCorrGetPropsReq::GetClkInfo,
                cluster_id: 0x1234,
            }
            .encode(false),
            [0xC6, 0x10, 0x01, 0x00, 0x34, 0x12]
        );
        assert_eq!(
            CmdGetDaqListUsbEndpoint {
                daq_list_no: 0x1234
            }
            .encode(false),
            [0xF2, 0xFF, 0x34, 0x12]
        );
        assert_eq!(
            CmdSetDaqListUsbEndpoint {
                daq_list_no: 0x1234,
                endpoint_no: 3
            }
            .encode(false),
            [0xF2, 0xFE, 0x34, 0x12, 0x03]
        );
    }

    #[test]
    fn cmd_encode_big_endian_swap() {
        assert_eq!(
            CmdSetMta {
                address_extension: 0x55,
                address: 0x11223344
            }
            .encode(true),
            [0xF6, 0x00, 0x00, 0x55, 0x11, 0x22, 0x33, 0x44]
        );
        assert_eq!(
            CmdSetRequest {
                mode: SetRequestMode::STORE_CAL_REQUEST,
                session_id: 0x1234
            }
            .encode(true),
            [0xF9, 0x01, 0x12, 0x34]
        );
        assert_eq!(
            CmdModifyBits {
                shift_value: 1,
                and_mask: 0xFF0F,
                xor_mask: 0x00F0
            }
            .encode(true),
            [0xEC, 0x01, 0xFF, 0x0F, 0x00, 0xF0]
        );
        assert_eq!(
            CmdSetDaqListMode {
                mode: DaqListMode::NONE,
                daq_list_no: 0x1234,
                event_channel_no: 0x5678,
                prescaler: 1,
                priority: 2,
            }
            .encode(true),
            [0xE0, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01, 0x02]
        );
        assert_eq!(
            CmdWriteDaq {
                bit_offset: 0,
                element_size: 4,
                address_extension: 1,
                address: 0xAABBCCDD
            }
            .encode(true),
            [0xE1, 0x00, 0x04, 0x01, 0xAA, 0xBB, 0xCC, 0xDD]
        );
    }

    #[test]
    fn build_cto_headers() {
        assert_eq!(
            build_cto(XcpHeaderLen::NotSet, 0, &[0xFF, 0x00], &[]),
            [0xFF, 0x00]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::BYTE, 0, &[0xF6, 0, 0], &[1, 2]),
            [5, 0xF6, 0, 0, 1, 2]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::CTR_BYTE, 0x1234, &[0xF6], &[]),
            [1, 0x34, 0xF6]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::WORD, 0, &[0xF6, 1], &[2]),
            [3, 0, 0xF6, 1, 2]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::CTR_WORD, 0x0201, &[0xF6], &[]),
            [1, 0, 0x01, 0x02, 0xF6]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::FILL_BYTE, 0, &[0xF6], &[]),
            [1, 0, 0xF6]
        );
        assert_eq!(
            build_cto(XcpHeaderLen::FILL_WORD, 0, &[0xF6], &[]),
            [1, 0, 0, 0, 0xF6]
        );
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[test]
    fn resp_connect_decode_and_display() {
        let data = [0xFF, 0x1D, 0xC4, 0x08, 0x08, 0x00, 0x01, 0x01];
        let r = RespConnect::decode(&data, false).unwrap();
        assert_eq!(r.pid, PidSlaveMaster::Res);
        assert_eq!(r.resource, ResourceType(0x1D));
        assert_eq!(r.comm_mode_basic, CommModeBasic(0xC4));
        assert_eq!(r.max_cto, 8);
        assert_eq!(r.max_dto, 8);
        assert_eq!(r.version, 0x0101);
        assert_eq!((r.version_major(), r.version_minor()), (1, 1));
        assert_eq!(r.address_granularity(), 4);
        assert!(!r.is_big_endian());
        assert_eq!(
            r.to_string(),
            "XCPVersion=1.1 LittleEndian MaxCTO=8 MaxDTO=8 AddressGranularity=DWORD Resources=CAL_PAG,DAQ,PGM,STIM,Slave Block mode supported"
        );
        let r2 = RespConnect::decode(&data, true).unwrap();
        assert_eq!(r2.max_dto, 0x0800);
        assert_eq!(r2.version, 0x0101);
        assert!(RespConnect::decode(&data[..7], false).is_none());
    }

    #[test]
    fn resp_decode_family() {
        let st = RespGetStatus::decode(&[0xFF, 0x40, 0x15, 0x00, 0x34, 0x12], false).unwrap();
        assert_eq!(st.session_state, SessionState(0x40));
        assert!(st.session_state.contains(SessionState::DAQ_RUNNING));
        assert_eq!(st.resource_protection_state, ResourceType(0x15));
        assert_eq!(st.session_configuration_id, 0x1234);
        assert!(!st.is_storing());
        let st2 = RespGetStatus::decode(&[0xFF, 0x05, 0x00, 0x00, 0x00, 0x00], false).unwrap();
        assert!(st2.is_storing());

        let cm = RespGetCommModeInfo::decode(&[0xFF, 0, 0x03, 0, 8, 1, 4, 0x10], false).unwrap();
        assert_eq!(cm.comm_mode, CommModeOptional(0x03));
        assert_eq!(
            (cm.max_bs, cm.min_st, cm.queue_size, cm.driver_version),
            (8, 1, 4, 0x10)
        );

        let id = RespGetId::decode(&[0xFF, 0x01, 0, 0, 0x78, 0x56, 0x34, 0x12], false).unwrap();
        assert_eq!(id.mode, GetIdRespType::TRANSFER_MODE);
        assert_eq!(id.length, 0x12345678);

        let seed = RespGetSeed::decode(&[0xFF, 0x04, 1, 2, 3, 4], false).unwrap();
        assert_eq!(seed.length, 4);
        let (_, rest) = RespGetSeed::decode_with_rest(&[0xFF, 0x04, 1, 2, 3, 4], false).unwrap();
        assert_eq!(rest, [1, 2, 3, 4]);

        let un = RespUnlock::decode(&[0xFF, 0x00], false).unwrap();
        assert_eq!(un.protection_state, ResourceType::NONE);

        let chk =
            RespBuildChecksum::decode(&[0xFF, 0x06, 0, 0, 0x78, 0x56, 0x34, 0x12], false).unwrap();
        assert_eq!(chk.xcp_type, 6);
        assert_eq!(chk.checksum_type(), Some(ChecksumType::ADD_44));
        assert_eq!(chk.checksum, 0x12345678);
        assert_eq!(
            RespBuildChecksum::new(ChecksumType::ADD_44, 1)
                .unwrap()
                .xcp_type,
            6
        );
        assert!(RespBuildChecksum::new(ChecksumType::CRC_8, 1).is_none());

        let page = RespGetCalPage::decode(&[0xFF, 0, 0, 0x01], false).unwrap();
        assert_eq!(page.page_no, 1);

        let pag = RespGetPagProcessorInfo::decode(&[0xFF, 0x02, 0x01], false).unwrap();
        assert_eq!(pag.max_segment, 2);
        assert!(pag
            .properties
            .contains(crate::ifdata_xcp::PagProperties::FREEZE_SUPPORTED));

        let seg = RespGetSegmentInfo::decode(&[0xFF, 2, 0, 3, 0, 0], false).unwrap();
        assert_eq!(
            (seg.max_pages, seg.address_extension, seg.max_mapping),
            (2, 0, 3)
        );

        let sega = RespGetSegmentInfoAddress::decode(&[0xFF, 0, 0, 0x00, 0x10, 0x00, 0x00], false)
            .unwrap();
        assert_eq!(sega.info, 0x1000);

        let pi = RespGetPageInfo::decode(&[0xFF, 0x3F, 0x00], false).unwrap();
        assert_eq!(pi.properties, PageProperties(0x3F));

        let sm = RespGetSegmentMode::decode(&[0xFF, 0, 0x01], false).unwrap();
        assert_eq!(sm.mode, SegmentMode::FREEZE);

        let ss = RespStartStopDaqList::decode(&[0xFF, 0x07], false).unwrap();
        assert_eq!(ss.first_pid, 7);

        let dc =
            RespGetDaqClock::decode(&[0xFF, 0, 0x05, 0x01, 0x78, 0x56, 0x34, 0x12], false).unwrap();
        assert_eq!(dc.timestamp, 0x12345678);
        assert_eq!(
            dc.trigger_initiator(),
            Some(TriggerInitiator::LeapSecondOccured)
        );
        assert_eq!(
            dc.time_of_ts_sampling(),
            Some(TimeOfTsSampling::DuringCmdProcessing)
        );

        let dpi = RespGetDaqProcessorInfo::decode(
            &[0xFF, 0x11, 0x05, 0x00, 0x02, 0x00, 0x01, 0xC0],
            false,
        )
        .unwrap();
        assert!(dpi.properties.contains(DaqProperties::TIMESTAMP_SUPPORTED));
        assert_eq!((dpi.max_daq, dpi.max_event_channel, dpi.min_daq), (5, 2, 1));
        assert_eq!(dpi.daq_key_byte, DaqKeyByte(0xC0));

        let dri =
            RespGetDaqResolutionInfo::decode(&[0xFF, 1, 8, 1, 8, 0x14, 0x64, 0x00], false).unwrap();
        assert_eq!(dri.timestamp_ticks, 100);
        assert_eq!(dri.ts_size(), 4);

        let dlm =
            RespGetDaqListMode::decode(&[0xFF, 0x50, 0, 0, 0x05, 0x00, 0x01, 0x00], false).unwrap();
        assert!(dlm.mode.contains(DaqListMode::TIMESTAMP));
        assert!(dlm.mode.contains(DaqListMode::RUNNING));
        assert_eq!(dlm.event_channel_no, 5);

        let dli = RespGetDaqListInfo::decode(&[0xFF, 0x07, 3, 7, 0x05, 0x00], false).unwrap();
        assert_eq!(
            (dli.max_odt, dli.max_odt_entries, dli.fixed_event),
            (3, 7, 5)
        );

        let dei = RespGetDaqEventInfo::decode(&[0xFF, 0x04, 2, 5, 10, 6, 1], false).unwrap();
        assert_eq!(dei.time_unit, XcpTimestampResolution::_1MS);

        let rd = RespReadDaq::decode(&[0xFF, 0xFF, 2, 0, 0x00, 0x30, 0x00, 0x00], false).unwrap();
        assert_eq!(rd.address, 0x3000);

        let dto = RespDtoCtrResp::decode(&[0xFF, 0x0B, 0x02, 0x00, 0x01], false).unwrap();
        assert_eq!(dto.current_related_event_channel_no, 2);
        assert_eq!(dto.mode, DtoCtrMode::Daq);

        let pgm = RespGetPgmProcessorInfo::decode(&[0xFF, 0x03, 0x0A], false).unwrap();
        assert_eq!(pgm.max_sector, 10);

        let si = RespGetSectorInfoModeAddressOrLen::decode(
            &[0xFF, 1, 2, 3, 0x00, 0x10, 0x00, 0x00],
            false,
        )
        .unwrap();
        assert_eq!(si.sector_info, 0x1000);

        let sn = RespGetSectorInfoModeSectorNameLen::decode(&[0xFF, 0x06], false).unwrap();
        assert_eq!(sn.sector_name_len, 6);

        let ps = RespProgramStart::decode(&[0xFF, 0, 0x41, 0x20, 4, 1, 2], false).unwrap();
        assert_eq!(
            (
                ps.comm_mode,
                ps.max_cto,
                ps.max_bs,
                ps.min_st,
                ps.queue_size
            ),
            (CommModeProgram(0x41), 0x20, 4, 1, 2)
        );

        let ue = RespGetDaqListUsbEndpoint::decode(&[0xFF, 0x01, 0, 0, 0x03], false).unwrap();
        assert_eq!(ue.type_, UsbEndpointType::Fixxed);
        assert_eq!(ue.endpoint_no, 3);

        let tc =
            RespTimeCorrelation::decode(&[0xFF, 0x01, 0x11, 0x01, 0x01, 0x00, 0x34, 0x12], false)
                .unwrap();
        assert_eq!(tc.cluster_id, 0x1234);

        let err = RespError::decode(&[0xFE, 0x22], false).unwrap();
        assert_eq!(err.error_code, CmdResult::ERR_OUT_OF_RANGE);

        let ev = RespEvent::decode(&[0xFD, 0x08], false).unwrap();
        assert_eq!(ev.event_code, EventCodes::TimeSync);

        let srv = RespService::decode(&[0xFC, 0x01], false).unwrap();
        assert_eq!(srv.service_request_code, ServiceRequestCode::Text);
    }

    #[test]
    fn resp_comm_mode_info_display_compatibility_behavior() {
        let both = RespGetCommModeInfo::decode(&[0xFF, 0, 0x03, 0, 8, 1, 4, 0], false).unwrap();
        assert_eq!(
            both.to_string(),
            "MasterBlockMode (MaxBS=8 MinST=1), InterLeavedMode (QueueSize=4)"
        );
        let mbm = RespGetCommModeInfo::decode(&[0xFF, 0, 0x01, 0, 8, 1, 4, 0], false).unwrap();
        assert_eq!(mbm.to_string(), "MasterBlockMode (MaxBS=8 MinST=1)");
        let ilm = RespGetCommModeInfo::decode(&[0xFF, 0, 0x02, 0, 8, 1, 4, 0], false).unwrap();
        assert_eq!(ilm.to_string(), "InterLeavedMode (QueueSize=8)");
        let none = RespGetCommModeInfo::decode(&[0xFF, 0, 0x00, 0, 8, 1, 4, 0], false).unwrap();
        assert_eq!(none.to_string(), "");
    }

    #[test]
    fn resp_get_status_display() {
        let st = RespGetStatus::decode(&[0xFF, 0x41, 0x15, 0x00, 0x34, 0x12], false).unwrap();
        assert_eq!(
            st.to_string(),
            "RespGetStatus: SessionState=65\nResourceProtectionState=21\nSessionID=4660"
        );
    }

    #[test]
    fn event_decode_family() {
        let ev = EventBase::decode(&[0xFD, 0x00, 0x34, 0x12]).unwrap();
        assert_eq!(ev.event_code, EventCodes::ResumeMode);

        let rm = EventResumeMode::decode(&[0xFD, 0x00, 0x34, 0x12], false).unwrap();
        assert_eq!(rm.session_id, 0x1234);

        let rmt =
            EventResumeModeTs::decode(&[0xFD, 0x00, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12], false)
                .unwrap();
        assert_eq!(rmt.base.session_id, 0x1234);
        assert_eq!(rmt.current_timestamp, 0x12345678);

        let sto = EventStimTimeout::decode(&[0xFD, 0x09, 0x01, 0x00, 0x02, 0x00], false).unwrap();
        assert_eq!(sto.mode, StimTimeoutMode::DaqListNo);
        assert_eq!(sto.list_no, 2);

        let ts = EventTimeSync::decode(&[0xFD, 0x08, 0x05, 0x01, 0x78, 0x56, 0x34, 0x12], false)
            .unwrap();
        assert_eq!(ts.timestamp, 0x12345678);
        assert_eq!(
            ts.trigger_initiator(),
            Some(TriggerInitiator::LeapSecondOccured)
        );
        assert!(EventTimeSync::decode(&[0xFD, 0x08, 0x05], false).is_none());
    }

    // ------------------------------------------------------------------
    // XcpFrame
    // ------------------------------------------------------------------

    #[test]
    fn xcp_frame_parse_and_text() {
        let raw = [0x03, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0xC4];
        let f = XcpFrame::new(XcpType::Sxi, "COM1", &raw, false, XcpHeaderLen::CTR_WORD);
        assert_eq!(f.len, 3);
        assert_eq!(f.ctr, 1);
        assert_eq!(f.data(), &[0xFF, 0x1D, 0xC4]);
        assert!(!f.is_daq());
        assert!(!f.is_error());
        assert_eq!(f.address(), "\u{2190} SxI COM1");
        assert_eq!(f.type_str(), "RES");

        let f2 = XcpFrame::new(
            XcpType::Can,
            "123",
            &[0xFE, 0x22],
            false,
            XcpHeaderLen::NotSet,
        );
        assert_eq!(f2.len, 2);
        assert!(f2.is_error());
        assert_eq!(f2.type_str(), "ERR(ERR_OUT_OF_RANGE)");

        let f3 = XcpFrame::new(
            XcpType::Can,
            "456",
            &[0x00, 0x11, 0x22],
            false,
            XcpHeaderLen::NotSet,
        );
        assert!(f3.is_daq());
        assert_eq!(f3.type_str(), "DAQ");

        let f4 = XcpFrame::new(
            XcpType::Can,
            "123",
            &[0xF6, 0, 0, 0, 1, 0, 0, 0],
            true,
            XcpHeaderLen::NotSet,
        );
        assert_eq!(f4.type_str(), "SetMTA");
        assert_eq!(f4.address(), "\u{2192} CAN 123");
        assert_eq!(f4.raw_frame_length(), 64);

        let f5 = XcpFrame::new(
            XcpType::Can,
            "1",
            &[0xFD, 0x08],
            false,
            XcpHeaderLen::NotSet,
        );
        assert_eq!(f5.type_str(), "EV(TimeSync)");
    }

    #[test]
    fn xcp_frame_display_and_csv() {
        let mut f = XcpFrame::new(
            XcpType::Can,
            "123",
            &[0xF5, 0x04],
            true,
            XcpHeaderLen::NotSet,
        );
        f.frame.elapsed = Duration::from_millis(250);
        assert_eq!(
            f.to_string(),
            "0.250 - Len:2, Ctr:0, Upload from \u{2192} CAN 123"
        );
        assert_eq!(
            f.to_csv(),
            "0.250;\"\u{2192} CAN 123\";2;\"-\";\"Upload\";\"F5 04\";\"..\""
        );
        assert_eq!(
            f.to_clipboard(),
            "0.250\t\u{2192} CAN 123\t2\t-\tUpload\tF5 04\t.."
        );

        let mut daq = XcpFrame::new(
            XcpType::Sxi,
            "COM1",
            &[0x02, 0x00, 0x07, 0x00, 0x00, 0x41],
            false,
            XcpHeaderLen::CTR_WORD,
        );
        daq.frame.elapsed = Duration::from_millis(1500);
        assert_eq!(daq.len, 2);
        assert_eq!(daq.ctr, 7);
        assert_eq!(
            daq.to_string(),
            "1.500 - Len:2, Ctr:7, DAQ 00 from \u{2190} SxI COM1"
        );
        assert_eq!(
            daq.to_csv(),
            "1.500;\"\u{2190} SxI COM1\";2;\"7\";\"DAQ\";\"00 41\";\".A\""
        );
    }

    // ------------------------------------------------------------------
    // XcpReceiveBuffer
    // ------------------------------------------------------------------

    fn sxi_config(checksum: ChecksumSxi, framing: bool) -> XcpOnSxi {
        let mut sxi = XcpOnSxi {
            header_len: XcpHeaderLen::CTR_WORD,
            checksum,
            ..XcpOnSxi::default()
        };
        if framing {
            sxi.children
                .push(XcpNode::Framing(crate::ifdata_xcp::XcpFraming {
                    sync: 0x55,
                    esc: 0x99,
                    children: Vec::new(),
                }));
        }
        sxi
    }

    #[test]
    fn receive_buffer_plain_ctr_word() {
        let mut buf = XcpReceiveBuffer::new(XcpHeaderLen::CTR_WORD, XcpAlignment::_8_BIT).unwrap();
        assert!(buf
            .get_frame_from_data(XcpType::Tcp, "s", &[0x02, 0x00])
            .is_none());
        let f = buf
            .get_frame_from_data(
                XcpType::Tcp,
                "s",
                &[0x01, 0x00, 0xFF, 0x1D, 0x02, 0x00, 0x02, 0x00, 0xFE, 0x00],
            )
            .unwrap();
        assert_eq!(f.len, 2);
        assert_eq!(f.ctr, 1);
        assert_eq!(f.data(), &[0xFF, 0x1D]);
        let f2 = buf.get_frame_from_data(XcpType::Tcp, "s", &[]).unwrap();
        assert_eq!(f2.data(), &[0xFE, 0x00]);
        assert_eq!(f2.ctr, 2);
        assert!(XcpReceiveBuffer::new(XcpHeaderLen::NotSet, XcpAlignment::_8_BIT).is_err());
    }

    #[test]
    fn receive_buffer_sxi_framing_with_checksum() {
        let sxi = sxi_config(ChecksumSxi::CHECKSUM_BYTE, true);
        let mut buf = XcpReceiveBuffer::new_sxi(&sxi).unwrap();
        let payload = [0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0x1F];
        let mut stream = vec![0x55];
        for &b in &payload {
            if b == 0x55 || b == 0x99 {
                stream.push(0x99);
            }
            stream.push(b);
        }
        assert!(buf
            .get_frame_from_data(XcpType::Sxi, "COM1", &stream[..3])
            .is_none());
        let f = buf
            .get_frame_from_data(XcpType::Sxi, "COM1", &stream[3..])
            .unwrap();
        assert_eq!(f.data(), &[0xFF, 0x1D]);
        assert_eq!(f.ctr, 1);
        let bad = [0x55, 0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0x20];
        assert!(buf
            .get_frame_from_data(XcpType::Sxi, "COM1", &bad)
            .is_none());
    }

    #[test]
    fn receive_buffer_sxi_escaped_sync_in_payload() {
        let sxi = sxi_config(ChecksumSxi::NO_CHECKSUM, true);
        let mut buf = XcpReceiveBuffer::new_sxi(&sxi).unwrap();
        // Payload contains raw 0x55/0x99 bytes, already escaped by the sender; expected frame data [FF, 55, 99]
        let payload = [0x03, 0x00, 0x05, 0x00, 0xFF, 0x55, 0x99];
        let mut stream = vec![0x55];
        for &b in &payload {
            if b == 0x55 || b == 0x99 {
                stream.push(0x99);
            }
            stream.push(b);
        }
        let f = buf
            .get_frame_from_data(XcpType::Sxi, "COM1", &stream)
            .unwrap();
        assert_eq!(f.data(), &[0xFF, 0x55, 0x99]);
        assert_eq!(f.len, 3);
    }

    #[test]
    fn receive_buffer_word_checksum() {
        let sxi = sxi_config(ChecksumSxi::CHECKSUM_WORD, false);
        let mut buf = XcpReceiveBuffer::new_sxi(&sxi).unwrap();
        // No FRAMING: split directly on the header; ADD_12 = 0x02+0x00+0x01+0x00+0xFF+0x1D = 0x011F
        let payload = [0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0x1F, 0x01];
        let f = buf
            .get_frame_from_data(XcpType::Sxi, "COM1", &payload)
            .unwrap();
        assert_eq!(f.data(), &[0xFF, 0x1D]);
    }

    // ------------------------------------------------------------------
    // SxI device (mock serial port)
    // ------------------------------------------------------------------

    struct MockSerialIo {
        open: bool,
        read_buf: VecDeque<u8>,
        written: Vec<Vec<u8>>,
    }

    impl SxiSerialIo for MockSerialIo {
        fn is_open(&self) -> bool {
            self.open
        }
        fn bytes_to_read(&mut self) -> usize {
            self.read_buf.len()
        }
        fn read(&mut self, buf: &mut [u8]) -> usize {
            let mut n = 0;
            while n < buf.len() {
                match self.read_buf.pop_front() {
                    Some(b) => {
                        buf[n] = b;
                        n += 1;
                    }
                    None => break,
                }
            }
            n
        }
        fn bytes_to_write(&mut self) -> usize {
            0
        }
        fn write(&mut self, data: &[u8]) {
            self.written.push(data.to_vec());
        }
        fn discard_buffers(&mut self) {
            self.read_buf.clear();
        }
    }

    #[test]
    fn sxi_device_framing_roundtrip() {
        let sxi = sxi_config(ChecksumSxi::CHECKSUM_BYTE, true);
        let mut device = SerialPortDevice::new("COM1", sxi, SxiHandshake::None, 100, |_cfg| {
            Some(MockSerialIo {
                open: true,
                read_buf: VecDeque::new(),
                written: Vec::new(),
            })
        })
        .unwrap();
        assert!(!device.is_available());
        assert!(device.open());
        assert!(device.is_available());
        assert_eq!(device.port(), "COM1");
        // sendMsg: payload [F6, 00] → +ADD_11(0xF6) → SYNC + escaping
        assert_eq!(device.send_msg(&[0xF6, 0x00]), 2);
        let io = device.io.as_ref().unwrap();
        assert_eq!(io.written.len(), 1);
        assert_eq!(io.written[0], [0x55, 0xF6, 0x00, 0xF6]);
        // Receive path: inject one frame → poll_once fires the callback → get_frame_from_data assembles the frame
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let r2 = std::sync::Arc::clone(&received);
        device
            .core_mut()
            .set_data_callback(Box::new(move |chunk: &[u8]| {
                r2.lock().unwrap().push(chunk.to_vec())
            }));
        let frame_bytes = [0x55, 0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0x1F];
        device.io.as_mut().unwrap().read_buf.extend(frame_bytes);
        assert!(device.poll_once());
        assert_eq!(received.lock().unwrap().len(), 1);
        let chunk = received.lock().unwrap()[0].clone();
        let f = device
            .get_frame_from_data(XcpType::Sxi, "COM1", &chunk)
            .unwrap();
        assert_eq!(f.data(), &[0xFF, 0x1D]);
        device.reset();
        device.close();
        assert!(!device.is_available());
    }

    #[test]
    fn sxi_serial_config_mapping() {
        let mut sxi = sxi_config(ChecksumSxi::NO_CHECKSUM, false);
        sxi.baudrate = 115200;
        sxi.duplex_mode = Some(crate::ifdata_xcp::XcpAsyncFullDuplexMode {
            parity: crate::ifdata_xcp::ParityType::EVEN,
            stop_bits: crate::ifdata_xcp::StopBitsType::TWO_STOP_BITS,
        });
        let cfg = SxiSerialConfig::new(&sxi, "COM3", 250, SxiHandshake::RequestToSend);
        assert_eq!(cfg.port, "COM3");
        assert_eq!(cfg.baudrate, 115200);
        assert_eq!(cfg.parity, SxiParity::Even);
        assert_eq!(cfg.stop_bits, SxiStopBits::Two);
        assert_eq!(cfg.data_bits, 8);
        assert_eq!(
            (cfg.read_buffer_size, cfg.write_buffer_size),
            (65536, 65536)
        );
        assert_eq!(cfg.read_timeout_ms, 250);
        assert_eq!(cfg.handshake, SxiHandshake::RequestToSend);
    }

    // ------------------------------------------------------------------
    // Mock XCP transport (scripted responses; shared handle for assertions)
    // ------------------------------------------------------------------

    #[derive(Default)]
    struct MockShared {
        sent: Vec<Vec<u8>>,
        script: VecDeque<Vec<Vec<u8>>>,
    }

    struct MockXcpTransport {
        shared: Arc<Mutex<MockShared>>,
        rx: VecDeque<XcpFrame>,
    }

    impl MockXcpTransport {
        fn new() -> Self {
            Self {
                shared: Arc::new(Mutex::new(MockShared::default())),
                rx: VecDeque::new(),
            }
        }

        fn handle(&self) -> Arc<Mutex<MockShared>> {
            Arc::clone(&self.shared)
        }

        /// Pre-registers the response frame sequence for the next send.
        fn expect(&mut self, frames: &[&[u8]]) {
            self.shared
                .lock()
                .unwrap()
                .script
                .push_back(frames.iter().map(|f| f.to_vec()).collect());
        }
    }

    #[async_trait]
    impl XcpTransport for MockXcpTransport {
        fn source(&self) -> &str {
            "MOCK 123"
        }
        fn frame_fmt(&self) -> XcpHeaderLen {
            XcpHeaderLen::NotSet
        }
        fn reset(&mut self) {
            self.rx.clear();
        }
        async fn send_bytes(&mut self, data: &[u8]) -> usize {
            let mut g = self.shared.lock().unwrap();
            g.sent.push(data.to_vec());
            if let Some(frames) = g.script.pop_front() {
                for f in frames {
                    self.rx.push_back(XcpFrame::new(
                        XcpType::Can,
                        "MOCK 123",
                        &f,
                        false,
                        XcpHeaderLen::NotSet,
                    ));
                }
            }
            data.len()
        }
        fn next_frame(&mut self) -> Option<XcpFrame> {
            self.rx.pop_front()
        }
    }

    fn protocol_layer() -> XcpProtocolLayer {
        XcpProtocolLayer {
            timings: [25, 50, 100, 200, 500, 1000, 2000],
            max_cto: 8,
            max_dto: 8,
            ..XcpProtocolLayer::default()
        }
    }

    #[cfg(feature = "blocking")]
    fn master_with(
        transport: MockXcpTransport,
        pl: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
    ) -> BlockingXcpMaster {
        let base = XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            pl,
            "Mock",
            Box::new(transport),
        );
        let mut m = BlockingXcpMaster(XcpMaster::with_base(base, daq));
        m.0.base.base.prevent_default_requests = true;
        m
    }

    fn sent_frames(h: &Arc<Mutex<MockShared>>) -> Vec<Vec<u8>> {
        h.lock().unwrap().sent.clone()
    }

    const CONNECT_RESP: [u8; 8] = [0xFF, 0x1D, 0xC0, 0x08, 0x08, 0x00, 0x01, 0x01];
    const CONNECT_RESP_AG4: [u8; 8] = [0xFF, 0x1D, 0xC4, 0x08, 0x08, 0x00, 0x01, 0x01];
    const STATUS_RESP: [u8; 6] = [0xFF, 0x00, 0x00, 0x00, 0x01, 0x00];

    #[cfg(feature = "blocking")]
    #[test]
    fn exchange_connect_set_mta_upload_flow() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF]]);
        mock.expect(&[&[0xFF, 0x11, 0x22, 0x33, 0x44]]);
        mock.expect(&[&STATUS_RESP]);
        let h = mock.handle();
        let base = XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        );

        let mut m = BlockingXcpMasterBase(base);
        let (res, resp) = m.connect(ConnectMode::Normal);
        assert_eq!(res, CmdResult::OK);
        let resp = resp.unwrap();
        assert_eq!(resp.resource, ResourceType(0x1D));
        assert!(m.0.base.slave_connected);
        assert_eq!(m.max_cto(), 8);
        assert_eq!(m.max_dto(), 8);
        assert_eq!(m.address_granularity(), 1);
        assert_eq!(sent_frames(&h)[0], [0xFF, 0x00]);

        assert_eq!(m.set_mta(0, 0x1000), CmdResult::OK);
        let (res, data) = m.upload(4);
        assert_eq!(res, CmdResult::OK);
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[2], [0xF5, 0x04]);
        assert_eq!(data, [0x11, 0x22, 0x33, 0x44]);

        let (res, st) = m.get_status();
        assert_eq!(res, CmdResult::OK);
        assert_eq!(st.unwrap().session_configuration_id, 1);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn exchange_error_and_timeout() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFE, 0x22]]); // SET_MTA → ERR_OUT_OF_RANGE
        mock.expect(&[]);
        let mut m = BlockingXcpMasterBase(XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        ));
        assert_eq!(m.connect(ConnectMode::Normal).0, CmdResult::OK);

        let res = m.set_mta(0, 0x1000);
        assert_eq!(res, CmdResult::ERR_OUT_OF_RANGE);
        assert_eq!(m.last_error_response(), CmdResult::ERR_OUT_OF_RANGE);
        assert_eq!(m.0.base.errors_received(), 1);
        assert!(m.0.base.slave_connected);

        let (res, _) = m.upload(1);
        assert_eq!(res, CmdResult::ERR_TIMEOUT);
        assert!(!m.0.base.slave_connected);
        assert!(matches!(
            m.0.pop_pending(),
            Some(PendingDispatch::Error(
                CmdResult::ERR_OUT_OF_RANGE,
                CommandCode::SetMta
            ))
        ));
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn exchange_pending_events_and_fire_forget() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFD, 0x05], &[0xFF]]);
        let mut m = BlockingXcpMasterBase(XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        ));
        m.connect(ConnectMode::Normal);
        assert_eq!(
            m.set_request(SetRequestMode::STORE_CAL_REQUEST, 0),
            CmdResult::OK
        );
        match m.0.pop_pending().unwrap() {
            PendingDispatch::Event(f) => assert_eq!(f.data(), &[0xFD, 0x05]),
            _ => panic!("expected event"),
        }
        let (res, _) = m.transport_layer_cmd(0xFA, &[1, 2, 3], 0);
        assert_eq!(res, CmdResult::OK);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn master_connect_default_request_chain() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP_AG4]);
        mock.expect(&[&STATUS_RESP]); // GET_STATUS
        mock.expect(&[&[0xFF, 0, 0x03, 0, 8, 1, 4, 0x10]]); // GET_COMM_MODE_INFO
        mock.expect(&[&[0xFF, 0, 0, 0x01]]); // GET_CAL_PAGE
        mock.expect(&[&[0xFF, 0x02, 0x01]]); // GET_PAG_PROCESSOR_INFO
        mock.expect(&[&[0xFF, 0x03, 0x0A]]); // GET_PGM_PROCESSOR_INFO
        mock.expect(&[&[0xFF, 1, 8, 1, 8, 0x14, 0x64, 0x00]]); // GET_DAQ_RESOLUTION_INFO
        mock.expect(&[&[0xFF, 0x11, 0x05, 0x00, 0x02, 0x00, 0x01, 0xC0]]); // GET_DAQ_PROCESSOR_INFO
        mock.expect(&[&[0xFF]]); // DISCONNECT
        let h = mock.handle();
        let mut pl = protocol_layer();
        pl.optional_cmds = vec![
            "GET_SEED".to_string(),
            "SET_CAL_PAGE".to_string(),
            "GET_PAG_PROCESSOR_INFO".to_string(),
            "GET_PGM_PROCESSOR_INFO".to_string(),
            "GET_DAQ_RESOLUTION_INFO".to_string(),
            "GET_DAQ_PROCESSOR_INFO".to_string(),
        ];
        let mut m = master_with(mock, pl, None);
        m.0.base.base.prevent_default_requests = false;
        let (res, _) = m.connect(ConnectMode::Normal);
        assert_eq!(res, CmdResult::OK);
        assert!(m.0.base.base.connected());
        let c = m.cached();
        assert!(c.connect.is_some());
        assert!(c.status.is_some());
        let cm = c.comm_mode_info.unwrap();
        assert_eq!(cm.max_bs, 8);
        assert_eq!(c.cal_page.unwrap().page_no, 1);
        assert_eq!(c.pag_proc_info.unwrap().max_segment, 2);
        assert_eq!(c.pgm_proc_info.unwrap().max_sector, 10);
        assert!(c.daq_proc_info.is_some());
        assert!(c.daq_resolution_info.is_some());
        assert!(m.can_write());
        assert_eq!(m.version(), "1.1");
        assert_eq!(m.str_address_granularity(), "DWORD");
        assert_eq!(m.byte_order(), ByteOrder::MSB_LAST);
        let codes: Vec<u8> = sent_frames(&h).iter().map(|f| f[0]).collect();
        assert_eq!(codes, vec![0xFF, 0xFD, 0xFB, 0xEA, 0xE9, 0xCE, 0xD9, 0xDA]);

        assert!(m.disconnect(-1));
        assert!(!m.0.base.base.connected());
        assert!(m.cached().connect.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn master_read_sync_flows() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF, 0xAA, 0xBB]]); // SHORT_UPLOAD 2
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[&[0xFF, 1, 2, 3, 4, 5, 6, 7]]); // UPLOAD 7
        mock.expect(&[&[0xFF, 8, 9, 10]]); // UPLOAD 3
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), None);
        m.connect(ConnectMode::Normal);
        m.0.respect_optional_cmds = false;
        let (ok, data) = m.read_sync(2, 0, 0x1000, None);
        assert!(ok);
        assert_eq!(data, [0xAA, 0xBB]);
        let (ok, data) = m.read_sync(10, 0, 0x2000, None);
        assert!(ok);
        assert_eq!(data, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF4, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[2], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(sent[3], [0xF5, 0x07]);
        assert_eq!(sent[4], [0xF5, 0x03]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn master_write_sync_flows() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[&[0xFF]]); // DOWNLOAD 6
        mock.expect(&[&[0xFF]]); // DOWNLOAD 3
                                 // DOWNLOAD_MAX 6(fire-forget)+ DOWNLOAD_NEXT 1 + DOWNLOAD 2)
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[]);
        mock.expect(&[&[0xFF]]); // DOWNLOAD_NEXT 1
        mock.expect(&[&[0xFF]]); // DOWNLOAD 2
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), None);
        m.connect(ConnectMode::Normal);
        assert!(m.write_sync(0, 0x2000, &[1, 2, 3, 4, 5, 6, 7, 8, 9], None));
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(sent[2], [0xF0, 0x06, 1, 2, 3, 4, 5, 6]);
        assert_eq!(sent[3], [0xF0, 0x03, 7, 8, 9]);
        m.0.respect_optional_cmds = false;
        assert!(m.write_sync(0, 0x3000, &[1, 2, 3, 4, 5, 6, 7, 8, 9], None));
        let sent = sent_frames(&h);
        assert_eq!(sent[5], vec![0xEE, 1, 2, 3, 4, 5, 6]);
        assert_eq!(sent[6], vec![0xEF, 0x01, 7]);
        assert_eq!(sent[7], vec![0xF0, 0x02, 8, 9]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn master_short_download_path() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&[0xFF, 0x1D, 0xC4, 0x20, 0x08, 0x00, 0x01, 0x01]]); // CONNECT, MaxCTO=0x20
        mock.expect(&[&[0xFF]]); // SHORT_DOWNLOAD
        let h = mock.handle();
        let mut pl = protocol_layer();
        pl.max_cto = 0x20;
        let mut m = master_with(mock, pl, None);
        m.connect(ConnectMode::Normal);
        assert_eq!(m.0.base.max_cto(), 0x20);
        m.0.respect_optional_cmds = false;
        assert!(m.write_sync(0, 0x1000, &[9, 9], None));
        let sent = sent_frames(&h);
        assert_eq!(
            sent[1],
            [0xED, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 9, 9]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn master_unlock_ecu_flow() {
        struct TestSeedKey;
        impl XcpSeedKeyProvider for TestSeedKey {
            fn compute_key_from_seed(
                &self,
                resource: u8,
                seed: &[u8],
            ) -> std::result::Result<Vec<u8>, i32> {
                assert_eq!(resource, ResourceType::CAL_PAG.bits());
                assert_eq!(seed, [1, 2, 3, 4, 5, 6]);
                Ok(vec![0xAA, 0xBB])
            }
        }
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF, 0x06, 1, 2, 3, 4, 5, 6]]);
        mock.expect(&[&[0xFF, 0x00]]); // UNLOCK
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), None);
        m.set_seed_key_provider(Box::new(TestSeedKey));
        m.connect(ConnectMode::Normal);
        let status = RespGetStatus {
            pid: PidSlaveMaster::Res,
            session_state: SessionState::NONE,
            resource_protection_state: ResourceType::CAL_PAG,
            session_configuration_id: 0,
        };
        assert_eq!(m.unlock_ecu(&status, ResourceType::CAL_PAG), CmdResult::OK);
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF8, 0x00, 0x01]);
        assert_eq!(sent[2], [0xF7, 0x02, 0xAA, 0xBB]);
        let mut mock2 = MockXcpTransport::new();
        mock2.expect(&[&CONNECT_RESP]);
        mock2.expect(&[&[0xFF, 0x01, 0x55]]);
        mock2.expect(&[&[0xFF, 0x01]]);
        let mut m2 = master_with(mock2, protocol_layer(), None);
        m2.set_seed_key_provider(Box::new(TestSeedKey));
        m2.connect(ConnectMode::Normal);
        let st2 = RespGetStatus {
            pid: PidSlaveMaster::Res,
            session_state: SessionState::NONE,
            resource_protection_state: ResourceType::CAL_PAG,
            session_configuration_id: 0,
        };
        struct AnyKey;
        impl XcpSeedKeyProvider for AnyKey {
            fn compute_key_from_seed(
                &self,
                _r: u8,
                _s: &[u8],
            ) -> std::result::Result<Vec<u8>, i32> {
                Ok(vec![1])
            }
        }
        m2.set_seed_key_provider(Box::new(AnyKey));
        assert_eq!(
            m2.unlock_ecu(&st2, ResourceType::CAL_PAG),
            CmdResult::ERR_ACCESS_LOCKED
        );
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    fn daq_config() -> XcpDaq {
        XcpDaq {
            mode: XcpDaqMode::DYNAMIC,
            max_daq: 4,
            max_evt_chn: 1,
            min_daq: 0,
            id_field: XcpIdFieldType::ABSOLUTE,
            children: vec![
                XcpNode::Event(XcpEvent {
                    id: 0,
                    daq_list_type: XcpDaqListType::DAQ,
                    max_daq_list: 1,
                    time_cycle: 10,
                    time_unit: XcpTimestampResolution::_1MS,
                    priority: 0,
                    ..XcpEvent::default()
                }),
                XcpNode::TimestampSupported(XcpTimestampSupported {
                    ticks: 1,
                    size: XcpTimestampSize::DWORD,
                    resolution: XcpTimestampResolution::_1US,
                    is_fixed: false,
                    children: Vec::new(),
                }),
            ],
            ..XcpDaq::default()
        }
    }

    fn daq_measurement(addr: u32, data_type: autors_a2l::model::enums::DataType) -> DaqMeasurement {
        DaqMeasurement::new(
            autors_comm::base::MeasurementInfo {
                name: format!("M_{addr:X}"),
                address: addr,
                data_type,
                ..autors_comm::base::MeasurementInfo::default()
            },
            0,
            None,
        )
    }

    #[test]
    fn daq_build_map_and_fill() {
        let daq = daq_config();
        let map = build_daq_and_evt_map(&daq);
        assert_eq!(map.len(), 1);
        let dae = &map[&0];
        assert!(dae.dynamic);
        assert_eq!(dae.daq_list.daq_no, 0);
        assert_eq!(dae.evt.id, 0);

        let mut dict = DaqDictXcp::default();
        let mut measurements = vec![daq_measurement(
            0x1000,
            autors_a2l::model::enums::DataType::UWord,
        )];
        let leftover = dict.fill(8, 5, 1, true, &map, &mut measurements).unwrap();
        assert_eq!(leftover, 0);
        assert_eq!(dict.lists.len(), 1);
        let list = &dict.lists[0];
        assert_eq!(list.daq_no, 0);
        assert!(!list.base.odts.is_empty());
        assert!(list.base.cache.is_some());
        assert_eq!(dict.get_first_pid(list), 0);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn daq_config_and_start_stop_flow() {
        let daq = daq_config();
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        // start:FREE_DAQ / ALLOC_DAQ / ALLOC_ODT / ALLOC_ODT_ENTRY / SET_DAQ_PTR /
        // WRITE_DAQ / SET_DAQ_LIST_MODE / START_STOP_DAQ_LIST(select) / START_STOP_SYNCH(start)
        for _ in 0..7 {
            mock.expect(&[&[0xFF]]);
        }
        mock.expect(&[&[0xFF, 0x00]]); // START_STOP_DAQ_LIST → firstPID 0
        mock.expect(&[&[0xFF]]); // START_STOP_SYNCH(start)
                                 // stop:START_STOP_DAQ_LIST(select) + START_STOP_SYNCH(stop)
        mock.expect(&[&[0xFF]]);
        mock.expect(&[&[0xFF]]);
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), Some(&daq));
        m.connect(ConnectMode::Normal);
        let mut measurements = vec![daq_measurement(
            0x1000,
            autors_a2l::model::enums::DataType::UWord,
        )];
        m.configure_measurements(&mut measurements).unwrap();
        assert_eq!(m.daqs().lists.len(), 1);
        assert!(m.start_measurements(true));
        assert!(m.is_daq_running());
        let sent = sent_frames(&h);
        let codes: Vec<u8> = sent[1..].iter().map(|f| f[0]).collect();
        assert_eq!(
            codes,
            vec![
                CommandCode::FreeDAQ.as_u8(),
                CommandCode::AllocDAQ.as_u8(),
                CommandCode::AllocODT.as_u8(),
                CommandCode::AllocODTEntry.as_u8(),
                CommandCode::SetDaqPtr.as_u8(),
                CommandCode::WriteDaq.as_u8(),
                CommandCode::SetDaqListMode.as_u8(),
                CommandCode::StartStopDaqList.as_u8(),
                CommandCode::StartStopSynch.as_u8(),
            ]
        );
        assert_eq!(sent[2], [0xD5, 0x00, 0x04, 0x00]);
        assert_eq!(sent[6], [0xE1, 0xFF, 0x02, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[7], [0xE0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]);
        assert!(m.stop_measurements(true));
        assert!(!m.is_daq_running());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn daq_frame_received_values_callback() {
        let daq = daq_config();
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        let mut m = master_with(mock, protocol_layer(), Some(&daq));
        m.connect(ConnectMode::Normal);
        let mut measurements = vec![daq_measurement(
            0x1000,
            autors_a2l::model::enums::DataType::UWord,
        )];
        m.configure_measurements(&mut measurements).unwrap();
        m.0.is_daq_running = true;
        let values = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(i32, Vec<u8>)>::new()));
        let v2 = std::sync::Arc::clone(&values);
        m.0.base
            .base
            .add_values_received_callback(Box::new(move |args| {
                v2.lock()
                    .unwrap()
                    .push((args.daq_list_no, args.data.clone()));
            }));
        let frame = XcpFrame::new(
            XcpType::Can,
            "123",
            &[0x00, 0x01, 0x00, 0x00, 0x00, 0x34, 0x12],
            false,
            XcpHeaderLen::NotSet,
        );
        m.on_daq_frame_received(&frame);
        assert!(values.lock().unwrap().is_empty());
        let frame2 = XcpFrame::new(
            XcpType::Can,
            "123",
            &[0x00, 0x02, 0x00, 0x00, 0x00, 0x56, 0x78],
            false,
            XcpHeaderLen::NotSet,
        );
        m.on_daq_frame_received(&frame2);
        let got = values.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 0);
        assert_eq!(got[0].1, [0x34, 0x12]);
    }

    #[test]
    fn daq_timestamp_decode_wrap() {
        let mut list = DaqListXcp::new(0, XcpEvent::default(), 0);
        let ts = XcpTimestampSupported {
            ticks: 1,
            size: XcpTimestampSize::BYTE,
            resolution: XcpTimestampResolution::_1US,
            is_fixed: false,
            children: Vec::new(),
        };
        assert_eq!(list.decode_timestamp(&[0x10], 0, Some(&ts), false), 0.0);
        let d = list.decode_timestamp(&[0x20], 0, Some(&ts), false);
        assert!((d - 16e-6).abs() < 1e-12);
        let d2 = list.decode_timestamp(&[0x10], 0, Some(&ts), false);
        assert!((d2 - 239e-6).abs() < 1e-12);
        assert!(list.decode_timestamp(&[0x10], 0, None, false).is_nan());
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    struct MockCanDevice {
        core: DeviceCore,
        sent: Vec<(u32, Vec<u8>)>,
        rx: Arc<Mutex<VecDeque<CanFrame>>>,
    }

    impl MockCanDevice {
        fn new() -> Self {
            Self {
                core: DeviceCore::new(),
                sent: Vec::new(),
                rx: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        fn rx_handle(&self) -> Arc<Mutex<VecDeque<CanFrame>>> {
            Arc::clone(&self.rx)
        }
    }

    #[async_trait]
    impl CanDevice for MockCanDevice {
        fn core(&self) -> &DeviceCore {
            &self.core
        }
        fn core_mut(&mut self) -> &mut DeviceCore {
            &mut self.core
        }
        async fn is_available(&mut self) -> autors_can::Result<bool> {
            Ok(true)
        }
        async fn open(&mut self, _config: CanConfiguration) -> autors_can::Result<bool> {
            Ok(true)
        }
        async fn close(&mut self) {}
        async fn send(
            &mut self,
            can_id: u32,
            data: &[u8],
            _frame_type: FrameType,
        ) -> autors_can::Result<usize> {
            self.sent.push((can_id, data.to_vec()));
            let _ = self.poll_once().await;
            Ok(data.len())
        }
        async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
            Ok(self.rx.lock().unwrap().pop_front())
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn can_transport_and_slave_ids() {
        let device = MockCanDevice::new();
        let rx = device.rx_handle();
        let shared: Arc<Mutex<dyn CanDevice + Send>> = Arc::new(Mutex::new(device));
        let xcp_can = XcpOnCan {
            can_id_resp: 0x123,
            can_id_cmd: 0x456,
            ..XcpOnCan::default()
        };
        let mut m = BlockingXcpMaster::new_can(
            ConnectBehaviourType::Manual,
            Arc::clone(&shared),
            &xcp_can,
            protocol_layer(),
            None,
            "Mock/CAN1",
        )
        .unwrap();
        assert_eq!(m.0.base.type_, XcpType::Can);
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            vec![0xFF, b'X', b'C', b'P', 0x56, 0x45, 0x00, 0x00],
            false,
            FrameType::CAN20B,
        ));
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            vec![0xFF, !b'X', !b'C', !b'P', 0x56, 0x45, 0x00, 0x00],
            false,
            FrameType::CAN20B,
        ));
        let (res, pairs) = m.get_slave_ids(&shared, 1).unwrap();
        assert_eq!(res, CmdResult::OK);
        assert_eq!(pairs, vec![(0x4556, 0x123)]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn get_slave_ids_wrong_type() {
        let base = XcpMasterBase::without_transport(
            ConnectBehaviourType::Manual,
            XcpType::Sxi,
            protocol_layer(),
        );
        let mut m = BlockingXcpMaster::with_base(base, None);
        let shared: Arc<Mutex<dyn CanDevice + Send>> = Arc::new(Mutex::new(MockCanDevice::new()));
        let err = m.get_slave_ids(&shared, 0).unwrap_err();
        assert!(err.to_string().contains("XCPonCAN"));
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[cfg(feature = "blocking")]
    #[test]
    fn program_sync_flow() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF, 0, 0x41, 0x20, 4, 1, 2]]); // PROGRAM_START(MaxCTO=0x20)
        mock.expect(&[&[0xFF]]); // PROGRAM_CLEAR
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[&[0xFF]]); // PROGRAM 4
        mock.expect(&[&[0xFF]]); // PROGRAM_RESET
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), None);
        m.connect(ConnectMode::Normal);
        m.0.base.base.slave_connected = true;
        m.0.respect_optional_cmds = false;
        let modes = XcpPrgParams::new(
            ProgramClearMode::AbsoluteAccess,
            ProgramVerifyMode::None,
            0,
            0,
        );
        assert!(m.program_sync(0, 0x1000, &[1, 2, 3, 4], None, Some(&modes)));
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xD2]);
        assert_eq!(sent[2], [0xD1, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00]);
        assert_eq!(sent[3], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[4], [0xD0, 0x04, 1, 2, 3, 4]);
        assert_eq!(sent[5], [0xCF]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn get_checksum_and_misc_commands() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[&[0xFF, 0x06, 0, 0, 0x78, 0x56, 0x34, 0x12]]); // BUILD_CHECKSUM
        mock.expect(&[&[0xFF]]); // SET_CAL_PAGE
        mock.expect(&[&[0xFF, 0, 0, 0x01]]); // GET_CAL_PAGE
        let h = mock.handle();
        let mut m = master_with(mock, protocol_layer(), None);
        m.connect(ConnectMode::Normal);
        assert_eq!(m.get_checksum(0, 0x1000, 0x100), Some(0x12345678));
        let sent = sent_frames(&h);
        assert_eq!(sent[2], [0xF3, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
        assert!(m.set_page(EcuPage::RAM));
        assert_eq!(m.active_page(), EcuPage::RAM);
        assert!(m.set_page(EcuPage::Flash));
        let sent2 = sent_frames(&h);
        assert_eq!(sent2[3], [0xEB, 0x83, 0x00, 0x00]);
        assert_eq!(sent2[4], [0xEA, 0x02, 0x00]);
        assert!(m.copy_page2page(EcuPage::RAM, EcuPage::RAM).is_err());
        let seg = XcpMemorySegment::new(0x1000, 4, vec![1, 2, 3, 4]);
        assert_eq!(seg.get_data_bytes(0x1001, 2), Some(vec![2, 3]));
        assert_eq!(seg.get_data_bytes(0x1003, 2), None);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn shared_registration_and_poll_alive() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&STATUS_RESP]);
        let h = mock.handle();
        let mut pl = protocol_layer();
        pl.timings[0] = 0;
        pl.timings[1] = 0;
        let mut m = master_with(mock, pl, None);
        m.0.base.base.connect_behaviour = ConnectBehaviourType::Automatic;
        let shared = m.into_shared().unwrap();
        assert!(CommKernel::is_registered(&shared));
        BlockingCommKernel::poll_clients();
        BlockingCommKernel::poll_clients();
        let codes: Vec<u8> = sent_frames(&h).iter().map(|f| f[0]).collect();
        assert_eq!(codes, vec![0xFF, 0xFD]);
        assert!(shared.lock().unwrap().base.base.connected());
        assert!(CommKernel::deregister_client(&shared));
        assert!(!CommKernel::is_registered(&shared));
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_exchange_connect_set_mta_upload_loopback() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF]]);
        mock.expect(&[&[0xFF, 0x11, 0x22, 0x33, 0x44]]);
        mock.expect(&[&STATUS_RESP]);
        let h = mock.handle();
        let base = XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        );

        let mut m = base;
        let (res, resp) = m.connect(ConnectMode::Normal).await;
        assert_eq!(res, CmdResult::OK);
        let resp = resp.unwrap();
        assert_eq!(resp.resource, ResourceType(0x1D));
        assert!(m.base.slave_connected);
        assert_eq!(m.max_cto(), 8);
        assert_eq!(m.max_dto(), 8);
        assert_eq!(m.address_granularity(), 1);
        assert_eq!(sent_frames(&h)[0], [0xFF, 0x00]);

        assert_eq!(m.set_mta(0, 0x1000).await, CmdResult::OK);
        let (res, data) = m.upload(4).await;
        assert_eq!(res, CmdResult::OK);
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[2], [0xF5, 0x04]);
        assert_eq!(data, [0x11, 0x22, 0x33, 0x44]);

        let (res, st) = m.get_status().await;
        assert_eq!(res, CmdResult::OK);
        assert_eq!(st.unwrap().session_configuration_id, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_exchange_error_and_timeout() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFE, 0x22]]); // SET_MTA → ERR_OUT_OF_RANGE
        mock.expect(&[]);
        let mut m = XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        );
        assert_eq!(m.connect(ConnectMode::Normal).await.0, CmdResult::OK);

        let res = m.set_mta(0, 0x1000).await;
        assert_eq!(res, CmdResult::ERR_OUT_OF_RANGE);
        assert_eq!(m.last_error_response(), CmdResult::ERR_OUT_OF_RANGE);
        assert_eq!(m.base.errors_received(), 1);
        assert!(m.base.slave_connected);

        let (res, _) = m.upload(1).await;
        assert_eq!(res, CmdResult::ERR_TIMEOUT);
        assert!(!m.base.slave_connected);
        assert!(matches!(
            m.pop_pending(),
            Some(PendingDispatch::Error(
                CmdResult::ERR_OUT_OF_RANGE,
                CommandCode::SetMta
            ))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_master_read_sync_loopback() {
        let mut mock = MockXcpTransport::new();
        mock.expect(&[&CONNECT_RESP]);
        mock.expect(&[&[0xFF, 0xAA, 0xBB]]); // SHORT_UPLOAD 2
        mock.expect(&[&[0xFF]]); // SET_MTA
        mock.expect(&[&[0xFF, 1, 2, 3, 4, 5, 6, 7]]); // UPLOAD 7
        mock.expect(&[&[0xFF, 8, 9, 10]]); // UPLOAD 3
        let h = mock.handle();
        let base = XcpMasterBase::new_can(
            ConnectBehaviourType::Manual,
            &XcpOnCan::default(),
            protocol_layer(),
            "Mock",
            Box::new(mock),
        );
        let mut m = XcpMaster::with_base(base, None);
        m.base.base.prevent_default_requests = true;
        m.connect(ConnectMode::Normal).await;
        m.respect_optional_cmds = false;
        let (ok, data) = m.read_sync(2, 0, 0x1000, None).await;
        assert!(ok);
        assert_eq!(data, [0xAA, 0xBB]);
        let (ok, data) = m.read_sync(10, 0, 0x2000, None).await;
        assert!(ok);
        assert_eq!(data, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let sent = sent_frames(&h);
        assert_eq!(sent[1], [0xF4, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[2], [0xF6, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(sent[3], [0xF5, 0x07]);
        assert_eq!(sent[4], [0xF5, 0x03]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_can_transport_command_loopback() {
        let device = MockCanDevice::new();
        let rx = device.rx_handle();
        let shared: Arc<Mutex<dyn CanDevice + Send>> = Arc::new(Mutex::new(device));
        let xcp_can = XcpOnCan {
            can_id_resp: 0x123,
            can_id_cmd: 0x456,
            ..XcpOnCan::default()
        };
        let mut m = XcpMaster::new_can(
            ConnectBehaviourType::Manual,
            Arc::clone(&shared),
            &xcp_can,
            protocol_layer(),
            None,
            "Mock/CAN1",
        )
        .unwrap();
        m.base.base.prevent_default_requests = true;
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            CONNECT_RESP.to_vec(),
            false,
            FrameType::CAN20B,
        ));
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            STATUS_RESP.to_vec(),
            false,
            FrameType::CAN20B,
        ));

        let (res, resp) = m.connect(ConnectMode::Normal).await;
        assert_eq!(res, CmdResult::OK);
        assert!(resp.is_some());
        assert!(m.base.base.slave_connected);
        assert_eq!(m.base.max_cto(), 8);

        let (res, st) = m.get_status().await;
        assert_eq!(res, CmdResult::OK);
        assert_eq!(st.unwrap().session_configuration_id, 1);
        let (res, _) = m.base.upload(1).await;
        assert_eq!(res, CmdResult::ERR_TIMEOUT);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_get_slave_ids_loopback() {
        let device = MockCanDevice::new();
        let rx = device.rx_handle();
        let shared: Arc<Mutex<dyn CanDevice + Send>> = Arc::new(Mutex::new(device));
        let xcp_can = XcpOnCan {
            can_id_resp: 0x123,
            can_id_cmd: 0x456,
            ..XcpOnCan::default()
        };
        let mut m = XcpMaster::new_can(
            ConnectBehaviourType::Manual,
            Arc::clone(&shared),
            &xcp_can,
            protocol_layer(),
            None,
            "Mock/CAN1",
        )
        .unwrap();
        assert_eq!(m.base.type_, XcpType::Can);
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            vec![0xFF, b'X', b'C', b'P', 0x56, 0x45, 0x00, 0x00],
            false,
            FrameType::CAN20B,
        ));
        rx.lock().unwrap().push_back(CanFrame::new(
            "B",
            0x123,
            vec![0xFF, !b'X', !b'C', !b'P', 0x56, 0x45, 0x00, 0x00],
            false,
            FrameType::CAN20B,
        ));
        let (res, pairs) = m.get_slave_ids(&shared, 1).await.unwrap();
        assert_eq!(res, CmdResult::OK);
        assert_eq!(pairs, vec![(0x4556, 0x123)]);
    }
}
