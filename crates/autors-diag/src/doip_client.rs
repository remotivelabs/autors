use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_util::helpers::TcpSocketWithTimeout;

use crate::doip::{
    Activation, BaseFrame, DiagnosticPowerModeResponse, DoIpType, EntityStatusResponse, Frame,
    ProtocolVersion, ResponseState, RoutingActivationResponse, SocketType, SrcDstFrame, SrcFrame,
    VehicleIdResponse, DEFAULT_PORT, DEFAULT_TIMEOUT_MS,
};
use crate::uds::{MsgState, UdsTransport};

const POLL_SLEEP: Duration = Duration::from_millis(1);

/// Optional selector carried by an ISO 13400 vehicle-identification request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VehicleIdentificationFilter {
    /// Discover every DoIP entity that receives the UDP request.
    #[default]
    All,
    /// Discover the entity with this six-byte entity identifier (EID).
    Eid([u8; 6]),
    /// Discover the entity with this 17-byte vehicle identification number.
    Vin([u8; 17]),
}

impl VehicleIdentificationFilter {
    fn request(&self) -> (DoIpType, Vec<u8>) {
        match self {
            Self::All => (DoIpType::VehicleIdRequest, Vec::new()),
            Self::Eid(eid) => (DoIpType::VehicleIdRequestEID, eid.to_vec()),
            Self::Vin(vin) => (DoIpType::VehicleIdRequestVIN, vin.to_vec()),
        }
    }
}

#[derive(Debug, Default)]
pub struct ResponseEvent {
    pub state: ResponseState,
    response: Option<Frame>,
    released: bool,
}

impl ResponseEvent {
    fn new() -> Self {
        Self::default()
    }

    fn restart(&mut self) {
        self.response = None;
        self.released = false;
    }

    fn on_received(&mut self, state: ResponseState, frame: Frame) {
        self.state = state;
        self.response = Some(frame);
        if matches!(state, ResponseState::Nack | ResponseState::Ok) {
            self.released = true;
        }
    }

    pub fn response(&self) -> Option<&Frame> {
        self.response.as_ref()
    }
}

pub struct DoIpClient {
    pub remote_ep: SocketAddr,
    pub local_ip: IpAddr,
    pub src_adr: u16,
    pub dst_adr: u16,
    pub activation_type: Activation,
    pub version: ProtocolVersion,
    pub timeout_ms: u32,
    pub p2_client_ms: u32,
    pub last_nack_code: u8,
    pub last_diag_nack_code: u8,
    pub reflect_diag_message_on_acknowledge: bool,
    tcp: Option<TcpStream>,
    udp: UdpSocket,
    tcp_rx: Vec<u8>,
    udp_rx: Vec<u8>,
    udp_sender: Option<SocketAddr>,
}

impl DoIpClient {
    /// Sends a UDP vehicle-identification request and collects every unique
    /// response received before `timeout_ms` elapses.
    ///
    /// `remote_ep` can be a global or directed IPv4 broadcast endpoint, or a
    /// unicast endpoint for deterministic probing. The returned entities are
    /// ordered by response arrival. Malformed or unrelated UDP datagrams are
    /// ignored so one noisy participant cannot abort a discovery pass.
    pub async fn discover(
        remote_ep: SocketAddr,
        local_ip: IpAddr,
        version: ProtocolVersion,
        filter: VehicleIdentificationFilter,
        timeout_ms: u32,
    ) -> crate::Result<Vec<EntityData>> {
        let socket =
            autors_runtime::spawn_blocking(move || UdpSocket::bind(SocketAddr::new(local_ip, 0)))
                .await?;
        socket.set_broadcast(true)?;
        socket.set_nonblocking(true)?;

        let (request_type, payload) = filter.request();
        let request = BaseFrame::new(
            version,
            request_type,
            SocketType::Dgram,
            None,
            payload,
            true,
        )
        .to_array();
        loop {
            match socket.send_to(&request, remote_ep) {
                Ok(written) if written == request.len() => break,
                Ok(written) => {
                    return Err(crate::Error::Protocol(format!(
                        "DoIP discovery sent {written} of {} bytes",
                        request.len()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    autors_runtime::sleep(POLL_SLEEP).await;
                }
                Err(error) => return Err(error.into()),
            }
        }

        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let mut entities = Vec::new();
        let mut datagram = [0u8; 4096];
        while Instant::now() < deadline {
            match socket.recv_from(&mut datagram) {
                Ok((received, sender)) => {
                    let mut remaining = &datagram[..received];
                    while !remaining.is_empty() {
                        let decoded = Frame::decode(remaining, SocketType::Dgram, Some(sender));
                        let Ok(Some((frame, consumed))) = decoded else {
                            break;
                        };
                        if frame.base().msg_type() == Some(DoIpType::VehicleIdResponse) {
                            let response = VehicleIdResponse::parse(frame.base());
                            let duplicate = entities.iter().any(|entity: &EntityData| {
                                entity.vehicle_id.sender == response.sender
                                    && entity.vehicle_id.logical_adr == response.logical_adr
                                    && entity.vehicle_id.eid == response.eid
                            });
                            if !duplicate {
                                entities.push(EntityData::new(response));
                            }
                        }
                        remaining = &remaining[consumed..];
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    autors_runtime::sleep(POLL_SLEEP).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(entities)
    }

    pub async fn connect(
        remote_ip: IpAddr,
        local_ip: IpAddr,
        activation_type: Activation,
        src_adr: u16,
        dst_adr: u16,
        version: ProtocolVersion,
    ) -> crate::Result<Self> {
        Self::connect_to(
            SocketAddr::new(remote_ip, DEFAULT_PORT),
            local_ip,
            activation_type,
            src_adr,
            dst_adr,
            version,
        )
        .await
    }

    pub async fn connect_to(
        remote_ep: SocketAddr,
        local_ip: IpAddr,
        activation_type: Activation,
        src_adr: u16,
        dst_adr: u16,
        version: ProtocolVersion,
    ) -> crate::Result<Self> {
        let udp =
            autors_runtime::spawn_blocking(move || UdpSocket::bind(SocketAddr::new(local_ip, 0)))
                .await?;
        udp.set_nonblocking(true)?;
        let tcp = autors_runtime::spawn_blocking(move || {
            TcpSocketWithTimeout::connect_default(&remote_ep)
        })
        .await?;
        tcp.set_nonblocking(true)?;
        Ok(Self {
            remote_ep,
            local_ip,
            src_adr,
            dst_adr,
            activation_type,
            version,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            p2_client_ms: 50,
            last_nack_code: 0xFF,
            last_diag_nack_code: 0xFF,
            reflect_diag_message_on_acknowledge: false,
            tcp: Some(tcp),
            udp,
            tcp_rx: Vec::new(),
            udp_rx: Vec::new(),
            udp_sender: None,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.tcp.is_some()
    }

    fn effective_timeout(&self, timeout_ms: u32) -> Duration {
        Duration::from_millis(u64::from(if timeout_ms > 0 {
            timeout_ms
        } else {
            self.timeout_ms
        }))
    }

    async fn ensure_tcp_comm(&mut self) {
        if self.tcp.is_some() {
            return;
        }
        let remote_ep = self.remote_ep;
        let connected = autors_runtime::spawn_blocking(move || {
            TcpSocketWithTimeout::connect_default(&remote_ep)
        })
        .await;
        if let Ok(tcp) = connected {
            let _ = tcp.set_nonblocking(true);
            self.tcp = Some(tcp);
            let mut payload = vec![self.activation_type.to_num()];
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(&0u32.to_be_bytes());
            match self.routing_activation_inner(payload, 0).await {
                (ResponseState::Ok, Some(resp))
                    if resp.activation_result == crate::doip::ActivationCode::RoutingActivated => {}
                _ => self.tcp = None,
            }
        }
    }

    async fn send_bytes(&mut self, bytes: &[u8], via_udp: bool) -> bool {
        if via_udp {
            loop {
                match self.udp.send_to(bytes, self.remote_ep) {
                    Ok(n) => return n == bytes.len(),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        autors_runtime::sleep(POLL_SLEEP).await;
                    }
                    Err(_) => return false,
                }
            }
        }
        let Some(tcp) = &mut self.tcp else {
            return false;
        };
        let mut sent = 0;
        while sent < bytes.len() {
            match tcp.write(&bytes[sent..]) {
                Ok(0) => break,
                Ok(n) => sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    autors_runtime::sleep(POLL_SLEEP).await;
                }
                Err(_) => break,
            }
        }
        if sent == bytes.len() {
            true
        } else {
            self.tcp = None;
            false
        }
    }

    async fn poll_once(&mut self, via_udp: bool) -> bool {
        let mut chunk = [0u8; 4096];
        if via_udp {
            match self.udp.recv_from(&mut chunk) {
                Ok((n, sender)) => {
                    self.udp_sender = Some(sender);
                    self.udp_rx.extend_from_slice(&chunk[..n]);
                    true
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    autors_runtime::sleep(POLL_SLEEP).await;
                    true
                }
                Err(_) => false,
            }
        } else {
            let Some(tcp) = &mut self.tcp else {
                return false;
            };
            match tcp.read(&mut chunk) {
                Ok(0) => false,
                Ok(n) => {
                    self.tcp_rx.extend_from_slice(&chunk[..n]);
                    true
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    autors_runtime::sleep(POLL_SLEEP).await;
                    true
                }
                Err(_) => false,
            }
        }
    }

    fn drain_frames(&mut self, via_udp: bool) -> Vec<Frame> {
        let socket_type = if via_udp {
            SocketType::Dgram
        } else {
            SocketType::Stream
        };
        let sender = if via_udp {
            self.udp_sender.unwrap_or(self.remote_ep)
        } else {
            self.remote_ep
        };
        let local_udp = self.udp.local_addr().ok();
        let buf = if via_udp {
            &mut self.udp_rx
        } else {
            &mut self.tcp_rx
        };
        let mut out = Vec::new();
        loop {
            match Frame::decode(buf, socket_type, Some(sender)) {
                Ok(Some((frame, consumed))) => {
                    buf.drain(..consumed);
                    if via_udp && Some(sender) == local_udp {
                        continue;
                    }
                    out.push(frame);
                }
                Ok(None) => break,
                // doip.rs `Frame::decode`).
                Err(_) => {
                    buf.clear();
                    break;
                }
            }
        }
        out
    }

    async fn dispatch(&mut self, frame: Frame, via_udp: bool, event: &mut ResponseEvent) {
        let socket_type = if via_udp {
            SocketType::Dgram
        } else {
            SocketType::Stream
        };
        match frame.base().msg_type() {
            Some(DoIpType::Nack) => {
                self.last_nack_code = frame.base().data.first().copied().unwrap_or(0xFF);
                return;
            }
            Some(DoIpType::DiagnosticMessageNack) => {
                self.last_diag_nack_code = frame.base().data.first().copied().unwrap_or(0xFF);
            }
            _ => {}
        }
        match frame.base().msg_type() {
            Some(DoIpType::DiagnosticMessageAck) => {
                event.on_received(ResponseState::Ack, frame);
            }
            Some(DoIpType::DiagnosticMessageNack) => {
                event.on_received(ResponseState::Nack, frame);
            }
            Some(DoIpType::EntityStatusResponse) | Some(DoIpType::DiagnosticPowerModeResponse) => {
                event.on_received(ResponseState::Ok, frame);
            }
            Some(DoIpType::DiagnosticMessage) => {
                if let Frame::SrcDst(request) = &frame {
                    let mut ack_data = vec![0u8];
                    if self.reflect_diag_message_on_acknowledge {
                        ack_data.extend_from_slice(&request.src.base.data);
                    }
                    let ack = SrcDstFrame::reply(
                        request,
                        DoIpType::DiagnosticMessageAck,
                        socket_type,
                        None,
                        ack_data,
                    );
                    let _ = self.send_bytes(&ack.to_array(), via_udp).await;
                }
                event.on_received(ResponseState::Ok, frame);
            }
            Some(DoIpType::VehicleIdResponse) => {
                let _ = VehicleIdResponse::parse(frame.base());
            }
            Some(DoIpType::RoutingActivationResponse) => {
                event.on_received(ResponseState::Ok, frame);
            }
            Some(DoIpType::AliveCheckRequest) => {
                let resp = SrcFrame::new(
                    self.version,
                    DoIpType::AliveCheckResponse,
                    self.src_adr,
                    socket_type,
                    None,
                    Vec::new(),
                    true,
                );
                let _ = self.send_bytes(&resp.to_array(), via_udp).await;
            }
            _ => {}
        }
    }

    async fn send_and_wait(
        &mut self,
        bytes: &[u8],
        timeout: Duration,
        via_udp: bool,
    ) -> (ResponseState, Option<Frame>) {
        let mut event = ResponseEvent::new();
        event.restart();
        if !self.send_bytes(bytes, via_udp).await {
            return (ResponseState::SendFailed, None);
        }
        if timeout.is_zero() {
            return (ResponseState::Timeout, None);
        }
        let deadline = Instant::now() + timeout;
        let mut closed = false;
        loop {
            if !closed {
                closed = !self.poll_once(via_udp).await;
            } else {
                autors_runtime::sleep(POLL_SLEEP).await;
            }
            let frames = self.drain_frames(via_udp);
            for frame in frames {
                self.dispatch(frame, via_udp, &mut event).await;
            }
            if event.released {
                break;
            }
            if Instant::now() >= deadline {
                return (ResponseState::Timeout, None);
            }
        }
        let state = event.state;
        let response = event.response.filter(|f| {
            matches!(
                f.base().msg_type(),
                Some(
                    DoIpType::VehicleIdResponse
                        | DoIpType::RoutingActivationResponse
                        | DoIpType::AliveCheckResponse
                        | DoIpType::EntityStatusResponse
                        | DoIpType::DiagnosticPowerModeResponse
                        | DoIpType::DiagnosticMessage
                )
            )
        });
        (state, response)
    }

    pub async fn routing_activation(
        &mut self,
        activation: Activation,
        reserved_by_iso: u32,
        reserved_by_oem: u32,
        timeout_ms: u32,
    ) -> (ResponseState, Option<RoutingActivationResponse>) {
        let mut payload = vec![activation.to_num()];
        payload.extend_from_slice(&reserved_by_iso.to_be_bytes());
        payload.extend_from_slice(&reserved_by_oem.to_be_bytes());
        self.ensure_tcp_comm().await;
        self.routing_activation_inner(payload, timeout_ms).await
    }

    async fn routing_activation_inner(
        &mut self,
        payload: Vec<u8>,
        timeout_ms: u32,
    ) -> (ResponseState, Option<RoutingActivationResponse>) {
        let frame = SrcFrame::new(
            self.version,
            DoIpType::RoutingActivationRequest,
            self.src_adr,
            SocketType::Stream,
            None,
            payload,
            true,
        );
        let (state, resp) = self
            .send_and_wait(&frame.to_array(), self.effective_timeout(timeout_ms), false)
            .await;
        let parsed = resp.and_then(|f| RoutingActivationResponse::parse(f.base()).ok());
        (state, parsed)
    }

    pub async fn entity_status(
        &mut self,
        timeout_ms: u32,
    ) -> (ResponseState, Option<EntityStatusResponse>) {
        let frame = BaseFrame::new(
            self.version,
            DoIpType::EntityStatusRequest,
            SocketType::Dgram,
            None,
            Vec::new(),
            true,
        );
        let (state, resp) = self
            .send_and_wait(&frame.to_array(), self.effective_timeout(timeout_ms), true)
            .await;
        let parsed = resp.and_then(|f| EntityStatusResponse::parse(f.base()).ok());
        (state, parsed)
    }

    pub async fn diagnostic_power_mode(
        &mut self,
        timeout_ms: u32,
    ) -> (ResponseState, Option<DiagnosticPowerModeResponse>) {
        let frame = BaseFrame::new(
            self.version,
            DoIpType::DiagnosticPowerModeRequest,
            SocketType::Dgram,
            None,
            Vec::new(),
            true,
        );
        let (state, resp) = self
            .send_and_wait(&frame.to_array(), self.effective_timeout(timeout_ms), true)
            .await;
        let parsed = resp.and_then(|f| DiagnosticPowerModeResponse::parse(f.base()).ok());
        (state, parsed)
    }

    pub async fn diagnose_request(
        &mut self,
        p2_client_ms: u32,
        req_data: &[u8],
        res_data: &mut Vec<u8>,
    ) -> MsgState {
        self.ensure_tcp_comm().await;
        if self.tcp.is_none() {
            return MsgState::ErrTimeout;
        }
        let frame = SrcDstFrame::new(
            self.version,
            DoIpType::DiagnosticMessage,
            self.src_adr,
            self.dst_adr,
            SocketType::Stream,
            None,
            req_data.to_vec(),
            true,
        );
        let (state, resp) = self
            .send_and_wait(
                &frame.to_array(),
                Duration::from_millis(u64::from(p2_client_ms)),
                false,
            )
            .await;
        match state {
            ResponseState::Ok => {
                if let Some(f) = resp {
                    res_data.extend_from_slice(&f.base().data);
                }
                MsgState::Success
            }
            ResponseState::Nack | ResponseState::Ack => MsgState::ErrGeneric,
            ResponseState::Timeout => MsgState::ErrTimeout,
            ResponseState::SendFailed => MsgState::ErrSendRequest,
        }
    }
}

#[async_trait]
impl UdsTransport for DoIpClient {
    /// `DoIPClient.DiagnoseRequest((int)P2Client, reqData, resData)`.
    async fn send_request(&mut self, request: &[u8], response: Option<&mut Vec<u8>>) -> MsgState {
        let mut buf = Vec::new();
        let state = self
            .diagnose_request(self.p2_client_ms, request, &mut buf)
            .await;
        if state == MsgState::Success {
            if let Some(out) = response {
                out.extend_from_slice(&buf);
            }
        }
        state
    }
}

pub struct EntityData {
    pub vehicle_id: VehicleIdResponse,
    pub entity_status: Option<EntityStatusResponse>,
    pub power_mode: Option<DiagnosticPowerModeResponse>,
    pub client: Option<DoIpClient>,
}

impl EntityData {
    pub fn new(vehicle_id: VehicleIdResponse) -> Self {
        Self {
            vehicle_id,
            entity_status: None,
            power_mode: None,
            client: None,
        }
    }
}

impl std::fmt::Display for EntityData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})",
            self.vehicle_id.vin, self.vehicle_id.logical_adr
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "blocking")]
    use crate::blocking::{BlockingDoIpClient, BlockingUdsClient};
    use crate::doip::{
        ActivationCode, DiagMsgNackCode, NackCode, NodeType, PowerMode, DEFAULT_ENTITY_ADR,
        DEFAULT_TESTER_ADR,
    };
    use crate::uds::{Sid, UdsClient};
    use std::net::TcpListener;
    use std::thread;

    const V2019: ProtocolVersion = ProtocolVersion::Iso13400_2019;

    struct MockEntity {
        addr: SocketAddr,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockEntity {
        fn start(script: impl FnOnce(TcpStream, UdpSocket) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let udp = UdpSocket::bind(addr).unwrap();
            udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let handle = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                script(stream, udp);
            });
            Self {
                addr,
                handle: Some(handle),
            }
        }
    }

    impl Drop for MockEntity {
        fn drop(&mut self) {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn read_frame(stream: &mut TcpStream) -> Frame {
        let mut hdr = [0u8; 8];
        stream.read_exact(&mut hdr).unwrap();
        let len = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).unwrap();
        let mut buf = hdr.to_vec();
        buf.extend_from_slice(&payload);
        Frame::decode(&buf, SocketType::Stream, None)
            .unwrap()
            .expect("complete frame")
            .0
    }

    fn write_frame(stream: &mut TcpStream, frame: &Frame) {
        stream.write_all(&frame.to_array()).unwrap();
    }

    fn expect_routing_activation(stream: &mut TcpStream, activation_code: u8) {
        let frame = read_frame(stream);
        let Frame::Src(req) = &frame else {
            panic!("expected Src frame, got {frame:?}");
        };
        assert_eq!(
            req.base.msg_type(),
            Some(DoIpType::RoutingActivationRequest)
        );
        assert_eq!(req.src_adr, DEFAULT_TESTER_ADR);
        assert_eq!(req.base.data, vec![0x00, 0, 0, 0, 0, 0, 0, 0, 0]);
        let resp = SrcDstFrame::new(
            V2019,
            DoIpType::RoutingActivationResponse,
            DEFAULT_ENTITY_ADR,
            DEFAULT_TESTER_ADR,
            SocketType::Stream,
            None,
            vec![activation_code],
            false,
        );
        write_frame(stream, &Frame::SrcDst(resp));
    }

    fn expect_diag_and_respond(stream: &mut TcpStream, expected_req: &[u8], resp_data: &[u8]) {
        let frame = read_frame(stream);
        let Frame::SrcDst(req) = &frame else {
            panic!("expected SrcDst frame, got {frame:?}");
        };
        assert_eq!(req.src.base.msg_type(), Some(DoIpType::DiagnosticMessage));
        assert_eq!(req.src.src_adr, DEFAULT_TESTER_ADR);
        assert_eq!(req.dst_adr, DEFAULT_ENTITY_ADR);
        assert_eq!(req.src.base.data, expected_req);
        let ack = SrcDstFrame::reply(
            req,
            DoIpType::DiagnosticMessageAck,
            SocketType::Stream,
            None,
            vec![0x00],
        );
        write_frame(stream, &Frame::SrcDst(ack));
        let resp = SrcDstFrame::reply(
            req,
            DoIpType::DiagnosticMessage,
            SocketType::Stream,
            None,
            resp_data.to_vec(),
        );
        write_frame(stream, &Frame::SrcDst(resp));
        let frame = read_frame(stream);
        let Frame::SrcDst(client_ack) = &frame else {
            panic!("expected SrcDst frame, got {frame:?}");
        };
        assert_eq!(
            client_ack.src.base.msg_type(),
            Some(DoIpType::DiagnosticMessageAck)
        );
        assert_eq!(client_ack.src.base.data, vec![0x00]);
    }

    #[cfg(feature = "blocking")]
    fn client_to(addr: SocketAddr) -> BlockingDoIpClient {
        BlockingDoIpClient::connect_to(
            addr,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            Activation::Default,
            DEFAULT_TESTER_ADR,
            DEFAULT_ENTITY_ADR,
            V2019,
        )
        .unwrap()
    }

    async fn async_client_to(addr: SocketAddr) -> DoIpClient {
        DoIpClient::connect_to(
            addr,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            Activation::Default,
            DEFAULT_TESTER_ADR,
            DEFAULT_ENTITY_ADR,
            V2019,
        )
        .await
        .unwrap()
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn routing_activation_and_diagnose_roundtrip() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            expect_diag_and_respond(
                &mut stream,
                &[0x22, 0xF1, 0x90],
                &[0x62, 0xF1, 0x90, 0xAA, 0xBB],
            );
        });
        let mut client = client_to(entity.addr);
        assert!(client.is_connected());
        let (state, resp) = client.routing_activation(Activation::Default, 0, 0, 0);
        assert_eq!(state, ResponseState::Ok);
        assert_eq!(
            resp.unwrap().activation_result,
            ActivationCode::RoutingActivated
        );
        let mut res = Vec::new();
        let state = client.diagnose_request(500, &[0x22, 0xF1, 0x90], &mut res);
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]);
        assert_eq!(client.0.last_nack_code, NackCode::None.to_num());
        assert_eq!(client.0.last_diag_nack_code, DiagMsgNackCode::None.to_num());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn routing_activation_denied_is_ok_with_code() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x02);
        });
        let mut client = client_to(entity.addr);
        let (state, resp) = client.routing_activation(Activation::Default, 0, 0, 0);
        assert_eq!(state, ResponseState::Ok);
        assert_eq!(
            resp.unwrap().activation_result,
            ActivationCode::DeniedDifferentSA
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn uds_client_over_doip() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            expect_diag_and_respond(&mut stream, &[0x22, 0xF1, 0x90], &[0x62, 0xF1, 0x90, 0x57]);
        });
        let mut client =
            BlockingUdsClient::new(UdsClient::new(client_to(entity.addr).into_inner()));
        let (state, resp) = autors_runtime::block_on(client.0.transport.routing_activation(
            Activation::Default,
            0,
            0,
            0,
        ));
        assert_eq!(state, ResponseState::Ok);
        assert!(resp.is_some());
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("positive response");
        assert_eq!(resp.base.service_id, Sid::ReadDataByIdentifier.as_value());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn diag_message_nack_maps_err_generic() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            let frame = read_frame(&mut stream);
            let Frame::SrcDst(req) = &frame else {
                panic!("expected SrcDst frame");
            };
            let nack = SrcDstFrame::reply(
                req,
                DoIpType::DiagnosticMessageNack,
                SocketType::Stream,
                None,
                vec![0x03], // UnknownTA
            );
            write_frame(&mut stream, &Frame::SrcDst(nack));
        });
        let mut client = client_to(entity.addr);
        client.routing_activation(Activation::Default, 0, 0, 0);
        let mut res = Vec::new();
        let state = client.diagnose_request(500, &[0x22, 0xF1, 0x90], &mut res);
        assert_eq!(state, MsgState::ErrGeneric);
        assert_eq!(
            client.0.last_diag_nack_code,
            DiagMsgNackCode::UnknownTA.to_num()
        );
        assert!(res.is_empty());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn generic_header_nack_records_code_and_times_out() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            let _req = read_frame(&mut stream);
            let nack = BaseFrame::new(
                V2019,
                DoIpType::Nack,
                SocketType::Stream,
                None,
                vec![0x01], // UnknownPayloadType
                false,
            );
            write_frame(&mut stream, &Frame::Base(nack));
        });
        let mut client = client_to(entity.addr);
        client.routing_activation(Activation::Default, 0, 0, 0);
        let mut res = Vec::new();
        let start = Instant::now();
        let state = client.diagnose_request(150, &[0x22, 0xF1, 0x90], &mut res);
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(start.elapsed() >= Duration::from_millis(140));
        assert_eq!(
            client.0.last_nack_code,
            NackCode::UnknownPayloadType.to_num()
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn entity_status_and_power_mode_over_udp() {
        let entity = MockEntity::start(|_stream, udp| {
            let mut buf = [0u8; 1024];
            let (n, sender) = udp.recv_from(&mut buf).unwrap();
            let (frame, _) = Frame::decode(&buf[..n], SocketType::Dgram, Some(sender))
                .unwrap()
                .expect("complete frame");
            assert_eq!(frame.base().msg_type(), Some(DoIpType::EntityStatusRequest));
            let resp = BaseFrame::new(
                V2019,
                DoIpType::EntityStatusResponse,
                SocketType::Dgram,
                None,
                vec![0x00, 0x10, 0x02, 0, 0, 0x10, 0x00],
                false,
            );
            udp.send_to(&resp.to_array(), sender).unwrap();
            let (n, sender) = udp.recv_from(&mut buf).unwrap();
            let (frame, _) = Frame::decode(&buf[..n], SocketType::Dgram, Some(sender))
                .unwrap()
                .expect("complete frame");
            assert_eq!(
                frame.base().msg_type(),
                Some(DoIpType::DiagnosticPowerModeRequest)
            );
            let resp = BaseFrame::new(
                V2019,
                DoIpType::DiagnosticPowerModeResponse,
                SocketType::Dgram,
                None,
                vec![0x01],
                false,
            );
            udp.send_to(&resp.to_array(), sender).unwrap();
        });
        let mut client = client_to(entity.addr);
        let (state, status) = client.entity_status(500);
        assert_eq!(state, ResponseState::Ok);
        let status = status.unwrap();
        assert_eq!(status.node_type, NodeType::Gateway);
        assert_eq!(status.max_sockets, 16);
        assert_eq!(status.open_sockets, 2);
        assert_eq!(status.max_data_size, 4096);
        let (state, mode) = client.diagnostic_power_mode(500);
        assert_eq!(state, ResponseState::Ok);
        assert_eq!(mode.unwrap().power_mode, PowerMode::Ready);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn alive_check_request_is_answered() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            let frame = read_frame(&mut stream);
            let Frame::SrcDst(req) = &frame else {
                panic!("expected SrcDst frame");
            };
            let alive = BaseFrame::new(
                V2019,
                DoIpType::AliveCheckRequest,
                SocketType::Stream,
                None,
                Vec::new(),
                false,
            );
            write_frame(&mut stream, &Frame::Base(alive));
            let frame = read_frame(&mut stream);
            let Frame::Src(resp) = &frame else {
                panic!("expected Src frame, got {frame:?}");
            };
            assert_eq!(resp.base.msg_type(), Some(DoIpType::AliveCheckResponse));
            assert_eq!(resp.src_adr, DEFAULT_TESTER_ADR);
            let resp = SrcDstFrame::reply(
                req,
                DoIpType::DiagnosticMessage,
                SocketType::Stream,
                None,
                vec![0x62, 0x01],
            );
            write_frame(&mut stream, &Frame::SrcDst(resp));
            let _ack = read_frame(&mut stream);
        });
        let mut client = client_to(entity.addr);
        client.routing_activation(Activation::Default, 0, 0, 0);
        let mut res = Vec::new();
        let state = client.diagnose_request(500, &[0x22, 0x01], &mut res);
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0x01]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn silent_entity_times_out() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            let _req = read_frame(&mut stream);
            thread::sleep(Duration::from_millis(300));
        });
        let mut client = client_to(entity.addr);
        client.routing_activation(Activation::Default, 0, 0, 0);
        let mut res = Vec::new();
        let start = Instant::now();
        let state = client.diagnose_request(120, &[0x22, 0xF1, 0x90], &mut res);
        assert_eq!(state, MsgState::ErrTimeout);
        assert!(start.elapsed() >= Duration::from_millis(110));
    }

    #[test]
    fn entity_data_display() {
        let mut payload = b"WVWZZZ3CZWE123456".to_vec();
        payload.extend_from_slice(&[0x10, 0x01]);
        let frame = BaseFrame::new(
            V2019,
            DoIpType::VehicleIdResponse,
            SocketType::Dgram,
            None,
            payload,
            false,
        );
        let data = EntityData::new(VehicleIdResponse::parse(&frame));
        assert_eq!(data.to_string(), "WVWZZZ3CZWE123456 (4097)");
        assert!(data.entity_status.is_none());
        assert!(data.client.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discovers_and_deduplicates_vehicle_identification_responses() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let endpoint = socket.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut request = [0u8; 128];
            let (received, tester) = socket.recv_from(&mut request).unwrap();
            let (frame, consumed) =
                Frame::decode(&request[..received], SocketType::Dgram, Some(tester))
                    .unwrap()
                    .unwrap();
            assert_eq!(consumed, received);
            assert_eq!(frame.base().msg_type(), Some(DoIpType::VehicleIdRequestEID));
            assert_eq!(frame.base().data, [1, 2, 3, 4, 5, 6]);

            let response = |logical_adr: u16, eid: [u8; 6]| {
                let mut payload = b"12345678901234567".to_vec();
                payload.extend_from_slice(&logical_adr.to_be_bytes());
                payload.extend_from_slice(&eid);
                payload.extend_from_slice(&[7, 8, 9, 10, 11, 12]);
                payload.extend_from_slice(&[0x10, 0x00]);
                BaseFrame::new(
                    V2019,
                    DoIpType::VehicleIdResponse,
                    SocketType::Dgram,
                    None,
                    payload,
                    false,
                )
                .to_array()
            };
            let first = response(0x1001, [1, 2, 3, 4, 5, 6]);
            socket.send_to(&first, tester).unwrap();
            socket.send_to(&first, tester).unwrap();
            socket
                .send_to(&response(0x1002, [6, 5, 4, 3, 2, 1]), tester)
                .unwrap();
        });

        let entities = DoIpClient::discover(
            endpoint,
            IpAddr::from([127, 0, 0, 1]),
            V2019,
            VehicleIdentificationFilter::Eid([1, 2, 3, 4, 5, 6]),
            100,
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].vehicle_id.logical_adr, 0x1001);
        assert_eq!(entities[1].vehicle_id.logical_adr, 0x1002);
        assert_eq!(entities[0].vehicle_id.vin, "12345678901234567");
        assert_eq!(entities[0].vehicle_id.sender, Some(endpoint));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_routing_activation_and_diagnose_roundtrip() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            expect_diag_and_respond(
                &mut stream,
                &[0x22, 0xF1, 0x90],
                &[0x62, 0xF1, 0x90, 0xAA, 0xBB],
            );
        });
        let mut client = async_client_to(entity.addr).await;
        assert!(client.is_connected());
        let (state, resp) = client
            .routing_activation(Activation::Default, 0, 0, 0)
            .await;
        assert_eq!(state, ResponseState::Ok);
        assert_eq!(
            resp.unwrap().activation_result,
            ActivationCode::RoutingActivated
        );
        let mut res = Vec::new();
        let state = client
            .diagnose_request(500, &[0x22, 0xF1, 0x90], &mut res)
            .await;
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]);
        assert_eq!(client.last_nack_code, NackCode::None.to_num());
        assert_eq!(client.last_diag_nack_code, DiagMsgNackCode::None.to_num());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_entity_status_and_power_mode_over_udp() {
        let entity = MockEntity::start(|_stream, udp| {
            let mut buf = [0u8; 1024];
            let (n, sender) = udp.recv_from(&mut buf).unwrap();
            let (frame, _) = Frame::decode(&buf[..n], SocketType::Dgram, Some(sender))
                .unwrap()
                .expect("complete frame");
            assert_eq!(frame.base().msg_type(), Some(DoIpType::EntityStatusRequest));
            let resp = BaseFrame::new(
                V2019,
                DoIpType::EntityStatusResponse,
                SocketType::Dgram,
                None,
                vec![0x00, 0x10, 0x02, 0, 0, 0x10, 0x00],
                false,
            );
            udp.send_to(&resp.to_array(), sender).unwrap();
            let (n, sender) = udp.recv_from(&mut buf).unwrap();
            let (frame, _) = Frame::decode(&buf[..n], SocketType::Dgram, Some(sender))
                .unwrap()
                .expect("complete frame");
            assert_eq!(
                frame.base().msg_type(),
                Some(DoIpType::DiagnosticPowerModeRequest)
            );
            let resp = BaseFrame::new(
                V2019,
                DoIpType::DiagnosticPowerModeResponse,
                SocketType::Dgram,
                None,
                vec![0x01],
                false,
            );
            udp.send_to(&resp.to_array(), sender).unwrap();
        });
        let mut client = async_client_to(entity.addr).await;
        let (state, status) = client.entity_status(500).await;
        assert_eq!(state, ResponseState::Ok);
        let status = status.unwrap();
        assert_eq!(status.node_type, NodeType::Gateway);
        assert_eq!(status.max_sockets, 16);
        assert_eq!(status.open_sockets, 2);
        assert_eq!(status.max_data_size, 4096);
        let (state, mode) = client.diagnostic_power_mode(500).await;
        assert_eq!(state, ResponseState::Ok);
        assert_eq!(mode.unwrap().power_mode, PowerMode::Ready);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_uds_client_over_doip() {
        let entity = MockEntity::start(|mut stream, _udp| {
            expect_routing_activation(&mut stream, 0x10);
            expect_diag_and_respond(&mut stream, &[0x22, 0xF1, 0x90], &[0x62, 0xF1, 0x90, 0x57]);
        });
        let mut client = UdsClient::new(async_client_to(entity.addr).await);
        let (state, resp) = client
            .transport
            .routing_activation(Activation::Default, 0, 0, 0)
            .await;
        assert_eq!(state, ResponseState::Ok);
        assert!(resp.is_some());
        let (state, resp) = client.read_data_by_identifier(&[0xF190]).await.unwrap();
        assert_eq!(state, MsgState::Success);
        let resp = resp.expect("positive response");
        assert_eq!(resp.base.service_id, Sid::ReadDataByIdentifier.as_value());
    }
}
