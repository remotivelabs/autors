//! Plain hexadecimal ASCII import and export.

use std::fmt::Write as _;
use std::path::Path;

use autors_a2l::model::enums::MemoryPrgType;

use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::image::OverlapPolicy;
use crate::{Error, Result};

/// Formatting controls for hexadecimal ASCII output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexAsciiOptions {
    pub separator: String,
    /// Zero emits all bytes on one line.
    pub bytes_per_line: usize,
    pub uppercase: bool,
}

impl Default for HexAsciiOptions {
    fn default() -> Self {
        Self {
            separator: " ".to_string(),
            bytes_per_line: 16,
            uppercase: true,
        }
    }
}

/// A plain hexadecimal ASCII byte stream.
#[derive(Debug, Clone)]
pub struct DataFileHexAscii {
    pub base: DataFileBase,
    pub base_address: u64,
}

impl DataFileHexAscii {
    pub fn new(
        source_filename: Option<String>,
        segments: MemorySegmentList,
        base_address: u64,
    ) -> Self {
        Self {
            base: DataFileBase::new(source_filename, segments),
            base_address,
        }
    }

    /// Parses bytes at address zero. One- or two-digit byte tokens and
    /// uninterrupted pairs are accepted; non-hexadecimal characters are
    /// separators. `0x` prefixes are also accepted.
    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        Self::parse_at(text, segments, 0)
    }

    pub fn parse_at(text: &str, segments: MemorySegmentList, base_address: u64) -> Result<Self> {
        let mut file = Self::new(None, segments, base_address);
        file.load_str(text)?;
        Ok(file)
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
        self.load_str(&content)
    }

    pub fn load_str(&mut self, text: &str) -> Result<()> {
        let data = parse_hex_ascii(text)?;
        if data.is_empty() {
            return Err(Error::DataFile {
                line: 0,
                message: "HEX ASCII input contains no byte values".to_string(),
            });
        }
        self.base.segment_list.insert_segment(
            MemorySegment::from_data(self.base_address, data, MemoryPrgType::DATA, true),
            OverlapPolicy::Overwrite,
        )?;
        self.base.data_bytes_per_line = 0;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        Ok(())
    }

    /// Exports initialized segments in ascending address order. Address holes
    /// are omitted because this format carries no address metadata.
    pub fn write_hex_ascii(&mut self, options: &HexAsciiOptions) -> String {
        let mut segments: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        segments.sort_by_key(|segment| segment.address);
        let byte_count: usize = segments.iter().map(|segment| segment.size()).sum();
        let mut output = String::new();
        let mut index = 0usize;
        for segment in segments {
            for byte in segment.data() {
                if index != 0 {
                    if options.bytes_per_line != 0 && index.is_multiple_of(options.bytes_per_line) {
                        output.push('\n');
                    } else {
                        output.push_str(&options.separator);
                    }
                }
                if options.uppercase {
                    let _ = write!(output, "{byte:02X}");
                } else {
                    let _ = write!(output, "{byte:02x}");
                }
                index += 1;
            }
        }
        if byte_count != 0 {
            output.push('\n');
        }
        self.base.data_bytes_per_line = options.bytes_per_line;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        output
    }

    pub fn save_file(&mut self, path: &Path, options: &HexAsciiOptions) -> Result<()> {
        let content = self.write_hex_ascii(options);
        std::fs::write(path, content)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

fn parse_hex_ascii(text: &str) -> Result<Vec<u8>> {
    let normalized = text.replace("0x", "").replace("0X", "");
    let mut data = Vec::new();
    for token in normalized
        .split(|character: char| !character.is_ascii_hexdigit())
        .filter(|token| !token.is_empty())
    {
        for pair in token.as_bytes().chunks(2) {
            let value = std::str::from_utf8(pair)
                .ok()
                .and_then(|value| u8::from_str_radix(value, 16).ok())
                .ok_or_else(|| Error::DataFile {
                    line: 0,
                    message: format!("invalid HEX ASCII byte '{token}'"),
                })?;
            data.push(value);
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    #[test]
    fn parses_single_pair_contiguous_and_prefixed_values() {
        let file =
            DataFileHexAscii::parse("34, 5, F3 0102 0xaa", MemorySegmentList::new()).unwrap();
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0, 6).unwrap())
                .unwrap(),
            &[0x34, 0x05, 0xF3, 0x01, 0x02, 0xAA]
        );
    }

    #[test]
    fn parse_at_merges_and_overwrites_existing_bytes() {
        let segments = MemorySegmentList {
            segments: vec![MemorySegment::from_data(
                0x1000,
                vec![0, 0, 0],
                MemoryPrgType::DATA,
                true,
            )],
        };
        let file = DataFileHexAscii::parse_at("AA BB", segments, 0x1001).unwrap();
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0x1000, 0x1003).unwrap())
                .unwrap(),
            &[0, 0xAA, 0xBB]
        );
    }

    #[test]
    fn writes_configurable_hex_ascii() {
        let mut file = DataFileHexAscii::parse("1 2 3 4 5", MemorySegmentList::new()).unwrap();
        let output = file.write_hex_ascii(&HexAsciiOptions {
            separator: ",".to_string(),
            bytes_per_line: 3,
            uppercase: false,
        });
        assert_eq!(output, "01,02,03\n04,05\n");
    }

    #[test]
    fn rejects_empty_input() {
        assert!(DataFileHexAscii::parse("---", MemorySegmentList::new()).is_err());
    }
}
