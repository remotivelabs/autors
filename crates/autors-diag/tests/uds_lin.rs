use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use autors_diag::uds::{MsgState, UdsClient};
use autors_diag::uds_transport::LinTpTransport;
use autors_isotp::isotp::MsgState as TpState;
use autors_isotp::lin::{segment_message, LinTpFsm, MASTER_REQUEST_FRAME_ID};
use autors_lin::device::{LinConfiguration, LinDevice, LinFrame};

#[cfg(feature = "blocking")]
use autors_diag::blocking::BlockingUdsClient;

type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

struct LinEcuStub {
    nad: u8,
    sent: Vec<(u8, Vec<u8>)>,
    requested: Vec<u8>,
    requests: Vec<Vec<u8>>,
    request_fsm: LinTpFsm,
    response_frames: VecDeque<Vec<u8>>,
    rx: VecDeque<LinFrame>,
    responder: Responder,
}

impl LinEcuStub {
    fn new(nad: u8, responder: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
        Self {
            nad,
            sent: Vec::new(),
            requested: Vec::new(),
            requests: Vec::new(),
            request_fsm: LinTpFsm::new_receiver(Some(nad)),
            response_frames: VecDeque::new(),
            rx: VecDeque::new(),
            responder: Box::new(responder),
        }
    }

    fn receive_request_pdu(&mut self, data: &[u8]) {
        let state = self.request_fsm.on_received(data).unwrap();
        if state != TpState::Success {
            return;
        }
        let request = self.request_fsm.take_received();
        let response = (self.responder)(&request);
        self.requests.push(request);
        self.request_fsm = LinTpFsm::new_receiver(Some(self.nad));
        if !response.is_empty() {
            for frame in segment_message(self.nad, &response, 0xff).unwrap() {
                self.response_frames.push_back(frame.to_vec());
            }
        }
    }
}

#[async_trait]
impl LinDevice for LinEcuStub {
    fn unique_bus_id(&self) -> i32 {
        1
    }

    fn is_available(&self) -> bool {
        true
    }

    async fn open(&mut self, _config: &LinConfiguration) -> autors_lin::Result<bool> {
        Ok(true)
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> autors_lin::Result<usize> {
        self.sent.push((id, data.to_vec()));
        if id == MASTER_REQUEST_FRAME_ID {
            self.receive_request_pdu(data);
        }
        self.rx
            .push_back(LinFrame::new("Stub/LIN1", id, data.to_vec(), true));
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> autors_lin::Result<bool> {
        self.requested.push(id);
        if let Some(data) = self.response_frames.pop_front() {
            self.rx
                .push_back(LinFrame::new("Stub/LIN1", id, data, false));
        }
        Ok(true)
    }

    async fn on_receive(&mut self) -> autors_lin::Result<Option<LinFrame>> {
        Ok(self.rx.pop_front())
    }

    async fn close(&mut self) {}
}

fn fast_transport(stub: LinEcuStub, nad: u8) -> LinTpTransport<LinEcuStub> {
    let mut transport = LinTpTransport::new(stub, nad);
    transport.lintp.config.p2_min = Duration::ZERO;
    transport.lintp.config.p2_timeout = Duration::from_millis(20);
    transport.lintp.config.p2_star_timeout = Duration::from_millis(20);
    transport.lintp.config.n_cr_timeout = Duration::from_millis(20);
    transport.lintp.config.response_slot_timeout = Duration::from_millis(2);
    transport.lintp.config.response_poll_interval = Duration::from_millis(1);
    transport
}

#[cfg(feature = "blocking")]
#[test]
fn typed_uds_read_did_over_lin_reassembles_segmented_response() {
    let stub = LinEcuStub::new(0x12, |request| {
        assert_eq!(request, [0x22, 0xf1, 0x90]);
        let mut response = vec![0x62, 0xf1, 0x90];
        response.extend(1..=12);
        response
    });
    let transport = fast_transport(stub, 0x12);
    let mut client = BlockingUdsClient::new(UdsClient::new(transport));
    let (state, response) = client.read_data_by_identifier(&[0xf190]).unwrap();
    assert_eq!(state, MsgState::Success);
    let response = response.unwrap();
    assert_eq!(&response.data[..2], [0xf1, 0x90]);
    assert_eq!(&response.data[2..], (1..=12).collect::<Vec<_>>());
    assert_eq!(client.0.transport.device.sent.len(), 1);
    assert_eq!(client.0.transport.device.requested.len(), 3);
}

#[cfg(feature = "blocking")]
#[test]
fn typed_uds_write_did_over_lin_segments_request() {
    let payload: Vec<u8> = (1..=20).collect();
    let expected = payload.clone();
    let stub = LinEcuStub::new(1, move |request| {
        assert_eq!(&request[..3], [0x2e, 0xf1, 0x90]);
        assert_eq!(&request[3..], expected);
        vec![0x6e, 0xf1, 0x90]
    });
    let transport = fast_transport(stub, 1);
    let mut client = BlockingUdsClient::new(UdsClient::new(transport));
    let (state, response) = client.write_data_by_identifier(0xf190, &payload).unwrap();
    assert_eq!(state, MsgState::Success);
    assert_eq!(response.unwrap().identifier, 0xf190);
    assert_eq!(client.0.transport.device.sent.len(), 4);
    assert_eq!(client.0.transport.device.requests[0].len(), 23);
}

#[cfg(feature = "blocking")]
#[test]
fn tester_present_without_response_sets_suppress_bit_and_does_not_poll() {
    let stub = LinEcuStub::new(1, |request| {
        assert_eq!(request, [0x3e, 0x80]);
        Vec::new()
    });
    let transport = fast_transport(stub, 1);
    let mut client = BlockingUdsClient::new(UdsClient::new(transport));
    let (state, response) = client.tester_present(false).unwrap();
    assert_eq!(state, MsgState::Success);
    assert!(response.is_none());
    assert!(client.0.transport.device.requested.is_empty());
}

#[tokio::test]
async fn async_uds_negative_response_over_lin_is_parsed() {
    let stub = LinEcuStub::new(1, |_| vec![0x7f, 0x22, 0x31]);
    let transport = fast_transport(stub, 1);
    let mut client = UdsClient::new(transport);
    let (state, response) = client.read_data_by_identifier(&[0xf190]).await.unwrap();
    assert_eq!(state, MsgState::Success);
    let response = response.unwrap();
    assert!(response.base.is_negative());
    assert_eq!(response.base.error_code, 0x31);
}
