use std::path::{Path, PathBuf};
use std::time::Duration;

use autors_a2l::Project;
use autors_dbc::dbc::DBCFile;
use autors_ldf::model::Ldf;
use autors_prm::prm::PrmFile;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::a2l_lab::A2lLab;
use crate::bus::{BusSession, RawCanProtocol};
use crate::catalog::{Capability, CapabilityCatalog, CapabilityGroup};
use crate::document::Document;
use crate::lin_bus::LinBusSession;
use crate::odx_lab::{OdxLab, OdxTransport};
use crate::prm_lab::PrmLab;
use crate::protocol::{ProtocolKind, ProtocolLab};
use crate::symbol_lab::SymbolLab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Open,
    Search,
    SendCan,
    InjectCan,
    ConfigureCan,
    SetCanPayload,
    SetCanPeriod,
    SaveCanTrace,
    SendLin,
    InjectLin,
    ConfigureLin,
    SetLinPayload,
    SaveLinTrace,
    SetA2lValue,
    SaveA2lMdf,
    ProtocolCommand,
    ConfigureProtocolTransport,
    LiveProtocolCommand,
    OdxServicePayload,
    LiveOdxServicePayload,
    ConfigureSymbolMultiplier,
    SaveSymbolUpdates,
}

#[derive(Debug, Clone, Copy)]
pub struct Playback {
    pub active: bool,
    pub speed: f64,
    pub position: f64,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            active: false,
            speed: 1.0,
            position: 0.0,
        }
    }
}

pub struct App {
    pub catalog: CapabilityCatalog,
    pub group_index: usize,
    pub list_index: usize,
    pub network: Option<DBCFile>,
    pub a2l_project: Option<Project>,
    pub a2l_path: Option<PathBuf>,
    pub document: Option<Document>,
    pub bus: BusSession,
    pub show_bus: bool,
    pub bus_index: usize,
    pub lin_bus: LinBusSession,
    pub show_lin_bus: bool,
    pub lin_bus_index: usize,
    pub lin_schedule_index: usize,
    pub protocol_lab: ProtocolLab,
    pub show_protocol_lab: bool,
    pub odx_lab: OdxLab,
    pub show_odx_lab: bool,
    pub a2l_lab: A2lLab,
    pub show_a2l_lab: bool,
    pub a2l_index: usize,
    pub prm_lab: PrmLab,
    pub show_prm_lab: bool,
    pub symbol_lab: SymbolLab,
    pub show_symbol_lab: bool,
    pub row_index: usize,
    pub playback: Playback,
    pub query: String,
    pub prompt: Option<PromptKind>,
    pub input: String,
    pub status: String,
    pub show_help: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(catalog: CapabilityCatalog) -> Self {
        let count = catalog.capabilities.len();
        Self {
            catalog,
            group_index: 0,
            list_index: 0,
            network: None,
            a2l_project: None,
            a2l_path: None,
            document: None,
            bus: BusSession::new(),
            show_bus: false,
            bus_index: 0,
            lin_bus: LinBusSession::new(),
            show_lin_bus: false,
            lin_bus_index: 0,
            lin_schedule_index: 0,
            protocol_lab: ProtocolLab::new(),
            show_protocol_lab: false,
            odx_lab: OdxLab::new(),
            show_odx_lab: false,
            a2l_lab: A2lLab::new(),
            show_a2l_lab: false,
            a2l_index: 0,
            prm_lab: PrmLab::new(),
            show_prm_lab: false,
            symbol_lab: SymbolLab::new(),
            show_symbol_lab: false,
            row_index: 0,
            playback: Playback::default(),
            query: String::new(),
            prompt: None,
            input: String::new(),
            status: format!("Discovered {count} autors crates"),
            show_help: false,
            should_quit: false,
        }
    }

    pub fn group(&self) -> CapabilityGroup {
        CapabilityGroup::ALL[self.group_index]
    }

    pub fn visible_capability_indices(&self) -> Vec<usize> {
        self.catalog.in_group(self.group(), &self.query)
    }

    pub fn selected_capability(&self) -> Option<&Capability> {
        self.visible_capability_indices()
            .get(self.list_index)
            .and_then(|index| self.catalog.capabilities.get(*index))
    }

    pub fn visible_row_indices(&self) -> Vec<usize> {
        self.document
            .as_ref()
            .map(|document| document.matching_rows(&self.query))
            .unwrap_or_default()
    }

    pub fn selected_row(&self) -> Option<usize> {
        self.visible_row_indices().get(self.row_index).copied()
    }

    pub fn open_document(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        match Document::open(path) {
            Ok(mut document) => {
                let rows = document.rows.len();
                if let Some(database) = document.database.clone() {
                    let messages = database.messages().count();
                    let scheduler_status = self
                        .bus
                        .attach_database(&database)
                        .map(|()| "remaining-bus scheduler ready".to_owned())
                        .unwrap_or_else(|error| format!("scheduler unavailable: {error}"));
                    self.network = Some(database);
                    self.status = format!(
                        "Opened {} · loaded {messages} DBC message(s) · {scheduler_status}",
                        document.path.display(),
                    );
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("ldf"))
                {
                    match Ldf::read(path) {
                        Ok(database) => {
                            let frames = database.unconditional_frames.len();
                            let schedules = database.schedule_tables.len();
                            let scheduler_status = self
                                .lin_bus
                                .attach_database(&database)
                                .map(|()| "LIN scheduler ready".to_owned())
                                .unwrap_or_else(|error| {
                                    format!("LIN scheduler unavailable: {error}")
                                });
                            self.status = format!(
                                "Opened {} · {frames} LIN frame(s) · {schedules} schedule(s) · {scheduler_status}",
                                document.path.display()
                            );
                        }
                        Err(error) => {
                            self.status = format!(
                                "Opened {} · LIN session unavailable: {error}",
                                document.path.display()
                            );
                        }
                    }
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("a2l"))
                {
                    match Project::parse_file(path) {
                        Ok(project) => {
                            self.a2l_project = Some(project.clone());
                            self.a2l_path = Some(path.to_owned());
                            match self.a2l_lab.attach_project(&project) {
                                Ok(()) => {
                                    self.a2l_index = 0;
                                    self.status = format!(
                                        "Opened {} · {} measurements · {} calibrations · DAQ ready",
                                        document.path.display(),
                                        self.a2l_lab.measurement_count(),
                                        self.a2l_lab.calibration_count()
                                    );
                                }
                                Err(error) => {
                                    self.status = format!(
                                        "Opened {} · A2L workbench unavailable: {error}",
                                        document.path.display()
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            self.status = format!(
                                "Opened {} · A2L workbench unavailable: {error}",
                                document.path.display()
                            );
                        }
                    }
                } else if is_odx_file(path) {
                    match self.odx_lab.attach_path(path) {
                        Ok(services) => {
                            self.status = format!(
                                "Opened {} · {} ODX variant(s) · {services} executable service row(s)",
                                document.path.display(),
                                self.odx_lab.variants.len()
                            );
                        }
                        Err(error) => {
                            self.status = format!(
                                "Opened {} · ODX diagnostic session unavailable: {error}",
                                document.path.display()
                            );
                        }
                    }
                } else if is_symbol_file(path) {
                    if let Some(project) = &self.a2l_project {
                        match self.symbol_lab.attach_source(path, project) {
                            Ok(count) => {
                                self.status = format!(
                                    "Opened {} · {count} A2L symbol match candidate(s) · sync lab ready",
                                    document.path.display()
                                );
                            }
                            Err(error) => {
                                self.status = format!(
                                    "Opened {} · A2L symbol matching failed: {error}",
                                    document.path.display()
                                );
                            }
                        }
                    } else {
                        self.status = format!(
                            "Opened {} · open the target A2L first to compute address updates",
                            document.path.display()
                        );
                    }
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("prm"))
                {
                    match PrmFile::open(path) {
                        Ok(project) => {
                            let entries = project.cmdsets.len();
                            let procedures = project.procedures.len();
                            let mode = project.mode;
                            self.prm_lab.attach_project(&project);
                            self.status = format!(
                                "Opened {} · {mode} flash · {entries} entries · {procedures} procedures · dry-run ready",
                                document.path.display()
                            );
                        }
                        Err(error) => {
                            self.status = format!(
                                "Opened {} · PRM execution unavailable: {error}",
                                document.path.display()
                            );
                        }
                    }
                } else if is_conservation_file(path) {
                    if let Some(project) = &self.a2l_project {
                        match Document::open_conservation(path, project) {
                            Ok(native) => {
                                document = native;
                                self.status = format!(
                                    "Opened {} · {} imported calibration object(s)",
                                    document.path.display(),
                                    document.rows.len()
                                );
                            }
                            Err(error) => {
                                self.status = format!(
                                    "Opened {} · calibration import failed: {error}",
                                    document.path.display()
                                );
                            }
                        }
                    } else {
                        self.status = format!(
                            "Opened {} as text · open its A2L first for typed DCM/PAR/MATLAB values",
                            document.path.display()
                        );
                    }
                } else {
                    let decoded = self
                        .network
                        .as_ref()
                        .map(|database| document.apply_dbc(database))
                        .unwrap_or_default();
                    self.status = if decoded == 0 {
                        format!("Opened {} · {rows} row(s)", document.path.display())
                    } else {
                        format!(
                            "Opened {} · {rows} row(s) · {decoded} DBC match(es)",
                            document.path.display()
                        )
                    };
                }
                self.document = Some(document);
                self.row_index = 0;
                self.playback = Playback::default();
                self.query.clear();
            }
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Some(prompt) = self.prompt {
            self.handle_prompt(key, prompt);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.show_bus {
            self.handle_bus_key(key);
            return;
        }
        if self.show_lin_bus {
            self.handle_lin_bus_key(key);
            return;
        }
        if self.show_protocol_lab {
            self.handle_protocol_key(key);
            return;
        }
        if self.show_odx_lab {
            self.handle_odx_key(key);
            return;
        }
        if self.show_a2l_lab {
            self.handle_a2l_key(key);
            return;
        }
        if self.show_prm_lab {
            self.handle_prm_key(key);
            return;
        }
        if self.show_symbol_lab {
            self.handle_symbol_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('o') => self.begin_prompt(PromptKind::Open),
            KeyCode::Char('b') => {
                self.show_bus = true;
                self.status = "CAN workbench".to_owned();
            }
            KeyCode::Char('n') => {
                self.show_lin_bus = true;
                self.status = "LIN workbench".to_owned();
            }
            KeyCode::Char('g') => {
                self.show_protocol_lab = true;
                self.status = "Diagnostic and calibration protocol lab".to_owned();
            }
            KeyCode::Char('d') => {
                self.show_odx_lab = true;
                self.status = if self.odx_lab.is_loaded() {
                    "ODX database-driven diagnostic workbench".to_owned()
                } else {
                    "Open an ODX diagnostic file to populate the service workbench".to_owned()
                };
            }
            KeyCode::Char('a') => {
                self.show_a2l_lab = true;
                self.status = if self.a2l_lab.is_loaded() {
                    "A2L measurement and calibration workbench".to_owned()
                } else {
                    "Open an A2L project to populate the measurement workbench".to_owned()
                };
            }
            KeyCode::Char('f') => {
                self.show_prm_lab = true;
                self.status = if self.prm_lab.is_loaded() {
                    "PRM flash procedure preflight workbench".to_owned()
                } else {
                    "Open a PRM project to populate the flash workbench".to_owned()
                };
            }
            KeyCode::Char('y') => {
                self.show_symbol_lab = true;
                self.status = if self.symbol_lab.is_loaded() {
                    "A2L symbol synchronization workbench".to_owned()
                } else {
                    "Open an A2L followed by an ELF/AXF or MAP file".to_owned()
                };
            }
            KeyCode::Char('/') => self.begin_prompt(PromptKind::Search),
            KeyCode::Char(' ') => self.toggle_playback(),
            KeyCode::Char('r') if self.document.as_ref().is_some_and(Document::is_trace) => {
                self.reset_playback()
            }
            KeyCode::Char('[') if self.document.as_ref().is_some_and(Document::is_trace) => {
                self.change_playback_speed(0.5)
            }
            KeyCode::Char(']') if self.document.as_ref().is_some_and(Document::is_trace) => {
                self.change_playback_speed(2.0)
            }
            KeyCode::Esc => self.escape(),
            KeyCode::Left | KeyCode::Char('h') if self.document.is_none() => self.change_group(-1),
            KeyCode::Right | KeyCode::Char('l') if self.document.is_none() => self.change_group(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.set_selection(0),
            KeyCode::End => self.set_selection(usize::MAX),
            KeyCode::Enter if self.document.is_none() => self.open_selected_readme(),
            KeyCode::F(5) if self.document.is_none() => self.reload_catalog(),
            _ => {}
        }
    }

    pub fn advance_playback(&mut self, elapsed: Duration) {
        self.bus.advance(elapsed);
        self.lin_bus.advance(elapsed);
        self.a2l_lab.advance(elapsed);
        if self.show_bus {
            if let Some(error) = &self.bus.last_error {
                self.status = format!("Bus error: {error}");
            }
        } else if self.show_lin_bus {
            if let Some(error) = &self.lin_bus.last_error {
                self.status = format!("LIN bus error: {error}");
            }
        } else if self.show_a2l_lab {
            if let Some(error) = &self.a2l_lab.last_error {
                self.status = format!("A2L DAQ error: {error}");
            }
        }
        if !self.playback.active {
            return;
        }
        let visible = self.visible_row_indices();
        let Some(document) = &self.document else {
            self.playback.active = false;
            return;
        };
        let Some(last_index) = visible.last().copied() else {
            self.playback.active = false;
            return;
        };
        self.playback.position += elapsed.as_secs_f64() * self.playback.speed;
        let mut selected = 0usize;
        for (position, index) in visible.iter().enumerate() {
            if document
                .row_timestamp(*index)
                .is_some_and(|timestamp| timestamp <= self.playback.position)
            {
                selected = position;
            } else {
                break;
            }
        }
        self.row_index = selected;
        if document
            .row_timestamp(last_index)
            .is_some_and(|timestamp| self.playback.position >= timestamp)
        {
            self.row_index = visible.len() - 1;
            self.playback.active = false;
            self.status = "Trace playback reached the end".to_owned();
        }
    }

    fn begin_prompt(&mut self, prompt: PromptKind) {
        self.prompt = Some(prompt);
        self.input = match prompt {
            PromptKind::Search => self.query.clone(),
            PromptKind::ConfigureCan => {
                format!("{} {}", self.bus.channel(), self.bus.hardware_type())
            }
            PromptKind::ConfigureLin => format!(
                "{} {}",
                self.lin_bus.channel(),
                self.lin_bus.hardware_type()
            ),
            PromptKind::SaveCanTrace => self
                .catalog
                .workspace_root
                .join("autors-live.asc")
                .display()
                .to_string(),
            PromptKind::SaveLinTrace => self
                .catalog
                .workspace_root
                .join("autors-live.ltrc")
                .display()
                .to_string(),
            PromptKind::SetA2lValue => String::new(),
            PromptKind::SaveA2lMdf => self
                .catalog
                .workspace_root
                .join("autors-daq.mdf")
                .display()
                .to_string(),
            PromptKind::ConfigureProtocolTransport => self.protocol_lab.transport_input(),
            PromptKind::ConfigureSymbolMultiplier => self.symbol_lab.address_multiplier.to_string(),
            PromptKind::SaveSymbolUpdates => self
                .a2l_path
                .as_deref()
                .map(suggest_symbol_output)
                .unwrap_or_default()
                .display()
                .to_string(),
            _ => String::new(),
        };
    }

    fn handle_prompt(&mut self, key: KeyEvent, prompt: PromptKind) {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                self.input.clear();
            }
            KeyCode::Enter => {
                let value = std::mem::take(&mut self.input);
                self.prompt = None;
                match prompt {
                    PromptKind::Open if !value.trim().is_empty() => {
                        self.open_document(resolve_input_path(
                            &value,
                            &self.catalog.workspace_root,
                        ));
                    }
                    PromptKind::Search => {
                        self.query = value;
                        self.set_selection(0);
                        self.status = if self.query.is_empty() {
                            "Search cleared".to_owned()
                        } else {
                            format!("Filtering by {:?}", self.query)
                        };
                    }
                    PromptKind::SendCan if !value.trim().is_empty() => {
                        match self.bus.send_text(&value) {
                            Ok(bytes) => {
                                self.status = format!("Transmitted {bytes} CAN payload byte(s)")
                            }
                            Err(error) => self.status = format!("CAN send failed: {error}"),
                        }
                    }
                    PromptKind::InjectCan if !value.trim().is_empty() => {
                        match self.bus.inject_text(&value) {
                            Ok(bytes) => {
                                self.status = format!("Injected {bytes} received CAN byte(s)")
                            }
                            Err(error) => self.status = format!("CAN injection failed: {error}"),
                        }
                    }
                    PromptKind::ConfigureCan if !value.trim().is_empty() => {
                        match self.bus.configure_adapter(&value) {
                            Ok(()) => {
                                self.status = format!(
                                    "CAN uses {} channel {} (hardware type {})",
                                    self.bus.adapter_name(),
                                    self.bus.channel(),
                                    self.bus.hardware_type()
                                )
                            }
                            Err(error) => {
                                self.status = format!("CAN configuration failed: {error}")
                            }
                        }
                    }
                    PromptKind::SetCanPayload => {
                        let Some(message) = self.bus.messages().get(self.bus_index).cloned() else {
                            self.status = "No scheduled CAN message selected".to_owned();
                            return;
                        };
                        match self.bus.set_payload_text(message.id, &value) {
                            Ok(()) => self.status = format!("Updated payload for {}", message.name),
                            Err(error) => self.status = format!("Payload update failed: {error}"),
                        }
                    }
                    PromptKind::SetCanPeriod if !value.trim().is_empty() => {
                        let Some(message) = self.bus.messages().get(self.bus_index).cloned() else {
                            self.status = "No scheduled CAN message selected".to_owned();
                            return;
                        };
                        match self.bus.set_period_text(message.id, &value) {
                            Ok(()) => self.status = format!("Updated cycle for {}", message.name),
                            Err(error) => self.status = format!("Cycle update failed: {error}"),
                        }
                    }
                    PromptKind::SaveCanTrace if !value.trim().is_empty() => {
                        let path = resolve_input_path(&value, &self.catalog.workspace_root);
                        match self.bus.save_asc(&path) {
                            Ok(count) => {
                                self.status = format!(
                                    "Saved {count} CAN trace record(s) to {}",
                                    path.display()
                                )
                            }
                            Err(error) => self.status = format!("ASC export failed: {error}"),
                        }
                    }
                    PromptKind::SendLin if !value.trim().is_empty() => {
                        match self.lin_bus.send_text(&value) {
                            Ok(bytes) => {
                                self.status = format!("Transmitted {bytes} LIN payload byte(s)")
                            }
                            Err(error) => self.status = format!("LIN send failed: {error}"),
                        }
                    }
                    PromptKind::InjectLin if !value.trim().is_empty() => {
                        match self.lin_bus.inject_text(&value) {
                            Ok(bytes) => {
                                self.status =
                                    format!("Injected {bytes} received LIN payload byte(s)")
                            }
                            Err(error) => self.status = format!("LIN injection failed: {error}"),
                        }
                    }
                    PromptKind::ConfigureLin if !value.trim().is_empty() => {
                        match self.lin_bus.configure_adapter(&value) {
                            Ok(()) => {
                                self.status = format!(
                                    "LIN uses {} channel {} (hardware type {})",
                                    self.lin_bus.adapter_name(),
                                    self.lin_bus.channel(),
                                    self.lin_bus.hardware_type()
                                )
                            }
                            Err(error) => {
                                self.status = format!("LIN configuration failed: {error}")
                            }
                        }
                    }
                    PromptKind::SetLinPayload => {
                        let Some(frame) = self.lin_bus.frames().get(self.lin_bus_index).cloned()
                        else {
                            self.status = "No scheduled LIN frame selected".to_owned();
                            return;
                        };
                        match self.lin_bus.set_payload_text(&frame.name, &value) {
                            Ok(()) => self.status = format!("Updated payload for {}", frame.name),
                            Err(error) => {
                                self.status = format!("LIN payload update failed: {error}")
                            }
                        }
                    }
                    PromptKind::SaveLinTrace if !value.trim().is_empty() => {
                        let path = resolve_input_path(&value, &self.catalog.workspace_root);
                        match self.lin_bus.save_ltrc(&path) {
                            Ok(count) => {
                                self.status = format!(
                                    "Saved {count} LIN trace record(s) to {}",
                                    path.display()
                                )
                            }
                            Err(error) => self.status = format!("LTRC export failed: {error}"),
                        }
                    }
                    PromptKind::SetA2lValue if !value.trim().is_empty() => {
                        let Some(index) = self.selected_a2l_object() else {
                            self.status = "No A2L object selected".to_owned();
                            return;
                        };
                        match self.a2l_lab.set_value_text(index, &value) {
                            Ok(value) => {
                                self.status = format!("Updated virtual ECU value to {value}")
                            }
                            Err(error) => self.status = format!("A2L value update failed: {error}"),
                        }
                    }
                    PromptKind::SaveA2lMdf if !value.trim().is_empty() => {
                        let path = resolve_input_path(&value, &self.catalog.workspace_root);
                        match self.a2l_lab.save_mdf(&path) {
                            Ok(samples) => {
                                self.status = format!(
                                    "Saved {samples} physical DAQ sample(s) to {}",
                                    path.display()
                                )
                            }
                            Err(error) => self.status = format!("MDF export failed: {error}"),
                        }
                    }
                    PromptKind::ProtocolCommand if !value.trim().is_empty() => {
                        match self.protocol_lab.submit(&value) {
                            Ok(record) => {
                                self.status =
                                    format!("{} · {} byte(s)", record.summary, record.bytes.len())
                            }
                            Err(error) => self.status = format!("Protocol input failed: {error}"),
                        }
                    }
                    PromptKind::ConfigureProtocolTransport if !value.trim().is_empty() => {
                        match self.protocol_lab.configure_transport(&value) {
                            Ok(status) => self.status = status,
                            Err(error) => {
                                self.status = format!("Transport configuration failed: {error}")
                            }
                        }
                    }
                    PromptKind::LiveProtocolCommand if !value.trim().is_empty() => {
                        self.execute_live_protocol(&value);
                    }
                    PromptKind::OdxServicePayload => match self.odx_lab.compose_selected(&value) {
                        Ok(request) => {
                            self.protocol_lab
                                .set_protocol(match self.odx_lab.transport {
                                    OdxTransport::UdsCan => 0,
                                    OdxTransport::DoIp => 1,
                                });
                            match self
                                .protocol_lab
                                .submit(&crate::protocol::hex_data(&request))
                            {
                                Ok(record) => {
                                    self.status = format!(
                                        "ODX request encoded · {} · {} byte(s)",
                                        record.summary,
                                        request.len()
                                    )
                                }
                                Err(error) => {
                                    self.status = format!("ODX request encoding failed: {error}")
                                }
                            }
                        }
                        Err(error) => {
                            self.status = format!("ODX request composition failed: {error}")
                        }
                    },
                    PromptKind::LiveOdxServicePayload => {
                        match self.odx_lab.compose_selected(&value) {
                            Ok(request) => {
                                self.protocol_lab
                                    .set_protocol(match self.odx_lab.transport {
                                        OdxTransport::UdsCan => 0,
                                        OdxTransport::DoIp => 1,
                                    });
                                if let Some(response) =
                                    self.execute_live_protocol(&crate::protocol::hex_data(&request))
                                {
                                    self.odx_lab.record_response(response);
                                }
                            }
                            Err(error) => {
                                self.status = format!("ODX request composition failed: {error}")
                            }
                        }
                    }
                    PromptKind::ConfigureSymbolMultiplier if !value.trim().is_empty() => {
                        let Some(project) = &self.a2l_project else {
                            self.status = "Open the target A2L first".to_owned();
                            return;
                        };
                        match self.symbol_lab.set_multiplier_text(&value, project) {
                            Ok(count) => {
                                self.status = format!(
                                    "Address multiplier {} · {count} candidate(s)",
                                    self.symbol_lab.address_multiplier
                                )
                            }
                            Err(error) => {
                                self.status = format!("Symbol multiplier failed: {error}")
                            }
                        }
                    }
                    PromptKind::SaveSymbolUpdates if !value.trim().is_empty() => {
                        let target = resolve_input_path(&value, &self.catalog.workspace_root);
                        let Some(project) = &mut self.a2l_project else {
                            self.status = "Open the target A2L first".to_owned();
                            return;
                        };
                        match self.symbol_lab.apply(project, &target) {
                            Ok(updated) => {
                                let lab_status = self
                                    .a2l_lab
                                    .attach_project(project)
                                    .map(|()| "A2L session refreshed".to_owned())
                                    .unwrap_or_else(|error| {
                                        format!("A2L session refresh failed: {error}")
                                    });
                                let _ = self.symbol_lab.refresh(project);
                                self.status = format!(
                                    "Wrote {updated} address update(s) to {} · {lab_status}",
                                    target.display()
                                );
                            }
                            Err(error) => self.status = format!("A2L write failed: {error}"),
                        }
                    }
                    PromptKind::Open => {}
                    PromptKind::SendCan
                    | PromptKind::InjectCan
                    | PromptKind::ConfigureCan
                    | PromptKind::SetCanPeriod
                    | PromptKind::SaveCanTrace
                    | PromptKind::SendLin
                    | PromptKind::InjectLin
                    | PromptKind::ConfigureLin
                    | PromptKind::SaveLinTrace
                    | PromptKind::SetA2lValue
                    | PromptKind::SaveA2lMdf
                    | PromptKind::ProtocolCommand
                    | PromptKind::ConfigureProtocolTransport
                    | PromptKind::LiveProtocolCommand
                    | PromptKind::ConfigureSymbolMultiplier
                    | PromptKind::SaveSymbolUpdates => {}
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.input.push(character);
            }
            _ => {}
        }
    }

    fn escape(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.set_selection(0);
            self.status = "Search cleared".to_owned();
        } else if self.document.take().is_some() {
            self.row_index = 0;
            self.playback = Playback::default();
            self.status = "Returned to capability center".to_owned();
        }
    }

    fn change_group(&mut self, delta: isize) {
        let count = CapabilityGroup::ALL.len();
        self.group_index = self.group_index.saturating_add_signed(delta).min(count - 1);
        self.list_index = 0;
        self.query.clear();
        self.status = self.group().description().to_owned();
    }

    fn move_selection(&mut self, delta: isize) {
        let length = if self.document.is_some() {
            self.visible_row_indices().len()
        } else {
            self.visible_capability_indices().len()
        };
        if length == 0 {
            self.set_selection(0);
            return;
        }
        let current = if self.document.is_some() {
            self.row_index
        } else {
            self.list_index
        };
        self.set_selection(current.saturating_add_signed(delta).min(length - 1));
    }

    fn set_selection(&mut self, index: usize) {
        if self.document.is_some() {
            self.row_index = index.min(self.visible_row_indices().len().saturating_sub(1));
        } else {
            self.list_index = index.min(self.visible_capability_indices().len().saturating_sub(1));
        }
    }

    fn open_selected_readme(&mut self) {
        let Some(capability) = self.selected_capability() else {
            return;
        };
        let Some(directory) = capability.manifest_path.parent() else {
            return;
        };
        let readme = directory.join("README.md");
        if readme.is_file() {
            self.open_document(readme);
        } else {
            self.status = format!("{} has no README.md", capability.name);
        }
    }

    fn reload_catalog(&mut self) {
        let manifest = self.catalog.workspace_root.join("Cargo.toml");
        match CapabilityCatalog::load(&manifest) {
            Ok(catalog) => {
                let count = catalog.capabilities.len();
                self.catalog = catalog;
                self.set_selection(self.list_index);
                self.status = format!("Workspace refreshed · {count} autors crates");
            }
            Err(error) => self.status = format!("Refresh failed: {error}"),
        }
    }

    fn handle_bus_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('b') => {
                self.show_bus = false;
                self.status = "Returned from CAN workbench".to_owned();
            }
            KeyCode::Char('c') => {
                if self.bus.connected {
                    self.bus.disconnect();
                    self.status = "CAN channel disconnected".to_owned();
                } else {
                    match self.bus.connect() {
                        Ok(()) => {
                            self.status =
                                format!("{} CAN channel connected", self.bus.adapter_name())
                        }
                        Err(error) => self.status = format!("CAN connect failed: {error}"),
                    }
                }
            }
            KeyCode::Char('a') => match self.bus.cycle_adapter() {
                Ok(adapter) => self.status = format!("Selected {adapter} CAN adapter"),
                Err(error) => self.status = format!("CAN adapter selection failed: {error}"),
            },
            KeyCode::Char('e') => match self.bus.refresh_channels() {
                Ok(count) => {
                    self.status = format!(
                        "{} CAN channel(s) discovered for {}",
                        count,
                        self.bus.adapter_name()
                    )
                }
                Err(error) => self.status = format!("CAN enumeration failed: {error}"),
            },
            KeyCode::Char('v') => self.begin_prompt(PromptKind::ConfigureCan),
            KeyCode::Char(',') => match self.bus.cycle_channel(-1) {
                Ok(channel) => self.status = format!("Selected CAN {channel}"),
                Err(error) => self.status = format!("CAN channel selection failed: {error}"),
            },
            KeyCode::Char('.') => match self.bus.cycle_channel(1) {
                Ok(channel) => self.status = format!("Selected CAN {channel}"),
                Err(error) => self.status = format!("CAN channel selection failed: {error}"),
            },
            KeyCode::Char(' ') => {
                let running = !self.bus.running;
                match self.bus.set_running(running) {
                    Ok(()) => {
                        self.status = if running {
                            "Remaining-bus simulation running".to_owned()
                        } else {
                            "Remaining-bus simulation paused".to_owned()
                        }
                    }
                    Err(error) => self.status = format!("Simulation unavailable: {error}"),
                }
            }
            KeyCode::Char('s') => self.begin_prompt(PromptKind::SendCan),
            KeyCode::Char('i') => self.begin_prompt(PromptKind::InjectCan),
            KeyCode::Char('p') if !self.bus.messages().is_empty() => {
                self.begin_prompt(PromptKind::SetCanPayload)
            }
            KeyCode::Char('m') if !self.bus.messages().is_empty() => {
                self.begin_prompt(PromptKind::SetCanPeriod)
            }
            KeyCode::Char('x') => {
                self.bus.clear_trace();
                self.status = "Bus trace cleared".to_owned();
            }
            KeyCode::Char('w') => self.begin_prompt(PromptKind::SaveCanTrace),
            KeyCode::Up | KeyCode::Char('k') => self.bus_index = self.bus_index.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.bus_index =
                    (self.bus_index + 1).min(self.bus.messages().len().saturating_sub(1));
            }
            KeyCode::Home => self.bus_index = 0,
            KeyCode::End => self.bus_index = self.bus.messages().len().saturating_sub(1),
            KeyCode::Enter => {
                let Some(message) = self.bus.messages().get(self.bus_index).cloned() else {
                    return;
                };
                match self.bus.set_message_enabled(message.id, !message.enabled) {
                    Ok(()) => {
                        self.status = format!(
                            "{} {}",
                            if message.enabled {
                                "Disabled"
                            } else {
                                "Enabled"
                            },
                            message.name
                        )
                    }
                    Err(error) => self.status = format!("Message selection failed: {error}"),
                }
            }
            KeyCode::Char('t') => {
                let Some(message) = self.bus.messages().get(self.bus_index).cloned() else {
                    return;
                };
                match self.bus.trigger(message.id) {
                    Ok(()) => {
                        self.status = format!("Queued one-shot transmission for {}", message.name)
                    }
                    Err(error) => self.status = format!("Trigger failed: {error}"),
                }
            }
            _ => {}
        }
    }

    fn handle_lin_bus_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.show_lin_bus = false;
                self.status = "Returned from LIN workbench".to_owned();
            }
            KeyCode::Char('c') => {
                if self.lin_bus.connected {
                    self.lin_bus.disconnect();
                    self.status = "LIN channel disconnected".to_owned();
                } else {
                    match self.lin_bus.connect() {
                        Ok(()) => {
                            self.status =
                                format!("{} LIN channel connected", self.lin_bus.adapter_name())
                        }
                        Err(error) => self.status = format!("LIN connect failed: {error}"),
                    }
                }
            }
            KeyCode::Char('a') => match self.lin_bus.cycle_adapter() {
                Ok(adapter) => self.status = format!("Selected {adapter} LIN adapter"),
                Err(error) => self.status = format!("LIN adapter selection failed: {error}"),
            },
            KeyCode::Char('v') => self.begin_prompt(PromptKind::ConfigureLin),
            KeyCode::Char(',') => match self.lin_bus.cycle_channel(-1) {
                Ok(channel) => self.status = format!("Selected LIN channel {channel}"),
                Err(error) => self.status = format!("LIN channel selection failed: {error}"),
            },
            KeyCode::Char('.') => match self.lin_bus.cycle_channel(1) {
                Ok(channel) => self.status = format!("Selected LIN channel {channel}"),
                Err(error) => self.status = format!("LIN channel selection failed: {error}"),
            },
            KeyCode::Char(' ') => {
                let running = !self.lin_bus.running;
                match self.lin_bus.set_running(running) {
                    Ok(()) => {
                        self.status = if running {
                            "LIN schedule running".to_owned()
                        } else {
                            "LIN schedule paused".to_owned()
                        }
                    }
                    Err(error) => self.status = format!("LIN scheduling unavailable: {error}"),
                }
            }
            KeyCode::Char('s') => self.begin_prompt(PromptKind::SendLin),
            KeyCode::Char('i') => self.begin_prompt(PromptKind::InjectLin),
            KeyCode::Char('p') if !self.lin_bus.frames().is_empty() => {
                self.begin_prompt(PromptKind::SetLinPayload)
            }
            KeyCode::Char('x') => {
                self.lin_bus.clear_trace();
                self.status = "LIN trace cleared".to_owned();
            }
            KeyCode::Char('w') => self.begin_prompt(PromptKind::SaveLinTrace),
            KeyCode::Char('[') => self.change_lin_schedule(-1),
            KeyCode::Char(']') => self.change_lin_schedule(1),
            KeyCode::Up | KeyCode::Char('k') => {
                self.lin_bus_index = self.lin_bus_index.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.lin_bus_index =
                    (self.lin_bus_index + 1).min(self.lin_bus.frames().len().saturating_sub(1));
            }
            KeyCode::Home => self.lin_bus_index = 0,
            KeyCode::End => self.lin_bus_index = self.lin_bus.frames().len().saturating_sub(1),
            KeyCode::Enter => {
                let Some(frame) = self.lin_bus.frames().get(self.lin_bus_index).cloned() else {
                    return;
                };
                let enabled = !(frame.enabled && frame.simulated);
                match self.lin_bus.set_frame_enabled(&frame.name, enabled) {
                    Ok(()) => {
                        self.status = format!(
                            "{} simulation for {}",
                            if enabled { "Enabled" } else { "Disabled" },
                            frame.name
                        )
                    }
                    Err(error) => self.status = format!("LIN frame selection failed: {error}"),
                }
            }
            KeyCode::Char('t') => {
                let Some(frame) = self.lin_bus.frames().get(self.lin_bus_index).cloned() else {
                    return;
                };
                match self.lin_bus.trigger(&frame.name) {
                    Ok(()) => self.status = format!("Queued one-shot LIN frame {}", frame.name),
                    Err(error) => self.status = format!("LIN trigger failed: {error}"),
                }
            }
            _ => {}
        }
    }

    fn change_lin_schedule(&mut self, delta: isize) {
        let count = self.lin_bus.schedules().len();
        if count == 0 {
            self.status = "Open an LDF containing schedule tables first".to_owned();
            return;
        }
        self.lin_schedule_index = self
            .lin_schedule_index
            .saturating_add_signed(delta)
            .min(count - 1);
        match self.lin_bus.select_schedule(self.lin_schedule_index) {
            Ok(name) => self.status = format!("Selected LIN schedule {name}"),
            Err(error) => self.status = format!("Schedule selection failed: {error}"),
        }
    }

    fn selected_a2l_object(&self) -> Option<usize> {
        self.a2l_lab.visible_indices().get(self.a2l_index).copied()
    }

    fn handle_a2l_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('a') => {
                self.show_a2l_lab = false;
                self.status = "Returned from A2L workbench".to_owned();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let view = self.a2l_lab.cycle_view();
                self.a2l_index = 0;
                self.status = format!("A2L view: {view}");
            }
            KeyCode::Char(' ') => {
                let running = !self.a2l_lab.running;
                match self.a2l_lab.set_running(running) {
                    Ok(()) => {
                        self.status = if running {
                            "Virtual ECU DAQ running at 10 Hz".to_owned()
                        } else {
                            "Virtual ECU DAQ paused".to_owned()
                        }
                    }
                    Err(error) => self.status = format!("DAQ unavailable: {error}"),
                }
            }
            KeyCode::Char('e') => {
                if self.selected_a2l_object().is_some() {
                    self.begin_prompt(PromptKind::SetA2lValue);
                }
            }
            KeyCode::Char('x') => {
                self.a2l_lab.clear_samples();
                self.status = "A2L DAQ samples cleared".to_owned();
            }
            KeyCode::Char('w') => self.begin_prompt(PromptKind::SaveA2lMdf),
            KeyCode::Up | KeyCode::Char('k') => self.a2l_index = self.a2l_index.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.a2l_index = (self.a2l_index + 1)
                    .min(self.a2l_lab.visible_indices().len().saturating_sub(1));
            }
            KeyCode::Home => self.a2l_index = 0,
            KeyCode::End => self.a2l_index = self.a2l_lab.visible_indices().len().saturating_sub(1),
            KeyCode::Enter => {
                let Some(index) = self.selected_a2l_object() else {
                    return;
                };
                match self.a2l_lab.toggle_armed(index) {
                    Ok(armed) => {
                        self.status = if armed {
                            "Measurement armed for DAQ".to_owned()
                        } else {
                            "Measurement removed from DAQ".to_owned()
                        }
                    }
                    Err(error) => self.status = format!("DAQ selection failed: {error}"),
                }
            }
            _ => {}
        }
    }

    fn handle_protocol_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('g') => {
                self.show_protocol_lab = false;
                self.status = "Returned from protocol lab".to_owned();
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.protocol_lab.change_protocol(1);
                self.status = format!("{} codec selected", self.protocol_lab.protocol().title());
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.protocol_lab.change_protocol(-1);
                self.status = format!("{} codec selected", self.protocol_lab.protocol().title());
            }
            KeyCode::Char(character @ '1'..='4') => {
                self.protocol_lab
                    .set_protocol(character.to_digit(10).unwrap_or(1) as usize - 1);
                self.status = format!("{} codec selected", self.protocol_lab.protocol().title());
            }
            KeyCode::Char('e') | KeyCode::Enter => self.begin_prompt(PromptKind::ProtocolCommand),
            KeyCode::Char('c') => self.begin_prompt(PromptKind::ConfigureProtocolTransport),
            KeyCode::Char('r') => self.begin_prompt(PromptKind::LiveProtocolCommand),
            KeyCode::Char('d') => match self.protocol_lab.discover_doip() {
                Ok(count) if count > 0 => {
                    self.status =
                        format!("Discovered {count} DoIP entity(s); [/] selects the routing target")
                }
                Ok(_) => self.status = "No DoIP entity answered the discovery request".to_owned(),
                Err(error) => self.status = format!("DoIP discovery failed: {error}"),
            },
            KeyCode::Char('[') => match self.protocol_lab.change_doip_entity(-1) {
                Ok(status) => self.status = status,
                Err(error) => self.status = format!("DoIP selection failed: {error}"),
            },
            KeyCode::Char(']') => match self.protocol_lab.change_doip_entity(1) {
                Ok(status) => self.status = status,
                Err(error) => self.status = format!("DoIP selection failed: {error}"),
            },
            KeyCode::Char('x') => {
                self.protocol_lab.clear();
                self.status = "Protocol history cleared".to_owned();
            }
            KeyCode::Up | KeyCode::Char('k') => self.protocol_lab.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.protocol_lab.move_selection(1),
            KeyCode::Home => self.protocol_lab.record_index = 0,
            KeyCode::End => {
                self.protocol_lab.record_index = self.protocol_lab.records().len().saturating_sub(1)
            }
            _ => {}
        }
    }

    fn handle_odx_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('d') => {
                self.show_odx_lab = false;
                self.status = "Returned from ODX diagnostic workbench".to_owned();
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.odx_lab.move_variant(1);
                self.status = format!(
                    "ODX variant {} selected",
                    self.odx_lab
                        .selected_variant()
                        .map_or("-", |variant| variant.name.as_str())
                );
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.odx_lab.move_variant(-1);
                self.status = format!(
                    "ODX variant {} selected",
                    self.odx_lab
                        .selected_variant()
                        .map_or("-", |variant| variant.name.as_str())
                );
            }
            KeyCode::Up | KeyCode::Char('k') => self.odx_lab.move_service(-1),
            KeyCode::Down | KeyCode::Char('j') => self.odx_lab.move_service(1),
            KeyCode::PageUp => self.odx_lab.move_service(-10),
            KeyCode::PageDown => self.odx_lab.move_service(10),
            KeyCode::Home => {
                self.odx_lab.service_index = 0;
                self.odx_lab.clear_exchange();
            }
            KeyCode::End => {
                self.odx_lab.service_index =
                    self.odx_lab.visible_services().len().saturating_sub(1);
                self.odx_lab.clear_exchange();
            }
            KeyCode::Char('t') => {
                self.odx_lab.toggle_transport();
                self.status = format!("ODX transport: {}", self.odx_lab.transport.title());
            }
            KeyCode::Char('c') => {
                let config = self
                    .odx_lab
                    .selected_can_parameters()
                    .map(|parameters| parameters.config);
                self.status = config.map_or_else(
                    || "Selected ODX variant has no complete physical CAN IDs".to_owned(),
                    |config| self.protocol_lab.apply_uds_can_transport(config),
                );
            }
            KeyCode::Char('e') | KeyCode::Enter => self.begin_prompt(PromptKind::OdxServicePayload),
            KeyCode::Char('r') => self.begin_prompt(PromptKind::LiveOdxServicePayload),
            KeyCode::Char('x') => {
                self.odx_lab.clear_exchange();
                self.status = "ODX request/response view cleared".to_owned();
            }
            _ => {}
        }
    }

    fn handle_prm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('f') => {
                self.show_prm_lab = false;
                self.status = "Returned from PRM flash workbench".to_owned();
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.prm_lab.move_entry(1);
                self.status = format!(
                    "PRM entry {} selected",
                    self.prm_lab.selected_entry().unwrap_or("-")
                );
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.prm_lab.move_entry(-1);
                self.status = format!(
                    "PRM entry {} selected",
                    self.prm_lab.selected_entry().unwrap_or("-")
                );
            }
            KeyCode::Char(' ') | KeyCode::Enter => match self.prm_lab.run_dry_run() {
                Ok(report) => {
                    self.status = format!(
                        "PRM preflight completed · {} steps · final state {}",
                        report.steps.len(),
                        report.final_state
                    )
                }
                Err(error) => self.status = format!("PRM preflight failed: {error}"),
            },
            KeyCode::Char('x') => {
                self.prm_lab.clear_report();
                self.status = "PRM execution report cleared".to_owned();
            }
            KeyCode::Up | KeyCode::Char('k') => self.prm_lab.move_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.prm_lab.move_step(1),
            KeyCode::Home => self.prm_lab.step_index = 0,
            KeyCode::End => {
                self.prm_lab.step_index = self
                    .prm_lab
                    .report
                    .as_ref()
                    .map_or(0, |report| report.steps.len().saturating_sub(1))
            }
            _ => {}
        }
    }

    fn handle_symbol_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('y') => {
                self.show_symbol_lab = false;
                self.status = "Returned from symbol synchronization".to_owned();
            }
            KeyCode::Up | KeyCode::Char('k') => self.symbol_lab.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.symbol_lab.move_selection(1),
            KeyCode::Home => self.symbol_lab.candidate_index = 0,
            KeyCode::End => {
                self.symbol_lab.candidate_index = self.symbol_lab.candidates.len().saturating_sub(1)
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.symbol_lab.toggle_selected() {
                Ok(selected) => {
                    self.status = if selected {
                        "Symbol address update selected".to_owned()
                    } else {
                        "Symbol address update unselected".to_owned()
                    }
                }
                Err(error) => self.status = format!("Selection unavailable: {error}"),
            },
            KeyCode::Char('m') => self.begin_prompt(PromptKind::ConfigureSymbolMultiplier),
            KeyCode::Char('z') => {
                self.symbol_lab.allow_size_mismatch = !self.symbol_lab.allow_size_mismatch;
                if !self.symbol_lab.allow_size_mismatch {
                    for candidate in &mut self.symbol_lab.candidates {
                        if candidate.record.typ
                            == autors_symbols::update::UpdateType::AdjustAddressAndSize
                        {
                            candidate.selected = false;
                        }
                    }
                }
                self.status = format!(
                    "Size-mismatch updates {}",
                    if self.symbol_lab.allow_size_mismatch {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
            }
            KeyCode::Char('p') => {
                self.symbol_lab.preserve_bit_mask = !self.symbol_lab.preserve_bit_mask;
                self.status = format!(
                    "Existing A2L bit masks {}",
                    if self.symbol_lab.preserve_bit_mask {
                        "preserved"
                    } else {
                        "updated from symbols"
                    }
                );
            }
            KeyCode::Char('r') => {
                let Some(project) = &self.a2l_project else {
                    self.status = "Open the target A2L first".to_owned();
                    return;
                };
                match self.symbol_lab.refresh(project) {
                    Ok(count) => self.status = format!("Recomputed {count} symbol candidate(s)"),
                    Err(error) => self.status = format!("Symbol refresh failed: {error}"),
                }
            }
            KeyCode::Char('s') => self.begin_prompt(PromptKind::SaveSymbolUpdates),
            _ => {}
        }
    }

    fn execute_live_protocol(&mut self, input: &str) -> Option<Vec<u8>> {
        let protocol = self.protocol_lab.protocol();
        if protocol == ProtocolKind::DoIp {
            return match self.protocol_lab.execute_doip_live(input) {
                Ok(record) => {
                    self.status = record.summary.clone();
                    record.response.clone()
                }
                Err(error) => {
                    self.status = format!("Live DoIP request failed: {error}");
                    None
                }
            };
        }
        let request = match self.protocol_lab.prepare_live_request(input) {
            Ok(request) => request,
            Err(error) => {
                self.status = format!("Live request is invalid: {error}");
                return None;
            }
        };
        let Some(config) = self.protocol_lab.can_transport() else {
            self.status = "Selected protocol has no CAN transport".to_owned();
            return None;
        };
        let result = match protocol {
            ProtocolKind::Uds => self
                .bus
                .diagnostic_request(
                    config.command_id,
                    config.response_id,
                    config.use_can_fd,
                    &request.bytes,
                )
                .map(|(state, response)| (format!("{state:?}"), response)),
            ProtocolKind::Ccp => self
                .bus
                .raw_protocol_request(
                    config.command_id,
                    config.response_id,
                    config.use_can_fd,
                    RawCanProtocol::Ccp,
                    &request.bytes,
                )
                .map(|response| ("Success".to_owned(), response)),
            ProtocolKind::Xcp => self
                .bus
                .raw_protocol_request(
                    config.command_id,
                    config.response_id,
                    config.use_can_fd,
                    RawCanProtocol::Xcp,
                    &request.bytes,
                )
                .map(|response| ("Success".to_owned(), response)),
            ProtocolKind::DoIp => unreachable!(),
        };
        match result {
            Ok((state, response)) => {
                let response_copy = response.clone();
                let record = self
                    .protocol_lab
                    .record_live_exchange(request, response, state);
                self.status = record.summary.clone();
                Some(response_copy)
            }
            Err(error) => {
                self.status = format!("Live {} request failed: {error}", protocol.title());
                None
            }
        }
    }

    fn toggle_playback(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        if !document.is_trace() {
            return;
        }
        self.playback.active = !self.playback.active;
        if self.playback.active {
            self.playback.position = self
                .selected_row()
                .and_then(|index| document.row_timestamp(index))
                .unwrap_or_default();
            self.status = format!("Trace playback running at {}×", self.playback.speed);
        } else {
            self.status = format!("Trace playback paused at {:.6} s", self.playback.position);
        }
    }

    fn reset_playback(&mut self) {
        self.playback.active = false;
        self.playback.position = 0.0;
        self.row_index = 0;
        self.status = "Trace playback reset".to_owned();
    }

    fn change_playback_speed(&mut self, factor: f64) {
        self.playback.speed = (self.playback.speed * factor).clamp(0.125, 16.0);
        self.status = format!("Trace playback speed {}×", self.playback.speed);
    }
}

fn resolve_input_path(input: &str, workspace_root: &Path) -> PathBuf {
    let input = input.trim().trim_matches('"');
    let path = PathBuf::from(input);
    if path.is_absolute() || path.exists() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn is_conservation_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "dcm" | "par" | "m")
        })
}

fn is_symbol_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "elf" | "axf" | "map"
            )
        })
}

fn is_odx_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.to_ascii_lowercase().starts_with("odx"))
}

fn suggest_symbol_output(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("updated");
    source.with_file_name(format!("{stem}.symbols.a2l"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> CapabilityCatalog {
        CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: vec![Capability {
                name: "autors-dbc".to_owned(),
                description: "DBC".to_owned(),
                version: "0.1.0".to_owned(),
                group: CapabilityGroup::Network,
                features: Vec::new(),
                dependencies: Vec::new(),
                manifest_path: PathBuf::from("crates/autors-dbc/Cargo.toml"),
            }],
        }
    }

    #[test]
    fn search_prompt_filters_the_capability_list() {
        let mut app = App::new(catalog());
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "missing".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.visible_capability_indices().is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.visible_capability_indices(), vec![0]);
    }

    #[test]
    fn group_navigation_clamps_at_both_edges() {
        let mut app = App::new(catalog());
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.group_index, 0);
        for _ in 0..20 {
            app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        assert_eq!(app.group_index, CapabilityGroup::ALL.len() - 1);
    }

    #[test]
    fn trace_playback_follows_record_timestamps_and_stops_at_end() {
        let mut app = App::new(catalog());
        app.document = Some(Document {
            path: PathBuf::from("trace.asc"),
            database: None,
            kind: "CANoe ASC trace".to_owned(),
            summary: Vec::new(),
            columns: vec!["Time".to_owned()],
            rows: vec![
                vec!["0.100".to_owned()],
                vec!["0.200".to_owned()],
                vec!["0.300".to_owned()],
            ],
            details: vec![Vec::new(); 3],
        });
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.advance_playback(Duration::from_millis(150));
        assert_eq!(app.row_index, 1);
        assert!(app.playback.active);
        app.advance_playback(Duration::from_millis(100));
        assert_eq!(app.row_index, 2);
        assert!(!app.playback.active);
    }
}
