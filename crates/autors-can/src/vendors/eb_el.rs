//! Eberspächer Electronics FlexCard CAN adapter.
//! Drives Eberspächer Electronics FlexCard PMC/PMC-II/PXIe series
//! CAN/FlexRay interface cards through the vendor's fcBase API, dynamically
//! loading `fcbase.dll` at runtime. All exports use the Winapi/stdcall
//! calling convention (identical to the C convention on x64), so every
//! function pointer is typed `extern "system"`. When the DLL is not installed
//! or an export is missing, construction fails with [`Error::Driver`].
//! The DLL name and the ten exported symbol names match the fcBase API names
//! published in star electronics' FlexCard Windows driver release notes.
//! Function signatures follow the vendor's managed API declarations; the
//! export table could not be checked against an installed driver.
//! Behavioral contract (intentional quirks included):
//! - `send` calls `fcbCANTransmit(handle, channel, 0u, data, 1, 0)`: the
//!   `canID` argument is discarded and every frame is sent with ID=0, the 5th
//!   argument=1 and the 6th argument=0. Without a real driver the parameter
//!   semantics of this API cannot be established, so the call is reproduced
//!   as-is. `FrameType` is ignored entirely, and the statistics CANFrame is
//!   always CAN20B.
//! - The message list returned by `fcbReceive` is walked node by node; nodes
//!   advance the same way as in the interface list traversal, and the
//!   per-message filtering lives in `accept_can_msg`.
//! - If configuration fails during the open sequence, the connection that was
//!   already established is not closed (there is no fcbClose on that path);
//!   cleanup falls to `close()`/`Drop`.

use std::collections::VecDeque;
use std::ffi::c_void;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// Name of the fcBase API DLL.
const FCBASE_DLL: &str = "fcbase.dll";

// ---- fcBase constants ----
/// fcb status code for success.
const FCB_OK: i32 = 0;
/// First argument of `fcFreeMemory` for releasing the message list returned
/// by `fcbReceive` (value 2).
const FCB_FREE_MESSAGE_LIST: i32 = 2;
/// First argument of `fcFreeMemory` for releasing the interface list returned
/// by `fcbGetEnumFlexCardsV3` (value 4).
const FCB_FREE_INTERFACE_LIST: i32 = 4;
/// Message list node type: CAN message (value 9).
const FCB_NODE_CAN_MESSAGE: i32 = 9;
/// Extended-frame flag bit in received message IDs (bit 29; `0x20000000`).
const FCB_MSG_EXT_FLAG: u32 = 0x2000_0000;
/// Mask for the ID portion of received message IDs (`0x1FFFFFFF`).
const FCB_MSG_ID_MASK: u32 = 0x1FFF_FFFF;
/// DLC mask within the DLC/flags byte (low 4 bits).
const FCB_DLC_MASK: u8 = 0x0F;
/// DLC/flags byte bit 4: when set the frame is discarded (meaning not
/// confirmed against vendor headers; suspected RTR).
const FCB_RX_SKIP_BIT4: u8 = 0x10;
/// DLC/flags byte bit 5: when set the frame is discarded (suspected error
/// frame/echo).
const FCB_RX_SKIP_BIT5: u8 = 0x20;

/// Pointer width (size of pointer fields in the Pack=1 native structures).
const PTR_SIZE: usize = std::mem::size_of::<*mut c_void>();

// ---- Native structure layouts (Pack=1) ----
// Message list node: i32 type + 2 pointers.
/// Offset of the node type field.
const MSG_NODE_TYPE_OFF: usize = 0;
/// Offset of the node payload pointer field.
const MSG_NODE_PAYLOAD_OFF: usize = 4;
/// Offset of the node next pointer field.
const MSG_NODE_NEXT_OFF: usize = 4 + PTR_SIZE;
/// Bytes to read per node.
const MSG_NODE_SIZE: usize = 4 + 2 * PTR_SIZE;

// CAN message (22 bytes): u32 id (bit 29 = extended) + u32 timestamp +
// u8 unused + u8 dlc/flags + i32 channel + u8 data[8].
/// Size of the message structure in bytes.
const CAN_MSG_SIZE: usize = 22;
/// Offset of the ID field.
const CAN_MSG_ID_OFF: usize = 0;
/// Offset of the DLC/flags byte.
const CAN_MSG_DLC_FLAGS_OFF: usize = 9;
/// Offset of the channel number field.
const CAN_MSG_CHANNEL_OFF: usize = 10;
/// Offset of the data field.
const CAN_MSG_DATA_OFF: usize = 14;

// Interface info node (the offsets below all precede the first pointer field
// and are pointer-width independent; the `next` pointer offset varies with
// pointer width):
/// Offset of the FlexCard ID (u32).
const IFACE_CARD_ID_OFF: usize = 0;
/// Offset of the status word (an int-typed enum; nonzero means valid):
/// 60 (start of the nested status struct) + 8 (field within it).
const IFACE_STATE_OFF: usize = 68;
/// Offset of the CAN channel count (byte; > 0 means the card has CAN).
const IFACE_CAN_COUNT_OFF: usize = 105;
/// Offset of the `next` pointer: 4 + 4 + 52 + (100 + PTR_SIZE) + 4.
const IFACE_NEXT_OFF: usize = 164 + PTR_SIZE;
/// Bytes to read per interface node (including the next pointer).
const IFACE_NODE_SIZE: usize = 164 + 2 * PTR_SIZE;

/// Names of the common fcb status codes, used in error messages (the member
/// name is what appears in the driver-facing error text).
fn status_name(status: i32) -> &'static str {
    match status {
        0 => "OK",
        0x63 => "INTERNAL_ERROR",
        0x64 => "RESERVED",
        0x65 => "NULL_PARAMETER",
        0x66 => "INVALID_PARAMETER",
        0x67 => "MEMORY_ALLOCATION_FAILED",
        0x68 => "INVALID_OBJECT_HANDLE",
        0x69 => "ACTION_NOT_SUPPORTED",
        0x6A => "PARAMETER_NOT_SUPPORTED",
        0x6B => "ACTION_FAILED",
        0x6C => "ACTION_NOT_ALLOWED_DURING_MONITORING",
        0x6D => "TEXT_NOT_DEFINIED", // Spelling as defined by the fcBase status enum.
        0x6E => "CONNECTION_ALREADY_OPEN",
        0x6F => "UNKOWN_FLEXCARD_ID", // Spelling as defined by the fcBase status enum.
        0x70 => "MONITORING_IS_ALREADY_ACTIVE",
        0x71 => "REGISTER_NOT_READABLE",
        0x72 => "REGISTER_NOT_WRITEABLE",
        0x73 => "PARSING_ERROR",
        0x74 => "INCOMPATIBLE_VERSION",
        0x75 => "CONFIGURATION_FAILED",
        0x76 => "RX_BUFFER_CANNOT_USED",
        0x77 => "INVALID_MSGBUF_SETTINGS",
        0x78 => "MINIMUM_FIFO_LIMIT",
        0x79 => "NOT_ENOUGH_MESSAGE_RAM",
        0x7A => "PAYLOAD_EXCEEDS_MAXIMUM",
        0x7B => "MSG_BUF_BUSY",
        0x7C => "MSG_BUF_LOCKED_FOR_TRANSMISSION",
        0x7D => "INCORRECT_POC_STATE",
        0x7E => "FIRMWARE_UPDATE_REQUIRED",
        0x7F => "FUNCTION_NOT_IMPLEMENTED",
        0x80 => "CONNECTION_RELEASED_AFTER_SURPRISE_REMOVAL",
        0x81 => "CC_INDEX_NOT_VALID",
        0x82 => "PMC_CARD_FUNCTION",
        0x83 => "TRIGGER_FUNCTION_VERSION_CONFLICT",
        0x84 => "XENOMAI_TASK_UNBLOCKED",
        0x85 => "XENOMAI_EVENT_DESTROYED",
        0x86 => "XENOMAI_TIMEDOUT",
        0x87 => "XENOMAI_ILLEGAL_INVOCATION",
        0x88 => "XENOMAI_NO_REALTIME_CONTEXT",
        0x89 => "CARDBUS_CARD_FUNCTION",
        0x8A => "DRIVER_UPDATE_REQUIRED",
        0x8B => "CC_CAN_NOT_AVAILABLE",
        0x8C => "VERSION_NOT_MATCH",
        0x8D => "INVALID_CCCONFIG_SETTINGS",
        0x8E => "BUS_TYPE_NOT_VALID",
        0x8F => "SELFSYNC_NOT_AVAILABLE",
        0x90 => "CAN_BUFFER_NOT_SET_YET",
        0x91 => "INVALID_HARDWARE_LICENSE",
        0x92 => "PMCII_CARD_FUNCTION",
        0x93 => "PCI_BASED_CARD_FUNCTION",
        0x94 => "USB_FULL_SPEED_UNSUPPORTED",
        0x95 => "TX_FIFO_NOT_CONFIGURED",
        0x96 => "TX_FIFO_FULL",
        0x97 => "TX_FIFO_MSGBUF_NR",
        _ => "FCB_???",
    }
}

/// Maps a non-OK status code to [`Error::Driver`]; the caller merges the
/// hardware ID in via [`config_err`].
fn check(status: i32, bus_id: &str, what: &str) -> Result<()> {
    if status == FCB_OK {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!("fcbase: {what} failed: {} ({status})", status_name(status)),
        ))
    }
}

/// CAN controller configuration, passed by value to
/// `fcbCANSetCcConfiguration`. With Pack=1, 4 × u16 + u32 + 6 × u32 = 36
/// bytes; all fields are naturally aligned, so the `#[repr(C)]` layout
/// matches byte for byte (no padding).
/// The semantics of the first four u16 fields are not confirmed against
/// vendor headers; they follow the customary naming
/// `prescaler`/`tseg1`/`tseg2`/`sjw`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FcbCcConfig {
    /// Prescaler-related value (1M→1, 500k→3, 250k→7, 125k→15, 100k→19,
    /// 50k→39, 20k→99, 10k→199, 800k→4).
    prescaler: u16,
    /// Default 1 (0 at 800k).
    tseg1: u16,
    /// Default 4 (1 at 800k).
    tseg2: u16,
    /// Default 1 (0 at 800k).
    sjw: u16,
    /// Never set; always 0.
    clock: u32,
    /// Reserved (always 0).
    reserved: [u32; 6],
}

/// Builds the controller configuration for a baud rate: start from the
/// defaults {0, 1, 4, 1}, then adjust `prescaler` per baud rate (800k
/// rewrites the whole row). Baud rates without a table entry (NotSet and
/// 2M/4M/5M/8M/10M) are passed through with the defaults — the driver may
/// then report CONFIGURATION_FAILED, which [`check`] maps to an error.
fn cc_config_for_baudrate(baudrate: CanBaudrate) -> FcbCcConfig {
    let mut cfg = FcbCcConfig {
        prescaler: 0,
        tseg1: 1,
        tseg2: 4,
        sjw: 1,
        clock: 0,
        reserved: [0; 6],
    };
    match baudrate {
        CanBaudrate::B1Mbit => cfg.prescaler = 1,
        CanBaudrate::B800Kbit => {
            cfg.prescaler = 4;
            cfg.tseg1 = 0;
            cfg.tseg2 = 1;
            cfg.sjw = 0;
        }
        CanBaudrate::B500Kbit => cfg.prescaler = 3,
        CanBaudrate::B250Kbit => cfg.prescaler = 7,
        CanBaudrate::B125Kbit => cfg.prescaler = 15,
        CanBaudrate::B100Kbit => cfg.prescaler = 19,
        CanBaudrate::B50Kbit => cfg.prescaler = 39,
        CanBaudrate::B20Kbit => cfg.prescaler = 99,
        CanBaudrate::B10Kbit => cfg.prescaler = 199,
        _ => {}
    }
    cfg
}

// ---------------------------------------------------------------------------
// Byte-buffer parsing helpers (pure functions, testable; the unsafe side only
// does fixed-size byte copies before calling them).
// ---------------------------------------------------------------------------

/// Reads a little-endian u32.
fn rd_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(
        buf[off..off + 4]
            .try_into()
            .expect("layout constant in range"),
    )
}

/// Reads a little-endian i32.
fn rd_i32(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(
        buf[off..off + 4]
            .try_into()
            .expect("layout constant in range"),
    )
}

/// Reads a pointer-width unsigned value (a pointer field in a Pack=1
/// structure), little-endian.
fn rd_ptr(buf: &[u8], off: usize) -> usize {
    let mut b = [0u8; 8];
    b[..PTR_SIZE].copy_from_slice(&buf[off..off + PTR_SIZE]);
    usize::from_le_bytes(b)
}

/// Parsed view of a CAN message (the meaningful fields of the 22-byte native
/// message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanMsgView {
    /// Raw ID (bit 29 = extended flag).
    id: u32,
    /// DLC/flags byte (low 4 bits DLC; bit4/bit5 are discard bits).
    dlc_flags: u8,
    /// Channel number.
    channel: i32,
    /// Payload (8 bytes).
    data: [u8; 8],
}

/// Parses a CAN message from a 22-byte buffer (layout constants: `CAN_MSG_*`).
fn parse_can_msg(buf: &[u8]) -> CanMsgView {
    debug_assert!(buf.len() >= CAN_MSG_SIZE);
    CanMsgView {
        id: rd_u32(buf, CAN_MSG_ID_OFF),
        dlc_flags: buf[CAN_MSG_DLC_FLAGS_OFF],
        channel: rd_i32(buf, CAN_MSG_CHANNEL_OFF),
        data: buf[CAN_MSG_DATA_OFF..CAN_MSG_DATA_OFF + 8]
            .try_into()
            .expect("layout constant in range"),
    }
}

/// Parses `(card ID, status word, CAN channel count, next pointer)` from an
/// interface node buffer.
fn parse_iface_node(buf: &[u8]) -> (u32, i32, u8, usize) {
    debug_assert!(buf.len() >= IFACE_NODE_SIZE);
    (
        rd_u32(buf, IFACE_CARD_ID_OFF),
        rd_i32(buf, IFACE_STATE_OFF),
        buf[IFACE_CAN_COUNT_OFF],
        rd_ptr(buf, IFACE_NEXT_OFF),
    )
}

/// Receive filtering and ID conversion: channel must match;
/// `id & 0x1FFFFFFF`, with [`CAN_EXT_FLAG`] added when bit 29 is set; DLC must
/// be > 0 with neither bit4 nor bit5 set. Returns `(library ID, DLC)`.
fn accept_can_msg(msg: &CanMsgView, channel: i32) -> Option<(u32, u8)> {
    if msg.channel != channel {
        return None;
    }
    let mut id = msg.id & FCB_MSG_ID_MASK;
    if msg.id & FCB_MSG_EXT_FLAG != 0 {
        id |= CAN_EXT_FLAG;
    }
    let dlc = msg.dlc_flags & FCB_DLC_MASK;
    if dlc == 0 || msg.dlc_flags & FCB_RX_SKIP_BIT5 != 0 || msg.dlc_flags & FCB_RX_SKIP_BIT4 != 0 {
        return None;
    }
    Some((id, dlc))
}

/// Walks the interface list and returns the ID of the first FlexCard with a
/// nonzero card ID, a nonzero status word, and CAN channel count > 0;
/// returns 0 when nothing matches.
/// # Safety
/// `list` must be a valid interface list head as returned by
/// `fcbGetEnumFlexCardsV3` (may be null, in which case 0 is returned) and
/// must not have been released with `fcFreeMemory` for the duration of the
/// call.
unsafe fn first_card_id_in_list(list: *mut c_void) -> u32 {
    let mut node = list.cast::<u8>();
    let mut buf = [0u8; IFACE_NODE_SIZE];
    while !node.is_null() {
        // SAFETY: node points to a driver-allocated interface node
        // (IFACE_NODE_SIZE bytes, Pack=1). The caller guarantees the list
        // stays valid until fcFreeMemory.
        unsafe { std::ptr::copy_nonoverlapping(node, buf.as_mut_ptr(), IFACE_NODE_SIZE) };
        let (card_id, state, can_channels, next) = parse_iface_node(&buf);
        if card_id != 0 && state != 0 && can_channels > 0 {
            return card_id;
        }
        node = next as *mut u8;
    }
    0
}

/// Walks the message list returned by `fcbReceive`, filters each message, and
/// enqueues the accepted frames.
/// Every node is visited in turn (nodes advance the same way as in the
/// interface list walk); filtering/conversion follows [`accept_can_msg`].
/// A DLC > 8 cannot be represented in a `CanFrame` and is mapped to
/// [`Error::Invalid`].
/// # Safety
/// `list` must be a valid message list head as returned by `fcbReceive` (may
/// be null, in which case the function returns immediately) and must not have
/// been released with `fcFreeMemory` for the duration of the call.
unsafe fn collect_msg_frames(
    list: *mut c_void,
    channel: i32,
    bus_id: &str,
    queue: &mut VecDeque<CanFrame>,
) -> Result<()> {
    let mut node = list.cast::<u8>();
    let mut node_buf = [0u8; MSG_NODE_SIZE];
    let mut msg_buf = [0u8; CAN_MSG_SIZE];
    while !node.is_null() {
        // SAFETY: node points to a driver-allocated message list node
        // (MSG_NODE_SIZE bytes, Pack=1).
        unsafe { std::ptr::copy_nonoverlapping(node, node_buf.as_mut_ptr(), MSG_NODE_SIZE) };
        let msg_type = rd_i32(&node_buf, MSG_NODE_TYPE_OFF);
        let payload = rd_ptr(&node_buf, MSG_NODE_PAYLOAD_OFF);
        let next = rd_ptr(&node_buf, MSG_NODE_NEXT_OFF);
        if msg_type == FCB_NODE_CAN_MESSAGE && payload != 0 {
            // SAFETY: payload points to the 22-byte CAN message structure
            // attached to the node.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    payload as *const u8,
                    msg_buf.as_mut_ptr(),
                    CAN_MSG_SIZE,
                )
            };
            let msg = parse_can_msg(&msg_buf);
            if let Some((id, dlc)) = accept_can_msg(&msg, channel) {
                let frame = CanFrame::with_len(
                    bus_id,
                    id,
                    msg.data.to_vec(),
                    dlc as usize,
                    false,
                    FrameType::CAN20B,
                )?;
                queue.push_back(frame);
            }
        }
        node = next as *mut u8;
    }
    Ok(())
}

/// Function pointer table for fcbase.dll (loaded and fully validated at
/// construction; a missing symbol is reported immediately instead of failing
/// at first use).
/// All functions return an fcb status code (i32).
#[derive(Debug)]
struct Fcbase {
    /// Keeps the library handle alive ([`DllWrapper`] from autors-native);
    /// never accessed directly.
    _dll: DllWrapper,
    /// fcbGetEnumFlexCardsV3(list: *mut *mut c_void, flags: u8): enumerates
    /// the FlexCard interface list (always called with flags=1 here).
    fcb_get_enum_flex_cards_v3: unsafe extern "system" fn(*mut *mut c_void, u8) -> i32,
    /// fcFreeMemory(type: i32, ptr: *mut c_void): releases a driver-allocated
    /// list (type 2 = message list, 4 = interface list).
    fc_free_memory: unsafe extern "system" fn(i32, *mut c_void) -> i32,
    /// fcbOpen(handle: *mut *mut c_void, flexCardId: u32).
    fcb_open: unsafe extern "system" fn(*mut *mut c_void, u32) -> i32,
    /// fcbClose(handle: *mut c_void).
    fcb_close: unsafe extern "system" fn(*mut c_void) -> i32,
    /// fcbCANMonitoringStart(handle, channel, enable, flags) (always called
    /// with enable=1, flags=0 here).
    fcb_can_monitoring_start: unsafe extern "system" fn(*mut c_void, i32, u8, i32) -> i32,
    /// fcbCANMonitoringStop(handle, channel).
    fcb_can_monitoring_stop: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    /// fcbCANTransmit(handle, channel, id, data, dlc, flags) (always called
    /// with id=0, dlc=1, flags=0 here).
    fcb_can_transmit: unsafe extern "system" fn(*mut c_void, i32, u32, *const u8, u8, u8) -> i32,
    /// fcbReceive(handle, list: *mut *mut c_void).
    fcb_receive: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    /// fcbCANGetCcState(handle, channel, state: *mut i32) (the out value is
    /// discarded; only the status code is checked).
    fcb_can_get_cc_state: unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> i32,
    /// fcbCANSetCcConfiguration(handle, channel, config) (36-byte struct by
    /// value).
    fcb_can_set_cc_configuration: unsafe extern "system" fn(*mut c_void, i32, FcbCcConfig) -> i32,
}

impl Fcbase {
    /// Loads fcbase.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(FCBASE_DLL)
    }

    /// Loads from the given path/name and resolves all exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe DllMain execution) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken and copied into raw function pointers within this
        // function; the library handle and the function pointers live in the
        // same struct, keeping the pointers valid. The generic T is a
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
            fcb_get_enum_flex_cards_v3: sym!(
                b"fcbGetEnumFlexCardsV3\0",
                unsafe extern "system" fn(*mut *mut c_void, u8) -> i32
            ),
            fc_free_memory: sym!(
                b"fcFreeMemory\0",
                unsafe extern "system" fn(i32, *mut c_void) -> i32
            ),
            fcb_open: sym!(
                b"fcbOpen\0",
                unsafe extern "system" fn(*mut *mut c_void, u32) -> i32
            ),
            fcb_close: sym!(b"fcbClose\0", unsafe extern "system" fn(*mut c_void) -> i32),
            fcb_can_monitoring_start: sym!(
                b"fcbCANMonitoringStart\0",
                unsafe extern "system" fn(*mut c_void, i32, u8, i32) -> i32
            ),
            fcb_can_monitoring_stop: sym!(
                b"fcbCANMonitoringStop\0",
                unsafe extern "system" fn(*mut c_void, i32) -> i32
            ),
            fcb_can_transmit: sym!(
                b"fcbCANTransmit\0",
                unsafe extern "system" fn(*mut c_void, i32, u32, *const u8, u8, u8) -> i32
            ),
            fcb_receive: sym!(
                b"fcbReceive\0",
                unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32
            ),
            fcb_can_get_cc_state: sym!(
                b"fcbCANGetCcState\0",
                unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> i32
            ),
            fcb_can_set_cc_configuration: sym!(
                b"fcbCANSetCcConfiguration\0",
                unsafe extern "system" fn(*mut c_void, i32, FcbCcConfig) -> i32
            ),
            _dll: dll,
        })
    }

    /// Interface enumeration: fetches the list with flags=1, selects the
    /// first usable FlexCard ID, and always calls `fcFreeMemory(4, list)`
    /// afterwards. Returns 0 when no card is present or enumeration fails.
    fn first_flex_card_id(&self) -> u32 {
        let mut list: *mut c_void = std::ptr::null_mut();
        // SAFETY: list is a stack out-variable; the function pointer comes
        // from the loaded fcbase.dll.
        let st = unsafe { (self.fcb_get_enum_flex_cards_v3)(&mut list, 1) };
        // On a non-OK status or an empty list, do not walk the list (the card
        // ID stays 0).
        let card_id = if st == FCB_OK && !list.is_null() {
            // SAFETY: list is a valid interface list just returned by the
            // driver; it is released right below.
            unsafe { first_card_id_in_list(list) }
        } else {
            0
        };
        if !list.is_null() {
            // SAFETY: list was allocated by fcbGetEnumFlexCardsV3; type 4 =
            // interface list.
            unsafe { (self.fc_free_memory)(FCB_FREE_INTERFACE_LIST, list) };
        }
        card_id
    }
}

/// FlexCard CAN channel adapter.
/// Reception is pull-based per the non-blocking contract of
/// [`CanDevice::receive`]: each call fetches one `fcbReceive` list and
/// drains it (blocking waits are handled by the polling cadence of
/// [`crate::device::start_dispatch`] above).
pub struct EbElCan {
    core: DeviceCore,
    api: Fcbase,
    /// FlexCard ID (enumeration result at construction; 0 = no usable card).
    card_id: u32,
    /// fcbOpen connection handle (usize to stay Send; 0 = not open).
    handle: usize,
    /// Channel number.
    channel: i32,
    /// Open-success flag (set after the open sequence succeeds, cleared by
    /// close).
    opened: bool,
    /// Bus ID (taken from the configuration or generated at open time, e.g.
    /// "EbEl/CAN1").
    bus_id: String,
    /// Receive frame buffer.
    rx_queue: VecDeque<CanFrame>,
}

impl EbElCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle != 0 {
            // fcbCANMonitoringStop + fcbClose (both return values are
            // intentionally ignored), then clear the handle; card_id is kept
            // so the channel can be reopened.
            // SAFETY: handle is valid.
            unsafe {
                (self.api.fcb_can_monitoring_stop)(self.handle_ptr(), self.channel);
                (self.api.fcb_close)(self.handle_ptr());
            }
            self.handle = 0;
        }
        self.opened = false;
    }

    /// Loads fcbase.dll, validates the symbol table, and enumerates the first
    /// usable FlexCard; returns [`Error::Driver`] when the driver is not
    /// installed or exports are missing. With no card, `flex_card_id() == 0`
    /// and a subsequent `open` returns `Ok(false)`.
    pub fn new() -> Result<Self> {
        let api = Fcbase::load()?;
        let card_id = api.first_flex_card_id();
        Ok(Self {
            core: DeviceCore::new(),
            api,
            card_id,
            handle: 0,
            channel: 0,
            opened: false,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.opened
    }

    /// The FlexCard ID enumerated at construction (0 = no usable card).
    pub fn flex_card_id(&self) -> u32 {
        self.card_id
    }

    /// Raw pointer form of the `handle` field (only used while this struct
    /// guarantees the handle is valid).
    fn handle_ptr(&self) -> *mut c_void {
        self.handle as *mut c_void
    }
}

#[async_trait]
impl CanDevice for EbElCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // `open` performs the full open sequence; `is_available` only reports
        // the current state (the same split as in kvaser.rs).
        Ok(self.opened)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        // Generate a default BusId ("EbEl/CAN1") when the configuration does
        // not provide one.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("EbEl", config.channel));
        // No card found at construction: open reports failure without
        // touching the driver.
        if self.card_id == 0 {
            return Ok(false);
        }
        // No guard against opening twice: a second fcbOpen on an open channel
        // fails in the driver with CONNECTION_ALREADY_OPEN, which `check`
        // maps to an error.
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: handle is a stack out-variable; card_id comes from a
        // successful enumeration.
        let st = unsafe { (self.api.fcb_open)(&mut handle, self.card_id) };
        check(st, &self.bus_id, "fcbOpen")?;
        self.handle = handle as usize;
        self.channel = config.channel;
        // Baudrate != NotSet: push the production configuration; then start
        // monitoring and query the CC state. CAN FD parameters are not
        // handled at all (classic CAN device). If configuration fails, the
        // connection is intentionally left open — the handle is kept and
        // cleanup falls to close()/Drop.
        if config.baudrate != CanBaudrate::NotSet {
            let cfg = cc_config_for_baudrate(config.baudrate);
            // SAFETY: handle is valid; cfg is passed by value (36 bytes,
            // Pack=1 layout).
            let st = unsafe {
                (self.api.fcb_can_set_cc_configuration)(self.handle_ptr(), self.channel, cfg)
            };
            check(st, &self.bus_id, "fcbCANSetCcConfiguration")?;
        }
        // SAFETY: handle is valid; arguments by value (enable=1, flags=0).
        let st =
            unsafe { (self.api.fcb_can_monitoring_start)(self.handle_ptr(), self.channel, 1, 0) };
        check(st, &self.bus_id, "fcbCANMonitoringStart")?;
        let mut state: i32 = 0;
        // SAFETY: handle is valid; state is a stack out-variable (its value
        // is discarded).
        let st =
            unsafe { (self.api.fcb_can_get_cc_state)(self.handle_ptr(), self.channel, &mut state) };
        check(st, &self.bus_id, "fcbCANGetCcState")?;
        self.opened = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], _frame_type: FrameType) -> Result<usize> {
        // Not open: return 0 without calling the driver.
        if !self.opened {
            return Ok(0);
        }
        // fcbCANTransmit(handle, channel, 0u, data, 1, 0) — the canID is
        // intentionally discarded and the call always uses 0/1/0 (an
        // intentional quirk kept as part of the behavioral contract; see the
        // module docs). Data is passed through an 8-byte buffer (matching the
        // data capacity of the driver message structure), avoiding a dangling
        // pointer for empty slices and out-of-bounds reads for oversized
        // slices.
        let mut buf = [0u8; 8];
        let n = data.len().min(8);
        buf[..n].copy_from_slice(&data[..n]);
        // SAFETY: handle is valid; buf stays valid for the call; the driver
        // sends synchronously and does not retain the pointer.
        let st = unsafe {
            (self.api.fcb_can_transmit)(self.handle_ptr(), self.channel, 0, buf.as_ptr(), 1, 0)
        };
        // Non-OK status: return 0 (no error is raised).
        if st != FCB_OK {
            return Ok(0);
        }
        // Record statistics and return data.len(); the statistics frame is
        // always CAN20B, even when the FrameType argument is something else.
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, FrameType::CAN20B);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        // Not open: clear the queue and return None.
        if !self.opened {
            self.rx_queue.clear();
            return Ok(None);
        }
        let mut list: *mut c_void = std::ptr::null_mut();
        // SAFETY: handle is valid; list is a stack out-variable.
        let st = unsafe { (self.api.fcb_receive)(self.handle_ptr(), &mut list) };
        if st == FCB_OK && !list.is_null() {
            // Collect first, then release, so fcFreeMemory runs even on a
            // parse-error path.
            // SAFETY: list is a valid message list just returned by the
            // driver; it is released right below.
            let collected =
                unsafe { collect_msg_frames(list, self.channel, &self.bus_id, &mut self.rx_queue) };
            // SAFETY: list was allocated by fcbReceive; type 2 = message list.
            unsafe { (self.api.fc_free_memory)(FCB_FREE_MESSAGE_LIST, list) };
            collected?;
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for EbElCan {
    fn drop(&mut self) {
        // Stop monitoring and close the connection; the library handle is
        // released by DllWrapper's Drop.
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: writes a little-endian u32 into a byte buffer.
    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Test helper: writes a little-endian i32 into a byte buffer.
    fn put_i32(buf: &mut [u8], off: usize, v: i32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Test helper: writes a pointer-width value into a byte buffer.
    fn put_ptr(buf: &mut [u8], off: usize, v: usize) {
        buf[off..off + PTR_SIZE].copy_from_slice(&v.to_le_bytes()[..PTR_SIZE]);
    }

    #[test]
    fn cc_config_layout_is_36_bytes_pack1() {
        assert_eq!(std::mem::size_of::<FcbCcConfig>(), 36);
        // 500k: prescaler=3, tseg1=1, tseg2=4, sjw=1, rest 0 — verify the
        // Pack=1 layout byte by byte (4×u16 LE + u32 + 6×u32).
        let cfg = cc_config_for_baudrate(CanBaudrate::B500Kbit);
        // SAFETY: FcbCcConfig is a repr(C) POD (integers only), so reading it
        // as bytes is sound.
        let bytes: [u8; 36] = unsafe { std::mem::transmute(cfg) };
        let mut expect = [0u8; 36];
        expect[0] = 3;
        expect[2] = 1;
        expect[4] = 4;
        expect[6] = 1;
        assert_eq!(bytes, expect);
    }

    #[test]
    fn cc_config_table_matches_expected_contract() {
        // The full baud-rate table.
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B1Mbit).prescaler, 1);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B500Kbit).prescaler, 3);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B250Kbit).prescaler, 7);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B125Kbit).prescaler, 15);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B100Kbit).prescaler, 19);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B50Kbit).prescaler, 39);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B20Kbit).prescaler, 99);
        assert_eq!(cc_config_for_baudrate(CanBaudrate::B10Kbit).prescaler, 199);
        // 800k rewrites the whole row to {4, 0, 1, 0}.
        let cfg = cc_config_for_baudrate(CanBaudrate::B800Kbit);
        assert_eq!((cfg.prescaler, cfg.tseg1, cfg.tseg2, cfg.sjw), (4, 0, 1, 0));
        // Regular rows only change prescaler; the rest keep the defaults
        // {1, 4, 1}.
        let cfg = cc_config_for_baudrate(CanBaudrate::B500Kbit);
        assert_eq!(
            (cfg.tseg1, cfg.tseg2, cfg.sjw, cfg.clock, cfg.reserved),
            (1, 4, 1, 0, [0; 6])
        );
        // Baud rates without a table entry (NotSet / 2M etc.): keep the
        // defaults {0, 1, 4, 1}.
        for b in [
            CanBaudrate::NotSet,
            CanBaudrate::B2Mbit,
            CanBaudrate::B8Mbit,
        ] {
            let cfg = cc_config_for_baudrate(b);
            assert_eq!((cfg.prescaler, cfg.tseg1, cfg.tseg2, cfg.sjw), (0, 1, 4, 1));
        }
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(0), "OK");
        assert_eq!(status_name(0x63), "INTERNAL_ERROR");
        assert_eq!(status_name(0x6E), "CONNECTION_ALREADY_OPEN");
        assert_eq!(status_name(0x97), "TX_FIFO_MSGBUF_NR");
        assert_eq!(status_name(1), "FCB_???");
        assert_eq!(status_name(-1), "FCB_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        let err = check(0x6E, "EbEl/CAN1", "fcbOpen").unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("EbEl/CAN1"));
                assert!(msg.contains("fcbOpen"));
                assert!(msg.contains("CONNECTION_ALREADY_OPEN"));
                assert!(msg.contains("110"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn layout_constants_match_pack1() {
        // Message list node: i32 + 2 pointers.
        assert_eq!(MSG_NODE_TYPE_OFF, 0);
        assert_eq!(MSG_NODE_PAYLOAD_OFF, 4);
        assert_eq!(MSG_NODE_NEXT_OFF, 4 + PTR_SIZE);
        assert_eq!(MSG_NODE_SIZE, 4 + 2 * PTR_SIZE);
        // CAN message: fixed 22 bytes.
        assert_eq!(CAN_MSG_SIZE, 22);
        assert_eq!(CAN_MSG_ID_OFF, 0);
        assert_eq!(CAN_MSG_DLC_FLAGS_OFF, 9);
        assert_eq!(CAN_MSG_CHANNEL_OFF, 10);
        assert_eq!(CAN_MSG_DATA_OFF, 14);
        // Interface node: fixed offsets (before the first pointer field) and
        // a width-dependent next pointer.
        assert_eq!(IFACE_CARD_ID_OFF, 0);
        assert_eq!(IFACE_STATE_OFF, 68);
        assert_eq!(IFACE_CAN_COUNT_OFF, 105);
        assert_eq!(IFACE_NEXT_OFF, 164 + PTR_SIZE);
        assert_eq!(IFACE_NODE_SIZE, 164 + 2 * PTR_SIZE);
        if PTR_SIZE == 8 {
            assert_eq!(MSG_NODE_NEXT_OFF, 12);
            assert_eq!(MSG_NODE_SIZE, 20);
            assert_eq!(IFACE_NEXT_OFF, 172);
            assert_eq!(IFACE_NODE_SIZE, 180);
        } else {
            assert_eq!(MSG_NODE_NEXT_OFF, 8);
            assert_eq!(MSG_NODE_SIZE, 12);
            assert_eq!(IFACE_NEXT_OFF, 168);
            assert_eq!(IFACE_NODE_SIZE, 172);
        }
    }

    /// Builds a 22-byte CAN message byte string.
    fn make_msg(id: u32, dlc_flags: u8, channel: i32, data: [u8; 8]) -> [u8; CAN_MSG_SIZE] {
        let mut buf = [0u8; CAN_MSG_SIZE];
        put_u32(&mut buf, CAN_MSG_ID_OFF, id);
        buf[CAN_MSG_DLC_FLAGS_OFF] = dlc_flags;
        put_i32(&mut buf, CAN_MSG_CHANNEL_OFF, channel);
        buf[CAN_MSG_DATA_OFF..CAN_MSG_DATA_OFF + 8].copy_from_slice(&data);
        buf
    }

    #[test]
    fn can_msg_parse_and_accept_filter() {
        let msg = parse_can_msg(&make_msg(0x123, 8, 1, [1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(msg.id, 0x123);
        assert_eq!(msg.dlc_flags, 8);
        assert_eq!(msg.channel, 1);
        // Matching channel + standard frame.
        assert_eq!(accept_can_msg(&msg, 1), Some((0x123, 8)));
        // Non-matching channel.
        assert_eq!(accept_can_msg(&msg, 0), None);
        // Extended frame: bit 29 → CAN_EXT_FLAG, ID mask 0x1FFFFFFF.
        let ext = parse_can_msg(&make_msg(0x2000_0123, 8, 1, [0u8; 8]));
        assert_eq!(accept_can_msg(&ext, 1), Some((0x8000_0123, 8)));
        let max = parse_can_msg(&make_msg(0x3FFF_FFFF, 8, 1, [0u8; 8]));
        assert_eq!(accept_can_msg(&max, 1), Some((0x9FFF_FFFF, 8)));
        // DLC = 0 / bit4 / bit5 are discarded.
        assert_eq!(
            accept_can_msg(&parse_can_msg(&make_msg(0x123, 0, 1, [0u8; 8])), 1),
            None
        );
        assert_eq!(
            accept_can_msg(&parse_can_msg(&make_msg(0x123, 0x18, 1, [0u8; 8])), 1),
            None
        );
        assert_eq!(
            accept_can_msg(&parse_can_msg(&make_msg(0x123, 0x28, 1, [0u8; 8])), 1),
            None
        );
        // DLC takes the low 4 bits (0x48 → 8; bit 6 has no effect).
        assert_eq!(
            accept_can_msg(&parse_can_msg(&make_msg(0x123, 0x48, 1, [0u8; 8])), 1),
            Some((0x123, 8))
        );
    }

    /// Fakes a two-node interface list on a heap buffer to verify traversal
    /// and card selection.
    #[test]
    fn iface_list_walk_selects_first_matching_card() {
        let mut mem = vec![0u8; 2 * IFACE_NODE_SIZE];
        let base = mem.as_mut_ptr() as usize;
        let node2 = base + IFACE_NODE_SIZE;
        // Node 1: card ID 0 (invalid), next → node 2.
        put_u32(&mut mem, IFACE_CARD_ID_OFF, 0);
        put_i32(&mut mem, IFACE_STATE_OFF, 1);
        mem[IFACE_CAN_COUNT_OFF] = 1;
        put_ptr(&mut mem, IFACE_NEXT_OFF, node2);
        // Node 2: card ID 7, status 1, CAN channels 2, next = 0.
        put_u32(&mut mem, IFACE_NODE_SIZE + IFACE_CARD_ID_OFF, 7);
        put_i32(&mut mem, IFACE_NODE_SIZE + IFACE_STATE_OFF, 1);
        mem[IFACE_NODE_SIZE + IFACE_CAN_COUNT_OFF] = 2;
        put_ptr(&mut mem, IFACE_NODE_SIZE + IFACE_NEXT_OFF, 0);
        // SAFETY: mem stays alive for the call; node layout and pointers are
        // faked per Pack=1.
        let id = unsafe { first_card_id_in_list(base as *mut c_void) };
        assert_eq!(id, 7);

        // First node matches: returns immediately without looking further.
        put_u32(&mut mem, IFACE_CARD_ID_OFF, 3);
        // SAFETY: same as above.
        let id = unsafe { first_card_id_in_list(base as *mut c_void) };
        assert_eq!(id, 3);

        // Status word 0 / CAN channel count 0: neither matches → 0.
        put_u32(&mut mem, IFACE_CARD_ID_OFF, 3);
        put_i32(&mut mem, IFACE_STATE_OFF, 0);
        put_i32(&mut mem, IFACE_NODE_SIZE + IFACE_STATE_OFF, 0);
        // SAFETY: same as above.
        let id = unsafe { first_card_id_in_list(base as *mut c_void) };
        assert_eq!(id, 0);
        put_i32(&mut mem, IFACE_STATE_OFF, 1);
        put_i32(&mut mem, IFACE_NODE_SIZE + IFACE_STATE_OFF, 1);
        mem[IFACE_CAN_COUNT_OFF] = 0;
        mem[IFACE_NODE_SIZE + IFACE_CAN_COUNT_OFF] = 0;
        // SAFETY: same as above.
        let id = unsafe { first_card_id_in_list(base as *mut c_void) };
        assert_eq!(id, 0);

        // Empty list.
        // SAFETY: a null pointer is valid input (returns 0 immediately).
        let id = unsafe { first_card_id_in_list(std::ptr::null_mut()) };
        assert_eq!(id, 0);
    }

    /// Fakes a three-node message list on a heap buffer to verify traversal,
    /// filtering, and enqueueing.
    #[test]
    fn msg_list_walk_collects_and_filters_frames() {
        // Layout: node1@0 (type 5, skipped) → node2@256 (valid CAN message) →
        // node3@512 (bit5 set, skipped) → end; msg@1024 / msg@1152.
        let mut mem = vec![0u8; 2048];
        let base = mem.as_mut_ptr() as usize;
        let (n2, n3, m1, m2) = (base + 256, base + 512, base + 1024, base + 1152);
        put_i32(&mut mem, MSG_NODE_TYPE_OFF, 5);
        put_ptr(&mut mem, MSG_NODE_PAYLOAD_OFF, m1); // Non-CAN node: payload must be ignored
        put_ptr(&mut mem, MSG_NODE_NEXT_OFF, n2);
        put_i32(&mut mem, 256 + MSG_NODE_TYPE_OFF, FCB_NODE_CAN_MESSAGE);
        put_ptr(&mut mem, 256 + MSG_NODE_PAYLOAD_OFF, m1);
        put_ptr(&mut mem, 256 + MSG_NODE_NEXT_OFF, n3);
        put_i32(&mut mem, 512 + MSG_NODE_TYPE_OFF, FCB_NODE_CAN_MESSAGE);
        put_ptr(&mut mem, 512 + MSG_NODE_PAYLOAD_OFF, m2);
        put_ptr(&mut mem, 512 + MSG_NODE_NEXT_OFF, 0);
        // msg1: standard frame 0x456, DLC 3, channel 1, data AA BB CC.
        let msg1 = make_msg(0x456, 3, 1, [0xAA, 0xBB, 0xCC, 0, 0, 0, 0, 0]);
        mem[1024..1024 + CAN_MSG_SIZE].copy_from_slice(&msg1);
        // msg2: bit5 set → discarded.
        let msg2 = make_msg(0x777, 0x28, 1, [0u8; 8]);
        mem[1152..1152 + CAN_MSG_SIZE].copy_from_slice(&msg2);

        let mut queue = VecDeque::new();
        // SAFETY: mem stays alive for the call; layout is faked per Pack=1.
        unsafe { collect_msg_frames(base as *mut c_void, 1, "EbEl/CAN1", &mut queue).unwrap() };
        assert_eq!(queue.len(), 1);
        let f = &queue[0];
        assert_eq!(f.id, 0x456);
        assert_eq!(f.data, vec![0xAA, 0xBB, 0xCC]); // Truncated to DLC
        assert_eq!(f.bus_id, "EbEl/CAN1");
        assert!(!f.is_master_frame);
        assert_eq!(f.frame_type, FrameType::CAN20B);

        // Non-matching channel: empty queue.
        let mut queue = VecDeque::new();
        // SAFETY: same as above.
        unsafe { collect_msg_frames(base as *mut c_void, 0, "B", &mut queue).unwrap() };
        assert!(queue.is_empty());
    }

    /// DLC > 8 cannot be represented in a frame and maps to Error::Invalid.
    #[test]
    fn msg_list_dlc_over_8_is_invalid_error() {
        let mut mem = vec![0u8; 1024];
        let base = mem.as_mut_ptr() as usize;
        put_i32(&mut mem, MSG_NODE_TYPE_OFF, FCB_NODE_CAN_MESSAGE);
        put_ptr(&mut mem, MSG_NODE_PAYLOAD_OFF, base + 512);
        put_ptr(&mut mem, MSG_NODE_NEXT_OFF, 0);
        let msg = make_msg(0x123, 9, 0, [0u8; 8]);
        mem[512..512 + CAN_MSG_SIZE].copy_from_slice(&msg);
        let mut queue = VecDeque::new();
        // SAFETY: same as above.
        let err =
            unsafe { collect_msg_frames(base as *mut c_void, 0, "B", &mut queue) }.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "got: {err:?}");
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that certainly does not exist: must yield Error::Driver,
        // not a panic.
        let err = Fcbase::load_from("no_such_fcbase_driver_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // Without the Eberspächer driver installed: Error::Driver (fcbase.dll
        // is missing); on a machine with the driver: construction should
        // succeed with a complete symbol table. Both outcomes are acceptable —
        // the key point is that it must not panic.
        match EbElCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                // While not open, send returns 0 and receive returns None.
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x123, &[1, 2, 3], FrameType::CAN20B))
                        .unwrap(),
                    0
                );
                assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
                // With no card, open returns false.
                if dev.flex_card_id() == 0 {
                    assert!(
                        !autors_runtime::block_on(dev.open(CanConfiguration::default())).unwrap()
                    );
                }
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
