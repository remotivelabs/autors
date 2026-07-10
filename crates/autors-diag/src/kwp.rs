//! KWP2000 service enumerations (negative response codes, service IDs) per
//! ISO 14230-3.
//! The ISO-TP transport layer (`IsoTpFsm`, `MsgState`, `FlowStatus`,
//! `FrameType`, `IsoTpType`, `NO_RESPONSE_MASK`, CAN send/receive with
//! flow-control waiting and timeout/abort handling) lives in the
//! autors-isotp crate. Typed parsing of the KWP-domain A2L
//! `IF_DATA ASAP1B_KWP2000` block lives in the [`crate::ifdata_kwp`] module.

/// Numeric enum helper macro (same pattern as in `doip.rs`; the helper is
/// kept local to each file by convention).
macro_rules! num_enum {
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty { $( $(#[$vmeta:meta])* $var:ident = $val:expr ),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr($t)]
        $vis enum $name {
            $( $(#[$vmeta])* $var = $val ),+
        }
        impl $name {
            /// Convert a numeric value to the enum; unknown values return `None`.
            pub fn from_num(v: $t) -> Option<Self> {
                match v {
                    $( x if x == Self::$var as $t => Some(Self::$var), )+
                    _ => None,
                }
            }
            /// Convert the enum to its numeric value.
            pub fn to_num(self) -> $t {
                self as $t
            }
        }
    };
}

num_enum! {
    /// KWP2000 negative response codes (ISO 14230-3).
    /// Note: `ImproperDownloadType` and `CanNotDownloadToSpecifiedAddress`
    /// intentionally share the value 0x42 (per ISO 14230 the former should be
    /// 0x41; the duplicate assignment is retained for wire compatibility).
    /// Rust enums do not allow duplicate discriminants, so the latter is
    /// exposed as the associated constant
    /// [`NegRespCode::CAN_NOT_DOWNLOAD_TO_SPECIFIED_ADDRESS`].
    pub NegRespCode : u8 {
        /// Positive response (not an error).
        Positive = 0x00,
        /// General reject.
        GeneralReject = 0x10,
        /// Service not supported.
        ServiceNotSupported = 0x11,
        /// Sub-function not supported.
        SubFunctionNotSupported = 0x12,
        /// Busy, repeat request.
        BusyRepeatRequest = 0x21,
        /// Conditions not correct.
        ConditionsNotCorrect = 0x22,
        /// Routine not complete or service in progress.
        RoutineNotCompleteOrServiceInProgress = 0x23,
        /// Request out of range.
        RequestOutOfRange = 0x31,
        /// Security access denied.
        SecurityAccessDenied = 0x33,
        /// Invalid key.
        InvalidKey = 0x35,
        /// Exceeded number of attempts.
        ExceededNumberOfAttempts = 0x36,
        /// Required time delay not expired.
        RequiredTimeDelayNotExpired = 0x37,
        /// Download not accepted.
        DownloadNotAccepted = 0x40,
        /// Improper download type (value 0x42; see the type-level note).
        ImproperDownloadType = 0x42,
        /// Cannot download the requested number of bytes.
        CanNotDownloadNumberOfBytesRequested = 0x43,
        /// Upload not accepted.
        UploadNotAccepted = 0x50,
        /// Improper upload type.
        ImproperUploadType = 0x51,
        /// Cannot upload from the specified address.
        CanNotUploadFromSpecifiedAddress = 0x52,
        /// Cannot upload the requested number of bytes.
        CanNotUploadNumberOfBytesRequested = 0x53,
        /// Transfer suspended.
        TransferSuspended = 0x71,
        /// Transfer aborted.
        TransferAborted = 0x72,
        /// Illegal address in block transfer.
        IllegalAddressInBlockTransfer = 0x74,
        /// Illegal byte count in block transfer.
        IllegalByteCountInBlockTransfer = 0x75,
        /// Illegal block transfer type.
        IllegalBlockTransferType = 0x76,
        /// Block transfer data checksum error.
        BlockTransferDataChecksumError = 0x77,
        /// Request correctly received, response pending.
        RequestCorrectlyReceivedResponsePending = 0x78,
        /// Incorrect byte count during block transfer.
        IncorrectByteCountDuringBlockTransfer = 0x79,
        /// Service not supported in the active session.
        ServiceNotSupportedInActiveSession = 0x80,
    }
}

impl NegRespCode {
    /// `CanNotDownloadToSpecifiedAddress` (intentionally shares the value
    /// 0x42 with [`NegRespCode::ImproperDownloadType`]; see the type-level
    /// note).
    pub const CAN_NOT_DOWNLOAD_TO_SPECIFIED_ADDRESS: NegRespCode =
        NegRespCode::ImproperDownloadType;
}

num_enum! {
    /// KWP2000 service IDs (ISO 14230-3).
    pub Sid : u8 {
        /// StartCommunication (0x81).
        StartCommunication = 0x81,
        /// StopCommunication (0x82).
        StopCommunication = 0x82,
        /// AccessTimingParameters (0x83).
        AccessTimingParameters = 0x83,
        /// TesterPresent (0x3E).
        TesterPresent = 0x3E,
        /// StartDiagnosticSession (0x10).
        StartDiagnosticSession = 0x10,
        /// StopDiagnosticSession (0x20).
        StopDiagnosticSession = 0x20,
        /// SecurityAccess (0x27).
        SecurityAccess = 0x27,
        /// EcuReset (0x11).
        EcuReset = 0x11,
        /// ReadEcuIdentification (0x1A).
        ReadEcuIdentification = 0x1A,
        /// ReadDataByLocalIdentifier (0x21).
        ReadDataByLocalIdentifier = 0x21,
        /// ReadDataByCommonIdentifier (0x22).
        ReadDataByCommonIdentifier = 0x22,
        /// ReadMemoryByAddress (0x23).
        ReadMemoryByAddress = 0x23,
        /// DynamicallyDefineLocalIdentifier (0x2C).
        DynamicallyDefineLocalIdentifier = 0x2C,
        /// WriteDataByLocalIdentifier (0x3B).
        WriteDataByLocalIdentifier = 0x3B,
        /// WriteDataByCommonIdentifier (0x2E).
        WriteDataByCommonIdentifier = 0x2E,
        /// WriteMemoryByAddress (0x3D).
        WriteMemoryByAddress = 0x3D,
        /// SetDataRates (0x26).
        SetDataRates = 0x26,
        /// StopRepeatedDataTransmission (0x25).
        StopRepeatedDataTransmission = 0x25,
        /// ReadDiagnosticTroubleCodes (0x13).
        ReadDiagnosticTroubleCodes = 0x13,
        /// ReadDiagnosticTroubleCodesByStatus (0x18).
        ReadDiagnosticTroubleCodesByStatus = 0x18,
        /// ReadStatusOfDiagnosticTroubleCodes (0x17).
        ReadStatusOfDiagnosticTroubleCodes = 0x17,
        /// ReadFreezeFrameData (0x12).
        ReadFreezeFrameData = 0x12,
        /// ClearDiagnosticInformation (0x14).
        ClearDiagnosticInformation = 0x14,
        /// InputOutputControlByLocalIdentifier (0x30).
        InputOutputControlByLocalIdentifier = 0x30,
        /// InputOutputControlByCommonIdentifier (0x2F).
        InputOutputControlByCommonIdentifier = 0x2F,
        /// StartRoutineByLocalIdentifier (0x31).
        StartRoutineByLocalIdentifier = 0x31,
        /// StartRoutineByAddress (0x38).
        StartRoutineByAddress = 0x38,
        /// StopRoutineByLocalIdentifier (0x32).
        StopRoutineByLocalIdentifier = 0x32,
        /// StopRoutineByAddress (0x39).
        StopRoutineByAddress = 0x39,
        /// RequestRoutineResultsByLocalIdentifier (0x33).
        RequestRoutineResultsByLocalIdentifier = 0x33,
        /// RequestRoutineResultsByAddress (0x3A).
        RequestRoutineResultsByAddress = 0x3A,
        /// RequestDownload (0x34).
        RequestDownload = 0x34,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neg_resp_code_values() {
        assert_eq!(NegRespCode::Positive.to_num(), 0x00);
        assert_eq!(NegRespCode::GeneralReject.to_num(), 0x10);
        assert_eq!(
            NegRespCode::RequestCorrectlyReceivedResponsePending.to_num(),
            0x78
        );
        assert_eq!(
            NegRespCode::ServiceNotSupportedInActiveSession.to_num(),
            0x80
        );
        // Intentional duplicate: both names map to 0x42.
        assert_eq!(NegRespCode::ImproperDownloadType.to_num(), 0x42);
        assert_eq!(
            NegRespCode::CAN_NOT_DOWNLOAD_TO_SPECIFIED_ADDRESS.to_num(),
            0x42
        );
        assert_eq!(
            NegRespCode::from_num(0x42),
            Some(NegRespCode::ImproperDownloadType)
        );
        assert_eq!(NegRespCode::from_num(0xFF), None);
    }

    #[test]
    fn sid_values() {
        assert_eq!(Sid::StartCommunication.to_num(), 0x81);
        assert_eq!(Sid::StopCommunication.to_num(), 0x82);
        assert_eq!(Sid::AccessTimingParameters.to_num(), 0x83);
        assert_eq!(Sid::TesterPresent.to_num(), 0x3E);
        assert_eq!(Sid::StartDiagnosticSession.to_num(), 0x10);
        assert_eq!(Sid::SecurityAccess.to_num(), 0x27);
        assert_eq!(Sid::RequestDownload.to_num(), 0x34);
        assert_eq!(Sid::from_num(0x21), Some(Sid::ReadDataByLocalIdentifier));
        assert_eq!(Sid::from_num(0x7F), None);
    }
}
