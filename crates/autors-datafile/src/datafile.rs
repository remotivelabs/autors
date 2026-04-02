//! Core data-file types and dispatch for supported sparse-image formats.
//! Core types: [`MemorySegment`] / [`MemorySegmentList`] (runtime data
//! segments; not to be confused with the A2L `MEMORY_SEGMENT` node type in
//! autors-a2l), the [`DataFileBase`] field group shared by all formats, the
//! format-specific file types [`DataFileBin`] / [`DataFileHex`] /
//! [`DataFileS19`], and the enums [`DataFileType`] and [`MotorolaFileType`].
//! Design: the format hierarchy is flattened into the [`DataFileBase`] field
//! group; the file-format polymorphism is provided by the [`DataFile`] enum.
//! IO and parsing are separated: parse entry points take `&str` / `&[u8]`,
//! and write-out produces `String` / `Vec<u8>`; `from_file` / `save_file` /
//! `reload` / [`DataFile::open`] are thin IO wrappers.

use std::fmt;
use std::path::{Path, PathBuf};

use autors_a2l::model::base::RecordLayoutRefFields;
use autors_a2l::model::enums::MemoryPrgType;

use crate::ascii_hex::DataFileHexAscii;
use crate::error::{Error, Result};
use crate::ford_ihex::DataFileFordIHex;
use crate::mixed::DataFileMixed;
use crate::ti_txt::DataFileTiTxt;
use crate::uf2::DataFileUf2;
use crate::vbf::DataFileVbf;

/// Default number of data bytes per line when none is specified.
pub const DEFAULT_DATA_BYTES_PER_LINE: usize = 32;

/// Recognized extension wildcards used when locating a nearby data file.
const DATA_FILE_WILDCARDS: &[&str] = &[
    "*.hex",
    "*.h86",
    "*.ihex",
    "*.ihx",
    "*.s19",
    "*.s28",
    "*.s37",
    "*.s",
    "*.s1",
    "*.s2",
    "*.s3",
    "*.sx",
    "*.srec",
    "*.hascii",
    "*.hexascii",
    "*.vbf",
    "*.titxt",
    "*.ti-txt",
    "*.ti_txt",
    "*.uf2",
];

/// Wildcard for raw binary files.
const BIN_WILDCARD: &str = "*.bin";

/// Default S0 header line of a newly created `DataFileS19`.
const DEFAULT_S0_HEADER: &str = "S0030000FC";

// ---------------------------------------------------------------------------
// MemorySegment / MemorySegmentList
// ---------------------------------------------------------------------------

/// A runtime memory data segment.
/// Design notes:
/// - A segment holds no back-reference to its owning file, so writing segment
///   data directly does not bubble up to the file-level dirty flag; that flag
///   is maintained by [`DataFileBase`] methods (`set_epk` /
///   `set_segments_data` etc.).
/// - The segment size is not stored separately; it is always equal to
///   `data.len()`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySegment {
    /// Segment start address.
    pub address: u64,
    /// Segment usage type (reuses the `MEMORYPRG_TYPE` enum of autors-a2l).
    pub prg_type: MemoryPrgType,
    /// Offset base inside a BIN file: `file offset = address - bin_file_offset`
    /// (written only by the BIN load path; may be negative).
    pub bin_file_offset: i64,
    data: Vec<u8>,
    is_initialized: bool,
}

impl MemorySegment {
    /// Allocates `size` bytes filled with 0xFF; `is_initialized = false`.
    pub fn new(address: u64, size: usize, prg_type: MemoryPrgType) -> Self {
        let mut seg = MemorySegment {
            address,
            prg_type,
            bin_file_offset: 0,
            data: vec![0; size],
            is_initialized: false,
        };
        seg.fill(0xFF);
        seg.is_initialized = false; // fill() does not set the initialized flag
        seg
    }

    /// Constructs a segment from existing data. The caller must pass the
    /// segment type explicitly (it is not silently defaulted to `UNKNOWN`).
    pub fn from_data(
        address: u64,
        data: Vec<u8>,
        prg_type: MemoryPrgType,
        is_initialized: bool,
    ) -> Self {
        MemorySegment {
            address,
            prg_type,
            bin_file_offset: 0,
            data,
            is_initialized,
        }
    }

    /// Segment created when HEX/S19 parse results are merged
    /// (`prg_type = DATA`, already initialized).
    fn file_data_segment(address: u64, data: Vec<u8>) -> Self {
        MemorySegment::from_data(address, data, MemoryPrgType::DATA, true)
    }

    /// Segment length in bytes.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// End address of the segment (exclusive).
    fn end(&self) -> u64 {
        self.address + self.data.len() as u64
    }

    /// Segment data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Whether the segment has been initialized (overwritten by data-file
    /// content).
    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    /// Internal setter for the initialized flag.
    pub(crate) fn set_initialized(&mut self, initialized: bool) {
        self.is_initialized = initialized;
    }

    /// Whether this segment counts as a data segment: UNKNOWN / DATA /
    /// OFFLINE_DATA / VARIABLES / SERAM / CALIBRATION_VARIABLES.
    pub fn is_data_segment(&self) -> bool {
        matches!(
            self.prg_type,
            MemoryPrgType::UNKNOWN
                | MemoryPrgType::DATA
                | MemoryPrgType::OFFLINE_DATA
                | MemoryPrgType::VARIABLES
                | MemoryPrgType::SERAM
                | MemoryPrgType::CALIBRATION_VARIABLES
        )
    }

    pub fn get_data_bytes(&self, address: u64, len: usize) -> Result<&[u8]> {
        if len == 0 {
            return Ok(&self.data[0..0]);
        }
        let last = address + len as u64 - 1;
        if address < self.address || last >= self.end() {
            return Err(Error::Value(format!(
                "address 0x{address:X}..0x{:X} out of segment 0x{:X}..0x{:X}",
                last + 1,
                self.address,
                self.end()
            )));
        }
        let off = (address - self.address) as usize;
        Ok(&self.data[off..off + len])
    }

    pub fn fill(&mut self, value: u8) {
        self.data.fill(value);
    }

    pub fn set_data_bytes(&mut self, offset: usize, data: &[u8]) {
        self.is_initialized = true;
        let count = data.len().min(self.data.len().saturating_sub(offset));
        self.data[offset..offset + count].copy_from_slice(&data[..count]);
    }

    pub fn set_data_bytes_at(&mut self, data: &[u8], address: u64) {
        let offset = if address != 0 {
            address.saturating_sub(self.address)
        } else {
            0
        };
        self.set_data_bytes(offset as usize, data);
    }

    fn matches_ascii_at(&self, address: u64, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        if !self.is_initialized || address + text.len() as u64 >= self.end() {
            return false;
        }
        match self.get_data_bytes(address, text.len()) {
            Ok(bytes) => bytes == text.as_bytes(),
            Err(_) => false,
        }
    }

    pub fn is_memory_in(&self, address: u64, len: usize) -> bool {
        let end = self.end();
        if address >= self.address && address < end {
            return true;
        }
        let next = (address + len as u64).min(u32::MAX as u64);
        next >= self.address && next <= end
    }
}

impl fmt::Display for MemorySegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} @0x{:X}, {} byte(s), {}",
            self.prg_type,
            self.address,
            self.size(),
            if self.is_initialized {
                "initialized"
            } else {
                "not initialized"
            }
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySegmentList {
    pub segments: Vec<MemorySegment>,
}

impl MemorySegmentList {
    pub fn new() -> Self {
        MemorySegmentList::default()
    }

    pub fn from_vec(segments: Vec<MemorySegment>) -> Self {
        let mut list = MemorySegmentList { segments };
        list.segments.sort_by(sort_data_segs_to_front);
        list
    }

    pub fn find_mem_seg(&self, address: u64, len: usize) -> Option<&MemorySegment> {
        self.segments.iter().find(|s| s.is_memory_in(address, len))
    }

    pub fn find_mem_seg_mut(&mut self, address: u64, len: usize) -> Option<&mut MemorySegment> {
        self.segments
            .iter_mut()
            .find(|s| s.is_memory_in(address, len))
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, MemorySegment> {
        self.segments.iter()
    }
}

pub fn sort_data_segs_to_front(a: &MemorySegment, b: &MemorySegment) -> std::cmp::Ordering {
    if a.prg_type != b.prg_type {
        if a.prg_type == MemoryPrgType::DATA {
            return std::cmp::Ordering::Less;
        }
        if b.prg_type == MemoryPrgType::DATA {
            return std::cmp::Ordering::Greater;
        }
    }
    a.address.cmp(&b.address)
}

impl fmt::Display for MemorySegmentList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} segment(s): [", self.segments.len())?;
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{seg}")?;
        }
        write!(f, "]")
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DataPart {
    address: u32,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct DataFileBase {
    pub data_bytes_per_line: usize,
    pub source_filename: Option<String>,
    pub is_dirty: bool,
    pub changed_values: Vec<RecordLayoutRefFields>,
    pub segment_list: MemorySegmentList,
}

impl DataFileBase {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        DataFileBase {
            data_bytes_per_line: 0,
            source_filename,
            is_dirty: false,
            changed_values: Vec::new(),
            segment_list: segments,
        }
    }

    pub fn mark_changed(&mut self, layout_ref: RecordLayoutRefFields) {
        self.is_dirty = true;
        if !self.changed_values.contains(&layout_ref) {
            self.changed_values.push(layout_ref);
        }
    }

    pub fn find_mem_seg(&self, address: u64, len: usize) -> Option<&MemorySegment> {
        self.segment_list.find_mem_seg(address, len)
    }

    pub fn epk_check(&self, epk_address: u32, expected_epk: &str) -> bool {
        if expected_epk.is_empty() {
            return true;
        }
        self.find_mem_seg(epk_address as u64, expected_epk.len())
            .is_some_and(|seg| seg.matches_ascii_at(epk_address as u64, expected_epk))
    }

    pub fn set_epk(&mut self, epk_address: u32, epk_to_set: &str) -> bool {
        if !epk_to_set.is_empty() {
            if let Some(seg) = self
                .segment_list
                .find_mem_seg_mut(epk_address as u64, epk_to_set.len())
            {
                seg.set_data_bytes_at(epk_to_set.as_bytes(), epk_address as u64);
                self.is_dirty = true;
                return true;
            }
        }
        false
    }

    pub fn set_segments_data(&mut self, segments: &MemorySegmentList) {
        for seg in &segments.segments {
            for own in &mut self.segment_list.segments {
                if own.address == seg.address && own.size() == seg.size() {
                    own.data.copy_from_slice(&seg.data);
                    self.is_dirty = true;
                    break;
                }
            }
        }
    }

    fn merge_parts(&mut self, parts: &mut [DataPart]) {
        parts.sort_by_key(|p| p.address);

        let mut new_segs: Vec<MemorySegment> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut next_addr: Option<u64> = None;
        let mut seg_start = 0u64;
        for part in parts.iter() {
            let addr = part.address as u64;
            if next_addr.is_none() {
                seg_start = addr;
            }
            if let Some(expected) = next_addr {
                if addr != expected {
                    new_segs.push(MemorySegment::file_data_segment(
                        seg_start,
                        std::mem::take(&mut buf),
                    ));
                    seg_start = addr;
                }
            }
            next_addr = Some(addr + part.data.len() as u64);
            buf.extend_from_slice(&part.data);
        }
        if !buf.is_empty() {
            new_segs.push(MemorySegment::file_data_segment(seg_start, buf));
        }

        let segs = &mut self.segment_list.segments;
        let mut code_written: Vec<(usize, u64)> = Vec::new();
        let mut i = new_segs.len();
        while i > 0 {
            i -= 1;
            let (new_start, new_end) = (new_segs[i].address, new_segs[i].end());
            let mut overlapped = false;
            let mut j = segs.len();
            while j > 0 {
                j -= 1;
                if segs[j].end() > new_start && segs[j].address <= new_end {
                    overlapped = true;
                    let src_off = segs[j].address.saturating_sub(new_start);
                    let dst_off = new_start.saturating_sub(segs[j].address);
                    let n = (segs[j].size() as u64)
                        .saturating_sub(dst_off)
                        .min((new_segs[i].size() as u64).saturating_sub(src_off));
                    let (dst_start, src_start, n) =
                        (dst_off as usize, src_off as usize, n as usize);
                    segs[j].data[dst_start..dst_start + n]
                        .copy_from_slice(&new_segs[i].data[src_start..src_start + n]);
                    if segs[j].prg_type == MemoryPrgType::CODE {
                        let extent = dst_off + n as u64;
                        match code_written.iter_mut().find(|(idx, _)| *idx == j) {
                            Some((_, e)) => *e = (*e).max(extent),
                            None => code_written.push((j, extent)),
                        }
                    }
                    segs[j].set_initialized(true);
                }
            }
            if overlapped {
                new_segs.remove(i);
            }
        }

        for (idx, extent) in code_written {
            if segs[idx].size() as u64 > extent {
                let data = segs[idx].data[..extent as usize].to_vec();
                let mut replacement =
                    MemorySegment::new(segs[idx].address, extent as usize, MemoryPrgType::CODE);
                replacement.set_data_bytes(0, &data);
                segs[idx] = replacement;
            }
        }

        segs.extend(new_segs);
        segs.sort_by(sort_data_segs_to_front);
        self.changed_values.clear();
        self.is_dirty = false;
    }
}

impl fmt::Display for DataFileBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.changed_values.is_empty() {
            write!(f, "{} changed value(s), ", self.changed_values.len())?;
        }
        write!(f, "{}", self.segment_list)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn hex_val(c: u8, line: u32, col: usize) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::DataFile {
            line,
            message: format!("invalid hex character at column {col}"),
        }),
    }
}

fn to_hex_byte(text: &str, pos: usize, line: u32) -> Result<u8> {
    let bytes = text.as_bytes();
    if pos + 2 > bytes.len() {
        return Err(Error::DataFile {
            line,
            message: format!("unexpected end of line (need 2 hex chars at column {pos})"),
        });
    }
    Ok((hex_val(bytes[pos], line, pos)? << 4) | hex_val(bytes[pos + 1], line, pos + 1)?)
}

fn checksum_failure(line: u32) -> Error {
    Error::DataFile {
        line,
        message: format!("Checksum failure in line {line}!"),
    }
}

fn write_hex_record(out: &mut String, rec: &[u8]) {
    use std::fmt::Write as _;
    let mut sum: u32 = 0;
    out.push(':');
    for b in rec {
        sum += *b as u32;
        let _ = write!(out, "{b:02X}");
    }
    let check = ((256 - (sum & 0xFF)) & 0xFF) as u8;
    let _ = writeln!(out, "{check:02X}\r");
}

fn write_s_record(out: &mut String, rec: &[u8]) {
    use std::fmt::Write as _;
    let mut sum: u32 = 0;
    let _ = write!(out, "S{}", rec[0]);
    for b in &rec[1..] {
        sum += *b as u32;
        let _ = write!(out, "{b:02X}");
    }
    let check = !(sum as u8);
    let _ = writeln!(out, "{check:02X}\r");
}

fn find_byte_matches(haystack: &[u8], needle: &[u8]) -> Vec<i64> {
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let last = needle.len() as i64 - 1;
    let mut pos = last;
    let mut ni = last;
    while pos < haystack.len() as i64 {
        if ni < 0 {
            pos += 1;
            matches.push(pos);
            pos += needle.len() as i64 + last;
            ni = last;
        } else if haystack[pos as usize] != needle[ni as usize] {
            pos += needle.len() as i64 - ni;
            ni = last;
        } else {
            ni -= 1;
            pos -= 1;
        }
    }
    matches.reverse();
    matches
}

// ---------------------------------------------------------------------------
// DataFileBin
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DataFileBin {
    pub base: DataFileBase,
    pub epk_address: u32,
    pub epk: Option<String>,
    original_image: Option<Vec<u8>>,
}

impl DataFileBin {
    pub fn new(
        source_filename: Option<String>,
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<String>,
    ) -> Self {
        DataFileBin {
            base: DataFileBase::new(source_filename, segments),
            epk_address,
            epk,
            original_image: None,
        }
    }

    pub fn parse(
        data: &[u8],
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<Self> {
        let mut file = Self::new(None, segments, epk_address, epk.map(str::to_string));
        file.load_bytes(data)?;
        Ok(file)
    }

    pub fn from_file(
        path: &Path,
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<Self> {
        let data = std::fs::read(path)?;
        let mut file = Self::parse(&data, segments, epk_address, epk)?;
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
        let data = std::fs::read(&path)?;
        self.load_bytes(&data)
    }

    pub fn load_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.original_image = Some(data.to_vec());
        let epk = self.epk.clone().unwrap_or_default();
        if epk.is_empty() {
            self.base
                .segment_list
                .segments
                .push(MemorySegment::from_data(
                    0,
                    data.to_vec(),
                    MemoryPrgType::UNKNOWN,
                    true,
                ));
            return Ok(());
        }
        let seg_idx = match self
            .base
            .segment_list
            .segments
            .iter()
            .position(|s| s.is_memory_in(self.epk_address as u64, epk.len()))
        {
            Some(idx) if self.base.segment_list.segments[idx].prg_type == MemoryPrgType::DATA => {
                idx
            }
            _ => {
                return Err(Error::DataFile {
                    line: 0,
                    message: format!(
                        "Data segment containing EPK Address 0x{:X} not found!",
                        self.epk_address
                    ),
                });
            }
        };
        let page_off = self.epk_address as i64 % 65536;
        for m in find_byte_matches(data, epk.as_bytes()) {
            if m % 65536 != page_off {
                continue;
            }
            let bin_off = self.epk_address as i64 - m;
            let seg = &self.base.segment_list.segments[seg_idx];
            let seg_file_off = seg.address as i64 - bin_off;
            if seg_file_off + seg.size() as i64 > data.len() as i64 {
                continue;
            }
            if seg_file_off < 0 {
                return Err(Error::DataFile {
                    line: 0,
                    message: format!(
                        "EPK match at 0x{m:X} maps segment 0x{:X} before start of BIN image",
                        seg.address
                    ),
                });
            }
            let (start, size) = (seg_file_off as usize, seg.size());
            {
                let seg = &mut self.base.segment_list.segments[seg_idx];
                seg.data.copy_from_slice(&data[start..start + size]);
                seg.set_initialized(true);
                seg.bin_file_offset = bin_off;
            }
            let seg = self.base.segment_list.segments.remove(seg_idx);
            self.base.segment_list.segments.clear();
            self.base.segment_list.segments.push(seg);
            self.base.changed_values.clear();
            self.base.is_dirty = false;
            break;
        }
        Ok(())
    }

    pub fn save_bin(&mut self) -> Result<Vec<u8>> {
        let mut image = self.original_image.clone().unwrap_or_default();
        for seg in &self.base.segment_list.segments {
            let pos = seg.address as i64 - seg.bin_file_offset;
            if pos < 0 {
                return Err(Error::DataFile {
                    line: 0,
                    message: format!(
                        "segment at 0x{:X} maps before start of BIN image",
                        seg.address
                    ),
                });
            }
            let (pos, size) = (pos as usize, seg.size());
            if pos + size > image.len() {
                image.resize(pos + size, 0);
            }
            image[pos..pos + size].copy_from_slice(&seg.data);
        }
        self.base.is_dirty = false;
        self.base.changed_values.clear();
        Ok(image)
    }

    pub fn save_file(&mut self, path: &Path) -> Result<()> {
        let image = self.save_bin()?;
        std::fs::write(path, image)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DataFileHex(Intel HEX)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DataFileHex {
    pub base: DataFileBase,
}

impl DataFileHex {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        DataFileHex {
            base: DataFileBase::new(source_filename, segments),
        }
    }

    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        let mut file = Self::new(None, segments);
        file.load_str(text)?;
        Ok(file)
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let content = std::fs::read(path)?;
        let mut file = Self::parse(&String::from_utf8_lossy(&content), segments)?;
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
        let content = std::fs::read(&path)?;
        self.load_str(&String::from_utf8_lossy(&content))
    }

    pub fn load_str(&mut self, text: &str) -> Result<()> {
        self.base.data_bytes_per_line = 0;
        let mut parts: Vec<DataPart> = Vec::new();
        let mut base_addr: u32 = 0;
        for (line_idx, raw) in text.lines().enumerate() {
            let line = line_idx as u32 + 1;
            let t = raw.trim();
            if t.len() <= 7 || !t.starts_with(':') {
                continue;
            }
            let count = to_hex_byte(t, 1, line)?;
            if count == 0 {
                continue;
            }
            let mut sum = count as u32;
            let rec_type = match to_hex_byte(t, 7, line) {
                Ok(b) => b,
                Err(_) => continue,
            };
            sum += rec_type as u32;
            match rec_type {
                2 => {
                    let hi = to_hex_byte(t, 9, line)?;
                    let lo = to_hex_byte(t, 11, line)?;
                    base_addr = ((hi as u32) << 12) + ((lo as u32) << 4);
                }
                3 | 5 => {
                    let b3 = to_hex_byte(t, 9, line)?;
                    let b4 = to_hex_byte(t, 11, line)?;
                    let b5 = to_hex_byte(t, 13, line)?;
                    let b6 = to_hex_byte(t, 15, line)?;
                    base_addr = b3 as u32 + ((b4 as u32) << 16) + ((b5 as u32) << 8) + b6 as u32;
                }
                4 => {
                    let hi = to_hex_byte(t, 9, line)?;
                    let lo = to_hex_byte(t, 11, line)?;
                    base_addr = ((hi as u32) << 24) + ((lo as u32) << 16);
                }
                0 => {
                    let a_hi = to_hex_byte(t, 3, line)?;
                    let a_lo = to_hex_byte(t, 5, line)?;
                    sum += a_hi as u32 + a_lo as u32;
                    let addr = base_addr.wrapping_add(((a_hi as u32) << 8) + a_lo as u32);
                    let mut data = vec![0u8; count as usize];
                    self.base.data_bytes_per_line =
                        self.base.data_bytes_per_line.max(count as usize);
                    let mut pos = 9;
                    for b in data.iter_mut() {
                        let v = to_hex_byte(t, pos, line)?;
                        sum += v as u32;
                        *b = v;
                        pos += 2;
                    }
                    let check = ((256 - (sum & 0xFF)) & 0xFF) as u8;
                    if check != to_hex_byte(t, t.len() - 2, line)? {
                        return Err(checksum_failure(line));
                    }
                    parts.push(DataPart {
                        address: addr,
                        data,
                    });
                }
                _ => {}
            }
        }
        self.base.merge_parts(&mut parts);
        Ok(())
    }

    pub fn write_hex(&mut self, only_data_segments: bool, data_bytes_per_line: usize) -> String {
        let segs: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|s| s.is_initialized() && (s.is_data_segment() || !only_data_segments))
            .collect();
        if segs.is_empty() {
            return String::new();
        }
        if data_bytes_per_line > 0 {
            self.base.data_bytes_per_line = data_bytes_per_line;
        } else if self.base.data_bytes_per_line == 0 {
            self.base.data_bytes_per_line = DEFAULT_DATA_BYTES_PER_LINE;
        }
        let dbpl = self.base.data_bytes_per_line;
        let mut segs = segs;
        segs.sort_by_key(|s| s.address);
        let big_endian_host = cfg!(target_endian = "big");
        let mut out = String::new();
        for seg in segs {
            let mut count = 0usize;
            let mut addr = seg.address;
            let end = seg.end();
            while addr < end {
                if addr == seg.address || (addr & 0xFFFF) == 0 {
                    let mut hi16 = (addr >> 16) as u16;
                    if big_endian_host {
                        hi16 = hi16.swap_bytes();
                    }
                    write_hex_record(&mut out, &[2, 0, 0, 4, (hi16 >> 8) as u8, hi16 as u8]);
                }
                count += 1;
                addr += 1;
                if count.is_multiple_of(dbpl) || (addr & 0xFFFF) == 0 || addr == end {
                    let start = addr - count as u64;
                    let mut a16 = start as u16;
                    if big_endian_host {
                        a16 = a16.swap_bytes();
                    }
                    let off = (start - seg.address) as usize;
                    let mut rec = Vec::with_capacity(4 + count);
                    rec.push(count as u8);
                    rec.push((a16 >> 8) as u8);
                    rec.push(a16 as u8);
                    rec.push(0);
                    rec.extend_from_slice(&seg.data[off..off + count]);
                    write_hex_record(&mut out, &rec);
                    count = 0;
                }
            }
        }
        out.push_str(":00000001FF\r\n");
        self.base.is_dirty = false;
        self.base.changed_values.clear();
        out
    }

    pub fn save_file(
        &mut self,
        path: &Path,
        only_data_segments: bool,
        data_bytes_per_line: usize,
    ) -> Result<()> {
        let text = self.write_hex(only_data_segments, data_bytes_per_line);
        std::fs::write(path, text)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DataFileS19(Motorola S-record)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotorolaFileType {
    #[default]
    NotSet = 0,
    TwoByte = 1,
    ThreeByte = 2,
    FourByte = 3,
}

impl MotorolaFileType {
    fn addr_len(self) -> usize {
        self as usize + 1
    }

    fn from_record_type(rec_type: u8) -> Self {
        match rec_type {
            1 => MotorolaFileType::TwoByte,
            2 => MotorolaFileType::ThreeByte,
            _ => MotorolaFileType::FourByte,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DataFileS19 {
    pub base: DataFileBase,
    pub header: String,
    pub write_file_type: MotorolaFileType,
}

impl DataFileS19 {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        DataFileS19 {
            base: DataFileBase::new(source_filename, segments),
            header: DEFAULT_S0_HEADER.to_string(),
            write_file_type: MotorolaFileType::FourByte,
        }
    }

    pub fn parse(text: &str, segments: MemorySegmentList) -> Result<Self> {
        let mut file = Self::new(None, segments);
        file.load_str(text)?;
        Ok(file)
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let content = std::fs::read(path)?;
        let mut file = Self::parse(&String::from_utf8_lossy(&content), segments)?;
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
        let content = std::fs::read(&path)?;
        self.load_str(&String::from_utf8_lossy(&content))
    }

    pub fn load_str(&mut self, text: &str) -> Result<()> {
        self.base.data_bytes_per_line = 0;
        let mut parts: Vec<DataPart> = Vec::new();
        for (line_idx, raw) in text.lines().enumerate() {
            let line = line_idx as u32 + 1;
            let t = raw.trim();
            let bytes = t.as_bytes();
            if bytes.is_empty() || !bytes[0].eq_ignore_ascii_case(&b'S') {
                continue;
            }
            if bytes.len() < 2 {
                continue;
            }
            let rec_type = bytes[1].wrapping_sub(b'0');
            match rec_type {
                0 => self.header = t.to_string(),
                1..=3 => {
                    let count = to_hex_byte(t, 2, line)? as usize;
                    if 4 + 2 * count != t.len() {
                        continue;
                    }
                    let addr_len = rec_type as usize + 1;
                    if count < addr_len + 1 {
                        return Err(Error::DataFile {
                            line,
                            message: format!(
                                "record count {count} too small for S{rec_type} record"
                            ),
                        });
                    }
                    let mut sum = count as u32;
                    let mut pos = 4;
                    let mut addr: u32 = 0;
                    for _ in 0..addr_len {
                        let b = to_hex_byte(t, pos, line)?;
                        addr = (addr << 8) + b as u32;
                        sum += b as u32;
                        pos += 2;
                    }
                    self.write_file_type = MotorolaFileType::from_record_type(rec_type);
                    let data_len = count - addr_len - 1;
                    self.base.data_bytes_per_line = self.base.data_bytes_per_line.max(data_len);
                    let mut data = vec![0u8; data_len];
                    for b in data.iter_mut() {
                        let v = to_hex_byte(t, pos, line)?;
                        sum += v as u32;
                        *b = v;
                        pos += 2;
                    }
                    if !(sum as u8) != to_hex_byte(t, pos, line)? {
                        return Err(checksum_failure(line));
                    }
                    parts.push(DataPart {
                        address: addr,
                        data,
                    });
                }
                _ => {}
            }
        }
        self.base.merge_parts(&mut parts);
        Ok(())
    }

    pub fn write_s19(&mut self, only_data_segments: bool, data_bytes_per_line: usize) -> String {
        let segs: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|s| s.is_initialized() && (s.is_data_segment() || !only_data_segments))
            .collect();
        if segs.is_empty() {
            return String::new();
        }
        if data_bytes_per_line > 0 {
            self.base.data_bytes_per_line = data_bytes_per_line;
        } else if self.base.data_bytes_per_line == 0 {
            self.base.data_bytes_per_line = DEFAULT_DATA_BYTES_PER_LINE;
        }
        let dbpl = self.base.data_bytes_per_line;
        let mut segs = segs;
        segs.sort_by_key(|s| s.address);
        let little_endian_host = cfg!(target_endian = "little");
        let ftype = if self.write_file_type == MotorolaFileType::NotSet {
            MotorolaFileType::FourByte
        } else {
            self.write_file_type
        };
        let mut out = String::new();
        out.push_str(&self.header);
        out.push_str("\r\n");
        for seg in segs {
            let mut addr = seg.address;
            let mut written = 0usize;
            while written < seg.size() {
                let n = (seg.size() - written).min(dbpl);
                let rec = build_s_record(
                    little_endian_host,
                    ftype,
                    addr as u32,
                    &seg.data,
                    n,
                    (addr - seg.address) as usize,
                );
                write_s_record(&mut out, &rec);
                written += dbpl;
                addr += dbpl as u64;
            }
        }
        let mut end_rec = vec![0u8; 2 + ftype as usize + 1];
        match ftype {
            MotorolaFileType::TwoByte => {
                end_rec[0] = 9;
                end_rec[1] = 3;
            }
            MotorolaFileType::ThreeByte => {
                end_rec[0] = 8;
                end_rec[1] = 4;
            }
            _ => {
                end_rec[0] = 7;
                end_rec[1] = 5;
            }
        }
        write_s_record(&mut out, &end_rec);
        self.base.is_dirty = false;
        self.base.changed_values.clear();
        out
    }

    pub fn save_file(
        &mut self,
        path: &Path,
        only_data_segments: bool,
        data_bytes_per_line: usize,
    ) -> Result<()> {
        let text = self.write_s19(only_data_segments, data_bytes_per_line);
        std::fs::write(path, text)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

fn build_s_record(
    little_endian: bool,
    ftype: MotorolaFileType,
    addr: u32,
    data: &[u8],
    n: usize,
    offset: usize,
) -> Vec<u8> {
    let addr_len = ftype.addr_len();
    let mut rec = vec![0u8; 2 + addr_len + n];
    rec[0] = ftype as u8;
    rec[1] = (1 + addr_len + n) as u8;
    match ftype {
        MotorolaFileType::TwoByte => {
            let a = if little_endian {
                addr.swap_bytes()
            } else {
                addr
            };
            rec[2] = a as u8;
            rec[3] = (a >> 8) as u8;
        }
        MotorolaFileType::ThreeByte => {
            let lo = if little_endian {
                (addr as u16).swap_bytes()
            } else {
                addr as u16
            };
            let hi = if little_endian {
                ((addr >> 16) as u16).swap_bytes()
            } else {
                (addr >> 16) as u16
            };
            rec[2] = (hi >> 8) as u8;
            rec[3] = lo as u8;
            rec[4] = (lo >> 8) as u8;
        }
        _ => {
            let a = if little_endian {
                addr.swap_bytes()
            } else {
                addr
            };
            rec[2] = a as u8;
            rec[3] = (a >> 8) as u8;
            rec[4] = (a >> 16) as u8;
            rec[5] = (a >> 24) as u8;
        }
    }
    rec[2 + addr_len..2 + addr_len + n].copy_from_slice(&data[offset..offset + n]);
    rec
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFileType {
    /// Intel HEX(`.hex` / `.h86`).
    IntelHex,
    /// Motorola S-record(`.s19` / `.s28` / `.s37` / `.s` / `.s1` / `.s2` /
    /// `.s3` / `.sx` / `.srec`).
    MotorolaS,
    /// A stream containing both Intel HEX and Motorola S-record lines.
    Mixed,
    /// Plain hexadecimal ASCII bytes without address metadata.
    HexAscii,
    /// Ford/Volvo Versatile Binary Format with raw data blocks.
    FordVbf,
    /// Ford metadata header followed by Intel HEX records.
    FordIHex,
    /// Texas Instruments address-marked text format.
    TiTxt,
    /// Microsoft 512-byte-block microcontroller flashing format.
    Uf2,
    Binary,
}

fn data_file_type_for_extension(ext_lower: &str) -> DataFileType {
    match ext_lower {
        ".hex" | ".h86" | ".ihex" | ".ihx" => DataFileType::IntelHex,
        ".s19" | ".s28" | ".s37" | ".s" | ".s1" | ".s2" | ".s3" | ".sx" | ".srec" => {
            DataFileType::MotorolaS
        }
        ".hascii" | ".hexascii" => DataFileType::HexAscii,
        ".vbf" => DataFileType::FordVbf,
        ".titxt" | ".ti-txt" | ".ti_txt" => DataFileType::TiTxt,
        ".uf2" => DataFileType::Uf2,
        _ => DataFileType::Binary,
    }
}

#[derive(Debug, Clone)]
pub enum DataFile {
    /// BIN.
    Bin(DataFileBin),
    /// Intel HEX.
    Hex(DataFileHex),
    /// Motorola S-record.
    S19(DataFileS19),
    /// Mixed Intel HEX and Motorola S-records (read-only).
    Mixed(DataFileMixed),
    /// Plain hexadecimal ASCII.
    HexAscii(DataFileHexAscii),
    /// Ford/Volvo VBF.
    Vbf(DataFileVbf),
    /// Ford Intel HEX container (read-only).
    FordIHex(DataFileFordIHex),
    /// Texas Instruments TI-TXT.
    TiTxt(DataFileTiTxt),
    /// Microsoft UF2.
    Uf2(DataFileUf2),
}

impl DataFile {
    pub fn base(&self) -> &DataFileBase {
        match self {
            DataFile::Bin(f) => &f.base,
            DataFile::Hex(f) => &f.base,
            DataFile::S19(f) => &f.base,
            DataFile::Mixed(f) => &f.base,
            DataFile::HexAscii(f) => &f.base,
            DataFile::Vbf(f) => &f.base,
            DataFile::FordIHex(f) => &f.base,
            DataFile::TiTxt(f) => &f.base,
            DataFile::Uf2(f) => &f.base,
        }
    }

    pub fn base_mut(&mut self) -> &mut DataFileBase {
        match self {
            DataFile::Bin(f) => &mut f.base,
            DataFile::Hex(f) => &mut f.base,
            DataFile::S19(f) => &mut f.base,
            DataFile::Mixed(f) => &mut f.base,
            DataFile::HexAscii(f) => &mut f.base,
            DataFile::Vbf(f) => &mut f.base,
            DataFile::FordIHex(f) => &mut f.base,
            DataFile::TiTxt(f) => &mut f.base,
            DataFile::Uf2(f) => &mut f.base,
        }
    }

    pub fn parse(
        file_type: DataFileType,
        content: &[u8],
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<Self> {
        Ok(match file_type {
            DataFileType::IntelHex => DataFile::Hex(DataFileHex::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::MotorolaS => DataFile::S19(DataFileS19::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::Mixed => DataFile::Mixed(DataFileMixed::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::HexAscii => DataFile::HexAscii(DataFileHexAscii::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::FordVbf => DataFile::Vbf(DataFileVbf::parse(content, segments)?),
            DataFileType::FordIHex => DataFile::FordIHex(DataFileFordIHex::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::TiTxt => DataFile::TiTxt(DataFileTiTxt::parse(
                &String::from_utf8_lossy(content),
                segments,
            )?),
            DataFileType::Uf2 => DataFile::Uf2(DataFileUf2::parse(content, segments)?),
            DataFileType::Binary => {
                DataFile::Bin(DataFileBin::parse(content, segments, epk_address, epk)?)
            }
        })
    }

    pub fn open(
        path: &Path,
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<Self> {
        Ok(match Self::file_type_for(path) {
            DataFileType::IntelHex => DataFile::Hex(DataFileHex::from_file(path, segments)?),
            DataFileType::MotorolaS => DataFile::S19(DataFileS19::from_file(path, segments)?),
            DataFileType::Mixed => DataFile::Mixed(DataFileMixed::from_file(path, segments)?),
            DataFileType::HexAscii => {
                DataFile::HexAscii(DataFileHexAscii::from_file(path, segments)?)
            }
            DataFileType::FordVbf => DataFile::Vbf(DataFileVbf::from_file(path, segments)?),
            DataFileType::FordIHex => {
                DataFile::FordIHex(DataFileFordIHex::from_file(path, segments)?)
            }
            DataFileType::TiTxt => DataFile::TiTxt(DataFileTiTxt::from_file(path, segments)?),
            DataFileType::Uf2 => DataFile::Uf2(DataFileUf2::from_file(path, segments)?),
            DataFileType::Binary => {
                DataFile::Bin(DataFileBin::from_file(path, segments, epk_address, epk)?)
            }
        })
    }

    pub fn file_type_for(path: &Path) -> DataFileType {
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        data_file_type_for_extension(&ext)
    }
}

pub fn nearest_data_file(src_file: &Path, epk: Option<&str>) -> Result<Option<PathBuf>> {
    let dir = match src_file.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    let matches_wildcard = |p: &PathBuf, wildcard: &str| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(&wildcard[1..]))
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for wildcard in DATA_FILE_WILDCARDS {
        candidates.extend(
            entries
                .iter()
                .filter(|p| matches_wildcard(p, wildcard))
                .cloned(),
        );
    }
    if epk.is_some_and(|e| !e.is_empty()) {
        candidates.extend(
            entries
                .iter()
                .filter(|p| matches_wildcard(p, BIN_WILDCARD))
                .cloned(),
        );
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let stem_of = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let src_stem = stem_of(src_file);
    if let Some(p) = candidates.iter().find(|p| stem_of(p) == src_stem) {
        return Ok(Some(p.clone()));
    }
    let lower = src_stem.to_lowercase();
    if let Some(p) = candidates.iter().find(|p| {
        let name = stem_of(p).to_lowercase();
        name.starts_with(&lower) || lower.starts_with(&name)
    }) {
        return Ok(Some(p.clone()));
    }
    Ok(Some(candidates[0].clone()))
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const HEX_SAMPLE: &str = ":020000040001F9\n\
                              :100010000102030405060708090A0B0C0D0E0F1058\n\
                              :100020001112131415161718191A1B1C1D1E1F2048\n\
                              :00000001FF\n";

    const HEX_SEG_ADDR: &str = ":020000020002FA\n\
                                :0400000001020304F2\n\
                                :0400100005060708D2\n\
                                :00000001FF\n";

    const HEX_START_ADDR: &str = ":04123405123456789D\n:04000000AABBCCDDEE\n";

    const S19_SAMPLE: &str = "S00600004844521B\n\
                              S107100001020304DE\n\
                              S206123456DEADD2\n\
                              S30712345678BEEF37\n\
                              S70500000000FA\n";

    fn data_seg(address: u64, data: &[u8]) -> MemorySegment {
        MemorySegment::from_data(address, data.to_vec(), MemoryPrgType::DATA, true)
    }

    fn seg_summary(list: &MemorySegmentList) -> Vec<(u64, MemoryPrgType, Vec<u8>, bool)> {
        list.segments
            .iter()
            .map(|s| (s.address, s.prg_type, s.data().to_vec(), s.is_initialized()))
            .collect()
    }

    #[test]
    fn hex_parse_basic() {
        let file = DataFileHex::parse(HEX_SAMPLE, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0x10010);
        assert_eq!(seg.prg_type, MemoryPrgType::DATA);
        assert!(seg.is_initialized());
        assert_eq!(seg.size(), 32);
        assert_eq!(seg.data()[0], 0x01);
        assert_eq!(seg.data()[31], 0x20);
        assert_eq!(file.base.data_bytes_per_line, 16);
        assert!(!file.base.is_dirty);
    }

    #[test]
    fn hex_parse_extended_segment_address() {
        let file = DataFileHex::parse(HEX_SEG_ADDR, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 2);
        assert_eq!(file.base.segment_list.segments[0].address, 0x20);
        assert_eq!(file.base.segment_list.segments[0].data(), &[1, 2, 3, 4]);
        assert_eq!(file.base.segment_list.segments[1].address, 0x30);
        assert_eq!(file.base.segment_list.segments[1].data(), &[5, 6, 7, 8]);
    }

    #[test]
    fn hex_parse_start_address_record() {
        let file = DataFileHex::parse(HEX_START_ADDR, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0x34568A);
        assert_eq!(seg.data(), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn hex_parse_contiguous_merge() {
        let text = ":0400000001020304F2\n:0400040005060708DE\n";
        let file = DataFileHex::parse(text, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0);
        assert_eq!(seg.data(), &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn hex_parse_skips_garbage() {
        let text = "garbage line\n\
                    :020000040001F9\n\
                    \n\
                    :x\n\
                    :100000ZZ0102030405060708090A0B0C0D0E0F1000\n\
                    :00000001FF\n";
        let file = DataFileHex::parse(text, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 0);
    }

    #[test]
    fn hex_parse_checksum_failure() {
        let bad = HEX_SAMPLE.replace("1D1E1F2048", "1D1E1F2049");
        let err = DataFileHex::parse(&bad, MemorySegmentList::new()).unwrap_err();
        match err {
            Error::DataFile { line, message } => {
                assert_eq!(line, 3);
                assert!(message.contains("Checksum failure"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn hex_parse_invalid_hex_char() {
        let err = DataFileHex::parse(":0Z000000FF\n", MemorySegmentList::new()).unwrap_err();
        assert!(matches!(err, Error::DataFile { line: 1, .. }));
    }

    #[test]
    fn hex_parse_truncated_data_record() {
        let err = DataFileHex::parse(":10000000010203\n", MemorySegmentList::new()).unwrap_err();
        assert!(matches!(err, Error::DataFile { line: 1, .. }));
    }

    #[test]
    fn hex_write_roundtrip() {
        let mut file = DataFileHex::parse(HEX_SAMPLE, MemorySegmentList::new()).unwrap();
        let out = file.write_hex(true, 0);
        let expected = ":020000040001F9\r\n\
                        :100010000102030405060708090A0B0C0D0E0F1058\r\n\
                        :100020001112131415161718191A1B1C1D1E1F2048\r\n\
                        :00000001FF\r\n";
        assert_eq!(out, expected);
        let file2 = DataFileHex::parse(&out, MemorySegmentList::new()).unwrap();
        assert_eq!(
            seg_summary(&file.base.segment_list),
            seg_summary(&file2.base.segment_list)
        );
    }

    #[test]
    fn hex_write_crosses_64k_boundary() {
        let data: Vec<u8> = (0u8..32).collect();
        let segs = MemorySegmentList::from_vec(vec![data_seg(0xFFF0, &data)]);
        let mut file = DataFileHex::new(None, segs);
        let out = file.write_hex(true, 0);
        let expected = ":020000040000FA\r\n\
                        :10FFF000000102030405060708090A0B0C0D0E0F89\r\n\
                        :020000040001F9\r\n\
                        :10000000101112131415161718191A1B1C1D1E1F78\r\n\
                        :00000001FF\r\n";
        assert_eq!(out, expected);
        let file2 = DataFileHex::parse(&out, MemorySegmentList::new()).unwrap();
        assert_eq!(file2.base.segment_list.len(), 1);
        assert_eq!(file2.base.segment_list.segments[0].data(), &data[..]);
    }

    #[test]
    fn hex_write_data_bytes_per_line_override() {
        let data: Vec<u8> = (1u8..=16).collect();
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x1000, &data)]);
        let mut file = DataFileHex::new(None, segs);
        let out = file.write_hex(true, 8);
        let expected = ":020000040000FA\r\n\
                        :081000000102030405060708C4\r\n\
                        :08100800090A0B0C0D0E0F107C\r\n\
                        :00000001FF\r\n";
        assert_eq!(out, expected);
        assert_eq!(file.base.data_bytes_per_line, 8);
    }

    #[test]
    fn hex_write_only_data_segments_filter() {
        let segs = MemorySegmentList::from_vec(vec![
            data_seg(0x1000, &[0xAA, 0xBB, 0xCC, 0xDD]),
            MemorySegment::from_data(0x2000, vec![0x11, 0x22], MemoryPrgType::CODE, true),
            MemorySegment::new(0x3000, 4, MemoryPrgType::DATA),
        ]);
        let mut file = DataFileHex::new(None, segs);
        let out = file.write_hex(true, 0);
        assert!(out.contains(":04100000AABBCCDDDE"));
        assert!(!out.contains("1122"));
        let out_all = file.write_hex(false, 0);
        assert!(out_all.contains("1122"));
    }

    #[test]
    fn s19_parse_basic() {
        let file = DataFileS19::parse(S19_SAMPLE, MemorySegmentList::new()).unwrap();
        assert_eq!(file.header, "S00600004844521B");
        assert_eq!(file.write_file_type, MotorolaFileType::FourByte);
        assert_eq!(file.base.segment_list.len(), 3);
        assert_eq!(file.base.segment_list.segments[0].address, 0x1000);
        assert_eq!(file.base.segment_list.segments[0].data(), &[1, 2, 3, 4]);
        assert_eq!(file.base.segment_list.segments[1].address, 0x123456);
        assert_eq!(file.base.segment_list.segments[1].data(), &[0xDE, 0xAD]);
        assert_eq!(file.base.segment_list.segments[2].address, 0x12345678);
        assert_eq!(file.base.segment_list.segments[2].data(), &[0xBE, 0xEF]);
        assert_eq!(file.base.data_bytes_per_line, 4);
    }

    #[test]
    fn s19_parse_sets_write_file_type() {
        let file = DataFileS19::parse("S10510000102E7\n", MemorySegmentList::new()).unwrap();
        assert_eq!(file.write_file_type, MotorolaFileType::TwoByte);
        let file = DataFileS19::parse("S206123456DEADD2\n", MemorySegmentList::new()).unwrap();
        assert_eq!(file.write_file_type, MotorolaFileType::ThreeByte);
    }

    #[test]
    fn s19_parse_checksum_failure() {
        let bad = S19_SAMPLE.replace("01020304DE", "01020304DF");
        let err = DataFileS19::parse(&bad, MemorySegmentList::new()).unwrap_err();
        match err {
            Error::DataFile { line, message } => {
                assert_eq!(line, 2);
                assert!(message.contains("Checksum failure"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn s19_parse_length_mismatch_skipped() {
        let file = DataFileS19::parse("S10510000102\n", MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 0);
    }

    #[test]
    fn s19_parse_skips_non_s_lines_and_termination() {
        let text = "HDR comment\ns9030000FC\n\nS70500000000FA\n";
        let file = DataFileS19::parse(text, MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 0);
        let file = DataFileS19::parse("s10510000102E7\n", MemorySegmentList::new()).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
    }

    #[test]
    fn s19_write_s3() {
        let data: Vec<u8> = (0u8..16).collect();
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x1000, &data)]);
        let mut file = DataFileS19::new(None, segs);
        let out = file.write_s19(true, 0);
        let expected = "S0030000FC\r\n\
                        S31500001000000102030405060708090A0B0C0D0E0F62\r\n\
                        S70500000000FA\r\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn s19_write_s2() {
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x123456, &[0xAA])]);
        let mut file = DataFileS19::new(None, segs);
        file.write_file_type = MotorolaFileType::ThreeByte;
        let out = file.write_s19(true, 0);
        let expected = "S0030000FC\r\nS205123456AAB4\r\nS804000000FB\r\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn s19_write_s1_address_quirk() {
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x1234, &[0xAA, 0xBB])]);
        let mut file = DataFileS19::new(None, segs);
        file.write_file_type = MotorolaFileType::TwoByte;
        let out = file.write_s19(true, 0);
        if cfg!(target_endian = "little") {
            assert_eq!(out, "S0030000FC\r\nS1050000AABB95\r\nS9030000FC\r\n");
        } else {
            assert_eq!(out, "S0030000FC\r\nS1051234AABB4F\r\nS9030000FC\r\n");
        }
    }

    #[test]
    fn s19_roundtrip() {
        let mut file = DataFileS19::parse(S19_SAMPLE, MemorySegmentList::new()).unwrap();
        let out = file.write_s19(true, 0);
        assert!(out.starts_with("S00600004844521B\r\n"));
        assert!(out.ends_with("S70500000000FA\r\n"));
        let file2 = DataFileS19::parse(&out, MemorySegmentList::new()).unwrap();
        assert_eq!(
            seg_summary(&file.base.segment_list),
            seg_summary(&file2.base.segment_list)
        );
    }

    #[test]
    fn s19_write_header_from_parse() {
        let file = DataFileS19::parse("S10510000102E7\n", MemorySegmentList::new()).unwrap();
        assert_eq!(file.header, DEFAULT_S0_HEADER);
    }

    #[test]
    fn merge_into_existing_code_segment_shrinks() {
        let segs = MemorySegmentList::from_vec(vec![MemorySegment::new(
            0x1000,
            0x100,
            MemoryPrgType::CODE,
        )]);
        let mut file = DataFileHex::new(None, segs);
        file.load_str(":101000000102030405060708090A0B0C0D0E0F1058\n")
            .unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.prg_type, MemoryPrgType::CODE);
        assert_eq!(seg.size(), 16);
        assert!(seg.is_initialized());
        let expected: Vec<u8> = (1u8..=16).collect();
        assert_eq!(seg.data(), &expected[..]);
    }

    #[test]
    fn merge_adjacent_before_existing_is_dropped() {
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x2000, &[0xCD; 0x100])]);
        let mut file = DataFileHex::new(None, segs);
        file.load_str(":101FF0000102030405060708090A0B0C0D0E0F1059\n")
            .unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0x2000);
        assert!(seg.data().iter().all(|&b| b == 0xCD));
    }

    #[test]
    fn merge_overlap_copies_into_existing_data_segment() {
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x1002, &[0xFF; 8])]);
        let mut file = DataFileHex::new(None, segs);
        file.load_str(":0410000001020304E2\n").unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(
            seg.data(),
            &[0x03, 0x04, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    // ------------------------- BIN -------------------------

    fn epk_bin_image() -> Vec<u8> {
        let mut image = vec![0u8; 0xF200];
        for (i, b) in image[0xF000..0xF100].iter_mut().enumerate() {
            *b = i as u8;
        }
        image[0xF000..0xF006].copy_from_slice(b"EPK123");
        image
    }

    fn epk_segments() -> MemorySegmentList {
        MemorySegmentList::from_vec(vec![
            MemorySegment::new(0x1F000, 0x100, MemoryPrgType::DATA),
            MemorySegment::new(0x30000, 0x10, MemoryPrgType::DATA),
        ])
    }

    #[test]
    fn bin_parse_without_epk() {
        let file = DataFileBin::parse(b"\x01\x02\x03", MemorySegmentList::new(), 0, None).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0);
        assert_eq!(seg.prg_type, MemoryPrgType::UNKNOWN);
        assert!(seg.is_initialized());
        assert_eq!(seg.data(), &[1, 2, 3]);
    }

    #[test]
    fn bin_parse_with_epk() {
        let image = epk_bin_image();
        let file = DataFileBin::parse(&image, epk_segments(), 0x1F000, Some("EPK123")).unwrap();
        assert_eq!(file.base.segment_list.len(), 1);
        let seg = &file.base.segment_list.segments[0];
        assert_eq!(seg.address, 0x1F000);
        assert_eq!(seg.bin_file_offset, 0x10000);
        assert!(seg.is_initialized());
        assert_eq!(seg.data(), &image[0xF000..0xF100]);
        assert!(!file.base.is_dirty);
        assert!(file.base.epk_check(0x1F000, "EPK123"));
    }

    #[test]
    fn bin_parse_epk_segment_missing() {
        let image = epk_bin_image();
        let err = DataFileBin::parse(&image, MemorySegmentList::new(), 0x1F000, Some("EPK123"))
            .unwrap_err();
        match err {
            Error::DataFile { line, message } => {
                assert_eq!(line, 0);
                assert_eq!(
                    message,
                    "Data segment containing EPK Address 0x1F000 not found!"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn bin_parse_epk_segment_wrong_type() {
        let segs = MemorySegmentList::from_vec(vec![MemorySegment::new(
            0x1F000,
            0x100,
            MemoryPrgType::CODE,
        )]);
        let image = epk_bin_image();
        let err = DataFileBin::parse(&image, segs, 0x1F000, Some("EPK123")).unwrap_err();
        assert!(matches!(err, Error::DataFile { .. }));
    }

    #[test]
    fn bin_parse_epk_not_found_in_image() {
        let image = vec![0u8; 0x100];
        let file = DataFileBin::parse(&image, epk_segments(), 0x1F000, Some("EPK123")).unwrap();
        assert_eq!(file.base.segment_list.len(), 2);
        assert!(file
            .base
            .segment_list
            .segments
            .iter()
            .all(|s| !s.is_initialized()));
    }

    #[test]
    fn bin_save_roundtrip() {
        let image = epk_bin_image();
        let mut file = DataFileBin::parse(&image, epk_segments(), 0x1F000, Some("EPK123")).unwrap();
        assert!(file.base.set_epk(0x1F008, "XY"));
        assert!(file.base.is_dirty);
        let out = file.save_bin().unwrap();
        assert!(!file.base.is_dirty);
        assert_eq!(out.len(), image.len());
        assert_eq!(&out[0xF008..0xF00A], b"XY");
        assert_eq!(out[0xF100], image[0xF100]);
    }

    #[test]
    fn bin_search_matches_descending() {
        assert_eq!(find_byte_matches(b"ABABAB", b"AB"), vec![4, 2, 0]);
        assert_eq!(find_byte_matches(b"AAAA", b"AA"), vec![2, 0]);
        assert_eq!(find_byte_matches(b"XXXX", b"AB"), Vec::<i64>::new());
        assert_eq!(find_byte_matches(b"AAAA", b""), Vec::<i64>::new());
        assert_eq!(find_byte_matches(b"A", b"AB"), Vec::<i64>::new());
    }

    #[test]
    fn epk_check_and_set_epk() {
        let segs = MemorySegmentList::from_vec(vec![data_seg(0x1000, b"HELLO_EPK_DATA__")]);
        let mut base = DataFileBase::new(None, segs);
        assert!(base.epk_check(0x1000, "HELLO"));
        assert!(!base.epk_check(0x1000, "hello"));
        assert!(base.epk_check(0x1000, ""));
        assert!(base.epk_check(0x100C, "TA"));
        assert!(!base.epk_check(0x100F, "_"));
        assert!(!base.epk_check(0x100E, "__"));
        assert!(base.set_epk(0x1005, "X"));
        assert!(base.is_dirty);
        assert!(base.epk_check(0x1000, "HELLO"));
        assert!(base.epk_check(0x1005, "XEPK_"));
        assert!(!base.set_epk(0x9000, "ZZ"));
    }

    #[test]
    fn set_segments_data_copies_matching() {
        let segs = MemorySegmentList::from_vec(vec![
            MemorySegment::new(0x1000, 4, MemoryPrgType::DATA),
            MemorySegment::new(0x2000, 4, MemoryPrgType::DATA),
        ]);
        let mut base = DataFileBase::new(None, segs);
        let source = MemorySegmentList::from_vec(vec![
            data_seg(0x1000, &[1, 2, 3, 4]),
            data_seg(0x1000, &[9, 9]),
        ]);
        base.set_segments_data(&source);
        assert_eq!(base.segment_list.segments[0].data(), &[1, 2, 3, 4]);
        assert_eq!(base.segment_list.segments[1].data(), &[0xFF; 4]);
        assert!(base.is_dirty);
    }

    #[test]
    fn memory_segment_range_guards() {
        let seg = data_seg(0x1000, &[1, 2, 3, 4]);
        assert_eq!(seg.get_data_bytes(0x1001, 2).unwrap(), &[2, 3]);
        assert!(seg.get_data_bytes(0x0FFF, 1).is_err());
        assert!(seg.get_data_bytes(0x1003, 2).is_err());
        assert!(seg.is_memory_in(0x1000, 1));
        assert!(seg.is_memory_in(0x1003, 1));
        assert!(!seg.is_memory_in(0x1004, 1));
        assert!(!seg.is_memory_in(0x0FF0, 1));
    }

    #[test]
    fn mark_changed_dedups_and_display() {
        let mut base = DataFileBase::new(None, MemorySegmentList::new());
        let rl = RecordLayoutRefFields {
            record_layout: "Layout1".to_string(),
            ..RecordLayoutRefFields::default()
        };
        base.mark_changed(rl.clone());
        base.mark_changed(rl);
        assert_eq!(base.changed_values.len(), 1);
        assert!(base.is_dirty);
        assert!(format!("{base}").contains("1 changed value(s)"));
    }

    #[test]
    fn file_type_for_extension() {
        assert_eq!(
            DataFile::file_type_for(Path::new("a.hex")),
            DataFileType::IntelHex
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.H86")),
            DataFileType::IntelHex
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.s19")),
            DataFileType::MotorolaS
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.S3")),
            DataFileType::MotorolaS
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.srec")),
            DataFileType::MotorolaS
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.bin")),
            DataFileType::Binary
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.hexascii")),
            DataFileType::HexAscii
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.titxt")),
            DataFileType::TiTxt
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.uf2")),
            DataFileType::Uf2
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a.txt")),
            DataFileType::Binary
        );
        assert_eq!(
            DataFile::file_type_for(Path::new("a")),
            DataFileType::Binary
        );
    }

    #[test]
    fn data_file_parse_dispatches() {
        let hex = DataFile::parse(
            DataFileType::IntelHex,
            HEX_SAMPLE.as_bytes(),
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(hex, DataFile::Hex(_)));
        let s19 = DataFile::parse(
            DataFileType::MotorolaS,
            S19_SAMPLE.as_bytes(),
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(s19, DataFile::S19(_)));
        let bin = DataFile::parse(
            DataFileType::Binary,
            b"\x00\x01",
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(bin, DataFile::Bin(_)));
    }

    #[test]
    fn nearest_data_file_matching() {
        let dir = std::env::temp_dir().join(format!(
            "autors_datafile_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("foo.hex"), b":00000001FF\n").unwrap();
        std::fs::write(dir.join("foobar.s19"), b"S0030000FC\n").unwrap();
        std::fs::write(dir.join("other.bin"), b"\x00").unwrap();

        let hit = nearest_data_file(&dir.join("foo.a2l"), None).unwrap();
        assert_eq!(hit.as_deref(), Some(dir.join("foo.hex").as_path()));
        let hit = nearest_data_file(&dir.join("FOOB.a2l"), None).unwrap();
        assert_eq!(hit.as_deref(), Some(dir.join("foo.hex").as_path()));
        let hit = nearest_data_file(&dir.join("zzz.a2l"), None).unwrap();
        assert_eq!(hit.as_deref(), Some(dir.join("foo.hex").as_path()));
        let hit = nearest_data_file(&dir.join("other.a2l"), Some("EPK")).unwrap();
        assert_eq!(hit.as_deref(), Some(dir.join("other.bin").as_path()));
        let hit = nearest_data_file(&dir.join("other.a2l"), None).unwrap();
        assert_eq!(hit.as_deref(), Some(dir.join("foo.hex").as_path()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nearest_data_file_empty_dir() {
        let dir = std::env::temp_dir().join(format!(
            "autors_datafile_empty_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hit = nearest_data_file(&dir.join("foo.a2l"), Some("EPK")).unwrap();
        assert_eq!(hit, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
