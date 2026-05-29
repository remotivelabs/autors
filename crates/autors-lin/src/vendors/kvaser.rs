//! Kvaser linlib driver adapter.
//! Dynamically loads `linlib.dll` (Kvaser LINlib, a public C API) at runtime.
//! linlib uses the stdcall calling convention; on x64 that matches the C
//! calling convention, so `extern "system"` is used uniformly. When the DLL is
//! not installed or an export is missing, construction returns
//! [`Error::Driver`].

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autors_native::DllWrapper;

use super::{config_err, next_unique_bus_id};
use crate::device::{make_bus_id, LinConfiguration, LinDevice, LinFrame, MAX_DATA_LEN};
use crate::error::{Error, Result};

/// linlib library name.
const LINLIB_DLL: &str = "linlib.dll";

/// LINstatus OK (linlib.h, same value as canstat.h canOK).
const LIN_OK: i32 = 0;
/// Flags constant passed to `linOpenChannel(channel, flags)` when opening a channel.
const LIN_OPEN_FLAGS: u32 = 1;
/// linlib receive flag bit0: sent by this node (master frame).
const LIN_RX_FLAG_TX: u32 = 1;
/// linlib receive flag bit1: received from another node.
const LIN_RX_FLAG_RX: u32 = 2;
/// Header-only and bus-error flags that do not carry a valid response.
const LIN_RX_ERROR_MASK: u32 = 8 | 16 | 32 | 64 | 128;

/// Static reference count for linlib: the first instance initializes the
/// library and the last instance calls the optional shutdown export.
static LINLIB_REFCOUNT: AtomicUsize = AtomicUsize::new(0);

/// LINstatus name from `linlib.h`.
fn status_name(status: i32) -> &'static str {
    match status {
        0 => "linOK",
        -1 => "linERR_NOMSG",
        -3 => "linERR_NOTRUNNING",
        -4 => "linERR_RUNNING",
        -5 => "linERR_MASTERONLY",
        -6 => "linERR_SLAVEONLY",
        -7 => "linERR_PARAM",
        -8 => "linERR_NOTFOUND",
        -9 => "linERR_NOMEM",
        -10 => "linERR_NOCHANNELS",
        -11 => "linERR_TIMEOUT",
        -12 => "linERR_NOTINITIALIZED",
        -13 => "linERR_NOHANDLES",
        -14 => "linERR_INVHANDLE",
        -15 => "linERR_CANERROR",
        -16 => "linERR_ERRRESP",
        -17 => "linERR_WRONGRESP",
        -18 => "linERR_DRIVER",
        -19 => "linERR_DRIVERFAILED",
        -20 => "linERR_NOCARD",
        -21 => "linERR_LICENSE",
        -22 => "linERR_INTERNAL",
        -23 => "linERR_NO_ACCESS",
        -24 => "linERR_VERSION",
        -25 => "linERR_NO_REF_POWER",
        -26 => "linERR_NOT_IMPLEMENTED",
        _ => "linERR_???",
    }
}

/// Whether a received frame is a master frame, per linlib semantics
/// (receive flag bit0 = TX).
fn is_master_frame(flags: u32) -> bool {
    flags & LIN_RX_FLAG_TX != 0
}

fn accept_lin_flags(flags: u32) -> bool {
    flags & LIN_RX_ERROR_MASK == 0 && flags & (LIN_RX_FLAG_TX | LIN_RX_FLAG_RX) != 0
}

/// Packed `LinMessageInfo` ABI structure from `linlib.h`.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
struct LinMessageInfo {
    timestamp: u32,
    synch_break_length: u32,
    frame_length: u32,
    bitrate: u32,
    checksum: u8,
    id_parity: u8,
    reserved: u16,
    synch_edge_time: [u32; 4],
    byte_time: [u32; 8],
}

const _: () = assert!(std::mem::size_of::<LinMessageInfo>() == 68);

/// linlib function-pointer table (loaded and validated at construction).
/// Field types follow the linlib.h signatures.
#[derive(Debug)]
struct Linlib {
    /// Keeps the library handle alive (autors-native [`DllWrapper`]); the field
    /// itself is never accessed directly.
    _dll: DllWrapper,
    /// void linInitializeLibrary(void).
    lin_initialize_library: unsafe extern "system" fn(),
    /// Optional library shutdown export; no call is made when it is absent.
    lin_unload_library: Option<unsafe extern "system" fn()>,
    /// LinHandle linOpenChannel(int channel, unsigned int flags).
    lin_open_channel: unsafe extern "system" fn(i32, u32) -> i32,
    /// LINstatus linSetBitrate(LinHandle hnd, unsigned int bps).
    lin_set_bitrate: unsafe extern "system" fn(i32, u32) -> i32,
    /// LINstatus linClose(LinHandle hnd).
    lin_close: unsafe extern "system" fn(i32) -> i32,
    /// LINstatus linBusOn(LinHandle hnd).
    lin_bus_on: unsafe extern "system" fn(i32) -> i32,
    /// LINstatus linBusOff(LinHandle hnd).
    lin_bus_off: unsafe extern "system" fn(i32) -> i32,
    /// LINstatus linWriteMessage(LinHandle hnd, unsigned int id, const void *msg,
    /// unsigned int dlc).
    lin_write_message: unsafe extern "system" fn(i32, u32, *const u8, u32) -> i32,
    /// LINstatus linRequestMessage(LinHandle hnd, unsigned int id).
    lin_request_message: unsafe extern "system" fn(i32, u32) -> i32,
    /// LINstatus linReadMessage(LinHandle hnd, unsigned int *id, void *msg,
    /// unsigned int *dlc, unsigned int *flags, LinMessageInfo *msgInfo).
    lin_read_message: unsafe extern "system" fn(
        i32,
        *mut u32,
        *mut u8,
        *mut u32,
        *mut u32,
        *mut LinMessageInfo,
    ) -> i32,
    /// LINstatus linUpdateMessage(LinHandle hnd, unsigned int id, const void *msg,
    /// unsigned int dlc).
    #[allow(dead_code)] // loaded for symbol-table completeness; the LIN flow never calls it
    lin_update_message: unsafe extern "system" fn(i32, u32, *const u8, u32) -> i32,
    /// LINstatus linSetupLIN(LinHandle hnd, unsigned int id, unsigned int flags).
    #[allow(dead_code)] // loaded for symbol-table completeness; the LIN flow never calls it
    lin_setup_lin: unsafe extern "system" fn(i32, u32, u32) -> i32,
    /// LINstatus linWriteWakeup(LinHandle hnd, unsigned int id, unsigned int flags).
    #[allow(dead_code)] // loaded for symbol-table completeness; the LIN flow never calls it
    lin_write_wakeup: unsafe extern "system" fn(i32, u32, u32) -> i32,
    /// LINstatus linClearMessage(LinHandle hnd, unsigned int id).
    #[allow(dead_code)] // loaded for symbol-table completeness; the LIN flow never calls it
    lin_clear_message: unsafe extern "system" fn(i32, u32) -> i32,
}

impl Linlib {
    /// Load linlib.dll from the default search path.
    fn load() -> Result<Self> {
        Self::load_from(LINLIB_DLL)
    }

    /// Load from the given path/name and resolve the exported symbols.
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe performed by DllMain) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function pointers;
        // the library handle and the function pointers live in the same struct,
        // which keeps the pointers valid for its lifetime. The generic T is a
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
        // Optional symbols (a missing export simply yields None).
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: as above; a missing symbol only returns None.
                let r = unsafe { dll.library().get::<$ty>($name) };
                r.ok().map(|s| *s)
            }};
        }
        let api = Self {
            lin_initialize_library: sym!(b"linInitializeLibrary\0", unsafe extern "system" fn()),
            lin_unload_library: opt_sym!(b"linUnloadLibrary\0", unsafe extern "system" fn()),
            lin_open_channel: sym!(
                b"linOpenChannel\0",
                unsafe extern "system" fn(i32, u32) -> i32
            ),
            lin_set_bitrate: sym!(
                b"linSetBitrate\0",
                unsafe extern "system" fn(i32, u32) -> i32
            ),
            lin_close: sym!(b"linClose\0", unsafe extern "system" fn(i32) -> i32),
            lin_bus_on: sym!(b"linBusOn\0", unsafe extern "system" fn(i32) -> i32),
            lin_bus_off: sym!(b"linBusOff\0", unsafe extern "system" fn(i32) -> i32),
            lin_write_message: sym!(
                b"linWriteMessage\0",
                unsafe extern "system" fn(i32, u32, *const u8, u32) -> i32
            ),
            lin_request_message: sym!(
                b"linRequestMessage\0",
                unsafe extern "system" fn(i32, u32) -> i32
            ),
            lin_read_message: sym!(
                b"linReadMessage\0",
                unsafe extern "system" fn(
                    i32,
                    *mut u32,
                    *mut u8,
                    *mut u32,
                    *mut u32,
                    *mut LinMessageInfo,
                ) -> i32
            ),
            lin_update_message: sym!(
                b"linUpdateMessage\0",
                unsafe extern "system" fn(i32, u32, *const u8, u32) -> i32
            ),
            lin_setup_lin: sym!(
                b"linSetupLIN\0",
                unsafe extern "system" fn(i32, u32, u32) -> i32
            ),
            lin_write_wakeup: sym!(
                b"linWriteWakeup\0",
                unsafe extern "system" fn(i32, u32, u32) -> i32
            ),
            lin_clear_message: sym!(
                b"linClearMessage\0",
                unsafe extern "system" fn(i32, u32) -> i32
            ),
            _dll: dll,
        };
        // The first instance runs linInitializeLibrary().
        if LINLIB_REFCOUNT.fetch_add(1, Ordering::SeqCst) == 0 {
            // SAFETY: the function pointer comes from the loaded linlib.dll; no
            // arguments, no return value.
            unsafe { (api.lin_initialize_library)() };
        }
        Ok(api)
    }
}

impl Drop for Linlib {
    fn drop(&mut self) {
        // The last instance runs the uninit function (only called when present;
        // see the field comment).
        if LINLIB_REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            if let Some(unload) = self.lin_unload_library {
                // SAFETY: paired with linInitializeLibrary; the library handle is
                // released only afterwards.
                unsafe { unload() };
            }
        }
    }
}

/// Kvaser LIN channel adapter.
/// `open()` performs the full open sequence (openChannel + setBitrate + busOn)
/// while `is_available()` only reports status, per the [`LinDevice`] split
/// (same convention as the autors-can adapters). Reception is driven by polling
/// [`LinDevice::on_receive`].
pub struct KvaserLin {
    api: Linlib,
    /// Process-wide unique bus ID.
    unique_bus_id: i32,
    /// linlib channel handle (< 0 means not open).
    handle: i32,
    /// Hardware-available flag.
    available: bool,
    /// Bus ID (generated at open or taken from the configuration; format
    /// `"Kvaser/LIN{channel+1}"`, matching [`make_bus_id`]).
    bus_id: String,
}

impl KvaserLin {
    /// Load linlib.dll and validate the symbol table; returns [`Error::Driver`]
    /// when the driver is not installed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: Linlib::load()?,
            unique_bus_id: next_unique_bus_id(),
            handle: -1,
            available: false,
            bus_id: String::new(),
        })
    }

    /// Whether a channel is currently open.
    pub fn is_open(&self) -> bool {
        self.handle >= 0
    }

    /// Synchronous close implementation (shared by `LinDevice::close` and `Drop`).
    fn close_sync(&mut self) {
        self.available = false;
        if self.handle >= 0 {
            // linBusOff + linClose (return values intentionally ignored).
            // SAFETY: the handle is valid.
            unsafe {
                (self.api.lin_bus_off)(self.handle);
                (self.api.lin_close)(self.handle);
            }
            self.handle = -1;
        }
    }
}

#[async_trait]
impl LinDevice for KvaserLin {
    fn unique_bus_id(&self) -> i32 {
        self.unique_bus_id
    }

    fn is_available(&self) -> bool {
        // Available flag plus an open channel handle.
        self.available && self.handle >= 0
    }

    async fn open(&mut self, config: &LinConfiguration) -> Result<bool> {
        if self.is_available() {
            return Ok(true);
        }
        // Generate the BusId ("Kvaser/LIN1").
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| make_bus_id("Kvaser", config.channel));
        // SAFETY: arguments passed by value; returns a channel handle (< 0 is a
        // LINstatus error code).
        let handle = unsafe { (self.api.lin_open_channel)(config.channel, LIN_OPEN_FLAGS) };
        if handle < 0 {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "linlib: linOpenChannel(channel={}, flags={LIN_OPEN_FLAGS}) failed: {} ({handle})",
                    config.channel,
                    status_name(handle)
                ),
            ));
        }
        self.handle = handle;
        // When setBitrate / busOn fails, report Ok(false) instead of an error
        // and leave the channel open; close() cleans it up.
        // SAFETY: the handle was returned by linOpenChannel (>= 0); arguments
        // passed by value.
        let st = unsafe { (self.api.lin_set_bitrate)(handle, u32::from(config.baudrate)) };
        if st != LIN_OK {
            return Ok(false);
        }
        // SAFETY: as above.
        let st = unsafe { (self.api.lin_bus_on)(handle) };
        self.available = st == LIN_OK;
        Ok(self.available)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> Result<usize> {
        // Return 0 when unavailable.
        if !self.is_available() {
            return Ok(0);
        }
        if data.len() > MAX_DATA_LEN {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds LIN maximum of {MAX_DATA_LEN}",
                data.len()
            )));
        }
        // On linWriteMessage failure return 0 instead of an error.
        // SAFETY: the handle is valid; the data pointer is valid for the
        // duration of the call and dlc matches the actual buffer; linlib sends
        // synchronously and does not retain the pointer.
        let st = unsafe {
            (self.api.lin_write_message)(
                self.handle,
                u32::from(id),
                data.as_ptr(),
                data.len() as u32,
            )
        };
        if st != LIN_OK {
            return Ok(0);
        }
        // On success, return the payload length directly.
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> Result<bool> {
        if !self.is_available() {
            return Ok(false);
        }
        // SAFETY: the handle is open and the identifier is passed by value.
        let status = unsafe { (self.api.lin_request_message)(self.handle, u32::from(id)) };
        Ok(status == LIN_OK)
    }

    async fn on_receive(&mut self) -> Result<Option<LinFrame>> {
        // No frame when unavailable.
        if !self.is_available() {
            return Ok(None);
        }
        let mut id: u32 = 0;
        let mut buf = [0u8; MAX_DATA_LEN];
        let mut dlc: u32 = 0;
        let mut flags: u32 = 0;
        let mut info = LinMessageInfo::default();
        // SAFETY: the handle is valid; buf is 8 bytes, the maximum LIN DLC; all
        // out pointers point to correctly sized variables on this stack frame.
        let st = unsafe {
            (self.api.lin_read_message)(
                self.handle,
                &mut id,
                buf.as_mut_ptr(),
                &mut dlc,
                &mut flags,
                &mut info,
            )
        };
        // Header-only and frames carrying bus errors have no usable payload.
        if st != LIN_OK || !accept_lin_flags(flags) {
            return Ok(None);
        }
        // A dlc > 8 frame is malformed: drop it and report no frame.
        if dlc > MAX_DATA_LEN as u32 {
            return Ok(None);
        }
        let frame = LinFrame::with_len(
            &self.bus_id,
            id as u8,
            buf.to_vec(),
            dlc as u8,
            is_master_frame(flags),
        )?;
        Ok(Some(frame))
    }
}

impl Drop for KvaserLin {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names() {
        assert_eq!(status_name(0), "linOK");
        assert_eq!(status_name(-1), "linERR_NOMSG");
        assert_eq!(status_name(-20), "linERR_NOCARD");
        assert_eq!(status_name(-26), "linERR_NOT_IMPLEMENTED");
        assert_eq!(status_name(-9999), "linERR_???");
    }

    #[test]
    fn receive_flags_accept_transmit_and_slave_response() {
        assert!(accept_lin_flags(LIN_RX_FLAG_TX));
        assert!(accept_lin_flags(LIN_RX_FLAG_RX));
        assert!(!accept_lin_flags(0));
        assert!(!accept_lin_flags(LIN_RX_FLAG_RX | 8));
        assert!(is_master_frame(LIN_RX_FLAG_TX));
        assert!(!is_master_frame(LIN_RX_FLAG_RX));
    }

    #[test]
    fn error_message_includes_bus_and_status() {
        // Error message convention: hardware ID + call site + LINstatus name.
        let err = config_err(
            "Kvaser/LIN1",
            format!("linlib: linBusOn failed: {} ({})", status_name(-11), -11),
        );
        match err {
            Error::Driver(msg) => {
                assert!(msg.contains("Kvaser/LIN1"));
                assert!(msg.contains("linBusOn"));
                assert!(msg.contains("linERR_TIMEOUT"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn rx_flag_interpretation() {
        // TX and RX are direction bits; error bits are filtered separately.
        assert!(is_master_frame(1));
        assert!(!is_master_frame(0));
        assert!(is_master_frame(3)); // bit0 set means master (even with status bits set)
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield Error::Driver,
        // not a panic.
        let err = Linlib::load_from("no_such_kvaser_linlib_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn load_real_driver_or_driver_error() {
        // A machine with linlib.dll but missing canlib32.dll: Error::Driver; a
        // machine with the full Kvaser driver: construction should succeed with
        // a complete symbol table (linUnloadLibrary may be missing, see the
        // Linlib field comment). Either outcome is acceptable; the key is no
        // panic.
        match KvaserLin::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!dev.is_available());
                assert!(dev.unique_bus_id() >= 1);
                // With no channel open, send returns 0 and on_receive returns None.
                assert_eq!(
                    autors_runtime::block_on(dev.send(0x3C, &[1, 2, 3])).unwrap(),
                    0
                );
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
