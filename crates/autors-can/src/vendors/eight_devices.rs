//! Adapter for the 8devices Korlan USB2CAN device, driven through the CANAL
//! (CAN Abstraction Layer) C API exported by `usb2can.dll`.
//! The DLL uses the Winapi/stdcall calling convention, represented by
//! `extern "system"`, and accepts ANSI strings. Construction returns
//! [`Error::Driver`] when the DLL is not installed or an export is missing.
//!
//! Driver interface details:
//! - DLL name: `usb2can.dll`. A load failure surfaces the message
//!   "Failed to load usb2can.dll!".
//! - Six exports are used: CanalOpen/CanalGetStatus/CanalSend/CanalReceive/
//!   CanalBlockingReceive/CanalClose, matching the public CANAL API and the
//!   python-can wrapper one by one.
//! - Open connection string format `"ED123456;{0}"`: the device serial
//!   number is hard-coded as ED123456 and `{0}` is the baud rate in kbps;
//!   see `EightDevicesCan::open` for the error message format strings.

use std::collections::VecDeque;
use std::ffi::{c_char, CString};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_DLC};
use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFrame, FrameType};

/// Name of the 8devices CANAL driver library.
const USB2CAN_DLL: &str = "usb2can.dll";

// ---- CANAL constants (per the 8devices CANAL_API.pdf and the python-can
// usb2can wrapper) ----
/// CANAL_ERROR_SUCCESS.
const CANAL_SUCCESS: u32 = 0;
/// Fixed flags value passed to CanalOpen when opening a channel. The CANAL
/// spec leaves this parameter driver-defined (python-can defaults to 0x8;
/// this library always passes 4).
const CANAL_OPEN_FLAGS: u32 = 4;
/// canalMsg.flags: extended frame ID (CANAL_IDFLAG_EXTENDED).
const CANAL_IDFLAG_EXTENDED: u32 = 0x01;
/// canalMsg.flags: remote frame (CANAL_IDFLAG_RTR).
const CANAL_IDFLAG_RTR: u32 = 0x02;
/// canalMsg.flags: error frame (CANAL_IDFLAG_ERROR).
const CANAL_IDFLAG_ERROR: u32 = 0x04;
/// Receive filter mask 6: skip remote frames and error frames.
const CANAL_RX_SKIP_FLAGS: u32 = CANAL_IDFLAG_RTR | CANAL_IDFLAG_ERROR;
/// Hard-coded device serial number used in the open connection string
/// (format `"ED123456;{0}"`, where `{0}` is the baud rate in kbps).
const CANAL_CONNECT_SERIAL: &str = "ED123456";

/// Name of a CANAL error code (matching canal.h CANAL_ERROR_*), used in
/// error messages.
fn canal_error_name(status: u32) -> &'static str {
    match status {
        0 => "CANAL_ERROR_SUCCESS",
        1 => "CANAL_ERROR_BAUDRATE",
        2 => "CANAL_ERROR_BUS_OFF",
        3 => "CANAL_ERROR_BUS_PASSIVE",
        4 => "CANAL_ERROR_BUS_WARNING",
        5 => "CANAL_ERROR_CAN_ID",
        6 => "CANAL_ERROR_CAN_MESSAGE",
        7 => "CANAL_ERROR_CHANNEL",
        8 => "CANAL_ERROR_FIFO_EMPTY",
        9 => "CANAL_ERROR_FIFO_FULL",
        10 => "CANAL_ERROR_FIFO_SIZE",
        11 => "CANAL_ERROR_FIFO_WAIT",
        12 => "CANAL_ERROR_GENERIC",
        13 => "CANAL_ERROR_HARDWARE",
        14 => "CANAL_ERROR_INIT_FAIL",
        15 => "CANAL_ERROR_INIT_MISSING",
        16 => "CANAL_ERROR_INIT_READY",
        17 => "CANAL_ERROR_NOT_SUPPORTED",
        18 => "CANAL_ERROR_OVERRUN",
        19 => "CANAL_ERROR_RCV_EMPTY",
        20 => "CANAL_ERROR_REGISTER",
        21 => "CANAL_ERROR_TRM_FULL",
        22 => "CANAL_ERROR_ERRFRM_STUFF",
        23 => "CANAL_ERROR_ERRFRM_FORM",
        24 => "CANAL_ERROR_ERRFRM_ACK",
        25 => "CANAL_ERROR_ERRFRM_BIT1",
        26 => "CANAL_ERROR_ERRFRM_BIT0",
        27 => "CANAL_ERROR_ERRFRM_CRC",
        28 => "CANAL_ERROR_LIBRARY",
        29 => "CANAL_ERROR_PROCADDRESS",
        30 => "CANAL_ERROR_ONLY_ONE_INSTANCE",
        31 => "CANAL_ERROR_SUB_DRIVER",
        32 => "CANAL_ERROR_TIMEOUT",
        33 => "CANAL_ERROR_NOT_OPEN",
        34 => "CANAL_ERROR_PARAMETER",
        35 => "CANAL_ERROR_MEMORY",
        36 => "CANAL_ERROR_INTERNAL",
        37 => "CANAL_ERROR_COMMUNICATION",
        // The table includes one entry beyond canal.h: USER (38).
        38 => "CANAL_ERROR_USER",
        _ => "CANAL_ERROR_???",
    }
}

/// Map a status other than CANAL_ERROR_SUCCESS to [`Error::Driver`]; the
/// hardware ID is merged in by the caller via [`config_err`].
fn check(status: u32, bus_id: &str, what: &str) -> Result<()> {
    if status == CANAL_SUCCESS {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "usb2can: {what} failed: {} ({status})",
                canal_error_name(status)
            ),
        ))
    }
}

/// Build the open connection string: device serial number + baud rate in
/// kbps (e.g. `"ED123456;500"` for 500 kbit/s). A `NotSet(0)` baud rate is
/// opened as-is ("ED123456;0") — intentional behavior.
fn build_connect_str(baudrate_hz: u32) -> String {
    format!("{CANAL_CONNECT_SERIAL};{}", baudrate_hz / 1000)
}

/// Build a TX message: the extended flag is extracted from bit 0x80000000
/// of the ID; the ID itself is stored unmasked (the driver decides by flags
/// and ignores the high bits); the data is copied into an 8-byte buffer.
fn build_tx_msg(can_id: u32, data: &[u8]) -> CanalMsg {
    let mut buf = [0u8; 8];
    buf[..data.len()].copy_from_slice(data);
    CanalMsg {
        flags: if can_id & CAN_EXT_FLAG != 0 {
            CANAL_IDFLAG_EXTENDED
        } else {
            0
        },
        obid: 0,
        id: can_id,
        size_data: data.len() as u8,
        data: buf,
        timestamp: 0,
    }
}

/// Receive-side frame filter: `dlc != 0 && (flags & 6) == 0` — skip
/// empty-data frames, remote frames, and error frames.
fn accept_rx_frame(flags: u32, dlc: u8) -> bool {
    dlc != 0 && flags & CANAL_RX_SKIP_FLAGS == 0
}

/// Build an RX frame: the extended flag sets bit 0x80000000 of the ID; the
/// data is packed as a little-endian u64 plus length (frame type defaults
/// to CAN20B).
fn build_rx_frame(bus_id: &str, flags: u32, id: u32, data: [u8; 8], dlc: u8) -> CanFrame {
    let id = if flags & CANAL_IDFLAG_EXTENDED != 0 {
        id | CAN_EXT_FLAG
    } else {
        id
    };
    CanFrame::from_u64_le(bus_id, id, u64::from_le_bytes(data), dlc as usize, false)
}

// ---- canal.h structures ----
//
// canalMsg is declared here with default C alignment (`#[repr(C)]`), i.e. a
// 28-byte layout (timestamp at @24, preceded by 3 padding bytes), matching
// how usb2can.dll is actually compiled — canal.h places no packed attribute
// on canalMsg. A 25-byte packed layout would still share the same offsets
// for the first 21 bytes (flags/obid/id/sizeData/data[8]), so code that
// only reads and writes those fields works with either layout; python-can's
// ctypes wrapper uses default alignment and works against the real DLL,
// which is direct evidence for the C layout.

/// canal.h `canalMsg`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CanalMsg {
    /// CANAL_IDFLAG_*.
    flags: u32,
    /// Object ID (always 0 here).
    obid: u32,
    /// CAN ID.
    id: u32,
    /// Data length (DLC).
    size_data: u8,
    /// Data (8 bytes; equivalent to a little-endian u64 load/store).
    data: [u8; 8],
    /// Timestamp (ms; set to 0 on TX, not read on RX).
    timestamp: u32,
}

/// canal.h `canalStatus` (92 bytes; a packed layout and default alignment
/// coincide for this struct).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CanalStatus {
    /// Channel status (must be 0 after open).
    channel_status: u32,
    /// Last error code (CANAL_ERROR_*).
    last_error_code: u32,
    /// Last error subcode.
    last_error_subcode: u32,
    /// Last error description (ANSI, 80 bytes).
    last_error_str: [u8; 80],
}

// [u8; 80] has no Default implementation, so zero it manually.
impl Default for CanalStatus {
    fn default() -> Self {
        Self {
            channel_status: 0,
            last_error_code: 0,
            last_error_subcode: 0,
            last_error_str: [0; 80],
        }
    }
}

/// Function pointer table for usb2can.dll: the library is loaded and all
/// six symbols are resolved at construction; a missing symbol is reported
/// immediately as an error instead of surfacing later as a null call.
/// Field types are transcribed from the canal.h definitions (`long` is
/// 32-bit on Windows).
#[derive(Debug)]
struct Usb2can {
    /// Keeps the library handle alive (the autors-native [`DllWrapper`]);
    /// never accessed directly.
    _dll: DllWrapper,
    /// long CanalOpen(const char *pConfigureStr, unsigned long flags)
    /// (returns a channel handle; < 0 on failure).
    canal_open: unsafe extern "system" fn(*const c_char, u32) -> i32,
    /// int CanalGetStatus(long handle, canalStatus *pStatus)
    canal_get_status: unsafe extern "system" fn(i32, *mut CanalStatus) -> u32,
    /// int CanalSend(long handle, canalMsg *pMsg)
    canal_send: unsafe extern "system" fn(i32, *const CanalMsg) -> u32,
    /// int CanalReceive(long handle, canalMsg *pMsg)
    /// Used for non-blocking polling per the [`CanDevice::receive`]
    /// contract: when no frame is pending, the driver returns
    /// CANAL_ERROR_RCV_EMPTY immediately.
    canal_receive: unsafe extern "system" fn(i32, *mut CanalMsg) -> u32,
    /// int CanalBlockingReceive(long handle, canalMsg *pMsg, unsigned long timeout)
    /// Not called by this implementation: a timeout of 0 means an infinite
    /// wait in the driver, which does not fit the non-blocking contract.
    #[allow(dead_code)]
    // loaded to keep the symbol table complete; not called in the current flow
    canal_blocking_receive: unsafe extern "system" fn(i32, *mut CanalMsg, u32) -> u32,
    /// int CanalClose(long handle)
    canal_close: unsafe extern "system" fn(i32) -> u32,
}

impl Usb2can {
    /// Load usb2can.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(USB2CAN_DLL)
    }

    /// Load from the given path/name and resolve all exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of DllMain execution) is
        // encapsulated in the autors-native DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses
        // are taken and copied as raw function pointers only within this
        // function; the library handle and the function pointers live in the
        // same struct, which keeps the pointers valid for the struct's
        // lifetime. The generic T is a function pointer type (Copy).
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
        // CANAL has no global init/teardown function (each instance only
        // does LoadLibrary/FreeLibrary, with no reference counting), so
        // resolving the symbol table completes initialization.
        Ok(Self {
            canal_open: sym!(
                b"CanalOpen\0",
                unsafe extern "system" fn(*const c_char, u32) -> i32
            ),
            canal_get_status: sym!(
                b"CanalGetStatus\0",
                unsafe extern "system" fn(i32, *mut CanalStatus) -> u32
            ),
            canal_send: sym!(
                b"CanalSend\0",
                unsafe extern "system" fn(i32, *const CanalMsg) -> u32
            ),
            canal_receive: sym!(
                b"CanalReceive\0",
                unsafe extern "system" fn(i32, *mut CanalMsg) -> u32
            ),
            canal_blocking_receive: sym!(
                b"CanalBlockingReceive\0",
                unsafe extern "system" fn(i32, *mut CanalMsg, u32) -> u32
            ),
            canal_close: sym!(b"CanalClose\0", unsafe extern "system" fn(i32) -> u32),
            _dll: dll,
        })
    }
}

/// 8devices USB2CAN channel adapter.
/// Reception follows the non-blocking [`CanDevice::receive`] contract:
/// `receive()` drains the driver queue with CanalReceive, and any blocking
/// wait is left to the polling cadence of the upper layer
/// (`crate::device::start_dispatch`).
/// Only classic CAN is supported (CANAL has no CAN FD): only the nominal
/// `Baudrate` is used at open; `BaudrateFD`/`BitRateConfig` and the TX
/// `FrameType` are ignored — intentional behavior.
pub struct EightDevicesCan {
    core: DeviceCore,
    api: Usb2can,
    /// CANAL channel handle (< 0 when not open).
    handle: i32,
    /// Bus ID (generated at open or taken from the configuration).
    bus_id: String,
    /// Received-frame buffer.
    rx_queue: VecDeque<CanFrame>,
}

impl EightDevicesCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle >= 0 {
            // Close sequence: CanalClose(handle) with the return value
            // ignored; there is no background thread to stop.
            // SAFETY: handle is valid.
            unsafe { (self.api.canal_close)(self.handle) };
            self.handle = -1;
            self.rx_queue.clear();
        }
    }

    /// Load usb2can.dll and validate the symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: Usb2can::load()?,
            handle: -1,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle >= 0
    }
}

#[async_trait]
impl CanDevice for EightDevicesCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // open() performs the full open sequence; is_available() only
        // reports the current state.
        Ok(self.handle >= 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle >= 0 {
            return Ok(true);
        }
        // Generate a BusId ("8devices" + channel, e.g. "8devices/CAN1") when
        // the configuration does not carry one.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("8devices", config.channel));
        // Connection string "ED123456;{baudrate kbps}"; only the nominal
        // baud rate participates, FD configuration is ignored (intentional).
        let conn = build_connect_str(config.baudrate.as_u32());
        let c_conn = CString::new(conn.as_str())
            .map_err(|e| Error::Invalid(format!("usb2can: connection string contains NUL: {e}")))?;
        // SAFETY: c_conn outlives the call; CanalOpen opens synchronously
        // and does not retain the pointer.
        let handle = unsafe { (self.api.canal_open)(c_conn.as_ptr(), CANAL_OPEN_FLAGS) };
        if handle < 0 {
            // The message text is part of the behavioral contract:
            // "Failed to open driver with connection string: ".
            return Err(config_err(
                &self.bus_id,
                format!("Failed to open driver with connection string: {conn}"),
            ));
        }
        self.handle = handle;
        // CanalGetStatus(handle, out status): ret != 0 ||
        // status.channel_status != 0 is an open failure. The message text is
        // part of the behavioral contract: "Failed to open driver channel
        // state:{0}" with {0} = channel_status in decimal.
        let result = (|| {
            let mut status = CanalStatus::default();
            // SAFETY: handle is valid; status points to this stack frame's
            // CanalStatus buffer.
            let st = unsafe { (self.api.canal_get_status)(self.handle, &mut status) };
            check(st, &self.bus_id, "CanalGetStatus")?;
            let channel_state = status.channel_status;
            if channel_state != 0 {
                return Err(config_err(
                    &self.bus_id,
                    format!("Failed to open driver channel state:{channel_state}"),
                ));
            }
            Ok(())
        })();
        if let Err(e) = result {
            // On this failure path the handle is closed and reset, so a
            // later is_available() cannot misreport an open channel.
            // SAFETY: handle is valid; the return value is intentionally
            // ignored.
            unsafe { (self.api.canal_close)(self.handle) };
            self.handle = -1;
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], _frame_type: FrameType) -> Result<usize> {
        // Sending on a channel that is not open (not activated) returns 0.
        if self.handle < 0 {
            return Ok(0);
        }
        // CANAL message data is fixed at 8 bytes; an oversized payload is
        // rejected explicitly instead of silently building a message whose
        // DLC and data disagree.
        if data.len() > MAX_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
                data.len()
            )));
        }
        let msg = build_tx_msg(can_id, data);
        // SAFETY: handle is valid; the msg pointer is valid for the duration
        // of the call and its layout matches the driver; CanalSend transmits
        // synchronously and does not retain the pointer.
        let st = unsafe { (self.api.canal_send)(self.handle, &msg) };
        // A CanalSend status other than SUCCESS yields 0 (no exception).
        if st != CANAL_SUCCESS {
            return Ok(0);
        }
        // Record statistics and return the data length; CANAL is classic CAN
        // only and the FrameType parameter does not take part in TX (the
        // recorded statistics frame type is the default CAN20B —
        // intentional).
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, FrameType::CAN20B);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if self.handle < 0 {
            return Ok(None);
        }
        // Drain the driver queue with single CanalReceive probes, honoring
        // the trait's non-blocking contract (CanalBlockingReceive with a
        // timeout is not used here).
        loop {
            let mut msg = CanalMsg::default();
            // SAFETY: handle is valid; msg points to this stack frame's
            // CanalMsg buffer, whose layout matches the driver.
            let st = unsafe { (self.api.canal_receive)(self.handle, &mut msg) };
            if st != CANAL_SUCCESS {
                // CANAL_ERROR_RCV_EMPTY / TIMEOUT means no frame; any other
                // non-SUCCESS status also just stops the drain loop.
                break;
            }
            let (flags, id, dlc, data) = (msg.flags, msg.id, msg.size_data, msg.data);
            if accept_rx_frame(flags, dlc) {
                let frame = build_rx_frame(&self.bus_id, flags, id, data, dlc);
                self.rx_queue.push_back(frame);
            }
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for EightDevicesCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canal_struct_layouts() {
        // CanalMsg: 28 bytes with default C alignment (timestamp @24; see
        // the comment at the struct declaration — the real DLL uses default
        // C alignment, not a 25-byte packed layout).
        assert_eq!(std::mem::size_of::<CanalMsg>(), 28);
        assert_eq!(std::mem::align_of::<CanalMsg>(), 4);
        // CanalStatus: 92 bytes (packed and default-alignment layouts
        // coincide).
        assert_eq!(std::mem::size_of::<CanalStatus>(), 92);
        assert_eq!(std::mem::align_of::<CanalStatus>(), 4);
    }

    #[test]
    fn connect_str_matches_expected_contract() {
        // Connection string format: "ED123456;{baudrate in kbps}".
        assert_eq!(build_connect_str(500_000), "ED123456;500");
        assert_eq!(build_connect_str(125_000), "ED123456;125");
        assert_eq!(build_connect_str(250_000), "ED123456;250");
        assert_eq!(build_connect_str(1_000_000), "ED123456;1000");
        // Baudrate = NotSet(0) opens as-is: "ED123456;0".
        assert_eq!(build_connect_str(0), "ED123456;0");
    }

    #[test]
    fn build_tx_msg_fields() {
        // Standard frame: flags=0, ID stored unmasked.
        let msg = build_tx_msg(0x123, &[1, 2, 3]);
        assert_eq!(msg.flags, 0);
        assert_eq!(msg.obid, 0);
        assert_eq!(msg.id, 0x123);
        assert_eq!(msg.size_data, 3);
        assert_eq!(&msg.data[..3], &[1, 2, 3]);
        assert_eq!(&msg.data[3..], &[0; 5]);
        assert_eq!(msg.timestamp, 0);
        // Extended frame: flags=CANAL_IDFLAG_EXTENDED; the ID is not masked
        // (bit 0x80000000 is kept).
        let msg = build_tx_msg(0x1234 | CAN_EXT_FLAG, &[0xAB; 8]);
        assert_eq!(msg.flags, CANAL_IDFLAG_EXTENDED);
        assert_eq!(msg.id, 0x1234 | CAN_EXT_FLAG);
        assert_eq!(msg.size_data, 8);
        assert!(msg.data.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn build_rx_frame_matches_expected_contract() {
        // RX path: little-endian u64 data + len; extended flag -> ID gets
        // bit 0x80000000.
        let frame = build_rx_frame(
            "B",
            0,
            0x123,
            [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
            8,
        );
        assert_eq!(frame.id, 0x123);
        assert_eq!(
            frame.data,
            vec![0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(frame.frame_type, FrameType::CAN20B);
        assert!(!frame.is_master_frame);
        let ext = build_rx_frame("B", CANAL_IDFLAG_EXTENDED, 0x123, [0u8; 8], 2);
        assert_eq!(ext.id, 0x123 | CAN_EXT_FLAG);
        assert_eq!(ext.data.len(), 2);
    }

    #[test]
    fn rx_accept_filter() {
        // Normal frame.
        assert!(accept_rx_frame(0, 8));
        assert!(accept_rx_frame(CANAL_IDFLAG_EXTENDED, 1));
        // DLC == 0 is dropped.
        assert!(!accept_rx_frame(0, 0));
        // RTR(2)/ERROR(4) are dropped (the `(flags & 6) == 0` condition).
        assert!(!accept_rx_frame(CANAL_IDFLAG_RTR, 8));
        assert!(!accept_rx_frame(CANAL_IDFLAG_ERROR, 8));
        assert!(!accept_rx_frame(CANAL_IDFLAG_RTR | CANAL_IDFLAG_ERROR, 8));
    }

    #[test]
    fn canal_error_names() {
        assert_eq!(canal_error_name(0), "CANAL_ERROR_SUCCESS");
        assert_eq!(canal_error_name(1), "CANAL_ERROR_BAUDRATE");
        assert_eq!(canal_error_name(19), "CANAL_ERROR_RCV_EMPTY");
        assert_eq!(canal_error_name(32), "CANAL_ERROR_TIMEOUT");
        assert_eq!(canal_error_name(37), "CANAL_ERROR_COMMUNICATION");
        assert_eq!(canal_error_name(38), "CANAL_ERROR_USER");
        assert_eq!(canal_error_name(999), "CANAL_ERROR_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        let err = check(19, "8devices/CAN1", "CanalGetStatus").unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("8devices/CAN1"));
                assert!(msg.contains("CanalGetStatus"));
                assert!(msg.contains("CANAL_ERROR_RCV_EMPTY"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield
        // Error::Driver, not a panic.
        let err = Usb2can::load_from("no_such_usb2can_canal_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // A missing driver reports Error::Driver; an available driver loads a
        // complete symbol table. Neither outcome may panic.
        match EightDevicesCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                // send returns 0 and receive returns None while not open.
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
