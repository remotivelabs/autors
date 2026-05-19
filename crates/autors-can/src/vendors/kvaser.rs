//! Kvaser CAN adapter backed by the Kvaser CANLIB SDK driver.
//! Loads `canlib32.dll` dynamically at runtime. The CANLIB SDK exposes a
//! public C API; all functions use the `__stdcall` calling convention (the
//! Winapi default, which matches the C calling convention on x64), so every
//! function pointer is declared `extern "system"`. Construction returns
//! [`Error::Driver`] when the DLL is not installed or an export is missing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, format_bus_id, CanDevice, ChannelInfo, DeviceCore, CAN_EXT_FLAG, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// Name of the CANLIB driver DLL.
const CANLIB_DLL: &str = "canlib32.dll";

// ---- canlib.h constants (from the public Kvaser CANLIB SDK headers canstat.h/canlib.h) ----
/// canOK.
const CAN_OK: i32 = 0;
/// canOPEN_ACCEPT_VIRTUAL — accept virtual channels (value 32).
const CANOPEN_ACCEPT_VIRTUAL: i32 = 0x0020;
/// canOPEN_CAN_FD — open in CAN FD mode (value 1024).
const CANOPEN_CAN_FD: i32 = 0x0400;
/// canOPEN_CAN_FD_NONISO — non-ISO CAN FD (value 2048).
const CANOPEN_CAN_FD_NONISO: i32 = 0x0800;
/// canMSG_STD.
const CANMSG_STD: u32 = 0x0002;
/// canMSG_EXT.
const CANMSG_EXT: u32 = 0x0004;
/// canFDMSG_FDF.
const CANFDMSG_FDF: u32 = 0x0001_0000;
/// canFDMSG_BRS.
const CANFDMSG_BRS: u32 = 0x0002_0000;
/// Receive filter mask 480 (0x1E0): canMSG_ERROR_FRAME(0x20) | canMSG_TXACK(0x40)
/// | canMSG_TXRQ(0x80) | canMSGERR_OVERRUN(0x100) — error frames and TX echoes are skipped.
const CANLIB_RX_SKIP_FLAGS: u32 = 0x01E0;

/// Static canlib reference count: the first instance calls canInitialize(),
/// the last one calls canUnloadLibrary().
static CANLIB_REFCOUNT: AtomicUsize = AtomicUsize::new(0);

/// Common names of canStatus error codes (matching canlib.h canStatus), used
/// in error messages.
fn status_name(status: i32) -> &'static str {
    match status {
        0 => "canOK",
        -1 => "canERR_PARAM",
        -2 => "canERR_NOMSG",
        -3 => "canERR_NOTFOUND",
        -4 => "canERR_NOMEM",
        -5 => "canERR_NOCHANNELS",
        -7 => "canERR_TIMEOUT",
        -8 => "canERR_NOTINITIALIZED",
        -9 => "canERR_NOHANDLES",
        -10 => "canERR_INVHANDLE",
        -11 => "canERR_INIFILE",
        -12 => "canERR_DRIVER",
        -13 => "canERR_TXBUFOFL",
        -15 => "canERR_HARDWARE",
        -16 => "canERR_DYNALOAD",
        -17 => "canERR_DYNALIB",
        -18 => "canERR_DYNAINIT",
        -19 => "canERR_NOT_SUPPORTED",
        -23 => "canERR_DRIVERLOAD",
        -24 => "canERR_DRIVERFAILED",
        -25 => "canERR_NOCONFIGMGR",
        -26 => "canERR_NOCARD",
        -28 => "canERR_REGISTRY",
        -29 => "canERR_LICENSE",
        -30 => "canERR_INTERNAL",
        -31 => "canERR_NO_ACCESS",
        -32 => "canERR_NOT_IMPLEMENTED",
        _ => "canERR_???",
    }
}

/// Maps a canStatus != canOK to [`Error::Driver`]; the hardware ID is merged
/// in by the caller via [`config_err`].
fn check(status: i32, bus_id: &str, what: &str) -> Result<()> {
    if status == CAN_OK {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "canlib32: {what} failed: {} ({status})",
                status_name(status)
            ),
        ))
    }
}

/// Maps an FD data bitrate (Hz) to a canlib predefined constant
/// (canFD_BITRATE_500K=-1000 ... canFD_BITRATE_8M=-1004).
/// Returns [`Error::Invalid`] for unsupported bitrates.
fn fd_bitrate_const(hz: u32) -> Result<i32> {
    match hz {
        500_000 => Ok(-1000),
        1_000_000 => Ok(-1001),
        2_000_000 => Ok(-1002),
        4_000_000 => Ok(-1003),
        8_000_000 => Ok(-1004),
        _ => Err(Error::Invalid(format!(
            "canlib32: no predefined FD bitrate constant for {hz} Hz \
             (supported: 500k/1M/2M/4M/8M)"
        ))),
    }
}

/// Open flags: `32 | (fd ? (non_iso ? 2048 : 1024) : 0)`.
fn open_flags(config: &CanConfiguration) -> i32 {
    let mut flags = CANOPEN_ACCEPT_VIRTUAL;
    if config.is_fd() {
        let non_iso = config
            .fd_bit_rate_config
            .as_ref()
            .is_some_and(|c| c.non_iso);
        flags |= if non_iso {
            CANOPEN_CAN_FD_NONISO
        } else {
            CANOPEN_CAN_FD
        };
    }
    flags
}

/// Computes the canWrite flags for a transmitted frame.
fn tx_msg_flags(can_id: u32, frame_type: FrameType, fd_opened: bool) -> u32 {
    let mut flags = if can_id & CAN_EXT_FLAG != 0 {
        CANMSG_EXT
    } else {
        CANMSG_STD
    };
    if fd_opened && !frame_type.is_classic() {
        if frame_type.contains(FrameType::FD) {
            flags |= CANFDMSG_FDF;
        }
        if frame_type.contains(FrameType::BRS) {
            flags |= CANFDMSG_BRS;
        }
    }
    flags
}

/// Receive-loop frame filter: `(flags & 480) == 0 && id != 0xFFFFFFFF && dlc != 0 && dlc <= 64`.
fn accept_rx_frame(flags: u32, id: u32, dlc: u32) -> bool {
    flags & CANLIB_RX_SKIP_FLAGS == 0 && id != u32::MAX && dlc != 0 && dlc <= 64
}

/// Function-pointer table for canlib32. All symbols are loaded and validated
/// at construction; a missing symbol is reported as an error immediately
/// instead of failing later at call time.
/// Field types follow canlib.h exactly (`long` is 32-bit on Windows).
#[derive(Debug)]
struct Canlib32 {
    /// Keeps the library handle alive (`DllWrapper` from autors-native); never accessed directly.
    _dll: DllWrapper,
    /// void canInitialize(void).
    can_initialize: unsafe extern "system" fn(),
    /// canStatus canUnloadLibrary(void).
    can_unload_library: unsafe extern "system" fn() -> i32,
    /// int canOpenChannel(int channel, int flags).
    can_open_channel: unsafe extern "system" fn(i32, i32) -> i32,
    /// canStatus canClose(const int hnd).
    can_close: unsafe extern "system" fn(i32) -> i32,
    /// canStatus canBusOn(const int hnd).
    can_bus_on: unsafe extern "system" fn(i32) -> i32,
    /// canStatus canBusOff(const int hnd).
    can_bus_off: unsafe extern "system" fn(i32) -> i32,
    /// canStatus canSetBitrate(const int hnd, const int bitrate).
    can_set_bitrate: unsafe extern "system" fn(i32, i32) -> i32,
    /// canStatus canSetBusParams(const int hnd, const int freq, const unsigned int
    /// tseg1, const unsigned int tseg2, const unsigned int sjw, const unsigned int
    /// noSamp, const unsigned int syncmode).
    can_set_bus_params: unsafe extern "system" fn(i32, i32, u32, u32, u32, u32, u32) -> i32,
    /// canStatus canSetBusParamsFd(const int hnd, const int freq_brs, const
    /// unsigned int tseg1, const unsigned int tseg2, const unsigned int sjw).
    can_set_bus_params_fd: unsafe extern "system" fn(i32, i32, u32, u32, u32) -> i32,
    /// canStatus canGetNumberOfChannels(int *channelCount).
    can_get_number_of_channels: unsafe extern "system" fn(*mut i32) -> i32,
    /// canStatus canWrite(const int hnd, long id, void *msg, unsigned int dlc,
    /// unsigned int flag).
    can_write: unsafe extern "system" fn(i32, i32, *const u8, u32, u32) -> i32,
    /// canStatus canRead(const int hnd, long *id, void *msg, unsigned int *dlc,
    /// unsigned int *flag, unsigned long *time).
    can_read:
        unsafe extern "system" fn(i32, *mut i32, *mut u8, *mut u32, *mut u32, *mut u32) -> i32,
    /// canStatus canReadWait(const int hnd, const int timeout).
    can_read_wait: unsafe extern "system" fn(i32, i32) -> i32,
}

impl Canlib32 {
    /// Loads canlib32.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(CANLIB_DLL)
    }

    /// Loads from the given path/name and resolves all exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe DllMain execution) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, keeping the pointers valid. The generic T is a
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
        let api = Self {
            can_initialize: sym!(b"canInitialize\0", unsafe extern "system" fn()),
            can_unload_library: sym!(b"canUnloadLibrary\0", unsafe extern "system" fn() -> i32),
            can_open_channel: sym!(
                b"canOpenChannel\0",
                unsafe extern "system" fn(i32, i32) -> i32
            ),
            can_close: sym!(b"canClose\0", unsafe extern "system" fn(i32) -> i32),
            can_bus_on: sym!(b"canBusOn\0", unsafe extern "system" fn(i32) -> i32),
            can_bus_off: sym!(b"canBusOff\0", unsafe extern "system" fn(i32) -> i32),
            can_set_bitrate: sym!(
                b"canSetBitrate\0",
                unsafe extern "system" fn(i32, i32) -> i32
            ),
            can_set_bus_params: sym!(
                b"canSetBusParams\0",
                unsafe extern "system" fn(i32, i32, u32, u32, u32, u32, u32) -> i32
            ),
            can_set_bus_params_fd: sym!(
                b"canSetBusParamsFd\0",
                unsafe extern "system" fn(i32, i32, u32, u32, u32) -> i32
            ),
            can_get_number_of_channels: sym!(
                b"canGetNumberOfChannels\0",
                unsafe extern "system" fn(*mut i32) -> i32
            ),
            can_write: sym!(
                b"canWrite\0",
                unsafe extern "system" fn(i32, i32, *const u8, u32, u32) -> i32
            ),
            can_read: sym!(
                b"canRead\0",
                unsafe extern "system" fn(
                    i32,
                    *mut i32,
                    *mut u8,
                    *mut u32,
                    *mut u32,
                    *mut u32,
                ) -> i32
            ),
            can_read_wait: sym!(b"canReadWait\0", unsafe extern "system" fn(i32, i32) -> i32),
            _dll: dll,
        };
        // The first instance runs canInitialize().
        if CANLIB_REFCOUNT.fetch_add(1, Ordering::SeqCst) == 0 {
            // SAFETY: the function pointer comes from the loaded canlib32.dll;
            // no parameters, no return value.
            unsafe { (api.can_initialize)() };
        }
        Ok(api)
    }
}

impl Drop for Canlib32 {
    fn drop(&mut self) {
        // The last instance runs canUnloadLibrary().
        if CANLIB_REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            // SAFETY: paired with canInitialize; the library handle is released
            // only after this struct is dropped.
            unsafe { (self.can_unload_library)() };
        }
    }
}

/// Kvaser CAN channel adapter.
/// Reception follows the non-blocking contract of [`CanDevice::receive`]:
/// a zero-timeout canReadWait probe is followed by draining canRead; blocking
/// waits are left to the polling cadence of the upper layer
/// (`crate::device::start_dispatch`).
pub struct KvaserCan {
    core: DeviceCore,
    api: Canlib32,
    /// canlib channel handle (< 0 when not open).
    handle: i32,
    /// Whether the channel was opened in FD mode.
    fd_opened: bool,
    /// Bus ID (generated at open, or taken from the configuration).
    bus_id: String,
    /// Receive-frame buffer queue.
    rx_queue: VecDeque<CanFrame>,
}

impl KvaserCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle >= 0 {
            // canBusOff + canClose (both return values are intentionally ignored).
            // SAFETY: handle is valid.
            unsafe {
                (self.api.can_bus_off)(self.handle);
                (self.api.can_close)(self.handle);
            }
            self.handle = -1;
            self.rx_queue.clear();
        }
    }

    /// Loads canlib32.dll and validates the symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: Canlib32::load()?,
            handle: -1,
            fd_opened: false,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle >= 0
    }

    /// Bitrate configuration (called after the channel has been opened).
    fn configure_bitrate(&self, config: &CanConfiguration) -> Result<()> {
        if config.baudrate == CanBaudrate::NotSet {
            // Baudrate <= CanBaudrate::NotSet means "leave the driver default"; nothing is set.
            return Ok(());
        }
        if self.fd_opened {
            if let Some(custom) = &config.fd_bit_rate_config {
                // Custom FD bit rates: canSetBusParams(nominal..., noSamp=1, syncmode=0)
                // + canSetBusParamsFd(data...).
                let n = custom.nominal;
                // SAFETY: handle was returned by canOpenChannel (>=0); all arguments passed by value.
                let st = unsafe {
                    (self.api.can_set_bus_params)(
                        self.handle,
                        n.brp,
                        n.tseg1 as u32,
                        n.tseg2 as u32,
                        n.sjw as u32,
                        1,
                        0,
                    )
                };
                check(st, &self.bus_id, "canSetBusParams")?;
                let d = custom.data;
                // SAFETY: same as above.
                let st = unsafe {
                    (self.api.can_set_bus_params_fd)(
                        self.handle,
                        d.brp,
                        d.tseg1 as u32,
                        d.tseg2 as u32,
                        d.sjw as u32,
                    )
                };
                check(st, &self.bus_id, "canSetBusParamsFd")?;
            } else {
                // Predefined constants: nominal from Baudrate, data from BaudrateFD.
                let nominal = fd_bitrate_const(config.baudrate.as_u32())?;
                // SAFETY: same as above; tseg/sjw = 0 lets the driver compute
                // them from the preset.
                let st =
                    unsafe { (self.api.can_set_bus_params)(self.handle, nominal, 0, 0, 0, 0, 0) };
                check(st, &self.bus_id, "canSetBusParams")?;
                let data = fd_bitrate_const(config.baudrate_fd.as_u32())?;
                // SAFETY: same as above.
                let st = unsafe { (self.api.can_set_bus_params_fd)(self.handle, data, 0, 0, 0) };
                check(st, &self.bus_id, "canSetBusParamsFd")?;
            }
        } else {
            // canSetBitrate(handle, (int)Baudrate) — the enum's integer value is
            // the rate in Hz; canlib accepts Hz or negative preset constants.
            // SAFETY: handle is valid; arguments passed by value.
            let st =
                unsafe { (self.api.can_set_bitrate)(self.handle, config.baudrate.as_u32() as i32) };
            check(st, &self.bus_id, "canSetBitrate")?;
        }
        Ok(())
    }
}

#[async_trait]
impl CanDevice for KvaserCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // open() performs the actual open sequence; is_available() only reports state.
        Ok(self.handle >= 0)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle >= 0 {
            return Ok(true);
        }
        // Generate the BusId ("Kvaser/CAN1").
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Kvaser", config.channel));
        self.fd_opened = config.is_fd();
        let flags = open_flags(&config);
        // SAFETY: arguments passed by value; returns a channel handle
        // (< 0 is a canStatus error code).
        let handle = unsafe { (self.api.can_open_channel)(config.channel, flags) };
        if handle < 0 {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "canlib32: canOpenChannel(channel={}, flags=0x{flags:X}) failed: {} ({handle})",
                    config.channel,
                    status_name(handle)
                ),
            ));
        }
        self.handle = handle;
        // On configuration failure the channel is closed before the error is returned.
        let result = (|| {
            self.configure_bitrate(&config)?;
            // SAFETY: handle is valid.
            let st = unsafe { (self.api.can_bus_on)(self.handle) };
            check(st, &self.bus_id, "canBusOn")
        })();
        if let Err(e) = result {
            // SAFETY: handle is valid.
            unsafe { (self.api.can_close)(self.handle) };
            self.handle = -1;
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // Returns 0 when the channel is not open.
        if self.handle < 0 {
            return Ok(0);
        }
        if data.len() > MAX_FD_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds CAN FD maximum of {MAX_FD_DLC}",
                data.len()
            )));
        }
        let flags = tx_msg_flags(can_id, frame_type, self.fd_opened);
        let id = (can_id & !CAN_EXT_FLAG) as i32;
        // SAFETY: handle is valid; the data pointer is valid for the duration
        // of the call and dlc matches the actual buffer; canlib sends
        // synchronously and does not retain the pointer.
        let st = unsafe {
            (self.api.can_write)(self.handle, id, data.as_ptr(), data.len() as u32, flags)
        };
        // A canWrite status != canOK returns 0 (no error is raised).
        if st != CAN_OK {
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
        if self.handle < 0 {
            return Ok(None);
        }
        // Per the trait's non-blocking contract, poll with a zero-timeout
        // canReadWait and then drain canRead.
        // SAFETY: handle is valid; arguments passed by value.
        let st = unsafe { (self.api.can_read_wait)(self.handle, 0) };
        if st != CAN_OK {
            // canERR_NOMSG / canERR_TIMEOUT: no frame; other errors are not
            // fatal either (any non-OK status simply yields no frame).
            return Ok(None);
        }
        let mut buf = [0u8; 65];
        loop {
            let mut id: i32 = 0;
            let mut dlc: u32 = 0;
            let mut flags: u32 = 0;
            let mut time: u32 = 0;
            // SAFETY: handle is valid; buf is 65 bytes >= canlib's maximum DLC
            // of 64 + 1; all out pointers point to variables on this stack frame.
            let st = unsafe {
                (self.api.can_read)(
                    self.handle,
                    &mut id,
                    buf.as_mut_ptr(),
                    &mut dlc,
                    &mut flags,
                    &mut time,
                )
            };
            if st != CAN_OK {
                break;
            }
            let id = id as u32;
            if accept_rx_frame(flags, id, dlc) {
                let mut id = id;
                if flags & CANMSG_EXT != 0 {
                    id |= CAN_EXT_FLAG;
                }
                let mut frame_type = FrameType::CAN20B;
                if flags & CANFDMSG_FDF != 0 {
                    frame_type = frame_type | FrameType::FD;
                }
                if flags & CANFDMSG_BRS != 0 {
                    frame_type = frame_type | FrameType::BRS;
                }
                self.rx_queue.push_back(CanFrame::new(
                    &self.bus_id,
                    id,
                    buf[..dlc as usize].to_vec(),
                    false,
                    frame_type,
                ));
            }
        }
        Ok(self.rx_queue.pop_front())
    }

    async fn available_channels(&self) -> Result<Vec<ChannelInfo>> {
        let mut count = 0i32;
        // SAFETY: `count` is a valid writable integer for the duration of the
        // CANLIB call. The driver has been initialized by `Canlib32::load`.
        let status = unsafe { (self.api.can_get_number_of_channels)(&mut count) };
        check(status, "Kvaser", "canGetNumberOfChannels")?;
        if count < 0 {
            return Err(Error::Driver(format!(
                "Kvaser: canGetNumberOfChannels returned invalid count {count}"
            )));
        }
        Ok((0..count)
            .map(|channel| ChannelInfo {
                channel,
                hardware_type: 0,
                name: format!("Kvaser CAN {}", channel + 1),
                // CANLIB's count API does not expose per-channel FD capability.
                supports_fd: false,
            })
            .collect())
    }
}

impl Drop for KvaserCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{BitRateConfig, CanFdBaudrate};

    #[test]
    fn fd_bitrate_const_table() {
        // The complete FD bitrate mapping table.
        assert_eq!(fd_bitrate_const(500_000).unwrap(), -1000);
        assert_eq!(fd_bitrate_const(1_000_000).unwrap(), -1001);
        assert_eq!(fd_bitrate_const(2_000_000).unwrap(), -1002);
        assert_eq!(fd_bitrate_const(4_000_000).unwrap(), -1003);
        assert_eq!(fd_bitrate_const(8_000_000).unwrap(), -1004);
        // Bitrates outside the table -> Invalid.
        assert!(matches!(fd_bitrate_const(125_000), Err(Error::Invalid(_))));
        assert!(matches!(fd_bitrate_const(0), Err(Error::Invalid(_))));
    }

    #[test]
    fn open_flags_computation() {
        // Classic CAN: canOPEN_ACCEPT_VIRTUAL(32) only.
        let c = CanConfiguration::default();
        assert_eq!(open_flags(&c), 32);
        // FD ISO: 32 | 1024.
        let c = CanConfiguration::new(0, CanBaudrate::B500Kbit, CanFdBaudrate::B2Mbit);
        assert_eq!(open_flags(&c), 32 | 1024);
        // FD non-ISO: 32 | 2048.
        let c = CanConfiguration::with_bit_rate_config(
            0,
            BitRateConfig {
                non_iso: true,
                ..Default::default()
            },
        );
        assert_eq!(open_flags(&c), 32 | 2048);
    }

    #[test]
    fn tx_msg_flags_computation() {
        // Standard frame, classic mode: canMSG_STD.
        assert_eq!(tx_msg_flags(0x123, FrameType::CAN20B, false), CANMSG_STD);
        // Extended frame: canMSG_EXT.
        assert_eq!(
            tx_msg_flags(0x123 | CAN_EXT_FLAG, FrameType::CAN20B, false),
            CANMSG_EXT
        );
        // FD opened + FD|BRS: canFDMSG_FDF/BRS are added.
        assert_eq!(
            tx_msg_flags(CAN_EXT_FLAG, FrameType::FD_BRS, true),
            CANMSG_EXT | CANFDMSG_FDF | CANFDMSG_BRS
        );
        // FD flags are ignored when the channel was not opened in FD mode.
        assert_eq!(tx_msg_flags(0x123, FrameType::FD_BRS, false), CANMSG_STD);
    }

    #[test]
    fn rx_accept_filter() {
        // Normal frame.
        assert!(accept_rx_frame(0, 0x123, 8));
        assert!(accept_rx_frame(CANMSG_EXT, 0x123, 64));
        // Error frames / TX echoes: any bit in 0x1E0.
        assert!(!accept_rx_frame(0x20, 0x123, 8));
        assert!(!accept_rx_frame(0x40, 0x123, 8));
        assert!(!accept_rx_frame(0x80, 0x123, 8));
        assert!(!accept_rx_frame(0x100, 0x123, 8));
        // Invalid ID / DLC.
        assert!(!accept_rx_frame(0, u32::MAX, 8));
        assert!(!accept_rx_frame(0, 0x123, 0));
        assert!(!accept_rx_frame(0, 0x123, 65));
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(0), "canOK");
        assert_eq!(status_name(-2), "canERR_NOMSG");
        assert_eq!(status_name(-26), "canERR_NOCARD");
        assert_eq!(status_name(-9999), "canERR_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        let err = check(-7, "Kvaser/CAN1", "canReadWait").unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("Kvaser/CAN1"));
                assert!(msg.contains("canReadWait"));
                assert!(msg.contains("canERR_TIMEOUT"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield Error::Driver, not a panic.
        let err = Canlib32::load_from("no_such_kvaser_canlib_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // On a machine without the Kvaser driver installed: Error::Driver
        // (canlib32.dll missing); on a machine with the driver installed:
        // construction must succeed with a complete symbol table. Both
        // outcomes are acceptable — the key point is that it must not panic.
        match KvaserCan::new() {
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
