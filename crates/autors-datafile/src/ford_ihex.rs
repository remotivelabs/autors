//! Ford Intel HEX container import.

use std::path::Path;

use crate::datafile::{DataFileBase, DataFileHex, MemorySegmentList};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FordIHexHeaderField {
    pub name: String,
    pub value: String,
}

/// Ford container header preceding standard Intel HEX records.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FordIHexHeader {
    /// Original non-empty header lines, excluding the `$` terminator.
    pub raw_lines: Vec<String>,
    /// Parsed `NAME>VALUE` or `NAME=VALUE` entries.
    pub fields: Vec<FordIHexHeaderField>,
}

impl FordIHexHeader {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .map(|field| field.value.as_str())
    }
}

/// A Ford metadata header plus standard Intel HEX payload. This container is
/// read-only until all mandatory OEM checksums can be regenerated safely.
#[derive(Debug, Clone)]
pub struct DataFileFordIHex {
    pub base: DataFileBase,
    pub header: FordIHexHeader,
}

impl DataFileFordIHex {
    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        let lines: Vec<&str> = text.lines().collect();
        let first_record = lines
            .iter()
            .position(|line| line.trim_start().starts_with(':'))
            .ok_or_else(|| Error::DataFile {
                line: 0,
                message: "Ford I-HEX container has no Intel HEX records".to_string(),
            })?;
        let header = parse_header(&lines[..first_record])?;
        let payload = lines[first_record..].join("\n");
        let hex = DataFileHex::parse(&payload, segments)?;
        Ok(Self {
            base: hex.base,
            header,
        })
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut file = Self::parse(&content, segments)?;
        file.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(file)
    }

    pub fn reload(&mut self) -> Result<()> {
        let path = self
            .base
            .source_filename
            .clone()
            .ok_or_else(|| Error::DataFile {
                line: 0,
                message: "reload without source filename".to_string(),
            })?;
        let content = std::fs::read_to_string(path)?;
        let mut reloaded = Self::parse(&content, MemorySegmentList::new())?;
        reloaded.base.source_filename = self.base.source_filename.clone();
        *self = reloaded;
        Ok(())
    }
}

fn parse_header(lines: &[&str]) -> Result<FordIHexHeader> {
    let mut header = FordIHexHeader::default();
    let mut terminated = false;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "$" {
            terminated = true;
            continue;
        }
        if terminated {
            return Err(Error::DataFile {
                line: 0,
                message: "unexpected Ford I-HEX header data after '$' terminator".to_string(),
            });
        }
        header.raw_lines.push(line.to_string());
        if let Some((name, value)) = line.split_once('>').or_else(|| line.split_once('=')) {
            let name = name.trim();
            let value = value.trim();
            if !name.is_empty() {
                header.fields.push(FordIHexHeaderField {
                    name: name.to_string(),
                    value: value.to_string(),
                });
            }
        }
    }
    if !terminated {
        return Err(Error::DataFile {
            line: 0,
            message: "Ford I-HEX header has no '$' terminator".to_string(),
        });
    }
    let application = header.field("APPLICATION").unwrap_or_default();
    if !application.to_ascii_lowercase().contains("ford") {
        return Err(Error::DataFile {
            line: 0,
            message: "Ford I-HEX APPLICATION field is missing or invalid".to_string(),
        });
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    const SAMPLE: &str = "APPLICATION>FORD FNOS-DemoIL\n\
MASK NUMBER>7\n\
FILE NAME>APPL.hex\n\
RELEASE DATE=10/05/2001\n\
MODULE TYPE>Powertrain Control Module\n\
FILE CHECKSUM>0x0A01\n\
$\n\
:0410000001020304E2\n\
:00000001FF\n";

    #[test]
    fn parses_header_fields_and_intel_hex_payload() {
        let file = DataFileFordIHex::parse(SAMPLE, MemorySegmentList::new()).unwrap();
        assert_eq!(file.header.field("APPLICATION"), Some("FORD FNOS-DemoIL"));
        assert_eq!(file.header.field("RELEASE DATE"), Some("10/05/2001"));
        assert_eq!(file.header.raw_lines.len(), 6);
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0x1000, 0x1004).unwrap())
                .unwrap(),
            &[1, 2, 3, 4]
        );
    }

    #[test]
    fn rejects_missing_terminator_or_non_ford_application() {
        assert!(DataFileFordIHex::parse(
            "APPLICATION>FORD\n:00000001FF\n",
            MemorySegmentList::new()
        )
        .is_err());
        assert!(DataFileFordIHex::parse(
            "APPLICATION>OTHER\n$\n:00000001FF\n",
            MemorySegmentList::new()
        )
        .is_err());
    }
}
