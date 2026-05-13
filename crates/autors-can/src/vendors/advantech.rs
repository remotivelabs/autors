//! Advantech CAN board adapter (PCI-1680U / PCM-3680 family and similar).
//! Unlike the Kvaser/Vector/Peak adapters, this vendor does **not** load a
//! vendor user-mode DLL: the adapter P/Invokes `kernel32.dll` directly. It
//! opens the device object `\\.\can{channel + 1}` exposed by the Advantech
//! CAN kernel driver with `CreateFile`, uses `DeviceIoControl` for status
//! queries and configuration, and exchanges fixed-size 22-byte wire frames
//! with `ReadFile`/`WriteFile` (`Pack = 1`: u32 flags + u32 reserved +
//! u32 can_id + u16 len + `u8 data[8]`). The length argument to the read/write
//! calls is always 1 — the driver interprets the ReadFile/WriteFile length
//! as a **message count**, not a byte count (pass one element and verify the
//! returned count == 1).
//!
//! Behavioral notes:
//! - `send` ignores `FrameType` entirely (classic CAN device; wire frames
//!   always carry 8 data bytes); `can_id` is written into the wire frame
//!   verbatim, including the 0x80000000 extended-frame flag bit.
//! - The extended-frame branch of the receive filter is dead code: bit 3
//!   first adds the extended flag to the ID, then frames with bit 3 set are
//!   unconditionally discarded (see `filter_rx_frame`).
//! - If `open` fails partway through, an already-opened handle is
//!   intentionally not closed by the failing call; cleanup is left to
//!   `close()`/`Drop`.
//!
//! Entry point: [`AdvantechCan`]. Windows-only (Win32 `kernel32` API).

use std::ffi::{c_void, CString};

use async_trait::async_trait;
use autors_native::DllWrapper;

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG, MAX_DLC};
use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFrame, FrameType};

/// Target DLL for all P/Invoke calls: everything binds to kernel32.
const KERNEL32_DLL: &str = "kernel32.dll";

// ---- Win32 / driver protocol constants ----
/// CreateFile dwDesiredAccess: GENERIC_READ | GENERIC_WRITE (3221225472).
const GENERIC_READ_WRITE: u32 = 0xC000_0000;
/// CreateFile dwCreationDisposition: OPEN_EXISTING (3).
const OPEN_EXISTING: u32 = 3;
/// CreateFile dwFlagsAndAttributes: FILE_ATTRIBUTE_NORMAL (128).
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// DeviceIoControl control code: status query (decimal 2237780; failure
/// message "DeviceIoControl/Status failed!"). CTL_CODE(0x22, 0x955, BUFFERED, ANY).
const IOCTL_CAN_STATUS: u32 = 0x0022_2554;
/// DeviceIoControl control code: configuration (decimal 2237764; shared by
/// the "Config stop" and "Config timing" failure messages).
/// CTL_CODE(0x22, 0x951, BUFFERED, ANY).
const IOCTL_CAN_CONFIG: u32 = 0x0022_2544;
/// Wire-frame size in bytes: 4 + 4 + 4 + 2 + 8 with Pack=1.
const WIRE_FRAME_SIZE: usize = 22;
/// Size of the configuration/baud-rate structure: 6 × u32.
const CFG_STRUCT_SIZE: usize = 24;
/// Size of the status structure: 17 × u32.
const STATUS_STRUCT_SIZE: usize = 68;
/// Wire-frame flags bit 0: frames with this bit set are discarded on receive
/// (name inferred from usage, likely RTR).
const MSG_RTR: u32 = 0x1;
/// Wire-frame flags bit 3: extended-frame flag (the receive path ORs
/// 0x80000000 into the ID based on it, then discards the frame anyway).
const MSG_EXTENDED: u32 = 0x8;
/// Value of an unopened handle (-1, i.e. INVALID_HANDLE_VALUE).
const INVALID_HANDLE: isize = -1;

/// Device path for a channel: `\\.\can{channel + 1}` (lowercase `can`).
fn device_path(channel: i32) -> String {
    format!("\\\\.\\can{}", channel + 1)
}

/// Baud rate in kbit/s: Hz / 1000. `NotSet`(0) maps to 0 (no special case).
fn kbit_rate(config: &CanConfiguration) -> u32 {
    config.baudrate.as_u32() / 1000
}

/// Little-endian encoding of the configuration structure (Pack=1, 6 × 32-bit
/// fields = 24 bytes).
fn pack6_u32(fields: [u32; 6]) -> [u8; CFG_STRUCT_SIZE] {
    let mut buf = [0u8; CFG_STRUCT_SIZE];
    for (i, f) in fields.iter().enumerate() {
        buf[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    buf
}

/// TX wire-frame encoding: flags = 0, reserved = 0 (never set on the TX
/// path), can_id verbatim, len, data zero-padded to 8 bytes. The caller
/// guarantees `data.len() <= MAX_DLC`.
fn encode_tx_frame(can_id: u32, data: &[u8]) -> [u8; WIRE_FRAME_SIZE] {
    let mut buf = [0u8; WIRE_FRAME_SIZE];
    // [0..4) flags = 0; [4..8) reserved = 0.
    buf[8..12].copy_from_slice(&can_id.to_le_bytes());
    buf[12..14].copy_from_slice(&(data.len() as u16).to_le_bytes());
    buf[14..14 + data.len()].copy_from_slice(data);
    buf
}

/// RX wire-frame decode: returns `(flags, can_id, len, data[8])` (same layout
/// as `encode_tx_frame`).
fn decode_rx_frame(buf: &[u8; WIRE_FRAME_SIZE]) -> (u32, u32, u16, [u8; 8]) {
    let flags = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let can_id = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let len = u16::from_le_bytes(buf[12..14].try_into().unwrap());
    let data: [u8; 8] = buf[14..22].try_into().unwrap();
    (flags, can_id, len, data)
}

/// Receive-path ID mapping and frame filtering: `Some(id)` = accept,
/// `None` = discard.
/// Intentional quirk, kept as part of the behavioral contract: bit 3
/// (extended frame) first ORs [`CAN_EXT_FLAG`] into the ID, then any frame
/// with bit 0 or bit 3 set is unconditionally discarded, making the third
/// bit-3 check unreachable — extended frames are always dropped, and setting
/// the extended flag is dead code.
fn filter_rx_frame(flags: u32, id: u32) -> Option<u32> {
    let id = if flags & MSG_EXTENDED != 0 {
        id | CAN_EXT_FLAG
    } else {
        id
    };
    if flags & MSG_RTR != 0 || flags & MSG_EXTENDED != 0 {
        return None;
    }
    if flags & MSG_EXTENDED != 0 {
        return None;
    }
    Some(id)
}

/// kernel32 function-pointer table (all five symbols are resolved and
/// validated at construction). kernel32 is resident in every process, so no
/// Kvaser-style reference-counted init/uninit is needed.
/// HANDLE is represented as `isize` (pointer width, matching the Win32 ABI).
#[derive(Debug)]
struct Kernel32Io {
    /// Keeps the library handle alive (autors-native `DllWrapper`); the field
    /// is never accessed directly.
    _dll: DllWrapper,
    /// HANDLE CreateFileA(LPCSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD,
    /// DWORD, HANDLE). EntryPoint "CreateFile" with the default CharSet.Ansi
    /// binds to CreateFileA (kernel32 exports only CreateFileA/CreateFileW).
    create_file_a:
        unsafe extern "system" fn(*const u8, u32, u32, *mut c_void, u32, u32, isize) -> isize,
    /// BOOL CloseHandle(HANDLE).
    close_handle: unsafe extern "system" fn(isize) -> i32,
    /// BOOL ReadFile(HANDLE, LPVOID, DWORD, LPDWORD, LPOVERLAPPED).
    read_file: unsafe extern "system" fn(isize, *mut u8, u32, *mut u32, *mut c_void) -> i32,
    /// BOOL WriteFile(HANDLE, LPCVOID, DWORD, LPDWORD, LPOVERLAPPED).
    write_file: unsafe extern "system" fn(isize, *const u8, u32, *mut u32, *mut c_void) -> i32,
    /// BOOL DeviceIoControl(HANDLE, DWORD, LPVOID, DWORD, LPVOID, DWORD,
    /// LPDWORD, LPOVERLAPPED). The declared lpInBuffer is a pointer to the
    /// 24-byte configuration structure.
    device_io_control: unsafe extern "system" fn(
        isize,
        u32,
        *const u8,
        u32,
        *mut u8,
        u32,
        *mut u32,
        *mut c_void,
    ) -> i32,
}

impl Kernel32Io {
    /// Load kernel32.dll and resolve all exported symbols.
    fn load() -> Result<Self> {
        Self::load_from(KERNEL32_DLL)
    }

    /// Load from the given path/name and resolve all exported symbols (used
    /// by tests for failure paths).
    fn load_from(path: &str) -> Result<Self> {
        // Library loading (including the unsafe of running DllMain) is
        // encapsulated in autors-native's DllWrapper.
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        // SAFETY (for each `get` in the macro expansion): symbol addresses are
        // only taken within this function and copied into raw function
        // pointers; the library handle and the function pointers live in the
        // same struct, so the pointers stay valid. The generic T is a
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
        Ok(Self {
            create_file_a: sym!(
                b"CreateFileA\0",
                unsafe extern "system" fn(
                    *const u8,
                    u32,
                    u32,
                    *mut c_void,
                    u32,
                    u32,
                    isize,
                ) -> isize
            ),
            close_handle: sym!(b"CloseHandle\0", unsafe extern "system" fn(isize) -> i32),
            read_file: sym!(
                b"ReadFile\0",
                unsafe extern "system" fn(isize, *mut u8, u32, *mut u32, *mut c_void) -> i32
            ),
            write_file: sym!(
                b"WriteFile\0",
                unsafe extern "system" fn(isize, *const u8, u32, *mut u32, *mut c_void) -> i32
            ),
            device_io_control: sym!(
                b"DeviceIoControl\0",
                unsafe extern "system" fn(
                    isize,
                    u32,
                    *const u8,
                    u32,
                    *mut u8,
                    u32,
                    *mut u32,
                    *mut c_void,
                ) -> i32
            ),
            _dll: dll,
        })
    }
}

/// Advantech CAN channel adapter.
/// `receive` follows the non-blocking convention of [`CanDevice::receive`]:
/// each call probes for a single frame — the driver returns count 0
/// immediately when no frame is pending, and blocking wait is left to the
/// polling cadence of the upper layer (`crate::device::start_dispatch`).
pub struct AdvantechCan {
    core: DeviceCore,
    api: Kernel32Io,
    /// Device handle; `INVALID_HANDLE` when not open.
    handle: isize,
    /// Bus ID (generated at `open` or taken from the configuration).
    bus_id: String,
}

impl AdvantechCan {
    /// Synchronous close body, shared by the async `close` method and `Drop`.
    fn close_sync(&mut self) {
        if self.handle != INVALID_HANDLE {
            // CloseHandle, then reset to -1; the return value is ignored.
            // SAFETY: the handle is valid and only closed here; it is reset to
            // INVALID_HANDLE immediately after closing.
            unsafe {
                (self.api.close_handle)(self.handle);
            }
            self.handle = INVALID_HANDLE;
        }
    }

    /// Load kernel32 and validate the symbol table (kernel32 is always
    /// resident, so this normally cannot fail).
    pub fn new() -> Result<Self> {
        Ok(Self {
            core: DeviceCore::new(),
            api: Kernel32Io::load()?,
            handle: INVALID_HANDLE,
            bus_id: String::new(),
        })
    }

    /// Whether the device is currently open.
    pub fn is_open(&self) -> bool {
        self.handle != INVALID_HANDLE
    }

    /// Configuration sequence run after CreateFile: Status query → Config
    /// stop → Config timing → Config stop. All DeviceIoControl calls pass
    /// NULL for lpBytesReturned/lpOverlapped, and the input-buffer length is
    /// always 0.
    fn configure_device(&self, config: &CanConfiguration) -> Result<()> {
        // Status query: all-zero input buffer; 68-byte status structure as
        // output buffer (its contents are unused and discarded after the call).
        let cfg_in = [0u8; CFG_STRUCT_SIZE];
        let mut status_out = [0u8; STATUS_STRUCT_SIZE];
        // SAFETY: the handle was returned by CreateFileA (!= -1); all pointers
        // refer to buffers on this stack frame large enough for the declared
        // lengths.
        let ok = unsafe {
            (self.api.device_io_control)(
                self.handle,
                IOCTL_CAN_STATUS,
                cfg_in.as_ptr(),
                0,
                status_out.as_mut_ptr(),
                STATUS_STRUCT_SIZE as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "DeviceIoControl/Status failed! ({})",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        // Input buffer for the next three calls: field0 = 2 (enum value whose
        // semantics are unknown; transcribed by value). The same structure is
        // shared by all three calls.
        let cfg_in = pack6_u32([2, 0, 0, 0, 0, 0]);
        // Config stop: 24-byte output buffer (contents unused).
        self.ioctl_config(
            &cfg_in,
            &[0u8; CFG_STRUCT_SIZE],
            "DeviceIoControl/Config stop failed!",
        )?;
        // Config timing: the baud-rate structure {0, 3, baud_khz, 0, 0, 0} is
        // passed as the **lpOutBuffer** pointer — the driver receives the
        // configuration through the out buffer. field1 = 3 likewise has
        // unknown semantics; transcribed by value.
        let timing = pack6_u32([0, 3, kbit_rate(config), 0, 0, 0]);
        self.ioctl_config(&cfg_in, &timing, "DeviceIoControl/Config timing failed!")?;
        // Config stop (second time).
        self.ioctl_config(
            &cfg_in,
            &[0u8; CFG_STRUCT_SIZE],
            "DeviceIoControl/Config stop failed!",
        )?;
        Ok(())
    }

    /// Wrapper for `DeviceIoControl(handle, IOCTL_CAN_CONFIG, ref cfgIn, 0,
    /// outBuf, 24, NULL, NULL)`: the input-buffer pointer is valid but its
    /// length is always 0, and the output buffer is 24 bytes. `out_init`
    /// provides the initial contents of the output buffer (the timing call
    /// uses this to pass the baud-rate structure to the driver).
    fn ioctl_config(
        &self,
        cfg_in: &[u8; CFG_STRUCT_SIZE],
        out_init: &[u8; CFG_STRUCT_SIZE],
        what: &str,
    ) -> Result<()> {
        let mut out = *out_init;
        // SAFETY: the handle was returned by CreateFileA (!= -1); both buffers
        // live on this stack frame and match the declared lengths (input
        // length always 0); lpBytesReturned/lpOverlapped are NULL.
        let ok = unsafe {
            (self.api.device_io_control)(
                self.handle,
                IOCTL_CAN_CONFIG,
                cfg_in.as_ptr(),
                0,
                out.as_mut_ptr(),
                CFG_STRUCT_SIZE as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(config_err(
                &self.bus_id,
                format!("{what} ({})", std::io::Error::last_os_error()),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl CanDevice for AdvantechCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        // `open()` performs the full open sequence; `is_available()` only
        // reports the current state.
        Ok(self.handle != INVALID_HANDLE)
    }

    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        if self.handle != INVALID_HANDLE {
            return Ok(true);
        }
        // Default bus ID when the configuration does not provide one:
        // "Advantech/CAN{channel + 1}".
        self.bus_id = config
            .bus_id
            .clone()
            .unwrap_or_else(|| format_bus_id("Advantech", config.channel));
        let path = device_path(config.channel);
        let c_path = CString::new(path.as_str())
            .map_err(|_| Error::Invalid(format!("device path contains NUL byte: {path:?}")))?;
        // SAFETY: c_path is NUL-terminated and valid for the call; the
        // remaining pointer arguments are NULL; the returned handle is owned
        // by this struct, which is responsible for CloseHandle.
        let handle = unsafe {
            (self.api.create_file_a)(
                c_path.as_ptr() as *const u8,
                GENERIC_READ_WRITE,
                0, // dwShareMode = 0 (exclusive access)
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                0, // hTemplateFile = NULL
            )
        };
        if handle == INVALID_HANDLE {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "DeviceIoControl/CreateFile failed! ({path}: {})",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        // Once CreateFile succeeds, the handle belongs to this adapter; if a
        // later IOCTL fails, the handle is intentionally **not** closed by the
        // failing call — cleanup is left to close()/Drop.
        self.handle = handle;
        self.configure_device(&config)?;
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, can_id: u32, data: &[u8], _frame_type: FrameType) -> Result<usize> {
        // Not open -> return 0. The FrameType parameter is ignored entirely
        // (classic CAN device; wire frames always carry 8 data bytes).
        if self.handle == INVALID_HANDLE {
            return Ok(0);
        }
        // The wire frame's data field is a fixed byte[8]; longer payloads are
        // rejected with Error::Invalid.
        if data.len() > MAX_DLC {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds classic CAN maximum of {MAX_DLC} \
                 (Advantech wire frame is fixed 8 bytes)",
                data.len()
            )));
        }
        let buf = encode_tx_frame(can_id, data);
        let mut written: u32 = 0;
        // SAFETY: the handle is valid; buf is 22 bytes and valid for the call;
        // the length argument is always 1 — the driver interprets it as a
        // message count (pass one element and check written == 1);
        // lpOverlapped is NULL.
        let ok = unsafe {
            (self.api.write_file)(
                self.handle,
                buf.as_ptr(),
                1,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        // WriteFile failure or a count != 1 yields 0 (no error is raised).
        if ok == 0 || written != 1 {
            return Ok(0);
        }
        // Record the statistics and return the data length (frame type
        // defaults to CAN20B).
        let frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, FrameType::CAN20B);
        Ok(self.core.record_sent(&frame))
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        // Not open or no frame pending -> Ok(None).
        if self.handle == INVALID_HANDLE {
            return Ok(None);
        }
        let mut buf = [0u8; WIRE_FRAME_SIZE];
        let mut read: u32 = 0;
        // SAFETY: the handle is valid; buf is 22 bytes, exactly one wire
        // frame; the length argument is always 1 (message count, see the note
        // in send); lpOverlapped is NULL.
        let ok = unsafe {
            (self.api.read_file)(
                self.handle,
                buf.as_mut_ptr(),
                1,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return Ok(None);
        }
        let (flags, raw_id, len, data) = decode_rx_frame(&buf);
        let id = match filter_rx_frame(flags, raw_id) {
            Some(id) => id,
            None => return Ok(None),
        };
        // len is first truncated to a byte (ushort -> low 8 bits); if the
        // truncated value exceeds 8, with_len fails with Error::Invalid, which
        // likewise terminates the upper-layer polling loop via Err.
        let frame = CanFrame::with_len(
            &self.bus_id,
            id,
            data.to_vec(),
            (len as u8) as usize,
            false,
            FrameType::CAN20B,
        )?;
        Ok(Some(frame))
    }
}

impl Drop for AdvantechCan {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{CanBaudrate, CanFdBaudrate};

    #[test]
    fn wire_layout_constants() {
        // Struct sizes with Pack=1: frame 22, config 24, status 68.
        assert_eq!(WIRE_FRAME_SIZE, 22);
        assert_eq!(CFG_STRUCT_SIZE, 24);
        assert_eq!(STATUS_STRUCT_SIZE, 68);
        // IOCTL codes as decimal literals: 2237780 / 2237764.
        assert_eq!(IOCTL_CAN_STATUS, 2237780);
        assert_eq!(IOCTL_CAN_CONFIG, 2237764);
        assert_eq!(GENERIC_READ_WRITE, 3221225472);
    }

    #[test]
    fn device_path_formatting() {
        // Format string "\\.\can{0}", channel number + 1.
        assert_eq!(device_path(0), "\\\\.\\can1");
        assert_eq!(device_path(1), "\\\\.\\can2");
        assert_eq!(device_path(3), "\\\\.\\can4");
    }

    #[test]
    fn kbit_rate_conversion() {
        // Baudrate / 1000.
        let c = CanConfiguration::new(0, CanBaudrate::B500Kbit, CanFdBaudrate::NotUsed);
        assert_eq!(kbit_rate(&c), 500);
        let c = CanConfiguration::new(0, CanBaudrate::B1Mbit, CanFdBaudrate::NotUsed);
        assert_eq!(kbit_rate(&c), 1000);
        let c = CanConfiguration::new(0, CanBaudrate::B125Kbit, CanFdBaudrate::NotUsed);
        assert_eq!(kbit_rate(&c), 125);
        // NotSet -> 0 (no special case).
        let c = CanConfiguration::default();
        assert_eq!(kbit_rate(&c), 0);
    }

    #[test]
    fn pack6_u32_layout() {
        let buf = pack6_u32([0x1122_3344, 2, 3, 0, 0, 0xAABB_CCDD]);
        assert_eq!(buf.len(), 24);
        assert_eq!(&buf[0..4], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(&buf[4..8], &2u32.to_le_bytes());
        assert_eq!(&buf[8..12], &3u32.to_le_bytes());
        assert_eq!(&buf[12..20], &[0; 8]);
        assert_eq!(&buf[20..24], &[0xDD, 0xCC, 0xBB, 0xAA]);
    }

    #[test]
    fn tx_frame_encoding() {
        let buf = encode_tx_frame(0x123, &[0x11, 0x22, 0x33]);
        assert_eq!(buf.len(), 22);
        // flags and reserved fields are always 0 (never set on the TX path).
        assert_eq!(&buf[0..8], &[0; 8]);
        assert_eq!(&buf[8..12], &0x123u32.to_le_bytes());
        assert_eq!(&buf[12..14], &3u16.to_le_bytes());
        assert_eq!(&buf[14..17], &[0x11, 0x22, 0x33]);
        assert_eq!(&buf[17..22], &[0; 5]); // zero padding

        // The extended-flag bit is passed through into the ID field verbatim
        // (no masking).
        let buf = encode_tx_frame(0x8000_0123, &[0xAB; 8]);
        assert_eq!(&buf[8..12], &0x8000_0123u32.to_le_bytes());
        assert_eq!(&buf[12..14], &8u16.to_le_bytes());
        assert_eq!(&buf[14..22], &[0xAB; 8]);
    }

    #[test]
    fn rx_frame_decoding() {
        let mut buf = [0u8; 22];
        buf[0..4].copy_from_slice(&0x8u32.to_le_bytes()); // flags
        buf[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // reserved (ignored)
        buf[8..12].copy_from_slice(&0x456u32.to_le_bytes());
        buf[12..14].copy_from_slice(&2u16.to_le_bytes());
        buf[14..16].copy_from_slice(&[0xAA, 0xBB]);
        let (flags, id, len, data) = decode_rx_frame(&buf);
        assert_eq!(flags, 0x8);
        assert_eq!(id, 0x456);
        assert_eq!(len, 2);
        assert_eq!(&data[0..2], &[0xAA, 0xBB]);
        assert_eq!(&data[2..8], &[0; 6]);
    }

    #[test]
    fn rx_filter_matches_expected_contract() {
        // Standard data frame (flags = 0): accepted, ID unchanged.
        assert_eq!(filter_rx_frame(0, 0x123), Some(0x123));
        // Unrecognized other bits (e.g. 0x2): not checked, accepted.
        assert_eq!(filter_rx_frame(0x2, 0x123), Some(0x123));
        // bit 0 (likely RTR): discarded.
        assert_eq!(filter_rx_frame(MSG_RTR, 0x123), None);
        // bit 3 (extended frame): dead branch — the extended flag is added to
        // the ID and the frame is then unconditionally discarded.
        assert_eq!(filter_rx_frame(MSG_EXTENDED, 0x123), None);
        assert_eq!(filter_rx_frame(MSG_RTR | MSG_EXTENDED, 0x123), None);
    }

    #[test]
    fn missing_dll_is_driver_error() {
        // A DLL name that definitely does not exist: must yield Error::Driver,
        // not a panic.
        let err = Kernel32Io::load_from("no_such_advantech_kernel32_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[test]
    fn kernel32_symbols_resolve() {
        // kernel32 is resident in every system process: loading it and
        // resolving all 5 symbols must succeed.
        Kernel32Io::load().expect("kernel32.dll must load with all 5 exports");
    }

    #[test]
    fn unopened_device_semantics() {
        // Unopened semantics: is_available() = false, send returns 0, receive
        // yields no frame.
        let mut dev = AdvantechCan::new().unwrap();
        assert!(!dev.is_open());
        assert!(!autors_runtime::block_on(dev.is_available()).unwrap());
        assert!(dev.unique_bus_id() >= 1);
        assert_eq!(
            autors_runtime::block_on(dev.send(0x123, &[1, 2, 3], FrameType::CAN20B)).unwrap(),
            0
        );
        // When unopened, even an overlong payload hits the "not open" branch
        // first and returns 0 (short-circuit).
        assert_eq!(
            autors_runtime::block_on(dev.send(0x123, &[0u8; 9], FrameType::CAN20B)).unwrap(),
            0
        );
        assert!(autors_runtime::block_on(dev.receive()).unwrap().is_none());
        autors_runtime::block_on(dev.close()); // no-op when unopened
        assert!(!dev.is_open());
    }

    #[test]
    fn open_without_driver_fails_cleanly() {
        // On a machine without the Advantech driver (\\.\can* fails with
        // ERROR_FILE_NOT_FOUND), open must return Error::Driver, not panic.
        // Channel 250 makes the chance that such a device actually exists
        // negligible; if a driver really is present, a successful open is
        // accepted too.
        let mut dev = AdvantechCan::new().unwrap();
        let cfg = CanConfiguration::new(250, CanBaudrate::B500Kbit, CanFdBaudrate::NotUsed);
        match autors_runtime::block_on(dev.open(cfg)) {
            Ok(_) => {
                assert!(dev.is_open());
                autors_runtime::block_on(dev.close());
                assert!(!dev.is_open());
            }
            Err(Error::Driver(msg)) => {
                // config_err merges the bus ID into the message.
                assert!(msg.contains("Advantech/CAN251"), "got: {msg}");
                assert!(msg.contains("DeviceIoControl/"), "got: {msg}");
                autors_runtime::block_on(dev.close()); // cleanup (the handle may still be open after an IOCTL failure)
                assert!(!dev.is_open());
            }
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
