//!   xlLinSendRequest / xlLinSetSlave / xlLinSwitchSlave / xlReceive /
//!   xlOpenPort / xlClosePort / xlActivateChannel / xlDeactivateChannel /
//!   xlSetNotification / xlFlushReceiveQueue);

use std::collections::VecDeque;
use std::ffi::CString;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autors_native::DllWrapper;

use super::{config_err, next_unique_bus_id};
use crate::device::{
    make_bus_id, ChecksumType, LinConfiguration, LinDevice, LinFrame, LinVersion, MAX_DATA_LEN,
};
use crate::error::{Error, Result};

#[cfg(target_pointer_width = "64")]
const VXLAPI_DLL: &str = "vxlapi64.dll";
#[cfg(target_pointer_width = "32")]
const VXLAPI_DLL: &str = "vxlapi.dll";

/// XL_SUCCESS.
const XL_SUCCESS: i32 = 0;
const XL_BUS_TYPE_LIN: u32 = 2;
const XL_INTERFACE_VERSION_V3: u32 = 3;
const XL_ACTIVATE_RESET_CLOCK: u32 = 8;
const RX_QUEUE_SIZE_LIN: u32 = 256;
const XL_LIN_MASTER: u32 = 1;
const XL_LIN_MSG: u8 = 20;
const XL_LIN_FLAG_SKIP_MASK: u8 = 0x81;
const XL_LIN_FLAG_TX: u8 = 0x40;
const XL_LIN_CHECKSUM_CLASSIC: u16 = 256;
const XL_LIN_CHECKSUM_ENHANCED: u16 = 512;

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
        129 => "XL_ERR_HW_NOT_PRESENT",
        131 => "XL_ERR_NOTIFY_ALREADY_ACTIVE",
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
        216 => "XL_ERR_QUEUE_OVERRUN",
        255 => "XL_ERROR",
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

fn xl_lin_version(version: LinVersion) -> u32 {
    match version {
        LinVersion::V1_3 => 1,
        LinVersion::V2_0 => 2,
        LinVersion::V2_1 => 3,
    }
}

/// CALC_CHECKSUM_ENHANCED→512(XL_LIN_CHECKSUM_*).
fn xl_checksum_const(checksum_type: ChecksumType) -> u16 {
    match checksum_type {
        ChecksumType::CalcChecksum => XL_LIN_CHECKSUM_CLASSIC,
        ChecksumType::CalcChecksumEnhanced => XL_LIN_CHECKSUM_ENHANCED,
    }
}

fn xl_checksum_for_id(id: u8, checksum_type: ChecksumType) -> u16 {
    if matches!(id & 0x3f, 0x3c | 0x3d) {
        XL_LIN_CHECKSUM_CLASSIC
    } else {
        xl_checksum_const(checksum_type)
    }
}

fn accept_lin_flags(flags: u8) -> bool {
    flags & XL_LIN_FLAG_SKIP_MASK == 0
}

fn is_master_frame(flags: u8) -> bool {
    flags & XL_LIN_FLAG_TX != 0
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct XlLinChannelParams {
    /// XL_LIN_MASTER(1)/XL_LIN_SLAVE(0).
    lin_mode: u32,
    bitrate: u32,
    /// XL_LIN_VERSION_1_3(1)/2_0(2)/2_1(3).
    lin_version: u32,
    reserved: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct XlLinEvent {
    tag: u8,
    chan_index: u8,
    trans_id: u16,
    port_handle: u16,
    flags: u8,
    reserved: u8,
    time_stamp: u64,
    /// linID(0) dlc(1) flags(2) data[8](3..11) crc(11).
    tag_data: [u8; 34],
}

impl Default for XlLinEvent {
    fn default() -> Self {
        Self {
            tag: 0,
            chan_index: 0,
            trans_id: 0,
            port_handle: 0,
            flags: 0,
            reserved: 0,
            time_stamp: 0,
            tag_data: [0; 34],
        }
    }
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

const _: () = assert!(std::mem::size_of::<XlLinChannelParams>() == 16);
const _: () = assert!(std::mem::size_of::<XlLinEvent>() == 50);
const _: () = assert!(std::mem::size_of::<XlChannelConfig>() == 227);
const _: () = assert!(std::mem::size_of::<XlDriverConfig>() == 4 + 4 + 40 + 64 * 227);

#[derive(Debug, Clone)]
struct XlChannelEntry {
    #[allow(dead_code)]
    name: String,
    /// XL_HWTYPE_*.
    hw_type: u8,
    hw_index: u8,
    hw_channel: u8,
    #[allow(dead_code)]
    channel_index: u8,
    channel_mask: u64,
}

#[derive(Debug)]
struct VxlApiLin {
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
    /// XLstatus xlLinSetChannelParams(XLportHandle, XLaccess, XLLinChannelParams*).
    xl_lin_set_channel_params:
        unsafe extern "system" fn(i32, u64, *const XlLinChannelParams) -> i32,
    /// XLstatus xlLinSetDLC(XLportHandle, XLaccess, unsigned char dlc[64]).
    xl_lin_set_dlc: unsafe extern "system" fn(i32, u64, *const u8) -> i32,
    /// XLstatus xlLinSendRequest(XLportHandle, XLaccess, unsigned char linID,
    /// unsigned int flags).
    xl_lin_send_request: unsafe extern "system" fn(i32, u64, u8, u32) -> i32,
    /// XLstatus xlLinSetSlave(XLportHandle, XLaccess, unsigned char linID,
    /// unsigned char data[8], unsigned char dlc, unsigned short checksumType).
    xl_lin_set_slave: unsafe extern "system" fn(i32, u64, u8, *const u8, u8, u16) -> i32,
    /// XLstatus xlLinSwitchSlave(XLportHandle, XLaccess, unsigned char linID,
    /// unsigned char slaveOn).
    #[allow(dead_code)]
    xl_lin_switch_slave: unsafe extern "system" fn(i32, u64, u8, u8) -> i32,
    /// XLstatus xlReceive(XLportHandle, XLuint *pEventCount, XLevent *pEvents).
    xl_receive: unsafe extern "system" fn(i32, *mut u32, *mut XlLinEvent) -> i32,
    /// XLstatus xlFlushReceiveQueue(XLportHandle).
    xl_flush_receive_queue: unsafe extern "system" fn(i32) -> i32,
}

impl VxlApiLin {
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
            xl_lin_set_channel_params: sym!(
                b"xlLinSetChannelParams\0",
                unsafe extern "system" fn(i32, u64, *const XlLinChannelParams) -> i32
            ),
            xl_lin_set_dlc: sym!(
                b"xlLinSetDLC\0",
                unsafe extern "system" fn(i32, u64, *const u8) -> i32
            ),
            xl_lin_send_request: sym!(
                b"xlLinSendRequest\0",
                unsafe extern "system" fn(i32, u64, u8, u32) -> i32
            ),
            xl_lin_set_slave: sym!(
                b"xlLinSetSlave\0",
                unsafe extern "system" fn(i32, u64, u8, *const u8, u8, u16) -> i32
            ),
            xl_lin_switch_slave: sym!(
                b"xlLinSwitchSlave\0",
                unsafe extern "system" fn(i32, u64, u8, u8) -> i32
            ),
            xl_receive: sym!(
                b"xlReceive\0",
                unsafe extern "system" fn(i32, *mut u32, *mut XlLinEvent) -> i32
            ),
            xl_flush_receive_queue: sym!(
                b"xlFlushReceiveQueue\0",
                unsafe extern "system" fn(i32) -> i32
            ),
            _dll: dll,
        };
        if VXLAPI_REFCOUNT.fetch_add(1, Ordering::SeqCst) == 0 {
            let st = unsafe { (api.xl_open_driver)() };
            check(st, "", "xlOpenDriver")?;
        }
        let mut cfg: XlDriverConfig = unsafe { std::mem::zeroed() };
        let st = unsafe { (api.xl_get_driver_config)(&mut cfg) };
        check(st, "", "xlGetDriverConfig")?;
        Ok(api)
    }
}

impl Drop for VxlApiLin {
    fn drop(&mut self) {
        if VXLAPI_REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            unsafe { (self.xl_close_driver)() };
        }
    }
}

pub struct VectorLin {
    api: VxlApiLin,
    unique_bus_id: i32,
    port: i32,
    access_mask: u64,
    dlc: u8,
    checksum_type: ChecksumType,
    bus_id: String,
    rx_queue: VecDeque<LinFrame>,
}

impl VectorLin {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: VxlApiLin::load()?,
            unique_bus_id: next_unique_bus_id(),
            port: -1,
            access_mask: 0,
            dlc: 0,
            checksum_type: ChecksumType::default(),
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
            let (hw_type, hw_index, hw_channel, channel_index, channel_mask, bus_type) = (
                ch.hw_type,
                ch.hw_index,
                ch.hw_channel,
                ch.channel_index,
                ch.channel_mask,
                ch.connected_bus_type,
            );
            if bus_type != XL_BUS_TYPE_LIN {
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
            });
        }
        Ok(out)
    }

    /// Deactivates and closes the current port, then clears queued frames.
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
}

#[async_trait]
impl LinDevice for VectorLin {
    fn unique_bus_id(&self) -> i32 {
        self.unique_bus_id
    }

    fn is_available(&self) -> bool {
        self.port >= 0
    }

    async fn open(&mut self, config: &LinConfiguration) -> Result<bool> {
        if self.port >= 0 {
            return Ok(true);
        }
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| make_bus_id("Vector", config.channel));
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
                        "vxlapi: no LIN channel with hwType={} hwChannel={} in driver config",
                        want_hw_type, config.channel
                    ),
                )
            })?;
        let app_name = CString::new(format!("autors-lin_{}", config.channel))
            .map_err(|e| Error::Invalid(format!("app name: {e}")))?;
        let st = unsafe {
            (self.api.xl_set_appl_config)(
                app_name.as_ptr() as *const u8,
                0,
                found.hw_type as u32,
                found.hw_index as u32,
                found.hw_channel as u32,
                XL_BUS_TYPE_LIN,
            )
        };
        check(st, &self.bus_id, "xlSetApplConfig")?;

        let access_mask = found.channel_mask;
        let mut permission_mask: u64 = 0;
        let mut port: i32 = -1;
        let st = unsafe {
            (self.api.xl_open_port)(
                &mut port,
                app_name.as_ptr() as *const u8,
                access_mask,
                &mut permission_mask,
                RX_QUEUE_SIZE_LIN,
                XL_INTERFACE_VERSION_V3,
                XL_BUS_TYPE_LIN,
            )
        };
        check(st, &self.bus_id, "xlOpenPort")?;
        self.port = port;
        self.access_mask = access_mask;
        self.dlc = config.dlc;
        self.checksum_type = config.checksum_type;

        let result = (|| -> Result<()> {
            let params = XlLinChannelParams {
                lin_mode: XL_LIN_MASTER,
                bitrate: u32::from(config.baudrate),
                lin_version: xl_lin_version(config.version),
                reserved: 0,
            };
            let st = unsafe { (self.api.xl_lin_set_channel_params)(port, access_mask, &params) };
            check(st, &self.bus_id, "xlLinSetChannelParams")?;
            let dlc_tab = [config.dlc; 64];
            let st = unsafe { (self.api.xl_lin_set_dlc)(port, access_mask, dlc_tab.as_ptr()) };
            check(st, &self.bus_id, "xlLinSetDLC")?;
            let zeros = [0u8; 8];
            let st = unsafe {
                (self.api.xl_lin_set_slave)(
                    port,
                    access_mask,
                    config.master_id,
                    zeros.as_ptr(),
                    config.dlc,
                    XL_LIN_CHECKSUM_CLASSIC,
                )
            };
            check(st, &self.bus_id, "xlLinSetSlave")?;
            let st = unsafe {
                (self.api.xl_activate_channel)(
                    port,
                    access_mask,
                    XL_BUS_TYPE_LIN,
                    XL_ACTIVATE_RESET_CLOCK,
                )
            };
            check(st, &self.bus_id, "xlActivateChannel")?;
            let st = unsafe { (self.api.xl_flush_receive_queue)(port) };
            check(st, &self.bus_id, "xlFlushReceiveQueue")
        })();
        if let Err(e) = result {
            self.close_port_internal();
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_port_internal();
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> Result<usize> {
        if self.port < 0 {
            return Ok(0);
        }
        if data.len() > usize::from(self.dlc) || data.len() > MAX_DATA_LEN {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds LIN DLC {} (max {MAX_DATA_LEN})",
                data.len(),
                self.dlc
            )));
        }
        let mut buf = [0u8; MAX_DATA_LEN];
        buf[..data.len()].copy_from_slice(data);
        let st = unsafe {
            (self.api.xl_lin_set_slave)(
                self.port,
                self.access_mask,
                id,
                buf.as_ptr(),
                data.len() as u8,
                xl_checksum_for_id(id, self.checksum_type),
            )
        };
        if st != XL_SUCCESS {
            return Ok(0);
        }
        let st = unsafe { (self.api.xl_lin_send_request)(self.port, self.access_mask, id, 0) };
        if st != XL_SUCCESS {
            return Ok(0);
        }
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> Result<bool> {
        if self.port < 0 {
            return Ok(false);
        }
        let status = unsafe { (self.api.xl_lin_send_request)(self.port, self.access_mask, id, 0) };
        Ok(status == XL_SUCCESS)
    }

    async fn on_receive(&mut self) -> Result<Option<LinFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if self.port < 0 {
            return Ok(None);
        }
        loop {
            let mut count: u32 = 1;
            let mut evt = XlLinEvent::default();
            let st = unsafe { (self.api.xl_receive)(self.port, &mut count, &mut evt) };
            if st != XL_SUCCESS {
                break;
            }
            let tag = evt.tag;
            if tag != XL_LIN_MSG {
                continue;
            }
            let tag_data = evt.tag_data;
            let (frame_id, dlc, flags) = (tag_data[0], tag_data[1], tag_data[2]);
            if !accept_lin_flags(flags) {
                continue;
            }
            if dlc > MAX_DATA_LEN as u8 {
                continue;
            }
            let mut data = [0u8; MAX_DATA_LEN];
            data.copy_from_slice(&tag_data[3..11]);
            let frame = LinFrame::with_len(
                &self.bus_id,
                frame_id,
                data.to_vec(),
                dlc,
                is_master_frame(flags),
            )?;
            self.rx_queue.push_back(frame);
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for VectorLin {
    fn drop(&mut self) {
        self.close_port_internal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_have_expected_layout() {
        assert_eq!(std::mem::size_of::<XlLinChannelParams>(), 16);
        assert_eq!(std::mem::size_of::<XlLinEvent>(), 50);
        assert_eq!(std::mem::size_of::<XlChannelConfig>(), 227);
        assert_eq!(std::mem::size_of::<XlDriverConfig>(), 4 + 4 + 40 + 64 * 227);
    }

    #[test]
    fn lin_version_mapping() {
        assert_eq!(xl_lin_version(LinVersion::V1_3), 1);
        assert_eq!(xl_lin_version(LinVersion::V2_0), 2);
        assert_eq!(xl_lin_version(LinVersion::V2_1), 3);
    }

    #[test]
    fn checksum_const_mapping() {
        assert_eq!(xl_checksum_const(ChecksumType::CalcChecksum), 256);
        assert_eq!(xl_checksum_const(ChecksumType::CalcChecksumEnhanced), 512);
        assert_eq!(
            xl_checksum_for_id(0x3D, ChecksumType::CalcChecksumEnhanced),
            XL_LIN_CHECKSUM_CLASSIC
        );
    }

    #[test]
    fn lin_rx_flags() {
        assert!(accept_lin_flags(0));
        assert!(accept_lin_flags(0x40));
        assert!(!accept_lin_flags(0x01));
        assert!(!accept_lin_flags(0x80));
        assert!(!accept_lin_flags(0x81));
        assert!(is_master_frame(0x40));
        assert!(!is_master_frame(0));
        assert!(!is_master_frame(0x01));
    }

    #[test]
    fn status_names() {
        assert_eq!(xl_status_name(0), "XL_SUCCESS");
        assert_eq!(xl_status_name(118), "XL_ERR_INVALID_PORT");
        assert_eq!(xl_status_name(202), "XL_ERR_WRONG_BUS_TYPE");
        assert_eq!(xl_status_name(-9999), "XL_ERR_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        match check(118, "Vector/LIN1", "xlOpenPort").unwrap_err() {
            Error::Driver(msg) => {
                assert!(msg.contains("Vector/LIN1"));
                assert!(msg.contains("xlOpenPort"));
                assert!(msg.contains("XL_ERR_INVALID_PORT"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_dll_is_driver_error() {
        let err = VxlApiLin::load_from("no_such_vxlapi_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn load_real_driver_or_driver_error() {
        match VectorLin::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!dev.is_available());
                assert!(dev.unique_bus_id() >= 1);
                assert_eq!(autors_runtime::block_on(dev.send(0x3C, &[1])).unwrap(), 0);
                assert!(autors_runtime::block_on(dev.on_receive())
                    .unwrap()
                    .is_none());
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
