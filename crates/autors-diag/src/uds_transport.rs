//! Wiring for running UDS/KWP sessions over CAN ISO-TP or LIN transport.
//! The transport adapters are shared by both KWP and UDS
//! services (the KWP domain defines only the `Sid`/`NegRespCode`
//! enumerations, no separate session class), so [`IsoTpTransport`] and
//! [`LinTpTransport`] can serve KWP services (e.g.
//! `kwp::Sid::StartCommunication` 0x81) and UDS services alike: KWP calls
//! [`crate::uds::UdsTransport::send_request`] directly, the UDS side is
//! driven through [`crate::uds::UdsClient`].
//! Behavioral contract:
//! - A missing response buffer means that the request completes after transmit;
//!   the suppressPosRsp bit has already been set by the UDS client.
//! - Device driver errors have no error channel in
//!   [`crate::uds::UdsTransport`]; adapters record them in `last_error` and report
//!   `MsgState::ErrSendRequest` (same value as a plain send failure — the
//!   mapping with the smallest observable difference).

use async_trait::async_trait;
use autors_can::device::CanDevice;
use autors_isotp::lin_transport::LinTp;
use autors_isotp::transport::IsoTp;
use autors_lin::device::LinDevice;

use crate::uds::{MsgState, UdsTransport};

/// Combines a CAN device with an ISO-TP transport and implements
/// [`UdsTransport`], so that [`crate::uds::UdsClient`] can run over a real
/// CAN channel without any mocks. Timeout and padding parameters
/// (`P2Client`/`P3Client`/`UseFillByte`/`FillByte`, `CmdId`/`RspId`) are read
/// and written directly through the `isotp` field.
pub struct IsoTpTransport<D: CanDevice> {
    /// CAN hardware abstraction.
    pub device: D,
    /// ISO-TP transport instance.
    pub isotp: IsoTp,
    /// Most recent device driver error (see the module-level note).
    last_error: Option<autors_isotp::Error>,
}

impl<D: CanDevice> IsoTpTransport<D> {
    /// Construct an ISO-TP transport for the given command/response CAN IDs
    /// (P2/P3 default to 50 ms; fill-byte and other defaults match the
    /// `IsoTp` defaults).
    /// Reception is driven by explicit polling on the caller's thread
    /// (consistent with autors-isotp); no receive callback is registered on
    /// the device.
    pub fn new(device: D, cmd_id: u32, rsp_id: u32, use_can_fd: bool) -> Self {
        Self {
            device,
            isotp: IsoTp::new(cmd_id, rsp_id, use_can_fd),
            last_error: None,
        }
    }

    /// Most recent device driver error, or `None` if none occurred.
    pub fn last_error(&self) -> Option<&autors_isotp::Error> {
        self.last_error.as_ref()
    }
}

/// Message-state mapping: autors-isotp (`i32` repr) and autors-diag
/// `uds::MsgState` (`i8` repr) share the same discriminant assignments;
/// unknown values (should not happen) degrade to `ErrGeneric`.
fn map_state(state: autors_isotp::isotp::MsgState) -> MsgState {
    MsgState::from_value(state.to_num() as i8).unwrap_or(MsgState::ErrGeneric)
}

#[async_trait]
impl<D: CanDevice + Send> UdsTransport for IsoTpTransport<D> {
    /// Always awaits the response (`await_response = true`); see the
    /// module-level notes.
    async fn send_request(&mut self, request: &[u8], response: Option<&mut Vec<u8>>) -> MsgState {
        match self
            .isotp
            .send_request(&mut self.device, request.to_vec(), response, true)
            .await
        {
            Ok(state) => map_state(state),
            Err(e) => {
                self.last_error = Some(e);
                MsgState::ErrSendRequest
            }
        }
    }

    /// Delegates to `IsoTp::max_msg_len`.
    fn max_msg_len(&self) -> u64 {
        u64::from(self.isotp.max_msg_len())
    }
}

/// Combines a LIN device with ISO 17987-2 transport so [`crate::uds::UdsClient`]
/// can use the same typed UDS services over a LIN cluster.
pub struct LinTpTransport<D: LinDevice> {
    /// LIN hardware abstraction.
    pub device: D,
    /// LIN diagnostic transport instance and its timing configuration.
    pub lintp: LinTp,
    /// Most recent device or LIN transport error.
    last_error: Option<autors_isotp::Error>,
}

impl<D: LinDevice> LinTpTransport<D> {
    /// Constructs a LIN diagnostic transport for a target node address.
    /// The device must be opened as a commander with DLC 8 and classic
    /// checksum for diagnostic IDs (the defaults of `LinConfiguration::new`).
    pub fn new(device: D, nad: u8) -> Self {
        Self {
            device,
            lintp: LinTp::new(nad),
            last_error: None,
        }
    }

    /// Constructs an adapter around an already configured LIN transport.
    pub fn with_transport(device: D, lintp: LinTp) -> Self {
        Self {
            device,
            lintp,
            last_error: None,
        }
    }

    /// Returns the most recent transport error, if one occurred.
    pub fn last_error(&self) -> Option<&autors_isotp::Error> {
        self.last_error.as_ref()
    }
}

#[async_trait]
impl<D: LinDevice + Send> UdsTransport for LinTpTransport<D> {
    async fn send_request(&mut self, request: &[u8], response: Option<&mut Vec<u8>>) -> MsgState {
        let await_response = response.is_some();
        match self
            .lintp
            .send_request(&mut self.device, request.to_vec(), response, await_response)
            .await
        {
            Ok(state) => map_state(state),
            Err(error) => {
                self.last_error = Some(error);
                MsgState::ErrSendRequest
            }
        }
    }

    fn max_msg_len(&self) -> u64 {
        u64::from(self.lintp.max_msg_len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "blocking")]
    use crate::blocking::{BlockingTransport, BlockingUdsClient};
    use crate::kwp;
    use crate::uds::{NegRespCode, Sid, UdsClient};
    use async_trait::async_trait;
    use autors_can::device::DeviceCore;
    use autors_can::frame::{CanConfiguration, CanFrame, FrameType as CanFrameType};
    use autors_isotp::isotp::{IsoTpFsm, MsgState as TpState};
    use std::collections::VecDeque;

    const CMD_ID: u32 = 0x7E0;
    const RSP_ID: u32 = 0x7E8;

    /// Maps a complete request message to a complete response message.
    type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

    /// Loopback ECU stub (modeled on the `EcuStub` + `ComplianceEcu` idea
    /// from the autors-isotp `transport` tests): implements [`CanDevice`],
    /// auto-replies with FC on receiving an FF, and once a full request
    /// message is assembled, calls the responder to build a response and
    /// segments it back.
    struct EcuStub {
        core: DeviceCore,
        rsp_id: u32,
        /// Log of frames sent by the tester.
        sent: Vec<Vec<u8>>,
        rx: VecDeque<CanFrame>,
        bs: u8,
        fsm: IsoTpFsm,
        responder: Responder,
    }

    impl EcuStub {
        fn new(bs: u8, responder: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
            Self {
                core: DeviceCore::new(),
                rsp_id: RSP_ID,
                sent: Vec::new(),
                rx: VecDeque::new(),
                bs,
                fsm: IsoTpFsm::new(Vec::new(), true, 8, bs, 0),
                responder: Box::new(responder),
            }
        }

        fn on_frame(&mut self, data: &[u8]) {
            let mut out = Vec::new();
            if self.fsm.on_received(data).is_ok() {
                Self::pump(&mut self.fsm, &mut out);
                if self.fsm.state() == TpState::Success {
                    let req = self.fsm.take_received();
                    let resp = (self.responder)(&req);
                    self.fsm = if resp.is_empty() {
                        IsoTpFsm::new(Vec::new(), true, 8, self.bs, 0)
                    } else {
                        IsoTpFsm::new(resp, false, 8, 0, 0)
                    };
                    Self::pump(&mut self.fsm, &mut out);
                }
            }
            for frame in out {
                self.rx.push_back(CanFrame::new(
                    "Stub/CAN1",
                    self.rsp_id,
                    frame,
                    false,
                    CanFrameType::CAN20B,
                ));
            }
        }

        fn pump(fsm: &mut IsoTpFsm, out: &mut Vec<Vec<u8>>) {
            loop {
                let (state, frame) = fsm.next_frame();
                match frame {
                    Some(f) => out.push(f),
                    None => break,
                }
                if state >= TpState::Success {
                    break;
                }
            }
        }
    }

    #[async_trait]
    impl CanDevice for EcuStub {
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
            _can_id: u32,
            data: &[u8],
            _frame_type: CanFrameType,
        ) -> autors_can::Result<usize> {
            self.sent.push(data.to_vec());
            self.on_frame(data);
            Ok(data.len())
        }
        async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
            Ok(self.rx.pop_front())
        }
    }

    #[cfg(feature = "blocking")]
    fn client_with_ecu(
        responder: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static,
    ) -> BlockingUdsClient<IsoTpTransport<EcuStub>> {
        BlockingUdsClient::new(UdsClient::new(IsoTpTransport::new(
            EcuStub::new(0, responder),
            CMD_ID,
            RSP_ID,
            false,
        )))
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_read_data_by_identifier_roundtrip() {
        let mut client = client_with_ecu(|req| {
            assert_eq!(req, &[0x22, 0xF1, 0x90]);
            vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]
        });
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("positive response");
        assert_eq!(resp.base.service_id, Sid::ReadDataByIdentifier.as_value());
        // The request goes out as an ISO-TP SF (UseFillByte defaults to true
        // → padded to 8 bytes).
        assert_eq!(
            client.0.transport.device.sent[0],
            vec![0x03, 0x22, 0xF1, 0x90, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_negative_response_maps_neg_resp_code() {
        // ECU replies with negative response 7F 22 31 (RequestOutOfRange).
        let mut client = client_with_ecu(|_| vec![0x7F, 0x22, 0x31]);
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).unwrap();
        // A negative response still yields MsgState::Success; the error code
        // lands in response.ErrorCode.
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("negative response still parses");
        assert!(resp.base.is_negative());
        assert_eq!(
            resp.base.error_code,
            NegRespCode::RequestOutOfRange.as_value()
        );
        assert_eq!(
            client.get_neg_res_code(resp.base.error_code),
            "RequestOutOfRange"
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_multi_frame_response_reassembled() {
        // ECU replies with a 20-byte multi-frame response.
        let mut client = client_with_ecu(|req| {
            assert_eq!(req, &[0x22, 0xF1, 0x90]);
            let mut r = vec![0x62, 0xF1, 0x90];
            r.extend(1..=17u8);
            r
        });
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("positive response");
        assert_eq!(resp.base.service_id, Sid::ReadDataByIdentifier.as_value());
        // Tester sent: request SF + one FC(CTS).
        let sent = &client.0.transport.device.sent;
        assert_eq!(sent.len(), 2);
        assert_eq!(&sent[1][..3], &[0x30, 0x00, 0x00]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_tester_present_no_response_suppresses_pos_rsp() {
        // await_response = false: the client sets the suppressPosRsp bit
        // (0x80) itself, the transport returns Success right after the
        // request is sent, and the ECU does not answer.
        let mut client = client_with_ecu(|_| vec![]);
        let (state, resp) = client.tester_present(false).unwrap();
        assert_eq!(state, MsgState::Success);
        assert!(resp.is_none());
        assert_eq!(
            client.0.transport.device.sent[0],
            vec![0x02, 0x3E, 0x80, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_timeout_maps_err_timeout() {
        // ECU stays silent: P2 timeout → ErrTimeout.
        let mut client = client_with_ecu(|_| vec![]);
        client.0.transport.isotp.p2_client = 20;
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(resp.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_unexpected_rsid() {
        // The ECU's positive response SID does not match the request
        // (0x62 != 0x50|0x40).
        let mut client = client_with_ecu(|_| vec![0x62, 0x01]);
        let (state, resp) = client
            .diagnostic_session_control(crate::uds::DiagnosticSessionType::Extended, true)
            .unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedRSID);
        assert!(resp.is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn session_tracking_updates_current_diag_session() {
        // Successful DiagnosticSessionControl → CurrentDiagSession =
        // sub-function.
        let mut client = client_with_ecu(|req| match req[0] {
            0x10 => vec![0x50, req[1], 0x00, 0x32, 0x01, 0xF4],
            0x11 => vec![0x51, req[1]],
            _ => vec![0x7F, req[0], 0x11],
        });
        assert_eq!(client.current_diag_session(), 1);
        let (state, resp) = client
            .diagnostic_session_control(crate::uds::DiagnosticSessionType::Extended, true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert!(resp.is_some());
        assert_eq!(client.current_diag_session(), 3);
        // Successful EcuReset → back to Default(1).
        let (state, _) = client
            .ecu_reset(crate::uds::ResetType::HardReset, true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(client.current_diag_session(), 1);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn kwp_start_communication_over_same_transport() {
        // KWP messages go through IsoTpTransport directly (the KWP domain
        // defines only enumerations, no separate session class).
        // StartCommunication(0x81) → C1 EA 8F.
        let stub = EcuStub::new(0, |req| {
            assert_eq!(req, &[kwp::Sid::StartCommunication.to_num()]);
            vec![0xC1, 0xEA, 0x8F]
        });
        let mut transport =
            BlockingTransport::new(IsoTpTransport::new(stub, CMD_ID, RSP_ID, false));
        let mut res = Vec::new();
        let state =
            transport.send_request(&[kwp::Sid::StartCommunication.to_num()], Some(&mut res));
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0xC1, 0xEA, 0x8F]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn kwp_negative_response_code() {
        // KWP negative response 7F 81 22 (ConditionsNotCorrect, KWP
        // NegRespCode).
        let stub = EcuStub::new(0, |_| vec![0x7F, 0x81, 0x22]);
        let mut transport =
            BlockingTransport::new(IsoTpTransport::new(stub, CMD_ID, RSP_ID, false));
        let mut res = Vec::new();
        let state =
            transport.send_request(&[kwp::Sid::StartCommunication.to_num()], Some(&mut res));
        assert_eq!(state, MsgState::Success);
        assert_eq!(res[0], 0x7F);
        assert_eq!(
            kwp::NegRespCode::from_num(res[2]),
            Some(kwp::NegRespCode::ConditionsNotCorrect)
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn max_msg_len_delegates_to_isotp() {
        let stub = EcuStub::new(0, |_| vec![]);
        let transport = BlockingTransport::new(IsoTpTransport::new(stub, CMD_ID, RSP_ID, false));
        // Classic CAN: 4095.
        assert_eq!(transport.max_msg_len(), 4095);
        assert!(transport.0.last_error().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_uds_read_data_by_identifier_roundtrip() {
        let mut client = UdsClient::new(IsoTpTransport::new(
            EcuStub::new(0, |req| {
                assert_eq!(req, &[0x22, 0xF1, 0x90]);
                vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]
            }),
            CMD_ID,
            RSP_ID,
            false,
        ));
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).await.unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("positive response");
        assert_eq!(resp.base.service_id, Sid::ReadDataByIdentifier.as_value());
        // The request goes out as an ISO-TP SF (UseFillByte defaults to true
        // → padded to 8 bytes).
        assert_eq!(
            client.transport.device.sent[0],
            vec![0x03, 0x22, 0xF1, 0x90, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_uds_timeout_maps_err_timeout() {
        // ECU stays silent: P2 timeout → ErrTimeout.
        let mut client = UdsClient::new(IsoTpTransport::new(
            EcuStub::new(0, |_| vec![]),
            CMD_ID,
            RSP_ID,
            false,
        ));
        client.transport.isotp.p2_client = 20;
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).await.unwrap();
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(resp.is_none());
    }
}
