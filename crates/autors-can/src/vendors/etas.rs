//! ETAS BOA (Basic Open API) V2 OCI driver adapter for ETAS CAN hardware
//! (ES600/ES891 etc.).
//! ETAS CAN hardware is accessed through BOA's OCI (Open Controller Interface)
//! C API. Two DLLs are loaded dynamically at runtime:
//! - `dll-ocdProxy.dll`: 7 CAN functions (`OCI_*`);
//! - `dll-csiBind.dll`: 2 hardware-topology enumeration functions (`CSI_*`, used
//!   when `open` is called without a BusId, and by `available_channels`).
//!
//! BOA is installed by default under
//! `%ProgramFiles%\ETAS\BOA_V2\Bin\{x64|Win32}\Dll\Framework\`.
//! All OCI/CSI entry points use the C calling convention (`extern "C"`, identical
//! to stdcall on x64). If a DLL is not installed or an export is missing,
//! construction returns [`Error::Driver`].
//!
//! The adapter uses these OCI/CSI entry points:
//! `OCI_OpenCANController`/`OCI_DestroyCANController`/`OCI_CreateCANTxQueue`/
//! `OCI_CreateCANRxQueue`/`OCI_WriteCANDataEx`/`CSI_CreateProtocolTree`/
//! `CSI_DestroyProtocolTree`, plus the `OCI_CANConfiguration`/`OCI_CANFDConfiguration`/
//! message/queue/filter layouts and enum values (EXT=0x1, BRS=0x8, FD=0x40,
//! RX=1/TX=2/FDRX=8/FDTX=9).
//!
//! Behavioral notes:
//! - The DLLs are probed by bare name first (works when BOA is on PATH), then
//!   under the standard BOA install path; there is no separate up-front existence
//!   check, and failure yields [`Error::Driver`]. The registry override
//!   `HKLM\SOFTWARE\ETAS\BOA\2\PATH` is not consulted.
//! - `open()` performs the full open sequence; `is_available()` only reports
//!   state (same split as kvaser.rs).
//! - The driver callback only enqueues received frames; blocking/polling waits
//!   are handled by the upper layer `crate::device::start_dispatch`. The
//!   callback's userData carries the receive-context pointer (OCI passes userData
//!   back verbatim as the callback's first argument, so this is safe).
//! - If `open` fails midway, the controller handle is destroyed immediately
//!   (retry semantics are equivalent, and half-open handles are avoided).
//! - The CSI protocol tree is built and destroyed on each enumeration (a
//!   topology snapshot; semantically equivalent to caching it).
//! - `CANConfiguration.fd_bit_rate_config` (custom bit timing) is not supported
//!   and is ignored; FD mode is decided solely by `baudrate_fd != NotUsed`.

use std::collections::VecDeque;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, CanDevice, ChannelInfo, DeviceCore, CAN_EXT_FLAG, MAX_DLC, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFdBaudrate, CanFrame, FrameType};

/// OCI proxy DLL (source of the CAN function table).
const OCI_DLL: &str = "dll-ocdProxy.dll";
/// CSI binding DLL (hardware enumeration).
const CSI_DLL: &str = "dll-csiBind.dll";

// ---- OCI/BOA constants (values match the public OCI headers) ----
/// OCI_SUCCESS.
const OCI_SUCCESS: u32 = 0;
/// OCI_ERROR category mask (`OCI_OpenCANController` treats `ERROR & result` as failure).
const OCI_ERROR: u32 = 0x8000_0000;

// ---- OCI_CANMessageDataType (values match python-can) ----
/// OCI_CAN_RX_MESSAGE (classic CAN receive message).
const MSG_TYPE_CAN_RX: u32 = 1;
/// OCI_CAN_TX_MESSAGE (classic CAN transmit message).
const MSG_TYPE_CAN_TX: u32 = 2;
/// OCI_CANFDRX_MESSAGE (CAN FD receive message).
const MSG_TYPE_CANFD_RX: u32 = 8;
/// OCI_CANFDTX_MESSAGE (CAN FD transmit message).
const MSG_TYPE_CANFD_TX: u32 = 9;

// ---- OCI_CAN_MSG_FLAG_* (u16 frame flags, values match python-can) ----
/// OCI_CAN_MSG_FLAG_EXTENDED (extended frame).
const FRAME_FLAG_EXTENDED: u16 = 0x1;
/// OCI_CAN_MSG_FLAG_FD_DATA_BIT_RATE (BRS).
const FRAME_FLAG_FD_BRS: u16 = 0x8;
/// OCI_CAN_MSG_FLAG_FD_DATA (FD frame).
const FRAME_FLAG_FD_DATA: u16 = 0x40;
/// FD|BRS combination (frame type value 3 maps to flags value 72).
const FRAME_FLAG_FD_BRS_COMBO: u16 = 0x48;

// ---- CSI (hardware topology enumeration) ----
/// CSI_NODE_TYPE MIN_PHYSICAL_NODE.
const CSI_MIN_PHYSICAL_NODE: u32 = 0x2000;
/// CSI_NODE_TYPE MAX_PHYSICAL_NODE.
const CSI_MAX_PHYSICAL_NODE: u32 = 0x2FFF;
/// CSI_NODE_TYPE CONTROLLER_PORT: the node type the enumeration filters for.
const CSI_CONTROLLER_PORT: i32 = 8193;
/// Offset of nodeType inside CSI_SubItem.
const CSI_NODE_TYPE_OFFSET: usize = 172;
/// Offset of uriName (128-byte ASCII) inside CSI_SubItem.
const CSI_NAME_OFFSET: usize = 176;
/// Length of the uriName buffer.
const CSI_NAME_LEN: usize = 128;
/// CSI_Tree.item (CSI_SubItem) is a fixed 628 bytes; pointer-field offsets follow
/// pointer-width alignment.
#[cfg(target_pointer_width = "64")]
const CSI_SIBLING_OFFSET: usize = 632;
#[cfg(target_pointer_width = "32")]
const CSI_SIBLING_OFFSET: usize = 628;
#[cfg(target_pointer_width = "64")]
const CSI_CHILD_OFFSET: usize = 640;
#[cfg(target_pointer_width = "32")]
const CSI_CHILD_OFFSET: usize = 632;

/// Walks a CSI protocol tree without taking ownership of the driver-allocated
/// nodes. Siblings retain the current path prefix; children inherit the path of
/// their parent.
unsafe fn walk_csi_tree(node: *const u8, filter: &str, prefix: &str, ports: &mut Vec<String>) {
    if node.is_null() {
        return;
    }

    // SAFETY: the caller guarantees that `node` points to a CSI_Tree node. The
    // fixed offsets and buffer length are part of the CSI ABI, and unaligned
    // reads are used because the allocation's Rust alignment is unknown.
    let node_type =
        unsafe { std::ptr::read_unaligned(node.add(CSI_NODE_TYPE_OFFSET) as *const i32) };
    let name_bytes = unsafe { std::slice::from_raw_parts(node.add(CSI_NAME_OFFSET), CSI_NAME_LEN) };
    let name_len = name_bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(CSI_NAME_LEN);
    let name = String::from_utf8_lossy(&name_bytes[..name_len]);
    let path = if prefix.is_empty() {
        format!("/{name}")
    } else {
        format!("{prefix}/{name}")
    };
    if node_type == CSI_CONTROLLER_PORT && path.contains(filter) {
        ports.push(path.clone());
    }

    let sibling =
        unsafe { std::ptr::read_unaligned(node.add(CSI_SIBLING_OFFSET) as *const *const u8) };
    let child = unsafe { std::ptr::read_unaligned(node.add(CSI_CHILD_OFFSET) as *const *const u8) };
    // SAFETY: sibling and child pointers are links within the same live CSI
    // protocol tree and therefore have the same validity guarantee as `node`.
    unsafe {
        walk_csi_tree(sibling, filter, prefix, ports);
        walk_csi_tree(child, filter, &path, ports);
    }
}

// ---- OCI receive message (RX) offsets/sizes: 8-byte header (type+reserved), then
// timeStamp(8)/tag(4)/frameID(4)/flags(2)/res(1)/dlc(1)/res1(4)/data[N] ----
/// frameID offset.
const RX_FRAME_ID_OFFSET: usize = 20;
/// flags offset.
const RX_FLAGS_OFFSET: usize = 24;
/// dlc/size offset.
const RX_DLC_OFFSET: usize = 27;
/// Data offset (the message header is 32 bytes).
const RX_DATA_OFFSET: usize = 32;
/// Total classic RX message length (40).
const RX_CLASSIC_SIZE: usize = RX_DATA_OFFSET + MAX_DLC;
/// Total FD RX message length (96).
const RX_FD_SIZE: usize = RX_DATA_OFFSET + MAX_FD_DLC;

/// Name table for the OCI_ErrorCode values; both mask values (ERROR/WARNING/category
/// bits) and full codes are included. Unknown values return `None`.
fn status_name(status: u32) -> Option<&'static str> {
    Some(match status {
        0x0000_0000 => "SUCCESS",
        0x8000_0000 => "ERROR",
        0x4000_0000 => "WARNING",
        0x0000_1000 => "PARAM",
        0x0000_3000 => "SEMANTIC",
        0x0000_4000 => "RESOURCE",
        0x0000_5000 => "COMMUNICATION",
        0x0000_6000 => "INTERNAL",
        0x0000_2000 => "BIND",
        0x8000_1000 => "INVALID_PARAMETER",
        0x8000_1001 => "INCONSISTENT_PARAMETER_SET",
        0x8000_1002 => "INVALID_HANDLE",
        0x8000_1003 => "BUFFER_OVERFLOW",
        0x8000_1004 => "UNSUPPORTED_PARAMETER",
        0x8000_1005 => "INVALID_FILTER",
        0x8000_1006 => "FILTER_UNKNOWN",
        0x4000_1000 => "INCONSISTENT_SELF_RECEPTION",
        0x8000_3000 => "INVALID_STATE",
        0x8000_3001 => "NO_CONFIG",
        0x8000_3002 => "QUEUE_IS_FULL",
        0x8000_3003 => "OBJECT_IN_USE",
        0x8000_3004 => "LATER_VERSION_IN_USE",
        0x8000_4000 => "INCOMPATIBLE_CONFIG",
        0x8000_4001 => "OUT_OF_MEMORY",
        0x8000_4002 => "NO_RESOURCES",
        0x8000_4004 => "TIMEOUT",
        0x4000_4000 => "PARAM_ADAPTED",
        0x4000_4001 => "NO_INORDER_TX",
        0x4000_4002 => "PERFORMANCE_RISK",
        0x8000_5000 => "DRIVER_NO_RESPONSE",
        0x8000_5001 => "DRIVER_DISCONNECTED",
        0x8000_6000 => "UNEXPECTED_NULL",
        0x8000_6001 => "HW_NOT_READY",
        0x8000_6002 => "OUT_OF_RANGE",
        0x8000_6003 => "INTERNAL_STATE",
        0x8000_6004 => "INTERNAL_HANDLE",
        0x8000_6005 => "INTERNAL_OVERFLOW",
        0x8000_6006 => "NOT_IMPLEMENTED",
        0x8000_6007 => "SYSTEM_ERROR",
        0x8000_6008 => "NO_DATA",
        0x8000_6009 => "NOT_INITIALIZED",
        0x8000_2000 => "PROTOCOL_VERSION_NOT_SUPPORTED",
        0x8000_2001 => "INVALID_ACCESS_SYNTAX",
        0x8000_2002 => "INVALID_ACCESS_PARAM",
        0x8000_2003 => "INVALID_TRANSFER_SYNTAX",
        0x8000_2004 => "NO_INTERFACE",
        0x8000_2005 => "HW_NOT_PRESENT",
        0x8000_2006 => "CANNOT_OPEN_DRIVER",
        0x8000_2007 => "LICENSE_MISSING",
        0x8000_2008 => "CANNOT_OPEN_DRIVER",
        0x8000_2009 => "NONUNIQUE_BIND_TARGET",
        0x8000_200A => "WRONG_API_VERSION",
        0x8000_200B => "UNKNOWN_SERVICE",
        0x4000_2000 => "OLDER_VERSION_IS_BETTER",
        0x4000_2001 => "API_V1_0_IS_UNAVAILABLE",
        _ => return None,
    })
}

/// Formats a status code the way the enum `"{0}"` format does: defined values
/// print the member name, undefined values print the decimal number.
fn result_str(status: u32) -> String {
    match status_name(status) {
        Some(name) => name.to_string(),
        None => status.to_string(),
    }
}

/// BOA_Version (matches python-can's `BOA_Version`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OciVersion {
    major: u8,
    minor: u8,
    bugfix: u8,
    build: u8,
}

/// Requested OCI API version 1.3.
const OCI_API_VERSION: OciVersion = OciVersion {
    major: 1,
    minor: 3,
    bugfix: 0,
    build: 0,
};

/// OCI_CANFDConfiguration (field semantics named per the public OCI header).
/// Note the OCI 1.3 layout: `can_fd_tx_config` precedes
/// `can_rx_mode`/`can_fd_rx_mode` (python-can's 1.4 layout is the reverse; the
/// values 4/2/4 are only consistent with TX first).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OciCanFdConfiguration {
    /// Data-phase baud rate (Hz; `CANFDBaudrate` enum value).
    data_bit_rate: u32,
    /// Data-phase sample point (fixed 80).
    data_sample_point: u32,
    /// Data-phase bit-timing cycles (fixed 10).
    data_btl_cycles: u32,
    /// Data-phase SJW (fixed 1).
    data_sjw: u32,
    /// flags (fixed 0).
    flags: u32,
    /// txSecondarySamplePointOffset (fixed 0).
    tx_secondary_sample_point_offset: u32,
    /// OCI_CANFDTX_USE_CAN_AND_CANFD_FRAMES (fixed 4).
    can_fd_tx_config: u32,
    /// OCI_CAN_RXMODE_CAN_FRAMES_USING_CAN_MESSAGE (fixed 2).
    can_rx_mode: u32,
    /// OCI_CANFDRXMODE_CANFD_FRAMES_USING_CANFD_MESSAGE (fixed 4).
    can_fd_rx_mode: u32,
    /// txSecondarySamplePointFilterWindow (fixed 0).
    tx_secondary_sample_point_filter_window: u16,
    /// reserved (fixed 0).
    reserved: u16,
}

/// OCI_CANConfiguration (field semantics named per the public OCI header; layout
/// matches python-can's `OCI_CANConfiguration`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OciCanConfiguration {
    /// Nominal baud rate (Hz; `CANBaudrate` enum value, NotSet=0 passed through as-is).
    baudrate: u32,
    /// Sample point (fixed 75).
    sample_point: u32,
    /// Samples per bit (fixed 1).
    samples_per_bit: u32,
    /// Bit-timing cycles (fixed 10).
    btl_cycles: u32,
    /// SJW (fixed 1).
    sjw: u32,
    /// OCI_CAN_SINGLE_SYNC_EDGE (fixed 1).
    sync_edge: u32,
    /// OCI_CAN_MEDIA_HIGH_SPEED (fixed 1).
    physical_media: u32,
    /// OCI_SELF_RECEPTION_OFF (fixed 0).
    self_reception_mode: u32,
    /// OCI_BUSMODE_ACTIVE (fixed 2).
    bus_participation_mode: u32,
    /// CAN FD enable (`BaudrateFD != NotUsed`).
    can_fd_enabled: u32,
    /// CAN FD sub-configuration.
    can_fd_config: OciCanFdConfiguration,
    /// OCI_CANTX_FIFO (fixed 1).
    can_tx_policy: u32,
}

/// Builds the OCI configuration from a channel configuration.
/// `fd_bit_rate_config` (custom bit timing) is not supported and is ignored.
fn oci_can_configuration(config: &CanConfiguration) -> OciCanConfiguration {
    OciCanConfiguration {
        baudrate: config.baudrate.as_u32(),
        sample_point: 75,
        samples_per_bit: 1,
        btl_cycles: 10,
        sjw: 1,
        sync_edge: 1,
        physical_media: 1,
        self_reception_mode: 0,
        bus_participation_mode: 2,
        can_fd_enabled: u32::from(fd_requested(config)),
        can_fd_config: OciCanFdConfiguration {
            data_bit_rate: config.baudrate_fd.as_u32(),
            data_sample_point: 80,
            data_btl_cycles: 10,
            data_sjw: 1,
            flags: 0,
            tx_secondary_sample_point_offset: 0,
            can_fd_tx_config: 4,
            can_rx_mode: 2,
            can_fd_rx_mode: 4,
            tx_secondary_sample_point_filter_window: 0,
            reserved: 0,
        },
        can_tx_policy: 1,
    }
}

/// FD open decision: only `BaudrateFD != CANFDBaudrate.NotUsed` is considered.
fn fd_requested(config: &CanConfiguration) -> bool {
    config.baudrate_fd != CanFdBaudrate::NotUsed
}

/// Connection name: `"ETAS:/" + busId` (CSI paths start with `/`, so the result
/// often contains a double slash, kept as-is).
fn connection_name(bus_id: &str) -> String {
    format!("ETAS:/{bus_id}")
}

/// OCI_CANTxMessage plus the common message header (layout matches python-can's
/// `OCI_CANMessage{ type, reserved, txMessage }`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OciCanTxMessage {
    /// OCI_CAN_TX_MESSAGE.
    msg_type: u32,
    /// Reserved (always 0).
    reserved: u32,
    /// CAN ID (extended flag stripped).
    frame_id: u32,
    /// OCI_CAN_MSG_FLAG_*.
    flags: u16,
    /// Reserved (always 0).
    res: u8,
    /// Data length (byte count, not DLC-encoded).
    dlc: u8,
    /// Data (zero-padded to 8 bytes).
    data: [u8; MAX_DLC],
}

/// OCI_CANFDTxMessage plus the common message header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OciCanFdTxMessage {
    /// OCI_CANFDTX_MESSAGE.
    msg_type: u32,
    /// Reserved (always 0).
    reserved: u32,
    /// CAN ID (extended flag stripped).
    frame_id: u32,
    /// OCI_CAN_MSG_FLAG_* (EXT plus FD/BRS).
    flags: u16,
    /// Reserved (always 0).
    res: u8,
    /// Data length (byte count).
    size: u8,
    /// Data (zero-padded to 64 bytes).
    data: [u8; MAX_FD_DLC],
}

/// Builds a classic TX message. Payloads longer than 8 bytes are rejected with
/// [`Error::Invalid`].
fn build_tx_message(can_id: u32, data: &[u8]) -> Result<OciCanTxMessage> {
    if data.len() > MAX_DLC {
        return Err(Error::Invalid(format!(
            "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
            data.len()
        )));
    }
    let mut msg = OciCanTxMessage {
        msg_type: MSG_TYPE_CAN_TX,
        reserved: 0,
        frame_id: can_id & !CAN_EXT_FLAG,
        flags: if can_id & CAN_EXT_FLAG != 0 {
            FRAME_FLAG_EXTENDED
        } else {
            0
        },
        res: 0,
        dlc: data.len() as u8,
        data: [0; MAX_DLC],
    };
    msg.data[..data.len()].copy_from_slice(data);
    Ok(msg)
}

/// Builds an FD TX message. Frame-type flags are set only for `FD_BRS`/`FD` (a
/// bare `BRS` and classic frames get no FD bits). Payloads longer than 64 bytes
/// are rejected with [`Error::Invalid`].
fn build_fd_tx_message(
    can_id: u32,
    data: &[u8],
    frame_type: FrameType,
) -> Result<OciCanFdTxMessage> {
    if data.len() > MAX_FD_DLC {
        return Err(Error::Invalid(format!(
            "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
            data.len()
        )));
    }
    let mut flags = if can_id & CAN_EXT_FLAG != 0 {
        FRAME_FLAG_EXTENDED
    } else {
        0
    };
    if frame_type == FrameType::FD_BRS {
        flags |= FRAME_FLAG_FD_BRS_COMBO;
    } else if frame_type == FrameType::FD {
        flags |= FRAME_FLAG_FD_DATA;
    }
    let mut msg = OciCanFdTxMessage {
        msg_type: MSG_TYPE_CANFD_TX,
        reserved: 0,
        frame_id: can_id & !CAN_EXT_FLAG,
        flags,
        res: 0,
        size: data.len() as u8,
        data: [0; MAX_FD_DLC],
    };
    msg.data[..data.len()].copy_from_slice(data);
    Ok(msg)
}

/// Converts a received OCI message into a CANFrame: type 1 = classic RX (only
/// EXT is checked), type 8 = FD RX (FD/BRS also checked), all other types are
/// ignored; the payload is truncated to `min(dlc, capacity)` bytes.
fn parse_rx_message(bytes: &[u8], bus_id: &str) -> Option<CanFrame> {
    if bytes.len() < RX_DATA_OFFSET {
        return None;
    }
    let msg_type = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let (fd_message, cap) = match msg_type {
        MSG_TYPE_CAN_RX => (false, MAX_DLC),
        MSG_TYPE_CANFD_RX => (true, MAX_FD_DLC),
        _ => return None,
    };
    if bytes.len() < RX_DATA_OFFSET + cap {
        return None;
    }
    let mut id = u32::from_le_bytes(
        bytes[RX_FRAME_ID_OFFSET..RX_FRAME_ID_OFFSET + 4]
            .try_into()
            .ok()?,
    );
    let flags = u16::from_le_bytes(
        bytes[RX_FLAGS_OFFSET..RX_FLAGS_OFFSET + 2]
            .try_into()
            .ok()?,
    );
    let dlc = bytes[RX_DLC_OFFSET] as usize;
    if flags & FRAME_FLAG_EXTENDED != 0 {
        id |= CAN_EXT_FLAG;
    }
    let mut frame_type = FrameType::CAN20B;
    if fd_message {
        if flags & FRAME_FLAG_FD_DATA != 0 {
            frame_type = frame_type | FrameType::FD;
        }
        if flags & FRAME_FLAG_FD_BRS != 0 {
            frame_type = frame_type | FrameType::BRS;
        }
    }
    let len = dlc.min(cap);
    let data = bytes[RX_DATA_OFFSET..RX_DATA_OFFSET + len].to_vec();
    Some(CanFrame::new(bus_id, id, data, false, frame_type))
}

/// Receive context registered with the driver at `open` and reclaimed by `close`
/// after the controller is destroyed.
#[derive(Debug)]
struct RxContext {
    /// Received-frame queue (shared between the callback thread and `receive`).
    queue: Mutex<VecDeque<CanFrame>>,
    /// Bus identifier stamped onto received frames.
    bus_id: String,
    /// Controller liveness flag (checked first thing in the callback).
    open: AtomicBool,
}

/// Receive callback signature: `void cb(void* userData, OCI_CANMessage* msg)`,
/// C calling convention. OCI passes userData back verbatim as the first
/// argument; here it carries the `RxContext` pointer.
type OciRxCallback = unsafe extern "C" fn(*mut c_void, *const u8);

unsafe extern "C" fn rx_callback(user: *mut c_void, msg: *const u8) {
    if user.is_null() || msg.is_null() {
        return;
    }
    // SAFETY: `user` points to the RxContext registered at open; close clears the
    // open flag before destroying the controller, and the heap memory is only
    // reclaimed once the driver can no longer invoke the callback, so the pointer
    // stays valid for the callback's lifetime. `msg` is a driver-supplied message
    // whose type field determines its fixed length (40/96 bytes).
    let ctx = unsafe { &*(user as *const RxContext) };
    if !ctx.open.load(Ordering::SeqCst) {
        return;
    }
    let msg_type = unsafe { std::ptr::read_unaligned(msg as *const u32) };
    let size = match msg_type {
        MSG_TYPE_CAN_RX => RX_CLASSIC_SIZE,
        MSG_TYPE_CANFD_RX => RX_FD_SIZE,
        _ => return, // unknown message types (event/error frames) are ignored
    };
    let bytes = unsafe { std::slice::from_raw_parts(msg, size) };
    if let Some(frame) = parse_rx_message(bytes, &ctx.bus_id) {
        if let Ok(mut queue) = ctx.queue.lock() {
            queue.push_back(frame);
        }
    }
}

/// OCI_CANRxQueueConfiguration (matches python-can): two callback slots
/// (onFrame/onEvent) plus the self-reception mode.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OciCanRxQueueConfiguration {
    /// onFrame callback function pointer.
    on_frame_fn: Option<OciRxCallback>,
    /// onFrame userData (the context pointer).
    on_frame_user: *mut c_void,
    /// onEvent callback (not registered).
    on_event_fn: Option<OciRxCallback>,
    /// onEvent userData.
    on_event_user: *mut c_void,
    /// OCI_SELF_RECEPTION_OFF (fixed 0).
    self_reception_mode: u32,
}

/// CSI_NodeRange (matches python-can).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CsiNodeRange {
    min: u32,
    max: u32,
}

/// Loads a DLL following the BOA install layout: bare name first (works when BOA
/// is on PATH), then `%ProgramFiles%\ETAS\BOA_V2\Bin\{x64|Win32}\Dll\Framework\`.
/// The registry path override is not consulted.
fn load_boa_dll(name: &str) -> Result<DllWrapper> {
    if let Ok(dll) = DllWrapper::load(name) {
        return Ok(dll);
    }
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
    let arch = if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "Win32"
    };
    let full_path = format!(r"{program_files}\ETAS\BOA_V2\Bin\{arch}\Dll\Framework\{name}");
    DllWrapper::load(&full_path)
        .map_err(|e| Error::Driver(format!("failed to load {name} (PATH and {full_path}): {e}")))
}

/// dll-ocdProxy function-pointer table. All symbols are resolved and validated
/// at construction; a missing symbol is reported immediately instead of
/// surfacing later as a call through a null pointer. Field types follow the
/// Cdecl signatures of the OCI API.
#[derive(Debug)]
struct DllOcdProxy {
    /// Keeps the library handle alive (autors-native [`DllWrapper`]); the field
    /// is never accessed directly.
    _dll: DllWrapper,
    /// OCI_ErrorCode OCI_CreateCANControllerVersion(const char* name,
    /// BOA_Version* version, OCI_ControllerHandle* controller).
    oci_create_can_controller_version:
        unsafe extern "C" fn(*const c_char, *mut OciVersion, *mut i32) -> u32,
    /// OCI_ErrorCode OCI_DestroyCANController(OCI_ControllerHandle).
    oci_destroy_can_controller: unsafe extern "C" fn(i32) -> u32,
    /// OCI_ErrorCode OCI_OpenCANController(OCI_ControllerHandle,
    /// OCI_CANConfiguration* config, OCI_CANControllerProperties* properties)
    /// (the third argument is 4 zero bytes = mode RUNNING).
    oci_open_can_controller: unsafe extern "C" fn(i32, *mut OciCanConfiguration, *mut u32) -> u32,
    /// OCI_ErrorCode OCI_CreateCANTxQueue(OCI_ControllerHandle,
    /// OCI_CANTxQueueConfiguration* config, OCI_QueueHandle* queue)
    /// (the configuration is 4 zero bytes = reserved).
    oci_create_can_tx_queue: unsafe extern "C" fn(i32, *mut u32, *mut i32) -> u32,
    /// OCI_ErrorCode OCI_CreateCANRxQueue(OCI_ControllerHandle,
    /// OCI_CANRxQueueConfiguration* config, OCI_QueueHandle* queue).
    oci_create_can_rx_queue:
        unsafe extern "C" fn(i32, *mut OciCanRxQueueConfiguration, *mut i32) -> u32,
    /// OCI_ErrorCode OCI_WriteCANDataEx(OCI_QueueHandle, OCI_Time timestamp,
    /// OCI_CANMessageEx** messages, uint32 count, uint32* remaining)
    /// (timestamp is passed as -1 = OCI_NO_TIME).
    oci_write_can_data_ex: unsafe extern "C" fn(i32, i64, *const *const u8, u32, *mut u32) -> u32,
    /// OCI_ErrorCode OCI_AddCANFrameFilter(OCI_QueueHandle,
    /// OCI_CANRxFilter* filters, uint32 count) (a filter is a 12-byte triple; a
    /// single all-zero filter = receive everything).
    oci_add_can_frame_filter: unsafe extern "C" fn(i32, *const u32, u32) -> u32,
}

impl DllOcdProxy {
    /// Loads dll-ocdProxy.dll from the default paths (PATH → BOA standard install
    /// directory).
    fn load() -> Result<Self> {
        let dll = load_boa_dll(OCI_DLL)?;
        Self::from_wrapper(dll, OCI_DLL)
    }

    /// Loads from an explicit path/name and resolves all exports (for tests and
    /// diagnostics).
    #[allow(dead_code)] // production goes through load(); this function lets tests probe error paths
    fn load_from(path: &str) -> Result<Self> {
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        Self::from_wrapper(dll, path)
    }

    /// Symbol resolution.
    fn from_wrapper(dll: DllWrapper, path: &str) -> Result<Self> {
        // SAFETY (for every `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function pointers;
        // the library handle and the function pointers live in the same struct,
        // keeping the pointers valid. The generic T is a function-pointer type
        // (Copy).
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
            oci_create_can_controller_version: sym!(
                b"OCI_CreateCANControllerVersion\0",
                unsafe extern "C" fn(*const c_char, *mut OciVersion, *mut i32) -> u32
            ),
            oci_destroy_can_controller: sym!(
                b"OCI_DestroyCANController\0",
                unsafe extern "C" fn(i32) -> u32
            ),
            oci_open_can_controller: sym!(
                b"OCI_OpenCANController\0",
                unsafe extern "C" fn(i32, *mut OciCanConfiguration, *mut u32) -> u32
            ),
            oci_create_can_tx_queue: sym!(
                b"OCI_CreateCANTxQueue\0",
                unsafe extern "C" fn(i32, *mut u32, *mut i32) -> u32
            ),
            oci_create_can_rx_queue: sym!(
                b"OCI_CreateCANRxQueue\0",
                unsafe extern "C" fn(i32, *mut OciCanRxQueueConfiguration, *mut i32) -> u32
            ),
            oci_write_can_data_ex: sym!(
                b"OCI_WriteCANDataEx\0",
                unsafe extern "C" fn(i32, i64, *const *const u8, u32, *mut u32) -> u32
            ),
            oci_add_can_frame_filter: sym!(
                b"OCI_AddCANFrameFilter\0",
                unsafe extern "C" fn(i32, *const u32, u32) -> u32
            ),
            _dll: dll,
        })
    }
}

/// dll-csiBind function-pointer table.
#[derive(Debug)]
struct DllCsiBind {
    /// Keeps the library handle alive; the field is never accessed directly.
    _dll: DllWrapper,
    /// BOA_ResultCode CSI_CreateProtocolTree(const char* filter, CSI_NodeRange range,
    /// CSI_Tree** tree) (called with an empty filter string and the physical-node
    /// range).
    csi_create_protocol_tree:
        unsafe extern "C" fn(*const c_char, CsiNodeRange, *mut *mut c_void) -> u32,
    /// BOA_ResultCode CSI_DestroyProtocolTree(CSI_Tree* tree).
    csi_destroy_protocol_tree: unsafe extern "C" fn(*mut c_void) -> u32,
}

impl DllCsiBind {
    /// Loads dll-csiBind.dll from the default paths.
    fn load() -> Result<Self> {
        let dll = load_boa_dll(CSI_DLL)?;
        Self::from_wrapper(dll, CSI_DLL)
    }

    /// Loads from an explicit path/name and resolves all exports.
    #[allow(dead_code)] // production goes through load(); this function lets tests probe error paths
    fn load_from(path: &str) -> Result<Self> {
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        Self::from_wrapper(dll, path)
    }

    /// Symbol resolution (same SAFETY argument as `DllOcdProxy::from_wrapper`).
    fn from_wrapper(dll: DllWrapper, path: &str) -> Result<Self> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: as in DllOcdProxy; symbol addresses are only copied into
                // raw function pointers.
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
            csi_create_protocol_tree: sym!(
                b"CSI_CreateProtocolTree\0",
                unsafe extern "C" fn(*const c_char, CsiNodeRange, *mut *mut c_void) -> u32
            ),
            csi_destroy_protocol_tree: sym!(
                b"CSI_DestroyProtocolTree\0",
                unsafe extern "C" fn(*mut c_void) -> u32
            ),
            _dll: dll,
        })
    }

    /// Enumerates protocol-tree nodes with `nodeType == CONTROLLER_PORT` whose
    /// path contains `filter`, returned sorted by ascending path length (paths
    /// look like `/ETAS/ES600/CAN:1`). The tree is built and destroyed per call
    /// (a snapshot; see the module docs).
    fn enumerate_ports(&self, filter: &str) -> Result<Vec<String>> {
        let empty = CString::new("").expect("static empty string");
        let range = CsiNodeRange {
            min: CSI_MIN_PHYSICAL_NODE,
            max: CSI_MAX_PHYSICAL_NODE,
        };
        let mut root: *mut c_void = std::ptr::null_mut();
        // SAFETY: range is passed by value; root is a stack out-pointer; on
        // success a driver-allocated tree is returned.
        let rc = unsafe { (self.csi_create_protocol_tree)(empty.as_ptr(), range, &mut root) };
        if root.is_null() {
            // A nonzero result with a null tree is a configuration failure.
            return Err(config_err(
                "ETAS",
                format!("Failed to create protocol tree, result={}", result_str(rc)),
            ));
        }
        let mut ports: Vec<String> = Vec::new();
        // SAFETY: root is a driver-allocated tree in CSI_Tree layout; the walk
        // only reads fields.
        unsafe { walk_csi_tree(root as *const u8, filter, "", &mut ports) };
        // SAFETY: paired with the create call above.
        unsafe { (self.csi_destroy_protocol_tree)(root) };
        // Sort by ascending path length.
        ports.sort_by_key(|p| p.len());
        Ok(ports)
    }
}

/// ETAS BOA CAN channel adapter.
///
/// Construction loads and validates both BOA DLLs. [`CanDevice::open`]
/// resolves a CSI channel when no bus ID is supplied, creates an OCI
/// controller and its queues, and registers a receive callback. Received
/// frames remain queued until the caller polls the device.
pub struct EtasCan {
    core: DeviceCore,
    api: DllOcdProxy,
    csi: DllCsiBind,
    controller_handle: i32,
    tx_queue_handle: i32,
    rx_context: Option<Box<RxContext>>,
    fd_opened: bool,
    bus_id: String,
}

impl EtasCan {
    /// Loads the BOA OCI and CSI libraries and resolves every required export.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: DllOcdProxy::load()?,
            csi: DllCsiBind::load()?,
            controller_handle: -1,
            tx_queue_handle: -1,
            rx_context: None,
            fd_opened: false,
            bus_id: String::new(),
        })
    }

    /// Whether an OCI controller is currently open.
    pub fn is_open(&self) -> bool {
        self.controller_handle != -1
    }

    /// Synchronous close body shared by [`CanDevice::close`] and [`Drop`].
    fn close_sync(&mut self) {
        if let Some(context) = self.rx_context.as_ref() {
            context.open.store(false, Ordering::SeqCst);
        }

        let controller = std::mem::replace(&mut self.controller_handle, -1);
        self.tx_queue_handle = -1;
        if controller != -1 {
            // SAFETY: the handle was returned by OCI_CreateCANControllerVersion
            // and is cleared before this call, so it is destroyed at most once.
            // OCI_DestroyCANController also destroys its queues and waits until
            // their callbacks can no longer run.
            unsafe { (self.api.oci_destroy_can_controller)(controller) };
        }
        self.rx_context = None;
    }

    fn fail_open<T>(&mut self, hardware_id: &str, message: String) -> Result<T> {
        self.close_sync();
        Err(config_err(hardware_id, message))
    }

    /// Selects the configured CSI bus or resolves the zero-based channel index
    /// from the currently available CAN controller ports.
    fn select_bus_id(&self, config: &CanConfiguration) -> Result<String> {
        if let Some(bus_id) = config.bus_id.as_deref().filter(|id| !id.is_empty()) {
            return Ok(bus_id.to_string());
        }
        if config.channel < 0 {
            return Err(config_err(
                "ETAS",
                "CAN channel index must not be negative.",
            ));
        }
        let ports = self.csi.enumerate_ports("CAN")?;
        if ports.is_empty() {
            return Err(config_err("ETAS", "No CAN controller ports were found."));
        }
        ports.get(config.channel as usize).cloned().ok_or_else(|| {
            config_err(
                "ETAS",
                format!(
                    "CAN channel index {} is outside the {} available ports.",
                    config.channel,
                    ports.len()
                ),
            )
        })
    }
}

#[async_trait]
impl CanDevice for EtasCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        Ok(self.is_open())
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.is_open() {
            return Ok(true);
        }

        self.bus_id = self.select_bus_id(&config)?;
        self.fd_opened = fd_requested(&config);
        let access_name = connection_name(&self.bus_id);
        let access_name_c = CString::new(access_name.as_str()).map_err(|_| {
            config_err(
                &self.bus_id,
                "The CSI bus ID contains an embedded NUL byte.",
            )
        })?;

        let mut version = OCI_API_VERSION;
        let mut controller = -1;
        // SAFETY: the access string and both out-pointers remain valid for the
        // duration of the synchronous OCI call.
        let rc = unsafe {
            (self.api.oci_create_can_controller_version)(
                access_name_c.as_ptr(),
                &mut version,
                &mut controller,
            )
        };
        self.controller_handle = controller;
        if rc != OCI_SUCCESS || controller == -1 {
            return self.fail_open(
                &access_name,
                format!(
                    "OCI_CreateCANControllerVersion failed, result={}",
                    result_str(rc)
                ),
            );
        }

        let mut oci_config = oci_can_configuration(&config);
        let mut properties = 0u32;
        // SAFETY: the controller is live and both mutable structures remain
        // valid until the synchronous call returns.
        let rc = unsafe {
            (self.api.oci_open_can_controller)(
                self.controller_handle,
                &mut oci_config,
                &mut properties,
            )
        };
        if rc & OCI_ERROR != 0 {
            return self.fail_open(
                &access_name,
                format!(
                    "OCI_OpenCANController failed for {}, result={}",
                    config.bit_rate_str(),
                    result_str(rc)
                ),
            );
        }

        let mut tx_config = 0u32;
        let mut tx_queue = -1;
        // SAFETY: the controller is open; the configuration and out-pointer
        // remain valid for the duration of the call.
        let rc = unsafe {
            (self.api.oci_create_can_tx_queue)(
                self.controller_handle,
                &mut tx_config,
                &mut tx_queue,
            )
        };
        if rc != OCI_SUCCESS || tx_queue == -1 {
            return self.fail_open(
                &access_name,
                format!("OCI_CreateCANTxQueue failed, result={}", result_str(rc)),
            );
        }
        self.tx_queue_handle = tx_queue;

        let mut context = Box::new(RxContext {
            queue: Mutex::new(VecDeque::new()),
            bus_id: self.bus_id.clone(),
            open: AtomicBool::new(true),
        });
        let context_ptr = (&mut *context as *mut RxContext).cast::<c_void>();
        self.rx_context = Some(context);
        let mut rx_config = OciCanRxQueueConfiguration {
            on_frame_fn: Some(rx_callback),
            on_frame_user: context_ptr,
            on_event_fn: None,
            on_event_user: std::ptr::null_mut(),
            self_reception_mode: 0,
        };
        let mut rx_queue = -1;
        // SAFETY: the controller is open, the queue configuration is live for
        // the call, and the boxed callback context remains at a stable address
        // until after the controller is destroyed.
        let rc = unsafe {
            (self.api.oci_create_can_rx_queue)(
                self.controller_handle,
                &mut rx_config,
                &mut rx_queue,
            )
        };
        if rc != OCI_SUCCESS || rx_queue == -1 {
            return self.fail_open(
                &access_name,
                format!("OCI_CreateCANRxQueue failed, result={}", result_str(rc)),
            );
        }

        let accept_all_filter = [0u32; 3];
        // SAFETY: `rx_queue` is valid and the 12-byte filter remains live for
        // the synchronous call. An all-zero filter accepts every CAN frame.
        let rc =
            unsafe { (self.api.oci_add_can_frame_filter)(rx_queue, accept_all_filter.as_ptr(), 1) };
        if rc != OCI_SUCCESS {
            return self.fail_open(
                &access_name,
                format!("OCI_AddCANFrameFilter failed, result={}", result_str(rc)),
            );
        }

        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        if !self.is_open() || self.tx_queue_handle == -1 {
            return Ok(0);
        }

        let mut remaining = 1u32;
        let rc = if self.fd_opened {
            let message = build_fd_tx_message(can_id, data, frame_type)?;
            let message_ptr = (&message as *const OciCanFdTxMessage).cast::<u8>();
            // SAFETY: the queue is live; the message and pointer array remain
            // valid for the duration of this synchronous write.
            unsafe {
                (self.api.oci_write_can_data_ex)(
                    self.tx_queue_handle,
                    -1,
                    &message_ptr,
                    1,
                    &mut remaining,
                )
            }
        } else {
            let message = build_tx_message(can_id, data)?;
            let message_ptr = (&message as *const OciCanTxMessage).cast::<u8>();
            // SAFETY: same lifetime and queue guarantees as the FD branch.
            unsafe {
                (self.api.oci_write_can_data_ex)(
                    self.tx_queue_handle,
                    -1,
                    &message_ptr,
                    1,
                    &mut remaining,
                )
            }
        };
        if rc != OCI_SUCCESS {
            return Ok(0);
        }

        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if !self.is_open() {
            return Ok(None);
        }
        let Some(context) = self.rx_context.as_ref() else {
            return Ok(None);
        };
        let mut queue = context
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(queue.pop_front())
    }

    async fn available_channels(&self) -> Result<Vec<ChannelInfo>> {
        Ok(self
            .csi
            .enumerate_ports("CAN")?
            .into_iter()
            .enumerate()
            .map(|(channel, name)| ChannelInfo {
                channel: channel as i32,
                hardware_type: 0,
                name,
                supports_fd: true,
            })
            .collect())
    }
}

impl Drop for EtasCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csi_node(name: &str, node_type: i32) -> Box<[u8]> {
        let mut node =
            vec![0u8; CSI_CHILD_OFFSET + std::mem::size_of::<*const u8>()].into_boxed_slice();
        assert!(name.len() < CSI_NAME_LEN);
        node[CSI_NAME_OFFSET..CSI_NAME_OFFSET + name.len()].copy_from_slice(name.as_bytes());
        // SAFETY: the test allocation includes the complete field at the fixed
        // CSI offset; unaligned writes mirror the production reads.
        unsafe {
            std::ptr::write_unaligned(
                node.as_mut_ptr().add(CSI_NODE_TYPE_OFFSET) as *mut i32,
                node_type,
            );
        }
        node
    }

    unsafe fn set_csi_link(node: &mut [u8], offset: usize, target: *const u8) {
        // SAFETY: callers pass a CSI link offset within `node`; the allocation
        // remains live while the test tree is traversed.
        unsafe {
            std::ptr::write_unaligned(node.as_mut_ptr().add(offset) as *mut *const u8, target);
        }
    }

    #[test]
    fn message_layouts_match_oci_abi() {
        assert_eq!(std::mem::size_of::<OciCanTxMessage>(), 24);
        assert_eq!(std::mem::size_of::<OciCanFdTxMessage>(), 80);
        assert_eq!(std::mem::size_of::<OciCanConfiguration>(), 84);
    }

    #[test]
    fn builds_classic_and_fd_messages() {
        let classic = build_tx_message(CAN_EXT_FLAG | 0x123, &[1, 2, 3]).unwrap();
        assert_eq!(classic.msg_type, MSG_TYPE_CAN_TX);
        assert_eq!(classic.frame_id, 0x123);
        assert_eq!(classic.flags, FRAME_FLAG_EXTENDED);
        assert_eq!(classic.dlc, 3);
        assert_eq!(&classic.data[..3], &[1, 2, 3]);

        let fd = build_fd_tx_message(0x456, &[4, 5], FrameType::FD_BRS).unwrap();
        assert_eq!(fd.msg_type, MSG_TYPE_CANFD_TX);
        assert_eq!(fd.flags, FRAME_FLAG_FD_BRS_COMBO);
        assert_eq!(fd.size, 2);
        assert_eq!(&fd.data[..2], &[4, 5]);
    }

    #[test]
    fn parses_fd_receive_message() {
        let mut bytes = [0u8; RX_FD_SIZE];
        bytes[..4].copy_from_slice(&MSG_TYPE_CANFD_RX.to_le_bytes());
        bytes[RX_FRAME_ID_OFFSET..RX_FRAME_ID_OFFSET + 4].copy_from_slice(&0x123u32.to_le_bytes());
        bytes[RX_FLAGS_OFFSET..RX_FLAGS_OFFSET + 2]
            .copy_from_slice(&(FRAME_FLAG_EXTENDED | FRAME_FLAG_FD_BRS_COMBO).to_le_bytes());
        bytes[RX_DLC_OFFSET] = 3;
        bytes[RX_DATA_OFFSET..RX_DATA_OFFSET + 3].copy_from_slice(&[7, 8, 9]);

        let frame = parse_rx_message(&bytes, "ETAS/CAN1").unwrap();
        assert_eq!(frame.id, CAN_EXT_FLAG | 0x123);
        assert_eq!(frame.frame_type, FrameType::FD_BRS);
        assert_eq!(frame.data, [7, 8, 9]);
        assert!(!frame.is_master_frame);
    }

    #[test]
    fn walks_siblings_and_children_with_the_correct_prefix() {
        let mut root = csi_node("ES600", CSI_CONTROLLER_PORT + 1);
        let sibling = csi_node("CAN:2", CSI_CONTROLLER_PORT);
        let child = csi_node("CAN:1", CSI_CONTROLLER_PORT);
        // SAFETY: all three boxed allocations remain live through the walk and
        // the links point to their first bytes.
        unsafe {
            set_csi_link(&mut root, CSI_SIBLING_OFFSET, sibling.as_ptr());
            set_csi_link(&mut root, CSI_CHILD_OFFSET, child.as_ptr());
        }

        let mut ports = Vec::new();
        // SAFETY: root and both linked nodes use the CSI layout expected by the
        // walker and remain live until traversal completes.
        unsafe { walk_csi_tree(root.as_ptr(), "CAN", "", &mut ports) };
        assert_eq!(ports, ["/CAN:2", "/ES600/CAN:1"]);
    }

    #[test]
    fn connection_name_preserves_csi_path() {
        assert_eq!(
            connection_name("/ETAS/ES600/CAN:1"),
            "ETAS://ETAS/ES600/CAN:1"
        );
    }
}
