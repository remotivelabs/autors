//! Built-in checksum algorithms for sparse memory images.

use crate::datafile::{DataFileBase, MemorySegmentList};
use crate::image::{AddressRange, OverlapPolicy};
use crate::{Error, Result};

/// Checksum algorithms available without a vendor plug-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    Xor8,
    ByteSum8,
    ByteSum16,
    ByteSum32,
    /// Sum of little-endian 16-bit input words, truncated to 16 bits. An odd
    /// final byte is the low byte of a zero-extended word.
    WordSum16Le,
    /// Sum of big-endian 16-bit input words, truncated to 16 bits. An odd
    /// final byte is the high byte of a zero-extended word.
    WordSum16Be,
    /// CRC-16/CCITT-FALSE: polynomial 0x1021, init 0xFFFF, no reflection,
    /// xor-out 0.
    Crc16CcittFalse,
    /// CRC-32/ISO-HDLC: reflected polynomial 0xEDB88320, init and xor-out
    /// 0xFFFFFFFF.
    Crc32IsoHdlc,
}

/// Selects bytes for checksum calculation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChecksumOptions {
    /// Restrict calculation to this address range. Without a range, all
    /// initialized segments are concatenated in ascending address order.
    pub range: Option<AddressRange>,
    /// Address ranges omitted from the calculation.
    pub excluded_ranges: Vec<AddressRange>,
    /// When a range is present, include address holes using this fill byte.
    /// If absent, holes do not contribute to the checksum.
    pub gap_fill: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    BigEndian,
    LittleEndian,
}

/// A calculated integer value together with its natural output width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksumValue {
    pub value: u32,
    pub width: u8,
}

impl ChecksumValue {
    pub fn to_bytes(self, byte_order: ByteOrder) -> Vec<u8> {
        let width = self.width as usize;
        let bytes = match byte_order {
            ByteOrder::BigEndian => self.value.to_be_bytes(),
            ByteOrder::LittleEndian => self.value.to_le_bytes(),
        };
        match byte_order {
            ByteOrder::BigEndian => bytes[bytes.len() - width..].to_vec(),
            ByteOrder::LittleEndian => bytes[..width].to_vec(),
        }
    }
}

fn is_excluded(address: u64, excluded_ranges: &[AddressRange]) -> bool {
    excluded_ranges.iter().any(|range| range.contains(address))
}

fn selected_bytes(image: &MemorySegmentList, options: &ChecksumOptions) -> Result<Vec<u8>> {
    image.validate_image()?;
    if let (Some(range), Some(fill)) = (options.range, options.gap_fill) {
        let materialized = image.read_filled(range, fill)?;
        return Ok(materialized
            .into_iter()
            .enumerate()
            .filter_map(|(offset, byte)| {
                let address = range.start + offset as u64;
                (!is_excluded(address, &options.excluded_ranges)).then_some(byte)
            })
            .collect());
    }

    let mut segments: Vec<_> = image
        .segments
        .iter()
        .filter(|segment| segment.is_initialized())
        .collect();
    segments.sort_by_key(|segment| segment.address);
    let mut output = Vec::new();
    for segment in segments {
        let segment_end = segment
            .address
            .checked_add(segment.size() as u64)
            .ok_or_else(|| Error::Value("segment address overflows u64".to_string()))?;
        let start = options
            .range
            .map_or(segment.address, |range| segment.address.max(range.start));
        let end = options
            .range
            .map_or(segment_end, |range| segment_end.min(range.end));
        if start >= end {
            continue;
        }
        for address in start..end {
            if !is_excluded(address, &options.excluded_ranges) {
                output.push(segment.data()[(address - segment.address) as usize]);
            }
        }
    }
    Ok(output)
}

/// Calculates a checksum over selected initialized image bytes.
pub fn calculate_checksum(
    image: &MemorySegmentList,
    algorithm: ChecksumAlgorithm,
    options: &ChecksumOptions,
) -> Result<ChecksumValue> {
    let data = selected_bytes(image, options)?;
    let result = match algorithm {
        ChecksumAlgorithm::Xor8 => ChecksumValue {
            value: data.iter().fold(0u8, |acc, byte| acc ^ byte) as u32,
            width: 1,
        },
        ChecksumAlgorithm::ByteSum8 => ChecksumValue {
            value: data.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte)) as u32,
            width: 1,
        },
        ChecksumAlgorithm::ByteSum16 => ChecksumValue {
            value: data
                .iter()
                .fold(0u16, |acc, byte| acc.wrapping_add(*byte as u16)) as u32,
            width: 2,
        },
        ChecksumAlgorithm::ByteSum32 => ChecksumValue {
            value: data
                .iter()
                .fold(0u32, |acc, byte| acc.wrapping_add(*byte as u32)),
            width: 4,
        },
        ChecksumAlgorithm::WordSum16Le => ChecksumValue {
            value: word_sum(&data, ByteOrder::LittleEndian) as u32,
            width: 2,
        },
        ChecksumAlgorithm::WordSum16Be => ChecksumValue {
            value: word_sum(&data, ByteOrder::BigEndian) as u32,
            width: 2,
        },
        ChecksumAlgorithm::Crc16CcittFalse => ChecksumValue {
            value: crc16_ccitt_false(&data) as u32,
            width: 2,
        },
        ChecksumAlgorithm::Crc32IsoHdlc => ChecksumValue {
            value: crc32_iso_hdlc(&data),
            width: 4,
        },
    };
    Ok(result)
}

fn word_sum(data: &[u8], byte_order: ByteOrder) -> u16 {
    data.chunks(2).fold(0u16, |sum, chunk| {
        let word = match (byte_order, chunk) {
            (ByteOrder::LittleEndian, [low, high]) => u16::from_le_bytes([*low, *high]),
            (ByteOrder::LittleEndian, [low]) => *low as u16,
            (ByteOrder::BigEndian, [high, low]) => u16::from_be_bytes([*high, *low]),
            (ByteOrder::BigEndian, [high]) => (*high as u16) << 8,
            (_, _) => 0,
        };
        sum.wrapping_add(word)
    })
}

pub(crate) fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub(crate) fn crc32_iso_hdlc(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

impl DataFileBase {
    pub fn checksum(
        &self,
        algorithm: ChecksumAlgorithm,
        options: &ChecksumOptions,
    ) -> Result<ChecksumValue> {
        calculate_checksum(&self.segment_list, algorithm, options)
    }

    /// Calculates and inserts the result atomically at an absolute address.
    pub fn calculate_and_insert_checksum(
        &mut self,
        algorithm: ChecksumAlgorithm,
        options: &ChecksumOptions,
        address: u64,
        byte_order: ByteOrder,
        overlap: OverlapPolicy,
    ) -> Result<ChecksumValue> {
        let value = self.checksum(algorithm, options)?;
        let bytes = value.to_bytes(byte_order);
        let mut work = self.segment_list.clone();
        let written = work.insert_segment(
            crate::datafile::MemorySegment::from_data(
                address,
                bytes,
                autors_a2l::model::enums::MemoryPrgType::DATA,
                true,
            ),
            overlap,
        )?;
        self.segment_list = work;
        self.is_dirty |= written != 0;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use autors_a2l::model::enums::MemoryPrgType;

    use super::*;
    use crate::datafile::MemorySegment;

    fn image(address: u64, data: &[u8]) -> MemorySegmentList {
        MemorySegmentList {
            segments: vec![MemorySegment::from_data(
                address,
                data.to_vec(),
                MemoryPrgType::DATA,
                true,
            )],
        }
    }

    #[test]
    fn standard_crc_check_values() {
        let image = image(0, b"123456789");
        assert_eq!(
            calculate_checksum(
                &image,
                ChecksumAlgorithm::Crc16CcittFalse,
                &ChecksumOptions::default()
            )
            .unwrap()
            .value,
            0x29B1
        );
        assert_eq!(
            calculate_checksum(
                &image,
                ChecksumAlgorithm::Crc32IsoHdlc,
                &ChecksumOptions::default()
            )
            .unwrap()
            .value,
            0xCBF4_3926
        );
    }

    #[test]
    fn additive_algorithms_wrap_and_encode() {
        let image = image(0, &[0xFF, 2, 3]);
        assert_eq!(
            calculate_checksum(
                &image,
                ChecksumAlgorithm::ByteSum8,
                &ChecksumOptions::default()
            )
            .unwrap()
            .value,
            4
        );
        let value = calculate_checksum(
            &image,
            ChecksumAlgorithm::WordSum16Le,
            &ChecksumOptions::default(),
        )
        .unwrap();
        assert_eq!(value.value, 0x02FF + 3);
        assert_eq!(value.to_bytes(ByteOrder::BigEndian), [0x03, 0x02]);
        assert_eq!(value.to_bytes(ByteOrder::LittleEndian), [0x02, 0x03]);
    }

    #[test]
    fn sparse_range_can_fill_gaps_and_exclude_addresses() {
        let image = MemorySegmentList {
            segments: vec![
                MemorySegment::from_data(0x1000, vec![1, 2], MemoryPrgType::DATA, true),
                MemorySegment::from_data(0x1004, vec![5], MemoryPrgType::DATA, true),
            ],
        };
        let options = ChecksumOptions {
            range: Some(AddressRange::new(0x1000, 0x1005).unwrap()),
            excluded_ranges: vec![AddressRange::new(0x1001, 0x1002).unwrap()],
            gap_fill: Some(0x10),
        };
        let value = calculate_checksum(&image, ChecksumAlgorithm::ByteSum16, &options).unwrap();
        assert_eq!(value.value, 1 + 0x10 + 0x10 + 5);
    }

    #[test]
    fn calculate_and_insert_is_transactional() {
        let mut base = DataFileBase::new(None, image(0x1000, &[1, 2, 3]));
        let value = base
            .calculate_and_insert_checksum(
                ChecksumAlgorithm::ByteSum16,
                &ChecksumOptions::default(),
                0x1003,
                ByteOrder::BigEndian,
                OverlapPolicy::Reject,
            )
            .unwrap();
        assert_eq!(value.value, 6);
        assert_eq!(
            base.segment_list
                .read_exact(AddressRange::new(0x1000, 0x1005).unwrap())
                .unwrap(),
            &[1, 2, 3, 0, 6]
        );
        assert!(base.is_dirty);

        let before = base.segment_list.clone();
        assert!(base
            .calculate_and_insert_checksum(
                ChecksumAlgorithm::ByteSum16,
                &ChecksumOptions::default(),
                0x1002,
                ByteOrder::BigEndian,
                OverlapPolicy::Reject,
            )
            .is_err());
        assert_eq!(base.segment_list, before);
    }
}
