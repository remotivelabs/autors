use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_asc::AscFile;
use autors_can::device::{is_can_id_valid, CanDevice, ChannelInfo, DeviceCore};
use autors_can::frame::{CanConfiguration, CanFrame, FrameType};
use autors_dbc::dbc::{DBCFile, MsgType};
use autors_isotp::isotp::MsgState;
use autors_isotp::transport::IsoTp;
use autors_scheduler::can::CanMessageState;
use autors_scheduler::CanScheduler;

use crate::hardware::{create_can, AdapterKind};

const TRACE_LIMIT: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawCanProtocol {
    Ccp,
    Xcp,
}

/// One trace row captured by the virtual CAN channel.
#[derive(Debug, Clone)]
pub struct BusTrace {
    pub timestamp: Duration,
    pub is_tx: bool,
    pub id: u32,
    pub message: String,
    pub frame_type: FrameType,
    pub data: Vec<u8>,
    pub signals: Vec<String>,
}

/// Runtime model behind the CANoe-style bus and remaining-bus panels.
pub struct BusSession {
    device: VirtualCanDevice,
    hardware_device: Option<TracingHardwareCanDevice>,
    adapter: AdapterKind,
    channel: i32,
    hardware_type: i32,
    channels: Vec<ChannelInfo>,
    scheduler: Option<CanScheduler>,
    database: Option<DBCFile>,
    origin: Instant,
    elapsed: Duration,
    trace: VecDeque<BusTrace>,
    pub connected: bool,
    pub running: bool,
    pub last_error: Option<String>,
    tx_frames: u64,
    rx_frames: u64,
    wire_bits: u64,
}

impl Default for BusSession {
    fn default() -> Self {
        Self::new()
    }
}

impl BusSession {
    pub fn new() -> Self {
        Self {
            device: VirtualCanDevice::new(),
            hardware_device: None,
            adapter: AdapterKind::Virtual,
            channel: 0,
            hardware_type: 0,
            channels: vec![virtual_channel()],
            scheduler: None,
            database: None,
            origin: Instant::now(),
            elapsed: Duration::ZERO,
            trace: VecDeque::new(),
            connected: false,
            running: false,
            last_error: None,
            tx_frames: 0,
            rx_frames: 0,
            wire_bits: 0,
        }
    }

    pub fn attach_database(&mut self, database: &DBCFile) -> Result<(), String> {
        let scheduler =
            CanScheduler::from_dbc_at(database, self.origin).map_err(|error| error.to_string())?;
        self.database = Some(database.clone());
        self.scheduler = Some(scheduler);
        self.last_error = None;
        Ok(())
    }

    pub fn connect(&mut self) -> Result<(), String> {
        if self.connected {
            return Ok(());
        }
        let bus_id = format!("{}/CAN{}", self.adapter.name(), self.channel + 1);
        let config = CanConfiguration {
            channel: self.channel,
            hardware_type: self.hardware_type,
            bus_id: Some(bus_id.clone()),
            ..CanConfiguration::default()
        };
        let opened = if self.adapter == AdapterKind::Virtual {
            autors_runtime::block_on(self.device.open(config)).map_err(|error| error.to_string())?
        } else {
            let mut device =
                TracingHardwareCanDevice::new(create_can(self.adapter)?, bus_id, self.elapsed);
            if let Ok(channels) = autors_runtime::block_on(device.available_channels()) {
                self.apply_discovered_channels(channels);
            }
            let config = CanConfiguration {
                channel: self.channel,
                hardware_type: self.hardware_type,
                bus_id: Some(format!("{}/CAN{}", self.adapter.name(), self.channel + 1)),
                ..CanConfiguration::default()
            };
            let opened =
                autors_runtime::block_on(device.open(config)).map_err(|error| error.to_string())?;
            self.hardware_device = Some(device);
            opened
        };
        if !opened {
            self.hardware_device = None;
            return Err(format!(
                "{} CAN channel refused to open",
                self.adapter.name()
            ));
        }
        self.connected = true;
        self.last_error = None;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if self.adapter == AdapterKind::Virtual {
            autors_runtime::block_on(self.device.close());
        } else if let Some(device) = &mut self.hardware_device {
            autors_runtime::block_on(device.close());
        }
        self.hardware_device = None;
        self.connected = false;
        self.running = false;
    }

    pub fn set_running(&mut self, running: bool) -> Result<(), String> {
        if running && !self.connected {
            return Err("connect the selected CAN channel first".to_owned());
        }
        if running && self.scheduler.is_none() {
            return Err("open a DBC before starting remaining-bus simulation".to_owned());
        }
        self.running = running;
        Ok(())
    }

    pub fn advance(&mut self, elapsed: Duration) {
        if !self.connected {
            return;
        }
        self.elapsed = self.elapsed.saturating_add(elapsed);
        if self.adapter == AdapterKind::Virtual {
            self.device.set_clock(self.elapsed);
        } else if let Some(device) = &mut self.hardware_device {
            device.set_clock(self.elapsed);
        }
        if self.running {
            let now = self.origin.checked_add(self.elapsed).unwrap_or(self.origin);
            if let Some(scheduler) = &mut self.scheduler {
                let result = if self.adapter == AdapterKind::Virtual {
                    autors_runtime::block_on(scheduler.poll_at(&mut self.device, now))
                } else if let Some(device) = &mut self.hardware_device {
                    autors_runtime::block_on(scheduler.poll_at(device, now))
                } else {
                    return;
                };
                if let Err(error) = result {
                    self.last_error = Some(error.to_string());
                    self.running = false;
                }
            }
        }
        self.collect_device_traffic();
    }

    pub fn send_text(&mut self, command: &str) -> Result<usize, String> {
        if !self.connected {
            return Err("connect the selected CAN channel first".to_owned());
        }
        let (id, data) = parse_send_command(command)?;
        let frame_type = if data.len() <= 8 {
            FrameType::CAN20B
        } else {
            FrameType::FD_BRS
        };
        let sent = if self.adapter == AdapterKind::Virtual {
            self.device.set_clock(self.elapsed);
            autors_runtime::block_on(self.device.send(id, &data, frame_type))
        } else {
            let device = self
                .hardware_device
                .as_mut()
                .ok_or_else(|| "selected CAN adapter is not open".to_owned())?;
            device.set_clock(self.elapsed);
            autors_runtime::block_on(device.send(id, &data, frame_type))
        }
        .map_err(|error| error.to_string())?;
        self.collect_device_traffic();
        Ok(sent)
    }

    pub fn inject_text(&mut self, command: &str) -> Result<usize, String> {
        if !self.connected {
            return Err("connect the selected CAN channel first".to_owned());
        }
        if self.adapter != AdapterKind::Virtual {
            return Err("receive injection is available only on the virtual adapter".to_owned());
        }
        let (id, data) = parse_send_command(command)?;
        let frame_type = if data.len() <= 8 {
            FrameType::CAN20B
        } else {
            FrameType::FD_BRS
        };
        self.device.inject(id, data.clone(), frame_type);
        self.collect_device_traffic();
        Ok(data.len())
    }

    pub fn clear_trace(&mut self) {
        self.trace.clear();
        self.tx_frames = 0;
        self.rx_frames = 0;
        self.wire_bits = 0;
    }

    pub fn trace(&self) -> &VecDeque<BusTrace> {
        &self.trace
    }

    pub fn save_asc(&self, path: &std::path::Path) -> Result<usize, String> {
        let channel = u32::try_from(self.channel)
            .ok()
            .and_then(|channel| channel.checked_add(1))
            .ok_or_else(|| "ASC channel must be representable as a one-based number".to_owned())?;
        let mut file = AscFile::new();
        for record in &self.trace {
            let mut frame = CanFrame::new(
                format!("{}/CAN{}", self.adapter.name(), channel),
                record.id,
                record.data.clone(),
                record.is_tx,
                record.frame_type,
            );
            frame.elapsed = record.timestamp;
            file.add_can_frame(&frame, channel)
                .map_err(|error| error.to_string())?;
        }
        file.save(path).map_err(|error| error.to_string())?;
        Ok(file.records.len())
    }

    pub fn messages(&self) -> Vec<CanMessageState> {
        self.scheduler
            .as_ref()
            .map(CanScheduler::messages)
            .unwrap_or_default()
    }

    pub fn set_message_enabled(&mut self, id: u32, enabled: bool) -> Result<(), String> {
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open a DBC first".to_owned())?
            .set_message_enabled(id, enabled)
            .map_err(|error| error.to_string())
    }

    pub fn trigger(&mut self, id: u32) -> Result<(), String> {
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open a DBC first".to_owned())?
            .trigger(id)
            .map_err(|error| error.to_string())
    }

    pub fn set_payload(&mut self, id: u32, payload: Vec<u8>) -> Result<(), String> {
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open a DBC first".to_owned())?
            .set_payload(id, payload)
            .map_err(|error| error.to_string())
    }

    pub fn set_payload_text(&mut self, id: u32, payload: &str) -> Result<(), String> {
        self.set_payload(id, parse_payload(payload)?)
    }

    pub fn set_period_text(&mut self, id: u32, period: &str) -> Result<(), String> {
        let period = match period.trim().to_ascii_lowercase().as_str() {
            "off" | "event" | "none" => None,
            value => Some(Duration::from_millis(value.parse::<u64>().map_err(
                |_| "enter a period in milliseconds or 'event'".to_owned(),
            )?)),
        };
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open a DBC first".to_owned())?
            .set_period(id, period)
            .map_err(|error| error.to_string())
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn tx_frames(&self) -> u64 {
        self.tx_frames
    }

    pub fn rx_frames(&self) -> u64 {
        self.rx_frames
    }

    pub fn average_bits_per_second(&self) -> f64 {
        if self.elapsed.is_zero() {
            0.0
        } else {
            self.wire_bits as f64 / self.elapsed.as_secs_f64()
        }
    }

    pub fn average_frames_per_second(&self) -> f64 {
        if self.elapsed.is_zero() {
            0.0
        } else {
            (self.tx_frames + self.rx_frames) as f64 / self.elapsed.as_secs_f64()
        }
    }

    pub fn network_name(&self) -> &str {
        if self.database.is_some() {
            "DBC network loaded"
        } else {
            "No DBC loaded"
        }
    }

    /// Executes one UDS/KWP PDU through the actual ISO-TP state machine on the
    /// selected CAN adapter. The virtual adapter supplies a small deterministic
    /// ECU response so the same path remains testable without hardware.
    pub fn diagnostic_request(
        &mut self,
        command_id: u32,
        response_id: u32,
        use_can_fd: bool,
        request: &[u8],
    ) -> Result<(MsgState, Vec<u8>), String> {
        if !self.connected {
            return Err("connect the selected CAN channel first".to_owned());
        }
        if request.is_empty() {
            return Err("diagnostic request PDU is empty".to_owned());
        }
        let mut transport = IsoTp::new(command_id, response_id, use_can_fd);
        transport.p2_client = 1_000;
        transport.p3_client = 1_000;
        let mut response = Vec::new();
        if self.adapter == AdapterKind::Virtual {
            self.device
                .set_diagnostic_endpoint(Some((command_id, response_id)));
        }
        let result = if self.adapter == AdapterKind::Virtual {
            autors_runtime::block_on(transport.send_request(
                &mut self.device,
                request.to_vec(),
                Some(&mut response),
                true,
            ))
        } else {
            let device = self
                .hardware_device
                .as_mut()
                .ok_or_else(|| "selected CAN adapter is not open".to_owned())?;
            autors_runtime::block_on(transport.send_request(
                device,
                request.to_vec(),
                Some(&mut response),
                true,
            ))
        };
        self.device.set_diagnostic_endpoint(None);
        self.collect_device_traffic();
        result
            .map(|state| (state, response))
            .map_err(|error| error.to_string())
    }

    /// Executes one request/response exchange for CAN-native calibration
    /// protocols. This keeps CCP/XCP live traffic on the same selected adapter
    /// and in the same trace as scheduled and manually transmitted frames.
    pub fn raw_protocol_request(
        &mut self,
        command_id: u32,
        response_id: u32,
        use_can_fd: bool,
        protocol: RawCanProtocol,
        request: &[u8],
    ) -> Result<Vec<u8>, String> {
        if !self.connected {
            return Err("connect the selected CAN channel first".to_owned());
        }
        if request.is_empty() {
            return Err("protocol request frame is empty".to_owned());
        }
        let frame_type = if use_can_fd {
            FrameType::FD_BRS
        } else {
            FrameType::CAN20B
        };
        if self.adapter == AdapterKind::Virtual {
            self.device.raw_endpoint = Some((command_id, response_id, protocol));
        }
        let result = if self.adapter == AdapterKind::Virtual {
            autors_runtime::block_on(exchange_raw_can(
                &mut self.device,
                command_id,
                response_id,
                request,
                frame_type,
            ))
        } else {
            let device = self
                .hardware_device
                .as_mut()
                .ok_or_else(|| "selected CAN adapter is not open".to_owned())?;
            autors_runtime::block_on(exchange_raw_can(
                device,
                command_id,
                response_id,
                request,
                frame_type,
            ))
        };
        self.device.raw_endpoint = None;
        self.collect_device_traffic();
        result.map_err(|error| error.to_string())?.ok_or_else(|| {
            format!(
                "timed out waiting for CAN response 0x{:X}",
                response_id & 0x1fff_ffff
            )
        })
    }

    pub fn adapter_name(&self) -> &'static str {
        self.adapter.name()
    }

    pub fn driver_name(&self) -> &'static str {
        self.adapter.driver()
    }

    pub fn channel(&self) -> i32 {
        self.channel
    }

    pub fn hardware_type(&self) -> i32 {
        self.hardware_type
    }

    pub fn selected_channel_name(&self) -> String {
        self.channels
            .iter()
            .find(|channel| {
                channel.channel == self.channel && channel.hardware_type == self.hardware_type
            })
            .map(|channel| channel.name.clone())
            .unwrap_or_else(|| format!("channel {}", self.channel))
    }

    pub fn discovered_channels(&self) -> &[ChannelInfo] {
        &self.channels
    }

    pub fn cycle_adapter(&mut self) -> Result<&'static str, String> {
        if self.connected {
            return Err("disconnect CAN before changing adapters".to_owned());
        }
        self.adapter = self.adapter.next();
        self.channel = 0;
        self.hardware_type = self.adapter.default_can_hardware_type();
        self.channels = if self.adapter == AdapterKind::Virtual {
            vec![virtual_channel()]
        } else {
            Vec::new()
        };
        self.last_error = None;
        Ok(self.adapter.name())
    }

    pub fn configure_adapter(&mut self, input: &str) -> Result<(), String> {
        if self.connected {
            return Err("disconnect CAN before changing its channel".to_owned());
        }
        let (channel, hardware_type) = parse_adapter_configuration(input)?;
        self.channel = channel;
        if let Some(hardware_type) = hardware_type {
            self.hardware_type = hardware_type;
        }
        Ok(())
    }

    pub fn refresh_channels(&mut self) -> Result<usize, String> {
        if self.connected {
            return Err("disconnect CAN before enumerating channels".to_owned());
        }
        if self.adapter == AdapterKind::Virtual {
            self.channels = vec![virtual_channel()];
            return Ok(1);
        }
        let device = create_can(self.adapter)?;
        let channels = autors_runtime::block_on(device.available_channels())
            .map_err(|error| error.to_string())?;
        let count = channels.len();
        self.apply_discovered_channels(channels);
        Ok(count)
    }

    pub fn cycle_channel(&mut self, delta: isize) -> Result<String, String> {
        if self.connected {
            return Err("disconnect CAN before changing its channel".to_owned());
        }
        if self.channels.is_empty() {
            self.channel = if delta.is_negative() {
                self.channel.saturating_sub(1)
            } else {
                self.channel.saturating_add(1)
            };
        } else {
            let current = self
                .channels
                .iter()
                .position(|channel| {
                    channel.channel == self.channel && channel.hardware_type == self.hardware_type
                })
                .unwrap_or_default();
            let next = current
                .saturating_add_signed(delta)
                .min(self.channels.len().saturating_sub(1));
            self.channel = self.channels[next].channel;
            self.hardware_type = self.channels[next].hardware_type;
        }
        Ok(self.selected_channel_name())
    }

    fn apply_discovered_channels(&mut self, channels: Vec<ChannelInfo>) {
        if let Some(first) = channels.first() {
            if !channels.iter().any(|channel| {
                channel.channel == self.channel && channel.hardware_type == self.hardware_type
            }) {
                self.channel = first.channel;
                self.hardware_type = first.hardware_type;
            }
        }
        self.channels = channels;
    }

    fn collect_device_traffic(&mut self) {
        let sent = if self.adapter == AdapterKind::Virtual {
            self.device.drain_sent().collect::<Vec<_>>()
        } else {
            self.hardware_device
                .as_mut()
                .map(|device| device.drain_sent().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for frame in sent {
            self.push_trace(frame);
        }
        let mut received_frames = if self.adapter == AdapterKind::Virtual {
            self.device.drain_captured_received().collect::<Vec<_>>()
        } else {
            self.hardware_device
                .as_mut()
                .map(|device| device.drain_captured_received().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        loop {
            let received = if self.adapter == AdapterKind::Virtual {
                autors_runtime::block_on(self.device.receive())
            } else if let Some(device) = &mut self.hardware_device {
                autors_runtime::block_on(device.receive())
            } else {
                break;
            };
            match received {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    break;
                }
            }
        }
        if self.adapter == AdapterKind::Virtual {
            received_frames.extend(self.device.drain_captured_received());
        } else if let Some(device) = &mut self.hardware_device {
            received_frames.extend(device.drain_captured_received());
        }
        for frame in received_frames {
            if self.adapter == AdapterKind::Virtual {
                self.device.core_mut().record_received(&frame);
            } else if let Some(device) = &mut self.hardware_device {
                device.core_mut().record_received(&frame);
            }
            self.push_trace(frame);
        }
    }

    fn push_trace(&mut self, frame: CanFrame) {
        let message = self.database.as_ref().and_then(|database| {
            database
                .messages()
                .find(|message| message.id & 0x1fff_ffff == frame.id & 0x1fff_ffff)
        });
        let name = message
            .map(|message| message.name.clone())
            .unwrap_or_else(|| "-".to_owned());
        let signals = message
            .map(|message| decode_signals(message, &frame.data))
            .unwrap_or_default();
        if frame.is_master_frame {
            self.tx_frames = self.tx_frames.saturating_add(1);
        } else {
            self.rx_frames = self.rx_frames.saturating_add(1);
        }
        self.wire_bits = self
            .wire_bits
            .saturating_add(frame.raw_frame_length() as u64);
        if self.trace.len() == TRACE_LIMIT {
            self.trace.pop_front();
        }
        self.trace.push_back(BusTrace {
            timestamp: frame.elapsed,
            is_tx: frame.is_master_frame,
            id: frame.id,
            message: name,
            frame_type: frame.frame_type,
            data: frame.data,
            signals,
        });
    }
}

struct TracingHardwareCanDevice {
    inner: Box<dyn CanDevice + Send + Sync>,
    bus_id: String,
    clock: Duration,
    sent: VecDeque<CanFrame>,
    received_trace: VecDeque<CanFrame>,
}

impl TracingHardwareCanDevice {
    fn new(inner: Box<dyn CanDevice + Send + Sync>, bus_id: String, clock: Duration) -> Self {
        Self {
            inner,
            bus_id,
            clock,
            sent: VecDeque::new(),
            received_trace: VecDeque::new(),
        }
    }

    fn set_clock(&mut self, clock: Duration) {
        self.clock = clock;
    }

    fn drain_sent(&mut self) -> impl Iterator<Item = CanFrame> + '_ {
        self.sent.drain(..)
    }

    fn drain_captured_received(&mut self) -> impl Iterator<Item = CanFrame> + '_ {
        self.received_trace.drain(..)
    }
}

#[async_trait]
impl CanDevice for TracingHardwareCanDevice {
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
        self.bus_id = config.bus_id.clone().unwrap_or_else(|| self.bus_id.clone());
        self.inner.open(config).await
    }

    async fn close(&mut self) {
        self.inner.close().await;
        self.sent.clear();
        self.received_trace.clear();
    }

    async fn send(
        &mut self,
        can_id: u32,
        data: &[u8],
        frame_type: FrameType,
    ) -> autors_can::Result<usize> {
        let sent = self.inner.send(can_id, data, frame_type).await?;
        if sent > 0 {
            let mut frame = CanFrame::new(
                &self.bus_id,
                can_id,
                data[..sent.min(data.len())].to_vec(),
                true,
                frame_type,
            );
            frame.elapsed = self.clock;
            self.sent.push_back(frame);
        }
        Ok(sent)
    }

    async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
        let frame = self.inner.receive().await?;
        if let Some(frame) = &frame {
            self.received_trace.push_back(frame.clone());
        }
        Ok(frame)
    }

    async fn available_channels(&self) -> autors_can::Result<Vec<ChannelInfo>> {
        self.inner.available_channels().await
    }
}

struct VirtualCanDevice {
    core: DeviceCore,
    opened: bool,
    bus_id: String,
    clock: Duration,
    sent: VecDeque<CanFrame>,
    received: VecDeque<CanFrame>,
    received_trace: VecDeque<CanFrame>,
    loopback: bool,
    diagnostic_endpoint: Option<(u32, u32)>,
    raw_endpoint: Option<(u32, u32, RawCanProtocol)>,
}

fn virtual_channel() -> ChannelInfo {
    ChannelInfo {
        channel: 0,
        hardware_type: 0,
        name: "Virtual CAN 1".to_owned(),
        supports_fd: true,
    }
}

impl VirtualCanDevice {
    fn new() -> Self {
        Self {
            core: DeviceCore::new(),
            opened: false,
            bus_id: "Virtual/CAN1".to_owned(),
            clock: Duration::ZERO,
            sent: VecDeque::new(),
            received: VecDeque::new(),
            received_trace: VecDeque::new(),
            loopback: true,
            diagnostic_endpoint: None,
            raw_endpoint: None,
        }
    }

    fn set_clock(&mut self, clock: Duration) {
        self.clock = clock;
    }

    fn inject(&mut self, id: u32, data: Vec<u8>, frame_type: FrameType) {
        let mut frame = CanFrame::new(&self.bus_id, id, data, false, frame_type);
        frame.elapsed = self.clock;
        self.received.push_back(frame);
    }

    fn drain_sent(&mut self) -> impl Iterator<Item = CanFrame> + '_ {
        self.sent.drain(..)
    }

    fn drain_captured_received(&mut self) -> impl Iterator<Item = CanFrame> + '_ {
        self.received_trace.drain(..)
    }

    fn set_diagnostic_endpoint(&mut self, endpoint: Option<(u32, u32)>) {
        self.diagnostic_endpoint = endpoint;
    }
}

#[async_trait]
impl CanDevice for VirtualCanDevice {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> autors_can::Result<bool> {
        Ok(true)
    }

    async fn open(&mut self, config: CanConfiguration) -> autors_can::Result<bool> {
        self.bus_id = config.bus_id.unwrap_or_else(|| "Virtual/CAN1".to_owned());
        self.opened = true;
        Ok(true)
    }

    async fn close(&mut self) {
        self.opened = false;
        self.received.clear();
        self.received_trace.clear();
    }

    async fn send(
        &mut self,
        can_id: u32,
        data: &[u8],
        frame_type: FrameType,
    ) -> autors_can::Result<usize> {
        if !self.opened {
            return Err(autors_can::Error::Driver(
                "virtual CAN channel is closed".to_owned(),
            ));
        }
        if !is_can_id_valid(can_id) {
            return Err(autors_can::Error::Invalid(format!(
                "invalid CAN identifier {can_id:#X}"
            )));
        }
        let maximum = if frame_type.is_classic() { 8 } else { 64 };
        if data.len() > maximum {
            return Err(autors_can::Error::Invalid(format!(
                "{} byte payload exceeds {maximum}",
                data.len()
            )));
        }
        let mut frame = CanFrame::new(&self.bus_id, can_id, data.to_vec(), true, frame_type);
        frame.elapsed = self.clock;
        self.core.record_sent(&frame);
        self.sent.push_back(frame.clone());
        if let Some((command_id, response_id)) = self.diagnostic_endpoint {
            if can_id == command_id {
                if let Some(response) = virtual_isotp_response(data) {
                    let mut response_frame =
                        CanFrame::new(&self.bus_id, response_id, response, false, frame_type);
                    response_frame.elapsed = self.clock;
                    self.received.push_back(response_frame);
                }
            }
        }
        if let Some((command_id, response_id, protocol)) = self.raw_endpoint {
            if can_id == command_id {
                let response = virtual_raw_response(protocol, data);
                let mut response_frame =
                    CanFrame::new(&self.bus_id, response_id, response, false, frame_type);
                response_frame.elapsed = self.clock;
                self.received.push_back(response_frame);
            }
        }
        if self.loopback {
            frame.is_master_frame = false;
            self.received.push_back(frame);
        }
        Ok(data.len())
    }

    async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
        if !self.opened {
            return Ok(None);
        }
        let frame = self.received.pop_front();
        if let Some(frame) = &frame {
            self.received_trace.push_back(frame.clone());
        }
        Ok(frame)
    }

    async fn available_channels(&self) -> autors_can::Result<Vec<ChannelInfo>> {
        Ok(vec![virtual_channel()])
    }
}

fn virtual_isotp_response(frame: &[u8]) -> Option<Vec<u8>> {
    let (offset, length): (usize, usize) = match frame.first().copied()? {
        0 if frame.len() >= 2 => (2, usize::from(frame[1])),
        pci if pci >> 4 == 0 => (1, usize::from(pci & 0x0f)),
        _ => return None,
    };
    let request = frame.get(offset..offset.checked_add(length)?)?;
    let service = *request.first()?;
    if service_has_subfunction(service)
        && request
            .get(1)
            .is_some_and(|subfunction| subfunction & 0x80 != 0)
    {
        return None;
    }
    let response = match service {
        0x10 => vec![
            0x50,
            request.get(1).copied().unwrap_or(1) & 0x7f,
            0x00,
            0x32,
            0x01,
            0xF4,
        ],
        0x11 => vec![0x51, request.get(1).copied().unwrap_or(1) & 0x7f],
        0x22 if request.len() >= 3 => {
            vec![0x62, request[1], request[2], 0x12, 0x34]
        }
        0x2e if request.len() >= 3 => vec![0x6e, request[1], request[2]],
        0x31 if request.len() >= 4 => vec![0x71, request[1], request[2], request[3]],
        0x3e => vec![0x7e, request.get(1).copied().unwrap_or_default() & 0x7f],
        _ => vec![0x7f, service, 0x11],
    };
    let mut transport = Vec::with_capacity(8);
    transport.push(response.len() as u8);
    transport.extend_from_slice(&response);
    transport.resize(8, 0xff);
    Some(transport)
}

fn virtual_raw_response(protocol: RawCanProtocol, request: &[u8]) -> Vec<u8> {
    match protocol {
        RawCanProtocol::Ccp => {
            let mut response = vec![0xff, 0x00, request.get(1).copied().unwrap_or_default()];
            response.resize(8, 0);
            response
        }
        RawCanProtocol::Xcp => {
            let mut response = vec![0xff];
            response.resize(if request.len() > 8 { request.len() } else { 8 }, 0);
            response
        }
    }
}

async fn exchange_raw_can<D: CanDevice + Send>(
    device: &mut D,
    command_id: u32,
    response_id: u32,
    request: &[u8],
    frame_type: FrameType,
) -> autors_can::Result<Option<Vec<u8>>> {
    let sent = device.send(command_id, request, frame_type).await?;
    if sent != request.len() {
        return Err(autors_can::Error::Driver(format!(
            "CAN adapter accepted {sent} of {} request byte(s)",
            request.len()
        )));
    }
    let deadline = Instant::now() + Duration::from_millis(1_000);
    loop {
        if let Some(frame) = device.receive().await? {
            if frame.id == response_id {
                return Ok(Some(frame.data));
            }
        } else if Instant::now() >= deadline {
            return Ok(None);
        } else {
            autors_runtime::sleep(Duration::from_millis(1)).await;
        }
    }
}

fn service_has_subfunction(service: u8) -> bool {
    matches!(
        service,
        0x10 | 0x11 | 0x19 | 0x27 | 0x28 | 0x31 | 0x3e | 0x83 | 0x84 | 0x85 | 0x86 | 0x87
    )
}

fn parse_adapter_configuration(input: &str) -> Result<(i32, Option<i32>), String> {
    let mut values = input.split_whitespace();
    let channel = values
        .next()
        .ok_or_else(|| "enter a zero-based channel and optional hardware type".to_owned())?;
    let channel = parse_i32(channel, "channel")?;
    if channel < 0 {
        return Err("channel must be zero or greater".to_owned());
    }
    let hardware_type = values
        .next()
        .map(|value| parse_i32(value, "hardware type"))
        .transpose()?;
    if values.next().is_some() {
        return Err("enter only a channel and optional hardware type".to_owned());
    }
    Ok((channel, hardware_type))
}

fn parse_i32(value: &str, field: &str) -> Result<i32, String> {
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        i32::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|_| format!("invalid {field} {value:?}"))
}

fn parse_send_command(command: &str) -> Result<(u32, Vec<u8>), String> {
    let mut fields = command.split_whitespace();
    let id_text = fields
        .next()
        .ok_or_else(|| "enter a CAN ID followed by hexadecimal bytes".to_owned())?;
    let extended = id_text.ends_with(['x', 'X']);
    let digits = id_text
        .trim_end_matches(['x', 'X'])
        .strip_prefix("0x")
        .unwrap_or_else(|| id_text.trim_end_matches(['x', 'X']));
    let raw_id = u32::from_str_radix(digits, 16)
        .map_err(|_| format!("invalid hexadecimal CAN ID {id_text:?}"))?;
    let id = raw_id | if extended { 0x8000_0000 } else { 0 };
    if !is_can_id_valid(id) {
        return Err(format!("CAN ID {id_text:?} is outside its valid range"));
    }
    let data = parse_payload_fields(fields)?;
    Ok((id, data))
}

fn parse_payload(payload: &str) -> Result<Vec<u8>, String> {
    parse_payload_fields(payload.split_whitespace())
}

fn parse_payload_fields<'a>(fields: impl Iterator<Item = &'a str>) -> Result<Vec<u8>, String> {
    let data = fields
        .map(|field| {
            u8::from_str_radix(field.trim_start_matches("0x"), 16)
                .map_err(|_| format!("invalid hexadecimal data byte {field:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if data.len() > 64 {
        return Err(format!(
            "CAN payload has {} bytes; maximum is 64",
            data.len()
        ));
    }
    Ok(data)
}

fn decode_signals(message: &MsgType, data: &[u8]) -> Vec<String> {
    let selector = message
        .get_multiplex_signal()
        .and_then(|signal| signal.extract_raw(data));
    message
        .signals
        .iter()
        .filter(|signal| {
            !signal.is_multiplexed()
                || selector.is_some_and(|value| value == i64::from(signal.multiplex_value))
        })
        .filter_map(|signal| {
            let raw = signal.extract_raw(data)?;
            let value = signal
                .enums
                .as_ref()
                .and_then(|values| values.get(&raw))
                .cloned()
                .unwrap_or_else(|| format!("{:.6}", signal.to_physical(raw)));
            Some(format!("{} = {} {}", signal.name, value, signal.unit))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBC: &str = r#"
VERSION "bus"
NS_ :
BS_:
BU_: ECU
BO_ 291 Status: 2 ECU
 SG_ Speed : 0|16@1+ (0.1,0) [0|250] "km/h" ECU
BA_DEF_ BO_ "GenMsgCycleTime" INT 0 10000;
BA_DEF_DEF_ "GenMsgCycleTime" 0;
BA_ "GenMsgCycleTime" BO_ 291 10;
"#;

    #[test]
    fn manual_send_and_injection_are_captured_with_dbc_decoding() {
        let database = DBCFile::parse_str(DBC).unwrap();
        let mut session = BusSession::new();
        session.attach_database(&database).unwrap();
        session.connect().unwrap();
        assert_eq!(session.send_text("123 64 00").unwrap(), 2);
        assert_eq!(session.tx_frames(), 1);
        assert_eq!(session.rx_frames(), 1);
        assert_eq!(session.trace()[0].message, "Status");
        assert!(session.trace()[0].signals[0].contains("10.000000 km/h"));
        assert_eq!(session.inject_text("123 C8 00").unwrap(), 2);
        assert_eq!(session.rx_frames(), 2);
    }

    #[test]
    fn scheduler_sends_enabled_dbc_messages_at_their_period() {
        let database = DBCFile::parse_str(DBC).unwrap();
        let mut session = BusSession::new();
        session.attach_database(&database).unwrap();
        session.connect().unwrap();
        session.set_message_enabled(0x123, true).unwrap();
        session.set_payload_text(0x123, "2A 00").unwrap();
        session.set_period_text(0x123, "20").unwrap();
        session.set_running(true).unwrap();
        session.advance(Duration::ZERO);
        assert_eq!(session.tx_frames(), 1);
        assert_eq!(session.trace()[0].data, [0x2A, 0]);
        session.advance(Duration::from_millis(20));
        assert_eq!(session.tx_frames(), 2);
    }

    #[test]
    fn parses_standard_extended_and_fd_send_commands() {
        assert_eq!(
            parse_send_command("123 AA BB").unwrap(),
            (0x123, vec![0xAA, 0xBB])
        );
        assert_eq!(parse_send_command("1ABCDEFX 01").unwrap().0, 0x81AB_CDEF);
        assert!(parse_send_command("800").is_err());
        assert!(parse_send_command("123 GG").is_err());
    }

    #[test]
    fn adapter_configuration_accepts_decimal_and_hex_hardware_types() {
        assert_eq!(
            parse_adapter_configuration("2 0x51").unwrap(),
            (2, Some(0x51))
        );
        assert_eq!(parse_adapter_configuration("0").unwrap(), (0, None));
        assert!(parse_adapter_configuration("-1").is_err());
        assert!(parse_adapter_configuration("0 3 extra").is_err());
    }

    #[test]
    fn virtual_channel_runs_a_real_isotp_exchange() {
        let mut session = BusSession::new();
        session.connect().unwrap();
        let (state, response) = session
            .diagnostic_request(0x7e0, 0x7e8, false, &[0x22, 0xf1, 0x90])
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(response, [0x62, 0xf1, 0x90, 0x12, 0x34]);
        assert!(session
            .trace()
            .iter()
            .any(|frame| !frame.is_tx && frame.id == 0x7e8));
    }

    #[test]
    fn virtual_channel_runs_ccp_and_xcp_request_response_exchanges() {
        let mut session = BusSession::new();
        session.connect().unwrap();
        let ccp = session
            .raw_protocol_request(
                0x600,
                0x601,
                false,
                RawCanProtocol::Ccp,
                &[0x01, 0x27, 0x34, 0x12],
            )
            .unwrap();
        assert_eq!(&ccp[..3], &[0xff, 0x00, 0x27]);
        let xcp = session
            .raw_protocol_request(0x600, 0x601, false, RawCanProtocol::Xcp, &[0xff, 0x00])
            .unwrap();
        assert_eq!(xcp[0], 0xff);
        assert_eq!(
            session
                .trace()
                .iter()
                .filter(|frame| !frame.is_tx && frame.id == 0x601)
                .count(),
            2
        );
    }

    #[test]
    fn exports_live_trace_as_roundtrippable_asc() {
        let mut session = BusSession::new();
        session.connect().unwrap();
        session.send_text("123 01 02").unwrap();
        let path =
            std::env::temp_dir().join(format!("autors-cli-live-can-{}.asc", std::process::id()));
        assert_eq!(session.save_asc(&path).unwrap(), 2);
        let trace = AscFile::open(&path).unwrap();
        assert_eq!(trace.records.len(), 2);
        std::fs::remove_file(path).unwrap();
    }
}
