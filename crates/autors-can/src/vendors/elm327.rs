//! ELM327 AT-command serial protocol adapter (ELM327 OBD interpreter chip).
//! Unlike the Kvaser/Vector/Peak adapters, ELM327 is **not a DLL P/Invoke
//! vendor**: the device is an OBD interpreter chip driven over a serial port
//! with AT commands, so this file loads no DLL. Instead it implements the
//! protocol codec following the `SxiSerialIo` pattern of
//! `autors-xcp/src/sxi_serial.rs`: the byte stream is abstracted behind the
//! `Elm327Io` trait (the subset of serial-port operations the protocol needs),
//! the protocol engine `Elm327Core` is generic over it, and tests drive it
//! with an in-memory fake device; a real serial-port adapter (serialport
//! crate) is wired in during the unified registration stage (autors-can
//! currently does not depend on serialport).
//! Design notes (details at each entry):
//! - Instead of a background receive task with event waits, a single-threaded
//!   pump model is used: while a command waits for its response,
//!   `Elm327Core::poll_io` drains the serial port itself (semantically
//!   equivalent, same polling convention as the rest of the crate).
//! - TX and RX handle `CAN_EXT_FLAG` explicitly: under a 29-bit protocol,
//!   received frames get `CAN_EXT_FLAG` added and the flag is stripped before
//!   sending (formatting the raw flagged `u32` would produce a wrong `ATCP`
//!   priority byte).
//! - `send` validates the data length and returns `Error::Invalid` above the
//!   classic CAN limit `MAX_DLC` (overlong payloads would otherwise fail
//!   silently on the device).
//! - Serial-port creation (baud-rate parsing of `COMPortConfig`, 8N1, 64KB
//!   buffers, timeout settings) is the responsibility of the IO factory, not
//!   the engine; `parse_com_port_baud` parses the baud rate from a config
//!   string of the form `"9600,8,N,1"`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::device::{config_err, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_DLC};
use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFrame, FrameType};

// ---------------------------------------------------------------------------
// Device protocol strings and the initialization command table
// ---------------------------------------------------------------------------

/// Expected response for a successful command.
const RESP_OK: &str = "OK";
/// Response produced when the device is busy with another command.
const RESP_NOT_READY: &str = "not ready to send";
/// Echo-off command; retried up to 3 times during open.
const CMD_ATE0: &str = "ATE0";
/// Monitor-all command (enter monitor mode).
const CMD_ATMA: &str = "ATMA";
/// Command querying the current protocol number.
const CMD_ATDPN: &str = "ATDPN";
/// Command prefix for setting the 29-bit transmit priority.
const CMD_ATCP: &str = "ATCP";
/// Command prefix for setting the transmit header.
const CMD_ATSH: &str = "ATSH";

/// The engine's 11 initialization AT commands: indices 0–4 are sent before the
/// protocol is set (the responses to 2/3/4 are stored as
/// Version/ELMName/Voltage), indices 5–10 after.
const INIT_COMMANDS: [&str; 11] = [
    "ATL0",   // [0] linefeeds off
    "ATS0",   // [1] spaces off
    "ATI",    // [2] version ID -> Version
    "AT@1",   // [3] device description -> ELMName
    "ATRV",   // [4] read voltage -> Voltage
    "ATCS",   // [5]
    "ATH1",   // [6] headers on (receive lines include the ID)
    "ATD1",   // [7] display DLC (receive lines carry 1 DLC digit after the ID)
    "ATCAF0", // [8] CAN auto-formatting off (no ISO-TP PCI insertion)
    "ATV0",   // [9]
    "ATBI",   // [10] bypass the initialization sequence
];

/// Open-failure message:
/// `"Failed to open " + COMPort + " (" + COMPortConfig + ")"`.
pub(crate) fn open_failed_msg(com_port: &str, com_port_config: &str) -> String {
    format!("Failed to open {com_port} ({com_port_config})")
}

// ---------------------------------------------------------------------------
// ELM327 protocol numbers (used with `ATSP`/`ATTP`)
// ---------------------------------------------------------------------------

/// ELM327 protocol numbers for the `ATSP`/`ATTP` commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum Elm327Protocol {
    /// Not set.
    None = -1,
    /// Automatic detection.
    #[default]
    Auto = 0,
    /// SAE J1850 PWM 41.6 kBaud.
    SaeJ1850_41_6 = 1,
    /// SAE J1850 VPW 10.4 kBaud.
    SaeJ1850_10_4 = 2,
    /// ISO 9141-2.
    Iso9141_2 = 3,
    /// ISO 14230-4 KWP (5 baud init).
    Iso14230 = 4,
    /// ISO 14230-4 KWP (fast init).
    Iso14230Fast = 5,
    /// ISO 15765-4 CAN, 500 kBit/s, 11-bit ID.
    Iso15765_500k11Bit = 6,
    /// ISO 15765-4 CAN, 500 kBit/s, 29-bit ID.
    Iso15765_500k29Bit = 7,
    /// ISO 15765-4 CAN, 250 kBit/s, 11-bit ID.
    Iso15765_250k11Bit = 8,
    /// ISO 15765-4 CAN, 250 kBit/s, 29-bit ID.
    Iso15765_250k29Bit = 9,
    /// SAE J1939.
    SaeJ1939 = 10,
    /// User CAN, 11-bit, 125 kBaud.
    UserCan11Bit125kBaud = 11,
    /// User CAN, 11-bit, 50 kBaud.
    UserCan11Bit50kBaud = 12,
}

impl Elm327Protocol {
    /// Numeric value of the variant.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Maps a protocol number to a variant; unknown values return `None`. The
    /// engine itself does not validate the range (see `Elm327Core::open`) —
    /// this method only lets callers test for known protocols.
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            -1 => Some(Self::None),
            0 => Some(Self::Auto),
            1 => Some(Self::SaeJ1850_41_6),
            2 => Some(Self::SaeJ1850_10_4),
            3 => Some(Self::Iso9141_2),
            4 => Some(Self::Iso14230),
            5 => Some(Self::Iso14230Fast),
            6 => Some(Self::Iso15765_500k11Bit),
            7 => Some(Self::Iso15765_500k29Bit),
            8 => Some(Self::Iso15765_250k11Bit),
            9 => Some(Self::Iso15765_250k29Bit),
            10 => Some(Self::SaeJ1939),
            11 => Some(Self::UserCan11Bit125kBaud),
            12 => Some(Self::UserCan11Bit50kBaud),
            _ => Option::None,
        }
    }

    /// Protocol name used in the "Failed to set CAN protocol {0}!" message.
    pub const fn cs_name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Auto => "AUTO",
            Self::SaeJ1850_41_6 => "SAEJ1850_41_6",
            Self::SaeJ1850_10_4 => "SAEJ1850_10_4",
            Self::Iso9141_2 => "ISO9141_2",
            Self::Iso14230 => "ISO14230",
            Self::Iso14230Fast => "ISO14230_fast",
            Self::Iso15765_500k11Bit => "ISO15765_500k_11Bit",
            Self::Iso15765_500k29Bit => "ISO15765_500k_29Bit",
            Self::Iso15765_250k11Bit => "ISO15765_250k_11Bit",
            Self::Iso15765_250k29Bit => "ISO15765_250k_29Bit",
            Self::SaeJ1939 => "SAEJ1939",
            Self::UserCan11Bit125kBaud => "USER_CAN_11Bit_125kBaud",
            Self::UserCan11Bit50kBaud => "USER_CAN_11Bit_50kBaud",
        }
    }
}

/// Whether the given protocol number is a 29-bit ID mode.
/// Intentional quirk: `SaeJ1939` (10) is physically 29-bit but does not count;
/// the non-CAN protocols (J1850/9141/14230) are likewise parsed as 11-bit
/// (3 hex digits).
fn is_29bit_protocol(protocol: i32) -> bool {
    protocol == Elm327Protocol::Iso15765_500k29Bit.as_i32()
        || protocol == Elm327Protocol::Iso15765_250k29Bit.as_i32()
}

// ---------------------------------------------------------------------------
// Fixed-length hex encoding/decoding helpers
// ---------------------------------------------------------------------------

/// Fixed-length uppercase hexadecimal: bits above `len * 4` are dropped (only
/// the low `len` nibbles are written).
fn to_hex_str(value: u64, len: usize) -> String {
    let mut s = vec![b'0'; len];
    let mut v = value;
    for i in (0..len).rev() {
        let d = (v & 0xF) as u8;
        s[i] = if d < 10 { b'0' + d } else { b'A' + d - 10 };
        v >>= 4;
    }
    // Pure ASCII, so unwrap is safe.
    String::from_utf8(s).unwrap()
}

/// Reads `len` characters starting at `offset` and accumulates them without
/// validation (`c < 'A'` uses the `'0'` base, otherwise `(c & 0x5F) - 'A' + 10`),
/// advancing `offset`. Out-of-range input returns `None`; the caller treats
/// that as a non-frame line.
fn from_hex_str(s: &[u8], offset: &mut usize, len: usize) -> Option<u64> {
    let mut num = 0u64;
    for _ in 0..len {
        let c = *s.get(*offset)?;
        *offset += 1;
        let d = if c < b'A' {
            c.wrapping_sub(b'0')
        } else {
            (c & 0x5F).wrapping_sub(b'A').wrapping_add(10)
        };
        num = (num << 4).wrapping_add(d as u64);
    }
    Some(num)
}

/// Two-character hex check used as a pre-check on receive lines.
fn is_hex_pair(s: &[u8]) -> bool {
    s.len() == 2 && s.iter().all(u8::is_ascii_hexdigit)
}

/// Parses one line of device output into a CAN frame.
/// Line format (with ATH1 + ATD1 + ATS0): `ID` (8 hex digits for 29-bit
/// protocols, otherwise 3) + 1 DLC digit + DLC × 2 data digits. Pre-checks:
/// line length >= 6 and both the first 2 and last 2 characters are hex; a DLC
/// of 0 is not a frame; any out-of-range/failed parse mid-way is treated as a
/// non-frame (malformed lines are skipped silently — intentional lenient
/// behavior). Trailing extra characters are ignored (the total length is not
/// validated).
fn parse_frame_line(line: &[u8], is_29bit: bool) -> Option<(u32, Vec<u8>)> {
    if line.len() < 6 {
        return None;
    }
    if !is_hex_pair(&line[..2]) || !is_hex_pair(&line[line.len() - 2..]) {
        return None;
    }
    let mut offset = 0usize;
    let id = from_hex_str(line, &mut offset, if is_29bit { 8 } else { 3 })? as u32;
    let dlc = from_hex_str(line, &mut offset, 1)?;
    if dlc == 0 {
        return None;
    }
    // DLC is a single hex digit (1..=15); no upper bound is enforced.
    let mut data = Vec::with_capacity(dlc as usize);
    for _ in 0..dlc {
        data.push(from_hex_str(line, &mut offset, 2)? as u8);
    }
    Some((id, data))
}

/// Extracts the baud rate from a serial-port config string of the form
/// `"9600,8,N,1"` (the trimmed first comma-separated field). A parse failure
/// returns [`Error::Invalid`]; callers typically wrap it in a
/// "Failed to open ..." message.
pub fn parse_com_port_baud(com_port_config: &str) -> Result<u32> {
    let first = com_port_config.split(',').next().unwrap_or("").trim();
    first.parse::<u32>().map_err(|_| {
        Error::Invalid(format!(
            "invalid COM port config {com_port_config:?}: baud rate {first:?} is not a number"
        ))
    })
}

// ---------------------------------------------------------------------------
// Elm327Io — byte-stream abstraction over the serial port
// ---------------------------------------------------------------------------

/// ELM327 byte-stream abstraction (the subset of serial-port operations this
/// protocol needs).
/// A real serial-port adapter (serialport crate) is provided during the
/// unified registration stage; tests substitute an in-memory implementation.
/// A read timeout is surfaced as "0 bytes read" (the polling path taken when a
/// serial port's read timeout expires with no bytes available).
pub trait Elm327Io {
    /// Whether the port is open.
    fn is_open(&self) -> bool;
    /// Number of bytes available to read.
    fn bytes_to_read(&mut self) -> usize;
    /// Single read call into `buf`, returning the number of bytes read.
    fn read(&mut self, buf: &mut [u8]) -> usize;
    /// Writes `data` followed by a trailing `'\r'`; a write failure sets
    /// `is_open` to false.
    fn write(&mut self, data: &[u8]);
    /// Discards the input and output buffers.
    fn discard_buffers(&mut self);
}

// ---------------------------------------------------------------------------
// Elm327Core — AT-command protocol state machine
// ---------------------------------------------------------------------------

/// Engine state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineState {
    /// Idle.
    Idle,
    /// Monitor mode reported an error.
    Error,
    /// Waiting for a command response.
    Waiting,
}

/// ELM327 protocol engine — the AT-command state machine.
/// Generic over [`Elm327Io`]; all receive-side progress is folded into
/// `Elm327Core::poll_io`, driven by command waits and `Elm327Can::receive`.
struct Elm327Core<IO: Elm327Io> {
    io: IO,
    /// State machine state.
    state: EngineState,
    /// Receive line buffer (NUL/LF filtered out, lines split on '\r').
    rx_line_buf: Vec<u8>,
    /// Response lines collected while waiting for a command response.
    response_lines: Vec<String>,
    /// Set when the '>' prompt is received (command response complete).
    response_ready: bool,
    /// Queue of parsed receive frames (the engine holds no BusId; frames are
    /// wrapped at the device layer).
    rx_queue: VecDeque<(u32, Vec<u8>)>,
    /// Initialization-complete flag.
    initialized: bool,
    /// Monitor mode flag.
    monitor: bool,
    /// Current protocol number (raw `i32`; the range is not validated).
    protocol: i32,
    /// Current transmit header (`None` = not set).
    current_header: Option<u32>,
    /// Command response timeout.
    cmd_timeout: Duration,
    /// Bus ID (used in error messages).
    bus_id: String,
    /// Voltage string (ATRV response).
    voltage: String,
    /// Version string (ATI response).
    version: String,
    /// Device name string (AT@1 response).
    device_name: String,
    /// InitProtocol log (initialization command/response log).
    init_log: String,
}

impl<IO: Elm327Io> Elm327Core<IO> {
    /// Creates an engine over an already-open byte stream.
    fn new(io: IO) -> Self {
        Self {
            io,
            state: EngineState::Idle,
            rx_line_buf: Vec::new(),
            response_lines: Vec::new(),
            response_ready: false,
            rx_queue: VecDeque::new(),
            initialized: false,
            monitor: false,
            protocol: Elm327Protocol::None.as_i32(),
            current_header: None,
            cmd_timeout: Duration::from_millis(200),
            bus_id: String::new(),
            voltage: String::new(),
            version: String::new(),
            device_name: String::new(),
            init_log: String::new(),
        }
    }

    /// Full open sequence.
    fn open(&mut self, config: &CanConfiguration, bus_id: &str) -> Result<()> {
        self.bus_id = bus_id.to_string();
        self.state = EngineState::Idle;
        self.current_header = None;
        self.initialized = false;
        self.rx_line_buf.clear();
        self.rx_queue.clear();
        self.cmd_timeout = Duration::from_millis(config.elm327_cmd_timeout.max(0) as u64);
        self.monitor = config.elm327_monitor;
        // Discard the input and output buffers first.
        self.io.discard_buffers();

        // ATE0 up to 3 times; only a write failure is retried — a response
        // timeout still counts as success. If every write fails, opening fails.
        let mut responded = false;
        for _ in 0..3 {
            if self.send_command(CMD_ATE0, true).is_ok() {
                responded = true;
                break;
            }
        }
        if !responded {
            return Err(config_err(bus_id, "Failed to response to init command!"));
        }

        // Init commands 0–4; the responses to 2/3/4 are stored as
        // Version/ELMName/Voltage, and all are recorded in the init log
        // ("cmd > response"; a missing response is logged as an empty string).
        for (i, cmd) in INIT_COMMANDS[..5].iter().enumerate() {
            let resp = self.send_command(cmd, true)?.unwrap_or_default();
            match i {
                2 => self.version = resp.clone(),
                3 => self.device_name = resp.clone(),
                4 => self.voltage = resp.clone(),
                _ => {}
            }
            self.init_log.push_str(cmd);
            self.init_log.push_str(" > ");
            self.init_log.push_str(&resp);
            self.init_log.push('\n');
        }

        // Read the current protocol number with ATDPN; if it matches the
        // target, done. Otherwise ATTP{n}, and if the response is "OK",
        // ATSP{n}; success is judged by the ATTP response.
        let desired = config.elm327_protocol;
        let dpn = self.send_command(CMD_ATDPN, true)?;
        let current = dpn
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<i32>().ok());
        // A missing or non-numeric ATDPN response is mapped to Error::Driver
        // (the hardware ID is included in the message).
        let current = current
            .ok_or_else(|| config_err(bus_id, format!("unexpected ATDPN response: {dpn:?}")))?;
        if current != desired {
            let tp = self.send_command(&format!("ATTP{desired}"), true)?;
            let tp_ok = tp.as_deref() == Some(RESP_OK);
            if tp_ok {
                let sp = self.send_command(&format!("ATSP{desired}"), true)?;
                self.init_log
                    .push_str(&format!("ATSP{desired} > {}\n", sp.unwrap_or_default()));
            }
            if !tp_ok {
                let name = Elm327Protocol::from_i32(desired)
                    .map(|p| p.cs_name().to_string())
                    .unwrap_or_else(|| desired.to_string());
                return Err(config_err(
                    bus_id,
                    format!("Failed to set CAN protocol {name}!"),
                ));
            }
        }
        self.protocol = desired;

        // Init commands 5–10.
        for cmd in &INIT_COMMANDS[5..] {
            let resp = self.send_command(cmd, true)?.unwrap_or_default();
            self.init_log.push_str(cmd);
            self.init_log.push_str(" > ");
            self.init_log.push_str(&resp);
            self.init_log.push('\n');
        }

        self.initialized = true;
        // Monitor mode sends ATMA; a write failure sets the Error state but
        // the open still succeeds.
        if self.monitor && self.send_command(CMD_ATMA, true).is_err() {
            self.state = EngineState::Error;
        }
        Ok(())
    }

    /// Closes the engine.
    /// A best-effort empty command (a single '\r') is sent to abort an ATMA
    /// monitor, without waiting for a response; closing the serial port itself
    /// is the responsibility of the IO owner (`Drop`).
    fn close(&mut self) {
        if self.initialized {
            let _ = self.send_command("", false);
        }
        self.initialized = false;
        self.state = EngineState::Idle;
        self.current_header = None;
        self.rx_queue.clear();
        self.response_lines.clear();
        self.rx_line_buf.clear();
    }

    /// Sends a command and (optionally) waits for its response.
    /// `Ok(None)` means the response timed out; `Ok(Some)` is the response
    /// lines joined with '\r\n' (trailing newline removed). A write failure
    /// returns [`Error::Driver`] ("Failed to send ...").
    fn send_command(&mut self, cmd: &str, wait_response: bool) -> Result<Option<String>> {
        // If initialized and a response is needed, a non-idle device yields
        // "not ready to send".
        if self.initialized && wait_response && !self.wait_until_idle() {
            return Ok(Some(RESP_NOT_READY.to_string()));
        }
        // Clear the response collection and enter the Waiting state.
        self.response_lines.clear();
        self.response_ready = false;
        self.state = EngineState::Waiting;
        // Write the ASCII bytes + '\r'; a closed port or a write failure is
        // reported as "Failed to send " + cmd. Waiting for the transmit buffer
        // to drain is left to the IO implementation.
        if !self.io.is_open() {
            return Err(config_err(&self.bus_id, format!("Failed to send {cmd}")));
        }
        self.io.write(cmd.as_bytes());
        self.io.write(b"\r");
        if !self.io.is_open() {
            return Err(config_err(&self.bus_id, format!("Failed to send {cmd}")));
        }
        // Without waiting for a response, return "OK" immediately.
        if !wait_response {
            return Ok(Some(RESP_OK.to_string()));
        }
        // Wait for the '>' prompt up to cmd_timeout; a timeout yields None.
        let deadline = Instant::now() + self.cmd_timeout;
        while !self.response_ready {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            self.poll_io();
            if !self.response_ready {
                // 1 ms polling cadence.
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // Join the response lines with "\r\n" (no trailing newline).
        let resp = self.response_lines.join("\r\n");
        self.response_lines.clear();
        Ok(Some(resp))
    }

    /// Waits for the state machine to return to idle (at most cmd_timeout).
    /// The wait loop pumps [`Self::poll_io`] itself to advance the state
    /// machine.
    fn wait_until_idle(&mut self) -> bool {
        let deadline = Instant::now() + self.cmd_timeout;
        while self.state != EngineState::Idle && Instant::now() < deadline {
            self.poll_io();
            std::thread::sleep(Duration::from_millis(1));
        }
        self.state == EngineState::Idle
    }

    /// Single iteration of the receive pump: drains serial-port bytes
    /// (filtering NUL/LF), splits lines on '\r', parses CAN frames line by
    /// line into the queue, and also collects response lines while waiting; a
    /// leading '>' prompt ends the Waiting state. Returns whether a frame was
    /// parsed this iteration.
    fn poll_io(&mut self) -> bool {
        // Drain all available bytes.
        let mut buf = [0u8; 4096];
        loop {
            if self.io.bytes_to_read() == 0 {
                break;
            }
            let n = self.io.read(&mut buf);
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if b != 0 && b != b'\n' {
                    self.rx_line_buf.push(b);
                }
            }
        }
        let mut got_frame = false;
        // Split out complete lines on '\r'; the remainder stays buffered.
        while let Some(pos) = self.rx_line_buf.iter().position(|&b| b == b'\r') {
            let line: Vec<u8> = self.rx_line_buf.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            if let Some(frame) = parse_frame_line(line, is_29bit_protocol(self.protocol)) {
                self.rx_queue.push_back(frame);
                got_frame = true;
            }
            // In the Waiting state, every line (including ones parsed as
            // frames) goes into the response collection.
            if self.state == EngineState::Waiting {
                self.response_lines
                    .push(String::from_utf8_lossy(line).into_owned());
            }
        }
        // A leading '>' in the buffer is the command-end prompt.
        if self.rx_line_buf.first() == Some(&b'>') {
            self.rx_line_buf.remove(0);
            let was_waiting = self.state == EngineState::Waiting;
            self.state = EngineState::Idle;
            if was_waiting {
                self.response_ready = true;
            }
        }
        got_frame
    }

    /// Sends one frame.
    /// Returns the number of data bytes sent; returns 0 when uninitialized,
    /// when an ATCP/ATSH response is not "OK", or on a write error (errors are
    /// swallowed silently — intentional lenient behavior). The data phase's
    /// response is ignored.
    fn send_frame(&mut self, can_id: u32, data: &[u8]) -> usize {
        if !self.initialized {
            return 0;
        }
        // Strip CAN_EXT_FLAG (formatting the raw flagged value would give ATCP
        // a wrong priority byte 0x80); ATCP takes the top byte (>> 24) and
        // ATSH the low 24/12 bits via fixed-length hex truncation.
        let raw = can_id & !CAN_EXT_FLAG;
        if self.current_header != Some(raw) {
            let header_ok = if is_29bit_protocol(self.protocol) {
                // 29-bit: ATCP + 2-digit priority, then after "OK" ATSH +
                // 6-digit address.
                let cp = format!("{CMD_ATCP}{}", to_hex_str((raw >> 24) as u64, 2));
                matches!(self.send_command(&cp, true), Ok(Some(resp)) if resp == RESP_OK) && {
                    let sh = format!("{CMD_ATSH}{}", to_hex_str(raw as u64, 6));
                    matches!(self.send_command(&sh, true), Ok(Some(resp)) if resp == RESP_OK)
                }
            } else {
                // 11-bit: ATSH + 3-digit address.
                let sh = format!("{CMD_ATSH}{}", to_hex_str(raw as u64, 3));
                matches!(self.send_command(&sh, true), Ok(Some(resp)) if resp == RESP_OK)
            };
            if !header_ok {
                return 0;
            }
            self.current_header = Some(raw);
        }
        // Data is sent byte by byte as 2-digit hex; the response is ignored.
        let mut line = String::with_capacity(data.len() * 2);
        for &b in data {
            line.push_str(&to_hex_str(b as u64, 2));
        }
        if self.send_command(&line, true).is_err() {
            return 0;
        }
        data.len()
    }
}

// ---------------------------------------------------------------------------
// Elm327Can — ELM327 CAN channel adapter
// ---------------------------------------------------------------------------

/// ELM327 CAN channel adapter.
/// `io` is an already-open serial byte stream (the factory provides it during
/// the unified registration stage; tests use an in-memory implementation).
/// [`Elm327Can::open`] performs the full open sequence, while
/// [`CanDevice::is_available`] only reports the resulting state.
pub struct Elm327Can<IO: Elm327Io> {
    core: DeviceCore,
    engine: Elm327Core<IO>,
    /// Cached availability flag set by open.
    opened: bool,
    /// Bus ID (generated at open or taken from the configuration).
    bus_id: String,
}

impl<IO: Elm327Io> Elm327Can<IO> {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.opened {
            // Stop receiving, then shut down the engine.
            self.engine.close();
            self.opened = false;
        }
    }

    /// Wraps an already-open serial byte stream.
    pub fn new(io: IO) -> Self {
        Self {
            core: DeviceCore::new(),
            engine: Elm327Core::new(io),
            opened: false,
            bus_id: String::new(),
        }
    }

    /// Whether the channel is currently open (init sequence completed).
    pub fn is_open(&self) -> bool {
        self.opened
    }

    /// Firmware version string (ATI response; empty until opened).
    pub fn version(&self) -> &str {
        &self.engine.version
    }

    /// Device description string (AT@1 response).
    pub fn device_name(&self) -> &str {
        &self.engine.device_name
    }

    /// Voltage reading string (ATRV response).
    pub fn voltage(&self) -> &str {
        &self.engine.voltage
    }

    /// Initialization command/response log.
    pub fn init_log(&self) -> &str {
        &self.engine.init_log
    }

    /// Passes an AT command through to the device and waits for the response.
    /// `Ok(None)` means the response timed out.
    pub fn send_cmd(&mut self, cmd: &str) -> Result<Option<String>> {
        // In the Error state, send an empty command first to abort.
        if self.engine.state == EngineState::Error {
            let _ = self.engine.send_command("", true)?;
        }
        self.engine.send_command(cmd, true)
    }
}

#[async_trait]
impl<IO: Elm327Io + Send> CanDevice for Elm327Can<IO> {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Opening already ran the full probe sequence; only report the state.
        Ok(self.opened)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.opened {
            return Ok(true);
        }
        // BusId = "{adapter}/{COMPort}/{media type}", e.g. "ELM327/COM1/CAN".
        let com_port = config.com_port.clone().unwrap_or_default();
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format!("ELM327/{com_port}/CAN"));
        // A missing COM port fails with "Failed to open  (...)"; since the IO
        // is pre-opened by the caller, a missing com_port is treated as that
        // failure path.
        if config.com_port.is_none() {
            return Err(config_err(
                &self.bus_id,
                open_failed_msg(&com_port, &config.com_port_config),
            ));
        }
        self.engine.open(&config, &self.bus_id)?;
        self.opened = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], _frame_type: FrameType) -> Result<usize> {
        // Not open -> 0; the FrameType parameter is not used by the engine
        // (ELM327 is classic CAN only, statistic frames are fixed CAN20B).
        if !self.opened {
            return Ok(0);
        }
        // Overlong payloads are rejected against the classic CAN limit instead
        // of failing silently on the device.
        if data.len() > MAX_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
                data.len()
            )));
        }
        // Statistics are recorded before the engine send (failures count too).
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, FrameType::CAN20B);
        self.core.record_sent(&frame);
        Ok(self.engine.send_frame(can_id, data))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if !self.opened {
            return Ok(None);
        }
        // Single pump per the trait's non-blocking contract, then take one
        // frame.
        self.engine.poll_io();
        let (id, data) = match self.engine.rx_queue.pop_front() {
            Some(f) => f,
            None => return Ok(None),
        };
        // Under a 29-bit protocol, add CAN_EXT_FLAG to the received ID.
        let id = if is_29bit_protocol(self.engine.protocol) {
            id | CAN_EXT_FLAG
        } else {
            id
        };
        Ok(Some(CanFrame::new(
            &self.bus_id,
            id,
            data,
            false,
            FrameType::CAN20B,
        )))
    }
}

impl<IO: Elm327Io> Drop for Elm327Can<IO> {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::CanBaudrate;

    /// In-memory fake device: each command terminated by '\r' pops one scripted
    /// response (followed by a "\r>" prompt); once the script is exhausted the
    /// default response is "OK". `auto_prompt = false` simulates a dead device
    /// that never answers. `inject` lets tests push unsolicited device output
    /// (receive frames, etc.).
    struct FakeElm {
        open: bool,
        rx: VecDeque<u8>,
        written: Vec<u8>,
        scripts: VecDeque<&'static str>,
        fail_writes: bool,
        auto_prompt: bool,
    }

    impl FakeElm {
        fn new(scripts: &[&'static str]) -> Self {
            Self {
                open: true,
                rx: VecDeque::new(),
                written: Vec::new(),
                scripts: scripts.iter().copied().collect(),
                fail_writes: false,
                auto_prompt: true,
            }
        }

        /// Standard open-sequence script (protocol 6, ATDPN matches directly).
        fn standard_open() -> Self {
            Self::new(&[
                "OK",                         // ATE0
                "OK",                         // ATL0
                "OK",                         // ATS0
                "ELM327 v1.5",                // ATI -> Version
                "OBDII to RS232 Interpreter", // AT@1 -> ELMName
                "12.4V",                      // ATRV -> Voltage
                "6",                          // ATDPN -> current protocol 6
                "OK",                         // ATCS
                "OK",                         // ATH1
                "OK",                         // ATD1
                "OK",                         // ATCAF0
                "OK",                         // ATV0
                "OK",                         // ATBI
            ])
        }

        fn inject(&mut self, s: &str) {
            self.rx.extend(s.bytes());
        }

        fn take_written(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.written)
        }
    }

    impl Elm327Io for FakeElm {
        fn is_open(&self) -> bool {
            self.open
        }
        fn bytes_to_read(&mut self) -> usize {
            self.rx.len()
        }
        fn read(&mut self, buf: &mut [u8]) -> usize {
            let n = buf.len().min(self.rx.len());
            for slot in &mut buf[..n] {
                *slot = self.rx.pop_front().unwrap();
            }
            n
        }
        fn write(&mut self, data: &[u8]) {
            if self.fail_writes {
                self.open = false;
                return;
            }
            self.written.extend_from_slice(data);
            if data == b"\r" && self.auto_prompt {
                // Command end: pop a scripted response ("OK" once exhausted),
                // formatted as "resp\r>".
                let resp = self.scripts.pop_front().unwrap_or("OK");
                if !resp.is_empty() {
                    self.rx.extend(resp.bytes());
                }
                self.rx.extend(b"\r>".iter());
            }
        }
        fn discard_buffers(&mut self) {
            self.rx.clear();
        }
    }

    fn opened_device() -> Elm327Can<FakeElm> {
        let mut dev = Elm327Can::new(FakeElm::standard_open());
        autors_runtime::block_on(dev.open(CanConfiguration::with_com_port("COM3", false, 50, 6)))
            .unwrap();
        dev
    }

    #[test]
    fn init_command_table_matches_extracted_strings() {
        // The engine's 11 initialization commands.
        assert_eq!(
            INIT_COMMANDS,
            [
                "ATL0", "ATS0", "ATI", "AT@1", "ATRV", "ATCS", "ATH1", "ATD1", "ATCAF0", "ATV0",
                "ATBI"
            ]
        );
        assert_eq!(CMD_ATE0, "ATE0");
        assert_eq!(CMD_ATMA, "ATMA");
        assert_eq!(CMD_ATDPN, "ATDPN");
        assert_eq!(CMD_ATCP, "ATCP");
        assert_eq!(CMD_ATSH, "ATSH");
        assert_eq!(RESP_OK, "OK");
        assert_eq!(RESP_NOT_READY, "not ready to send");
    }

    #[test]
    fn protocol_enum_values_match_expected_contract() {
        // Integer values of the protocol numbers.
        assert_eq!(Elm327Protocol::None.as_i32(), -1);
        assert_eq!(Elm327Protocol::Auto.as_i32(), 0);
        assert_eq!(Elm327Protocol::Iso15765_500k11Bit.as_i32(), 6);
        assert_eq!(Elm327Protocol::Iso15765_500k29Bit.as_i32(), 7);
        assert_eq!(Elm327Protocol::Iso15765_250k11Bit.as_i32(), 8);
        assert_eq!(Elm327Protocol::Iso15765_250k29Bit.as_i32(), 9);
        assert_eq!(Elm327Protocol::SaeJ1939.as_i32(), 10);
        assert_eq!(Elm327Protocol::UserCan11Bit50kBaud.as_i32(), 12);
        assert_eq!(
            Elm327Protocol::from_i32(7),
            Some(Elm327Protocol::Iso15765_500k29Bit)
        );
        assert_eq!(Elm327Protocol::from_i32(99), Option::None);
        assert_eq!(
            Elm327Protocol::Iso15765_500k11Bit.cs_name(),
            "ISO15765_500k_11Bit"
        );
    }

    #[test]
    fn is_29bit_protocol_truth_table() {
        // Only the two 15765 29-bit protocols count; J1939 does not (quirk).
        assert!(!is_29bit_protocol(-1));
        assert!(!is_29bit_protocol(0));
        assert!(!is_29bit_protocol(6));
        assert!(is_29bit_protocol(7));
        assert!(!is_29bit_protocol(8));
        assert!(is_29bit_protocol(9));
        assert!(!is_29bit_protocol(10));
    }

    #[test]
    fn hex_codecs_match_expected_contract() {
        // to_hex_str: fixed-length uppercase, high bits truncated.
        assert_eq!(to_hex_str(0x18, 2), "18");
        assert_eq!(to_hex_str(0x18DAF110, 6), "DAF110");
        assert_eq!(to_hex_str(0x7E0, 3), "7E0");
        assert_eq!(to_hex_str(0x8000_07E0, 3), "7E0");
        assert_eq!(to_hex_str(0xAB, 2), "AB");
        assert_eq!(to_hex_str(0, 2), "00");
        // from_hex_str: fixed-length read, advances the offset, accepts either
        // case.
        let mut off = 0usize;
        assert_eq!(
            from_hex_str(b"18DAF1102AABB", &mut off, 8),
            Some(0x18DAF110)
        );
        assert_eq!(off, 8);
        assert_eq!(from_hex_str(b"18DAF1102AABB", &mut off, 1), Some(2));
        assert_eq!(from_hex_str(b"18DAF1102AABB", &mut off, 2), Some(0xAA));
        let mut lower = 0usize;
        assert_eq!(from_hex_str(b"abcdef", &mut lower, 6), Some(0xABCDEF));
        // Out of range -> None.
        let mut end = 2usize;
        assert_eq!(from_hex_str(b"12", &mut end, 2), Option::None);
    }

    #[test]
    fn parse_frame_line_11bit_and_29bit() {
        // 11-bit: 3-digit ID + 1 DLC digit + data.
        let (id, data) = parse_frame_line(b"7E880112233445566778", false).unwrap();
        assert_eq!(id, 0x7E8);
        assert_eq!(data, vec![0x01, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78]);
        // 29-bit: 8-digit ID.
        let (id, data) = parse_frame_line(b"18DAF1102AABB", true).unwrap();
        assert_eq!(id, 0x18DAF110);
        assert_eq!(data, vec![0xAA, 0xBB]);
        // A DLC of 0 is not a frame.
        assert!(parse_frame_line(b"7E80", false).is_none());
        // Short lines / non-hex at either end -> not a frame.
        assert!(parse_frame_line(b"OK", false).is_none());
        assert!(parse_frame_line(b"STOPPED", false).is_none());
        assert!(parse_frame_line(b"SEARCHING...", false).is_none());
        // DLC claiming more data than the line holds -> not a frame.
        assert!(parse_frame_line(b"7E8801", false).is_none());
        // Trailing extra characters are ignored (the total length is not
        // validated).
        let (id, data) = parse_frame_line(b"7E82AABBCC", false).unwrap();
        assert_eq!((id, data.len()), (0x7E8, 2));
    }

    #[test]
    fn parse_com_port_baud_matches_expected_contract() {
        assert_eq!(parse_com_port_baud("9600,8,N,1").unwrap(), 9600);
        assert_eq!(parse_com_port_baud(" 38400 ,8,N,1").unwrap(), 38400);
        assert_eq!(parse_com_port_baud("115200").unwrap(), 115200);
        assert!(matches!(
            parse_com_port_baud("x,8,N,1"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(parse_com_port_baud(""), Err(Error::Invalid(_))));
    }

    #[test]
    fn open_sequence_happy_path() {
        let mut dev = Elm327Can::new(FakeElm::standard_open());
        assert!(!dev.is_open());
        assert!(autors_runtime::block_on(
            dev.open(CanConfiguration::with_com_port("COM3", false, 50, 6))
        )
        .unwrap());
        assert!(dev.is_open());
        assert!(autors_runtime::block_on(dev.is_available()).unwrap());
        assert_eq!(dev.bus_id, "ELM327/COM3/CAN");
        assert_eq!(dev.version(), "ELM327 v1.5");
        assert_eq!(dev.device_name(), "OBDII to RS232 Interpreter");
        assert_eq!(dev.voltage(), "12.4V");
        assert!(dev.init_log().contains("ATI > ELM327 v1.5"));
        assert!(dev.init_log().contains("ATBI > OK"));
        // The full written command sequence (ATDPN matches, no ATTP/ATSP).
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert_eq!(
            written,
            "ATE0\rATL0\rATS0\rATI\rAT@1\rATRV\rATDPN\rATCS\rATH1\rATD1\rATCAF0\rATV0\rATBI\r"
        );
        assert!(dev.unique_bus_id() >= 1);
        // Repeated open is idempotent.
        assert!(autors_runtime::block_on(dev.open(CanConfiguration::default())).unwrap());
        autors_runtime::block_on(dev.close());
        assert!(!dev.is_open());
    }

    #[test]
    fn open_switches_protocol_via_attp_atsp() {
        // ATDPN returns 0 (AUTO), target 7: ATTP7 -> OK -> ATSP7.
        let mut dev = Elm327Can::new(FakeElm::new(&[
            "OK",
            "OK",
            "OK",
            "ELM327 v1.5",
            "x",
            "12.0V",
            "0",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
        ]));
        autors_runtime::block_on(dev.open(CanConfiguration::with_com_port("COM1", false, 50, 7)))
            .unwrap();
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert!(
            written.contains("ATDPN\rATTP7\rATSP7\r"),
            "written: {written}"
        );
        assert_eq!(dev.engine.protocol, 7);
    }

    #[test]
    fn open_fails_when_protocol_rejected() {
        // ATTP response not OK -> "Failed to set CAN protocol {name}!".
        let mut dev = Elm327Can::new(FakeElm::new(&[
            "OK",
            "OK",
            "OK",
            "ELM327 v1.5",
            "x",
            "12.0V",
            "6",
            "?",
        ]));
        let err = autors_runtime::block_on(
            dev.open(CanConfiguration::with_com_port("COM1", false, 50, 7)),
        )
        .unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("ELM327/COM1/CAN"), "msg: {msg}");
                assert!(
                    msg.contains("Failed to set CAN protocol ISO15765_500k_29Bit!"),
                    "msg: {msg}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn open_fails_when_device_silent_or_write_fails() {
        // Write failure: after three ATE0 retries, "Failed to response to init
        // command!".
        let mut fake = FakeElm::new(&[]);
        fake.fail_writes = true;
        let mut dev = Elm327Can::new(fake);
        let err = autors_runtime::block_on(
            dev.open(CanConfiguration::with_com_port("COM1", false, 5, 6)),
        )
        .unwrap_err();
        match err {
            Error::Driver(msg) => assert!(
                msg.contains("Failed to response to init command!"),
                "msg: {msg}"
            ),
            other => panic!("unexpected error: {other}"),
        }

        // Writes accepted but never answered: ATE0 times out (a missing
        // response is not a failure there), failing at ATDPN.
        let mut fake = FakeElm::new(&[]);
        fake.auto_prompt = false;
        let mut dev = Elm327Can::new(fake);
        let err = autors_runtime::block_on(
            dev.open(CanConfiguration::with_com_port("COM1", false, 5, 6)),
        )
        .unwrap_err();
        match err {
            Error::Driver(msg) => assert!(msg.contains("ATDPN"), "msg: {msg}"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn open_requires_com_port() {
        let mut dev = Elm327Can::new(FakeElm::standard_open());
        let err = autors_runtime::block_on(dev.open(CanConfiguration::default())).unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("Failed to open  (9600,8,N,1)"), "msg: {msg}")
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn send_switches_header_and_formats_data() {
        let mut dev = opened_device();
        dev.engine.io.take_written();
        // 11-bit protocol: ATSH + 3 digits; data is contiguous hex.
        assert_eq!(
            autors_runtime::block_on(dev.send(0x7E0, &[0x02, 0x3E, 0x80], FrameType::CAN20B))
                .unwrap(),
            3
        );
        // Same ID: no header switch.
        assert_eq!(
            autors_runtime::block_on(dev.send(0x7E0, &[0x01], FrameType::CAN20B)).unwrap(),
            1
        );
        // New ID: ATSH again.
        assert_eq!(
            autors_runtime::block_on(dev.send(0x700, &[0xAA], FrameType::CAN20B)).unwrap(),
            1
        );
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert_eq!(written, "ATSH7E0\r023E80\r01\rATSH700\rAA\r");
    }

    #[test]
    fn send_29bit_uses_atcp_atsh_and_strips_ext_flag() {
        let mut dev = Elm327Can::new(FakeElm::new(&[
            "OK",
            "OK",
            "OK",
            "ELM327 v1.5",
            "x",
            "12.0V",
            "7",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
        ]));
        autors_runtime::block_on(dev.open(CanConfiguration::with_com_port("COM1", false, 50, 7)))
            .unwrap();
        dev.engine.io.take_written();
        // Extended ID with CAN_EXT_FLAG: ATCP18 + ATSHDAF110 (the flag is
        // stripped).
        assert_eq!(
            autors_runtime::block_on(dev.send(
                0x18DAF110 | CAN_EXT_FLAG,
                &[0x02, 0x3E],
                FrameType::CAN20B
            ))
            .unwrap(),
            2
        );
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert_eq!(written, "ATCP18\rATSHDAF110\r023E\r");
    }

    #[test]
    fn send_guards_match_expected_contract() {
        let mut dev = opened_device();
        // Not open (after close) -> 0.
        autors_runtime::block_on(dev.close());
        assert_eq!(
            autors_runtime::block_on(dev.send(0x123, &[1, 2, 3], FrameType::CAN20B)).unwrap(),
            0
        );
        assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
        // Reopen; overlong payload -> Invalid.
        let mut dev = opened_device();
        let err =
            autors_runtime::block_on(dev.send(0x123, &[0u8; 9], FrameType::CAN20B)).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
        assert_eq!(
            autors_runtime::block_on(dev.send(0x123, &[0u8; 8], FrameType::CAN20B)).unwrap(),
            8
        );
    }

    #[test]
    fn receive_parses_device_stream() {
        let mut dev = opened_device();
        // One 11-bit frame plus one non-frame line.
        dev.engine.io.inject("7E880112233445566778\rNO DATA\r");
        let frame = autors_runtime::block_on(dev.receive())
            .unwrap()
            .expect("frame");
        assert_eq!(frame.id, 0x7E8); // 11-bit protocol: no extended flag
        assert_eq!(
            frame.data,
            vec![0x01, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78]
        );
        assert!(!frame.is_master_frame);
        assert_eq!(frame.frame_type, FrameType::CAN20B);
        assert_eq!(frame.bus_id, "ELM327/COM3/CAN");
        assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
    }

    #[test]
    fn receive_29bit_sets_ext_flag() {
        let mut dev = Elm327Can::new(FakeElm::new(&[
            "OK",
            "OK",
            "OK",
            "ELM327 v1.5",
            "x",
            "12.0V",
            "9",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
        ]));
        autors_runtime::block_on(dev.open(CanConfiguration::with_com_port("COM1", false, 50, 9)))
            .unwrap();
        dev.engine.io.inject("18DAF1102AABB\r");
        let frame = autors_runtime::block_on(dev.receive())
            .unwrap()
            .expect("frame");
        // 29-bit protocol: CAN_EXT_FLAG is added.
        assert_eq!(frame.id, 0x18DAF110 | CAN_EXT_FLAG);
        assert_eq!(frame.data, vec![0xAA, 0xBB]);
    }

    #[test]
    fn command_response_multiline_and_busy() {
        let mut dev = opened_device();
        // Multi-line responses are joined with \r\n and the trailing newline
        // is removed (a \n inside a scripted response would be filtered out of
        // device output, so the fake injects two lines directly).
        dev.engine.io.scripts.clear();
        dev.engine.io.scripts.push_back("LINE1\rLINE2");
        let resp = dev.send_cmd("ATX").unwrap();
        assert_eq!(resp.as_deref(), Some("LINE1\r\nLINE2"));
        // No answer -> timeout None; the state stays Waiting, so the next
        // waiting command reports "not ready to send".
        dev.engine.io.auto_prompt = false;
        let resp = dev.send_cmd("ATY").unwrap();
        assert!(resp.is_none());
        let resp = dev.send_cmd("ATZ").unwrap();
        assert_eq!(resp.as_deref(), Some(RESP_NOT_READY));
    }

    #[test]
    fn monitor_mode_sends_atma() {
        let mut dev = Elm327Can::new(FakeElm::new(&[
            "OK",
            "OK",
            "OK",
            "ELM327 v1.5",
            "x",
            "12.0V",
            "6",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
            "OK",
        ]));
        autors_runtime::block_on(dev.open(CanConfiguration::with_com_port("COM1", true, 50, 6)))
            .unwrap();
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert!(written.ends_with("ATBI\rATMA\r"), "written: {written}");
        // Frames in the monitor stream can be received directly ("7DF" + DLC 2
        // + 02 3E).
        dev.engine.io.inject("7DF2023E\r");
        let frame = autors_runtime::block_on(dev.receive())
            .unwrap()
            .expect("frame");
        assert_eq!(frame.id, 0x7DF);
        assert_eq!(frame.data, vec![0x02, 0x3E]);
        // close sends an empty command (a single '\r') to abort monitoring.
        autors_runtime::block_on(dev.close());
        let written = String::from_utf8(dev.engine.io.take_written()).unwrap();
        assert_eq!(written, "\r");
    }

    #[test]
    fn baudrate_config_irrelevant_but_accepted() {
        // Serial devices do not consume the CAN baudrate field (only
        // ELM327Protocol matters).
        let mut cfg = CanConfiguration::with_com_port("COM9", false, 50, 6);
        cfg.baudrate = CanBaudrate::B500Kbit;
        let mut dev = Elm327Can::new(FakeElm::standard_open());
        assert!(autors_runtime::block_on(dev.open(cfg)).unwrap());
        assert_eq!(dev.bus_id, "ELM327/COM9/CAN");
    }
}
