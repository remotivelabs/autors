use async_trait::async_trait;

use crate::device::{CanDevice, DeviceCore};
use crate::error::{Error, Result};
use crate::frame::{CanConfiguration, CanFrame, FrameType};

pub struct StubDevice {
    core: DeviceCore,
}

impl StubDevice {
    pub fn new() -> Self {
        Self {
            core: DeviceCore::new(),
        }
    }

    fn not_supported() -> Error {
        Error::NotSupported("CAN hardware access is not supported on this platform".to_string())
    }
}

impl Default for StubDevice {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CanDevice for StubDevice {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> Result<bool> {
        Ok(false)
    }

    async fn open(&mut self, _config: CanConfiguration) -> Result<bool> {
        Err(Self::not_supported())
    }

    async fn close(&mut self) {}

    async fn send(&mut self, _can_id: u32, _data: &[u8], _frame_type: FrameType) -> Result<usize> {
        Err(Self::not_supported())
    }

    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        Err(Self::not_supported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn CanDevice) {}

    #[test]
    fn construction_and_object_safety() {
        let p = StubDevice::new();
        _assert_object_safe(&p);
        assert!(p.unique_bus_id() >= 1);
        let boxed: Box<dyn CanDevice> = Box::new(StubDevice::default());
        drop(boxed);
    }

    #[test]
    fn runtime_reports_not_supported() {
        let mut p = StubDevice::new();
        assert!(!autors_runtime::block_on(p.is_available()).unwrap());
        match autors_runtime::block_on(p.open(CanConfiguration::default())) {
            Err(Error::NotSupported(_)) => {}
            other => panic!("expected NotSupported, got {other:?}"),
        }
        match autors_runtime::block_on(p.send(0x123, &[1], FrameType::CAN20B)) {
            Err(Error::NotSupported(_)) => {}
            other => panic!("expected NotSupported, got {other:?}"),
        }
        match autors_runtime::block_on(p.receive()) {
            Err(Error::NotSupported(_)) => {}
            other => panic!("expected NotSupported, got {other:?}"),
        }
        autors_runtime::block_on(p.close());
        assert!(autors_runtime::block_on(p.poll_error()).unwrap().is_none());
        assert_eq!(p.client_count(), 0);
    }
}
