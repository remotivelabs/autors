//! LDF schedule-table execution and LIN remaining-bus simulation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use autors_ldf::codec::SignalValues;
use autors_ldf::diagnostic::{
    DiagnosticRequest, MASTER_REQUEST_FRAME_ID, SID_ASSIGN_FRAME_ID, SLAVE_RESPONSE_FRAME_ID,
};
use autors_ldf::model::{FrameRef, Ldf, ScheduleCommand, ScheduleEntry, SlaveNode};
use autors_lin::device::{protected_id, LinDevice, LinFrame};

use crate::error::{Error, Result};
use crate::hook::{HookId, HookResult, SendOutcome, TransmissionCause};

/// Selects LIN hooks by logical LDF frame name or numeric identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinHookSelector {
    /// Match one named unconditional, sporadic, event-triggered, or diagnostic frame.
    Frame(String),
    /// Match every transmitted frame or header request with this identifier.
    Id(u8),
}

impl From<String> for LinHookSelector {
    fn from(value: String) -> Self {
        Self::Frame(value)
    }
}

impl From<&str> for LinHookSelector {
    fn from(value: &str) -> Self {
        Self::Frame(value.to_string())
    }
}

impl From<u8> for LinHookSelector {
    fn from(value: u8) -> Self {
        Self::Id(value)
    }
}

/// Metadata supplied to LIN hooks.
#[derive(Debug, Clone)]
pub struct LinHookContext {
    /// Logical LDF frame or schedule-command name.
    pub frame_name: String,
    /// Frame identifier before hook mutation.
    pub frame_id: u8,
    /// Active schedule table, absent for one-shot triggers.
    pub schedule_name: Option<String>,
    /// Zero-based entry within the active table.
    pub schedule_index: Option<usize>,
    /// Reason the operation became due.
    pub cause: TransmissionCause,
    /// Deadline that made the operation due.
    pub scheduled_for: Instant,
}

/// Effective operation performed for a LIN schedule slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinOperation {
    /// A complete header and response payload were sent.
    Sent,
    /// Only a header was sent to request a physical slave response.
    Requested,
    /// Selection rules or sporadic state suppressed the slot.
    Skipped,
}

/// Result of one LIN trigger or schedule-table slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinTransmission {
    /// Logical frame or command name.
    pub frame_name: String,
    /// Numeric LIN identifier, when the slot maps to a bus frame.
    pub frame_id: Option<u8>,
    /// Active schedule name, absent for one-shot triggers.
    pub schedule_name: Option<String>,
    /// Zero-based schedule entry.
    pub schedule_index: Option<usize>,
    /// Adapter operation performed.
    pub operation: LinOperation,
    /// Payload bytes accepted by the adapter for [`LinOperation::Sent`].
    pub bytes: usize,
}

/// Observable state of an unconditional or diagnostic LDF frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinFrameState {
    /// LDF frame name.
    pub name: String,
    /// Numeric LIN identifier.
    pub id: u8,
    /// Publisher node for unconditional frames.
    pub publisher: Option<String>,
    /// Whether an LDF schedule slot is allowed to process this frame.
    pub enabled: bool,
    /// Whether the scheduler will provide the response payload instead of
    /// requesting it from a physical slave.
    pub simulated: bool,
    /// Current payload.
    pub payload: Vec<u8>,
}

type BeforeHook = Box<dyn FnMut(&mut LinFrame, &LinHookContext) -> HookResult + Send>;
type AfterHook = Box<dyn FnMut(&LinFrame, &LinHookContext, &SendOutcome) -> HookResult + Send>;

struct HookEntry<T> {
    id: HookId,
    selector: LinHookSelector,
    callback: T,
}

#[derive(Debug, Clone)]
struct RuntimeFrame {
    id: u8,
    publisher: Option<String>,
    payload: Vec<u8>,
    enabled_override: Option<bool>,
}

struct ActiveSchedule {
    name: String,
    index: usize,
    next_due: Instant,
}

enum PreparedOperation {
    Send { name: String, frame: LinFrame },
    Request { name: String, frame: LinFrame },
    Skip { name: String, id: Option<u8> },
}

/// Cooperative LDF schedule-table executor.
///
/// Starting a table schedules its first entry immediately. For an
/// unconditional frame, the master and selected simulated nodes send their
/// stored payload; unselected slave nodes receive a header-only request so a
/// physical ECU can answer. A per-frame `false` override suppresses its slot.
pub struct LinScheduler {
    ldf: Ldf,
    frames: BTreeMap<String, RuntimeFrame>,
    wrapper_overrides: BTreeMap<String, bool>,
    enabled_nodes: BTreeSet<String>,
    before_hooks: Vec<HookEntry<BeforeHook>>,
    after_hooks: Vec<HookEntry<AfterHook>>,
    next_hook_id: u64,
    active: Option<ActiveSchedule>,
    triggered: VecDeque<String>,
    sporadic_pending: BTreeSet<String>,
    last_now: Instant,
}

impl LinScheduler {
    /// Builds a scheduler and initializes unconditional payloads from LDF
    /// signal initial values.
    pub fn from_ldf(ldf: &Ldf) -> Result<Self> {
        Self::from_ldf_at(ldf, Instant::now())
    }

    /// Deterministic form of [`Self::from_ldf`] with an explicit clock origin.
    pub fn from_ldf_at(ldf: &Ldf, now: Instant) -> Result<Self> {
        ldf.validate()?;
        let mut frames = BTreeMap::new();
        for frame in ldf.unconditional_frames.values() {
            let payload = ldf.encode_frame_raw(&frame.name, &SignalValues::new())?;
            frames.insert(
                frame.name.clone(),
                RuntimeFrame {
                    id: frame.id,
                    publisher: Some(frame.publisher.clone()),
                    payload,
                    enabled_override: None,
                },
            );
        }
        for frame in ldf.diagnostic_frames.values() {
            frames.insert(
                frame.name.clone(),
                RuntimeFrame {
                    id: frame.id,
                    publisher: None,
                    payload: vec![0; 8],
                    enabled_override: None,
                },
            );
        }
        Ok(Self {
            ldf: ldf.clone(),
            frames,
            wrapper_overrides: BTreeMap::new(),
            enabled_nodes: BTreeSet::new(),
            before_hooks: Vec::new(),
            after_hooks: Vec::new(),
            next_hook_id: 1,
            active: None,
            triggered: VecDeque::new(),
            sporadic_pending: BTreeSet::new(),
            last_now: now,
        })
    }

    /// Lists runtime states in ascending frame-name order.
    pub fn frames(&self) -> Vec<LinFrameState> {
        self.frames
            .iter()
            .map(|(name, frame)| self.state_for(name, frame))
            .collect()
    }

    /// Returns one unconditional or diagnostic frame state.
    pub fn frame(&self, name: &str) -> Option<LinFrameState> {
        self.frames
            .get(name)
            .map(|frame| self.state_for(name, frame))
    }

    /// Names of the LDF schedule tables.
    pub fn schedules(&self) -> impl Iterator<Item = &str> {
        self.ldf.schedule_tables.keys().map(String::as_str)
    }

    /// Starts or switches to a schedule table. Its first slot is due on the
    /// next poll.
    pub fn start_schedule(&mut self, name: &str) -> Result<()> {
        self.ldf
            .schedule_tables
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("LDF schedule {name:?}")))?;
        self.active = Some(ActiveSchedule {
            name: name.to_string(),
            index: 0,
            next_due: self.last_now,
        });
        Ok(())
    }

    /// Stops schedule-table execution. One-shot triggers remain queued.
    pub fn stop_schedule(&mut self) {
        self.active = None;
    }

    /// Active schedule table name.
    pub fn active_schedule(&self) -> Option<&str> {
        self.active.as_ref().map(|active| active.name.as_str())
    }

    /// Enables or disables simulation of one LDF master or slave node.
    pub fn set_node_enabled(&mut self, node: &str, enabled: bool) -> Result<()> {
        if node != self.ldf.master.name && !self.ldf.slaves.contains_key(node) {
            return Err(Error::NotFound(format!("LDF node {node:?}")));
        }
        if enabled {
            self.enabled_nodes.insert(node.to_string());
        } else {
            self.enabled_nodes.remove(node);
        }
        Ok(())
    }

    /// Whether a node is selected for simulation.
    pub fn node_enabled(&self, node: &str) -> bool {
        self.enabled_nodes.contains(node)
    }

    /// Sets an enable override on a named LDF frame. `true` also makes an
    /// unconditional slave frame simulated without selecting its whole node.
    pub fn set_frame_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
        if let Some(frame) = self.frames.get_mut(name) {
            frame.enabled_override = Some(enabled);
            return Ok(());
        }
        if self.ldf.sporadic_frames.contains_key(name)
            || self.ldf.event_triggered_frames.contains_key(name)
        {
            self.wrapper_overrides.insert(name.to_string(), enabled);
            return Ok(());
        }
        Err(Error::NotFound(format!("LDF frame {name:?}")))
    }

    /// Clears a frame override so publisher-node selection applies again.
    pub fn clear_frame_override(&mut self, name: &str) -> Result<()> {
        if let Some(frame) = self.frames.get_mut(name) {
            frame.enabled_override = None;
            return Ok(());
        }
        if self.wrapper_overrides.remove(name).is_some()
            || self.ldf.sporadic_frames.contains_key(name)
            || self.ldf.event_triggered_frames.contains_key(name)
        {
            return Ok(());
        }
        Err(Error::NotFound(format!("LDF frame {name:?}")))
    }

    /// Replaces an unconditional or diagnostic frame payload. Updating an
    /// unconditional frame also marks it pending for a sporadic-frame slot.
    pub fn set_payload(&mut self, name: &str, payload: Vec<u8>) -> Result<()> {
        if payload.len() > 8 {
            return Err(Error::Invalid(format!(
                "LIN frame {name:?} payload has {} bytes",
                payload.len()
            )));
        }
        let frame = self
            .frames
            .get_mut(name)
            .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}")))?;
        if frame.publisher.is_some() && payload.len() != frame.payload.len() {
            return Err(Error::Invalid(format!(
                "LIN frame {name:?} requires {} bytes, got {}",
                frame.payload.len(),
                payload.len()
            )));
        }
        frame.payload = payload;
        if frame.publisher.is_some() {
            self.sporadic_pending.insert(name.to_string());
        }
        Ok(())
    }

    /// Explicitly marks an unconditional frame as pending for a sporadic slot.
    pub fn mark_sporadic_pending(&mut self, name: &str) -> Result<()> {
        let frame = self
            .frames
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}")))?;
        if frame.publisher.is_none() {
            return Err(Error::Invalid(format!(
                "diagnostic frame {name:?} cannot be sporadic"
            )));
        }
        self.sporadic_pending.insert(name.to_string());
        Ok(())
    }

    /// Queues a named unconditional, event-triggered, sporadic, or diagnostic
    /// frame as a one-shot operation independent of the active schedule.
    pub fn trigger_frame(&mut self, name: &str) -> Result<()> {
        self.ldf
            .frame(name)
            .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}")))?;
        self.triggered.push_back(name.to_string());
        Ok(())
    }

    /// Registers a mutable before-send hook. Hooks also run before header-only
    /// requests, with an empty payload.
    pub fn add_before_hook<S, F>(&mut self, selector: S, hook: F) -> Result<HookId>
    where
        S: Into<LinHookSelector>,
        F: FnMut(&mut LinFrame, &LinHookContext) -> HookResult + Send + 'static,
    {
        let selector = selector.into();
        self.validate_selector(&selector)?;
        let id = self.allocate_hook_id();
        self.before_hooks.push(HookEntry {
            id,
            selector,
            callback: Box::new(hook),
        });
        Ok(id)
    }

    /// Registers an after-send hook.
    pub fn add_after_hook<S, F>(&mut self, selector: S, hook: F) -> Result<HookId>
    where
        S: Into<LinHookSelector>,
        F: FnMut(&LinFrame, &LinHookContext, &SendOutcome) -> HookResult + Send + 'static,
    {
        let selector = selector.into();
        self.validate_selector(&selector)?;
        let id = self.allocate_hook_id();
        self.after_hooks.push(HookEntry {
            id,
            selector,
            callback: Box::new(hook),
        });
        Ok(id)
    }

    /// Removes a before- or after-send hook.
    pub fn remove_hook(&mut self, id: HookId) -> bool {
        if let Some(index) = self.before_hooks.iter().position(|hook| hook.id == id) {
            drop(self.before_hooks.remove(index));
            return true;
        }
        if let Some(index) = self.after_hooks.iter().position(|hook| hook.id == id) {
            drop(self.after_hooks.remove(index));
            return true;
        }
        false
    }

    /// Earliest queued trigger or schedule-table deadline.
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.triggered.is_empty() {
            Some(self.last_now)
        } else {
            self.active.as_ref().map(|active| active.next_due)
        }
    }

    /// Executes queued triggers and at most one due schedule slot using the
    /// current monotonic time.
    pub async fn poll<D>(&mut self, device: &mut D) -> Result<Vec<LinTransmission>>
    where
        D: LinDevice + Send + ?Sized,
    {
        self.poll_at(device, Instant::now()).await
    }

    /// Deterministic poll with an explicit monotonic time.
    pub async fn poll_at<D>(&mut self, device: &mut D, now: Instant) -> Result<Vec<LinTransmission>>
    where
        D: LinDevice + Send + ?Sized,
    {
        if now < self.last_now {
            return Err(Error::Invalid(
                "LIN scheduler clock moved backwards".to_string(),
            ));
        }
        self.last_now = now;
        let mut results = Vec::new();
        while let Some(name) = self.triggered.pop_front() {
            let prepared = self.prepare_named_frame(&name, true)?;
            results.push(
                self.execute(
                    device,
                    prepared,
                    None,
                    None,
                    TransmissionCause::Triggered,
                    now,
                )
                .await?,
            );
        }
        let due_slot = self
            .active
            .as_ref()
            .is_some_and(|active| active.next_due <= now);
        if due_slot {
            let (schedule_name, index, scheduled_for, entry) = {
                let active = self.active.as_ref().expect("checked above");
                let table = self
                    .ldf
                    .schedule_tables
                    .get(&active.name)
                    .expect("active schedule was validated");
                if table.entries.is_empty() {
                    return Err(Error::Invalid(format!(
                        "LDF schedule {:?} has no entries",
                        active.name
                    )));
                }
                (
                    active.name.clone(),
                    active.index,
                    active.next_due,
                    table.entries[active.index].clone(),
                )
            };
            self.advance_schedule(&entry, now)?;
            let prepared = self.prepare_command(&entry.command)?;
            results.push(
                self.execute(
                    device,
                    prepared,
                    Some(schedule_name),
                    Some(index),
                    TransmissionCause::Schedule,
                    scheduled_for,
                )
                .await?,
            );
        }
        Ok(results)
    }

    fn state_for(&self, name: &str, frame: &RuntimeFrame) -> LinFrameState {
        LinFrameState {
            name: name.to_string(),
            id: frame.id,
            publisher: frame.publisher.clone(),
            enabled: frame.enabled_override.unwrap_or(true),
            simulated: self.frame_simulated(frame),
            payload: frame.payload.clone(),
        }
    }

    fn frame_simulated(&self, frame: &RuntimeFrame) -> bool {
        match frame.enabled_override {
            Some(value) => value,
            None => frame.publisher.as_ref().is_some_and(|publisher| {
                publisher == &self.ldf.master.name || self.enabled_nodes.contains(publisher)
            }),
        }
    }

    fn prepare_named_frame(&mut self, name: &str, forced: bool) -> Result<PreparedOperation> {
        match self
            .ldf
            .frame(name)
            .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}")))?
        {
            FrameRef::Unconditional(_) | FrameRef::Diagnostic(_) => {
                self.prepare_runtime_frame(name, forced)
            }
            FrameRef::Sporadic(frame) => {
                if self.wrapper_overrides.get(name) == Some(&false) {
                    return Ok(PreparedOperation::Skip {
                        name: name.to_string(),
                        id: None,
                    });
                }
                let references = frame.frames.clone();
                let selected = if forced || self.wrapper_overrides.get(name) == Some(&true) {
                    references.first().cloned()
                } else {
                    references
                        .iter()
                        .find(|candidate| self.sporadic_pending.contains(*candidate))
                        .cloned()
                };
                let Some(selected) = selected else {
                    return Ok(PreparedOperation::Skip {
                        name: name.to_string(),
                        id: None,
                    });
                };
                self.sporadic_pending.remove(&selected);
                self.prepare_runtime_frame(&selected, forced)
            }
            FrameRef::EventTriggered(frame) => {
                let id = frame.id;
                if self.wrapper_overrides.get(name) == Some(&false) {
                    return Ok(PreparedOperation::Skip {
                        name: name.to_string(),
                        id: Some(id),
                    });
                }
                let references = frame.frames.clone();
                let simulated = references.iter().find(|candidate| {
                    self.frames
                        .get(*candidate)
                        .is_some_and(|runtime| self.frame_simulated(runtime))
                });
                if let Some(candidate) = simulated {
                    let runtime = self
                        .frames
                        .get(candidate)
                        .ok_or_else(|| Error::NotFound(format!("LDF frame {candidate:?}")))?;
                    if runtime.enabled_override == Some(false) {
                        return Ok(PreparedOperation::Skip {
                            name: name.to_string(),
                            id: Some(id),
                        });
                    }
                    let mut payload = runtime.payload.clone();
                    if let Some(first) = payload.first_mut() {
                        *first = protected_id(runtime.id);
                    }
                    Ok(PreparedOperation::Send {
                        name: name.to_string(),
                        frame: LinFrame::new("", id, payload, true),
                    })
                } else {
                    Ok(PreparedOperation::Request {
                        name: name.to_string(),
                        frame: LinFrame::new("", id, Vec::new(), true),
                    })
                }
            }
        }
    }

    fn prepare_runtime_frame(&self, name: &str, forced: bool) -> Result<PreparedOperation> {
        let frame = self
            .frames
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}")))?;
        if !forced && frame.enabled_override == Some(false) {
            return Ok(PreparedOperation::Skip {
                name: name.to_string(),
                id: Some(frame.id),
            });
        }
        if forced || self.frame_simulated(frame) {
            Ok(PreparedOperation::Send {
                name: name.to_string(),
                frame: LinFrame::new("", frame.id, frame.payload.clone(), true),
            })
        } else {
            Ok(PreparedOperation::Request {
                name: name.to_string(),
                frame: LinFrame::new("", frame.id, Vec::new(), true),
            })
        }
    }

    fn prepare_command(&mut self, command: &ScheduleCommand) -> Result<PreparedOperation> {
        match command {
            ScheduleCommand::Frame(name) => self.prepare_named_frame(name, false),
            ScheduleCommand::MasterRequest => {
                self.prepare_diagnostic_frame(MASTER_REQUEST_FRAME_ID, true)
            }
            ScheduleCommand::SlaveResponse => {
                self.prepare_diagnostic_frame(SLAVE_RESPONSE_FRAME_ID, false)
            }
            ScheduleCommand::AssignNad { node } => {
                let slave = self.slave(node)?;
                let product = slave.product_id.ok_or_else(|| {
                    Error::Invalid(format!("LDF slave {node:?} has no product_id"))
                })?;
                let initial_nad = slave.initial_nad.or(slave.configured_nad).ok_or_else(|| {
                    Error::Invalid(format!("LDF slave {node:?} has no initial NAD"))
                })?;
                let new_nad = slave.configured_nad.ok_or_else(|| {
                    Error::Invalid(format!("LDF slave {node:?} has no configured NAD"))
                })?;
                Ok(self.diagnostic_send(
                    format!("AssignNAD({node})"),
                    DiagnosticRequest::assign_nad(
                        initial_nad,
                        product.supplier_id,
                        product.function_id,
                        new_nad,
                    ),
                ))
            }
            ScheduleCommand::ConditionalChangeNad {
                nad,
                identifier,
                byte,
                mask,
                invert,
                new_nad,
            } => Ok(self.diagnostic_send(
                "ConditionalChangeNAD".to_string(),
                DiagnosticRequest::conditional_change_nad(
                    *nad,
                    *identifier,
                    *byte,
                    *mask,
                    *invert,
                    *new_nad,
                ),
            )),
            ScheduleCommand::DataDump { node, data } => Ok(self.diagnostic_send(
                format!("DataDump({node})"),
                DiagnosticRequest::data_dump(self.node_nad(node)?, *data),
            )),
            ScheduleCommand::SaveConfiguration { node } => Ok(self.diagnostic_send(
                format!("SaveConfiguration({node})"),
                DiagnosticRequest::save_configuration(self.node_nad(node)?),
            )),
            ScheduleCommand::AssignFrameIdRange {
                node,
                frame_index,
                protected_ids,
            } => {
                let pids = match protected_ids {
                    Some(pids) => *pids,
                    None => self.configurable_pids(node, *frame_index)?,
                };
                Ok(self.diagnostic_send(
                    format!("AssignFrameIdRange({node})"),
                    DiagnosticRequest::assign_frame_id_range(
                        self.node_nad(node)?,
                        *frame_index,
                        pids,
                    ),
                ))
            }
            ScheduleCommand::AssignFrameId { node, frame } => {
                let pid = self
                    .frames
                    .get(frame)
                    .map(|runtime| protected_id(runtime.id))
                    .ok_or_else(|| Error::NotFound(format!("LDF frame {frame:?}")))?;
                Ok(self.assign_frame_id(node, frame, pid)?)
            }
            ScheduleCommand::UnassignFrameId { node, frame } => {
                Ok(self.assign_frame_id(node, frame, 0)?)
            }
            ScheduleCommand::FreeFormat(payload) => {
                Ok(self.diagnostic_send("FreeFormat".to_string(), *payload))
            }
        }
    }

    fn prepare_diagnostic_frame(&self, id: u8, master_request: bool) -> Result<PreparedOperation> {
        let name = self
            .frames
            .iter()
            .find_map(|(name, frame)| (frame.id == id).then_some(name.as_str()))
            .unwrap_or(if master_request {
                "MasterReq"
            } else {
                "SlaveResp"
            });
        if let Some(frame) = self.frames.get(name) {
            if master_request || self.frame_simulated(frame) {
                return Ok(PreparedOperation::Send {
                    name: name.to_string(),
                    frame: LinFrame::new("", id, frame.payload.clone(), true),
                });
            }
        }
        Ok(PreparedOperation::Request {
            name: name.to_string(),
            frame: LinFrame::new("", id, Vec::new(), true),
        })
    }

    fn diagnostic_send(&self, name: String, payload: [u8; 8]) -> PreparedOperation {
        PreparedOperation::Send {
            name,
            frame: LinFrame::new("", MASTER_REQUEST_FRAME_ID, payload.to_vec(), true),
        }
    }

    fn assign_frame_id(&self, node: &str, frame: &str, pid: u8) -> Result<PreparedOperation> {
        let slave = self.slave(node)?;
        let product = slave
            .product_id
            .ok_or_else(|| Error::Invalid(format!("LDF slave {node:?} has no product_id")))?;
        let message_id = slave
            .configurable_frames
            .iter()
            .find(|configurable| configurable.frame == frame)
            .map(|configurable| configurable.index)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "LDF slave {node:?} has no configurable frame {frame:?}"
                ))
            })?;
        let payload = DiagnosticRequest::raw(
            self.node_nad(node)?,
            6,
            SID_ASSIGN_FRAME_ID,
            [
                product.supplier_id as u8,
                (product.supplier_id >> 8) as u8,
                message_id as u8,
                (message_id >> 8) as u8,
                pid,
            ],
        );
        Ok(self.diagnostic_send(format!("AssignFrameId({node},{frame})"), payload))
    }

    fn configurable_pids(&self, node: &str, start: u8) -> Result<[u8; 4]> {
        let slave = self.slave(node)?;
        let mut output = [0xff; 4];
        for (offset, target) in output.iter_mut().enumerate() {
            let index = u16::from(start) + offset as u16;
            if let Some(configurable) = slave
                .configurable_frames
                .iter()
                .find(|configurable| configurable.index == index)
            {
                let frame = self.frames.get(&configurable.frame).ok_or_else(|| {
                    Error::NotFound(format!("LDF frame {:?}", configurable.frame))
                })?;
                *target = protected_id(frame.id);
            }
        }
        Ok(output)
    }

    fn slave(&self, node: &str) -> Result<&SlaveNode> {
        self.ldf
            .slaves
            .get(node)
            .ok_or_else(|| Error::NotFound(format!("LDF slave {node:?}")))
    }

    fn node_nad(&self, node: &str) -> Result<u8> {
        let slave = self.slave(node)?;
        slave.configured_nad.or(slave.initial_nad).ok_or_else(|| {
            Error::Invalid(format!(
                "LDF slave {node:?} has no configured or initial NAD"
            ))
        })
    }

    fn advance_schedule(&mut self, entry: &ScheduleEntry, now: Instant) -> Result<()> {
        let active = self.active.as_mut().expect("called for active schedule");
        let table = self
            .ldf
            .schedule_tables
            .get(&active.name)
            .expect("active schedule was validated");
        active.index = (active.index + 1) % table.entries.len();
        let deadline = active.next_due.checked_add(entry.delay).ok_or_else(|| {
            Error::Invalid(format!(
                "delay in LDF schedule {:?} exceeds the monotonic clock range",
                active.name
            ))
        })?;
        active.next_due = if deadline <= now && !entry.delay.is_zero() {
            now.checked_add(entry.delay).ok_or_else(|| {
                Error::Invalid(format!(
                    "delay in LDF schedule {:?} exceeds the monotonic clock range",
                    active.name
                ))
            })?
        } else {
            deadline
        };
        Ok(())
    }

    async fn execute<D>(
        &mut self,
        device: &mut D,
        prepared: PreparedOperation,
        schedule_name: Option<String>,
        schedule_index: Option<usize>,
        cause: TransmissionCause,
        scheduled_for: Instant,
    ) -> Result<LinTransmission>
    where
        D: LinDevice + Send + ?Sized,
    {
        let (name, mut frame, operation) = match prepared {
            PreparedOperation::Skip { name, id } => {
                return Ok(LinTransmission {
                    frame_name: name,
                    frame_id: id,
                    schedule_name,
                    schedule_index,
                    operation: LinOperation::Skipped,
                    bytes: 0,
                });
            }
            PreparedOperation::Send { name, frame } => (name, frame, LinOperation::Sent),
            PreparedOperation::Request { name, frame } => (name, frame, LinOperation::Requested),
        };
        let context = LinHookContext {
            frame_name: name.clone(),
            frame_id: frame.id,
            schedule_name: schedule_name.clone(),
            schedule_index,
            cause,
            scheduled_for,
        };
        for hook in &mut self.before_hooks {
            if selector_matches(&hook.selector, &name, frame.id) {
                (hook.callback)(&mut frame, &context).map_err(|error| Error::Hook {
                    phase: "before-send",
                    hook_id: hook.id.get(),
                    message: name.clone(),
                    reason: error.to_string(),
                })?;
            }
        }
        let io_result: std::result::Result<usize, autors_lin::Error> = match operation {
            LinOperation::Sent => device.send(frame.id, &frame.data).await,
            LinOperation::Requested => match device.request(frame.id).await {
                Ok(true) => Ok(1),
                Ok(false) => Ok(0),
                Err(error) => Err(error),
            },
            LinOperation::Skipped => unreachable!(),
        };
        let outcome = match &io_result {
            Ok(1) if operation == LinOperation::Requested => SendOutcome::Requested,
            Ok(bytes) if operation == LinOperation::Requested => SendOutcome::Failed {
                error: format!("adapter rejected header request (accepted {bytes})"),
            },
            Ok(bytes) if *bytes == frame.data.len() => SendOutcome::Sent { bytes: *bytes },
            Ok(bytes) => SendOutcome::Failed {
                error: format!(
                    "adapter accepted {bytes} of {} payload bytes",
                    frame.data.len()
                ),
            },
            Err(error) => SendOutcome::Failed {
                error: error.to_string(),
            },
        };
        for hook in &mut self.after_hooks {
            if selector_matches(&hook.selector, &name, frame.id) {
                (hook.callback)(&frame, &context, &outcome).map_err(|error| Error::Hook {
                    phase: "after-send",
                    hook_id: hook.id.get(),
                    message: name.clone(),
                    reason: error.to_string(),
                })?;
            }
        }
        let bytes = io_result?;
        let expected = if operation == LinOperation::Requested {
            1
        } else {
            frame.data.len()
        };
        if bytes != expected {
            return Err(Error::IncompleteTransmission {
                bus: "LIN",
                message: name,
                expected,
                actual: bytes,
            });
        }
        Ok(LinTransmission {
            frame_name: name,
            frame_id: Some(frame.id),
            schedule_name,
            schedule_index,
            operation,
            bytes: if operation == LinOperation::Sent {
                bytes
            } else {
                0
            },
        })
    }

    fn validate_selector(&self, selector: &LinHookSelector) -> Result<()> {
        match selector {
            LinHookSelector::Frame(name) => self
                .ldf
                .frame(name)
                .map(|_| ())
                .ok_or_else(|| Error::NotFound(format!("LDF frame {name:?}"))),
            LinHookSelector::Id(id) if *id <= 0x3f => Ok(()),
            LinHookSelector::Id(id) => Err(Error::Invalid(format!(
                "LIN hook ID 0x{id:02X} exceeds 0x3f"
            ))),
        }
    }

    fn allocate_hook_id(&mut self) -> HookId {
        let id = HookId(self.next_hook_id);
        self.next_hook_id = self.next_hook_id.saturating_add(1);
        id
    }
}

fn selector_matches(selector: &LinHookSelector, name: &str, id: u8) -> bool {
    match selector {
        LinHookSelector::Frame(expected) => expected == name,
        LinHookSelector::Id(expected) => *expected == id,
    }
}
