//! Synchronous facades over the async cooperative schedulers.

use std::time::Instant;

use autors_can::device::CanDevice;
use autors_lin::device::LinDevice;

use crate::can::CanTransmission;
use crate::lin::LinTransmission;
use crate::{CanScheduler, LinScheduler, Result};

/// Synchronous wrapper around [`CanScheduler`].
pub struct BlockingCanScheduler(pub CanScheduler);

impl BlockingCanScheduler {
    /// Wraps an existing scheduler without changing its runtime state.
    pub const fn new(scheduler: CanScheduler) -> Self {
        Self(scheduler)
    }

    /// Advances the scheduler using the current monotonic time.
    pub fn poll<D>(&mut self, device: &mut D) -> Result<Vec<CanTransmission>>
    where
        D: CanDevice + Send + ?Sized,
    {
        autors_runtime::block_on(self.0.poll(device))
    }

    /// Deterministic poll with an explicit monotonic time.
    pub fn poll_at<D>(&mut self, device: &mut D, now: Instant) -> Result<Vec<CanTransmission>>
    where
        D: CanDevice + Send + ?Sized,
    {
        autors_runtime::block_on(self.0.poll_at(device, now))
    }
}

/// Synchronous wrapper around [`LinScheduler`].
pub struct BlockingLinScheduler(pub LinScheduler);

impl BlockingLinScheduler {
    /// Wraps an existing scheduler without changing its runtime state.
    pub const fn new(scheduler: LinScheduler) -> Self {
        Self(scheduler)
    }

    /// Advances the scheduler using the current monotonic time.
    pub fn poll<D>(&mut self, device: &mut D) -> Result<Vec<LinTransmission>>
    where
        D: LinDevice + Send + ?Sized,
    {
        autors_runtime::block_on(self.0.poll(device))
    }

    /// Deterministic poll with an explicit monotonic time.
    pub fn poll_at<D>(&mut self, device: &mut D, now: Instant) -> Result<Vec<LinTransmission>>
    where
        D: LinDevice + Send + ?Sized,
    {
        autors_runtime::block_on(self.0.poll_at(device, now))
    }
}
