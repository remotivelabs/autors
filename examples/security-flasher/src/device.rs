use std::collections::VecDeque;

use async_trait::async_trait;
use autors_can::blocking::BlockingDevice;
use autors_can::device::{CanDevice, ChannelInfo, DeviceCore, FrameCallback, CAN_EXT_FLAG};
use autors_can::frame::{CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType};
use autors_isotp::isotp::{IsoTpFsm, MsgState as TpState};

use crate::config::CanSection;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Demo,
    Peak,
    Vector,
    Tosun,
}

impl AdapterKind {
    pub const LABELS: [&'static str; 4] = ["Virtual ECU", "PEAK", "Vector", "TOSUN"];

    pub fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Demo),
            1 => Some(Self::Peak),
            2 => Some(Self::Vector),
            3 => Some(Self::Tosun),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceChoice {
    pub adapter: AdapterKind,
    pub channel: i32,
    pub hardware_type: i32,
    pub serial: Option<String>,
    pub supports_fd: bool,
    pub label: String,
}

pub struct DynCanDevice {
    inner: Box<dyn CanDevice + Send + Sync>,
}

impl DynCanDevice {
    fn new(device: impl CanDevice + Send + Sync + 'static) -> Self {
        Self {
            inner: Box::new(device),
        }
    }
}

#[async_trait]
impl CanDevice for DynCanDevice {
    fn core(&self) -> &DeviceCore {
        self.inner.core()
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        self.inner.core_mut()
    }

    async fn is_available(&mut self) -> autors_can::Result<bool> {
        self.inner.is_available().await
    }

    async fn open(&mut self, config: CanConfiguration) -> autors_can::Result<bool> {
        self.inner.open(config).await
    }

    async fn close(&mut self) {
        self.inner.close().await;
    }

    async fn send(
        &mut self,
        can_id: u32,
        data: &[u8],
        frame_type: FrameType,
    ) -> autors_can::Result<usize> {
        self.inner.send(can_id, data, frame_type).await
    }

    async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
        self.inner.receive().await
    }

    async fn available_channels(&self) -> autors_can::Result<Vec<ChannelInfo>> {
        self.inner.available_channels().await
    }

    fn register_listener(&mut self, ids: Option<&[u32]>, callback: FrameCallback) -> u64 {
        self.inner.register_listener(ids, callback)
    }

    fn unregister_listener(&mut self, token: u64) -> bool {
        self.inner.unregister_listener(token)
    }
}

pub fn scan_devices(adapter: AdapterKind) -> Result<Vec<DeviceChoice>> {
    match adapter {
        AdapterKind::Demo => Ok(vec![DeviceChoice {
            adapter,
            channel: 0,
            hardware_type: 0,
            serial: None,
            supports_fd: true,
            label: "Virtual ECU / CAN 1".to_string(),
        }]),
        AdapterKind::Peak => scan_peak(),
        AdapterKind::Vector => scan_vector(),
        AdapterKind::Tosun => scan_tosun(),
    }
}

pub fn open_device(
    choice: &DeviceChoice,
    can: &CanSection,
    response_id: u32,
) -> Result<DynCanDevice> {
    if can.data_baud_rate != 0 && !choice.supports_fd {
        return Err(Error::Can(format!(
            "{} does not report CAN FD support",
            choice.label
        )));
    }
    let mut device = create_device(choice, response_id)?;
    let nominal = CanBaudrate::from_u32(can.baud_rate)
        .ok_or_else(|| Error::Can("unsupported nominal bitrate".to_string()))?;
    let data = CanFdBaudrate::from_u32(can.data_baud_rate)
        .ok_or_else(|| Error::Can("unsupported data bitrate".to_string()))?;
    let mut configuration = CanConfiguration::new(choice.channel, nominal, data);
    configuration.hardware_type = choice.hardware_type;
    configuration.bus_id = Some(choice.label.clone());
    let mut blocking = BlockingDevice::new(device);
    let opened = blocking
        .open(configuration)
        .map_err(|error| Error::Can(error.to_string()))?;
    device = blocking.into_inner();
    if !opened {
        return Err(Error::Can(format!("failed to open {}", choice.label)));
    }
    Ok(device)
}

pub fn close_device(device: DynCanDevice) {
    let mut blocking = BlockingDevice::new(device);
    blocking.close();
}

pub fn wire_id(id: u32) -> u32 {
    if id > 0x7FF {
        id | CAN_EXT_FLAG
    } else {
        id
    }
}

fn create_device(choice: &DeviceChoice, response_id: u32) -> Result<DynCanDevice> {
    match choice.adapter {
        AdapterKind::Demo => Ok(DynCanDevice::new(DemoEcu::new(wire_id(response_id)))),
        #[cfg(windows)]
        AdapterKind::Peak => autors_can::vendors::peak::PeakCan::new()
            .map(DynCanDevice::new)
            .map_err(|error| Error::Can(error.to_string())),
        #[cfg(windows)]
        AdapterKind::Vector => autors_can::vendors::vector::VectorCan::new()
            .map(DynCanDevice::new)
            .map_err(|error| Error::Can(error.to_string())),
        #[cfg(windows)]
        AdapterKind::Tosun => {
            let mut device = autors_can::vendors::tosun::TosunCan::new()
                .map_err(|error| Error::Can(error.to_string()))?;
            device.device_serial = choice.serial.clone();
            Ok(DynCanDevice::new(device))
        }
        #[cfg(not(windows))]
        _ => Err(Error::Can(
            "the selected vendor adapter is available on Windows only".to_string(),
        )),
    }
}

#[cfg(windows)]
fn scan_peak() -> Result<Vec<DeviceChoice>> {
    use autors_can::vendors::peak::{PeakCan, PeakHwType};

    PeakCan::new().map_err(|error| Error::Can(error.to_string()))?;
    Ok((0..8)
        .map(|channel| DeviceChoice {
            adapter: AdapterKind::Peak,
            channel,
            hardware_type: PeakHwType::UsbBus as i32,
            serial: None,
            supports_fd: true,
            label: format!("PEAK USB / CAN {}", channel + 1),
        })
        .collect())
}

#[cfg(not(windows))]
fn scan_peak() -> Result<Vec<DeviceChoice>> {
    Err(Error::Can(
        "PEAK adapters are available on Windows only".to_string(),
    ))
}

#[cfg(windows)]
fn scan_vector() -> Result<Vec<DeviceChoice>> {
    let device = autors_can::vendors::vector::VectorCan::new()
        .map_err(|error| Error::Can(error.to_string()))?;
    let channels = BlockingDevice::new(device)
        .available_channels()
        .map_err(|error| Error::Can(error.to_string()))?;
    Ok(channels
        .into_iter()
        .map(|channel| DeviceChoice {
            adapter: AdapterKind::Vector,
            channel: channel.channel,
            hardware_type: channel.hardware_type,
            serial: None,
            supports_fd: channel.supports_fd,
            label: format!("{} / CAN {}", channel.name, channel.channel + 1),
        })
        .collect())
}

#[cfg(not(windows))]
fn scan_vector() -> Result<Vec<DeviceChoice>> {
    Err(Error::Can(
        "Vector adapters are available on Windows only".to_string(),
    ))
}

#[cfg(windows)]
fn scan_tosun() -> Result<Vec<DeviceChoice>> {
    let device = autors_can::vendors::tosun::TosunCan::new()
        .map_err(|error| Error::Can(error.to_string()))?;
    let mut choices = Vec::new();
    for entry in device.scan_devices() {
        for channel in 0..entry.can_channel_count() {
            choices.push(DeviceChoice {
                adapter: AdapterKind::Tosun,
                channel,
                hardware_type: 0,
                serial: (!entry.serial.is_empty()).then_some(entry.serial.clone()),
                supports_fd: true,
                label: format!("{} / CAN {}", entry.display_name(), channel + 1),
            });
        }
    }
    Ok(choices)
}

#[cfg(not(windows))]
fn scan_tosun() -> Result<Vec<DeviceChoice>> {
    Err(Error::Can(
        "TOSUN adapters are available on Windows only".to_string(),
    ))
}

struct DemoEcu {
    core: DeviceCore,
    response_id: u32,
    opened: bool,
    rx: VecDeque<CanFrame>,
    transport: IsoTpFsm,
}

impl DemoEcu {
    fn new(response_id: u32) -> Self {
        Self {
            core: DeviceCore::new(),
            response_id,
            opened: false,
            rx: VecDeque::new(),
            transport: Self::receiver(),
        }
    }

    fn receiver() -> IsoTpFsm {
        IsoTpFsm::new(Vec::new(), true, 8, 0, 0)
    }

    fn process_frame(&mut self, data: &[u8]) {
        if self.transport.on_received(data).is_err() {
            return;
        }
        Self::pump(&mut self.transport, self.response_id, &mut self.rx);
        if self.transport.state() != TpState::Success {
            return;
        }

        let request = self.transport.take_received();
        let response = demo_response(&request);
        if response.is_empty() {
            self.transport = Self::receiver();
            return;
        }
        self.transport = IsoTpFsm::new(response, false, 8, 0, 0);
        Self::pump(&mut self.transport, self.response_id, &mut self.rx);
    }

    fn pump(fsm: &mut IsoTpFsm, response_id: u32, rx: &mut VecDeque<CanFrame>) {
        loop {
            let (state, frame) = fsm.next_frame();
            let Some(data) = frame else { break };
            rx.push_back(CanFrame::new(
                "Virtual ECU / CAN 1",
                response_id,
                data,
                false,
                FrameType::CAN20B,
            ));
            if state >= TpState::Success {
                break;
            }
        }
    }
}

#[async_trait]
impl CanDevice for DemoEcu {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> autors_can::Result<bool> {
        Ok(true)
    }

    async fn open(&mut self, _config: CanConfiguration) -> autors_can::Result<bool> {
        self.opened = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.opened = false;
    }

    async fn send(
        &mut self,
        _can_id: u32,
        data: &[u8],
        _frame_type: FrameType,
    ) -> autors_can::Result<usize> {
        if !self.opened {
            return Ok(0);
        }
        self.process_frame(data);
        Ok(data.len())
    }

    async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
        Ok(self.rx.pop_front())
    }
}

fn demo_response(request: &[u8]) -> Vec<u8> {
    let Some(&sid) = request.first() else {
        return Vec::new();
    };
    let sub_function = request.get(1).copied().unwrap_or_default();
    if matches!(sid, 0x10 | 0x11 | 0x27 | 0x28 | 0x31 | 0x3E | 0x85) && sub_function & 0x80 != 0 {
        return Vec::new();
    }
    match sid {
        0x10 => vec![0x50, sub_function, 0x00, 0x32, 0x01, 0xF4],
        0x11 => vec![0x51, sub_function],
        0x14 => vec![0x54],
        0x22 if request.len() >= 3 => {
            let mut response = vec![0x62, request[1], request[2]];
            response.extend_from_slice(b"EXAMPLE-1.0");
            response
        }
        0x27 if sub_function == 0x11 => {
            let mut response = vec![0x67, sub_function];
            response.extend(0x10..=0x1F);
            response
        }
        0x27 => vec![0x67, sub_function],
        0x28 => vec![0x68, sub_function],
        0x2E if request.len() >= 3 => vec![0x6E, request[1], request[2]],
        0x31 if request.len() >= 4 => {
            vec![0x71, sub_function, request[2], request[3], 0x04]
        }
        0x34 => vec![0x74, 0x20, 0x04, 0x02],
        0x36 => vec![0x76, request.get(1).copied().unwrap_or_default()],
        0x37 => vec![0x77],
        0x85 => vec![0xC5, sub_function],
        _ => vec![0x7F, sid, 0x11],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_ids_receive_the_library_flag() {
        assert_eq!(wire_id(0x700), 0x700);
        assert_eq!(wire_id(0x18DA_00F1), 0x98DA_00F1);
    }

    #[test]
    fn demo_security_seed_has_the_expected_length() {
        let response = demo_response(&[0x27, 0x11]);
        assert_eq!(response.len(), 18);
    }

    #[test]
    fn did_high_byte_is_not_treated_as_a_suppress_response_bit() {
        let response = demo_response(&[0x22, 0xF1, 0x83]);
        assert_eq!(&response[..3], &[0x62, 0xF1, 0x83]);
    }
}
