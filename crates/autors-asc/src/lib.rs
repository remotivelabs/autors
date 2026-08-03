//! Vector ASC CAN trace reader and writer.
//!
//! [`AscFile`] keeps header information and timestamped records in memory.
//! Classic CAN and CAN FD data frames can be converted to the shared
//! [`autors_can::frame::CanFrame`] representation. Remote and error frames
//! remain available in the ASC-specific object model because `CanFrame` does
//! not carry those states.

mod error;

pub use error::{Error, Result};

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use autors_can::device::{CAN_EXT_FLAG, CAN_EXT_ID_MASK};
use autors_can::frame::{CanFrame, FrameType};

/// Numeric base selected by the ASC header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NumberBase {
    /// Hexadecimal identifiers, DLC values, and payload bytes.
    #[default]
    Hex,
    /// Decimal identifiers, DLC values, and payload bytes.
    Decimal,
}

impl NumberBase {
    fn radix(self) -> u32 {
        match self {
            Self::Hex => 16,
            Self::Decimal => 10,
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Decimal => "dec",
        }
    }
}

/// Meaning of record timestamps in the textual file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimestampMode {
    /// Each timestamp is an offset from the trigger-block start.
    #[default]
    Absolute,
    /// Each timestamp is a delta from the preceding event.
    Relative,
}

impl TimestampMode {
    fn keyword(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Relative => "relative",
        }
    }
}

/// Receive/transmit direction recorded by CANoe or CANalyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Frame received from the bus.
    Rx,
    /// Frame transmitted by the logging node.
    Tx,
}

impl Direction {
    fn parse(value: &str, line: usize) -> Result<Self> {
        if value.eq_ignore_ascii_case("rx") {
            Ok(Self::Rx)
        } else if value.eq_ignore_ascii_case("tx") {
            Ok(Self::Tx)
        } else {
            parse_err(line, format!("unknown CAN direction {value:?}"))
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Rx => "Rx",
            Self::Tx => "Tx",
        }
    }
}

/// ASC header fields that affect message interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AscHeader {
    /// Date text following the leading `date` keyword.
    pub date: Option<String>,
    /// Number base used by identifier, DLC, and byte columns.
    pub number_base: NumberBase,
    /// On-file timestamp interpretation.
    pub timestamp_mode: TimestampMode,
    /// Whether the header declares that internal events were logged.
    pub internal_events_logged: bool,
    /// Date text following `Begin Triggerblock`, when present.
    pub trigger_date: Option<String>,
}

impl Default for AscHeader {
    fn default() -> Self {
        Self {
            date: Some("Thu Jan 01 00:00:00.000 1970".to_owned()),
            number_base: NumberBase::Hex,
            timestamp_mode: TimestampMode::Absolute,
            internal_events_logged: true,
            trigger_date: None,
        }
    }
}

/// A classic CAN or CAN FD message record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AscMessage {
    /// Elapsed time from the beginning of the trigger block.
    pub timestamp: Duration,
    /// One-based ASC channel number.
    pub channel: u32,
    /// CAN identifier; [`CAN_EXT_FLAG`] marks a 29-bit identifier.
    pub id: u32,
    /// Receive/transmit direction.
    pub direction: Direction,
    /// Classic CAN, CAN FD, or CAN FD with bit-rate switching.
    pub frame_type: FrameType,
    /// Raw CAN DLC nibble as recorded in the file.
    pub dlc: u8,
    /// Payload bytes. Remote frames have an empty payload.
    pub data: Vec<u8>,
    /// Whether this is a remote-request frame.
    pub is_remote: bool,
    /// CAN FD error-state-indicator bit.
    pub error_state_indicator: bool,
    /// Optional symbolic frame name present on some CAN FD lines.
    pub symbolic_name: Option<String>,
    /// Remaining CAN FD columns after the payload. They are preserved on
    /// read/write so vendor-specific timing values are not discarded.
    pub fd_metadata: Vec<String>,
}

impl AscMessage {
    /// Converts a data frame to the shared CAN representation. Remote frames
    /// return `None` because `CanFrame` has no remote-frame flag.
    pub fn to_can_frame(&self) -> Option<CanFrame> {
        if self.is_remote {
            return None;
        }
        let mut frame = CanFrame::new(
            self.channel.to_string(),
            self.id,
            self.data.clone(),
            self.direction == Direction::Tx,
            self.frame_type,
        );
        frame.elapsed = self.timestamp;
        Some(frame)
    }

    /// Builds an ASC data record from a shared CAN frame.
    pub fn from_can_frame(frame: &CanFrame, channel: u32) -> Result<Self> {
        if channel == 0 {
            return Err(Error::Write("ASC channel numbers are one-based".to_owned()));
        }
        let maximum = if frame.frame_type.is_classic() { 8 } else { 64 };
        if frame.data.len() > maximum {
            return Err(Error::Write(format!(
                "{} frame payload length {} exceeds {maximum}",
                if frame.frame_type.is_classic() {
                    "classic CAN"
                } else {
                    "CAN FD"
                },
                frame.data.len()
            )));
        }
        if frame.raw_id() > CAN_EXT_ID_MASK {
            return Err(Error::Write(format!(
                "CAN identifier {:#X} exceeds 29 bits",
                frame.raw_id()
            )));
        }
        Ok(Self {
            timestamp: frame.elapsed,
            channel,
            id: frame.id,
            direction: if frame.is_master_frame {
                Direction::Tx
            } else {
                Direction::Rx
            },
            frame_type: frame.frame_type,
            dlc: length_to_dlc(frame.data.len()),
            data: frame.data.clone(),
            is_remote: false,
            error_state_indicator: false,
            symbolic_name: None,
            fd_metadata: Vec::new(),
        })
    }
}

/// A classic or CAN FD error-frame notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AscErrorFrame {
    /// Elapsed time from the beginning of the trigger block.
    pub timestamp: Duration,
    /// One-based ASC channel number.
    pub channel: u32,
    /// Direction when supplied by the CAN FD form.
    pub direction: Option<Direction>,
    /// Whether the line uses the CAN FD syntax.
    pub is_fd: bool,
    /// Uninterpreted fields following `ErrorFrame`.
    pub metadata: Vec<String>,
}

/// A timestamped ASC event not represented as a CAN frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AscEvent {
    /// Elapsed time from the beginning of the trigger block.
    pub timestamp: Duration,
    /// Event text after the timestamp.
    pub text: String,
}

/// One timestamped ASC record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AscRecord {
    /// Classic CAN or CAN FD message.
    Message(AscMessage),
    /// CAN error-frame notification.
    ErrorFrame(AscErrorFrame),
    /// Other timestamped event.
    Event(AscEvent),
}

impl AscRecord {
    /// Elapsed time from the beginning of the trigger block.
    pub fn timestamp(&self) -> Duration {
        match self {
            Self::Message(record) => record.timestamp,
            Self::ErrorFrame(record) => record.timestamp,
            Self::Event(record) => record.timestamp,
        }
    }
}

/// In-memory Vector ASC file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AscFile {
    /// File and trigger-block metadata.
    pub header: AscHeader,
    /// Timestamped records in file order.
    pub records: Vec<AscRecord>,
}

impl AscFile {
    /// Creates an empty ASC file with a deterministic epoch header.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses ASC text.
    pub fn parse(input: &str) -> Result<Self> {
        let mut file = Self::new();
        file.records.reserve(input.len() / 64);
        let mut last_timestamp = Duration::ZERO;

        for (index, raw_line) in input.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            if let Some(value) = strip_ascii_prefix(line, "date ") {
                file.header.date = Some(value.trim().to_owned());
                continue;
            }
            if starts_ascii(line, "base ") {
                parse_base_header(line, line_number, &mut file.header)?;
                continue;
            }
            if line.eq_ignore_ascii_case("internal events logged") {
                file.header.internal_events_logged = true;
                continue;
            }
            if line.eq_ignore_ascii_case("no internal events logged") {
                file.header.internal_events_logged = false;
                continue;
            }
            if let Some(value) = strip_ascii_prefix(line, "begin triggerblock ") {
                file.header.trigger_date = Some(value.trim().to_owned());
                last_timestamp = Duration::ZERO;
                continue;
            }
            if line.eq_ignore_ascii_case("end triggerblock") {
                continue;
            }

            let Some((timestamp_text, body)) = split_first(line) else {
                continue;
            };
            let Ok(on_file_seconds) = timestamp_text.parse::<f64>() else {
                // Header/event forms not understood by this crate are ignored.
                continue;
            };
            let on_file_timestamp = duration_from_seconds(on_file_seconds, line_number)?;
            let timestamp = match file.header.timestamp_mode {
                TimestampMode::Absolute => on_file_timestamp,
                TimestampMode::Relative => last_timestamp
                    .checked_add(on_file_timestamp)
                    .ok_or_else(|| Error::Parse {
                        line: line_number,
                        message: "relative timestamp overflow".to_owned(),
                    })?,
            };
            last_timestamp = timestamp;

            if body.eq_ignore_ascii_case("start of measurement") {
                continue;
            }
            file.records.push(parse_record(
                timestamp,
                body,
                file.header.number_base,
                line_number,
            )?);
        }
        Ok(file)
    }

    /// Reads and parses an ASC file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Serializes this file as CANoe/CANalyzer-compatible ASC text.
    pub fn write(&self) -> Result<String> {
        let mut output = String::new();
        if let Some(date) = &self.header.date {
            writeln!(output, "date {date}").map_err(format_error)?;
        }
        writeln!(
            output,
            "base {}  timestamps {}",
            self.header.number_base.keyword(),
            self.header.timestamp_mode.keyword()
        )
        .map_err(format_error)?;
        writeln!(
            output,
            "{}internal events logged",
            if self.header.internal_events_logged {
                ""
            } else {
                "no "
            }
        )
        .map_err(format_error)?;
        let trigger_date = self
            .header
            .trigger_date
            .as_ref()
            .or(self.header.date.as_ref())
            .map(String::as_str)
            .unwrap_or("Thu Jan 01 00:00:00.000 1970");
        writeln!(output, "Begin Triggerblock {trigger_date}").map_err(format_error)?;
        writeln!(output, " 0.000000 Start of measurement").map_err(format_error)?;

        let mut previous = Duration::ZERO;
        for record in &self.records {
            let elapsed = record.timestamp();
            let written = match self.header.timestamp_mode {
                TimestampMode::Absolute => elapsed,
                TimestampMode::Relative => elapsed.checked_sub(previous).ok_or_else(|| {
                    Error::Write("records are not ordered by timestamp".to_owned())
                })?,
            };
            previous = elapsed;
            write!(output, "{:>9.6} ", written.as_secs_f64()).map_err(format_error)?;
            write_record(&mut output, record, self.header.number_base)?;
            output.push('\n');
        }
        output.push_str("End TriggerBlock\n");
        Ok(output)
    }

    /// Writes this file to disk.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.write()?)?;
        Ok(())
    }

    /// Adds a shared CAN data frame on the given one-based ASC channel.
    pub fn add_can_frame(&mut self, frame: &CanFrame, channel: u32) -> Result<()> {
        self.records
            .push(AscRecord::Message(AscMessage::from_can_frame(
                frame, channel,
            )?));
        Ok(())
    }

    /// Returns all non-remote data frames in file order.
    pub fn can_frames(&self) -> Vec<CanFrame> {
        self.records
            .iter()
            .filter_map(|record| match record {
                AscRecord::Message(message) => message.to_can_frame(),
                _ => None,
            })
            .collect()
    }
}

fn parse_record(
    timestamp: Duration,
    body: &str,
    number_base: NumberBase,
    line: usize,
) -> Result<AscRecord> {
    // A CAN FD line with a full 64-byte payload still fits inline. Avoid a
    // heap allocation for the token pointer list on the common parse path,
    // while retaining a spill vector for vendor-specific trailing columns.
    let mut inline_tokens = [""; 96];
    let mut token_count = 0;
    let mut spill = Vec::new();
    for token in body.split_whitespace() {
        if token_count < inline_tokens.len() {
            inline_tokens[token_count] = token;
        } else {
            if spill.is_empty() {
                spill.extend_from_slice(&inline_tokens);
            }
            spill.push(token);
        }
        token_count += 1;
    }
    let tokens = if spill.is_empty() {
        &inline_tokens[..token_count]
    } else {
        spill.as_slice()
    };
    if tokens.is_empty() {
        return parse_err(line, "timestamp without an event");
    }
    if tokens[0].eq_ignore_ascii_case("canfd") {
        parse_fd_record(timestamp, &tokens[1..], number_base, line)
    } else if let Ok(channel) = tokens[0].parse::<u32>() {
        let fields = &tokens[1..];
        let looks_like_can = fields
            .first()
            .is_some_and(|value| value.eq_ignore_ascii_case("errorframe"))
            || fields.get(1).is_some_and(|value| {
                value.eq_ignore_ascii_case("rx") || value.eq_ignore_ascii_case("tx")
            });
        if looks_like_can {
            parse_classic_record(timestamp, channel, fields, number_base, line)
        } else {
            Ok(AscRecord::Event(AscEvent {
                timestamp,
                text: body.to_owned(),
            }))
        }
    } else {
        Ok(AscRecord::Event(AscEvent {
            timestamp,
            text: body.to_owned(),
        }))
    }
}

fn parse_classic_record(
    timestamp: Duration,
    channel: u32,
    tokens: &[&str],
    base: NumberBase,
    line: usize,
) -> Result<AscRecord> {
    if channel == 0 {
        return parse_err(line, "ASC channel numbers are one-based");
    }
    if tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("errorframe"))
    {
        return Ok(AscRecord::ErrorFrame(AscErrorFrame {
            timestamp,
            channel,
            direction: None,
            is_fd: false,
            metadata: tokens[1..]
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }));
    }
    if tokens.len() < 4 {
        return parse_err(line, "classic CAN record has fewer than four columns");
    }
    let id = parse_id(tokens[0], base, line)?;
    let direction = Direction::parse(tokens[1], line)?;
    let kind = tokens[2];
    let dlc_value = parse_u32(tokens[3], base, "DLC", line)?;
    let dlc = u8::try_from(dlc_value)
        .map_err(|_| parse_error(line, format!("DLC {dlc_value} exceeds u8")))?;
    let is_remote = kind.eq_ignore_ascii_case("r");
    if !is_remote && !kind.eq_ignore_ascii_case("d") {
        return parse_err(line, format!("unknown classic CAN frame kind {kind:?}"));
    }
    let data_length = if is_remote {
        0
    } else {
        usize::from(dlc.min(8))
    };
    if tokens.len() < 4 + data_length {
        return parse_err(
            line,
            format!(
                "classic CAN record declares {data_length} data bytes but only {} are present",
                tokens.len().saturating_sub(4)
            ),
        );
    }
    let data = parse_bytes(&tokens[4..4 + data_length], base, line)?;
    Ok(AscRecord::Message(AscMessage {
        timestamp,
        channel,
        id,
        direction,
        frame_type: FrameType::CAN20B,
        dlc,
        data,
        is_remote,
        error_state_indicator: false,
        symbolic_name: None,
        fd_metadata: Vec::new(),
    }))
}

fn parse_fd_record(
    timestamp: Duration,
    tokens: &[&str],
    base: NumberBase,
    line: usize,
) -> Result<AscRecord> {
    if tokens.len() < 3 {
        return parse_err(line, "CAN FD record has fewer than three columns");
    }
    let channel = tokens[0]
        .parse::<u32>()
        .map_err(|_| parse_error(line, format!("invalid CAN FD channel {:?}", tokens[0])))?;
    if channel == 0 {
        return parse_err(line, "ASC channel numbers are one-based");
    }
    let direction = Direction::parse(tokens[1], line)?;
    if tokens[2].eq_ignore_ascii_case("errorframe") {
        return Ok(AscRecord::ErrorFrame(AscErrorFrame {
            timestamp,
            channel,
            direction: Some(direction),
            is_fd: true,
            metadata: tokens[3..]
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }));
    }
    if tokens.len() < 7 {
        return parse_err(line, "CAN FD data record has too few columns");
    }
    let id = parse_id(tokens[2], base, line)?;
    let (symbolic_name, controls_start) = if is_bit(tokens[3]) {
        (None, 3)
    } else {
        if tokens.len() < 8 {
            return parse_err(line, "CAN FD record ends after its symbolic name");
        }
        (Some(tokens[3].to_owned()), 4)
    };
    let brs = parse_bit(tokens[controls_start], "BRS", line)?;
    let esi = parse_bit(tokens[controls_start + 1], "ESI", line)?;
    let dlc_value = parse_u32(tokens[controls_start + 2], base, "DLC", line)?;
    let dlc = u8::try_from(dlc_value)
        .map_err(|_| parse_error(line, format!("DLC {dlc_value} exceeds u8")))?;
    let data_length = tokens[controls_start + 3].parse::<usize>().map_err(|_| {
        parse_error(
            line,
            format!(
                "invalid CAN FD data length {:?}",
                tokens[controls_start + 3]
            ),
        )
    })?;
    if data_length > 64 {
        return parse_err(line, format!("CAN FD data length {data_length} exceeds 64"));
    }
    let data_start = controls_start + 4;
    if tokens.len() < data_start + data_length {
        return parse_err(
            line,
            format!(
                "CAN FD record declares {data_length} data bytes but only {} are present",
                tokens.len().saturating_sub(data_start)
            ),
        );
    }
    let data = parse_bytes(&tokens[data_start..data_start + data_length], base, line)?;
    let fd_metadata = tokens[data_start + data_length..]
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    Ok(AscRecord::Message(AscMessage {
        timestamp,
        channel,
        id,
        direction,
        frame_type: if brs {
            FrameType::FD_BRS
        } else {
            FrameType::FD
        },
        dlc,
        data,
        is_remote: data_length == 0,
        error_state_indicator: esi,
        symbolic_name,
        fd_metadata,
    }))
}

fn write_record(output: &mut String, record: &AscRecord, base: NumberBase) -> Result<()> {
    match record {
        AscRecord::Message(message) if message.frame_type.is_classic() => {
            validate_message(message)?;
            let id = format_id(message.id, base);
            let marker = if message.is_remote { 'r' } else { 'd' };
            write!(
                output,
                "{}  {:<15} {:<4} {marker} {}",
                message.channel,
                id,
                message.direction.as_str(),
                format_number(u32::from(message.dlc), base)
            )
            .map_err(format_error)?;
            for byte in &message.data {
                write!(output, " {}", format_byte(*byte, base)).map_err(format_error)?;
            }
        }
        AscRecord::Message(message) => {
            validate_message(message)?;
            let id = format_id(message.id, base);
            write!(
                output,
                "CANFD {:>3} {:<4} {:>8}  ",
                message.channel,
                message.direction.as_str(),
                id
            )
            .map_err(format_error)?;
            if let Some(name) = &message.symbolic_name {
                write!(output, "{name} ").map_err(format_error)?;
            }
            write!(
                output,
                "{} {} {} {:>2}",
                u8::from(message.frame_type.contains(FrameType::BRS)),
                u8::from(message.error_state_indicator),
                format_number(u32::from(message.dlc), base),
                message.data.len()
            )
            .map_err(format_error)?;
            for byte in &message.data {
                write!(output, " {}", format_byte(*byte, base)).map_err(format_error)?;
            }
            if message.fd_metadata.is_empty() {
                let mut flags = 1u32 << 12;
                if message.frame_type.contains(FrameType::BRS) {
                    flags |= 1 << 13;
                }
                if message.error_state_indicator {
                    flags |= 1 << 14;
                }
                write!(output, " 0 0 {flags:X} 0 0 0 0 0").map_err(format_error)?;
            } else {
                for value in &message.fd_metadata {
                    write!(output, " {value}").map_err(format_error)?;
                }
            }
        }
        AscRecord::ErrorFrame(error) => {
            if error.channel == 0 {
                return Err(Error::Write("ASC channel numbers are one-based".to_owned()));
            }
            if error.is_fd {
                write!(
                    output,
                    "CANFD {:>3} {:<4} ErrorFrame",
                    error.channel,
                    error.direction.unwrap_or(Direction::Rx).as_str()
                )
                .map_err(format_error)?;
            } else {
                write!(output, "{}  ErrorFrame", error.channel).map_err(format_error)?;
            }
            for value in &error.metadata {
                write!(output, " {value}").map_err(format_error)?;
            }
        }
        AscRecord::Event(event) => {
            if event.text.contains('\r') || event.text.contains('\n') {
                return Err(Error::Write(
                    "an ASC event must not contain a line break".to_owned(),
                ));
            }
            output.push_str(&event.text);
        }
    }
    Ok(())
}

fn validate_message(message: &AscMessage) -> Result<()> {
    if message.channel == 0 {
        return Err(Error::Write("ASC channel numbers are one-based".to_owned()));
    }
    let raw_id = message.id & CAN_EXT_ID_MASK;
    if message.id & !(CAN_EXT_FLAG | CAN_EXT_ID_MASK) != 0 {
        return Err(Error::Write(format!(
            "CAN identifier {:#X} contains unsupported flag or identifier bits",
            message.id
        )));
    }
    if message.id & CAN_EXT_FLAG == 0 && raw_id > 0x7ff {
        return Err(Error::Write(format!(
            "standard CAN identifier 0x{raw_id:X} exceeds 11 bits"
        )));
    }
    let maximum = if message.frame_type.is_classic() {
        8
    } else {
        64
    };
    if message.data.len() > maximum {
        return Err(Error::Write(format!(
            "CAN payload length {} exceeds {maximum}",
            message.data.len()
        )));
    }
    if !message.frame_type.is_classic() && message.dlc > 15 {
        return Err(Error::Write(format!(
            "CAN FD DLC {} exceeds 15",
            message.dlc
        )));
    }
    if message.is_remote && !message.data.is_empty() {
        return Err(Error::Write(
            "a remote CAN frame must not carry data bytes".to_owned(),
        ));
    }
    Ok(())
}

fn parse_base_header(line_text: &str, line: usize, header: &mut AscHeader) -> Result<()> {
    let tokens: Vec<&str> = line_text.split_whitespace().collect();
    if tokens.len() < 2 {
        return parse_err(line, "base header is missing its number base");
    }
    header.number_base = if tokens[1].eq_ignore_ascii_case("hex") {
        NumberBase::Hex
    } else if tokens[1].eq_ignore_ascii_case("dec") {
        NumberBase::Decimal
    } else {
        return parse_err(line, format!("unknown ASC number base {:?}", tokens[1]));
    };
    if let Some(position) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("timestamps"))
    {
        let Some(mode) = tokens.get(position + 1) else {
            return parse_err(line, "timestamps header is missing its mode");
        };
        header.timestamp_mode = if mode.eq_ignore_ascii_case("absolute") {
            TimestampMode::Absolute
        } else if mode.eq_ignore_ascii_case("relative") {
            TimestampMode::Relative
        } else {
            return parse_err(line, format!("unknown timestamp mode {mode:?}"));
        };
    }
    Ok(())
}

fn parse_id(value: &str, base: NumberBase, line: usize) -> Result<u32> {
    let (digits, extended) = match value.as_bytes().last() {
        Some(b'x' | b'X') => (&value[..value.len() - 1], true),
        _ => (value, false),
    };
    let raw = parse_u32(digits, base, "CAN identifier", line)?;
    if raw > CAN_EXT_ID_MASK {
        return parse_err(line, format!("CAN identifier {value:?} exceeds 29 bits"));
    }
    if !extended && raw > 0x7ff {
        return parse_err(
            line,
            format!("standard CAN identifier {value:?} exceeds 11 bits"),
        );
    }
    Ok(raw | if extended { CAN_EXT_FLAG } else { 0 })
}

fn parse_bytes(values: &[&str], base: NumberBase, line: usize) -> Result<Vec<u8>> {
    values
        .iter()
        .map(|value| {
            let parsed = parse_u32(value, base, "CAN data byte", line)?;
            u8::try_from(parsed)
                .map_err(|_| parse_error(line, format!("CAN data byte {value:?} exceeds 0xFF")))
        })
        .collect()
}

fn parse_u32(value: &str, base: NumberBase, what: &str, line: usize) -> Result<u32> {
    u32::from_str_radix(value, base.radix())
        .map_err(|_| parse_error(line, format!("invalid {what} {value:?}")))
}

fn is_bit(value: &str) -> bool {
    value == "0" || value == "1"
}

fn parse_bit(value: &str, what: &str, line: usize) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => parse_err(line, format!("{what} must be 0 or 1, got {value:?}")),
    }
}

fn length_to_dlc(length: usize) -> u8 {
    match length {
        0..=8 => length as u8,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

fn duration_from_seconds(value: f64, line: usize) -> Result<Duration> {
    if !value.is_finite() || value < 0.0 || value > Duration::MAX.as_secs_f64() {
        return parse_err(
            line,
            format!("timestamp {value} is outside Duration's range"),
        );
    }
    Ok(Duration::from_secs_f64(value))
}

fn format_id(id: u32, base: NumberBase) -> String {
    let mut value = format_number(id & CAN_EXT_ID_MASK, base);
    if id & CAN_EXT_FLAG != 0 {
        value.push('x');
    }
    value
}

fn format_number(value: u32, base: NumberBase) -> String {
    match base {
        NumberBase::Hex => format!("{value:X}"),
        NumberBase::Decimal => value.to_string(),
    }
}

fn format_byte(value: u8, base: NumberBase) -> String {
    match base {
        NumberBase::Hex => format!("{value:02X}"),
        NumberBase::Decimal => value.to_string(),
    }
}

fn starts_ascii(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    starts_ascii(value, prefix).then(|| &value[prefix.len()..])
}

fn split_first(value: &str) -> Option<(&str, &str)> {
    let index = value.find(char::is_whitespace)?;
    Some((&value[..index], value[index..].trim_start()))
}

fn parse_error(line: usize, message: impl Into<String>) -> Error {
    Error::Parse {
        line,
        message: message.into(),
    }
}

fn parse_err<T>(line: usize, message: impl Into<String>) -> Result<T> {
    Err(parse_error(line, message))
}

fn format_error(error: std::fmt::Error) -> Error {
    Error::Write(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "date Mon Apr 10 15:19:28.789 2023\n\
base hex  timestamps absolute\n\
internal events logged\n\
Begin Triggerblock Mon Apr 10 15:19:28.789 2023\n\
 0.000000 Start of measurement\n\
 0.001000 1  123             Rx   d 3 01 02 A0\n\
 0.002000 2  1ABCDEFX       Tx   r 8\n\
 0.003000 1  ErrorFrame\n\
 0.004000 CANFD   2 Tx   1ABCDEFX  1 0 9 12 00 01 02 03 04 05 06 07 08 09 0A 0B 10 20 3000 40 50 60 70 80\n\
 0.005000 statistic CAN 1 42\n\
End TriggerBlock\n";

    #[test]
    fn parses_classic_fd_remote_error_and_event_records() {
        let file = AscFile::parse(SAMPLE).unwrap();
        assert_eq!(file.records.len(), 5);
        let AscRecord::Message(classic) = &file.records[0] else {
            panic!("classic message expected");
        };
        assert_eq!(classic.id, 0x123);
        assert_eq!(classic.data, [1, 2, 0xA0]);
        assert_eq!(classic.direction, Direction::Rx);

        let AscRecord::Message(remote) = &file.records[1] else {
            panic!("remote message expected");
        };
        assert!(remote.is_remote);
        assert_eq!(remote.id, CAN_EXT_FLAG | 0x1ABCDEF);
        assert!(remote.to_can_frame().is_none());

        assert!(matches!(file.records[2], AscRecord::ErrorFrame(_)));
        let AscRecord::Message(fd) = &file.records[3] else {
            panic!("CAN FD message expected");
        };
        assert_eq!(fd.frame_type, FrameType::FD_BRS);
        assert_eq!(fd.data.len(), 12);
        assert_eq!(fd.fd_metadata.len(), 8);
        assert!(matches!(file.records[4], AscRecord::Event(_)));
        assert_eq!(file.can_frames().len(), 2);
    }

    #[test]
    fn roundtrip_preserves_supported_records() {
        let parsed = AscFile::parse(SAMPLE).unwrap();
        let rendered = parsed.write().unwrap();
        let reparsed = AscFile::parse(&rendered).unwrap();
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn relative_timestamps_are_accumulated_and_written_as_deltas() {
        let text = "base dec timestamps relative\n\
no internal events logged\n\
0.100000 1 291 Rx d 1 10\n\
0.250000 1 292 Tx d 1 20\n";
        let file = AscFile::parse(text).unwrap();
        assert_eq!(file.records[0].timestamp(), Duration::from_millis(100));
        assert_eq!(file.records[1].timestamp(), Duration::from_millis(350));
        let reparsed = AscFile::parse(&file.write().unwrap()).unwrap();
        assert_eq!(reparsed.records, file.records);
    }

    #[test]
    fn shared_can_frame_conversion_preserves_core_fields() {
        let mut frame = CanFrame::new(
            "bus",
            CAN_EXT_FLAG | 0x12345,
            vec![1; 12],
            true,
            FrameType::FD_BRS,
        );
        frame.elapsed = Duration::from_micros(42);
        let asc = AscMessage::from_can_frame(&frame, 3).unwrap();
        let back = asc.to_can_frame().unwrap();
        assert_eq!(back.id, frame.id);
        assert_eq!(back.data, frame.data);
        assert_eq!(back.frame_type, frame.frame_type);
        assert_eq!(back.elapsed, frame.elapsed);
        assert!(back.is_master_frame);
        assert_eq!(back.bus_id, "3");
    }

    #[test]
    fn invalid_declared_data_length_is_rejected_with_line_number() {
        let error = AscFile::parse("0.0 1 123 Rx d 2 AA\n").unwrap_err();
        assert!(error.to_string().contains("line 1"));
        assert!(error.to_string().contains("declares 2 data bytes"));
    }

    #[test]
    fn save_and_open_roundtrip() {
        let file = AscFile::parse(SAMPLE).unwrap();
        let path =
            std::env::temp_dir().join(format!("autors-asc-{}-{}.asc", std::process::id(), line!()));
        file.save(&path).unwrap();
        let reopened = AscFile::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(reopened, file);
    }

    #[test]
    fn writer_rejects_invalid_public_message_fields() {
        let mut file = AscFile::new();
        let mut frame = CanFrame::new("bus", 1, vec![1], false, FrameType::CAN20B);
        frame.elapsed = Duration::ZERO;
        let mut message = AscMessage::from_can_frame(&frame, 1).unwrap();
        message.channel = 0;
        file.records.push(AscRecord::Message(message));
        assert!(file.write().unwrap_err().to_string().contains("one-based"));
    }
}
