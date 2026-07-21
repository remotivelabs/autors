use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_ldf::model::{FrameRef, Ldf, SignalValue};
use autors_lin::device::{
    compute_checksum, ChecksumType as LinChecksumType, LinConfiguration, LinDevice, LinFrame,
};
use autors_ltrc::{
    ChecksumType as LtrcChecksumType, Direction as LtrcDirection, LtrcFile, LtrcVersion,
    Record as LtrcRecord, StartTime, TraceFrame,
};
use autors_scheduler::lin::LinFrameState;
use autors_scheduler::LinScheduler;

use crate::hardware::{create_lin, AdapterKind};

const TRACE_LIMIT: usize = 10_000;

/// One operation captured by the virtual LIN channel.
#[derive(Debug, Clone)]
pub struct LinBusTrace {
    pub timestamp: Duration,
    pub is_tx: bool,
    pub id: u8,
    pub frame: String,
    pub operation: &'static str,
    pub data: Vec<u8>,
    pub dlc: u8,
    pub signals: Vec<String>,
}

/// Runtime model for LDF scheduling and virtual LIN traffic.
pub struct LinBusSession {
    device: VirtualLinDevice,
    hardware_device: Option<TracingHardwareLinDevice>,
    adapter: AdapterKind,
    channel: i32,
    hardware_type: i32,
    scheduler: Option<LinScheduler>,
    database: Option<Ldf>,
    origin: Instant,
    elapsed: Duration,
    trace: VecDeque<LinBusTrace>,
    pub connected: bool,
    pub running: bool,
    pub last_error: Option<String>,
    tx_frames: u64,
    rx_frames: u64,
    wire_bits: u64,
}

impl Default for LinBusSession {
    fn default() -> Self {
        Self::new()
    }
}

impl LinBusSession {
    pub fn new() -> Self {
        Self {
            device: VirtualLinDevice::new(),
            hardware_device: None,
            adapter: AdapterKind::Virtual,
            channel: 0,
            hardware_type: 0,
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

    pub fn attach_database(&mut self, database: &Ldf) -> Result<(), String> {
        let scheduler =
            LinScheduler::from_ldf_at(database, self.origin).map_err(|error| error.to_string())?;
        self.database = Some(database.clone());
        self.scheduler = Some(scheduler);
        self.running = false;
        self.last_error = None;
        Ok(())
    }

    pub fn connect(&mut self) -> Result<(), String> {
        if self.connected {
            return Ok(());
        }
        let baud_rate = self
            .database
            .as_ref()
            .map_or(19_200, |database| database.baud_rate);
        let baud_rate = u16::try_from(baud_rate)
            .map_err(|_| format!("LIN baud rate {baud_rate} exceeds the adapter range"))?;
        let mut configuration = LinConfiguration::new(0x3c, baud_rate);
        configuration.channel = self.channel;
        configuration.hardware_type = self.hardware_type;
        let bus_id = format!("{}/LIN{}", self.adapter.name(), self.channel + 1);
        configuration.bus_id = Some(bus_id.clone());
        let opened = if self.adapter == AdapterKind::Virtual {
            autors_runtime::block_on(self.device.open(&configuration))
                .map_err(|error| error.to_string())?
        } else {
            let mut device =
                TracingHardwareLinDevice::new(create_lin(self.adapter)?, bus_id, self.elapsed);
            let opened = autors_runtime::block_on(device.open(&configuration))
                .map_err(|error| error.to_string())?;
            self.hardware_device = Some(device);
            opened
        };
        if !opened {
            self.hardware_device = None;
            return Err(format!(
                "{} LIN channel refused to open",
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
            return Err("connect the selected LIN channel first".to_owned());
        }
        if running {
            let scheduler = self
                .scheduler
                .as_mut()
                .ok_or_else(|| "open an LDF before starting LIN scheduling".to_owned())?;
            if scheduler.active_schedule().is_none() {
                let schedule = scheduler
                    .schedules()
                    .next()
                    .map(str::to_owned)
                    .ok_or_else(|| "the LDF has no schedule tables".to_owned())?;
                scheduler
                    .start_schedule(&schedule)
                    .map_err(|error| error.to_string())?;
            }
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
            return Err("connect the selected LIN channel first".to_owned());
        }
        let (id, data) = parse_send_command(command)?;
        let sent = if self.adapter == AdapterKind::Virtual {
            self.device.set_clock(self.elapsed);
            autors_runtime::block_on(self.device.send(id, &data))
        } else {
            let device = self
                .hardware_device
                .as_mut()
                .ok_or_else(|| "selected LIN adapter is not open".to_owned())?;
            device.set_clock(self.elapsed);
            autors_runtime::block_on(device.send(id, &data))
        }
        .map_err(|error| error.to_string())?;
        self.collect_device_traffic();
        Ok(sent)
    }

    pub fn inject_text(&mut self, command: &str) -> Result<usize, String> {
        if !self.connected {
            return Err("connect the selected LIN channel first".to_owned());
        }
        if self.adapter != AdapterKind::Virtual {
            return Err("receive injection is available only on the virtual adapter".to_owned());
        }
        let (id, data) = parse_send_command(command)?;
        self.device.inject(id, data.clone());
        self.collect_device_traffic();
        Ok(data.len())
    }

    pub fn clear_trace(&mut self) {
        self.trace.clear();
        self.tx_frames = 0;
        self.rx_frames = 0;
        self.wire_bits = 0;
    }

    pub fn trace(&self) -> &VecDeque<LinBusTrace> {
        &self.trace
    }

    pub fn save_ltrc(&self, path: &std::path::Path) -> Result<usize, String> {
        let records = self
            .trace
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let header_only = record.operation == "Header";
                let checksum = if header_only {
                    0
                } else {
                    compute_checksum(
                        LinChecksumType::CalcChecksumEnhanced,
                        record.id,
                        &record.data,
                    )
                };
                LtrcRecord::Frame(TraceFrame {
                    index: index as u64 + 1,
                    timestamp: record.timestamp,
                    direction: if header_only {
                        LtrcDirection::Subscriber
                    } else {
                        LtrcDirection::Publisher
                    },
                    id: record.id,
                    dlc: record.dlc,
                    data: if header_only {
                        vec![None; usize::from(record.dlc)]
                    } else {
                        record.data.iter().copied().map(Some).collect()
                    },
                    checksum,
                    checksum_type: LtrcChecksumType::Enhanced,
                    errors: Vec::new(),
                })
            })
            .collect::<Vec<_>>();
        let count = records.len();
        LtrcFile {
            version: LtrcVersion::V1_2,
            start_time: Some(StartTime::OleAutomationDays(0.0)),
            records,
        }
        .save(path)
        .map_err(|error| error.to_string())?;
        Ok(count)
    }

    pub fn frames(&self) -> Vec<LinFrameState> {
        self.scheduler
            .as_ref()
            .map(LinScheduler::frames)
            .unwrap_or_default()
    }

    pub fn schedules(&self) -> Vec<String> {
        self.scheduler
            .as_ref()
            .map(|scheduler| scheduler.schedules().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    pub fn active_schedule(&self) -> Option<&str> {
        self.scheduler
            .as_ref()
            .and_then(LinScheduler::active_schedule)
    }

    pub fn select_schedule(&mut self, index: usize) -> Result<String, String> {
        let names = self.schedules();
        let name = names
            .get(index)
            .ok_or_else(|| "the LDF has no schedule at that index".to_owned())?
            .clone();
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open an LDF first".to_owned())?
            .start_schedule(&name)
            .map_err(|error| error.to_string())?;
        Ok(name)
    }

    pub fn set_frame_enabled(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open an LDF first".to_owned())?
            .set_frame_enabled(name, enabled)
            .map_err(|error| error.to_string())
    }

    pub fn trigger(&mut self, name: &str) -> Result<(), String> {
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open an LDF first".to_owned())?
            .trigger_frame(name)
            .map_err(|error| error.to_string())
    }

    pub fn set_payload_text(&mut self, name: &str, payload: &str) -> Result<(), String> {
        let payload = parse_payload(payload)?;
        self.scheduler
            .as_mut()
            .ok_or_else(|| "open an LDF first".to_owned())?
            .set_payload(name, payload)
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

    pub fn baud_rate(&self) -> u32 {
        self.database
            .as_ref()
            .map_or(19_200, |database| database.baud_rate)
    }

    pub fn network_name(&self) -> &str {
        self.database
            .as_ref()
            .and_then(|database| database.channel_name.as_deref())
            .unwrap_or(if self.database.is_some() {
                "LDF network loaded"
            } else {
                "No LDF loaded"
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

    pub fn cycle_adapter(&mut self) -> Result<&'static str, String> {
        if self.connected {
            return Err("disconnect LIN before changing adapters".to_owned());
        }
        self.adapter = self.adapter.next();
        self.channel = 0;
        self.hardware_type = self.adapter.default_lin_hardware_type();
        self.last_error = None;
        Ok(self.adapter.name())
    }

    pub fn configure_adapter(&mut self, input: &str) -> Result<(), String> {
        if self.connected {
            return Err("disconnect LIN before changing its channel".to_owned());
        }
        let (channel, hardware_type) = parse_adapter_configuration(input)?;
        self.channel = channel;
        if let Some(hardware_type) = hardware_type {
            self.hardware_type = hardware_type;
        }
        Ok(())
    }

    pub fn cycle_channel(&mut self, delta: isize) -> Result<i32, String> {
        if self.connected {
            return Err("disconnect LIN before changing its channel".to_owned());
        }
        self.channel = if delta.is_negative() {
            self.channel.saturating_sub(1)
        } else {
            self.channel.saturating_add(1)
        };
        Ok(self.channel)
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
        for record in sent {
            self.push_trace(record.frame, record.header_only);
        }
        loop {
            let received = if self.adapter == AdapterKind::Virtual {
                autors_runtime::block_on(self.device.on_receive())
            } else if let Some(device) = &mut self.hardware_device {
                autors_runtime::block_on(device.on_receive())
            } else {
                break;
            };
            match received {
                Ok(Some(frame)) => self.push_trace(frame, false),
                Ok(None) => break,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    break;
                }
            }
        }
    }

    fn push_trace(&mut self, frame: LinFrame, header_only: bool) {
        let name = self
            .database
            .as_ref()
            .and_then(|database| database.frame_by_id(frame.id))
            .map(frame_name)
            .unwrap_or_else(|| "-".to_owned());
        let signals = if header_only {
            Vec::new()
        } else {
            self.database
                .as_ref()
                .and_then(|database| {
                    database
                        .unconditional_frame_by_id(frame.id)
                        .and_then(|definition| {
                            database
                                .decode_frame(&definition.name, &frame.data, true)
                                .ok()
                        })
                })
                .map(|values| {
                    values
                        .into_iter()
                        .map(|(name, value)| format!("{name} = {}", format_signal_value(&value)))
                        .collect()
                })
                .unwrap_or_default()
        };
        if frame.is_master_frame {
            self.tx_frames = self.tx_frames.saturating_add(1);
        } else {
            self.rx_frames = self.rx_frames.saturating_add(1);
        }
        let bits = if header_only {
            34
        } else {
            44 + (frame.data.len() as u64 * 10)
        };
        self.wire_bits = self.wire_bits.saturating_add(bits);
        if self.trace.len() == TRACE_LIMIT {
            self.trace.pop_front();
        }
        let dlc = self
            .database
            .as_ref()
            .and_then(|database| database.unconditional_frame_by_id(frame.id))
            .map_or(frame.data.len() as u8, |definition| definition.length)
            .clamp(1, 8);
        self.trace.push_back(LinBusTrace {
            timestamp: frame.elapsed,
            is_tx: frame.is_master_frame,
            id: frame.id,
            frame: name,
            operation: if header_only { "Header" } else { "Frame" },
            data: frame.data,
            dlc,
            signals,
        });
    }
}

struct VirtualLinRecord {
    frame: LinFrame,
    header_only: bool,
}

struct TracingHardwareLinDevice {
    inner: Box<dyn LinDevice + Send>,
    bus_id: String,
    clock: Duration,
    sent: VecDeque<VirtualLinRecord>,
}

impl TracingHardwareLinDevice {
    fn new(inner: Box<dyn LinDevice + Send>, bus_id: String, clock: Duration) -> Self {
        Self {
            inner,
            bus_id,
            clock,
            sent: VecDeque::new(),
        }
    }

    fn set_clock(&mut self, clock: Duration) {
        self.clock = clock;
    }

    fn drain_sent(&mut self) -> impl Iterator<Item = VirtualLinRecord> + '_ {
        self.sent.drain(..)
    }

    fn record_sent(&mut self, id: u8, data: Vec<u8>, header_only: bool) {
        let mut frame = LinFrame::new(&self.bus_id, id, data, true);
        frame.elapsed = self.clock;
        self.sent.push_back(VirtualLinRecord { frame, header_only });
    }
}

#[async_trait]
impl LinDevice for TracingHardwareLinDevice {
    fn unique_bus_id(&self) -> i32 {
        self.inner.unique_bus_id()
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    async fn open(&mut self, config: &LinConfiguration) -> autors_lin::Result<bool> {
        self.bus_id = config.bus_id.clone().unwrap_or_else(|| self.bus_id.clone());
        self.inner.open(config).await
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> autors_lin::Result<usize> {
        let sent = self.inner.send(id, data).await?;
        if sent > 0 {
            self.record_sent(id, data[..sent.min(data.len())].to_vec(), false);
        }
        Ok(sent)
    }

    async fn request(&mut self, id: u8) -> autors_lin::Result<bool> {
        let sent = self.inner.request(id).await?;
        if sent {
            self.record_sent(id, Vec::new(), true);
        }
        Ok(sent)
    }

    async fn on_receive(&mut self) -> autors_lin::Result<Option<LinFrame>> {
        self.inner.on_receive().await
    }

    async fn close(&mut self) {
        self.inner.close().await;
        self.sent.clear();
    }
}

struct VirtualLinDevice {
    opened: bool,
    bus_id: String,
    clock: Duration,
    sent: VecDeque<VirtualLinRecord>,
    received: VecDeque<LinFrame>,
}

impl VirtualLinDevice {
    fn new() -> Self {
        Self {
            opened: false,
            bus_id: "Virtual/LIN1".to_owned(),
            clock: Duration::ZERO,
            sent: VecDeque::new(),
            received: VecDeque::new(),
        }
    }

    fn set_clock(&mut self, clock: Duration) {
        self.clock = clock;
    }

    fn inject(&mut self, id: u8, data: Vec<u8>) {
        let mut frame = LinFrame::new(&self.bus_id, id, data, false);
        frame.elapsed = self.clock;
        self.received.push_back(frame);
    }

    fn drain_sent(&mut self) -> impl Iterator<Item = VirtualLinRecord> + '_ {
        self.sent.drain(..)
    }

    fn validate_operation(&self, id: u8, data: &[u8]) -> autors_lin::Result<()> {
        if !self.opened {
            return Err(autors_lin::Error::Driver(
                "virtual LIN channel is closed".to_owned(),
            ));
        }
        if id > 0x3f {
            return Err(autors_lin::Error::Invalid(format!(
                "LIN ID 0x{id:02X} exceeds 0x3F"
            )));
        }
        if data.len() > 8 {
            return Err(autors_lin::Error::Invalid(format!(
                "LIN payload has {} bytes; maximum is 8",
                data.len()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl LinDevice for VirtualLinDevice {
    fn unique_bus_id(&self) -> i32 {
        1
    }

    fn is_available(&self) -> bool {
        true
    }

    async fn open(&mut self, configuration: &LinConfiguration) -> autors_lin::Result<bool> {
        self.bus_id = configuration
            .bus_id
            .clone()
            .unwrap_or_else(|| "Virtual/LIN1".to_owned());
        self.opened = true;
        Ok(true)
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> autors_lin::Result<usize> {
        self.validate_operation(id, data)?;
        let mut frame = LinFrame::new(&self.bus_id, id, data.to_vec(), true);
        frame.elapsed = self.clock;
        self.sent.push_back(VirtualLinRecord {
            frame,
            header_only: false,
        });
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> autors_lin::Result<bool> {
        self.validate_operation(id, &[])?;
        let mut frame = LinFrame::new(&self.bus_id, id, Vec::new(), true);
        frame.elapsed = self.clock;
        self.sent.push_back(VirtualLinRecord {
            frame,
            header_only: true,
        });
        Ok(true)
    }

    async fn on_receive(&mut self) -> autors_lin::Result<Option<LinFrame>> {
        if !self.opened {
            return Ok(None);
        }
        Ok(self.received.pop_front())
    }

    async fn close(&mut self) {
        self.opened = false;
        self.received.clear();
    }
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

fn parse_send_command(command: &str) -> Result<(u8, Vec<u8>), String> {
    let mut fields = command.split_whitespace();
    let id_text = fields
        .next()
        .ok_or_else(|| "enter a LIN ID followed by hexadecimal bytes".to_owned())?;
    let id = u8::from_str_radix(id_text.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid hexadecimal LIN ID {id_text:?}"))?;
    if id > 0x3f {
        return Err(format!("LIN ID {id_text:?} exceeds 0x3F"));
    }
    Ok((id, parse_payload_fields(fields)?))
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
    if data.len() > 8 {
        return Err(format!(
            "LIN payload has {} bytes; maximum is 8",
            data.len()
        ));
    }
    Ok(data)
}

fn frame_name(frame: FrameRef<'_>) -> String {
    match frame {
        FrameRef::Unconditional(frame) => frame.name.clone(),
        FrameRef::Sporadic(frame) => frame.name.clone(),
        FrameRef::EventTriggered(frame) => frame.name.clone(),
        FrameRef::Diagnostic(frame) => frame.name.clone(),
    }
}

fn format_signal_value(value: &SignalValue) -> String {
    match value {
        SignalValue::Integer(value) => value.to_string(),
        SignalValue::Float(value) => format!("{value:.6}"),
        SignalValue::Text(value) => value.clone(),
        SignalValue::Bytes(value) => value
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LDF: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;
Channel_name = "BodyLIN";

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Slave;
}
Signals {
    CommandValue: 8, 7, Master, Slave;
    StatusValue: 8, 2, Slave, Master;
}
Frames {
    Command: 3, Master, 1 { CommandValue, 0; }
    Status: 1, Slave, 1 { StatusValue, 0; }
}
Node_attributes {
    Slave {
        LIN_protocol = "2.2";
        configured_NAD = 1;
        product_id = 1, 2, 3;
    }
}
Schedule_tables {
    Main {
        Command delay 10 ms;
        Status delay 10 ms;
    }
}
"#;

    #[test]
    fn manual_send_and_injection_are_traced_and_decoded() {
        let database = Ldf::parse_str(LDF).unwrap();
        let mut session = LinBusSession::new();
        session.attach_database(&database).unwrap();
        session.connect().unwrap();
        assert_eq!(session.send_text("03 2A").unwrap(), 1);
        assert_eq!(session.inject_text("01 05").unwrap(), 1);
        assert_eq!(session.tx_frames(), 1);
        assert_eq!(session.rx_frames(), 1);
        assert_eq!(session.trace()[0].frame, "Command");
        assert_eq!(session.trace()[1].signals, ["StatusValue = 5"]);
    }

    #[test]
    fn ldf_schedule_sends_master_and_requests_unsimulated_slave() {
        let database = Ldf::parse_str(LDF).unwrap();
        let mut session = LinBusSession::new();
        session.attach_database(&database).unwrap();
        session.connect().unwrap();
        session.set_payload_text("Command", "2A").unwrap();
        session.set_running(true).unwrap();
        session.advance(Duration::ZERO);
        assert_eq!(session.trace()[0].frame, "Command");
        assert_eq!(session.trace()[0].data, [0x2a]);
        session.advance(Duration::from_millis(10));
        assert_eq!(session.trace()[1].frame, "Status");
        assert_eq!(session.trace()[1].operation, "Header");
    }

    #[test]
    fn exports_live_trace_as_roundtrippable_ltrc() {
        let database = Ldf::parse_str(LDF).unwrap();
        let mut session = LinBusSession::new();
        session.attach_database(&database).unwrap();
        session.connect().unwrap();
        session.set_running(true).unwrap();
        session.advance(Duration::ZERO);
        session.advance(Duration::from_millis(10));
        let path =
            std::env::temp_dir().join(format!("autors-cli-live-lin-{}.ltrc", std::process::id()));
        assert_eq!(session.save_ltrc(&path).unwrap(), 2);
        let trace = LtrcFile::open(&path).unwrap();
        assert_eq!(trace.records.len(), 2);
        let LtrcRecord::Frame(header) = &trace.records[1] else {
            panic!("expected header request")
        };
        assert!(header.data.iter().all(Option::is_none));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn parses_lin_commands_and_rejects_invalid_ranges() {
        assert_eq!(
            parse_send_command("22 AA BB").unwrap(),
            (0x22, vec![0xaa, 0xbb])
        );
        assert!(parse_send_command("40 00").is_err());
        assert!(parse_send_command("01 GG").is_err());
        assert!(parse_send_command("01 00 01 02 03 04 05 06 07 08").is_err());
    }

    #[test]
    fn adapter_configuration_accepts_vendor_hardware_type() {
        assert_eq!(parse_adapter_configuration("1 3").unwrap(), (1, Some(3)));
        assert_eq!(parse_adapter_configuration("4 0x2").unwrap(), (4, Some(2)));
        assert!(parse_adapter_configuration("").is_err());
    }
}
