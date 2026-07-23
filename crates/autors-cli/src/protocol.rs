use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};

use autors_ccp::ccp::{
    CcpCommand, CcpFrame, CmdBase as CcpCmdBase, CmdConnect as CcpCmdConnect, CmdGetCcpVersion,
    CmdSetMta as CcpCmdSetMta, CmdUpload as CcpCmdUpload, CommandCode as CcpCommandCode,
};
use autors_diag::doip::{
    Activation, ActivationCode, DoIpType, Frame as DoIpFrame, ProtocolVersion, ResponseState,
    SocketType, SrcDstFrame, DEFAULT_PORT,
};
use autors_diag::doip_client::{DoIpClient, EntityData, VehicleIdentificationFilter};
use autors_diag::uds::{NegRespCode, RespBase, Sid};
use autors_xcp::ifdata_xcp::XcpHeaderLen;
use autors_xcp::xcp::{
    CmdBare as XcpCmdBare, CmdConnect as XcpCmdConnect, CmdDisconnect as XcpCmdDisconnect,
    CmdSetMta as XcpCmdSetMta, CmdUpload as XcpCmdUpload, CommandCode as XcpCommandCode,
    ConnectMode, XcpCommand, XcpFrame, XcpType,
};

const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolKind {
    Uds,
    DoIp,
    Ccp,
    Xcp,
}

impl ProtocolKind {
    pub const ALL: [Self; 4] = [Self::Uds, Self::DoIp, Self::Ccp, Self::Xcp];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Uds => "UDS / KWP",
            Self::DoIp => "DoIP",
            Self::Ccp => "CCP",
            Self::Xcp => "XCP",
        }
    }

    pub const fn help(self) -> &'static [&'static str] {
        match self {
            Self::Uds => &[
                "session default|programming|extended",
                "reset hard|keyoff|soft",
                "read F190 [DID...]",
                "write F190 <bytes...>",
                "routine start|stop|result <ID> [data]",
                "tester",
                "or enter a raw UDS response/request PDU",
            ],
            Self::DoIp => &[
                "d discovers UDP DoIP entities; [/] selects one",
                "diag <src> <dst> <UDS PDU>",
                "example: diag 0E80 1000 22 F1 90",
                "or enter a complete raw DoIP frame",
                "frames are encoded and decoded with ISO 13400 types",
            ],
            Self::Ccp => &[
                "connect <station-address>",
                "status",
                "version [major release]",
                "setmta <number> <extension> <address>",
                "upload <size>",
                "rx <CRM/DTO bytes> or raw CRO bytes",
            ],
            Self::Xcp => &[
                "connect | disconnect | status",
                "setmta <extension> <address>",
                "upload <elements>",
                "rx <response/DAQ bytes> or raw CTO bytes",
                "raw input uses XCP-on-CAN framing",
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolRecord {
    pub protocol: ProtocolKind,
    pub input: String,
    pub bytes: Vec<u8>,
    pub response: Option<Vec<u8>>,
    pub summary: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanTransportConfig {
    pub command_id: u32,
    pub response_id: u32,
    pub use_can_fd: bool,
}

impl CanTransportConfig {
    pub fn display(self) -> String {
        format!(
            "0x{:X} → 0x{:X} · {}",
            self.command_id & 0x1fff_ffff,
            self.response_id & 0x1fff_ffff,
            if self.use_can_fd {
                "CAN FD"
            } else {
                "classic CAN"
            }
        )
    }

    fn input(self) -> String {
        format!(
            "{:X} {:X} {}",
            self.command_id,
            self.response_id,
            if self.use_can_fd { "fd" } else { "classic" }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoIpTransportConfig {
    pub remote: SocketAddr,
    pub local_ip: IpAddr,
    pub source_address: u16,
    pub target_address: u16,
    pub p2_ms: u32,
}

impl DoIpTransportConfig {
    fn input(&self) -> String {
        format!(
            "{} {} {:04X} {:04X} {}",
            self.remote, self.local_ip, self.source_address, self.target_address, self.p2_ms
        )
    }
}

pub struct ProtocolLab {
    pub protocol_index: usize,
    pub record_index: usize,
    records: VecDeque<ProtocolRecord>,
    ccp_counter: u8,
    uds_can: CanTransportConfig,
    ccp_can: CanTransportConfig,
    xcp_can: CanTransportConfig,
    doip_config: DoIpTransportConfig,
    doip_client: Option<DoIpClient>,
    doip_status: String,
    doip_entities: Vec<EntityData>,
    pub doip_entity_index: usize,
}

impl Default for ProtocolLab {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolLab {
    pub fn new() -> Self {
        Self {
            protocol_index: 0,
            record_index: 0,
            records: VecDeque::new(),
            ccp_counter: 0,
            uds_can: CanTransportConfig {
                command_id: 0x7e0,
                response_id: 0x7e8,
                use_can_fd: false,
            },
            ccp_can: CanTransportConfig {
                command_id: 0x600,
                response_id: 0x601,
                use_can_fd: false,
            },
            xcp_can: CanTransportConfig {
                command_id: 0x600,
                response_id: 0x601,
                use_can_fd: false,
            },
            doip_config: DoIpTransportConfig {
                remote: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), DEFAULT_PORT),
                local_ip: IpAddr::from([0, 0, 0, 0]),
                source_address: 0x0e80,
                target_address: 0x1000,
                p2_ms: 1_000,
            },
            doip_client: None,
            doip_status: "not connected".to_owned(),
            doip_entities: Vec::new(),
            doip_entity_index: 0,
        }
    }

    pub fn protocol(&self) -> ProtocolKind {
        ProtocolKind::ALL[self.protocol_index]
    }

    pub fn records(&self) -> &VecDeque<ProtocolRecord> {
        &self.records
    }

    pub fn selected(&self) -> Option<&ProtocolRecord> {
        self.records.get(self.record_index)
    }

    pub fn change_protocol(&mut self, delta: isize) {
        self.protocol_index = self
            .protocol_index
            .saturating_add_signed(delta)
            .min(ProtocolKind::ALL.len() - 1);
    }

    pub fn set_protocol(&mut self, index: usize) {
        self.protocol_index = index.min(ProtocolKind::ALL.len() - 1);
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.record_index = self
            .record_index
            .saturating_add_signed(delta)
            .min(self.records.len().saturating_sub(1));
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.record_index = 0;
    }

    pub fn can_transport(&self) -> Option<CanTransportConfig> {
        match self.protocol() {
            ProtocolKind::Uds => Some(self.uds_can),
            ProtocolKind::Ccp => Some(self.ccp_can),
            ProtocolKind::Xcp => Some(self.xcp_can),
            ProtocolKind::DoIp => None,
        }
    }

    pub fn transport_input(&self) -> String {
        self.can_transport()
            .map(CanTransportConfig::input)
            .unwrap_or_else(|| self.doip_config.input())
    }

    pub fn transport_summary(&self, can_connected: bool) -> String {
        match self.protocol() {
            ProtocolKind::Uds => format!(
                "ISO-TP on {} · CAN {}",
                self.uds_can.display(),
                if can_connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
            ProtocolKind::Ccp => format!(
                "CCP CRO/DTO on {} · CAN {}",
                self.ccp_can.display(),
                if can_connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
            ProtocolKind::Xcp => format!(
                "XCP-on-CAN on {} · CAN {}",
                self.xcp_can.display(),
                if can_connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
            ProtocolKind::DoIp => format!(
                "TCP {} · SA {:04X} → TA {:04X}\n{}\nUDP discovery · {}",
                self.doip_config.remote,
                self.doip_config.source_address,
                self.doip_config.target_address,
                self.doip_status,
                self.selected_doip_entity()
                    .map(|entity| format!(
                        "{}/{} · {}",
                        self.doip_entity_index + 1,
                        self.doip_entities.len(),
                        entity.vehicle_id.vin
                    ))
                    .unwrap_or_else(|| "not scanned".to_owned())
            ),
        }
    }

    pub fn selected_doip_entity(&self) -> Option<&EntityData> {
        self.doip_entities.get(self.doip_entity_index)
    }

    pub fn discover_doip(&mut self) -> Result<usize, String> {
        if self.protocol() != ProtocolKind::DoIp {
            return Err("select the DoIP protocol before discovery".to_owned());
        }
        let discovery_endpoint = match self.doip_config.local_ip {
            IpAddr::V4(_) => SocketAddr::new(
                IpAddr::from([255, 255, 255, 255]),
                self.doip_config.remote.port(),
            ),
            IpAddr::V6(_) if self.doip_config.remote.is_ipv6() => self.doip_config.remote,
            IpAddr::V6(_) => {
                return Err(
                    "configure an IPv6 DoIP endpoint before IPv6 unicast discovery".to_owned(),
                );
            }
        };
        self.doip_status = format!("discovering via UDP {discovery_endpoint}");
        self.doip_entities = autors_runtime::block_on(DoIpClient::discover(
            discovery_endpoint,
            self.doip_config.local_ip,
            ProtocolVersion::Iso13400_2019,
            VehicleIdentificationFilter::All,
            self.doip_config.p2_ms,
        ))
        .map_err(|error| {
            self.doip_status = format!("discovery failed: {error}");
            self.doip_status.clone()
        })?;
        self.doip_entity_index = 0;
        if self.doip_entities.is_empty() {
            self.doip_status = format!("no entity answered {discovery_endpoint}");
        } else {
            self.apply_selected_doip_entity();
        }
        Ok(self.doip_entities.len())
    }

    pub fn change_doip_entity(&mut self, delta: isize) -> Result<String, String> {
        if self.doip_entities.is_empty() {
            return Err("run DoIP discovery with d first".to_owned());
        }
        self.doip_entity_index = self
            .doip_entity_index
            .saturating_add_signed(delta)
            .min(self.doip_entities.len() - 1);
        self.apply_selected_doip_entity();
        Ok(self.doip_status.clone())
    }

    pub fn doip_discovery_details(&self) -> Vec<String> {
        if self.doip_entities.is_empty() {
            return vec![
                "DoIP entity discovery".to_owned(),
                "Press d to broadcast a vehicle-identification request.".to_owned(),
                "The configured local interface and P2 timeout are reused.".to_owned(),
            ];
        }
        let mut lines = vec![format!(
            "DoIP entity discovery · {} response(s)",
            self.doip_entities.len()
        )];
        for (index, entity) in self.doip_entities.iter().enumerate() {
            let vehicle = &entity.vehicle_id;
            lines.push(format!(
                "{} {} · {} · LA {:04X}",
                if index == self.doip_entity_index {
                    "▶"
                } else {
                    " "
                },
                vehicle
                    .sender
                    .map_or_else(|| "unknown endpoint".to_owned(), |value| value.to_string()),
                vehicle.vin,
                vehicle.logical_adr
            ));
            lines.push(format!(
                "    EID {} · GID {} · {:?} · {:?}",
                hex_data(&vehicle.eid),
                hex_data(&vehicle.gid),
                vehicle.further_action,
                vehicle.synch_status
            ));
        }
        lines
    }

    pub fn configure_transport(&mut self, input: &str) -> Result<String, String> {
        match self.protocol() {
            ProtocolKind::Uds | ProtocolKind::Ccp | ProtocolKind::Xcp => {
                let config = parse_can_transport(input)?;
                match self.protocol() {
                    ProtocolKind::Uds => self.uds_can = config,
                    ProtocolKind::Ccp => self.ccp_can = config,
                    ProtocolKind::Xcp => self.xcp_can = config,
                    ProtocolKind::DoIp => unreachable!(),
                }
                Ok(format!("CAN transport configured: {}", config.display()))
            }
            ProtocolKind::DoIp => {
                self.doip_config = parse_doip_transport(input)?;
                self.doip_client = None;
                self.connect_doip()?;
                Ok(format!(
                    "DoIP routing activated at {}",
                    self.doip_config.remote
                ))
            }
        }
    }

    pub fn apply_uds_can_transport(&mut self, config: CanTransportConfig) -> String {
        self.uds_can = config;
        self.protocol_index = 0;
        format!("ODX CAN transport applied: {}", config.display())
    }

    pub fn prepare_live_request(&mut self, input: &str) -> Result<ProtocolRecord, String> {
        let mut record = match self.protocol() {
            ProtocolKind::Uds => uds_record(input)?,
            ProtocolKind::DoIp => uds_record(input)?,
            ProtocolKind::Ccp => {
                let record = ccp_record(input, self.ccp_counter)?;
                self.ccp_counter = self.ccp_counter.wrapping_add(1);
                record
            }
            ProtocolKind::Xcp => xcp_record(input)?,
        };
        record.protocol = self.protocol();
        Ok(record)
    }

    pub fn record_live_exchange(
        &mut self,
        mut request: ProtocolRecord,
        response: Vec<u8>,
        state: impl Into<String>,
    ) -> &ProtocolRecord {
        let state = state.into();
        let response_analysis = match request.protocol {
            ProtocolKind::Uds | ProtocolKind::DoIp => analyze_uds(&response).ok(),
            ProtocolKind::Ccp => ccp_record(&format!("rx {}", hex_data(&response)), 0)
                .ok()
                .map(|record| (record.summary, record.details)),
            ProtocolKind::Xcp => xcp_record(&format!("rx {}", hex_data(&response)))
                .ok()
                .map(|record| (record.summary, record.details)),
        };
        let response_summary = response_analysis
            .as_ref()
            .map(|(summary, _)| summary.as_str())
            .unwrap_or("no decodable response");
        request.summary = format!("Live {state} · {response_summary}");
        request
            .details
            .insert(0, format!("Transport result: {state}"));
        request
            .details
            .insert(1, format!("Request bytes: {}", hex_data(&request.bytes)));
        request
            .details
            .insert(2, format!("Response bytes: {}", hex_data(&response)));
        if let Some((_, details)) = response_analysis {
            request.details.push(String::new());
            request.details.push("Decoded response".to_owned());
            request.details.extend(details);
        }
        request.response = Some(response);
        self.push_record(request)
    }

    pub fn execute_doip_live(&mut self, input: &str) -> Result<&ProtocolRecord, String> {
        let request = self.prepare_live_request(input)?;
        if self.doip_client.is_none() {
            self.connect_doip()?;
        }
        let mut response = Vec::new();
        let state = autors_runtime::block_on(
            self.doip_client
                .as_mut()
                .expect("DoIP client connected above")
                .diagnose_request(self.doip_config.p2_ms, &request.bytes, &mut response),
        );
        if !matches!(state, autors_diag::uds::MsgState::Success) {
            self.doip_status = format!("request failed: {state:?}");
        }
        Ok(self.record_live_exchange(request, response, format!("{state:?}")))
    }

    pub fn submit(&mut self, input: &str) -> Result<&ProtocolRecord, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("enter a protocol command or hexadecimal PDU".to_owned());
        }
        let record = match self.protocol() {
            ProtocolKind::Uds => uds_record(input)?,
            ProtocolKind::DoIp => doip_record(input)?,
            ProtocolKind::Ccp => {
                let record = ccp_record(input, self.ccp_counter)?;
                self.ccp_counter = self.ccp_counter.wrapping_add(1);
                record
            }
            ProtocolKind::Xcp => xcp_record(input)?,
        };
        Ok(self.push_record(record))
    }

    fn push_record(&mut self, record: ProtocolRecord) -> &ProtocolRecord {
        if self.records.len() == HISTORY_LIMIT {
            self.records.pop_front();
        }
        self.records.push_back(record);
        self.record_index = self.records.len() - 1;
        self.records.back().expect("record was just inserted")
    }

    fn connect_doip(&mut self) -> Result<(), String> {
        self.doip_status = format!("connecting to {}", self.doip_config.remote);
        let mut client = autors_runtime::block_on(DoIpClient::connect_to(
            self.doip_config.remote,
            self.doip_config.local_ip,
            Activation::Default,
            self.doip_config.source_address,
            self.doip_config.target_address,
            ProtocolVersion::Iso13400_2019,
        ))
        .map_err(|error| {
            self.doip_status = format!("connect failed: {error}");
            self.doip_status.clone()
        })?;
        let (state, activation) = autors_runtime::block_on(client.routing_activation(
            Activation::Default,
            0,
            0,
            self.doip_config.p2_ms,
        ));
        if state != ResponseState::Ok
            || !activation.is_some_and(|response| {
                response.activation_result == ActivationCode::RoutingActivated
            })
        {
            self.doip_status = format!("routing activation failed: {state:?} {activation:?}");
            return Err(self.doip_status.clone());
        }
        self.doip_client = Some(client);
        self.doip_status = "routing active".to_owned();
        Ok(())
    }

    fn apply_selected_doip_entity(&mut self) {
        let Some(vehicle) = self.selected_doip_entity().map(|entity| &entity.vehicle_id) else {
            return;
        };
        let sender = vehicle.sender;
        let logical_adr = vehicle.logical_adr;
        let vin = vehicle.vin.clone();
        if let Some(sender) = sender {
            self.doip_config.remote = sender;
        }
        self.doip_config.target_address = logical_adr;
        self.doip_client = None;
        self.doip_status = format!("selected {vin} · press c to activate route");
    }
}

fn parse_can_transport(input: &str) -> Result<CanTransportConfig, String> {
    let mut fields = input.split_whitespace();
    let command_id = parse_u32(
        fields
            .next()
            .ok_or_else(|| "enter command ID, response ID, and classic/fd".to_owned())?,
    )?;
    let response_id = parse_u32(
        fields
            .next()
            .ok_or_else(|| "enter a response CAN ID".to_owned())?,
    )?;
    let use_can_fd = match fields
        .next()
        .unwrap_or("classic")
        .to_ascii_lowercase()
        .as_str()
    {
        "classic" | "can" | "0" => false,
        "fd" | "canfd" | "can-fd" | "1" => true,
        value => return Err(format!("unknown CAN transport mode {value:?}")),
    };
    if fields.next().is_some() {
        return Err("enter only command ID, response ID, and classic/fd".to_owned());
    }
    if !autors_can::device::is_can_id_valid(command_id)
        || !autors_can::device::is_can_id_valid(response_id)
    {
        return Err("CAN identifiers must be valid 11-bit IDs or flagged 29-bit IDs".to_owned());
    }
    Ok(CanTransportConfig {
        command_id,
        response_id,
        use_can_fd,
    })
}

fn parse_doip_transport(input: &str) -> Result<DoIpTransportConfig, String> {
    let mut fields = input.split_whitespace();
    let remote_text = fields.next().ok_or_else(|| {
        "enter remote endpoint, local IP, source, target, and optional P2 ms".to_owned()
    })?;
    let remote = remote_text
        .parse::<SocketAddr>()
        .or_else(|_| {
            remote_text
                .parse::<IpAddr>()
                .map(|address| SocketAddr::new(address, DEFAULT_PORT))
        })
        .map_err(|_| format!("invalid DoIP remote endpoint {remote_text:?}"))?;
    let local_text = fields
        .next()
        .ok_or_else(|| "enter the local interface IP".to_owned())?;
    let local_ip = local_text
        .parse::<IpAddr>()
        .map_err(|_| format!("invalid local IP {local_text:?}"))?;
    let source_address = parse_u16(
        fields
            .next()
            .ok_or_else(|| "enter the tester source address".to_owned())?,
    )?;
    let target_address = parse_u16(
        fields
            .next()
            .ok_or_else(|| "enter the ECU target address".to_owned())?,
    )?;
    let p2_ms = fields
        .next()
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| format!("invalid P2 timeout {value:?}"))
        })
        .transpose()?
        .unwrap_or(1_000);
    if p2_ms == 0 {
        return Err("P2 timeout must be greater than zero".to_owned());
    }
    if fields.next().is_some() {
        return Err("enter only remote, local IP, source, target, and optional P2 ms".to_owned());
    }
    Ok(DoIpTransportConfig {
        remote,
        local_ip,
        source_address,
        target_address,
        p2_ms,
    })
}

fn uds_record(input: &str) -> Result<ProtocolRecord, String> {
    let mut fields = input.split_whitespace();
    let command = fields.next().unwrap_or_default().to_ascii_lowercase();
    let bytes = match command.as_str() {
        "session" => {
            let session = match fields
                .next()
                .unwrap_or("default")
                .to_ascii_lowercase()
                .as_str()
            {
                "default" => 1,
                "programming" | "program" => 2,
                "extended" => 3,
                "safety" => 4,
                value => parse_u8(value)?,
            };
            vec![Sid::DiagnosticSessionControl.as_value(), session]
        }
        "reset" => {
            let reset = match fields
                .next()
                .unwrap_or("hard")
                .to_ascii_lowercase()
                .as_str()
            {
                "hard" => 1,
                "keyoff" | "key-off" => 2,
                "soft" => 3,
                value => parse_u8(value)?,
            };
            vec![Sid::ECUReset.as_value(), reset]
        }
        "read" => {
            let identifiers = fields.map(parse_u16).collect::<Result<Vec<_>, _>>()?;
            if identifiers.is_empty() {
                return Err("read requires at least one 16-bit DID".to_owned());
            }
            let mut bytes = vec![Sid::ReadDataByIdentifier.as_value()];
            for identifier in identifiers {
                bytes.extend_from_slice(&identifier.to_be_bytes());
            }
            bytes
        }
        "write" => {
            let identifier = parse_u16(
                fields
                    .next()
                    .ok_or_else(|| "write requires a 16-bit DID".to_owned())?,
            )?;
            let mut bytes = vec![Sid::WriteDataByIdentifier.as_value()];
            bytes.extend_from_slice(&identifier.to_be_bytes());
            bytes.extend(parse_hex_fields(fields)?);
            bytes
        }
        "routine" => {
            let action = match fields
                .next()
                .unwrap_or("start")
                .to_ascii_lowercase()
                .as_str()
            {
                "start" => 1,
                "stop" => 2,
                "result" | "results" => 3,
                value => parse_u8(value)?,
            };
            let identifier = parse_u16(
                fields
                    .next()
                    .ok_or_else(|| "routine requires a 16-bit routine ID".to_owned())?,
            )?;
            let mut bytes = vec![Sid::RoutineControl.as_value(), action];
            bytes.extend_from_slice(&identifier.to_be_bytes());
            bytes.extend(parse_hex_fields(fields)?);
            bytes
        }
        "tester" | "present" => vec![Sid::TesterPresent.as_value(), 0],
        _ => parse_hex(input)?,
    };
    let (summary, mut details) = analyze_uds(&bytes)?;
    details.insert(0, format!("PDU: {}", hex_data(&bytes)));
    Ok(ProtocolRecord {
        protocol: ProtocolKind::Uds,
        input: input.to_owned(),
        bytes,
        response: None,
        summary,
        details,
    })
}

fn analyze_uds(bytes: &[u8]) -> Result<(String, Vec<String>), String> {
    let first = *bytes.first().ok_or_else(|| "UDS PDU is empty".to_owned())?;
    if first == 0x7f {
        let mut base = RespBase::default();
        let mut offset = 0;
        base.initialize(bytes, &mut offset)
            .map_err(|error| error.to_string())?;
        let service = Sid::from_value(base.service_id)
            .map(Sid::name)
            .unwrap_or("UnknownService");
        let code = NegRespCode::from_value(base.error_code)
            .map(NegRespCode::name)
            .unwrap_or("UnknownNRC");
        return Ok((
            format!("Negative response · {service} · {code}"),
            vec![
                format!("Requested SID: 0x{:02X} ({service})", base.service_id),
                format!("NRC: 0x{:02X} ({code})", base.error_code),
            ],
        ));
    }
    if first >= 0x40 {
        let mut base = RespBase::default();
        let mut offset = 0;
        base.initialize(bytes, &mut offset)
            .map_err(|error| error.to_string())?;
        let service = base
            .service_id_enum()
            .map(Sid::name)
            .unwrap_or("UnknownService");
        return Ok((
            format!("Positive response · {service}"),
            vec![
                format!("Response SID: 0x{first:02X}"),
                format!("Request SID: 0x{:02X}", base.service_id),
                format!("Payload bytes: {}", bytes.len().saturating_sub(1)),
            ],
        ));
    }
    let service = Sid::from_value(first)
        .map(Sid::name)
        .unwrap_or("UnknownService");
    let mut details = vec![
        format!("Request SID: 0x{first:02X} ({service})"),
        format!("Payload bytes: {}", bytes.len().saturating_sub(1)),
    ];
    if let Some(subfunction) = bytes
        .get(1)
        .filter(|_| Sid::from_value(first).is_some_and(autors_diag::uds::has_subfunction))
    {
        details.push(format!(
            "Subfunction: 0x{:02X}{}",
            subfunction & 0x7f,
            if subfunction & 0x80 != 0 {
                " (suppress positive response)"
            } else {
                ""
            }
        ));
    }
    Ok((format!("Request · {service}"), details))
}

fn doip_record(input: &str) -> Result<ProtocolRecord, String> {
    let mut fields = input.split_whitespace();
    let command = fields.next().unwrap_or_default();
    let bytes = if command.eq_ignore_ascii_case("diag") {
        let source = parse_u16(
            fields
                .next()
                .ok_or_else(|| "diag requires a source address".to_owned())?,
        )?;
        let target = parse_u16(
            fields
                .next()
                .ok_or_else(|| "diag requires a target address".to_owned())?,
        )?;
        let pdu = parse_hex_fields(fields)?;
        if pdu.is_empty() {
            return Err("diag requires a UDS/KWP payload".to_owned());
        }
        SrcDstFrame::new(
            ProtocolVersion::Iso13400_2019,
            DoIpType::DiagnosticMessage,
            source,
            target,
            SocketType::Stream,
            None,
            pdu,
            true,
        )
        .to_array()
    } else {
        parse_hex(input)?
    };
    let Some((frame, consumed)) =
        DoIpFrame::decode(&bytes, SocketType::Stream, None).map_err(|error| error.to_string())?
    else {
        return Err(format!(
            "DoIP frame is incomplete ({} byte(s) supplied)",
            bytes.len()
        ));
    };
    let base = frame.base();
    let message_type = base
        .msg_type()
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| format!("OEM 0x{:04X}", base.msg_type));
    let mut details = vec![
        format!("Frame: {}", doip_display(&frame)),
        format!("Wire bytes: {}", hex_data(&bytes)),
        format!("Protocol: {:?}", base.version),
        format!("Payload type: 0x{:04X} ({message_type})", base.msg_type),
        format!("Payload bytes: {}", base.data.len()),
        format!("Consumed bytes: {consumed}"),
    ];
    if base.msg_type() == Some(DoIpType::DiagnosticMessage) && !base.data.is_empty() {
        if let Ok((summary, uds_details)) = analyze_uds(&base.data) {
            details.push(String::new());
            details.push(format!("Embedded {summary}"));
            details.extend(uds_details);
        }
    }
    Ok(ProtocolRecord {
        protocol: ProtocolKind::DoIp,
        input: input.to_owned(),
        bytes,
        response: None,
        summary: format!("{message_type} · {} payload byte(s)", base.data.len()),
        details,
    })
}

fn ccp_record(input: &str, counter: u8) -> Result<ProtocolRecord, String> {
    let mut fields = input.split_whitespace();
    let command = fields.next().unwrap_or_default().to_ascii_lowercase();
    let (bytes, is_master) = match command.as_str() {
        "connect" => (
            CcpCmdConnect::new(parse_u16(fields.next().unwrap_or("0"))?).encode(counter, false),
            true,
        ),
        "status" => (
            CcpCmdBase::new(CcpCommandCode::GetSStatus).encode(counter, false),
            true,
        ),
        "version" => {
            let major = fields.next().map(parse_u8).transpose()?.unwrap_or(2);
            let release = fields.next().map(parse_u8).transpose()?.unwrap_or(1);
            (
                CmdGetCcpVersion::new(major, release).encode(counter, false),
                true,
            )
        }
        "setmta" => {
            let number = parse_u8(
                fields
                    .next()
                    .ok_or_else(|| "setmta requires an MTA number".to_owned())?,
            )?;
            let extension = parse_u8(
                fields
                    .next()
                    .ok_or_else(|| "setmta requires an address extension".to_owned())?,
            )?;
            let address = parse_u32(
                fields
                    .next()
                    .ok_or_else(|| "setmta requires an address".to_owned())?,
            )?;
            (
                CcpCmdSetMta::new(number, extension, address).encode(counter, false),
                true,
            )
        }
        "upload" => (
            CcpCmdUpload::new(parse_u8(
                fields
                    .next()
                    .ok_or_else(|| "upload requires a byte count".to_owned())?,
            )?)
            .encode(counter, false),
            true,
        ),
        "rx" => (parse_hex_fields(fields)?, false),
        _ => (parse_hex(input)?, true),
    };
    if bytes.is_empty() {
        return Err("CCP frame is empty".to_owned());
    }
    let frame = CcpFrame::new("ProtocolLab", 0x600, bytes.clone(), is_master);
    let summary = frame.type_str();
    let details = vec![
        format!(
            "Direction: {}",
            if is_master {
                "CRO request"
            } else {
                "DTO response"
            }
        ),
        format!("Type: {summary}"),
        format!("Counter: {}", frame.ctr()),
        format!("Payload: {}", hex_data(&bytes)),
        format!("Error response: {}", frame.is_error()),
        format!("DAQ packet: {}", frame.is_daq()),
    ];
    Ok(ProtocolRecord {
        protocol: ProtocolKind::Ccp,
        input: input.to_owned(),
        bytes,
        response: None,
        summary,
        details,
    })
}

fn xcp_record(input: &str) -> Result<ProtocolRecord, String> {
    let mut fields = input.split_whitespace();
    let command = fields.next().unwrap_or_default().to_ascii_lowercase();
    let (bytes, is_master) = match command.as_str() {
        "connect" => (
            XcpCmdConnect {
                mode: ConnectMode::Normal,
            }
            .encode(false),
            true,
        ),
        "disconnect" => (XcpCmdDisconnect.encode(false), true),
        "status" => (
            XcpCmdBare::new(XcpCommandCode::GetStatus).encode(false),
            true,
        ),
        "setmta" => {
            let extension = parse_u8(
                fields
                    .next()
                    .ok_or_else(|| "setmta requires an address extension".to_owned())?,
            )?;
            let address = parse_u32(
                fields
                    .next()
                    .ok_or_else(|| "setmta requires an address".to_owned())?,
            )?;
            (
                XcpCmdSetMta {
                    address_extension: extension,
                    address,
                }
                .encode(false),
                true,
            )
        }
        "upload" => (
            XcpCmdUpload {
                number_of_elements: parse_u8(
                    fields
                        .next()
                        .ok_or_else(|| "upload requires an element count".to_owned())?,
                )?,
            }
            .encode(false),
            true,
        ),
        "rx" => (parse_hex_fields(fields)?, false),
        _ => (parse_hex(input)?, true),
    };
    if bytes.is_empty() {
        return Err("XCP frame is empty".to_owned());
    }
    let frame = XcpFrame::new(
        XcpType::Can,
        "ProtocolLab",
        &bytes,
        is_master,
        XcpHeaderLen::NotSet,
    );
    let summary = frame.type_str();
    let details = vec![
        format!(
            "Direction: {}",
            if is_master {
                "CTO request"
            } else {
                "response / DTO"
            }
        ),
        format!("Type: {summary}"),
        format!("Transport: {}", frame.type_),
        format!("Payload: {}", hex_data(frame.data())),
        format!("Error response: {}", frame.is_error()),
        format!("DAQ packet: {}", frame.is_daq()),
    ];
    Ok(ProtocolRecord {
        protocol: ProtocolKind::Xcp,
        input: input.to_owned(),
        bytes,
        response: None,
        summary,
        details,
    })
}

fn doip_display(frame: &DoIpFrame) -> String {
    match frame {
        DoIpFrame::Base(frame) => frame.to_string(),
        DoIpFrame::Src(frame) => frame.to_string(),
        DoIpFrame::SrcDst(frame) => frame.to_string(),
    }
}

fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    parse_hex_fields(input.split_whitespace())
}

fn parse_hex_fields<'a>(fields: impl Iterator<Item = &'a str>) -> Result<Vec<u8>, String> {
    fields.map(parse_u8).collect()
}

fn parse_u8(value: &str) -> Result<u8, String> {
    u8::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid hexadecimal byte {value:?}"))
}

fn parse_u16(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid hexadecimal 16-bit value {value:?}"))
}

fn parse_u32(value: &str) -> Result<u32, String> {
    u32::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid hexadecimal 32-bit value {value:?}"))
}

pub fn hex_data(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_decodes_typed_uds_commands_and_responses() {
        let request = uds_record("read F190").unwrap();
        assert_eq!(request.bytes, [0x22, 0xf1, 0x90]);
        assert_eq!(request.summary, "Request · ReadDataByIdentifier");
        let negative = uds_record("7F 22 31").unwrap();
        assert!(negative.summary.contains("RequestOutOfRange"));
        let positive = uds_record("62 F1 90 56 49 4E").unwrap();
        assert!(positive.summary.contains("Positive response"));
    }

    #[test]
    fn wraps_and_decodes_doip_diagnostic_messages() {
        let record = doip_record("diag 0E80 1000 22 F1 90").unwrap();
        assert_eq!(&record.bytes[..4], &[0x03, 0xfc, 0x80, 0x01]);
        assert!(record.summary.contains("DiagnosticMessage"));
        assert!(record
            .details
            .iter()
            .any(|line| line.contains("ReadDataByIdentifier")));
    }

    #[test]
    fn uses_ccp_and_xcp_typed_command_codecs() {
        let ccp = ccp_record("connect 1234", 7).unwrap();
        assert_eq!(ccp.bytes, [0x01, 0x07, 0x34, 0x12]);
        assert_eq!(ccp.summary, "Connect");
        let xcp = xcp_record("setmta 01 12345678").unwrap();
        assert_eq!(xcp.bytes, [0xf6, 0, 0, 1, 0x78, 0x56, 0x34, 0x12]);
        assert_eq!(xcp.summary, "SetMTA");
    }

    #[test]
    fn lab_tracks_protocol_selection_and_bounded_history() {
        let mut lab = ProtocolLab::new();
        lab.submit("tester").unwrap();
        lab.set_protocol(3);
        lab.submit("connect").unwrap();
        assert_eq!(lab.records().len(), 2);
        assert_eq!(lab.selected().unwrap().protocol, ProtocolKind::Xcp);
        lab.clear();
        assert!(lab.records().is_empty());
    }

    #[test]
    fn configures_can_transports_and_records_live_response_details() {
        let mut lab = ProtocolLab::new();
        lab.configure_transport("7DF 7E8 fd").unwrap();
        assert_eq!(
            lab.can_transport(),
            Some(CanTransportConfig {
                command_id: 0x7df,
                response_id: 0x7e8,
                use_can_fd: true,
            })
        );
        let request = lab.prepare_live_request("read F190").unwrap();
        let record =
            lab.record_live_exchange(request, vec![0x62, 0xf1, 0x90, 0x12, 0x34], "Success");
        assert!(record.summary.contains("Positive response"));
        assert!(record
            .details
            .iter()
            .any(|line| line.contains("62 F1 90 12 34")));
        assert!(lab.configure_transport("7DF 7E8 invalid").is_err());
    }

    #[test]
    fn parses_doip_transport_with_default_and_explicit_ports() {
        let default = parse_doip_transport("192.0.2.1 0.0.0.0 0E80 1000").unwrap();
        assert_eq!(default.remote.port(), DEFAULT_PORT);
        let explicit = parse_doip_transport("[2001:db8::1]:14000 :: 0E80 1000 2500").unwrap();
        assert_eq!(explicit.remote.port(), 14_000);
        assert_eq!(explicit.p2_ms, 2_500);
    }
}
