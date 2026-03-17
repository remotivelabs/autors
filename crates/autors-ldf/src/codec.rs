//! Encoding and decoding of LDF-defined frame payloads.

use crate::error::{Error, Result};
use crate::model::{EncodingValue, Ldf, Signal, SignalValue, UnconditionalFrame};
use indexmap::IndexMap;

/// Ordered signal-name/value map used by frame codecs.
pub type SignalValues = IndexMap<String, SignalValue>;

/// Value used for unoccupied frame bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Padding {
    /// Clear unoccupied bits.
    #[default]
    Zero,
    /// Set unoccupied bits.
    One,
}

impl Padding {
    const fn byte(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::One => 0xff,
        }
    }
}

/// Options for payload encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Padding for unoccupied bits.
    pub padding: Padding,
}

impl Ldf {
    /// Encodes raw integer/byte-array signal values into an unconditional frame.
    /// Missing values use the signals' declared initial values.
    pub fn encode_frame_raw(&self, frame_name: &str, values: &SignalValues) -> Result<Vec<u8>> {
        self.encode_frame_raw_with(frame_name, values, EncodeOptions::default())
    }

    /// Encodes raw values with explicit padding behavior.
    pub fn encode_frame_raw_with(
        &self,
        frame_name: &str,
        values: &SignalValues,
        options: EncodeOptions,
    ) -> Result<Vec<u8>> {
        let frame = self.frame_for_codec(frame_name)?;
        self.check_input_names(frame, values)?;
        let mut payload = vec![options.padding.byte(); usize::from(frame.length)];
        for placement in &frame.signals {
            let signal = self.signals.get(&placement.signal).ok_or_else(|| {
                Error::Invalid(format!(
                    "frame {} references missing signal {}",
                    frame.name, placement.signal
                ))
            })?;
            let value = values.get(&signal.name).unwrap_or(&signal.initial_value);
            write_signal(&mut payload, placement.bit_offset, signal, value)?;
        }
        Ok(payload)
    }

    /// Decodes raw signal values from an unconditional frame payload.
    pub fn decode_frame_raw(&self, frame_name: &str, payload: &[u8]) -> Result<SignalValues> {
        let frame = self.frame_for_codec(frame_name)?;
        check_payload_length(frame, payload)?;
        let mut values = IndexMap::new();
        for placement in &frame.signals {
            let signal = self.signals.get(&placement.signal).ok_or_else(|| {
                Error::Invalid(format!(
                    "frame {} references missing signal {}",
                    frame.name, placement.signal
                ))
            })?;
            values.insert(
                signal.name.clone(),
                read_signal(payload, placement.bit_offset, signal),
            );
        }
        Ok(values)
    }

    /// Converts human-readable values through each signal's assigned encoding
    /// type, then encodes the unconditional frame payload.
    pub fn encode_frame(&self, frame_name: &str, values: &SignalValues) -> Result<Vec<u8>> {
        self.encode_frame_with(frame_name, values, EncodeOptions::default())
    }

    /// Encodes converted values with explicit padding behavior.
    pub fn encode_frame_with(
        &self,
        frame_name: &str,
        values: &SignalValues,
        options: EncodeOptions,
    ) -> Result<Vec<u8>> {
        let frame = self.frame_for_codec(frame_name)?;
        self.check_input_names(frame, values)?;
        let mut raw = IndexMap::new();
        for (name, value) in values {
            let signal = self.signals.get(name).ok_or_else(|| {
                Error::Codec(format!("frame {frame_name} has no signal named {name}"))
            })?;
            raw.insert(name.clone(), self.encode_signal_value(signal, value)?);
        }
        self.encode_frame_raw_with(frame_name, &raw, options)
    }

    /// Decodes a payload and applies assigned signal encoding types.
    pub fn decode_frame(
        &self,
        frame_name: &str,
        payload: &[u8],
        keep_unit: bool,
    ) -> Result<SignalValues> {
        let raw = self.decode_frame_raw(frame_name, payload)?;
        let mut values = IndexMap::new();
        for (name, value) in raw {
            let signal = self.signals.get(&name).ok_or_else(|| {
                Error::Invalid(format!("decoded missing signal definition {name}"))
            })?;
            values.insert(name, self.decode_signal_value(signal, &value, keep_unit)?);
        }
        Ok(values)
    }

    fn frame_for_codec(&self, frame_name: &str) -> Result<&UnconditionalFrame> {
        self.unconditional_frames
            .get(frame_name)
            .ok_or_else(|| Error::NotFound(format!("unconditional frame {frame_name:?}")))
    }

    fn check_input_names(&self, frame: &UnconditionalFrame, values: &SignalValues) -> Result<()> {
        for name in values.keys() {
            if !frame
                .signals
                .iter()
                .any(|placement| &placement.signal == name)
            {
                return Err(Error::Codec(format!(
                    "frame {} has no signal named {name}",
                    frame.name
                )));
            }
        }
        Ok(())
    }

    fn encode_signal_value(&self, signal: &Signal, value: &SignalValue) -> Result<SignalValue> {
        let Some(encoding_name) = &signal.encoding_type else {
            return raw_value_for_signal(signal, value);
        };
        let encoding = self
            .signal_encoding_types
            .get(encoding_name)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "signal {} references missing encoding type {encoding_name}",
                    signal.name
                ))
            })?;
        for converter in &encoding.values {
            if let Ok(converted) = encode_with(converter, value, signal) {
                return raw_value_for_signal(signal, &converted);
            }
        }
        Err(Error::Codec(format!(
            "cannot encode {value:?} as {} for signal {}",
            encoding.name, signal.name
        )))
    }

    fn decode_signal_value(
        &self,
        signal: &Signal,
        value: &SignalValue,
        keep_unit: bool,
    ) -> Result<SignalValue> {
        let Some(encoding_name) = &signal.encoding_type else {
            return Ok(value.clone());
        };
        let encoding = self
            .signal_encoding_types
            .get(encoding_name)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "signal {} references missing encoding type {encoding_name}",
                    signal.name
                ))
            })?;
        for converter in &encoding.values {
            if let Ok(converted) = decode_with(converter, value, keep_unit) {
                return Ok(converted);
            }
        }
        Err(Error::Codec(format!(
            "cannot decode {value:?} as {} for signal {}",
            encoding.name, signal.name
        )))
    }
}

fn check_payload_length(frame: &UnconditionalFrame, payload: &[u8]) -> Result<()> {
    if payload.len() != usize::from(frame.length) {
        return Err(Error::Codec(format!(
            "frame {} requires {} bytes, got {}",
            frame.name,
            frame.length,
            payload.len()
        )));
    }
    Ok(())
}

fn raw_value_for_signal(signal: &Signal, value: &SignalValue) -> Result<SignalValue> {
    if signal.is_array() {
        match value {
            SignalValue::Bytes(bytes) if bytes.len() == usize::from(signal.width / 8) => {
                Ok(value.clone())
            }
            SignalValue::Integer(integer) if *integer >= 0 => {
                let width = usize::from(signal.width / 8);
                let integer = u64::try_from(*integer).map_err(|_| {
                    Error::Codec(format!(
                        "negative value {integer} for signal {}",
                        signal.name
                    ))
                })?;
                if signal.width < 64 && integer >= (1_u64 << signal.width) {
                    return Err(Error::Codec(format!(
                        "value {integer} does not fit signal {}:{}",
                        signal.name, signal.width
                    )));
                }
                let bytes = integer.to_be_bytes();
                Ok(SignalValue::Bytes(bytes[8 - width..].to_vec()))
            }
            _ => Err(Error::Codec(format!(
                "array signal {} requires {} bytes or a fitting integer",
                signal.name,
                signal.width / 8
            ))),
        }
    } else {
        match value {
            SignalValue::Integer(integer)
                if *integer >= 0 && (*integer as u128) < (1_u128 << signal.width) =>
            {
                Ok(value.clone())
            }
            _ => Err(Error::Codec(format!(
                "scalar signal {} requires a non-negative {}-bit integer",
                signal.name, signal.width
            ))),
        }
    }
}

fn write_signal(
    payload: &mut [u8],
    bit_offset: u16,
    signal: &Signal,
    value: &SignalValue,
) -> Result<()> {
    let value = raw_value_for_signal(signal, value)?;
    match value {
        SignalValue::Integer(integer) => {
            write_bits(payload, bit_offset, signal.width, integer as u64)
        }
        SignalValue::Bytes(bytes) => {
            for (index, byte) in bytes.into_iter().enumerate() {
                write_bits(payload, bit_offset + index as u16 * 8, 8, u64::from(byte));
            }
        }
        SignalValue::Float(_) | SignalValue::Text(_) => unreachable!(),
    }
    Ok(())
}

fn read_signal(payload: &[u8], bit_offset: u16, signal: &Signal) -> SignalValue {
    if signal.is_array() {
        let mut bytes = Vec::with_capacity(usize::from(signal.width / 8));
        for index in 0..signal.width / 8 {
            bytes.push(read_bits(payload, bit_offset + u16::from(index) * 8, 8) as u8);
        }
        SignalValue::Bytes(bytes)
    } else {
        SignalValue::Integer(read_bits(payload, bit_offset, signal.width) as i64)
    }
}

fn write_bits(payload: &mut [u8], offset: u16, width: u8, value: u64) {
    for index in 0..width {
        let absolute = usize::from(offset) + usize::from(index);
        let mask = 1_u8 << (absolute % 8);
        if value & (1_u64 << index) == 0 {
            payload[absolute / 8] &= !mask;
        } else {
            payload[absolute / 8] |= mask;
        }
    }
}

fn read_bits(payload: &[u8], offset: u16, width: u8) -> u64 {
    let mut value = 0_u64;
    for index in 0..width {
        let absolute = usize::from(offset) + usize::from(index);
        if payload[absolute / 8] & (1_u8 << (absolute % 8)) != 0 {
            value |= 1_u64 << index;
        }
    }
    value
}

fn encode_with(
    converter: &EncodingValue,
    value: &SignalValue,
    signal: &Signal,
) -> Result<SignalValue> {
    match converter {
        EncodingValue::Logical { raw, text } => {
            let matches = match (text, value) {
                (Some(expected), SignalValue::Text(actual)) => expected == actual,
                (None, SignalValue::Integer(actual)) => raw == actual,
                _ => false,
            };
            matches
                .then_some(SignalValue::Integer(*raw))
                .ok_or_else(|| Error::Codec("logical value does not match".to_string()))
        }
        EncodingValue::Physical {
            raw_min,
            raw_max,
            scale,
            offset,
            unit,
        } => {
            let physical = numeric_value(value, unit.as_deref())?;
            let raw = if *scale == 0.0 {
                offset.round_ties_even()
            } else {
                ((physical - offset) / scale).round_ties_even()
            };
            if !raw.is_finite() || raw < *raw_min as f64 || raw > *raw_max as f64 {
                return Err(Error::Codec(format!(
                    "physical value maps outside {raw_min}..={raw_max}"
                )));
            }
            Ok(SignalValue::Integer(raw as i64))
        }
        EncodingValue::Bcd => {
            let SignalValue::Integer(mut integer) = value else {
                return Err(Error::Codec("BCD input must be an integer".to_string()));
            };
            let digits = usize::from(signal.width / 8);
            if integer < 0 || (integer as u128) >= 10_u128.pow(digits as u32) {
                return Err(Error::Codec("BCD input is out of range".to_string()));
            }
            let mut bytes = vec![0; digits];
            for byte in bytes.iter_mut().rev() {
                *byte = (integer % 10) as u8;
                integer /= 10;
            }
            Ok(SignalValue::Bytes(bytes))
        }
        EncodingValue::Ascii => {
            let SignalValue::Text(text) = value else {
                return Err(Error::Codec("ASCII input must be text".to_string()));
            };
            if !text.is_ascii() || text.len() != usize::from(signal.width / 8) {
                return Err(Error::Codec(format!(
                    "ASCII signal {} requires exactly {} characters",
                    signal.name,
                    signal.width / 8
                )));
            }
            Ok(SignalValue::Bytes(text.as_bytes().to_vec()))
        }
    }
}

fn decode_with(
    converter: &EncodingValue,
    value: &SignalValue,
    keep_unit: bool,
) -> Result<SignalValue> {
    match converter {
        EncodingValue::Logical { raw, text } => {
            let integer = integer_value(value)?;
            if integer != *raw {
                return Err(Error::Codec("logical value does not match".to_string()));
            }
            Ok(text
                .as_ref()
                .map(|text| SignalValue::Text(text.clone()))
                .unwrap_or(SignalValue::Integer(integer)))
        }
        EncodingValue::Physical {
            raw_min,
            raw_max,
            scale,
            offset,
            unit,
        } => {
            let raw = integer_value(value)?;
            if raw < *raw_min || raw > *raw_max {
                return Err(Error::Codec(
                    "raw value is outside physical range".to_string(),
                ));
            }
            let physical = raw as f64 * scale + offset;
            if keep_unit {
                let unit = unit.as_deref().unwrap_or("");
                Ok(SignalValue::Text(if unit.is_empty() {
                    format!("{physical:.3}")
                } else {
                    format!("{physical:.3} {unit}")
                }))
            } else {
                Ok(SignalValue::Float(physical))
            }
        }
        EncodingValue::Bcd => {
            let SignalValue::Bytes(bytes) = value else {
                return Err(Error::Codec("BCD value is not a byte array".to_string()));
            };
            let mut output = 0_i64;
            for byte in bytes {
                if *byte > 9 {
                    return Err(Error::Codec("BCD digit is greater than 9".to_string()));
                }
                output = output * 10 + i64::from(*byte);
            }
            Ok(SignalValue::Integer(output))
        }
        EncodingValue::Ascii => {
            let SignalValue::Bytes(bytes) = value else {
                return Err(Error::Codec("ASCII value is not a byte array".to_string()));
            };
            if !bytes.is_ascii() {
                return Err(Error::Codec(
                    "ASCII value contains non-ASCII bytes".to_string(),
                ));
            }
            Ok(SignalValue::Text(
                String::from_utf8(bytes.clone())
                    .map_err(|_| Error::Codec("invalid ASCII bytes".to_string()))?,
            ))
        }
    }
}

fn numeric_value(value: &SignalValue, unit: Option<&str>) -> Result<f64> {
    match value {
        SignalValue::Integer(value) => Ok(*value as f64),
        SignalValue::Float(value) => Ok(*value),
        SignalValue::Text(text) => {
            let number = if let Some(unit) = unit {
                text.strip_suffix(unit)
                    .ok_or_else(|| {
                        Error::Codec(format!("value {text:?} does not end with {unit:?}"))
                    })?
                    .trim()
            } else {
                text.as_str()
            };
            number
                .parse()
                .map_err(|_| Error::Codec(format!("value {text:?} is not numeric")))
        }
        SignalValue::Bytes(_) => Err(Error::Codec("byte array is not numeric".to_string())),
    }
}

fn integer_value(value: &SignalValue) -> Result<i64> {
    match value {
        SignalValue::Integer(value) => Ok(*value),
        SignalValue::Bytes(bytes) if bytes.len() <= 8 => Ok(bytes
            .iter()
            .fold(0_i64, |value, byte| (value << 8) | i64::from(*byte))),
        _ => Err(Error::Codec("value is not an integer".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LinVersion, MasterNode, SignalPlacement};
    use std::time::Duration;

    fn network() -> Ldf {
        let mut signals = IndexMap::new();
        signals.insert(
            "Byte".to_string(),
            Signal::new("Byte", 8, SignalValue::Integer(0xff)).unwrap(),
        );
        signals.insert(
            "Nibble".to_string(),
            Signal::new("Nibble", 4, SignalValue::Integer(0xf)).unwrap(),
        );
        signals.insert(
            "Flag".to_string(),
            Signal::new("Flag", 1, SignalValue::Integer(1)).unwrap(),
        );
        let frame = UnconditionalFrame {
            name: "Frame".to_string(),
            id: 1,
            publisher: "Master".to_string(),
            length: 2,
            signals: vec![
                SignalPlacement {
                    signal: "Byte".to_string(),
                    bit_offset: 0,
                },
                SignalPlacement {
                    signal: "Nibble".to_string(),
                    bit_offset: 8,
                },
                SignalPlacement {
                    signal: "Flag".to_string(),
                    bit_offset: 15,
                },
            ],
        };
        Ldf {
            protocol_version: LinVersion::LIN_2_2,
            language_version: LinVersion::LIN_2_2,
            baud_rate: 19_200,
            channel_name: None,
            file_revision: None,
            signal_byte_order: None,
            master: MasterNode {
                name: "Master".to_string(),
                time_base: Duration::ZERO,
                jitter: Duration::ZERO,
                max_header_length_bits: None,
                response_tolerance: None,
            },
            slaves: IndexMap::new(),
            signals,
            diagnostic_signals: IndexMap::new(),
            unconditional_frames: [(frame.name.clone(), frame)].into_iter().collect(),
            sporadic_frames: IndexMap::new(),
            event_triggered_frames: IndexMap::new(),
            diagnostic_frames: IndexMap::new(),
            diagnostic_addresses: IndexMap::new(),
            node_compositions: Vec::new(),
            schedule_tables: IndexMap::new(),
            signal_groups: IndexMap::new(),
            signal_encoding_types: IndexMap::new(),
            comments: Vec::new(),
            has_node_attributes: false,
        }
    }

    #[test]
    fn raw_codec_and_padding() {
        let network = network();
        let mut values = SignalValues::new();
        values.insert("Nibble".to_string(), SignalValue::Integer(10));
        values.insert("Flag".to_string(), SignalValue::Integer(1));
        assert_eq!(
            network.encode_frame_raw("Frame", &values).unwrap(),
            [0xff, 0x8a]
        );
        let payload = network
            .encode_frame_raw_with(
                "Frame",
                &values,
                EncodeOptions {
                    padding: Padding::One,
                },
            )
            .unwrap();
        assert_eq!(payload, [0xff, 0xfa]);
        let decoded = network.decode_frame_raw("Frame", &[0x64, 0x8a]).unwrap();
        assert_eq!(decoded["Byte"], SignalValue::Integer(0x64));
        assert_eq!(decoded["Nibble"], SignalValue::Integer(10));
        assert_eq!(decoded["Flag"], SignalValue::Integer(1));
    }

    #[test]
    fn converters_cover_physical_logical_bcd_and_ascii() {
        let signal = Signal::new("A", 24, SignalValue::Bytes(vec![0; 3])).unwrap();
        assert_eq!(
            encode_with(&EncodingValue::Bcd, &SignalValue::Integer(123), &signal).unwrap(),
            SignalValue::Bytes(vec![1, 2, 3])
        );
        assert_eq!(
            decode_with(
                &EncodingValue::Bcd,
                &SignalValue::Bytes(vec![1, 2, 3]),
                false
            )
            .unwrap(),
            SignalValue::Integer(123)
        );
        assert_eq!(
            encode_with(
                &EncodingValue::Ascii,
                &SignalValue::Text("ABC".to_string()),
                &signal
            )
            .unwrap(),
            SignalValue::Bytes(b"ABC".to_vec())
        );
    }
}
