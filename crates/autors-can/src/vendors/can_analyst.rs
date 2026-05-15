//! CANalyst-II (Chuangxin Technology USB-CAN analyzer) adapter built on the
//! `ControlCAN.dll` driver.
//! `ControlCAN.dll` is a ZLG-compatible USBCAN C API (the `VCI_*` function
//! family); device type 3 = USBCAN-I, 4 = USBCAN-II (CANalyst-II uses 4).
//! All exports use the stdcall calling convention, so they are bound uniformly
//! as `extern "system"`. If the DLL is not installed or an export is missing,
//! construction returns [`Error::Driver`].
//! The `ControlCAN.dll` installed by the CANalyst-II companion tool (typically
//! under `C:\Program Files (x86)\USB_CAN TOOL\`) is 32-bit (PE32/i386): an x64
//! process must install the 64-bit `ControlCAN.dll` from the vendor driver
//! package instead, otherwise loading fails with [`Error::Driver`].
//! Classic CAN only: payloads over 8 bytes and CAN FD requests are rejected.

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG, CAN_EXT_ID_MASK,
    CAN_STD_ID_MASK,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// Name of the ControlCAN driver DLL.
const CONTROLCAN_DLL: &str = "ControlCAN.dll";

// ---- ControlCAN constants ----
/// Device type constant for USBCAN-I.
const DEV_USBCAN: u32 = 3;
/// Device type constant for USBCAN-II / CANalyst-II (the default).
const DEV_USBCAN2: u32 = 4;
/// Default wait time (milliseconds) passed to `VCI_Receive`.
const DEFAULT_RECEIVE_WAIT_TIME_MS: i32 = 1;
/// Error return value of `VCI_Receive`.
const VCI_RECEIVE_ERROR: u32 = u32::MAX;

/// Rejects CAN FD configurations (a data-phase baudrate or a full bit-timing
/// configuration) with [`Error::Driver`].
fn ensure_classic_can_only(config: &CanConfiguration) -> Result<()> {
    if config.is_fd() {
        return Err(config_err(
            "CANAnalyst",
            "ControlCAN.dll wrapper supports classic CAN only; CAN FD is not supported.",
        ));
    }
    Ok(())
}

/// Maps a baudrate to the (Timing0, Timing1) register pair.
/// Baudrates outside the table are rejected with [`Error::Driver`] (the
/// hardware ID "CANAnalyst" is included in the message, as with
/// [`config_err`]).
fn baudrate_timing(baudrate: CanBaudrate) -> Result<(u8, u8)> {
    match baudrate {
        CanBaudrate::B10Kbit => Ok((49, 28)),
        CanBaudrate::B20Kbit => Ok((24, 28)),
        CanBaudrate::B50Kbit => Ok((9, 28)),
        CanBaudrate::B100Kbit => Ok((3, 47)),
        CanBaudrate::B125Kbit => Ok((3, 28)),
        CanBaudrate::B250Kbit => Ok((1, 28)),
        CanBaudrate::B500Kbit => Ok((0, 28)),
        CanBaudrate::B800Kbit => Ok((0, 22)),
        CanBaudrate::B1Mbit => Ok((0, 20)),
        other => Err(config_err(
            "CANAnalyst",
            format!("Unsupported ControlCAN baudrate: {other}."),
        )),
    }
}

/// Selects the device type: a `HardwareType` of 3 or 4 is used as-is,
/// anything else defaults to `DEV_USBCAN2`.
fn device_type(hardware_type: i32) -> u32 {
    match hardware_type {
        3 => DEV_USBCAN,
        4 => DEV_USBCAN2,
        _ => DEV_USBCAN2,
    }
}

/// Splits a CAN ID into (native ID, library-level ID, is-extended).
/// `native_id = can_id & 0x1FFFFFFF`; the frame counts as extended when the
/// extended flag is already set or the bare ID exceeds 11 bits. The result is
/// always `Some` (the masked ID is always valid); the `Option` wrapper is kept
/// to mirror a boolean success/failure result shape.
fn normalize_can_id(can_id: u32) -> Option<(u32, u32, bool)> {
    let native_id = can_id & CAN_EXT_ID_MASK;
    let extended = (can_id & CAN_EXT_FLAG != 0) || native_id > CAN_STD_ID_MASK;
    if extended {
        let raw_id = native_id | CAN_EXT_FLAG;
        (native_id <= CAN_EXT_ID_MASK).then_some((native_id, raw_id, true))
    } else {
        (native_id <= CAN_STD_ID_MASK).then_some((native_id, native_id, false))
    }
}

/// ControlCAN frame structure (24 bytes, 4-byte alignment).
/// Field layout: `u32, u32`, then five `u8` fields (offsets 8–12), `data[8]`
/// (offsets 13–20) and `reserved[3]` (offsets 21–23), 24 bytes total.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VciCanObj {
    id: u32,
    time_stamp: u32,
    time_flag: u8,
    send_type: u8,
    remote_flag: u8,
    extern_flag: u8,
    data_len: u8,
    data: [u8; 8],
    reserved: [u8; 3],
}

/// Initialization parameters for `VCI_InitCAN` (16 bytes, 4-byte alignment).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VciInitConfig {
    acc_code: u32,
    acc_mask: u32,
    reserved: u32,
    filter: u8,
    timing0: u8,
    timing1: u8,
    mode: u8,
}

/// Board information structure used by `VCI_FindUsbDevice` for device probing
/// (52 bytes, 2-byte alignment).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VciBoardInfo1 {
    hw_version: u16,
    fw_version: u16,
    dr_version: u16,
    in_version: u16,
    irq_num: u16,
    can_num: u8,
    reserved: u8,
    str_serial_num: [u8; 8],
    str_hw_type: [u8; 16],
    str_usb_serial: [u8; 16],
}

/// Board information structure used by `VCI_ReadBoardInfo` (loaded for
/// symbol-table completeness only; not called by the current flow; 75 bytes
/// padded to 76, 2-byte alignment).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct VciBoardInfo {
    hw_version: u16,
    fw_version: u16,
    dr_version: u16,
    in_version: u16,
    irq_num: u16,
    can_num: u8,
    str_serial_num: [u8; 20],
    str_hw_type: [u8; 40],
    reserved: [u8; 4],
}

/// Builds the TX frame object: `SendType=0`, `RemoteFlag=0`, and
/// `ExternFlag=1` for extended frames. `data` must be at most 8 bytes
/// (validated by the caller).
/// Returns (frame object, library-level ID); the library-level ID feeds the
/// error messages and the sent-frame bookkeeping.
fn build_tx_obj(can_id: u32, data: &[u8]) -> Option<(VciCanObj, u32)> {
    let (native_id, raw_id, extended) = normalize_can_id(can_id)?;
    let mut obj = VciCanObj {
        id: native_id,
        send_type: 0,
        remote_flag: 0,
        extern_flag: u8::from(extended),
        data_len: data.len() as u8,
        ..VciCanObj::default()
    };
    obj.data[..data.len()].copy_from_slice(data);
    Some((obj, raw_id))
}

/// Reconstructs a received frame: `DataLen` is clamped to 8, the ID is masked
/// with `0x1FFFFFFF`, and the extended flag is set when `ExternFlag != 0`.
fn rx_frame_from_obj(bus_id: &str, obj: &VciCanObj) -> CanFrame {
    let len = obj.data_len.min(8) as usize;
    let mut id = obj.id & CAN_EXT_ID_MASK;
    if obj.extern_flag != 0 {
        id |= CAN_EXT_FLAG;
    }
    CanFrame::new(
        bus_id,
        id,
        obj.data[..len].to_vec(),
        false,
        FrameType::CAN20B,
    )
}

/// Table of ControlCAN function pointers.
/// The 8 symbols of the core TX/RX flow are mandatory (validated at
/// construction); probing/auxiliary symbols such as `VCI_FindUsbDevice` are
/// optional — a missing probe export is treated as "available" during
/// availability checks, so optional loading matches that lenient semantics.
#[derive(Debug)]
struct ControlCan {
    /// Keeps the library handle alive (autors-native's [`DllWrapper`]); the
    /// field is never accessed directly.
    _dll: DllWrapper,
    /// uint VCI_OpenDevice(uint deviceType, uint deviceIndex, uint reserved).
    vci_open_device: unsafe extern "system" fn(u32, u32, u32) -> u32,
    /// uint VCI_CloseDevice(uint deviceType, uint deviceIndex).
    vci_close_device: unsafe extern "system" fn(u32, u32) -> u32,
    /// uint VCI_InitCAN(uint deviceType, uint deviceIndex, uint canIndex,
    /// ref VciInitConfig initConfig).
    vci_init_can: unsafe extern "system" fn(u32, u32, u32, *const VciInitConfig) -> u32,
    /// uint VCI_ClearBuffer(uint deviceType, uint deviceIndex, uint canIndex).
    vci_clear_buffer: unsafe extern "system" fn(u32, u32, u32) -> u32,
    /// uint VCI_StartCAN(uint deviceType, uint deviceIndex, uint canIndex).
    vci_start_can: unsafe extern "system" fn(u32, u32, u32) -> u32,
    /// uint VCI_ResetCAN(uint deviceType, uint deviceIndex, uint canIndex).
    vci_reset_can: unsafe extern "system" fn(u32, u32, u32) -> u32,
    /// uint VCI_Transmit(uint deviceType, uint deviceIndex, uint canIndex,
    /// ref VciCanObj sendObject, uint length).
    vci_transmit: unsafe extern "system" fn(u32, u32, u32, *const VciCanObj, u32) -> u32,
    /// uint VCI_Receive(uint deviceType, uint deviceIndex, uint canIndex,
    /// ref VciCanObj receiveObject, uint length, int waitTime).
    vci_receive: unsafe extern "system" fn(u32, u32, u32, *mut VciCanObj, u32, i32) -> u32,
    /// uint VCI_FindUsbDevice(ref VciBoardInfo1 info) (optional, see struct docs).
    vci_find_usb_device: Option<unsafe extern "system" fn(*mut VciBoardInfo1) -> u32>,
    /// uint VCI_ReadBoardInfo(uint deviceType, uint deviceIndex, ref VciBoardInfo).
    #[allow(dead_code)] // loaded for symbol-table completeness; not called by the current flow
    vci_read_board_info: Option<unsafe extern "system" fn(u32, u32, *mut VciBoardInfo) -> u32>,
    /// uint VCI_GetReceiveNum(uint deviceType, uint deviceIndex, uint canIndex).
    #[allow(dead_code)] // same as above
    vci_get_receive_num: Option<unsafe extern "system" fn(u32, u32, u32) -> u32>,
    /// uint VCI_ConnectDevice(uint deviceType, uint deviceIndex).
    #[allow(dead_code)] // same as above
    vci_connect_device: Option<unsafe extern "system" fn(u32, u32) -> u32>,
    /// uint VCI_UsbDeviceReset(uint deviceType, uint deviceIndex, uint reserved).
    #[allow(dead_code)] // same as above
    vci_usb_device_reset: Option<unsafe extern "system" fn(u32, u32, u32) -> u32>,
}

impl ControlCan {
    /// Loads ControlCAN.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(CONTROLCAN_DLL)
    }

    /// Loads the DLL from the given path/name and resolves its exports (the 8
    /// core symbols are mandatory; probing/auxiliary ones are optional).
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe execution of DllMain) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (each `get` in the macro expansions): symbol addresses are only
        // resolved and copied out as raw function pointers here; the library
        // handle and the function pointers live in the same struct, keeping the
        // pointers valid for the struct's lifetime. The generic T is a
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
        // Optional symbols (a missing export only matters if it is ever called).
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: same as above; a missing symbol simply yields None.
                let r = unsafe { dll.library().get::<$ty>($name) };
                r.ok().map(|s| *s)
            }};
        }
        Ok(Self {
            vci_open_device: sym!(
                b"VCI_OpenDevice\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            vci_close_device: sym!(
                b"VCI_CloseDevice\0",
                unsafe extern "system" fn(u32, u32) -> u32
            ),
            vci_init_can: sym!(
                b"VCI_InitCAN\0",
                unsafe extern "system" fn(u32, u32, u32, *const VciInitConfig) -> u32
            ),
            vci_clear_buffer: sym!(
                b"VCI_ClearBuffer\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            vci_start_can: sym!(
                b"VCI_StartCAN\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            vci_reset_can: sym!(
                b"VCI_ResetCAN\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            vci_transmit: sym!(
                b"VCI_Transmit\0",
                unsafe extern "system" fn(u32, u32, u32, *const VciCanObj, u32) -> u32
            ),
            vci_receive: sym!(
                b"VCI_Receive\0",
                unsafe extern "system" fn(u32, u32, u32, *mut VciCanObj, u32, i32) -> u32
            ),
            vci_find_usb_device: opt_sym!(
                b"VCI_FindUsbDevice\0",
                unsafe extern "system" fn(*mut VciBoardInfo1) -> u32
            ),
            vci_read_board_info: opt_sym!(
                b"VCI_ReadBoardInfo\0",
                unsafe extern "system" fn(u32, u32, *mut VciBoardInfo) -> u32
            ),
            vci_get_receive_num: opt_sym!(
                b"VCI_GetReceiveNum\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            vci_connect_device: opt_sym!(
                b"VCI_ConnectDevice\0",
                unsafe extern "system" fn(u32, u32) -> u32
            ),
            vci_usb_device_reset: opt_sym!(
                b"VCI_UsbDeviceReset\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            _dll: dll,
        })
    }
}

/// CANalyst-II CAN channel adapter.
/// Locking (the `syncRoot` role) is left to an upper-level `Mutex`, following
/// the convention shared by all vendor adapters in this crate; background
/// TX/RX threads are not built in — dispatch is driven by
/// [`crate::device::start_dispatch`] or upper-level polling. Classic CAN only.
pub struct CanAnalystCan {
    core: DeviceCore,
    api: ControlCan,
    /// The channel has been started with `VCI_StartCAN`.
    is_open: bool,
    /// `VCI_OpenDevice` has succeeded.
    /// This is tracked separately from `is_open` (which is only set after
    /// `VCI_StartCAN` succeeds) so that a device whose `VCI_OpenDevice`
    /// succeeded is still closed via `VCI_CloseDevice` when `open` fails
    /// partway through; guarding cleanup on `is_open` alone would leak the
    /// device handle in that case.
    device_opened: bool,
    /// Device type (3 = USBCAN-I, 4 = USBCAN-II; default 4).
    device_type: u32,
    /// Device index.
    device_index: u32,
    /// CAN channel index.
    can_index: u32,
    /// Wait time in milliseconds passed to `VCI_Receive` (default 1).
    receive_wait_time_ms: i32,
    /// Set after `VCI_Receive` returns 0xFFFFFFFF.
    err_received: bool,
    /// Text of the most recent error.
    last_error: Option<String>,
    /// Bus ID determined at open time: the user-provided `config.bus_id` wins,
    /// otherwise `"CANAnalyst/CAN{channel+1}"` is generated.
    bus_id: String,
}

impl CanAnalystCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        // Background threads are stopped by the upper layer; this only performs
        // the native close.
        self.close_native_no_throw();
    }

    /// Loads ControlCAN.dll and validates its symbol table; returns
    /// [`Error::Driver`] when the driver is not installed (or has a mismatched
    /// bitness).
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: ControlCan::load()?,
            is_open: false,
            device_opened: false,
            device_type: DEV_USBCAN2,
            device_index: 0,
            can_index: 0,
            receive_wait_time_ms: DEFAULT_RECEIVE_WAIT_TIME_MS,
            err_received: false,
            last_error: None,
            bus_id: String::new(),
        })
    }

    /// Whether the channel is currently open.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Whether `VCI_Receive` has returned 0xFFFFFFFF since the last open.
    pub fn err_received(&self) -> bool {
        self.err_received
    }

    /// Text of the most recent error, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Issues `VCI_ResetCAN` + `VCI_CloseDevice`, ignoring all return values
    /// (errors are swallowed intentionally). Guarded by `device_opened`.
    fn close_native_no_throw(&mut self) {
        if self.device_opened {
            // SAFETY: device_type/device_index/can_index come from a session
            // whose VCI_OpenDevice succeeded; all arguments are passed by
            // value, no pointers.
            unsafe {
                if self.is_open {
                    (self.api.vci_reset_can)(self.device_type, self.device_index, self.can_index);
                }
                (self.api.vci_close_device)(self.device_type, self.device_index);
            }
            self.device_opened = false;
            self.is_open = false;
        }
    }
}

#[async_trait]
impl CanDevice for CanAnalystCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Probe with VCI_FindUsbDevice; when the export is missing the device
        // is treated as "available" (true). DLL load failures already surfaced
        // as Error::Driver during construction, so only device probing happens
        // here.
        match self.api.vci_find_usb_device {
            Some(find) => {
                let mut info = VciBoardInfo1::default();
                // SAFETY: `info` is a correctly sized struct on this stack
                // frame; the function does not retain the pointer.
                Ok(unsafe { find(&mut info) } != 0)
            }
            None => Ok(true),
        }
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        // Classic-CAN-only guard.
        ensure_classic_can_only(&config)?;
        // Baudrates outside the timing table are rejected.
        let (timing0, timing1) = baudrate_timing(config.baudrate)?;
        // Close any previous session before reopening.
        self.close_sync();
        // The channel index must be non-negative.
        if config.channel < 0 {
            return Err(config_err(
                "CANAnalyst",
                "CAN channel index must be zero-based and non-negative.",
            ));
        }
        self.err_received = false;
        self.last_error = None;
        self.device_type = device_type(config.hardware_type);
        self.device_index = 0; // device index defaults to 0
        self.can_index = config.channel as u32;
        // The generated bus ID template is "{adapter name}/CAN{channel+1}"
        // (see format_bus_id); a user-provided bus ID always wins.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("CANAnalyst", config.channel));
        // SAFETY (all FFI calls in this function): device_type/device_index/
        // can_index are passed by value; the init_config pointer references a
        // struct on this stack frame, valid for the duration of the call, and
        // the driver does not retain it. Return 0 = failure, non-zero = success.
        unsafe {
            // VCI_OpenDevice(type, index, 0) == 0 -> record LastError, return false.
            if (self.api.vci_open_device)(self.device_type, self.device_index, 0) == 0 {
                self.last_error = Some(format!(
                    "VCI_OpenDevice failed. DeviceType={}, DeviceIndex={}.",
                    self.device_type, self.device_index
                ));
                return Ok(false);
            }
            self.device_opened = true;
            // AccCode=0, AccMask=0xFFFFFFFF, Reserved=0, Filter=1, Timing0/1, Mode=0.
            let init_config = VciInitConfig {
                acc_code: 0,
                acc_mask: u32::MAX,
                reserved: 0,
                filter: 1,
                timing0,
                timing1,
                mode: 0,
            };
            if (self.api.vci_init_can)(
                self.device_type,
                self.device_index,
                self.can_index,
                &init_config,
            ) == 0
            {
                self.last_error = Some(format!(
                    "VCI_InitCAN failed. CANIndex={}, Baudrate={}.",
                    self.can_index, config.baudrate
                ));
                self.close_native_no_throw();
                return Ok(false);
            }
            // The receive buffer is always cleared on open.
            (self.api.vci_clear_buffer)(self.device_type, self.device_index, self.can_index);
            if (self.api.vci_start_can)(self.device_type, self.device_index, self.can_index) == 0 {
                self.last_error =
                    Some(format!("VCI_StartCAN failed. CANIndex={}.", self.can_index));
                self.close_native_no_throw();
                return Ok(false);
            }
        }
        self.is_open = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], _frame_type: FrameType) -> Result<usize> {
        // The FrameType parameter is unused (classic CAN only).
        // Payloads over 8 bytes: record LastError and return 0 (no error raised).
        if data.len() > 8 {
            self.last_error = Some(
                "ControlCAN supports classic CAN only; payload length must be 0..8 bytes."
                    .to_string(),
            );
            return Ok(0);
        }
        // Normalize the ID and build the frame object; an invalid ID records
        // LastError and returns 0 (unreachable with the current implementation,
        // see normalize_can_id).
        let Some((obj, raw_id)) = build_tx_obj(can_id, data) else {
            self.last_error = Some(format!("Invalid CAN ID: 0x{can_id:08X}."));
            return Ok(0);
        };
        // Not open: record LastError and return 0.
        if !self.is_open {
            self.last_error = Some("CANAnalystCAN is not open.".to_string());
            return Ok(0);
        }
        // SAFETY: `obj` points to a struct on this stack frame; VCI_Transmit
        // sends synchronously and does not retain the pointer; the device/CAN
        // indices come from a successfully opened session; length=1 means a
        // single frame.
        let st = unsafe {
            (self.api.vci_transmit)(self.device_type, self.device_index, self.can_index, &obj, 1)
        };
        // Transmit failure: record LastError and return 0.
        if st == 0 {
            self.last_error = Some(format!("VCI_Transmit failed. CANID=0x{raw_id:08X}."));
            return Ok(0);
        }
        // Record the sent frame and return its data length.
        let frame = CanFrame::new(&self.bus_id, raw_id, data.to_vec(), true, FrameType::CAN20B);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        // Not open: no frame.
        if !self.is_open {
            return Ok(None);
        }
        let mut obj = VciCanObj::default();
        // SAFETY: `obj` points to a struct on this stack frame, valid for the
        // duration of the call; length=1 requests a single frame; waitTime is
        // receive_wait_time_ms (a short 1 ms wait by default, so no long
        // blocking).
        let st = unsafe {
            (self.api.vci_receive)(
                self.device_type,
                self.device_index,
                self.can_index,
                &mut obj,
                1,
                self.receive_wait_time_ms,
            )
        };
        match st {
            // 0xFFFFFFFF: set err_received, record LastError, no frame.
            VCI_RECEIVE_ERROR => {
                self.err_received = true;
                self.last_error = Some("VCI_Receive returned 0xFFFFFFFF.".to_string());
                Ok(None)
            }
            // 0: no frame available.
            0 => Ok(None),
            // A frame was received.
            _ => Ok(Some(rx_frame_from_obj(&self.bus_id, &obj))),
        }
    }
}

impl Drop for CanAnalystCan {
    fn drop(&mut self) {
        // Close the channel.
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::CanFdBaudrate;
    use std::mem::{align_of, size_of};

    #[test]
    fn baudrate_timing_table() {
        // The complete baudrate -> (Timing0, Timing1) mapping.
        assert_eq!(baudrate_timing(CanBaudrate::B10Kbit).unwrap(), (49, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B20Kbit).unwrap(), (24, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B50Kbit).unwrap(), (9, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B100Kbit).unwrap(), (3, 47));
        assert_eq!(baudrate_timing(CanBaudrate::B125Kbit).unwrap(), (3, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B250Kbit).unwrap(), (1, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B500Kbit).unwrap(), (0, 28));
        assert_eq!(baudrate_timing(CanBaudrate::B800Kbit).unwrap(), (0, 22));
        assert_eq!(baudrate_timing(CanBaudrate::B1Mbit).unwrap(), (0, 20));
        // A baudrate outside the table -> Driver error.
        let err = baudrate_timing(CanBaudrate::B2Mbit).unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("CANAnalyst"));
                assert!(msg.contains("_2MBit"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(baudrate_timing(CanBaudrate::NotSet).is_err());
    }

    #[test]
    fn device_type_selection() {
        // Device type selection rules.
        assert_eq!(device_type(3), DEV_USBCAN);
        assert_eq!(device_type(4), DEV_USBCAN2);
        assert_eq!(device_type(0), DEV_USBCAN2);
        assert_eq!(device_type(-1), DEV_USBCAN2);
        assert_eq!(device_type(99), DEV_USBCAN2);
    }

    #[test]
    fn normalize_can_id_rules() {
        // CAN ID normalization rules.
        // Standard frame: bare ID <= 0x7FF.
        assert_eq!(normalize_can_id(0x123), Some((0x123, 0x123, false)));
        assert_eq!(normalize_can_id(0x7FF), Some((0x7FF, 0x7FF, false)));
        // A bare ID beyond 11 bits is automatically treated as extended (even
        // without the flag).
        assert_eq!(
            normalize_can_id(0x800),
            Some((0x800, 0x800 | CAN_EXT_FLAG, true))
        );
        // Extended flag already set.
        assert_eq!(
            normalize_can_id(0x123 | CAN_EXT_FLAG),
            Some((0x123, 0x123 | CAN_EXT_FLAG, true))
        );
        // 29-bit upper bound.
        assert_eq!(
            normalize_can_id(0x1FFF_FFFF),
            Some((0x1FFF_FFFF, 0x1FFF_FFFF | CAN_EXT_FLAG, true))
        );
    }

    #[test]
    fn vci_struct_layouts() {
        // Expected layouts (see the per-struct docs for offsets and alignment).
        assert_eq!(size_of::<VciCanObj>(), 24);
        assert_eq!(align_of::<VciCanObj>(), 4);
        assert_eq!(size_of::<VciInitConfig>(), 16);
        assert_eq!(align_of::<VciInitConfig>(), 4);
        assert_eq!(size_of::<VciBoardInfo1>(), 52);
        assert_eq!(align_of::<VciBoardInfo1>(), 2);
        assert_eq!(size_of::<VciBoardInfo>(), 76);
        assert_eq!(align_of::<VciBoardInfo>(), 2);
    }

    #[test]
    fn build_tx_obj_mapping() {
        // TX frame object construction.
        let (obj, raw_id) = build_tx_obj(0x123, &[0x11, 0x22, 0x33]).unwrap();
        assert_eq!(raw_id, 0x123);
        assert_eq!(obj.id, 0x123);
        assert_eq!(obj.send_type, 0);
        assert_eq!(obj.remote_flag, 0);
        assert_eq!(obj.extern_flag, 0);
        assert_eq!(obj.data_len, 3);
        assert_eq!(&obj.data[..3], &[0x11, 0x22, 0x33]);
        assert_eq!(&obj.data[3..], &[0; 5]);

        // Extended frame: ExternFlag = 1, the flag is stripped from obj.ID,
        // raw_id keeps it.
        let (obj, raw_id) = build_tx_obj(0x456 | CAN_EXT_FLAG, &[]).unwrap();
        assert_eq!(raw_id, 0x456 | CAN_EXT_FLAG);
        assert_eq!(obj.id, 0x456);
        assert_eq!(obj.extern_flag, 1);
        assert_eq!(obj.data_len, 0);

        // A bare ID beyond 11 bits becomes extended automatically (same as
        // normalize_can_id).
        let (obj, _) = build_tx_obj(0x800, &[1]).unwrap();
        assert_eq!(obj.extern_flag, 1);
    }

    #[test]
    fn rx_frame_from_obj_mapping() {
        // Successful-receive frame reconstruction.
        let mut obj = VciCanObj {
            id: 0x2ABC_DEF0, // high-bit noise (bit 29), masked off by 0x1FFFFFFF
            extern_flag: 1,
            data_len: 8,
            data: [1, 2, 3, 4, 5, 6, 7, 8],
            ..VciCanObj::default()
        };
        let frame = rx_frame_from_obj("CANAnalyst:0:0", &obj);
        assert_eq!(frame.id, 0x0ABC_DEF0 | CAN_EXT_FLAG);
        assert_eq!(frame.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(!frame.is_master_frame);
        assert_eq!(frame.frame_type, FrameType::CAN20B);
        assert_eq!(frame.bus_id, "CANAnalyst:0:0");

        // DataLen > 8 is clamped to 8; standard frames do not get the extended flag.
        obj.id = CAN_STD_ID_MASK;
        obj.extern_flag = 0;
        obj.data_len = 200;
        let frame = rx_frame_from_obj("B", &obj);
        assert_eq!(frame.id, 0x7FF);
        assert_eq!(frame.data.len(), 8);
    }

    #[test]
    fn fd_config_rejected_on_open() {
        // FD configurations are rejected at open time (before any FFI call, so
        // no real DLL/hardware is needed).
        let classic = CanConfiguration::new(0, CanBaudrate::B500Kbit, CanFdBaudrate::NotUsed);
        assert!(ensure_classic_can_only(&classic).is_ok());

        let fd = CanConfiguration::new(0, CanBaudrate::B500Kbit, CanFdBaudrate::B2Mbit);
        match ensure_classic_can_only(&fd).unwrap_err() {
            Error::Driver(msg) => {
                assert!(msg.contains("CANAnalyst"));
                assert!(msg.contains("classic CAN only"));
            }
            other => panic!("unexpected error: {other}"),
        }
        let fd2 = CanConfiguration::with_bit_rate_config(0, Default::default());
        assert!(matches!(
            ensure_classic_can_only(&fd2),
            Err(Error::Driver(_))
        ));
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A definitely nonexistent DLL name must yield Error::Driver, not a panic.
        let err = ControlCan::load_from("no_such_controlcan_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // The ControlCAN.dll installed by the CANalyst-II tool (C:\Program
        // Files (x86)\USB_CAN TOOL\) is 32-bit: loading it from an x64 test
        // process must fail with Error::Driver; if a driver of matching bitness
        // is installed, construction should succeed with a complete symbol
        // table. Either outcome is acceptable — the key requirement is no panic.
        match CanAnalystCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!dev.err_received());
                assert!(dev.last_error().is_none());
                assert!(dev.unique_bus_id() >= 1);
                // is_available only probes and must not panic (returns false
                // without hardware).
                let _ = autors_runtime::block_on(dev.is_available()).unwrap();
                // When not open, send returns 0 and receive returns None.
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x123, &[1, 2, 3], FrameType::CAN20B))
                        .unwrap(),
                    0
                );
                assert_eq!(dev.last_error(), Some("CANAnalystCAN is not open."));
                assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
                // Payloads over 8 bytes -> return 0.
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x123, &[0u8; 9], FrameType::CAN20B))
                        .unwrap(),
                    0
                );
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
