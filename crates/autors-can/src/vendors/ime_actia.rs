//! I+ME Actia / Bosch Motorsport LevelX driver adapter (LXN4atxs / LXN4xs1u
//! and similar CAN interfaces).
//! IMEActia is the CAN interface family from I+ME Actia GmbH (the assembly
//! metadata says "Uses the ESD CAN API", referring to the hardware OEM; the
//! actual communication is a proprietary LevelX message protocol). The driver
//! DLL name is not fixed: the full path is stored in the registry value `Dll`
//! under
//! `HKLM\SOFTWARE\[WOW6432Node\]{Bosch Motorsport|I+ME Actia GmbH}\LevelX\Interfaces\
//! {LXN4atxs|LXN4xs1u}`. The DLL exports only three functions: LXRDOBJECT
//! (read a message) / LXWROBJECT (write a message) / LXSETEVENT (register an
//! event notified on received frames); all take a single pointer argument and
//! return a u16 status code.
//! The registry key names, value names, export names, the login name "CL21"
//! and the error message texts are obfuscated strings inside the vendor
//! assembly; their plain-text values are baked in here as constants. No
//! LevelX driver was installed on the development machine (none of the three
//! registry keys existed, and no candidate DLL was found under System32/
//! SysWOW64/Program Files), so **the export table could not be exercised
//! against a live driver DLL; it rests on the vendor assembly's declarations
//! and embedded strings**. All exports use the cdecl calling convention
//! (`extern "C"`, unlike Kvaser's stdcall). When the driver is not installed
//! or an export is missing, construction returns [`Error::Driver`].

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_DLC};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

// ---------------------------------------------------------------------------
// LevelX message protocol constants
// ---------------------------------------------------------------------------

/// Read/write buffer size (the driver message buffers are allocated with
/// 4096 bytes).
const LX_BUF_SIZE: usize = 4096;

/// Message command codes. A message header is [total length u8][command code
/// u16 little-endian].
/// DriverOpen request (login, command 1).
const CMD_DRIVER_OPEN: u16 = 0x0200;
/// DriverOpen confirmation (version/report string, command 2).
const CMD_DRIVER_OPEN_CONF: u16 = 0x0300;
/// DriverClose (command 3).
const CMD_DRIVER_CLOSE: u16 = 0x0400;
/// SetupCan (channel + baudrate, command 4).
const CMD_SETUP_CAN: u16 = 0x0403;
/// SetupCan confirmation (command 5).
const CMD_SETUP_CAN_CONF: u16 = 0x0503;
/// SetupFilterMasks (command 6).
const CMD_SETUP_FILTER: u16 = 0x0603;
/// FilterMasks confirmation (command 7).
const CMD_SETUP_FILTER_CONF: u16 = 0x0703;
/// BusOn (command 8).
const CMD_BUS_ON: u16 = 0x0803;
/// BusOff (command 9).
const CMD_BUS_OFF: u16 = 0x0A03;
/// Transmit frame (command 10).
const CMD_TX_FRAME: u16 = 0x1403;
/// Receive frame (command 11).
const CMD_RX_FRAME: u16 = 0x1903;

/// LevelX status code Ok (the status codes form a u16 enum).
const LX_OK: u16 = 0;
/// Status code NoObjectAvailable (read queue empty).
const LX_NO_OBJECT_AVAILABLE: u16 = 1;
/// Status code WriteBufferFull (transmit buffer full; retried for up to
/// 20 ms).
const LX_WRITE_BUFFER_FULL: u16 = 2;

/// DriverOpen message total length (login struct: header 3 + reserved 1 +
/// name 9 + password 14).
const MSG_DRIVER_OPEN_LEN: u8 = 27;
/// SetupCan message total length (header 3 + 9 parameter bytes).
const MSG_SETUP_CAN_LEN: u8 = 12;
/// FilterMasks message total length (header 3 + channel 1 + 4×u32 masks).
const MSG_SETUP_FILTER_LEN: u8 = 20;
/// BusOn/BusOff message total length (header 3 + channel 1).
const MSG_BUS_ON_OFF_LEN: u8 = 4;
/// DriverClose message total length (header only).
const MSG_DRIVER_CLOSE_LEN: u8 = 3;
/// TX/RX frame message total length (header 3 + channel 1 + reserved 1 +
/// DLC/flags 1 + ID field 4 + data 8).
const MSG_FRAME_LEN: u8 = 18;

/// Frame flags byte bit 7: extended frame.
const RX_FLAG_EXT: u8 = 0x80;
/// Frame flags byte bit 6: error/status frame (skipped by the receive path).
const RX_FLAG_SKIP: u8 = 0x40;
/// Frame flags byte low 4 bits: DLC.
const RX_DLC_MASK: u8 = 0x0F;

/// DriverOpen login name ("CL21").
const LOGIN_NAME: &[u8] = b"CL21";

/// Magic byte at SetupCan message offset 7 (218).
const SETUP_CAN_MAGIC: u8 = 218;

// ---------------------------------------------------------------------------
// Status codes / baudrate / ID conversions (pure functions)
// ---------------------------------------------------------------------------

/// Name of a LevelX status code; unnamed values return None so that callers
/// can fall back to the numeric form (`format!("{status}")` style).
fn status_name(status: u16) -> Option<&'static str> {
    match status {
        0 => Some("Ok"),
        1 => Some("NoObjectAvailable"),
        2 => Some("WriteBufferFull"),
        3 => Some("SetEvent"),
        4 => Some("ChannelOpen"),
        5 => Some("InvalidBoard"),
        6 => Some("IllegalObject"),
        _ => None,
    }
}

/// Display form of a status code (enum name or number).
fn status_display(status: u16) -> String {
    match status_name(status) {
        Some(name) => name.to_string(),
        None => status.to_string(),
    }
}

/// Maps a library baudrate enum value to the driver baudrate byte.
/// Unsupported rates return [`Error::NotSupported`] with the message text
/// `Baudrate '<name>' not supported!`.
fn baudrate_code(baudrate: CanBaudrate) -> Result<u8> {
    match baudrate {
        CanBaudrate::B10Kbit => Ok(0),
        CanBaudrate::B20Kbit => Ok(1),
        CanBaudrate::B50Kbit => Ok(2),
        CanBaudrate::B100Kbit => Ok(6),
        CanBaudrate::B125Kbit => Ok(7),
        CanBaudrate::B250Kbit => Ok(8),
        CanBaudrate::B500Kbit => Ok(9),
        CanBaudrate::B800Kbit => Ok(10),
        CanBaudrate::B1Mbit => Ok(11),
        other => Err(Error::NotSupported(format!(
            "Baudrate '{}' not supported!",
            other.cs_name()
        ))),
    }
}

/// Encodes a CAN ID for the wire: returns (wire field value, extended flag
/// bit). The wire field is a 32-bit byte-swapped value; stored little-endian
/// in the message struct, it appears big-endian on the wire.
fn tx_id_field(can_id: u32) -> (u32, u8) {
    if can_id & CAN_EXT_FLAG != 0 {
        let v = ((can_id & 0x7FFF_FFFF) << 3) & 0xFFFF_FFF8;
        (v.swap_bytes(), RX_FLAG_EXT)
    } else {
        let v = ((can_id << 5) & 0xFFE0) << 16;
        (v.swap_bytes(), 0)
    }
}

/// Decodes a received CAN ID; the exact inverse of `tx_id_field`.
fn rx_can_id(field: u32, flags: u8) -> u32 {
    if flags & RX_FLAG_EXT != 0 {
        ((field & 0xFF00_0000) >> 27)
            | ((field & 0x00FF_0000) >> 11)
            | ((field & 0x0000_FF00) << 5)
            | ((field & 0x0000_00FF) << 21)
            | CAN_EXT_FLAG
    } else {
        ((field & 0xFF00) >> 13) | ((field & 0xFF) << 3)
    }
}

// ---------------------------------------------------------------------------
// Message encode/decode (packed byte-level layout: every field follows the
// 3-byte header in declaration order, with no alignment padding)
// ---------------------------------------------------------------------------

/// Message header: [0] = total length, [1..3] = command code little-endian.
fn write_hdr(buf: &mut [u8], total_len: u8, cmd: u16) {
    buf[0] = total_len;
    buf[1..3].copy_from_slice(&cmd.to_le_bytes());
}

/// DriverOpen (cmd 0x0200): login name "CL21", empty password, parameter
/// byte 0. The message area is zeroed first.
fn build_driver_open(buf: &mut [u8]) {
    buf[..MSG_DRIVER_OPEN_LEN as usize].fill(0);
    write_hdr(buf, MSG_DRIVER_OPEN_LEN, CMD_DRIVER_OPEN);
    buf[4..4 + LOGIN_NAME.len()].copy_from_slice(LOGIN_NAME);
}

/// SetupCan (cmd 0x0403): channel, baudrate byte, mode always 0, magic byte
/// 218 at offset 7.
fn build_setup_can(buf: &mut [u8], channel: u8, baud_code: u8) {
    buf[..MSG_SETUP_CAN_LEN as usize].fill(0);
    write_hdr(buf, MSG_SETUP_CAN_LEN, CMD_SETUP_CAN);
    buf[3] = channel;
    buf[4] = baud_code;
    buf[7] = SETUP_CAN_MAGIC;
    // buf[8] = 0: the mode value is always 0.
}

/// FilterMasks (cmd 0x0603): channel field only; the 4×u32 masks stay 0.
fn build_setup_filter(buf: &mut [u8], channel: u8) {
    buf[..MSG_SETUP_FILTER_LEN as usize].fill(0);
    write_hdr(buf, MSG_SETUP_FILTER_LEN, CMD_SETUP_FILTER);
    buf[3] = channel;
}

/// BusOn / BusOff (cmd 0x0803/0x0A03): channel only.
fn build_bus_on_off(buf: &mut [u8], cmd: u16, channel: u8) {
    buf[..MSG_BUS_ON_OFF_LEN as usize].fill(0);
    write_hdr(buf, MSG_BUS_ON_OFF_LEN, cmd);
    buf[3] = channel;
}

/// DriverClose (cmd 0x0400): header only.
fn build_driver_close(buf: &mut [u8]) {
    buf[..MSG_DRIVER_CLOSE_LEN as usize].fill(0);
    write_hdr(buf, MSG_DRIVER_CLOSE_LEN, CMD_DRIVER_CLOSE);
}

/// Transmit frame (cmd 0x1403): channel + DLC/extended flags + wire ID +
/// data (the data field is fixed at 8 bytes, zero-padded).
fn build_tx_frame(buf: &mut [u8], channel: u8, can_id: u32, data: &[u8]) {
    debug_assert!(data.len() <= MAX_DLC);
    buf[..MSG_FRAME_LEN as usize].fill(0);
    write_hdr(buf, MSG_FRAME_LEN, CMD_TX_FRAME);
    let (field, ext) = tx_id_field(can_id);
    buf[3] = channel;
    // Flags byte: (Length & 0xF) | (ext ? 0x80 : 0).
    buf[5] = (data.len() as u8) | ext;
    // The u32 field is stored little-endian in the message struct.
    buf[6..10].copy_from_slice(&field.to_le_bytes());
    buf[10..10 + data.len()].copy_from_slice(data);
}

/// Report string of the DriverOpenConfirmation: buf[3..84] ASCII up to the
/// first NUL; bytes >= 0x80 are replaced with '?' (ASCII decoding
/// semantics). An empty string means the open succeeded; a non-empty string
/// is driver error text.
fn parse_open_report(buf: &[u8]) -> String {
    let field = &buf[3..84];
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    field[..end]
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '?' })
        .collect()
}

/// Parses a receive-frame message (cmd 0x1903): frames with bit 6 set are
/// error/status frames and are skipped; DLC = low 4 bits.
/// Frames with DLC > 8 are discarded and reception continues (intentionally
/// lenient behavior).
fn parse_rx_frame(buf: &[u8], bus_id: &str) -> Option<CanFrame> {
    let flags = buf[5];
    if flags & RX_FLAG_SKIP != 0 {
        return None;
    }
    let dlc = (flags & RX_DLC_MASK) as usize;
    if dlc > MAX_DLC {
        return None;
    }
    let field = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let id = rx_can_id(field, flags);
    Some(CanFrame::new(
        bus_id,
        id,
        buf[10..10 + dlc].to_vec(),
        false,
        FrameType::CAN20B,
    ))
}

// ---------------------------------------------------------------------------
// Win32 FFI (registry query + event handle; no extra crate dependencies,
// unsafe confined to this file)
// ---------------------------------------------------------------------------

#[link(name = "advapi32")]
extern "system" {
    /// LSTATUS RegGetValueW(HKEY, LPCWSTR, LPCWSTR, DWORD, LPDWORD, PVOID, LPDWORD).
    fn RegGetValueW(
        hkey: usize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        pdw_type: *mut u32,
        pv_data: *mut u16,
        pcb_data: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    /// HANDLE CreateEventW(LPSECURITY_ATTRIBUTES, BOOL, BOOL, LPCWSTR).
    fn CreateEventW(
        attrs: *mut std::ffi::c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> isize;
    /// BOOL CloseHandle(HANDLE).
    fn CloseHandle(handle: isize) -> i32;
}

/// HKEY_LOCAL_MACHINE (predefined handle: the 32-bit value 0x80000002
/// sign-extended to pointer width).
const HKEY_LOCAL_MACHINE: usize = (-2147483646isize) as usize;
/// RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ (both string types are accepted and
/// environment variables are not expanded).
const RRF_RT_STRING: u32 = 0x0000_0002 | 0x0000_0004;
/// ERROR_SUCCESS.
const ERROR_SUCCESS: i32 = 0;

/// Candidate registry key suffixes (the part after
/// HKLM\SOFTWARE\[WOW6432Node\]).
const REG_KEY_SUFFIXES: [&str; 3] = [
    r"Bosch Motorsport\LevelX\Interfaces\LXN4atxs",
    r"I+ME Actia GmbH\LevelX\Interfaces\LXN4atxs",
    r"I+ME Actia GmbH\LevelX\Interfaces\LXN4xs1u",
];
/// Registry value name holding the driver DLL path.
const REG_VALUE_NAME: &str = "Dll";

/// Reads a REG_SZ string with RegGetValueW; returns None when the key/value
/// does not exist or has a different type.
fn reg_query_string(subkey: &str, value: &str) -> Option<String> {
    let sub: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut size: u32 = 0;
    // SAFETY: only queries the data size (pv_data = null; by Win32 convention
    // the required byte count is returned); all pointers are valid for the
    // duration of this call.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_STRING,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; size as usize / 2 + 2];
    // SAFETY: buf capacity >= size bytes; RegGetValueW writes at most size
    // bytes and reports the actual size back.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_STRING,
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Queries the candidate registry keys' `Dll` values in order and returns
/// the first path that points to an existing file.
fn find_driver_path() -> Option<String> {
    // Select the registry view by pointer width (32-bit view on 64-bit
    // Windows).
    const VIEW: &str = if cfg!(target_pointer_width = "64") {
        "WOW6432Node\\"
    } else {
        ""
    };
    for suffix in REG_KEY_SUFFIXES {
        let subkey = format!("SOFTWARE\\{VIEW}{suffix}");
        if let Some(path) = reg_query_string(&subkey, REG_VALUE_NAME) {
            // Empty strings and non-existent files fall through to the next
            // key.
            if !path.is_empty() && std::path::Path::new(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Win32 auto-reset event used as the LevelX driver's receive-notification
/// handle, registered/unregistered via LXSETEVENT.
struct WinEvent(isize);

impl WinEvent {
    /// Creates an anonymous, auto-reset, initially non-signaled event.
    fn new_auto_reset() -> Result<Self> {
        // SAFETY: all-null parameters create an anonymous event; a NULL (0)
        // return means failure.
        let h = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) };
        if h == 0 {
            Err(Error::Driver("CreateEventW failed".to_string()))
        } else {
            Ok(Self(h))
        }
    }

    /// Raw handle value (passed to LXSETEVENT).
    fn handle(&self) -> isize {
        self.0
    }
}

impl Drop for WinEvent {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed exactly
        // once, here.
        unsafe { CloseHandle(self.0) };
    }
}

// ---------------------------------------------------------------------------
// LevelX function pointer table
// ---------------------------------------------------------------------------

/// LevelX function pointer table (all three symbols are loaded and validated
/// at construction; a missing export is reported as an error immediately).
/// All cdecl.
#[derive(Debug)]
struct LevelX {
    /// Keeps the library handle alive (the autors-native `DllWrapper`); the
    /// field is never accessed directly.
    _dll: DllWrapper,
    /// ushort LXSETEVENT(void *hEvent) — registers/unregisters the
    /// receive-notification event (NULL unregisters).
    lx_set_event: unsafe extern "C" fn(isize) -> u16,
    /// ushort LXRDOBJECT(void *buf) — reads one driver message into the
    /// buffer (>= LX_BUF_SIZE bytes).
    lx_rd_object: unsafe extern "C" fn(*mut u8) -> u16,
    /// ushort LXWROBJECT(void *buf) — writes one driver message from the
    /// buffer.
    lx_wr_object: unsafe extern "C" fn(*mut u8) -> u16,
}

impl LevelX {
    /// Locates and loads the driver DLL via the registry `Dll` value.
    fn load() -> Result<Self> {
        let path = find_driver_path().ok_or_else(|| {
            Error::Driver(format!(
                "IMEActia LevelX driver not found: no 'Dll' value under \
                 HKLM\\SOFTWARE\\[..]\\{}",
                REG_KEY_SUFFIXES.join(" or ")
            ))
        })?;
        Self::load_from(&path)
    }

    /// Loads the DLL from the given path/name and resolves all exported
    /// symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of DllMain execution) is
        // encapsulated in the autors-native DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied as raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, which keeps the pointers valid. The generic T is a
        // function pointer type (Copy).
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let s: libloading::Symbol<$ty> = unsafe { dll.library().get::<$ty>($name) }
                    .map_err(|e| {
                        Error::Driver(format!(
                            "{}: missing export {:?}: {}",
                            path,
                            String::from_utf8_lossy($name),
                            e
                        ))
                    })?;
                *s
            }};
        }
        Ok(Self {
            lx_set_event: sym!(b"LXSETEVENT\0", unsafe extern "C" fn(isize) -> u16),
            lx_rd_object: sym!(b"LXRDOBJECT\0", unsafe extern "C" fn(*mut u8) -> u16),
            lx_wr_object: sym!(b"LXWROBJECT\0", unsafe extern "C" fn(*mut u8) -> u16),
            _dll: dll,
        })
    }
}

// ---------------------------------------------------------------------------
// ImeActiaCan — CanDevice adapter
// ---------------------------------------------------------------------------

/// LevelX CAN channel adapter for I+ME Actia / Bosch Motorsport interfaces.
/// Following the non-blocking convention of [`CanDevice::receive`],
/// `receive` drains the driver message queue directly (blocking waits are
/// handled by the polling cadence of the upper-level
/// [`crate::device::start_dispatch`]). The notification event is still
/// registered with the driver so that it keeps delivering receive messages.
pub struct ImeActiaCan {
    core: DeviceCore,
    api: LevelX,
    /// Receive-notification event (auto-reset).
    event: WinEvent,
    /// Read buffer.
    rx_buf: Box<[u8; LX_BUF_SIZE]>,
    /// Write buffer.
    tx_buf: Box<[u8; LX_BUF_SIZE]>,
    /// Whether the channel is open.
    opened: bool,
    /// Current channel (needed by the send/close messages).
    channel: u8,
    /// Bus ID (generated at open or taken from the configuration).
    bus_id: String,
    /// Receive frame queue.
    rx_queue: VecDeque<CanFrame>,
}

impl ImeActiaCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        // Not open: return immediately.
        if !self.opened {
            return;
        }
        self.opened = false;
        self.rx_queue.clear();
        // Close sequence: build a BusOff message (cmd 0x0A03) -> write
        // DriverClose (0x0400) -> LXSETEVENT(NULL) to unregister -> write
        // DriverClose (0x0400) again; all return values are ignored.
        // Intentional quirk, kept as part of the behavioral contract: the
        // BusOff message is built but never actually written to the driver.
        build_bus_on_off(&mut self.tx_buf[..], CMD_BUS_OFF, self.channel);
        // (Built but not sent, see above.)
        build_driver_close(&mut self.tx_buf[..]);
        // SAFETY: tx_buf is valid; the driver does not retain the pointer.
        unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
        // SAFETY: NULL unregisters the event.
        unsafe { (self.api.lx_set_event)(0) };
        // SAFETY: same as above (tx_buf still holds the DriverClose message).
        unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
    }

    /// Locates and loads the LevelX driver DLL and creates the notification
    /// event; returns [`Error::Driver`] when the driver is not installed (no
    /// `Dll` path in the registry) or an export is missing.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: LevelX::load()?,
            event: WinEvent::new_auto_reset()?,
            rx_buf: Box::new([0; LX_BUF_SIZE]),
            tx_buf: Box::new([0; LX_BUF_SIZE]),
            opened: false,
            channel: 0,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether the channel is currently open.
    pub fn is_open(&self) -> bool {
        self.opened
    }

    /// Reads one message into rx_buf via LXRDOBJECT, returning (status code,
    /// command code).
    fn read_message(&mut self) -> (u16, u16) {
        // SAFETY: rx_buf is a valid LX_BUF_SIZE-byte buffer; the driver
        // writes one message per the protocol.
        let st = unsafe { (self.api.lx_rd_object)(self.rx_buf.as_mut_ptr()) };
        let cmd = if st == LX_OK {
            u16::from_le_bytes([self.rx_buf[1], self.rx_buf[2]])
        } else {
            0
        };
        (st, cmd)
    }

    /// Polls for a confirmation message with the given command code (1 ms
    /// interval, 2000 ms timeout; non-matching messages are consumed and
    /// discarded). Returns the status code of the last read.
    fn wait_confirmation(&mut self, expected_cmd: u16) -> u16 {
        let deadline = Instant::now() + Duration::from_millis(2000);
        let mut status = LX_NO_OBJECT_AVAILABLE;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            let (st, cmd) = self.read_message();
            status = st;
            if st == LX_OK && cmd == expected_cmd {
                return LX_OK;
            }
        }
        status
    }
}

#[async_trait]
impl CanDevice for ImeActiaCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Availability reporting only: `open` performs the full open
        // sequence, so this just reports the current state.
        Ok(self.opened)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        // Already open: return true immediately.
        if self.opened {
            return Ok(true);
        }
        // Generate the BusId ("IMEActia/CAN1") unless the configuration
        // carries one.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("IMEActia", config.channel));
        let channel = config.channel as u8;
        // FD configuration is not read (the device is classic CAN only);
        // BaudrateFD/BitRateConfig are ignored.

        // 1. DriverOpen (cmd 0x0200, login name "CL21"); no cleanup on
        //    failure.
        build_driver_open(&mut self.tx_buf[..]);
        // SAFETY: tx_buf is a valid LX_BUF_SIZE-byte buffer and the message
        // has been written per the protocol layout; the driver does not
        // retain the pointer.
        let st = unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
        if st != LX_OK {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "levelx: DriverOpen request failed with {}",
                    status_display(st)
                ),
            ));
        }
        // 2. DriverOpenConfirmation (cmd 0x0300); no cleanup on failure.
        let st = self.wait_confirmation(CMD_DRIVER_OPEN_CONF);
        if st != LX_OK {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "levelx: Failed to receive a DriverOpenConfirmation {}",
                    status_display(st)
                ),
            ));
        }
        // A non-empty report string is a driver error.
        let report = parse_open_report(&self.rx_buf[..]);
        if !report.is_empty() {
            // close_sync is a no-op at this point because `opened` is still
            // false (intentional).
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!("levelx: Driver open reports {report}"),
            ));
        }
        // 3. SetupCan (cmd 0x0403 -> 0x0503). If the baudrate mapping fails,
        //    no cleanup is performed (the mapping happens before the write
        //    call).
        let baud_code = baudrate_code(config.baudrate)?;
        build_setup_can(&mut self.tx_buf[..], channel, baud_code);
        // SAFETY: same as 1.
        let st = unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!("levelx: Failed to setup CAN {}", status_display(st)),
            ));
        }
        let st = self.wait_confirmation(CMD_SETUP_CAN_CONF);
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!(
                    "levelx: Failed to receive setup CAN confirmation {}",
                    status_display(st)
                ),
            ));
        }
        // 4. FilterMasks (cmd 0x0603 -> 0x0703).
        build_setup_filter(&mut self.tx_buf[..], channel);
        // SAFETY: same as 1.
        let st = unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!(
                    "levelx: Failed to setup CAN filter masks {}",
                    status_display(st)
                ),
            ));
        }
        let st = self.wait_confirmation(CMD_SETUP_FILTER_CONF);
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!(
                    "levelx: Failed to receive CAN filter masks confirmation {}",
                    status_display(st)
                ),
            ));
        }
        // 5. BusOn (cmd 0x0803, no confirmation wait).
        build_bus_on_off(&mut self.tx_buf[..], CMD_BUS_ON, channel);
        // SAFETY: same as 1.
        let st = unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!("levelx: Failed to turn Bus On {}", status_display(st)),
            ));
        }
        // 6. Register the receive-notification event.
        // SAFETY: the event handle was created by CreateEventW at
        // construction.
        let st = unsafe { (self.api.lx_set_event)(self.event.handle()) };
        if st != LX_OK {
            self.close_sync();
            return Err(config_err(
                &self.bus_id,
                format!("levelx: SetEvent failed with {}", status_display(st)),
            ));
        }
        self.channel = channel;
        self.opened = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // Not open: return 0.
        if !self.opened {
            return Ok(0);
        }
        // LevelX frame messages have a fixed 8-byte data field (classic CAN,
        // no FD): payloads over 8 bytes return [`Error::Invalid`].
        if data.len() > MAX_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC} \
                 (LevelX API has no CAN FD support)",
                data.len()
            )));
        }
        build_tx_frame(&mut self.tx_buf[..], self.channel, can_id, data);
        // WriteBufferFull: busy-retry (no sleep) for up to 20 ms; any other
        // non-Ok status returns 0 (no exception).
        let deadline = Instant::now() + Duration::from_millis(20);
        loop {
            // SAFETY: tx_buf is valid; the message has been written per the
            // layout.
            let st = unsafe { (self.api.lx_wr_object)(self.tx_buf.as_mut_ptr()) };
            if st == LX_OK {
                // Record statistics and return the data length.
                let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
                return Ok(self.core.record_sent(&frame));
            }
            if st != LX_WRITE_BUFFER_FULL || Instant::now() >= deadline {
                return Ok(0);
            }
        }
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if !self.opened {
            return Ok(None);
        }
        // Drain the driver message queue in one non-blocking pass; only
        // cmd 0x1903 messages are processed.
        loop {
            let (st, cmd) = self.read_message();
            if st != LX_OK {
                break;
            }
            if cmd != CMD_RX_FRAME {
                continue; // Late confirmation/status messages are discarded.
            }
            if let Some(frame) = parse_rx_frame(&self.rx_buf[..], &self.bus_id) {
                self.rx_queue.push_back(frame);
            }
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for ImeActiaCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_codes_match_expected_contract() {
        // Command codes as u16 built from byte pairs {lo, hi}.
        assert_eq!(CMD_DRIVER_OPEN, 512); // {0, 2}
        assert_eq!(CMD_DRIVER_OPEN_CONF, 768); // {0, 3}
        assert_eq!(CMD_DRIVER_CLOSE, 1024); // {0, 4}
        assert_eq!(CMD_SETUP_CAN, 1027); // {3, 4}
        assert_eq!(CMD_SETUP_CAN_CONF, 1283); // {3, 5}
        assert_eq!(CMD_SETUP_FILTER, 1539); // {3, 6}
        assert_eq!(CMD_SETUP_FILTER_CONF, 1795); // {3, 7}
        assert_eq!(CMD_BUS_ON, 2051); // {3, 8}
        assert_eq!(CMD_BUS_OFF, 2563); // {3, 10}
        assert_eq!(CMD_TX_FRAME, 5123); // {3, 20}
        assert_eq!(CMD_RX_FRAME, 6403); // {3, 25}
    }

    #[test]
    fn message_lengths_have_expected_struct_layouts() {
        // Packed struct sizes (Pack=1): the base header is 3 bytes (byte +
        // ushort).
        assert_eq!(MSG_DRIVER_OPEN_LEN, 3 + 1 + 9 + 14); // login struct
        assert_eq!(MSG_SETUP_CAN_LEN, 3 + 9); // SetupCan struct
        assert_eq!(MSG_SETUP_FILTER_LEN, 3 + 1 + 16); // FilterMasks struct
        assert_eq!(MSG_BUS_ON_OFF_LEN, 3 + 1); // BusOn/BusOff struct
        assert_eq!(MSG_DRIVER_CLOSE_LEN, 3); // DriverClose header only
        assert_eq!(MSG_FRAME_LEN, 3 + 2 + 1 + 4 + 8); // TX/RX frame struct
    }

    #[test]
    fn driver_open_message_layout() {
        let mut buf = [0xFFu8; LX_BUF_SIZE];
        build_driver_open(&mut buf);
        assert_eq!(buf[0], 27);
        assert_eq!(&buf[1..3], &CMD_DRIVER_OPEN.to_le_bytes());
        assert_eq!(buf[3], 0); // parameter byte is always 0
        assert_eq!(&buf[4..8], b"CL21"); // login name
        assert!(buf[8..27].iter().all(|&b| b == 0)); // name padding + 14-byte password
    }

    #[test]
    fn setup_can_message_layout() {
        let mut buf = [0xFFu8; LX_BUF_SIZE];
        build_setup_can(&mut buf, 2, 9); // channel 2, 500k -> 9
        assert_eq!(buf[0], 12);
        assert_eq!(&buf[1..3], &CMD_SETUP_CAN.to_le_bytes());
        assert_eq!(buf[3], 2);
        assert_eq!(buf[4], 9);
        assert_eq!(buf[5], 0);
        assert_eq!(buf[6], 0);
        assert_eq!(buf[7], 218); // magic byte
        assert_eq!(buf[8], 0); // mode is always 0
        assert_eq!(&buf[9..12], &[0, 0, 0]);
    }

    #[test]
    fn control_messages_layout() {
        let mut buf = [0xFFu8; LX_BUF_SIZE];
        build_setup_filter(&mut buf, 1);
        assert_eq!(buf[0], 20);
        assert_eq!(&buf[1..3], &CMD_SETUP_FILTER.to_le_bytes());
        assert_eq!(buf[3], 1);
        assert!(buf[4..20].iter().all(|&b| b == 0)); // all four u32 masks are 0

        build_bus_on_off(&mut buf, CMD_BUS_ON, 1);
        assert_eq!(buf[0], 4);
        assert_eq!(&buf[1..3], &CMD_BUS_ON.to_le_bytes());
        assert_eq!(buf[3], 1);
        build_bus_on_off(&mut buf, CMD_BUS_OFF, 1);
        assert_eq!(&buf[1..3], &CMD_BUS_OFF.to_le_bytes());

        build_driver_close(&mut buf);
        assert_eq!(buf[0], 3);
        assert_eq!(&buf[1..3], &CMD_DRIVER_CLOSE.to_le_bytes());
    }

    #[test]
    fn baudrate_code_table() {
        // Full baudrate mapping table.
        assert_eq!(baudrate_code(CanBaudrate::B10Kbit).unwrap(), 0);
        assert_eq!(baudrate_code(CanBaudrate::B20Kbit).unwrap(), 1);
        assert_eq!(baudrate_code(CanBaudrate::B50Kbit).unwrap(), 2);
        assert_eq!(baudrate_code(CanBaudrate::B100Kbit).unwrap(), 6);
        assert_eq!(baudrate_code(CanBaudrate::B125Kbit).unwrap(), 7);
        assert_eq!(baudrate_code(CanBaudrate::B250Kbit).unwrap(), 8);
        assert_eq!(baudrate_code(CanBaudrate::B500Kbit).unwrap(), 9);
        assert_eq!(baudrate_code(CanBaudrate::B800Kbit).unwrap(), 10);
        assert_eq!(baudrate_code(CanBaudrate::B1Mbit).unwrap(), 11);
        // Rates outside the table -> NotSupported (the message contains the
        // enum name).
        match baudrate_code(CanBaudrate::NotSet) {
            Err(Error::NotSupported(msg)) => assert_eq!(msg, "Baudrate 'NotSet' not supported!"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            baudrate_code(CanBaudrate::B2Mbit),
            Err(Error::NotSupported(_))
        ));
    }

    #[test]
    fn wire_id_encoding_and_roundtrip() {
        // Standard frame: ((id << 5) & 0xFFE0) << 16, then byte-swapped.
        let (field, ext) = tx_id_field(0x123);
        assert_eq!(ext, 0);
        assert_eq!(field, 0x0000_6024); // bswap32(0x2460_0000)
        assert_eq!(rx_can_id(field, ext), 0x123);
        // Extended frame: ((id & 0x7FFFFFFF) << 3) & 0xFFFFFFF8, then
        // byte-swapped.
        let (field, ext) = tx_id_field(0x8000_0123);
        assert_eq!(ext, 0x80);
        assert_eq!(field, 0x1809_0000); // bswap32(0x0000_0918)
        assert_eq!(rx_can_id(field, ext), 0x8000_0123);
        // Boundary round-trips (the receive-path flags contain DLC bits,
        // which do not affect decoding).
        let (field, ext) = tx_id_field(0x9FFF_FFFF);
        assert_eq!(rx_can_id(field, ext | 8), 0x9FFF_FFFF);
        let (field, ext) = tx_id_field(0x7FF);
        assert_eq!(rx_can_id(field, ext | 8), 0x7FF);
        let (field, ext) = tx_id_field(0);
        assert_eq!(rx_can_id(field, ext), 0);
    }

    #[test]
    fn tx_frame_message_layout() {
        let mut buf = [0xFFu8; LX_BUF_SIZE];
        build_tx_frame(&mut buf, 2, 0x123, &[0x11, 0x22, 0x33]);
        assert_eq!(buf[0], 18);
        assert_eq!(&buf[1..3], &CMD_TX_FRAME.to_le_bytes());
        assert_eq!(buf[3], 2); // channel
        assert_eq!(buf[4], 0); // reserved
        assert_eq!(buf[5], 3); // DLC=3, no extended bit
        assert_eq!(&buf[6..10], &[0x24, 0x60, 0x00, 0x00]); // wire ID field stored little-endian
        assert_eq!(&buf[10..13], &[0x11, 0x22, 0x33]);
        assert!(buf[13..18].iter().all(|&b| b == 0)); // data zero-padded to 8 bytes

        // Extended frame: bit 7 set.
        build_tx_frame(&mut buf, 0, 0x8000_0123, &[1; 8]);
        assert_eq!(buf[5], 8 | 0x80);
        assert_eq!(&buf[6..10], &[0x00, 0x00, 0x09, 0x18]);
    }

    #[test]
    fn rx_frame_parsing() {
        let mut buf = [0u8; LX_BUF_SIZE];
        write_hdr(&mut buf, MSG_FRAME_LEN, CMD_RX_FRAME);
        buf[3] = 1;
        let (field, ext) = tx_id_field(0x8000_0456);
        buf[5] = 8 | ext;
        buf[6..10].copy_from_slice(&field.to_le_bytes());
        buf[10..18].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let f = parse_rx_frame(&buf, "IMEActia/CAN1").unwrap();
        assert_eq!(f.id, 0x8000_0456);
        assert_eq!(f.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(!f.is_master_frame);
        assert_eq!(f.frame_type, FrameType::CAN20B);
        assert_eq!(f.bus_id, "IMEActia/CAN1");

        // Standard frame + DLC < 8: data truncated to DLC.
        let (field, _) = tx_id_field(0x456);
        buf[5] = 3;
        buf[6..10].copy_from_slice(&field.to_le_bytes());
        let f = parse_rx_frame(&buf, "B").unwrap();
        assert_eq!(f.id, 0x456);
        assert_eq!(f.data, vec![1, 2, 3]);

        // bit 6 set -> skipped (error/status frame filter).
        buf[5] = 0x40 | 8;
        assert!(parse_rx_frame(&buf, "B").is_none());
        // DLC > 8 -> the frame is discarded.
        buf[5] = 0x0F;
        assert!(parse_rx_frame(&buf, "B").is_none());
    }

    #[test]
    fn open_report_parsing() {
        let mut buf = [0u8; LX_BUF_SIZE];
        // All zeros -> empty string (open succeeded).
        assert_eq!(parse_open_report(&buf), "");
        // ASCII up to the first NUL.
        buf[3..7].copy_from_slice(b"busy");
        assert_eq!(parse_open_report(&buf), "busy");
        // Non-ASCII byte -> '?'.
        buf[3] = 0xC4;
        assert_eq!(parse_open_report(&buf), "?usy");
    }

    #[test]
    fn status_names_and_display() {
        // LevelX status enum names.
        assert_eq!(status_name(0), Some("Ok"));
        assert_eq!(status_name(1), Some("NoObjectAvailable"));
        assert_eq!(status_name(2), Some("WriteBufferFull"));
        assert_eq!(status_name(3), Some("SetEvent"));
        assert_eq!(status_name(4), Some("ChannelOpen"));
        assert_eq!(status_name(5), Some("InvalidBoard"));
        assert_eq!(status_name(6), Some("IllegalObject"));
        assert_eq!(status_name(7), None);
        assert_eq!(status_display(0), "Ok");
        assert_eq!(status_display(42), "42");
    }

    #[test]
    fn reg_query_string_reads_real_value() {
        // A value present on every Windows machine; verifies the
        // RegGetValueW FFI.
        let v = reg_query_string(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "ProductName",
        );
        assert!(v.is_some());
        assert!(v.unwrap().contains("Windows"));
        // Non-existent key -> None.
        assert!(reg_query_string(r"SOFTWARE\NoSuch\LevelX\Path", "Dll").is_none());
    }

    #[test]
    fn driver_path_lookup_does_not_panic() {
        // Missing LevelX registry keys return None; matching keys return the
        // configured path. The lookup must not panic.
        let _ = find_driver_path();
    }

    #[test]
    fn win_event_create_and_drop() {
        let ev = WinEvent::new_auto_reset().unwrap();
        assert!(ev.handle() != 0);
        drop(ev); // passes if CloseHandle does not crash
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that certainly does not exist: must yield Error::Driver,
        // not a panic.
        let err = LevelX::load_from("no_such_levelx_driver_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // Without a LevelX driver installed: Error::Driver; with the driver
        // installed: construction succeeds with a complete symbol table.
        // Both outcomes are acceptable; the key point is no panic.
        match ImeActiaCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                // When not open, send returns 0 and receive returns None.
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x123, &[1, 2, 3], FrameType::CAN20B))
                        .unwrap(),
                    0
                );
                assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
