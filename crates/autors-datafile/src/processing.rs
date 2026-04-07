//! Transactional and extensible data-processing operations.

use autors_a2l::model::enums::MemoryPrgType;

use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::image::{AddressRange, OverlapPolicy};
use crate::{Error, Result};

/// A format-independent transformation applied to one address block.
///
/// Implementations may change the byte length. All selected blocks are
/// transformed before the image is modified, and the complete operation is
/// committed only when every output block can be inserted successfully.
pub trait DataProcessor {
    fn name(&self) -> &str;

    fn process(&self, address: u64, data: &[u8]) -> Result<Vec<u8>>;
}

/// Selection and collision rules for a processing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingOptions {
    /// `None` processes every initialized segment independently. A range
    /// processes exactly one block starting at `range.start`.
    pub range: Option<AddressRange>,
    /// When a range is selected, materialize address holes with this byte.
    /// Without a fill value, the complete range must be initialized.
    pub gap_fill: Option<u8>,
    /// How resized output treats bytes outside the original selection.
    pub overlap: OverlapPolicy,
}

impl Default for ProcessingOptions {
    fn default() -> Self {
        Self {
            range: None,
            gap_fill: None,
            overlap: OverlapPolicy::Reject,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessingReport {
    pub processor: String,
    pub blocks_processed: usize,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub image_changed: bool,
}

#[derive(Debug, Clone)]
struct ProcessedBlock {
    source_range: AddressRange,
    output: Vec<u8>,
    prg_type: MemoryPrgType,
    bin_file_offset: i64,
}

impl MemorySegmentList {
    /// Applies a processor atomically to all initialized segments or one
    /// explicit address range.
    pub fn process_with(
        &mut self,
        processor: &dyn DataProcessor,
        options: ProcessingOptions,
    ) -> Result<ProcessingReport> {
        self.validate_image()?;
        let blocks = match options.range {
            Some(range) => vec![self.process_explicit_range(processor, range, options.gap_fill)?],
            None => self.process_initialized_segments(processor)?,
        };

        let mut report = ProcessingReport {
            processor: processor.name().to_string(),
            blocks_processed: blocks.len(),
            input_bytes: blocks.iter().map(|block| block.source_range.len()).sum(),
            output_bytes: blocks.iter().map(|block| block.output.len() as u64).sum(),
            image_changed: false,
        };
        let before = self.clone();
        let mut work = self.clone();
        for block in &blocks {
            work.erase_range(block.source_range)?;
        }
        for block in blocks {
            if block.output.is_empty() {
                continue;
            }
            let mut segment = MemorySegment::from_data(
                block.source_range.start,
                block.output,
                block.prg_type,
                true,
            );
            segment.bin_file_offset = block.bin_file_offset;
            work.insert_segment(segment, options.overlap)?;
        }
        report.image_changed = work != before;
        *self = work;
        Ok(report)
    }

    fn process_initialized_segments(
        &self,
        processor: &dyn DataProcessor,
    ) -> Result<Vec<ProcessedBlock>> {
        let mut segments: Vec<&MemorySegment> = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized() && segment.size() != 0)
            .collect();
        segments.sort_by_key(|segment| segment.address);
        segments
            .into_iter()
            .map(|segment| {
                let source_range = AddressRange::from_start_len(segment.address, segment.size())?;
                let output = processor.process(segment.address, segment.data())?;
                Ok(ProcessedBlock {
                    source_range,
                    output,
                    prg_type: segment.prg_type,
                    bin_file_offset: segment.bin_file_offset,
                })
            })
            .collect()
    }

    fn process_explicit_range(
        &self,
        processor: &dyn DataProcessor,
        range: AddressRange,
        gap_fill: Option<u8>,
    ) -> Result<ProcessedBlock> {
        let data = match gap_fill {
            Some(fill) => self.read_filled(range, fill)?,
            None => self.read_exact(range)?,
        };
        let source_segment = self.segments.iter().find(|segment| {
            segment.is_initialized() && segment.address <= range.start && {
                segment
                    .address
                    .checked_add(segment.size() as u64)
                    .is_some_and(|end| end > range.start)
            }
        });
        let prg_type = source_segment
            .map(|segment| segment.prg_type)
            .unwrap_or(MemoryPrgType::DATA);
        let bin_file_offset = source_segment
            .map(|segment| segment.bin_file_offset)
            .unwrap_or(0);
        let output = processor.process(range.start, &data)?;
        Ok(ProcessedBlock {
            source_range: range,
            output,
            prg_type,
            bin_file_offset,
        })
    }
}

impl DataFileBase {
    pub fn process_with(
        &mut self,
        processor: &dyn DataProcessor,
        options: ProcessingOptions,
    ) -> Result<ProcessingReport> {
        let report = self.segment_list.process_with(processor, options)?;
        self.is_dirty |= report.image_changed;
        Ok(report)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProcessor;

impl DataProcessor for NoopProcessor {
    fn name(&self) -> &str {
        "no action"
    }

    fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

/// Repeats a non-empty XOR pattern across every selected block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorProcessor {
    pattern: Vec<u8>,
}

impl XorProcessor {
    pub fn new(pattern: impl Into<Vec<u8>>) -> Result<Self> {
        let pattern = pattern.into();
        if pattern.is_empty() {
            return Err(Error::Value("XOR pattern must not be empty".to_string()));
        }
        Ok(Self { pattern })
    }

    pub fn invert() -> Self {
        Self {
            pattern: vec![0xFF],
        }
    }

    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }
}

impl DataProcessor for XorProcessor {
    fn name(&self) -> &str {
        "XOR"
    }

    fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data
            .iter()
            .zip(self.pattern.iter().cycle())
            .map(|(byte, mask)| byte ^ mask)
            .collect())
    }
}

/// Reverses byte order inside fixed-size units. A short final unit is also
/// reversed, so the operation is its own inverse for every input length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapProcessor {
    unit_size: usize,
}

impl SwapProcessor {
    pub fn new(unit_size: usize) -> Result<Self> {
        if unit_size < 2 {
            return Err(Error::Value(
                "swap unit size must be at least two bytes".to_string(),
            ));
        }
        Ok(Self { unit_size })
    }

    pub fn words() -> Self {
        Self { unit_size: 2 }
    }

    pub fn longwords() -> Self {
        Self { unit_size: 4 }
    }

    pub fn unit_size(self) -> usize {
        self.unit_size
    }
}

impl DataProcessor for SwapProcessor {
    fn name(&self) -> &str {
        "byte-order swap"
    }

    fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
        let mut output = data.to_vec();
        for unit in output.chunks_mut(self.unit_size) {
            unit.reverse();
        }
        Ok(output)
    }
}

/// Automotive run-length encoding with a two-bit item type and six-bit count.
/// Packet types are plain bytes, repeated byte, repeated word, and repeated
/// longword. Counts are in the range 1..=63.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArleCompressor;

impl DataProcessor for ArleCompressor {
    fn name(&self) -> &str {
        "ARLE compression"
    }

    fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
        Ok(arle_compress(data))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArleDecompressor {
    pub max_output_size: usize,
}

impl Default for ArleDecompressor {
    fn default() -> Self {
        Self {
            max_output_size: 256 * 1024 * 1024,
        }
    }
}

impl DataProcessor for ArleDecompressor {
    fn name(&self) -> &str {
        "ARLE decompression"
    }

    fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
        arle_decompress(data, self.max_output_size)
    }
}

const ARLE_PLAIN: u8 = 0b00 << 6;
const ARLE_BYTE: u8 = 0b01 << 6;
const ARLE_WORD: u8 = 0b10 << 6;
const ARLE_LONGWORD: u8 = 0b11 << 6;
const ARLE_MAX_COUNT: usize = 0x3F;

fn repeated_units(data: &[u8], offset: usize, unit_size: usize) -> usize {
    if offset + unit_size > data.len() {
        return 0;
    }
    let unit = &data[offset..offset + unit_size];
    let mut count = 1usize;
    while count < ARLE_MAX_COUNT {
        let start = offset + count * unit_size;
        let end = start + unit_size;
        if end > data.len() || &data[start..end] != unit {
            break;
        }
        count += 1;
    }
    count
}

fn best_repeat(data: &[u8], offset: usize) -> Option<(u8, usize, usize)> {
    let candidates = [
        (ARLE_BYTE, 1usize, 3usize),
        (ARLE_WORD, 2usize, 2usize),
        (ARLE_LONGWORD, 4usize, 2usize),
    ];
    candidates
        .into_iter()
        .filter_map(|(kind, unit_size, minimum_count)| {
            let count = repeated_units(data, offset, unit_size);
            if count < minimum_count {
                return None;
            }
            let input_size = count * unit_size;
            let output_size = 1 + unit_size;
            Some((kind, unit_size, count, input_size - output_size))
        })
        .max_by_key(|candidate| candidate.3)
        .map(|(kind, unit_size, count, _)| (kind, unit_size, count))
}

pub fn arle_compress(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        if let Some((kind, unit_size, count)) = best_repeat(data, offset) {
            output.push(kind | count as u8);
            output.extend_from_slice(&data[offset..offset + unit_size]);
            offset += unit_size * count;
            continue;
        }

        let plain_start = offset;
        offset += 1;
        while offset < data.len()
            && offset - plain_start < ARLE_MAX_COUNT
            && best_repeat(data, offset).is_none()
        {
            offset += 1;
        }
        let count = offset - plain_start;
        output.push(ARLE_PLAIN | count as u8);
        output.extend_from_slice(&data[plain_start..offset]);
    }
    output
}

pub fn arle_decompress(data: &[u8], max_output_size: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let header = data[offset];
        offset += 1;
        let count = (header & 0x3F) as usize;
        if count == 0 {
            return Err(Error::DataFile {
                line: 0,
                message: format!("ARLE packet at offset 0x{:X} has a zero count", offset - 1),
            });
        }
        let (unit_size, repetitions) = match header & 0xC0 {
            ARLE_PLAIN => (count, 1usize),
            ARLE_BYTE => (1usize, count),
            ARLE_WORD => (2usize, count),
            ARLE_LONGWORD => (4usize, count),
            _ => unreachable!("two type bits cover every value"),
        };
        let end = offset
            .checked_add(unit_size)
            .ok_or_else(|| Error::Value("ARLE packet size overflows usize".to_string()))?;
        if end > data.len() {
            return Err(Error::DataFile {
                line: 0,
                message: format!("truncated ARLE packet at offset 0x{:X}", offset - 1),
            });
        }
        let additional = unit_size
            .checked_mul(repetitions)
            .ok_or_else(|| Error::Value("ARLE output size overflows usize".to_string()))?;
        let output_size = output
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::Value("ARLE output size overflows usize".to_string()))?;
        if output_size > max_output_size {
            return Err(Error::Value(format!(
                "ARLE output exceeds configured limit of {max_output_size} byte(s)"
            )));
        }
        let unit = &data[offset..end];
        for _ in 0..repetitions {
            output.extend_from_slice(unit);
        }
        offset = end;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    fn segment(address: u64, data: &[u8]) -> MemorySegment {
        MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::DATA, true)
    }

    #[test]
    fn xor_and_swap_match_observable_byte_operations() {
        let xor = XorProcessor::new(vec![0xFF, 0x0F]).unwrap();
        assert_eq!(
            xor.process(0, &[0x00, 0xF0, 0x55]).unwrap(),
            [0xFF, 0xFF, 0xAA]
        );

        let swap = SwapProcessor::longwords();
        assert_eq!(
            swap.process(0, &[1, 2, 3, 4, 5, 6]).unwrap(),
            [4, 3, 2, 1, 6, 5]
        );
        let restored = swap.process(0, &[4, 3, 2, 1, 6, 5]).unwrap();
        assert_eq!(restored, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn arle_uses_all_packet_types_and_round_trips() {
        let mut data = Vec::new();
        data.extend_from_slice(b"plain");
        data.extend_from_slice(&[0xAA; 10]);
        for _ in 0..6 {
            data.extend_from_slice(&[0x12, 0x34]);
        }
        for _ in 0..4 {
            data.extend_from_slice(&[1, 2, 3, 4]);
        }
        let compressed = arle_compress(&data);
        assert!(compressed.iter().any(|byte| byte & 0xC0 == ARLE_BYTE));
        assert!(compressed.iter().any(|byte| byte & 0xC0 == ARLE_WORD));
        assert!(compressed.iter().any(|byte| byte & 0xC0 == ARLE_LONGWORD));
        assert!(compressed.len() < data.len());
        assert_eq!(arle_decompress(&compressed, data.len()).unwrap(), data);
    }

    #[test]
    fn arle_splits_long_packets_and_rejects_malformed_input() {
        let data = vec![0xCC; 200];
        let compressed = arle_compress(&data);
        assert_eq!(arle_decompress(&compressed, 200).unwrap(), data);
        assert!(arle_decompress(&[ARLE_BYTE], 100).is_err());
        assert!(arle_decompress(&[ARLE_BYTE | 2, 0xAA], 1).is_err());
        assert!(arle_decompress(&[ARLE_PLAIN | 3, 1, 2], 100).is_err());
    }

    #[test]
    fn image_processing_is_transactional_when_resized_output_collides() {
        struct Grow;
        impl DataProcessor for Grow {
            fn name(&self) -> &str {
                "grow"
            }

            fn process(&self, _address: u64, data: &[u8]) -> Result<Vec<u8>> {
                let mut output = data.to_vec();
                output.extend_from_slice(&[9, 9]);
                Ok(output)
            }
        }

        let original = MemorySegmentList {
            segments: vec![segment(0x1000, &[1, 2]), segment(0x1003, &[3])],
        };
        let mut image = original.clone();
        assert!(image
            .process_with(
                &Grow,
                ProcessingOptions {
                    range: Some(AddressRange::new(0x1000, 0x1002).unwrap()),
                    ..ProcessingOptions::default()
                }
            )
            .is_err());
        assert_eq!(image, original);
    }

    #[test]
    fn range_processing_can_materialize_holes_and_resize() {
        let mut image = MemorySegmentList {
            segments: vec![segment(0x1000, &[1]), segment(0x1002, &[3])],
        };
        let report = image
            .process_with(
                &XorProcessor::invert(),
                ProcessingOptions {
                    range: Some(AddressRange::new(0x1000, 0x1003).unwrap()),
                    gap_fill: Some(0xFF),
                    overlap: OverlapPolicy::Reject,
                },
            )
            .unwrap();
        assert!(report.image_changed);
        assert_eq!(report.input_bytes, 3);
        assert_eq!(
            image
                .read_exact(AddressRange::new(0x1000, 0x1003).unwrap())
                .unwrap(),
            [0xFE, 0x00, 0xFC]
        );
    }

    #[test]
    fn base_dirty_state_changes_only_when_output_changes() {
        let mut base = DataFileBase::new(
            None,
            MemorySegmentList {
                segments: vec![segment(0x1000, &[1, 2])],
            },
        );
        let report = base
            .process_with(&NoopProcessor, ProcessingOptions::default())
            .unwrap();
        assert!(!report.image_changed);
        assert!(!base.is_dirty);
        base.process_with(&XorProcessor::invert(), ProcessingOptions::default())
            .unwrap();
        assert!(base.is_dirty);
    }
}
