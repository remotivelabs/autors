//! Texas Instruments TI-TXT firmware import and export.

use std::fmt::Write as _;
use std::path::Path;

use autors_a2l::model::enums::MemoryPrgType;

use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::image::{MergeOptions, OverlapPolicy};
use crate::{Error, Result};

/// A TI-TXT image with address-marked sections and a mandatory terminator.
#[derive(Debug, Clone)]
pub struct DataFileTiTxt {
    pub base: DataFileBase,
}

impl DataFileTiTxt {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        Self {
            base: DataFileBase::new(source_filename, segments),
        }
    }

    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        let mut file = Self::new(None, segments);
        file.load_str(text)?;
        Ok(file)
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut file = Self::parse(&text, segments)?;
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
        let text = std::fs::read_to_string(path)?;
        self.load_str(&text)
    }

    pub fn load_str(&mut self, text: &str) -> Result<()> {
        let parsed = parse_ti_txt(text)?;
        let mut work = self.base.segment_list.clone();
        work.merge_from(
            &parsed,
            MergeOptions {
                overlap: OverlapPolicy::Overwrite,
                ..MergeOptions::default()
            },
        )?;
        self.base.segment_list = work;
        self.base.data_bytes_per_line = 16;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        Ok(())
    }

    /// Writes canonical uppercase TI-TXT with 16 data bytes per line.
    pub fn write_ti_txt(&mut self) -> Result<String> {
        self.base.segment_list.validate_image()?;
        let mut segments: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|segment| segment.is_initialized() && segment.size() != 0)
            .collect();
        segments.sort_by_key(|segment| segment.address);

        let mut output = String::new();
        for segment in segments {
            if segment.address % 2 != 0 {
                return Err(Error::Value(format!(
                    "TI-TXT section address 0x{:X} must be even",
                    segment.address
                )));
            }
            let _ = writeln!(output, "@{:X}", segment.address);
            for line in segment.data().chunks(16) {
                for (index, byte) in line.iter().enumerate() {
                    if index != 0 {
                        output.push(' ');
                    }
                    let _ = write!(output, "{byte:02X}");
                }
                output.push('\n');
            }
        }
        output.push_str("q\n");
        self.base.data_bytes_per_line = 16;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        Ok(output)
    }

    pub fn save_file(&mut self, path: &Path) -> Result<()> {
        let output = self.write_ti_txt()?;
        std::fs::write(path, output)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

fn parse_ti_txt(text: &str) -> Result<MemorySegmentList> {
    let mut parsed = MemorySegmentList::new();
    let mut address = None;
    let mut section_start = 0u64;
    let mut section = Vec::new();
    let mut terminated = false;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index as u32 + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("q") {
            flush_section(&mut parsed, section_start, &mut section)?;
            terminated = true;
            if text
                .lines()
                .skip(index + 1)
                .any(|tail| !tail.trim().is_empty())
            {
                return Err(Error::DataFile {
                    line: line_number,
                    message: "non-empty data after TI-TXT terminator".to_string(),
                });
            }
            break;
        }
        if let Some(value) = line.strip_prefix('@') {
            flush_section(&mut parsed, section_start, &mut section)?;
            let parsed_address =
                u64::from_str_radix(value.trim(), 16).map_err(|_| Error::DataFile {
                    line: line_number,
                    message: "invalid TI-TXT section address".to_string(),
                })?;
            if parsed_address % 2 != 0 {
                return Err(Error::DataFile {
                    line: line_number,
                    message: "TI-TXT section address must be even".to_string(),
                });
            }
            address = Some(parsed_address);
            section_start = parsed_address;
            continue;
        }

        if address.is_none() {
            return Err(Error::DataFile {
                line: line_number,
                message: "TI-TXT data appears before a section address".to_string(),
            });
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() || tokens.len() > 16 {
            return Err(Error::DataFile {
                line: line_number,
                message: "TI-TXT data line must contain 1 to 16 bytes".to_string(),
            });
        }
        for token in tokens {
            if token.len() != 2 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(Error::DataFile {
                    line: line_number,
                    message: format!("invalid TI-TXT data byte '{token}'"),
                });
            }
            let byte = u8::from_str_radix(token, 16).map_err(|_| Error::DataFile {
                line: line_number,
                message: format!("invalid TI-TXT data byte '{token}'"),
            })?;
            section.push(byte);
        }
    }

    if !terminated {
        return Err(Error::DataFile {
            line: 0,
            message: "TI-TXT input is missing mandatory q terminator".to_string(),
        });
    }
    if parsed.is_empty() {
        return Err(Error::DataFile {
            line: 0,
            message: "TI-TXT input contains no data sections".to_string(),
        });
    }
    Ok(parsed)
}

fn flush_section(image: &mut MemorySegmentList, address: u64, data: &mut Vec<u8>) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    image.insert_segment(
        MemorySegment::from_data(address, std::mem::take(data), MemoryPrgType::DATA, true),
        OverlapPolicy::Reject,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    const SAMPLE: &str =
        "@F000\n31 40 00 03 B2 40 80 5A 20 01 D2 D3 22 00 D2 E3\n21 00 3F 40\n@FFFE\n00 F0\nQ\n";

    #[test]
    fn parses_multiple_sections_and_mandatory_terminator() {
        let file = DataFileTiTxt::parse(SAMPLE, MemorySegmentList::new()).unwrap();
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0xF000, 0xF014).unwrap())
                .unwrap()
                .len(),
            20
        );
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0xFFFE, 0x10000).unwrap())
                .unwrap(),
            [0, 0xF0]
        );
        assert!(DataFileTiTxt::parse("@1000\n00\n", MemorySegmentList::new()).is_err());
    }

    #[test]
    fn rejects_odd_overlapping_and_invalid_sections() {
        assert!(DataFileTiTxt::parse("@1001\n00\nq\n", MemorySegmentList::new()).is_err());
        assert!(DataFileTiTxt::parse("@1000\nGG\nq\n", MemorySegmentList::new()).is_err());
        assert!(
            DataFileTiTxt::parse("@1000\n00 01\n@1000\n02\nq\n", MemorySegmentList::new()).is_err()
        );
    }

    #[test]
    fn canonical_writer_round_trips() {
        let mut file = DataFileTiTxt::parse(SAMPLE, MemorySegmentList::new()).unwrap();
        let output = file.write_ti_txt().unwrap();
        assert!(output.starts_with("@F000\n"));
        assert!(output.ends_with("q\n"));
        let reparsed = DataFileTiTxt::parse(&output, MemorySegmentList::new()).unwrap();
        assert_eq!(reparsed.base.segment_list, file.base.segment_list);
    }
}
