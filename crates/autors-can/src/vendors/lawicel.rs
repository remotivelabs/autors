//! Adapter for the Lawicel CANUSB driver (`canusbdrv64.dll` on x64,
//! `canusbdrv.dll` on x86).
//! The driver DLL is loaded dynamically at runtime; the pointer width selects
//! the DLL name (64-bit builds load `canusbdrv64.dll`, 32-bit builds load
//! `canusbdrv.dll`). If the driver is not installed or an export is missing,
//! construction returns [`Error::Driver`] instead of panicking.
//!
//! Driver interface details:
//! - Exported symbols: `canusb_Open`, `canusb_Close`, `canusb_Read`,
//!   `canusb_Write`, `canusb_Status`, `canusb_SetTimeouts`,
//!   `canusb_setReceiveCallBack` (the 32-bit and 64-bit DLLs have identical
//!   export sets).
//! - Baudrate strings accepted by `canusb_Open`: "10", "20", "50", "100",
//!   "125", "250", "500", "800", "1000" (kbps), matching the public header
//!   documentation.
//! - Return codes: the read/write/control functions return 1 on success and
//!   negative values on failure (`ERROR_CANUSB_OK = 1`, see `error_name`).
//! - `CANMsg` layout: an 18-byte packed struct per the public header (`data`
//!   directly follows `len`), but the driver internally reads and writes the
//!   struct as 5 u32 words (20 bytes) — i.e. it touches 2 bytes past the end
//!   of the struct. All TX/RX buffers are therefore 20 bytes
//!   (`CANUSB_BUF_SIZE`) so the driver's accesses stay in bounds.
//!
//! Calling convention: the driver exports use `WINAPI` (stdcall on 32-bit;
//! identical to the C calling convention on x64), so `extern "system"` is
//! used uniformly.

use std::ffi::{c_void, CString};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// Driver DLL name, selected by pointer width.
#[cfg(target_pointer_width = "64")]
const CANUSB_DLL: &str = "canusbdrv64.dll";
/// See `CANUSB_DLL`.
#[cfg(not(target_pointer_width = "64"))]
const CANUSB_DLL: &str = "canusbdrv.dll";

// ---- Constants from the public LAWICEL CANUSB DLL header lawicel_can.h ----
/// ERROR_CANUSB_OK — success return value of the read/write/control
/// functions (a send counts as successful iff the return value == 1).
const CANUSB_OK: i32 = 1;
/// CANMSG_EXTENDED — extended-frame flag (value 128).
const CANMSG_EXTENDED: u8 = 0x80;
/// CANMSG_RTR — remote-frame flag (value 64), used for RX filtering.
const CANMSG_RTR: u8 = 0x40;
/// CANUSB_ACCEPTANCE_CODE_ALL — acceptance code passed to `canusb_Open` (0).
const CANUSB_ACCEPTANCE_CODE_ALL: u32 = 0;
/// CANUSB_ACCEPTANCE_MASK_ALL — acceptance mask passed to `canusb_Open`
/// (0xFFFFFFFF).
const CANUSB_ACCEPTANCE_MASK_ALL: u32 = 0xFFFF_FFFF;
/// CANUSB_FLAG_TIMESTAMP — fifth argument to `canusb_Open` (1).
const CANUSB_FLAG_TIMESTAMP: u32 = 0x0001;
/// Sentinel for an unopened handle (`uint.MaxValue`).
const INVALID_HANDLE: u32 = u32::MAX;
/// CAN ID mask applied when packing a frame: `id & 0x1FFFFFFF`.
const CANUSB_ID_MASK: u32 = 0x1FFF_FFFF;
/// Size of the packed CANMsg struct (Pack=1: u32 id + u32 timestamp +
/// u8 flags + u8 len + u64 data = 18 bytes, matching the public header
/// lawicel_can.h).
#[allow(dead_code)] // Layout anchor constant: referenced only by test assertions (buffers are actually allocated per CANUSB_BUF_SIZE)
const CANUSB_MSG_SIZE: usize = 18;
/// TX/RX buffer size: the driver internally reads and writes CANMsg as 5 u32
/// words (20 bytes) on both x86 and x64, so buffers must be >= 20 bytes to
/// keep the driver's accesses in bounds; bytes 18..20 are padding.
const CANUSB_BUF_SIZE: usize = 20;

/// Name of a Lawicel error code (per the public header lawicel_can.h:
/// success is 1, no-message is -7, timeout is -10, etc.), used in error
/// messages.
fn error_name(code: i32) -> &'static str {
    match code {
        1 => "ERROR_CANUSB_OK",
        -1 => "ERROR_CANUSB_GENERAL",
        -2 => "ERROR_CANUSB_OPEN_SUBSYSTEM",
        -3 => "ERROR_CANUSB_COMMAND_SUBSYSTEM",
        -4 => "ERROR_CANUSB_NOT_OPEN",
        -5 => "ERROR_CANUSB_TX_FIFO_FULL",
        -6 => "ERROR_CANUSB_INVALID_PARAM",
        -7 => "ERROR_CANUSB_NO_MESSAGE",
        -8 => "ERROR_CANUSB_MEMORY_ERROR",
        -9 => "ERROR_CANUSB_NO_DEVICE",
        -10 => "ERROR_CANUSB_TIMEOUT",
        -11 => "ERROR_CANUSB_INVALID_HARDWARE",
        _ => "ERROR_CANUSB_???",
    }
}

/// Maps a baudrate to the string `canusb_Open` expects.
/// The mapping ("10" = 10 kbps ... "1000" = 1 Mbps) matches the public
/// lawicel_can.h documentation. Baudrates outside the table yield
/// [`Error::NotSupported`] with the `CanBaudrate` enum name as the message
/// text (e.g. `NotSet`, `_2MBit`).
fn baudrate_str(baudrate: CanBaudrate) -> Result<&'static str> {
    match baudrate {
        CanBaudrate::B10Kbit => Ok("10"),
        CanBaudrate::B20Kbit => Ok("20"),
        CanBaudrate::B50Kbit => Ok("50"),
        CanBaudrate::B100Kbit => Ok("100"),
        CanBaudrate::B125Kbit => Ok("125"),
        CanBaudrate::B250Kbit => Ok("250"),
        CanBaudrate::B500Kbit => Ok("500"),
        CanBaudrate::B800Kbit => Ok("800"),
        CanBaudrate::B1Mbit => Ok("1000"),
        other => Err(Error::NotSupported(other.cs_name().to_string())),
    }
}

/// Packs a TX frame into the CANMsg wire layout: 18 bytes of payload +
/// 2 bytes of padding.
/// Two intentional quirks of the packing logic:
/// - The id is masked with `id & 0x1FFFFFFF`, silently dropping the high
///   bits of an extended ID;
/// - The extended-frame test inspects the **already masked** id
///   (`id & 0x80000000`), which is always false, so `flags` is always 0 —
///   the extended-frame flag is never set on TX (kept as part of the
///   behavioral contract).
///
/// `timestamp` is always 0; `len` is the raw data length cast to a byte (no
/// 8-byte truncation — oversized frames are rejected driver-side); the first
/// 8 data bytes are packed little-endian into a u64.
fn pack_tx_msg(can_id: u32, data: &[u8]) -> [u8; CANUSB_BUF_SIZE] {
    let mut buf = [0u8; CANUSB_BUF_SIZE];
    buf[0..4].copy_from_slice(&(can_id & CANUSB_ID_MASK).to_le_bytes());
    // buf[4..8] timestamp = 0; buf[8] flags = 0 (see above).
    buf[9] = data.len() as u8;
    let n = data.len().min(8);
    buf[10..10 + n].copy_from_slice(&data[..n]);
    buf
}

/// Frame acceptance and unpacking for received messages: only frames with
/// `len != 0` that are not RTR are accepted; when `flags & CANMSG_EXTENDED`
/// the id is ORed with [`CAN_EXT_FLAG`]; dlc is `len & 0xF` and the data is
/// unpacked little-endian from the u64 field. Returns `(id, data)`.
/// The dlc is additionally clamped to 8 as a defensive measure (the driver
/// protocol's len is 0-8 anyway, so `& 0xF` never exceeds 8).
fn unpack_rx_msg(buf: &[u8; CANUSB_BUF_SIZE]) -> Option<(u32, Vec<u8>)> {
    let flags = buf[8];
    let len = buf[9];
    if len == 0 || flags & CANMSG_RTR != 0 {
        return None;
    }
    let mut id = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if flags & CANMSG_EXTENDED != 0 {
        id |= CAN_EXT_FLAG;
    }
    let dlc = (len & 0xF).min(8) as usize;
    Some((id, buf[10..10 + dlc].to_vec()))
}

/// Function-pointer table for canusbdrv (all 7 symbols are loaded and
/// validated at construction).
/// The ABI uses 32-bit Windows `long` values and a `u32` `CANHANDLE`.
#[derive(Debug)]
struct CanusbDrv {
    /// Keeps the library handle alive (`DllWrapper` from autors-native); the
    /// field is never accessed directly.
    _dll: DllWrapper,
    /// CANHANDLE canusb_Open(LPCSTR szID, LPCSTR szBitrate, u32 acceptance_code,
    /// u32 acceptance_mask, u32 flags). szID = NULL opens the first device.
    canusb_open: unsafe extern "system" fn(*const c_void, *const c_void, u32, u32, u32) -> u32,
    /// int canusb_Close(CANHANDLE h).
    canusb_close: unsafe extern "system" fn(u32) -> i32,
    /// int canusb_Read(CANHANDLE h, CANMsg *msg). Used to poll for received
    /// frames, per the trait's non-blocking contract (no receive callback is
    /// registered).
    canusb_read: unsafe extern "system" fn(u32, *mut u8) -> i32,
    /// int canusb_Write(CANHANDLE h, CANMsg *msg).
    canusb_write: unsafe extern "system" fn(u32, *const u8) -> i32,
    /// int canusb_Status(CANHANDLE h); called right after opening, return
    /// value ignored.
    canusb_status: unsafe extern "system" fn(u32) -> i32,
    /// int canusb_SetTimeouts(CANHANDLE h, u32 receiveTimeout, u32 transmitTimeout);
    /// called with (0, 0) after opening.
    canusb_set_timeouts: unsafe extern "system" fn(u32, u32, u32) -> i32,
    /// int canusb_setReceiveCallBack(CANHANDLE h, LPFNDLL_RECEIVE_CALLBACK fn).
    /// Not used — this implementation polls instead of registering a callback
    /// (see the note on `LawicelCan::receive`); the symbol is still resolved
    /// so that loading validates the complete expected export set.
    #[allow(dead_code)]
    canusb_set_receive_callback: unsafe extern "system" fn(u32, *mut c_void) -> i32,
}

impl CanusbDrv {
    /// Loads the driver DLL from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(CANUSB_DLL)
    }

    /// Loads the driver DLL from the given path/name and resolves all
    /// exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe DllMain execution) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // taken only within this function and copied out as raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, so the pointers stay valid. The generic T is a
        // function-pointer type (Copy).
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
            canusb_open: sym!(
                b"canusb_Open\0",
                unsafe extern "system" fn(*const c_void, *const c_void, u32, u32, u32) -> u32
            ),
            canusb_close: sym!(b"canusb_Close\0", unsafe extern "system" fn(u32) -> i32),
            canusb_read: sym!(
                b"canusb_Read\0",
                unsafe extern "system" fn(u32, *mut u8) -> i32
            ),
            canusb_write: sym!(
                b"canusb_Write\0",
                unsafe extern "system" fn(u32, *const u8) -> i32
            ),
            canusb_status: sym!(b"canusb_Status\0", unsafe extern "system" fn(u32) -> i32),
            canusb_set_timeouts: sym!(
                b"canusb_SetTimeouts\0",
                unsafe extern "system" fn(u32, u32, u32) -> i32
            ),
            canusb_set_receive_callback: sym!(
                b"canusb_setReceiveCallBack\0",
                unsafe extern "system" fn(u32, *mut c_void) -> i32
            ),
            _dll: dll,
        })
    }
}

/// Lawicel CANUSB channel adapter.
/// Receive model and open/availability split:
/// - Instead of registering a receive callback via
///   `canusb_setReceiveCallBack` plus a queue and an event, received frames
///   are polled per the non-blocking contract of `CanDevice::receive` by
///   draining the driver's internal receive queue with `canusb_Read`
///   (`canusb_SetTimeouts(h, 0, 0)` guarantees non-blocking reads); the
///   frame acceptance rules are identical to callback-based reception;
/// - `open()` performs the open sequence while `is_available()` only
///   reports the current state (the same split as the Kvaser adapter).
pub struct LawicelCan {
    core: DeviceCore,
    api: CanusbDrv,
    /// CANUSB channel handle (`INVALID_HANDLE` means not opened).
    handle: u32,
    /// Bus ID assigned at open time (taken from the configuration when present).
    bus_id: String,
}

impl LawicelCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        // Close the channel: canusb_Close (return value ignored) + reset the
        // handle sentinel.
        if self.handle != INVALID_HANDLE {
            // SAFETY: the handle is valid.
            unsafe { (self.api.canusb_close)(self.handle) };
            self.handle = INVALID_HANDLE;
        }
    }

    /// Loads canusbdrv(64).dll and validates the symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: CanusbDrv::load()?,
            handle: INVALID_HANDLE,
            bus_id: String::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle != INVALID_HANDLE
    }
}

#[async_trait]
impl CanDevice for LawicelCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // `open()` performs the open sequence; `is_available()` only reports
        // the current state.
        Ok(self.handle != INVALID_HANDLE)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle != INVALID_HANDLE {
            return Ok(true);
        }
        // Generate the BusId ("Lawicel/CAN1" style). Note: canusb_Open's
        // szID is always NULL (opens the first device), so `config.channel`
        // only feeds the generated BusId, not device selection.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Lawicel", config.channel));
        // Baudrates outside the table are rejected as not supported.
        let bitrate =
            CString::new(baudrate_str(config.baudrate)?).expect("baudrate strings contain no NUL");
        // SAFETY: szID = NULL (first device); the bitrate pointer is valid
        // for the duration of the call; the remaining arguments are by value.
        // Returns the channel handle; only a return value of
        // uint.MaxValue (-1) is treated as failure (message text
        // "canusb_Open failed: ..." is part of the behavioral contract); any
        // other value is taken as a handle as-is (intentional lenient
        // behavior).
        let handle = unsafe {
            (self.api.canusb_open)(
                std::ptr::null(),
                bitrate.as_ptr().cast(),
                CANUSB_ACCEPTANCE_CODE_ALL,
                CANUSB_ACCEPTANCE_MASK_ALL,
                CANUSB_FLAG_TIMESTAMP,
            )
        };
        if handle == INVALID_HANDLE {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "canusb_Open failed: {} ({})",
                    error_name(handle as i32),
                    handle as i32
                ),
            ));
        }
        self.handle = handle;
        // After opening: canusb_Status + canusb_SetTimeouts(0, 0); both
        // return values are ignored.
        // SAFETY: the handle was returned by canusb_Open; arguments are by value.
        unsafe {
            (self.api.canusb_status)(self.handle);
            (self.api.canusb_set_timeouts)(self.handle, 0, 0);
        }
        // No receive callback is registered; receive() polls canusb_Read
        // instead (the driver enqueues received frames into an internal queue
        // by default).
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // FrameType is ignored: the hardware is CAN 2.0A/B only (the CANMsg
        // struct has no FD fields).
        let _ = frame_type;
        // When not open, send returns 0.
        if self.handle == INVALID_HANDLE {
            return Ok(0);
        }
        let msg = pack_tx_msg(can_id, data);
        // SAFETY: the handle is valid; the 20-byte msg buffer is valid for
        // the duration of the call (the driver reads 20 bytes, see
        // CANUSB_BUF_SIZE); canusb_Write enqueues synchronously and does not
        // retain the pointer.
        let st = unsafe { (self.api.canusb_write)(self.handle, msg.as_ptr()) };
        // A return value != 1 (ERROR_CANUSB_OK) yields 0 bytes sent (no
        // exception is raised).
        if st != CANUSB_OK {
            return Ok(0);
        }
        // Update statistics and return the data length.
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, FrameType::CAN20B);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        // When not open, receive returns None. Polls by draining canusb_Read
        // directly; the discard rules for RTR/zero-length frames are the same
        // as for callback-based reception (see unpack_rx_msg).
        if self.handle == INVALID_HANDLE {
            return Ok(None);
        }
        let mut buf = [0u8; CANUSB_BUF_SIZE];
        loop {
            // SAFETY: the handle is valid; buf is 20 bytes, no smaller than
            // the 20 bytes the driver writes (see CANUSB_BUF_SIZE).
            let st = unsafe { (self.api.canusb_read)(self.handle, buf.as_mut_ptr()) };
            if st != CANUSB_OK {
                // ERROR_CANUSB_NO_MESSAGE (-7) etc.: no frame available (any
                // non-OK status maps to None).
                return Ok(None);
            }
            if let Some((id, data)) = unpack_rx_msg(&buf) {
                return Ok(Some(CanFrame::new(
                    &self.bus_id,
                    id,
                    data,
                    false,
                    FrameType::CAN20B,
                )));
            }
        }
    }
}

impl Drop for LawicelCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baudrate_str_table() {
        // The complete baudrate-string mapping.
        assert_eq!(baudrate_str(CanBaudrate::B10Kbit).unwrap(), "10");
        assert_eq!(baudrate_str(CanBaudrate::B20Kbit).unwrap(), "20");
        assert_eq!(baudrate_str(CanBaudrate::B50Kbit).unwrap(), "50");
        assert_eq!(baudrate_str(CanBaudrate::B100Kbit).unwrap(), "100");
        assert_eq!(baudrate_str(CanBaudrate::B125Kbit).unwrap(), "125");
        assert_eq!(baudrate_str(CanBaudrate::B250Kbit).unwrap(), "250");
        assert_eq!(baudrate_str(CanBaudrate::B500Kbit).unwrap(), "500");
        assert_eq!(baudrate_str(CanBaudrate::B800Kbit).unwrap(), "800");
        assert_eq!(baudrate_str(CanBaudrate::B1Mbit).unwrap(), "1000");
        // Outside the table (including FD baudrates and NotSet) ->
        // NotSupported, with the CanBaudrate enum name as the message.
        match baudrate_str(CanBaudrate::NotSet) {
            Err(Error::NotSupported(m)) => assert_eq!(m, "NotSet"),
            other => panic!("unexpected: {other:?}"),
        }
        match baudrate_str(CanBaudrate::B2Mbit) {
            Err(Error::NotSupported(m)) => assert_eq!(m, "_2MBit"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tx_msg_layout() {
        // Standard frame: id as-is, timestamp=0, flags=0, len, data
        // little-endian from offset 10.
        let m = pack_tx_msg(0x123, &[0x11, 0x22, 0x33]);
        assert_eq!(&m[0..4], &0x123u32.to_le_bytes());
        assert_eq!(&m[4..8], &[0; 4]);
        assert_eq!(m[8], 0);
        assert_eq!(m[9], 3);
        assert_eq!(&m[10..13], &[0x11, 0x22, 0x33]);
        assert_eq!(&m[13..], &[0; 7]); // remaining data bytes + 2 padding bytes are 0

        // Extended-flag quirk: the test inspects the masked id, so flags is
        // always 0; the id is masked with & 0x1FFFFFFF.
        let m = pack_tx_msg(0x8000_0123 | CAN_EXT_FLAG, &[1]);
        assert_eq!(
            m[8], 0,
            "the compatibility contract leaves the extended flag unset"
        );
        let m = pack_tx_msg(0xA000_0123, &[1]);
        assert_eq!(&m[0..4], &0x123u32.to_le_bytes());

        // Oversized data: the len byte holds the raw length ((byte)12); the
        // data is truncated to the first 8 bytes.
        let m = pack_tx_msg(0x7FF, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(m[9], 12);
        assert_eq!(&m[10..18], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rx_msg_accept_filter() {
        let mut buf = [0u8; CANUSB_BUF_SIZE];
        buf[0..4].copy_from_slice(&0x456u32.to_le_bytes());
        buf[8] = 0; // flags
        buf[9] = 3; // len
        buf[10..13].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        let (id, data) = unpack_rx_msg(&buf).unwrap();
        assert_eq!(id, 0x456);
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC]);

        // Extended frame: the id gets CAN_EXT_FLAG.
        buf[8] = CANMSG_EXTENDED;
        let (id, _) = unpack_rx_msg(&buf).unwrap();
        assert_eq!(id, 0x456 | CAN_EXT_FLAG);

        // RTR frames are discarded; len == 0 is discarded.
        buf[8] = CANMSG_RTR;
        assert!(unpack_rx_msg(&buf).is_none());
        buf[8] = 0;
        buf[9] = 0;
        assert!(unpack_rx_msg(&buf).is_none());

        // dlc is len & 0xF.
        buf[9] = 0x10 | 5;
        let (_, data) = unpack_rx_msg(&buf).unwrap();
        assert_eq!(data.len(), 5);
    }

    #[test]
    fn error_names() {
        assert_eq!(error_name(1), "ERROR_CANUSB_OK");
        assert_eq!(error_name(-7), "ERROR_CANUSB_NO_MESSAGE");
        assert_eq!(error_name(-10), "ERROR_CANUSB_TIMEOUT");
        assert_eq!(error_name(-9999), "ERROR_CANUSB_???");
    }

    #[test]
    fn struct_layout_constants() {
        // Packed CANMsg = 18 bytes; the driver reads/writes 20 bytes ->
        // 20-byte buffers.
        assert_eq!(CANUSB_MSG_SIZE, 4 + 4 + 1 + 1 + 8);
        const { assert!(CANUSB_BUF_SIZE >= CANUSB_MSG_SIZE + 2) };
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield Error::Driver,
        // not a panic.
        let err = CanusbDrv::load_from("no_such_canusbdrv_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // A missing driver reports Error::Driver; an available driver loads a
        // complete symbol table. Neither outcome may panic.
        match LawicelCan::new() {
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
