//! Sparse memory-image operations shared by all data-file formats.

use autors_a2l::model::enums::MemoryPrgType;

use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::{Error, Result};

/// A half-open address range (`start..end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AddressRange {
    pub start: u64,
    pub end: u64,
}

impl AddressRange {
    /// Creates a non-empty half-open range.
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if start >= end {
            return Err(Error::Value(format!(
                "invalid address range 0x{start:X}..0x{end:X}"
            )));
        }
        Ok(Self { start, end })
    }

    /// Creates a range from a start address and a non-zero byte length.
    pub fn from_start_len(start: u64, len: usize) -> Result<Self> {
        if len == 0 {
            return Err(Error::Value("address range must not be empty".to_string()));
        }
        let end = start
            .checked_add(len as u64)
            .ok_or_else(|| Error::Value("address range overflows u64".to_string()))?;
        Self::new(start, end)
    }

    pub fn len(self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn contains(self, address: u64) -> bool {
        address >= self.start && address < self.end
    }

    pub fn intersects(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Self { start, end })
    }
}

/// Parses colon-separated ranges. Each range is either `start-end` (inclusive
/// end, matching HexView notation) or `start,length`. Numbers are decimal or
/// use a `0x` hexadecimal prefix.
pub fn parse_address_ranges(text: &str) -> Result<Vec<AddressRange>> {
    let mut ranges = Vec::new();
    for item in text
        .split(':')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        if let Some((start, length)) = item.split_once(',') {
            let start = parse_address_number(start)?;
            let length = parse_address_number(length)?;
            let length = usize::try_from(length)
                .map_err(|_| Error::Value("range length does not fit usize".to_string()))?;
            ranges.push(AddressRange::from_start_len(start, length)?);
        } else if let Some((start, inclusive_end)) = item.split_once('-') {
            let start = parse_address_number(start)?;
            let inclusive_end = parse_address_number(inclusive_end)?;
            let end = inclusive_end
                .checked_add(1)
                .ok_or_else(|| Error::Value("inclusive range end overflows u64".to_string()))?;
            ranges.push(AddressRange::new(start, end)?);
        } else {
            return Err(Error::Value(format!(
                "range '{item}' must use start-end or start,length notation"
            )));
        }
    }
    if ranges.is_empty() {
        return Err(Error::Value("no address ranges specified".to_string()));
    }
    Ok(ranges)
}

fn parse_address_number(text: &str) -> Result<u64> {
    let value = text.trim();
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|_| Error::Value(format!("invalid address value '{value}'")))
}

/// How an inserted or merged segment treats existing initialized bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlapPolicy {
    /// Return an error when initialized address ranges overlap.
    #[default]
    Reject,
    /// Keep existing initialized bytes and insert only into holes.
    Preserve,
    /// Replace existing bytes with the newly inserted bytes.
    Overwrite,
}

/// Controls a merge from another sparse image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeOptions {
    /// Optional source range. Bytes outside it are ignored.
    pub source_range: Option<AddressRange>,
    /// Signed address adjustment applied after clipping.
    pub address_offset: i64,
    pub overlap: OverlapPolicy,
    /// Ignore uninitialized layout placeholders in the source image.
    pub initialized_only: bool,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            source_range: None,
            address_offset: 0,
            overlap: OverlapPolicy::Reject,
            initialized_only: true,
        }
    }
}

/// Result statistics for a merge operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeReport {
    pub segments_processed: usize,
    pub bytes_processed: u64,
    pub overlapping_bytes: u64,
    pub bytes_written: u64,
}

/// Classification of one contiguous difference between two images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferenceKind {
    LeftOnly,
    RightOnly,
    Changed,
}

/// One contiguous difference. Missing sides are represented by `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDifference {
    pub address: u64,
    pub left: Option<Vec<u8>>,
    pub right: Option<Vec<u8>>,
}

impl ImageDifference {
    pub fn kind(&self) -> DifferenceKind {
        match (&self.left, &self.right) {
            (Some(_), Some(_)) => DifferenceKind::Changed,
            (Some(_), None) => DifferenceKind::LeftOnly,
            (None, Some(_)) => DifferenceKind::RightOnly,
            (None, None) => unreachable!("a difference always has at least one side"),
        }
    }

    pub fn len(&self) -> usize {
        self.left
            .as_ref()
            .map(Vec::len)
            .or_else(|| self.right.as_ref().map(Vec::len))
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn range(&self) -> AddressRange {
        AddressRange {
            start: self.address,
            end: self.address + self.len() as u64,
        }
    }
}

/// Aggregate information about initialized image data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageSummary {
    pub segment_count: usize,
    pub byte_count: u64,
    pub address_range: Option<AddressRange>,
    pub holes: Vec<AddressRange>,
}

fn segment_range(segment: &MemorySegment) -> Result<AddressRange> {
    AddressRange::from_start_len(segment.address, segment.size())
}

fn slice_segment(segment: &MemorySegment, range: AddressRange) -> MemorySegment {
    let offset = (range.start - segment.address) as usize;
    let len = range.len() as usize;
    let mut result = MemorySegment::from_data(
        range.start,
        segment.data()[offset..offset + len].to_vec(),
        segment.prg_type,
        segment.is_initialized(),
    );
    result.bin_file_offset = segment.bin_file_offset;
    result
}

fn subtract_segment(segment: &MemorySegment, cut: AddressRange) -> Result<Vec<MemorySegment>> {
    let range = segment_range(segment)?;
    let Some(overlap) = range.intersection(cut) else {
        return Ok(vec![segment.clone()]);
    };
    let mut pieces = Vec::with_capacity(2);
    if range.start < overlap.start {
        pieces.push(slice_segment(
            segment,
            AddressRange {
                start: range.start,
                end: overlap.start,
            },
        ));
    }
    if overlap.end < range.end {
        pieces.push(slice_segment(
            segment,
            AddressRange {
                start: overlap.end,
                end: range.end,
            },
        ));
    }
    Ok(pieces)
}

fn subtract_many(
    mut pieces: Vec<MemorySegment>,
    cuts: &[AddressRange],
) -> Result<Vec<MemorySegment>> {
    for cut in cuts {
        let mut next = Vec::new();
        for piece in &pieces {
            next.extend(subtract_segment(piece, *cut)?);
        }
        pieces = next;
    }
    Ok(pieces)
}

fn coalesce_segments(mut segments: Vec<MemorySegment>) -> Result<Vec<MemorySegment>> {
    segments.retain(|segment| segment.size() != 0);
    segments.sort_by_key(|segment| segment.address);
    let mut result: Vec<MemorySegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if let Some(previous) = result.last_mut() {
            let previous_end = previous
                .address
                .checked_add(previous.size() as u64)
                .ok_or_else(|| Error::Value("segment address overflows u64".to_string()))?;
            if previous_end == segment.address
                && previous.prg_type == segment.prg_type
                && previous.is_initialized() == segment.is_initialized()
                && previous.bin_file_offset == segment.bin_file_offset
            {
                let mut data = Vec::with_capacity(previous.size() + segment.size());
                data.extend_from_slice(previous.data());
                data.extend_from_slice(segment.data());
                let mut joined = MemorySegment::from_data(
                    previous.address,
                    data,
                    previous.prg_type,
                    previous.is_initialized(),
                );
                joined.bin_file_offset = previous.bin_file_offset;
                *previous = joined;
                continue;
            }
        }
        result.push(segment);
    }
    Ok(result)
}

fn shifted_address(address: u64, offset: i64) -> Result<u64> {
    if offset >= 0 {
        address
            .checked_add(offset as u64)
            .ok_or_else(|| Error::Value("address offset overflows u64".to_string()))
    } else {
        address
            .checked_sub(offset.unsigned_abs())
            .ok_or_else(|| Error::Value("address offset moves data below zero".to_string()))
    }
}

fn initialized_overlap_len(segments: &[MemorySegment], range: AddressRange) -> Result<u64> {
    let mut total = 0u64;
    for segment in segments.iter().filter(|segment| segment.is_initialized()) {
        if let Some(overlap) = segment_range(segment)?.intersection(range) {
            total += overlap.len();
        }
    }
    Ok(total)
}

impl MemorySegmentList {
    /// Checks address overflow and rejects overlapping segment ranges.
    pub fn validate_image(&self) -> Result<()> {
        let mut ranges: Vec<AddressRange> = self
            .segments
            .iter()
            .filter(|segment| segment.size() != 0)
            .map(segment_range)
            .collect::<Result<_>>()?;
        ranges.sort_by_key(|range| range.start);
        for pair in ranges.windows(2) {
            if pair[0].end > pair[1].start {
                return Err(Error::Value(format!(
                    "overlapping segments at 0x{:X}..0x{:X}",
                    pair[1].start,
                    pair[0].end.min(pair[1].end)
                )));
            }
        }
        Ok(())
    }

    /// Returns summary information for initialized bytes only.
    pub fn image_summary(&self) -> Result<ImageSummary> {
        self.validate_image()?;
        let mut ranges: Vec<AddressRange> = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized() && segment.size() != 0)
            .map(segment_range)
            .collect::<Result<_>>()?;
        ranges.sort_by_key(|range| range.start);
        let byte_count = ranges.iter().map(|range| range.len()).sum();
        let address_range = match (ranges.first(), ranges.last()) {
            (Some(first), Some(last)) => Some(AddressRange {
                start: first.start,
                end: last.end,
            }),
            _ => None,
        };
        let holes = ranges
            .windows(2)
            .filter_map(|pair| {
                (pair[0].end < pair[1].start).then_some(AddressRange {
                    start: pair[0].end,
                    end: pair[1].start,
                })
            })
            .collect();
        Ok(ImageSummary {
            segment_count: ranges.len(),
            byte_count,
            address_range,
            holes,
        })
    }

    /// Reads a fully initialized range and fails on the first hole.
    pub fn read_exact(&self, range: AddressRange) -> Result<Vec<u8>> {
        self.validate_image()?;
        let output_len = usize::try_from(range.len())
            .map_err(|_| Error::Value("requested range is too large".to_string()))?;
        let mut output = Vec::with_capacity(output_len);
        let mut cursor = range.start;
        let mut segments: Vec<&MemorySegment> = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        segments.sort_by_key(|segment| segment.address);
        for segment in segments {
            let segment_range = segment_range(segment)?;
            if segment_range.end <= cursor || segment_range.start >= range.end {
                continue;
            }
            if segment_range.start > cursor {
                return Err(Error::Value(format!("uninitialized byte at 0x{cursor:X}")));
            }
            let copy_end = segment_range.end.min(range.end);
            let offset = (cursor - segment.address) as usize;
            let len = (copy_end - cursor) as usize;
            output.extend_from_slice(&segment.data()[offset..offset + len]);
            cursor = copy_end;
            if cursor == range.end {
                return Ok(output);
            }
        }
        Err(Error::Value(format!("uninitialized byte at 0x{cursor:X}")))
    }

    /// Materializes a range, replacing holes with `fill`.
    pub fn read_filled(&self, range: AddressRange, fill: u8) -> Result<Vec<u8>> {
        self.validate_image()?;
        let output_len = usize::try_from(range.len())
            .map_err(|_| Error::Value("requested range is too large".to_string()))?;
        let mut output = vec![fill; output_len];
        for segment in self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
        {
            let Some(overlap) = segment_range(segment)?.intersection(range) else {
                continue;
            };
            let source = (overlap.start - segment.address) as usize;
            let target = (overlap.start - range.start) as usize;
            let len = overlap.len() as usize;
            output[target..target + len].copy_from_slice(&segment.data()[source..source + len]);
        }
        Ok(output)
    }

    /// Inserts one segment transactionally using the selected overlap policy.
    pub fn insert_segment(&mut self, segment: MemorySegment, policy: OverlapPolicy) -> Result<u64> {
        if segment.size() == 0 {
            return Ok(0);
        }
        self.validate_image()?;
        let new_range = segment_range(&segment)?;
        let overlap = initialized_overlap_len(&self.segments, new_range)?;
        let mut result = Vec::new();
        match policy {
            OverlapPolicy::Overwrite => {
                for existing in &self.segments {
                    result.extend(subtract_segment(existing, new_range)?);
                }
                result.push(segment);
            }
            OverlapPolicy::Reject => {
                if overlap != 0 {
                    return Err(Error::Value(format!(
                        "new segment overlaps {overlap} initialized byte(s)"
                    )));
                }
                for existing in &self.segments {
                    if existing.is_initialized() {
                        result.push(existing.clone());
                    } else {
                        result.extend(subtract_segment(existing, new_range)?);
                    }
                }
                result.push(segment);
            }
            OverlapPolicy::Preserve => {
                let initialized_ranges = self
                    .segments
                    .iter()
                    .filter(|existing| existing.is_initialized())
                    .map(segment_range)
                    .collect::<Result<Vec<_>>>()?;
                let pending = subtract_many(vec![segment], &initialized_ranges)?;
                let pending_ranges = pending
                    .iter()
                    .map(segment_range)
                    .collect::<Result<Vec<_>>>()?;
                for existing in &self.segments {
                    if existing.is_initialized() {
                        result.push(existing.clone());
                    } else {
                        result.extend(subtract_many(vec![existing.clone()], &pending_ranges)?);
                    }
                }
                result.extend(pending);
            }
        }
        self.segments = coalesce_segments(result)?;
        Ok(match policy {
            OverlapPolicy::Preserve => new_range.len() - overlap,
            _ => new_range.len(),
        })
    }

    /// Writes initialized bytes at an absolute address, replacing old data.
    pub fn write_at(&mut self, address: u64, data: &[u8]) -> Result<u64> {
        if data.is_empty() {
            return Ok(0);
        }
        self.insert_segment(
            MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::UNKNOWN, true),
            OverlapPolicy::Overwrite,
        )
    }

    /// Merges a source image with optional clipping and address relocation.
    pub fn merge_from(
        &mut self,
        source: &MemorySegmentList,
        options: MergeOptions,
    ) -> Result<MergeReport> {
        self.validate_image()?;
        source.validate_image()?;
        let mut work = self.clone();
        let mut report = MergeReport::default();
        let mut source_segments: Vec<&MemorySegment> = source.segments.iter().collect();
        source_segments.sort_by_key(|segment| segment.address);
        for source_segment in source_segments {
            if options.initialized_only && !source_segment.is_initialized() {
                continue;
            }
            let source_segment_range = segment_range(source_segment)?;
            let clipped = match options.source_range {
                Some(filter) => source_segment_range.intersection(filter),
                None => Some(source_segment_range),
            };
            let Some(clipped) = clipped else {
                continue;
            };
            let mut segment = slice_segment(source_segment, clipped);
            segment.address = shifted_address(segment.address, options.address_offset)?;
            let target_range = segment_range(&segment)?;
            let overlap = initialized_overlap_len(&work.segments, target_range)?;
            let written = work.insert_segment(segment, options.overlap)?;
            report.segments_processed += 1;
            report.bytes_processed += clipped.len();
            report.overlapping_bytes += overlap;
            report.bytes_written += written;
        }
        *self = work;
        Ok(report)
    }

    /// Removes bytes in a range and splits segments at the range boundaries.
    pub fn erase_range(&mut self, range: AddressRange) -> Result<u64> {
        self.validate_image()?;
        let erased = initialized_overlap_len(&self.segments, range)?;
        let mut result = Vec::new();
        for segment in &self.segments {
            result.extend(subtract_segment(segment, range)?);
        }
        self.segments = coalesce_segments(result)?;
        Ok(erased)
    }

    /// Fills a range with a repeating non-empty pattern.
    pub fn fill_range(
        &mut self,
        range: AddressRange,
        pattern: &[u8],
        policy: OverlapPolicy,
    ) -> Result<u64> {
        if pattern.is_empty() {
            return Err(Error::Value("fill pattern must not be empty".to_string()));
        }
        let len = usize::try_from(range.len())
            .map_err(|_| Error::Value("fill range is too large".to_string()))?;
        let data = pattern.iter().copied().cycle().take(len).collect();
        self.insert_segment(
            MemorySegment::from_data(range.start, data, MemoryPrgType::UNKNOWN, true),
            policy,
        )
    }

    /// Aligns every initialized block start and end, filling new bytes without
    /// replacing existing data.
    pub fn align_blocks(&mut self, alignment: u64, fill: u8) -> Result<u64> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(Error::Value(
                "alignment must be a non-zero power of two".to_string(),
            ));
        }
        self.validate_image()?;
        let ranges = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .map(segment_range)
            .collect::<Result<Vec<_>>>()?;
        let mut work = self.clone();
        let mut written = 0u64;
        for range in ranges {
            let start = range.start & !(alignment - 1);
            let end = range
                .end
                .checked_add(alignment - 1)
                .ok_or_else(|| Error::Value("aligned address overflows u64".to_string()))?
                & !(alignment - 1);
            written += work.fill_range(
                AddressRange { start, end },
                &[fill],
                OverlapPolicy::Preserve,
            )?;
        }
        *self = work;
        Ok(written)
    }

    /// Splits segments into blocks no larger than `max_block_size`.
    pub fn split_blocks(&mut self, max_block_size: usize) -> Result<usize> {
        if max_block_size == 0 {
            return Err(Error::Value(
                "maximum block size must not be zero".to_string(),
            ));
        }
        self.validate_image()?;
        let mut result = Vec::new();
        for segment in &self.segments {
            let range = segment_range(segment)?;
            let mut start = range.start;
            while start < range.end {
                let end = range.end.min(start + max_block_size as u64);
                result.push(slice_segment(segment, AddressRange { start, end }));
                start = end;
            }
        }
        let count = result.len();
        result.sort_by_key(|segment| segment.address);
        self.segments = result;
        Ok(count)
    }

    /// Moves bytes in `source_range` by a signed address offset.
    pub fn remap_range(
        &mut self,
        source_range: AddressRange,
        address_offset: i64,
        overlap: OverlapPolicy,
    ) -> Result<MergeReport> {
        self.validate_image()?;
        let mut extracted = MemorySegmentList::new();
        for segment in self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
        {
            if let Some(range) = segment_range(segment)?.intersection(source_range) {
                extracted.segments.push(slice_segment(segment, range));
            }
        }
        let mut work = self.clone();
        work.erase_range(source_range)?;
        let report = work.merge_from(
            &extracted,
            MergeOptions {
                address_offset,
                overlap,
                ..MergeOptions::default()
            },
        )?;
        *self = work;
        Ok(report)
    }

    /// Finds every occurrence in initialized data. Matches may cross adjacent
    /// segment boundaries but never cross address holes.
    pub fn find_all(&self, needle: &[u8]) -> Result<Vec<u64>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        self.validate_image()?;
        let mut segments: Vec<&MemorySegment> = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        segments.sort_by_key(|segment| segment.address);
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for segment in segments {
            if let Some((start, data)) = runs.last_mut() {
                if *start + data.len() as u64 == segment.address {
                    data.extend_from_slice(segment.data());
                    continue;
                }
            }
            runs.push((segment.address, segment.data().to_vec()));
        }
        let mut matches = Vec::new();
        for (start, data) in runs {
            if data.len() < needle.len() {
                continue;
            }
            for offset in 0..=data.len() - needle.len() {
                if data[offset..].starts_with(needle) {
                    matches.push(start + offset as u64);
                }
            }
        }
        Ok(matches)
    }

    /// Compares initialized bytes and returns only differing address spans.
    pub fn compare(&self, other: &MemorySegmentList) -> Result<Vec<ImageDifference>> {
        self.validate_image()?;
        other.validate_image()?;
        let left: Vec<&MemorySegment> = self
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        let right: Vec<&MemorySegment> = other
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        let mut boundaries = Vec::with_capacity((left.len() + right.len()) * 2);
        for segment in left.iter().chain(right.iter()) {
            let range = segment_range(segment)?;
            boundaries.push(range.start);
            boundaries.push(range.end);
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut differences = Vec::new();
        for pair in boundaries.windows(2) {
            let range = AddressRange {
                start: pair[0],
                end: pair[1],
            };
            let left_bytes = bytes_for_interval(&left, range);
            let right_bytes = bytes_for_interval(&right, range);
            match (left_bytes, right_bytes) {
                (None, None) => {}
                (Some(bytes), None) => {
                    push_difference(&mut differences, range.start, Some(bytes), None)
                }
                (None, Some(bytes)) => {
                    push_difference(&mut differences, range.start, None, Some(bytes))
                }
                (Some(left_bytes), Some(right_bytes)) => {
                    let mut offset = 0usize;
                    while offset < left_bytes.len() {
                        if left_bytes[offset] == right_bytes[offset] {
                            offset += 1;
                            continue;
                        }
                        let start = offset;
                        while offset < left_bytes.len() && left_bytes[offset] != right_bytes[offset]
                        {
                            offset += 1;
                        }
                        push_difference(
                            &mut differences,
                            range.start + start as u64,
                            Some(left_bytes[start..offset].to_vec()),
                            Some(right_bytes[start..offset].to_vec()),
                        );
                    }
                }
            }
        }
        Ok(differences)
    }
}

fn bytes_for_interval(segments: &[&MemorySegment], range: AddressRange) -> Option<Vec<u8>> {
    segments.iter().find_map(|segment| {
        let end = segment.address + segment.size() as u64;
        if segment.address <= range.start && end >= range.end {
            let offset = (range.start - segment.address) as usize;
            let len = range.len() as usize;
            Some(segment.data()[offset..offset + len].to_vec())
        } else {
            None
        }
    })
}

fn push_difference(
    differences: &mut Vec<ImageDifference>,
    address: u64,
    left: Option<Vec<u8>>,
    right: Option<Vec<u8>>,
) {
    let kind = match (&left, &right) {
        (Some(_), Some(_)) => DifferenceKind::Changed,
        (Some(_), None) => DifferenceKind::LeftOnly,
        (None, Some(_)) => DifferenceKind::RightOnly,
        (None, None) => return,
    };
    if let Some(previous) = differences.last_mut() {
        if previous.address + previous.len() as u64 == address && previous.kind() == kind {
            match (&mut previous.left, left) {
                (Some(previous), Some(next)) => previous.extend(next),
                (None, None) => {}
                _ => unreachable!("difference kind fixes side presence"),
            }
            match (&mut previous.right, right) {
                (Some(previous), Some(next)) => previous.extend(next),
                (None, None) => {}
                _ => unreachable!("difference kind fixes side presence"),
            }
            return;
        }
    }
    differences.push(ImageDifference {
        address,
        left,
        right,
    });
}

impl DataFileBase {
    /// Writes bytes and marks the file dirty when data changed.
    pub fn write_at(&mut self, address: u64, data: &[u8]) -> Result<u64> {
        let written = self.segment_list.write_at(address, data)?;
        self.is_dirty |= written != 0;
        Ok(written)
    }

    /// Merges another file image and marks the file dirty on success.
    pub fn merge_image(
        &mut self,
        source: &DataFileBase,
        options: MergeOptions,
    ) -> Result<MergeReport> {
        let report = self
            .segment_list
            .merge_from(&source.segment_list, options)?;
        self.is_dirty |= report.bytes_written != 0;
        Ok(report)
    }

    pub fn erase_range(&mut self, range: AddressRange) -> Result<u64> {
        let erased = self.segment_list.erase_range(range)?;
        self.is_dirty |= erased != 0;
        Ok(erased)
    }

    pub fn fill_range(
        &mut self,
        range: AddressRange,
        pattern: &[u8],
        policy: OverlapPolicy,
    ) -> Result<u64> {
        let written = self.segment_list.fill_range(range, pattern, policy)?;
        self.is_dirty |= written != 0;
        Ok(written)
    }

    pub fn align_blocks(&mut self, alignment: u64, fill: u8) -> Result<u64> {
        let written = self.segment_list.align_blocks(alignment, fill)?;
        self.is_dirty |= written != 0;
        Ok(written)
    }

    pub fn split_blocks(&mut self, max_block_size: usize) -> Result<usize> {
        let old_count = self.segment_list.len();
        let count = self.segment_list.split_blocks(max_block_size)?;
        self.is_dirty |= count != old_count;
        Ok(count)
    }

    pub fn remap_range(
        &mut self,
        source_range: AddressRange,
        address_offset: i64,
        overlap: OverlapPolicy,
    ) -> Result<MergeReport> {
        let report = self
            .segment_list
            .remap_range(source_range, address_offset, overlap)?;
        self.is_dirty |= report.bytes_written != 0;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(address: u64, data: &[u8]) -> MemorySegment {
        MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::DATA, true)
    }

    fn image(segments: Vec<MemorySegment>) -> MemorySegmentList {
        MemorySegmentList { segments }
    }

    #[test]
    fn range_validation_and_intersection() {
        assert!(AddressRange::new(2, 2).is_err());
        assert!(AddressRange::from_start_len(u64::MAX, 2).is_err());
        let a = AddressRange::new(10, 20).unwrap();
        let b = AddressRange::new(15, 25).unwrap();
        assert_eq!(a.intersection(b), Some(AddressRange { start: 15, end: 20 }));
        assert!(a.contains(10));
        assert!(!a.contains(20));
    }

    #[test]
    fn parses_hexview_style_range_lists() {
        assert_eq!(
            parse_address_ranges("0x190,0x20:0x9020-0x903f").unwrap(),
            vec![
                AddressRange {
                    start: 0x190,
                    end: 0x1B0,
                },
                AddressRange {
                    start: 0x9020,
                    end: 0x9040,
                },
            ]
        );
        assert!(parse_address_ranges("0x1000").is_err());
    }

    #[test]
    fn exact_and_filled_reads_handle_sparse_ranges() {
        let image = image(vec![segment(0x1000, &[1, 2]), segment(0x1004, &[5, 6])]);
        assert_eq!(
            image
                .read_filled(AddressRange::new(0x1000, 0x1006).unwrap(), 0xFF)
                .unwrap(),
            &[1, 2, 0xFF, 0xFF, 5, 6]
        );
        assert!(image
            .read_exact(AddressRange::new(0x1000, 0x1006).unwrap())
            .is_err());
        assert_eq!(
            image
                .read_exact(AddressRange::new(0x1004, 0x1006).unwrap())
                .unwrap(),
            &[5, 6]
        );
    }

    #[test]
    fn insert_policies_are_transactional() {
        let original = image(vec![segment(0x1002, &[9, 9])]);
        let incoming = segment(0x1000, &[1, 2, 3, 4, 5, 6]);

        let mut reject = original.clone();
        assert!(reject
            .insert_segment(incoming.clone(), OverlapPolicy::Reject)
            .is_err());
        assert_eq!(reject, original);

        let mut preserve = original.clone();
        assert_eq!(
            preserve
                .insert_segment(incoming.clone(), OverlapPolicy::Preserve)
                .unwrap(),
            4
        );
        assert_eq!(
            preserve
                .read_exact(AddressRange::new(0x1000, 0x1006).unwrap())
                .unwrap(),
            &[1, 2, 9, 9, 5, 6]
        );

        let mut overwrite = original;
        overwrite
            .insert_segment(incoming, OverlapPolicy::Overwrite)
            .unwrap();
        assert_eq!(
            overwrite
                .read_exact(AddressRange::new(0x1000, 0x1006).unwrap())
                .unwrap(),
            &[1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn merge_clips_relocates_and_reports_overlap() {
        let mut target = image(vec![segment(0x2002, &[0xEE, 0xEE])]);
        let source = image(vec![segment(0x1000, &[0, 1, 2, 3, 4, 5])]);
        let report = target
            .merge_from(
                &source,
                MergeOptions {
                    source_range: Some(AddressRange::new(0x1001, 0x1005).unwrap()),
                    address_offset: 0x1000,
                    overlap: OverlapPolicy::Preserve,
                    initialized_only: true,
                },
            )
            .unwrap();
        assert_eq!(report.bytes_processed, 4);
        assert_eq!(report.overlapping_bytes, 2);
        assert_eq!(report.bytes_written, 2);
        assert_eq!(
            target
                .read_exact(AddressRange::new(0x2001, 0x2005).unwrap())
                .unwrap(),
            &[1, 0xEE, 0xEE, 4]
        );
    }

    #[test]
    fn erase_fill_align_split_and_remap() {
        let mut image = image(vec![segment(0x1003, &[1, 2, 3, 4, 5])]);
        assert_eq!(image.align_blocks(4, 0xFF).unwrap(), 3);
        assert_eq!(
            image
                .read_exact(AddressRange::new(0x1000, 0x1008).unwrap())
                .unwrap(),
            &[0xFF, 0xFF, 0xFF, 1, 2, 3, 4, 5]
        );
        assert_eq!(image.split_blocks(3).unwrap(), 3);
        assert_eq!(
            image
                .erase_range(AddressRange::new(0x1002, 0x1004).unwrap())
                .unwrap(),
            2
        );
        image
            .fill_range(
                AddressRange::new(0x1002, 0x1004).unwrap(),
                &[0xA5, 0x5A],
                OverlapPolicy::Reject,
            )
            .unwrap();
        image
            .remap_range(
                AddressRange::new(0x1000, 0x1004).unwrap(),
                0x1000,
                OverlapPolicy::Reject,
            )
            .unwrap();
        assert_eq!(
            image
                .read_exact(AddressRange::new(0x2000, 0x2004).unwrap())
                .unwrap(),
            &[0xFF, 0xFF, 0xA5, 0x5A]
        );
    }

    #[test]
    fn find_crosses_adjacent_segments_but_not_holes() {
        let image = image(vec![
            segment(0x1000, b"AB"),
            segment(0x1002, b"ABA"),
            segment(0x2000, b"ABA"),
        ]);
        assert_eq!(
            image.find_all(b"ABA").unwrap(),
            vec![0x1000, 0x1002, 0x2000]
        );
        assert!(image.find_all(b"AX").unwrap().is_empty());
    }

    #[test]
    fn compare_classifies_changed_and_missing_spans() {
        let left = image(vec![segment(0x1000, &[1, 2, 3, 4])]);
        let right = image(vec![segment(0x1001, &[2, 9, 4, 5])]);
        let differences = left.compare(&right).unwrap();
        assert_eq!(differences.len(), 3);
        assert_eq!(differences[0].kind(), DifferenceKind::LeftOnly);
        assert_eq!(differences[0].address, 0x1000);
        assert_eq!(differences[1].kind(), DifferenceKind::Changed);
        assert_eq!(differences[1].address, 0x1002);
        assert_eq!(differences[2].kind(), DifferenceKind::RightOnly);
        assert_eq!(differences[2].address, 0x1004);
    }

    #[test]
    fn summary_reports_holes_and_ignores_uninitialized_layouts() {
        let image = image(vec![
            segment(0x1000, &[1, 2]),
            MemorySegment::new(0x1002, 2, MemoryPrgType::DATA),
            segment(0x1004, &[5]),
        ]);
        let summary = image.image_summary().unwrap();
        assert_eq!(summary.segment_count, 2);
        assert_eq!(summary.byte_count, 3);
        assert_eq!(
            summary.holes,
            vec![AddressRange {
                start: 0x1002,
                end: 0x1004
            }]
        );
    }

    #[test]
    fn base_operations_update_dirty_state() {
        let mut base = DataFileBase::new(None, image(vec![segment(0x1000, &[1])]));
        base.write_at(0x1001, &[2]).unwrap();
        assert!(base.is_dirty);
        assert_eq!(base.segment_list.find_all(&[1, 2]).unwrap(), vec![0x1000]);
    }
}
