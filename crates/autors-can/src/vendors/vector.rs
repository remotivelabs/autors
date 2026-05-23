use std::collections::VecDeque;
use std::ffi::CString;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, dlc_to_length, format_bus_id, length_to_dlc, CanDevice, ChannelInfo, DeviceCore,
    MAX_DLC, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{BitRateConfig, CanBaudrate, CanConfiguration, CanFrame, FrameType};

#[cfg(target_pointer_width = "64")]
const VXLAPI_DLL: &str = "vxlapi64.dll";
#[cfg(target_pointer_width = "32")]
const VXLAPI_DLL: &str = "vxlapi.dll";

/// XL_SUCCESS.
const XL_SUCCESS: i32 = 0;
const XL_ERR_QUEUE_IS_FULL: i32 = 11;
/// XL_BUS_TYPE_CAN.
const XL_BUS_TYPE_CAN: u32 = 1;
const XL_INTERFACE_VERSION_V3: u32 = 3;
const XL_INTERFACE_VERSION_V4: u32 = 4;
/// XL_ACTIVATE_RESET_CLOCK.
const XL_ACTIVATE_RESET_CLOCK: u32 = 8;
const XL_RECEIVE_MSG: u8 = 1;
const XL_TRANSMIT_MSG: u8 = 10;
const XL_CAN_MSG_FLAG_TX_COMPLETED: u32 = 0x40;
/// XL_CAN_EV_TAG_RX_OK(V4 RX tag).
const XL_CAN_EV_TAG_RX_OK: u16 = 0x0400;
#[allow(dead_code)]
const XL_CAN_EV_TAG_TX_OK: u16 = 0x0404;
/// XL_CAN_EV_TAG_TX_MSG(V4 TX tag).
const XL_CAN_EV_TAG_TX_MSG: u16 = 0x0440;
/// XL_CAN_RXMSG_FLAG_EDL / XL_CAN_TXMSG_FLAG_EDL.
const XL_CAN_MSG_FLAG_EDL: u32 = 1;
/// XL_CAN_RXMSG_FLAG_BRS / XL_CAN_TXMSG_FLAG_BRS.
const XL_CAN_MSG_FLAG_BRS: u32 = 2;
/// XL_CANFD_CONFOPT_NO_ISO.
const XL_CANFD_CONFOPT_NO_ISO: u8 = 8;
const XL_CHANNEL_FLAG_CANFD_ISO_SUPPORT: u32 = 0x8000_0000;
const XL_CHANNEL_FLAG_CANFD_BOSCH_SUPPORT: u32 = 0x2000_0000;
const RX_QUEUE_SIZE_FD: u32 = 16384;
const RX_QUEUE_SIZE_CLASSIC: u32 = 4096;
const TX_QUEUE_FULL_MAX_RETRIES: u32 = 20;

static VXLAPI_REFCOUNT: AtomicUsize = AtomicUsize::new(0);

fn xl_status_name(status: i32) -> &'static str {
    match status {
        0 => "XL_SUCCESS",
        1 => "XL_PENDING",
        10 => "XL_ERR_QUEUE_IS_EMPTY",
        11 => "XL_ERR_QUEUE_IS_FULL",
        12 => "XL_ERR_TX_NOT_POSSIBLE",
        14 => "XL_ERR_NO_LICENSE",
        101 => "XL_ERR_WRONG_PARAMETER",
        110 => "XL_ERR_TWICE_REGISTER",
        111 => "XL_ERR_INVALID_CHAN_INDEX",
        112 => "XL_ERR_INVALID_ACCESS",
        113 => "XL_ERR_PORT_IS_OFFLINE",
        116 => "XL_ERR_CHAN_IS_ONLINE",
        117 => "XL_ERR_NOT_IMPLEMENTED",
        118 => "XL_ERR_INVALID_PORT",
        120 => "XL_ERR_HW_NOT_READY",
        121 => "XL_ERR_CMD_TIMEOUT",
        122 => "XL_ERR_CMD_HANDLING",
        129 => "XL_ERR_HW_NOT_PRESENT",
        131 => "XL_ERR_NOTIFY_ALREADY_ACTIVE",
        132 => "XL_ERR_INVALID_TAG",
        133 => "XL_ERR_INVALID_RESERVED_FLD",
        134 => "XL_ERR_INVALID_SIZE",
        135 => "XL_ERR_INSUFFICIENT_BUFFER",
        136 => "XL_ERR_ERROR_CRC",
        139 => "XL_ERR_NOT_FOUND",
        144 => "XL_ERR_INTERNAL_ERROR",
        152 => "XL_ERR_NO_RESOURCES",
        153 => "XL_ERR_WRONG_CHIP_TYPE",
        154 => "XL_ERR_WRONG_COMMAND",
        155 => "XL_ERR_INVALID_HANDLE",
        157 => "XL_ERR_RESERVED_NOT_ZERO",
        158 => "XL_ERR_INIT_ACCESS_MISSING",
        201 => "XL_ERR_CANNOT_OPEN_DRIVER",
        202 => "XL_ERR_WRONG_BUS_TYPE",
        203 => "XL_ERR_DLL_NOT_FOUND",
        204 => "XL_ERR_INVALID_CHANNEL_MASK",
        205 => "XL_ERR_NOT_SUPPORTED",
        210 => "XL_ERR_CONNECTION_BROKEN",
        211 => "XL_ERR_CONNECTION_CLOSED",
        216 => "XL_ERR_QUEUE_OVERRUN",
        255 => "XL_ERROR",
        513 => "XL_ERR_INVALID_DLC",
        514 => "XL_ERR_INVALID_CANID",
        515 => "XL_ERR_INVALID_FDFLAG_MODE20",
        516 => "XL_ERR_EDL_RTR",
        517 => "XL_ERR_EDL_NOT_SET",
        518 => "XL_ERR_UNKNOWN_FLAG",
        _ => "XL_ERR_???",
    }
}

fn check(status: i32, bus_id: &str, what: &str) -> Result<()> {
    if status == XL_SUCCESS {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "vxlapi: {what} failed: {} ({status})",
                xl_status_name(status)
            ),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwType;

#[allow(dead_code)]
impl HwType {
    pub const NONE: u8 = 0;
    pub const VIRTUAL: u8 = 1;
    pub const CANCARDX: u8 = 2;
    /// CAN-AC2-PCI.
    pub const CANAC2PCI: u8 = 6;
    pub const CANCARDY: u8 = 12;
    pub const CANCARDXL: u8 = 15;
    pub const CANCASEXL: u8 = 21;
    /// CANboardXL, CANboardXL PCIe.
    pub const CANBOARDXL: u8 = 25;
    /// CANboardXL pxi.
    pub const CANBOARDXL_PXI: u8 = 27;
    pub const VN2600: u8 = 29;
    pub const VN2610: u8 = 29;
    pub const VN3300: u8 = 37;
    pub const VN3600: u8 = 39;
    pub const VN7600: u8 = 41;
    pub const CANCARDXLE: u8 = 43;
    pub const VN8900: u8 = 45;
    pub const VN8950: u8 = 47;
    pub const VN2640: u8 = 53;
    pub const VN1610: u8 = 55;
    pub const VN1630: u8 = 57;
    pub const VN1640: u8 = 59;
    pub const VN8970: u8 = 61;
    pub const VN1611: u8 = 63;
    pub const VN5240: u8 = 64;
    pub const VN5610: u8 = 65;
    pub const VN5620: u8 = 66;
    pub const VN7570: u8 = 67;
    pub const VN5650: u8 = 68;
    pub const IPCLIENT: u8 = 69;
    pub const VX1121: u8 = 73;
    pub const VX1131: u8 = 75;
    pub const VT6204: u8 = 77;
    pub const VN1630_LOG: u8 = 79;
    pub const VN7610: u8 = 81;
    pub const VN7572: u8 = 83;
    pub const VN8972: u8 = 85;
    pub const VN0601: u8 = 87;
    pub const VN5640: u8 = 89;
    pub const VX0312: u8 = 91;
    pub const VN8800: u8 = 95;
    pub const IPCL8800: u8 = 96;
    pub const IPSRV8800: u8 = 97;
    pub const CSMCAN: u8 = 98;
    pub const VN5610A: u8 = 101;
    pub const VN7640: u8 = 102;
    pub const VX1135: u8 = 104;
    pub const VN4610: u8 = 105;
    pub const VT6306: u8 = 107;
    pub const VT6104A: u8 = 108;
    pub const VN5430: u8 = 109;
    pub const VTSSERVICE: u8 = 110;
    pub const VN1530: u8 = 112;
    pub const VN1531: u8 = 113;
    pub const VX1161A: u8 = 114;
    pub const VX1161B: u8 = 115;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlEvent {
    tag: u8,
    chan_index: u8,
    trans_id: u16,
    port_handle: u16,
    flags: u8,
    reserved: u8,
    time_stamp: u64,
    id: u32,
    /// XL_CAN_MSG_FLAG_*.
    msg_flags: u32,
    dlc: u16,
    res1: u64,
    data: u64,
    res2: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlCanTxMsg {
    /// XL_CAN_EV_TAG_TX_MSG(0x0440).
    tag: u16,
    channel_index: u16,
    user_handle: u8,
    flags_reserved: u16,
    reserved: u8,
    can_id: u32,
    /// XL_CAN_TXMSG_FLAG_EDL(1)/BRS(2).
    msg_flags: u32,
    dlc: u8,
    reserved1: u8,
    reserved2: u16,
    reserved3: u32,
    data: [u8; 64],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlCanRxMsg {
    size: u32,
    /// XL_CAN_EV_TAG_RX_OK(0x0400)/TX_OK(0x0404)/….
    tag: u16,
    channel_index: u16,
    user_handle: u32,
    flags_chip: u16,
    reserved0: u16,
    reserved1: u64,
    time_stamp: u64,
    can_id: u32,
    /// XL_CAN_RXMSG_FLAG_EDL(1)/BRS(2).
    msg_flags: u32,
    crc: u32,
    reserved2: u64,
    reserved3: u32,
    total_bit_cnt: u16,
    dlc: u8,
    reserved4: u8,
    reserved5: u32,
    data: [u8; 64],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlCanFdConf {
    arbitration_bit_rate: u32,
    sjw_abr: u32,
    tseg1_abr: u32,
    tseg2_abr: u32,
    data_bit_rate: u32,
    sjw_dbr: u32,
    tseg1_dbr: u32,
    tseg2_dbr: u32,
    reserved: u8,
    options: u8,
    reserved2: u16,
    reserved3: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlChannelConfig {
    name: [u8; 32],
    hw_type: u8,
    hw_index: u8,
    hw_channel: u8,
    transceiver_type: u16,
    transceiver_state: u16,
    config_error: u16,
    channel_index: u8,
    channel_mask: u64,
    channel_capabilities: u32,
    channel_bus_capabilities: u32,
    is_on_bus: u8,
    reserved_c1: u32,
    connected_bus_type: u32,
    bus_params: [u8; 28],
    do_not_use: u32,
    driver_version: u32,
    interface_version: u32,
    raw_data: [u32; 10],
    serial_number: u32,
    article_number: u32,
    transceiver_name: [u8; 32],
    special_cab_flags: u32,
    dominant_timeout: u32,
    reserved_d1: u8,
    reserved_d2: u8,
    reserved_d3: u8,
    reserved_d4: u8,
    reserved_e1: u16,
    reserved_e2: u16,
    reserved_f1: u32,
    reserved_g1: u8,
    reserved_g2: u8,
    reserved_h1: u16,
    reserved_h2: u16,
    reserved_h3: u16,
    reserved_i: [u32; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlDriverConfig {
    size: u32,
    channel_count: u32,
    reserved: [u32; 10],
    channel: [XlChannelConfig; 64],
}

const _: () = assert!(std::mem::size_of::<XlEvent>() == 50);
const _: () = assert!(std::mem::size_of::<XlCanTxMsg>() == 88);
const _: () = assert!(std::mem::size_of::<XlCanRxMsg>() == 128);
const _: () = assert!(std::mem::size_of::<XlCanFdConf>() == 40);
const _: () = assert!(std::mem::size_of::<XlChannelConfig>() == 227);
const _: () = assert!(std::mem::size_of::<XlDriverConfig>() == 4 + 4 + 40 + 64 * 227);

static CAN_BAUD_PARAMS: &[(u32, [u32; 3])] = &[
    (10_000, [16, 248, 1]),
    (20_000, [8, 248, 1]),
    (50_000, [4, 198, 1]),
    (100_000, [2, 198, 1]),
    (125_000, [2, 158, 1]),
    (250_000, [10, 11, 1]),
    (500_000, [119, 40, 40]),
    (800_000, [1, 48, 1]),
    (1_000_000, [59, 20, 20]),
    (2_000_000, [29, 10, 10]),
    (4_000_000, [14, 5, 5]),
    (5_000_000, [11, 4, 4]),
    (8_000_000, [5, 3, 3]),
    (10_000_000, [1, 1, 2]),
];

/// {tseg1Dbr, tseg2Dbr, sjwDbr}.
static CAN_FD_DATA_PARAMS: &[(u32, [u32; 3])] = &[
    (500_000, [119, 40, 1]),
    (1_000_000, [59, 20, 20]),
    (2_000_000, [29, 10, 10]),
    (4_000_000, [14, 5, 5]),
    (5_000_000, [11, 4, 4]),
    (8_000_000, [5, 3, 3]),
    (10_000_000, [1, 1, 2]),
];

fn fd_conf_from_tables(baudrate: u32, baudrate_fd: u32) -> Result<XlCanFdConf> {
    let find = |table: &[(u32, [u32; 3])], hz: u32| -> Result<[u32; 3]> {
        table
            .iter()
            .find(|(k, _)| *k == hz)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                Error::Invalid(format!("vxlapi: no FD bus params table entry for {hz} Hz"))
            })
    };
    let nominal = find(CAN_BAUD_PARAMS, baudrate)?;
    let data = find(CAN_FD_DATA_PARAMS, baudrate_fd)?;
    Ok(XlCanFdConf {
        arbitration_bit_rate: baudrate,
        sjw_abr: nominal[2],
        tseg1_abr: nominal[0],
        tseg2_abr: nominal[1],
        data_bit_rate: baudrate_fd,
        sjw_dbr: data[2],
        tseg1_dbr: data[0],
        tseg2_dbr: data[1],
        reserved: 0,
        options: 0,
        reserved2: 0,
        reserved3: 0,
    })
}

fn fd_conf_from_config(cfg: &BitRateConfig) -> XlCanFdConf {
    XlCanFdConf {
        arbitration_bit_rate: cfg.nominal.brp as u32,
        sjw_abr: cfg.nominal.sjw as u32,
        tseg1_abr: cfg.nominal.tseg1 as u32,
        tseg2_abr: cfg.nominal.tseg2 as u32,
        data_bit_rate: cfg.data.brp as u32,
        sjw_dbr: cfg.data.sjw as u32,
        tseg1_dbr: cfg.data.tseg1 as u32,
        tseg2_dbr: cfg.data.tseg2 as u32,
        reserved: 0,
        options: if cfg.non_iso {
            XL_CANFD_CONFOPT_NO_ISO
        } else {
            0
        },
        reserved2: 0,
        reserved3: 0,
    }
}

#[derive(Debug, Clone)]
struct XlChannelEntry {
    name: String,
    /// XL_HWTYPE_*.
    hw_type: u8,
    hw_index: u8,
    hw_channel: u8,
    channel_index: u8,
    channel_mask: u64,
    supports_fd: bool,
}

#[derive(Debug)]
struct VxlApi {
    _dll: DllWrapper,
    /// XLstatus xlOpenDriver(void).
    xl_open_driver: unsafe extern "system" fn() -> i32,
    /// XLstatus xlCloseDriver(void).
    xl_close_driver: unsafe extern "system" fn() -> i32,
    /// XLstatus xlGetDriverConfig(XLdriverConfig *pxDriverConfig).
    xl_get_driver_config: unsafe extern "system" fn(*mut XlDriverConfig) -> i32,
    /// XLstatus xlSetApplConfig(const char *appName, XLuint appChannel, XLuint
    /// hwType, XLuint hwIndex, XLuint hwChannel, XLuint busType).
    xl_set_appl_config: unsafe extern "system" fn(*const u8, u32, u32, u32, u32, u32) -> i32,
    /// XLstatus xlOpenPort(XLportHandle *portHandle, const char *appName, XLaccess
    /// accessMask, XLaccess *permissionMask, XLuint rxQueueSize, XLuint
    /// xlInterfaceVersion, XLuint busType).
    xl_open_port:
        unsafe extern "system" fn(*mut i32, *const u8, u64, *mut u64, u32, u32, u32) -> i32,
    /// XLstatus xlClosePort(XLportHandle portHandle).
    xl_close_port: unsafe extern "system" fn(i32) -> i32,
    /// XLstatus xlActivateChannel(XLportHandle, XLaccess, XLuint busType, XLuint flags).
    xl_activate_channel: unsafe extern "system" fn(i32, u64, u32, u32) -> i32,
    /// XLstatus xlDeactivateChannel(XLportHandle, XLaccess).
    xl_deactivate_channel: unsafe extern "system" fn(i32, u64) -> i32,
    /// XLstatus xlCanTransmit(XLportHandle, XLaccess, XLuint *msgCnt, void *pMsg).
    xl_can_transmit: unsafe extern "system" fn(i32, u64, *mut u32, *const XlEvent) -> i32,
    /// XLstatus xlReceive(XLportHandle, XLuint *pEventCount, XLevent *pEvents).
    xl_receive: unsafe extern "system" fn(i32, *mut u32, *mut XlEvent) -> i32,
    /// XLstatus xlCanSetChannelBitrate(XLportHandle, XLaccess, XLuint bitrate).
    xl_can_set_channel_bitrate: unsafe extern "system" fn(i32, u64, u32) -> i32,
    /// XLstatus xlCanSetChannelParamsC200(XLportHandle, unsigned char btr0,
    /// unsigned char btr1).
    xl_can_set_channel_params_c200: unsafe extern "system" fn(i32, u8, u8) -> i32,
    /// XLstatus xlCanSetChannelParams(XLportHandle, XLaccess, XLcanFdConf*)——
    xl_can_set_channel_params: Option<unsafe extern "system" fn(i32, u64, *mut XlCanFdConf) -> i32>,
    /// XLstatus xlCanTransmitEx(XLportHandle, XLaccess, XLuint msgCnt, XLuint
    /// *pMsgCntSent, XLCAN_TX_MSG *pMsg).
    xl_can_transmit_ex:
        Option<unsafe extern "system" fn(i32, u64, u32, *mut u32, *mut XlCanTxMsg) -> i32>,
    /// XLstatus xlCanReceive(XLportHandle, XLCAN_RX_MSG *pxCanRxMsg).
    xl_can_receive: Option<unsafe extern "system" fn(i32, *mut XlCanRxMsg) -> i32>,
}

impl VxlApi {
    fn load() -> Result<Self> {
        Self::load_from(VXLAPI_DLL)
    }

    fn load_from(path: &str) -> Result<Self> {
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
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
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                let r = unsafe { dll.library().get::<$ty>($name) };
                r.ok().map(|s| *s)
            }};
        }
        let api = Self {
            xl_open_driver: sym!(b"xlOpenDriver\0", unsafe extern "system" fn() -> i32),
            xl_close_driver: sym!(b"xlCloseDriver\0", unsafe extern "system" fn() -> i32),
            xl_get_driver_config: sym!(
                b"xlGetDriverConfig\0",
                unsafe extern "system" fn(*mut XlDriverConfig) -> i32
            ),
            xl_set_appl_config: sym!(
                b"xlSetApplConfig\0",
                unsafe extern "system" fn(*const u8, u32, u32, u32, u32, u32) -> i32
            ),
            xl_open_port: sym!(
                b"xlOpenPort\0",
                unsafe extern "system" fn(*mut i32, *const u8, u64, *mut u64, u32, u32, u32) -> i32
            ),
            xl_close_port: sym!(b"xlClosePort\0", unsafe extern "system" fn(i32) -> i32),
            xl_activate_channel: sym!(
                b"xlActivateChannel\0",
                unsafe extern "system" fn(i32, u64, u32, u32) -> i32
            ),
            xl_deactivate_channel: sym!(
                b"xlDeactivateChannel\0",
                unsafe extern "system" fn(i32, u64) -> i32
            ),
            xl_can_transmit: sym!(
                b"xlCanTransmit\0",
                unsafe extern "system" fn(i32, u64, *mut u32, *const XlEvent) -> i32
            ),
            xl_receive: sym!(
                b"xlReceive\0",
                unsafe extern "system" fn(i32, *mut u32, *mut XlEvent) -> i32
            ),
            xl_can_set_channel_bitrate: sym!(
                b"xlCanSetChannelBitrate\0",
                unsafe extern "system" fn(i32, u64, u32) -> i32
            ),
            xl_can_set_channel_params_c200: sym!(
                b"xlCanSetChannelParamsC200\0",
                unsafe extern "system" fn(i32, u8, u8) -> i32
            ),
            xl_can_set_channel_params: opt_sym!(
                b"xlCanSetChannelParams\0",
                unsafe extern "system" fn(i32, u64, *mut XlCanFdConf) -> i32
            ),
            xl_can_transmit_ex: opt_sym!(
                b"xlCanTransmitEx\0",
                unsafe extern "system" fn(i32, u64, u32, *mut u32, *mut XlCanTxMsg) -> i32
            ),
            xl_can_receive: opt_sym!(
                b"xlCanReceive\0",
                unsafe extern "system" fn(i32, *mut XlCanRxMsg) -> i32
            ),
            _dll: dll,
        };
        if VXLAPI_REFCOUNT.fetch_add(1, Ordering::SeqCst) == 0 {
            let st = unsafe { (api.xl_open_driver)() };
            check(st, "", "xlOpenDriver")?;
        }
        Ok(api)
    }

    fn has_fd_support(&self) -> bool {
        self.xl_can_set_channel_params.is_some()
            && self.xl_can_transmit_ex.is_some()
            && self.xl_can_receive.is_some()
    }
}

impl Drop for VxlApi {
    fn drop(&mut self) {
        if VXLAPI_REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            unsafe { (self.xl_close_driver)() };
        }
    }
}

pub struct VectorCan {
    core: DeviceCore,
    api: VxlApi,
    port: i32,
    access_mask: u64,
    channel_index: u8,
    fd: bool,
    bus_id: String,
    rx_queue: VecDeque<CanFrame>,
}

impl VectorCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        self.close_port_internal();
    }

    pub fn new() -> Result<Self> {
        let api = VxlApi::load()?;
        Ok(Self {
            core: DeviceCore::new(),
            api,
            port: -1,
            access_mask: 0,
            channel_index: 0,
            fd: false,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.port >= 0
    }

    fn channel_entries(&self) -> Result<Vec<XlChannelEntry>> {
        let mut cfg: XlDriverConfig = unsafe { std::mem::zeroed() };
        let st = unsafe { (self.api.xl_get_driver_config)(&mut cfg) };
        check(st, "", "xlGetDriverConfig")?;
        let channel_arr = cfg.channel;
        let count = (cfg.channel_count as usize).min(channel_arr.len());
        let mut out = Vec::new();
        for ch in &channel_arr[..count] {
            let (hw_type, hw_index, hw_channel, channel_index, channel_mask, caps, bus_type) = (
                ch.hw_type,
                ch.hw_index,
                ch.hw_channel,
                ch.channel_index,
                ch.channel_mask,
                ch.channel_capabilities,
                ch.connected_bus_type,
            );
            if bus_type != XL_BUS_TYPE_CAN {
                continue;
            }
            let name = ch.name;
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            out.push(XlChannelEntry {
                name: String::from_utf8_lossy(&name[..end]).into_owned(),
                hw_type,
                hw_index,
                hw_channel,
                channel_index,
                channel_mask,
                supports_fd: caps
                    & (XL_CHANNEL_FLAG_CANFD_ISO_SUPPORT | XL_CHANNEL_FLAG_CANFD_BOSCH_SUPPORT)
                    != 0,
            });
        }
        Ok(out)
    }

    fn close_port_internal(&mut self) {
        if self.port >= 0 {
            unsafe {
                (self.api.xl_deactivate_channel)(self.port, self.access_mask);
                (self.api.xl_close_port)(self.port);
            }
            self.port = -1;
            self.rx_queue.clear();
        }
    }

    fn build_tx_event(channel_index: u8, can_id: u32, data: &[u8]) -> XlEvent {
        let mut bytes = [0u8; 8];
        bytes[..data.len()].copy_from_slice(data);
        XlEvent {
            tag: XL_TRANSMIT_MSG,
            chan_index: 1u8 << channel_index,
            trans_id: 0,
            port_handle: 0,
            flags: 0,
            reserved: 0,
            time_stamp: 0,
            id: can_id,
            msg_flags: 0,
            dlc: data.len() as u16,
            res1: 0,
            data: u64::from_le_bytes(bytes),
            res2: 0,
        }
    }

    fn build_tx_msg(
        channel_index: u8,
        can_id: u32,
        data: &[u8],
        frame_type: FrameType,
    ) -> Result<XlCanTxMsg> {
        let mut msg_flags = 0u32;
        if frame_type.contains(FrameType::FD) {
            msg_flags |= XL_CAN_MSG_FLAG_EDL;
        }
        if frame_type.contains(FrameType::BRS) {
            msg_flags |= XL_CAN_MSG_FLAG_BRS;
        }
        let mut buf = [0u8; 64];
        buf[..data.len()].copy_from_slice(data);
        Ok(XlCanTxMsg {
            tag: XL_CAN_EV_TAG_TX_MSG,
            channel_index: 0xFFFF,
            user_handle: channel_index,
            flags_reserved: 0,
            reserved: 0,
            can_id,
            msg_flags,
            dlc: length_to_dlc(data.len())?,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            data: buf,
        })
    }
}

#[async_trait]
impl CanDevice for VectorCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        Ok(self.port >= 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.port >= 0 {
            return Ok(true);
        }
        // `format_bus_id("Vector", channel)`.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Vector", config.channel));
        let want_hw_type = config.hardware_type as u8;
        let channels = self.channel_entries()?;
        let found = channels
            .iter()
            .find(|c| c.hw_type == want_hw_type && c.hw_channel == config.channel as u8)
            .cloned()
            .ok_or_else(|| {
                config_err(
                    &self.bus_id,
                    format!(
                        "vxlapi: no CAN channel with hwType={} hwChannel={} in driver config",
                        want_hw_type, config.channel
                    ),
                )
            })?;
        let app_name = CString::new(format!("autors-can_{}", config.channel))
            .map_err(|e| Error::Invalid(format!("app name: {e}")))?;
        let st = unsafe {
            (self.api.xl_set_appl_config)(
                app_name.as_ptr() as *const u8,
                0,
                found.hw_type as u32,
                found.hw_index as u32,
                found.hw_channel as u32,
                XL_BUS_TYPE_CAN,
            )
        };
        check(st, &self.bus_id, "xlSetApplConfig")?;

        self.fd = config.is_fd();
        if self.fd && !self.api.has_fd_support() {
            return Err(config_err(
                &self.bus_id,
                "vxlapi: CAN FD requested but xlCanSetChannelParams/xlCanTransmitEx/xlCanReceive not available",
            ));
        }
        let access_mask = found.channel_mask;
        let mut permission_mask: u64 = 0;
        let mut port: i32 = -1;
        let (queue_size, version) = if self.fd {
            (RX_QUEUE_SIZE_FD, XL_INTERFACE_VERSION_V4)
        } else {
            (RX_QUEUE_SIZE_CLASSIC, XL_INTERFACE_VERSION_V3)
        };
        let st = unsafe {
            (self.api.xl_open_port)(
                &mut port,
                app_name.as_ptr() as *const u8,
                access_mask,
                &mut permission_mask,
                queue_size,
                version,
                XL_BUS_TYPE_CAN,
            )
        };
        check(st, &self.bus_id, "xlOpenPort")?;
        self.port = port;
        self.access_mask = access_mask;
        self.channel_index = found.channel_index;

        let result = (|| -> Result<()> {
            if self.fd {
                if permission_mask != access_mask {
                    return Err(config_err(
                        &self.bus_id,
                        "vxlapi: xlOpenPort denied init access (permissionMask != accessMask)",
                    ));
                }
                let mut conf = match &config.fd_bit_rate_config {
                    Some(custom) => fd_conf_from_config(custom),
                    None => {
                        fd_conf_from_tables(config.baudrate.as_u32(), config.baudrate_fd.as_u32())?
                    }
                };
                let set_params = self.api.xl_can_set_channel_params.ok_or_else(|| {
                    config_err(&self.bus_id, "vxlapi: xlCanSetChannelParams unavailable")
                })?;
                let st = unsafe { set_params(port, access_mask, &mut conf) };
                check(st, &self.bus_id, "xlCanSetChannelParams")?;
            } else if config.baudrate != CanBaudrate::NotSet {
                let st = unsafe {
                    (self.api.xl_can_set_channel_bitrate)(
                        port,
                        access_mask,
                        config.baudrate.as_u32(),
                    )
                };
                check(st, &self.bus_id, "xlCanSetChannelBitrate")?;
            }
            unsafe { (self.api.xl_can_set_channel_params_c200)(port, 1, 1) };
            let st = unsafe {
                (self.api.xl_activate_channel)(
                    port,
                    access_mask,
                    XL_BUS_TYPE_CAN,
                    XL_ACTIVATE_RESET_CLOCK,
                )
            };
            check(st, &self.bus_id, "xlActivateChannel")
        })();
        if let Err(e) = result {
            self.close_port_internal();
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        if self.port < 0 {
            return Ok(0);
        }
        if self.fd {
            if data.len() > MAX_FD_DLC {
                return Err(Error::Invalid(format!(
                    "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
                    data.len()
                )));
            }
        } else if data.len() > MAX_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
                data.len()
            )));
        }
        let mut retries = 0u32;
        loop {
            let mut sent: u32 = 1;
            let st = if self.fd {
                let mut msg = Self::build_tx_msg(self.channel_index, can_id, data, frame_type)?;
                let transmit_ex = self.api.xl_can_transmit_ex.ok_or_else(|| {
                    config_err(&self.bus_id, "vxlapi: xlCanTransmitEx unavailable")
                })?;
                unsafe { transmit_ex(self.port, self.access_mask, 1, &mut sent, &mut msg) }
            } else {
                let evt = Self::build_tx_event(self.channel_index, can_id, data);
                unsafe { (self.api.xl_can_transmit)(self.port, self.access_mask, &mut sent, &evt) }
            };
            match st {
                XL_SUCCESS => {
                    if sent != 1 {
                        return Ok(0);
                    }
                    let frame =
                        CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
                    return Ok(self.core.record_sent(&frame));
                }
                XL_ERR_QUEUE_IS_FULL => {
                    retries += 1;
                    if retries > TX_QUEUE_FULL_MAX_RETRIES {
                        return Ok(0);
                    }
                    unsafe {
                        (self.api.xl_deactivate_channel)(self.port, self.access_mask);
                        (self.api.xl_activate_channel)(
                            self.port,
                            self.access_mask,
                            XL_BUS_TYPE_CAN,
                            XL_ACTIVATE_RESET_CLOCK,
                        );
                    }
                }
                _ => return Ok(0),
            }
        }
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if self.port < 0 {
            return Ok(None);
        }
        if self.fd {
            let can_receive = self
                .api
                .xl_can_receive
                .ok_or_else(|| config_err(&self.bus_id, "vxlapi: xlCanReceive unavailable"))?;
            loop {
                let mut msg: XlCanRxMsg = unsafe { std::mem::zeroed() };
                msg.size = std::mem::size_of::<XlCanRxMsg>() as u32;
                let st = unsafe { can_receive(self.port, &mut msg) };
                if st != XL_SUCCESS {
                    break;
                }
                let (tag, can_id, msg_flags, dlc) = (msg.tag, msg.can_id, msg.msg_flags, msg.dlc);
                let Ok(len) = dlc_to_length(dlc) else {
                    continue;
                };
                if len == 0 {
                    continue;
                }
                if tag != XL_CAN_EV_TAG_RX_OK {
                    // `send` when `tx_timeout > 0` and reports the send as failed on
                    // timeout; this port always reports success once the driver queue
                    // accepts the frame. Needs real hardware to validate a fix.
                    continue;
                }
                let mut frame_type = FrameType::CAN20B;
                if msg_flags & XL_CAN_MSG_FLAG_EDL != 0 {
                    frame_type = frame_type | FrameType::FD;
                }
                if msg_flags & XL_CAN_MSG_FLAG_BRS != 0 {
                    frame_type = frame_type | FrameType::BRS;
                }
                let data = msg.data;
                self.rx_queue.push_back(CanFrame::new(
                    &self.bus_id,
                    can_id,
                    data[..len as usize].to_vec(),
                    false,
                    frame_type,
                ));
            }
        } else {
            loop {
                let mut evt: XlEvent = unsafe { std::mem::zeroed() };
                let mut count: u32 = 1;
                let st = unsafe { (self.api.xl_receive)(self.port, &mut count, &mut evt) };
                if st != XL_SUCCESS || count == 0 {
                    break;
                }
                let (tag, id, msg_flags, dlc) = (evt.tag, evt.id, evt.msg_flags, evt.dlc);
                if dlc == 0 || tag != XL_RECEIVE_MSG {
                    continue;
                }
                if msg_flags & XL_CAN_MSG_FLAG_TX_COMPLETED != 0 {
                    continue;
                }
                if msg_flags != 0 {
                    continue;
                }
                let len = (dlc as usize).min(8);
                self.rx_queue.push_back(CanFrame::new(
                    &self.bus_id,
                    id,
                    evt.data.to_le_bytes()[..len].to_vec(),
                    false,
                    FrameType::CAN20B,
                ));
            }
        }
        Ok(self.rx_queue.pop_front())
    }

    async fn available_channels(&self) -> Result<Vec<ChannelInfo>> {
        Ok(self
            .channel_entries()?
            .into_iter()
            .map(|e| ChannelInfo {
                channel: e.hw_channel as i32,
                hardware_type: i32::from(e.hw_type),
                name: e.name,
                supports_fd: e.supports_fd,
            })
            .collect())
    }
}

impl Drop for VectorCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::BitRatePar;

    #[test]
    fn struct_sizes_have_expected_layout() {
        assert_eq!(std::mem::size_of::<XlEvent>(), 50);
        assert_eq!(std::mem::size_of::<XlCanTxMsg>(), 88);
        assert_eq!(std::mem::size_of::<XlCanRxMsg>(), 128);
        assert_eq!(std::mem::size_of::<XlCanFdConf>(), 40);
        assert_eq!(std::mem::size_of::<XlChannelConfig>(), 227);
        assert_eq!(std::mem::size_of::<XlDriverConfig>(), 14576);
    }

    #[test]
    fn status_names() {
        assert_eq!(xl_status_name(0), "XL_SUCCESS");
        assert_eq!(xl_status_name(XL_ERR_QUEUE_IS_FULL), "XL_ERR_QUEUE_IS_FULL");
        assert_eq!(xl_status_name(255), "XL_ERROR");
        assert_eq!(xl_status_name(518), "XL_ERR_UNKNOWN_FLAG");
        assert_eq!(xl_status_name(-1), "XL_ERR_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        match check(113, "Vector/CAN1", "xlOpenPort").unwrap_err() {
            Error::Driver(msg) => {
                assert!(msg.contains("Vector/CAN1"));
                assert!(msg.contains("xlOpenPort"));
                assert!(msg.contains("XL_ERR_PORT_IS_OFFLINE"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn fd_conf_table_lookup() {
        // 500k/2M:nominal {tseg1=119, tseg2=40, sjw=40},data {119, 40, 1}.
        let conf = fd_conf_from_tables(500_000, 2_000_000).unwrap();
        let (arb, t1a, t2a, sjwa, dat, t1d, t2d, sjwd, opts) = (
            conf.arbitration_bit_rate,
            conf.tseg1_abr,
            conf.tseg2_abr,
            conf.sjw_abr,
            conf.data_bit_rate,
            conf.tseg1_dbr,
            conf.tseg2_dbr,
            conf.sjw_dbr,
            conf.options,
        );
        assert_eq!(arb, 500_000);
        assert_eq!(t1a, 119);
        assert_eq!(t2a, 40);
        assert_eq!(sjwa, 40);
        assert_eq!(dat, 2_000_000);
        assert_eq!(t1d, 29);
        assert_eq!(t2d, 10);
        assert_eq!(sjwd, 10);
        assert_eq!(opts, 0);
        assert!(matches!(
            fd_conf_from_tables(333_333, 2_000_000),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            fd_conf_from_tables(500_000, 3_000_000),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn fd_conf_from_custom_config() {
        let cfg = BitRateConfig {
            clock: 40,
            nominal: BitRatePar {
                brp: 5,
                tseg1: 11,
                tseg2: 4,
                sjw: 1,
            },
            data: BitRatePar {
                brp: 2,
                tseg1: 14,
                tseg2: 5,
                sjw: 1,
            },
            non_iso: true,
        };
        let conf = fd_conf_from_config(&cfg);
        let (arb, t1a, dat, t1d, opts) = (
            conf.arbitration_bit_rate,
            conf.tseg1_abr,
            conf.data_bit_rate,
            conf.tseg1_dbr,
            conf.options,
        );
        assert_eq!(arb, 5);
        assert_eq!(t1a, 11);
        assert_eq!(dat, 2);
        assert_eq!(t1d, 14);
        assert_eq!(opts, XL_CANFD_CONFOPT_NO_ISO);
    }

    #[test]
    fn hw_type_values_match_expected_contract() {
        assert_eq!(HwType::VIRTUAL, 1);
        assert_eq!(HwType::VN1610, 55);
        assert_eq!(HwType::VN1630, 57);
        assert_eq!(HwType::VN7640, 102);
        assert_eq!(HwType::VX1161B, 115);
        assert_eq!(HwType::VN2600, HwType::VN2610);
    }

    #[test]
    fn build_tx_event_fields() {
        let evt = VectorCan::build_tx_event(2, 0x8000_0123, &[0x11, 0x22, 0x33]);
        let (tag, chan_index, id, msg_flags, dlc, data) = (
            evt.tag,
            evt.chan_index,
            evt.id,
            evt.msg_flags,
            evt.dlc,
            evt.data,
        );
        assert_eq!(tag, XL_TRANSMIT_MSG);
        assert_eq!(chan_index, 1 << 2);
        assert_eq!(id, 0x8000_0123);
        assert_eq!(msg_flags, 0);
        assert_eq!(dlc, 3);
        assert_eq!(data, 0x00_00_00_00_00_33_22_11);
    }

    #[test]
    fn build_tx_msg_fields() {
        // V4:tag=0x0440, channelIndex=0xFFFF, FD+BRS -> msgFlags EDL|BRS.
        let msg = VectorCan::build_tx_msg(3, 0x123, &[0xAA; 12], FrameType::FD_BRS).unwrap();
        let (tag, channel_index, user_handle, can_id, msg_flags, dlc, data) = (
            msg.tag,
            msg.channel_index,
            msg.user_handle,
            msg.can_id,
            msg.msg_flags,
            msg.dlc,
            msg.data,
        );
        assert_eq!(tag, 0x0440);
        assert_eq!(channel_index, 0xFFFF);
        assert_eq!(user_handle, 3);
        assert_eq!(can_id, 0x123);
        assert_eq!(msg_flags, XL_CAN_MSG_FLAG_EDL | XL_CAN_MSG_FLAG_BRS);
        assert_eq!(dlc, 9); // length_to_dlc(12)
        assert!(data[..12].iter().all(|&b| b == 0xAA));
        assert!(data[12..].iter().all(|&b| b == 0));
        let msg = VectorCan::build_tx_msg(0, 0x123, &[1; 8], FrameType::CAN20B).unwrap();
        let (msg_flags, dlc) = (msg.msg_flags, msg.dlc);
        assert_eq!(msg_flags, 0);
        assert_eq!(dlc, 8);
    }

    #[test]
    fn missing_dll_is_driver_error() {
        let err = VxlApi::load_from("no_such_vxlapi_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        match VectorCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                for ch in autors_runtime::block_on(dev.available_channels()).unwrap() {
                    assert!(ch.channel >= 0);
                }
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x123, &[1], FrameType::CAN20B)).unwrap(),
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
