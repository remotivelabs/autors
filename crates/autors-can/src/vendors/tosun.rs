//! TOSUN (TSMaster) CAN adapter — backend for devices driven by
//! `libTSCAN.dll`, the user-mode library of the TOSUN TSMaster driver.
//! The DLL is loaded dynamically at runtime. Its exports use Delphi-style
//! `__stdcall`, which is identical to the C calling convention on x64, so all
//! function pointers are uniformly declared `extern "system"`. If the DLL is
//! not installed or an expected export is missing, construction fails with
//! [`Error::Driver`].
//! All 13 imported symbols were checked against a real `libTSCAN.dll` export
//! table. Note that some driver packages ship a 32-bit (pei-i386) DLL; an x64
//! process needs the x64 build of the TSMaster driver. The adapter loads the
//! library by name and relies on the system DLL search path to resolve a
//! driver of matching bitness.
//! Design notes (details at each item):
//! - Receive callbacks: the driver callback signature carries no user pointer,
//!   so adapter instances are distinguished through a fixed pool of 16
//!   pre-generated slot callbacks; when all slots are taken, `open` fails with
//!   [`Error::Driver`].
//! - Received frames are buffered per instance in a queue and dequeued
//!   non-blockingly by [`CanDevice::receive`]; any waiting/pacing is left to
//!   the polling loop in [`crate::device::start_dispatch`].
//! - Adapter options (device serial, termination resistor, FD controller
//!   type/mode) are plain pub fields on [`TosunCan`] to be set before `open`.
//! - Failure semantics of `open`/`send`: recoverable failures return
//!   `Ok(false)`/`Ok(0)` and record a human-readable reason in
//!   [`TosunCan::last_error`]; only configuration errors (channel out of
//!   range, baudrate not set, unsupported FD custom bit timing) map to `Err`.
//!   The `last_error` text appends the symbolic status name after `failed: N.`
//!   for easier diagnosis.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, CanDevice, DeviceCore, CAN_EXT_FLAG};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType};

/// libTSCAN library name (loaded by name; resolution is left to the system
/// DLL search path).
const LIBTSCAN_DLL: &str = "libTSCAN.dll";

// ---- libTSCAN API constants ----
/// Payload capacity of a classic CAN frame.
const CAN_DATA_SIZE: usize = 8;
/// Payload capacity of a CAN FD frame.
const CAN_FD_DATA_SIZE: usize = 64;
/// Status code: success (`IDX_ERR_OK`).
const TSCAN_OK: u32 = 0;
/// Status code: device already connected; treated as success by `open`.
const IDX_ERR_ALREADY_CONNECTED: u32 = 5;
/// `FProperties` bit of `TLIBCAN`/`TLIBCANFD`: frame was transmitted (echo),
/// not received from the bus.
const PROP_IS_TX: u8 = 0x01;
/// `FProperties` bit: remote frame. Never set on the transmit path; kept only
/// for completeness.
#[allow(dead_code)]
const PROP_IS_REMOTE: u8 = 0x02;
/// `FProperties` bit: extended (29-bit) ID.
const PROP_IS_EXT: u8 = 0x04;
/// `FFDProperties` bit of `TLIBCANFD`: FD frame (EDL).
const FDPROP_EDL: u8 = 0x01;
/// `FFDProperties` bit: bit-rate switch (BRS).
const FDPROP_BRS: u8 = 0x02;
/// Size of the receive-callback slot pool (see the module docs).
const RX_SLOT_COUNT: usize = 16;

/// CAN FD controller type: plain CAN (no FD).
pub const FD_CONTROLLER_TYPE_CAN: i32 = 0;
/// CAN FD controller type: ISO CAN FD (default).
pub const FD_CONTROLLER_TYPE_ISO_CAN: i32 = 1;
/// CAN FD controller type: non-ISO CAN FD.
pub const FD_CONTROLLER_TYPE_NON_ISO_CAN: i32 = 2;
/// CAN FD controller mode: normal (default).
pub const FD_CONTROLLER_MODE_NORMAL: i32 = 0;
/// CAN FD controller mode: acknowledge off.
pub const FD_CONTROLLER_MODE_ACK_OFF: i32 = 1;
/// CAN FD controller mode: restricted.
pub const FD_CONTROLLER_MODE_RESTRICTED: i32 = 2;

/// Process-wide reference count for `initialize_lib_tscan`: the first
/// instance performs the one-time library initialization; the library is
/// never finalized.
static TSCAN_REFCOUNT: AtomicUsize = AtomicUsize::new(0);

/// Symbolic names of the libTSCAN status codes, used in `last_error` texts.
fn tscan_status_name(code: u32) -> &'static str {
    match code {
        0 => "IDX_ERR_OK",
        1 => "IDX_ERR_IDX_OUT_OF_RANGE",
        2 => "IDX_ERR_CONNECT_FAILED",
        3 => "IDX_ERR_DEV_NOT_FOUND",
        4 => "IDX_ERR_CODE_NOT_VALID",
        5 => "IDX_ERR_ALREADY_CONNECTED",
        6 => "IDX_ERR_SEND_FAILED",
        _ => "IDX_ERR_???",
    }
}

// ---- TLIBCAN / TLIBCANFD wire structs ----
// The driver structs use packed (Pack = 1) layout. Their first four u8 fields
// fill bytes 0..4 exactly and every following i32/u64/array field is naturally
// aligned, so the packed layout is identical to plain `#[repr(C)]` (24 / 80
// bytes; see the compile-time assertions below).

/// Classic CAN frame as exchanged with the driver (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TlibCan {
    /// Channel index (0-based).
    idx_chn: u8,
    /// `PROP_IS_*` bitmask.
    properties: u8,
    /// Data length (for classic CAN this is the plain payload length 0..8).
    dlc: u8,
    /// Reserved.
    reserved: u8,
    /// Raw CAN ID, without an extended-frame flag.
    identifier: i32,
    /// Timestamp in microseconds (0 when transmitting).
    time_us: u64,
    /// Payload bytes.
    data: [u8; CAN_DATA_SIZE],
}

/// CAN FD frame as exchanged with the driver (80 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct TlibCanFd {
    /// Channel index.
    idx_chn: u8,
    /// `PROP_IS_*` bitmask.
    properties: u8,
    /// DLC; for FD this is a DLC code, convert it with [`fd_dlc_to_length`].
    dlc: u8,
    /// `FDPROP_*` bitmask.
    fd_properties: u8,
    /// Raw CAN ID.
    identifier: i32,
    /// Timestamp in microseconds.
    time_us: u64,
    /// Payload bytes.
    data: [u8; CAN_FD_DATA_SIZE],
}

impl Default for TlibCanFd {
    fn default() -> Self {
        Self {
            idx_chn: 0,
            properties: 0,
            dlc: 0,
            fd_properties: 0,
            identifier: 0,
            time_us: 0,
            data: [0; CAN_FD_DATA_SIZE],
        }
    }
}

// Layout self-check (compile time; the packed driver structs are 24 / 80 bytes).
const _: () = assert!(std::mem::size_of::<TlibCan>() == 24);
const _: () = assert!(std::mem::size_of::<TlibCanFd>() == 80);

// ---- Pure helper functions ----

/// Validates the channel number (0..=31); out-of-range is a configuration error.
fn to_channel(channel: i32) -> Result<u8> {
    if !(0..=31).contains(&channel) {
        return Err(config_err("TOSUN", "CAN channel index must be 0..31."));
    }
    Ok(channel as u8)
}

/// Converts a baudrate from bit/s to kbit/s; an unset baudrate is a
/// configuration error.
fn to_kbps(baudrate: CanBaudrate) -> Result<f64> {
    let kbps = f64::from(baudrate.as_u32()) / 1000.0;
    if kbps <= 0.0 {
        return Err(config_err(
            "TOSUN",
            "Arbitration/nominal baudrate is not set.",
        ));
    }
    Ok(kbps)
}

/// Returns the FD data-phase bitrate in kbit/s. Custom FD bit timing is not
/// supported by `tscan_config_canfd_by_baudrate` and is rejected as a
/// configuration error.
fn fd_data_kbps(config: &CanConfiguration) -> Result<f64> {
    if config.baudrate_fd == CanFdBaudrate::NotUsed {
        return Err(config_err(
            "TOSUN",
            "CAN FD requires CANConfiguration.BaudrateFD; custom FDBitRateConfig \
             is not supported by tscan_config_canfd_by_baudrate.",
        ));
    }
    Ok(f64::from(config.baudrate_fd.as_u32()) / 1000.0)
}

/// Default bus id used by `open` when the configuration provides none:
/// `"TOSUN:" + channel` (note: not the `Xxx/CANn` form of `format_bus_id`).
fn default_bus_id(channel: u8) -> String {
    format!("TOSUN:{channel}")
}

/// Splits a CAN ID into (native id, raw id, extended flag). After masking the
/// id is always <= 0x1FFFFFFF, so the result is always valid and can be
/// returned as a plain tuple (an "Invalid CAN ID" outcome is unreachable).
fn normalize_can_id(can_id: u32) -> (i32, u32, bool) {
    let num = can_id & 0x1FFF_FFFF;
    let extended = can_id & CAN_EXT_FLAG != 0 || num > 0x7FF;
    let raw_id = if extended { num | CAN_EXT_FLAG } else { num };
    (num as i32, raw_id, extended)
}

/// Maps a payload length to its FD DLC code; lengths > 48 always yield 15
/// (payloads > 64 bytes are rejected earlier by `send`).
fn length_to_fd_dlc(length: usize) -> u8 {
    if length <= 8 {
        length as u8
    } else if length <= 12 {
        9
    } else if length <= 16 {
        10
    } else if length <= 20 {
        11
    } else if length <= 24 {
        12
    } else if length <= 32 {
        13
    } else if length <= 48 {
        14
    } else {
        15
    }
}

/// Maps an FD DLC code to a payload length; codes > 15 yield 0.
fn fd_dlc_to_length(dlc: u8) -> usize {
    match dlc {
        0..=8 => dlc as usize,
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        15 => 64,
        _ => 0,
    }
}

/// Builds a classic TX frame: TX flag set, remote flag clear, zero-padded
/// 8-byte payload.
fn build_tx_can(channel: u8, native_id: i32, extended: bool, data: &[u8]) -> TlibCan {
    let mut buf = [0u8; CAN_DATA_SIZE];
    buf[..data.len()].copy_from_slice(data);
    TlibCan {
        idx_chn: channel,
        properties: PROP_IS_TX | if extended { PROP_IS_EXT } else { 0 },
        dlc: data.len() as u8,
        reserved: 0,
        identifier: native_id,
        time_us: 0,
        data: buf,
    }
}

/// Builds a CAN FD TX frame: TX/EDL flags set, BRS on request, DLC-encoded
/// length, zero-padded 64-byte payload.
fn build_tx_canfd(
    channel: u8,
    native_id: i32,
    extended: bool,
    brs: bool,
    data: &[u8],
) -> TlibCanFd {
    let mut buf = [0u8; CAN_FD_DATA_SIZE];
    buf[..data.len()].copy_from_slice(data);
    TlibCanFd {
        idx_chn: channel,
        properties: PROP_IS_TX | if extended { PROP_IS_EXT } else { 0 },
        dlc: length_to_fd_dlc(data.len()),
        fd_properties: FDPROP_EDL | if brs { FDPROP_BRS } else { 0 },
        identifier: native_id,
        time_us: 0,
        data: buf,
    }
}

/// Converts a received `TLIBCAN` to a `CanFrame` (classic CAN, not a master frame).
fn convert_classic(bus_id: &str, msg: &TlibCan) -> CanFrame {
    // Payload length is clamped: min(DLC, 8).
    let len = (msg.dlc as usize).min(CAN_DATA_SIZE);
    let mut id = (msg.identifier as u32) & 0x1FFF_FFFF;
    if msg.properties & PROP_IS_EXT != 0 {
        id |= CAN_EXT_FLAG;
    }
    CanFrame::new(
        bus_id,
        id,
        msg.data[..len].to_vec(),
        false,
        FrameType::CAN20B,
    )
}

/// Converts a received `TLIBCANFD` to a `CanFrame` (with FD/BRS mapping).
fn convert_fd(bus_id: &str, msg: &TlibCanFd) -> CanFrame {
    // Payload length: FdDlcToLength(DLC), clamped to the 64-byte buffer.
    let len = fd_dlc_to_length(msg.dlc).min(CAN_FD_DATA_SIZE);
    let mut id = (msg.identifier as u32) & 0x1FFF_FFFF;
    if msg.properties & PROP_IS_EXT != 0 {
        id |= CAN_EXT_FLAG;
    }
    let mut frame_type = FrameType::CAN20B;
    if msg.fd_properties & FDPROP_EDL != 0 {
        frame_type = frame_type | FrameType::FD;
        if msg.fd_properties & FDPROP_BRS != 0 {
            frame_type = frame_type | FrameType::BRS;
        }
    }
    CanFrame::new(bus_id, id, msg.data[..len].to_vec(), false, frame_type)
}

// ---- Receive callback slot pool ----
//
// The driver callbacks have a fixed signature with no user pointer
// (`void __stdcall(ref TLIBCAN)`), so a single Rust fn address cannot
// distinguish adapter instances. Instead, 16 pairs of slot callbacks are
// pre-generated; `open` acquires a slot and registers that slot's callbacks,
// `close` unregisters and releases it. When all slots are taken, `open`
// reports `Error::Driver`.

/// Slot contents: the bus id is needed to build frames, and the queue buffers
/// received frames until `receive` dequeues them.
struct RxSlot {
    bus_id: String,
    queue: VecDeque<CanFrame>,
}

/// Slot number -> shared slot (both the callback side and the instance hold
/// an `Arc`).
fn rx_registry() -> &'static Mutex<HashMap<usize, Arc<Mutex<RxSlot>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<Mutex<RxSlot>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Locks the registry, tolerating lock poisoning (same policy as
/// [`DeviceCore::dispatch`]).
fn lock_registry() -> MutexGuard<'static, HashMap<usize, Arc<Mutex<RxSlot>>>> {
    rx_registry().lock().unwrap_or_else(|p| p.into_inner())
}

/// Acquires the lowest free slot; returns `None` when the pool is exhausted.
fn acquire_slot(bus_id: &str) -> Option<(usize, Arc<Mutex<RxSlot>>)> {
    let mut reg = lock_registry();
    for idx in 0..RX_SLOT_COUNT {
        if let Entry::Vacant(e) = reg.entry(idx) {
            let slot = Arc::new(Mutex::new(RxSlot {
                bus_id: bus_id.to_string(),
                queue: VecDeque::new(),
            }));
            e.insert(Arc::clone(&slot));
            return Some((idx, slot));
        }
    }
    None
}

/// Releases a slot (call after unregistering the callbacks; callbacks still
/// in flight afterwards find no slot and their frames are dropped).
fn release_slot(idx: usize) {
    lock_registry().remove(&idx);
}

/// Shared body of the classic-CAN receive callbacks.
fn dispatch_classic(slot: usize, data: *const TlibCan) {
    // A panic must never cross the FFI boundary, so the whole body is wrapped
    // in catch_unwind; a panicking callback is isolated and its frame dropped.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if data.is_null() {
            return;
        }
        // SAFETY: the driver passes a valid `TLIBCAN` pointer per the
        // `TCANQueueEvent_Win32` callback signature; this only reads from it.
        let msg = unsafe { &*data };
        // Only bus-received frames are enqueued; transmit echoes are dropped.
        if msg.properties & PROP_IS_TX != 0 {
            return;
        }
        let entry = lock_registry().get(&slot).cloned();
        if let Some(entry) = entry {
            let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            let frame = convert_classic(&guard.bus_id, msg);
            guard.queue.push_back(frame);
        }
    }));
}

/// Shared body of the CAN FD receive callbacks.
fn dispatch_fd(slot: usize, data: *const TlibCanFd) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if data.is_null() {
            return;
        }
        // SAFETY: same as above; the signature corresponds to
        // `TCANFDQueueEvent_Win32` (`ref TLIBCANFD`).
        let msg = unsafe { &*data };
        if msg.properties & PROP_IS_TX != 0 {
            return;
        }
        let entry = lock_registry().get(&slot).cloned();
        if let Some(entry) = entry {
            let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            let frame = convert_fd(&guard.bus_id, msg);
            guard.queue.push_back(frame);
        }
    }));
}

macro_rules! define_rx_callbacks {
    ($($idx:literal: $can:ident, $canfd:ident;)*) => {$(
        // Classic CAN callback for slot $idx (stdcall, matching the driver
        // callback convention).
        unsafe extern "system" fn $can(data: *mut TlibCan) {
            dispatch_classic($idx, data);
        }
        // CAN FD callback for slot $idx.
        unsafe extern "system" fn $canfd(data: *mut TlibCanFd) {
            dispatch_fd($idx, data);
        }
    )*};
}

define_rx_callbacks! {
    0: rx_can_0, rx_canfd_0;
    1: rx_can_1, rx_canfd_1;
    2: rx_can_2, rx_canfd_2;
    3: rx_can_3, rx_canfd_3;
    4: rx_can_4, rx_canfd_4;
    5: rx_can_5, rx_canfd_5;
    6: rx_can_6, rx_canfd_6;
    7: rx_can_7, rx_canfd_7;
    8: rx_can_8, rx_canfd_8;
    9: rx_can_9, rx_canfd_9;
    10: rx_can_10, rx_canfd_10;
    11: rx_can_11, rx_canfd_11;
    12: rx_can_12, rx_canfd_12;
    13: rx_can_13, rx_canfd_13;
    14: rx_can_14, rx_canfd_14;
    15: rx_can_15, rx_canfd_15;
}

/// Slot number -> classic CAN callback pointer (registration and
/// unregistration must pass the same pointer).
static RX_CAN_CALLBACKS: [unsafe extern "system" fn(*mut TlibCan); RX_SLOT_COUNT] = [
    rx_can_0, rx_can_1, rx_can_2, rx_can_3, rx_can_4, rx_can_5, rx_can_6, rx_can_7, rx_can_8,
    rx_can_9, rx_can_10, rx_can_11, rx_can_12, rx_can_13, rx_can_14, rx_can_15,
];

/// Slot number -> CAN FD callback pointer.
static RX_CANFD_CALLBACKS: [unsafe extern "system" fn(*mut TlibCanFd); RX_SLOT_COUNT] = [
    rx_canfd_0,
    rx_canfd_1,
    rx_canfd_2,
    rx_canfd_3,
    rx_canfd_4,
    rx_canfd_5,
    rx_canfd_6,
    rx_canfd_7,
    rx_canfd_8,
    rx_canfd_9,
    rx_canfd_10,
    rx_canfd_11,
    rx_canfd_12,
    rx_canfd_13,
    rx_canfd_14,
    rx_canfd_15,
];

// ---- libTSCAN function pointer table ----

/// libTSCAN function pointer table (loaded and fully validated at construction).
/// 13 symbols are imported. Signature notes: `bool` parameters follow the
/// 4-byte Win32 `BOOL` convention and are typed `u32`; opaque handles are
/// `usize`; ANSI strings are `*const c_char` (NULL allowed — a null device
/// serial connects to the first device).
#[derive(Debug)]
struct TscanApi {
    /// Keeps the library handle alive (the `DllWrapper` from autors-native);
    /// the field is never accessed directly.
    _dll: DllWrapper,
    /// void initialize_lib_tscan(bool AEnableFIFO, bool AEnableErrorFrame, bool AEnableTurbe)
    initialize_lib_tscan: unsafe extern "system" fn(u32, u32, u32),
    /// uint tscan_scan_devices(ref uint ADeviceCount)
    tscan_scan_devices: unsafe extern "system" fn(*mut u32) -> u32,
    /// uint tscan_get_device_info(uint ADeviceIndex, ref IntPtr AFManufacturer,
    /// ref IntPtr AFProduct, ref IntPtr AFSerial); the written pointers
    /// reference driver-owned static ANSI strings.
    tscan_get_device_info: unsafe extern "system" fn(
        u32,
        *mut *const c_char,
        *mut *const c_char,
        *mut *const c_char,
    ) -> u32,
    /// uint tscan_connect(string ADeviceSerial, ref IntPtr ADeviceHandle)
    tscan_connect: unsafe extern "system" fn(*const c_char, *mut usize) -> u32,
    /// uint tscan_disconnect_by_handle(IntPtr ADeviceHandle)
    tscan_disconnect_by_handle: unsafe extern "system" fn(usize) -> u32,
    /// uint tscan_register_event_can(IntPtr, TCANQueueEvent_Win32)
    tscan_register_event_can:
        unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCan)) -> u32,
    /// uint tscan_unregister_event_can(IntPtr, TCANQueueEvent_Win32)
    tscan_unregister_event_can:
        unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCan)) -> u32,
    /// uint tscan_register_event_canfd(IntPtr, TCANFDQueueEvent_Win32)
    tscan_register_event_canfd:
        unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCanFd)) -> u32,
    /// uint tscan_unregister_event_canfd(IntPtr, TCANFDQueueEvent_Win32)
    tscan_unregister_event_canfd:
        unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCanFd)) -> u32,
    /// uint tscan_transmit_can_async(IntPtr, ref TLIBCAN)
    tscan_transmit_can_async: unsafe extern "system" fn(usize, *mut TlibCan) -> u32,
    /// uint tscan_transmit_canfd_async(IntPtr, ref TLIBCANFD)
    tscan_transmit_canfd_async: unsafe extern "system" fn(usize, *mut TlibCanFd) -> u32,
    /// uint tscan_config_can_by_baudrate(IntPtr, CHANNEL_INDEX, double, uint)
    tscan_config_can_by_baudrate: unsafe extern "system" fn(usize, u8, f64, u32) -> u32,
    /// uint tscan_config_canfd_by_baudrate(IntPtr, CHANNEL_INDEX, double, double,
    /// TLIBCANFDControllerType, TLIBCANFDControllerMode, uint)
    tscan_config_canfd_by_baudrate:
        unsafe extern "system" fn(usize, u8, f64, f64, i32, i32, u32) -> u32,
}

impl TscanApi {
    /// Loads `libTSCAN.dll` via the system search path.
    fn load() -> Result<Self> {
        Self::load_from(LIBTSCAN_DLL)
    }

    /// Loads the DLL from the given path/name and resolves every export.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of running DllMain) is
        // encapsulated in autors-native's `DllWrapper`.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (each `get` in the macro expansion): symbol addresses are
        // only taken here and copied into plain function pointers; the library
        // handle and the function pointers live in the same struct, keeping
        // the pointers valid. The generic T is always a (Copy) function
        // pointer type.
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
        let api = Self {
            initialize_lib_tscan: sym!(
                b"initialize_lib_tscan\0",
                unsafe extern "system" fn(u32, u32, u32)
            ),
            tscan_scan_devices: sym!(
                b"tscan_scan_devices\0",
                unsafe extern "system" fn(*mut u32) -> u32
            ),
            tscan_get_device_info: sym!(
                b"tscan_get_device_info\0",
                unsafe extern "system" fn(
                    u32,
                    *mut *const c_char,
                    *mut *const c_char,
                    *mut *const c_char,
                ) -> u32
            ),
            tscan_connect: sym!(
                b"tscan_connect\0",
                unsafe extern "system" fn(*const c_char, *mut usize) -> u32
            ),
            tscan_disconnect_by_handle: sym!(
                b"tscan_disconnect_by_handle\0",
                unsafe extern "system" fn(usize) -> u32
            ),
            tscan_register_event_can: sym!(
                b"tscan_register_event_can\0",
                unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCan)) -> u32
            ),
            tscan_unregister_event_can: sym!(
                b"tscan_unregister_event_can\0",
                unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCan)) -> u32
            ),
            tscan_register_event_canfd: sym!(
                b"tscan_register_event_canfd\0",
                unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCanFd)) -> u32
            ),
            tscan_unregister_event_canfd: sym!(
                b"tscan_unregister_event_canfd\0",
                unsafe extern "system" fn(usize, unsafe extern "system" fn(*mut TlibCanFd)) -> u32
            ),
            tscan_transmit_can_async: sym!(
                b"tscan_transmit_can_async\0",
                unsafe extern "system" fn(usize, *mut TlibCan) -> u32
            ),
            tscan_transmit_canfd_async: sym!(
                b"tscan_transmit_canfd_async\0",
                unsafe extern "system" fn(usize, *mut TlibCanFd) -> u32
            ),
            tscan_config_can_by_baudrate: sym!(
                b"tscan_config_can_by_baudrate\0",
                unsafe extern "system" fn(usize, u8, f64, u32) -> u32
            ),
            tscan_config_canfd_by_baudrate: sym!(
                b"tscan_config_canfd_by_baudrate\0",
                unsafe extern "system" fn(usize, u8, f64, f64, i32, i32, u32) -> u32
            ),
            _dll: dll,
        };
        // One-time library initialization, performed by the first instance in
        // the process: initialize_lib_tscan(AEnableFIFO: true,
        // AEnableErrorFrame: true, AEnableTurbe: false); never repeated.
        if TSCAN_REFCOUNT.fetch_add(1, Ordering::SeqCst) == 0 {
            // SAFETY: the function pointer comes from the loaded libTSCAN.dll;
            // the three bools are passed as 4-byte Win32 BOOLs with values 1/1/0.
            unsafe { (api.initialize_lib_tscan)(1, 1, 0) };
        }
        Ok(api)
    }
}

impl Drop for TscanApi {
    fn drop(&mut self) {
        // The library is never finalized; only the reference count is
        // decremented.
        TSCAN_REFCOUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---- Device enumeration ----

/// Counterpart of `PtrToStringAnsi`: NULL maps to an empty string.
unsafe fn ansi_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees `p` points to a NUL-terminated ANSI string
    // in driver static storage (success path of `tscan_get_device_info`); the
    // string is copied out immediately.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Parses the CAN channel count from a product name: a hand-rolled
/// implementation of the regex `CAN(?:FD)?\s*(\d+)` (IgnoreCase). At the
/// first match position the `FD` path is tried first (matching regex
/// backtracking order); the captured number is accepted if within `1..=32`,
/// otherwise the result falls back to 1; integer parse overflow also falls
/// back to 1. No regex dependency.
fn parse_can_channel_count(product: &str) -> i32 {
    let lower = product.to_lowercase();
    let b = lower.as_bytes();
    let is_regex_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C);
    let mut i = 0;
    while i + 3 <= b.len() {
        if &b[i..i + 3] == b"can" {
            let after = i + 3;
            // Try order for `(?:FD)?`: with FD first, then backtrack without
            // (the repeated attempt is idempotent).
            let starts: [usize; 2] = if b[after..].starts_with(b"fd") {
                [after + 2, after]
            } else {
                [after, after]
            };
            for start in starts {
                let mut j = start;
                while j < b.len() && is_regex_space(b[j]) {
                    j += 1;
                }
                let digits_start = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digits_start {
                    return match lower[digits_start..j].parse::<i32>() {
                        Ok(v) if (1..=32).contains(&v) => v,
                        // Out of range or overflow (failed integer parse) -> 1.
                        _ => 1,
                    };
                }
            }
        }
        i += 1;
    }
    1
}

/// One enumerated TOSUN device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TosunDeviceEntry {
    /// Device index (0-based).
    pub device_index: i32,
    /// Manufacturer string (empty when it could not be retrieved).
    pub manufacturer: String,
    /// Product name string.
    pub product: String,
    /// Serial number string.
    pub serial: String,
}

impl TosunDeviceEntry {
    /// Parses the CAN channel count from the product name (e.g. "TC1014
    /// CANFD 4" -> 4).
    pub fn can_channel_count(&self) -> i32 {
        parse_can_channel_count(&self.product)
    }

    /// Display name: `"{Product} ({Serial})"`, falling back to `"TOSUN CAN"`
    /// when the product name is empty.
    pub fn display_name(&self) -> String {
        let mut name = if self.product.trim().is_empty() {
            "TOSUN CAN".to_string()
        } else {
            self.product.clone()
        };
        if !self.serial.trim().is_empty() {
            name = format!("{name} ({})", self.serial);
        }
        name
    }
}

// ---- TosunCan ----

/// TOSUN (TSMaster) CAN channel adapter.
/// Receive model: driver callbacks push bus frames into a per-instance queue,
/// and [`CanDevice::receive`] dequeues them non-blockingly (see the module
/// docs for details).
pub struct TosunCan {
    core: DeviceCore,
    api: TscanApi,
    /// Device handle (0 = not connected).
    device_handle: usize,
    /// Channel number (0-based CHANNEL_INDEX).
    channel: u8,
    /// Whether the channel was opened with an FD configuration.
    fd_enabled: bool,
    /// Whether the channel is currently open.
    is_open: bool,
    /// Bus id (`"TOSUN:{channel}"` generated at `open`, or the value from the
    /// configuration).
    bus_id: String,
    /// Reason for the last failed `open`/`send`; cleared on success paths.
    last_error: Option<String>,
    /// Acquired receive-callback slot.
    rx_slot: Option<(usize, Arc<Mutex<RxSlot>>)>,
    /// Whether the classic callback is registered.
    registered_can: bool,
    /// Whether the FD callback is registered.
    registered_canfd: bool,
    /// Device serial to connect to (`None` = connect to the first device);
    /// set before `open`.
    pub device_serial: Option<String>,
    /// Whether to enable the internal termination resistor (default true).
    pub terminal_resistor: bool,
    /// FD controller type (default [`FD_CONTROLLER_TYPE_ISO_CAN`]).
    pub fd_controller_type: i32,
    /// FD controller mode (default [`FD_CONTROLLER_MODE_NORMAL`]).
    pub fd_controller_mode: i32,
}

impl TosunCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        // Tears down the native side only.
        self.close_native_no_throw();
    }

    /// Loads libTSCAN.dll and validates the symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: TscanApi::load()?,
            device_handle: 0,
            channel: 0,
            fd_enabled: false,
            is_open: false,
            bus_id: String::new(),
            last_error: None,
            rx_slot: None,
            registered_can: false,
            registered_canfd: false,
            device_serial: None,
            terminal_resistor: true,
            fd_controller_type: FD_CONTROLLER_TYPE_ISO_CAN,
            fd_controller_mode: FD_CONTROLLER_MODE_NORMAL,
        })
    }

    /// Whether the channel is currently open.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Human-readable reason of the last failed `open`/`send`, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Scans for connected devices. Scan or per-device info failures yield
    /// (partially) empty results instead of an error.
    pub fn scan_devices(&self) -> Vec<TosunDeviceEntry> {
        let mut entries = Vec::new();
        let mut count: u32 = 0;
        // SAFETY: `count` is an out parameter on this stack frame.
        let code = unsafe { (self.api.tscan_scan_devices)(&mut count) };
        if code != TSCAN_OK {
            return entries;
        }
        for idx in 0..count {
            let mut manufacturer: *const c_char = ptr::null();
            let mut product: *const c_char = ptr::null();
            let mut serial: *const c_char = ptr::null();
            // SAFETY: the three out pointers are stack locals; on success the
            // driver writes pointers to its static ANSI strings, which
            // `ansi_to_string` copies immediately; on failure they stay NULL
            // and map to empty strings (the entry is still added).
            let code = unsafe {
                (self.api.tscan_get_device_info)(idx, &mut manufacturer, &mut product, &mut serial)
            };
            let (manufacturer, product, serial) = if code == TSCAN_OK {
                // SAFETY: see above; the pointers come from a successful driver call.
                unsafe {
                    (
                        ansi_to_string(manufacturer),
                        ansi_to_string(product),
                        ansi_to_string(serial),
                    )
                }
            } else {
                (String::new(), String::new(), String::new())
            };
            entries.push(TosunDeviceEntry {
                device_index: idx as i32,
                manufacturer,
                product,
                serial,
            });
        }
        entries
    }

    /// Native part of `open`: connect -> configure baudrates -> register
    /// receive callbacks. Recoverable failures record `last_error` and return
    /// `Ok(false)`.
    fn open_native(
        &mut self,
        channel: u8,
        fd: bool,
        arb_kbps: f64,
        data_kbps: f64,
        slot_idx: usize,
    ) -> Result<bool> {
        let serial = match &self.device_serial {
            Some(s) => Some(
                CString::new(s.as_str())
                    .map_err(|e| Error::Invalid(format!("device serial contains NUL: {e}")))?,
            ),
            None => None,
        };
        let serial_ptr = serial.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let mut handle: usize = 0;
        // SAFETY: `serial_ptr` is NULL (connect to the first device) or points
        // to a NUL-terminated `CString` valid for the duration of the call;
        // `handle` is an out parameter on this stack frame.
        let code = unsafe { (self.api.tscan_connect)(serial_ptr, &mut handle) };
        if code != TSCAN_OK && code != IDX_ERR_ALREADY_CONNECTED {
            // Intentional: on failure the handle written by the driver is not
            // disconnected (its validity is not guaranteed); the handle is
            // only stored after success.
            self.last_error = Some(format!(
                "tscan_connect failed: {code} ({}).",
                tscan_status_name(code)
            ));
            self.close_native_no_throw();
            return Ok(false);
        }
        self.device_handle = handle;
        let ohm = u32::from(self.terminal_resistor);
        // SAFETY: `handle` was returned by `tscan_connect`; all arguments are
        // by value.
        let code = if fd {
            unsafe {
                (self.api.tscan_config_canfd_by_baudrate)(
                    handle,
                    channel,
                    arb_kbps,
                    data_kbps,
                    self.fd_controller_type,
                    self.fd_controller_mode,
                    ohm,
                )
            }
        } else {
            unsafe { (self.api.tscan_config_can_by_baudrate)(handle, channel, arb_kbps, ohm) }
        };
        if code != TSCAN_OK {
            let what = if fd {
                "tscan_config_canfd_by_baudrate"
            } else {
                "tscan_config_can_by_baudrate"
            };
            self.last_error = Some(format!(
                "{what} failed: {code} ({}).",
                tscan_status_name(code)
            ));
            self.close_native_no_throw();
            return Ok(false);
        }
        // SAFETY: `handle` is valid; the callback pointer comes from the
        // static slot table, and unregistration passes the same pointer.
        let code = if fd {
            unsafe { (self.api.tscan_register_event_canfd)(handle, RX_CANFD_CALLBACKS[slot_idx]) }
        } else {
            unsafe { (self.api.tscan_register_event_can)(handle, RX_CAN_CALLBACKS[slot_idx]) }
        };
        if code != TSCAN_OK {
            self.last_error = Some(format!(
                "tscan_register_event_can{} failed.",
                if fd { "fd" } else { "" }
            ));
            self.close_native_no_throw();
            return Ok(false);
        }
        if fd {
            self.registered_canfd = true;
        } else {
            self.registered_can = true;
        }
        self.is_open = true;
        Ok(true)
    }

    /// Tears down the native side: unregister callbacks -> disconnect; all
    /// errors are ignored.
    fn close_native_no_throw(&mut self) {
        if self.device_handle != 0 {
            if let Some((slot_idx, _)) = &self.rx_slot {
                let slot_idx = *slot_idx;
                // SAFETY: same callback pointer as at registration; the return
                // value is intentionally ignored.
                if self.registered_can {
                    unsafe {
                        (self.api.tscan_unregister_event_can)(
                            self.device_handle,
                            RX_CAN_CALLBACKS[slot_idx],
                        );
                    }
                }
                if self.registered_canfd {
                    unsafe {
                        (self.api.tscan_unregister_event_canfd)(
                            self.device_handle,
                            RX_CANFD_CALLBACKS[slot_idx],
                        );
                    }
                }
            }
            // SAFETY: `handle` is valid; the return value is intentionally ignored.
            unsafe {
                (self.api.tscan_disconnect_by_handle)(self.device_handle);
            }
        }
        self.registered_can = false;
        self.registered_canfd = false;
        self.device_handle = 0;
        self.is_open = false;
        // The slot is released only after unregistration: callbacks already in
        // flight before unregistration still find the slot (pushing into an
        // orphaned queue, which is harmless); callbacks arriving after
        // unregistration find no slot and are dropped.
        if let Some((slot_idx, _)) = self.rx_slot.take() {
            release_slot(slot_idx);
        }
    }
}

#[async_trait]
impl CanDevice for TosunCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Availability check: the library was loaded at construction and
        // initialized by the first instance, so this only probes the device
        // count: tscan_scan_devices must succeed and report at least one
        // device. All failures map to `Ok(false)`.
        let mut count: u32 = 0;
        // SAFETY: `count` is an out parameter on this stack frame.
        let code = unsafe { (self.api.tscan_scan_devices)(&mut count) };
        Ok(code == TSCAN_OK && count != 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        // Open sequence: compute baudrates first (a configuration error
        // aborts), then close any previous session, then validate the channel
        // number, then connect/configure/register (recoverable failures
        // return `Ok(false)`).
        let fd = config.is_fd();
        let arb_kbps = to_kbps(config.baudrate)?;
        let data_kbps = if fd { fd_data_kbps(&config)? } else { 0.0 };
        // A previous session is always closed first.
        self.close_sync();
        self.last_error = None;
        let channel = to_channel(config.channel)?;
        self.channel = channel;
        self.fd_enabled = fd;
        // Generate "TOSUN:{channel}" when the configuration provides no bus id.
        self.bus_id = config
            .bus_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_bus_id(channel));
        // Acquire a callback slot (with a fresh empty queue).
        let (slot_idx, slot) = acquire_slot(&self.bus_id).ok_or_else(|| {
            config_err(
                &self.bus_id,
                format!("libTSCAN: all {RX_SLOT_COUNT} receive callback slots are in use"),
            )
        })?;
        self.rx_slot = Some((slot_idx, slot));
        // Any remaining session bookkeeping is covered by the shared device
        // core and the upper-level dispatch loop.
        self.open_native(channel, fd, arb_kbps, data_kbps, slot_idx)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // The send path is serialized by `&mut self`.
        let (native_id, raw_id, extended) = normalize_can_id(can_id);
        let is_fd = frame_type.contains(FrameType::FD);
        if !self.is_open {
            self.last_error = Some("TosunCAN is not open.".to_string());
            return Ok(0);
        }
        let code = if is_fd {
            if data.len() > CAN_FD_DATA_SIZE {
                self.last_error = Some("CAN FD payload length must be 0..64 bytes.".to_string());
                return Ok(0);
            }
            let mut msg = build_tx_canfd(
                self.channel,
                native_id,
                extended,
                frame_type.contains(FrameType::BRS),
                data,
            );
            // SAFETY: `handle` is valid; `msg` stays valid for the duration of
            // the call and the driver copies it synchronously into its queue
            // without retaining the pointer.
            unsafe { (self.api.tscan_transmit_canfd_async)(self.device_handle, &mut msg) }
        } else {
            if data.len() > CAN_DATA_SIZE {
                self.last_error =
                    Some("Classic CAN payload length must be 0..8 bytes.".to_string());
                return Ok(0);
            }
            let mut msg = build_tx_can(self.channel, native_id, extended, data);
            // SAFETY: same as above.
            unsafe { (self.api.tscan_transmit_can_async)(self.device_handle, &mut msg) }
        };
        if code != TSCAN_OK {
            self.last_error = Some(format!(
                "transmit failed: {code} ({}).",
                tscan_status_name(code)
            ));
            return Ok(0);
        }
        // Statistics: classic frames are recorded as CAN20B, FD frames keep
        // the caller's frame type; returns the recorded payload length.
        let frame = CanFrame::new(
            &self.bus_id,
            raw_id,
            data.to_vec(),
            true,
            if is_fd { frame_type } else { FrameType::CAN20B },
        );
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        // Receive is non-blocking per the trait contract: when not open there
        // is simply no frame; otherwise the next queued frame is dequeued
        // directly (pacing is left to the upper-level polling loop).
        if !self.is_open {
            return Ok(None);
        }
        let Some((_, slot)) = &self.rx_slot else {
            return Ok(None);
        };
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        Ok(guard.queue.pop_front())
    }
}

impl Drop for TosunCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}
