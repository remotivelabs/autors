//! Intrepid Control Systems (ICS neoVI) adapter for the `icsneo40` driver API.
//! Loads `icsneo40.dll` at runtime (Intrepid Control Systems neoVI API, a public
//! C API; all exports use `__stdcall`, which matches the C calling convention on
//! x64, so `extern "system"` is used uniformly). If the DLL is not installed or
//! an export is missing, construction returns [`Error::Driver`] instead of
//! panicking.
//! Driver notes:
//! - The LIN portion of the neoVI API is out of scope for this adapter.
//! - `icsneoFindDevices` is not exported by older driver versions (it was added
//!   in a newer driver release); it is loaded as an optional symbol and checked
//!   at `open()` time, so the adapter loads fine against older drivers and only
//!   fails when a device is actually opened.
//! - Network IDs follow the public icsnvc40.h `NETID_HSCAN*` constants (see
//!   `net_id_for_channel`): NETID_HSCAN=1, HSCAN2=42, HSCAN3=44, HSCAN4=61,
//!   HSCAN5=62, HSCAN6=96, HSCAN7=97. Channels outside this table are rejected
//!   with "Channel {0} is not supported".
//! - The icsSpyMessage / NeoDevice(Ex) / OptionsFindNeoEx layouts match the
//!   public icsnvc40.h header field for field; status bits and protocol numbers
//!   follow the public cicsSpyStatusBits.h (SPY_STATUS_TX_MSG=0x02,
//!   XTD_FRAME=0x04, SPY_STATUS3_CANFD_FDF=0x08, BRS=0x10, SPY_PROTOCOL_CAN=1,
//!   CANFD=30).

use std::collections::VecDeque;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, data_from_array, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG,
    CAN_EXT_ID_MASK,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType};

/// Name of the icsneo40 driver DLL.
const ICSNEO40_DLL: &str = "icsneo40.dll";

// ---- icsnvc40.h / cicsSpyStatusBits.h constants ----
/// Every icsneo* call reports failure as any return value `!= 1`.
const ICS_SUCCESS: i32 = 1;
/// SPY_STATUS_TX_MSG (RX filter: skip echoes of frames this device sent).
const SPY_STATUS_TX_MSG: u32 = 0x02;
/// SPY_STATUS_XTD_FRAME (extended-frame flag, used on both TX and RX).
const SPY_STATUS_XTD_FRAME: u32 = 0x04;
/// SPY_STATUS3_CANFD_FDF (StatusBitField3: CAN FD frame).
const SPY_STATUS3_CANFD_FDF: u32 = 0x08;
/// SPY_STATUS3_CANFD_BRS (StatusBitField3: bit-rate switch).
const SPY_STATUS3_CANFD_BRS: u32 = 0x10;
/// SPY_PROTOCOL_CAN.
const SPY_PROTOCOL_CAN: u8 = 1;
/// SPY_PROTOCOL_CANFD.
const SPY_PROTOCOL_CANFD: u8 = 30;
/// Receive-buffer capacity in messages; the API returns at most 20000 messages
/// per call.
const RX_BUFFER_MESSAGES: usize = 20000;

// ---- icsneo40 structures (#[repr(C)] with natural alignment; field order and
// types match the public icsnvc40.h header) ----

/// icsneo40 `NeoDevice` (20 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct IcsNeoDevice {
    device_type: u32,
    handle: i32,
    number_of_clients: i32,
    serial_number: i32,
    max_allowed_clients: i32,
}

/// icsneo40 `NeoDeviceEx` (96 bytes on x64).
/// The trailing `tcpPort` (u16) + `Reserved0` (u16) pair is stored as a single
/// u32 field; the layout matches the public header.
#[repr(C)]
#[derive(Clone, Copy)]
struct IcsNeoDeviceEx {
    neo_device: IcsNeoDevice,
    firmware_major: u32,
    firmware_minor: u32,
    status: u32,
    options: u32,
    avail_wifi_network: *mut u8,
    wifi_interface_info: *mut u8,
    is_ethernet_device: i32,
    mac_address: [u8; 6],
    hardware_rev: u16,
    rev_reserved: u16,
    ip_address: [u32; 4],
    /// Combined tcpPort + Reserved0.
    tcp_port_and_reserved0: u32,
    reserved1: u32,
}

/// icsneo40 `OptionsFindNeoEx` (17 ints = 68 bytes; the public header declares
/// this as union { int32 iNetworkID; uint32 Reserved[16] }).
#[repr(C)]
#[derive(Clone, Copy)]
struct IcsOptionsFindNeoEx {
    reserved: [i32; 17],
}

/// icsneo40 `icsSpyMessage` (72 bytes on x64).
/// Uses the older public icsnvc40.h layout: `NumberBytesData` is followed
/// directly by `DescriptionID`, without the newer `NetworkID2` field.
#[repr(C)]
#[derive(Clone, Copy)]
struct IcsSpyMessage {
    /// SPY_STATUS_* bit field.
    status_bit_field: u32,
    /// SPY_STATUS2_* bit field.
    status_bit_field2: u32,
    time_hardware: i32,
    time_hardware2: i32,
    time_system: i32,
    time_system2: i32,
    time_stamp_hardware_id: u8,
    time_stamp_system_id: u8,
    /// Network number (NETID_*).
    network_id: u8,
    node_id: u8,
    /// SPY_PROTOCOL_* protocol number.
    protocol: u8,
    message_piece_id: u8,
    extra_data_ptr_enabled: u8,
    number_bytes_header: u8,
    /// Number of data bytes (truncated to u8 on TX).
    number_bytes_data: u8,
    description_id: i16,
    /// CAN ID (29-bit mask; the extended-frame flag lives in status_bit_field).
    arb_id_or_header: u32,
    /// First 8 data bytes, packed little-endian.
    data: u64,
    /// SPY_STATUS3_* bit field.
    status_bit_field3: u32,
    /// Declared as i32 here; the public header's StatusBitField4 is u32 (same width).
    status_bit_field4: i32,
    /// Extended data pointer for FD payloads longer than 8 bytes.
    extra_data_ptr: *mut u8,
    misc_data: u8,
}

// SAFETY: plain FFI message POD; `extra_data_ptr` is a driver-owned data
// pointer only dereferenced under exclusive (`&mut self`) device access, so
// moving the buffer to another thread cannot alias an active read.
unsafe impl Send for IcsSpyMessage {}

// Compile-time layout size checks (byte-for-byte match with the driver ABI).
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<IcsNeoDevice>() == 20);
    assert!(std::mem::size_of::<IcsNeoDeviceEx>() == 96);
    assert!(std::mem::size_of::<IcsOptionsFindNeoEx>() == 68);
    assert!(std::mem::size_of::<IcsSpyMessage>() == 72);
};
#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(std::mem::size_of::<IcsNeoDevice>() == 20);
    assert!(std::mem::size_of::<IcsNeoDeviceEx>() == 84);
    assert!(std::mem::size_of::<IcsOptionsFindNeoEx>() == 68);
    assert!(std::mem::size_of::<IcsSpyMessage>() == 64);
};

/// Maps a zero-based channel index to the icsneo40 network ID of the
/// corresponding HSCAN channel ("HSCAN{channel+1}").
/// The mapping follows the public icsnvc40.h `NETID_HSCAN*` constants
/// (NETID_HSCAN=1, HSCAN2=42, HSCAN3=44, HSCAN4=61, HSCAN5=62, HSCAN6=96,
/// HSCAN7=97). Unsupported channels return `None`.
fn net_id_for_channel(channel: i32) -> Option<i32> {
    match channel {
        0 => Some(1),  // HSCAN1 = NETID_HSCAN
        1 => Some(42), // HSCAN2
        2 => Some(44), // HSCAN3
        3 => Some(61), // HSCAN4
        4 => Some(62), // HSCAN5
        5 => Some(96), // HSCAN6
        6 => Some(97), // HSCAN7
        _ => None,
    }
}

/// Builds the TX message for a frame, kept separate from the actual send so it
/// can be unit-tested. Returns (message, whether to use the
/// TxMessagesEx/ExtraDataPtr path).
fn build_tx_message(
    can_id: u32,
    data: &[u8],
    frame_type: FrameType,
    fd_opened: bool,
) -> (IcsSpyMessage, bool) {
    // SAFETY: all-zero POD (integers and null pointers are valid bit patterns).
    let mut msg: IcsSpyMessage = unsafe { std::mem::zeroed() };
    msg.status_bit_field = if can_id & CAN_EXT_FLAG != 0 {
        SPY_STATUS_XTD_FRAME
    } else {
        0
    };
    msg.arb_id_or_header = can_id & CAN_EXT_ID_MASK;
    // Intentional quirk, kept as part of the behavioral contract: lengths above
    // 255 are truncated to u8.
    msg.number_bytes_data = data.len() as u8;
    if !fd_opened || frame_type == FrameType::CAN20B {
        // Classic path: first 8 bytes packed little-endian; payloads longer
        // than 8 bytes are truncated.
        msg.data = data_from_array(data);
        (msg, false)
    } else {
        msg.protocol = SPY_PROTOCOL_CANFD;
        if frame_type.contains(FrameType::FD) {
            msg.status_bit_field3 = SPY_STATUS3_CANFD_FDF;
        }
        if frame_type.contains(FrameType::BRS) {
            // Intentional quirk, kept as part of the behavioral contract: BRS
            // is assigned rather than OR-ed in, so for FD|BRS frames the FDF
            // bit is overwritten and only BRS remains.
            msg.status_bit_field3 = SPY_STATUS3_CANFD_BRS;
        }
        if data.len() <= 8 {
            // Intentional quirk, kept as part of the behavioral contract: this
            // branch never writes msg.data (it stays zero), so FD frames of 8
            // bytes or fewer transmit an all-zero payload.
            (msg, false)
        } else {
            msg.extra_data_ptr_enabled = 1;
            (msg, true)
        }
    }
}

/// Receive filter: NetworkID match, not a TX echo (SPY_STATUS_TX_MSG=0), and
/// protocol CAN(1) or CANFD(30).
fn accept_rx_frame(msg_network_id: u8, net_id: i32, status_bit_field: u32, protocol: u8) -> bool {
    msg_network_id == net_id as u8
        && status_bit_field & SPY_STATUS_TX_MSG == 0
        && (protocol == SPY_PROTOCOL_CAN || protocol == SPY_PROTOCOL_CANFD)
}

/// Derives the FrameType from the FDF/BRS bits of StatusBitField3.
fn rx_frame_type(status_bit_field3: u32) -> FrameType {
    let mut frame_type = FrameType::CAN20B;
    if status_bit_field3 & SPY_STATUS3_CANFD_FDF != 0 {
        frame_type = frame_type | FrameType::FD;
    }
    if status_bit_field3 & SPY_STATUS3_CANFD_BRS != 0 {
        frame_type = frame_type | FrameType::BRS;
    }
    frame_type
}

/// icsneo40 function-pointer table; all 9 required functions are validated at
/// load time and a missing symbol is reported immediately as an error.
/// `icsneoFindDevices` is the exception: older driver versions do not export
/// it, so it is loaded as an optional symbol and checked at `open()` time.
/// Pointer-sized arguments use isize; all functions return 1 on success.
#[derive(Debug)]
struct IcsNeo40 {
    /// Keeps the library handle alive (`DllWrapper` from autors-native); never
    /// accessed directly.
    _dll: DllWrapper,
    /// int icsneoFindDevices(NeoDeviceEx *devices, int *numDevices,
    /// uint deviceTypes(=0), uint numDeviceTypes(=0), OptionsFindNeoEx *options,
    /// uint reserved(=0)).
    icsneo_find_devices: Option<
        unsafe extern "system" fn(
            *mut IcsNeoDeviceEx,
            *mut i32,
            u32,
            u32,
            *mut IcsOptionsFindNeoEx,
            u32,
        ) -> i32,
    >,
    /// int icsneoOpenNeoDevice(NeoDevice *device, IntPtr *hObject,
    /// unsigned char *serial(=NULL), int networkReadOnly(=1), int options(=0)).
    icsneo_open_neo_device:
        unsafe extern "system" fn(*mut IcsNeoDevice, *mut isize, *const u8, i32, i32) -> i32,
    /// int icsneoClosePort(IntPtr hObject, int *numErrors).
    icsneo_close_port: unsafe extern "system" fn(isize, *mut i32) -> i32,
    /// int icsneoSetBitRate(IntPtr hObject, int bitRate, int networkId).
    icsneo_set_bit_rate: unsafe extern "system" fn(isize, i32, i32) -> i32,
    /// int icsneoSetFDBitRate(IntPtr hObject, int bitRateFD, int networkId).
    icsneo_set_fd_bit_rate: unsafe extern "system" fn(isize, i32, i32) -> i32,
    /// int icsneoGetMessages(IntPtr hObject, icsSpyMessage *msgs,
    /// int *numMessages, int *numErrors).
    icsneo_get_messages:
        unsafe extern "system" fn(isize, *mut IcsSpyMessage, *mut i32, *mut i32) -> i32,
    /// int icsneoTxMessages(IntPtr hObject, icsSpyMessage *msgs, int networkId,
    /// int numMessages).
    icsneo_tx_messages: unsafe extern "system" fn(isize, *mut IcsSpyMessage, i32, i32) -> i32,
    /// int icsneoTxMessagesEx(IntPtr hObject, icsSpyMessage *msgs, int networkId,
    /// int numMessages, uint *numTxed, uint reserved(=0)).
    icsneo_tx_messages_ex:
        unsafe extern "system" fn(isize, *mut IcsSpyMessage, i32, i32, *mut u32, u32) -> i32,
    /// int icsneoWaitForRxMessagesWithTimeOut(IntPtr hObject, uint timeoutMs).
    icsneo_wait_for_rx_messages_with_time_out: unsafe extern "system" fn(isize, u32) -> i32,
}

impl IcsNeo40 {
    /// Loads icsneo40.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(ICSNEO40_DLL)
    }

    /// Loads the DLL from the given path/name and resolves all exported
    /// symbols (`icsneoFindDevices` optional).
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe DllMain execution) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function
        // pointers; the library handle lives in the same struct as the
        // pointers, keeping them valid for the struct's lifetime. The generic
        // T is a function-pointer type (Copy).
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
        // Optional symbol: a missing export only surfaces when the function is
        // actually used.
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: same as above; a missing symbol simply yields None.
                let r = unsafe { dll.library().get::<$ty>($name) };
                r.ok().map(|s| *s)
            }};
        }
        // There is no canInitialize/xlOpenDriver-style global initialization,
        // so no reference counting or Drop cleanup is needed: dropping the
        // DllWrapper unloads the library.
        Ok(Self {
            icsneo_find_devices: opt_sym!(
                b"icsneoFindDevices\0",
                unsafe extern "system" fn(
                    *mut IcsNeoDeviceEx,
                    *mut i32,
                    u32,
                    u32,
                    *mut IcsOptionsFindNeoEx,
                    u32,
                ) -> i32
            ),
            icsneo_open_neo_device: sym!(
                b"icsneoOpenNeoDevice\0",
                unsafe extern "system" fn(
                    *mut IcsNeoDevice,
                    *mut isize,
                    *const u8,
                    i32,
                    i32,
                ) -> i32
            ),
            icsneo_close_port: sym!(
                b"icsneoClosePort\0",
                unsafe extern "system" fn(isize, *mut i32) -> i32
            ),
            icsneo_set_bit_rate: sym!(
                b"icsneoSetBitRate\0",
                unsafe extern "system" fn(isize, i32, i32) -> i32
            ),
            icsneo_set_fd_bit_rate: sym!(
                b"icsneoSetFDBitRate\0",
                unsafe extern "system" fn(isize, i32, i32) -> i32
            ),
            icsneo_get_messages: sym!(
                b"icsneoGetMessages\0",
                unsafe extern "system" fn(isize, *mut IcsSpyMessage, *mut i32, *mut i32) -> i32
            ),
            icsneo_tx_messages: sym!(
                b"icsneoTxMessages\0",
                unsafe extern "system" fn(isize, *mut IcsSpyMessage, i32, i32) -> i32
            ),
            icsneo_tx_messages_ex: sym!(
                b"icsneoTxMessagesEx\0",
                unsafe extern "system" fn(
                    isize,
                    *mut IcsSpyMessage,
                    i32,
                    i32,
                    *mut u32,
                    u32,
                ) -> i32
            ),
            icsneo_wait_for_rx_messages_with_time_out: sym!(
                b"icsneoWaitForRxMessagesWithTimeOut\0",
                unsafe extern "system" fn(isize, u32) -> i32
            ),
            _dll: dll,
        })
    }
}

/// Intrepid (ICS neoVI) CAN channel adapter.
/// Reception follows the non-blocking contract of [`CanDevice::receive`]: a
/// zero-timeout wait probes for pending messages, which are then drained into
/// an internal queue; blocking waits are the responsibility of the caller's
/// polling loop.
/// `open()` performs the full open sequence; `is_available()` only reports
/// whether the device is currently open, without opening it.
pub struct IntrepidCan {
    core: DeviceCore,
    api: IcsNeo40,
    /// Device handle.
    handle: isize,
    /// Whether the device is open.
    opened: bool,
    /// Whether the device was configured in FD mode. Intentional quirk, kept
    /// as part of the behavioral contract: this flag is never reset, so
    /// closing and reopening with a classic configuration still leaves it set.
    fd: bool,
    /// NETID of the current channel.
    net_id: i32,
    /// Bus ID (taken from the configuration, or generated at open time).
    bus_id: String,
    /// Receive-frame queue.
    rx_queue: VecDeque<CanFrame>,
    /// 20000-message buffer for GetMessages, allocated at construction.
    rx_buf: Vec<IcsSpyMessage>,
}

impl IntrepidCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.opened {
            let mut num_errors: i32 = 0;
            // icsneoClosePort; the return value is ignored.
            // SAFETY: handle is valid; num_errors is a local stack variable.
            unsafe { (self.api.icsneo_close_port)(self.handle, &mut num_errors) };
            self.handle = 0;
            self.opened = false;
            self.rx_queue.clear();
        }
    }

    /// Loads icsneo40.dll and validates its symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        // SAFETY: all-zero POD (integers and null pointers are valid bit patterns).
        let zero: IcsSpyMessage = unsafe { std::mem::zeroed() };
        Ok(Self {
            core: DeviceCore::new(),
            api: IcsNeo40::load()?,
            handle: 0,
            opened: false,
            fd: false,
            net_id: 0,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
            rx_buf: vec![zero; RX_BUFFER_MESSAGES],
        })
    }

    /// Returns whether the device is currently open.
    pub fn is_open(&self) -> bool {
        self.opened
    }
}

#[async_trait]
impl CanDevice for IntrepidCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // Only reports status; does not open the device (see the type-level docs).
        Ok(self.opened)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        // Already open: return true immediately without reconfiguring.
        if self.opened {
            return Ok(true);
        }
        // Default BusId when the configuration does not provide one (e.g. "Intrepid/CAN1").
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Intrepid", config.channel));
        // FindDevices + OpenNeoDevice.
        let find = self.api.icsneo_find_devices.ok_or_else(|| {
            config_err(
                &self.bus_id,
                "icsneo40: export icsneoFindDevices not found (driver too old for this wrapper)",
            )
        })?;
        // SAFETY: all-zero POD.
        let mut device: IcsNeoDeviceEx = unsafe { std::mem::zeroed() };
        let mut count: i32 = 1;
        let mut options: IcsOptionsFindNeoEx = unsafe { std::mem::zeroed() };
        // SAFETY: all pointers reference local stack variables; deviceTypes=0
        // (NULL), numDeviceTypes=0, reserved=0.
        let ret = unsafe { find(&mut device, &mut count, 0, 0, &mut options, 0) };
        if ret != ICS_SUCCESS {
            // The message text is part of the behavioral contract: "Failed({0}) call to FindDevices".
            return Err(config_err(
                &self.bus_id,
                format!("Failed({ret}) call to FindDevices"),
            ));
        }
        if count < 0 {
            // The message text is part of the behavioral contract: "No connected devices found".
            return Err(config_err(&self.bus_id, "No connected devices found"));
        }
        let mut handle: isize = 0;
        // SAFETY: device.neo_device comes from a successful FindDevices;
        // serial=NULL, networkReadOnly=1, options=0.
        let ret = unsafe {
            (self.api.icsneo_open_neo_device)(
                &mut device.neo_device,
                &mut handle,
                std::ptr::null(),
                1,
                0,
            )
        };
        if ret != ICS_SUCCESS {
            // The message text is part of the behavioral contract: "Failed({0}) call to OpenDevice".
            return Err(config_err(
                &self.bus_id,
                format!("Failed({ret}) call to OpenDevice"),
            ));
        }
        self.handle = handle;
        self.opened = true;
        // Channel/baudrate configuration; on failure the device is closed
        // before the error is returned.
        let result = (|| -> Result<()> {
            self.net_id = net_id_for_channel(config.channel).ok_or_else(|| {
                // The message text is part of the behavioral contract: "Channel {0} is not supported".
                config_err(
                    &self.bus_id,
                    format!("Channel {} is not supported", config.channel + 1),
                )
            })?;
            if config.baudrate != CanBaudrate::NotSet {
                // SAFETY: handle is valid; arguments are passed by value.
                let ret = unsafe {
                    (self.api.icsneo_set_bit_rate)(
                        self.handle,
                        config.baudrate.as_u32() as i32,
                        self.net_id,
                    )
                };
                if ret != ICS_SUCCESS {
                    // The message text is part of the behavioral contract: "Failed({0}) call to SetBitRate for ${1}".
                    return Err(config_err(
                        &self.bus_id,
                        format!(
                            "Failed({ret}) call to SetBitRate for ${}",
                            config.bit_rate_str()
                        ),
                    ));
                }
            }
            if config.baudrate_fd != CanFdBaudrate::NotUsed {
                // Custom FD bit timing is not supported (fd_bit_rate_config is
                // ignored; only the numeric FD baudrate is passed).
                // SAFETY: handle is valid; arguments are passed by value.
                let ret = unsafe {
                    (self.api.icsneo_set_fd_bit_rate)(
                        self.handle,
                        config.baudrate_fd.as_u32() as i32,
                        self.net_id,
                    )
                };
                if ret != ICS_SUCCESS {
                    // The message text is part of the behavioral contract: "Failed({0}) call to SetFDBitRate for {1}".
                    return Err(config_err(
                        &self.bus_id,
                        format!(
                            "Failed({ret}) call to SetFDBitRate for {}",
                            config.bit_rate_str()
                        ),
                    ));
                }
                self.fd = true;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.close_sync();
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // Returns 0 when the device is not open.
        if !self.opened {
            return Ok(0);
        }
        let (mut msg, use_ex) = build_tx_message(can_id, data, frame_type, self.fd);
        let ret = if use_ex {
            // The data pointer is borrowed directly: icsneoTxMessagesEx reads
            // synchronously and does not retain the pointer.
            msg.extra_data_ptr = data.as_ptr() as *mut u8;
            let mut num_txed: u32 = 0;
            // SAFETY: handle is valid; msg and data outlive the call; num_txed
            // points to a local stack variable.
            unsafe {
                (self.api.icsneo_tx_messages_ex)(
                    self.handle,
                    &mut msg,
                    self.net_id,
                    1,
                    &mut num_txed,
                    0,
                )
            }
        } else {
            // SAFETY: handle is valid; msg is a local stack variable that the
            // DLL reads synchronously without retaining the pointer.
            unsafe { (self.api.icsneo_tx_messages)(self.handle, &mut msg, self.net_id, 1) }
        };
        // A return value != 1 means failure; send reports 0 bytes rather than
        // an error.
        if ret != ICS_SUCCESS {
            return Ok(0);
        }
        // Record statistics and return the data length.
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if !self.opened {
            return Ok(None);
        }
        // Per the trait's non-blocking contract, probe once with a zero
        // timeout, then drain pending messages with GetMessages.
        // SAFETY: handle is valid; arguments are passed by value.
        let ret = unsafe { (self.api.icsneo_wait_for_rx_messages_with_time_out)(self.handle, 0) };
        if ret == 0 {
            return Ok(None);
        }
        let mut count: i32 = 0;
        let mut num_errors: i32 = 0;
        // SAFETY: handle is valid; rx_buf holds 20000 entries (the API returns
        // at most 20000 per call); count/num_errors point to local stack
        // variables.
        let ret = unsafe {
            (self.api.icsneo_get_messages)(
                self.handle,
                self.rx_buf.as_mut_ptr(),
                &mut count,
                &mut num_errors,
            )
        };
        if ret == 0 {
            return Ok(None);
        }
        // Defensively clamp count to the buffer range (a well-behaved driver
        // never exceeds it).
        let count = (count.max(0) as usize).min(self.rx_buf.len());
        for msg in self.rx_buf[..count].iter() {
            let msg = *msg;
            if !accept_rx_frame(
                msg.network_id,
                self.net_id,
                msg.status_bit_field,
                msg.protocol,
            ) {
                continue;
            }
            let mut id = msg.arb_id_or_header;
            if msg.status_bit_field & SPY_STATUS_XTD_FRAME != 0 {
                id |= CAN_EXT_FLAG;
            }
            let frame_type = rx_frame_type(msg.status_bit_field3);
            let n = msg.number_bytes_data as usize;
            let data = if msg.extra_data_ptr_enabled > 0 && !msg.extra_data_ptr.is_null() {
                // SAFETY: the driver-returned pointer refers to an internal
                // buffer of NumberBytesData bytes, valid until the next
                // GetMessages call; it is copied immediately. The null check
                // above guards against a null pointer.
                unsafe { std::slice::from_raw_parts(msg.extra_data_ptr, n).to_vec() }
            } else {
                // Unpack the little-endian u64 and take the first dlc bytes
                // (at most 8).
                msg.data.to_le_bytes()[..n.min(8)].to_vec()
            };
            self.rx_queue
                .push_back(CanFrame::new(&self.bus_id, id, data, false, frame_type));
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for IntrepidCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_have_expected_layout() {
        // Byte-for-byte layout match with the driver ABI (runtime check in
        // addition to the compile-time asserts).
        assert_eq!(std::mem::size_of::<IcsNeoDevice>(), 20);
        assert_eq!(std::mem::size_of::<IcsOptionsFindNeoEx>(), 68);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<IcsNeoDeviceEx>(), 96);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<IcsSpyMessage>(), 72);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(std::mem::size_of::<IcsNeoDeviceEx>(), 84);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(std::mem::size_of::<IcsSpyMessage>(), 64);
    }

    #[test]
    fn net_id_table_matches_public_header() {
        // NETID_HSCAN* constants from the public icsnvc40.h ("HSCAN{channel+1}").
        assert_eq!(net_id_for_channel(0), Some(1));
        assert_eq!(net_id_for_channel(1), Some(42));
        assert_eq!(net_id_for_channel(2), Some(44));
        assert_eq!(net_id_for_channel(3), Some(61));
        assert_eq!(net_id_for_channel(4), Some(62));
        assert_eq!(net_id_for_channel(5), Some(96));
        assert_eq!(net_id_for_channel(6), Some(97));
        // Channels outside the table -> None.
        assert_eq!(net_id_for_channel(7), None);
        assert_eq!(net_id_for_channel(-1), None);
    }

    #[test]
    fn tx_message_classic_fields() {
        // Classic extended frame: status=XTD_FRAME, 29-bit arb ID mask,
        // little-endian packed data, non-Ex path.
        let (msg, use_ex) =
            build_tx_message(0x8000_0123, &[0x11, 0x22, 0x33], FrameType::CAN20B, false);
        assert!(!use_ex);
        assert_eq!(msg.status_bit_field, SPY_STATUS_XTD_FRAME);
        assert_eq!(msg.arb_id_or_header, 0x123);
        assert_eq!(msg.number_bytes_data, 3);
        assert_eq!(msg.protocol, 0);
        assert_eq!(msg.data, 0x00_00_00_00_00_33_22_11);
        assert_eq!(msg.extra_data_ptr_enabled, 0);
        // Classic standard frame: no XTD bit.
        let (msg, _) = build_tx_message(0x7FF, &[0xAA; 8], FrameType::CAN20B, false);
        assert_eq!(msg.status_bit_field, 0);
        assert_eq!(msg.arb_id_or_header, 0x7FF);
        assert_eq!(msg.data, u64::from_le_bytes([0xAA; 8]));
        // Classic frame with > 8 bytes: data truncated to the first 8 bytes,
        // NumberBytesData keeps the original length.
        let (msg, _) = build_tx_message(
            0x123,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9],
            FrameType::CAN20B,
            false,
        );
        assert_eq!(msg.number_bytes_data, 9);
        assert_eq!(msg.data, u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));
        // FD opened but frame type is CAN20B: still uses the classic path.
        let (msg, use_ex) = build_tx_message(0x123, &[0xCC; 4], FrameType::CAN20B, true);
        assert!(!use_ex);
        assert_eq!(msg.protocol, 0);
        assert_eq!(msg.data, 0x0000_0000_CCCC_CCCC);
    }

    #[test]
    fn tx_message_fd_fields() {
        // FD <= 8 bytes: protocol CANFD(30), FDF set, data stays all-zero
        // (intentional quirk, part of the behavioral contract).
        let (msg, use_ex) = build_tx_message(0x123, &[0xAA; 4], FrameType::FD, true);
        assert!(!use_ex);
        assert_eq!(msg.protocol, SPY_PROTOCOL_CANFD);
        assert_eq!(msg.status_bit_field3, SPY_STATUS3_CANFD_FDF);
        assert_eq!(msg.data, 0);
        assert_eq!(msg.extra_data_ptr_enabled, 0);
        // FD|BRS: BRS overwrites FDF (assignment, not |=).
        let (msg, _) = build_tx_message(0x123, &[0xAA; 4], FrameType::FD_BRS, true);
        assert_eq!(msg.status_bit_field3, SPY_STATUS3_CANFD_BRS);
        // FD > 8 bytes: ExtraDataPtr path (EDP=1, data pointer set by send at
        // call time).
        let (msg, use_ex) = build_tx_message(0x123, &[0xAA; 12], FrameType::FD_BRS, true);
        assert!(use_ex);
        assert_eq!(msg.protocol, SPY_PROTOCOL_CANFD);
        assert_eq!(msg.number_bytes_data, 12);
        assert_eq!(msg.extra_data_ptr_enabled, 1);
        // When not opened in FD mode, FD frame types are ignored and the
        // classic path is used.
        let (msg, use_ex) = build_tx_message(0x123, &[1, 2], FrameType::FD_BRS, false);
        assert!(!use_ex);
        assert_eq!(msg.protocol, 0);
        assert_eq!(msg.data, 0x0000_0000_0000_0201);
    }

    #[test]
    fn rx_accept_filter() {
        // Normal frames: network match, no TX echo bit, protocol CAN/CANFD.
        assert!(accept_rx_frame(1, 1, 0, SPY_PROTOCOL_CAN));
        assert!(accept_rx_frame(
            42,
            42,
            SPY_STATUS_XTD_FRAME,
            SPY_PROTOCOL_CANFD
        ));
        // Network ID mismatch.
        assert!(!accept_rx_frame(2, 1, 0, SPY_PROTOCOL_CAN));
        // TX echo (SPY_STATUS_TX_MSG).
        assert!(!accept_rx_frame(1, 1, SPY_STATUS_TX_MSG, SPY_PROTOCOL_CAN));
        // Other protocols (LIN=12 etc.).
        assert!(!accept_rx_frame(1, 1, 0, 12));
        assert!(!accept_rx_frame(1, 1, 0, 0));
    }

    #[test]
    fn rx_frame_type_mapping() {
        assert_eq!(rx_frame_type(0), FrameType::CAN20B);
        assert_eq!(rx_frame_type(SPY_STATUS3_CANFD_FDF), FrameType::FD);
        assert_eq!(rx_frame_type(SPY_STATUS3_CANFD_BRS), FrameType::BRS);
        assert_eq!(
            rx_frame_type(SPY_STATUS3_CANFD_FDF | SPY_STATUS3_CANFD_BRS),
            FrameType::FD_BRS
        );
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that certainly does not exist: must yield Error::Driver,
        // not a panic.
        let err = IcsNeo40::load_from("no_such_icsneo40_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // On a machine with the Intrepid driver installed (System32
        // icsneo40.dll): loading must succeed with all 9 required symbols
        // (icsneoFindDevices is loaded as optional; a missing export is not an
        // error). On a machine without the driver: Error::Driver. Both
        // outcomes are acceptable; the test must not panic.
        match IntrepidCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                // With the device closed, send returns 0 and receive returns None.
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
