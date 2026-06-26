//! XCP over UDP/TCP Ethernet transport (`std::net` implementation).
//! Both transports share a common design: an `XCPReceiveBuffer` configured as
//! `(CTR_WORD, _8_BIT)` plus a remote endpoint; the frame header format is
//! always [`XcpHeaderLen::CTR_WORD`]. See [`TcpXcpTransport`] and
//! [`UdpXcpTransport`].
//! Design notes:
//! - Reception follows the crate-wide polling model: [`XcpTransport::poll`]
//!   drains the socket non-blockingly (the socket is set nonblocking right
//!   after creation; `tokio::net` is deliberately not used, matching the
//!   autors-diag DoIP approach). Truly blocking calls (`UdpSocket::bind`/
//!   `TcpSocketWithTimeout::connect_default`) are moved off the executor via
//!   `autors_runtime::spawn_blocking` (sockets are Send, so they are created
//!   inside the closure and returned); `WouldBlock` on non-blocking I/O is
//!   retried with `autors_runtime::sleep(1ms)` plus a deadline.
//! - UDP uses the system default TTL; TCP reuses [`TcpSocketWithTimeout`]
//!   (TTL 32, connect timeout 200 ms, NoDelay enabled).
//! - On TCP EOF (peer closed) the local socket is closed and the next send
//!   lazily reconnects; i.e. the connection is only ever rebuilt on the send
//!   path (intentional behavior).

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use autors_util::helpers::TcpSocketWithTimeout;

use crate::ifdata_xcp::{XcpAlignment, XcpHeaderLen};
use crate::xcp::{XcpFrame, XcpReceiveBuffer, XcpTransport, XcpType};
use crate::Result;

/// Idle async sleep for the non-blocking poll loop (same as autors-diag DoIP's `POLL_SLEEP`).
const POLL_SLEEP: Duration = Duration::from_millis(1);

/// Non-blocking write-all (TCP send path; XCP frames are small, on WouldBlock
/// yield asynchronously and wait, with a 1 s upper bound).
async fn write_all_wait(mut stream: &TcpStream, data: &[u8]) -> std::io::Result<()> {
    let mut off = 0;
    let deadline = Instant::now() + Duration::from_secs(1);
    while off < data.len() {
        match stream.write(&data[off..]) {
            Ok(0) => return Err(std::io::Error::from(ErrorKind::WriteZero)),
            Ok(n) => off += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                autors_runtime::sleep(POLL_SLEEP).await;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Common receive-side state for the Ethernet transports:
/// CTR_WORD receive buffer + queue of assembled frames.
struct FrameSink {
    recv: XcpReceiveBuffer,
    queue: VecDeque<XcpFrame>,
    type_: XcpType,
}

impl FrameSink {
    fn new(type_: XcpType) -> Result<Self> {
        Ok(Self {
            recv: XcpReceiveBuffer::new(XcpHeaderLen::CTR_WORD, XcpAlignment::_8_BIT)?,
            queue: VecDeque::new(),
            type_,
        })
    }

    /// Framing loop: after feeding a chunk of data, keep draining with an
    /// empty slice until the buffer holds no complete frame.
    fn feed(&mut self, source: &str, mut data: &[u8]) {
        while let Some(f) = self.recv.get_frame_from_data(self.type_, source, data) {
            self.queue.push_back(f);
            data = &[];
        }
    }

    fn reset(&mut self) {
        self.recv.reset();
        self.queue.clear();
    }
}

/// XCP-over-UDP transport.
/// Binds `Any:0` lazily on the first send; the local endpoint string serves as
/// the frame source identifier. Each datagram is fed into the CTR_WORD receive
/// buffer for frame reassembly. A failed send returns 0 and keeps the socket.
pub struct UdpXcpTransport {
    remote: SocketAddr,
    socket: Option<UdpSocket>,
    source: String,
    sink: FrameSink,
}

impl UdpXcpTransport {
    /// Creates a transport for `remote` (the socket is not created until the first send).
    pub fn new(remote: SocketAddr) -> Result<Self> {
        Ok(Self {
            remote,
            socket: None,
            source: String::new(),
            sink: FrameSink::new(XcpType::Udp)?,
        })
    }

    /// Lazily creates the UDP socket (binds `Any:0`, records the local endpoint).
    /// `bind` is a genuinely blocking syscall, so it is moved off the executor
    /// via `spawn_blocking`.
    async fn ensure_socket(&mut self) -> bool {
        if self.socket.is_none() {
            let any = match self.remote {
                SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            };
            match autors_runtime::spawn_blocking(move || UdpSocket::bind(any)).await {
                Ok(s) => {
                    if s.set_nonblocking(true).is_err() {
                        return false;
                    }
                    self.source = s.local_addr().map(|a| a.to_string()).unwrap_or_default();
                    self.socket = Some(s);
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// Closes the socket.
    pub fn close(&mut self) {
        self.socket = None;
    }

    /// Returns the remote endpoint.
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }
}

#[async_trait]
impl XcpTransport for UdpXcpTransport {
    fn source(&self) -> &str {
        &self.source
    }

    fn frame_fmt(&self) -> XcpHeaderLen {
        XcpHeaderLen::CTR_WORD
    }

    fn reset(&mut self) {
        self.sink.reset();
    }

    /// Sends one datagram; returns 0 on length mismatch or error (the socket
    /// is kept). WouldBlock on the non-blocking socket is retried with
    /// `sleep(1ms)` plus a 1 s deadline (same as the TCP write path).
    async fn send_bytes(&mut self, data: &[u8]) -> usize {
        if !self.ensure_socket().await {
            return 0;
        }
        let remote = self.remote;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let result = self.socket.as_ref().map(|s| s.send_to(data, remote));
            match result {
                Some(Ok(n)) => return if n == data.len() { n } else { 0 },
                Some(Err(e)) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return 0;
                    }
                    autors_runtime::sleep(POLL_SLEEP).await;
                }
                _ => return 0,
            }
        }
    }

    fn next_frame(&mut self) -> Option<XcpFrame> {
        self.sink.queue.pop_front()
    }

    /// Drains the socket non-blockingly, framing each datagram into the queue.
    async fn poll(&mut self) {
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        if let Some(socket) = self.socket.as_ref() {
            let mut buf = [0u8; 65536];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((0, _)) => break,
                    Ok((n, _)) => chunks.push(buf[..n].to_vec()),
                    Err(_) => break,
                }
            }
        }
        let source = self.source.clone();
        for chunk in &chunks {
            self.sink.feed(&source, chunk);
        }
    }
}

impl Drop for UdpXcpTransport {
    fn drop(&mut self) {
        self.close();
    }
}

/// XCP-over-TCP transport.
/// Connects lazily on the first send (NoDelay enabled); the local endpoint
/// string serves as the frame source identifier. The received byte stream is
/// reassembled into frames via the CTR_WORD receive buffer. A failed send
/// closes the socket and returns 0; the next send automatically reconnects.
pub struct TcpXcpTransport {
    remote: SocketAddr,
    stream: Option<TcpStream>,
    source: String,
    sink: FrameSink,
}

impl TcpXcpTransport {
    /// Creates a transport for `remote` (the socket is not connected until the first send).
    pub fn new(remote: SocketAddr) -> Result<Self> {
        Ok(Self {
            remote,
            stream: None,
            source: String::new(),
            sink: FrameSink::new(XcpType::Tcp)?,
        })
    }

    /// Lazily connects (NoDelay + connect timeout, see module-level notes).
    /// `connect` is a genuinely blocking syscall, so it is moved off the
    /// executor via `spawn_blocking`.
    async fn ensure_stream(&mut self) -> bool {
        if self.stream.is_none() {
            let remote = self.remote;
            match autors_runtime::spawn_blocking(move || {
                TcpSocketWithTimeout::connect_default(&remote)
            })
            .await
            {
                Ok(s) => {
                    if s.set_nonblocking(true).is_err() {
                        return false;
                    }
                    self.source = s.local_addr().map(|a| a.to_string()).unwrap_or_default();
                    self.stream = Some(s);
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// Shuts down both directions and closes the socket.
    pub fn close(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Returns the remote endpoint.
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }
}

#[async_trait]
impl XcpTransport for TcpXcpTransport {
    fn source(&self) -> &str {
        &self.source
    }

    fn frame_fmt(&self) -> XcpHeaderLen {
        XcpHeaderLen::CTR_WORD
    }

    fn reset(&mut self) {
        self.sink.reset();
    }

    /// Sends one frame; returns the length on success, closes the socket and
    /// returns 0 on failure.
    async fn send_bytes(&mut self, data: &[u8]) -> usize {
        if !self.ensure_stream().await {
            return 0;
        }
        let Some(stream) = self.stream.as_ref() else {
            return 0;
        };
        match write_all_wait(stream, data).await {
            Ok(()) => data.len(),
            Err(_) => {
                self.close();
                0
            }
        }
    }

    fn next_frame(&mut self) -> Option<XcpFrame> {
        self.sink.queue.pop_front()
    }

    /// Drains the socket non-blockingly; on EOF (peer closed) closes the local
    /// socket so the next send lazily reconnects.
    async fn poll(&mut self) {
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut eof = false;
        if let Some(stream) = self.stream.as_ref() {
            let mut s = stream;
            let mut buf = [0u8; 65536];
            loop {
                match s.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => chunks.push(buf[..n].to_vec()),
                    Err(_) => break,
                }
            }
        }
        if eof {
            self.close();
        }
        let source = self.source.clone();
        for chunk in &chunks {
            self.sink.feed(&source, chunk);
        }
    }
}

impl Drop for TcpXcpTransport {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use autors_comm::base::ConnectBehaviourType;

    use crate::ifdata_xcp::XcpProtocolLayer;
    use crate::xcp::{CmdResult, ConnectMode, XcpMaster};

    #[cfg(feature = "blocking")]
    use crate::blocking::{BlockingXcpMaster, BlockingXcpTransport};

    /// CTR_WORD frame: len u16 LE + ctr u16 LE + payload.
    fn ctr_frame(ctr: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(4 + payload.len());
        v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        v.extend_from_slice(&ctr.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// Drives poll until a frame arrives (5 s timeout to avoid hanging).
    #[cfg(feature = "blocking")]
    fn wait_frame(t: &mut (dyn XcpTransport + Send)) -> XcpFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(f) = t.next_frame() {
                return f;
            }
            autors_runtime::block_on(t.poll());
            if let Some(f) = t.next_frame() {
                return f;
            }
            assert!(Instant::now() < deadline, "timed out waiting for frame");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    const CONNECT_RESP: [u8; 8] = [0xFF, 0x1D, 0xC0, 0x08, 0x08, 0x00, 0x01, 0x01];

    /// Protocol layer for end-to-end tests (T1 relaxed to 2 s to avoid false
    /// timeouts from thread scheduling jitter).
    fn e2e_protocol_layer() -> XcpProtocolLayer {
        XcpProtocolLayer {
            timings: [2000, 50, 100, 200, 500, 1000, 2000],
            max_cto: 8,
            max_dto: 8,
            ..XcpProtocolLayer::default()
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn udp_master_connect_end_to_end() {
        let slave = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = slave.local_addr().unwrap().port();
        slave
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let (n, peer) = slave.recv_from(&mut buf).unwrap();
            let req = buf[..n].to_vec();
            slave.send_to(&ctr_frame(0, &CONNECT_RESP), peer).unwrap();
            req
        });

        let mut m = BlockingXcpMaster::new_udp_tcp(
            ConnectBehaviourType::Manual,
            XcpType::Udp,
            "127.0.0.1",
            port as i32,
            e2e_protocol_layer(),
            None,
        )
        .unwrap();
        m.0.base.base.prevent_default_requests = true;
        assert_eq!(m.0.base.type_, XcpType::Udp);
        assert_eq!(m.0.base.port, port as i32);
        let (res, resp) = m.connect(ConnectMode::Normal);
        assert_eq!(res, CmdResult::OK);
        assert!(resp.is_some());
        assert_eq!(m.0.base.max_cto(), 8);
        // Wire frame sent by the master: CTR_WORD header + CONNECT
        assert_eq!(handle.join().unwrap(), ctr_frame(0, &[0xFF, 0x00]));
    }

    #[test]
    fn new_udp_tcp_rejects_bad_host_and_type() {
        assert!(XcpMaster::new_udp_tcp(
            ConnectBehaviourType::Manual,
            XcpType::Udp,
            "not-an-ip",
            5555,
            e2e_protocol_layer(),
            None,
        )
        .is_err());
        assert!(XcpMaster::new_udp_tcp(
            ConnectBehaviourType::Manual,
            XcpType::Can,
            "127.0.0.1",
            5555,
            e2e_protocol_layer(),
            None,
        )
        .is_err());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn udp_roundtrip_loopback() {
        let slave = UdpSocket::bind("127.0.0.1:0").unwrap();
        let slave_addr = slave.local_addr().unwrap();
        slave
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let echo = ctr_frame(7, &CONNECT_RESP);
        let echo2 = echo.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let (n, peer) = slave.recv_from(&mut buf).unwrap();
            (buf[..n].to_vec(), slave.send_to(&echo2, peer).unwrap())
        });

        let mut t = BlockingXcpTransport::new(UdpXcpTransport::new(slave_addr).unwrap());
        assert_eq!(t.frame_fmt(), XcpHeaderLen::CTR_WORD);
        assert_eq!(t.source(), "");
        assert_eq!(t.0.remote(), slave_addr);

        let req = ctr_frame(0, &[0xFF, 0x00]);
        assert_eq!(t.send_bytes(&req), req.len());
        assert!(!t.source().is_empty());

        let f = wait_frame(&mut t.0);
        assert_eq!(f.type_, XcpType::Udp);
        assert_eq!(f.data(), &CONNECT_RESP);
        assert_eq!(f.ctr, 7);

        let (got, sent) = handle.join().unwrap();
        assert_eq!(got, req);
        assert_eq!(sent, echo.len());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn udp_two_frames_one_datagram_and_reset() {
        let slave = UdpSocket::bind("127.0.0.1:0").unwrap();
        let slave_addr = slave.local_addr().unwrap();
        slave
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let (_, peer) = slave.recv_from(&mut buf).unwrap();
            // Two frames packed into a single datagram
            let mut d = ctr_frame(1, &[0xFF]);
            d.extend_from_slice(&ctr_frame(2, &[0xFE, 0x00]));
            slave.send_to(&d, peer).unwrap();
        });

        let mut t = BlockingXcpTransport::new(UdpXcpTransport::new(slave_addr).unwrap());
        assert_eq!(t.send_bytes(&ctr_frame(0, &[0xFF, 0x00])), 6);
        let f1 = wait_frame(&mut t.0);
        assert_eq!(f1.data(), &[0xFF]);
        assert_eq!(f1.ctr, 1);
        let f2 = t.next_frame().unwrap();
        assert_eq!(f2.data(), &[0xFE, 0x00]);
        assert_eq!(f2.ctr, 2);
        assert!(t.next_frame().is_none());

        // reset clears the receive buffer and the queue
        t.reset();
        assert!(t.next_frame().is_none());
        handle.join().unwrap();
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn udp_send_unbound_after_close() {
        // After close the socket is lazily recreated, so sending still succeeds.
        let slave = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut t =
            BlockingXcpTransport::new(UdpXcpTransport::new(slave.local_addr().unwrap()).unwrap());
        t.0.close();
        assert_eq!(t.send_bytes(&ctr_frame(0, &[0xFF])), 5);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn tcp_roundtrip_fragmented() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let echo = ctr_frame(9, &CONNECT_RESP);
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // Read the request (CTR_WORD header + payload)
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).unwrap();
            let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
            let mut payload = vec![0u8; len];
            s.read_exact(&mut payload).unwrap();
            // Write the response in two pieces to exercise stream reassembly
            let mid = echo.len() / 2;
            s.write_all(&echo[..mid]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            s.write_all(&echo[mid..]).unwrap();
            (hdr.to_vec(), payload)
        });

        let mut t = BlockingXcpTransport::new(TcpXcpTransport::new(addr).unwrap());
        assert_eq!(t.source(), "");
        let req = ctr_frame(0, &[0xFF, 0x00]);
        assert_eq!(t.send_bytes(&req), req.len());
        assert!(!t.source().is_empty());

        let f = wait_frame(&mut t.0);
        assert_eq!(f.type_, XcpType::Tcp);
        assert_eq!(f.data(), &CONNECT_RESP);
        assert_eq!(f.ctr, 9);

        let (hdr, payload) = handle.join().unwrap();
        assert_eq!(hdr, [0x02, 0x00, 0x00, 0x00]);
        assert_eq!(payload, [0xFF, 0x00]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn tcp_send_refused_returns_zero() {
        // Unlistened port: connection refused -> 0
        let mut t = BlockingXcpTransport::new(
            TcpXcpTransport::new("127.0.0.1:1".parse().unwrap()).unwrap(),
        );
        assert_eq!(t.send_bytes(&ctr_frame(0, &[0xFF, 0x00])), 0);
        assert_eq!(t.source(), "");
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn tcp_reconnect_after_peer_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            // First connection: receive the request, then close immediately
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf);
            drop(s);
            // Second connection: answer with one frame
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let _ = s.read(&mut buf);
            s.write_all(&ctr_frame(3, &[0xFF])).unwrap();
        });

        let mut t = BlockingXcpTransport::new(TcpXcpTransport::new(addr).unwrap());
        assert_eq!(t.send_bytes(&ctr_frame(0, &[0xFF, 0x00])), 6);
        // Wait for the peer close and EOF detection
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            t.poll();
            if t.0.stream.is_none() {
                break;
            }
            assert!(Instant::now() < deadline, "peer close not detected");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Send again: lazy reconnect succeeds
        assert_eq!(t.send_bytes(&ctr_frame(1, &[0xFF, 0x00])), 6);
        let f = wait_frame(&mut t.0);
        assert_eq!(f.data(), &[0xFF]);
        assert_eq!(f.ctr, 3);
        handle.join().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_roundtrip_loopback_async() {
        let slave = UdpSocket::bind("127.0.0.1:0").unwrap();
        let slave_addr = slave.local_addr().unwrap();
        slave
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let echo = ctr_frame(7, &CONNECT_RESP);
        let echo2 = echo.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let (n, peer) = slave.recv_from(&mut buf).unwrap();
            (buf[..n].to_vec(), slave.send_to(&echo2, peer).unwrap())
        });

        let mut t = UdpXcpTransport::new(slave_addr).unwrap();
        let req = ctr_frame(0, &[0xFF, 0x00]);
        assert_eq!(t.send_bytes(&req).await, req.len());
        assert!(!t.source().is_empty());

        // Drive the async poll until a frame arrives (5 s timeout to avoid hanging)
        let deadline = Instant::now() + Duration::from_secs(5);
        let f = loop {
            if let Some(f) = t.next_frame() {
                break f;
            }
            t.poll().await;
            if let Some(f) = t.next_frame() {
                break f;
            }
            assert!(Instant::now() < deadline, "timed out waiting for frame");
            autors_runtime::sleep(Duration::from_millis(1)).await;
        };
        assert_eq!(f.type_, XcpType::Udp);
        assert_eq!(f.data(), &CONNECT_RESP);
        assert_eq!(f.ctr, 7);

        let (got, sent) = handle.join().unwrap();
        assert_eq!(got, req);
        assert_eq!(sent, echo.len());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_master_connect_end_to_end_async() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).unwrap();
            let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
            let mut payload = vec![0u8; len];
            s.read_exact(&mut payload).unwrap();
            s.write_all(&ctr_frame(0, &CONNECT_RESP)).unwrap();
            (hdr.to_vec(), payload)
        });

        let mut m = XcpMaster::new_udp_tcp(
            ConnectBehaviourType::Manual,
            XcpType::Tcp,
            "127.0.0.1",
            addr.port() as i32,
            e2e_protocol_layer(),
            None,
        )
        .unwrap();
        m.base.base.prevent_default_requests = true;
        let (res, resp) = m.connect(ConnectMode::Normal).await;
        assert_eq!(res, CmdResult::OK);
        assert!(resp.is_some());
        assert_eq!(m.base.max_cto(), 8);
        assert_eq!(
            handle.join().unwrap(),
            ([0x02, 0x00, 0x00, 0x00].to_vec(), vec![0xFF, 0x00])
        );
    }
}
