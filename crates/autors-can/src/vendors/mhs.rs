//! MHS Elektronik Tiny-CAN adapter (MHSCAN).
//! Loads `mhstcan.dll` (the MHS Tiny-CAN canLib API) dynamically at runtime.
//! All entry points use `extern "system"` (the Winapi/stdcall convention the
//! canLib API expects). If the DLL is not installed or a required export is
//! missing, construction returns [`Error::Driver`].
//! The driver provides nine core exports (`CanInitDriver`/`CanDownDriver`/
//! `CanDeviceOpen`/`CanDeviceClose`/`CanTransmit`/`CanReceive`/`CanSetSpeed`/
//! `CanSetMode`/`CanSetEvents`) plus three optional ones
//! (`CanSetRxEventCallbackFct`, `CanFdTransmit`, `CanFdReceive`) that older
//! driver builds may lack; the optional symbols are loaded as `Option`, and
//! features depending on them report a clear error when unavailable.
//! Deliberate behavioral choices:
//! - Classic CAN transmit validates the payload length: frames longer than
//!   8 bytes return [`Error::Invalid`] instead of writing a truncated DLC
//!   (`len & 0xF`) and letting the driver read out of bounds.
//! - Whether transmit takes the CAN FD path is decided solely by the FD
//!   baudrate configured at open time (`BaudrateFD != NotUsed`), not by the
//!   per-frame `FrameType`.

use std::collections::VecDeque;
use std::ffi::c_char;
use std::sync::Mutex;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, format_bus_id, length_to_dlc, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_DLC,
    MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType};

/// mhstcan.dll(MHS Tiny-CAN canLib).
const MHSTCAN_DLL: &str = "mhstcan.dll";

const INIT_CONFIG: &[u8] = b"CanCallThread=0\0";
const EVENT_RX_ENABLE: u16 = 8;
const EVENT_DISABLE_ALL: u16 = 0xFF00;
const MODE_START: u8 = 1;
const ACCEPT_ALL: u16 = 0x0FFF;

const CANMSG_LEN_MASK: u32 = 0x0F;
const CANMSG_EXT: u32 = 0x80;
const CANMSG_RX_SKIP: u32 = 0x30;
const CANFD_EXT: u32 = 0x0008_0000;
const CANFD_FDF: u32 = 0x0010_0000;
const CANFD_BRS: u32 = 0x0020_0000;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct Reserved {
    r0: u32,
    r1: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct CanMsg {
    id: u32,
    flags: u32,
    data: u64,
    reserved: Reserved,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CanFdMsg {
    id: u32,
    /// DLC << 8 | bit19 ext | bit20 FD | bit21 BRS(L119-131).
    flags: u32,
    data: [u8; 64],
    reserved: Reserved,
}

impl Default for CanFdMsg {
    fn default() -> Self {
        Self {
            id: 0,
            flags: 0,
            data: [0; 64],
            reserved: Reserved::default(),
        }
    }
}

type RxCallback = unsafe extern "system" fn(u32, *const CanMsg, i32);

fn tx_flags_classic(can_id: u32, len: usize) -> u32 {
    let mut flags = len as u32 & CANMSG_LEN_MASK;
    if can_id & CAN_EXT_FLAG != 0 {
        flags |= CANMSG_EXT;
    }
    flags
}

fn tx_flags_fd(can_id: u32, len: usize, frame_type: FrameType) -> Result<u32> {
    let mut flags = (length_to_dlc(len)? as u32) << 8;
    if can_id & CAN_EXT_FLAG != 0 {
        flags |= CANFD_EXT;
    }
    if frame_type.contains(FrameType::FD) {
        flags |= CANFD_FDF;
    }
    if frame_type.contains(FrameType::BRS) {
        flags |= CANFD_BRS;
    }
    Ok(flags)
}

fn accept_rx_flags(flags: u32) -> bool {
    flags & CANMSG_LEN_MASK != 0 && flags & CANMSG_RX_SKIP == 0
}

fn speed_kbit(baudrate: CanBaudrate) -> u16 {
    (baudrate.as_u32() / 1000) as u16
}

struct RxSink {
    bus_id: String,
    queue: VecDeque<CanFrame>,
}

static RX_SINK: Mutex<Option<RxSink>> = Mutex::new(None);

unsafe extern "system" fn rx_event_callback(_idx: u32, msg: *const CanMsg, _count: i32) {
    let msg = match unsafe { msg.as_ref() } {
        Some(m) => *m,
        None => return,
    };
    if !accept_rx_flags(msg.flags) {
        return;
    }
    let len = (msg.flags & CANMSG_LEN_MASK) as usize;
    let len = len.min(MAX_DLC);
    let mut id = msg.id;
    if msg.flags & CANMSG_EXT != 0 {
        id |= CAN_EXT_FLAG;
    }
    let data = msg.data.to_le_bytes()[..len].to_vec();
    if let Ok(mut guard) = RX_SINK.lock() {
        if let Some(sink) = guard.as_mut() {
            let bus_id = sink.bus_id.clone();
            sink.queue
                .push_back(CanFrame::new(bus_id, id, data, false, FrameType::CAN20B));
        }
    }
}

#[derive(Debug)]
struct MhsTCan {
    _dll: DllWrapper,
    /// int CanInitDriver(string config).
    can_init_driver: unsafe extern "system" fn(*const c_char) -> i32,
    /// void CanDownDriver(void).
    can_down_driver: unsafe extern "system" fn(),
    can_device_open: unsafe extern "system" fn(*mut u32, *const c_char) -> i32,
    /// int CanDeviceClose(uint index).
    can_device_close: unsafe extern "system" fn(u32) -> i32,
    /// int CanTransmit(uint index, ref CAN_MSG msg, int count).
    can_transmit: unsafe extern "system" fn(u32, *const CanMsg, i32) -> i32,
    /// int CanReceive(uint index, ref CAN_MSG msg, int count).
    #[allow(dead_code)]
    can_receive: unsafe extern "system" fn(u32, *mut CanMsg, i32) -> i32,
    /// int CanSetSpeed(uint index, ushort speedKbit).
    can_set_speed: unsafe extern "system" fn(u32, u16) -> i32,
    /// int CanSetMode(uint index, byte mode, ushort filter).
    can_set_mode: unsafe extern "system" fn(u32, u8, u16) -> i32,
    /// void CanSetEvents(ushort events).
    can_set_events: unsafe extern "system" fn(u16),
    can_set_rx_event_callback_fct: Option<unsafe extern "system" fn(RxCallback) -> i32>,
    can_fd_transmit: Option<unsafe extern "system" fn(u32, *const CanFdMsg, i32) -> i32>,
    #[allow(dead_code)]
    can_fd_receive: Option<unsafe extern "system" fn(u32, *mut CanFdMsg, i32) -> i32>,
}

impl MhsTCan {
    fn load() -> Result<Self> {
        Self::load_from(MHSTCAN_DLL)
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
        Ok(Self {
            can_init_driver: sym!(
                b"CanInitDriver\0",
                unsafe extern "system" fn(*const c_char) -> i32
            ),
            can_down_driver: sym!(b"CanDownDriver\0", unsafe extern "system" fn()),
            can_device_open: sym!(
                b"CanDeviceOpen\0",
                unsafe extern "system" fn(*mut u32, *const c_char) -> i32
            ),
            can_device_close: sym!(b"CanDeviceClose\0", unsafe extern "system" fn(u32) -> i32),
            can_transmit: sym!(
                b"CanTransmit\0",
                unsafe extern "system" fn(u32, *const CanMsg, i32) -> i32
            ),
            can_receive: sym!(
                b"CanReceive\0",
                unsafe extern "system" fn(u32, *mut CanMsg, i32) -> i32
            ),
            can_set_speed: sym!(b"CanSetSpeed\0", unsafe extern "system" fn(u32, u16) -> i32),
            can_set_mode: sym!(
                b"CanSetMode\0",
                unsafe extern "system" fn(u32, u8, u16) -> i32
            ),
            can_set_events: sym!(b"CanSetEvents\0", unsafe extern "system" fn(u16)),
            can_set_rx_event_callback_fct: opt_sym!(
                b"CanSetRxEventCallbackFct\0",
                unsafe extern "system" fn(RxCallback) -> i32
            ),
            can_fd_transmit: opt_sym!(
                b"CanFdTransmit\0",
                unsafe extern "system" fn(u32, *const CanFdMsg, i32) -> i32
            ),
            can_fd_receive: opt_sym!(
                b"CanFdReceive\0",
                unsafe extern "system" fn(u32, *mut CanFdMsg, i32) -> i32
            ),
            _dll: dll,
        })
    }
}

pub struct MhsCan {
    core: DeviceCore,
    api: MhsTCan,
    device: u32,
    driver_initialized: bool,
    fd_opened: bool,
    bus_id: String,
}

impl MhsCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.device != 0 {
            unsafe {
                (self.api.can_set_events)(EVENT_DISABLE_ALL);
                (self.api.can_device_close)(self.device);
            }
            self.device = 0;
        }
        if self.driver_initialized {
            self.driver_initialized = false;
            unsafe { (self.api.can_down_driver)() };
        }
        if let Ok(mut guard) = RX_SINK.lock() {
            *guard = None;
        }
    }

    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: MhsTCan::load()?,
            device: 0,
            driver_initialized: false,
            fd_opened: false,
            bus_id: String::new(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.device != 0
    }

    fn fail(&self, what: &str, code: i32) -> Error {
        config_err(
            &self.bus_id,
            format!("mhstcan: {what} failed, error={code}"),
        )
    }
}

#[async_trait]
impl CanDevice for MhsCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        Ok(self.device != 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.device != 0 {
            return Ok(true);
        }
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("MHS", config.channel));
        self.fd_opened = config.baudrate_fd != CanFdBaudrate::NotUsed;
        let result = (|| {
            if !self.driver_initialized {
                let st = unsafe { (self.api.can_init_driver)(INIT_CONFIG.as_ptr().cast()) };
                if st != 0 {
                    return Err(self.fail("open driver", st));
                }
                self.driver_initialized = true;
            }
            if let Some(set_cb) = self.api.can_set_rx_event_callback_fct {
                unsafe { set_cb(rx_event_callback) };
            }
            unsafe { (self.api.can_set_events)(EVENT_RX_ENABLE) };
            let mut idx: u32 = 0;
            let st = unsafe { (self.api.can_device_open)(&mut idx, std::ptr::null()) };
            if st != 0 {
                return Err(self.fail("open CAN device", st));
            }
            self.device = idx;
            if config.baudrate != CanBaudrate::NotSet {
                let st = unsafe { (self.api.can_set_speed)(0, speed_kbit(config.baudrate)) };
                if st != 0 {
                    return Err(self.fail(
                        &format!("set baudrate to {}", config.baudrate.cs_name()),
                        st,
                    ));
                }
            }
            let st = unsafe { (self.api.can_set_mode)(0, MODE_START, ACCEPT_ALL) };
            if st != 0 {
                return Err(self.fail("set start mode", st));
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.close_sync();
            return Err(e);
        }
        if let Ok(mut guard) = RX_SINK.lock() {
            *guard = Some(RxSink {
                bus_id: self.bus_id.clone(),
                queue: VecDeque::new(),
            });
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        if self.device == 0 {
            return Ok(0);
        }
        if data.len() > MAX_FD_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
                data.len()
            )));
        }
        let st = if self.fd_opened {
            let msg = CanFdMsg {
                id: can_id,
                flags: tx_flags_fd(can_id, data.len(), frame_type)?,
                data: {
                    let mut buf = [0u8; 64];
                    buf[..data.len()].copy_from_slice(data);
                    buf
                },
                reserved: Reserved::default(),
            };
            match self.api.can_fd_transmit {
                Some(fd_tx) => unsafe { fd_tx(0, &msg, 1) },
                None => {
                    return Err(Error::Driver(
                        "mhstcan.dll: CanFdTransmit export missing, cannot send CAN FD frames"
                            .to_string(),
                    ))
                }
            }
        } else {
            if data.len() > MAX_DLC {
                return Err(Error::Invalid(format!(
                    "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
                    data.len()
                )));
            }
            let mut payload = [0u8; 8];
            payload[..data.len()].copy_from_slice(data);
            let msg = CanMsg {
                id: can_id,
                flags: tx_flags_classic(can_id, data.len()),
                data: u64::from_le_bytes(payload),
                reserved: Reserved::default(),
            };
            unsafe { (self.api.can_transmit)(0, &msg, 1) }
        };
        if st != 0 {
            return Ok(0);
        }
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if self.device == 0 {
            return Ok(None);
        }
        let mut guard = RX_SINK.lock().unwrap_or_else(|p| p.into_inner());
        Ok(guard.as_mut().and_then(|sink| sink.queue.pop_front()))
    }
}

impl Drop for MhsCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layout_sizes() {
        assert_eq!(std::mem::size_of::<CanMsg>(), 24);
        assert_eq!(std::mem::align_of::<CanMsg>(), 8);
        assert_eq!(std::mem::size_of::<CanFdMsg>(), 80);
        assert_eq!(std::mem::align_of::<CanFdMsg>(), 4);
    }

    #[test]
    fn tx_flags_classic_computation() {
        assert_eq!(tx_flags_classic(0x123, 8), 8);
        assert_eq!(tx_flags_classic(0x123, 0), 0);
        assert_eq!(tx_flags_classic(0x123 | CAN_EXT_FLAG, 3), 0x80 | 3);
        assert_eq!(tx_flags_classic(CAN_EXT_FLAG, 1), 0x81);
    }

    #[test]
    fn tx_flags_fd_computation() {
        // L119-131:DLC << 8;bit19 ext, bit20 FD, bit21 BRS.
        assert_eq!(tx_flags_fd(0x123, 8, FrameType::CAN20B).unwrap(), 8 << 8);
        assert_eq!(
            tx_flags_fd(0x123 | CAN_EXT_FLAG, 8, FrameType::FD_BRS).unwrap(),
            (8 << 8) | CANFD_EXT | CANFD_FDF | CANFD_BRS
        );
        assert_eq!(
            tx_flags_fd(0x123, 12, FrameType::FD).unwrap(),
            (9 << 8) | CANFD_FDF
        );
        assert!(matches!(
            tx_flags_fd(0x123, 65, FrameType::FD_BRS),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn rx_accept_filter() {
        assert!(accept_rx_flags(8));
        assert!(accept_rx_flags(0x80 | 1));
        assert!(!accept_rx_flags(0)); // DLC 0
        assert!(!accept_rx_flags(0x10)); // bit4
        assert!(!accept_rx_flags(0x20)); // bit5
        assert!(!accept_rx_flags(0x30 | 8));
    }

    #[test]
    fn speed_kbit_conversion() {
        // L242:Baudrate(Hz) / 1000.
        assert_eq!(speed_kbit(CanBaudrate::B500Kbit), 500);
        assert_eq!(speed_kbit(CanBaudrate::B125Kbit), 125);
        assert_eq!(speed_kbit(CanBaudrate::B1Mbit), 1000);
        assert_eq!(speed_kbit(CanBaudrate::B10Kbit), 10);
    }

    #[test]
    fn rx_callback_enqueues_and_filters() {
        {
            let mut guard = RX_SINK.lock().unwrap();
            *guard = Some(RxSink {
                bus_id: "MHS/CAN1".to_string(),
                queue: VecDeque::new(),
            });
        }
        let mut payload = [0u8; 8];
        payload[..3].copy_from_slice(&[1, 2, 3]);
        let msg = CanMsg {
            id: 0x123,
            flags: 3,
            data: u64::from_le_bytes(payload),
            reserved: Reserved::default(),
        };
        unsafe { rx_event_callback(0, &msg, 1) };
        let ext = CanMsg {
            id: 0x1FFF_FFFF,
            flags: 0x80 | 8,
            ..msg
        };
        unsafe { rx_event_callback(0, &ext, 1) };
        let dlc0 = CanMsg { flags: 0, ..msg };
        unsafe { rx_event_callback(0, &dlc0, 1) };
        let err = CanMsg {
            flags: 0x20 | 8,
            ..msg
        };
        unsafe { rx_event_callback(0, &err, 1) };
        unsafe { rx_event_callback(0, std::ptr::null(), 1) };

        let mut guard = RX_SINK.lock().unwrap();
        let sink = guard.as_mut().unwrap();
        assert_eq!(sink.queue.len(), 2);
        let f = sink.queue.pop_front().unwrap();
        assert_eq!(f.bus_id, "MHS/CAN1");
        assert_eq!(f.id, 0x123);
        assert_eq!(f.data, vec![1, 2, 3]);
        assert!(!f.is_master_frame);
        assert_eq!(f.frame_type, FrameType::CAN20B);
        let f = sink.queue.pop_front().unwrap();
        assert_eq!(f.id, 0x1FFF_FFFF | CAN_EXT_FLAG);
        *guard = None;
    }

    #[test]
    fn fail_maps_error() {
        let err = config_err("MHS/CAN1", "mhstcan: open driver failed, error=3");
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("MHS/CAN1"));
                assert!(msg.contains("open driver"));
                assert!(msg.contains("error=3"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_dll_is_driver_error() {
        let err = MhsTCan::load_from("no_such_mhs_tcan_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        match MhsCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
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
