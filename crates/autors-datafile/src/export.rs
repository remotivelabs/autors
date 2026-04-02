//! Generic exports from the sparse in-memory representation.

use std::fmt::Write as _;

use crate::datafile::MemorySegmentList;
use crate::image::AddressRange;
use crate::{Error, Result};

/// Controls conversion to an address-less binary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BinaryExportOptions {
    /// Optional source address range.
    pub range: Option<AddressRange>,
    /// Materialize holes with this byte. If absent, initialized blocks are
    /// concatenated and address gaps are omitted.
    pub gap_fill: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryBlock {
    pub address: u64,
    pub data: Vec<u8>,
}

/// Exports initialized bytes in ascending address order.
pub fn export_binary(image: &MemorySegmentList, options: BinaryExportOptions) -> Result<Vec<u8>> {
    image.validate_image()?;
    if let Some(fill) = options.gap_fill {
        let range = match options.range {
            Some(range) => range,
            None => image.image_summary()?.address_range.ok_or_else(|| {
                Error::Value("cannot materialize an empty memory image".to_string())
            })?,
        };
        return image.read_filled(range, fill);
    }
    let blocks = export_binary_blocks(image, options.range)?;
    let length = blocks.iter().try_fold(0usize, |total, block| {
        total
            .checked_add(block.data.len())
            .ok_or_else(|| Error::Value("binary output size overflows usize".to_string()))
    })?;
    let mut output = Vec::with_capacity(length);
    for block in blocks {
        output.extend_from_slice(&block.data);
    }
    Ok(output)
}

/// Exports one binary block per initialized segment, optionally clipped.
pub fn export_binary_blocks(
    image: &MemorySegmentList,
    range: Option<AddressRange>,
) -> Result<Vec<BinaryBlock>> {
    image.validate_image()?;
    let mut segments: Vec<_> = image
        .segments
        .iter()
        .filter(|segment| segment.is_initialized())
        .collect();
    segments.sort_by_key(|segment| segment.address);
    let mut blocks = Vec::new();
    for segment in segments {
        let segment_end = segment
            .address
            .checked_add(segment.size() as u64)
            .ok_or_else(|| Error::Value("segment address overflows u64".to_string()))?;
        let start = range.map_or(segment.address, |range| segment.address.max(range.start));
        let end = range.map_or(segment_end, |range| segment_end.min(range.end));
        if start >= end {
            continue;
        }
        let offset = (start - segment.address) as usize;
        let length = (end - start) as usize;
        blocks.push(BinaryBlock {
            address: start,
            data: segment.data()[offset..offset + length].to_vec(),
        });
    }
    Ok(blocks)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CArrayOptions {
    /// Prefix used for generated array and descriptor identifiers.
    pub symbol_prefix: String,
    pub bytes_per_line: usize,
    pub include_segment_table: bool,
    pub range: Option<AddressRange>,
}

impl Default for CArrayOptions {
    fn default() -> Self {
        Self {
            symbol_prefix: "firmware".to_string(),
            bytes_per_line: 12,
            include_segment_table: true,
            range: None,
        }
    }
}

/// Exports each sparse block as a C `uint8_t` array. An optional descriptor
/// table preserves each block's absolute address.
pub fn export_c_arrays(image: &MemorySegmentList, options: &CArrayOptions) -> Result<String> {
    if options.bytes_per_line == 0 {
        return Err(Error::Value(
            "C array bytes per line must not be zero".to_string(),
        ));
    }
    if !is_c_identifier(&options.symbol_prefix) {
        return Err(Error::Value(format!(
            "'{}' is not a valid C identifier",
            options.symbol_prefix
        )));
    }
    let blocks = export_binary_blocks(image, options.range)?;
    let mut output = String::from("#include <stddef.h>\n#include <stdint.h>\n\n");
    for (index, block) in blocks.iter().enumerate() {
        let _ = writeln!(
            output,
            "static const uint8_t {}_block_{index}[] = {{",
            options.symbol_prefix
        );
        for (offset, byte) in block.data.iter().enumerate() {
            if offset.is_multiple_of(options.bytes_per_line) {
                output.push_str("    ");
            }
            let _ = write!(output, "0x{byte:02X},");
            if (offset + 1).is_multiple_of(options.bytes_per_line) || offset + 1 == block.data.len()
            {
                output.push('\n');
            } else {
                output.push(' ');
            }
        }
        output.push_str("};\n\n");
    }
    if options.include_segment_table {
        output.push_str(
            "typedef struct {\n    uint64_t address;\n    size_t size;\n    const uint8_t *data;\n} autors_data_segment;\n\n",
        );
        let _ = writeln!(
            output,
            "static const autors_data_segment {}_segments[] = {{",
            options.symbol_prefix
        );
        for (index, block) in blocks.iter().enumerate() {
            let _ = writeln!(
                output,
                "    {{ UINT64_C(0x{:X}), sizeof({}_block_{index}), {}_block_{index} }},",
                block.address, options.symbol_prefix, options.symbol_prefix
            );
        }
        output.push_str("};\n");
        let _ = writeln!(
            output,
            "static const size_t {}_segment_count = sizeof({}_segments) / sizeof({}_segments[0]);",
            options.symbol_prefix, options.symbol_prefix, options.symbol_prefix
        );
    }
    Ok(output)
}

fn is_c_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Exports a binary view using RFC 4648 Base64 with optional line wrapping.
/// A `line_width` of zero disables wrapping.
pub fn export_base64(
    image: &MemorySegmentList,
    binary_options: BinaryExportOptions,
    line_width: usize,
) -> Result<String> {
    let data = export_binary(image, binary_options)?;
    let encoded = encode_base64(&data);
    if line_width == 0 || encoded.is_empty() {
        return Ok(encoded);
    }
    let mut output = String::with_capacity(encoded.len() + encoded.len() / line_width + 1);
    for chunk in encoded.as_bytes().chunks(line_width) {
        output.push_str(std::str::from_utf8(chunk).map_err(|_| {
            Error::Value("internal Base64 encoder produced invalid ASCII".to_string())
        })?);
        output.push('\n');
    }
    Ok(output)
}

fn encode_base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0x0F) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use autors_a2l::model::enums::MemoryPrgType;

    use super::*;
    use crate::datafile::MemorySegment;

    fn sparse_image() -> MemorySegmentList {
        MemorySegmentList {
            segments: vec![
                MemorySegment::from_data(0x1000, vec![1, 2], MemoryPrgType::DATA, true),
                MemorySegment::from_data(0x1004, vec![5, 6], MemoryPrgType::DATA, true),
            ],
        }
    }

    #[test]
    fn binary_export_can_concatenate_or_materialize_holes() {
        let image = sparse_image();
        assert_eq!(
            export_binary(&image, BinaryExportOptions::default()).unwrap(),
            &[1, 2, 5, 6]
        );
        assert_eq!(
            export_binary(
                &image,
                BinaryExportOptions {
                    range: None,
                    gap_fill: Some(0xFF),
                }
            )
            .unwrap(),
            &[1, 2, 0xFF, 0xFF, 5, 6]
        );
    }

    #[test]
    fn binary_block_export_preserves_addresses_and_clips() {
        let blocks = export_binary_blocks(
            &sparse_image(),
            Some(AddressRange::new(0x1001, 0x1005).unwrap()),
        )
        .unwrap();
        assert_eq!(
            blocks,
            vec![
                BinaryBlock {
                    address: 0x1001,
                    data: vec![2]
                },
                BinaryBlock {
                    address: 0x1004,
                    data: vec![5]
                }
            ]
        );
    }

    #[test]
    fn c_array_export_includes_sparse_descriptor_table() {
        let output = export_c_arrays(
            &sparse_image(),
            &CArrayOptions {
                symbol_prefix: "ecu_image".to_string(),
                bytes_per_line: 2,
                include_segment_table: true,
                range: None,
            },
        )
        .unwrap();
        assert!(output.contains("ecu_image_block_0"));
        assert!(output.contains("UINT64_C(0x1004)"));
        assert!(output.contains("ecu_image_segment_count"));
        assert!(export_c_arrays(
            &sparse_image(),
            &CArrayOptions {
                symbol_prefix: "not-valid".to_string(),
                ..CArrayOptions::default()
            }
        )
        .is_err());
    }

    #[test]
    fn base64_export_matches_standard_vectors_and_wraps() {
        let image = MemorySegmentList {
            segments: vec![MemorySegment::from_data(
                0,
                b"foobar".to_vec(),
                MemoryPrgType::DATA,
                true,
            )],
        };
        assert_eq!(
            export_base64(&image, BinaryExportOptions::default(), 0).unwrap(),
            "Zm9vYmFy"
        );
        assert_eq!(
            export_base64(&image, BinaryExportOptions::default(), 4).unwrap(),
            "Zm9v\nYmFy\n"
        );
    }
}
