//! Wildcard byte-pattern search and transactional replacement.

use crate::datafile::{DataFileBase, MemorySegmentList};
use crate::image::AddressRange;
use crate::{Error, Result};

/// A byte pattern with an independent bit mask for every byte.
///
/// In text form, `DE AD ?? B? ?F` means two exact bytes, any byte, a byte
/// whose high nibble is `B`, and a byte whose low nibble is `F`. A contiguous
/// form such as `DEADBEEF` is accepted too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytePattern {
    values: Vec<u8>,
    masks: Vec<u8>,
}

impl BytePattern {
    pub fn exact(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let values = bytes.into();
        if values.is_empty() {
            return Err(Error::Value("search pattern must not be empty".to_string()));
        }
        Ok(Self {
            masks: vec![0xFF; values.len()],
            values,
        })
    }

    pub fn from_values_and_masks(values: Vec<u8>, masks: Vec<u8>) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::Value("search pattern must not be empty".to_string()));
        }
        if values.len() != masks.len() {
            return Err(Error::Value(
                "search pattern values and masks must have equal length".to_string(),
            ));
        }
        let values = values
            .into_iter()
            .zip(&masks)
            .map(|(value, mask)| value & mask)
            .collect();
        Ok(Self { values, masks })
    }

    pub fn parse(text: &str) -> Result<Self> {
        let raw_tokens: Vec<&str> = text
            .split(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ':' | '_')
            })
            .filter(|token| !token.is_empty())
            .collect();
        if raw_tokens.is_empty() {
            return Err(Error::Value("search pattern must not be empty".to_string()));
        }

        let mut tokens = Vec::new();
        for raw in raw_tokens {
            let token = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(raw);
            if token == "?" {
                tokens.push("??");
            } else if token.len() == 2 {
                tokens.push(token);
            } else if token.len() > 2 && token.len().is_multiple_of(2) {
                tokens.extend(
                    (0..token.len())
                        .step_by(2)
                        .map(|offset| &token[offset..offset + 2]),
                );
            } else {
                return Err(Error::Value(format!("invalid byte-pattern token '{raw}'")));
            }
        }

        let mut values = Vec::with_capacity(tokens.len());
        let mut masks = Vec::with_capacity(tokens.len());
        for token in tokens {
            let bytes = token.as_bytes();
            let (high, high_mask) = parse_nibble(bytes[0], token)?;
            let (low, low_mask) = parse_nibble(bytes[1], token)?;
            values.push((high << 4) | low);
            masks.push((high_mask << 4) | low_mask);
        }
        Self::from_values_and_masks(values, masks)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn values(&self) -> &[u8] {
        &self.values
    }

    pub fn masks(&self) -> &[u8] {
        &self.masks
    }

    pub fn matches(&self, data: &[u8]) -> bool {
        data.len() >= self.len()
            && data
                .iter()
                .zip(&self.values)
                .zip(&self.masks)
                .all(|((&actual, &expected), &mask)| actual & mask == expected)
    }

    pub fn canonical(&self) -> String {
        let mut result = String::with_capacity(self.len() * 3);
        for (index, (&value, &mask)) in self.values.iter().zip(&self.masks).enumerate() {
            if index != 0 {
                result.push(' ');
            }
            result.push(render_nibble(value >> 4, mask >> 4));
            result.push(render_nibble(value & 0x0F, mask & 0x0F));
        }
        result
    }
}

fn parse_nibble(nibble: u8, token: &str) -> Result<(u8, u8)> {
    match nibble {
        b'?' => Ok((0, 0)),
        b'0'..=b'9' => Ok((nibble - b'0', 0x0F)),
        b'a'..=b'f' => Ok((nibble - b'a' + 10, 0x0F)),
        b'A'..=b'F' => Ok((nibble - b'A' + 10, 0x0F)),
        _ => Err(Error::Value(format!(
            "invalid wildcard byte-pattern token '{token}'"
        ))),
    }
}

fn render_nibble(value: u8, mask: u8) -> char {
    if mask == 0 {
        '?'
    } else {
        char::from_digit(value as u32, 16)
            .unwrap_or('?')
            .to_ascii_uppercase()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub range: Option<AddressRange>,
    /// Match only addresses that are multiples of this value.
    pub alignment: u64,
    /// Stop after this many matches. `None` has no result limit.
    pub max_results: Option<usize>,
    pub allow_overlaps: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            range: None,
            alignment: 1,
            max_results: None,
            allow_overlaps: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaceReport {
    pub matches: usize,
    pub replacements: usize,
    pub bytes_changed: u64,
}

impl MemorySegmentList {
    /// Finds a masked byte pattern in initialized contiguous address runs.
    /// Matches never cross a hole but may cross adjacent segment boundaries.
    pub fn find_pattern(&self, pattern: &BytePattern, options: SearchOptions) -> Result<Vec<u64>> {
        if options.alignment == 0 {
            return Err(Error::Value(
                "search alignment must be greater than zero".to_string(),
            ));
        }
        if options.max_results == Some(0) {
            return Ok(Vec::new());
        }
        self.validate_image()?;
        let mut segments = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized() && segment.size() != 0)
            .collect::<Vec<_>>();
        segments.sort_by_key(|segment| segment.address);

        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for segment in segments {
            let segment_end = segment
                .address
                .checked_add(segment.size() as u64)
                .ok_or_else(|| Error::Value("segment address overflows u64".to_string()))?;
            let (start, end) = match options.range {
                Some(range) => (segment.address.max(range.start), segment_end.min(range.end)),
                None => (segment.address, segment_end),
            };
            if start >= end {
                continue;
            }
            let offset = (start - segment.address) as usize;
            let len = (end - start) as usize;
            if let Some((run_start, run_data)) = runs.last_mut() {
                if *run_start + run_data.len() as u64 == start {
                    run_data.extend_from_slice(&segment.data()[offset..offset + len]);
                    continue;
                }
            }
            runs.push((start, segment.data()[offset..offset + len].to_vec()));
        }

        let mut matches = Vec::new();
        for (run_start, data) in runs {
            if data.len() < pattern.len() {
                continue;
            }
            let mut offset = 0usize;
            while offset <= data.len() - pattern.len() {
                let address = run_start + offset as u64;
                if address.is_multiple_of(options.alignment)
                    && pattern.matches(&data[offset..offset + pattern.len()])
                {
                    matches.push(address);
                    if options
                        .max_results
                        .is_some_and(|maximum| matches.len() >= maximum)
                    {
                        return Ok(matches);
                    }
                    offset += if options.allow_overlaps {
                        1
                    } else {
                        pattern.len()
                    };
                } else {
                    offset += 1;
                }
            }
        }
        Ok(matches)
    }

    /// Replaces non-overlapping matches atomically without changing segment
    /// addresses, sizes, or memory-program types.
    pub fn replace_pattern(
        &mut self,
        pattern: &BytePattern,
        replacement: &[u8],
        mut options: SearchOptions,
    ) -> Result<ReplaceReport> {
        if replacement.len() != pattern.len() {
            return Err(Error::Value(
                "replacement length must equal search pattern length".to_string(),
            ));
        }
        options.allow_overlaps = false;
        let matches = self.find_pattern(pattern, options)?;
        let mut work = self.clone();
        let mut bytes_changed = 0u64;
        for address in &matches {
            let original =
                self.read_exact(AddressRange::from_start_len(*address, pattern.len())?)?;
            bytes_changed += original
                .iter()
                .zip(replacement)
                .filter(|(left, right)| left != right)
                .count() as u64;
            write_preserving_segments(&mut work, *address, replacement)?;
        }
        *self = work;
        Ok(ReplaceReport {
            matches: matches.len(),
            replacements: matches.len(),
            bytes_changed,
        })
    }
}

fn write_preserving_segments(
    image: &mut MemorySegmentList,
    mut address: u64,
    mut data: &[u8],
) -> Result<()> {
    while !data.is_empty() {
        let segment = image
            .segments
            .iter_mut()
            .find(|segment| {
                segment.is_initialized()
                    && segment.address <= address
                    && segment
                        .address
                        .checked_add(segment.size() as u64)
                        .is_some_and(|end| address < end)
            })
            .ok_or_else(|| Error::Value(format!("uninitialized byte at 0x{address:X}")))?;
        let offset = (address - segment.address) as usize;
        let count = data.len().min(segment.size() - offset);
        segment.set_data_bytes(offset, &data[..count]);
        address = address
            .checked_add(count as u64)
            .ok_or_else(|| Error::Value("replacement address overflows u64".to_string()))?;
        data = &data[count..];
    }
    Ok(())
}

impl DataFileBase {
    pub fn find_pattern(&self, pattern: &BytePattern, options: SearchOptions) -> Result<Vec<u64>> {
        self.segment_list.find_pattern(pattern, options)
    }

    pub fn replace_pattern(
        &mut self,
        pattern: &BytePattern,
        replacement: &[u8],
        options: SearchOptions,
    ) -> Result<ReplaceReport> {
        let report = self
            .segment_list
            .replace_pattern(pattern, replacement, options)?;
        self.is_dirty |= report.bytes_changed != 0;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use autors_a2l::model::enums::MemoryPrgType;

    use super::*;
    use crate::datafile::MemorySegment;

    fn segment(address: u64, data: &[u8]) -> MemorySegment {
        MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::CODE, true)
    }

    #[test]
    fn parses_exact_and_nibble_wildcard_forms() {
        let pattern = BytePattern::parse("0xDE AD,?? B? ?f").unwrap();
        assert_eq!(pattern.canonical(), "DE AD ?? B? ?F");
        assert!(pattern.matches(&[0xDE, 0xAD, 0x55, 0xB7, 0x2F]));
        assert!(!pattern.matches(&[0xDE, 0xAD, 0x55, 0xA7, 0x2F]));
        assert_eq!(BytePattern::parse("DEADBEEF").unwrap().len(), 4);
        assert!(BytePattern::parse("ABC").is_err());
        assert!(BytePattern::parse("GG").is_err());
    }

    #[test]
    fn search_crosses_adjacent_segments_and_honors_filters() {
        let image = MemorySegmentList {
            segments: vec![
                segment(0x1000, &[0xAA, 0xBB]),
                segment(0x1002, &[0xCC, 0xAA, 0xBC, 0xCC]),
                segment(0x2000, &[0xAA, 0xBD, 0xCC]),
            ],
        };
        let pattern = BytePattern::parse("AA B? CC").unwrap();
        assert_eq!(
            image
                .find_pattern(&pattern, SearchOptions::default())
                .unwrap(),
            [0x1000, 0x1003, 0x2000]
        );
        assert_eq!(
            image
                .find_pattern(
                    &pattern,
                    SearchOptions {
                        range: Some(AddressRange::new(0x1001, 0x2003).unwrap()),
                        alignment: 2,
                        max_results: Some(1),
                        allow_overlaps: true,
                    }
                )
                .unwrap(),
            [0x2000]
        );
    }

    #[test]
    fn replacement_is_atomic_and_preserves_segment_metadata() {
        let original = MemorySegmentList {
            segments: vec![segment(0x1000, &[1, 2]), segment(0x1002, &[3, 1, 2, 3])],
        };
        let pattern = BytePattern::exact(vec![1, 2, 3]).unwrap();
        let mut image = original.clone();
        assert!(image
            .replace_pattern(&pattern, &[9, 9], SearchOptions::default())
            .is_err());
        assert_eq!(image, original);

        let report = image
            .replace_pattern(&pattern, &[9, 8, 7], SearchOptions::default())
            .unwrap();
        assert_eq!(report.matches, 2);
        assert_eq!(report.bytes_changed, 6);
        assert!(image
            .segments
            .iter()
            .all(|segment| segment.prg_type == MemoryPrgType::CODE));
        assert_eq!(
            image
                .read_exact(AddressRange::new(0x1000, 0x1006).unwrap())
                .unwrap(),
            [9, 8, 7, 9, 8, 7]
        );
    }

    #[test]
    fn data_file_dirty_state_only_tracks_changed_bytes() {
        let mut base = DataFileBase::new(
            None,
            MemorySegmentList {
                segments: vec![segment(0x1000, &[1, 2, 3])],
            },
        );
        let pattern = BytePattern::exact(vec![1, 2, 3]).unwrap();
        base.replace_pattern(&pattern, &[1, 2, 3], SearchOptions::default())
            .unwrap();
        assert!(!base.is_dirty);
        base.replace_pattern(&pattern, &[3, 2, 1], SearchOptions::default())
            .unwrap();
        assert!(base.is_dirty);
    }
}
