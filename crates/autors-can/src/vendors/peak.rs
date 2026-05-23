//! Adapter for PEAK PCAN-Basic hardware.
//! `PCANBasic.dll` (PEAK PCAN-Basic API, a public C API using the
//! `__stdcall` calling convention — PCANBasic.h declares its functions as
//! `TPCANStatus __stdcall`; under x64 this coincides with the C calling
//! convention, so `extern "system"` is used uniformly) is loaded dynamically
//! at runtime. Construction returns [`Error::Driver`] when the DLL is not
//! installed or an export is missing.
//! The struct layouts below are transcribed with byte packing (`Pack = 1`),
//! consistent with PCANBasic.h.

use std::collections::VecDeque;
use std::ffi::CString;
use std::time::Instant;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, dlc_to_length, format_bus_id, length_to_dlc, CanDevice, DeviceCore, CAN_EXT_FLAG,
    MAX_DLC, MAX_FD_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{BitRateConfig, CanConfiguration, CanFrame, FrameType};

/// PCAN-Basic library name.
const PCANBASIC_DLL: &str = "PCANBasic.dll";

// ---- PCANBasic.h constants ----
/// PCAN_ERROR_OK.
const PCAN_OK: u32 = 0x0;
/// PCAN_ERROR_BUSOFF (treated as a successful send).
const PCAN_BUSOFF: u32 = 0x10;
/// PCAN_ERROR_QXMTFULL (transmit queue full; the send path retries on this).
const PCAN_QXMTFULL: u32 = 0x80;
/// PCAN_BUSOFF_AUTORESET parameter number.
const PCAN_BUSOFF_AUTORESET: u8 = 0x07;
/// PCAN_MESSAGE_RTR.
const PCAN_MESSAGE_RTR: u8 = 0x01;
/// PCAN_MESSAGE_EXTENDED.
const PCAN_MESSAGE_EXTENDED: u8 = 0x02;
/// PCAN_MESSAGE_FD.
const PCAN_MESSAGE_FD: u8 = 0x04;
/// PCAN_MESSAGE_BRS.
const PCAN_MESSAGE_BRS: u8 = 0x08;
/// PCAN_MESSAGE_ESI.
const PCAN_MESSAGE_ESI: u8 = 0x10;
/// PCAN_MESSAGE_ERRFRAME.
const PCAN_MESSAGE_ERRFRAME: u8 = 0x40;
/// PCAN_MESSAGE_STATUS.
const PCAN_MESSAGE_STATUS: u8 = 0x80;
/// Send-retry window in ms (a 50 ms polling loop).
const TX_RETRY_WINDOW_MS: u128 = 50;
/// Value of a not-yet-opened channel handle (ushort.MaxValue).
const INVALID_HANDLE: u16 = u16::MAX;

/// Human-readable name for a TPCANStatus code (PCANBasic.h).
fn peak_status_name(status: u32) -> &'static str {
    match status {
        0x0 => "PCAN_ERROR_OK",
        0x1 => "PCAN_ERROR_XMTFULL",
        0x2 => "PCAN_ERROR_OVERRUN",
        0x4 => "PCAN_ERROR_BUSLIGHT",
        0x8 => "PCAN_ERROR_BUSHEAVY",
        0x10 => "PCAN_ERROR_BUSOFF",
        0x20 => "PCAN_ERROR_QRCVEMPTY",
        0x40 => "PCAN_ERROR_QOVERRUN",
        0x80 => "PCAN_ERROR_QXMTFULL",
        0x100 => "PCAN_ERROR_REGTEST",
        0x200 => "PCAN_ERROR_NODRIVER",
        0x400 => "PCAN_ERROR_HWINUSE",
        0x800 => "PCAN_ERROR_NETINUSE",
        0x1400 => "PCAN_ERROR_ILLHW",
        0x1800 => "PCAN_ERROR_ILLNET",
        0x1C00 => "PCAN_ERROR_ILLCLIENT",
        0x2000 => "PCAN_ERROR_RESOURCE",
        0x4000 => "PCAN_ERROR_ILLPARAMTYPE",
        0x8000 => "PCAN_ERROR_ILLPARAMVAL",
        0x10000 => "PCAN_ERROR_UNKNOWN",
        0x20000 => "PCAN_ERROR_ILLDATA",
        0x2000000 => "PCAN_ERROR_CAUTION",
        0x4000000 => "PCAN_ERROR_INITIALIZE",
        0x8000000 => "PCAN_ERROR_ILLOPERATION",
        _ => "PCAN_ERROR_???",
    }
}

/// Maps a status other than PCAN_ERROR_OK to [`Error::Driver`]; the hardware
/// ID is merged in by the caller via [`config_err`].
fn check(status: u32, bus_id: &str, what: &str) -> Result<()> {
    if status == PCAN_OK {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "PCANBasic: {what} failed: {} (0x{status:X})",
                peak_status_name(status)
            ),
        ))
    }
}

/// PEAK hardware type identifiers (values are the PCAN_XXXBUS constants from
/// PCANBasic.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
#[allow(dead_code)] // Public hardware type table for configuring CanConfiguration::hardware_type
pub enum PeakHwType {
    /// ISA interface.
    IsaBus = 0x21,
    /// Dongle/LPT interface.
    DngBus = 0x31,
    /// PCI interface.
    PciBus = 0x41,
    /// USB interface.
    UsbBus = 0x51,
    /// PC Card interface.
    PccBus = 0x61,
    /// PCAN Virtual hardware (not used by PCAN-Basic).
    PcanVirtual = 0x701,
    /// LAN interface.
    LanBus = 0x801,
}

/// Channel handle = HardwareType + Channel
/// (e.g. USBBUS(0x51) + 0 -> PCAN_USBBUS1).
fn channel_handle(hardware_type: i32, channel: i32) -> u16 {
    (hardware_type + channel) as u16
}

/// HwType parameter (TPCANType) for CAN_Initialize:
/// 33 (ISABUS) -> PCAN_TYPE_ISA(1), 49 (DNGBUS) -> PCAN_TYPE_DNG(2),
/// anything else -> 0 (default).
fn pcan_hw_type(hardware_type: i32) -> u8 {
    match hardware_type {
        33 => 1,
        49 => 2,
        _ => 0,
    }
}

/// Classic-mode BTR0BTR1 code
/// (PCAN_BAUD_10K=0x672F ... PCAN_BAUD_1M=0x0014).
/// Unsupported baud rates return [`Error::Invalid`].
fn btr0btr1(baudrate: u32) -> Result<u16> {
    match baudrate {
        10_000 => Ok(0x672F),
        20_000 => Ok(0x532F),
        50_000 => Ok(0x472F),
        100_000 => Ok(0x432F),
        125_000 => Ok(0x031C),
        250_000 => Ok(0x011C),
        500_000 => Ok(0x001C),
        800_000 => Ok(0x0016),
        1_000_000 => Ok(0x0014),
        _ => Err(Error::Invalid(format!(
            "PCANBasic: no BTR0BTR1 code for baudrate {baudrate} Hz \
             (supported: 10k/20k/50k/100k/125k/250k/500k/800k/1M)"
        ))),
    }
}

/// Static table: baud rate (Hz) -> {brp, tseg1, tseg2, sjw}
/// (numeric values for the FD bitrate string).
static PEAK_FD_PARAMS: &[(u32, [u32; 4])] = &[
    (10_000, [16, 248, 1, 1]),
    (20_000, [8, 248, 1, 1]),
    (50_000, [4, 198, 1, 1]),
    (100_000, [2, 198, 1, 1]),
    (125_000, [2, 158, 1, 1]),
    (250_000, [10, 11, 4, 1]),
    (500_000, [5, 11, 4, 1]),
    (800_000, [1, 48, 1, 1]),
    (1_000_000, [5, 5, 2, 1]),
    (2_000_000, [4, 3, 1, 1]),
    (4_000_000, [1, 7, 1, 1]),
    (5_000_000, [1, 5, 2, 2]),
    (8_000_000, [1, 2, 2, 2]),
    (10_000_000, [1, 1, 2, 2]),
];

fn peak_fd_params(hz: u32) -> Result<[u32; 4]> {
    PEAK_FD_PARAMS
        .iter()
        .find(|(k, _)| *k == hz)
        .map(|(_, v)| *v)
        .ok_or_else(|| Error::Invalid(format!("PCANBasic: no FD params table entry for {hz} Hz")))
}

/// Assembles a PCAN-Basic FD bitrate definition string.
/// The parameter list (clock, nom brp/tseg1/tseg2/sjw, data
/// brp/tseg1/tseg2/sjw) follows the FD bitrate definition format from the
/// PCAN-Basic documentation. `f_clock_mhz` is in MHz: the table-based path
/// passes the literal 40 (MHz), consistent with the documented unit;
/// `BitRateConfig.Clock` is likewise treated as MHz.
fn fd_bitrate_string(clock: i32, nominal: [u32; 4], data: [u32; 4]) -> String {
    format!(
        "f_clock_mhz={clock},nom_brp={},nom_tseg1={},nom_tseg2={},nom_sjw={},\
         data_brp={},data_tseg1={},data_tseg2={},data_sjw={}",
        nominal[0], nominal[1], nominal[2], nominal[3], data[0], data[1], data[2], data[3],
    )
}

/// Table-based FD bitrate path (fixed 40 MHz clock).
fn fd_bitrate_string_from_tables(baudrate: u32, baudrate_fd: u32) -> Result<String> {
    let nominal = peak_fd_params(baudrate)?;
    let data = peak_fd_params(baudrate_fd)?;
    Ok(fd_bitrate_string(40, nominal, data))
}

/// Custom FD bitrate path from a `BitRateConfig`.
fn fd_bitrate_string_from_config(cfg: &BitRateConfig) -> String {
    fd_bitrate_string(
        cfg.clock,
        [
            cfg.nominal.brp as u32,
            cfg.nominal.tseg1 as u32,
            cfg.nominal.tseg2 as u32,
            cfg.nominal.sjw as u32,
        ],
        [
            cfg.data.brp as u32,
            cfg.data.tseg1 as u32,
            cfg.data.tseg2 as u32,
            cfg.data.sjw as u32,
        ],
    )
}

/// Receive-frame filter: drop RTR (0x01), ESI (0x10), ERRFRAME (0x40) and
/// STATUS (0x80).
fn accept_msg_type(msg_type: u8) -> bool {
    msg_type & (PCAN_MESSAGE_RTR | PCAN_MESSAGE_ESI | PCAN_MESSAGE_ERRFRAME | PCAN_MESSAGE_STATUS)
        == 0
}

// ---- PCANBasic.h structs (#[repr(C, packed)] for the Pack = 1 layout) ----

/// PCANBasic.h `TPCANTimestamp` (8 bytes; TPCANTimestampFD used by FD reads
/// is also 8 bytes, so both read paths share this struct).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct TpcanTimestamp {
    millis: u32,
    millis_overflow: u16,
    micros: u16,
}

/// PCANBasic.h `TPCANMsg` (14 bytes; layout: ID(4) + MSGTYPE(1) + LEN(1) +
/// DATA(8)).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct TpcanMsg {
    id: u32,
    /// PCAN_MESSAGE_* flags.
    msgtype: u8,
    len: u8,
    data: [u8; 8],
}

/// PCANBasic.h `TPCANMsgFD` (70 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct TpcanMsgFd {
    id: u32,
    /// PCAN_MESSAGE_* flags.
    msgtype: u8,
    dlc: u8,
    data: [u8; 64],
}

impl Default for TpcanMsgFd {
    fn default() -> Self {
        Self {
            id: 0,
            msgtype: 0,
            dlc: 0,
            data: [0; 64],
        }
    }
}

// Compile-time layout size checks.
const _: () = assert!(std::mem::size_of::<TpcanTimestamp>() == 8);
const _: () = assert!(std::mem::size_of::<TpcanMsg>() == 14);
const _: () = assert!(std::mem::size_of::<TpcanMsgFd>() == 70);

/// Builds a classic transmit message.
fn build_tx_msg(can_id: u32, data: &[u8]) -> TpcanMsg {
    let mut buf = [0u8; 8];
    buf[..data.len()].copy_from_slice(data);
    TpcanMsg {
        id: can_id & 0x1FFF_FFFF,
        msgtype: if can_id & CAN_EXT_FLAG != 0 {
            PCAN_MESSAGE_EXTENDED
        } else {
            0
        },
        len: data.len() as u8,
        data: buf,
    }
}

/// Builds an FD transmit message (DLC encoding + 64-byte buffer).
fn build_tx_msg_fd(can_id: u32, data: &[u8], frame_type: FrameType) -> Result<TpcanMsgFd> {
    let mut msgtype = 0u8;
    if can_id & CAN_EXT_FLAG != 0 {
        msgtype |= PCAN_MESSAGE_EXTENDED;
    }
    if frame_type.contains(FrameType::FD) {
        msgtype |= PCAN_MESSAGE_FD;
    }
    if frame_type.contains(FrameType::BRS) {
        msgtype |= PCAN_MESSAGE_BRS;
    }
    let mut buf = [0u8; 64];
    buf[..data.len()].copy_from_slice(data);
    Ok(TpcanMsgFd {
        id: can_id & 0x1FFF_FFFF,
        msgtype,
        dlc: length_to_dlc(data.len())?,
        data: buf,
    })
}

/// PCANBasic function pointer table; all symbols are validated at
/// construction.
#[derive(Debug)]
struct PcanBasic {
    /// Keeps the library handle alive (autors-native `DllWrapper`); the field
    /// is never accessed directly.
    _dll: DllWrapper,
    /// TPCANStatus CAN_Initialize(TPCANHandle Channel, TPCANBaudrate Btr0Btr1,
    /// TPCANType HwType, DWORD IOPort, WORD Interrupt).
    can_initialize: unsafe extern "system" fn(u16, u16, u8, u32, u16) -> u32,
    /// TPCANStatus CAN_InitializeFD(TPCANHandle Channel, TPCANBitrateFD BitrateFD).
    can_initialize_fd: unsafe extern "system" fn(u16, *const u8) -> u32,
    /// TPCANStatus CAN_Uninitialize(TPCANHandle Channel).
    can_uninitialize: unsafe extern "system" fn(u16) -> u32,
    /// TPCANStatus CAN_Reset(TPCANHandle Channel).
    #[allow(dead_code)] // Loaded for symbol-table completeness; not used by the CAN flow
    can_reset: unsafe extern "system" fn(u16) -> u32,
    /// TPCANStatus CAN_GetStatus(TPCANHandle Channel).
    #[allow(dead_code)] // Loaded for symbol-table completeness; not used by the CAN flow
    can_get_status: unsafe extern "system" fn(u16) -> u32,
    /// TPCANStatus CAN_SetValue(TPCANHandle, TPCANParameter, void *NumericBuffer,
    /// DWORD BufferLength) (the adapter only ever passes a u32 value).
    can_set_value: unsafe extern "system" fn(u16, u8, *mut u32, u32) -> u32,
    /// TPCANStatus CAN_Read(TPCANHandle, TPCANMsg *MessageBuffer,
    /// TPCANTimestamp *TimestampBuffer).
    can_read: unsafe extern "system" fn(u16, *mut TpcanMsg, *mut TpcanTimestamp) -> u32,
    /// TPCANStatus CAN_ReadFD(TPCANHandle, TPCANMsgFD *MessageBuffer,
    /// TPCANTimestampFD *TimestampBuffer).
    can_read_fd: unsafe extern "system" fn(u16, *mut TpcanMsgFd, *mut u64) -> u32,
    /// TPCANStatus CAN_Write(TPCANHandle, TPCANMsg *MessageBuffer).
    can_write: unsafe extern "system" fn(u16, *mut TpcanMsg) -> u32,
    /// TPCANStatus CAN_WriteFD(TPCANHandle, TPCANMsgFD *MessageBuffer).
    can_write_fd: unsafe extern "system" fn(u16, *mut TpcanMsgFd) -> u32,
}

impl PcanBasic {
    fn load() -> Result<Self> {
        Self::load_from(PCANBASIC_DLL)
    }

    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of running DllMain) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (per get): the symbol address is copied to a raw function
        // pointer and stored alongside the library handle in the same struct,
        // so their lifetimes match.
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
            can_initialize: sym!(
                b"CAN_Initialize\0",
                unsafe extern "system" fn(u16, u16, u8, u32, u16) -> u32
            ),
            can_initialize_fd: sym!(
                b"CAN_InitializeFD\0",
                unsafe extern "system" fn(u16, *const u8) -> u32
            ),
            can_uninitialize: sym!(b"CAN_Uninitialize\0", unsafe extern "system" fn(u16) -> u32),
            can_reset: sym!(b"CAN_Reset\0", unsafe extern "system" fn(u16) -> u32),
            can_get_status: sym!(b"CAN_GetStatus\0", unsafe extern "system" fn(u16) -> u32),
            can_set_value: sym!(
                b"CAN_SetValue\0",
                unsafe extern "system" fn(u16, u8, *mut u32, u32) -> u32
            ),
            can_read: sym!(
                b"CAN_Read\0",
                unsafe extern "system" fn(u16, *mut TpcanMsg, *mut TpcanTimestamp) -> u32
            ),
            can_read_fd: sym!(
                b"CAN_ReadFD\0",
                unsafe extern "system" fn(u16, *mut TpcanMsgFd, *mut u64) -> u32
            ),
            can_write: sym!(
                b"CAN_Write\0",
                unsafe extern "system" fn(u16, *mut TpcanMsg) -> u32
            ),
            can_write_fd: sym!(
                b"CAN_WriteFD\0",
                unsafe extern "system" fn(u16, *mut TpcanMsgFd) -> u32
            ),
            _dll: dll,
        })
    }
}

/// PEAK CAN channel adapter.
/// Frames are received by polling CAN_Read/CAN_ReadFD directly, per the
/// non-blocking contract of [`CanDevice::receive`]; blocking waits are
/// handled by the polling cadence of the upper layer
/// [`crate::device::start_dispatch`].
pub struct PeakCan {
    core: DeviceCore,
    api: PcanBasic,
    /// Channel handle (ushort.MaxValue means not opened).
    handle: u16,
    /// Whether the channel was initialized in FD mode.
    fd: bool,
    /// Whether an error frame has been received.
    err_received: bool,
    /// Bus ID (generated at open time, or taken from the configuration).
    bus_id: String,
    rx_queue: VecDeque<CanFrame>,
}

impl PeakCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle != INVALID_HANDLE {
            // SAFETY: the handle is valid (the return value is intentionally
            // ignored).
            unsafe {
                (self.api.can_uninitialize)(self.handle);
            }
            self.handle = INVALID_HANDLE;
            self.rx_queue.clear();
        }
    }

    /// Loads PCANBasic.dll and validates the symbol table; returns
    /// [`Error::Driver`] if the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: PcanBasic::load()?,
            handle: INVALID_HANDLE,
            fd: false,
            err_received: false,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle != INVALID_HANDLE
    }

    /// Whether an error frame has been received.
    pub fn err_received(&self) -> bool {
        self.err_received
    }

    /// Reads a single frame (classic and FD paths).
    fn read_one_frame(&mut self) -> Result<bool> {
        if self.fd {
            let mut msg = TpcanMsgFd::default();
            let mut ts: u64 = 0;
            // SAFETY: the handle is valid; msg/ts are local stack variables.
            let st = unsafe { (self.api.can_read_fd)(self.handle, &mut msg, &mut ts) };
            if st != PCAN_OK {
                return Ok(false);
            }
            let (id, msgtype, dlc) = (msg.id, msg.msgtype, msg.dlc);
            // LEN == 0 or a filtered-out message type drops the frame; a
            // failed DLC-to-length conversion (dlc > 15) skips it.
            if dlc == 0 || !accept_msg_type(msgtype) {
                return Ok(true);
            }
            let Ok(len) = dlc_to_length(dlc) else {
                return Ok(true);
            };
            let mut id = id;
            if msgtype & PCAN_MESSAGE_EXTENDED != 0 {
                id |= CAN_EXT_FLAG;
            }
            let mut frame_type = FrameType::CAN20B;
            if msgtype & PCAN_MESSAGE_FD != 0 {
                frame_type = frame_type | FrameType::FD;
            }
            if msgtype & PCAN_MESSAGE_BRS != 0 {
                frame_type = frame_type | FrameType::BRS;
            }
            let data = msg.data;
            self.rx_queue.push_back(CanFrame::new(
                &self.bus_id,
                id,
                data[..len as usize].to_vec(),
                false,
                frame_type,
            ));
        } else {
            let mut msg = TpcanMsg::default();
            let mut ts = TpcanTimestamp::default();
            // SAFETY: the handle is valid; msg/ts are local stack variables.
            let st = unsafe { (self.api.can_read)(self.handle, &mut msg, &mut ts) };
            if st != PCAN_OK {
                return Ok(false);
            }
            let (id, msgtype, len) = (msg.id, msg.msgtype, msg.len);
            if len == 0 || !accept_msg_type(msgtype) {
                // An error frame sets the err_received flag.
                if msgtype & PCAN_MESSAGE_ERRFRAME != 0 {
                    self.err_received = true;
                }
                return Ok(true);
            }
            let mut id = id;
            if msgtype & PCAN_MESSAGE_EXTENDED != 0 {
                id |= CAN_EXT_FLAG;
            }
            let len = (len as usize).min(8);
            let data = msg.data;
            self.rx_queue.push_back(CanFrame::new(
                &self.bus_id,
                id,
                data[..len].to_vec(),
                false,
                FrameType::CAN20B,
            ));
        }
        Ok(true)
    }
}

#[async_trait]
impl CanDevice for PeakCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        Ok(self.handle != INVALID_HANDLE)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle != INVALID_HANDLE {
            return Ok(true);
        }
        // The bus ID is generated at open time as
        // `format_bus_id("Peak", channel)`, consistent with the other
        // backends, unless the configuration supplies one.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Peak", config.channel));
        let handle = channel_handle(config.hardware_type, config.channel);
        self.fd = config.is_fd();
        if self.fd {
            let bitrate = match &config.fd_bit_rate_config {
                Some(custom) => fd_bitrate_string_from_config(custom),
                None => fd_bitrate_string_from_tables(
                    config.baudrate.as_u32(),
                    config.baudrate_fd.as_u32(),
                )?,
            };
            let c_bitrate =
                CString::new(bitrate).map_err(|e| Error::Invalid(format!("FD bitrate: {e}")))?;
            // SAFETY: c_bitrate is NUL-terminated and valid for the duration
            // of the call.
            let st =
                unsafe { (self.api.can_initialize_fd)(handle, c_bitrate.as_ptr() as *const u8) };
            check(st, &self.bus_id, "CAN_InitializeFD")?;
        } else {
            let btr = btr0btr1(config.baudrate.as_u32())?;
            // SAFETY: arguments are passed by value.
            let st = unsafe {
                (self.api.can_initialize)(handle, btr, pcan_hw_type(config.hardware_type), 0, 0)
            };
            check(st, &self.bus_id, "CAN_Initialize")?;
        }
        // Enable automatic bus-off reset (PCAN_BUSOFF_AUTORESET = 1); the
        // result is intentionally ignored.
        // SAFETY: the handle is valid; val is a local stack variable.
        let mut val: u32 = 1;
        unsafe {
            (self.api.can_set_value)(handle, PCAN_BUSOFF_AUTORESET, &mut val, 4);
        }
        self.handle = handle;
        self.err_received = false;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // Sending on an unopened channel returns 0.
        if self.handle == INVALID_HANDLE {
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
        // OK and BUSOFF count as success; QXMTFULL is retried within a 50 ms
        // window.
        let start = Instant::now();
        loop {
            let st = if self.fd {
                let mut msg = build_tx_msg_fd(can_id, data, frame_type)?;
                // SAFETY: the handle is valid; msg is valid for the duration
                // of the call.
                unsafe { (self.api.can_write_fd)(self.handle, &mut msg) }
            } else {
                let mut msg = build_tx_msg(can_id, data);
                // SAFETY: the handle is valid; msg is valid for the duration
                // of the call.
                unsafe { (self.api.can_write)(self.handle, &mut msg) }
            };
            match st {
                PCAN_OK | PCAN_BUSOFF => {
                    let frame =
                        CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
                    return Ok(self.core.record_sent(&frame));
                }
                PCAN_QXMTFULL => {
                    if start.elapsed().as_millis() >= TX_RETRY_WINDOW_MS {
                        return Ok(0);
                    }
                }
                _ => return Ok(0),
            }
        }
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        // Receive is non-blocking per the trait contract: poll the driver
        // directly rather than relying on event-driven background delivery.
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if self.handle == INVALID_HANDLE {
            return Ok(None);
        }
        // Drain the driver receive queue (CAN_Read until QRCVEMPTY).
        while self.read_one_frame()? {}
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for PeakCan {
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
        assert_eq!(std::mem::size_of::<TpcanTimestamp>(), 8);
        assert_eq!(std::mem::size_of::<TpcanMsg>(), 14);
        assert_eq!(std::mem::size_of::<TpcanMsgFd>(), 70);
    }

    #[test]
    fn btr0btr1_codes() {
        // Full table of standard PCAN_BAUD_* values.
        assert_eq!(btr0btr1(10_000).unwrap(), 0x672F);
        assert_eq!(btr0btr1(20_000).unwrap(), 0x532F);
        assert_eq!(btr0btr1(50_000).unwrap(), 0x472F);
        assert_eq!(btr0btr1(100_000).unwrap(), 0x432F);
        assert_eq!(btr0btr1(125_000).unwrap(), 0x031C);
        assert_eq!(btr0btr1(250_000).unwrap(), 0x011C);
        assert_eq!(btr0btr1(500_000).unwrap(), 0x001C);
        assert_eq!(btr0btr1(800_000).unwrap(), 0x0016);
        assert_eq!(btr0btr1(1_000_000).unwrap(), 0x0014);
        assert!(matches!(btr0btr1(33_333), Err(Error::Invalid(_))));
        assert!(matches!(btr0btr1(0), Err(Error::Invalid(_))));
    }

    #[test]
    fn channel_handle_computation() {
        // PCAN_USBBUS(0x51) + channel.
        assert_eq!(channel_handle(0x51, 0), 0x51);
        assert_eq!(channel_handle(0x51, 5), 0x56);
        // PCAN_PCIBUS(0x41) + channel.
        assert_eq!(channel_handle(0x41, 1), 0x42);
    }

    #[test]
    fn pcan_hw_type_mapping() {
        assert_eq!(pcan_hw_type(33), 1); // ISABUS -> PCAN_TYPE_ISA
        assert_eq!(pcan_hw_type(49), 2); // DNGBUS -> PCAN_TYPE_DNG
        assert_eq!(pcan_hw_type(0x51), 0); // USB etc. -> default
    }

    #[test]
    fn peak_hw_type_values() {
        assert_eq!(PeakHwType::IsaBus as u16, 33);
        assert_eq!(PeakHwType::DngBus as u16, 49);
        assert_eq!(PeakHwType::PciBus as u16, 65);
        assert_eq!(PeakHwType::UsbBus as u16, 81);
        assert_eq!(PeakHwType::PccBus as u16, 97);
        assert_eq!(PeakHwType::PcanVirtual as u16, 1793);
        assert_eq!(PeakHwType::LanBus as u16, 2049);
    }

    #[test]
    fn fd_bitrate_string_tables() {
        // 500k/2M: nominal {5,11,4,1}, data {4,3,1,1}, 40 MHz clock.
        let s = fd_bitrate_string_from_tables(500_000, 2_000_000).unwrap();
        assert_eq!(
            s,
            "f_clock_mhz=40,nom_brp=5,nom_tseg1=11,nom_tseg2=4,nom_sjw=1,\
             data_brp=4,data_tseg1=3,data_tseg2=1,data_sjw=1"
        );
        // A baud rate missing from the table -> Invalid.
        assert!(matches!(
            fd_bitrate_string_from_tables(500_000, 3_000_000),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn fd_bitrate_string_custom() {
        let cfg = BitRateConfig {
            clock: 80,
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
            non_iso: false,
        };
        let s = fd_bitrate_string_from_config(&cfg);
        assert_eq!(
            s,
            "f_clock_mhz=80,nom_brp=5,nom_tseg1=11,nom_tseg2=4,nom_sjw=1,\
             data_brp=2,data_tseg1=14,data_tseg2=5,data_sjw=1"
        );
    }

    #[test]
    fn accept_msg_type_filter() {
        assert!(accept_msg_type(0)); // STANDARD
        assert!(accept_msg_type(PCAN_MESSAGE_EXTENDED));
        assert!(accept_msg_type(
            PCAN_MESSAGE_FD | PCAN_MESSAGE_BRS | PCAN_MESSAGE_EXTENDED
        ));
        assert!(!accept_msg_type(PCAN_MESSAGE_RTR));
        assert!(!accept_msg_type(PCAN_MESSAGE_ESI));
        assert!(!accept_msg_type(PCAN_MESSAGE_ERRFRAME));
        assert!(!accept_msg_type(PCAN_MESSAGE_STATUS));
    }

    #[test]
    fn status_names() {
        assert_eq!(peak_status_name(0), "PCAN_ERROR_OK");
        assert_eq!(peak_status_name(0x20), "PCAN_ERROR_QRCVEMPTY");
        assert_eq!(peak_status_name(PCAN_BUSOFF), "PCAN_ERROR_BUSOFF");
        assert_eq!(peak_status_name(0x123), "PCAN_ERROR_???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        match check(0x200, "Peak/CAN1", "CAN_Initialize").unwrap_err() {
            Error::Driver(msg) => {
                assert!(msg.contains("Peak/CAN1"));
                assert!(msg.contains("CAN_Initialize"));
                assert!(msg.contains("PCAN_ERROR_NODRIVER"));
            }
            other => panic!("unexpected error variant: {other}"),
        }
    }

    #[test]
    fn build_tx_msg_fields() {
        // Standard frame: ID masked, msgtype=0 (packed fields are copied
        // before asserting).
        let msg = build_tx_msg(0x123, &[1, 2, 3]);
        let (id, msgtype, len, data) = (msg.id, msg.msgtype, msg.len, msg.data);
        assert_eq!(id, 0x123);
        assert_eq!(msgtype, 0);
        assert_eq!(len, 3);
        assert_eq!(&data[..3], &[1, 2, 3]);
        // Extended frame: flag bit stripped from the ID, msgtype=EXTENDED.
        let msg = build_tx_msg(0x1234 | CAN_EXT_FLAG, &[0xAB; 8]);
        let (id, msgtype, len) = (msg.id, msg.msgtype, msg.len);
        assert_eq!(id, 0x1234);
        assert_eq!(msgtype, PCAN_MESSAGE_EXTENDED);
        assert_eq!(len, 8);
    }

    #[test]
    fn build_tx_msg_fd_fields() {
        let msg = build_tx_msg_fd(CAN_EXT_FLAG | 0x55, &[7u8; 12], FrameType::FD_BRS).unwrap();
        let (id, msgtype, dlc, data) = (msg.id, msg.msgtype, msg.dlc, msg.data);
        assert_eq!(id, 0x55);
        assert_eq!(
            msgtype,
            PCAN_MESSAGE_EXTENDED | PCAN_MESSAGE_FD | PCAN_MESSAGE_BRS
        );
        assert_eq!(dlc, 9); // length_to_dlc(12)
        assert!(data[..12].iter().all(|&b| b == 7));
        // Classic frame type on an FD channel: no FD/BRS bits set.
        let msg = build_tx_msg_fd(0x55, &[1; 8], FrameType::CAN20B).unwrap();
        let msgtype = msg.msgtype;
        assert_eq!(msgtype, 0);
    }

    #[test]
    fn missing_dll_is_driver_error() {
        let err = PcanBasic::load_from("no_such_pcanbasic_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // On a machine with the PEAK driver installed, loading must succeed
        // with a complete symbol table; without the driver, expect
        // Error::Driver. Both outcomes are acceptable — the key requirement
        // is that it must not panic.
        match PeakCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(!dev.err_received());
                assert!(dev.unique_bus_id() >= 1);
                // With no channel open, send returns 0 and receive returns
                // None.
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
