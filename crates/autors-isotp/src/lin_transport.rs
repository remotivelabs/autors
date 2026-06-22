//! Commander-side ISO 17987-2 diagnostic transport over a LIN device.

use std::time::{Duration, Instant};

use autors_lin::device::LinDevice;

use crate::error::{Error, Result};
use crate::isotp::{IsoTpType, MsgState, MAX_MSG_LEN_LIN, NO_RESPONSE_MASK};
use crate::lin::{
    LinTpFsm, LinTpPdu, MASTER_REQUEST_FRAME_ID, NAD_BROADCAST, NAD_FUNCTIONAL,
    SLAVE_RESPONSE_FRAME_ID,
};

/// Timing and frame identifiers for a commander-side LIN transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinTpConfig {
    /// Diagnostic master-request frame identifier.
    pub request_id: u8,
    /// Diagnostic slave-response frame identifier.
    pub response_id: u8,
    /// Padding byte used in unused diagnostic-frame data bytes.
    pub fill_byte: u8,
    /// Minimum delay after the request before polling the first response.
    pub p2_min: Duration,
    /// Maximum wait for the first response PDU.
    pub p2_timeout: Duration,
    /// Maximum wait after a UDS response-pending (NRC 0x78) message.
    pub p2_star_timeout: Duration,
    /// Maximum time accepted for one device transmit operation.
    pub n_as_timeout: Duration,
    /// Whether each master-request PDU must be observed as a transmitted frame
    /// before the next PDU is scheduled.
    pub require_tx_confirmation: bool,
    /// Maximum wait between received FF/CF PDUs.
    pub n_cr_timeout: Duration,
    /// Minimum spacing between diagnostic PDUs in one transfer.
    pub st_min: Duration,
    /// Delay between unanswered slave-response header requests.
    pub response_poll_interval: Duration,
    /// Time spent polling the device after each response header request.
    pub response_slot_timeout: Duration,
    /// Delay used when a device poll returns no frame.
    pub device_poll_interval: Duration,
}

impl Default for LinTpConfig {
    fn default() -> Self {
        Self {
            request_id: MASTER_REQUEST_FRAME_ID,
            response_id: SLAVE_RESPONSE_FRAME_ID,
            fill_byte: 0xff,
            p2_min: Duration::from_millis(50),
            p2_timeout: Duration::from_millis(500),
            p2_star_timeout: Duration::from_secs(5),
            n_as_timeout: Duration::from_secs(1),
            require_tx_confirmation: true,
            n_cr_timeout: Duration::from_secs(1),
            st_min: Duration::ZERO,
            response_poll_interval: Duration::from_millis(10),
            response_slot_timeout: Duration::from_millis(20),
            device_poll_interval: Duration::from_millis(1),
        }
    }
}

impl LinTpConfig {
    /// Validates IDs and non-zero timeout values.
    pub fn validate(&self) -> Result<()> {
        if self.request_id > 0x3f || self.response_id > 0x3f {
            return Err(Error::Protocol(
                "LIN TP frame identifiers must be in 0x00..=0x3F".to_string(),
            ));
        }
        if self.p2_timeout.is_zero()
            || self.p2_star_timeout.is_zero()
            || self.n_as_timeout.is_zero()
            || self.n_cr_timeout.is_zero()
            || self.response_slot_timeout.is_zero()
            || self.device_poll_interval.is_zero()
        {
            return Err(Error::Protocol(
                "LIN TP timeout and device polling durations must be non-zero".to_string(),
            ));
        }
        if self.p2_min >= self.p2_timeout || self.p2_min >= self.p2_star_timeout {
            return Err(Error::Protocol(
                "LIN TP p2_min must be shorter than p2_timeout and p2_star_timeout".to_string(),
            ));
        }
        Ok(())
    }
}

/// Commander-side LIN diagnostic transport.
pub struct LinTp {
    /// Target node address placed in outgoing diagnostic PDUs.
    pub nad: u8,
    /// Expected response NAD. `None` accepts any NAD, useful after a functional
    /// or broadcast request when the caller has ensured collision-free polling.
    pub response_nad: Option<u8>,
    /// Transport timing, padding, and diagnostic frame identifiers.
    pub config: LinTpConfig,
}

impl LinTp {
    /// Creates a transport for `nad` with standard diagnostic IDs and timings.
    pub fn new(nad: u8) -> Self {
        Self {
            nad,
            response_nad: if matches!(nad, NAD_FUNCTIONAL | NAD_BROADCAST) {
                None
            } else {
                Some(nad)
            },
            config: LinTpConfig::default(),
        }
    }

    /// Creates a transport with an explicit configuration.
    pub fn with_config(nad: u8, config: LinTpConfig) -> Result<Self> {
        config.validate()?;
        let mut transport = Self::new(nad);
        transport.config = config;
        Ok(transport)
    }

    /// Updates timing values commonly supplied by a node's LDF attributes.
    pub fn set_node_timing(
        &mut self,
        p2_min: Duration,
        st_min: Duration,
        n_as_timeout: Duration,
        n_cr_timeout: Duration,
    ) {
        self.config.p2_min = p2_min;
        self.config.st_min = st_min;
        self.config.n_as_timeout = n_as_timeout;
        self.config.n_cr_timeout = n_cr_timeout;
    }

    /// Returns the transport kind.
    pub fn tp_type(&self) -> IsoTpType {
        IsoTpType::Lin
    }

    /// Returns the largest supported message length.
    pub fn max_msg_len(&self) -> u32 {
        MAX_MSG_LEN_LIN
    }

    /// Sends one diagnostic request and optionally reassembles a response.
    pub async fn send_request<D: LinDevice + Send>(
        &mut self,
        device: &mut D,
        mut request: Vec<u8>,
        response: Option<&mut Vec<u8>>,
        await_response: bool,
    ) -> Result<MsgState> {
        self.config.validate()?;
        if request.len() > MAX_MSG_LEN_LIN as usize {
            return Ok(MsgState::ErrRequestLenExceeded);
        }
        if !await_response && request.len() > 1 {
            request[1] |= NO_RESPONSE_MASK;
        }

        let mut sender = LinTpFsm::new_sender(self.nad, request, self.config.fill_byte)?;
        loop {
            let (state, frame) = sender.next_frame();
            let Some(frame) = frame else { break };
            let tx_deadline = Instant::now() + self.config.n_as_timeout;
            let sent = device.send(self.config.request_id, &frame).await?;
            if sent != frame.len() {
                return Ok(MsgState::ErrSendRequest);
            }
            if self.config.require_tx_confirmation
                && !self
                    .wait_for_tx_confirmation(device, &frame, tx_deadline)
                    .await?
            {
                return Ok(MsgState::ErrTimeout);
            }
            if state == MsgState::Success {
                break;
            }
            if !self.config.st_min.is_zero() {
                autors_runtime::sleep(self.config.st_min).await;
            }
        }

        if !await_response || response.is_none() {
            return Ok(MsgState::Success);
        }
        if !self.config.p2_min.is_zero() {
            autors_runtime::sleep(self.config.p2_min).await;
        }

        let mut timeout = self.config.p2_timeout;
        loop {
            let (state, message) = self.receive_message(device, timeout).await?;
            if state != MsgState::Success {
                return Ok(state);
            }
            if is_response_pending(&message) {
                timeout = self.config.p2_star_timeout;
                if !self.config.p2_min.is_zero() {
                    autors_runtime::sleep(self.config.p2_min).await;
                }
                continue;
            }
            if let Some(output) = response {
                output.extend_from_slice(&message);
            }
            return Ok(MsgState::Success);
        }
    }

    async fn receive_message<D: LinDevice + Send>(
        &self,
        device: &mut D,
        first_pdu_timeout: Duration,
    ) -> Result<(MsgState, Vec<u8>)> {
        let mut receiver = LinTpFsm::new_receiver(self.response_nad);
        let mut deadline = Instant::now() + first_pdu_timeout;

        loop {
            if Instant::now() >= deadline {
                let state = if receiver.received().is_empty() {
                    MsgState::ErrTimeout
                } else {
                    MsgState::ErrTimeoutAwaitingCFFrame
                };
                return Ok((state, Vec::new()));
            }
            if !device.request(self.config.response_id).await? {
                return Ok((MsgState::ErrSendRequest, Vec::new()));
            }

            let slot_deadline = (Instant::now() + self.config.response_slot_timeout).min(deadline);
            let mut received_pdu = false;
            while Instant::now() < slot_deadline {
                match device.on_receive().await? {
                    Some(frame)
                        if frame.id == self.config.response_id
                            && frame.data.len() == crate::lin::LIN_TP_FRAME_LEN =>
                    {
                        let pdu = LinTpPdu::decode(&frame.data)?;
                        if self.response_nad.is_some_and(|nad| nad != pdu.nad()) {
                            continue;
                        }
                        let state = receiver.on_received(&frame.data)?;
                        if state == MsgState::Success {
                            return Ok((state, receiver.take_received()));
                        }
                        if state > MsgState::Success {
                            return Ok((state, Vec::new()));
                        }
                        deadline = Instant::now() + self.config.n_cr_timeout;
                        received_pdu = true;
                        break;
                    }
                    Some(_) => {}
                    None => autors_runtime::sleep(self.config.device_poll_interval).await,
                }
            }

            let delay = if received_pdu {
                self.config.st_min
            } else {
                self.config.response_poll_interval
            };
            if !delay.is_zero() {
                autors_runtime::sleep(delay).await;
            }
        }
    }

    async fn wait_for_tx_confirmation<D: LinDevice + Send>(
        &self,
        device: &mut D,
        data: &[u8],
        deadline: Instant,
    ) -> Result<bool> {
        loop {
            match device.on_receive().await? {
                Some(frame)
                    if frame.id == self.config.request_id
                        && frame.is_master_frame
                        && frame.data == data =>
                {
                    return Ok(true);
                }
                Some(_) => {}
                None => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    autors_runtime::sleep(self.config.device_poll_interval).await;
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
        }
    }
}

/// Returns whether a complete UDS response asks the client to keep waiting.
pub fn is_response_pending(message: &[u8]) -> bool {
    message.len() >= 3 && message[0] == 0x7f && message[2] == 0x78
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use async_trait::async_trait;
    use autors_lin::device::{LinConfiguration, LinFrame};

    use super::*;
    use crate::lin::segment_message;

    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingLinTp;

    struct StubDevice {
        sent: Vec<(u8, Vec<u8>)>,
        requested: Vec<u8>,
        response_frames: VecDeque<Vec<u8>>,
        rx: VecDeque<LinFrame>,
        echo_sent_frames: bool,
    }

    impl StubDevice {
        fn new(messages: &[Vec<u8>], nad: u8) -> Self {
            let mut response_frames = VecDeque::new();
            for message in messages {
                for frame in segment_message(nad, message, 0xff).unwrap() {
                    response_frames.push_back(frame.to_vec());
                }
            }
            Self {
                sent: Vec::new(),
                requested: Vec::new(),
                response_frames,
                rx: VecDeque::new(),
                echo_sent_frames: true,
            }
        }
    }

    #[async_trait]
    impl LinDevice for StubDevice {
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
            if self.echo_sent_frames {
                self.rx
                    .push_back(LinFrame::new("Stub/LIN1", id, data.to_vec(), true));
            }
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

    fn fast_transport(nad: u8) -> LinTp {
        let mut transport = LinTp::new(nad);
        transport.config.p2_min = Duration::ZERO;
        transport.config.p2_timeout = Duration::from_millis(15);
        transport.config.p2_star_timeout = Duration::from_millis(15);
        transport.config.n_cr_timeout = Duration::from_millis(15);
        transport.config.response_slot_timeout = Duration::from_millis(2);
        transport.config.response_poll_interval = Duration::from_millis(1);
        transport
    }

    #[tokio::test]
    async fn single_frame_exchange_uses_diagnostic_ids_and_nad() {
        let mut device = StubDevice::new(&[vec![0x62, 0xf1, 0x90, 0xaa]], 0x12);
        let mut transport = fast_transport(0x12);
        let mut response = Vec::new();
        let state = transport
            .send_request(
                &mut device,
                vec![0x22, 0xf1, 0x90],
                Some(&mut response),
                true,
            )
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(response, [0x62, 0xf1, 0x90, 0xaa]);
        assert_eq!(device.sent[0].0, MASTER_REQUEST_FRAME_ID);
        assert_eq!(
            device.sent[0].1,
            [0x12, 3, 0x22, 0xf1, 0x90, 0xff, 0xff, 0xff]
        );
        assert_eq!(device.requested, [SLAVE_RESPONSE_FRAME_ID]);
    }

    #[tokio::test]
    async fn segmented_request_and_response() {
        let response: Vec<u8> = (0x40..0x54).collect();
        let mut device = StubDevice::new(std::slice::from_ref(&response), 1);
        let mut transport = fast_transport(1);
        let request: Vec<u8> = (0..20).collect();
        let mut output = Vec::new();
        let state = transport
            .send_request(&mut device, request, Some(&mut output), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(device.sent.len(), 4);
        assert_eq!(device.sent[0].1[1], 0x10);
        assert_eq!(device.sent[1].1[1], 0x21);
        assert_eq!(output, response);
        assert_eq!(device.requested.len(), 4);
    }

    #[tokio::test]
    async fn response_pending_is_hidden_from_uds_layer() {
        let messages = vec![vec![0x7f, 0x22, 0x78], vec![0x62, 0xf1, 0x90]];
        let mut device = StubDevice::new(&messages, 1);
        let mut transport = fast_transport(1);
        let mut response = Vec::new();
        let state = transport
            .send_request(
                &mut device,
                vec![0x22, 0xf1, 0x90],
                Some(&mut response),
                true,
            )
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(response, [0x62, 0xf1, 0x90]);
        assert_eq!(device.requested.len(), 2);
    }

    #[tokio::test]
    async fn no_response_request_sets_suppress_bit_without_polling() {
        let mut device = StubDevice::new(&[], 1);
        let mut transport = fast_transport(1);
        let state = transport
            .send_request(&mut device, vec![0x3e, 0], None, false)
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(device.sent[0].1[2..4], [0x3e, 0x80]);
        assert!(device.requested.is_empty());
    }

    #[tokio::test]
    async fn missing_response_times_out() {
        let mut device = StubDevice::new(&[], 1);
        let mut transport = fast_transport(1);
        let mut response = Vec::new();
        let state = transport
            .send_request(&mut device, vec![0x22], Some(&mut response), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(response.is_empty());
        assert!(!device.requested.is_empty());
    }

    #[tokio::test]
    async fn missing_transmit_confirmation_hits_n_as_timeout() {
        let mut device = StubDevice::new(&[], 1);
        device.echo_sent_frames = false;
        let mut transport = fast_transport(1);
        transport.config.n_as_timeout = Duration::from_millis(5);
        let state = transport
            .send_request(&mut device, vec![0x3e, 0], None, false)
            .await
            .unwrap();
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(device.requested.is_empty());
    }

    #[tokio::test]
    async fn missing_consecutive_frame_hits_n_cr_timeout() {
        let response: Vec<u8> = (0..20).collect();
        let mut device = StubDevice::new(&[response], 1);
        device.response_frames.truncate(1);
        let mut transport = fast_transport(1);
        let mut output = Vec::new();
        let state = transport
            .send_request(&mut device, vec![0x22], Some(&mut output), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::ErrTimeoutAwaitingCFFrame);
        assert!(output.is_empty());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn blocking_facade_runs_lin_exchange() {
        let mut device = StubDevice::new(&[vec![0x62, 1]], 1);
        let mut transport = BlockingLinTp::new(fast_transport(1));
        let mut response = Vec::new();
        let state = transport
            .send_request(&mut device, vec![0x22], Some(&mut response), true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(response, [0x62, 1]);
        assert_eq!(transport.tp_type(), IsoTpType::Lin);
        assert_eq!(transport.max_msg_len(), MAX_MSG_LEN_LIN);
    }
}
