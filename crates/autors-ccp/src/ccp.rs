//! CAN Calibration Protocol command codecs and master implementation.
//! The module provides typed commands and responses, synchronous state
//! tracking, DAQ configuration, and an asynchronous master over `CanDevice`.
//! Response decoding is exposed through [`CcpResponse`]; malformed command
//! arguments are reported as [`CmdResult::InvalidArgument`].

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::EcuPage;
use autors_a2l::model::module::ModPar;
use autors_can::device::{to_can_id_string, CanDevice};
use autors_can::frame::CanFrame;

use crate::error::Result;
use crate::ifdata_ccp::{get_cycle_time, CcpMemoryPageType, CcpTpBlob, SourceAndRaster};
use autors_comm::base::{
    check_epk, now_elapsed, CommMaster, CommMasterHandle, ConnectBehaviourType, DaqDict, DaqList,
    DaqMeasurement, EpkCheckResult, Frame, ProgressCallback, SeedKeyProvider, SkType,
    StartStopMode, XcpPrgParams, STR_TRIMMER,
};

// ===========================================================================
// Protocol commands and responses
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCode {
    /// CONNECT (0x01).
    Connect,
    /// SET_MTA (0x02).
    SetMTA,
    /// DNLOAD(0x03).
    Dnload,
    /// UPLOAD(0x04).
    Upload,
    /// TEST(0x05).
    Test,
    /// START_STOP(0x06).
    StartStop,
    /// DISCONNECT(0x07).
    Disconnect,
    /// START_STOP_ALL(0x08).
    StartStopAll,
    /// GET_ACTIVE_CAL_PAGE(0x09).
    GetActiveCALPage,
    /// SET_S_STATUS(0x0C).
    SetSStatus,
    /// GET_S_STATUS(0x0D).
    GetSStatus,
    /// BUILD_CHKSUM(0x0E).
    BuildChksum,
    /// SHORT_UP(0x0F).
    ShortUp,
    /// CLEAR_MEMORY(0x10).
    ClearMemory,
    /// SELECT_CAL_PAGE(0x11).
    SelectCALPage,
    /// GET_SEED(0x12).
    GetSeed,
    /// UNLOCK(0x13).
    Unlock,
    /// GET_DAQ_SIZE(0x14).
    GetDAQSize,
    /// SET_DAQ_PTR(0x15).
    SetDAQPtr,
    /// WRITE_DAQ(0x16).
    WriteDAQ,
    /// EXCHANGE_ID(0x17).
    ExchangeID,
    /// PROGRAM(0x18).
    Program,
    /// MOVE(0x19).
    Move,
    /// GET_CCP_VERSION(0x1B).
    GetCCPVersion,
    /// DIAG_SERVICE(0x20).
    DiagService,
    /// ACTION_SERVICE(0x21).
    ActionService,
    /// PROGRAM_6(0x22).
    Program6,
    /// DNLOAD_6(0x23).
    Dnload6,
}

impl CommandCode {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Connect => 0x01,
            Self::SetMTA => 0x02,
            Self::Dnload => 0x03,
            Self::Upload => 0x04,
            Self::Test => 0x05,
            Self::StartStop => 0x06,
            Self::Disconnect => 0x07,
            Self::StartStopAll => 0x08,
            Self::GetActiveCALPage => 0x09,
            Self::SetSStatus => 0x0C,
            Self::GetSStatus => 0x0D,
            Self::BuildChksum => 0x0E,
            Self::ShortUp => 0x0F,
            Self::ClearMemory => 0x10,
            Self::SelectCALPage => 0x11,
            Self::GetSeed => 0x12,
            Self::Unlock => 0x13,
            Self::GetDAQSize => 0x14,
            Self::SetDAQPtr => 0x15,
            Self::WriteDAQ => 0x16,
            Self::ExchangeID => 0x17,
            Self::Program => 0x18,
            Self::Move => 0x19,
            Self::GetCCPVersion => 0x1B,
            Self::DiagService => 0x20,
            Self::ActionService => 0x21,
            Self::Program6 => 0x22,
            Self::Dnload6 => 0x23,
        }
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Connect),
            0x02 => Some(Self::SetMTA),
            0x03 => Some(Self::Dnload),
            0x04 => Some(Self::Upload),
            0x05 => Some(Self::Test),
            0x06 => Some(Self::StartStop),
            0x07 => Some(Self::Disconnect),
            0x08 => Some(Self::StartStopAll),
            0x09 => Some(Self::GetActiveCALPage),
            0x0C => Some(Self::SetSStatus),
            0x0D => Some(Self::GetSStatus),
            0x0E => Some(Self::BuildChksum),
            0x0F => Some(Self::ShortUp),
            0x10 => Some(Self::ClearMemory),
            0x11 => Some(Self::SelectCALPage),
            0x12 => Some(Self::GetSeed),
            0x13 => Some(Self::Unlock),
            0x14 => Some(Self::GetDAQSize),
            0x15 => Some(Self::SetDAQPtr),
            0x16 => Some(Self::WriteDAQ),
            0x17 => Some(Self::ExchangeID),
            0x18 => Some(Self::Program),
            0x19 => Some(Self::Move),
            0x1B => Some(Self::GetCCPVersion),
            0x20 => Some(Self::DiagService),
            0x21 => Some(Self::ActionService),
            0x22 => Some(Self::Program6),
            0x23 => Some(Self::Dnload6),
            _ => None,
        }
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::SetMTA => "SetMTA",
            Self::Dnload => "Dnload",
            Self::Upload => "Upload",
            Self::Test => "Test",
            Self::StartStop => "StartStop",
            Self::Disconnect => "Disconnect",
            Self::StartStopAll => "StartStopAll",
            Self::GetActiveCALPage => "GetActiveCALPage",
            Self::SetSStatus => "SetSStatus",
            Self::GetSStatus => "GetSStatus",
            Self::BuildChksum => "BuildChksum",
            Self::ShortUp => "ShortUp",
            Self::ClearMemory => "ClearMemory",
            Self::SelectCALPage => "SelectCALPage",
            Self::GetSeed => "GetSeed",
            Self::Unlock => "Unlock",
            Self::GetDAQSize => "GetDAQSize",
            Self::SetDAQPtr => "SetDAQPtr",
            Self::WriteDAQ => "WriteDAQ",
            Self::ExchangeID => "ExchangeID",
            Self::Program => "Program",
            Self::Move => "Move",
            Self::GetCCPVersion => "GetCCPVersion",
            Self::DiagService => "DiagService",
            Self::ActionService => "ActionService",
            Self::Program6 => "Program6",
            Self::Dnload6 => "Dnload6",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmdResult {
    OK,
    DAQProcOverload,
    CMDProcBusy,
    DAQActive,
    InternalTimeout,
    KeyRequest,
    SessionStateRequest,
    ColdStartRequest,
    CALDataInitRequest,
    DAQListInitRequest,
    CodeUpdateRequest,
    UnknownCommand,
    CommandSyntax,
    ParOutOfRange,
    AccessDenied,
    Overload,
    AccessLocked,
    ResFuncNotAvailable,
    ProtocolFailure,
    Generic,
    SndCmdFailed,
    Timeout,
    InvalidArgument,
    Other(i32),
}

impl CmdResult {
    pub const fn as_i32(self) -> i32 {
        match self {
            Self::OK => 0,
            Self::DAQProcOverload => 1,
            Self::CMDProcBusy => 0x10,
            Self::DAQActive => 17,
            Self::InternalTimeout => 18,
            Self::KeyRequest => 24,
            Self::SessionStateRequest => 25,
            Self::ColdStartRequest => 26,
            Self::CALDataInitRequest => 27,
            Self::DAQListInitRequest => 28,
            Self::CodeUpdateRequest => 29,
            Self::UnknownCommand => 48,
            Self::CommandSyntax => 49,
            Self::ParOutOfRange => 50,
            Self::AccessDenied => 51,
            Self::Overload => 52,
            Self::AccessLocked => 53,
            Self::ResFuncNotAvailable => 54,
            Self::ProtocolFailure => 253,
            Self::Generic => 254,
            Self::SndCmdFailed => 0x100,
            Self::Timeout => 0x101,
            Self::InvalidArgument => 0x102,
            Self::Other(v) => v,
        }
    }

    pub const fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::OK,
            1 => Self::DAQProcOverload,
            0x10 => Self::CMDProcBusy,
            17 => Self::DAQActive,
            18 => Self::InternalTimeout,
            24 => Self::KeyRequest,
            25 => Self::SessionStateRequest,
            26 => Self::ColdStartRequest,
            27 => Self::CALDataInitRequest,
            28 => Self::DAQListInitRequest,
            29 => Self::CodeUpdateRequest,
            48 => Self::UnknownCommand,
            49 => Self::CommandSyntax,
            50 => Self::ParOutOfRange,
            51 => Self::AccessDenied,
            52 => Self::Overload,
            53 => Self::AccessLocked,
            54 => Self::ResFuncNotAvailable,
            253 => Self::ProtocolFailure,
            254 => Self::Generic,
            0x100 => Self::SndCmdFailed,
            0x101 => Self::Timeout,
            0x102 => Self::InvalidArgument,
            _ => Self::Other(v),
        }
    }

    pub fn cs_name(self) -> String {
        match self {
            Self::OK => "OK".to_string(),
            Self::DAQProcOverload => "DAQProcOverload".to_string(),
            Self::CMDProcBusy => "CMDProcBusy".to_string(),
            Self::DAQActive => "DAQActive".to_string(),
            Self::InternalTimeout => "InternalTimeout".to_string(),
            Self::KeyRequest => "KeyRequest".to_string(),
            Self::SessionStateRequest => "SessionStateRequest".to_string(),
            Self::ColdStartRequest => "ColdStartRequest".to_string(),
            Self::CALDataInitRequest => "CALDataInitRequest".to_string(),
            Self::DAQListInitRequest => "DAQListInitRequest".to_string(),
            Self::CodeUpdateRequest => "CodeUpdateRequest".to_string(),
            Self::UnknownCommand => "UnknownCommand".to_string(),
            Self::CommandSyntax => "CommandSyntax".to_string(),
            Self::ParOutOfRange => "ParOutOfRange".to_string(),
            Self::AccessDenied => "AccessDenied".to_string(),
            Self::Overload => "Overload".to_string(),
            Self::AccessLocked => "AccessLocked".to_string(),
            Self::ResFuncNotAvailable => "ResFuncNotAvailable".to_string(),
            Self::ProtocolFailure => "ProtocolFailure".to_string(),
            Self::Generic => "Generic".to_string(),
            Self::SndCmdFailed => "SndCmdFailed".to_string(),
            Self::Timeout => "Timeout".to_string(),
            Self::InvalidArgument => "InvalidArgument".to_string(),
            Self::Other(v) => v.to_string(),
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::OK => "Successful",
            Self::DAQProcOverload => "DAQ processor overload",
            Self::CMDProcBusy => "command processor busy",
            Self::DAQActive => "DAQ processor busy",
            Self::InternalTimeout => "internal timeout",
            Self::KeyRequest => "key request",
            Self::SessionStateRequest => "session status request",
            Self::ColdStartRequest => "cold start request",
            Self::CALDataInitRequest => "cal. data init. request",
            Self::DAQListInitRequest => "DAQ list init. request",
            Self::CodeUpdateRequest => "code update request",
            Self::UnknownCommand => "unknown command",
            Self::CommandSyntax => "command syntax",
            Self::ParOutOfRange => "parameter(s) out of range",
            Self::AccessDenied => "access denied",
            Self::Overload => "overload",
            Self::AccessLocked => "access locked",
            Self::ResFuncNotAvailable => "resource/function not available",
            Self::ProtocolFailure => "Protocol failure (unexpected response length)",
            Self::Generic => "Unspecified failure",
            Self::SndCmdFailed => "Send command failed",
            Self::Timeout => "Timeout",
            Self::InvalidArgument => "Invalid argument failure",
            Self::Other(_) => "Unspecified failure",
        }
    }
}

impl fmt::Display for CmdResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.cs_name())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DisconnectMode {
    #[default]
    Temporary = 0,
    EndOfSession = 1,
}

impl DisconnectMode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PidSlaveMaster {
    EV,
    RES,
}

impl PidSlaveMaster {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::EV => 0xFE,
            Self::RES => 0xFF,
        }
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0xFE => Some(Self::EV),
            0xFF => Some(Self::RES),
            _ => None,
        }
    }

    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::EV => "EV",
            Self::RES => "RES",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceType(pub u8);

impl ResourceType {
    pub const NONE: Self = Self(0x0);
    pub const CAL: Self = Self(0x1);
    /// DAQ(0x02).
    pub const DAQ: Self = Self(0x2);
    pub const PGM: Self = Self(0x40);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for ResourceType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for ResourceType {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const FLAGS: [(u8, &str); 3] = [
            (ResourceType::CAL.0, "CAL"),
            (ResourceType::DAQ.0, "DAQ"),
            (ResourceType::PGM.0, "PGM"),
        ];
        fmt_flags(f, self.0, &FLAGS)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionState(pub u8);

impl SessionState {
    pub const NONE: Self = Self(0x0);
    pub const CAL: Self = Self(0x1);
    pub const DAQ: Self = Self(0x2);
    pub const RESUME: Self = Self(0x4);
    pub const STORE: Self = Self(0x40);
    pub const RUN: Self = Self(0x80);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for SessionState {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for SessionState {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::Not for SessionState {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const FLAGS: [(u8, &str); 5] = [
            (SessionState::CAL.0, "CAL"),
            (SessionState::DAQ.0, "DAQ"),
            (SessionState::RESUME.0, "RESUME"),
            (SessionState::STORE.0, "STORE"),
            (SessionState::RUN.0, "RUN"),
        ];
        fmt_flags(f, self.0, &FLAGS)
    }
}

fn fmt_flags(f: &mut fmt::Formatter<'_>, v: u8, flags: &[(u8, &str)]) -> fmt::Result {
    if v == 0 {
        return f.write_str("None");
    }
    let mut rest = v;
    let mut parts: Vec<&str> = Vec::new();
    for (bit, name) in flags {
        if *bit != 0 && v & bit == *bit {
            parts.push(name);
            rest &= !*bit;
        }
    }
    if rest != 0 {
        return write!(f, "{v}");
    }
    f.write_str(&parts.join(", "))
}

// ===========================================================================
// ===========================================================================

pub trait CcpCommand {
    fn code(&self) -> CommandCode;

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8>;
}

fn push_u16(out: &mut Vec<u8>, v: u16, be: bool) {
    let bytes = if be { v.to_be_bytes() } else { v.to_le_bytes() };
    out.extend_from_slice(&bytes);
}

fn push_u32(out: &mut Vec<u8>, v: u32, be: bool) {
    let bytes = if be { v.to_be_bytes() } else { v.to_le_bytes() };
    out.extend_from_slice(&bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdBase {
    pub code: CommandCode,
}

impl CmdBase {
    pub fn new(code: CommandCode) -> Self {
        Self { code }
    }
}

impl CcpCommand for CmdBase {
    fn code(&self) -> CommandCode {
        self.code
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code.as_u8(), ctr]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdConnect {
    pub station_address: u16,
}

impl CmdConnect {
    pub fn new(station_address: u16) -> Self {
        Self { station_address }
    }
}

impl CcpCommand for CmdConnect {
    fn code(&self) -> CommandCode {
        CommandCode::Connect
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr];
        push_u16(&mut v, self.station_address, false);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdDisconnect {
    pub mode: DisconnectMode,
    pub station_address: u16,
}

impl CmdDisconnect {
    pub fn new(mode: DisconnectMode, station_address: u16) -> Self {
        Self {
            mode,
            station_address,
        }
    }
}

impl CcpCommand for CmdDisconnect {
    fn code(&self) -> CommandCode {
        CommandCode::Disconnect
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr, self.mode.as_u8(), 0];
        push_u16(&mut v, self.station_address, false);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdDownload {
    pub code: CommandCode,
    pub size: u8,
}

impl CmdDownload {
    pub fn new(code: CommandCode, size: u8) -> Self {
        Self { code, size }
    }
}

impl CcpCommand for CmdDownload {
    fn code(&self) -> CommandCode {
        self.code
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code.as_u8(), ctr, self.size]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetCcpVersion {
    pub desired_main: u8,
    pub desired_release: u8,
}

impl CmdGetCcpVersion {
    pub fn new(desired_main: u8, desired_release: u8) -> Self {
        Self {
            desired_main,
            desired_release,
        }
    }
}

impl CcpCommand for CmdGetCcpVersion {
    fn code(&self) -> CommandCode {
        CommandCode::GetCCPVersion
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![
            self.code().as_u8(),
            ctr,
            self.desired_main,
            self.desired_release,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetDaqSize {
    pub daq_no: u8,
    pub can_id: u32,
}

impl CmdGetDaqSize {
    pub fn new(daq_no: u8, can_id: u32) -> Self {
        Self { daq_no, can_id }
    }
}

impl CcpCommand for CmdGetDaqSize {
    fn code(&self) -> CommandCode {
        CommandCode::GetDAQSize
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr, self.daq_no, 0];
        push_u32(&mut v, self.can_id, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdGetSeed {
    pub resource: ResourceType,
}

impl CmdGetSeed {
    pub fn new(resource: ResourceType) -> Self {
        Self { resource }
    }
}

impl CcpCommand for CmdGetSeed {
    fn code(&self) -> CommandCode {
        CommandCode::GetSeed
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code().as_u8(), ctr, self.resource.0]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdMoveClearBc {
    pub code: CommandCode,
    pub size: u32,
}

impl CmdMoveClearBc {
    pub fn new(code: CommandCode, size: u32) -> Self {
        Self { code, size }
    }
}

impl CcpCommand for CmdMoveClearBc {
    fn code(&self) -> CommandCode {
        self.code
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code.as_u8(), ctr];
        push_u32(&mut v, self.size, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetDaqPtr {
    pub daq_no: u8,
    pub odt_no: u8,
    pub element_no: u8,
}

impl CmdSetDaqPtr {
    pub fn new(daq_no: u8, odt_no: u8, element_no: u8) -> Self {
        Self {
            daq_no,
            odt_no,
            element_no,
        }
    }
}

impl CcpCommand for CmdSetDaqPtr {
    fn code(&self) -> CommandCode {
        CommandCode::SetDAQPtr
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![
            self.code().as_u8(),
            ctr,
            self.daq_no,
            self.odt_no,
            self.element_no,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetMta {
    pub number: u8,
    pub extension: u8,
    pub address: u32,
}

impl CmdSetMta {
    pub fn new(number: u8, extension: u8, address: u32) -> Self {
        Self {
            number,
            extension,
            address,
        }
    }
}

impl CcpCommand for CmdSetMta {
    fn code(&self) -> CommandCode {
        CommandCode::SetMTA
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr, self.number, self.extension];
        push_u32(&mut v, self.address, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSetSStatus {
    pub session_state: SessionState,
}

impl CmdSetSStatus {
    pub fn new(session_state: SessionState) -> Self {
        Self { session_state }
    }
}

impl CcpCommand for CmdSetSStatus {
    fn code(&self) -> CommandCode {
        CommandCode::SetSStatus
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code().as_u8(), ctr, self.session_state.0]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdShortUp {
    pub size: u8,
    pub extension: u8,
    pub address: u32,
}

impl CmdShortUp {
    pub fn new(size: u8, extension: u8, address: u32) -> Self {
        Self {
            size,
            extension,
            address,
        }
    }
}

impl CcpCommand for CmdShortUp {
    fn code(&self) -> CommandCode {
        CommandCode::ShortUp
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr, self.size, self.extension];
        push_u32(&mut v, self.address, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdStartStop {
    pub mode: StartStopMode,
    pub daq_no: u8,
    pub last_odt_no: u8,
    pub evt_chn_no: u8,
    pub prescaler: u16,
}

impl CmdStartStop {
    pub fn new(
        mode: StartStopMode,
        daq_no: u8,
        last_odt_no: u8,
        evt_chn_no: u8,
        prescaler: u16,
    ) -> Self {
        Self {
            mode,
            daq_no,
            last_odt_no,
            evt_chn_no,
            prescaler,
        }
    }
}

impl CcpCommand for CmdStartStop {
    fn code(&self) -> CommandCode {
        CommandCode::StartStop
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![
            self.code().as_u8(),
            ctr,
            self.mode.as_u8(),
            self.daq_no,
            self.last_odt_no,
            self.evt_chn_no,
        ];
        push_u16(&mut v, self.prescaler, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdStartStopAll {
    pub mode: StartStopMode,
}

impl CmdStartStopAll {
    pub fn new(mode: StartStopMode) -> Self {
        Self { mode }
    }
}

impl CcpCommand for CmdStartStopAll {
    fn code(&self) -> CommandCode {
        CommandCode::StartStopAll
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code().as_u8(), ctr, self.mode.as_u8()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdTest {
    pub station_address: u16,
}

impl CmdTest {
    pub fn new(station_address: u16) -> Self {
        Self { station_address }
    }
}

impl CcpCommand for CmdTest {
    fn code(&self) -> CommandCode {
        CommandCode::Test
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr];
        push_u16(&mut v, self.station_address, false);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdUpload {
    pub size: u8,
}

impl CmdUpload {
    pub fn new(size: u8) -> Self {
        Self { size }
    }
}

impl CcpCommand for CmdUpload {
    fn code(&self) -> CommandCode {
        CommandCode::Upload
    }

    fn encode(&self, ctr: u8, _change_endianess: bool) -> Vec<u8> {
        vec![self.code().as_u8(), ctr, self.size]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdWriteDaq {
    pub size: u8,
    pub extension: u8,
    pub address: u32,
}

impl CmdWriteDaq {
    pub fn new(size: u8, extension: u8, address: u32) -> Self {
        Self {
            size,
            extension,
            address,
        }
    }
}

impl CcpCommand for CmdWriteDaq {
    fn code(&self) -> CommandCode {
        CommandCode::WriteDAQ
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code().as_u8(), ctr, self.size, self.extension];
        push_u32(&mut v, self.address, change_endianess);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdActionDiagService {
    pub code: CommandCode,
    pub no: u16,
}

impl CmdActionDiagService {
    /// Creates an ACTION_SERVICE command for the given service number.
    pub fn new(code: CommandCode, no: u16) -> Self {
        Self { code, no }
    }
}

impl CcpCommand for CmdActionDiagService {
    fn code(&self) -> CommandCode {
        self.code
    }

    fn encode(&self, ctr: u8, change_endianess: bool) -> Vec<u8> {
        let mut v = vec![self.code.as_u8(), ctr];
        push_u16(&mut v, self.no, change_endianess);
        v
    }
}

// ===========================================================================
// Response (DTO/CRM) decoding
// ===========================================================================

/// Decoding trait for CCP responses.
/// `decode` reads multi-byte fields little-endian; when the master's
/// `ChangeEndianess` flag is set, `change_endianness()` is called
/// afterwards to byte-swap the multi-byte fields.
pub trait CcpResponse: Sized {
    /// Minimum struct length, including the 3-byte response header.
    const MIN_LEN: usize;

    /// Decodes from `data` (`data.len() >= Self::MIN_LEN` is guaranteed by
    /// the caller; returns `None` defensively).
    fn decode(data: &[u8]) -> Option<Self>;

    /// The base three bytes (PID / result code / CTR).
    fn base(&self) -> &RespBase;

    /// Byte-swaps multi-byte fields for big-endian slaves (default: no-op).
    fn change_endianness(&mut self) {}

    /// Snapshot written to the master's response cache on success (only
    /// Connect/GetActiveCALPage/GetSStatus/ExchangeID/GetCCPVersion are
    /// cached; all others return `None`).
    fn to_cached(&self, _cmd: CommandCode) -> Option<CachedResponse> {
        None
    }
}

/// Little-endian read helper (length already checked).
fn u32_le_at(data: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
}

#[derive(Debug, Clone)]
pub enum CachedResponse {
    Connect(RespBase),
    GetActiveCalPage(RespGetActiveCalPage),
    GetSStatus(RespGetSStatus),
    ExchangeId(RespExchangeId),
    GetCcpVersion(RespGetCcpVersion),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespBase {
    pub pid: u8,
    pub result_code: u8,
    pub ctr: u8,
}

impl RespBase {
    pub fn new(code: PidSlaveMaster, result: CmdResult, ctr: u8) -> Self {
        Self {
            pid: code.as_u8(),
            result_code: result.as_i32() as u8,
            ctr,
        }
    }

    pub fn result(&self) -> CmdResult {
        CmdResult::from_i32(self.result_code as i32)
    }

    fn decode_header(data: &[u8]) -> Self {
        Self {
            pid: data[0],
            result_code: data[1],
            ctr: data[2],
        }
    }
}

impl CcpResponse for RespBase {
    const MIN_LEN: usize = 3;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self::decode_header(data))
    }

    fn base(&self) -> &RespBase {
        self
    }

    fn to_cached(&self, cmd: CommandCode) -> Option<CachedResponse> {
        (cmd == CommandCode::Connect).then(|| CachedResponse::Connect(self.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespGetCcpVersion {
    pub base: RespBase,
    pub main: u8,
    pub release: u8,
}

impl CcpResponse for RespGetCcpVersion {
    const MIN_LEN: usize = 5;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            main: data[3],
            release: data[4],
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn to_cached(&self, cmd: CommandCode) -> Option<CachedResponse> {
        (cmd == CommandCode::GetCCPVersion).then(|| CachedResponse::GetCcpVersion(self.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespExchangeId {
    pub base: RespBase,
    pub length_of_id: u8,
    pub data_type_qualifier: u8,
    pub availability: ResourceType,
    pub protection: ResourceType,
}

impl CcpResponse for RespExchangeId {
    const MIN_LEN: usize = 7;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            length_of_id: data[3],
            data_type_qualifier: data[4],
            availability: ResourceType(data[5]),
            protection: ResourceType(data[6]),
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn to_cached(&self, cmd: CommandCode) -> Option<CachedResponse> {
        (cmd == CommandCode::ExchangeID).then(|| CachedResponse::ExchangeId(self.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespGetSeed {
    pub base: RespBase,
    pub protection_state: u8,
}

impl CcpResponse for RespGetSeed {
    const MIN_LEN: usize = 4;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            protection_state: data[3],
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespUnlock {
    pub base: RespBase,
    pub privilege_state: ResourceType,
}

impl CcpResponse for RespUnlock {
    const MIN_LEN: usize = 4;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            privilege_state: ResourceType(data[3]),
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespDownload {
    pub base: RespBase,
    pub extension: u8,
    pub address: u32,
}

impl CcpResponse for RespDownload {
    const MIN_LEN: usize = 8;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            extension: data[3],
            address: u32_le_at(data, 4),
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn change_endianness(&mut self) {
        self.address = self.address.swap_bytes();
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespGetSStatus {
    pub base: RespBase,
    pub session_state: SessionState,
    pub status_qualifier: u8,
}

impl CcpResponse for RespGetSStatus {
    const MIN_LEN: usize = 5;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            session_state: SessionState(data[3]),
            status_qualifier: data[4],
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn to_cached(&self, cmd: CommandCode) -> Option<CachedResponse> {
        (cmd == CommandCode::GetSStatus).then(|| CachedResponse::GetSStatus(self.clone()))
    }
}

impl fmt::Display for RespGetSStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RespGetSStatus: SessionState={}", self.session_state)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespGetDaqSize {
    pub base: RespBase,
    pub daq_list_size: u8,
    pub first_pid: u8,
}

impl CcpResponse for RespGetDaqSize {
    const MIN_LEN: usize = 5;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            daq_list_size: data[3],
            first_pid: data[4],
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespBuildChksum {
    pub base: RespBase,
    pub size: u8,
    pub checksum: u32,
}

impl CcpResponse for RespBuildChksum {
    const MIN_LEN: usize = 8;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            size: data[3],
            checksum: u32_le_at(data, 4),
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn change_endianness(&mut self) {
        match self.size {
            2 => self.checksum = (self.checksum as u16).swap_bytes() as u32,
            4 => self.checksum = self.checksum.swap_bytes(),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespGetActiveCalPage {
    pub base: RespBase,
    pub extension: u8,
    pub address: u32,
}

impl CcpResponse for RespGetActiveCalPage {
    const MIN_LEN: usize = 8;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            extension: data[3],
            address: u32_le_at(data, 4),
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }

    fn change_endianness(&mut self) {
        self.address = self.address.swap_bytes();
    }

    fn to_cached(&self, cmd: CommandCode) -> Option<CachedResponse> {
        (cmd == CommandCode::GetActiveCALPage)
            .then(|| CachedResponse::GetActiveCalPage(self.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespActionDiagService {
    pub base: RespBase,
    pub length: u8,
    pub data_type_qualifier: u8,
}

impl CcpResponse for RespActionDiagService {
    const MIN_LEN: usize = 5;

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_LEN {
            return None;
        }
        Some(Self {
            base: RespBase::decode_header(data),
            length: data[3],
            data_type_qualifier: data[4],
        })
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Debug, Clone)]
pub struct CcpFrame {
    pub frame: Frame,
    pub bus_id: String,
    pub id: u32,
}

impl CcpFrame {
    pub const RESPONSE_INDEX: usize = 0;

    pub fn new(
        bus_id: impl Into<String>,
        can_id: u32,
        data: Vec<u8>,
        is_master_frame: bool,
    ) -> Self {
        Self {
            frame: Frame::new(data, is_master_frame),
            bus_id: bus_id.into(),
            id: can_id,
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.frame.data
    }

    pub fn len(&self) -> usize {
        self.frame.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame.data.is_empty()
    }

    pub fn address(&self) -> String {
        format!(
            "{} {}",
            self.frame.rw_indicator(),
            to_can_id_string(self.id)
        )
    }

    pub fn is_daq(&self) -> bool {
        !self.frame.is_master_frame && !self.frame.data.is_empty() && self.frame.data[0] < 0xFE
    }

    pub fn is_error(&self) -> bool {
        !self.frame.is_master_frame
            && self.frame.data.len() > 1
            && self.frame.data[0] == PidSlaveMaster::RES.as_u8()
            && self.frame.data[1] != 0
    }

    pub fn ctr(&self) -> String {
        if self.is_daq() {
            return String::new();
        }
        if !self.frame.is_master_frame {
            if self.frame.data.len() <= 2 {
                return String::new();
            }
            return self.frame.data[2].to_string();
        }
        if self.frame.data.len() <= 1 {
            return String::new();
        }
        self.frame.data[1].to_string()
    }

    pub fn type_str(&self) -> String {
        if self.frame.is_master_frame {
            return match CommandCode::from_u8(self.frame.data[0]) {
                Some(code) => code.cs_name().to_string(),
                None => self.frame.data[0].to_string(),
            };
        }
        let pid = self.frame.data[0];
        if (0xFE..=0xFF).contains(&pid) {
            let pid_name =
                PidSlaveMaster::from_u8(pid).map_or(pid.to_string(), |p| p.cs_name().to_string());
            let result = CmdResult::from_i32(self.frame.data.get(1).copied().unwrap_or(0) as i32);
            return format!("{pid_name}({})", result.cs_name());
        }
        "DAQ".to_string()
    }

    /// `{time};"{bus}";"{addr}";{len};{ctr};"{type}";"{data}";"{ascii}"`.
    pub fn to_csv(&self) -> String {
        format!(
            "{};\"{}\";\"{}\";{};{};\"{}\";\"{}\";\"{}\"",
            self.frame.time_str(0.0),
            self.bus_id,
            self.address(),
            self.len(),
            self.ctr(),
            self.type_str(),
            self.frame.data_str(),
            self.frame.data_ascii_str('"')
        )
    }

    pub fn to_clipboard(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.frame.time_str(0.0),
            self.bus_id,
            self.address(),
            self.len(),
            self.ctr(),
            self.type_str(),
            self.frame.data_str(),
            self.frame.data_ascii_str('\t')
        )
    }

    pub fn raw_frame_length(&self) -> usize {
        CanFrame::raw_frame_length_for(self.id, self.len())
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcpEventArgs {
    pub error_code: CmdResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcpErrorArgs {
    pub error_code: CmdResult,
    pub command: CommandCode,
}

pub type CcpEventCallback = Box<dyn FnMut(&CcpEventArgs) + Send>;

pub type CcpErrorCallback = Box<dyn FnMut(&CcpErrorArgs) + Send>;

// ===========================================================================
// ===========================================================================

/// `max_dto = 8`, `max_odt = QP_BLOB.Length`, `max_odt_entries = 7`,
pub struct DaqDictCcp;

impl DaqDictCcp {
    pub fn build(
        source_map: &BTreeMap<u16, SourceAndRaster>,
        measurements: &mut Vec<DaqMeasurement>,
    ) -> Result<DaqDict> {
        let mut dict = DaqDict::default();
        for (key, sr) in source_map {
            let qp = if sr.source.active {
                sr.source.qp_blob.clone()
            } else {
                None
            };
            match qp {
                Some(qp) => {
                    let cycle = get_cycle_time(sr.source.scaling_unit, sr.source.rate);
                    let first_pid = if qp.first_pid != u8::MAX {
                        qp.first_pid
                    } else {
                        0
                    };
                    dict.lists.push(DaqList::new(
                        *key,
                        first_pid,
                        sr.raster.evt_chn_no as u16,
                        8,
                        qp.length as u8,
                        7,
                        1,
                        1,
                        cycle,
                        qp.can_id,
                    ));
                }
                None => dict.lists.push(DaqList::new_inactive(*key)),
            }
        }
        dict.fill_daq_lists(measurements, true)?;
        Ok(dict)
    }
}

// ===========================================================================
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcpConfig {
    pub respect_optional_cmds: bool,
    pub address_translation: bool,
    pub timeout_fact: i32,
    pub connection_test_cycle: i32,
}

impl Default for CcpConfig {
    fn default() -> Self {
        Self {
            respect_optional_cmds: true,
            address_translation: false,
            timeout_fact: 1,
            connection_test_cycle: 1000,
        }
    }
}

fn default_timeout(cmd: Option<CommandCode>, timeout_fact: i32) -> i32 {
    match cmd {
        Some(CommandCode::BuildChksum | CommandCode::ClearMemory | CommandCode::Move) => 30_000,
        Some(CommandCode::ActionService) => 5_000,
        Some(CommandCode::DiagService) => 500,
        Some(CommandCode::Program | CommandCode::Program6) => 100,
        _ => 25 * timeout_fact,
    }
}

// ===========================================================================
// ===========================================================================

pub struct CcpMaster<D: CanDevice> {
    pub base: CommMaster,
    pub ccp_if: CcpTpBlob,
    pub device: D,
    pub config: CcpConfig,
    pub source_and_raster_map: BTreeMap<u16, SourceAndRaster>,
    command_ctr: u8,
    last_response: Option<CcpFrame>,
    last_error: CmdResult,
    cached: HashMap<CommandCode, CachedResponse>,
    is_daq_running: bool,
    daq_config_dirty: bool,
    exchange_id_str: String,
    event_callbacks: Vec<CcpEventCallback>,
    error_callbacks: Vec<CcpErrorCallback>,
}

impl<D: CanDevice + Send> CcpMaster<D> {
    ///   `byte_order != MSB_LAST`(`BitConverter.IsLittleEndian != (bo == MSB_LAST)`).
    pub fn new(connect_behaviour: ConnectBehaviourType, ccp_if: CcpTpBlob, device: D) -> Self {
        let mut base = CommMaster::new(connect_behaviour);
        base.name = ccp_if.to_string();
        base.set_change_endianess(ccp_if.byte_order != ByteOrder::MSB_LAST);
        Self {
            base,
            ccp_if,
            device,
            config: CcpConfig::default(),
            source_and_raster_map: BTreeMap::new(),
            command_ctr: 0,
            last_response: None,
            last_error: CmdResult::OK,
            cached: HashMap::new(),
            is_daq_running: false,
            daq_config_dirty: false,
            exchange_id_str: String::new(),
            event_callbacks: Vec::new(),
            error_callbacks: Vec::new(),
        }
    }

    pub fn set_source_and_raster_map(&mut self, map: BTreeMap<u16, SourceAndRaster>) {
        self.source_and_raster_map = map;
    }

    pub fn add_event_callback(&mut self, cb: CcpEventCallback) {
        self.event_callbacks.push(cb);
    }

    pub fn add_error_callback(&mut self, cb: CcpErrorCallback) {
        self.error_callbacks.push(cb);
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    fn cmd_can_id(&self) -> u32 {
        self.ccp_if.can_id_cmd & 0x9FFF_FFFF
    }

    fn resp_can_id(&self) -> u32 {
        self.ccp_if.can_id_resp & 0x9FFF_FFFF
    }

    fn frame_bus_id(&self, id: u32) -> String {
        if id == self.resp_can_id() {
            self.base.name.clone()
        } else {
            to_can_id_string(id)
        }
    }

    fn classify_frame(&mut self, frame: &autors_can::frame::CanFrame) -> Option<CcpFrame> {
        let ccp = CcpFrame::new(
            self.frame_bus_id(frame.id),
            frame.id,
            frame.data.clone(),
            false,
        );
        self.base.record_frame_received(ccp.raw_frame_length());
        match ccp.data().first().copied() {
            Some(v) if v == PidSlaveMaster::RES.as_u8() => {
                self.last_response = Some(ccp.clone());
                Some(ccp)
            }
            Some(v) if v == PidSlaveMaster::EV.as_u8() => {
                self.on_event_received(ccp);
                None
            }
            _ => {
                self.on_daq_frame_received(&ccp);
                None
            }
        }
    }

    async fn wait_response(&mut self, timeout_ms: i32) -> Option<CcpFrame> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        loop {
            match self.device.receive().await {
                Ok(Some(frame)) => {
                    if let Some(resp) = self.classify_frame(&frame) {
                        return Some(resp);
                    }
                }
                Ok(None) | Err(_) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    autors_runtime::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }

    pub async fn poll(&mut self) -> usize {
        let mut n = 0;
        while let Ok(Some(frame)) = self.device.receive().await {
            let _ = self.classify_frame(&frame);
            n += 1;
        }
        n
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    async fn execute<R: CcpResponse>(
        &mut self,
        mut payload: Vec<u8>,
        use_timeout: i32,
    ) -> (CmdResult, Option<R>, Vec<u8>) {
        if payload.len() < 2 {
            return (CmdResult::InvalidArgument, None, Vec::new());
        }
        payload[1] = self.command_ctr;
        self.command_ctr = self.command_ctr.wrapping_add(1);
        let sent_ctr = payload[1];
        let cmd = CommandCode::from_u8(payload[0]);
        self.last_response = None;
        let sent = match self
            .device
            .send_msg(self.ccp_if.can_id_cmd, &payload, true, 8, 0)
            .await
        {
            Ok(n) => n,
            Err(_) => {
                Box::pin(self.set_connection_state(None)).await;
                return (CmdResult::Timeout, None, Vec::new());
            }
        };
        if sent != payload.len() {
            Box::pin(self.set_connection_state(None)).await;
            return (CmdResult::SndCmdFailed, None, Vec::new());
        }
        self.base.increase_ctr(CanFrame::raw_frame_length_for(
            self.cmd_can_id(),
            payload.len(),
        ));
        let timeout = if use_timeout < 0 {
            default_timeout(cmd, self.config.timeout_fact)
        } else {
            use_timeout
        };
        if use_timeout == 0 {
            return (CmdResult::OK, None, Vec::new());
        }
        let Some(resp) = self.wait_response(timeout).await else {
            Box::pin(self.set_connection_state(None)).await;
            return (CmdResult::Timeout, None, Vec::new());
        };
        if resp.len() < 3 {
            Box::pin(self.set_connection_state(None)).await;
            return (CmdResult::Timeout, None, Vec::new());
        }
        let result = CmdResult::from_i32(resp.data()[1] as i32);
        if result != CmdResult::OK {
            self.on_error_received(result, cmd.unwrap_or(CommandCode::Connect));
            return (result, None, Vec::new());
        }
        if sent_ctr != resp.data()[2] {
            return (CmdResult::ProtocolFailure, None, Vec::new());
        }
        if R::MIN_LEN > resp.len() {
            return (CmdResult::ProtocolFailure, None, Vec::new());
        }
        let Some(mut parsed) = R::decode(resp.data()) else {
            return (CmdResult::ProtocolFailure, None, Vec::new());
        };
        if self.base.change_endianess() {
            parsed.change_endianness();
        }
        let additional = if resp.len() > R::MIN_LEN {
            resp.data()[R::MIN_LEN..].to_vec()
        } else {
            Vec::new()
        };
        self.on_response_received(cmd, &parsed);
        (result, Some(parsed), additional)
    }

    fn on_response_received<R: CcpResponse>(&mut self, cmd: Option<CommandCode>, resp: &R) {
        self.base.set_last_received_time(now_elapsed());
        if let Some(cmd) = cmd {
            if let Some(snapshot) = resp.to_cached(cmd) {
                self.cached.insert(cmd, snapshot);
            }
        }
    }

    fn on_error_received(&mut self, result: CmdResult, code: CommandCode) {
        self.base.inc_errors_received();
        self.last_error = result;
        let args = CcpErrorArgs {
            error_code: result,
            command: code,
        };
        for cb in &mut self.error_callbacks {
            cb(&args);
        }
    }

    fn on_event_received(&mut self, frame: CcpFrame) {
        self.base.inc_events_received();
        let code = CmdResult::from_i32(frame.data().get(1).copied().unwrap_or(0) as i32);
        let args = CcpEventArgs { error_code: code };
        for cb in &mut self.event_callbacks {
            cb(&args);
        }
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    fn set_connection_state_base(&mut self, resp: Option<&RespBase>) {
        self.base.set_last_state_change_now();
        if resp.is_some() {
            self.base.slave_connected = true;
        } else {
            self.base.slave_connected = false;
            self.base.reset_counters_on_disconnect();
        }
    }

    pub async fn set_connection_state(&mut self, resp: Option<RespBase>) {
        let connected = self.base.connected();
        self.set_connection_state_base(resp.as_ref());
        let mut flag = self.base.slave_connected;
        if flag == connected {
            return;
        }
        if flag && !self.base.prevent_default_requests {
            'default_requests: {
                if self.get_ccp_versions(2, 1).await.0 != CmdResult::OK {
                    flag = false;
                    break 'default_requests;
                }
                let (r, resp2) = self.exchange_id().await;
                if r != CmdResult::OK {
                    flag = false;
                    break 'default_requests;
                }
                if let Some(resp2) = &resp2 {
                    if resp2.length_of_id > 0 {
                        if let Some(data) = self
                            .read_sync_impl(
                                resp2.length_of_id as usize,
                                0,
                                u32::MAX,
                                None::<&mut fn(&mut autors_comm::base::ProgressArgs)>,
                            )
                            .await
                        {
                            self.exchange_id_str = String::from_utf8_lossy(&data)
                                .trim_matches('\0')
                                .to_string();
                        }
                    }
                }
                if self.is_allowed_request(CommandCode::GetSStatus)
                    && self.get_status().await.0 != CmdResult::OK
                {
                    flag = false;
                    break 'default_requests;
                }
                if self.is_allowed_request(CommandCode::Unlock) {
                    if let Some(resp2) = &resp2 {
                        if !resp2.protection.is_empty() && self.base.seed_and_key.is_some() {
                            let _ = self.unlock_ecu_multi(resp2.protection).await;
                        }
                    }
                }
                if self.is_allowed_request(CommandCode::SetSStatus) {
                    let _ = self.set_status(SessionState::CAL | SessionState::RUN).await;
                }
                if self.is_allowed_request(CommandCode::GetActiveCALPage) {
                    let _ = self.get_active_cal_page().await;
                }
                self.update_daq_sizes(true, u16::MAX).await;
                self.daq_config_dirty = true;
            }
        }
        let changed = self.base.connected() != flag;
        self.base.set_connected(flag);
        if !self.base.connected() {
            self.cached.clear();
        }
        if changed {
            self.base.raise_connection_state_changed();
        }
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub fn is_allowed_request(&self, cmd: CommandCode) -> bool {
        if !self.config.respect_optional_cmds {
            return true;
        }
        self.ccp_if.optional_cmds.contains(&cmd.as_u8())
    }

    pub async fn connect(&mut self) -> CmdResult {
        let payload = CmdConnect::new(self.ccp_if.station_address).encode(0, false);
        let (result, resp, _) = self.execute::<RespBase>(payload, -1).await;
        self.set_connection_state(resp).await;
        result
    }

    pub async fn internal_connect(&mut self) -> bool {
        self.connect().await == CmdResult::OK
    }

    pub async fn internal_get_status(&mut self) -> bool {
        if self.is_allowed_request(CommandCode::GetSStatus) {
            self.get_status().await.0 == CmdResult::OK
        } else {
            self.get_ccp_versions(2, 1).await.0 == CmdResult::OK
        }
    }

    pub async fn disconnect(&mut self, use_timeout: i32) -> bool {
        let _ = self.stop_measurements(false).await;
        self.base.daqs.clear_data();
        self.disconnect_mode(DisconnectMode::EndOfSession, use_timeout)
            .await
    }

    pub async fn disconnect_mode(&mut self, mode: DisconnectMode, use_timeout: i32) -> bool {
        let mut result = CmdResult::OK;
        if self.base.slave_connected {
            let payload = CmdDisconnect::new(mode, self.ccp_if.station_address).encode(0, false);
            let (r, _, _) = self.execute::<RespBase>(payload, use_timeout).await;
            result = r;
            self.set_connection_state(None).await;
        }
        result == CmdResult::OK
    }

    pub async fn close(mut self) {
        let _ = self.disconnect(0).await;
        self.device.close().await;
    }

    /// Negotiates the CCP protocol version with GET_CCP_VERSION.
    pub async fn get_ccp_versions(
        &mut self,
        desired_main: u8,
        desired_release: u8,
    ) -> (CmdResult, Option<RespGetCcpVersion>) {
        let payload = CmdGetCcpVersion::new(desired_main, desired_release).encode(0, false);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn exchange_id(&mut self) -> (CmdResult, Option<RespExchangeId>) {
        let payload = CmdBase::new(CommandCode::ExchangeID).encode(0, false);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn get_seed(
        &mut self,
        resource: ResourceType,
    ) -> (CmdResult, Option<RespGetSeed>, Vec<u8>) {
        let payload = CmdGetSeed::new(resource).encode(0, false);
        self.execute(payload, -1).await
    }

    pub async fn set_mta(&mut self, mta_no: u8, address_extension: u8, address: u32) -> CmdResult {
        let be = self.base.change_endianess();
        let payload = CmdSetMta::new(mta_no, address_extension, address).encode(0, be);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn move_(&mut self, size: u32) -> CmdResult {
        let be = self.base.change_endianess();
        let payload = CmdMoveClearBc::new(CommandCode::Move, size).encode(0, be);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn clear_memory(&mut self, size: u32) -> CmdResult {
        let be = self.base.change_endianess();
        let payload = CmdMoveClearBc::new(CommandCode::ClearMemory, size).encode(0, be);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn build_chksum(&mut self, block_size: u32) -> (CmdResult, Option<RespBuildChksum>) {
        let be = self.base.change_endianess();
        let payload = CmdMoveClearBc::new(CommandCode::BuildChksum, block_size).encode(0, be);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn unlock(&mut self, key: &[u8]) -> (CmdResult, Option<RespUnlock>) {
        let mut payload = CmdBase::new(CommandCode::Unlock).encode(0, false);
        payload.extend_from_slice(key);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn upload(&mut self, size: u8) -> (CmdResult, Vec<u8>) {
        let payload = CmdUpload::new(size).encode(0, false);
        let (r, _, mut data) = self.execute::<RespBase>(payload, -1).await;
        if r == CmdResult::OK && data.len() > size as usize {
            data.truncate(size as usize);
        }
        (r, data)
    }

    pub async fn short_up(
        &mut self,
        size: u8,
        address_extension: u8,
        address: u32,
    ) -> (CmdResult, Vec<u8>) {
        let be = self.base.change_endianess();
        let payload = CmdShortUp::new(size, address_extension, address).encode(0, be);
        let (r, _, data) = self.execute::<RespBase>(payload, -1).await;
        (r, data)
    }

    pub async fn download(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        if bytes.len() > 5 {
            return (CmdResult::InvalidArgument, None);
        }
        let mut payload = CmdDownload::new(CommandCode::Dnload, bytes.len() as u8).encode(0, false);
        payload.extend_from_slice(bytes);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn program(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        if bytes.len() > 5 {
            return (CmdResult::InvalidArgument, None);
        }
        let mut payload =
            CmdDownload::new(CommandCode::Program, bytes.len() as u8).encode(0, false);
        payload.extend_from_slice(bytes);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    /// [`CmdResult::InvalidArgument`].
    pub async fn download6(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        if bytes.len() != 6 {
            return (CmdResult::InvalidArgument, None);
        }
        let mut payload = CmdBase::new(CommandCode::Dnload6).encode(0, false);
        payload.extend_from_slice(bytes);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn program6(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        if bytes.len() != 6 {
            return (CmdResult::InvalidArgument, None);
        }
        let mut payload = CmdBase::new(CommandCode::Program6).encode(0, false);
        payload.extend_from_slice(bytes);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn get_status(&mut self) -> (CmdResult, Option<RespGetSStatus>) {
        let payload = CmdBase::new(CommandCode::GetSStatus).encode(0, false);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn set_status(&mut self, state: SessionState) -> CmdResult {
        let payload = CmdSetSStatus::new(state).encode(0, false);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn get_daq_size(
        &mut self,
        daq_no: u8,
        can_id: u32,
    ) -> (CmdResult, Option<RespGetDaqSize>) {
        let be = self.base.change_endianess();
        let payload = CmdGetDaqSize::new(daq_no, can_id).encode(0, be);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn action_service(
        &mut self,
        no: u16,
        add_bytes: Option<&[u8]>,
    ) -> (CmdResult, Option<RespActionDiagService>, Vec<u8>) {
        self.action_diag_service(CommandCode::ActionService, no, add_bytes)
            .await
    }

    pub async fn diag_service(
        &mut self,
        no: u16,
        add_bytes: Option<&[u8]>,
    ) -> (CmdResult, Option<RespActionDiagService>, Vec<u8>) {
        self.action_diag_service(CommandCode::DiagService, no, add_bytes)
            .await
    }

    async fn action_diag_service(
        &mut self,
        code: CommandCode,
        no: u16,
        add_bytes: Option<&[u8]>,
    ) -> (CmdResult, Option<RespActionDiagService>, Vec<u8>) {
        let be = self.base.change_endianess();
        let mut payload = CmdActionDiagService::new(code, no).encode(0, be);
        if let Some(b) = add_bytes {
            payload.extend_from_slice(b);
        }
        self.execute(payload, -1).await
    }

    pub async fn set_daq_ptr(&mut self, daq_list_no: u8, odt_no: u8, element_no: u8) -> CmdResult {
        let payload = CmdSetDaqPtr::new(daq_list_no, odt_no, element_no).encode(0, false);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn write_daq(&mut self, size: u8, extension: u8, address: u32) -> CmdResult {
        let be = self.base.change_endianess();
        let payload = CmdWriteDaq::new(size, extension, address).encode(0, be);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn start_stop(
        &mut self,
        mode: StartStopMode,
        daq_no: u8,
        last_odt_no: u8,
        evt_chn_no: u8,
        prescaler: u16,
    ) -> CmdResult {
        let be = self.base.change_endianess();
        let payload =
            CmdStartStop::new(mode, daq_no, last_odt_no, evt_chn_no, prescaler).encode(0, be);
        self.execute::<RespBase>(payload, -1).await.0
    }

    /// [`CmdResult::InvalidArgument`].
    pub async fn start_stop_all(&mut self, mode: StartStopMode) -> CmdResult {
        if mode == StartStopMode::Select {
            return CmdResult::InvalidArgument;
        }
        let payload = CmdStartStopAll::new(mode).encode(0, false);
        self.execute::<RespBase>(payload, -1).await.0
    }

    pub async fn get_active_cal_page(&mut self) -> (CmdResult, Option<RespGetActiveCalPage>) {
        let payload = CmdBase::new(CommandCode::GetActiveCALPage).encode(0, false);
        let (r, resp, _) = self.execute(payload, -1).await;
        (r, resp)
    }

    pub async fn select_cal_page(&mut self) -> CmdResult {
        let payload = CmdBase::new(CommandCode::SelectCALPage).encode(0, false);
        self.execute::<RespBase>(payload, -1).await.0
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub fn connect_response(&self) -> Option<&RespBase> {
        match self.cached.get(&CommandCode::Connect) {
            Some(CachedResponse::Connect(r)) => Some(r),
            _ => None,
        }
    }

    pub fn status_response(&self) -> Option<&RespGetSStatus> {
        match self.cached.get(&CommandCode::GetSStatus) {
            Some(CachedResponse::GetSStatus(r)) => Some(r),
            _ => None,
        }
    }

    pub fn exchange_id_response(&self) -> Option<&RespExchangeId> {
        match self.cached.get(&CommandCode::ExchangeID) {
            Some(CachedResponse::ExchangeId(r)) => Some(r),
            _ => None,
        }
    }

    pub fn exchange_id_str(&self) -> &str {
        &self.exchange_id_str
    }

    pub fn version(&self) -> String {
        match self.cached.get(&CommandCode::GetCCPVersion) {
            Some(CachedResponse::GetCcpVersion(r)) => format!("{}.{}", r.main, r.release),
            _ => String::new(),
        }
    }

    pub fn active_page(&self) -> EcuPage {
        let Some(CachedResponse::GetActiveCalPage(resp)) =
            self.cached.get(&CommandCode::GetActiveCALPage)
        else {
            return EcuPage::Flash;
        };
        match self.ccp_if.find_page(resp.extension, resp.address) {
            Some(p) if p.page_type.contains(CcpMemoryPageType::RAM) => EcuPage::RAM,
            _ => EcuPage::Flash,
        }
    }

    pub fn can_write(&self) -> bool {
        match self.exchange_id_response() {
            Some(r) => {
                !(r.availability & ResourceType::CAL).is_empty()
                    && (r.protection & ResourceType::CAL).is_empty()
            }
            None => false,
        }
    }

    pub fn is_daq_running(&self) -> bool {
        self.is_daq_running
    }

    pub fn last_response(&self) -> Option<&CcpFrame> {
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

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub async fn unlock_ecu(&mut self, resource: ResourceType) -> CmdResult {
        let Some(sk) = self.base.seed_and_key.take() else {
            return CmdResult::InvalidArgument;
        };
        let (result, sk) = self.unlock_ecu_impl(sk, resource).await;
        self.base.seed_and_key = Some(sk);
        result
    }

    /// [`Self::unlock_ecu`]/[`Self::unlock_ecu_multi`]).
    pub async fn unlock_ecu_with(
        &mut self,
        sk: &(dyn SeedKeyProvider + 'static),
        resource: ResourceType,
    ) -> CmdResult {
        self.unlock_ecu_impl(sk, resource).await.0
    }

    async fn unlock_ecu_impl<S: std::borrow::Borrow<dyn SeedKeyProvider>>(
        &mut self,
        sk: S,
        resource: ResourceType,
    ) -> (CmdResult, S) {
        let result = 'body: {
            if !sk.borrow().sk_type().contains(SkType::CCP)
                || !self.is_allowed_request(CommandCode::GetSeed)
            {
                break 'body CmdResult::InvalidArgument;
            }
            if !self.base.slave_connected {
                break 'body CmdResult::Timeout;
            }
            if resource.is_empty() {
                break 'body CmdResult::OK;
            }
            let (r, resp, seed) = self.get_seed(resource).await;
            if r != CmdResult::OK {
                break 'body r;
            }
            let Some(resp) = resp else {
                break 'body CmdResult::Generic;
            };
            if resp.protection_state == 0 {
                break 'body CmdResult::OK;
            }
            let Some(key) = sk.borrow().compute_key_from_seed(&seed) else {
                break 'body CmdResult::InvalidArgument;
            };
            if key.is_empty() {
                break 'body CmdResult::InvalidArgument;
            }
            let n = key.len().min(4);
            let (r, _) = self.unlock(&key[..n]).await;
            if r != CmdResult::OK {
                break 'body r;
            }
            let (_, resp3) = self.exchange_id().await;
            match resp3 {
                None => CmdResult::Generic,
                Some(resp3) => {
                    if !(resp3.protection & resource).is_empty() {
                        break 'body CmdResult::Generic;
                    }
                    CmdResult::OK
                }
            }
        };
        (result, sk)
    }

    pub async fn unlock_ecu_multi(&mut self, resource: ResourceType) -> CmdResult {
        if resource.is_empty() {
            return CmdResult::OK;
        }
        let mut result = CmdResult::OK;
        if resource.contains(ResourceType::DAQ) {
            let r = self.unlock_ecu(ResourceType::DAQ).await;
            if r != CmdResult::OK {
                result = r;
            }
        }
        if resource.contains(ResourceType::CAL) {
            let r = self.unlock_ecu(ResourceType::CAL).await;
            if r != CmdResult::OK {
                result = r;
            }
        }
        if resource.contains(ResourceType::PGM) {
            let r = self.unlock_ecu(ResourceType::PGM).await;
            if r != CmdResult::OK {
                result = r;
            }
            if r != CmdResult::OK {
                result = r;
            }
        }
        result
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub fn map_address(&self, address: u32) -> u32 {
        address
    }

    /// `Self::read_sync_impl`.
    pub async fn read_sync(
        &mut self,
        len: usize,
        address_extension: u8,
        address: u32,
        progress: ProgressCallback<'_>,
    ) -> Option<Vec<u8>> {
        self.read_sync_impl(len, address_extension, address, progress)
            .await
    }

    async fn read_sync_impl<F: FnMut(&mut autors_comm::base::ProgressArgs) + ?Sized>(
        &mut self,
        len: usize,
        address_extension: u8,
        mut address: u32,
        mut progress: Option<&mut F>,
    ) -> Option<Vec<u8>> {
        if !self.base.slave_connected {
            return None;
        }
        address = self.map_address(address);
        if address != u32::MAX {
            let page = self.active_page();
            if self.config.address_translation {
                address = self
                    .ccp_if
                    .compute_address(address_extension, address, page);
            }
        }
        let mut result = CmdResult::OK;
        let mut data: Vec<u8> = Vec::new();
        if address != u32::MAX && len <= 5 && self.is_allowed_request(CommandCode::ShortUp) {
            let (r, d) = self.short_up(len as u8, address_extension, address).await;
            result = r;
            data = d;
        } else {
            if address != u32::MAX {
                result = self.set_mta(0, address_extension, address).await;
            }
            if result == CmdResult::OK {
                let mut num = (len / 1024).max(1);
                let mut num2 = 0usize;
                let mut remaining = len;
                while remaining > 0 {
                    let b = if remaining > 5 { 5u8 } else { remaining as u8 };
                    let (r, chunk) = self.upload(b).await;
                    result = r;
                    if result != CmdResult::OK {
                        break;
                    }
                    data.extend_from_slice(&chunk);
                    remaining -= b as usize;
                    num2 += b as usize;
                    if let Some(cb) = &mut progress {
                        if num2 > num {
                            let mut args = autors_comm::base::ProgressArgs::new(
                                (num2 as f64 * 100.0 / len as f64) as i32,
                            );
                            cb(&mut args);
                            if args.cancel {
                                return None;
                            }
                            num += (len / 1024).max(1);
                        }
                    }
                }
            }
        }
        if let Some(cb) = &mut progress {
            cb(&mut autors_comm::base::ProgressArgs::new(100));
        }
        (result == CmdResult::OK).then_some(data)
    }

    async fn write_impl(
        &mut self,
        address_extension: u8,
        mut address: u32,
        data: &[u8],
        prog: bool,
        mut progress: ProgressCallback<'_>,
    ) -> bool {
        if !self.base.slave_connected {
            return false;
        }
        let page = self.active_page();
        if !prog && page == EcuPage::Flash {
            return false;
        }
        address = self.map_address(address);
        if self.config.address_translation {
            address = self
                .ccp_if
                .compute_address(address_extension, address, page);
        }
        let mut result = CmdResult::OK;
        let ok = 'body: {
            if address == u32::MAX && data.len() <= 5 {
                result = if prog {
                    self.program(data).await.0
                } else {
                    self.download(data).await.0
                };
            } else {
                if address != u32::MAX {
                    result = self.set_mta(0, address_extension, address).await;
                    if result != CmdResult::OK {
                        break 'body false;
                    }
                }
                let num = if self.is_allowed_request(if prog {
                    CommandCode::Program6
                } else {
                    CommandCode::Dnload6
                }) {
                    6
                } else {
                    5
                };
                let mut num2 = (data.len() / 100).max(1);
                let mut num3 = 0usize;
                while num3 < data.len() {
                    let n = num.min(data.len() - num3);
                    let chunk = &data[num3..num3 + n];
                    result = if chunk.len() != 6 {
                        if prog {
                            self.program(chunk).await.0
                        } else {
                            self.download(chunk).await.0
                        }
                    } else if prog {
                        self.program6(chunk).await.0
                    } else {
                        self.download6(chunk).await.0
                    };
                    if result != CmdResult::OK {
                        break;
                    }
                    num3 += chunk.len();
                    if let Some(cb) = &mut progress {
                        if num3 > num2 {
                            let mut args = autors_comm::base::ProgressArgs::new(
                                (num3 as f64 * 100.0 / data.len() as f64) as i32,
                            );
                            cb(&mut args);
                            if args.cancel {
                                break 'body false;
                            }
                            num2 += (data.len() / 1000).max(1);
                        }
                    }
                }
            }
            result == CmdResult::OK
        };
        if let Some(cb) = &mut progress {
            cb(&mut autors_comm::base::ProgressArgs::new(100));
        }
        ok
    }

    pub async fn write_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
    ) -> bool {
        self.write_impl(address_extension, address, data, false, progress)
            .await
    }

    pub async fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
        _modes: Option<&XcpPrgParams>,
    ) -> bool {
        self.write_impl(address_extension, address, data, true, progress)
            .await
    }

    pub async fn copy_page2page(&mut self, src_page: EcuPage, dst_page: EcuPage) -> bool {
        if !self.base.slave_connected || !self.is_allowed_request(CommandCode::Move) {
            return false;
        }
        let flash_types = CcpMemoryPageType(CcpMemoryPageType::ROM | CcpMemoryPageType::FLASH);
        let ram_types = CcpMemoryPageType(CcpMemoryPageType::RAM);
        let pick = |page: EcuPage| {
            self.ccp_if
                .get_pages(if page == EcuPage::Flash {
                    flash_types
                } else {
                    ram_types
                })
                .first()
                .map(|p| (p.address_ext, p.address, p.length))
        };
        let Some((se, sa, sl)) = pick(src_page) else {
            return false;
        };
        let Some((de, da, dl)) = pick(dst_page) else {
            return false;
        };
        if self.set_mta(0, se, sa).await != CmdResult::OK {
            return false;
        }
        if self.set_mta(1, de, da).await != CmdResult::OK {
            return false;
        }
        self.move_(sl.min(dl)).await == CmdResult::OK
    }

    pub async fn set_page(&mut self, page: EcuPage) -> bool {
        if !self.base.slave_connected || !self.is_allowed_request(CommandCode::SelectCALPage) {
            return false;
        }
        if self.active_page() == page {
            return true;
        }
        let Some(CachedResponse::GetActiveCalPage(cached)) =
            self.cached.get(&CommandCode::GetActiveCALPage)
        else {
            return false;
        };
        let (ext, addr) = (cached.extension, cached.address);
        let Some((target, _offset)) = self.ccp_if.find_cal_page(ext, addr, page) else {
            return false;
        };
        let (te, ta) = (target.address_ext, target.address);
        self.cached.remove(&CommandCode::GetActiveCALPage);
        if self.set_mta(0, te, ta).await != CmdResult::OK {
            return false;
        }
        if self.select_cal_page().await != CmdResult::OK {
            return false;
        }
        self.get_active_cal_page().await.0 == CmdResult::OK
    }

    pub async fn get_checksum(
        &mut self,
        address_extension: u8,
        address: u32,
        size: u32,
    ) -> Option<u32> {
        if self.set_mta(0, address_extension, address).await != CmdResult::OK {
            return None;
        }
        let (r, resp) = self.build_chksum(size).await;
        if r != CmdResult::OK {
            return None;
        }
        let resp = resp?;
        Some(match resp.size {
            1 => resp.checksum & 0xFF,
            2 => resp.checksum & 0xFFFF,
            _ => resp.checksum,
        })
    }

    pub async fn epk_check(
        &mut self,
        epk_address: u32,
        expected_epk: &str,
    ) -> (EpkCheckResult, Option<String>) {
        let expected = expected_epk.trim_end_matches(STR_TRIMMER);
        if expected.is_empty() {
            return (EpkCheckResult::NotApplicable, None);
        }
        let data = self.read_sync(expected.len(), 0, epk_address, None).await;
        check_epk(expected_epk, data.as_deref())
    }

    pub async fn epk_check_mod_par(
        &mut self,
        mod_par: Option<&ModPar>,
    ) -> (EpkCheckResult, Option<String>) {
        let (epk, epk_address) = match mod_par {
            Some(mp) => match mp.epk.as_deref() {
                Some(epk) if !epk.is_empty() => (epk, mp.epk_address.unwrap_or(u32::MAX)),
                _ => return (EpkCheckResult::NotApplicable, None),
            },
            None => return (EpkCheckResult::NotApplicable, None),
        };
        let address = self.map_address(epk_address);
        self.epk_check(address, epk).await
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    pub fn configure_measurements(
        &mut self,
        mut measurements: Vec<DaqMeasurement>,
    ) -> Result<usize> {
        self.base.daqs.clear();
        self.daq_config_dirty = true;
        self.base.daqs = DaqDictCcp::build(&self.source_and_raster_map, &mut measurements)?;
        Ok(measurements.len())
    }

    async fn update_daq_sizes(&mut self, apply: bool, daq_no: u16) {
        let keys: Vec<u16> = self.source_and_raster_map.keys().copied().collect();
        for key in keys {
            let Some(qp) = self
                .source_and_raster_map
                .get(&key)
                .and_then(|sr| sr.source.qp_blob.clone())
            else {
                continue;
            };
            if !(daq_no == u16::MAX || daq_no == qp.daq_no) {
                continue;
            }
            let (r, resp) = self.get_daq_size(qp.daq_no as u8, qp.can_id).await;
            if r == CmdResult::OK && apply {
                if let Some(resp) = resp {
                    if let Some(sr) = self.source_and_raster_map.get_mut(&key) {
                        if let Some(qp) = &mut sr.source.qp_blob {
                            qp.length = qp.length.min(resp.daq_list_size as u16);
                            qp.first_pid = resp.first_pid;
                        }
                    }
                }
            }
        }
    }

    async fn write_daq_list(&mut self, list_idx: usize) -> bool {
        let Some(list) = self.base.daqs.lists.get(list_idx) else {
            return false;
        };
        let daq_no = list.daq_no() as u8;
        let odts: Vec<Vec<Arc<autors_comm::base::OdtEntry>>> =
            list.odts.values().map(|o| o.entries.clone()).collect();
        for (b, entries) in odts.iter().enumerate() {
            if !entries.is_empty() {
                for (b2, entry) in entries.iter().enumerate() {
                    if self.set_daq_ptr(daq_no, b as u8, b2 as u8).await != CmdResult::OK {
                        return false;
                    }
                    let ext = entry.measurement.address_extension as u8;
                    if self.write_daq(entry.size, ext, entry.address()).await != CmdResult::OK {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub async fn start_measurements(&mut self, do_synchronized: bool) -> bool {
        {
            if !self.base.connected() || self.is_daq_running {
                return false;
            }
            self.base.daqs.clear_data();
            if self.base.daqs.lists.is_empty() {
                return true;
            }
        }
        self.base.daq_clock.reset();
        if self.daq_config_dirty {
            let mut response: Option<RespGetSStatus> = None;
            if self.is_allowed_request(CommandCode::GetSStatus)
                && self.is_allowed_request(CommandCode::SetSStatus)
            {
                let (r, resp) = self.get_status().await;
                if r == CmdResult::OK {
                    response = resp;
                }
                if let Some(resp) = &response {
                    let _ = self
                        .set_status(resp.session_state & !SessionState::DAQ)
                        .await;
                }
            }
            for idx in 0..self.base.daqs.lists.len() {
                let daq_no = self.base.daqs.lists[idx].daq_no();
                self.update_daq_sizes(false, daq_no).await;
                if !self.write_daq_list(idx).await {
                    return false;
                }
            }
            if let Some(resp) = &response {
                let _ = self
                    .set_status(resp.session_state | SessionState::DAQ)
                    .await;
            }
            self.daq_config_dirty = false;
        }
        let do_sync = do_synchronized && self.is_allowed_request(CommandCode::StartStopAll);
        let mut flag = false;
        let daq_nos: Vec<u16> = self
            .base
            .daqs
            .lists
            .iter()
            .filter(|l| !l.odts.is_empty())
            .map(|l| l.daq_no())
            .collect();
        for daq_no in daq_nos {
            let Some(evt) = self
                .source_and_raster_map
                .get(&daq_no)
                .map(|sr| sr.raster.evt_chn_no)
            else {
                return false;
            };
            flag = true;
            let last_odt = self
                .base
                .daqs
                .lists
                .iter()
                .find(|l| l.daq_no() == daq_no)
                .map_or(0, |l| l.odts.len().saturating_sub(1) as u8);
            let mode = if do_sync {
                StartStopMode::Select
            } else {
                StartStopMode::Start
            };
            if self.start_stop(mode, daq_no as u8, last_odt, evt, 1).await != CmdResult::OK {
                return false;
            }
        }
        if do_sync && flag && self.start_stop_all(StartStopMode::Start).await != CmdResult::OK {
            return false;
        }
        self.is_daq_running = flag;
        true
    }

    pub async fn stop_measurements(&mut self, _do_synchronized: bool) -> bool {
        let mut result = true;
        if self.is_daq_running {
            if self.is_allowed_request(CommandCode::StartStopAll) {
                result = self.start_stop_all(StartStopMode::Stop).await == CmdResult::OK;
            } else {
                let daq_nos: Vec<u16> = self
                    .base
                    .daqs
                    .lists
                    .iter()
                    .filter(|l| !l.odts.is_empty())
                    .map(|l| l.daq_no())
                    .collect();
                for daq_no in daq_nos {
                    let Some(evt) = self
                        .source_and_raster_map
                        .get(&daq_no)
                        .map(|sr| sr.raster.evt_chn_no)
                    else {
                        result = false;
                        break;
                    };
                    let last_odt = self
                        .base
                        .daqs
                        .lists
                        .iter()
                        .find(|l| l.daq_no() == daq_no)
                        .map_or(0, |l| l.odts.len().saturating_sub(1) as u8);
                    if self
                        .start_stop(StartStopMode::Stop, daq_no as u8, last_odt, evt, 1)
                        .await
                        != CmdResult::OK
                    {
                        result = false;
                        break;
                    }
                }
            }
        }
        self.is_daq_running = false;
        result
    }

    pub fn on_daq_frame_received(&mut self, frame: &CcpFrame) {
        if !self.base.connected() || !self.is_daq_running {
            return;
        }
        let Some(list_idx) = self
            .base
            .daqs
            .lists
            .iter()
            .position(|l| l.can_id == frame.id)
        else {
            return;
        };
        if self.base.daqs.lists[list_idx].cache.is_none() {
            return;
        }
        let Some(&pid) = frame.data().first() else {
            return;
        };
        let Some(odt) = self.base.daqs.lists[list_idx].odts.get(&pid) else {
            return;
        };
        let odt_pid = odt.pid;
        let odt_size = odt.size;
        if odt_pid == self.base.daqs.lists[list_idx].first_pid() {
            let ts = self.base.daq_clock.elapsed_daq_seconds();
            self.base.daq_clock.set_last_timestamp(ts);
            self.base.daqs.lists[list_idx].last_timestamp = ts;
            let daq_no = self.base.daqs.lists[list_idx].daq_no();
            if let Some(cache) = &mut self.base.daqs.lists[list_idx].cache {
                if let Some(data) = cache.complete() {
                    self.base.raise_on_values_received(daq_no as i32, ts, &data);
                    self.base.daqs.lists[list_idx]
                        .values
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .add(ts, data);
                }
            }
        }
        if !self.base.daqs.lists[list_idx].last_timestamp.is_nan() {
            let num = odt_size as usize + 1;
            let len = frame.data().len();
            if len >= num {
                let trailing = len - num;
                if let Some(cache) = &mut self.base.daqs.lists[list_idx].cache {
                    cache.insert(frame.data().to_vec(), 1, trailing);
                }
            }
        }
    }
}

#[async_trait]
impl<D: CanDevice + Send> CommMasterHandle for CcpMaster<D> {
    async fn poll_alive(&mut self) {
        if self.base.connect_behaviour != ConnectBehaviourType::Automatic {
            return;
        }
        let elapsed = now_elapsed().saturating_sub(self.base.last_received_time());
        if (elapsed.as_millis() as i64) < self.config.connection_test_cycle as i64 {
            return;
        }
        if !self.base.slave_connected {
            let _ = self.internal_connect().await;
        } else {
            let _ = self.internal_get_status().await;
        }
    }

    fn name(&self) -> &str {
        &self.base.name
    }
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::model::enums::DataType;
    use autors_can::device::DeviceCore;
    use autors_can::frame::{CanConfiguration, FrameType};
    use autors_comm::base::{CommKernel, MeasurementInfo};
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingCcpMaster;
    #[cfg(feature = "blocking")]
    use autors_comm::blocking::BlockingCommKernel;

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    type Responder = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;

    struct MockDevice {
        core: DeviceCore,
        opened: bool,
        sent: Vec<(u32, Vec<u8>)>,
        rx: VecDeque<CanFrame>,
        respond: Responder,
    }

    impl MockDevice {
        fn new(respond: Responder) -> Self {
            Self {
                core: DeviceCore::new(),
                opened: true,
                sent: Vec::new(),
                rx: VecDeque::new(),
                respond,
            }
        }

        fn push_rx(&mut self, id: u32, data: &[u8]) {
            self.rx.push_back(CanFrame::new(
                "Mock/CAN1",
                id,
                data.to_vec(),
                false,
                FrameType::CAN20B,
            ));
        }
    }

    #[async_trait]
    impl CanDevice for MockDevice {
        fn core(&self) -> &DeviceCore {
            &self.core
        }
        fn core_mut(&mut self) -> &mut DeviceCore {
            &mut self.core
        }
        async fn is_available(&mut self) -> autors_can::error::Result<bool> {
            Ok(true)
        }
        async fn open(&mut self, _config: CanConfiguration) -> autors_can::error::Result<bool> {
            self.opened = true;
            Ok(true)
        }
        async fn close(&mut self) {
            self.opened = false;
        }
        async fn send(
            &mut self,
            can_id: u32,
            data: &[u8],
            _frame_type: FrameType,
        ) -> autors_can::error::Result<usize> {
            self.sent.push((can_id, data.to_vec()));
            for resp in (self.respond)(data) {
                self.rx.push_back(CanFrame::new(
                    "Mock/CAN1",
                    0x202,
                    resp,
                    false,
                    FrameType::CAN20B,
                ));
            }
            Ok(data.len())
        }
        async fn receive(&mut self) -> autors_can::error::Result<Option<CanFrame>> {
            Ok(self.rx.pop_front())
        }
    }

    fn tp_blob() -> CcpTpBlob {
        CcpTpBlob {
            station_address: 0x0030,
            can_id_cmd: 0x201,
            can_id_resp: 0x202,
            byte_order: ByteOrder::MSB_LAST, // → change_endianess = false
            ..CcpTpBlob::default()
        }
    }

    fn master_with(respond: Responder) -> CcpMaster<MockDevice> {
        let mut m = CcpMaster::new(
            ConnectBehaviourType::Manual,
            tp_blob(),
            MockDevice::new(respond),
        );
        m.config.respect_optional_cmds = false;
        m
    }

    #[cfg(feature = "blocking")]
    fn blocking_master_with(respond: Responder) -> BlockingCcpMaster<MockDevice> {
        BlockingCcpMaster::new(master_with(respond))
    }

    fn basic_ecu() -> Responder {
        Box::new(|data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => vec![0xFF, 0, ctr, 0, 0, 0x03, 0x00],
                0x0D => vec![0xFF, 0, ctr, 0x81, 0x00],
                0x14 => vec![0xFF, 0, ctr, 2, 0],
                0x09 => vec![0xFF, 0, ctr, 0, 0, 0, 0, 0],
                0x03 | 0x18 | 0x22 | 0x23 => vec![0xFF, 0, ctr, 0, 0, 0, 0, 0],
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        })
    }

    fn sent_payloads(m: &CcpMaster<MockDevice>) -> Vec<Vec<u8>> {
        m.device.sent.iter().map(|(_, d)| d.clone()).collect()
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[test]
    fn command_code_values() {
        assert_eq!(CommandCode::Connect.as_u8(), 0x01);
        assert_eq!(CommandCode::SetMTA.as_u8(), 0x02);
        assert_eq!(CommandCode::Dnload.as_u8(), 0x03);
        assert_eq!(CommandCode::Upload.as_u8(), 0x04);
        assert_eq!(CommandCode::Test.as_u8(), 0x05);
        assert_eq!(CommandCode::StartStop.as_u8(), 0x06);
        assert_eq!(CommandCode::Disconnect.as_u8(), 0x07);
        assert_eq!(CommandCode::StartStopAll.as_u8(), 0x08);
        assert_eq!(CommandCode::GetActiveCALPage.as_u8(), 0x09);
        assert_eq!(CommandCode::SetSStatus.as_u8(), 0x0C);
        assert_eq!(CommandCode::GetSStatus.as_u8(), 0x0D);
        assert_eq!(CommandCode::BuildChksum.as_u8(), 0x0E);
        assert_eq!(CommandCode::ShortUp.as_u8(), 0x0F);
        assert_eq!(CommandCode::ClearMemory.as_u8(), 0x10);
        assert_eq!(CommandCode::SelectCALPage.as_u8(), 0x11);
        assert_eq!(CommandCode::GetSeed.as_u8(), 0x12);
        assert_eq!(CommandCode::Unlock.as_u8(), 0x13);
        assert_eq!(CommandCode::GetDAQSize.as_u8(), 0x14);
        assert_eq!(CommandCode::SetDAQPtr.as_u8(), 0x15);
        assert_eq!(CommandCode::WriteDAQ.as_u8(), 0x16);
        assert_eq!(CommandCode::ExchangeID.as_u8(), 0x17);
        assert_eq!(CommandCode::Program.as_u8(), 0x18);
        assert_eq!(CommandCode::Move.as_u8(), 0x19);
        assert_eq!(CommandCode::GetCCPVersion.as_u8(), 0x1B);
        assert_eq!(CommandCode::DiagService.as_u8(), 0x20);
        assert_eq!(CommandCode::ActionService.as_u8(), 0x21);
        assert_eq!(CommandCode::Program6.as_u8(), 0x22);
        assert_eq!(CommandCode::Dnload6.as_u8(), 0x23);
        assert_eq!(CommandCode::from_u8(0x1B), Some(CommandCode::GetCCPVersion));
        assert_eq!(CommandCode::from_u8(0x1A), None);
        assert_eq!(CommandCode::GetCCPVersion.cs_name(), "GetCCPVersion");
    }

    #[test]
    fn cmd_result_values_and_text() {
        assert_eq!(CmdResult::OK.as_i32(), 0);
        assert_eq!(CmdResult::CMDProcBusy.as_i32(), 0x10);
        assert_eq!(CmdResult::UnknownCommand.as_i32(), 48);
        assert_eq!(CmdResult::ProtocolFailure.as_i32(), 253);
        assert_eq!(CmdResult::Generic.as_i32(), 254);
        assert_eq!(CmdResult::SndCmdFailed.as_i32(), 0x100);
        assert_eq!(CmdResult::Timeout.as_i32(), 0x101);
        assert_eq!(CmdResult::InvalidArgument.as_i32(), 0x102);
        assert_eq!(CmdResult::from_i32(48), CmdResult::UnknownCommand);
        assert_eq!(CmdResult::from_i32(77), CmdResult::Other(77));
        assert_eq!(CmdResult::UnknownCommand.cs_name(), "UnknownCommand");
        assert_eq!(CmdResult::Other(77).cs_name(), "77");
        assert_eq!(CmdResult::UnknownCommand.description(), "unknown command");
        assert_eq!(CmdResult::Timeout.description(), "Timeout");
        assert_eq!(CmdResult::ColdStartRequest.to_string(), "ColdStartRequest");
    }

    #[test]
    fn flags_display_matches_expected_contract() {
        assert_eq!(ResourceType::NONE.to_string(), "None");
        assert_eq!(
            (ResourceType::CAL | ResourceType::DAQ).to_string(),
            "CAL, DAQ"
        );
        assert_eq!(
            (ResourceType::CAL | ResourceType::PGM).to_string(),
            "CAL, PGM"
        );
        assert_eq!(ResourceType(0x08).to_string(), "8");
        assert_eq!(SessionState::NONE.to_string(), "None");
        assert_eq!(SessionState(0x83).to_string(), "CAL, DAQ, RUN");
        assert!(ResourceType(0x03).contains(ResourceType::CAL));
        assert!(!ResourceType(0x03).contains(ResourceType::PGM));
        assert_eq!(PidSlaveMaster::EV.as_u8(), 0xFE);
        assert_eq!(PidSlaveMaster::RES.as_u8(), 0xFF);
        assert_eq!(PidSlaveMaster::from_u8(0xFE), Some(PidSlaveMaster::EV));
        assert_eq!(DisconnectMode::EndOfSession.as_u8(), 1);
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[test]
    fn command_encoding_golden() {
        assert_eq!(
            CmdConnect::new(0x3001).encode(0x55, false),
            vec![0x01, 0x55, 0x01, 0x30]
        );
        assert_eq!(
            CmdConnect::new(0x3001).encode(0x55, true),
            vec![0x01, 0x55, 0x01, 0x30]
        );
        assert_eq!(
            CmdDisconnect::new(DisconnectMode::EndOfSession, 0x3001).encode(0, false),
            vec![0x07, 0x00, 0x01, 0x00, 0x01, 0x30]
        );
        assert_eq!(
            CmdDisconnect::new(DisconnectMode::Temporary, 0x3001).encode(0, false),
            vec![0x07, 0x00, 0x00, 0x00, 0x01, 0x30]
        );
        assert_eq!(
            CmdTest::new(0x3001).encode(0, false),
            vec![0x05, 0x00, 0x01, 0x30]
        );
        assert_eq!(
            CmdBase::new(CommandCode::ExchangeID).encode(0, false),
            vec![0x17, 0x00]
        );
        assert_eq!(
            CmdBase::new(CommandCode::GetSStatus).encode(0, false),
            vec![0x0D, 0x00]
        );
        assert_eq!(
            CmdBase::new(CommandCode::SelectCALPage).encode(0, false),
            vec![0x11, 0x00]
        );
        assert_eq!(
            CmdBase::new(CommandCode::GetActiveCALPage).encode(0, false),
            vec![0x09, 0x00]
        );
        // GET_CCP_VERSION
        assert_eq!(
            CmdGetCcpVersion::new(2, 1).encode(0, false),
            vec![0x1B, 0x00, 0x02, 0x01]
        );
        assert_eq!(
            CmdDownload::new(CommandCode::Dnload, 3).encode(0, false),
            vec![0x03, 0x00, 0x03]
        );
        assert_eq!(
            CmdDownload::new(CommandCode::Program, 5).encode(0, false),
            vec![0x18, 0x00, 0x05]
        );
        assert_eq!(
            CmdGetDaqSize::new(3, 0x12345678).encode(0, false),
            vec![0x14, 0x00, 0x03, 0x00, 0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(
            CmdGetDaqSize::new(3, 0x12345678).encode(0, true),
            vec![0x14, 0x00, 0x03, 0x00, 0x12, 0x34, 0x56, 0x78]
        );
        // GET_SEED
        assert_eq!(
            CmdGetSeed::new(ResourceType::CAL | ResourceType::DAQ).encode(0, false),
            vec![0x12, 0x00, 0x03]
        );
        // MOVE/CLEAR_MEMORY/BUILD_CHKSUM
        assert_eq!(
            CmdMoveClearBc::new(CommandCode::Move, 0xAABBCCDD).encode(0, false),
            vec![0x19, 0x00, 0xDD, 0xCC, 0xBB, 0xAA]
        );
        assert_eq!(
            CmdMoveClearBc::new(CommandCode::BuildChksum, 0xAABBCCDD).encode(0, true),
            vec![0x0E, 0x00, 0xAA, 0xBB, 0xCC, 0xDD]
        );
        // SET_DAQ_PTR
        assert_eq!(
            CmdSetDaqPtr::new(1, 2, 3).encode(0, false),
            vec![0x15, 0x00, 0x01, 0x02, 0x03]
        );
        // SET_MTA
        assert_eq!(
            CmdSetMta::new(1, 0x55, 0x12345678).encode(0, false),
            vec![0x02, 0x00, 0x01, 0x55, 0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(
            CmdSetMta::new(0, 0x55, 0x12345678).encode(0, true),
            vec![0x02, 0x00, 0x00, 0x55, 0x12, 0x34, 0x56, 0x78]
        );
        // SET_S_STATUS
        assert_eq!(
            CmdSetSStatus::new(SessionState::CAL | SessionState::RUN).encode(0, false),
            vec![0x0C, 0x00, 0x81]
        );
        // SHORT_UP
        assert_eq!(
            CmdShortUp::new(5, 0, 0x1000).encode(0, false),
            vec![0x0F, 0x00, 0x05, 0x00, 0x00, 0x10, 0x00, 0x00]
        );
        assert_eq!(
            CmdStartStop::new(StartStopMode::Start, 2, 3, 4, 0x0102).encode(0, false),
            vec![0x06, 0x00, 0x01, 0x02, 0x03, 0x04, 0x02, 0x01]
        );
        assert_eq!(
            CmdStartStop::new(StartStopMode::Select, 2, 3, 4, 0x0102).encode(0, true),
            vec![0x06, 0x00, 0x02, 0x02, 0x03, 0x04, 0x01, 0x02]
        );
        // START_STOP_ALL
        assert_eq!(
            CmdStartStopAll::new(StartStopMode::Stop).encode(0, false),
            vec![0x08, 0x00, 0x00]
        );
        // UPLOAD
        assert_eq!(CmdUpload::new(5).encode(0, false), vec![0x04, 0x00, 0x05]);
        // WRITE_DAQ
        assert_eq!(
            CmdWriteDaq::new(4, 0, 0x12345678).encode(0, false),
            vec![0x16, 0x00, 0x04, 0x00, 0x78, 0x56, 0x34, 0x12]
        );
        // ACTION_SERVICE/DIAG_SERVICE
        assert_eq!(
            CmdActionDiagService::new(CommandCode::ActionService, 0x1234).encode(0, false),
            vec![0x21, 0x00, 0x34, 0x12]
        );
        assert_eq!(
            CmdActionDiagService::new(CommandCode::DiagService, 0x1234).encode(0, true),
            vec![0x20, 0x00, 0x12, 0x34]
        );
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[test]
    fn response_decoding() {
        let r = RespBase::decode(&[0xFF, 0x00, 0x07]).unwrap();
        assert_eq!(r.pid, 0xFF);
        assert_eq!(r.result(), CmdResult::OK);
        assert_eq!(r.ctr, 7);
        assert!(RespBase::decode(&[0xFF, 0x00]).is_none());

        let v = RespGetCcpVersion::decode(&[0xFF, 0, 7, 2, 1]).unwrap();
        assert_eq!((v.main, v.release), (2, 1));
        assert!(v.to_cached(CommandCode::GetCCPVersion).is_some());
        assert!(v.to_cached(CommandCode::Connect).is_none());

        let e = RespExchangeId::decode(&[0xFF, 0, 7, 5, 1, 0x03, 0x40]).unwrap();
        assert_eq!(e.length_of_id, 5);
        assert_eq!(e.data_type_qualifier, 1);
        assert_eq!(e.availability, ResourceType(0x03));
        assert_eq!(e.protection, ResourceType::PGM);

        let s = RespGetSeed::decode(&[0xFF, 0, 7, 1]).unwrap();
        assert_eq!(s.protection_state, 1);
        let u = RespUnlock::decode(&[0xFF, 0, 7, 0x03]).unwrap();
        assert_eq!(u.privilege_state, ResourceType(0x03));

        let mut d = RespDownload::decode(&[0xFF, 0, 7, 9, 0x78, 0x56, 0x34, 0x12]).unwrap();
        assert_eq!(d.extension, 9);
        assert_eq!(d.address, 0x12345678);
        d.change_endianness();
        assert_eq!(d.address, 0x78563412);

        let ss = RespGetSStatus::decode(&[0xFF, 0, 9, 0x83, 0x00]).unwrap();
        assert_eq!(ss.session_state, SessionState(0x83));
        assert_eq!(ss.to_string(), "RespGetSStatus: SessionState=CAL, DAQ, RUN");
        assert!(ss.to_cached(CommandCode::GetSStatus).is_some());

        let q = RespGetDaqSize::decode(&[0xFF, 0, 7, 10, 3]).unwrap();
        assert_eq!((q.daq_list_size, q.first_pid), (10, 3));

        let mut c2 = RespBuildChksum::decode(&[0xFF, 0, 7, 2, 0x34, 0x12, 0, 0]).unwrap();
        assert_eq!(c2.checksum, 0x1234);
        c2.change_endianness();
        assert_eq!(c2.checksum, 0x3412);
        let mut c4 = RespBuildChksum::decode(&[0xFF, 0, 7, 4, 0x78, 0x56, 0x34, 0x12]).unwrap();
        c4.change_endianness();
        assert_eq!(c4.checksum, 0x78563412);
        let mut c1 = RespBuildChksum::decode(&[0xFF, 0, 7, 1, 0xAA, 0, 0, 0]).unwrap();
        c1.change_endianness();
        assert_eq!(c1.checksum, 0xAA);

        let mut p = RespGetActiveCalPage::decode(&[0xFF, 0, 7, 1, 0x78, 0x56, 0x34, 0x12]).unwrap();
        assert_eq!(p.address, 0x12345678);
        p.change_endianness();
        assert_eq!(p.address, 0x78563412);
        assert!(p.to_cached(CommandCode::GetActiveCALPage).is_some());

        let a = RespActionDiagService::decode(&[0xFF, 0, 7, 4, 0x55]).unwrap();
        assert_eq!((a.length, a.data_type_qualifier), (4, 0x55));
        assert!(RespGetCcpVersion::decode(&[0xFF, 0, 7, 2]).is_none());
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    fn frame_at_250ms(bus: &str, id: u32, data: &[u8], master: bool) -> CcpFrame {
        let mut f = CcpFrame::new(bus, id, data.to_vec(), master);
        f.frame.elapsed = Duration::from_millis(250);
        f
    }

    #[test]
    fn ccp_frame_formats_match_expected_contract() {
        let master = frame_at_250ms("TESTBUS", 0x123, &[1, 7], true);
        assert_eq!(master.address(), "\u{2192} 123");
        assert_eq!(master.ctr(), "7");
        assert_eq!(master.len(), 2);
        assert!(!master.is_daq());
        assert!(!master.is_error());
        assert_eq!(master.type_str(), "Connect");
        assert_eq!(
            master.to_csv(),
            "0.250;\"TESTBUS\";\"\u{2192} 123\";2;7;\"Connect\";\"01 07\";\"..\""
        );
        assert_eq!(
            master.to_clipboard(),
            "0.250\tTESTBUS\t\u{2192} 123\t2\t7\tConnect\t01 07\t.."
        );
        assert_eq!(master.raw_frame_length(), 44 + 16);

        let res = frame_at_250ms("TESTBUS", 0x123, &[0xFF, 0x00, 0x07], false);
        assert_eq!(res.address(), "\u{2190} 123");
        assert_eq!(res.ctr(), "7");
        assert_eq!(res.type_str(), "RES(OK)");
        assert!(!res.is_error());
        assert_eq!(
            res.to_csv(),
            "0.250;\"TESTBUS\";\"\u{2190} 123\";3;7;\"RES(OK)\";\"FF 00 07\";\"...\""
        );

        let err = frame_at_250ms("TESTBUS", 0x123, &[0xFF, 0x30, 0x07], false);
        assert!(err.is_error());
        assert_eq!(err.type_str(), "RES(UnknownCommand)");

        let ev = frame_at_250ms("TESTBUS", 0x123, &[0xFE, 0x1A, 0x03], false);
        assert_eq!(ev.ctr(), "3");
        assert_eq!(ev.type_str(), "EV(ColdStartRequest)");
        assert!(!ev.is_daq());

        let daq = frame_at_250ms("TESTBUS", 0x123, &[0x00, 1, 2, 3], false);
        assert!(daq.is_daq());
        assert_eq!(daq.ctr(), "");
        assert_eq!(daq.type_str(), "DAQ");
        assert_eq!(
            daq.to_csv(),
            "0.250;\"TESTBUS\";\"\u{2190} 123\";4;;\"DAQ\";\"00 01 02 03\";\"....\""
        );

        let ext = frame_at_250ms("TESTBUS", 0x8000_0123, &[0xFF, 0x00, 0x07], false);
        assert_eq!(ext.address(), "\u{2190} 123(X)");
        assert_eq!(CcpFrame::RESPONSE_INDEX, 0);
    }

    #[test]
    fn default_timeout_mapping() {
        assert_eq!(default_timeout(Some(CommandCode::BuildChksum), 1), 30_000);
        assert_eq!(default_timeout(Some(CommandCode::ClearMemory), 1), 30_000);
        assert_eq!(default_timeout(Some(CommandCode::Move), 1), 30_000);
        assert_eq!(default_timeout(Some(CommandCode::ActionService), 1), 5_000);
        assert_eq!(default_timeout(Some(CommandCode::DiagService), 1), 500);
        assert_eq!(default_timeout(Some(CommandCode::Program), 1), 100);
        assert_eq!(default_timeout(Some(CommandCode::Program6), 1), 100);
        assert_eq!(default_timeout(Some(CommandCode::Connect), 2), 50);
        assert_eq!(default_timeout(None, 1), 25);
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    struct MockSeedKey {
        seen: std::sync::Arc<StdMutex<Vec<Vec<u8>>>>,
    }

    impl SeedKeyProvider for MockSeedKey {
        fn compute_key_from_seed(&self, seed: &[u8]) -> Option<Vec<u8>> {
            self.seen.lock().unwrap().push(seed.to_vec());
            Some(seed.iter().rev().copied().collect())
        }
    }

    struct XcpOnlyKey;

    impl SeedKeyProvider for XcpOnlyKey {
        fn sk_type(&self) -> SkType {
            SkType::XCP
        }
        fn compute_key_from_seed(&self, seed: &[u8]) -> Option<Vec<u8>> {
            Some(seed.to_vec())
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn connect_flow_with_seed_unlock() {
        let exchange_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ec = std::sync::Arc::clone(&exchange_calls);
        let respond = Box::new(move |data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => {
                    let n = ec.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let prot = if n == 1 { 0x01 } else { 0x00 };
                    vec![0xFF, 0, ctr, 0, 0, 0x03, prot]
                }
                0x0D => vec![0xFF, 0, ctr, 0x81, 0x00],
                0x12 => vec![0xFF, 0, ctr, 1, 0x11, 0x22, 0x33, 0x44],
                0x13 => vec![0xFF, 0, ctr, 0x00],
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        });
        let mut m = blocking_master_with(respond);
        let sk_seen = std::sync::Arc::new(StdMutex::new(Vec::new()));
        m.0.base.seed_and_key = Some(Box::new(MockSeedKey {
            seen: std::sync::Arc::clone(&sk_seen),
        }));
        let states = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let st = std::sync::Arc::clone(&states);
        m.0.base
            .add_connection_state_callback(Box::new(move |c| st.lock().unwrap().push(c)));

        assert_eq!(m.connect(), CmdResult::OK);
        assert!(m.0.base.connected());
        assert!(m.0.base.slave_connected);
        // CONNECT, GET_CCP_VERSION(2.1), EXCHANGE_ID, GET_S_STATUS,
        // GET_SEED(CAL), UNLOCK(key), EXCHANGE_ID, SET_S_STATUS(CAL|RUN), GET_ACTIVE_CAL_PAGE
        assert_eq!(
            sent_payloads(&m.0),
            vec![
                vec![0x01, 0x00, 0x30, 0x00, 0, 0, 0, 0],
                vec![0x1B, 0x01, 0x02, 0x01, 0, 0, 0, 0],
                vec![0x17, 0x02, 0, 0, 0, 0, 0, 0],
                vec![0x0D, 0x03, 0, 0, 0, 0, 0, 0],
                vec![0x12, 0x04, 0x01, 0, 0, 0, 0, 0],
                vec![0x13, 0x05, 0x44, 0x33, 0x22, 0x11, 0, 0],
                vec![0x17, 0x06, 0, 0, 0, 0, 0, 0],
                vec![0x0C, 0x07, 0x81, 0, 0, 0, 0, 0],
                vec![0x09, 0x08, 0, 0, 0, 0, 0, 0],
            ]
        );
        assert!(m.0.device.sent.iter().all(|(id, _)| *id == 0x201));
        assert_eq!(m.version(), "2.1");
        assert!(m.connect_response().is_some());
        assert!(m.status_response().is_some());
        let ex = m.exchange_id_response().unwrap().clone();
        assert!(ex.protection.is_empty());
        assert!(m.can_write());
        assert_eq!(m.active_page(), EcuPage::Flash);
        assert_eq!(m.0.base.frames_sent(), 9);
        assert_eq!(m.0.base.frames_received(), 9);
        assert_eq!(*states.lock().unwrap(), vec![true]);
        assert_eq!(*sk_seen.lock().unwrap(), vec![vec![0x11, 0x22, 0x33, 0x44]]);
        assert_eq!(exchange_calls.load(std::sync::atomic::Ordering::SeqCst), 2);

        let (r, resp, seed) = m.get_seed(ResourceType::CAL);
        assert_eq!(r, CmdResult::OK);
        assert_eq!(resp.unwrap().protection_state, 1);
        assert_eq!(seed, vec![0x11, 0x22, 0x33, 0x44]);
        let (r, unlock_resp) = m.unlock(&seed.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(r, CmdResult::OK);
        assert_eq!(unlock_resp.unwrap().privilege_state, ResourceType::NONE);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn connect_flow_unprotected_skips_unlock() {
        let mut m = blocking_master_with(basic_ecu());
        assert_eq!(m.connect(), CmdResult::OK);
        let sent = sent_payloads(&m.0);
        assert!(!sent.iter().any(|f| f[0] == 0x12));
        assert!(!sent.iter().any(|f| f[0] == 0x13));
        // CONNECT, VERSION, EXCHANGE_ID, GET_S_STATUS, SET_S_STATUS, GET_ACTIVE_CAL_PAGE
        assert_eq!(sent.len(), 6);
        assert!(m.can_write());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn exchange_id_string_read_via_upload() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => vec![0xFF, 0, ctr, 3, 0, 0x03, 0x00], // LengthOfID = 3
                0x0D => vec![0xFF, 0, ctr, 0x81, 0x00],
                0x04 => vec![0xFF, 0, ctr, b'I', b'D', b'X', 0, 0], // UPLOAD
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        });
        let mut m = blocking_master_with(respond);
        assert_eq!(m.connect(), CmdResult::OK);
        assert_eq!(m.exchange_id_str(), "IDX");
        assert!(m.0.device.sent.iter().any(|(_, d)| d[0] == 0x04));
        assert!(!m.0.device.sent.iter().any(|(_, d)| d[0] == 0x0F));
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn respect_optional_cmds_limits_requests() {
        let mut m = BlockingCcpMaster::new(CcpMaster::new(
            ConnectBehaviourType::Manual,
            tp_blob(),
            MockDevice::new(basic_ecu()),
        ));
        m.0.ccp_if
            .optional_cmds
            .insert(CommandCode::GetSStatus.as_u8());
        assert!(m.is_allowed_request(CommandCode::GetSStatus));
        assert!(!m.is_allowed_request(CommandCode::SetSStatus));
        assert!(!m.is_allowed_request(CommandCode::Unlock));
        assert_eq!(m.connect(), CmdResult::OK);
        assert!(!m.0.device.sent.iter().any(|(_, d)| d[0] == 0x0C));
        assert!(m.0.device.sent.iter().any(|(_, d)| d[0] == 0x0D));
        assert!(m.0.device.sent.iter().any(|(_, d)| d[0] == 0x1B));
        assert!(m.0.device.sent.iter().any(|(_, d)| d[0] == 0x17));
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn unlock_ecu_argument_checks() {
        let mut m = blocking_master_with(basic_ecu());
        assert_eq!(m.connect(), CmdResult::OK);
        assert_eq!(m.unlock_ecu(ResourceType::CAL), CmdResult::InvalidArgument);
        assert_eq!(
            m.unlock_ecu_with(&XcpOnlyKey, ResourceType::CAL),
            CmdResult::InvalidArgument
        );
        let sk = MockSeedKey {
            seen: std::sync::Arc::new(StdMutex::new(Vec::new())),
        };
        assert_eq!(m.unlock_ecu_with(&sk, ResourceType::NONE), CmdResult::OK);
        m.0.base.slave_connected = false;
        assert_eq!(
            m.unlock_ecu_with(&sk, ResourceType::CAL),
            CmdResult::Timeout
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn error_response_raises_callback_and_keeps_connection() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            if data[0] == 0x0D {
                return vec![vec![0xFF, 0x30, ctr]]; // UnknownCommand
            }
            vec![vec![0xFF, 0, ctr]]
        });
        let mut m = blocking_master_with(respond);
        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let er = std::sync::Arc::clone(&errors);
        m.add_error_callback(Box::new(move |a| er.lock().unwrap().push(*a)));
        let (r, resp) = m.get_status();
        assert_eq!(r, CmdResult::UnknownCommand);
        assert!(resp.is_none());
        assert_eq!(m.last_error_response(), CmdResult::UnknownCommand);
        assert_eq!(m.last_error_text(), "UnknownCommand");
        assert_eq!(m.0.base.errors_received(), 1);
        let got = errors.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].error_code, CmdResult::UnknownCommand);
        assert_eq!(got[0].command, CommandCode::GetSStatus);
        drop(got);
        m.0.base.slave_connected = true;
        let _ = m.get_status();
        assert!(m.0.base.slave_connected);
        m.reset_last_error();
        assert_eq!(m.last_error_response(), CmdResult::OK);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn timeout_resets_connection_state() {
        let respond = Box::new(|_data: &[u8]| Vec::new());
        let mut m = blocking_master_with(respond);
        m.0.base.slave_connected = true;
        let (r, resp) = m.get_ccp_versions(2, 1);
        assert_eq!(r, CmdResult::Timeout);
        assert!(resp.is_none());
        assert!(!m.0.base.slave_connected);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn ctr_mismatch_is_protocol_failure() {
        let respond = Box::new(|_data: &[u8]| vec![vec![0xFF, 0, 0x99]]);
        let mut m = blocking_master_with(respond);
        assert_eq!(m.connect(), CmdResult::ProtocolFailure);
        assert!(!m.0.base.slave_connected);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn short_response_is_protocol_failure() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            vec![vec![0xFF, 0, ctr]]
        });
        let mut m = blocking_master_with(respond);
        let (r, resp) = m.exchange_id();
        assert_eq!(r, CmdResult::ProtocolFailure);
        assert!(resp.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn send_failure_is_snd_cmd_failed() {
        struct ClosedDevice {
            core: DeviceCore,
        }
        #[async_trait]
        impl CanDevice for ClosedDevice {
            fn core(&self) -> &DeviceCore {
                &self.core
            }
            fn core_mut(&mut self) -> &mut DeviceCore {
                &mut self.core
            }
            async fn is_available(&mut self) -> autors_can::error::Result<bool> {
                Ok(false)
            }
            async fn open(&mut self, _c: CanConfiguration) -> autors_can::error::Result<bool> {
                Ok(false)
            }
            async fn close(&mut self) {}
            async fn send(
                &mut self,
                _id: u32,
                _d: &[u8],
                _t: FrameType,
            ) -> autors_can::error::Result<usize> {
                Ok(0)
            }
            async fn receive(&mut self) -> autors_can::error::Result<Option<CanFrame>> {
                Ok(None)
            }
        }
        let device = ClosedDevice {
            core: DeviceCore::new(),
        };
        let mut m = BlockingCcpMaster::new(CcpMaster::new(
            ConnectBehaviourType::Manual,
            tp_blob(),
            device,
        ));
        m.0.config.respect_optional_cmds = false;
        m.0.base.slave_connected = true;
        assert_eq!(m.connect(), CmdResult::SndCmdFailed);
        assert!(!m.0.base.slave_connected);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn read_sync_short_up_and_upload() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => vec![0xFF, 0, ctr, 0, 0, 0x03, 0x00],
                0x0D => vec![0xFF, 0, ctr, 0x81, 0x00],
                0x0F => vec![0xFF, 0, ctr, 0xAA, 0xBB, 0xCC, 0xDD],
                0x04 => vec![0xFF, 0, ctr, 0x01, 0x02, 0x03, 0x04, 0x05],
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        });
        let mut m = blocking_master_with(respond);
        assert_eq!(m.connect(), CmdResult::OK);
        let before = m.0.device.sent.len();
        let data = m.read_sync(4, 0, 0x1000, None).unwrap();
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        let sent = &m.0.device.sent[before..];
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].1,
            vec![0x0F, 0x06, 0x04, 0x00, 0x00, 0x10, 0x00, 0x00]
        );
        let before = m.0.device.sent.len();
        let data = m.read_sync(12, 0, 0x2000, None).unwrap();
        assert_eq!(data.len(), 12);
        assert_eq!(data, vec![1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1, 2]);
        let sent: Vec<&Vec<u8>> = m.0.device.sent[before..].iter().map(|(_, d)| d).collect();
        assert_eq!(
            sent[0][..8],
            [0x02, 0x07, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00]
        );
        assert_eq!(sent[1][..3], [0x04, 0x08, 0x05]);
        assert_eq!(sent[2][..3], [0x04, 0x09, 0x05]);
        assert_eq!(sent[3][..3], [0x04, 0x0A, 0x02]);
        let mut m2 = blocking_master_with(basic_ecu());
        assert_eq!(m2.read_sync(4, 0, 0x1000, None), None);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn epk_check_mod_par_variant() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => vec![0xFF, 0, ctr, 0, 0, 0x03, 0x00],
                0x0F | 0x04 => vec![0xFF, 0, ctr, b'E', b'P', b'K', b'1', b'2'],
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        });
        let mut m = blocking_master_with(respond);
        assert_eq!(m.connect(), CmdResult::OK);

        let before = m.0.device.sent.len();
        assert_eq!(
            m.epk_check_mod_par(None),
            (EpkCheckResult::NotApplicable, None)
        );
        let no_epk = ModPar::default();
        assert_eq!(
            m.epk_check_mod_par(Some(&no_epk)),
            (EpkCheckResult::NotApplicable, None)
        );
        let blank_epk = ModPar {
            epk: Some(String::new()),
            ..ModPar::default()
        };
        assert_eq!(
            m.epk_check_mod_par(Some(&blank_epk)),
            (EpkCheckResult::NotApplicable, None)
        );
        assert_eq!(m.0.device.sent.len(), before);

        let mp = ModPar {
            epk: Some("EPK12".into()),
            epk_address: Some(0x1000),
            ..ModPar::default()
        };
        let before = m.0.device.sent.len();
        let (r, dev) = m.epk_check_mod_par(Some(&mp));
        assert_eq!(r, EpkCheckResult::Equal);
        assert_eq!(dev.as_deref(), Some("EPK12"));
        let sent = &m.0.device.sent[before..];
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1[0], 0x0F); // SHORT_UP
        assert_eq!(sent[0].1[2], 5);
        assert_eq!(sent[0].1[4..8], [0x00, 0x10, 0x00, 0x00]); // ADDR_EPK 0x1000(LE)

        let mp_diff = ModPar {
            epk: Some("XXXXX".into()),
            epk_address: Some(0x1000),
            ..ModPar::default()
        };
        let (r, dev) = m.epk_check_mod_par(Some(&mp_diff));
        assert_eq!(r, EpkCheckResult::NotEqual);
        assert_eq!(dev.as_deref(), Some("EPK12"));

        let mp_no_addr = ModPar {
            epk: Some("EPK12".into()),
            ..ModPar::default()
        };
        let before = m.0.device.sent.len();
        let (r, _) = m.epk_check_mod_par(Some(&mp_no_addr));
        assert_eq!(r, EpkCheckResult::Equal);
        let sent = &m.0.device.sent[before..];
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1[0], 0x04); // UPLOAD
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn epk_check_mod_par_read_failure() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            match data[0] {
                0x1B => vec![vec![0xFF, 0, ctr, 2, 1]],
                0x17 => vec![vec![0xFF, 0, ctr, 0, 0, 0x03, 0x00]],
                0x0F | 0x04 => Vec::new(),
                _ => vec![vec![0xFF, 0, ctr]],
            }
        });
        let mut m = blocking_master_with(respond);
        assert_eq!(m.connect(), CmdResult::OK);
        let mp = ModPar {
            epk: Some("EPK12".into()),
            epk_address: Some(0x1000),
            ..ModPar::default()
        };
        let (r, dev) = m.epk_check_mod_par(Some(&mp));
        assert_eq!(r, EpkCheckResult::Failed);
        assert_eq!(dev, None);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn write_sync_chunking_and_flash_guard() {
        let mut m = blocking_master_with(basic_ecu());
        assert_eq!(m.connect(), CmdResult::OK);
        let before = m.0.device.sent.len();
        assert!(!m.write_sync(0, 0x2000, &[1, 2, 3], None));
        assert_eq!(m.0.device.sent.len(), before);
        let data: Vec<u8> = (0..13u8).collect();
        assert!(m.program_sync(0, 0x2000, &data, None, None));
        let sent: Vec<&Vec<u8>> = m.0.device.sent[before..].iter().map(|(_, d)| d).collect();
        assert_eq!(
            sent[0][..8],
            [0x02, 0x06, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00]
        ); // SET_MTA
        assert_eq!(sent[1][..8], [0x22, 0x07, 0, 1, 2, 3, 4, 5]); // PROGRAM_6
        assert_eq!(sent[2][..8], [0x22, 0x08, 6, 7, 8, 9, 10, 11]);
        assert_eq!(sent[3][..4], [0x18, 0x09, 0x01, 12]); // PROGRAM(1)
        let mut tp = tp_blob();
        tp.children.push(crate::ifdata_ccp::CcpNode::DefinedPages(
            crate::ifdata_ccp::CcpDefinedPages {
                no: 0,
                page_name: "RAM".to_string(),
                address_ext: 0,
                address: 0,
                length: 0x1_0000,
                page_type: CcpMemoryPageType(CcpMemoryPageType::RAM),
            },
        ));
        let mut m3 = BlockingCcpMaster::new(CcpMaster::new(
            ConnectBehaviourType::Manual,
            tp,
            MockDevice::new(basic_ecu()),
        ));
        m3.0.config.respect_optional_cmds = false;
        assert_eq!(m3.connect(), CmdResult::OK);
        assert_eq!(m3.active_page(), EcuPage::RAM);
        let before = m3.0.device.sent.len();
        assert!(m3.write_sync(0, u32::MAX, &[9, 8, 7], None));
        let sent: Vec<&Vec<u8>> = m3.0.device.sent[before..].iter().map(|(_, d)| d).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0][0], 0x03); // DNLOAD
        assert_eq!(sent[0][2..6], [0x03, 9, 8, 7]);
        assert_eq!(m.download(&[0; 6]).0, CmdResult::InvalidArgument);
        assert_eq!(m.program(&[0; 6]).0, CmdResult::InvalidArgument);
        assert_eq!(m.download6(&[0; 5]).0, CmdResult::InvalidArgument);
        assert_eq!(m.program6(&[0; 7]).0, CmdResult::InvalidArgument);
        assert_eq!(
            m.start_stop_all(StartStopMode::Select),
            CmdResult::InvalidArgument
        );
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    use crate::ifdata_ccp::{CcpQpBlob, CcpRaster, CcpSource};

    fn daq_source_map() -> BTreeMap<u16, SourceAndRaster> {
        let mut map = BTreeMap::new();
        map.insert(
            0u16,
            SourceAndRaster {
                source: CcpSource {
                    active: true,
                    qp_blob: Some(CcpQpBlob {
                        daq_no: 0,
                        length: 2,
                        can_id: 0x300,
                        ..CcpQpBlob::default()
                    }),
                    ..CcpSource::default()
                },
                raster: CcpRaster {
                    evt_chn_no: 5,
                    ..CcpRaster::default()
                },
            },
        );
        map
    }

    fn meas(name: &str, addr: u32, dt: DataType) -> MeasurementInfo {
        MeasurementInfo {
            name: name.to_string(),
            address: addr,
            data_type: dt,
            ..MeasurementInfo::default()
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn daq_configure_start_receive_stop() {
        let mut m = blocking_master_with(basic_ecu());
        m.set_source_and_raster_map(daq_source_map());
        assert_eq!(m.connect(), CmdResult::OK);
        let measurements = vec![
            DaqMeasurement::new(meas("a", 0x1000, DataType::UByte), 0, None),
            DaqMeasurement::new(meas("b", 0x1001, DataType::UWord), 0, None),
        ];
        let remaining = m.configure_measurements(measurements).unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(m.0.base.daqs.lists.len(), 1);
        assert_eq!(m.0.base.daqs.lists[0].odts.len(), 1);
        let before = m.0.device.sent.len();
        assert!(m.start_measurements(false));
        assert!(m.is_daq_running());
        let sent: Vec<&Vec<u8>> = m.0.device.sent[before..].iter().map(|(_, d)| d).collect();
        let cmds: Vec<u8> = sent.iter().map(|d| d[0]).collect();
        assert_eq!(
            cmds,
            vec![0x0D, 0x0C, 0x14, 0x15, 0x16, 0x15, 0x16, 0x0C, 0x06],
            "GET_S_STATUS, SET_S_STATUS(~DAQ), GET_DAQ_SIZE, (SET_DAQ_PTR+WRITE_DAQ)×2, SET_S_STATUS(DAQ), START_STOP"
        );
        assert_eq!(sent[3][0], 0x15);
        assert_eq!(sent[3][2..5], [0x00, 0x00, 0x00]);
        assert_eq!(sent[4][0], 0x16);
        assert_eq!(sent[4][2..8], [0x01, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(sent[5][0], 0x15);
        assert_eq!(sent[5][2..5], [0x00, 0x00, 0x01]);
        assert_eq!(sent[6][0], 0x16);
        assert_eq!(sent[6][2..8], [0x02, 0x00, 0x01, 0x10, 0x00, 0x00]);
        assert_eq!(sent[1][2], 0x81);
        assert_eq!(sent[7][2], 0x83);
        // START_STOP(Start, daq 0, lastODT 0, evt 5, prescaler 1)
        assert_eq!(sent[8][0], 0x06);
        assert_eq!(sent[8][2..8], [0x01, 0x00, 0x00, 0x05, 0x01, 0x00]);

        let values = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let vv = std::sync::Arc::clone(&values);
        m.0.base
            .add_values_received_callback(Box::new(move |a| vv.lock().unwrap().push(a.clone())));
        m.0.device.push_rx(0x300, &[0x00, 0x10, 0x34, 0x12]);
        assert_eq!(m.poll(), 1);
        assert!(values.lock().unwrap().is_empty());
        m.0.device.push_rx(0x300, &[0x00, 0x20, 0x78, 0x56]);
        assert_eq!(m.poll(), 1);
        let got = values.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].daq_list_no, 0);
        assert_eq!(got[0].data, vec![0x10, 0x34, 0x12]);
        drop(got);
        assert_eq!(
            m.0.base
                .get_daq_raw_value(&meas("a", 0x1000, DataType::UByte), -1, 0),
            Some(0x10 as f64)
        );
        assert_eq!(
            m.0.base
                .get_daq_raw_value(&meas("b", 0x1001, DataType::UWord), -1, 0),
            Some(0x1234 as f64)
        );

        assert!(m.stop_measurements(false));
        assert!(!m.is_daq_running());
        let last = m.0.device.sent.last().unwrap().1.clone();
        assert_eq!(last[0], 0x08);
        assert_eq!(last[2], 0x00);
    }

    #[test]
    fn daq_dict_ccp_build_variants() {
        let mut map = BTreeMap::new();
        map.insert(
            0u16,
            SourceAndRaster {
                source: CcpSource {
                    active: true,
                    qp_blob: Some(CcpQpBlob {
                        daq_no: 0,
                        length: 3,
                        can_id: 0x300,
                        ..CcpQpBlob::default()
                    }),
                    ..CcpSource::default()
                },
                raster: CcpRaster {
                    evt_chn_no: 5,
                    ..CcpRaster::default()
                },
            },
        );
        map.insert(
            1u16,
            SourceAndRaster {
                source: CcpSource {
                    active: false,
                    qp_blob: None,
                    ..CcpSource::default()
                },
                raster: CcpRaster::default(),
            },
        );
        let mut measurements = vec![DaqMeasurement::new(
            meas("a", 0x1000, DataType::UByte),
            0,
            None,
        )];
        let dict = DaqDictCcp::build(&map, &mut measurements).unwrap();
        assert_eq!(dict.lists.len(), 2);
        assert_eq!(dict.lists[0].daq_no(), 0);
        assert_eq!(dict.lists[0].first_pid(), 0); // QP first_pid 0xFF → 0
        assert_eq!(dict.lists[0].can_id, 0x300);
        assert_eq!(dict.lists[0].evt_no, 5);
        assert_eq!(dict.lists[1].evt_no, u16::MAX);
        assert!(dict.lists[1].odts.is_empty());
        assert!(measurements.is_empty());
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[cfg(feature = "blocking")]
    #[test]
    fn disconnect_flow_resets_state() {
        let mut m = blocking_master_with(basic_ecu());
        let states = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let st = std::sync::Arc::clone(&states);
        m.0.base
            .add_connection_state_callback(Box::new(move |c| st.lock().unwrap().push(c)));
        assert_eq!(m.connect(), CmdResult::OK);
        assert_eq!(m.0.base.frames_sent(), 6);
        assert!(m.disconnect(-1));
        assert!(!m.0.base.slave_connected);
        assert!(!m.0.base.connected());
        let last = m.0.device.sent.last().unwrap().1.clone();
        assert_eq!(last[..6], [0x07, 0x06, 0x01, 0x00, 0x30, 0x00]);
        assert_eq!(m.0.base.frames_sent(), 0);
        assert_eq!(m.0.base.frames_received(), 0);
        assert_eq!(*states.lock().unwrap(), vec![true, false]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn comm_kernel_registration_and_poll_alive() {
        let m = master_with(basic_ecu());
        let shared = std::sync::Arc::new(std::sync::Mutex::new(m));
        assert!(CommKernel::register_client(&shared).is_ok());
        assert!(CommKernel::is_registered(&shared));
        BlockingCommKernel::poll_clients();
        assert!(shared.lock().unwrap().device.sent.is_empty());
        {
            let mut g = shared.lock().unwrap();
            g.base.connect_behaviour = ConnectBehaviourType::Automatic;
            g.config.connection_test_cycle = 0;
        }
        BlockingCommKernel::poll_clients();
        assert!(!shared.lock().unwrap().device.sent.is_empty());
        assert!(shared.lock().unwrap().base.connected());
        assert!(CommKernel::deregister_client(&shared));
        assert!(!CommKernel::is_registered(&shared));
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_connect_flow_and_command_loopback() {
        let mut m = master_with(basic_ecu());
        assert_eq!(m.connect().await, CmdResult::OK);
        assert!(m.base.connected());
        // CONNECT, VERSION, EXCHANGE_ID, GET_S_STATUS, SET_S_STATUS, GET_ACTIVE_CAL_PAGE
        assert_eq!(m.device.sent.len(), 6);
        assert_eq!(m.version(), "2.1");
        assert!(m.connect_response().is_some());
        assert!(m.can_write());

        let (r, st) = m.get_status().await;
        assert_eq!(r, CmdResult::OK);
        assert_eq!(st.unwrap().session_state, SessionState(0x81));
        let (r, ex) = m.exchange_id().await;
        assert_eq!(r, CmdResult::OK);
        assert_eq!(ex.unwrap().availability, ResourceType(0x03));
        let (r, size) = m.get_daq_size(0, 0x300).await;
        assert_eq!(r, CmdResult::OK);
        let size = size.unwrap();
        assert_eq!((size.daq_list_size, size.first_pid), (2, 0));
        assert!(m.device.sent.iter().all(|(id, _)| *id == 0x201));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_seed_unlock_and_read_sync_loopback() {
        let respond = Box::new(|data: &[u8]| {
            let ctr = data[1];
            let resp = match data[0] {
                0x1B => vec![0xFF, 0, ctr, 2, 1],
                0x17 => vec![0xFF, 0, ctr, 0, 0, 0x03, 0x01],
                0x0D => vec![0xFF, 0, ctr, 0x81, 0x00],
                0x12 => vec![0xFF, 0, ctr, 1, 0x11, 0x22, 0x33, 0x44], // GET_SEED
                0x13 => vec![0xFF, 0, ctr, 0x03],                      // UNLOCK → CAL|DAQ
                0x0F => vec![0xFF, 0, ctr, 0xAA, 0xBB, 0xCC, 0xDD],    // SHORT_UP
                _ => vec![0xFF, 0, ctr],
            };
            vec![resp]
        });
        let mut m = master_with(respond);
        assert_eq!(m.connect().await, CmdResult::OK);
        let (r, resp, seed) = m.get_seed(ResourceType::CAL).await;
        assert_eq!(r, CmdResult::OK);
        assert_eq!(resp.unwrap().protection_state, 1);
        assert_eq!(seed, vec![0x11, 0x22, 0x33, 0x44]);
        let (r, unlock_resp) = m
            .unlock(&seed.iter().rev().copied().collect::<Vec<_>>())
            .await;
        assert_eq!(r, CmdResult::OK);
        assert_eq!(unlock_resp.unwrap().privilege_state, ResourceType(0x03));
        let data = m.read_sync(4, 0, 0x1000, None).await.unwrap();
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(
            m.device.sent.last().unwrap().1,
            vec![0x0F, 0x08, 0x04, 0x00, 0x00, 0x10, 0x00, 0x00]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_timeout_and_poll_loopback() {
        let respond = Box::new(|_data: &[u8]| Vec::new());
        let mut m = master_with(respond);
        m.base.slave_connected = true;
        let (r, resp) = m.get_ccp_versions(2, 1).await;
        assert_eq!(r, CmdResult::Timeout);
        assert!(resp.is_none());
        assert!(!m.base.slave_connected);

        let mut m2 = master_with(basic_ecu());
        let events = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let ev = std::sync::Arc::clone(&events);
        m2.add_event_callback(Box::new(move |a| ev.lock().unwrap().push(*a)));
        m2.device.push_rx(0x202, &[0xFE, 0x1A, 0x03]); // EV(ColdStartRequest)
        m2.device.push_rx(0x300, &[0x00, 1, 2, 3]);
        assert_eq!(m2.poll().await, 2);
        let got = events.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].error_code, CmdResult::ColdStartRequest);
        assert_eq!(m2.base.events_received(), 1);
    }
}
