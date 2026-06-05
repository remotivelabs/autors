//! General-purpose BLF event objects.

use crate::error::Result;
use crate::objects::{
    parse_err, put_f64, put_u16, put_u32, put_u64, put_u8, LObj, ObjectFlags, ObjectType, Reader,
    LOBJ_SIZE,
};

fn checked_object(bytes: &[u8], base: u64, minimum_size: usize) -> Result<(LObj, &[u8])> {
    let header = LObj::parse(bytes, base)?;
    let object_size = header.base.object_size as usize;
    if object_size < minimum_size || object_size > bytes.len() {
        return parse_err(
            base,
            format!(
                "object size {object_size} is outside {minimum_size}..={}",
                bytes.len()
            ),
        );
    }
    Ok((header, &bytes[..object_size]))
}

fn split_lengths<'a>(bytes: &'a [u8], lengths: &[u32], base: u64) -> Result<Vec<&'a [u8]>> {
    let expected = lengths
        .iter()
        .try_fold(0usize, |total, &length| total.checked_add(length as usize));
    let Some(expected) = expected else {
        return parse_err(base, "variable-field lengths overflow addressable memory");
    };
    if expected > bytes.len() {
        return parse_err(
            base,
            format!(
                "variable fields declare {expected} bytes but only {} remain",
                bytes.len()
            ),
        );
    }
    let mut fields = Vec::with_capacity(lengths.len());
    let mut offset = 0;
    for &length in lengths {
        let end = offset + length as usize;
        fields.push(&bytes[offset..end]);
        offset = end;
    }
    fields.push(&bytes[offset..]);
    Ok(fields)
}

fn dynamic_size(fixed: usize, fields: &[&[u8]]) -> u32 {
    fields
        .iter()
        .fold(fixed, |size, field| size.saturating_add(field.len())) as u32
}

/// Application trigger window and flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTrigger {
    pub header: LObj,
    pub pre_trigger_time: u64,
    pub post_trigger_time: u64,
    pub channel: u16,
    pub flags: u16,
    pub app_specific: u32,
}

impl AppTrigger {
    pub const SIZE: usize = 56;

    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::AppTrigger,
                object_flags,
                timestamp,
                Self::SIZE as u32,
            ),
            pre_trigger_time: 0,
            post_trigger_time: 0,
            channel: 0,
            flags: 0,
            app_specific: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            pre_trigger_time: r.u64()?,
            post_trigger_time: r.u64()?,
            channel: r.u16()?,
            flags: r.u16()?,
            app_specific: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u64(out, self.pre_trigger_time);
        put_u64(out, self.post_trigger_time);
        put_u16(out, self.channel);
        put_u16(out, self.flags);
        put_u32(out, self.app_specific);
    }
}

/// An environment variable value. Its object type determines the value encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    pub header: LObj,
    pub reserved: u64,
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl EnvironmentVariable {
    pub const FIXED_SIZE: usize = 48;

    pub fn new(
        object_type: ObjectType,
        object_flags: ObjectFlags,
        timestamp: u64,
        name: impl Into<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<Self> {
        if !matches!(
            object_type,
            ObjectType::EnvInteger
                | ObjectType::EnvDouble
                | ObjectType::EnvString
                | ObjectType::EnvData
        ) {
            return parse_err(0, "environment variable requires an ENV_* object type");
        }
        let name = name.into();
        let size = dynamic_size(Self::FIXED_SIZE, &[&name, &data]);
        Ok(Self {
            header: LObj::new(object_type, object_flags, timestamp, size),
            reserved: 0,
            name,
            data,
            trailing: Vec::new(),
        })
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let name_length = r.u32()?;
        let data_length = r.u32()?;
        let reserved = r.u64()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[name_length, data_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            reserved,
            name: fields[0].to_vec(),
            data: fields[1].to_vec(),
            trailing: fields[2].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            dynamic_size(Self::FIXED_SIZE, &[&self.name, &self.data, &self.trailing]);
        header.write_to(out);
        put_u32(out, self.name.len() as u32);
        put_u32(out, self.data.len() as u32);
        put_u64(out, self.reserved);
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.trailing);
    }
}

/// Correlation between the logger clock and real time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeClock {
    pub header: LObj,
    pub time: u64,
    pub logging_offset: u64,
}

impl RealtimeClock {
    pub const SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            time: r.u64()?,
            logging_offset: r.u64()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u64(out, self.time);
        put_u64(out, self.logging_offset);
    }
}

/// Driver receive-buffer overrun information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOverrun {
    pub header: LObj,
    pub bus_type: u32,
    pub channel: u16,
    pub reserved: u16,
}

impl DriverOverrun {
    pub const SIZE: usize = 40;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            bus_type: r.u32()?,
            channel: r.u16()?,
            reserved: r.u16()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.bus_type);
        put_u16(out, self.channel);
        put_u16(out, self.reserved);
    }
}

/// A textual comment attached to an event type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventComment {
    pub header: LObj,
    pub commented_event_type: u32,
    pub reserved: u64,
    pub text: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl EventComment {
    pub const FIXED_SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let commented_event_type = r.u32()?;
        let text_length = r.u32()?;
        let reserved = r.u64()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[text_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            commented_event_type,
            reserved,
            text: fields[0].to_vec(),
            trailing: fields[1].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size = dynamic_size(Self::FIXED_SIZE, &[&self.text, &self.trailing]);
        header.write_to(out);
        put_u32(out, self.commented_event_type);
        put_u32(out, self.text.len() as u32);
        put_u64(out, self.reserved);
        out.extend_from_slice(&self.text);
        out.extend_from_slice(&self.trailing);
    }
}

/// A named, colored global marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalMarker {
    pub header: LObj,
    pub commented_event_type: u32,
    pub foreground_color: u32,
    pub background_color: u32,
    pub is_relocatable: u8,
    pub reserved1: u8,
    pub reserved2: u16,
    pub reserved3: u32,
    pub reserved4: u64,
    pub group_name: Vec<u8>,
    pub marker_name: Vec<u8>,
    pub description: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl GlobalMarker {
    pub const FIXED_SIZE: usize = 72;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let commented_event_type = r.u32()?;
        let foreground_color = r.u32()?;
        let background_color = r.u32()?;
        let is_relocatable = r.u8()?;
        let reserved1 = r.u8()?;
        let reserved2 = r.u16()?;
        let group_name_length = r.u32()?;
        let marker_name_length = r.u32()?;
        let description_length = r.u32()?;
        let reserved3 = r.u32()?;
        let reserved4 = r.u64()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[group_name_length, marker_name_length, description_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            commented_event_type,
            foreground_color,
            background_color,
            is_relocatable,
            reserved1,
            reserved2,
            reserved3,
            reserved4,
            group_name: fields[0].to_vec(),
            marker_name: fields[1].to_vec(),
            description: fields[2].to_vec(),
            trailing: fields[3].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let fields = [
            self.group_name.as_slice(),
            self.marker_name.as_slice(),
            self.description.as_slice(),
            self.trailing.as_slice(),
        ];
        let mut header = self.header.clone();
        header.base.object_size = dynamic_size(Self::FIXED_SIZE, &fields);
        header.write_to(out);
        put_u32(out, self.commented_event_type);
        put_u32(out, self.foreground_color);
        put_u32(out, self.background_color);
        put_u8(out, self.is_relocatable);
        put_u8(out, self.reserved1);
        put_u16(out, self.reserved2);
        put_u32(out, self.group_name.len() as u32);
        put_u32(out, self.marker_name.len() as u32);
        put_u32(out, self.description.len() as u32);
        put_u32(out, self.reserved3);
        put_u64(out, self.reserved4);
        for field in fields {
            out.extend_from_slice(field);
        }
    }
}

/// A GPS position, speed, and course sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpsEvent {
    pub header: LObj,
    pub flags: u32,
    pub channel: u16,
    pub reserved: u16,
    pub latitude: crate::objects::Float64,
    pub longitude: crate::objects::Float64,
    pub altitude: crate::objects::Float64,
    pub speed: crate::objects::Float64,
    pub course: crate::objects::Float64,
}

impl GpsEvent {
    pub const SIZE: usize = 80;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            flags: r.u32()?,
            channel: r.u16()?,
            reserved: r.u16()?,
            latitude: crate::objects::Float64::new(r.f64()?),
            longitude: crate::objects::Float64::new(r.f64()?),
            altitude: crate::objects::Float64::new(r.f64()?),
            speed: crate::objects::Float64::new(r.f64()?),
            course: crate::objects::Float64::new(r.f64()?),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.flags);
        put_u16(out, self.channel);
        put_u16(out, self.reserved);
        put_f64(out, self.latitude.value());
        put_f64(out, self.longitude.value());
        put_f64(out, self.altitude.value());
        put_f64(out, self.speed.value());
        put_f64(out, self.course.value());
    }
}

/// Start of a data-loss interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLostBegin {
    pub header: LObj,
    pub queue_identifier: u32,
}

impl DataLostBegin {
    pub const SIZE: usize = 36;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            queue_identifier: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.queue_identifier);
    }
}

/// End of a data-loss interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLostEnd {
    pub header: LObj,
    pub queue_identifier: u32,
    pub first_object_lost_timestamp: u64,
    pub number_of_lost_events: u32,
}

impl DataLostEnd {
    pub const SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            queue_identifier: r.u32()?,
            first_object_lost_timestamp: r.u64()?,
            number_of_lost_events: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.queue_identifier);
        put_u64(out, self.first_object_lost_timestamp);
        put_u32(out, self.number_of_lost_events);
    }
}

/// Queue high/low watermark transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaterMarkEvent {
    pub header: LObj,
    pub queue_state: u32,
    pub reserved: u32,
}

impl WaterMarkEvent {
    pub const SIZE: usize = 40;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = checked_object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            queue_state: r.u32()?,
            reserved: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.queue_state);
        put_u32(out, self.reserved);
    }
}

/// A trigger block and its textual condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerCondition {
    pub header: LObj,
    pub state: u32,
    pub trigger_block_name: Vec<u8>,
    pub trigger_condition: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl TriggerCondition {
    pub const FIXED_SIZE: usize = 44;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let state = r.u32()?;
        let name_length = r.u32()?;
        let condition_length = r.u32()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[name_length, condition_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            state,
            trigger_block_name: fields[0].to_vec(),
            trigger_condition: fields[1].to_vec(),
            trailing: fields[2].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let fields = [
            self.trigger_block_name.as_slice(),
            self.trigger_condition.as_slice(),
            self.trailing.as_slice(),
        ];
        let mut header = self.header.clone();
        header.base.object_size = dynamic_size(Self::FIXED_SIZE, &fields);
        header.write_to(out);
        put_u32(out, self.state);
        put_u32(out, self.trigger_block_name.len() as u32);
        put_u32(out, self.trigger_condition.len() as u32);
        for field in fields {
            out.extend_from_slice(field);
        }
    }
}

/// A distributed-object member value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedObjectMember {
    pub header: LObj,
    pub member_type: u32,
    pub detail_type: u32,
    pub path: Vec<u8>,
    pub data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl DistributedObjectMember {
    pub const FIXED_SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let member_type = r.u32()?;
        let detail_type = r.u32()?;
        let path_length = r.u32()?;
        let data_length = r.u32()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[path_length, data_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            member_type,
            detail_type,
            path: fields[0].to_vec(),
            data: fields[1].to_vec(),
            trailing: fields[2].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            dynamic_size(Self::FIXED_SIZE, &[&self.path, &self.data, &self.trailing]);
        header.write_to(out);
        put_u32(out, self.member_type);
        put_u32(out, self.detail_type);
        put_u32(out, self.path.len() as u32);
        put_u32(out, self.data.len() as u32);
        out.extend_from_slice(&self.path);
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.trailing);
    }
}

/// An attribute update for an attributable object member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeEvent {
    pub header: LObj,
    pub main_object_path: Vec<u8>,
    pub member_path: Vec<u8>,
    pub attribute_definition_path: Vec<u8>,
    pub data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl AttributeEvent {
    pub const FIXED_SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let lengths = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &lengths,
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            main_object_path: fields[0].to_vec(),
            member_path: fields[1].to_vec(),
            attribute_definition_path: fields[2].to_vec(),
            data: fields[3].to_vec(),
            trailing: fields[4].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let fields = [
            self.main_object_path.as_slice(),
            self.member_path.as_slice(),
            self.attribute_definition_path.as_slice(),
            self.data.as_slice(),
            self.trailing.as_slice(),
        ];
        let mut header = self.header.clone();
        header.base.object_size = dynamic_size(Self::FIXED_SIZE, &fields);
        header.write_to(out);
        put_u32(out, self.main_object_path.len() as u32);
        put_u32(out, self.member_path.len() as u32);
        put_u32(out, self.attribute_definition_path.len() as u32);
        put_u32(out, self.data.len() as u32);
        for field in fields {
            out.extend_from_slice(field);
        }
    }
}

/// A function-bus object and its opaque value data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionBus {
    pub header: LObj,
    pub function_bus_object_type: u32,
    pub value_entity_type: u32,
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl FunctionBus {
    pub const FIXED_SIZE: usize = 48;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let function_bus_object_type = r.u32()?;
        let value_entity_type = r.u32()?;
        let name_length = r.u32()?;
        let data_length = r.u32()?;
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &[name_length, data_length],
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            function_bus_object_type,
            value_entity_type,
            name: fields[0].to_vec(),
            data: fields[1].to_vec(),
            trailing: fields[2].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            dynamic_size(Self::FIXED_SIZE, &[&self.name, &self.data, &self.trailing]);
        header.write_to(out);
        put_u32(out, self.function_bus_object_type);
        put_u32(out, self.value_entity_type);
        put_u32(out, self.name.len() as u32);
        put_u32(out, self.data.len() as u32);
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.trailing);
    }
}

/// Diagnostic request interpretation and qualifier strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagRequestInterpretation {
    pub header: LObj,
    pub description_handle: u32,
    pub variant_handle: u32,
    pub service_handle: u32,
    pub ecu_qualifier: Vec<u8>,
    pub variant_qualifier: Vec<u8>,
    pub service_qualifier: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl DiagRequestInterpretation {
    pub const FIXED_SIZE: usize = 56;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, object) = checked_object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&object[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let description_handle = r.u32()?;
        let variant_handle = r.u32()?;
        let service_handle = r.u32()?;
        let lengths = [r.u32()?, r.u32()?, r.u32()?];
        let fields = split_lengths(
            &object[Self::FIXED_SIZE..],
            &lengths,
            base + Self::FIXED_SIZE as u64,
        )?;
        Ok(Self {
            header,
            description_handle,
            variant_handle,
            service_handle,
            ecu_qualifier: fields[0].to_vec(),
            variant_qualifier: fields[1].to_vec(),
            service_qualifier: fields[2].to_vec(),
            trailing: fields[3].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let fields = [
            self.ecu_qualifier.as_slice(),
            self.variant_qualifier.as_slice(),
            self.service_qualifier.as_slice(),
            self.trailing.as_slice(),
        ];
        let mut header = self.header.clone();
        header.base.object_size = dynamic_size(Self::FIXED_SIZE, &fields);
        header.write_to(out);
        put_u32(out, self.description_handle);
        put_u32(out, self.variant_handle);
        put_u32(out, self.service_handle);
        put_u32(out, self.ecu_qualifier.len() as u32);
        put_u32(out, self.variant_qualifier.len() as u32);
        put_u32(out, self.service_qualifier.len() as u32);
        for field in fields {
            out.extend_from_slice(field);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGS: ObjectFlags = ObjectFlags::TIME_ONE_NANS;

    #[test]
    fn environment_variable_roundtrip_preserves_extensions() {
        let mut value = EnvironmentVariable::new(
            ObjectType::EnvData,
            FLAGS,
            10,
            b"temperature".to_vec(),
            vec![1, 2, 3],
        )
        .unwrap();
        value.trailing = vec![0xAA, 0xBB];
        value.header.base.object_size =
            (EnvironmentVariable::FIXED_SIZE + value.name.len() + value.data.len() + 2) as u32;
        let mut bytes = Vec::new();
        value.write_to(&mut bytes);
        assert_eq!(EnvironmentVariable::parse(&bytes, 0).unwrap(), value);
    }

    #[test]
    fn app_trigger_has_reference_layout() {
        let mut trigger = AppTrigger::new(FLAGS, 20);
        trigger.pre_trigger_time = 5;
        trigger.post_trigger_time = 7;
        trigger.channel = 2;
        let mut bytes = Vec::new();
        trigger.write_to(&mut bytes);
        assert_eq!(bytes.len(), AppTrigger::SIZE);
        assert_eq!(AppTrigger::parse(&bytes, 0).unwrap(), trigger);
    }
}
