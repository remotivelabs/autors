//! ESD NTCAN driver adapter.
//! Dynamically loads the ESD CAN-API (NTCAN) at runtime: `ntcan64.dll` in a
//! 64-bit process, `ntcan.dll` in a 32-bit process. All entry points use the
//! C calling convention (`extern "C"`; cdecl on 32-bit, unlike the stdcall
//! Kvaser/Peak drivers; the x64 convention is uniform). If the DLL is not
//! installed or an export is missing, construction returns [`Error::Driver`].
//! The driver exports used are `canOpen`/`canClose`/`canReadT`/`canWriteT`/
//! `canReadX`/`canWriteX`/`canSetBaudrate`/`canSetBaudrateX`/`canIdRegionAdd`,
//! matching the public NTCAN API naming. Function signatures follow the
//! documented NTCAN prototypes; the error-code names match NTCAN_RESULT in
//! the public ntcan.h.
//! Design notes (details in the per-item comments):
//! - No background receive thread is built in: [`CanDevice::receive`] does a
//!   single direct read that blocks inside the driver for at most the 200 ms
//!   receive timeout set via canOpen. Blocking and dispatch scheduling are
//!   handled by [`crate::device::start_dispatch`].
//! - If configuration after canOpen fails, the open NTCAN handle is closed
//!   with canClose before the error is returned (no handle leak).

use std::ffi::c_void;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, dlc_to_length, format_bus_id, from_ni_can_id, length_to_dlc, to_ni_can_id,
    CanDevice, DeviceCore, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// NTCAN library name (`ntcan64.dll` on 64-bit, `ntcan.dll` on 32-bit).
#[cfg(target_pointer_width = "64")]
const NTCAN_DLL: &str = "ntcan64.dll";
/// See the 64-bit [`NTCAN_DLL`].
#[cfg(not(target_pointer_width = "64"))]
const NTCAN_DLL: &str = "ntcan.dll";

/// NTCAN channel handle (NTCAN_HANDLE = void*).
type NtcHandle = *mut c_void;

// ---- NTCAN constants (error codes match the public ntcan.h NTCAN_RESULT;
// flag/parameter values follow the NTCAN API conventions) ----
/// NTCAN_SUCCESS.
const NTCAN_OK: u32 = 0;
/// canOpen base flags.
const CANOPEN_BASE_FLAGS: u32 = 16;
/// canOpen CAN FD flag (no ISO/non-ISO distinction is made).
const CANOPEN_CAN_FD: u32 = 262144;
/// canSetBaudrate explicit-baudrate flag (OR'd with the baudrate value in Hz).
const USER_BAUDRATE: u32 = 0x2000_0000;
/// NTCAN extended-ID flag (CAN ID bit 29, same value as NI's 0x20000000).
const NTCAN_EXT_FLAG: u32 = 0x2000_0000;
/// canOpen TX queue length.
const TX_QUEUE_SIZE: i32 = 1000;
/// canOpen RX queue length.
const RX_QUEUE_SIZE: i32 = 1000;
/// canOpen TX timeout in ms (0 = wait indefinitely for queue space).
const TX_TIMEOUT_MS: i32 = 0;
/// canOpen RX timeout in ms (a read call blocks inside the driver for at most this long).
const RX_TIMEOUT_MS: i32 = 200;
/// DLC mask within a CMSG len byte (`& 0xF`).
const LEN_DLC_MASK: u8 = 0x0F;
/// CAN FD frame flag in a CMSG_X len byte (`|= 128`).
const X_LEN_FD: u8 = 0x80;
/// "No BRS" flag in a CMSG_X len byte (set on TX when BRS is not requested;
/// if set on RX, BRS is cleared from the decoded frame type).
const X_LEN_NO_BRS: u8 = 0x10;

/// Name of an NTCAN_RESULT status code, used in error messages.
fn status_name(status: u32) -> &'static str {
    match status {
        0x0 => "NTCAN_SUCCESS",
        0x1 => "NTCAN_NEED_FW_UPDATE",
        0x2 => "NTCAN_HW_ERROR",
        0xE000_0001 => "NTCAN_RX_TIMEOUT",
        0xE000_0002 => "NTCAN_TX_TIMEOUT",
        0xE000_0004 => "NTCAN_TX_ERROR",
        0xE000_0005 => "NTCAN_CONTR_OFF_BUS",
        0xE000_0006 => "NTCAN_CONTR_BUSY",
        0xE000_0007 => "NTCAN_CONTR_WARN",
        0xE000_0008 => "NTCAN_OLDDATA",
        0xE000_0009 => "NTCAN_NO_ID_ENABLED",
        0xE000_000A => "NTCAN_ID_ALREADY_ENABLED",
        0xE000_000B => "NTCAN_ID_NOT_ENABLED",
        0xE000_000D => "NTCAN_INVALID_FIRMWARE",
        0xE000_000E => "NTCAN_MESSAGE_LOST",
        0xE000_000F => "NTCAN_INVALID_HARDWARE",
        0xE000_0010 => "NTCAN_PENDING_WRITE",
        0xE000_0011 => "NTCAN_PENDING_READ",
        0xE000_0012 => "NTCAN_INVALID_DRIVER",
        0xE000_0013 => "NTCAN_WRONG_DEVICE_STATE",
        0xE000_0014 => "NTCAN_HANDLE_FORCED_CLOSE",
        0xE000_0015 => "NTCAN_NOT_SUPPORTED",
        0xE000_0016 => "NTCAN_CONTR_ERR_PASSIVE",
        0xE000_0017 => "NTCAN_ERROR_NO_BAUDRATE",
        0xE000_0018 => "NTCAN_ERROR_LOM",
        0xE000_0019 => "NTCAN_NO_CAN_CAPABILITY",
        0xE000_001A => "NTCAN_NO_LIN_CAPABILITY",
        0xE000_0080 => "NTCAN_SOCK_CONN_TIMEOUT",
        0xE000_0081 => "NTCAN_SOCK_CMD_TIMEOUT",
        0xE000_0082 => "NTCAN_SOCK_HOST_NOT_FOUND",
        _ => "NTCAN_???",
    }
}

// ---------------------------------------------------------------------------
// NTCAN message structures. Natural Rust alignment is byte-identical to the
// packed layout the driver expects, so repr(C) is used.
// ---------------------------------------------------------------------------

/// CMSG header, 8 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)] // msg_lost/reserved are written by the driver only and never read here
struct CmsgHeader {
    /// NTCAN CAN ID (bit 29 = extended-frame flag; converted via [`to_ni_can_id`]).
    id: u32,
    /// len: low 4 bits = DLC; in CMSG_X, bit 7 = FD, bit 4 = no BRS.
    len: u8,
    /// Lost-frame counter (unused).
    msg_lost: u8,
    /// Reserved.
    reserved: [u8; 2],
}

/// Timestamped classic CMSG (ntcan.h CMSG_T), 24 bytes.
/// The data field is accessed as a little-endian u64; an 8-byte array is equivalent.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // timestamp is written by the driver only and never read
struct CmsgT {
    hdr: CmsgHeader,
    data: [u8; 8],
    timestamp: u64,
}

impl CmsgT {
    /// All-zero message.
    fn zeroed() -> Self {
        Self {
            hdr: CmsgHeader::default(),
            data: [0; 8],
            timestamp: 0,
        }
    }

    /// TX message: converts the ID, dlc = len & 0xF, copies up to 8 data
    /// bytes (zero-padded), timestamp 0.
    fn new_tx(can_id: u32, data: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        let n = data.len().min(8);
        buf[..n].copy_from_slice(&data[..n]);
        Self {
            hdr: CmsgHeader {
                id: to_ni_can_id(can_id),
                len: (data.len() as u8) & LEN_DLC_MASK,
                msg_lost: 0,
                reserved: [0; 2],
            },
            data: buf,
            timestamp: 0,
        }
    }
}

/// Timestamped CAN FD CMSG (ntcan.h CMSG_X), 80 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // timestamp is written by the driver only and never read
struct CmsgX {
    hdr: CmsgHeader,
    data: [u8; 64],
    timestamp: u64,
}

impl CmsgX {
    /// All-zero message (64-byte data cleared).
    fn zeroed() -> Self {
        Self {
            hdr: CmsgHeader::default(),
            data: [0; 64],
            timestamp: 0,
        }
    }

    /// TX message: converts the ID, computes the len byte via
    /// [`tx_x_len_byte`], copies the data into the 64-byte buffer,
    /// timestamp 0.
    fn new_tx(can_id: u32, data: &[u8], frame_type: FrameType) -> Result<Self> {
        let mut buf = [0u8; 64];
        buf[..data.len()].copy_from_slice(data);
        Ok(Self {
            hdr: CmsgHeader {
                id: to_ni_can_id(can_id),
                len: tx_x_len_byte(data.len(), frame_type)?,
                msg_lost: 0,
                reserved: [0; 2],
            },
            data: buf,
            timestamp: 0,
        })
    }
}

/// Shared 24-byte layout used by the BTR-register mode (mode=3) and the
/// simple-baudrate mode (mode=4): the first 8 bytes are identical
/// (mode/clock/4 reserved bytes); the remaining 16 bytes hold nominal/data
/// parameters (`[baudrate Hz, 0]` for mode=4; 4×u16 BTR parameters for
/// mode=3). Passed by value to canSetBaudrateX.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)] // FFI layout fields: the whole struct is passed by value to the driver; only tests read individual fields
struct BaudrateX {
    /// Mode: 3 = BTR registers; 4 = simple baudrate.
    mode: u16,
    /// Clock selection (always 1).
    clock: u16,
    /// Reserved (always 0).
    reserved: [u8; 4],
    /// Nominal (arbitration) phase parameters.
    nominal: [u32; 2],
    /// Data phase parameters.
    data: [u32; 2],
}

impl BaudrateX {
    /// mode=4, clock=1; nominal/data are baudrates in Hz.
    fn simple(nominal_hz: u32, data_hz: u32) -> Self {
        Self {
            mode: 4,
            clock: 1,
            reserved: [0; 4],
            nominal: [nominal_hz, 0],
            data: [data_hz, 0],
        }
    }

    /// mode=3, clock=1, everything else 0.
    /// Note: the configuration contents are intentionally ignored (only mode
    /// and clock are set); this quirk is part of the behavioral contract.
    fn btr_registers() -> Self {
        Self {
            mode: 3,
            clock: 1,
            ..Self::default()
        }
    }
}

// Layout sizes are checked at compile time.
const _: () = assert!(std::mem::size_of::<CmsgHeader>() == 8);
const _: () = assert!(std::mem::size_of::<CmsgT>() == 24);
const _: () = assert!(std::mem::size_of::<CmsgX>() == 80);
const _: () = assert!(std::mem::size_of::<BaudrateX>() == 24);

// ---------------------------------------------------------------------------
// Static helpers
// ---------------------------------------------------------------------------

/// canOpen flags: `16 | (fd ? 262144 : 0)`.
fn open_flags(config: &CanConfiguration) -> u32 {
    let mut flags = CANOPEN_BASE_FLAGS;
    if config.is_fd() {
        flags |= CANOPEN_CAN_FD;
    }
    flags
}

/// len byte for a CMSG_X TX message: low 4 bits are the DLC
/// (`length_to_dlc`, > 64 → [`Error::Invalid`]); FD sets 0x80; when BRS is
/// not requested, 0x10 is set (NTCAN uses that bit to mean "no bitrate
/// switch").
fn tx_x_len_byte(payload_len: usize, frame_type: FrameType) -> Result<u8> {
    let mut len = length_to_dlc(payload_len)?;
    if frame_type.contains(FrameType::FD) {
        len |= X_LEN_FD;
    }
    if !frame_type.contains(FrameType::BRS) {
        len |= X_LEN_NO_BRS;
    }
    Ok(len)
}

/// Decodes the frame type of a received FD frame: bit 0x80 → FD|BRS;
/// bit 0x10 clears BRS.
fn rx_x_frame_type(len: u8) -> FrameType {
    let mut frame_type = FrameType::CAN20B;
    if len & X_LEN_FD != 0 {
        frame_type = FrameType::FD_BRS;
    }
    if len & X_LEN_NO_BRS != 0 {
        frame_type = FrameType(frame_type.bits() & !FrameType::BRS.bits());
    }
    frame_type
}

/// NTCAN function-pointer table. All symbols are loaded and validated at
/// construction time; a missing symbol is reported as an error immediately.
/// Field types follow the NTCAN prototypes (`extern "C"` calling
/// convention); ref/out parameters correspond to raw pointers, and the
/// overlapped parameter is always null (synchronous calls).
#[derive(Debug)]
struct Ntcan {
    /// Keeps the library handle alive (`DllWrapper`); the field is never accessed directly.
    _dll: DllWrapper,
    /// NTCAN_RESULT canOpen(int32_t net, uint32_t flags, int32_t txqueuesize,
    /// int32_t rxqueuesize, int32_t txtimeout, int32_t rxtimeout,
    /// NTCAN_HANDLE *phandle).
    can_open: unsafe extern "C" fn(i32, u32, i32, i32, i32, i32, *mut NtcHandle) -> u32,
    /// NTCAN_RESULT canClose(NTCAN_HANDLE handle).
    can_close: unsafe extern "C" fn(NtcHandle) -> u32,
    /// NTCAN_RESULT canReadT(NTCAN_HANDLE, CMSG_T *cmsg, int32_t *len,
    /// OVERLAPPED *ovlp).
    can_read_t: unsafe extern "C" fn(NtcHandle, *mut CmsgT, *mut i32, *mut c_void) -> u32,
    /// NTCAN_RESULT canWriteT(NTCAN_HANDLE, CMSG_T *cmsg, int32_t *len,
    /// OVERLAPPED *ovlp).
    can_write_t: unsafe extern "C" fn(NtcHandle, *const CmsgT, *mut i32, *mut c_void) -> u32,
    /// NTCAN_RESULT canReadX(NTCAN_HANDLE, CMSG_X *cmsg, int32_t *len,
    /// OVERLAPPED *ovlp) (CAN FD).
    can_read_x: unsafe extern "C" fn(NtcHandle, *mut CmsgX, *mut i32, *mut c_void) -> u32,
    /// NTCAN_RESULT canWriteX(NTCAN_HANDLE, CMSG_X *cmsg, int32_t *len,
    /// OVERLAPPED *ovlp) (CAN FD).
    can_write_x: unsafe extern "C" fn(NtcHandle, *const CmsgX, *mut i32, *mut c_void) -> u32,
    /// NTCAN_RESULT canSetBaudrate(NTCAN_HANDLE, uint32_t baud).
    can_set_baudrate: unsafe extern "C" fn(NtcHandle, u32) -> u32,
    /// NTCAN_RESULT canSetBaudrateX(NTCAN_HANDLE, NTCAN_BITRATE_X btr)
    /// (CAN FD; the 24-byte struct is passed by value).
    can_set_baudrate_x: unsafe extern "C" fn(NtcHandle, BaudrateX) -> u32,
    /// NTCAN_RESULT canIdRegionAdd(NTCAN_HANDLE, int32_t id_start,
    /// int32_t *id_stop).
    can_id_region_add: unsafe extern "C" fn(NtcHandle, i32, *mut i32) -> u32,
}

impl Ntcan {
    /// Loads the NTCAN library from the default path (64-bit `ntcan64.dll`, 32-bit `ntcan.dll`).
    fn load() -> Result<Self> {
        Self::load_from(NTCAN_DLL)
    }

    /// Loads the library from the given path/name and resolves all exports.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe DllMain execution) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (each get inside the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, keeping the pointers valid for its lifetime. The
        // generic T is a function-pointer type (Copy).
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
            can_open: sym!(
                b"canOpen\0",
                unsafe extern "C" fn(i32, u32, i32, i32, i32, i32, *mut NtcHandle) -> u32
            ),
            can_close: sym!(b"canClose\0", unsafe extern "C" fn(NtcHandle) -> u32),
            can_read_t: sym!(
                b"canReadT\0",
                unsafe extern "C" fn(NtcHandle, *mut CmsgT, *mut i32, *mut c_void) -> u32
            ),
            can_write_t: sym!(
                b"canWriteT\0",
                unsafe extern "C" fn(NtcHandle, *const CmsgT, *mut i32, *mut c_void) -> u32
            ),
            can_read_x: sym!(
                b"canReadX\0",
                unsafe extern "C" fn(NtcHandle, *mut CmsgX, *mut i32, *mut c_void) -> u32
            ),
            can_write_x: sym!(
                b"canWriteX\0",
                unsafe extern "C" fn(NtcHandle, *const CmsgX, *mut i32, *mut c_void) -> u32
            ),
            can_set_baudrate: sym!(
                b"canSetBaudrate\0",
                unsafe extern "C" fn(NtcHandle, u32) -> u32
            ),
            can_set_baudrate_x: sym!(
                b"canSetBaudrateX\0",
                unsafe extern "C" fn(NtcHandle, BaudrateX) -> u32
            ),
            can_id_region_add: sym!(
                b"canIdRegionAdd\0",
                unsafe extern "C" fn(NtcHandle, i32, *mut i32) -> u32
            ),
            _dll: dll,
        })
    }
}

/// ESD CAN channel adapter.
/// Receiving is a single direct read per [`CanDevice::receive`] call (see
/// [`EsdCan::receive`]); no background receive thread is used.
pub struct EsdCan {
    core: DeviceCore,
    api: Ntcan,
    /// NTCAN channel handle (0 = not open; stored as isize to stay Send;
    /// -1 is the invalid handle value canOpen may return).
    handle: isize,
    /// Whether the channel was opened in FD mode.
    fd_opened: bool,
    /// Bus ID (generated at open time or taken from the configuration).
    bus_id: String,
}

impl EsdCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle != 0 {
            // Clear the handle first (stops further use), then canClose;
            // the return value is intentionally ignored.
            let handle = std::mem::take(&mut self.handle);
            // SAFETY: the handle came from a successful canOpen and has not been closed.
            unsafe { (self.api.can_close)(handle as NtcHandle) };
        }
    }

    /// Loads the NTCAN library and validates the symbol table; returns [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: Ntcan::load()?,
            handle: 0,
            fd_opened: false,
            bus_id: String::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle != 0
    }

    /// Bitrate configuration step of the open sequence (called after the channel is open).
    fn configure_bitrate(&self, config: &CanConfiguration) -> Result<()> {
        if self.fd_opened {
            let btr = match &config.fd_bit_rate_config {
                // BTR path: build the mode=3 BTR structure — the
                // configuration contents are intentionally ignored, only
                // mode/clock are set.
                Some(_) => BaudrateX::btr_registers(),
                None => BaudrateX::simple(config.baudrate.as_u32(), config.baudrate_fd.as_u32()),
            };
            // SAFETY: the handle was returned by canOpen (!= 0); the 24-byte struct is passed by value.
            let st = unsafe { (self.api.can_set_baudrate_x)(self.handle as NtcHandle, btr) };
            if st != NTCAN_OK {
                // Error message text: "SetBaudrateX: failed ({0}) to set baudrates to {1}".
                return Err(config_err(
                    &self.bus_id,
                    format!(
                        "SetBaudrateX: failed ({} 0x{st:08X}) to set baudrates to {}",
                        status_name(st),
                        config.bit_rate_str()
                    ),
                ));
            }
        } else if config.baudrate != CanBaudrate::NotSet {
            // When Baudrate != NotSet, call canSetBaudrate(handle, 0x20000000 | Hz);
            // when NotSet, the baudrate is left at the driver default.
            // SAFETY: the handle is valid; the argument is passed by value.
            let st = unsafe {
                (self.api.can_set_baudrate)(
                    self.handle as NtcHandle,
                    USER_BAUDRATE | config.baudrate.as_u32(),
                )
            };
            if st != NTCAN_OK {
                // Error message text: "SetBaudrate: failed to set baudrate to " + bitRateStr.
                return Err(config_err(
                    &self.bus_id,
                    format!(
                        "SetBaudrate: failed to set baudrate to {} ({} 0x{st:08X})",
                        config.bit_rate_str(),
                        status_name(st)
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CanDevice for EsdCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Opening is performed by open(); is_available() only reports the state.
        Ok(self.handle != 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle != 0 {
            return Ok(true);
        }
        // Generate the BusId (e.g. "ESD/CAN1") when the configuration has none.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("ESD", config.channel));
        self.fd_opened = config.is_fd();
        let flags = open_flags(&config);
        let mut raw: NtcHandle = std::ptr::null_mut();
        // SAFETY: all arguments are passed by value; raw points to a stack variable that is written before the function returns.
        let st = unsafe {
            (self.api.can_open)(
                config.channel,
                flags,
                TX_QUEUE_SIZE,
                RX_QUEUE_SIZE,
                TX_TIMEOUT_MS,
                RX_TIMEOUT_MS,
                &mut raw,
            )
        };
        if st != NTCAN_OK {
            // Error message text: "canOpen returned error {0}".
            return Err(config_err(
                &self.bus_id,
                format!("canOpen returned error {} (0x{st:08X})", status_name(st)),
            ));
        }
        if raw as isize == -1 {
            // Error message text: "canOpen: invalid handle configuring channel {0}".
            return Err(config_err(
                &self.bus_id,
                format!(
                    "canOpen: invalid handle configuring channel {}",
                    config.channel
                ),
            ));
        }
        self.handle = raw as isize;
        // On configuration failure, close the handle before returning (no leak).
        let result: Result<()> = (|| {
            self.configure_bitrate(&config)?;
            // Enable all receive IDs: standard frames 0..0x7FF, extended frames
            // 0x20000000..0x200007FF. The extended-region end is deliberately
            // 0x200007FF (the full 11-bit range under the extended flag).
            // Return values are intentionally not checked: a receive-filter
            // enable failure is silently ignored.
            let mut stop: i32 = 0x7FF;
            // SAFETY: the handle is valid; stop points to a stack variable.
            unsafe { (self.api.can_id_region_add)(self.handle as NtcHandle, 0, &mut stop) };
            let mut stop: i32 = (NTCAN_EXT_FLAG | 0x7FF) as i32;
            // SAFETY: same as above.
            unsafe {
                (self.api.can_id_region_add)(
                    self.handle as NtcHandle,
                    NTCAN_EXT_FLAG as i32,
                    &mut stop,
                )
            };
            Ok(())
        })();
        if let Err(e) = result {
            // SAFETY: the handle is valid and not yet closed.
            unsafe { (self.api.can_close)(self.handle as NtcHandle) };
            self.handle = 0;
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // When not open, send returns 0 without an error.
        if self.handle == 0 {
            return Ok(0);
        }
        if data.len() > MAX_FD_DLC {
            // Payloads exceeding the FD maximum are rejected uniformly with
            // Error::Invalid (the classic path would otherwise silently truncate).
            return Err(Error::Invalid(format!(
                "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
                data.len()
            )));
        }
        let mut len: i32 = 1;
        // Frame types beyond CAN20B (any FD bit) use canWriteX, otherwise canWriteT.
        let st = if !frame_type.is_classic() {
            let msg = CmsgX::new_tx(can_id, data, frame_type)?;
            // SAFETY: the handle is valid; the msg pointer is valid for the
            // duration of the call, len points to a stack variable; overlapped
            // is null (synchronous call); the driver does not retain the pointer.
            unsafe {
                (self.api.can_write_x)(
                    self.handle as NtcHandle,
                    &msg,
                    &mut len,
                    std::ptr::null_mut(),
                )
            }
        } else {
            let msg = CmsgT::new_tx(can_id, data);
            // SAFETY: same as above.
            unsafe {
                (self.api.can_write_t)(
                    self.handle as NtcHandle,
                    &msg,
                    &mut len,
                    std::ptr::null_mut(),
                )
            }
        };
        // On error (status != OK or len <= 0) send returns 0 instead of failing.
        if st != NTCAN_OK || len <= 0 {
            return Ok(0);
        }
        // Update the has-sent statistics and return the data length.
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if self.handle == 0 {
            return Ok(None);
        }
        // Each call performs a single read with len=1, blocking inside the
        // driver for at most the 200 ms receive timeout set by canOpen.
        // No frame, timeout, or error all return Ok(None).
        let mut len: i32 = 1;
        if self.fd_opened {
            let mut msg = CmsgX::zeroed();
            // SAFETY: the handle is valid; msg/len point to stack variables; overlapped is null.
            let st = unsafe {
                (self.api.can_read_x)(
                    self.handle as NtcHandle,
                    &mut msg,
                    &mut len,
                    std::ptr::null_mut(),
                )
            };
            // Accept a frame only when: OK && len > 0 && hdr.len > 0.
            if st != NTCAN_OK || len <= 0 || msg.hdr.len == 0 {
                return Ok(None);
            }
            let data_len = dlc_to_length(msg.hdr.len & LEN_DLC_MASK)? as usize;
            let frame_type = rx_x_frame_type(msg.hdr.len);
            // The received data is truncated to the frame length.
            let frame = CanFrame::with_len(
                &self.bus_id,
                from_ni_can_id(msg.hdr.id),
                msg.data.to_vec(),
                data_len,
                false,
                frame_type,
            )?;
            Ok(Some(frame))
        } else {
            let mut msg = CmsgT::zeroed();
            // SAFETY: same as above.
            let st = unsafe {
                (self.api.can_read_t)(
                    self.handle as NtcHandle,
                    &mut msg,
                    &mut len,
                    std::ptr::null_mut(),
                )
            };
            // Accept a frame only when: OK && len > 0 && hdr.len > 0.
            if st != NTCAN_OK || len <= 0 || msg.hdr.len == 0 {
                return Ok(None);
            }
            // dlc = len & 0xF; the data is loaded as a little-endian u64.
            let data_len = (msg.hdr.len & LEN_DLC_MASK) as usize;
            let frame = CanFrame::from_u64_le(
                &self.bus_id,
                from_ni_can_id(msg.hdr.id),
                u64::from_le_bytes(msg.data),
                data_len,
                false,
            );
            Ok(Some(frame))
        }
    }
}

impl Drop for EsdCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CAN_EXT_FLAG;
    use crate::frame::{BitRateConfig, CanFdBaudrate};

    #[test]
    fn struct_layout_sizes() {
        // Layout sizes (header 8 / CMSG_T 24 / CMSG_X 80 / BaudrateX 24).
        assert_eq!(std::mem::size_of::<CmsgHeader>(), 8);
        assert_eq!(std::mem::size_of::<CmsgT>(), 24);
        assert_eq!(std::mem::size_of::<CmsgX>(), 80);
        assert_eq!(std::mem::size_of::<BaudrateX>(), 24);
    }

    #[test]
    fn open_flags_computation() {
        // Classic CAN: only the base flag 16.
        let c = CanConfiguration::default();
        assert_eq!(open_flags(&c), 16);
        // FD (enum baudrate): 16 | 262144.
        let c = CanConfiguration::new(0, CanBaudrate::B500Kbit, CanFdBaudrate::B2Mbit);
        assert_eq!(open_flags(&c), 16 | 262144);
        // FD (full bit-timing config, non_iso): still 16 | 262144 (no ISO/non-ISO distinction).
        let c = CanConfiguration::with_bit_rate_config(
            0,
            BitRateConfig {
                non_iso: true,
                ..Default::default()
            },
        );
        assert_eq!(open_flags(&c), 16 | 262144);
    }

    #[test]
    fn tx_x_len_byte_computation() {
        // FD|BRS 8 bytes: dlc=8, sets 0x80.
        assert_eq!(tx_x_len_byte(8, FrameType::FD_BRS).unwrap(), 0x88);
        // FD without BRS: sets 0x80 | 0x10.
        assert_eq!(tx_x_len_byte(8, FrameType::FD).unwrap(), 0x98);
        // 12 bytes -> DLC 9.
        assert_eq!(tx_x_len_byte(12, FrameType::FD_BRS).unwrap(), 0x89);
        // 64 bytes -> DLC 15.
        assert_eq!(tx_x_len_byte(64, FrameType::FD_BRS).unwrap(), 0x8F);
        // CAN20B (never takes the X path in practice, but the builder logic allows it): only 0x10 set.
        assert_eq!(tx_x_len_byte(8, FrameType::CAN20B).unwrap(), 0x18);
        // > 64 -> Invalid.
        assert!(matches!(
            tx_x_len_byte(65, FrameType::FD_BRS),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn rx_x_frame_type_decoding() {
        // 0x80 -> FD|BRS.
        assert_eq!(rx_x_frame_type(0x88), FrameType::FD_BRS);
        // 0x80 | 0x10 -> FD (BRS cleared).
        assert_eq!(rx_x_frame_type(0x98), FrameType::FD);
        // No flags -> CAN20B.
        assert_eq!(rx_x_frame_type(0x08), FrameType::CAN20B);
        // Only 0x10 -> CAN20B (clearing the BRS bit of a zero value stays 0).
        assert_eq!(rx_x_frame_type(0x18), FrameType::CAN20B);
    }

    #[test]
    fn classic_cmsg_t_tx_layout() {
        // Extended ID: 0x80000000 -> 0x20000000; dlc = len & 0xF; data zero-padded.
        let msg = CmsgT::new_tx(0x8000_0123, &[0x11, 0x22, 0x33]);
        assert_eq!(msg.hdr.id, 0x2000_0123);
        assert_eq!(msg.hdr.len, 3);
        assert_eq!(msg.data, [0x11, 0x22, 0x33, 0, 0, 0, 0, 0]);
        assert_eq!(msg.timestamp, 0);
        // Standard ID unchanged; full 8 bytes.
        let msg = CmsgT::new_tx(0x456, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(msg.hdr.id, 0x456);
        assert_eq!(msg.hdr.len, 8);
        assert_eq!(msg.data, [1, 2, 3, 4, 5, 6, 7, 8]);
        // The classic path silently truncates: with > 8 bytes, dlc = len & 0xF
        // and only the first 8 bytes are kept.
        let msg = CmsgT::new_tx(0x1, &[0xAA; 10]);
        assert_eq!(msg.hdr.len, 10);
        assert_eq!(msg.data, [0xAA; 8]);
    }

    #[test]
    fn fd_cmsg_x_tx_layout() {
        let msg = CmsgX::new_tx(0x123, &[0xDE, 0xAD], FrameType::FD_BRS).unwrap();
        assert_eq!(msg.hdr.id, 0x123);
        assert_eq!(msg.hdr.len, 0x82); // dlc=2 | FD
        assert_eq!(msg.data[0..2], [0xDE, 0xAD]);
        assert!(msg.data[2..].iter().all(|&b| b == 0));
        assert_eq!(msg.timestamp, 0);
    }

    #[test]
    fn id_region_enable_values() {
        // Standard-frame region 0..2047; extended-frame start 536870912. The
        // extended-region end is deliberately 0x20000000|0x7FF = 536872959
        // (see the note in open()).
        assert_eq!(0x7FF, 2047);
        assert_eq!(NTCAN_EXT_FLAG, 536_870_912);
        assert_eq!(NTCAN_EXT_FLAG | 0x7FF, 536_872_959);
    }

    #[test]
    fn id_conversion_matches_expected_contract() {
        // The ID conversion follows the NI bit arithmetic.
        assert_eq!(to_ni_can_id(0x123), 0x123);
        assert_eq!(to_ni_can_id(0x8000_0123), 0x2000_0123);
        assert_eq!(to_ni_can_id(CAN_EXT_FLAG | 0x1FFF_FFFF), 0x3FFF_FFFF);
        assert_eq!(from_ni_can_id(0x123), 0x123);
        assert_eq!(from_ni_can_id(0x2000_0123), 0x8000_0123);
        // round-trip
        assert_eq!(from_ni_can_id(to_ni_can_id(0x9FFF_FFFF)), 0x9FFF_FFFF);
    }

    #[test]
    fn baudrate_x_builders() {
        // Simple baudrate: mode=4, clock=1; nominal/data in Hz.
        let b = BaudrateX::simple(500_000, 2_000_000);
        assert_eq!(b.mode, 4);
        assert_eq!(b.clock, 1);
        assert_eq!(b.reserved, [0; 4]);
        assert_eq!(b.nominal, [500_000, 0]);
        assert_eq!(b.data, [2_000_000, 0]);
        // BTR registers: mode=3, clock=1, everything else 0 (configuration contents ignored).
        let b = BaudrateX::btr_registers();
        assert_eq!(b.mode, 3);
        assert_eq!(b.clock, 1);
        assert_eq!(b.nominal, [0, 0]);
        assert_eq!(b.data, [0, 0]);
        assert_eq!(std::mem::size_of_val(&b), 24);
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(0), "NTCAN_SUCCESS");
        assert_eq!(status_name(0xE000_0001), "NTCAN_RX_TIMEOUT");
        assert_eq!(status_name(0xE000_0016), "NTCAN_CONTR_ERR_PASSIVE");
        assert_eq!(status_name(0xE000_0082), "NTCAN_SOCK_HOST_NOT_FOUND");
        assert_eq!(status_name(0xDEAD_BEEF), "NTCAN_???");
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that certainly does not exist: must yield Error::Driver, not a panic.
        let err = Ntcan::load_from("no_such_ntcan_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // Without the ESD driver installed (ntcan64.dll/ntcan.dll missing):
        // Error::Driver. On a machine with the driver installed: construction
        // succeeds and the symbol table is complete. Both outcomes are
        // acceptable; the key point is that there is no panic.
        match EsdCan::new() {
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
