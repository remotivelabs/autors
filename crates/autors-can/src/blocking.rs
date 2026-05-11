//! Synchronous facade over the async [`CanDevice`] core.
//! [`BlockingDevice`] wraps any [`CanDevice`] and exposes the old-style
//! synchronous API: every async trait method is driven to completion on the
//! calling thread via [`autors_runtime::block_on`], while the pure state
//! accessors (listener registration, bus load, bus id) delegate directly.
//! Do not call these methods from within async code running on the shared
//! runtime: [`autors_runtime::block_on`] panics on its executor threads, the
//! same restriction as `tokio::runtime::Runtime::block_on`.

use crate::device::{CanDevice, ChannelInfo, FrameCallback};
use crate::error::Result;
use crate::frame::{CanConfiguration, CanFrame, FrameType};

/// Synchronous wrapper around a [`CanDevice`].
/// The wrapped device is publicly accessible (`.0`), so inherent methods of
/// the concrete backend remain reachable.
pub struct BlockingDevice<D>(pub D);

impl<D> BlockingDevice<D> {
    /// Wraps `device` in the synchronous facade.
    pub fn new(device: D) -> Self {
        Self(device)
    }

    /// Unwraps the facade, returning the inner device.
    pub fn into_inner(self) -> D {
        self.0
    }
}

impl<D: CanDevice + Send> BlockingDevice<D> {
    /// Drives [`CanDevice::is_available`] to completion on the calling thread.
    pub fn is_available(&mut self) -> Result<bool> {
        autors_runtime::block_on(self.0.is_available())
    }

    /// Drives [`CanDevice::open`] to completion on the calling thread.
    pub fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        autors_runtime::block_on(self.0.open(config))
    }

    /// Drives [`CanDevice::close`] to completion on the calling thread.
    pub fn close(&mut self) {
        autors_runtime::block_on(self.0.close())
    }

    /// Drives [`CanDevice::send`] to completion on the calling thread.
    pub fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        autors_runtime::block_on(self.0.send(can_id, data, frame_type))
    }

    /// Drives [`CanDevice::receive`] to completion on the calling thread.
    pub fn receive(&mut self) -> Result<Option<CanFrame>> {
        autors_runtime::block_on(self.0.receive())
    }

    /// Drives [`CanDevice::send_simple`] to completion on the calling thread.
    pub fn send_simple(&mut self, can_id: u32, data: &[u8], send_as_can20: bool) -> Result<usize> {
        autors_runtime::block_on(self.0.send_simple(can_id, data, send_as_can20))
    }

    /// Drives [`CanDevice::send_msg`] to completion on the calling thread.
    pub fn send_msg(
        &mut self,
        can_id: u32,
        data: &[u8],
        send_as_can20: bool,
        max_dlc: usize,
        fill_byte: u8,
    ) -> Result<usize> {
        autors_runtime::block_on(
            self.0
                .send_msg(can_id, data, send_as_can20, max_dlc, fill_byte),
        )
    }

    /// Drives [`CanDevice::poll_once`] to completion on the calling thread.
    pub fn poll_once(&mut self) -> Result<bool> {
        autors_runtime::block_on(self.0.poll_once())
    }

    /// Drives [`CanDevice::poll_error`] to completion on the calling thread.
    pub fn poll_error(&mut self) -> Result<Option<String>> {
        autors_runtime::block_on(self.0.poll_error())
    }
}

impl<D: CanDevice + Sync> BlockingDevice<D> {
    /// Drives [`CanDevice::available_channels`] to completion on the calling thread.
    pub fn available_channels(&self) -> Result<Vec<ChannelInfo>> {
        autors_runtime::block_on(self.0.available_channels())
    }
}

impl<D: CanDevice> BlockingDevice<D> {
    /// Delegates to [`CanDevice::register_listener`] (inherently synchronous).
    pub fn register_listener(&mut self, ids: Option<&[u32]>, callback: FrameCallback) -> u64 {
        self.0.register_listener(ids, callback)
    }

    /// Delegates to [`CanDevice::unregister_listener`] (inherently synchronous).
    pub fn unregister_listener(&mut self, token: u64) -> bool {
        self.0.unregister_listener(token)
    }

    /// Delegates to [`CanDevice::busload`] (inherently synchronous).
    pub fn busload(&mut self) -> (f64, i32) {
        self.0.busload()
    }

    /// Delegates to [`CanDevice::client_count`] (inherently synchronous).
    pub fn client_count(&self) -> usize {
        self.0.client_count()
    }

    /// Delegates to [`CanDevice::unique_bus_id`] (inherently synchronous).
    pub fn unique_bus_id(&self) -> u32 {
        self.0.unique_bus_id()
    }
}
