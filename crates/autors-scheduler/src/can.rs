//! DBC-driven CAN/CAN FD scheduling.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use autors_can::device::{is_can_id_valid, CanDevice};
use autors_can::frame::{CanFrame, FrameType};
use autors_dbc::dbc::{AttribDefault, AttributeType, DBCFile, MsgType, SignalType};

use crate::error::{Error, Result};
use crate::hook::{HookId, HookResult, SendOutcome, TransmissionCause};

const CYCLE_ATTRIBUTE: &str = "GenMsgCycleTime";
const START_DELAY_ATTRIBUTE: &str = "GenMsgStartDelayTime";
const SIGNAL_START_ATTRIBUTE: &str = "GenSigStartValue";

/// Metadata supplied to CAN hooks.
#[derive(Debug, Clone)]
pub struct CanHookContext {
    /// DBC message identifier before any hook mutation.
    pub message_id: u32,
    /// DBC message name.
    pub message_name: String,
    /// Transmitting DBC node.
    pub node_name: String,
    /// Reason this transmission became due.
    pub cause: TransmissionCause,
    /// Deadline that made the transmission due.
    pub scheduled_for: Instant,
}

/// Observable runtime state of one DBC message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanMessageState {
    /// CAN identifier, including the DBC extended-ID marker when present.
    pub id: u32,
    /// DBC message name.
    pub name: String,
    /// DBC transmitter node.
    pub node: String,
    /// Current cyclic period. `None` means one-shot only.
    pub period: Option<Duration>,
    /// Whether node selection and the optional message override make it active.
    pub enabled: bool,
    /// Current payload used as the input to before-send hooks.
    pub payload: Vec<u8>,
    /// Current classic/FD mode.
    pub frame_type: FrameType,
}

/// One successful CAN transmission performed by a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanTransmission {
    /// DBC message identifier.
    pub message_id: u32,
    /// DBC message name.
    pub message_name: String,
    /// Transmission reason.
    pub cause: TransmissionCause,
    /// Payload bytes accepted by the adapter.
    pub bytes: usize,
}

type BeforeHook = Box<dyn FnMut(&mut CanFrame, &CanHookContext) -> HookResult + Send>;
type AfterHook = Box<dyn FnMut(&CanFrame, &CanHookContext, &SendOutcome) -> HookResult + Send>;

struct HookEntry<T> {
    id: HookId,
    callback: T,
}

#[derive(Default)]
struct CanHooks {
    before: Vec<HookEntry<BeforeHook>>,
    after: Vec<HookEntry<AfterHook>>,
}

struct ScheduledMessage {
    id: u32,
    name: String,
    node: String,
    period: Option<Duration>,
    start_delay: Duration,
    payload: Vec<u8>,
    frame_type: FrameType,
    enabled_override: Option<bool>,
    next_due: Option<Instant>,
    pending_triggers: usize,
}

/// Cooperative DBC-driven CAN scheduler.
///
/// Messages are inactive until their transmitter node is enabled, the message
/// is explicitly enabled, or a one-shot trigger is queued. Message overrides
/// take precedence over node selection and can be cleared at runtime.
pub struct CanScheduler {
    messages: BTreeMap<u32, ScheduledMessage>,
    message_names: BTreeMap<String, u32>,
    node_messages: BTreeMap<String, Vec<u32>>,
    enabled_nodes: BTreeSet<String>,
    hooks: BTreeMap<u32, CanHooks>,
    next_hook_id: u64,
    last_now: Instant,
}

impl CanScheduler {
    /// Builds a scheduler from DBC `GenMsgCycleTime`,
    /// `GenMsgStartDelayTime`, and `GenSigStartValue` attributes.
    pub fn from_dbc(dbc: &DBCFile) -> Result<Self> {
        Self::from_dbc_at(dbc, Instant::now())
    }

    /// Deterministic form of [`Self::from_dbc`] with an explicit clock origin.
    pub fn from_dbc_at(dbc: &DBCFile, now: Instant) -> Result<Self> {
        let mut messages = BTreeMap::new();
        let mut message_names = BTreeMap::new();
        let mut node_messages: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for message in dbc.messages() {
            if !is_can_id_valid(message.id) {
                return Err(Error::Invalid(format!(
                    "DBC message {} has invalid CAN ID {:#x}",
                    message.name, message.id
                )));
            }
            if messages.contains_key(&message.id) {
                return Err(Error::Invalid(format!(
                    "DBC contains duplicate message ID {}",
                    message.id
                )));
            }
            if message_names
                .insert(message.name.clone(), message.id)
                .is_some()
            {
                return Err(Error::Invalid(format!(
                    "DBC contains duplicate message name {:?}",
                    message.name
                )));
            }
            let cycle_ms = numeric_attribute(dbc, message, CYCLE_ATTRIBUTE).unwrap_or(0.0);
            let start_ms = numeric_attribute(dbc, message, START_DELAY_ATTRIBUTE).unwrap_or(0.0);
            let period = positive_duration_ms(cycle_ms, CYCLE_ATTRIBUTE, &message.name)?;
            let start_delay =
                nonnegative_duration_ms(start_ms, START_DELAY_ATTRIBUTE, &message.name)?;
            if now.checked_add(start_delay).is_none()
                || period.is_some_and(|period| now.checked_add(period).is_none())
            {
                return Err(Error::Invalid(format!(
                    "timing for CAN message {} exceeds the monotonic clock range",
                    message.name
                )));
            }
            let mut payload = vec![0; usize::from(message.dlc)];
            for signal in &message.signals {
                if let Some(value) = signal_numeric_attribute(dbc, signal, SIGNAL_START_ATTRIBUTE) {
                    insert_signal_raw(&mut payload, signal, value.round_ties_even() as i128)?;
                }
            }
            let frame_type = if message.dlc <= 8 {
                FrameType::CAN20B
            } else {
                FrameType::FD_BRS
            };
            node_messages
                .entry(message.source.clone())
                .or_default()
                .push(message.id);
            messages.insert(
                message.id,
                ScheduledMessage {
                    id: message.id,
                    name: message.name.clone(),
                    node: message.source.clone(),
                    period,
                    start_delay,
                    payload,
                    frame_type,
                    enabled_override: None,
                    next_due: None,
                    pending_triggers: 0,
                },
            );
        }
        Ok(Self {
            messages,
            message_names,
            node_messages,
            enabled_nodes: BTreeSet::new(),
            hooks: BTreeMap::new(),
            next_hook_id: 1,
            last_now: now,
        })
    }

    /// Returns all known message states in ascending CAN-ID order.
    pub fn messages(&self) -> Vec<CanMessageState> {
        self.messages
            .values()
            .map(|message| self.state_for(message))
            .collect()
    }

    /// Returns one message state by CAN ID.
    pub fn message(&self, id: u32) -> Option<CanMessageState> {
        self.messages
            .get(&id)
            .map(|message| self.state_for(message))
    }

    /// Resolves a DBC message name to its CAN ID.
    pub fn message_id(&self, name: &str) -> Option<u32> {
        self.message_names.get(name).copied()
    }

    /// Sets a per-message enable override by DBC message name.
    pub fn set_message_enabled_by_name(&mut self, name: &str, enabled: bool) -> Result<()> {
        let id = self.id_for_name(name)?;
        self.set_message_enabled(id, enabled)
    }

    /// Enables or disables all messages transmitted by `node`.
    /// Explicit per-message overrides retain precedence.
    pub fn set_node_enabled(&mut self, node: &str, enabled: bool) -> Result<()> {
        let ids = self
            .node_messages
            .get(node)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("DBC node {node:?}")))?;
        let before: Vec<(u32, bool)> = ids
            .iter()
            .map(|id| (*id, self.message_enabled_by_id(*id)))
            .collect();
        if enabled {
            self.enabled_nodes.insert(node.to_string());
        } else {
            self.enabled_nodes.remove(node);
        }
        for (id, was_enabled) in before {
            let is_enabled = self.message_enabled_by_id(id);
            if was_enabled != is_enabled {
                self.reset_message_deadline(id, is_enabled)?;
            }
        }
        Ok(())
    }

    /// Returns whether a DBC node is selected for simulation.
    pub fn node_enabled(&self, node: &str) -> bool {
        self.enabled_nodes.contains(node)
    }

    /// Sets a per-message enable override.
    pub fn set_message_enabled(&mut self, id: u32, enabled: bool) -> Result<()> {
        let was_enabled = self.message_enabled_by_id_checked(id)?;
        self.messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?
            .enabled_override = Some(enabled);
        let is_enabled = self.message_enabled_by_id(id);
        if was_enabled != is_enabled {
            self.reset_message_deadline(id, is_enabled)?;
        }
        Ok(())
    }

    /// Clears a per-message override so transmitter-node selection applies.
    pub fn clear_message_override(&mut self, id: u32) -> Result<()> {
        let was_enabled = self.message_enabled_by_id_checked(id)?;
        self.messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?
            .enabled_override = None;
        let is_enabled = self.message_enabled_by_id(id);
        if was_enabled != is_enabled {
            self.reset_message_deadline(id, is_enabled)?;
        }
        Ok(())
    }

    /// Replaces the payload used by subsequent transmissions.
    pub fn set_payload(&mut self, id: u32, payload: Vec<u8>) -> Result<()> {
        let message = self
            .messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?;
        if payload.len() > 64 {
            return Err(Error::Invalid(format!(
                "CAN message {} payload has {} bytes",
                message.name,
                payload.len()
            )));
        }
        message.payload = payload;
        message.frame_type = if message.payload.len() <= 8 {
            FrameType::CAN20B
        } else {
            FrameType::FD_BRS
        };
        Ok(())
    }

    /// Replaces a payload by DBC message name.
    pub fn set_payload_by_name(&mut self, name: &str, payload: Vec<u8>) -> Result<()> {
        let id = self.id_for_name(name)?;
        self.set_payload(id, payload)
    }

    /// Selects classic CAN or CAN FD for a message.
    pub fn set_frame_type(&mut self, id: u32, frame_type: FrameType) -> Result<()> {
        self.messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?
            .frame_type = frame_type;
        Ok(())
    }

    /// Changes the cyclic period. `None` makes the message one-shot only.
    pub fn set_period(&mut self, id: u32, period: Option<Duration>) -> Result<()> {
        if period == Some(Duration::ZERO) {
            return Err(Error::Invalid(
                "CAN period must be greater than zero".to_string(),
            ));
        }
        if period.is_some_and(|period| self.last_now.checked_add(period).is_none()) {
            return Err(Error::Invalid(
                "CAN period exceeds the monotonic clock range".to_string(),
            ));
        }
        let enabled = self.message_enabled_by_id_checked(id)?;
        let message = self
            .messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?;
        message.period = period;
        message.next_due = (enabled && period.is_some()).then_some(self.last_now);
        Ok(())
    }

    /// Queues one transmission for the next poll, even when the message or its
    /// transmitter node is disabled.
    pub fn trigger(&mut self, id: u32) -> Result<()> {
        let message = self
            .messages
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))?;
        message.pending_triggers = message.pending_triggers.saturating_add(1);
        Ok(())
    }

    /// Registers a mutable before-send hook for a CAN ID.
    pub fn add_before_hook<F>(&mut self, id: u32, hook: F) -> Result<HookId>
    where
        F: FnMut(&mut CanFrame, &CanHookContext) -> HookResult + Send + 'static,
    {
        self.ensure_message(id)?;
        let hook_id = self.allocate_hook_id();
        self.hooks.entry(id).or_default().before.push(HookEntry {
            id: hook_id,
            callback: Box::new(hook),
        });
        Ok(hook_id)
    }

    /// Registers a mutable before-send hook by DBC message name.
    pub fn add_before_hook_by_name<F>(&mut self, name: &str, hook: F) -> Result<HookId>
    where
        F: FnMut(&mut CanFrame, &CanHookContext) -> HookResult + Send + 'static,
    {
        let id = self.id_for_name(name)?;
        self.add_before_hook(id, hook)
    }

    /// Registers an after-send hook for a CAN ID.
    pub fn add_after_hook<F>(&mut self, id: u32, hook: F) -> Result<HookId>
    where
        F: FnMut(&CanFrame, &CanHookContext, &SendOutcome) -> HookResult + Send + 'static,
    {
        self.ensure_message(id)?;
        let hook_id = self.allocate_hook_id();
        self.hooks.entry(id).or_default().after.push(HookEntry {
            id: hook_id,
            callback: Box::new(hook),
        });
        Ok(hook_id)
    }

    /// Registers an after-send hook by DBC message name.
    pub fn add_after_hook_by_name<F>(&mut self, name: &str, hook: F) -> Result<HookId>
    where
        F: FnMut(&CanFrame, &CanHookContext, &SendOutcome) -> HookResult + Send + 'static,
    {
        let id = self.id_for_name(name)?;
        self.add_after_hook(id, hook)
    }

    /// Removes a before- or after-send hook.
    pub fn remove_hook(&mut self, hook_id: HookId) -> bool {
        for hooks in self.hooks.values_mut() {
            if let Some(index) = hooks.before.iter().position(|hook| hook.id == hook_id) {
                drop(hooks.before.remove(index));
                return true;
            }
            if let Some(index) = hooks.after.iter().position(|hook| hook.id == hook_id) {
                drop(hooks.after.remove(index));
                return true;
            }
        }
        false
    }

    /// Earliest cyclic deadline or `last_now` when a trigger is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.messages
            .values()
            .filter_map(|message| {
                if message.pending_triggers > 0 {
                    Some(self.last_now)
                } else if self.message_enabled(message) {
                    message.next_due
                } else {
                    None
                }
            })
            .min()
    }

    /// Sends all messages due at the current monotonic time.
    pub async fn poll<D>(&mut self, device: &mut D) -> Result<Vec<CanTransmission>>
    where
        D: CanDevice + Send + ?Sized,
    {
        self.poll_at(device, Instant::now()).await
    }

    /// Deterministic poll with an explicit monotonic time.
    pub async fn poll_at<D>(&mut self, device: &mut D, now: Instant) -> Result<Vec<CanTransmission>>
    where
        D: CanDevice + Send + ?Sized,
    {
        if now < self.last_now {
            return Err(Error::Invalid(
                "CAN scheduler clock moved backwards".to_string(),
            ));
        }
        self.last_now = now;
        let ids: Vec<u32> = self.messages.keys().copied().collect();
        let mut sent = Vec::new();
        for id in ids {
            let enabled = self.message_enabled_by_id(id);
            let due = {
                let message = self.messages.get(&id).expect("ID collected from map");
                message.pending_triggers > 0
                    || (enabled
                        && message.period.is_some()
                        && message.next_due.is_some_and(|deadline| deadline <= now))
            };
            if !due {
                continue;
            }
            let (mut frame, context) = {
                let message = self.messages.get_mut(&id).expect("ID collected from map");
                let triggered = message.pending_triggers > 0;
                if triggered {
                    message.pending_triggers -= 1;
                }
                let scheduled_for = if triggered {
                    now
                } else {
                    message.next_due.unwrap_or(now)
                };
                if enabled {
                    if let (Some(period), Some(deadline)) = (message.period, message.next_due) {
                        message.next_due = Some(next_periodic_deadline(deadline, period, now)?);
                    }
                }
                let context = CanHookContext {
                    message_id: message.id,
                    message_name: message.name.clone(),
                    node_name: message.node.clone(),
                    cause: if triggered {
                        TransmissionCause::Triggered
                    } else {
                        TransmissionCause::Cyclic
                    },
                    scheduled_for,
                };
                (
                    CanFrame::new(
                        "",
                        message.id,
                        message.payload.clone(),
                        true,
                        message.frame_type,
                    ),
                    context,
                )
            };
            if let Some(hooks) = self.hooks.get_mut(&id) {
                for hook in &mut hooks.before {
                    (hook.callback)(&mut frame, &context).map_err(|error| Error::Hook {
                        phase: "before-send",
                        hook_id: hook.id.get(),
                        message: context.message_name.clone(),
                        reason: error.to_string(),
                    })?;
                }
            }
            let send_result = device.send(frame.id, &frame.data, frame.frame_type).await;
            let outcome = match &send_result {
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
            if let Some(hooks) = self.hooks.get_mut(&id) {
                for hook in &mut hooks.after {
                    (hook.callback)(&frame, &context, &outcome).map_err(|error| Error::Hook {
                        phase: "after-send",
                        hook_id: hook.id.get(),
                        message: context.message_name.clone(),
                        reason: error.to_string(),
                    })?;
                }
            }
            let bytes = send_result?;
            if bytes != frame.data.len() {
                return Err(Error::IncompleteTransmission {
                    bus: "CAN",
                    message: context.message_name,
                    expected: frame.data.len(),
                    actual: bytes,
                });
            }
            sent.push(CanTransmission {
                message_id: id,
                message_name: context.message_name,
                cause: context.cause,
                bytes,
            });
        }
        Ok(sent)
    }

    fn state_for(&self, message: &ScheduledMessage) -> CanMessageState {
        CanMessageState {
            id: message.id,
            name: message.name.clone(),
            node: message.node.clone(),
            period: message.period,
            enabled: self.message_enabled(message),
            payload: message.payload.clone(),
            frame_type: message.frame_type,
        }
    }

    fn message_enabled(&self, message: &ScheduledMessage) -> bool {
        message
            .enabled_override
            .unwrap_or_else(|| self.enabled_nodes.contains(&message.node))
    }

    fn message_enabled_by_id(&self, id: u32) -> bool {
        self.messages
            .get(&id)
            .is_some_and(|message| self.message_enabled(message))
    }

    fn message_enabled_by_id_checked(&self, id: u32) -> Result<bool> {
        self.ensure_message(id)?;
        Ok(self.message_enabled_by_id(id))
    }

    fn reset_message_deadline(&mut self, id: u32, enabled: bool) -> Result<()> {
        if let Some(message) = self.messages.get_mut(&id) {
            message.next_due = if enabled && message.period.is_some() {
                Some(
                    self.last_now
                        .checked_add(message.start_delay)
                        .ok_or_else(|| {
                            Error::Invalid(format!(
                                "start delay for CAN message {} exceeds the monotonic clock range",
                                message.name
                            ))
                        })?,
                )
            } else {
                None
            };
        }
        Ok(())
    }

    fn ensure_message(&self, id: u32) -> Result<()> {
        self.messages
            .contains_key(&id)
            .then_some(())
            .ok_or_else(|| Error::NotFound(format!("CAN message {id}")))
    }

    fn id_for_name(&self, name: &str) -> Result<u32> {
        self.message_id(name)
            .ok_or_else(|| Error::NotFound(format!("CAN message {name:?}")))
    }

    fn allocate_hook_id(&mut self) -> HookId {
        let id = HookId(self.next_hook_id);
        self.next_hook_id = self.next_hook_id.saturating_add(1);
        id
    }
}

fn numeric_attribute(dbc: &DBCFile, message: &MsgType, name: &str) -> Option<f64> {
    attribute_number(message.attributes.get(name)).or_else(|| {
        dbc.attribute_definitions.get(name).and_then(|definition| {
            match definition.default_value.as_ref() {
                Some(AttribDefault::Number(value)) => Some(*value),
                Some(AttribDefault::Text(value)) => value.parse().ok(),
                None => None,
            }
        })
    })
}

fn signal_numeric_attribute(dbc: &DBCFile, signal: &SignalType, name: &str) -> Option<f64> {
    attribute_number(signal.attributes.get(name)).or_else(|| {
        dbc.attribute_definitions.get(name).and_then(|definition| {
            match definition.default_value.as_ref() {
                Some(AttribDefault::Number(value)) => Some(*value),
                Some(AttribDefault::Text(value)) => value.parse().ok(),
                None => None,
            }
        })
    })
}

fn attribute_number(attribute: Option<&AttributeType>) -> Option<f64> {
    attribute.and_then(|attribute| attribute.value.parse().ok())
}

fn positive_duration_ms(value: f64, attribute: &str, message: &str) -> Result<Option<Duration>> {
    if value == 0.0 {
        return Ok(None);
    }
    Ok(Some(nonnegative_duration_ms(value, attribute, message)?))
}

fn nonnegative_duration_ms(value: f64, attribute: &str, message: &str) -> Result<Duration> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::Invalid(format!(
            "{attribute}={value} on CAN message {message}"
        )));
    }
    Duration::try_from_secs_f64(value / 1_000.0).map_err(|_| {
        Error::Invalid(format!(
            "{attribute}={value} on CAN message {message} is out of range"
        ))
    })
}

fn insert_signal_raw(payload: &mut [u8], signal: &SignalType, value: i128) -> Result<()> {
    if signal.len == 0 || signal.len > 64 {
        return Err(Error::Invalid(format!(
            "GenSigStartValue on signal {} has unsupported width {}",
            signal.name, signal.len
        )));
    }
    let mask = if signal.len == 64 {
        u64::MAX
    } else {
        (1_u64 << signal.len) - 1
    };
    let raw = (value as u64) & mask;
    if signal.is_little_endian() {
        let end = usize::from(signal.start) + usize::from(signal.len);
        if end > payload.len() * 8 {
            return Err(Error::Invalid(format!(
                "signal {} lies outside its CAN payload",
                signal.name
            )));
        }
        for index in 0..signal.len {
            let absolute = usize::from(signal.start + index);
            set_bit(payload, absolute, (raw >> index) & 1 != 0, false);
        }
    } else {
        let linear_start = usize::from((signal.start & !7) + (7 - signal.start % 8));
        let end = linear_start + usize::from(signal.len);
        if end > payload.len() * 8 {
            return Err(Error::Invalid(format!(
                "signal {} lies outside its CAN payload",
                signal.name
            )));
        }
        for index in 0..signal.len {
            let source_bit = u32::from(signal.len - 1 - index);
            set_bit(
                payload,
                linear_start + usize::from(index),
                (raw >> source_bit) & 1 != 0,
                true,
            );
        }
    }
    Ok(())
}

fn set_bit(payload: &mut [u8], absolute: usize, set: bool, msb_numbered: bool) {
    let bit = if msb_numbered {
        7 - absolute % 8
    } else {
        absolute % 8
    };
    let mask = 1_u8 << bit;
    if set {
        payload[absolute / 8] |= mask;
    } else {
        payload[absolute / 8] &= !mask;
    }
}

fn next_periodic_deadline(
    mut deadline: Instant,
    period: Duration,
    now: Instant,
) -> Result<Instant> {
    if deadline <= now {
        deadline = now.checked_add(period).ok_or_else(|| {
            Error::Invalid("CAN deadline exceeds the monotonic clock range".to_string())
        })?;
    }
    Ok(deadline)
}
