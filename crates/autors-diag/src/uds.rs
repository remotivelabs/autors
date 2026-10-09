//! Unified Diagnostic Services message codecs and client state machine.
//! Requests and responses use network byte order. The asynchronous client
//! delegates physical transport to [`UdsTransport`], handles negative
//! responses, and exposes typed results for sessions, routines, DTCs, and data.

use crate::error::{Error, Result};
use async_trait::async_trait;
use indexmap::IndexMap;

// ---------------------------------------------------------------------------
// `array_to_num([0x12, 0x34])` produces `0x1234`.
// ---------------------------------------------------------------------------

fn read_u8(response: &[u8], offset: &mut usize) -> Result<u8> {
    let b = response.get(*offset).ok_or_else(|| {
        Error::Parse(format!(
            "UDS response too short: no byte at offset {offset}"
        ))
    })?;
    *offset += 1;
    Ok(*b)
}

fn array_to_num(response: &[u8], offset: &mut usize, len: usize) -> Result<u64> {
    let mut value = 0u64;
    for _ in 0..len {
        value = (value << 8) | u64::from(read_u8(response, offset)?);
    }
    Ok(value)
}

fn num_to_array(value: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for i in (0..len).rev() {
        out.push((value >> (8 * i)) as u8);
    }
    out
}

fn byte_len(value: i64) -> u8 {
    if value > i64::from(u32::MAX) {
        8
    } else if value > 0xFFFF {
        4
    } else if value <= 0xFF {
        1
    } else {
        2
    }
}

macro_rules! int_enum {
    ($(#[$meta:meta])* $name:ident : $repr:ty { $($variant:ident = $value:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr($repr)]
        pub enum $name {
            $($variant = $value),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn from_value(value: $repr) -> Option<Self> {
                $(if value == $value { return Some(Self::$variant); })+
                None
            }

            pub fn as_value(self) -> $repr {
                self as $repr
            }

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant)),+
                }
            }
        }
    };
}

int_enum! {
    MsgState: i8 {
        PendingSnd = -4,
        PendingSndFC = -3,
        PendingRcvFC = -2,
        PendingRcv = -1,
        Success = 0,
        ErrRequestLenExceeded = 1,
        ErrMsgInProcess = 2,
        ErrSendRequest = 3,
        ErrUnexpectedSequenceNo = 4,
        ErrTimeoutAwaitingCFFrame = 5,
        ErrReceivedUnexpectedFCFrame = 6,
        ErrFlowControlOverflow = 7,
        ErrTimeout = 8,
        ErrUnexpectedRSID = 9,
        ErrGeneric = 10,
    }
}

impl MsgState {
    pub fn to_code(self) -> u8 {
        self as i8 as u8
    }
}

int_enum! {
    Sid: u8 {
        DiagnosticSessionControl = 0x10,
        ECUReset = 0x11,
        SecurityAccess = 0x27,
        CommunicationControl = 0x28,
        TesterPresent = 0x3E,
        AccessTimingParameter = 0x83,
        SecuredDataTransmission = 0x84,
        ControlDTCSetting = 0x85,
        ResponseOnEvent = 0x86,
        LinkControl = 0x87,
        ReadDataByIdentifier = 0x22,
        ReadMemoryByAddress = 0x23,
        ReadScalingDataByIdentifier = 0x24,
        ReadDataByPeriodicIdentifier = 0x2A,
        DynamicallyDefineDataIdentifier = 0x2C,
        WriteDataByIdentifier = 0x2E,
        WriteMemoryByAddress = 0x3D,
        ClearDiagnosticInformation = 0x14,
        ReadDTCInformation = 0x19,
        InputOutputControlByIdentifier = 0x2F,
        RoutineControl = 0x31,
        RequestDownload = 0x34,
        RequestUpload = 0x35,
        TransferData = 0x36,
        RequestTransferExit = 0x37,
        RequestFileTransfer = 0x38,
    }
}

int_enum! {
    NegRespCode: u8 {
        Positive = 0x00,
        GeneralReject = 0x10,
        ServiceNotSupported = 0x11,
        SubFunctionNotSupported = 0x12,
        IncorrectMessageLengthOrInvalidFormat = 0x13,
        ResponseTooLong = 0x14,
        BusyRepeatRequest = 0x21,
        ConditionsNotCorrect = 0x22,
        RequestSequenceError = 0x24,
        NoResponseFromSubnetComponent = 0x25,
        FailurePreventsExecutionOfRequestedAction = 0x26,
        RequestOutOfRange = 0x31,
        SecurityAccessDenied = 0x33,
        InvalidKey = 0x35,
        ExceededNumberOfAttempts = 0x36,
        RequiredTimeDelayNotExpired = 0x37,
        UploadDownloadNotAccepted = 0x70,
        TransferDataSuspended = 0x71,
        GeneralProgrammingFailure = 0x72,
        WrongBlockSequenceCounter = 0x73,
        RequestCorrectlyReceivedResponsePending = 0x78,
        SubFunctionNotSupportedInActiveSession = 0x7E,
        ServiceNotSupportedInActiveSession = 0x7F,
        RpmTooHigh = 0x81,
        RpmTooLow = 0x82,
        EngineIsRunning = 0x83,
        EngineIsNotRunning = 0x84,
        EngineRunTimeTooLow = 0x85,
        TemperatureTooHigh = 0x86,
        TemperatureTooLow = 0x87,
        VehicleSpeedTooHigh = 0x88,
        VehicleSpeedTooLow = 0x89,
        ThrottlePedalTooHigh = 0x8A,
        ThrottlePedalTooLow = 0x8B,
        TransmissionRangeNotInNeutral = 0x8C,
        TransmissionRangeNotInGear = 0x8D,
        BrakeSwitchesNotClosed = 0x8F,
        ShifterLeverNotInPark = 0x90,
        TorqueConverterClutchLocked = 0x91,
        VoltageTooHigh = 0x92,
        VoltageTooLow = 0x93,
        UserCancelled = 0xFE,
        Timeout = 0xFF,
    }
}

int_enum! {
    CommunicationControlType: u8 {
        EnableRxAndTx = 0,
        EnableRxAndDisableTx = 1,
        DisableRxAndEnableTx = 2,
        DisableRxAndTx = 3,
        EnableRxAndDisableTxWithEnhancedAddressInformation = 4,
        EnableRxAndTxWithEnhancedAddressInformation = 5,
    }
}

int_enum! {
    CommunicationType: u8 {
        NormalCommunicationMessages = 1,
        NetworkCommunicationMessages = 2,
        NormalAndNetworkCommunicationMessages = 3,
        Reserved1 = 4,
        Reserved2 = 8,
    }
}

int_enum! {
    DefinitionType: u8 {
        DefineByIdentifier = 1,
        DefineByMemoryAddress = 2,
        ClearDynamicallyDefinedDataIdentifier = 3,
    }
}

int_enum! {
    DiagnosticSessionType: u8 {
        Default = 1,
        Programming = 2,
        Extended = 3,
        SafetySystem = 4,
    }
}

int_enum! {
    DtcFormatIdentifier: u8 {
        SaeJ2012DaDtcFormat00 = 0,
        Iso14229DtcFormat = 1,
        SaeJ1939DtcFormat = 2,
        Iso11992DtcFormat = 3,
        SaeJ2012DaDtcFormat04 = 4,
    }
}

int_enum! {
    DtcSettingType: u8 {
        On = 1,
        Off = 2,
    }
}

int_enum! {
    LinkControlType: u8 {
        VerifyModeTransitionWithFixedParameter = 1,
        VerifyModeTransitionWithSpecificParameter = 2,
        TransitionMode = 3,
    }
}

int_enum! {
    ModeOfOperationType: u8 {
        AddFile = 1,
        DeleteFile = 2,
        ReplaceFile = 3,
        ReadFile = 4,
        ReadDir = 5,
    }
}

int_enum! {
    ReportDtcType: u8 {
        ReportNumberOfDTCByStatusMask = 0x01,
        ReportDTCByStatusMask = 0x02,
        ReportDTCSnapshotIdentification = 0x03,
        ReportDTCSnapshotRecordByDTCNumber = 0x04,
        ReportDTCStoredDataByRecordNumber = 0x05,
        ReportDTCExtDataRecordByDTCNumber = 0x06,
        ReportNumberOfDTCBySeverityMaskRecord = 0x07,
        ReportDTCBySeverityMaskRecord = 0x08,
        ReportSeverityInformationOfDTC = 0x09,
        ReportSupportedDTC = 0x0A,
        ReportFirstTestFailedDTC = 0x0B,
        ReportFirstConfirmedDTC = 0x0C,
        ReportMostRecentTestFailedDTC = 0x0D,
        ReportMostRecentConfirmedDTC = 0x0E,
        ReportMirrorMemoryDTCByStatusMask = 0x0F,
        ReportMirrorMemoryDTCExtDataRecordByDTCNumber = 0x10,
        ReportNumberOfMirrorMemoryDTCByStatusMask = 0x11,
        ReportNumberOfEmissionsOBDDTCByStatusMask = 0x12,
        ReportEmissionsOBDDTCByStatusMask = 0x13,
        ReportDTCFaultDetectionCounter = 0x14,
        ReportDTCWithPermanentStatus = 0x15,
        ReportDTCExtDataRecordByRecordNumber = 0x16,
        ReportUserDefMemoryDTCByStatusMask = 0x17,
        ReportUserDefMemoryDTCSnapshotRecordByDTCNumber = 0x18,
        ReportUserDefMemoryDTCExtDataRecordByDTCNumber = 0x19,
        ReportWWHOBDDTCByMaskRecord = 0x42,
        ReportWWHOBDDTCWithPermanentStatus = 0x43,
    }
}

int_enum! {
    ResetType: u8 {
        HardReset = 1,
        KeyOffOnReset = 2,
        SoftReset = 3,
        EnableRapidPowerShutDown = 4,
        DisableRapidPowerShutDown = 5,
    }
}

int_enum! {
    ResponseOnEventType: u8 {
        StopResponseOnEvent = 0x00,
        OnDTCStatusChange = 0x01,
        OnTimerInterrupt = 0x02,
        OnChangeOfDataIdentifier = 0x03,
        ReportActivatedEvents = 0x04,
        StartResponseOnEvent = 0x05,
        ClearResponseOnEvent = 0x06,
        OnComparisonOfValues = 0x07,
        StoreEvent = 0x40,
    }
}

int_enum! {
    RoutineControlType: u8 {
        StartRoutine = 1,
        StopRoutine = 2,
        RequestRoutineResults = 3,
    }
}

int_enum! {
    RoutineIdentifierType: u16 {
        DeployLoopRoutineID = 0xE200,
        EraseMemory = 0xFF00,
        CheckProgrammingDependencies = 0xFF01,
        EraseMirrorMemoryDTCs = 0xFF02,
    }
}

int_enum! {
    TimingParameterAccessType: u8 {
        ReadExtendedTimingParameterSet = 1,
        SetTimingParametersToDefaultValues = 2,
        ReadCurrentlyActiveTimingParameters = 3,
        SetTimingParametersToGivenValues = 4,
    }
}

int_enum! {
    TransmissionModeType: u8 {
        SendAtSlowRate = 1,
        SendAtMediumRate = 2,
        SendAtFastRate = 3,
        StopSending = 4,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DtcStatusMask(pub u8);

impl DtcStatusMask {
    pub const TEST_FAILED: Self = Self(0x01);
    pub const TEST_FAILED_THIS_OPERATION_CYCLE: Self = Self(0x02);
    pub const PENDING_DTC: Self = Self(0x04);
    pub const CONFIRMED_DTC: Self = Self(0x08);
    pub const TEST_NOT_COMPLETED_SINCE_LAST_CLEAR: Self = Self(0x10);
    pub const TEST_FAILED_SINCE_LAST_CLEAR: Self = Self(0x20);
    pub const TEST_NOT_COMPLETED_THIS_OPERATION_CYCLE: Self = Self(0x40);
    pub const WARNING_INDICATOR_REQUESTED: Self = Self(0x80);

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for DtcStatusMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DtcAndStatusRecord {
    pub dtc: u32,
    pub status_of_dtc: DtcStatusMask,
}

impl DtcAndStatusRecord {
    fn parse(response: &[u8], offset: &mut usize) -> Result<Self> {
        let dtc = array_to_num(response, offset, 3)? as u32;
        let status_of_dtc = DtcStatusMask(read_u8(response, offset)?);
        Ok(DtcAndStatusRecord { dtc, status_of_dtc })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SrcDataIdentifier {
    pub identifier: u16,
    pub position_in_src_data_record: u8,
    pub memory_size: u8,
}

impl SrcDataIdentifier {
    pub fn new(identifier: u16, position_in_src_data_record: u8, memory_size: u8) -> Self {
        SrcDataIdentifier {
            identifier,
            position_in_src_data_record,
            memory_size,
        }
    }

    pub fn to_array(&self) -> Vec<u8> {
        let mut out = num_to_array(u64::from(self.identifier), 2);
        out.push(self.position_in_src_data_record);
        out.push(self.memory_size);
        out
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SrcDataMemory {
    pub address: u64,
    pub size: u64,
}

impl SrcDataMemory {
    pub fn new(address: u64, size: u64) -> Self {
        SrcDataMemory { address, size }
    }

    pub fn to_array(&self, adr_len: u8, size_len: u8) -> Vec<u8> {
        let mut out = num_to_array(self.address, usize::from(adr_len));
        out.extend_from_slice(&num_to_array(self.size, usize::from(size_len)));
        out
    }
}

pub fn has_subfunction(sid: Sid) -> bool {
    match sid {
        Sid::DiagnosticSessionControl
        | Sid::ECUReset
        | Sid::SecurityAccess
        | Sid::CommunicationControl
        | Sid::TesterPresent
        | Sid::AccessTimingParameter
        | Sid::ControlDTCSetting
        | Sid::ResponseOnEvent
        | Sid::LinkControl
        | Sid::DynamicallyDefineDataIdentifier
        | Sid::ReadDTCInformation
        | Sid::RoutineControl => true,
        Sid::SecuredDataTransmission
        | Sid::ReadDataByIdentifier
        | Sid::ReadMemoryByAddress
        | Sid::ReadScalingDataByIdentifier
        | Sid::ReadDataByPeriodicIdentifier
        | Sid::WriteDataByIdentifier
        | Sid::WriteMemoryByAddress
        | Sid::ClearDiagnosticInformation
        | Sid::InputOutputControlByIdentifier
        | Sid::RequestDownload
        | Sid::RequestUpload
        | Sid::TransferData
        | Sid::RequestTransferExit
        | Sid::RequestFileTransfer => false,
    }
}

// ---------------------------------------------------------------------------
//
// ---------------------------------------------------------------------------

pub trait UdsResponse: Default {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool>;

    fn base(&self) -> &RespBase;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespBase {
    pub error_code: u8,
    pub service_id: u8,
}

impl RespBase {
    pub fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        let first = read_u8(response, offset)?;
        if first == NEG_RESPONSE_VALUE {
            self.service_id = read_u8(response, offset)?;
            self.error_code = read_u8(response, offset)?;
            return Ok(false);
        }
        self.service_id = first & 0x3F;
        Ok(true)
    }

    pub fn service_id_enum(&self) -> Option<Sid> {
        Sid::from_value(self.service_id)
    }

    pub fn is_negative(&self) -> bool {
        self.error_code != 0
    }
}

impl UdsResponse for RespBase {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        RespBase::initialize(self, response, offset)
    }

    fn base(&self) -> &RespBase {
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespSFBase {
    pub base: RespBase,
    pub sub_function: u16,
}

impl RespSFBase {
    pub fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.sub_function = u16::from(read_u8(response, offset)?);
        Ok(true)
    }
}

impl UdsResponse for RespSFBase {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        RespSFBase::initialize(self, response, offset)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespControlDtcSetting {
    pub base: RespSFBase,
}

impl RespControlDtcSetting {
    pub fn setting_type(&self) -> Option<DtcSettingType> {
        DtcSettingType::from_value(self.base.sub_function as u8)
    }
}

impl UdsResponse for RespControlDtcSetting {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        self.base.initialize(response, offset)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespData {
    pub base: RespBase,
    pub data: Vec<u8>,
}

impl UdsResponse for RespData {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.data = response[*offset..].to_vec();
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespDtcCount {
    pub base: RespSFBase,
    pub status_availability_mask: DtcStatusMask,
    pub format_identifier: u8,
    pub count: u16,
}

impl RespDtcCount {
    pub fn format_identifier_enum(&self) -> Option<DtcFormatIdentifier> {
        DtcFormatIdentifier::from_value(self.format_identifier)
    }
}

impl UdsResponse for RespDtcCount {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.status_availability_mask = DtcStatusMask(read_u8(response, offset)?);
        self.format_identifier = read_u8(response, offset)?;
        self.count = array_to_num(response, offset, 2)? as u16;
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespDtcRecords {
    pub base: RespSFBase,
    pub status_availability_mask: DtcStatusMask,
    pub dtcs: Vec<DtcAndStatusRecord>,
}

impl UdsResponse for RespDtcRecords {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.status_availability_mask = DtcStatusMask(read_u8(response, offset)?);
        while *offset < response.len() {
            self.dtcs.push(DtcAndStatusRecord::parse(response, offset)?);
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespDtcSeverityRecords {
    pub base: RespSFBase,
    pub severity_dtcs: u16,
    pub status_availability_mask: DtcStatusMask,
    pub dtcs: Vec<DtcAndStatusRecord>,
}

impl UdsResponse for RespDtcSeverityRecords {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.status_availability_mask = DtcStatusMask(read_u8(response, offset)?);
        while *offset < response.len() {
            self.dtcs.push(DtcAndStatusRecord::parse(response, offset)?);
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespDynamicallyDefine {
    pub base: RespSFBase,
    pub identifier: u16,
}

impl UdsResponse for RespDynamicallyDefine {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.identifier = array_to_num(response, offset, 2)? as u16;
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespIdentifier {
    pub base: RespBase,
    pub identifier: u16,
}

impl UdsResponse for RespIdentifier {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.identifier = array_to_num(response, offset, 2)? as u16;
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespLinkControl {
    pub base: RespSFBase,
}

impl RespLinkControl {
    pub fn link_control_type(&self) -> Option<LinkControlType> {
        LinkControlType::from_value(self.base.sub_function as u8)
    }
}

impl UdsResponse for RespLinkControl {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        self.base.initialize(response, offset)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespRequestUpDownload {
    pub base: RespBase,
    pub length_format_identifier: u8,
    pub max_number_of_block_length: u64,
}

impl UdsResponse for RespRequestUpDownload {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.length_format_identifier = read_u8(response, offset)? >> 4;
        self.max_number_of_block_length =
            array_to_num(response, offset, usize::from(self.length_format_identifier))?;
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespReset {
    pub base: RespSFBase,
    pub power_down_time: u8,
}

impl UdsResponse for RespReset {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        if *offset < response.len() {
            self.power_down_time = read_u8(response, offset)?;
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespResponseOnEvent {
    pub base: RespSFBase,
    pub number_of_identified_events: u8,
    pub event_window_time: u8,
}

impl RespResponseOnEvent {
    pub fn event_type(&self) -> Option<ResponseOnEventType> {
        ResponseOnEventType::from_value(self.base.sub_function as u8)
    }
}

impl UdsResponse for RespResponseOnEvent {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        if self.base.sub_function == ResponseOnEventType::ReportActivatedEvents.as_value() as u16 {
            self.number_of_identified_events = read_u8(response, offset)?;
            self.event_window_time = read_u8(response, offset)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespRoutineControl {
    pub base: RespSFBase,
    pub routine_identifier: u16,
    pub routine_info: u8,
    pub routine_status_record: Option<Vec<u8>>,
}

impl UdsResponse for RespRoutineControl {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.routine_identifier = array_to_num(response, offset, 2)? as u16;
        if *offset < response.len() {
            self.routine_info = read_u8(response, offset)?;
        }
        if response.len() > *offset {
            self.routine_status_record = Some(response[*offset..].to_vec());
            *offset = response.len();
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespSecurityAccess {
    pub base: RespSFBase,
    pub security_seed: Option<Vec<u8>>,
}

impl UdsResponse for RespSecurityAccess {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        if response.len() > *offset {
            self.security_seed = Some(response[*offset..].to_vec());
            *offset = response.len();
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespSession {
    pub base: RespSFBase,
    pub p2_server_max: u16,
    pub p2ex_server_max: u32,
}

impl UdsResponse for RespSession {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        if response.len() > *offset {
            self.p2_server_max = array_to_num(response, offset, 2)? as u16;
        }
        if response.len() > *offset {
            self.p2ex_server_max = (array_to_num(response, offset, 2)? * 10) as u32;
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespTimingParameter {
    pub base: RespSFBase,
    pub timing_parameters: Vec<u8>,
}

impl UdsResponse for RespTimingParameter {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.timing_parameters = response[*offset..].to_vec();
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespTransferData {
    pub base: RespBase,
    pub block_sequence_counter: u8,
    pub uploaded_data: Option<Vec<u8>>,
}

impl UdsResponse for RespTransferData {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.block_sequence_counter = read_u8(response, offset)?;
        if response.len() > *offset {
            self.uploaded_data = Some(response[*offset..].to_vec());
            *offset = response.len();
        }
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespWriteMemoryByAddress {
    pub base: RespBase,
    pub address_and_length_format: u8,
    pub memory_address: u64,
    pub memory_size: u64,
}

impl RespWriteMemoryByAddress {
    pub fn adr_len(&self) -> u8 {
        self.address_and_length_format >> 4
    }

    pub fn size_len(&self) -> u8 {
        self.address_and_length_format & 0x0F
    }
}

impl UdsResponse for RespWriteMemoryByAddress {
    fn initialize(&mut self, response: &[u8], offset: &mut usize) -> Result<bool> {
        if !self.base.initialize(response, offset)? {
            return Ok(false);
        }
        self.address_and_length_format = read_u8(response, offset)?;
        self.memory_address = array_to_num(response, offset, usize::from(self.adr_len()))?;
        self.memory_size = array_to_num(response, offset, usize::from(self.size_len()))?;
        Ok(true)
    }

    fn base(&self) -> &RespBase {
        &self.base
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub const NEG_RESPONSE_VALUE: u8 = 0x7F;

pub const POS_RESPONSE_MASK: u8 = 0x40;

#[async_trait]
pub trait UdsTransport {
    async fn send_request(&mut self, request: &[u8], response: Option<&mut Vec<u8>>) -> MsgState;

    fn max_msg_len(&self) -> u64 {
        4095
    }
}

pub trait SeedKeyProvider {
    fn compute_key_from_seed(
        &self,
        request_seed_sf: u8,
        variant: Option<&[u8]>,
        seed: &[u8],
    ) -> std::result::Result<Vec<u8>, i32>;
}

pub type ServiceResult<R> = Result<(MsgState, Option<R>)>;

fn outcome_code<R: UdsResponse>(state: MsgState, response: &Option<R>) -> u8 {
    if state != MsgState::Success {
        return state.to_code();
    }
    resp_error_code(response)
}

fn resp_error_code<R: UdsResponse>(response: &Option<R>) -> u8 {
    response.as_ref().map_or(0, |r| r.base().error_code)
}

pub struct UdsClient<T: UdsTransport> {
    pub transport: T,
    pub p2_client: u32,
    pub p3_client: u32,
    pub negative_response_codes: IndexMap<u8, String>,
    pub no_response_tester_present: bool,
    current_diag_session: u8,
}

impl<T: UdsTransport + Send> UdsClient<T> {
    pub fn new(transport: T) -> Self {
        let mut negative_response_codes = IndexMap::new();
        for code in NegRespCode::ALL {
            if *code != NegRespCode::Positive {
                negative_response_codes.insert(code.as_value(), code.name().to_string());
            }
        }
        UdsClient {
            transport,
            p2_client: 50,
            p3_client: 50,
            negative_response_codes,
            no_response_tester_present: false,
            current_diag_session: DiagnosticSessionType::Default.as_value(),
        }
    }

    pub fn overlay_error_codes(&mut self, codes: impl IntoIterator<Item = (u8, String)>) {
        for (code, text) in codes {
            self.negative_response_codes.insert(code, text);
        }
    }

    pub fn current_diag_session(&self) -> u8 {
        self.current_diag_session
    }

    pub fn get_neg_res_code(&self, error_code: u8) -> String {
        if let Some(text) = self.negative_response_codes.get(&error_code) {
            return text.clone();
        }
        if error_code >= 16 {
            match NegRespCode::from_value(error_code) {
                Some(code) => code.name().to_string(),
                None => error_code.to_string(),
            }
        } else {
            match MsgState::from_value(error_code as i8) {
                Some(state) => state.name().to_string(),
                None => error_code.to_string(),
            }
        }
    }

    fn post_process(&mut self, state: MsgState, response: Option<&RespBase>, sub_function: u8) {
        if state != MsgState::Success {
            return;
        }
        let Some(resp) = response else { return };
        if resp.error_code != 0 {
            return;
        }
        if resp.service_id == Sid::DiagnosticSessionControl.as_value() {
            self.current_diag_session = sub_function;
        } else if resp.service_id == Sid::ECUReset.as_value() {
            self.current_diag_session = DiagnosticSessionType::Default.as_value();
        }
    }

    fn parse_response<R: UdsResponse>(
        state: MsgState,
        request: &[u8],
        response: Option<&[u8]>,
    ) -> (MsgState, Option<R>) {
        if state != MsgState::Success {
            return (state, None);
        }
        let Some(resp) = response else {
            return (state, None);
        };
        let (Some(&first), Some(&req_sid)) = (resp.first(), request.first()) else {
            return (MsgState::ErrGeneric, None);
        };
        let mismatched = if first == NEG_RESPONSE_VALUE {
            resp.get(1) != Some(&req_sid)
        } else {
            first != (req_sid | POS_RESPONSE_MASK)
        };
        if mismatched {
            return (MsgState::ErrUnexpectedRSID, None);
        }
        let mut parsed = R::default();
        let mut offset = 0;
        match parsed.initialize(resp, &mut offset) {
            Ok(_) => (MsgState::Success, Some(parsed)),
            Err(_) => (MsgState::ErrGeneric, None),
        }
    }

    pub async fn execute_service_sf<R: UdsResponse>(
        &mut self,
        service: Sid,
        sub_function: u8,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> (MsgState, Option<R>) {
        let mut request = vec![
            service.as_value(),
            if await_response {
                sub_function
            } else {
                sub_function | 0x80
            },
        ];
        if let Some(data) = data {
            request.extend_from_slice(data);
        }
        let mut buf = Vec::new();
        let state = self
            .transport
            .send_request(&request, if await_response { Some(&mut buf) } else { None })
            .await;
        let (state, response) = Self::parse_response::<R>(
            state,
            &request,
            if await_response { Some(&buf) } else { None },
        );
        self.post_process(
            state,
            response.as_ref().map(UdsResponse::base),
            sub_function,
        );
        (state, response)
    }

    pub async fn execute_service<R: UdsResponse>(
        &mut self,
        service: Sid,
        data: Option<&[u8]>,
    ) -> (MsgState, Option<R>) {
        let mut request = vec![service.as_value()];
        if let Some(data) = data {
            request.extend_from_slice(data);
        }
        let mut buf = Vec::new();
        let state = self.transport.send_request(&request, Some(&mut buf)).await;
        let (state, response) = Self::parse_response::<R>(state, &request, Some(&buf));
        let sub_function = data.map_or(0xFF, |d| d.first().copied().unwrap_or(0xFF));
        self.post_process(
            state,
            response.as_ref().map(UdsResponse::base),
            sub_function,
        );
        (state, response)
    }

    pub async fn diagnostic_session_control(
        &mut self,
        sf: DiagnosticSessionType,
        await_response: bool,
    ) -> ServiceResult<RespSession> {
        Ok(self
            .execute_service_sf(
                Sid::DiagnosticSessionControl,
                sf.as_value(),
                None,
                await_response,
            )
            .await)
    }

    pub async fn ecu_reset(
        &mut self,
        sf: ResetType,
        await_response: bool,
    ) -> ServiceResult<RespReset> {
        Ok(self
            .execute_service_sf(Sid::ECUReset, sf.as_value(), None, await_response)
            .await)
    }

    pub async fn security_access_request_seed(
        &mut self,
        security_access_type: u8,
        security_access_data_record: Option<&[u8]>,
    ) -> ServiceResult<RespSecurityAccess> {
        Ok(self
            .execute_service_sf(
                Sid::SecurityAccess,
                security_access_type,
                security_access_data_record,
                true,
            )
            .await)
    }

    pub async fn security_access_send_key(
        &mut self,
        security_access_type: u8,
        security_key: &[u8],
        await_response: bool,
    ) -> ServiceResult<RespSecurityAccess> {
        Ok(self
            .execute_service_sf(
                Sid::SecurityAccess,
                security_access_type,
                Some(security_key),
                await_response,
            )
            .await)
    }

    pub async fn communication_control(
        &mut self,
        sf: CommunicationControlType,
        communication_type: u8,
        node_id: u16,
        await_response: bool,
    ) -> ServiceResult<RespSFBase> {
        let mut data = vec![communication_type];
        if matches!(
            sf,
            CommunicationControlType::EnableRxAndDisableTxWithEnhancedAddressInformation
                | CommunicationControlType::EnableRxAndTxWithEnhancedAddressInformation
        ) {
            data.extend_from_slice(&num_to_array(u64::from(node_id), 2));
        }
        Ok(self
            .execute_service_sf(
                Sid::CommunicationControl,
                sf.as_value(),
                Some(&data),
                await_response,
            )
            .await)
    }

    pub async fn tester_present(&mut self, await_response: bool) -> ServiceResult<RespSFBase> {
        Ok(self
            .execute_service_sf(Sid::TesterPresent, 0, None, await_response)
            .await)
    }

    pub async fn access_timing_service(
        &mut self,
        sf: TimingParameterAccessType,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespTimingParameter> {
        Ok(self
            .execute_service_sf(
                Sid::AccessTimingParameter,
                sf.as_value(),
                data,
                await_response,
            )
            .await)
    }

    pub async fn secured_data_transmission(&mut self, data: &[u8]) -> ServiceResult<RespBase> {
        Ok(self
            .execute_service(Sid::SecuredDataTransmission, Some(data))
            .await)
    }

    pub async fn control_dtc_setting(
        &mut self,
        sf: DtcSettingType,
        await_response: bool,
    ) -> ServiceResult<RespControlDtcSetting> {
        Ok(self
            .execute_service_sf(Sid::ControlDTCSetting, sf.as_value(), None, await_response)
            .await)
    }

    pub async fn response_on_event(
        &mut self,
        sf: ResponseOnEventType,
        event_window_time: u8,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespResponseOnEvent> {
        let mut payload = vec![event_window_time];
        if let Some(data) = data {
            payload.extend_from_slice(data);
        }
        Ok(self
            .execute_service_sf(
                Sid::ResponseOnEvent,
                sf.as_value(),
                Some(&payload),
                await_response,
            )
            .await)
    }

    pub async fn link_control(
        &mut self,
        sf: LinkControlType,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespLinkControl> {
        Ok(self
            .execute_service_sf(Sid::LinkControl, sf.as_value(), data, await_response)
            .await)
    }

    pub async fn read_data_by_identifier(
        &mut self,
        identifiers: &[u16],
    ) -> ServiceResult<RespData> {
        let mut data = Vec::with_capacity(identifiers.len() * 2);
        for id in identifiers {
            data.extend_from_slice(&num_to_array(u64::from(*id), 2));
        }
        Ok(self
            .execute_service(Sid::ReadDataByIdentifier, Some(&data))
            .await)
    }

    pub async fn read_memory_by_address(
        &mut self,
        address: i64,
        size: i64,
    ) -> ServiceResult<RespData> {
        let adr_len = byte_len(address);
        let size_len = byte_len(size);
        if size > self.transport.max_msg_len() as i64 - 1 {
            return Err(Error::Protocol(format!(
                "memory size {size} exceeds max message length {}",
                self.transport.max_msg_len() - 1
            )));
        }
        let mut data = vec![(size_len << 4) | adr_len];
        data.extend_from_slice(&num_to_array(address as u64, usize::from(adr_len)));
        data.extend_from_slice(&num_to_array(size as u64, usize::from(size_len)));
        Ok(self
            .execute_service(Sid::ReadMemoryByAddress, Some(&data))
            .await)
    }

    pub async fn read_scaling_data_by_identifier(
        &mut self,
        identifier: u16,
    ) -> ServiceResult<RespData> {
        Ok(self
            .execute_service(
                Sid::ReadScalingDataByIdentifier,
                Some(&num_to_array(u64::from(identifier), 2)),
            )
            .await)
    }

    pub async fn read_data_by_periodic_identifier(
        &mut self,
        sf: TransmissionModeType,
        identifier_ids: Option<&[u8]>,
    ) -> ServiceResult<RespData> {
        let mut data = vec![sf.as_value()];
        if let Some(ids) = identifier_ids {
            data.extend_from_slice(ids);
        }
        Ok(self
            .execute_service(Sid::ReadDataByPeriodicIdentifier, Some(&data))
            .await)
    }

    const DYN_ID_MIN: u16 = 0xF200;
    const DYN_ID_MAX: u16 = 0xF3FF;

    pub async fn dynamically_define_data_identifier_by_id(
        &mut self,
        dyn_id: u16,
        src: &[SrcDataIdentifier],
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        if !(Self::DYN_ID_MIN..=Self::DYN_ID_MAX).contains(&dyn_id) {
            return Err(Error::Protocol(format!(
                "dynamic identifier 0x{dyn_id:04X} out of range 0xF200..0xF3FF"
            )));
        }
        let mut data = num_to_array(u64::from(dyn_id), 2);
        for s in src {
            data.extend_from_slice(&s.to_array());
        }
        Ok(self
            .execute_service_sf(
                Sid::DynamicallyDefineDataIdentifier,
                DefinitionType::DefineByIdentifier.as_value(),
                Some(&data),
                await_response,
            )
            .await)
    }

    pub async fn dynamically_define_data_identifier_by_adr(
        &mut self,
        dyn_id: u16,
        src_adr_len: u8,
        src_size_len: u8,
        src: &[SrcDataMemory],
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        if !(Self::DYN_ID_MIN..=Self::DYN_ID_MAX).contains(&dyn_id) {
            return Err(Error::Protocol(format!(
                "dynamic identifier 0x{dyn_id:04X} out of range 0xF200..0xF3FF"
            )));
        }
        if src_size_len > 15 {
            return Err(Error::Protocol("srcSizeLen must be <= 15".to_string()));
        }
        if src_adr_len > 15 {
            return Err(Error::Protocol("srcAdrLen must be <= 15".to_string()));
        }
        let mut data = num_to_array(u64::from(dyn_id), 2);
        data.push((src_size_len << 4) | src_adr_len);
        for s in src {
            data.extend_from_slice(&s.to_array(src_adr_len, src_size_len));
        }
        Ok(self
            .execute_service_sf(
                Sid::DynamicallyDefineDataIdentifier,
                DefinitionType::DefineByMemoryAddress.as_value(),
                Some(&data),
                await_response,
            )
            .await)
    }

    pub async fn dynamically_clear_data_identifier(
        &mut self,
        dyn_id: u16,
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        Ok(self
            .execute_service_sf(
                Sid::DynamicallyDefineDataIdentifier,
                DefinitionType::ClearDynamicallyDefinedDataIdentifier.as_value(),
                Some(&num_to_array(u64::from(dyn_id), 2)),
                await_response,
            )
            .await)
    }

    pub async fn write_data_by_identifier(
        &mut self,
        identifier: u16,
        data: &[u8],
    ) -> ServiceResult<RespIdentifier> {
        let mut payload = num_to_array(u64::from(identifier), 2);
        payload.extend_from_slice(data);
        Ok(self
            .execute_service(Sid::WriteDataByIdentifier, Some(&payload))
            .await)
    }

    pub async fn write_memory_by_address(
        &mut self,
        address: i64,
        size: i64,
        data: &[u8],
    ) -> ServiceResult<RespWriteMemoryByAddress> {
        let adr_len = byte_len(address);
        let size_len = byte_len(size);
        if size > self.transport.max_msg_len() as i64 - 1 {
            return Err(Error::Protocol(format!(
                "memory size {size} exceeds max message length {}",
                self.transport.max_msg_len() - 1
            )));
        }
        let mut payload = vec![(size_len << 4) | adr_len];
        payload.extend_from_slice(&num_to_array(address as u64, usize::from(adr_len)));
        payload.extend_from_slice(&num_to_array(size as u64, usize::from(size_len)));
        payload.extend_from_slice(data);
        Ok(self
            .execute_service(Sid::WriteMemoryByAddress, Some(&payload))
            .await)
    }

    pub async fn clear_diagnostic_information(
        &mut self,
        group_of_dtc: u32,
    ) -> ServiceResult<RespBase> {
        Ok(self
            .execute_service(
                Sid::ClearDiagnosticInformation,
                Some(&num_to_array(u64::from(group_of_dtc), 3)),
            )
            .await)
    }

    /// ReportNumberOfDTCByStatusMask(0x01)/ReportNumberOfDTCBySeverityMaskRecord(0x07)/
    /// ReportNumberOfMirrorMemoryDTCByStatusMask(0x11)/ReportNumberOfEmissionsOBDDTCByStatusMask(0x12).
    pub async fn read_dtc_information_count(
        &mut self,
        sf: ReportDtcType,
        status_mask: DtcStatusMask,
        severity_mask: u16,
    ) -> ServiceResult<RespDtcCount> {
        if !matches!(
            sf,
            ReportDtcType::ReportNumberOfDTCByStatusMask
                | ReportDtcType::ReportNumberOfDTCBySeverityMaskRecord
                | ReportDtcType::ReportNumberOfMirrorMemoryDTCByStatusMask
                | ReportDtcType::ReportNumberOfEmissionsOBDDTCByStatusMask
        ) {
            return Err(Error::Protocol(format!(
                "invalid subfunction {} for DTC count request",
                sf.name()
            )));
        }
        let mut data = vec![status_mask.bits()];
        if severity_mask > 0 {
            data.extend_from_slice(&num_to_array(u64::from(severity_mask), 2));
        }
        Ok(self
            .execute_service_sf(Sid::ReadDTCInformation, sf.as_value(), Some(&data), true)
            .await)
    }

    pub async fn read_dtc_information_records(
        &mut self,
        sf: ReportDtcType,
        status_mask: DtcStatusMask,
    ) -> ServiceResult<RespDtcRecords> {
        let data: &[u8] = match sf {
            ReportDtcType::ReportDTCByStatusMask
            | ReportDtcType::ReportMirrorMemoryDTCByStatusMask
            | ReportDtcType::ReportNumberOfEmissionsOBDDTCByStatusMask => &[status_mask.bits()],
            ReportDtcType::ReportSupportedDTC
            | ReportDtcType::ReportFirstTestFailedDTC
            | ReportDtcType::ReportFirstConfirmedDTC
            | ReportDtcType::ReportMostRecentTestFailedDTC
            | ReportDtcType::ReportMostRecentConfirmedDTC
            | ReportDtcType::ReportDTCWithPermanentStatus => &[],
            _ => {
                return Err(Error::Protocol(format!(
                    "invalid subfunction {} for DTC records request",
                    sf.name()
                )))
            }
        };
        Ok(self
            .execute_service_sf(Sid::ReadDTCInformation, sf.as_value(), Some(data), true)
            .await)
    }

    /// snapshotRecordNumber=0, awaitResponse=true).
    pub async fn read_dtc_information(
        &mut self,
        sf: ReportDtcType,
        group_of_dtc: u32,
        snapshot_record_number: u8,
        await_response: bool,
    ) -> ServiceResult<RespSFBase> {
        let data: Vec<u8> = match sf {
            ReportDtcType::ReportDTCSnapshotRecordByDTCNumber => {
                let mut d = num_to_array(u64::from(group_of_dtc), 3);
                d.push(snapshot_record_number);
                d
            }
            ReportDtcType::ReportDTCSnapshotIdentification => Vec::new(),
            _ => {
                return Err(Error::Protocol(format!(
                    "invalid subfunction {} for DTC information request",
                    sf.name()
                )))
            }
        };
        Ok(self
            .execute_service_sf(
                Sid::ReadDTCInformation,
                sf.as_value(),
                Some(&data),
                await_response,
            )
            .await)
    }

    pub async fn input_output_control_by_identifier(
        &mut self,
        identifier: u16,
    ) -> ServiceResult<RespIdentifier> {
        Ok(self
            .execute_service(
                Sid::InputOutputControlByIdentifier,
                Some(&num_to_array(u64::from(identifier), 2)),
            )
            .await)
    }

    pub async fn request_download(
        &mut self,
        address: i64,
        size: i64,
        adr_and_len_fmt: u8,
        compression_method: u8,
        encryption_method: u8,
    ) -> ServiceResult<RespRequestUpDownload> {
        self.request_up_download(
            Sid::RequestDownload,
            address,
            size,
            adr_and_len_fmt,
            compression_method,
            encryption_method,
        )
        .await
    }

    pub async fn request_upload(
        &mut self,
        address: i64,
        size: i64,
        adr_and_len_fmt: u8,
        compression_method: u8,
        encryption_method: u8,
    ) -> ServiceResult<RespRequestUpDownload> {
        self.request_up_download(
            Sid::RequestUpload,
            address,
            size,
            adr_and_len_fmt,
            compression_method,
            encryption_method,
        )
        .await
    }

    async fn request_up_download(
        &mut self,
        sid: Sid,
        address: i64,
        size: i64,
        adr_and_len_fmt: u8,
        compression_method: u8,
        encryption_method: u8,
    ) -> ServiceResult<RespRequestUpDownload> {
        debug_assert!(matches!(sid, Sid::RequestUpload | Sid::RequestDownload));
        if compression_method > 15 {
            return Err(Error::Protocol(
                "compressionMethod must be <= 15".to_string(),
            ));
        }
        if encryption_method > 15 {
            return Err(Error::Protocol(
                "encryptionMethod must be <= 15".to_string(),
            ));
        }
        let fmt = if adr_and_len_fmt == 0 {
            (byte_len(size) << 4) | byte_len(address)
        } else {
            adr_and_len_fmt
        };
        let mut data = vec![(compression_method << 4) | encryption_method, fmt];
        data.extend_from_slice(&num_to_array(address as u64, usize::from(fmt & 0x0F)));
        data.extend_from_slice(&num_to_array(size as u64, usize::from(fmt >> 4)));
        Ok(self.execute_service(sid, Some(&data)).await)
    }

    pub async fn transfer_data(
        &mut self,
        block_sequence_counter: u8,
        data_to_download: Option<&[u8]>,
    ) -> ServiceResult<RespTransferData> {
        let mut data = vec![block_sequence_counter];
        if let Some(payload) = data_to_download {
            data.extend_from_slice(payload);
        }
        Ok(self.execute_service(Sid::TransferData, Some(&data)).await)
    }

    pub async fn request_transfer_exit(&mut self, data: Option<&[u8]>) -> ServiceResult<RespData> {
        Ok(self.execute_service(Sid::RequestTransferExit, data).await)
    }

    pub async fn request_file_transfer(
        &mut self,
        sf: ModeOfOperationType,
        file_path_and_name: &str,
    ) -> ServiceResult<RespBase> {
        let ascii: Vec<u8> = file_path_and_name
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect();
        let mut data = vec![sf.as_value()];
        data.extend_from_slice(&num_to_array(ascii.len() as u64, 2));
        data.extend_from_slice(&ascii);
        Ok(self
            .execute_service(Sid::RequestFileTransfer, Some(&data))
            .await)
    }

    pub async fn routine_control(
        &mut self,
        sf: RoutineControlType,
        routine_identifier: u16,
        routine_control_option_record: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespRoutineControl> {
        let mut data = num_to_array(u64::from(routine_identifier), 2);
        if let Some(record) = routine_control_option_record {
            data.extend_from_slice(record);
        }
        Ok(self
            .execute_service_sf(
                Sid::RoutineControl,
                sf.as_value(),
                Some(&data),
                await_response,
            )
            .await)
    }

    pub async fn unlock(
        &mut self,
        request_seed_sf: u8,
        sk: &dyn SeedKeyProvider,
        variant: Option<&[u8]>,
    ) -> Result<u8> {
        let (state, response) = self
            .security_access_request_seed(request_seed_sf, None)
            .await?;
        if state != MsgState::Success || resp_error_code(&response) != 0 {
            return Ok(outcome_code(state, &response));
        }
        let Some(seed) = response.and_then(|r| r.security_seed) else {
            return Ok(0);
        };
        let key = match sk.compute_key_from_seed(request_seed_sf, variant, &seed) {
            Ok(key) => key,
            Err(_) => return Ok(NegRespCode::GeneralReject.as_value()),
        };
        let (state, response) = self
            .security_access_send_key(request_seed_sf.wrapping_add(1), &key, true)
            .await?;
        Ok(outcome_code(state, &response))
    }

    pub async fn download(
        &mut self,
        address: i64,
        data: &[u8],
        adr_and_len_fmt: u8,
        omit_erase_mem: bool,
        mut progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u8> {
        if !omit_erase_mem {
            let mut record = num_to_array(address as u32 as u64, 4);
            record.extend_from_slice(&num_to_array(
                (address + data.len() as i64 - 1) as u32 as u64,
                4,
            ));
            let (state, response) = self
                .routine_control(
                    RoutineControlType::StartRoutine,
                    RoutineIdentifierType::EraseMemory.as_value(),
                    Some(&record),
                    true,
                )
                .await?;
            if state != MsgState::Success || resp_error_code(&response) != 0 {
                return Ok(outcome_code(state, &response));
            }
        }
        let (state, response) = self
            .request_download(address, data.len() as i64, adr_and_len_fmt, 0, 0)
            .await?;
        if state != MsgState::Success || resp_error_code(&response) != 0 {
            return Ok(outcome_code(state, &response));
        }
        let max_block = response.map_or(0, |r| r.max_number_of_block_length);
        if max_block < 2 {
            return Err(Error::Protocol(format!(
                "maxNumberOfBlockLength {max_block} too small for download"
            )));
        }
        let chunk = (max_block - 2) as i64;
        let total = data.len() as i64;
        let notify_step = std::cmp::max(1, total / 100);
        let mut sent = 0i64;
        let mut block_seq = 1u8;
        let mut early: Option<u8> = None;
        while sent < total {
            let n = std::cmp::min(chunk, total - sent);
            let part = &data[sent as usize..(sent + n) as usize];
            let (state, response) = self.transfer_data(block_seq, Some(part)).await?;
            if state != MsgState::Success || resp_error_code(&response) != 0 {
                early = Some(outcome_code(state, &response));
                break;
            }
            let resp = response.expect("success implies parsed response");
            if resp.block_sequence_counter != block_seq {
                early = Some(NegRespCode::WrongBlockSequenceCounter.as_value());
                break;
            }
            block_seq = block_seq.wrapping_add(1);
            sent += n;
            if let Some(cb) = progress.as_mut() {
                if sent > notify_step && cb((sent as f64 * 100.0 / total as f64) as u32) {
                    early = Some(NegRespCode::UserCancelled.as_value());
                    break;
                }
            }
        }
        let (exit_state, exit_resp) = self.request_transfer_exit(None).await?;
        if let Some(code) = early {
            return Ok(code);
        }
        let mut result = 0u8;
        if exit_state != MsgState::Success || resp_error_code(&exit_resp) != 0 {
            result = outcome_code(exit_state, &exit_resp);
        }
        if let Some(cb) = progress.as_mut() {
            cb(100);
        }
        Ok(result)
    }

    pub async fn upload(
        &mut self,
        address: i64,
        len: i64,
        adr_and_len_fmt: u8,
        mut progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<(u8, Option<Vec<u8>>)> {
        if len < 0 {
            return Err(Error::Protocol(format!("invalid upload length {len}")));
        }
        let (state, response) = self
            .request_upload(address, len, adr_and_len_fmt, 0, 0)
            .await?;
        if state != MsgState::Success || resp_error_code(&response) != 0 {
            return Ok((outcome_code(state, &response), None));
        }
        let notify_step = std::cmp::max(1, len / 100);
        let mut received = 0i64;
        let mut block_seq = 1u8;
        let mut data = vec![0u8; len as usize];
        let mut early: Option<u8> = None;
        while received < len {
            let (state, response) = self.transfer_data(block_seq, None).await?;
            if state != MsgState::Success || resp_error_code(&response) != 0 {
                early = Some(outcome_code(state, &response));
                break;
            }
            let resp = response.expect("success implies parsed response");
            if resp.block_sequence_counter != block_seq {
                early = Some(NegRespCode::WrongBlockSequenceCounter.as_value());
                break;
            }
            let block = resp.uploaded_data.unwrap_or_default();
            if block.is_empty() || received + block.len() as i64 > len {
                return Err(Error::Protocol("upload block length mismatch".to_string()));
            }
            data[received as usize..received as usize + block.len()].copy_from_slice(&block);
            block_seq = block_seq.wrapping_add(1);
            received += block.len() as i64;
            if let Some(cb) = progress.as_mut() {
                if received > notify_step && cb((received as f64 * 100.0 / len as f64) as u32) {
                    early = Some(NegRespCode::UserCancelled.as_value());
                    break;
                }
            }
        }
        let (exit_state, exit_resp) = self.request_transfer_exit(None).await?;
        if let Some(code) = early {
            return Ok((code, Some(data)));
        }
        let mut result = 0u8;
        if exit_state != MsgState::Success || resp_error_code(&exit_resp) != 0 {
            result = outcome_code(exit_state, &exit_resp);
        }
        if let Some(cb) = progress.as_mut() {
            cb(100);
        }
        Ok((result, Some(data)))
    }

    pub async fn write_memory(
        &mut self,
        address: i64,
        data: &[u8],
        max_len: i64,
        mut progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u8> {
        let chunk = std::cmp::min(max_len, self.transport.max_msg_len() as i64 - 1);
        if chunk <= 0 {
            return Err(Error::Protocol(format!("invalid write chunk size {chunk}")));
        }
        let total = data.len() as i64;
        let mut sent = 0i64;
        while sent < total {
            let n = std::cmp::min(total - sent, chunk);
            let (state, response) = self
                .write_memory_by_address(
                    address + sent,
                    n,
                    &data[sent as usize..(sent + n) as usize],
                )
                .await?;
            if state != MsgState::Success || resp_error_code(&response) != 0 {
                return Ok(outcome_code(state, &response));
            }
            sent += n;
            if let Some(cb) = progress.as_mut() {
                if cb((sent as f64 * 100.0 / total as f64) as u32) {
                    return Ok(NegRespCode::UserCancelled.as_value());
                }
            }
        }
        if let Some(cb) = progress.as_mut() {
            cb(100);
        }
        Ok(0)
    }

    pub async fn read_memory(
        &mut self,
        address: i64,
        len: i64,
        max_len: i64,
        mut progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<(u8, Option<Vec<u8>>)> {
        if len < 0 {
            return Err(Error::Protocol(format!("invalid read length {len}")));
        }
        let chunk = std::cmp::min(max_len, self.transport.max_msg_len() as i64 - 1);
        if chunk <= 0 {
            return Err(Error::Protocol(format!("invalid read chunk size {chunk}")));
        }
        let mut data: Option<Vec<u8>> = None;
        let mut received = 0i64;
        while received < len {
            let size = std::cmp::min(len - received, chunk);
            let (state, response) = self
                .read_memory_by_address(address + received, size)
                .await?;
            if state != MsgState::Success || resp_error_code(&response) != 0 {
                return Ok((outcome_code(state, &response), data));
            }
            let resp = response.expect("success implies parsed response");
            if resp.data.is_empty() || received + resp.data.len() as i64 > len {
                return Err(Error::Protocol("read block length mismatch".to_string()));
            }
            let buf = data.get_or_insert_with(|| vec![0u8; len as usize]);
            buf[received as usize..received as usize + resp.data.len()].copy_from_slice(&resp.data);
            received += resp.data.len() as i64;
            if let Some(cb) = progress.as_mut() {
                if cb((received as f64 * 100.0 / len as f64) as u32) {
                    return Ok((NegRespCode::UserCancelled.as_value(), data));
                }
            }
        }
        if let Some(cb) = progress.as_mut() {
            cb(100);
        }
        Ok((0, data))
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UdsFrame {
    pub id: u32,
    pub data: Vec<u8>,
    pub is_master_frame: bool,
}

impl UdsFrame {
    pub fn new(can_id: u32, data: Vec<u8>, is_master_frame: bool) -> Self {
        UdsFrame {
            id: can_id,
            data,
            is_master_frame,
        }
    }

    pub fn len(&self) -> u32 {
        self.data.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn is_error(&self) -> bool {
        !self.is_master_frame && self.data.first() == Some(&NEG_RESPONSE_VALUE)
    }

    pub fn service_id(&self) -> u8 {
        if self.is_master_frame {
            return self.data.first().copied().unwrap_or(0);
        }
        if self.data.first() == Some(&NEG_RESPONSE_VALUE) {
            return self.data.get(1).copied().unwrap_or(0);
        }
        self.data.first().copied().unwrap_or(0) & 0xBF
    }

    pub fn service_id_enum(&self) -> Option<Sid> {
        Sid::from_value(self.service_id())
    }

    pub fn service_str(&self) -> String {
        let mut s = match self.service_id_enum() {
            Some(sid) => sid.name().to_string(),
            None => self.service_id().to_string(),
        };
        if self.is_error() {
            if let Some(&code) = self.data.get(2) {
                let name = match NegRespCode::from_value(code) {
                    Some(c) => c.name().to_string(),
                    None => code.to_string(),
                };
                s.push_str(&format!("({name})"));
            }
        }
        s
    }

    pub fn subfunction(&self) -> u8 {
        let no_subfunction = match self.service_id_enum() {
            Some(sid) => !has_subfunction(sid),
            None => true,
        };
        if no_subfunction || self.data.first() == Some(&NEG_RESPONSE_VALUE) {
            return u8::MAX;
        }
        self.data.get(1).copied().unwrap_or(0) & 0x7F
    }

    pub fn subfunction_str(&self) -> String {
        let sf = self.subfunction();
        if sf != u8::MAX {
            format!("{sf:02}")
        } else {
            String::new()
        }
    }
}

// ---------------------------------------------------------------------------
//
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingUdsClient;
    use async_trait::async_trait;
    use std::collections::VecDeque;

    #[test]
    fn num_array_big_endian() {
        // numToArray(0x1234,3)=[0,18,52];arrayToNum([0x12,0x34])=4660;[0x12,0x34,0x56]→1193046
        assert_eq!(num_to_array(0x1234, 2), vec![0x12, 0x34]);
        assert_eq!(num_to_array(0x1234_5678, 4), vec![0x12, 0x34, 0x56, 0x78]);
        assert_eq!(num_to_array(0x1234, 3), vec![0x00, 0x12, 0x34]);
        assert_eq!(num_to_array(0xFF, 0), Vec::<u8>::new());

        let mut off = 0;
        assert_eq!(
            array_to_num(&[0x12, 0x34, 0x56], &mut off, 2).unwrap(),
            4660
        );
        assert_eq!(off, 2);
        let mut off = 0;
        assert_eq!(
            array_to_num(&[0x12, 0x34, 0x56], &mut off, 3).unwrap(),
            1_193_046
        );
        let mut off = 2;
        assert!(array_to_num(&[0x12, 0x34, 0x56], &mut off, 2).is_err());
    }

    #[test]
    fn byte_len_thresholds() {
        assert_eq!(byte_len(0xFF), 1);
        assert_eq!(byte_len(0x100), 2);
        assert_eq!(byte_len(0xFFFF), 2);
        assert_eq!(byte_len(0x1_0000), 4);
        assert_eq!(byte_len(0xFFFF_FFFF), 4);
        assert_eq!(byte_len(0x1_0000_0000), 8);
        assert_eq!(byte_len(-1), 1);
    }

    #[test]
    fn enums_roundtrip() {
        assert_eq!(Sid::from_value(0x22), Some(Sid::ReadDataByIdentifier));
        assert_eq!(Sid::from_value(0x05), None);
        assert_eq!(Sid::ControlDTCSetting.as_value(), 0x85);
        assert_eq!(Sid::ALL.len(), 26);
        assert_eq!(NegRespCode::ALL.len(), 43);
        assert_eq!(
            NegRespCode::from_value(0x31),
            Some(NegRespCode::RequestOutOfRange)
        );
        assert_eq!(NegRespCode::RequestOutOfRange.name(), "RequestOutOfRange");
        assert_eq!(MsgState::Success.to_code(), 0);
        assert_eq!(MsgState::ErrUnexpectedRSID.to_code(), 9);
        assert_eq!(MsgState::from_value(-4), Some(MsgState::PendingSnd));
        assert_eq!(RoutineIdentifierType::EraseMemory.as_value(), 0xFF00);
    }

    #[test]
    fn dtc_status_mask_flags() {
        let mask = DtcStatusMask::TEST_FAILED | DtcStatusMask::CONFIRMED_DTC;
        assert_eq!(mask.bits(), 0x09);
        assert!(mask.contains(DtcStatusMask::TEST_FAILED));
        assert!(!mask.contains(DtcStatusMask::PENDING_DTC));
        assert_eq!(DtcStatusMask::WARNING_INDICATOR_REQUESTED.bits(), 0x80);
    }

    #[test]
    fn has_subfunction_table() {
        assert!(has_subfunction(Sid::DiagnosticSessionControl));
        assert!(!has_subfunction(Sid::ReadDataByIdentifier));
        assert!(has_subfunction(Sid::RoutineControl));
        assert!(!has_subfunction(Sid::TransferData));
        assert!(has_subfunction(Sid::ReadDTCInformation));
        assert!(!has_subfunction(Sid::RequestFileTransfer));
        for sid in Sid::ALL {
            let _ = has_subfunction(*sid);
        }
    }

    #[test]
    fn src_data_identifier_encode() {
        let s = SrcDataIdentifier::new(0xF190, 1, 4);
        assert_eq!(s.to_array(), vec![0xF1, 0x90, 0x01, 0x04]);
    }

    #[test]
    fn src_data_memory_encode() {
        let m = SrcDataMemory::new(0x12_3456, 0x100);
        assert_eq!(m.to_array(3, 2), vec![0x12, 0x34, 0x56, 0x01, 0x00]);
    }

    fn parse<R: UdsResponse>(bytes: &[u8]) -> Result<(bool, R)> {
        let mut r = R::default();
        let mut off = 0;
        let ok = r.initialize(bytes, &mut off)?;
        Ok((ok, r))
    }

    #[test]
    fn resp_base_positive_and_negative() {
        let (ok, r) = parse::<RespBase>(&[0x62, 0xF1, 0x90]).unwrap();
        assert!(ok);
        assert_eq!(r.service_id, 0x22);
        assert_eq!(r.service_id_enum(), Some(Sid::ReadDataByIdentifier));
        assert_eq!(r.error_code, 0);
        assert!(!r.is_negative());
        let (ok, r) = parse::<RespBase>(&[0x7F, 0x22, 0x31]).unwrap();
        assert!(!ok);
        assert_eq!(r.service_id, 0x22);
        assert_eq!(r.error_code, 0x31);
        assert!(r.is_negative());
    }

    #[test]
    fn resp_data_keeps_did() {
        let (ok, r) = parse::<RespData>(&[0x62, 0xF1, 0x90, 0x01, 0x02]).unwrap();
        assert!(ok);
        assert_eq!(r.data, vec![0xF1, 0x90, 0x01, 0x02]);
        let (ok, r) = parse::<RespData>(&[0x7F, 0x22, 0x31]).unwrap();
        assert!(!ok);
        assert!(r.data.is_empty());
    }

    #[test]
    fn resp_session_timing() {
        let (ok, r) = parse::<RespSession>(&[0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]).unwrap();
        assert!(ok);
        assert_eq!(r.base.sub_function, 3);
        assert_eq!(r.p2_server_max, 50);
        assert_eq!(r.p2ex_server_max, 5000);
        let (ok, r) = parse::<RespSession>(&[0x50, 0x03]).unwrap();
        assert!(ok);
        assert_eq!(r.p2_server_max, 0);
        assert_eq!(r.p2ex_server_max, 0);
        assert!(parse::<RespSession>(&[0x50, 0x03, 0x32]).is_err());
    }

    #[test]
    fn resp_reset_power_down_time() {
        let (ok, r) = parse::<RespReset>(&[0x51, 0x04, 0x0A]).unwrap();
        assert!(ok);
        assert_eq!(r.base.sub_function, 4);
        assert_eq!(r.power_down_time, 10);
        let (ok, r) = parse::<RespReset>(&[0x51, 0x01]).unwrap();
        assert!(ok);
        assert_eq!(r.power_down_time, 0);
    }

    #[test]
    fn resp_security_access_seed() {
        let (ok, r) = parse::<RespSecurityAccess>(&[0x67, 0x01, 0xAA, 0xBB]).unwrap();
        assert!(ok);
        assert_eq!(r.security_seed, Some(vec![0xAA, 0xBB]));
        let (ok, r) = parse::<RespSecurityAccess>(&[0x67, 0x02]).unwrap();
        assert!(ok);
        assert_eq!(r.security_seed, None);
    }

    #[test]
    fn resp_control_dtc_setting_sid_quirk() {
        let (ok, r) = parse::<RespControlDtcSetting>(&[0xC5, 0x01]).unwrap();
        assert!(ok);
        assert_eq!(r.base.base.service_id, 0x05);
        assert_eq!(r.base.base.service_id_enum(), None);
        assert_eq!(r.setting_type(), Some(DtcSettingType::On));
    }

    #[test]
    fn resp_timing_parameter() {
        let (ok, r) = parse::<RespTimingParameter>(&[0xC3, 0x03, 0xAA, 0xBB]).unwrap();
        assert!(ok);
        assert_eq!(r.base.base.service_id, 0x03);
        assert_eq!(r.base.sub_function, 3);
        assert_eq!(r.timing_parameters, vec![0xAA, 0xBB]);
    }

    #[test]
    fn resp_response_on_event() {
        let (ok, r) = parse::<RespResponseOnEvent>(&[0xC6, 0x04, 0x02, 0x03]).unwrap();
        assert!(ok);
        assert_eq!(
            r.event_type(),
            Some(ResponseOnEventType::ReportActivatedEvents)
        );
        assert_eq!(r.number_of_identified_events, 2);
        assert_eq!(r.event_window_time, 3);
        let (ok, r) = parse::<RespResponseOnEvent>(&[0xC6, 0x01, 0x02, 0x03]).unwrap();
        assert!(!ok);
        assert_eq!(r.number_of_identified_events, 0);
        assert_eq!(r.event_window_time, 0);
    }

    #[test]
    fn resp_link_control() {
        let (ok, r) = parse::<RespLinkControl>(&[0xC7, 0x02]).unwrap();
        assert!(ok);
        assert_eq!(r.base.base.service_id, 0x07);
        assert_eq!(
            r.link_control_type(),
            Some(LinkControlType::VerifyModeTransitionWithSpecificParameter)
        );
    }

    #[test]
    fn resp_dtc_count() {
        let (ok, r) = parse::<RespDtcCount>(&[0x59, 0x01, 0xFF, 0x00, 0x00, 0x05]).unwrap();
        assert!(ok);
        assert_eq!(r.status_availability_mask.bits(), 0xFF);
        assert_eq!(r.format_identifier, 0);
        assert_eq!(
            r.format_identifier_enum(),
            Some(DtcFormatIdentifier::SaeJ2012DaDtcFormat00)
        );
        assert_eq!(r.count, 5);
    }

    #[test]
    fn resp_dtc_records() {
        let (ok, r) = parse::<RespDtcRecords>(&[
            0x59, 0x02, 0xFF, 0x12, 0x34, 0x56, 0x0A, 0xAB, 0xCD, 0xEF, 0x80,
        ])
        .unwrap();
        assert!(ok);
        assert_eq!(r.status_availability_mask.bits(), 0xFF);
        assert_eq!(r.dtcs.len(), 2);
        assert_eq!(r.dtcs[0].dtc, 0x12_3456);
        assert_eq!(r.dtcs[0].status_of_dtc.bits(), 0x0A);
        assert_eq!(r.dtcs[1].dtc, 0xAB_CDEF);
        assert_eq!(r.dtcs[1].status_of_dtc.bits(), 0x80);
        assert!(parse::<RespDtcRecords>(&[0x59, 0x02, 0xFF, 0x12]).is_err());
    }

    #[test]
    fn resp_dtc_severity_records_unparsed_field() {
        let (ok, r) =
            parse::<RespDtcSeverityRecords>(&[0x59, 0x08, 0xFF, 0x12, 0x34, 0x56, 0x0A]).unwrap();
        assert!(ok);
        assert_eq!(r.severity_dtcs, 0);
        assert_eq!(r.dtcs.len(), 1);
        assert_eq!(r.dtcs[0].dtc, 0x12_3456);
    }

    #[test]
    fn resp_dynamically_define() {
        let (ok, r) = parse::<RespDynamicallyDefine>(&[0x6C, 0x01, 0xF2, 0x00]).unwrap();
        assert!(ok);
        assert_eq!(r.identifier, 0xF200);
    }

    #[test]
    fn resp_identifier() {
        let (ok, r) = parse::<RespIdentifier>(&[0x6E, 0xF1, 0x90]).unwrap();
        assert!(ok);
        assert_eq!(r.identifier, 0xF190);
    }

    #[test]
    fn resp_request_up_download() {
        let (ok, r) = parse::<RespRequestUpDownload>(&[0x74, 0x20, 0x12, 0x34]).unwrap();
        assert!(ok);
        assert_eq!(r.length_format_identifier, 2);
        assert_eq!(r.max_number_of_block_length, 4660);
        let (ok, r) =
            parse::<RespRequestUpDownload>(&[0x74, 0x40, 0x00, 0x00, 0x10, 0x00]).unwrap();
        assert!(ok);
        assert_eq!(r.length_format_identifier, 4);
        assert_eq!(r.max_number_of_block_length, 4096);
    }

    #[test]
    fn resp_transfer_data() {
        let (ok, r) = parse::<RespTransferData>(&[0x76, 0x05, 0xAA, 0xBB]).unwrap();
        assert!(ok);
        assert_eq!(r.block_sequence_counter, 5);
        assert_eq!(r.uploaded_data, Some(vec![0xAA, 0xBB]));
        let (ok, r) = parse::<RespTransferData>(&[0x76, 0x05]).unwrap();
        assert!(ok);
        assert_eq!(r.uploaded_data, None);
    }

    #[test]
    fn resp_routine_control() {
        let (ok, r) = parse::<RespRoutineControl>(&[0x71, 0x01, 0xFF, 0x00, 0x12, 0x34]).unwrap();
        assert!(ok);
        assert_eq!(r.base.sub_function, 1);
        assert_eq!(r.routine_identifier, 0xFF00);
        assert_eq!(r.routine_info, 0x12);
        assert_eq!(r.routine_status_record, Some(vec![0x34]));
        let (ok, r) = parse::<RespRoutineControl>(&[0x71, 0x01, 0xFF, 0x00]).unwrap();
        assert!(ok);
        assert_eq!(r.routine_info, 0);
        assert_eq!(r.routine_status_record, None);
    }

    #[test]
    fn resp_write_memory_by_address() {
        let (ok, r) = parse::<RespWriteMemoryByAddress>(&[0x7D, 0x21, 0xAA, 0xBB, 0x05]).unwrap();
        assert!(ok);
        assert_eq!(r.adr_len(), 2);
        assert_eq!(r.size_len(), 1);
        assert_eq!(r.memory_address, 0xAABB);
        assert_eq!(r.memory_size, 5);
    }

    #[test]
    fn resp_sf_base_and_negative() {
        let (ok, r) = parse::<RespSFBase>(&[0x7E, 0x00]).unwrap();
        assert!(ok);
        assert_eq!(r.base.service_id_enum(), Some(Sid::TesterPresent));
        assert_eq!(r.sub_function, 0);
        let (ok, r) = parse::<RespSFBase>(&[0x7F, 0x3E, 0x78]).unwrap();
        assert!(!ok);
        assert_eq!(r.base.service_id, 0x3E);
        assert_eq!(r.base.error_code, 0x78);
        assert_eq!(r.sub_function, 0);
    }

    #[test]
    fn uds_frame_request() {
        let f = UdsFrame::new(0x7E0, vec![0x22, 0xF1, 0x90], true);
        assert_eq!(f.len(), 3);
        assert!(!f.is_empty());
        assert_eq!(f.service_id_enum(), Some(Sid::ReadDataByIdentifier));
        assert_eq!(f.subfunction(), 0xFF);
        assert_eq!(f.subfunction_str(), "");
        assert!(!f.is_error());
        assert_eq!(f.service_str(), "ReadDataByIdentifier");
    }

    #[test]
    fn uds_frame_positive_response() {
        let f = UdsFrame::new(0x7E8, vec![0x62, 0xF1, 0x90, 0x01], false);
        assert_eq!(f.service_id_enum(), Some(Sid::ReadDataByIdentifier));
        assert!(!f.is_error());
    }

    #[test]
    fn uds_frame_negative_response() {
        let f = UdsFrame::new(0x7E8, vec![0x7F, 0x22, 0x31], false);
        assert!(f.is_error());
        assert_eq!(f.service_id_enum(), Some(Sid::ReadDataByIdentifier));
        assert_eq!(f.subfunction(), 0xFF);
        assert_eq!(f.service_str(), "ReadDataByIdentifier(RequestOutOfRange)");
    }

    #[test]
    fn uds_frame_subfunction_d2_format() {
        let f = UdsFrame::new(0x7E0, vec![0x10, 0x83], true);
        assert_eq!(f.service_id_enum(), Some(Sid::DiagnosticSessionControl));
        assert_eq!(f.subfunction(), 3);
        assert_eq!(f.subfunction_str(), "03");
        let f = UdsFrame::new(0x7E8, vec![0x50, 0x83], false);
        assert_eq!(f.subfunction(), 3);
        assert_eq!(f.subfunction_str(), "03");
    }

    #[test]
    fn uds_frame_sid_above_0x80() {
        let f = UdsFrame::new(0x7E0, vec![0x85, 0x81], true);
        assert_eq!(f.service_id_enum(), Some(Sid::ControlDTCSetting));
        assert_eq!(f.subfunction(), 1);
        assert_eq!(f.subfunction_str(), "01");
        let f = UdsFrame::new(0x7E8, vec![0xC5, 0x01], false);
        assert_eq!(f.service_id_enum(), Some(Sid::ControlDTCSetting));
        assert_eq!(f.subfunction(), 1);
        assert!(!f.is_error());
    }

    struct MockTransport {
        requests: Vec<Vec<u8>>,
        script: VecDeque<(MsgState, Vec<u8>)>,
        max_len: u64,
    }

    #[async_trait]
    impl UdsTransport for MockTransport {
        async fn send_request(
            &mut self,
            request: &[u8],
            response: Option<&mut Vec<u8>>,
        ) -> MsgState {
            self.requests.push(request.to_vec());
            let (state, bytes) = self
                .script
                .pop_front()
                .unwrap_or((MsgState::Success, Vec::new()));
            if let Some(buf) = response {
                *buf = bytes;
            }
            state
        }

        fn max_msg_len(&self) -> u64 {
            self.max_len
        }
    }

    #[cfg(feature = "blocking")]
    fn client(script: Vec<(MsgState, Vec<u8>)>) -> BlockingUdsClient<MockTransport> {
        BlockingUdsClient::new(UdsClient::new(MockTransport {
            requests: Vec::new(),
            script: script.into(),
            max_len: 4095,
        }))
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_diagnostic_session_control() {
        let mut c = client(vec![(
            MsgState::Success,
            vec![0x50, 0x03, 0x00, 0x32, 0x01, 0xF4],
        )]);
        let (state, resp) = c
            .diagnostic_session_control(DiagnosticSessionType::Extended, true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(c.0.transport.requests[0], vec![0x10, 0x03]);
        let resp = resp.unwrap();
        assert_eq!(resp.p2_server_max, 50);
        assert_eq!(resp.p2ex_server_max, 5000);
        assert_eq!(c.current_diag_session(), 3);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_diagnostic_session_control_no_response() {
        let mut c = client(vec![]);
        let (state, resp) = c
            .diagnostic_session_control(DiagnosticSessionType::Programming, false)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(c.0.transport.requests[0], vec![0x10, 0x82]);
        assert!(resp.is_none());
        assert_eq!(c.current_diag_session(), 1);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_ecu_reset_restores_default_session() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
            (MsgState::Success, vec![0x51, 0x04, 0x0A]),
        ]);
        c.diagnostic_session_control(DiagnosticSessionType::Extended, true)
            .unwrap();
        assert_eq!(c.current_diag_session(), 3);
        let (state, resp) = c
            .ecu_reset(ResetType::EnableRapidPowerShutDown, true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(c.0.transport.requests[1], vec![0x11, 0x04]);
        assert_eq!(resp.unwrap().power_down_time, 10);
        assert_eq!(c.current_diag_session(), 1);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_security_access_seed_and_key() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x67, 0x01, 0xAA, 0xBB]),
            (MsgState::Success, vec![0x67, 0x02]),
        ]);
        let (_, resp) = c.security_access_request_seed(0x01, None).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x27, 0x01]);
        assert_eq!(resp.unwrap().security_seed, Some(vec![0xAA, 0xBB]));
        let (_, resp) = c
            .security_access_send_key(0x02, &[0x11, 0x22], true)
            .unwrap();
        assert_eq!(c.0.transport.requests[1], vec![0x27, 0x02, 0x11, 0x22]);
        assert!(resp.unwrap().security_seed.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_communication_control_node_id_only_for_enhanced() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x68, 0x00]),
            (MsgState::Success, vec![0x68, 0x05]),
        ]);
        c.communication_control(CommunicationControlType::EnableRxAndTx, 0x01, 0x1234, true)
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x28, 0x00, 0x01]);
        c.communication_control(
            CommunicationControlType::EnableRxAndTxWithEnhancedAddressInformation,
            0x01,
            0x1234,
            true,
        )
        .unwrap();
        assert_eq!(
            c.0.transport.requests[1],
            vec![0x28, 0x05, 0x01, 0x12, 0x34]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_tester_present() {
        let mut c = client(vec![(MsgState::Success, vec![0x7E, 0x00])]);
        let (_, resp) = c.tester_present(true).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x3E, 0x00]);
        assert_eq!(resp.unwrap().sub_function, 0);
        let mut c = client(vec![]);
        c.tester_present(false).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x3E, 0x80]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_access_timing_service() {
        let mut c = client(vec![(MsgState::Success, vec![0xC3, 0x03, 0xAA])]);
        let (_, resp) = c
            .access_timing_service(
                TimingParameterAccessType::ReadCurrentlyActiveTimingParameters,
                None,
                true,
            )
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x83, 0x03]);
        assert_eq!(resp.unwrap().timing_parameters, vec![0xAA]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_secured_data_transmission() {
        let mut c = client(vec![(MsgState::Success, vec![0xC4, 0x99])]);
        let (_, resp) = c.secured_data_transmission(&[0x01, 0x02]).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x84, 0x01, 0x02]);
        assert_eq!(resp.unwrap().service_id, 0x04);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_control_dtc_setting() {
        let mut c = client(vec![(MsgState::Success, vec![0xC5, 0x02])]);
        let (_, resp) = c.control_dtc_setting(DtcSettingType::Off, true).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x85, 0x02]);
        assert_eq!(resp.unwrap().setting_type(), Some(DtcSettingType::Off));
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_response_on_event() {
        let mut c = client(vec![(MsgState::Success, vec![0xC6, 0x04, 0x02, 0x03])]);
        let (_, resp) = c
            .response_on_event(
                ResponseOnEventType::ReportActivatedEvents,
                0x0A,
                Some(&[0x01]),
                true,
            )
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x86, 0x04, 0x0A, 0x01]);
        assert_eq!(resp.unwrap().number_of_identified_events, 2);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_link_control() {
        let mut c = client(vec![(MsgState::Success, vec![0xC7, 0x01])]);
        let (_, resp) = c
            .link_control(
                LinkControlType::VerifyModeTransitionWithFixedParameter,
                Some(&[0x24]),
                true,
            )
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x87, 0x01, 0x24]);
        assert_eq!(
            resp.unwrap().link_control_type(),
            Some(LinkControlType::VerifyModeTransitionWithFixedParameter)
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_data_by_identifier() {
        let mut c = client(vec![(MsgState::Success, vec![0x62, 0xF1, 0x90, 0x41])]);
        let (_, resp) = c.read_data_by_identifier(&[0xF190, 0xF186]).unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x22, 0xF1, 0x90, 0xF1, 0x86]
        );
        assert_eq!(resp.unwrap().data, vec![0xF1, 0x90, 0x41]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_memory_by_address() {
        let mut c = client(vec![(MsgState::Success, vec![0x63, 0xAA, 0xBB])]);
        let (_, resp) = c.read_memory_by_address(0x1234, 0x10).unwrap();
        // byte_len(addr)=2, byte_len(size)=1 → fmt = 0x12
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x23, 0x12, 0x12, 0x34, 0x10]
        );
        assert_eq!(resp.unwrap().data, vec![0xAA, 0xBB]);
        let mut c = client(vec![]);
        assert!(c.read_memory_by_address(0x1234, 4095).is_err());
        assert!(c.0.transport.requests.is_empty());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_scaling_data_by_identifier() {
        let mut c = client(vec![(MsgState::Success, vec![0x64, 0x01])]);
        c.read_scaling_data_by_identifier(0xF190).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x24, 0xF1, 0x90]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_data_by_periodic_identifier() {
        let mut c = client(vec![(MsgState::Success, vec![0x6A, 0x01])]);
        c.read_data_by_periodic_identifier(
            TransmissionModeType::SendAtSlowRate,
            Some(&[0x01, 0x02]),
        )
        .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x2A, 0x01, 0x01, 0x02]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_dynamically_define_by_id() {
        let mut c = client(vec![(MsgState::Success, vec![0x6C, 0x01, 0xF2, 0x00])]);
        let (_, resp) = c
            .dynamically_define_data_identifier_by_id(
                0xF200,
                &[SrcDataIdentifier::new(0xF190, 1, 4)],
                true,
            )
            .unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x2C, 0x01, 0xF2, 0x00, 0xF1, 0x90, 0x01, 0x04]
        );
        assert_eq!(resp.unwrap().identifier, 0xF200);
        let mut c = client(vec![]);
        assert!(c
            .dynamically_define_data_identifier_by_id(0xF100, &[], true)
            .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_dynamically_define_by_adr() {
        let mut c = client(vec![(MsgState::Success, vec![0x6C, 0x02, 0xF2, 0x00])]);
        c.dynamically_define_data_identifier_by_adr(
            0xF200,
            2,
            1,
            &[SrcDataMemory::new(0x1234, 0x20)],
            true,
        )
        .unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x2C, 0x02, 0xF2, 0x00, 0x12, 0x12, 0x34, 0x20]
        );
        let mut c = client(vec![]);
        assert!(c
            .dynamically_define_data_identifier_by_adr(0xF200, 2, 16, &[], true)
            .is_err());
        assert!(c
            .dynamically_define_data_identifier_by_adr(0xF200, 16, 1, &[], true)
            .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_dynamically_clear() {
        let mut c = client(vec![(MsgState::Success, vec![0x6C, 0x03, 0xF2, 0x00])]);
        c.dynamically_clear_data_identifier(0xF200, true).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x2C, 0x03, 0xF2, 0x00]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_write_data_by_identifier() {
        let mut c = client(vec![(MsgState::Success, vec![0x6E, 0xF1, 0x90])]);
        let (_, resp) = c.write_data_by_identifier(0xF190, &[0x41]).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x2E, 0xF1, 0x90, 0x41]);
        assert_eq!(resp.unwrap().identifier, 0xF190);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_write_memory_by_address() {
        let mut c = client(vec![(
            MsgState::Success,
            vec![0x7D, 0x21, 0x12, 0x34, 0x02],
        )]);
        let (_, resp) = c.write_memory_by_address(0x1234, 2, &[0xAA, 0xBB]).unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x3D, 0x12, 0x12, 0x34, 0x02, 0xAA, 0xBB]
        );
        let resp = resp.unwrap();
        assert_eq!(resp.adr_len(), 2);
        assert_eq!(resp.memory_address, 0x1234);
        assert_eq!(resp.memory_size, 2);
        let mut c = client(vec![]);
        assert!(c.write_memory_by_address(0x1234, 4095, &[]).is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_clear_diagnostic_information() {
        let mut c = client(vec![(MsgState::Success, vec![0x54])]);
        c.clear_diagnostic_information(0x00FF_FFFF).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x14, 0xFF, 0xFF, 0xFF]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_dtc_information_count() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x59, 0x01, 0xFF, 0x00, 0x00, 0x05]),
            (MsgState::Success, vec![0x59, 0x07, 0xFF, 0x01, 0x00, 0x02]),
        ]);
        let (_, resp) = c
            .read_dtc_information_count(
                ReportDtcType::ReportNumberOfDTCByStatusMask,
                DtcStatusMask(0xFF),
                0,
            )
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x19, 0x01, 0xFF]);
        assert_eq!(resp.unwrap().count, 5);
        c.read_dtc_information_count(
            ReportDtcType::ReportNumberOfDTCBySeverityMaskRecord,
            DtcStatusMask(0xFF),
            0x1234,
        )
        .unwrap();
        assert_eq!(
            c.0.transport.requests[1],
            vec![0x19, 0x07, 0xFF, 0x12, 0x34]
        );
        let mut c = client(vec![]);
        assert!(c
            .read_dtc_information_count(
                ReportDtcType::ReportDTCByStatusMask,
                DtcStatusMask(0xFF),
                0
            )
            .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_dtc_information_records() {
        let mut c = client(vec![
            (
                MsgState::Success,
                vec![0x59, 0x02, 0xFF, 0x12, 0x34, 0x56, 0x0A],
            ),
            (MsgState::Success, vec![0x59, 0x0A, 0xFF]),
        ]);
        let (_, resp) = c
            .read_dtc_information_records(ReportDtcType::ReportDTCByStatusMask, DtcStatusMask(0x09))
            .unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x19, 0x02, 0x09]);
        let resp = resp.unwrap();
        assert_eq!(resp.dtcs.len(), 1);
        assert_eq!(resp.dtcs[0].dtc, 0x12_3456);
        c.read_dtc_information_records(ReportDtcType::ReportSupportedDTC, DtcStatusMask(0))
            .unwrap();
        assert_eq!(c.0.transport.requests[1], vec![0x19, 0x0A]);
        let mut c = client(vec![]);
        assert!(c
            .read_dtc_information_records(
                ReportDtcType::ReportUserDefMemoryDTCByStatusMask,
                DtcStatusMask(0xFF)
            )
            .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_read_dtc_information_snapshot() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x59, 0x04]),
            (MsgState::Success, vec![0x59, 0x03]),
        ]);
        c.read_dtc_information(
            ReportDtcType::ReportDTCSnapshotRecordByDTCNumber,
            0x12_3456,
            0x01,
            true,
        )
        .unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x19, 0x04, 0x12, 0x34, 0x56, 0x01]
        );
        c.read_dtc_information(ReportDtcType::ReportDTCSnapshotIdentification, 0, 0, true)
            .unwrap();
        assert_eq!(c.0.transport.requests[1], vec![0x19, 0x03]);
        let mut c = client(vec![]);
        assert!(c
            .read_dtc_information(ReportDtcType::ReportDTCByStatusMask, 0, 0, true)
            .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_input_output_control_by_identifier() {
        let mut c = client(vec![(MsgState::Success, vec![0x6F, 0xF1, 0x90])]);
        let (_, resp) = c.input_output_control_by_identifier(0xF190).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x2F, 0xF1, 0x90]);
        assert_eq!(resp.unwrap().identifier, 0xF190);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_request_download() {
        let mut c = client(vec![(MsgState::Success, vec![0x74, 0x20, 0x12, 0x34])]);
        let (_, resp) = c.request_download(0x1234_5678, 0x1000, 0, 0, 0).unwrap();
        // fmt = (byte_len(size) << 4) | byte_len(addr) = 0x24
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x34, 0x00, 0x24, 0x12, 0x34, 0x56, 0x78, 0x10, 0x00]
        );
        assert_eq!(resp.unwrap().max_number_of_block_length, 4660);
        let mut c = client(vec![(MsgState::Success, vec![0x74, 0x10, 0x08])]);
        c.request_download(0x10, 0x10, 0x11, 1, 2).unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x34, 0x12, 0x11, 0x10, 0x10]
        );
        let mut c = client(vec![]);
        assert!(c.request_download(0, 1, 0, 16, 0).is_err());
        assert!(c.request_download(0, 1, 0, 0, 16).is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_request_upload() {
        let mut c = client(vec![(
            MsgState::Success,
            vec![0x75, 0x40, 0x00, 0x00, 0x10, 0x00],
        )]);
        let (_, resp) = c.request_upload(0x1234_5678, 0x1000, 0, 0, 0).unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x35, 0x00, 0x24, 0x12, 0x34, 0x56, 0x78, 0x10, 0x00]
        );
        assert_eq!(resp.unwrap().max_number_of_block_length, 4096);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_transfer_data() {
        let mut c = client(vec![(MsgState::Success, vec![0x76, 0x05, 0xAA])]);
        let (_, resp) = c.transfer_data(5, Some(&[0xAA, 0xBB])).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x36, 0x05, 0xAA, 0xBB]);
        assert_eq!(resp.unwrap().block_sequence_counter, 5);
        let mut c = client(vec![(MsgState::Success, vec![0x76, 0x01, 0x01])]);
        c.transfer_data(1, None).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x36, 0x01]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_request_transfer_exit() {
        let mut c = client(vec![(MsgState::Success, vec![0x77, 0x01])]);
        let (_, resp) = c.request_transfer_exit(None).unwrap();
        assert_eq!(c.0.transport.requests[0], vec![0x37]);
        assert_eq!(resp.unwrap().data, vec![0x01]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_request_file_transfer() {
        let mut c = client(vec![(MsgState::Success, vec![0x78])]);
        c.request_file_transfer(ModeOfOperationType::AddFile, "/f.bin")
            .unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x38, 0x01, 0x00, 0x06, 0x2F, 0x66, 0x2E, 0x62, 0x69, 0x6E]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn svc_routine_control() {
        let mut c = client(vec![(MsgState::Success, vec![0x71, 0x01, 0xFF, 0x00])]);
        let (_, resp) = c
            .routine_control(
                RoutineControlType::StartRoutine,
                RoutineIdentifierType::EraseMemory.as_value(),
                Some(&[0x01]),
                true,
            )
            .unwrap();
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x31, 0x01, 0xFF, 0x00, 0x01]
        );
        assert_eq!(resp.unwrap().routine_identifier, 0xFF00);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn negative_response_is_success_state_with_error_code() {
        let mut c = client(vec![(MsgState::Success, vec![0x7F, 0x22, 0x31])]);
        let (state, resp) = c.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.unwrap();
        assert_eq!(resp.base.error_code, 0x31);
        assert_eq!(resp.base.service_id_enum(), Some(Sid::ReadDataByIdentifier));
        assert_eq!(
            c.get_neg_res_code(resp.base.error_code),
            "RequestOutOfRange"
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn unexpected_rsid_detected() {
        let mut c = client(vec![(MsgState::Success, vec![0x7F, 0x10, 0x11])]);
        let (state, resp) = c.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedRSID);
        assert!(resp.is_none());
        let mut c = client(vec![(MsgState::Success, vec![0x50, 0x03])]);
        let (state, _) = c.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedRSID);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn malformed_response_maps_to_err_generic() {
        let mut c = client(vec![(MsgState::Success, vec![0x50, 0x03, 0x32])]);
        let (state, resp) = c
            .diagnostic_session_control(DiagnosticSessionType::Extended, true)
            .unwrap();
        assert_eq!(state, MsgState::ErrGeneric);
        assert!(resp.is_none());
        let mut c = client(vec![(MsgState::ErrTimeout, vec![])]);
        let (state, resp) = c.tester_present(true).unwrap();
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(resp.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn neg_res_code_lookup() {
        let mut c = client(vec![]);
        assert_eq!(c.get_neg_res_code(0x31), "RequestOutOfRange");
        assert_eq!(c.get_neg_res_code(0x01), "ErrRequestLenExceeded");
        assert_eq!(c.get_neg_res_code(0x05), "ErrTimeoutAwaitingCFFrame");
        assert_eq!(c.get_neg_res_code(0x0F), "15");
        assert_eq!(c.get_neg_res_code(0x99), "153");
        c.overlay_error_codes([(0x31, "custom out of range".to_string())]);
        assert_eq!(c.get_neg_res_code(0x31), "custom out of range");
    }

    #[cfg(feature = "blocking")]
    struct XorKey;

    #[cfg(feature = "blocking")]
    impl SeedKeyProvider for XorKey {
        fn compute_key_from_seed(
            &self,
            _request_seed_sf: u8,
            _variant: Option<&[u8]>,
            seed: &[u8],
        ) -> std::result::Result<Vec<u8>, i32> {
            Ok(seed.iter().map(|b| b.wrapping_add(1)).collect())
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn unlock_flow() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x67, 0x01, 0xAA, 0xBB]),
            (MsgState::Success, vec![0x67, 0x02]),
        ]);
        let code = c.unlock(0x01, &XorKey, None).unwrap();
        assert_eq!(code, 0);
        assert_eq!(c.0.transport.requests[0], vec![0x27, 0x01]);
        assert_eq!(c.0.transport.requests[1], vec![0x27, 0x02, 0xAB, 0xBC]);
        let mut c = client(vec![(MsgState::Success, vec![0x7F, 0x27, 0x33])]);
        let code = c.unlock(0x01, &XorKey, None).unwrap();
        assert_eq!(code, 0x33);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn download_happy_path() {
        let data: Vec<u8> = (0u8..30).collect();
        let mut c = client(vec![
            (MsgState::Success, vec![0x71, 0x01, 0xFF, 0x00]), // EraseMemory
            (MsgState::Success, vec![0x74, 0x20, 0x00, 0x10]), // RequestDownload, maxBlock=16
            (MsgState::Success, vec![0x76, 0x01]),
            (MsgState::Success, vec![0x76, 0x02]),
            (MsgState::Success, vec![0x76, 0x03]),
            (MsgState::Success, vec![0x77]), // TransferExit
        ]);
        let mut percents = Vec::new();
        let code = {
            let mut cb = |p: u32| -> bool {
                percents.push(p);
                false
            };
            c.download(0x1000, &data, 0, false, Some(&mut cb)).unwrap()
        };
        assert_eq!(code, 0);
        let reqs = &c.0.transport.requests;
        assert_eq!(
            reqs[0],
            vec![0x31, 0x01, 0xFF, 0x00, 0, 0, 0x10, 0x00, 0, 0, 0x10, 0x1D]
        );
        assert_eq!(reqs[1], vec![0x34, 0x00, 0x12, 0x10, 0x00, 0x1E]);
        assert_eq!(
            reqs[2],
            [vec![0x36, 0x01], (0u8..14).collect::<Vec<_>>()].concat()
        );
        assert_eq!(
            reqs[3],
            [vec![0x36, 0x02], (14u8..28).collect::<Vec<_>>()].concat()
        );
        assert_eq!(reqs[4], vec![0x36, 0x03, 28, 29]);
        assert_eq!(reqs[5], vec![0x37]);
        assert_eq!(percents, vec![46, 93, 100, 100]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn download_block_counter_mismatch() {
        let data: Vec<u8> = (0u8..10).collect();
        let mut c = client(vec![
            (MsgState::Success, vec![0x71, 0x01, 0xFF, 0x00]),
            (MsgState::Success, vec![0x74, 0x20, 0x00, 0x10]),
            (MsgState::Success, vec![0x76, 0x09]),
            (MsgState::Success, vec![0x77]),
        ]);
        let code = c.download(0x1000, &data, 0, false, None).unwrap();
        assert_eq!(code, 0x73); // WrongBlockSequenceCounter
        assert_eq!(c.0.transport.requests.len(), 4);
        assert_eq!(c.0.transport.requests[3], vec![0x37]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn download_user_cancel() {
        let data: Vec<u8> = (0u8..30).collect();
        let mut c = client(vec![
            (MsgState::Success, vec![0x74, 0x20, 0x00, 0x10]),
            (MsgState::Success, vec![0x76, 0x01]),
            (MsgState::Success, vec![0x77]),
        ]);
        let code = {
            let mut cb = |_p: u32| -> bool { true };
            c.download(0x1000, &data, 0, true, Some(&mut cb)).unwrap()
        };
        assert_eq!(code, 0xFE); // UserCancelled
        assert_eq!(c.0.transport.requests[0][0], 0x34);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn upload_happy_path() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x75, 0x20, 0x00, 0x10]),
            (MsgState::Success, vec![0x76, 0x01, 1, 2, 3]),
            (MsgState::Success, vec![0x76, 0x02, 4, 5]),
            (MsgState::Success, vec![0x77]),
        ]);
        let (code, data) = c.upload(0x1000, 5, 0, None).unwrap();
        assert_eq!(code, 0);
        assert_eq!(data, Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(c.0.transport.requests[1], vec![0x36, 0x01]);
        assert_eq!(c.0.transport.requests[2], vec![0x36, 0x02]);
        let mut c = client(vec![(MsgState::Success, vec![0x7F, 0x35, 0x70])]);
        let (code, data) = c.upload(0x1000, 5, 0, None).unwrap();
        assert_eq!(code, 0x70);
        assert_eq!(data, None);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn write_memory_chunks() {
        let data: Vec<u8> = (0u8..10).collect();
        let mut c = client(vec![
            (MsgState::Success, vec![0x7D, 0x21, 0x10, 0x00, 0x04]),
            (MsgState::Success, vec![0x7D, 0x21, 0x10, 0x04, 0x04]),
            (MsgState::Success, vec![0x7D, 0x21, 0x10, 0x08, 0x02]),
        ]);
        let code = c.write_memory(0x1000, &data, 4, None).unwrap();
        assert_eq!(code, 0);
        let reqs = &c.0.transport.requests;
        assert_eq!(reqs[0], vec![0x3D, 0x12, 0x10, 0x00, 0x04, 0, 1, 2, 3]);
        assert_eq!(reqs[1], vec![0x3D, 0x12, 0x10, 0x04, 0x04, 4, 5, 6, 7]);
        assert_eq!(reqs[2], vec![0x3D, 0x12, 0x10, 0x08, 0x02, 8, 9]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn read_memory_chunks() {
        let mut c = client(vec![
            (MsgState::Success, vec![0x63, 1, 2, 3]),
            (MsgState::Success, vec![0x63, 4, 5]),
        ]);
        let (code, data) = c.read_memory(0x1000, 5, 3, None).unwrap();
        assert_eq!(code, 0);
        assert_eq!(data, Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(
            c.0.transport.requests[0],
            vec![0x23, 0x12, 0x10, 0x00, 0x03]
        );
        assert_eq!(
            c.0.transport.requests[1],
            vec![0x23, 0x12, 0x10, 0x03, 0x02]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_execute_service_roundtrip() {
        let mut c = UdsClient::new(MockTransport {
            requests: Vec::new(),
            script: vec![(MsgState::Success, vec![0x62, 0xF1, 0x90, 0x41])].into(),
            max_len: 4095,
        });
        let (state, resp) = c.read_data_by_identifier(&[0xF190]).await.unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(c.transport.requests[0], vec![0x22, 0xF1, 0x90]);
        assert_eq!(resp.unwrap().data, vec![0xF1, 0x90, 0x41]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_download_flow() {
        let data: Vec<u8> = (0u8..30).collect();
        let mut c = UdsClient::new(MockTransport {
            requests: Vec::new(),
            script: vec![
                (MsgState::Success, vec![0x74, 0x20, 0x00, 0x10]), // RequestDownload, maxBlock=16
                (MsgState::Success, vec![0x76, 0x01]),
                (MsgState::Success, vec![0x76, 0x02]),
                (MsgState::Success, vec![0x76, 0x03]),
                (MsgState::Success, vec![0x77]), // TransferExit
            ]
            .into(),
            max_len: 4095,
        });
        let code = c.download(0x1000, &data, 0, true, None).await.unwrap();
        assert_eq!(code, 0);
        let reqs = &c.transport.requests;
        assert_eq!(reqs[0], vec![0x34, 0x00, 0x12, 0x10, 0x00, 0x1E]);
        assert_eq!(
            reqs[1],
            [vec![0x36, 0x01], (0u8..14).collect::<Vec<_>>()].concat()
        );
        assert_eq!(
            reqs[2],
            [vec![0x36, 0x02], (14u8..28).collect::<Vec<_>>()].concat()
        );
        assert_eq!(reqs[3], vec![0x36, 0x03, 28, 29]);
        assert_eq!(reqs[4], vec![0x37]);
    }
}
