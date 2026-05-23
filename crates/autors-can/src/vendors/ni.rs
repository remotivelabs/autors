//! National Instruments NI-CAN driver adapter (`nican.dll`).
//! Wraps the NI-CAN Frame API, a public C API (`ncConfig`/`ncOpenObject`/
//! `ncCloseObject`/`ncAction`/`ncReadObject`/`ncWriteObject`/`ncWaitForState`/
//! `ncCreateNotification`). The exported functions use the standard call
//! convention, which matches the C calling convention on x64, so all function
//! pointers are declared `extern "system"`. Construction returns
//! [`Error::Driver`] when the DLL is not installed or an expected export is
//! missing.
//! Receive strategy: per the non-blocking contract of
//! [`CanDevice::receive`], `receive()` polls `ncReadObject` directly instead
//! of registering a notification callback; blocking waits are handled by the
//! polling cadence of the upper-layer dispatch loop
//! (`crate::device::start_dispatch`). The notification entry point
//! (`ncCreateNotification`) is still loaded as an optional symbol (failed
//! lookup -> `None`) to keep the symbol table complete, but the current flow
//! never calls it.
//! NI-CAN supports classic CAN only (8-byte payloads). Payloads longer than
//! 8 bytes are rejected explicitly with [`Error::Invalid`]. CAN-FD
//! configuration is ignored entirely; only the `baudrate` field of the
//! configuration is used.

use std::collections::VecDeque;
use std::ffi::CString;

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{
    config_err, data_from_array, format_bus_id, from_ni_can_id, to_ni_can_id, CanDevice,
    DeviceCore, MAX_DLC,
};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFrame, FrameType};

/// Name of the NI-CAN driver DLL (nican.h, NI-CAN Frame API runtime library).
const NICAN_DLL: &str = "nican.dll";

// ---- nican.h constants (NI-CAN Frame API public header) ----
/// Success status (`CanSuccess`).
const NC_SUCCESS: u32 = 0;
/// NC_OP_START: start object communication.
const NC_OP_START: u32 = 0x8000_0001;
/// NC_OP_STOP: stop object communication.
const NC_OP_STOP: u32 = 0x8000_0002;
/// NC_ATTR_START_ON_OPEN: start communication as soon as the object is opened
/// (configured value: 1).
const NC_ATTR_START_ON_OPEN: u32 = 0x8000_0006;
/// NC_ATTR_BAUD_RATE: baud rate in Hz.
const NC_ATTR_BAUD_RATE: u32 = 0x8000_0007;
/// NC_ATTR_READ_Q_LEN: read queue length (configured value: 0).
const NC_ATTR_READ_Q_LEN: u32 = 0x8000_0013;
/// NC_ATTR_WRITE_Q_LEN: write queue length (configured value: 0).
const NC_ATTR_WRITE_Q_LEN: u32 = 0x8000_0014;
/// NC_ATTR_CAN_COMP_STD: standard-frame comparison ID (configured value: 0).
const NC_ATTR_CAN_COMP_STD: u32 = 0x8001_0001;
/// NC_ATTR_CAN_MASK_STD: standard-frame mask (configured value: 0).
const NC_ATTR_CAN_MASK_STD: u32 = 0x8001_0002;
/// NC_ATTR_CAN_COMP_EXT: extended-frame comparison ID (configured value: 0).
const NC_ATTR_CAN_COMP_EXT: u32 = 0x8001_0003;
/// NC_ATTR_CAN_MASK_EXT: extended-frame mask (configured value: 0).
const NC_ATTR_CAN_MASK_EXT: u32 = 0x8001_0004;
/// Number of entries in the attribute table passed to `ncConfig`.
const NC_CONFIG_NUM_ATTRS: usize = 8;

// ---- NI-CAN Frame API structures (`#[repr(C, packed)]` matches the DLL's
// packed ABI with 1-byte alignment) ----

/// nican.h `NCTYPE_CAN_STRUCT` (TX; 14 bytes packed).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct NcCanStruct {
    /// Arbitration ID (NI format: bit 29 (0x20000000) marks an extended
    /// frame; see [`to_ni_can_id`]).
    arbitration_id: u32,
    /// Remote-frame flag (always 0 on TX).
    is_remote: u8,
    /// Data length in bytes.
    data_length: u8,
    /// Payload, up to 8 bytes packed little-endian (see `data_from_array`).
    data: u64,
}

/// nican.h `NCTYPE_CAN_FRAME_TIMED` (RX; 22 bytes packed).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct NcCanFrameTimed {
    /// Timestamp (absolute time; currently unused).
    timestamp: u64,
    /// Arbitration ID (NI format; converted back with [`from_ni_can_id`]).
    arbitration_id: u32,
    /// Frame-type byte (not used for filtering).
    frame_type: u8,
    /// Data length; only frames with `data_length > 0` are accepted.
    data_length: u8,
    /// Payload, up to 8 bytes packed little-endian.
    data: u64,
}

// Compile-time layout self-check: the packed sizes must be 14 and 22 bytes.
const _: () = assert!(std::mem::size_of::<NcCanStruct>() == 14);
const _: () = assert!(std::mem::size_of::<NcCanFrameTimed>() == 22);

/// Common NCTYPE_STATUS names (NI-CAN status codes, BASE=0xBFF62000), used in
/// error messages.
fn status_name(status: u32) -> &'static str {
    match status {
        0x0000_0000 => "CanSuccess",
        0xBFF6_2001 => "CanErrFunctionTimeout",
        0xBFF6_2002 => "CanErrDriver",
        0xBFF6_2003 => "CanErrBadNameSyntax",
        0xBFF6_2004 => "CanErrBadParam",
        0xBFF6_2005 => "CanErrBadAttributeValue",
        0xBFF6_2006 => "CanErrAlreadyOpen",
        0xBFF6_2007 => "CanErrNotStopped",
        0xBFF6_2008 => "CanErrOverflowWrite",
        0xBFF6_200A => "CanErrNotSupported",
        0xBFF6_200B => "CanErrComm",
        0xBFF6_2023 => "CanErrBadIntfName",
        0xBFF6_2024 => "CanErrBadHandle",
        0xBFF6_2112 => "CanErrNotStarted",
        0xBFF6_2124 => "CanErrBadBaudRate",
        _ => "CanErr???",
    }
}

/// Maps any NCTYPE_STATUS other than `CanSuccess` to [`Error::Driver`]; the
/// hardware ID is merged in by the caller via [`config_err`].
fn check(status: u32, bus_id: &str, what: &str) -> Result<()> {
    if status == NC_SUCCESS {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "nican: {what} failed: {} (0x{status:08X})",
                status_name(status)
            ),
        ))
    }
}

/// NI-CAN supports only 10k/100k/125k/250k/500k/1M baud rates; the Hz value
/// is used directly. Any other baud rate is rejected with
/// [`Error::NotSupported`].
fn ni_baudrate_hz(baudrate: CanBaudrate) -> Result<u32> {
    match baudrate {
        CanBaudrate::B10Kbit
        | CanBaudrate::B100Kbit
        | CanBaudrate::B125Kbit
        | CanBaudrate::B250Kbit
        | CanBaudrate::B500Kbit
        | CanBaudrate::B1Mbit => Ok(baudrate.as_u32()),
        _ => Err(Error::NotSupported(format!(
            "nican: baudrate {baudrate} is not supported by the NI-CAN device \
             (supported: 10k/100k/125k/250k/500k/1M)"
        ))),
    }
}

/// Builds the `ncConfig` attribute table (8 entries in a fixed order):
/// start-on-open, zeroed read/write queues, zeroed standard/extended
/// compare-and-mask (receive all frames), and the baud rate.
fn build_config_attrs(
    baudrate_hz: u32,
) -> ([u32; NC_CONFIG_NUM_ATTRS], [u32; NC_CONFIG_NUM_ATTRS]) {
    (
        [
            NC_ATTR_START_ON_OPEN,
            NC_ATTR_READ_Q_LEN,
            NC_ATTR_WRITE_Q_LEN,
            NC_ATTR_CAN_COMP_STD,
            NC_ATTR_CAN_COMP_EXT,
            NC_ATTR_CAN_MASK_STD,
            NC_ATTR_CAN_MASK_EXT,
            NC_ATTR_BAUD_RATE,
        ],
        [1, 0, 0, 0, 0, 0, 0, baudrate_hz],
    )
}

/// Builds the TX structure: arbitration ID converted to NI format,
/// IsRemote=0, DataLength=payload length, payload packed little-endian
/// (associated function to keep it unit-testable).
fn build_tx_struct(can_id: u32, data: &[u8]) -> NcCanStruct {
    NcCanStruct {
        arbitration_id: to_ni_can_id(can_id),
        is_remote: 0,
        data_length: data.len() as u8,
        data: data_from_array(data),
    }
}

/// RX acceptance filter: only frames with `data_length > 0` are queued. The
/// frame-type byte is intentionally not tested.
fn accept_rx_frame(data_length: u8) -> bool {
    data_length > 0
}

/// NI-CAN interface name for a channel (`CAN{channel}`), following the NI-CAN
/// channel naming convention.
fn interface_name(channel: i32) -> String {
    format!("CAN{channel}")
}

/// nican.dll function-pointer table. All required symbols are loaded and
/// validated at construction; a missing symbol is reported as
/// [`Error::Driver`].
/// NI-CAN handles and status codes are 32-bit unsigned values.
#[derive(Debug)]
struct NiCanApi {
    /// Keeps the library handle alive (autors-native [`DllWrapper`]); the
    /// field is never accessed directly.
    _dll: DllWrapper,
    /// NCTYPE_STATUS ncConfig(NCTYPE_STRING objName, NCTYPE_UINT32 numAttrs,
    /// NCTYPE_ATTRID_P attrIdList, NCTYPE_UINT32_P attrValueList).
    nc_config: unsafe extern "system" fn(*const u8, u32, *const u32, *const u32) -> u32,
    /// NCTYPE_STATUS ncOpenObject(NCTYPE_STRING objName, NCTYPE_OBJH_P objHandlePtr).
    nc_open_object: unsafe extern "system" fn(*const u8, *mut u32) -> u32,
    /// NCTYPE_STATUS ncCloseObject(NCTYPE_OBJH objHandle).
    nc_close_object: unsafe extern "system" fn(u32) -> u32,
    /// NCTYPE_STATUS ncAction(NCTYPE_OBJH objHandle, NCTYPE_OPCODE opcode,
    /// NCTYPE_UINT32 param) (STOP then START on open; STOP on close).
    nc_action: unsafe extern "system" fn(u32, u32, u32) -> u32,
    /// NCTYPE_STATUS ncWriteObject(NCTYPE_OBJH objHandle, NCTYPE_UINT32 dataSize,
    /// NCTYPE_CAN_STRUCT_P dataPtr).
    nc_write_object: unsafe extern "system" fn(u32, u32, *const NcCanStruct) -> u32,
    /// NCTYPE_STATUS ncReadObject(NCTYPE_OBJH objHandle, NCTYPE_UINT32 dataSize,
    /// NCTYPE_CAN_FRAME_TIMED_P dataPtr).
    nc_read_object: unsafe extern "system" fn(u32, u32, *mut NcCanFrameTimed) -> u32,
    /// NCTYPE_STATUS ncWaitForState(NCTYPE_OBJH objHandle, NCTYPE_STATE
    /// desiredState, NCTYPE_DURATION timeout, NCTYPE_STATE_P statePtr).
    #[allow(dead_code)] // loaded for symbol-table completeness; never called
    nc_wait_for_state: unsafe extern "system" fn(u32, u32, u32, *mut u32) -> u32,
    /// NCTYPE_STATUS ncCreateNotification(NCTYPE_OBJH objHandle, NCTYPE_STATE
    /// desiredState, NCTYPE_DURATION timeout, NCTYPE_ANY_P refData,
    /// NCTYPE_NOTIFY_CALLBACK callback) — an optional callback-based
    /// notification mechanism, probed for presence (failed lookup -> `None`,
    /// meaning notifications are unavailable). The current implementation
    /// polls `ncReadObject` directly and never calls it.
    #[allow(dead_code)] // optional symbol: loaded when present, never called
    nc_create_notification: Option<unsafe extern "system" fn(u32, u32, u32, usize, usize) -> u32>,
}

impl NiCanApi {
    /// Loads nican.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(NICAN_DLL)
    }

    /// Loads the DLL from the given path/name and resolves all exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of running DllMain) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, so the pointers stay valid for its lifetime. The
        // generic T is a (Copy) function-pointer type.
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
        // Optional symbol: a failed lookup means no notification mechanism is
        // available.
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: as above; a missing symbol simply yields None.
                let r = unsafe { dll.library().get::<$ty>($name) };
                r.ok().map(|s| *s)
            }};
        }
        Ok(Self {
            nc_config: sym!(
                b"ncConfig\0",
                unsafe extern "system" fn(*const u8, u32, *const u32, *const u32) -> u32
            ),
            nc_open_object: sym!(
                b"ncOpenObject\0",
                unsafe extern "system" fn(*const u8, *mut u32) -> u32
            ),
            nc_close_object: sym!(b"ncCloseObject\0", unsafe extern "system" fn(u32) -> u32),
            nc_action: sym!(
                b"ncAction\0",
                unsafe extern "system" fn(u32, u32, u32) -> u32
            ),
            nc_write_object: sym!(
                b"ncWriteObject\0",
                unsafe extern "system" fn(u32, u32, *const NcCanStruct) -> u32
            ),
            nc_read_object: sym!(
                b"ncReadObject\0",
                unsafe extern "system" fn(u32, u32, *mut NcCanFrameTimed) -> u32
            ),
            nc_wait_for_state: sym!(
                b"ncWaitForState\0",
                unsafe extern "system" fn(u32, u32, u32, *mut u32) -> u32
            ),
            nc_create_notification: opt_sym!(
                b"ncCreateNotification\0",
                unsafe extern "system" fn(u32, u32, u32, usize, usize) -> u32
            ),
            _dll: dll,
        })
    }
}

/// NI CAN channel adapter backed by the NI-CAN Frame API (`nican.dll`).
/// Receive path: per the non-blocking contract of [`CanDevice::receive`],
/// `receive()` drains `ncReadObject` directly instead of using a notification
/// callback (the same polling approach as the Kvaser adapter).
pub struct NiCan {
    core: DeviceCore,
    api: NiCanApi,
    /// NI-CAN object handle (`u32::MAX` means "not open").
    handle: u32,
    /// Bus ID (generated on open, or taken from the configuration).
    bus_id: String,
    /// Buffer for received frames.
    rx_queue: VecDeque<CanFrame>,
}

impl NiCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        self.close_internal();
    }

    /// Loads nican.dll and validates the symbol table; returns
    /// [`Error::Driver`] when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: NiCanApi::load()?,
            handle: u32::MAX,
            bus_id: String::new(),
            rx_queue: VecDeque::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle != u32::MAX
    }

    /// Closes the object (internal; used by open-error rollback and close).
    fn close_internal(&mut self) {
        if self.handle != u32::MAX {
            // Close sequence: ncAction(NC_OP_STOP) + ncCloseObject; the return
            // values are intentionally ignored. No notification was
            // registered, so there is nothing to unregister.
            // SAFETY: handle comes from a successful ncOpenObject.
            unsafe {
                (self.api.nc_action)(self.handle, NC_OP_STOP, 0);
                (self.api.nc_close_object)(self.handle);
            }
            self.handle = u32::MAX;
            self.rx_queue.clear();
        }
    }
}

#[async_trait]
impl CanDevice for NiCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // `is_available` only reports the current open state; the actual open
        // sequence happens in `open()`.
        Ok(self.handle != u32::MAX)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle != u32::MAX {
            return Ok(true);
        }
        // Generate the BusId (e.g. "NI/CAN1") when the configuration does not
        // provide one.
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("NI", config.channel));
        // Baud-rate mapping happens before ncConfig; unsupported rates are
        // rejected up front.
        let baudrate_hz = ni_baudrate_hz(config.baudrate)?;
        let (attr_ids, attr_values) = build_config_attrs(baudrate_hz);
        let intf = CString::new(interface_name(config.channel))
            .map_err(|e| Error::Invalid(format!("interface name: {e}")))?;
        // SAFETY: intf is NUL-terminated and valid for the duration of the
        // call; the array lengths match numAttrs.
        let st = unsafe {
            (self.api.nc_config)(
                intf.as_ptr() as *const u8,
                NC_CONFIG_NUM_ATTRS as u32,
                attr_ids.as_ptr(),
                attr_values.as_ptr(),
            )
        };
        check(st, &self.bus_id, "ncConfig")?;
        let mut handle: u32 = 0;
        // SAFETY: intf is valid; handle points to a variable on this stack frame.
        let st = unsafe { (self.api.nc_open_object)(intf.as_ptr() as *const u8, &mut handle) };
        check(st, &self.bus_id, "ncOpenObject")?;
        self.handle = handle;
        // On failure after ncOpenObject: ncAction(STOP) + ncCloseObject, then
        // propagate the error.
        let result = (|| {
            // STOP is issued before START; only the START return value is
            // checked.
            // SAFETY: handle is valid; arguments are passed by value.
            unsafe { (self.api.nc_action)(self.handle, NC_OP_STOP, 0) };
            // SAFETY: as above.
            let st = unsafe { (self.api.nc_action)(self.handle, NC_OP_START, 0) };
            check(st, &self.bus_id, "ncAction(NC_OP_START)")?;
            // A notification callback would normally be registered here; this
            // implementation polls ncReadObject in receive() instead (see the
            // module docs).
            Ok(())
        })();
        if let Err(e) = result {
            self.close_internal();
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        // When the channel is not open, send returns 0. NI-CAN is classic CAN
        // only, so frame_type does not affect the TX path; it is only used
        // for the sent-frame record.
        if self.handle == u32::MAX {
            return Ok(0);
        }
        if data.len() > MAX_DLC {
            // Payloads longer than 8 bytes are rejected explicitly (see the
            // module docs).
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC}",
                data.len()
            )));
        }
        let tx = build_tx_struct(can_id, data);
        // SAFETY: handle is valid; tx lives on this stack frame and its size
        // matches the layout NI-CAN expects (14 bytes); ncWriteObject sends
        // synchronously and does not retain the pointer.
        let st = unsafe {
            (self.api.nc_write_object)(self.handle, std::mem::size_of::<NcCanStruct>() as u32, &tx)
        };
        // A non-success ncWriteObject status yields 0 (no error is raised).
        if st != NC_SUCCESS {
            return Ok(0);
        }
        // Update statistics and return the payload length.
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(Some(frame));
        }
        if self.handle == u32::MAX {
            return Ok(None);
        }
        // Drain ncReadObject until a non-success status; frames with
        // DataLength > 0 are queued (error/empty frames are skipped; a
        // non-OK status simply ends the drain).
        loop {
            let mut rx = NcCanFrameTimed {
                timestamp: 0,
                arbitration_id: 0,
                frame_type: 0,
                data_length: 0,
                data: 0,
            };
            // SAFETY: handle is valid; rx lives on this stack frame and its
            // size matches the layout NI-CAN expects (22 bytes).
            let st = unsafe {
                (self.api.nc_read_object)(
                    self.handle,
                    std::mem::size_of::<NcCanFrameTimed>() as u32,
                    &mut rx,
                )
            };
            if st != NC_SUCCESS {
                break;
            }
            // Read packed fields by value (references to packed fields are
            // not allowed).
            let (arbitration_id, data_length, data) = (rx.arbitration_id, rx.data_length, rx.data);
            if accept_rx_frame(data_length) {
                // Build the frame from the NI-format ID and the
                // little-endian-packed payload.
                self.rx_queue.push_back(CanFrame::from_u64_le(
                    &self.bus_id,
                    from_ni_can_id(arbitration_id),
                    data,
                    data_length as usize,
                    false,
                ));
            }
        }
        Ok(self.rx_queue.pop_front())
    }
}

impl Drop for NiCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CAN_EXT_FLAG;

    #[test]
    fn struct_layout_sizes() {
        // The packed struct sizes must be 14 and 22 bytes.
        assert_eq!(std::mem::size_of::<NcCanStruct>(), 14);
        assert_eq!(std::mem::size_of::<NcCanFrameTimed>(), 22);
    }

    #[test]
    fn baudrate_table_matches_expected_contract() {
        // The full supported-baud-rate mapping (values are Hz).
        assert_eq!(ni_baudrate_hz(CanBaudrate::B10Kbit).unwrap(), 10_000);
        assert_eq!(ni_baudrate_hz(CanBaudrate::B100Kbit).unwrap(), 100_000);
        assert_eq!(ni_baudrate_hz(CanBaudrate::B125Kbit).unwrap(), 125_000);
        assert_eq!(ni_baudrate_hz(CanBaudrate::B250Kbit).unwrap(), 250_000);
        assert_eq!(ni_baudrate_hz(CanBaudrate::B500Kbit).unwrap(), 500_000);
        assert_eq!(ni_baudrate_hz(CanBaudrate::B1Mbit).unwrap(), 1_000_000);
        // Baud rates outside the table -> NotSupported.
        for b in [
            CanBaudrate::NotSet,
            CanBaudrate::B20Kbit,
            CanBaudrate::B50Kbit,
            CanBaudrate::B800Kbit,
            CanBaudrate::B2Mbit,
        ] {
            assert!(
                matches!(ni_baudrate_hz(b), Err(Error::NotSupported(_))),
                "baudrate {b} should be rejected"
            );
        }
    }

    #[test]
    fn config_attrs_match_expected_contract() {
        // The attribute table: 8 entries in a fixed order (baud rate last).
        let (ids, values) = build_config_attrs(500_000);
        assert_eq!(
            ids,
            [
                0x8000_0006,
                0x8000_0013,
                0x8000_0014,
                0x8001_0001,
                0x8001_0003,
                0x8001_0002,
                0x8001_0004,
                0x8000_0007,
            ]
        );
        assert_eq!(values, [1, 0, 0, 0, 0, 0, 0, 500_000]);
    }

    #[test]
    fn tx_struct_matches_expected_contract() {
        // Standard frame: arbitration_id as-is; IsRemote=0; payload packed
        // little-endian.
        let tx = build_tx_struct(0x123, &[0x11, 0x22, 0x33]);
        // Packed struct: copy fields to locals by value before asserting
        // (references to packed fields are not allowed).
        let (arb_id, data) = (tx.arbitration_id, tx.data);
        assert_eq!(arb_id, 0x123);
        assert_eq!(tx.is_remote, 0);
        assert_eq!(tx.data_length, 3);
        assert_eq!(data, 0x0000_0000_0033_2211);
        // Extended frame: 0x80000000 -> NI 0x20000000 (see `to_ni_can_id`).
        let tx = build_tx_struct(0x123 | CAN_EXT_FLAG, &[0xAB; 8]);
        let (arb_id, data) = (tx.arbitration_id, tx.data);
        assert_eq!(arb_id, 0x2000_0123);
        assert_eq!(tx.data_length, 8);
        assert_eq!(data, 0xABAB_ABAB_ABAB_ABAB);
        // Empty payload.
        let tx = build_tx_struct(0x7FF, &[]);
        assert_eq!(tx.data_length, 0);
        assert_eq!({ tx.data }, 0);
    }

    #[test]
    fn rx_accept_filter() {
        // Only frames with DataLength > 0 are queued; the frame-type test is
        // constant-false and thus ignored.
        assert!(accept_rx_frame(1));
        assert!(accept_rx_frame(8));
        assert!(!accept_rx_frame(0));
    }

    #[test]
    fn interface_name_format() {
        // NI-CAN channel naming convention: "CAN{channel}".
        assert_eq!(interface_name(0), "CAN0");
        assert_eq!(interface_name(3), "CAN3");
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(0), "CanSuccess");
        assert_eq!(status_name(0xBFF6_2001), "CanErrFunctionTimeout");
        assert_eq!(status_name(0xBFF6_2024), "CanErrBadHandle");
        assert_eq!(status_name(0xBFF6_2124), "CanErrBadBaudRate");
        assert_eq!(status_name(0xDEAD_BEEF), "CanErr???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        let err = check(0xBFF6_2001, "NI/CAN1", "ncReadObject").unwrap_err();
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("NI/CAN1"));
                assert!(msg.contains("ncReadObject"));
                assert!(msg.contains("CanErrFunctionTimeout"));
                assert!(msg.contains("0xBFF62001"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield Error::Driver,
        // not a panic.
        let err = NiCanApi::load_from("no_such_nican_dll_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn load_real_driver_or_driver_error() {
        // Without the NI-CAN runtime installed: Error::Driver (nican.dll
        // missing). On a machine with the driver installed: construction must
        // succeed with a complete symbol table. Both outcomes are acceptable;
        // the key point is that neither panics.
        match NiCan::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
                assert!(dev.unique_bus_id() >= 1);
                // With no open channel, send returns 0 and receive returns None.
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

    #[test]
    fn classic_can_max_payload_is_8() {
        // NI-CAN is classic CAN only: the TX structure has a fixed 8-byte
        // data field and send() returns Error::Invalid for > 8 bytes (the
        // hardware path is not exercised here; this anchors the boundary
        // constant).
        assert_eq!(MAX_DLC, 8);
        let tx = build_tx_struct(0x123, &[0u8; MAX_DLC]);
        assert_eq!(tx.data_length, 8);
    }
}
