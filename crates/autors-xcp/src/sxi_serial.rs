//! XCP over SxI serial transport: serialport adapter + SxI→XCP transport bridge.
//! The frame codec core (checksum + SYNC/ESC escaping, receive reassembly)
//! lives in [`crate::xcp::SxiCore`]/[`crate::xcp::XcpReceiveBuffer`], generic
//! over the [`crate::xcp::SxiSerialIo`] byte-stream abstraction — this module
//! only provides the two concrete pieces:
//! - [`SerialPortIo`]: an [`SxiSerialIo`] adapter for the serialport crate
//!   (real serial ports); tests can substitute any [`SxiSerialIo`]
//!   implementation such as an in-memory pipe.
//! - [`SxiXcpTransport`]: wraps a [`SerialPortDevice`] into a
//!   [`crate::xcp::XcpTransport`] for wiring into `XcpMasterBase::new_sxi`.
//!
//! Design notes:
//! - `Handshake.RequestToSendXOnXOff` maps to serialport's hardware flow
//!   control (serialport has no combined RTS+XON/XOFF mode).
//! - Reception follows the crate-wide polling model in [`XcpTransport::poll`]
//!   (first drain the serial port via [`SerialPortDevice::poll_once`], then
//!   frame the bytes into the queue).

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use crate::ifdata_xcp::XcpHeaderLen;
use crate::xcp::{
    SerialPortDevice, SxiDevice, SxiHandshake, SxiParity, SxiSerialConfig, SxiSerialIo,
    SxiStopBits, XcpFrame, XcpTransport, XcpType,
};

/// Concrete serial-port implementation backed by the serialport crate.
/// `is_open` is true while the port is open; it is set to false after a
/// non-timeout IO error on read/write (i.e. the port is considered gone once
/// an unexpected disconnect occurs).
pub struct SerialPortIo {
    inner: Box<dyn SerialPort>,
    open: bool,
}

impl SerialPortIo {
    /// Wraps an already-open serialport port.
    pub fn new(inner: Box<dyn SerialPort>) -> Self {
        Self { inner, open: true }
    }

    /// Returns the underlying port (for configuration queries).
    pub fn inner(&self) -> &dyn SerialPort {
        &*self.inner
    }
}

impl SxiSerialIo for SerialPortIo {
    fn is_open(&self) -> bool {
        self.open
    }

    fn bytes_to_read(&mut self) -> usize {
        self.inner.bytes_to_read().unwrap_or(0) as usize
    }

    /// A read timeout (serialport returns `ErrorKind::TimedOut`) is treated as 0 bytes.
    fn read(&mut self, buf: &mut [u8]) -> usize {
        match self.inner.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(_) => {
                self.open = false;
                0
            }
        }
    }

    fn bytes_to_write(&mut self) -> usize {
        self.inner.bytes_to_write().unwrap_or(0) as usize
    }

    fn write(&mut self, data: &[u8]) {
        if self.inner.write_all(data).is_err() {
            self.open = false;
        }
    }

    /// Discards both the receive and the transmit buffers.
    fn discard_buffers(&mut self) {
        let _ = self.inner.clear(ClearBuffer::All);
    }
}

/// Opens a real serial port according to [`SxiSerialConfig`] (returns None on
/// failure, matching the intended lenient open semantics). Can be passed
/// directly as the `open_fn` of [`SerialPortDevice::new`].
pub fn open_serial_port(config: &SxiSerialConfig) -> Option<SerialPortIo> {
    let parity = match config.parity {
        SxiParity::None => Parity::None,
        SxiParity::Odd => Parity::Odd,
        SxiParity::Even => Parity::Even,
    };
    let stop_bits = match config.stop_bits {
        SxiStopBits::One => StopBits::One,
        SxiStopBits::Two => StopBits::Two,
    };
    let flow = match config.handshake {
        SxiHandshake::None => FlowControl::None,
        SxiHandshake::XOnXOff => FlowControl::Software,
        // serialport has no combined RTS+XON/XOFF mode; use hardware flow control.
        SxiHandshake::RequestToSend | SxiHandshake::RequestToSendXOnXOff => FlowControl::Hardware,
    };
    let timeout = std::time::Duration::from_millis(config.read_timeout_ms.max(0) as u64);
    serialport::new(&config.port, config.baudrate)
        .data_bits(DataBits::Eight)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow)
        .timeout(timeout)
        .open()
        .ok()
        .map(SerialPortIo::new)
}

/// Bridges an `ISxIDevice`-style serial device into an XCP transport.
/// On construction a receive callback is registered on the device; received
/// raw bytes are staged first. [`XcpTransport::poll`] pumps
/// [`SerialPortDevice::poll_once`] and then feeds the staged bytes into the
/// SxI framer (framing is deferred to poll rather than done synchronously
/// inside the callback; the observable behavior is equivalent). Sending goes
/// through [`SxiDevice::send_msg`] (checksum + SYNC/ESC escaping live in
/// [`crate::xcp::SxiCore`]).
pub struct SxiXcpTransport<IO: SxiSerialIo> {
    device: SerialPortDevice<IO>,
    /// Raw bytes staged by the receive callback.
    pending: Arc<Mutex<Vec<u8>>>,
    queue: VecDeque<XcpFrame>,
    frame_fmt: XcpHeaderLen,
}

impl<IO: SxiSerialIo> SxiXcpTransport<IO> {
    /// Creates the transport around a serial device.
    pub fn new(mut device: SerialPortDevice<IO>) -> Self {
        let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = Arc::clone(&pending);
        device
            .core_mut()
            .set_data_callback(Box::new(move |chunk: &[u8]| {
                p2.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(chunk);
            }));
        let frame_fmt = device.core().sxi.header_len;
        Self {
            device,
            pending,
            queue: VecDeque::new(),
            frame_fmt,
        }
    }

    /// Returns the underlying device.
    pub fn device(&self) -> &SerialPortDevice<IO> {
        &self.device
    }

    /// See [`SxiXcpTransport::device`].
    pub fn device_mut(&mut self) -> &mut SerialPortDevice<IO> {
        &mut self.device
    }
}

#[async_trait]
impl<IO: SxiSerialIo + Send> XcpTransport for SxiXcpTransport<IO> {
    /// The source identifier is the port name.
    fn source(&self) -> &str {
        &self.device.core().port
    }

    /// Returns the SxI frame header format from the configuration.
    fn frame_fmt(&self) -> XcpHeaderLen {
        self.frame_fmt
    }

    /// Resets the device (receive buffer reset + serial port buffers
    /// cleared), and additionally clears the frame queue.
    fn reset(&mut self) {
        self.device.reset();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.queue.clear();
    }

    /// Sends one message via the device.
    // TODO(hardware): route serial waits through spawn_blocking if executor stall becomes an issue
    async fn send_bytes(&mut self, data: &[u8]) -> usize {
        self.device.send_msg(data)
    }

    fn next_frame(&mut self) -> Option<XcpFrame> {
        self.queue.pop_front()
    }

    /// Polling form of the receive path: drains the serial port, then frames
    /// the staged bytes into the queue (after feeding the staged bytes once,
    /// keep draining with an empty slice until no complete frame remains).
    async fn poll(&mut self) {
        while self.device.poll_once() {}
        let chunk = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        if chunk.is_empty() {
            return;
        }
        let port = self.device.core().port.clone();
        let mut data: &[u8] = &chunk;
        while let Some(f) = self.device.get_frame_from_data(XcpType::Sxi, &port, data) {
            self.queue.push_back(f);
            data = &[];
        }
    }
}

impl<IO: SxiSerialIo> Drop for SxiXcpTransport<IO> {
    /// Closes the device.
    fn drop(&mut self) {
        self.device.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifdata_xcp::{ChecksumSxi, XcpNode, XcpOnSxi, XcpProtocolLayer};
    use crate::xcp::{CmdResult, ConnectMode, SxiCore};
    use autors_comm::base::ConnectBehaviourType;
    use std::time::{Duration, Instant};

    #[cfg(feature = "blocking")]
    use crate::blocking::{BlockingXcpMaster, BlockingXcpTransport};

    /// In-memory full-duplex pipe endpoint (simulates a serial port; the two
    /// ends share a pair of buffers).
    #[derive(Clone)]
    struct MemEnd {
        incoming: Arc<Mutex<VecDeque<u8>>>,
        outgoing: Arc<Mutex<VecDeque<u8>>>,
        open: bool,
    }

    fn mem_pair() -> (MemEnd, MemEnd) {
        let a: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
        let b: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
        (
            MemEnd {
                incoming: Arc::clone(&a),
                outgoing: Arc::clone(&b),
                open: true,
            },
            MemEnd {
                incoming: b,
                outgoing: a,
                open: true,
            },
        )
    }

    impl SxiSerialIo for MemEnd {
        fn is_open(&self) -> bool {
            self.open
        }
        fn bytes_to_read(&mut self) -> usize {
            self.incoming.lock().unwrap().len()
        }
        fn read(&mut self, buf: &mut [u8]) -> usize {
            let mut g = self.incoming.lock().unwrap();
            let mut n = 0;
            while n < buf.len() {
                match g.pop_front() {
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
            self.outgoing.lock().unwrap().extend(data);
        }
        fn discard_buffers(&mut self) {
            self.incoming.lock().unwrap().clear();
        }
    }

    fn sxi_config(checksum: ChecksumSxi) -> XcpOnSxi {
        let mut sxi = XcpOnSxi {
            header_len: XcpHeaderLen::CTR_WORD,
            checksum,
            ..XcpOnSxi::default()
        };
        sxi.children
            .push(XcpNode::Framing(crate::ifdata_xcp::XcpFraming {
                sync: 0x55,
                esc: 0x99,
                children: Vec::new(),
            }));
        sxi
    }

    fn mem_transport(io: MemEnd, sxi: XcpOnSxi) -> SxiXcpTransport<MemEnd> {
        let device = SerialPortDevice::new("MEM1", sxi, SxiHandshake::None, 100, move |_| {
            Some(io.clone())
        })
        .unwrap();
        let mut t = SxiXcpTransport::new(device);
        assert!(t.device_mut().open());
        t
    }

    /// CTR_WORD frame: len u16 LE + ctr u16 LE + payload.
    fn ctr_frame(ctr: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(4 + payload.len());
        v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        v.extend_from_slice(&ctr.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn sxi_transport_roundtrip_with_escape() {
        let (io_a, io_b) = mem_pair();
        let tap_a_tx = Arc::clone(&io_a.outgoing);
        let mut ta =
            BlockingXcpTransport::new(mem_transport(io_a, sxi_config(ChecksumSxi::CHECKSUM_BYTE)));
        let mut tb =
            BlockingXcpTransport::new(mem_transport(io_b, sxi_config(ChecksumSxi::CHECKSUM_BYTE)));

        assert_eq!(ta.source(), "MEM1");
        assert_eq!(ta.frame_fmt(), XcpHeaderLen::CTR_WORD);

        // Payload contains raw SYNC/ESC bytes: exercises escaping + checksum round-trip
        let req = ctr_frame(5, &[0xFF, 0x55, 0x99]);
        assert_eq!(ta.send_bytes(&req), req.len());

        // Wire bytes: leading SYNC, both 0x55/0x99 escaped with ESC, trailing ADD_11 checksum
        let wire: Vec<u8> = tap_a_tx.lock().unwrap().iter().copied().collect();
        assert_eq!(wire[0], 0x55);
        assert!(wire.windows(2).any(|w| w == [0x99, 0x55]));
        assert!(wire.windows(2).any(|w| w == [0x99, 0x99]));
        let sum: u8 = req.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        assert_eq!(*wire.last().unwrap(), sum);

        tb.poll();
        let f = tb.next_frame().unwrap();
        assert_eq!(f.type_, XcpType::Sxi);
        assert_eq!(f.data(), &[0xFF, 0x55, 0x99]);
        assert_eq!(f.ctr, 5);
        assert!(tb.next_frame().is_none());

        // Reverse direction: B answers -> A receives (full-duplex round-trip)
        let resp = ctr_frame(5, &[0xFF, 0x1D]);
        assert_eq!(tb.send_bytes(&resp), resp.len());
        ta.poll();
        let f = ta.next_frame().unwrap();
        assert_eq!(f.data(), &[0xFF, 0x1D]);
        assert!(ta.next_frame().is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn sxi_transport_checksum_error_then_resync() {
        let (io_a, io_b) = mem_pair();
        let tap_a_rx = Arc::clone(&io_b.outgoing);
        let mut ta =
            BlockingXcpTransport::new(mem_transport(io_a, sxi_config(ChecksumSxi::CHECKSUM_BYTE)));

        // Inject a frame with a bad checksum directly onto the wire: dropped, no frame produced
        let mut bad = vec![0x55];
        bad.extend_from_slice(&ctr_frame(1, &[0xFF, 0x1D]));
        bad.push(0x00); // wrong checksum
        tap_a_rx.lock().unwrap().extend(bad);
        ta.poll();
        assert!(ta.next_frame().is_none());

        // The master resets before each exchange: once the framing state is
        // reset a valid frame is received; noise bytes before SYNC are skipped
        // in the idle state
        ta.reset();
        let mut good = vec![0x00, 0x11, 0x22]; // noise
        good.push(0x55);
        let body = ctr_frame(2, &[0xFF, 0x1D]);
        let sum: u8 = body.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        good.extend_from_slice(&body);
        good.push(sum);
        tap_a_rx.lock().unwrap().extend(good);
        ta.poll();
        let f = ta.next_frame().unwrap();
        assert_eq!(f.data(), &[0xFF, 0x1D]);
        assert_eq!(f.ctr, 2);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn sxi_transport_send_unopened_returns_zero() {
        let (io_a, _io_b) = mem_pair();
        let device = SerialPortDevice::new(
            "MEM1",
            sxi_config(ChecksumSxi::NO_CHECKSUM),
            SxiHandshake::None,
            100,
            move |_| Some(io_a.clone()),
        )
        .unwrap();
        let mut ta = BlockingXcpTransport::new(SxiXcpTransport::new(device));
        // Not opened: send returns 0 (port-not-open path)
        assert_eq!(ta.send_bytes(&ctr_frame(0, &[0xFF])), 0);
        ta.0.device_mut().close();
    }

    #[test]
    fn sxi_core_send_msg_via_transport_core() {
        // Direct SxiCore-level check: without FRAMING config the bytes are
        // written as-is (with the checksum appended)
        let mut sxi = XcpOnSxi {
            header_len: XcpHeaderLen::CTR_WORD,
            checksum: ChecksumSxi::CHECKSUM_WORD,
            ..XcpOnSxi::default()
        };
        sxi.children.clear();
        let mut core = SxiCore::new("MEM1", sxi).unwrap();
        let mut written = Vec::new();
        let n = core.send_msg(&[0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D], |d| {
            written.extend_from_slice(d);
            d.len()
        });
        assert_eq!(n, 8);
        // ADD_12 = 0x02+0x00+0x01+0x00+0xFF+0x1D = 0x011F (appended LE)
        assert_eq!(written, [0x02, 0x00, 0x01, 0x00, 0xFF, 0x1D, 0x1F, 0x01]);
    }

    #[test]
    fn open_serial_port_bad_port_returns_none() {
        let cfg = SxiSerialConfig::new(
            &sxi_config(ChecksumSxi::NO_CHECKSUM),
            "AUTORS_NONEXISTENT_PORT_0",
            100,
            SxiHandshake::None,
        );
        assert!(open_serial_port(&cfg).is_none());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn sxi_master_connect_end_to_end() {
        const CONNECT_RESP: [u8; 8] = [0xFF, 0x1D, 0xC0, 0x08, 0x08, 0x00, 0x01, 0x01];
        let (io_a, io_b) = mem_pair();
        let tap_master_tx = Arc::clone(&io_a.outgoing);
        let tap_master_rx = Arc::clone(&io_b.outgoing);

        // Simulated XCP slave: waits for the master's wire frame, replies with
        // an SxI-framed CONNECT response
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if !tap_master_tx.lock().unwrap().is_empty() {
                    break;
                }
                assert!(Instant::now() < deadline, "master never sent");
                std::thread::sleep(Duration::from_millis(1));
            }
            let req: Vec<u8> = tap_master_tx.lock().unwrap().drain(..).collect();
            // SYNC + CONNECT frame (len=2, ctr=0, FF 00) + ADD_11 checksum (0x02)
            assert_eq!(req, [0x55, 0x02, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x01]);
            let body = ctr_frame(0, &CONNECT_RESP);
            let sum: u8 = body.iter().fold(0u8, |s, &b| s.wrapping_add(b));
            let mut resp = vec![0x55];
            resp.extend_from_slice(&body);
            resp.push(sum);
            tap_master_rx.lock().unwrap().extend(resp);
        });

        let mut device = SerialPortDevice::new(
            "MEM1",
            sxi_config(ChecksumSxi::CHECKSUM_BYTE),
            SxiHandshake::None,
            100,
            move |_| Some(io_a.clone()),
        )
        .unwrap();
        assert!(device.open());
        let pl = XcpProtocolLayer {
            timings: [2000, 50, 100, 200, 500, 1000, 2000],
            max_cto: 8,
            max_dto: 8,
            ..XcpProtocolLayer::default()
        };
        let mut m = BlockingXcpMaster::new_sxi(
            ConnectBehaviourType::Manual,
            device,
            &sxi_config(ChecksumSxi::CHECKSUM_BYTE),
            pl,
            None,
        );
        m.0.base.base.prevent_default_requests = true;
        assert_eq!(m.0.base.type_, XcpType::Sxi);
        let (res, resp) = m.connect(ConnectMode::Normal);
        assert_eq!(res, CmdResult::OK);
        assert!(resp.is_some());
        assert_eq!(m.0.base.max_cto(), 8);
        handle.join().unwrap();
    }
}
