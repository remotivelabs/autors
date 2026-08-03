//! Reader for PEAK PLIN-View Pro `.ltrc` LIN traces.
//!
//! The object model covers LTRC versions 1.0, 1.1, and 1.2. Missing bytes in
//! erroneous frames are represented as `None`; complete frame records can be
//! converted to [`autors_lin::device::LinFrame`].

mod error;

pub use error::{Error, Result};

use std::path::Path;
use std::time::Duration;

use autors_lin::device::LinFrame;

/// Supported PLIN-View LTRC format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LtrcVersion {
    /// PLIN-View Pro 1.0 format.
    V1_0,
    /// PLIN-View Pro 1.1.4 format with OLE Automation start time.
    V1_1,
    /// PLIN-View Pro 3.0 format with bus events.
    V1_2,
}

impl LtrcVersion {
    /// Parses the value of the `$FILEVERSION` keyword.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "1.0" => Some(Self::V1_0),
            "1.1" => Some(Self::V1_1),
            "1.2" => Some(Self::V1_2),
            _ => None,
        }
    }

    /// Returns the keyword value used by PLIN-View.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1_0 => "1.0",
            Self::V1_1 => "1.1",
            Self::V1_2 => "1.2",
        }
    }
}

/// Absolute trace start value from `$STARTTIME`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StartTime {
    /// Version 1.0's integral timestamp representation.
    Legacy(u64),
    /// Days since 1899-12-30, including the fractional part of the day.
    OleAutomationDays(f64),
}

/// Direction column of a PLIN-View frame record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Publisher frame (`Pub`).
    Publisher,
    /// Subscriber frame (`Sub`).
    Subscriber,
    /// Subscriber frame with automatic length (`SubAL`).
    SubscriberAutoLength,
}

impl Direction {
    fn parse(value: &str, line: usize) -> Result<Self> {
        if value.eq_ignore_ascii_case("pub") {
            Ok(Self::Publisher)
        } else if value.eq_ignore_ascii_case("sub") {
            Ok(Self::Subscriber)
        } else if value.eq_ignore_ascii_case("subal") {
            Ok(Self::SubscriberAutoLength)
        } else {
            parse_err(line, format!("unknown LIN direction {value:?}"))
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Publisher => "Pub",
            Self::Subscriber => "Sub",
            Self::SubscriberAutoLength => "SubAL",
        }
    }
}

/// Checksum model recorded for a LIN frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumType {
    /// LIN classic checksum (`CL`).
    Classic,
    /// LIN enhanced checksum (`EH`).
    Enhanced,
    /// Driver-selected checksum (`AU`).
    Auto,
}

impl ChecksumType {
    fn parse(value: &str, line: usize) -> Result<Self> {
        if value.eq_ignore_ascii_case("cl") {
            Ok(Self::Classic)
        } else if value.eq_ignore_ascii_case("eh") {
            Ok(Self::Enhanced)
        } else if value.eq_ignore_ascii_case("au") {
            Ok(Self::Auto)
        } else {
            parse_err(line, format!("unknown LIN checksum type {value:?}"))
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Classic => "CL",
            Self::Enhanced => "EH",
            Self::Auto => "AU",
        }
    }
}

/// Error code appended to an LTRC frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Checksum invalid (`CK`).
    Checksum,
    /// Bus shorted to ground (`GS`).
    GroundShort,
    /// Wrong identifier parity bit 0 (`P0`).
    IdParityBit0,
    /// Wrong identifier parity bit 1 (`P1`).
    IdParityBit1,
    /// Synchronization-field error (`IS`).
    InconsistentSync,
    /// Slave did not answer in time (`SR`).
    SlaveNotResponding,
    /// Slot delay was too small (`SD`).
    SlotDelay,
    /// Message timeout (`TO`).
    Timeout,
    /// Bus shorted to battery voltage (`VS`).
    VBatShort,
}

impl FrameError {
    fn parse(value: &str, line: usize) -> Result<Self> {
        if value.eq_ignore_ascii_case("ck") {
            Ok(Self::Checksum)
        } else if value.eq_ignore_ascii_case("gs") {
            Ok(Self::GroundShort)
        } else if value.eq_ignore_ascii_case("p0") {
            Ok(Self::IdParityBit0)
        } else if value.eq_ignore_ascii_case("p1") {
            Ok(Self::IdParityBit1)
        } else if value.eq_ignore_ascii_case("is") {
            Ok(Self::InconsistentSync)
        } else if value.eq_ignore_ascii_case("sr") {
            Ok(Self::SlaveNotResponding)
        } else if value.eq_ignore_ascii_case("sd") {
            Ok(Self::SlotDelay)
        } else if value.eq_ignore_ascii_case("to") {
            Ok(Self::Timeout)
        } else if value.eq_ignore_ascii_case("vs") {
            Ok(Self::VBatShort)
        } else {
            parse_err(line, format!("unknown LIN frame error code {value:?}"))
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Checksum => "CK",
            Self::GroundShort => "GS",
            Self::IdParityBit0 => "P0",
            Self::IdParityBit1 => "P1",
            Self::InconsistentSync => "IS",
            Self::SlaveNotResponding => "SR",
            Self::SlotDelay => "SD",
            Self::Timeout => "TO",
            Self::VBatShort => "VS",
        }
    }
}

/// Bus event introduced by LTRC 1.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusEventKind {
    /// LIN bus entered sleep mode.
    BusSleep,
    /// LIN bus woke up.
    BusWakeUp,
    /// PLIN client receive queue overrun.
    ClientQueueOverrun,
    /// Hardware or driver overrun.
    Overrun,
    /// A future event name retained for forward compatibility.
    Other(String),
}

impl BusEventKind {
    fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("Bus Sleep") {
            Self::BusSleep
        } else if value.eq_ignore_ascii_case("Bus Wake Up") {
            Self::BusWakeUp
        } else if value.eq_ignore_ascii_case("Client Queue Overrun") {
            Self::ClientQueueOverrun
        } else if value.eq_ignore_ascii_case("Overrun") {
            Self::Overrun
        } else {
            Self::Other(value.to_owned())
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::BusSleep => "Bus Sleep",
            Self::BusWakeUp => "Bus Wake Up",
            Self::ClientQueueOverrun => "Client Queue Overrun",
            Self::Overrun => "Overrun",
            Self::Other(value) => value,
        }
    }
}

/// A LIN frame line from an LTRC trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFrame {
    /// Recorded frame index.
    pub index: u64,
    /// Offset from trace start, at one-microsecond resolution.
    pub timestamp: Duration,
    /// Publisher/subscriber direction.
    pub direction: Direction,
    /// Six-bit LIN frame identifier.
    pub id: u8,
    /// Declared frame length.
    pub dlc: u8,
    /// Data bytes; unavailable bytes are `None` and correspond to `--`.
    pub data: Vec<Option<u8>>,
    /// Recorded checksum byte.
    pub checksum: u8,
    /// Checksum model.
    pub checksum_type: ChecksumType,
    /// Zero or more slash-separated error codes.
    pub errors: Vec<FrameError>,
}

impl TraceFrame {
    /// Converts a complete record to the shared LIN frame representation.
    /// Records containing unavailable bytes return `None`.
    pub fn to_lin_frame(&self, bus_id: impl Into<String>) -> Option<LinFrame> {
        let data: Option<Vec<u8>> = self.data.iter().copied().collect();
        let mut frame = LinFrame::new(
            &bus_id.into(),
            self.id,
            data?,
            self.direction == Direction::Publisher,
        );
        frame.elapsed = self.timestamp;
        Some(frame)
    }
}

/// A timestamped bus event line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusEvent {
    /// Recorded event index.
    pub index: u64,
    /// Offset from trace start, at one-microsecond resolution.
    pub timestamp: Duration,
    /// Event kind.
    pub kind: BusEventKind,
}

/// One LTRC data line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// LIN frame.
    Frame(TraceFrame),
    /// Bus event.
    BusEvent(BusEvent),
}

/// In-memory PLIN-View trace.
#[derive(Debug, Clone, PartialEq)]
pub struct LtrcFile {
    /// Format version from `$FILEVERSION`.
    pub version: LtrcVersion,
    /// Absolute trace start from `$STARTTIME`, when present.
    pub start_time: Option<StartTime>,
    /// Frames and bus events in file order.
    pub records: Vec<Record>,
}

impl LtrcFile {
    /// Parses LTRC text.
    pub fn parse(input: &str) -> Result<Self> {
        let mut version: Option<(LtrcVersion, usize)> = None;
        let mut start_time: Option<(String, usize)> = None;
        let mut records = Vec::with_capacity(input.len() / 64);

        for (index, raw_line) in input.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim().trim_start_matches('\u{feff}');
            if line.is_empty() {
                continue;
            }
            if let Some(value) = keyword(line, "$FILEVERSION") {
                let parsed = LtrcVersion::parse(value).ok_or_else(|| Error::Parse {
                    line: line_number,
                    message: format!("unsupported LTRC version {value:?}"),
                })?;
                version = Some((parsed, line_number));
                continue;
            }
            if let Some(value) = keyword(line, "$STARTTIME") {
                start_time = Some((value.trim().to_owned(), line_number));
                continue;
            }
            if line.starts_with(';') {
                continue;
            }
            records.push(parse_record(line, line_number)?);
        }

        let Some((version, _)) = version else {
            return parse_err(1, "missing $FILEVERSION keyword");
        };
        let start_time = start_time
            .map(|(value, line)| parse_start_time(&value, version, line))
            .transpose()?;
        Ok(Self {
            version,
            start_time,
            records,
        })
    }

    /// Reads and parses an LTRC file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Serializes this trace using the documented PLIN-View Pro column layout.
    pub fn write(&self) -> Result<String> {
        let mut output = format!(";$FILEVERSION={}\n", self.version.as_str());
        if let Some(start) = self.start_time {
            match start {
                StartTime::Legacy(value) => output.push_str(&format!(";$STARTTIME={value}\n")),
                StartTime::OleAutomationDays(value) => {
                    if !value.is_finite() {
                        return parse_err(1, "OLE Automation start time is not finite");
                    }
                    output.push_str(&format!(";$STARTTIME={value}\n"));
                }
            }
        }
        for record in &self.records {
            match record {
                Record::Frame(frame) => {
                    if frame.id > 0x3f || !(1..=8).contains(&frame.dlc) {
                        return parse_err(1, "cannot write invalid LIN frame identifier or length");
                    }
                    if frame.data.len() != usize::from(frame.dlc) {
                        return parse_err(
                            1,
                            "LIN frame data count differs from its declared length",
                        );
                    }
                    output.push_str(&format!(
                        "{}) {} {} {:02X} {}",
                        frame.index,
                        frame.timestamp.as_micros(),
                        frame.direction.keyword(),
                        frame.id,
                        frame.dlc
                    ));
                    for byte in &frame.data {
                        match byte {
                            Some(byte) => output.push_str(&format!(" {byte:02X}")),
                            None => output.push_str(" --"),
                        }
                    }
                    output.push_str(&format!(
                        " {:02X} {}",
                        frame.checksum,
                        frame.checksum_type.keyword()
                    ));
                    if !frame.errors.is_empty() {
                        output.push(' ');
                        output.push_str(
                            &frame
                                .errors
                                .iter()
                                .map(|error| error.keyword())
                                .collect::<Vec<_>>()
                                .join("/"),
                        );
                    }
                    output.push('\n');
                }
                Record::BusEvent(event) => output.push_str(&format!(
                    "{}) {} --- -- - {} -- --\n",
                    event.index,
                    event.timestamp.as_micros(),
                    event.kind.name()
                )),
            }
        }
        Ok(output)
    }

    /// Writes a PLIN-View-compatible LTRC text file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.write()?)?;
        Ok(())
    }

    /// Converts every complete frame record to a shared LIN frame.
    pub fn lin_frames(&self, bus_id: impl Into<String>) -> Vec<LinFrame> {
        let bus_id = bus_id.into();
        self.records
            .iter()
            .filter_map(|record| match record {
                Record::Frame(frame) => frame.to_lin_frame(bus_id.clone()),
                Record::BusEvent(_) => None,
            })
            .collect()
    }
}

fn parse_record(line_text: &str, line: usize) -> Result<Record> {
    let mut inline_tokens = [""; 32];
    let mut token_count = 0;
    let mut spill = Vec::new();
    for token in line_text.split_whitespace() {
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
    if tokens.len() < 5 {
        return parse_err(line, "LTRC record has fewer than five columns");
    }
    let index_text = tokens[0]
        .strip_suffix(')')
        .ok_or_else(|| parse_error(line, format!("invalid record index {:?}", tokens[0])))?;
    let index = index_text
        .parse::<u64>()
        .map_err(|_| parse_error(line, format!("invalid record index {:?}", tokens[0])))?;
    let micros = tokens[1]
        .parse::<u64>()
        .map_err(|_| parse_error(line, format!("invalid timestamp {:?}", tokens[1])))?;
    let timestamp = Duration::from_micros(micros);

    if tokens[2] == "---" {
        if tokens[3] != "--" || tokens[4] != "-" || tokens.len() < 8 {
            return parse_err(line, "malformed LTRC bus event record");
        }
        if tokens[tokens.len() - 2..] != ["--", "--"] {
            return parse_err(line, "bus event is missing checksum placeholders");
        }
        let name = tokens[5..tokens.len() - 2].join(" ");
        if name.is_empty() {
            return parse_err(line, "bus event has an empty name");
        }
        return Ok(Record::BusEvent(BusEvent {
            index,
            timestamp,
            kind: BusEventKind::parse(&name),
        }));
    }

    let direction = Direction::parse(tokens[2], line)?;
    let id = u8::from_str_radix(tokens[3], 16)
        .map_err(|_| parse_error(line, format!("invalid LIN identifier {:?}", tokens[3])))?;
    if id > 0x3f {
        return parse_err(line, format!("LIN identifier 0x{id:02X} exceeds 0x3F"));
    }
    let dlc = tokens[4]
        .parse::<u8>()
        .map_err(|_| parse_error(line, format!("invalid LIN frame length {:?}", tokens[4])))?;
    if !(1..=8).contains(&dlc) {
        return parse_err(line, format!("LIN frame length {dlc} is outside 1..=8"));
    }
    let data_start = 5;
    let checksum_index = data_start + usize::from(dlc);
    if tokens.len() < checksum_index + 2 {
        return parse_err(
            line,
            format!("LIN frame declares {dlc} data bytes but the record has too few columns"),
        );
    }
    let data = tokens[data_start..checksum_index]
        .iter()
        .map(|value| {
            if *value == "--" {
                Ok(None)
            } else {
                u8::from_str_radix(value, 16)
                    .map(Some)
                    .map_err(|_| parse_error(line, format!("invalid LIN data byte {value:?}")))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let checksum = u8::from_str_radix(tokens[checksum_index], 16).map_err(|_| {
        parse_error(
            line,
            format!("invalid LIN checksum {:?}", tokens[checksum_index]),
        )
    })?;
    let checksum_type = ChecksumType::parse(tokens[checksum_index + 1], line)?;
    let errors = tokens[checksum_index + 2..]
        .iter()
        .flat_map(|value| value.split('/'))
        .map(|value| FrameError::parse(value, line))
        .collect::<Result<Vec<_>>>()?;

    Ok(Record::Frame(TraceFrame {
        index,
        timestamp,
        direction,
        id,
        dlc,
        data,
        checksum,
        checksum_type,
        errors,
    }))
}

fn parse_start_time(value: &str, version: LtrcVersion, line: usize) -> Result<StartTime> {
    match version {
        LtrcVersion::V1_0 => value
            .parse::<u64>()
            .map(StartTime::Legacy)
            .map_err(|_| parse_error(line, format!("invalid version 1.0 start time {value:?}"))),
        LtrcVersion::V1_1 | LtrcVersion::V1_2 => {
            let days = value.parse::<f64>().map_err(|_| {
                parse_error(line, format!("invalid OLE Automation start time {value:?}"))
            })?;
            if !days.is_finite() {
                return parse_err(line, "OLE Automation start time is not finite");
            }
            Ok(StartTime::OleAutomationDays(days))
        }
    }
}

fn keyword<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let line = line.strip_prefix(';').unwrap_or(line).trim_start();
    let (key, value) = line.split_once('=')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
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

#[cfg(test)]
mod tests {
    use super::*;

    const V1_0: &str = ";$FILEVERSION=1.0\n\
;$STARTTIME=8708618495\n\
 1) 11307 Pub 05 2 00 00 7A EH\n\
 2) 36305 Sub 02 2 -- -- 00 EH SR/TO\n\
 3) 61305 Sub 07 8 -- -- -- -- -- -- -- -- 00 EH SR/TO\n\
 4) 136303 Sub 07 8 C1 38 FE FF 3F F0 3E 6D FC EH Ck\n";

    const V1_2: &str = ";$FILEVERSION=1.2\n\
;$STARTTIME=44824.3776155093\n\
 1) 1841968 --- -- - Bus Wake Up -- --\n\
 2) 2576094 Sub 02 2 -- -- 00 EH SR/TO\n\
 3) 3078112 Pub 01 4 00 00 00 00 3E EH\n\
 4) 9091922 --- -- - Bus Sleep -- --\n\
 5) 10000000 --- -- - Client Queue Overrun -- --\n\
 6) 11000000 --- -- - Overrun -- --\n";

    #[test]
    fn writes_parseable_ltrc_with_frames_and_bus_events() {
        let parsed = LtrcFile::parse(V1_2).unwrap();
        let text = parsed.write().unwrap();
        assert!(text.contains(";$FILEVERSION=1.2"));
        assert!(text.contains("Bus Wake Up -- --"));
        assert_eq!(LtrcFile::parse(&text).unwrap(), parsed);
    }

    #[test]
    fn parses_version_1_0_frames_missing_bytes_and_legacy_ck_spelling() {
        let file = LtrcFile::parse(V1_0).unwrap();
        assert_eq!(file.version, LtrcVersion::V1_0);
        assert_eq!(file.start_time, Some(StartTime::Legacy(8_708_618_495)));
        assert_eq!(file.records.len(), 4);
        let Record::Frame(first) = &file.records[0] else {
            panic!("frame expected");
        };
        assert_eq!(first.direction, Direction::Publisher);
        assert_eq!(first.data, [Some(0), Some(0)]);
        assert_eq!(first.timestamp, Duration::from_micros(11_307));

        let Record::Frame(error) = &file.records[1] else {
            panic!("frame expected");
        };
        assert_eq!(error.data, [None, None]);
        assert_eq!(
            error.errors,
            [FrameError::SlaveNotResponding, FrameError::Timeout]
        );
        assert!(error.to_lin_frame("LIN1").is_none());

        let Record::Frame(checksum_error) = &file.records[3] else {
            panic!("frame expected");
        };
        assert_eq!(checksum_error.errors, [FrameError::Checksum]);
    }

    #[test]
    fn parses_all_version_1_2_bus_events_and_complete_lin_frames() {
        let file = LtrcFile::parse(V1_2).unwrap();
        assert_eq!(file.version, LtrcVersion::V1_2);
        assert!(matches!(
            file.start_time,
            Some(StartTime::OleAutomationDays(value)) if (value - 44824.3776155093).abs() < f64::EPSILON
        ));
        let kinds: Vec<&BusEventKind> = file
            .records
            .iter()
            .filter_map(|record| match record {
                Record::BusEvent(event) => Some(&event.kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                &BusEventKind::BusWakeUp,
                &BusEventKind::BusSleep,
                &BusEventKind::ClientQueueOverrun,
                &BusEventKind::Overrun,
            ]
        );
        let frames = file.lin_frames("PLIN1");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bus_id, "PLIN1");
        assert_eq!(frames[0].id, 1);
        assert_eq!(frames[0].data, [0, 0, 0, 0]);
        assert!(frames[0].is_master_frame);
        assert_eq!(frames[0].elapsed, Duration::from_micros(3_078_112));
    }

    #[test]
    fn parses_version_1_1_ole_start_time() {
        let file = LtrcFile::parse(
            ";$FILEVERSION=1.1\n;$STARTTIME=40694.6971444676\n1) 1 SubAL 3F 1 01 FF AU\n",
        )
        .unwrap();
        assert_eq!(file.version.as_str(), "1.1");
        let Record::Frame(frame) = &file.records[0] else {
            panic!("frame expected");
        };
        assert_eq!(frame.direction, Direction::SubscriberAutoLength);
        assert_eq!(frame.id, 0x3f);
        assert_eq!(frame.checksum_type, ChecksumType::Auto);
    }

    #[test]
    fn rejects_invalid_id_with_line_number() {
        let error = LtrcFile::parse(";$FILEVERSION=1.2\n1) 0 Pub 40 1 00 FF CL\n").unwrap_err();
        assert!(error.to_string().contains("line 2"));
        assert!(error.to_string().contains("exceeds 0x3F"));
    }

    #[test]
    fn opens_trace_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "autors-ltrc-{}-{}.ltrc",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, V1_2).unwrap();
        let file = LtrcFile::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(file.version, LtrcVersion::V1_2);
        assert_eq!(file.records.len(), 6);
    }
}
