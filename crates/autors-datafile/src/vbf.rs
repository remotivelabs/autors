//! Ford/Volvo Versatile Binary Format (VBF) 2.x raw-block support.

use std::path::Path;

use autors_a2l::model::enums::MemoryPrgType;

use crate::checksum::{crc16_ccitt_false, crc32_iso_hdlc};
use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::image::OverlapPolicy;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VbfHeaderField {
    pub name: String,
    /// Header value in VBF C-like syntax, without the trailing semicolon.
    pub value: String,
}

/// Parsed VBF header. Unknown fields are preserved as raw values and emitted
/// again by the canonical writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VbfHeader {
    pub version: String,
    pub fields: Vec<VbfHeaderField>,
}

impl Default for VbfHeader {
    fn default() -> Self {
        Self::new("2.2")
    }
}

impl VbfHeader {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            fields: Vec::new(),
        }
    }

    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .map(|field| field.value.as_str())
    }

    pub fn set_field(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if let Some(field) = self
            .fields
            .iter_mut()
            .find(|field| field.name.eq_ignore_ascii_case(&name))
        {
            field.value = value;
        } else {
            self.fields.push(VbfHeaderField { name, value });
        }
    }

    pub fn file_checksum(&self) -> Option<u32> {
        self.field("file_checksum").and_then(parse_c_u32)
    }

    pub fn data_format_identifier(&self) -> Option<u32> {
        self.field("data_format_identifier").and_then(parse_c_u32)
    }

    fn render(&self) -> String {
        let mut output = format!("vbf_version = {};\nheader {{\n", self.version);
        for field in &self.fields {
            output.push_str("    ");
            output.push_str(&field.name);
            output.push_str(" = ");
            output.push_str(&field.value);
            output.push_str(";\n");
        }
        output.push('}');
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbfBlockInfo {
    pub address: u32,
    pub length: u32,
    pub checksum: u16,
    pub checksum_valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbfParseOptions {
    pub verify_block_checksums: bool,
    pub verify_file_checksum: bool,
}

impl Default for VbfParseOptions {
    fn default() -> Self {
        Self {
            verify_block_checksums: true,
            verify_file_checksum: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VbfWriteOptions {
    /// Zero keeps each initialized memory segment as one VBF block.
    pub max_block_size: usize,
}

#[derive(Debug, Clone)]
pub struct DataFileVbf {
    pub base: DataFileBase,
    pub header: VbfHeader,
    pub blocks: Vec<VbfBlockInfo>,
    pub file_checksum_valid: Option<bool>,
}

impl DataFileVbf {
    pub fn new(
        source_filename: Option<String>,
        segments: MemorySegmentList,
        header: VbfHeader,
    ) -> Self {
        Self {
            base: DataFileBase::new(source_filename, segments),
            header,
            blocks: Vec::new(),
            file_checksum_valid: None,
        }
    }

    pub fn parse(content: &[u8], segments: MemorySegmentList) -> Result<Self> {
        Self::parse_with_options(content, segments, VbfParseOptions::default())
    }

    pub fn parse_with_options(
        content: &[u8],
        segments: MemorySegmentList,
        options: VbfParseOptions,
    ) -> Result<Self> {
        let (header, body_start) = parse_header(content)?;
        if let Some(identifier) = header.data_format_identifier() {
            if identifier != 0 {
                return Err(Error::UnsupportedFormat(format!(
                    "VBF data format identifier 0x{identifier:X} (compressed or encrypted blocks)"
                )));
            }
        }

        let candidates = body_start_candidates(content, body_start);
        let mut last_error = None;
        let mut parsed = None;
        for candidate in candidates {
            match parse_blocks(&content[candidate..], options.verify_block_checksums) {
                Ok(value) => {
                    parsed = Some((candidate, value));
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let (body_start, (parsed_segments, blocks)) = parsed.ok_or_else(|| {
            last_error.unwrap_or_else(|| Error::DataFile {
                line: 0,
                message: "unable to locate VBF binary blocks".to_string(),
            })
        })?;

        let actual_file_checksum = if content.len() == body_start {
            0xFFFF_FFFF
        } else {
            crc32_iso_hdlc(&content[body_start..])
        };
        let file_checksum_valid = header
            .file_checksum()
            .map(|expected| expected == actual_file_checksum);
        if options.verify_file_checksum && file_checksum_valid == Some(false) {
            return Err(Error::DataFile {
                line: 0,
                message: format!(
                    "VBF file checksum mismatch: expected 0x{:08X}, calculated 0x{actual_file_checksum:08X}",
                    header.file_checksum().unwrap_or_default()
                ),
            });
        }

        let mut work = segments;
        for segment in parsed_segments {
            work.insert_segment(segment, OverlapPolicy::Reject)?;
        }
        Ok(Self {
            base: DataFileBase::new(None, work),
            header,
            blocks,
            file_checksum_valid,
        })
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let content = std::fs::read(path)?;
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
        let content = std::fs::read(path)?;
        let mut reloaded = Self::parse(&content, MemorySegmentList::new())?;
        reloaded.base.source_filename = self.base.source_filename.clone();
        *self = reloaded;
        Ok(())
    }

    pub fn write_vbf(&mut self, options: VbfWriteOptions) -> Result<Vec<u8>> {
        if self.header.data_format_identifier().unwrap_or(0) != 0 {
            return Err(Error::UnsupportedFormat(
                "writing compressed or encrypted VBF blocks".to_string(),
            ));
        }
        self.base.segment_list.validate_image()?;
        let mut segments: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|segment| segment.is_initialized())
            .collect();
        segments.sort_by_key(|segment| segment.address);

        let mut body = Vec::new();
        let mut blocks = Vec::new();
        for segment in segments {
            let max_block_size = if options.max_block_size == 0 {
                segment.size().max(1)
            } else {
                options.max_block_size
            };
            let mut offset = 0usize;
            while offset < segment.size() {
                let length = (segment.size() - offset).min(max_block_size);
                let address = segment
                    .address
                    .checked_add(offset as u64)
                    .ok_or_else(|| Error::Value("VBF block address overflows u64".to_string()))?;
                let address = u32::try_from(address).map_err(|_| {
                    Error::Value("VBF 2.x block address exceeds 32 bits".to_string())
                })?;
                let length_u32 = u32::try_from(length)
                    .map_err(|_| Error::Value("VBF block exceeds 32-bit length".to_string()))?;
                let data = &segment.data()[offset..offset + length];
                let checksum = crc16_ccitt_false(data);
                body.extend_from_slice(&address.to_be_bytes());
                body.extend_from_slice(&length_u32.to_be_bytes());
                body.extend_from_slice(data);
                body.extend_from_slice(&checksum.to_be_bytes());
                blocks.push(VbfBlockInfo {
                    address,
                    length: length_u32,
                    checksum,
                    checksum_valid: true,
                });
                offset += length;
            }
        }

        let file_checksum = if body.is_empty() {
            0xFFFF_FFFF
        } else {
            crc32_iso_hdlc(&body)
        };
        self.header
            .set_field("file_checksum", format!("0x{file_checksum:08X}"));
        let header = self.header.render();
        let mut output = Vec::with_capacity(header.len() + body.len());
        output.extend_from_slice(header.as_bytes());
        output.extend_from_slice(&body);
        self.blocks = blocks;
        self.file_checksum_valid = Some(true);
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        Ok(output)
    }

    pub fn save_file(&mut self, path: &Path, options: VbfWriteOptions) -> Result<()> {
        let content = self.write_vbf(options)?;
        std::fs::write(path, content)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

fn parse_c_u32(value: &str) -> Option<u32> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '_')
        .collect();
    if let Some(value) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
    {
        u32::from_str_radix(value, 16).ok()
    } else {
        compact.parse().ok()
    }
}

fn parse_header(content: &[u8]) -> Result<(VbfHeader, usize)> {
    let header_end = find_header_end(content)?;
    let text = std::str::from_utf8(&content[..header_end]).map_err(|_| Error::DataFile {
        line: 0,
        message: "VBF header is not valid UTF-8/ASCII".to_string(),
    })?;
    let version = assignment_value(text, "vbf_version").ok_or_else(|| Error::DataFile {
        line: 0,
        message: "VBF header has no vbf_version declaration".to_string(),
    })?;
    let open = text.find('{').ok_or_else(|| Error::DataFile {
        line: 0,
        message: "VBF header has no opening brace".to_string(),
    })?;
    let inner = remove_comments(&text[open + 1..]);
    let fields = split_header_fields(&inner);
    Ok((VbfHeader { version, fields }, header_end))
}

fn assignment_value(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find(&name.to_ascii_lowercase())? + name.len();
    let rest = text.get(start..)?;
    let equal = rest.find('=')?;
    let value = &rest[equal + 1..];
    let end = value.find(';')?;
    Some(value[..end].trim().to_string())
}

fn find_header_end(content: &[u8]) -> Result<usize> {
    let prefix_len = content.len().min(1024 * 1024);
    let text = String::from_utf8_lossy(&content[..prefix_len]);
    let lower = text.to_ascii_lowercase();
    let header = lower.find("header").ok_or_else(|| Error::DataFile {
        line: 0,
        message: "VBF header keyword not found".to_string(),
    })?;
    let open = content[header..]
        .iter()
        .position(|byte| *byte == b'{')
        .map(|offset| header + offset)
        .ok_or_else(|| Error::DataFile {
            line: 0,
            message: "VBF header has no opening brace".to_string(),
        })?;

    let mut depth = 0usize;
    let mut index = open;
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < content.len() {
        let byte = content[index];
        let next = content.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
        } else if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                index += 1;
            }
        } else if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
            index += 1;
        } else if byte == b'/' && next == Some(b'*') {
            block_comment = true;
            index += 1;
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Ok(index + 1);
            }
        }
        index += 1;
    }
    Err(Error::DataFile {
        line: 0,
        message: "unterminated VBF header".to_string(),
    })
}

fn remove_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0usize;
    let mut in_string = false;
    while index < bytes.len() {
        if !in_string && bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if !in_string && bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            if bytes[index] == b'"' && (index == 0 || bytes[index - 1] != b'\\') {
                in_string = !in_string;
            }
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    output
}

fn split_header_fields(text: &str) -> Vec<VbfHeaderField> {
    let mut fields = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'"' && (index == 0 || bytes[index - 1] != b'\\') {
            in_string = !in_string;
        } else if !in_string {
            match byte {
                b'{' => depth += 1,
                b'}' => depth = depth.saturating_sub(1),
                b';' if depth == 0 => {
                    if let Some(field) = parse_header_field(&text[start..index]) {
                        fields.push(field);
                    }
                    start = index + 1;
                }
                _ => {}
            }
        }
    }
    fields
}

fn parse_header_field(statement: &str) -> Option<VbfHeaderField> {
    let (name, value) = statement.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    (!name.is_empty() && !value.is_empty()).then(|| VbfHeaderField {
        name: name.to_string(),
        value: value.to_string(),
    })
}

fn body_start_candidates(content: &[u8], body_start: usize) -> Vec<usize> {
    let mut candidates = vec![body_start];
    let mut index = body_start;
    if content.get(index) == Some(&b'.') {
        index += 1;
        candidates.push(index);
    }
    while index < content.len()
        && content[index].is_ascii_whitespace()
        && index.saturating_sub(body_start) < 4
    {
        index += 1;
        candidates.push(index);
    }
    candidates
}

fn parse_blocks(
    body: &[u8],
    verify_checksums: bool,
) -> Result<(Vec<MemorySegment>, Vec<VbfBlockInfo>)> {
    let mut segments = Vec::new();
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    while offset < body.len() {
        if body.len() - offset < 10 {
            return Err(Error::DataFile {
                line: 0,
                message: format!("truncated VBF block header at file-body offset 0x{offset:X}"),
            });
        }
        let address = u32::from_be_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]);
        let length = u32::from_be_bytes([
            body[offset + 4],
            body[offset + 5],
            body[offset + 6],
            body[offset + 7],
        ]);
        let length_usize = usize::try_from(length)
            .map_err(|_| Error::Value("VBF block length does not fit usize".to_string()))?;
        let data_start = offset + 8;
        let checksum_start = data_start
            .checked_add(length_usize)
            .ok_or_else(|| Error::Value("VBF block length overflows usize".to_string()))?;
        if checksum_start + 2 > body.len() {
            return Err(Error::DataFile {
                line: 0,
                message: format!(
                    "VBF block at 0x{address:08X} declares {length} byte(s), exceeding the file"
                ),
            });
        }
        let data = &body[data_start..checksum_start];
        let checksum = u16::from_be_bytes([body[checksum_start], body[checksum_start + 1]]);
        let calculated = crc16_ccitt_false(data);
        let checksum_valid = checksum == calculated;
        if verify_checksums && !checksum_valid {
            return Err(Error::DataFile {
                line: 0,
                message: format!(
                    "VBF block checksum mismatch at 0x{address:08X}: expected 0x{checksum:04X}, calculated 0x{calculated:04X}"
                ),
            });
        }
        segments.push(MemorySegment::from_data(
            address as u64,
            data.to_vec(),
            MemoryPrgType::DATA,
            true,
        ));
        blocks.push(VbfBlockInfo {
            address,
            length,
            checksum,
            checksum_valid,
        });
        offset = checksum_start + 2;
    }
    Ok((segments, blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    fn segment(address: u64, data: &[u8]) -> MemorySegment {
        MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::DATA, true)
    }

    #[test]
    fn header_parser_preserves_arrays_strings_and_unknown_fields() {
        let source = b"vbf_version = 2.3;\nheader {\n// comment\ndescription = {\"a; b\", \"c}\"};\nerase = {{0x1000, 0x20}};\ncustom = TOKEN;\nfile_checksum = 0xFFFFFFFF;\n}";
        let (header, end) = parse_header(source).unwrap();
        assert_eq!(end, source.len());
        assert_eq!(header.version, "2.3");
        assert_eq!(header.field("custom"), Some("TOKEN"));
        assert_eq!(header.field("erase"), Some("{{0x1000, 0x20}}"));
        assert_eq!(header.file_checksum(), Some(0xFFFF_FFFF));
    }

    #[test]
    fn vbf_raw_blocks_round_trip_with_both_crc_levels() {
        let mut header = VbfHeader::new("2.2");
        header.set_field("sw_part_number", "\"TEST-123\"");
        header.set_field("sw_part_type", "EXE");
        let segments = MemorySegmentList {
            segments: vec![segment(0x1000, b"123456789"), segment(0x2000, &[1, 2, 3])],
        };
        let mut file = DataFileVbf::new(None, segments, header);
        let output = file
            .write_vbf(VbfWriteOptions { max_block_size: 5 })
            .unwrap();
        assert_eq!(file.blocks.len(), 3);
        assert_eq!(file.blocks[0].checksum, crc16_ccitt_false(b"12345"));

        let parsed = DataFileVbf::parse(&output, MemorySegmentList::new()).unwrap();
        assert_eq!(parsed.file_checksum_valid, Some(true));
        assert!(parsed.blocks.iter().all(|block| block.checksum_valid));
        assert_eq!(
            parsed
                .base
                .segment_list
                .read_exact(AddressRange::new(0x1000, 0x1009).unwrap())
                .unwrap(),
            b"123456789"
        );
        assert_eq!(parsed.header.field("sw_part_number"), Some("\"TEST-123\""));
    }

    #[test]
    fn block_checksum_corruption_is_rejected() {
        let mut file = DataFileVbf::new(
            None,
            MemorySegmentList {
                segments: vec![segment(0x1000, b"123456789")],
            },
            VbfHeader::default(),
        );
        let mut output = file.write_vbf(VbfWriteOptions::default()).unwrap();
        let (_, body) = parse_header(&output).unwrap();
        output[body + 8] ^= 1;
        assert!(DataFileVbf::parse(&output, MemorySegmentList::new()).is_err());
    }

    #[test]
    fn nonzero_data_format_identifier_is_explicitly_unsupported() {
        let source =
            b"vbf_version=2.4;header{data_format_identifier=0x10;file_checksum=0xFFFFFFFF;}";
        assert!(matches!(
            DataFileVbf::parse(source, MemorySegmentList::new()),
            Err(Error::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn empty_vbf_uses_specified_checksum_value() {
        let mut file = DataFileVbf::new(None, MemorySegmentList::new(), VbfHeader::default());
        let output = file.write_vbf(VbfWriteOptions::default()).unwrap();
        assert_eq!(file.header.file_checksum(), Some(0xFFFF_FFFF));
        let parsed = DataFileVbf::parse(&output, MemorySegmentList::new()).unwrap();
        assert!(parsed.blocks.is_empty());
        assert_eq!(parsed.file_checksum_valid, Some(true));
    }
}
