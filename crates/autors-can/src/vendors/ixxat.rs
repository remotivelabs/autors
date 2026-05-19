//! IXXAT VCI (Virtual Communication Interface) CAN driver adapter.
//! Gated on `#[cfg(windows)]` and the `vendor-ixxat` feature (see the parent
//! module). The adapter dynamically loads the IXXAT VCI native library at
//! runtime:
//! - `vcinpl.dll` (VCI 3.x, classic CAN only);
//! - `vcinpl2.dll` (VCI 4.x, CAN FD capable).
//!
//! Library selection follows a preference order (classic prefers `vcinpl.dll`,
//! FD prefers `vcinpl2.dll`), falling back to the other DLL. The `LoadLibrary`
//! search order already covers System32, so no `File.Exists` pre-check is
//! performed. If neither DLL is installed or an export is missing,
//! construction fails with `Error::Driver`.
//! VCI 3 and VCI 4 use different signatures and wire structures for channel
//! initialization, message IO, and controller status. The adapter selects the
//! matching function table and layouts from the loaded library generation.
//!
//! Entry points: [`IxxatCan::new`] (classic CAN) and [`IxxatCan::new_can_fd`]
//! (CAN FD).

use std::collections::VecDeque;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, dlc_to_length, length_to_dlc, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{
    BitRateConfig, CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType,
};

/// VCI 3.x native library (classic CAN).
const VCI3_DLL: &str = "vcinpl.dll";
/// VCI 4.x native library (CAN FD).
const VCI4_DLL: &str = "vcinpl2.dll";

/// VCI native call handle (passed by machine word; usize keeps the adapter
/// Send, the same consideration as the Kvaser adapter with its i32 handles).
type Handle = usize;
/// VCI HRESULT (OK = 0).
type HResult = u32;

// ---- VCI error codes (consistent with vciError.h) ----
/// VCI_OK.
const VCI_OK: HResult = 0;
/// VCI_E_TIMEOUT (receive wait timed out).
#[allow(dead_code)] // Table completeness: the wait-timeout return code; any non-OK is currently treated as "no frame".
const VCI_E_TIMEOUT: HResult = 0xE001_000B;
/// VCI_E_RXQUEUE_EMPTY (receive queue empty; returned by canChannelReadMessage when no frame is pending).
#[allow(dead_code)] // Semantic completeness: the read-empty return code; any non-OK currently stops the read loop.
const VCI_E_RXQUEUE_EMPTY: HResult = 0xE001_0012;
/// VCI_E_NO_MORE_ITEMS (end of enumeration).
#[allow(dead_code)]
const VCI_E_NO_MORE_ITEMS: HResult = 0xE001_000F;

// ---- CAN message type bytes (the 4 msgType bytes of the VCI message structs) ----
/// msgType[0]: CAN_MSGTYPE_DATA (data frame; only 0 is accepted on both TX and RX).
const CAN_MSGTYPE_DATA: u8 = 0;
/// msgType[1]: FD frame flag (TX: FrameType::FD -> 4, FD|BRS -> 12; RX: tested with &4/&8).
const CAN_MSGFLAGS_FD: u8 = 4;
/// msgType[1]: FD BRS flag.
const CAN_MSGFLAGS_BRS: u8 = 8;
/// msgType[2]: mask for the low 4 DLC bits.
const CAN_DLC_MASK: u8 = 0x0F;
/// msgType[2]: RTR bit (frames with this bit set are skipped on receive).
const CAN_DLC_RTR: u8 = 0x20;
/// msgType[2]: extended-frame bit (the on-wire counterpart of the internal CAN_EXT_FLAG).
const CAN_DLC_EXT: u8 = 0x80;

// ---- Channel/controller constants ----
/// Receive queue length.
const RX_QUEUE_SIZE: u16 = 1024;
/// Receive threshold.
const RX_THRESHOLD: u16 = 1;
/// Transmit queue length.
const TX_QUEUE_SIZE: u16 = 128;
/// Transmit threshold.
const TX_THRESHOLD: u16 = 1;
/// CAN operating mode (standard + extended frames).
const CAN_OPMODE: u8 = 3;
/// CAN FD operating-mode extension (OR in 4 for non-ISO).
const CAN_OPMODE_EX_FD: u8 = 3;
/// Additional non-ISO FD bit.
const CAN_OPMODE_EX_NONISO: u8 = 4;
/// Filter argument of VCI4 canChannelInitialize (a byte enum, passed by word on x64).
const VCI4_CHN_INIT_FILTER: u32 = 2;

/// Which VCI library was loaded, determined from the load path
/// (`vcinpl2.dll` -> V4, otherwise V3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VciLib {
    /// vcinpl.dll (VCI 3.x, classic CAN only).
    V3,
    /// vcinpl2.dll (VCI 4.x, CAN FD).
    V4,
}

/// HRESULT name (used in error messages).
fn vci_error_name(hr: HResult) -> &'static str {
    match hr {
        0 => "VCI_OK",
        0xE001_0001 => "VCI_E_UNEXPECTED",
        0xE001_0002 => "VCI_E_NOT_IMPLEMENTED",
        0xE001_0003 => "VCI_E_OUTOFMEMORY",
        0xE001_0004 => "VCI_E_INVALIDARG",
        0xE001_0005 => "VCI_E_NOINTERFACE",
        0xE001_0006 => "VCI_E_INVPOINTER",
        0xE001_0007 => "VCI_E_INVHANDLE",
        0xE001_0008 => "VCI_E_ABORT",
        0xE001_0009 => "VCI_E_FAIL",
        0xE001_000A => "VCI_E_ACCESSDENIED",
        0xE001_000B => "VCI_E_TIMEOUT",
        0xE001_000C => "VCI_E_BUSY",
        0xE001_000D => "VCI_E_PENDING",
        0xE001_000E => "VCI_E_NO_DATA",
        0xE001_000F => "VCI_E_NO_MORE_ITEMS",
        0xE001_0010 => "VCI_E_NOT_INITIALIZED",
        0xE001_0011 => "VCI_E_ALREADY_INITIALIZED",
        0xE001_0012 => "VCI_E_RXQUEUE_EMPTY",
        0xE001_0013 => "VCI_E_TXQUEUE_FULL",
        0xE001_0014 => "VCI_E_BUFFER_OVERFLOW",
        0xE001_0015 => "VCI_E_INVALID_STATE",
        0xE001_0016 => "VCI_E_OBJECT_ALREADY_EXISTS",
        0xE001_0017 => "VCI_E_INVALID_INDEX",
        0xE001_0018 => "VCI_E_END_OF_FILE",
        0xE001_0019 => "VCI_E_DISCONNECTED",
        0xE001_001A => "VCI_E_WRONG_FLASHFWVERSION",
        0xE001_001B => "VCI_E_INVALID_LICENSE",
        0xE001_001C => "VCI_E_NO_SUCH_LICENSE",
        0xE001_001D => "VCI_E_LICENSE_EXPIRED",
        0xE001_001E => "VCI_E_LICENSE_QUOTA_EXCEEDED",
        0xE001_001F => "VCI_E_INVALID_TIMING",
        0xE001_0020 => "VCI_E_IN_USE",
        0xE001_0021 => "VCI_E_NO_SUCH_DEVICE",
        0xE001_0022 => "VCI_E_DEVICE_NOT_CONNECTED",
        0xE001_0023 => "VCI_E_DEVICE_NOT_READY",
        0xE001_0024 => "VCI_E_TYPE_MISMATCH",
        0xE001_0025 => "VCI_E_NOT_SUPPORTED",
        0xE001_0026 => "VCI_E_DUPLICATE_OBJECTID",
        0xE001_0027 => "VCI_E_OBJECTID_NOT_FOUND",
        0xE001_0028 => "VCI_E_WRONG_LEVEL",
        0xE001_0029 => "VCI_E_WRONG_DRV_VERSION",
        0xE001_002A => "VCI_E_LUIDS_EXHAUSTED",
        _ => "VCI_E_???",
    }
}

/// Maps an HRESULT != VCI_OK to [`Error::Driver`], folding the bus ID into the
/// message via [`config_err`].
fn check(hr: HResult, bus_id: &str, what: &str) -> Result<()> {
    if hr == VCI_OK {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!("vcinpl: {what} failed: {} (0x{hr:08X})", vci_error_name(hr)),
        ))
    }
}

// ---- VCI structs (#[repr(C)]; the field order keeps natural alignment
// identical to Pack = 1: all byte/array fields, no padding differences. Large
// arrays have no std Default impl, so the affected types implement Default
// manually) ----

/// VCI3 classic CAN message (20 bytes: dwTime, dwMsgId, uMsgType[4], data[8]).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CanMsg {
    /// Timestamp (set to 0 when sending).
    dw_time: u32,
    /// CAN ID (stored as `id & 0x7FFFFFFF`; the extended flag lives in msg_type[2]).
    dw_msg_id: u32,
    /// 4 type bytes: [0]=message type, [1]=FD flags, [2]=DLC|RTR|EXT, [3]=reserved.
    msg_type: [u8; 4],
    /// 8 data bytes.
    data: [u8; 8],
}

/// VCI4 CAN FD message (80 bytes: dwTime, reserved, dwMsgId, uMsgType[4],
/// data[64]).
#[repr(C)]
#[derive(Clone, Copy)]
struct CanFdMsg {
    dw_time: u32,
    /// Reserved, always 0.
    reserved: u32,
    dw_msg_id: u32,
    msg_type: [u8; 4],
    data: [u8; 64],
}

impl Default for CanFdMsg {
    fn default() -> Self {
        Self {
            dw_time: 0,
            reserved: 0,
            dw_msg_id: 0,
            msg_type: [0; 4],
            data: [0; 64],
        }
    }
}

/// CAN (FD) bit-timing register set (16 bytes). For classic configurations
/// tseg1/tseg2/sjw come from the literal table; FD presets are looked up via
/// `fd_btr_lookup`; custom FD configurations map `Brp->bitrate, TSeg1->tseg1,
/// TSeg2->tseg2, Sjw->sjw` (writing Brp into the bitrate field is intentional).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct CanBtr {
    /// Flags (no path sets any bit; always 0).
    flags: u32,
    /// Baud rate in Hz (holds Brp for custom FD configurations, see above).
    bitrate: u32,
    tseg1: u16,
    tseg2: u16,
    sjw: u16,
    /// Only used by the FD preset table; 0 on the classic/custom paths.
    f3: u16,
}

/// VCI3 controller status (8 bytes; read and discarded during open).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CanLineStatus {
    op_mode: u8,
    b0: u8,
    b1: u8,
    b2: u8,
    status: u32,
}

/// VCI4 controller status (40 bytes; read and discarded during open).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CanFdStatus {
    op_mode: u8,
    op_mode_ex: u8,
    b0: u8,
    b1: u8,
    nominal: CanBtr,
    data: CanBtr,
    status: u32,
}

/// VCIDEVICEINFO (304 bytes). Only luid (device unique ID) and unique_hw_id
/// (description GUID, used in BusId) take part in the logic; the roles of the
/// intermediate fields follow the layout of the public VCI header's
/// VCIDEVICEINFO and are kept as opaque placeholders.
#[repr(C)]
#[derive(Clone, Copy)]
struct VciDeviceInfo {
    /// Device LUID (the key for vciDeviceOpen).
    vci_luid: i64,
    guid0: [u8; 16],
    b0: u8,
    b1: u8,
    s0: u16,
    b2: u8,
    b3: u8,
    b4: u8,
    b5: u8,
    /// Unique hardware GUID (formatted into the device description string).
    unique_hw_id: [u8; 16],
    description: [u8; 128],
    manufacturer: [u8; 126],
    s1: u16,
}

impl Default for VciDeviceInfo {
    fn default() -> Self {
        Self {
            vci_luid: 0,
            guid0: [0; 16],
            b0: 0,
            b1: 0,
            s0: 0,
            b2: 0,
            b3: 0,
            b4: 0,
            b5: 0,
            unique_hw_id: [0; 16],
            description: [0; 128],
            manufacturer: [0; 126],
            s1: 0,
        }
    }
}

/// Device capabilities string (254 bytes; the corresponding export is loaded
/// but never called, and the type is kept only for symbol-table completeness,
/// so it is never constructed and has no Default impl).
#[repr(C)]
#[derive(Clone, Copy)]
struct VciDeviceCaps {
    count: u16,
    caps: [u16; 126],
}

/// Maps a classic baudrate to SJA1000-style BTR0/BTR1. Unsupported rates
/// return [`Error::Invalid`].
fn btr01_from_baudrate(baudrate: CanBaudrate) -> Result<(u8, u8)> {
    match baudrate {
        CanBaudrate::B10Kbit => Ok((49, 28)),
        CanBaudrate::B20Kbit => Ok((24, 28)),
        CanBaudrate::B50Kbit => Ok((9, 28)),
        CanBaudrate::B100Kbit => Ok((4, 28)),
        CanBaudrate::B125Kbit => Ok((3, 28)),
        CanBaudrate::B250Kbit => Ok((1, 28)),
        CanBaudrate::B500Kbit => Ok((0, 28)),
        CanBaudrate::B800Kbit => Ok((0, 22)),
        CanBaudrate::B1Mbit => Ok((0, 20)),
        other => Err(Error::Invalid(format!(
            "vcinpl: no predefined BTR0/BTR1 for baudrate {other} \
             (supported: 10k/20k/50k/100k/125k/250k/500k/800k/1M)"
        ))),
    }
}

/// Bit-timing literals for classic baudrates under VCI4. Rates outside the
/// table keep tseg1 = 0 (including NotSet; bitrate is still filled with the Hz
/// value).
fn canbtr_from_baudrate(baudrate: CanBaudrate) -> CanBtr {
    let tseg1 = match baudrate {
        CanBaudrate::B10Kbit
        | CanBaudrate::B20Kbit
        | CanBaudrate::B50Kbit
        | CanBaudrate::B100Kbit
        | CanBaudrate::B125Kbit
        | CanBaudrate::B250Kbit
        | CanBaudrate::B500Kbit => 14,
        CanBaudrate::B800Kbit => 8,
        CanBaudrate::B1Mbit => 6,
        _ => 0,
    };
    CanBtr {
        flags: 0,
        bitrate: baudrate.as_u32(),
        tseg1,
        tseg2: 2,
        sjw: 1,
        f3: 0,
    }
}

/// FD baudrate (Hz) -> preset CanBtr table (flags always 0; field order
/// tseg1/tseg2/sjw/f3).
fn fd_btr_preset(hz: u32) -> Option<CanBtr> {
    let (tseg1, tseg2, sjw, f3) = match hz {
        500_000 => (6400, 1600, 1600, 6400),
        1_000_000 => (6400, 1600, 1600, 6400),
        2_000_000 => (1600, 400, 400, 1600),
        4_000_000 => (800, 200, 200, 800),
        5_000_000 => (600, 200, 200, 600),
        8_000_000 => (400, 100, 100, 250),
        10_000_000 => (300, 100, 100, 200),
        _ => return None,
    };
    Some(CanBtr {
        flags: 0,
        bitrate: hz,
        tseg1,
        tseg2,
        sjw,
        f3,
    })
}

/// Looks up the nominal/data-phase baudrates in the preset table; **if either
/// rate misses, BOTH fall back to defaults with only bitrate filled**
/// (intentional quirk: the two lookups are coupled).
fn fd_btr_lookup(nominal_hz: u32, data_hz: u32) -> (CanBtr, CanBtr) {
    match (fd_btr_preset(nominal_hz), fd_btr_preset(data_hz)) {
        (Some(n), Some(d)) => (n, d),
        _ => (
            CanBtr {
                bitrate: nominal_hz,
                ..Default::default()
            },
            CanBtr {
                bitrate: data_hz,
                ..Default::default()
            },
        ),
    }
}

/// Maps custom FD bit timing to a CanBtr pair (Brp->bitrate, TSeg1->tseg1,
/// TSeg2->tseg2, Sjw->sjw).
fn fd_btr_from_custom(cfg: &BitRateConfig) -> (CanBtr, CanBtr) {
    let map = |p: &crate::frame::BitRatePar| CanBtr {
        flags: 0,
        bitrate: p.brp as u32,
        tseg1: p.tseg1 as u16,
        tseg2: p.tseg2 as u16,
        sjw: p.sjw as u16,
        f3: 0,
    };
    (map(&cfg.nominal), map(&cfg.data))
}

/// (Guid memory layout = u32/u16/u16 little-endian + 8 bytes in order),
/// truncated to the first 13 characters. Used in BusId:
/// `"IXXAT[{desc13}]/CAN{ch+1}"`.
fn guid_desc13(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let s = format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    );
    s[..13].to_string()
}

/// Builds a classic TX message: the id is masked to 0x7FFFFFFF, the data is
/// truncated to 8 bytes, msg_type[2] = (len & 0xF) | (0x80 if extended).
fn build_can_msg(can_id: u32, data: &[u8]) -> CanMsg {
    let mut msg = CanMsg {
        dw_msg_id: can_id & !CAN_EXT_FLAG,
        ..Default::default()
    };
    let n = data.len().min(8);
    msg.data[..n].copy_from_slice(&data[..n]);
    msg.msg_type[2] = (data.len() as u8) & CAN_DLC_MASK;
    if can_id & CAN_EXT_FLAG != 0 {
        msg.msg_type[2] |= CAN_DLC_EXT;
    }
    msg
}

/// Builds an FD TX message: msg_type[1] is set only for `FrameType::FD` -> 4
/// and `FrameType::FD_BRS` -> 12 (passing BRS alone yields 0); msg_type[2] =
/// lengthToDLC(len) & 0xF | extended bit.
fn build_canfd_msg(can_id: u32, data: &[u8], frame_type: FrameType) -> Result<CanFdMsg> {
    // Validate the length first (>64 -> Invalid, same as lengthToDLC), then copy, to avoid overruns.
    let mut dlc = length_to_dlc(data.len())? & CAN_DLC_MASK;
    let mut msg = CanFdMsg {
        dw_msg_id: can_id & !CAN_EXT_FLAG,
        ..Default::default()
    };
    msg.data[..data.len()].copy_from_slice(data);
    msg.msg_type[1] = match frame_type {
        t if t == FrameType::FD => CAN_MSGFLAGS_FD,
        t if t == FrameType::FD_BRS => CAN_MSGFLAGS_FD | CAN_MSGFLAGS_BRS,
        _ => 0,
    };
    if can_id & CAN_EXT_FLAG != 0 {
        dlc |= CAN_DLC_EXT;
    }
    msg.msg_type[2] = dlc;
    Ok(msg)
}

/// Classic RX filter: a non-DATA type stops the whole read batch; DLC==0 or
/// RTR frames are skipped; extended frames get CAN_EXT_FLAG. Returns (id, dlc).
fn parse_rx_classic(msg: &CanMsg) -> Option<(u32, u8)> {
    if msg.msg_type[0] != CAN_MSGTYPE_DATA {
        return None;
    }
    let dlc = msg.msg_type[2] & CAN_DLC_MASK;
    if dlc == 0 || msg.msg_type[2] & CAN_DLC_RTR != 0 {
        return None;
    }
    let mut id = msg.dw_msg_id;
    if msg.msg_type[2] & CAN_DLC_EXT != 0 {
        id |= CAN_EXT_FLAG;
    }
    Some((id, dlc))
}

/// FD RX filter: the length is recovered from the DLC via dlcToLength;
/// msg_type[1] &4/&8 map to FrameType FD/BRS. Returns (id, len, frame_type).
fn parse_rx_fd(msg: &CanFdMsg) -> Result<Option<(u32, u8, FrameType)>> {
    if msg.msg_type[0] != CAN_MSGTYPE_DATA {
        return Ok(None);
    }
    let dlc = msg.msg_type[2] & CAN_DLC_MASK;
    let len = dlc_to_length(dlc)?;
    if len == 0 || msg.msg_type[2] & CAN_DLC_RTR != 0 {
        return Ok(None);
    }
    let mut id = msg.dw_msg_id;
    if msg.msg_type[2] & CAN_DLC_EXT != 0 {
        id |= CAN_EXT_FLAG;
    }
    let mut frame_type = FrameType::CAN20B;
    if msg.msg_type[1] & CAN_MSGFLAGS_FD != 0 {
        frame_type = frame_type | FrameType::FD;
    }
    if msg.msg_type[1] & CAN_MSGFLAGS_BRS != 0 {
        frame_type = frame_type | FrameType::BRS;
    }
    Ok(Some((id, len, frame_type)))
}

/// Function-pointer table for vcinpl/vcinpl2 (all symbols are resolved and
/// validated at construction; a missing export fails immediately with an error
/// instead of surfacing later as a call through a null pointer).
/// Field types follow the usual Windows marshalling conventions (`bool` as a
/// 4-byte BOOL -> i32; byte-enum arguments passed by word on x64). Same-named
/// exports with two per-library signatures are stored once per signature (see
/// the module header).
#[derive(Debug)]
struct CanPl {
    /// Keeps the library handle alive (autors-native [`DllWrapper`]); the field
    /// itself is never accessed directly.
    _dll: DllWrapper,
    /// Which library was loaded.
    lib: VciLib,
    // ---- Device/enumeration API (12 exports) ----
    /// HRESULT vciInitialize(void) (the return value is ignored).
    vci_initialize: unsafe extern "system" fn() -> HResult,
    /// HRESULT vciEnumDeviceOpen(out HANDLE).
    vci_enum_device_open: unsafe extern "system" fn(*mut Handle) -> HResult,
    /// HRESULT vciEnumDeviceClose(HANDLE).
    vci_enum_device_close: unsafe extern "system" fn(Handle) -> HResult,
    /// HRESULT vciEnumDeviceNext(HANDLE, out VCIDEVICEINFO).
    vci_enum_device_next: unsafe extern "system" fn(Handle, *mut VciDeviceInfo) -> HResult,
    /// HRESULT vciEnumDeviceReset(HANDLE).
    #[allow(dead_code)] // Symbol-table completeness: loaded but never called.
    vci_enum_device_reset: unsafe extern "system" fn(Handle) -> HResult,
    /// HRESULT vciDeviceOpenDlg(HWND, out HANDLE): multi-device selection dialog.
    #[allow(dead_code)] // This implementation always uses the first device (see create).
    vci_device_open_dlg: unsafe extern "system" fn(Handle, *mut Handle) -> HResult,
    /// HRESULT vciDeviceOpen(ref int64 luid, out HANDLE).
    vci_device_open: unsafe extern "system" fn(*mut i64, *mut Handle) -> HResult,
    /// HRESULT vciDeviceClose(HANDLE).
    vci_device_close: unsafe extern "system" fn(Handle) -> HResult,
    /// HRESULT vciDeviceGetInfo(HANDLE, out VCIDEVICEINFO).
    vci_device_get_info: unsafe extern "system" fn(Handle, *mut VciDeviceInfo) -> HResult,
    /// HRESULT vciDeviceGetCaps(HANDLE, ref caps).
    #[allow(dead_code)] // Symbol-table completeness: loaded but never called.
    vci_device_get_caps: unsafe extern "system" fn(Handle, *mut VciDeviceCaps) -> HResult,
    /// HRESULT vciSelectDeviceDlg(HWND, out int64 luid).
    #[allow(dead_code)] // Symbol-table completeness: loaded but never called.
    vci_select_device_dlg: unsafe extern "system" fn(Handle, *mut i64) -> HResult,
    /// void vciFormatError(HRESULT, StringBuilder, uint).
    #[allow(dead_code)] // Symbol-table completeness: loaded but never called.
    vci_format_error: unsafe extern "system" fn(HResult, *mut u16, u32),
    // ---- CAN channel API ----
    /// HRESULT canChannelOpen(HANDLE hDevice, uint dwChannelNo, BOOL bExclusive,
    /// out HANDLE phChannel).
    can_channel_open: unsafe extern "system" fn(Handle, u32, i32, *mut Handle) -> HResult,
    /// HRESULT canChannelClose(HANDLE).
    can_channel_close: unsafe extern "system" fn(Handle) -> HResult,
    /// HRESULT canChannelInitialize(HANDLE, ushort rxSize, ushort rxThreshold,
    /// ushort txSize, ushort txThreshold) (VCI3 signature).
    can_channel_initialize_v3: unsafe extern "system" fn(Handle, u16, u16, u16, u16) -> HResult,
    /// Same export, VCI4 signature: appends (uint reserved=0, byte filter=2).
    can_channel_initialize_v4:
        unsafe extern "system" fn(Handle, u16, u16, u16, u16, u32, u32) -> HResult,
    /// HRESULT canChannelActivate(HANDLE, BOOL bActivate).
    can_channel_activate: unsafe extern "system" fn(Handle, i32) -> HResult,
    /// HRESULT canChannelWaitRxEvent(HANDLE, uint dwTimeoutMs).
    can_channel_wait_rx_event: unsafe extern "system" fn(Handle, u32) -> HResult,
    /// HRESULT canChannelReadMessage(HANDLE, uint timeout, out CANMSG) (VCI3).
    can_channel_read_message_v3: unsafe extern "system" fn(Handle, u32, *mut CanMsg) -> HResult,
    /// Same export, VCI4 signature (out CANFDMSG).
    can_channel_read_message_v4: unsafe extern "system" fn(Handle, u32, *mut CanFdMsg) -> HResult,
    /// HRESULT canChannelSendMessage(HANDLE, uint timeout, ref CANMSG) (VCI3).
    #[allow(dead_code)]
    // Symbol-table completeness: loaded but never called (sending goes through PostMessage).
    can_channel_send_message_v3: unsafe extern "system" fn(Handle, u32, *mut CanMsg) -> HResult,
    /// Same export, VCI4 signature (ref CANFDMSG).
    #[allow(dead_code)] // Symbol-table completeness: same as above.
    can_channel_send_message_v4: unsafe extern "system" fn(Handle, u32, *mut CanFdMsg) -> HResult,
    /// HRESULT canChannelPostMessage(HANDLE, ref CANMSG) (VCI3 send).
    can_channel_post_message_v3: unsafe extern "system" fn(Handle, *const CanMsg) -> HResult,
    /// Same export, VCI4 signature (ref CANFDMSG).
    can_channel_post_message_v4: unsafe extern "system" fn(Handle, *const CanFdMsg) -> HResult,
    // ---- CAN controller API ----
    /// HRESULT canControlOpen(HANDLE hDevice, uint dwCanNo, out HANDLE).
    can_control_open: unsafe extern "system" fn(Handle, u32, *mut Handle) -> HResult,
    /// HRESULT canControlClose(HANDLE).
    can_control_close: unsafe extern "system" fn(Handle) -> HResult,
    /// HRESULT canControlGetStatus(HANDLE, out CANLINESTATUS) (VCI3, 8 bytes).
    can_control_get_status_v3: unsafe extern "system" fn(Handle, *mut CanLineStatus) -> HResult,
    /// Same export, VCI4 signature (out CANFDSTATUS, 40 bytes).
    can_control_get_status_v4: unsafe extern "system" fn(Handle, *mut CanFdStatus) -> HResult,
    /// HRESULT canControlInitialize(HANDLE, byte opMode, byte btr0, byte btr1)
    /// (VCI3 signature).
    can_control_initialize_v3: unsafe extern "system" fn(Handle, u8, u8, u8) -> HResult,
    /// Same export, VCI4 signature: (HANDLE, byte opMode, byte opModeEx, byte,
    /// byte, uint, uint, ref CANBTR nominal, ref CANBTR data).
    can_control_initialize_v4: unsafe extern "system" fn(
        Handle,
        u8,
        u8,
        u8,
        u8,
        u32,
        u32,
        *const CanBtr,
        *const CanBtr,
    ) -> HResult,
    /// HRESULT canControlStart(HANDLE, BOOL bStart).
    can_control_start: unsafe extern "system" fn(Handle, i32) -> HResult,
}

impl CanPl {
    /// Library selection strategy: classic prefers vcinpl.dll, FD prefers
    /// vcinpl2.dll, each falling back to the other; if both fail, returns
    /// [`Error::Driver`].
    fn load(prefer_vci3: bool) -> Result<Self> {
        let (primary, secondary) = if prefer_vci3 {
            (VCI3_DLL, VCI4_DLL)
        } else {
            (VCI4_DLL, VCI3_DLL)
        };
        match Self::load_from(primary) {
            Ok(api) => Ok(api),
            Err(e1) => {
                Self::load_from(secondary).map_err(|e2| Error::Driver(format!("{e1}; {e2}")))
            }
        }
    }

    /// Loads from the given path/name and resolves all export symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the DllMain-executing unsafe) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // The library type is determined from the file name ("vcinpl.dll" -> V3, otherwise V4).
        let lib = if path.contains("vcinpl2") {
            VciLib::V4
        } else {
            VciLib::V3
        };
        // SAFETY (each `get` in the macro expansion): symbol addresses are only
        // taken within this function and copied into raw function pointers; the
        // library handle and the pointers live in the same struct, keeping the
        // pointers valid. The generic T is a function-pointer type (Copy).
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
            lib,
            vci_initialize: sym!(b"vciInitialize\0", unsafe extern "system" fn() -> HResult),
            vci_enum_device_open: sym!(
                b"vciEnumDeviceOpen\0",
                unsafe extern "system" fn(*mut Handle) -> HResult
            ),
            vci_enum_device_close: sym!(
                b"vciEnumDeviceClose\0",
                unsafe extern "system" fn(Handle) -> HResult
            ),
            vci_enum_device_next: sym!(
                b"vciEnumDeviceNext\0",
                unsafe extern "system" fn(Handle, *mut VciDeviceInfo) -> HResult
            ),
            vci_enum_device_reset: sym!(
                b"vciEnumDeviceReset\0",
                unsafe extern "system" fn(Handle) -> HResult
            ),
            vci_device_open_dlg: sym!(
                b"vciDeviceOpenDlg\0",
                unsafe extern "system" fn(Handle, *mut Handle) -> HResult
            ),
            vci_device_open: sym!(
                b"vciDeviceOpen\0",
                unsafe extern "system" fn(*mut i64, *mut Handle) -> HResult
            ),
            vci_device_close: sym!(
                b"vciDeviceClose\0",
                unsafe extern "system" fn(Handle) -> HResult
            ),
            vci_device_get_info: sym!(
                b"vciDeviceGetInfo\0",
                unsafe extern "system" fn(Handle, *mut VciDeviceInfo) -> HResult
            ),
            vci_device_get_caps: sym!(
                b"vciDeviceGetCaps\0",
                unsafe extern "system" fn(Handle, *mut VciDeviceCaps) -> HResult
            ),
            vci_select_device_dlg: sym!(
                b"vciSelectDeviceDlg\0",
                unsafe extern "system" fn(Handle, *mut i64) -> HResult
            ),
            vci_format_error: sym!(
                b"vciFormatError\0",
                unsafe extern "system" fn(HResult, *mut u16, u32)
            ),
            can_channel_open: sym!(
                b"canChannelOpen\0",
                unsafe extern "system" fn(Handle, u32, i32, *mut Handle) -> HResult
            ),
            can_channel_close: sym!(
                b"canChannelClose\0",
                unsafe extern "system" fn(Handle) -> HResult
            ),
            can_channel_initialize_v3: sym!(
                b"canChannelInitialize\0",
                unsafe extern "system" fn(Handle, u16, u16, u16, u16) -> HResult
            ),
            can_channel_initialize_v4: sym!(
                b"canChannelInitialize\0",
                unsafe extern "system" fn(Handle, u16, u16, u16, u16, u32, u32) -> HResult
            ),
            can_channel_activate: sym!(
                b"canChannelActivate\0",
                unsafe extern "system" fn(Handle, i32) -> HResult
            ),
            can_channel_wait_rx_event: sym!(
                b"canChannelWaitRxEvent\0",
                unsafe extern "system" fn(Handle, u32) -> HResult
            ),
            can_channel_read_message_v3: sym!(
                b"canChannelReadMessage\0",
                unsafe extern "system" fn(Handle, u32, *mut CanMsg) -> HResult
            ),
            can_channel_read_message_v4: sym!(
                b"canChannelReadMessage\0",
                unsafe extern "system" fn(Handle, u32, *mut CanFdMsg) -> HResult
            ),
            can_channel_send_message_v3: sym!(
                b"canChannelSendMessage\0",
                unsafe extern "system" fn(Handle, u32, *mut CanMsg) -> HResult
            ),
            can_channel_send_message_v4: sym!(
                b"canChannelSendMessage\0",
                unsafe extern "system" fn(Handle, u32, *mut CanFdMsg) -> HResult
            ),
            can_channel_post_message_v3: sym!(
                b"canChannelPostMessage\0",
                unsafe extern "system" fn(Handle, *const CanMsg) -> HResult
            ),
            can_channel_post_message_v4: sym!(
                b"canChannelPostMessage\0",
                unsafe extern "system" fn(Handle, *const CanFdMsg) -> HResult
            ),
            can_control_open: sym!(
                b"canControlOpen\0",
                unsafe extern "system" fn(Handle, u32, *mut Handle) -> HResult
            ),
            can_control_close: sym!(
                b"canControlClose\0",
                unsafe extern "system" fn(Handle) -> HResult
            ),
            can_control_get_status_v3: sym!(
                b"canControlGetStatus\0",
                unsafe extern "system" fn(Handle, *mut CanLineStatus) -> HResult
            ),
            can_control_get_status_v4: sym!(
                b"canControlGetStatus\0",
                unsafe extern "system" fn(Handle, *mut CanFdStatus) -> HResult
            ),
            can_control_initialize_v3: sym!(
                b"canControlInitialize\0",
                unsafe extern "system" fn(Handle, u8, u8, u8) -> HResult
            ),
            can_control_initialize_v4: sym!(
                b"canControlInitialize\0",
                unsafe extern "system" fn(
                    Handle,
                    u8,
                    u8,
                    u8,
                    u8,
                    u32,
                    u32,
                    *const CanBtr,
                    *const CanBtr,
                ) -> HResult
            ),
            can_control_start: sym!(
                b"canControlStart\0",
                unsafe extern "system" fn(Handle, i32) -> HResult
            ),
            _dll: dll,
        };
        // vciInitialize() runs once per instance (there is no paired uninit;
        // the library is unloaded via FreeLibrary). The return value is ignored.
        // SAFETY: the function pointer comes from the loaded library; no arguments.
        unsafe { (api.vci_initialize)() };
        Ok(api)
    }
}

/// IXXAT CAN channel adapter.
/// Reception follows the non-blocking convention of [`CanDevice::receive`]:
/// canChannelWaitRxEvent(0) is used as a single poll, followed by draining
/// reads (blocking waits are handled by the polling cadence of
/// [`crate::device::start_dispatch`]).
pub struct IxxatCan {
    core: DeviceCore,
    api: CanPl,
    /// VCI device handle (opened at construction, 0 = invalid).
    device: Handle,
    /// CAN channel handle (0 = not opened).
    channel: Handle,
    /// CAN controller handle.
    control: Handle,
    /// First 13 characters of the device description string (used in BusId).
    desc13: String,
    /// Whether the channel was opened with an FD configuration.
    fd_enabled: bool,
    /// Controller started (intentional quirk: this flag is set regardless of
    /// the canControlStart result).
    started: bool,
    /// Bus ID (generated at open, or taken from the configuration).
    bus_id: String,
    /// Receive frame buffer.
    rx_queue: VecDeque<CanFrame>,
}

impl IxxatCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        // Close sequence: take the channel handle first (so a concurrent
        // receive loop exits), deactivate + close the channel, then stop and
        // close the controller (return values are ignored).
        let channel = std::mem::take(&mut self.channel);
        if channel != 0 {
            // SAFETY: channel was returned by canChannelOpen and has not been closed.
            unsafe {
                (self.api.can_channel_activate)(channel, 0);
                (self.api.can_channel_close)(channel);
            }
        }
        if self.control != 0 {
            if self.started {
                // SAFETY: control was returned by canControlOpen and has not been closed.
                unsafe { (self.api.can_control_start)(self.control, 0) };
            }
            self.started = false;
            // SAFETY: same as above.
            unsafe { (self.api.can_control_close)(self.control) };
            self.control = 0;
        }
        self.rx_queue.clear();
    }

    /// Classic-preference construction (vcinpl.dll first, falling back to
    /// vcinpl2.dll); opens the first device. Returns Err if the driver is not
    /// installed or no device is present.
    pub fn new() -> Result<Self> {
        Self::create(true)
    }

    /// FD-preference construction (vcinpl2.dll first).
    pub fn new_can_fd() -> Result<Self> {
        Self::create(false)
    }

    /// Common constructor: loads the library, then enumerates and opens the
    /// first device.
    fn create(prefer_vci3: bool) -> Result<Self> {
        let api = CanPl::load(prefer_vci3)?;
        // vciEnumDeviceOpen -> loop vciEnumDeviceNext counting devices
        // (recording the first LUID) -> vciEnumDeviceClose.
        let mut enumerator: Handle = 0;
        // SAFETY: the function pointer comes from the loaded library; the out
        // pointer refers to a stack variable of this frame.
        let hr = unsafe { (api.vci_enum_device_open)(&mut enumerator) };
        let mut count = 0u32;
        let mut first_luid: i64 = -1;
        if hr == VCI_OK {
            loop {
                let mut info = VciDeviceInfo::default();
                // SAFETY: enumerator was returned by vciEnumDeviceOpen; info
                // points to a 304-byte buffer in this stack frame.
                let hr = unsafe { (api.vci_enum_device_next)(enumerator, &mut info) };
                if hr != VCI_OK {
                    break;
                }
                count += 1;
                if first_luid == -1 {
                    first_luid = info.vci_luid;
                }
            }
            // SAFETY: enumerator is valid.
            unsafe { (api.vci_enum_device_close)(enumerator) };
        }
        // No devices -> "No device available".
        if count == 0 {
            return Err(Error::NotSupported(
                "vcinpl: No device available".to_string(),
            ));
        }
        // With more than one device a selection dialog would normally be
        // shown; headless environments always use the first device instead
        // (dialogs are not portable).
        let mut device: Handle = 0;
        // SAFETY: the luid pointer refers to a stack variable of this frame;
        // the out handle likewise.
        let hr = unsafe { (api.vci_device_open)(&mut first_luid, &mut device) };
        if hr != VCI_OK || device == 0 {
            return Err(Error::NotSupported(format!(
                "vcinpl: No device available (vciDeviceOpen: {} (0x{hr:08X}))",
                vci_error_name(hr)
            )));
        }
        // vciDeviceGetInfo -> description string = formatted uniqueHardwareId.
        let mut info = VciDeviceInfo::default();
        // SAFETY: device is valid; info points to a stack buffer.
        let hr = unsafe { (api.vci_device_get_info)(device, &mut info) };
        let desc13 = if hr == VCI_OK {
            guid_desc13(&info.unique_hw_id)
        } else {
            String::new()
        };
        Ok(Self {
            core: DeviceCore::new(),
            api,
            device,
            channel: 0,
            control: 0,
            desc13,
            fd_enabled: false,
            started: false,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.channel != 0
    }

    /// VCI4 bitrate configuration for open (nine-argument canControlInitialize
    /// form).
    fn configure_bitrate_v4(&self, config: &CanConfiguration) -> Result<()> {
        let (op_mode_ex, nominal, data);
        if config.baudrate_fd == CanFdBaudrate::NotUsed && config.fd_bit_rate_config.is_none() {
            // Pure classic: opModeEx=0; nominal and data use the same literal table.
            op_mode_ex = 0;
            nominal = canbtr_from_baudrate(config.baudrate);
            data = nominal;
        } else {
            op_mode_ex = if config
                .fd_bit_rate_config
                .as_ref()
                .is_some_and(|c| c.non_iso)
            {
                CAN_OPMODE_EX_FD | CAN_OPMODE_EX_NONISO
            } else {
                CAN_OPMODE_EX_FD
            };
            match &config.fd_bit_rate_config {
                // Preset table lookup (including the "either miss -> both fall
                // back" behavior).
                None => {
                    let (n, d) =
                        fd_btr_lookup(config.baudrate.as_u32(), config.baudrate_fd.as_u32());
                    nominal = n;
                    data = d;
                }
                // Custom bit-timing mapping.
                Some(custom) => {
                    let (n, d) = fd_btr_from_custom(custom);
                    nominal = n;
                    data = d;
                }
            }
        }
        // SAFETY: control was returned by canControlOpen (non-zero); the CanBtr
        // pointers refer to stack variables of this frame, valid for the call.
        let hr = unsafe {
            (self.api.can_control_initialize_v4)(
                self.control,
                CAN_OPMODE,
                op_mode_ex,
                0,
                0,
                0,
                0,
                &nominal,
                &data,
            )
        };
        check(hr, &self.bus_id, "canControlInitialize")
    }
}

#[async_trait]
impl CanDevice for IxxatCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // is_available only reports state; the actual open sequence happens in
        // open().
        Ok(self.is_open())
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.channel != 0 {
            return Ok(true);
        }
        // BusId = "IXXAT[{desc13}]/CAN{channel+1}"; config.bus_id takes
        // precedence when set, otherwise the ID is generated in this format.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format!("IXXAT[{}]/CAN{}", self.desc13, config.channel + 1));
        // FD requested on a VCI3 library -> error (only BaudrateFD is checked,
        // not FDBitRateConfig — intentional).
        if config.baudrate_fd != CanFdBaudrate::NotUsed && self.api.lib == VciLib::V3 {
            return Err(config_err(&self.bus_id, "VCI3 doesn't support CAN FD"));
        }
        // SAFETY: device is valid; the out handle refers to a stack variable;
        // bExclusive=FALSE.
        let mut channel: Handle = 0;
        let hr = unsafe {
            (self.api.can_channel_open)(self.device, config.channel as u32, 0, &mut channel)
        };
        // Error format: "failed({0}) to open CAN (channel {1})".
        if hr != VCI_OK {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "failed({} (0x{hr:08X})) to open CAN (channel {})",
                    vci_error_name(hr),
                    config.channel
                ),
            ));
        }
        self.channel = channel;
        self.fd_enabled = false;
        // On failure at any later step the handles opened so far are cleaned up
        // before returning the error (close_sync handles the teardown).
        let result = (|| {
            // SAFETY (all calls in this closure): the channel/control handles
            // were returned by the open calls above; struct pointers refer to
            // stack variables of this frame and are valid for the calls.
            let hr = match self.api.lib {
                VciLib::V3 => unsafe {
                    (self.api.can_channel_initialize_v3)(
                        self.channel,
                        RX_QUEUE_SIZE,
                        RX_THRESHOLD,
                        TX_QUEUE_SIZE,
                        TX_THRESHOLD,
                    )
                },
                VciLib::V4 => unsafe {
                    (self.api.can_channel_initialize_v4)(
                        self.channel,
                        RX_QUEUE_SIZE,
                        RX_THRESHOLD,
                        TX_QUEUE_SIZE,
                        TX_THRESHOLD,
                        0,
                        VCI4_CHN_INIT_FILTER,
                    )
                },
            };
            check(hr, &self.bus_id, "canChannelInitialize")?;
            // SAFETY: same as above.
            let hr = unsafe { (self.api.can_channel_activate)(self.channel, 1) };
            check(hr, &self.bus_id, "canChannelActivate")?;
            // SAFETY: same as above.
            let hr = unsafe {
                (self.api.can_control_open)(self.device, config.channel as u32, &mut self.control)
            };
            check(hr, &self.bus_id, "canControlOpen")?;
            match self.api.lib {
                VciLib::V3 => {
                    // Baudrate == NotSet is treated as 1MBit.
                    let baudrate = if config.baudrate == CanBaudrate::NotSet {
                        CanBaudrate::B1Mbit
                    } else {
                        config.baudrate
                    };
                    let (btr0, btr1) = match btr01_from_baudrate(baudrate) {
                        Ok(pair) => pair,
                        Err(Error::Invalid(msg)) => return Err(config_err(&self.bus_id, msg)),
                        Err(other) => return Err(other),
                    };
                    // SAFETY: same as above.
                    let hr = unsafe {
                        (self.api.can_control_initialize_v3)(self.control, CAN_OPMODE, btr0, btr1)
                    };
                    check(hr, &self.bus_id, "canControlInitialize")?;
                    // The canControlGetStatus result is read and discarded, its
                    // return value unchecked (intentional).
                    let mut status = CanLineStatus::default();
                    // SAFETY: same as above.
                    unsafe { (self.api.can_control_get_status_v3)(self.control, &mut status) };
                }
                VciLib::V4 => {
                    self.fd_enabled = config.is_fd();
                    self.configure_bitrate_v4(&config)?;
                    // Same as above: read and discarded.
                    let mut status = CanFdStatus::default();
                    // SAFETY: same as above.
                    unsafe { (self.api.can_control_get_status_v4)(self.control, &mut status) };
                }
            }
            // started is set regardless of the canControlStart result
            // (intentional); on failure open returns Ok(false) instead of an error.
            // SAFETY: same as above.
            let hr = unsafe { (self.api.can_control_start)(self.control, 1) };
            self.started = true;
            Ok(hr == VCI_OK)
        })();
        if result.is_err() {
            self.close_sync();
        }
        result
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // Sending while not open returns 0.
        if self.channel == 0 {
            return Ok(0);
        }
        if data.len() > MAX_FD_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
                data.len()
            )));
        }
        let hr = match self.api.lib {
            VciLib::V3 => {
                let msg = build_can_msg(can_id, data);
                // SAFETY: channel is valid; the msg pointer is valid for the
                // call and the driver does not retain it.
                unsafe { (self.api.can_channel_post_message_v3)(self.channel, &msg) }
            }
            VciLib::V4 => {
                // On a channel not opened as FD, frames are always built as
                // classic frames.
                let ft = if self.fd_enabled {
                    frame_type
                } else {
                    FrameType::CAN20B
                };
                let msg = build_canfd_msg(can_id, data, ft)?;
                // SAFETY: same as above.
                unsafe { (self.api.can_channel_post_message_v4)(self.channel, &msg) }
            }
        };
        // A PostMessage result other than OK returns 0 (no error is raised).
        if hr != VCI_OK {
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
        if self.channel == 0 {
            return Ok(None);
        }
        // Single non-blocking poll with a 0 timeout, then drain. Any non-OK
        // (timeout/empty queue/other) is treated as "no frame" — receive
        // errors are intentionally not propagated.
        // SAFETY: channel is valid; arguments are passed by value.
        let hr = unsafe { (self.api.can_channel_wait_rx_event)(self.channel, 0) };
        if hr != VCI_OK {
            return Ok(None);
        }
        loop {
            match self.api.lib {
                VciLib::V3 => {
                    let mut msg = CanMsg::default();
                    // SAFETY: channel is valid; msg points to a 20-byte buffer
                    // in this stack frame.
                    let hr = unsafe {
                        (self.api.can_channel_read_message_v3)(self.channel, 0, &mut msg)
                    };
                    // A non-OK read or a non-DATA frame stops this read round
                    // (non-DATA is a break, not a continue — intentional).
                    if hr != VCI_OK || msg.msg_type[0] != CAN_MSGTYPE_DATA {
                        break;
                    }
                    if let Some((id, dlc)) = parse_rx_classic(&msg) {
                        // The payload is truncated to the 8-byte maximum when
                        // copying, defending against hardware reporting DLC > 8.
                        let n = (dlc as usize).min(8);
                        self.rx_queue.push_back(CanFrame::new(
                            &self.bus_id,
                            id,
                            msg.data[..n].to_vec(),
                            false,
                            FrameType::CAN20B,
                        ));
                    }
                }
                VciLib::V4 => {
                    let mut msg = CanFdMsg::default();
                    // SAFETY: channel is valid; msg points to an 80-byte buffer
                    // in this stack frame.
                    let hr = unsafe {
                        (self.api.can_channel_read_message_v4)(self.channel, 0, &mut msg)
                    };
                    if hr != VCI_OK || msg.msg_type[0] != CAN_MSGTYPE_DATA {
                        break;
                    }
                    if let Some((id, len, frame_type)) = parse_rx_fd(&msg)? {
                        self.rx_queue.push_back(CanFrame::new(
                            &self.bus_id,
                            id,
                            msg.data[..len as usize].to_vec(),
                            false,
                            frame_type,
                        ));
                    }
                }
            }
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for IxxatCan {
    fn drop(&mut self) {
        // Teardown order: channel/controller first, then vciDeviceClose on the device.
        self.close_sync();
        if self.device != 0 {
            // SAFETY: device was returned by vciDeviceOpen and is closed only
            // here; the library handle in CanPl is still alive while drop()
            // runs.
            unsafe { (self.api.vci_device_close)(self.device) };
            self.device = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::BitRatePar;

    #[test]
    fn struct_sizes_use_pack1_layout() {
        // Struct sizes under the Pack = 1 layout (accumulated field by field).
        assert_eq!(std::mem::size_of::<CanMsg>(), 20);
        assert_eq!(std::mem::size_of::<CanFdMsg>(), 80);
        assert_eq!(std::mem::size_of::<CanBtr>(), 16);
        assert_eq!(std::mem::size_of::<CanLineStatus>(), 8);
        assert_eq!(std::mem::size_of::<CanFdStatus>(), 40);
        assert_eq!(std::mem::size_of::<VciDeviceInfo>(), 304);
        assert_eq!(std::mem::size_of::<VciDeviceCaps>(), 254);
        // Field-offset spot checks (layout: luid@0, uniqueHwId@32).
        assert_eq!(std::mem::offset_of!(VciDeviceInfo, vci_luid), 0);
        assert_eq!(std::mem::offset_of!(VciDeviceInfo, unique_hw_id), 32);
        assert_eq!(std::mem::offset_of!(CanFdMsg, data), 16);
    }

    #[test]
    fn btr01_table_matches_expected_contract() {
        // The complete baudrate -> BTR0/BTR1 mapping.
        assert_eq!(btr01_from_baudrate(CanBaudrate::B10Kbit).unwrap(), (49, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B20Kbit).unwrap(), (24, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B50Kbit).unwrap(), (9, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B100Kbit).unwrap(), (4, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B125Kbit).unwrap(), (3, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B250Kbit).unwrap(), (1, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B500Kbit).unwrap(), (0, 28));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B800Kbit).unwrap(), (0, 22));
        assert_eq!(btr01_from_baudrate(CanBaudrate::B1Mbit).unwrap(), (0, 20));
        // Rates outside the table -> Invalid.
        assert!(matches!(
            btr01_from_baudrate(CanBaudrate::NotSet),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            btr01_from_baudrate(CanBaudrate::B2Mbit),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn canbtr_from_baudrate_matches_expected_contract() {
        // <=500k: tseg1=14, tseg2=2, sjw=1.
        let b = canbtr_from_baudrate(CanBaudrate::B500Kbit);
        assert_eq!(
            b,
            CanBtr {
                flags: 0,
                bitrate: 500_000,
                tseg1: 14,
                tseg2: 2,
                sjw: 1,
                f3: 0
            }
        );
        assert_eq!(canbtr_from_baudrate(CanBaudrate::B800Kbit).tseg1, 8);
        assert_eq!(canbtr_from_baudrate(CanBaudrate::B1Mbit).tseg1, 6);
        // Outside the table (NotSet/FD rates): tseg1=0, bitrate still holds the Hz value.
        let b = canbtr_from_baudrate(CanBaudrate::NotSet);
        assert_eq!(
            b,
            CanBtr {
                flags: 0,
                bitrate: 0,
                tseg1: 0,
                tseg2: 2,
                sjw: 1,
                f3: 0
            }
        );
        assert_eq!(canbtr_from_baudrate(CanBaudrate::B2Mbit).tseg1, 0);
    }

    #[test]
    fn fd_btr_preset_table_matches_expected_contract() {
        // The static FD preset table.
        assert_eq!(
            fd_btr_preset(500_000).unwrap(),
            CanBtr {
                flags: 0,
                bitrate: 500_000,
                tseg1: 6400,
                tseg2: 1600,
                sjw: 1600,
                f3: 6400
            }
        );
        assert_eq!(fd_btr_preset(1_000_000).unwrap().tseg1, 6400);
        assert_eq!(
            fd_btr_preset(8_000_000).unwrap(),
            CanBtr {
                flags: 0,
                bitrate: 8_000_000,
                tseg1: 400,
                tseg2: 100,
                sjw: 100,
                f3: 250
            }
        );
        assert_eq!(fd_btr_preset(10_000_000).unwrap().f3, 200);
        assert!(fd_btr_preset(125_000).is_none());
    }

    #[test]
    fn fd_btr_lookup_fallback_matches_expected_contract() {
        // Both hit: each looked up individually.
        let (n, d) = fd_btr_lookup(500_000, 2_000_000);
        assert_eq!(n.tseg1, 6400);
        assert_eq!(d.tseg1, 1600);
        // Either miss -> both fall back to bitrate-only defaults.
        let (n, d) = fd_btr_lookup(125_000, 2_000_000);
        assert_eq!(
            n,
            CanBtr {
                bitrate: 125_000,
                ..Default::default()
            }
        );
        assert_eq!(
            d,
            CanBtr {
                bitrate: 2_000_000,
                ..Default::default()
            }
        );
        let (n, d) = fd_btr_lookup(500_000, 3_000_000);
        assert_eq!(
            n,
            CanBtr {
                bitrate: 500_000,
                ..Default::default()
            }
        );
        assert_eq!(
            d,
            CanBtr {
                bitrate: 3_000_000,
                ..Default::default()
            }
        );
    }

    #[test]
    fn fd_btr_from_custom_maps_fields() {
        // Brp->bitrate, TSeg1->tseg1, TSeg2->tseg2, Sjw->sjw (f3=0).
        let cfg = BitRateConfig {
            clock: 0,
            non_iso: true,
            nominal: BitRatePar {
                brp: 8,
                tseg1: 31,
                tseg2: 8,
                sjw: 8,
            },
            data: BitRatePar {
                brp: 2,
                tseg1: 15,
                tseg2: 4,
                sjw: 4,
            },
        };
        let (n, d) = fd_btr_from_custom(&cfg);
        assert_eq!(
            n,
            CanBtr {
                flags: 0,
                bitrate: 8,
                tseg1: 31,
                tseg2: 8,
                sjw: 8,
                f3: 0
            }
        );
        assert_eq!(
            d,
            CanBtr {
                flags: 0,
                bitrate: 2,
                tseg1: 15,
                tseg2: 4,
                sjw: 4,
                f3: 0
            }
        );
    }

    #[test]
    fn guid_desc13_formats_dotnet_guid() {
        // All-zero GUID.
        assert_eq!(guid_desc13(&[0; 16]), "00000000-0000");
        let g: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        assert_eq!(guid_desc13(&g), "03020100-0504");
    }

    #[test]
    fn build_can_msg_matches_expected_contract() {
        // Standard frame: id as-is, dlc=len&0xF, data packed little-endian.
        let m = build_can_msg(0x123, &[1, 2, 3]);
        assert_eq!(m.dw_msg_id, 0x123);
        assert_eq!(m.msg_type, [0, 0, 3, 0]);
        assert_eq!(&m.data[..3], &[1, 2, 3]);
        assert_eq!(m.dw_time, 0);
        // Extended frame: id masked + 0x80.
        let m = build_can_msg(0x8000_0123, &[0xAA; 8]);
        assert_eq!(m.dw_msg_id, 0x123);
        assert_eq!(m.msg_type[2], 8 | CAN_DLC_EXT);
        assert_eq!(m.data, [0xAA; 8]);
        // >8 bytes: data truncated to 8, dlc still len&0xF (intentional quirk).
        let m = build_can_msg(0x1, &[7u8; 12]);
        assert_eq!(m.data, [7u8; 8]);
        assert_eq!(m.msg_type[2], 12);
    }

    #[test]
    fn build_canfd_msg_matches_expected_contract() {
        // FD_BRS: msg_type[1]=12, dlc=lengthToDLC(len).
        let m = build_canfd_msg(0x123, &[0u8; 12], FrameType::FD_BRS).unwrap();
        assert_eq!(m.msg_type[1], CAN_MSGFLAGS_FD | CAN_MSGFLAGS_BRS);
        assert_eq!(m.msg_type[2], 9);
        // FD (no BRS): msg_type[1]=4.
        let m = build_canfd_msg(0x123, &[0u8; 64], FrameType::FD).unwrap();
        assert_eq!(m.msg_type[1], CAN_MSGFLAGS_FD);
        assert_eq!(m.msg_type[2], 15);
        // Classic type / BRS passed alone: msg_type[1]=0.
        let m = build_canfd_msg(0x123, &[1, 2], FrameType::CAN20B).unwrap();
        assert_eq!(m.msg_type[1], 0);
        assert_eq!(m.msg_type[2], 2);
        let m = build_canfd_msg(0x123, &[1, 2], FrameType::BRS).unwrap();
        assert_eq!(m.msg_type[1], 0);
        // Extended-frame bit.
        let m = build_canfd_msg(0x8000_0123, &[1], FrameType::FD_BRS).unwrap();
        assert_eq!(m.dw_msg_id, 0x123);
        assert_eq!(m.msg_type[2], 1 | CAN_DLC_EXT);
        // >64 bytes -> Invalid (lengthToDLC), must not panic.
        assert!(matches!(
            build_canfd_msg(0x123, &[0u8; 65], FrameType::FD_BRS),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn parse_rx_classic_matches_expected_contract() {
        // Normal standard frame.
        let mut m = CanMsg {
            dw_msg_id: 0x123,
            ..Default::default()
        };
        m.msg_type[2] = 8;
        assert_eq!(parse_rx_classic(&m), Some((0x123, 8)));
        // Extended frame: CAN_EXT_FLAG set.
        m.msg_type[2] = 8 | CAN_DLC_EXT;
        assert_eq!(parse_rx_classic(&m), Some((0x123 | CAN_EXT_FLAG, 8)));
        // DLC=0 / RTR -> skipped; non-DATA type -> None.
        m.msg_type[2] = 0;
        assert_eq!(parse_rx_classic(&m), None);
        m.msg_type[2] = 8 | CAN_DLC_RTR;
        assert_eq!(parse_rx_classic(&m), None);
        m.msg_type[0] = 1;
        m.msg_type[2] = 8;
        assert_eq!(parse_rx_classic(&m), None);
    }

    #[test]
    fn parse_rx_fd_matches_expected_contract() {
        let mut m = CanFdMsg {
            dw_msg_id: 0x123,
            ..Default::default()
        };
        // DLC 9 -> 12 bytes; FD+BRS.
        m.msg_type[1] = CAN_MSGFLAGS_FD | CAN_MSGFLAGS_BRS;
        m.msg_type[2] = 9;
        assert_eq!(
            parse_rx_fd(&m).unwrap(),
            Some((0x123, 12, FrameType::FD_BRS))
        );
        // FD only; extended ID.
        m.msg_type[1] = CAN_MSGFLAGS_FD;
        m.msg_type[2] = 15 | CAN_DLC_EXT;
        assert_eq!(
            parse_rx_fd(&m).unwrap(),
            Some((0x123 | CAN_EXT_FLAG, 64, FrameType::FD))
        );
        // Classic frame (no FD bit).
        m.msg_type[1] = 0;
        m.msg_type[2] = 8;
        assert_eq!(
            parse_rx_fd(&m).unwrap(),
            Some((0x123, 8, FrameType::CAN20B))
        );
        // DLC=0 / RTR / non-DATA.
        m.msg_type[2] = 0;
        assert_eq!(parse_rx_fd(&m).unwrap(), None);
        m.msg_type[2] = 8 | CAN_DLC_RTR;
        assert_eq!(parse_rx_fd(&m).unwrap(), None);
        m.msg_type[0] = 2;
        m.msg_type[2] = 8;
        assert_eq!(parse_rx_fd(&m).unwrap(), None);
    }

    #[test]
    fn vci_error_names() {
        assert_eq!(vci_error_name(0), "VCI_OK");
        assert_eq!(vci_error_name(0xE001_000B), "VCI_E_TIMEOUT");
        assert_eq!(vci_error_name(0xE001_0012), "VCI_E_RXQUEUE_EMPTY");
        assert_eq!(vci_error_name(0xE001_002A), "VCI_E_LUIDS_EXHAUSTED");
        assert_eq!(vci_error_name(0xDEAD_BEEF), "VCI_E_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        let err = check(0xE001_000B, "IXXAT[abc]/CAN1", "canChannelWaitRxEvent").unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("IXXAT[abc]/CAN1"));
                assert!(msg.contains("canChannelWaitRxEvent"));
                assert!(msg.contains("VCI_E_TIMEOUT"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn bus_id_format_matches_expected_contract() {
        // IXXAT[desc13]/CAN{ch+1} (all-zero GUID instance, channel 0).
        let desc13 = guid_desc13(&[0; 16]);
        assert_eq!(
            format!("IXXAT[{desc13}]/CAN{}", 1),
            "IXXAT[00000000-0000]/CAN1"
        );
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that certainly does not exist: must yield Error::Driver,
        // not a panic.
        let err = CanPl::load_from("no_such_ixxat_vcinpl_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // If the IXXAT VCI driver is not installed, this must fail with
        // Error::Driver; on a machine with the driver installed it proceeds to
        // device enumeration: Error::NotSupported ("No device available") when
        // no device is present, or a successful construction with a complete
        // symbol table. All three outcomes are acceptable; the key is no panic.
        match IxxatCan::new() {
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
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) | Err(Error::NotSupported(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
        // FD-preference construction likewise must not panic.
        match IxxatCan::new_can_fd() {
            Ok(_) | Err(Error::Driver(_)) | Err(Error::NotSupported(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
