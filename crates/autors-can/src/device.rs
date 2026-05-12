//! CAN hardware abstraction: the `CanDevice` trait plus shared static helpers.
//! A device implementation embeds a [`DeviceCore`] — a unique bus ID,
//! bus-load statistics, and a listener table keyed by CAN ID — and exposes it
//! through [`CanDevice::core`] / [`CanDevice::core_mut`]. The trait's
//! default methods (`send_msg`, `busload`, listener registration, ...) build
//! on top of that core.
//! Design notes:
//! - Listeners are registered with a callback and unregistered by the token
//!   returned from registration, not by callback identity.
//! - The trait does not run a background receive/dispatch thread itself;
//!   [`start_dispatch`] (or a driver-managed thread) drives
//!   [`CanDevice::poll_once`].
//! - Hardware enumeration returns typed [`ChannelInfo`] values.
//! - Configuration failures carrying a hardware ID map to [`Error::Driver`]
//!   with the hardware ID folded into the message text (see [`config_err`]).
//! - Return-value convention: sends that could not be performed (e.g. channel
//!   not opened) report `Ok(0)`; configuration and driver errors map to `Err`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "blocking")]
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFrame, FrameType};

// ---------------------------------------------------------------------------
// CAN ID mask and flag constants
// ---------------------------------------------------------------------------

/// Mask for standard (11-bit) CAN IDs.
pub const CAN_STD_ID_MASK: u32 = 0x7FF;
/// Mask for extended (29-bit) CAN IDs.
pub const CAN_EXT_ID_MASK: u32 = 0x1FFF_FFFF;
/// Mask of the ID bits the library considers valid (29-bit ID plus extended flag).
pub const CAN_ID_LIBRARY_VALID_MASK: u32 = 0x9FFF_FFFF;
/// Extended-frame flag (top ID bit).
pub const CAN_EXT_FLAG: u32 = 0x8000_0000;
/// XCP 1.5 CAN FD flag bit.
pub const CAN_FD_XCP15_FLAG: u32 = 0x4000_0000;
/// XCP 1.5 valid-ID mask.
pub const CAN_ID_XCP15_VALID_MASK: u32 = 0xDFFF_FFFF;
/// Maximum payload length of a classic CAN frame.
pub const MAX_DLC: usize = 8;
/// Maximum payload length of a CAN FD frame.
pub const MAX_FD_DLC: usize = 64;

/// Listener key matching every frame.
pub const ALL_FRAMES: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Static CAN ID / DLC helpers
// ---------------------------------------------------------------------------

/// Uppercase hex rendering of a CAN ID; extended frames get an `(X)` suffix
/// (e.g. 0x123 -> `123`, 0x80000123 -> `123(X)`).
pub fn to_can_id_string(can_id: u32) -> String {
    let suffix = if can_id & CAN_EXT_FLAG != 0 {
        "(X)"
    } else {
        ""
    };
    format!("{:X}{suffix}", can_id & CAN_EXT_ID_MASK)
}

/// Renders two CAN IDs as `{id1}/{id2}` (separator `/`).
pub fn to_can_id_pair_string(can_id1: u32, can_id2: u32) -> String {
    format!(
        "{}/{}",
        to_can_id_string(can_id1),
        to_can_id_string(can_id2)
    )
}

/// Orders by raw ID first, then by the extended flag (so 0x123 < 0x80000123).
pub fn can_id_sort(a: u32, b: u32) -> Ordering {
    let raw_a = a & 0x7FFF_FFFF;
    let raw_b = b & 0x7FFF_FFFF;
    raw_a
        .cmp(&raw_b)
        .then_with(|| (a & CAN_EXT_FLAG != 0).cmp(&(b & CAN_EXT_FLAG != 0)))
}

/// Maps a payload length to the smallest DLC that can carry it.
/// The message text is part of the behavioral contract: length > 64 ->
/// `Length {n} is not allowed in a CAN frame.`
pub fn length_to_dlc(length: usize) -> Result<u8> {
    if length <= 8 {
        return Ok(length as u8);
    }
    let dlc = if length <= 12 {
        9
    } else if length <= 16 {
        10
    } else if length <= 20 {
        11
    } else if length <= 24 {
        12
    } else if length <= 32 {
        13
    } else if length <= 48 {
        14
    } else if length <= 64 {
        15
    } else {
        return Err(Error::Invalid(format!(
            "Length {length} is not allowed in a CAN frame."
        )));
    };
    Ok(dlc)
}

/// Maps a DLC to the payload length it encodes.
/// The message text is part of the behavioral contract: DLC > 15 ->
/// `Invalid DLC {n} for a CAN Frame`.
pub fn dlc_to_length(dlc: u8) -> Result<u8> {
    if dlc <= 8 {
        return Ok(dlc);
    }
    match dlc {
        9 => Ok(12),
        10 => Ok(16),
        11 => Ok(20),
        12 => Ok(24),
        13 => Ok(32),
        14 => Ok(48),
        15 => Ok(64),
        _ => Err(Error::Invalid(format!("Invalid DLC {dlc} for a CAN Frame"))),
    }
}

/// Returns `max_dlc` when `max_dlc > 0`; otherwise the payload size unchanged
/// when <= 8, or rounded up to the next FD length tier (12/16/20/24/32/48/64).
pub fn send_msg_len(payload_size: usize, max_dlc: usize) -> usize {
    if max_dlc > 0 {
        return max_dlc;
    }
    if payload_size <= 8 {
        payload_size
    } else if payload_size <= 12 {
        12
    } else if payload_size <= 16 {
        16
    } else if payload_size <= 20 {
        20
    } else if payload_size <= 24 {
        24
    } else if payload_size <= 32 {
        32
    } else if payload_size <= 48 {
        48
    } else {
        64
    }
}

/// Converts the extended flag 0x80000000 into NI's 0x20000000 flag.
pub fn to_ni_can_id(id: u32) -> u32 {
    (if id & CAN_EXT_FLAG != 0 {
        id | 0x2000_0000
    } else {
        id
    }) & 0x7FFF_FFFF
}

/// Converts NI's 0x20000000 flag back into the extended flag 0x80000000.
pub fn from_ni_can_id(id: u32) -> u32 {
    (if id & 0x2000_0000 != 0 {
        id | CAN_EXT_FLAG
    } else {
        id
    }) & 0xDFFF_FFFF
}

/// Reports whether `id` is a valid CAN ID under the library's encoding.
pub fn is_can_id_valid(id: u32) -> bool {
    if id & CAN_EXT_FLAG == 0 {
        id <= CAN_STD_ID_MASK
    } else {
        id & 0x7FFF_FFFF <= CAN_EXT_ID_MASK
    }
}

/// Packs up to 8 bytes little-endian into a `u64` (input longer than 8 bytes
/// is truncated).
pub fn data_from_array(data: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for (i, &b) in data.iter().take(8).enumerate() {
        v |= (b as u64) << (8 * i);
    }
    v
}

/// Formats a bus ID as `"{device_name}/CAN{channel + 1}"`
/// (e.g. `Kvaser` channel 0 -> `Kvaser/CAN1`).
pub fn format_bus_id(device_name: &str, channel: i32) -> String {
    format!("{device_name}/CAN{}", channel + 1)
}

/// Builds an [`Error::Driver`] for a configuration failure, folding the
/// hardware ID into the message text.
pub fn config_err(hardware_id: &str, message: impl std::fmt::Display) -> Error {
    Error::Driver(format!("{hardware_id}: {message}"))
}

// ---------------------------------------------------------------------------
// DeviceCore — shared device state (bus ID, statistics, listeners)
// ---------------------------------------------------------------------------

/// Frame listener callback invoked for each dispatched frame.
pub type FrameCallback = Box<dyn FnMut(&CanFrame) + Send + 'static>;

// Callback shared across several ID keys registered together.
type SharedCallback = Arc<Mutex<FrameCallback>>;

/// Typed description of an available hardware channel (minimal common shape
/// across vendor backends).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Channel number (passed to `CanConfiguration::channel`).
    pub channel: i32,
    /// Vendor-specific hardware type passed to
    /// [`CanConfiguration::hardware_type`]. Zero means that the backend does
    /// not require a separate hardware-type selector.
    pub hardware_type: i32,
    /// Human-readable channel name.
    pub name: String,
    /// Whether the channel supports CAN FD.
    pub supports_fd: bool,
}

/// Shared state for a CAN device: a unique bus ID, bus-load statistics,
/// and a listener table keyed by CAN ID.
/// Thread synchronization is the caller's responsibility (typically an outer
/// `Mutex`).
#[derive(Default)]
pub struct DeviceCore {
    unique_bus_id: u32,
    bits_transferred: u64,
    msg_transferred: u64,
    last_busload_query: Option<Instant>,
    cached_bits_per_sec: f64,
    cached_msg_per_sec: i32,
    listeners: HashMap<u32, Vec<(u64, SharedCallback)>>,
    next_listener_token: u64,
}

impl DeviceCore {
    /// Creates a core with a process-wide unique bus ID (the first one is 1).
    pub fn new() -> Self {
        static NEXT_BUS_ID: AtomicU32 = AtomicU32::new(0);
        Self {
            unique_bus_id: NEXT_BUS_ID.fetch_add(1, AtomicOrdering::SeqCst) + 1,
            ..Self::default()
        }
    }

    /// The device's unique bus ID.
    pub fn unique_bus_id(&self) -> u32 {
        self.unique_bus_id
    }

    /// Registers `callback` for the given CAN IDs; `ids = None` or an empty
    /// slice subscribes to all frames (key [`ALL_FRAMES`]).
    /// Returns a token for [`DeviceCore::unregister_listener`]. Multiple IDs
    /// share one callback instance (`Arc<Mutex<_>>`), so a single unregister
    /// removes all of them.
    pub fn register_listener(&mut self, ids: Option<&[u32]>, callback: FrameCallback) -> u64 {
        self.next_listener_token += 1;
        let token = self.next_listener_token;
        let shared: SharedCallback = Arc::new(Mutex::new(callback));
        match ids {
            Some(list) if !list.is_empty() => {
                for &id in list {
                    self.listeners
                        .entry(id)
                        .or_default()
                        .push((token, Arc::clone(&shared)));
                }
            }
            _ => {
                self.listeners
                    .entry(ALL_FRAMES)
                    .or_default()
                    .push((token, shared));
            }
        }
        token
    }

    /// Unregisters a listener by token; returns whether anything was removed.
    pub fn unregister_listener(&mut self, token: u64) -> bool {
        let mut removed = false;
        self.listeners.retain(|_, cbs| {
            let before = cbs.len();
            cbs.retain(|(t, _)| *t != token);
            removed |= cbs.len() != before;
            !cbs.is_empty()
        });
        removed
    }

    /// Number of listener registrations, counted per ID key (a shared callback
    /// registered under several IDs counts once per key).
    pub fn client_count(&self) -> usize {
        self.listeners.values().map(Vec::len).sum()
    }

    /// Dispatches a frame to the listeners registered for its ID and to the
    /// [`ALL_FRAMES`] listeners.
    /// Panics inside a callback are contained with `catch_unwind` so one bad
    /// listener cannot break dispatch (poisoned callback locks are still
    /// recovered).
    pub fn dispatch(&mut self, frame: &CanFrame) {
        let keys = [frame.id, ALL_FRAMES];
        for key in keys {
            if let Some(cbs) = self.listeners.get_mut(&key) {
                for (_, cb) in cbs.iter_mut() {
                    let mut guard = cb.lock().unwrap_or_else(|p| p.into_inner());
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (*guard)(frame)));
                }
            }
        }
    }

    /// Accounts a sent frame in the statistics and returns its data length.
    pub fn record_sent(&mut self, frame: &CanFrame) -> usize {
        self.msg_transferred += 1;
        self.bits_transferred += frame.raw_frame_length() as u64;
        frame.data.len()
    }

    /// Accounts a received frame in the statistics (master frames are not
    /// counted).
    pub fn record_received(&mut self, frame: &CanFrame) {
        if !frame.is_master_frame {
            self.msg_transferred += 1;
            self.bits_transferred += frame.raw_frame_length() as u64;
        }
    }

    /// Returns the bus load as `(bits/s, frames/s)`.
    /// Refreshes at most once per second; queries within the same second
    /// return the last cached values. Before any traffic exists the result is
    /// `(0.0, 0)`, and the first query after traffic starts only opens the
    /// measurement window.
    pub fn busload(&mut self) -> (f64, i32) {
        if self.bits_transferred == 0 {
            return (0.0, 0);
        }
        match self.last_busload_query {
            None => {
                self.last_busload_query = Some(Instant::now());
                (0.0, 0)
            }
            Some(last) => {
                let secs = last.elapsed().as_secs_f64();
                if secs > 1.0 {
                    self.cached_bits_per_sec = self.bits_transferred as f64 / secs;
                    self.cached_msg_per_sec = (self.msg_transferred as f64 / secs).round() as i32;
                    self.bits_transferred = 0;
                    self.msg_transferred = 0;
                    self.last_busload_query = Some(Instant::now());
                }
                (self.cached_bits_per_sec, self.cached_msg_per_sec)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CanDevice trait — hardware abstraction for CAN bus access
// ---------------------------------------------------------------------------

/// Object-safe abstraction over a CAN bus adapter (vendor hardware backends,
/// SocketCAN, test mocks).
/// Implementors embed a [`DeviceCore`] and provide the core IO methods;
/// send/receive semantics:
/// - `send` returns the number of payload bytes actually sent (`Ok(0)` =
///   not opened / not sent);
/// - `receive` is non-blocking: `Ok(Some)` when a frame arrived, `Ok(None)`
///   when none is pending.
/// IO methods are async (kept object-safe via `async_trait`, futures are
/// `Send` by default); pure state access (`core`, listener registration,
/// `busload`, ...) stays synchronous. Synchronous callers can use the
/// `blocking` feature's `blocking::BlockingDevice` facade or the
/// [`start_dispatch`] background thread.
#[async_trait]
pub trait CanDevice {
    /// Shared device state (see the module docs).
    fn core(&self) -> &DeviceCore;
    /// See [`CanDevice::core`].
    fn core_mut(&mut self) -> &mut DeviceCore;

    /// Whether the hardware/driver is available (may open channels as a side
    /// effect; configuration failures are reported via `Err`).
    async fn is_available(&mut self) -> Result<bool>;

    /// Opens the channel and applies `config`.
    async fn open(&mut self, config: CanConfiguration) -> Result<bool>;

    /// Closes the channel. Resources are released here or on `Drop`.
    async fn close(&mut self);

    /// Sends a frame; returns the number of payload bytes actually sent.
    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize>;

    /// Non-blocking receive of one frame (`Ok(None)` when none is pending).
    async fn receive(&mut self) -> Result<Option<CanFrame>>;

    /// Enumerates the available hardware channels.
    /// The default implementation reports `Error::NotSupported`.
    async fn available_channels(&self) -> Result<Vec<ChannelInfo>> {
        Err(Error::NotSupported(
            "hardware enumeration not implemented for this device".to_string(),
        ))
    }

    /// The device's unique bus ID.
    fn unique_bus_id(&self) -> u32 {
        self.core().unique_bus_id()
    }

    /// Number of active listener registrations.
    fn client_count(&self) -> usize {
        self.core().client_count()
    }

    /// Registers `callback` for `ids`; `ids = None` listens to all frames
    /// ([`ALL_FRAMES`]).
    /// Returns a token used for unregistration. Multiple IDs share one
    /// callback; a single unregister removes all of them.
    fn register_listener(&mut self, ids: Option<&[u32]>, callback: FrameCallback) -> u64 {
        self.core_mut().register_listener(ids, callback)
    }

    /// Unregisters a listener by its token.
    fn unregister_listener(&mut self, token: u64) -> bool {
        self.core_mut().unregister_listener(token)
    }

    /// Current bus load as `(bits/s, frames/s)`.
    fn busload(&mut self) -> (f64, i32) {
        self.core_mut().busload()
    }

    /// Sends `data` as a CAN 2.0B frame when `send_as_can20` is set, otherwise
    /// as CAN FD with BRS.
    async fn send_simple(
        &mut self,
        can_id: u32,
        data: &[u8],
        send_as_can20: bool,
    ) -> Result<usize> {
        self.send(
            can_id,
            data,
            if send_as_can20 {
                FrameType::CAN20B
            } else {
                FrameType::FD_BRS
            },
        )
        .await
    }

    /// Sends with length padding: the ID is masked with
    /// [`CAN_ID_LIBRARY_VALID_MASK`], short payloads are padded with
    /// `fill_byte` up to the target DLC, and the result is `Ok(0)` when the
    /// actual sent length differs from the buffer length, otherwise
    /// `data.len()`.
    async fn send_msg(
        &mut self,
        can_id: u32,
        data: &[u8],
        send_as_can20: bool,
        max_dlc: usize,
        fill_byte: u8,
    ) -> Result<usize> {
        let target_len = send_msg_len(data.len(), max_dlc);
        let buf: Cow<'_, [u8]> = if target_len > data.len() {
            let mut padded = vec![fill_byte; target_len];
            padded[..data.len()].copy_from_slice(data);
            Cow::Owned(padded)
        } else {
            Cow::Borrowed(data)
        };
        let frame_type = if send_as_can20 {
            FrameType::CAN20B
        } else {
            FrameType::FD_BRS
        };
        let sent = self
            .send(can_id & CAN_ID_LIBRARY_VALID_MASK, &buf, frame_type)
            .await?;
        Ok(if sent != buf.len() { 0 } else { data.len() })
    }

    /// One iteration of the receive loop: fetch one frame, update statistics,
    /// dispatch to listeners. Returns whether a frame was fetched.
    async fn poll_once(&mut self) -> Result<bool> {
        match self.receive().await? {
            Some(frame) => {
                self.core_mut().record_received(&frame);
                self.core_mut().dispatch(&frame);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Reads one bus-error frame description. Backends without error-frame
    /// support (e.g. those that simply drop error frames) keep the default,
    /// which reports no error frame.
    async fn poll_error(&mut self) -> Result<Option<String>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Background dispatch thread
// ---------------------------------------------------------------------------

/// Handle to a background thread looping [`CanDevice::poll_once`], sleeping
/// 1 ms when idle.
/// A driver error ends the thread; stopping happens on drop or via an
/// explicit [`DispatchHandle::stop`].
#[cfg(feature = "blocking")]
pub struct DispatchHandle {
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[cfg(feature = "blocking")]
impl DispatchHandle {
    /// Stops the dispatch thread and waits for it to exit.
    pub fn stop(mut self) {
        self.cancel.store(true, AtomicOrdering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(feature = "blocking")]
impl Drop for DispatchHandle {
    fn drop(&mut self) {
        self.cancel.store(true, AtomicOrdering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts the background dispatch thread for `device`.
#[cfg(feature = "blocking")]
pub fn start_dispatch<D>(device: Arc<Mutex<D>>) -> DispatchHandle
where
    D: CanDevice + Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    let join = thread::spawn(move || {
        while !thread_cancel.load(AtomicOrdering::SeqCst) {
            let got_frame = {
                let mut guard = match device.lock() {
                    Ok(guard) => guard,
                    Err(_) => break, // Poisoned lock: end the thread.
                };
                match autors_runtime::block_on(guard.poll_once()) {
                    Ok(got) => got,
                    Err(_) => break, // Driver error: end the thread.
                }
            };
            if !got_frame {
                thread::sleep(Duration::from_millis(1));
            }
        }
    });
    DispatchHandle {
        cancel,
        join: Some(join),
    }
}

/// Async counterpart of `start_dispatch`: poll until `cancel` is set.
/// Same iteration semantics as the thread version (poll once, sleep 1 ms when
/// idle); the first driver error ends the loop and is returned to the caller.
/// Spawn this on the caller's own async runtime.
pub async fn dispatch_loop<D: CanDevice + ?Sized + Send>(
    device: &mut D,
    cancel: &AtomicBool,
) -> Result<()> {
    while !cancel.load(AtomicOrdering::SeqCst) {
        if !device.poll_once().await? {
            autors_runtime::sleep(Duration::from_millis(1)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{CanBaudrate, CanFdBaudrate};
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingDevice;

    /// Compile-time object-safety assertion.
    fn _assert_object_safe(_: &dyn CanDevice) {}

    struct MockDevice {
        core: DeviceCore,
        opened: bool,
        sent: Vec<(u32, Vec<u8>, FrameType)>,
        rx: VecDeque<CanFrame>,
    }

    impl MockDevice {
        fn new() -> Self {
            Self {
                core: DeviceCore::new(),
                opened: false,
                sent: Vec::new(),
                rx: VecDeque::new(),
            }
        }
    }

    #[async_trait]
    impl CanDevice for MockDevice {
        fn core(&self) -> &DeviceCore {
            &self.core
        }
        fn core_mut(&mut self) -> &mut DeviceCore {
            &mut self.core
        }
        async fn is_available(&mut self) -> Result<bool> {
            Ok(true)
        }
        async fn open(&mut self, _config: CanConfiguration) -> Result<bool> {
            self.opened = true;
            Ok(true)
        }
        async fn close(&mut self) {
            self.opened = false;
        }
        async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
            if !self.opened {
                return Ok(0); // Not opened: report zero bytes sent.
            }
            self.sent.push((can_id, data.to_vec(), frame_type));
            let frame = CanFrame::new("Mock/CAN1", can_id, data.to_vec(), true, frame_type);
            Ok(self.core.record_sent(&frame))
        }
        async fn receive(&mut self) -> Result<Option<CanFrame>> {
            Ok(self.rx.pop_front())
        }
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn object_safety_and_boxing() {
        let mut boxed: Box<dyn CanDevice> = Box::new(MockDevice::new());
        _assert_object_safe(&*boxed);
        assert!(autors_runtime::block_on(boxed.is_available()).unwrap());
        assert!(autors_runtime::block_on(boxed.open(CanConfiguration::new(
            0,
            CanBaudrate::B500Kbit,
            CanFdBaudrate::NotUsed
        )))
        .unwrap());
        assert_eq!(
            autors_runtime::block_on(boxed.send(0x123, &[1, 2, 3], FrameType::CAN20B)).unwrap(),
            3
        );
        assert!(autors_runtime::block_on(boxed.receive()).unwrap().is_none());
        autors_runtime::block_on(boxed.close());

        // Arc<Mutex<dyn>> is another common object-safe usage.
        let shared: Arc<Mutex<dyn CanDevice + Send>> = Arc::new(Mutex::new(MockDevice::new()));
        assert!(autors_runtime::block_on(shared.lock().unwrap().is_available()).unwrap());
    }

    #[test]
    fn unique_bus_id_increments() {
        let a = DeviceCore::new().unique_bus_id();
        let b = DeviceCore::new().unique_bus_id();
        assert_eq!(b, a + 1);
        assert!(a >= 1);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn send_simple_selects_frame_type() {
        let mut p = BlockingDevice::new(MockDevice::new());
        p.open(CanConfiguration::default()).unwrap();
        p.send_simple(0x123, &[1], true).unwrap();
        p.send_simple(0x123, &[1], false).unwrap();
        assert_eq!(p.0.sent[0].2, FrameType::CAN20B);
        assert_eq!(p.0.sent[1].2, FrameType::FD_BRS);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn send_msg_follows_expected_logic() {
        let mut p = BlockingDevice::new(MockDevice::new());
        p.open(CanConfiguration::default()).unwrap();

        // maxDLC = 8: pad to 8 bytes (CCP usage), return the original data length.
        assert_eq!(
            p.send_msg(0x8000_0123, &[1, 2, 3], true, 8, 0xAA).unwrap(),
            3
        );
        let (id, data, ftype) = &p.0.sent[0];
        assert_eq!(*id, 0x123 | CAN_EXT_FLAG); // & CAN_ID_LIBRARY_VALID_MASK preserves the extended flag.
        assert_eq!(data, &vec![1, 2, 3, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);
        assert_eq!(*ftype, FrameType::CAN20B);

        // maxDLC = 0, 9 data bytes: round up to FD tier 12, sent as FD_BRS.
        assert_eq!(p.send_msg(0x123, &[0u8; 9], false, 0, 0).unwrap(), 9);
        assert_eq!(p.0.sent[1].1.len(), 12);
        assert_eq!(p.0.sent[1].2, FrameType::FD_BRS);

        // 0x40000000 (CAN_FD_XCP15_FLAG) is masked off.
        p.send_msg(0xC000_0123, &[1], true, 0, 0).unwrap();
        assert_eq!(p.0.sent[2].0, 0x8000_0123);

        // Send failure (returns 0) -> send_msg returns 0.
        p.close();
        assert_eq!(p.send_msg(0x123, &[1, 2], true, 0, 0).unwrap(), 0);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn listener_dispatch_and_unregister() {
        let hits: Arc<StdMutex<Vec<u32>>> = Arc::new(StdMutex::new(Vec::new()));
        let all_hits: Arc<StdMutex<Vec<u32>>> = Arc::new(StdMutex::new(Vec::new()));
        let mut p = BlockingDevice::new(MockDevice::new());

        let h = Arc::clone(&hits);
        let tok = p.register_listener(
            Some(&[0x123]),
            Box::new(move |f: &CanFrame| h.lock().unwrap().push(f.id)),
        );
        let h2 = Arc::clone(&all_hits);
        let tok_all = p.register_listener(
            None,
            Box::new(move |f: &CanFrame| h2.lock().unwrap().push(f.id)),
        );
        assert_eq!(p.client_count(), 2);

        p.0.rx
            .push_back(CanFrame::new("B", 0x123, vec![1], false, FrameType::CAN20B));
        p.0.rx
            .push_back(CanFrame::new("B", 0x456, vec![1], false, FrameType::CAN20B));
        assert!(p.poll_once().unwrap());
        assert!(p.poll_once().unwrap());
        assert!(!p.poll_once().unwrap());
        assert_eq!(*hits.lock().unwrap(), vec![0x123]);
        assert_eq!(*all_hits.lock().unwrap(), vec![0x123, 0x456]);

        // No more hits after unregistering.
        assert!(p.unregister_listener(tok));
        assert!(!p.unregister_listener(tok));
        p.0.rx
            .push_back(CanFrame::new("B", 0x123, vec![1], false, FrameType::CAN20B));
        p.poll_once().unwrap();
        assert_eq!(hits.lock().unwrap().len(), 1);
        assert_eq!(all_hits.lock().unwrap().len(), 3);
        assert!(p.unregister_listener(tok_all));
        assert_eq!(p.client_count(), 0);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn panic_in_listener_is_swallowed() {
        // Panics in listener callbacks must be contained.
        let mut p = BlockingDevice::new(MockDevice::new());
        p.register_listener(None, Box::new(|_| panic!("boom")));
        p.0.rx
            .push_back(CanFrame::new("B", 0x1, vec![1], false, FrameType::CAN20B));
        assert!(p.poll_once().unwrap());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn busload_statistics() {
        let mut p = BlockingDevice::new(MockDevice::new());
        // No traffic -> (0, 0).
        assert_eq!(p.busload(), (0.0, 0));
        p.open(CanConfiguration::default()).unwrap();
        p.send(0x123, &[0u8; 8], FrameType::CAN20B).unwrap();
        // First query: opens the measurement window, returns (0, 0).
        assert_eq!(p.busload(), (0.0, 0));
        // Query within the same second: cached values.
        assert_eq!(p.busload(), (0.0, 0));
        assert_eq!(p.0.core.msg_transferred, 1);
        assert_eq!(p.0.core.bits_transferred, 108); // 44 + 8*8
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn dispatch_thread_delivers_frames() {
        let hits: Arc<StdMutex<Vec<u32>>> = Arc::new(StdMutex::new(Vec::new()));
        let p = Arc::new(Mutex::new(MockDevice::new()));
        let h = Arc::clone(&hits);
        p.lock().unwrap().register_listener(
            None,
            Box::new(move |f: &CanFrame| h.lock().unwrap().push(f.id)),
        );
        p.lock().unwrap().rx.push_back(CanFrame::new(
            "B",
            0x123,
            vec![1],
            false,
            FrameType::CAN20B,
        ));
        let handle = start_dispatch(Arc::clone(&p));
        for _ in 0..100 {
            if !hits.lock().unwrap().is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        handle.stop();
        assert_eq!(*hits.lock().unwrap(), vec![0x123]);
    }

    #[test]
    fn static_constants_match_expected_contract() {
        assert_eq!(CAN_STD_ID_MASK, 2047);
        assert_eq!(CAN_EXT_ID_MASK, 536_870_911);
        assert_eq!(CAN_ID_LIBRARY_VALID_MASK, 2_684_354_559);
        assert_eq!(CAN_EXT_FLAG, 2_147_483_648);
        assert_eq!(CAN_FD_XCP15_FLAG, 1_073_741_824);
        assert_eq!(CAN_ID_XCP15_VALID_MASK, 3_758_096_383);
        assert_eq!(MAX_DLC, 8);
        assert_eq!(MAX_FD_DLC, 64);
    }

    #[test]
    fn id_string_helpers_match_expected_contract() {
        // Behavioral contract values.
        assert_eq!(to_can_id_string(0x123), "123");
        assert_eq!(to_can_id_string(0x8000_0123), "123(X)");
        assert_eq!(to_can_id_string(0x8000_0000), "0(X)");
        assert_eq!(to_can_id_string(0x1FFF_FFFF), "1FFFFFFF");
        assert_eq!(to_can_id_pair_string(0x123, 0x456), "123/456");
        assert_eq!(to_can_id_pair_string(0x8000_0123, 0x456), "123(X)/456");
        assert_eq!(format_bus_id("Kvaser", 0), "Kvaser/CAN1");
    }

    #[test]
    fn can_id_sort_matches_expected_contract() {
        assert_eq!(can_id_sort(0x123, 0x8000_0123), Ordering::Less); // Standard sorts before extended.
        assert_eq!(can_id_sort(0x8000_0123, 0x123), Ordering::Greater); // And the reverse sorts after.
        assert_eq!(can_id_sort(0x123, 0x124), Ordering::Less);
        assert_eq!(can_id_sort(0x123, 0x123), Ordering::Equal);
        assert_eq!(can_id_sort(0x8000_0123, 0x8000_0123), Ordering::Equal);
    }

    #[test]
    fn dlc_mapping_matches_expected_contract() {
        // Covers the complete length <-> DLC tier table.
        for len in 0..=8usize {
            assert_eq!(length_to_dlc(len).unwrap(), len as u8);
        }
        assert_eq!(length_to_dlc(9).unwrap(), 9);
        assert_eq!(length_to_dlc(12).unwrap(), 9);
        assert_eq!(length_to_dlc(16).unwrap(), 10);
        assert_eq!(length_to_dlc(20).unwrap(), 11);
        assert_eq!(length_to_dlc(24).unwrap(), 12);
        assert_eq!(length_to_dlc(32).unwrap(), 13);
        assert_eq!(length_to_dlc(48).unwrap(), 14);
        assert_eq!(length_to_dlc(64).unwrap(), 15);
        let err = length_to_dlc(65).unwrap_err().to_string();
        assert_eq!(
            err,
            "invalid frame or parameter: Length 65 is not allowed in a CAN frame."
        );

        for dlc in 0..=8u8 {
            assert_eq!(dlc_to_length(dlc).unwrap(), dlc);
        }
        assert_eq!(dlc_to_length(9).unwrap(), 12);
        assert_eq!(dlc_to_length(10).unwrap(), 16);
        assert_eq!(dlc_to_length(11).unwrap(), 20);
        assert_eq!(dlc_to_length(12).unwrap(), 24);
        assert_eq!(dlc_to_length(13).unwrap(), 32);
        assert_eq!(dlc_to_length(14).unwrap(), 48);
        assert_eq!(dlc_to_length(15).unwrap(), 64);
        let err = dlc_to_length(16).unwrap_err().to_string();
        assert_eq!(
            err,
            "invalid frame or parameter: Invalid DLC 16 for a CAN Frame"
        );
    }

    #[test]
    fn send_msg_len_matches_expected_contract() {
        assert_eq!(send_msg_len(5, 8), 8); // maxDLC takes precedence.
        assert_eq!(send_msg_len(5, 0), 5);
        assert_eq!(send_msg_len(9, 0), 12); // 9 rounds up to the 12 tier.
        assert_eq!(send_msg_len(13, 0), 16);
        assert_eq!(send_msg_len(17, 0), 20);
        assert_eq!(send_msg_len(21, 0), 24);
        assert_eq!(send_msg_len(25, 0), 32);
        assert_eq!(send_msg_len(33, 0), 48);
        assert_eq!(send_msg_len(49, 0), 64);
    }

    #[test]
    fn ni_id_conversion_matches_expected_contract() {
        // Extended-flag conversion values.
        assert_eq!(to_ni_can_id(0x8000_0123), 0x2000_0123);
        assert_eq!(from_ni_can_id(0x2000_0123), 0x8000_0123);
        assert_eq!(to_ni_can_id(0x123), 0x123);
        assert_eq!(from_ni_can_id(0x123), 0x123);
        // round-trip
        assert_eq!(from_ni_can_id(to_ni_can_id(0x9FFF_FFFF)), 0x9FFF_FFFF);
    }

    #[test]
    fn id_validity_matches_expected_contract() {
        // Boundary values.
        assert!(is_can_id_valid(0x7FF));
        assert!(!is_can_id_valid(0x800));
        assert!(is_can_id_valid(0x9FFF_FFFF));
        assert!(!is_can_id_valid(0xA000_0000));
        assert!(is_can_id_valid(0));
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn available_channels_default_not_supported() {
        let p = BlockingDevice::new(MockDevice::new());
        match p.available_channels() {
            Err(Error::NotSupported(_)) => {}
            other => panic!("expected NotSupported, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_send_receive_poll_once_loopback() {
        let mut p = MockDevice::new();
        p.open(CanConfiguration::default()).await.unwrap();
        assert_eq!(
            p.send(0x123, &[1, 2, 3], FrameType::CAN20B).await.unwrap(),
            3
        );
        assert_eq!(p.sent.len(), 1);

        // Async receive / poll_once loopback.
        p.rx.push_back(CanFrame::new("B", 0x123, vec![1], false, FrameType::CAN20B));
        let frame = p.receive().await.unwrap().expect("expected a frame");
        assert_eq!(frame.id, 0x123);
        assert!(p.receive().await.unwrap().is_none());

        p.rx.push_back(CanFrame::new("B", 0x456, vec![2], false, FrameType::CAN20B));
        assert!(p.poll_once().await.unwrap());
        assert!(!p.poll_once().await.unwrap());

        p.close().await;
        assert!(!p.opened);
        // send returns 0 when the channel is not opened.
        assert_eq!(p.send(0x123, &[1], FrameType::CAN20B).await.unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_loop_delivers_frames_to_listeners() {
        let hits: Arc<StdMutex<Vec<u32>>> = Arc::new(StdMutex::new(Vec::new()));
        let mut p = MockDevice::new();
        let h = Arc::clone(&hits);
        p.register_listener(
            None,
            Box::new(move |f: &CanFrame| h.lock().unwrap().push(f.id)),
        );
        p.rx.push_back(CanFrame::new("B", 0x123, vec![1], false, FrameType::CAN20B));

        let cancel = Arc::new(AtomicBool::new(false));
        let loop_cancel = Arc::clone(&cancel);
        let driver = tokio::spawn(async move {
            dispatch_loop(&mut p, &loop_cancel)
                .await
                .expect("dispatch loop failed");
        });
        for _ in 0..100 {
            if !hits.lock().unwrap().is_empty() {
                break;
            }
            autors_runtime::sleep(Duration::from_millis(5)).await;
        }
        cancel.store(true, AtomicOrdering::SeqCst);
        driver.await.expect("dispatch task panicked");
        assert_eq!(*hits.lock().unwrap(), vec![0x123]);
    }
}
