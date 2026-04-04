//! Mixed Intel HEX and Motorola S-record input.

use std::path::Path;

use crate::datafile::{DataFileBase, DataFileHex, DataFileS19, MemorySegmentList};
use crate::image::{MergeOptions, OverlapPolicy};
use crate::{Error, Result};

/// A read-only mixed record container. It intentionally has no same-format
/// writer because mixed record streams do not have an unambiguous output
/// representation.
#[derive(Debug, Clone)]
pub struct DataFileMixed {
    pub base: DataFileBase,
    pub contains_intel_hex: bool,
    pub contains_motorola_s: bool,
}

impl DataFileMixed {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        Self {
            base: DataFileBase::new(source_filename, segments),
            contains_intel_hex: false,
            contains_motorola_s: false,
        }
    }

    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        let mut file = Self::new(None, segments);
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
        let mut intel_hex = String::new();
        let mut motorola_s = String::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(':') {
                intel_hex.push_str(trimmed);
                intel_hex.push('\n');
            } else if trimmed.as_bytes().get(..2).is_some_and(|prefix| {
                prefix[0].eq_ignore_ascii_case(&b'S') && prefix[1].is_ascii_digit()
            }) {
                motorola_s.push_str(trimmed);
                motorola_s.push('\n');
            }
        }
        if intel_hex.is_empty() || motorola_s.is_empty() {
            return Err(Error::DataFile {
                line: 0,
                message: "mixed input requires both Intel HEX and Motorola S-records".to_string(),
            });
        }

        let hex = DataFileHex::parse(&intel_hex, MemorySegmentList::new())?;
        let s_record = DataFileS19::parse(&motorola_s, MemorySegmentList::new())?;
        let mut work = self.base.segment_list.clone();
        work.merge_from(
            &hex.base.segment_list,
            MergeOptions {
                overlap: OverlapPolicy::Reject,
                ..MergeOptions::default()
            },
        )?;
        work.merge_from(
            &s_record.base.segment_list,
            MergeOptions {
                overlap: OverlapPolicy::Reject,
                ..MergeOptions::default()
            },
        )?;
        self.base.segment_list = work;
        self.base.data_bytes_per_line = hex
            .base
            .data_bytes_per_line
            .max(s_record.base.data_bytes_per_line);
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        self.contains_intel_hex = true;
        self.contains_motorola_s = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    #[test]
    fn parses_non_overlapping_mixed_records() {
        let text = ":021000000102EB\nS10520000304D3\n:00000001FF\n";
        let file = DataFileMixed::parse(text, MemorySegmentList::new()).unwrap();
        assert!(file.contains_intel_hex);
        assert!(file.contains_motorola_s);
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0x1000, 0x1002).unwrap())
                .unwrap(),
            &[1, 2]
        );
        assert_eq!(
            file.base
                .segment_list
                .read_exact(AddressRange::new(0x2000, 0x2002).unwrap())
                .unwrap(),
            &[3, 4]
        );
    }

    #[test]
    fn rejects_ambiguous_overlapping_formats() {
        let text = ":021000000102EB\nS10510000304E3\n";
        assert!(DataFileMixed::parse(text, MemorySegmentList::new()).is_err());
    }

    #[test]
    fn requires_both_record_families() {
        assert!(DataFileMixed::parse(":00000001FF\n", MemorySegmentList::new()).is_err());
    }
}
