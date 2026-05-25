//! Synchronous facade over the async [`LinDevice`] core.
//! [`BlockingDevice`] wraps any [`LinDevice`] and exposes the old-style
//! synchronous API: every async trait method is driven to completion on the
//! calling thread via [`autors_runtime::block_on`], while the pure state
//! accessors (availability, bus id, send logging) delegate directly.
//! Do not call these methods from within async code running on the shared
//! runtime: [`autors_runtime::block_on`] panics on its executor threads, the
//! same restriction as `tokio::runtime::Runtime::block_on`.

use crate::device::{LinConfiguration, LinDevice, LinFrame};
use crate::error::Result;

/// Synchronous wrapper around a [`LinDevice`].
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

impl<D: LinDevice + Send> BlockingDevice<D> {
    /// Drives [`LinDevice::open`] to completion on the calling thread.
    pub fn open(&mut self, config: &LinConfiguration) -> Result<bool> {
        autors_runtime::block_on(self.0.open(config))
    }

    /// Drives [`LinDevice::send`] to completion on the calling thread.
    pub fn send(&mut self, id: u8, data: &[u8]) -> Result<usize> {
        autors_runtime::block_on(self.0.send(id, data))
    }

    /// Drives [`LinDevice::request`] to completion on the calling thread.
    pub fn request(&mut self, id: u8) -> Result<bool> {
        autors_runtime::block_on(self.0.request(id))
    }

    /// Drives [`LinDevice::on_receive`] to completion on the calling thread.
    pub fn on_receive(&mut self) -> Result<Option<LinFrame>> {
        autors_runtime::block_on(self.0.on_receive())
    }

    /// Drives [`LinDevice::close`] to completion on the calling thread.
    pub fn close(&mut self) {
        autors_runtime::block_on(self.0.close())
    }
}

impl<D: LinDevice> BlockingDevice<D> {
    /// Delegates to [`LinDevice::unique_bus_id`] (inherently synchronous).
    pub fn unique_bus_id(&self) -> i32 {
        self.0.unique_bus_id()
    }

    /// Delegates to [`LinDevice::is_available`] (inherently synchronous).
    pub fn is_available(&self) -> bool {
        self.0.is_available()
    }

    /// Delegates to [`LinDevice::has_sent`] (inherently synchronous).
    pub fn has_sent(&self, frame: LinFrame) -> usize {
        self.0.has_sent(frame)
    }
}
