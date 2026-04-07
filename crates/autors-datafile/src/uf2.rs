//! Microsoft UF2 firmware import and export.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use autors_a2l::model::enums::MemoryPrgType;

use crate::datafile::{DataFileBase, MemorySegment, MemorySegmentList};
use crate::image::{MergeOptions, OverlapPolicy};
use crate::{Error, Result};

pub const UF2_BLOCK_SIZE: usize = 512;
pub const UF2_MAX_PAYLOAD_SIZE: usize = 476;
pub const UF2_FLAG_NOT_MAIN_FLASH: u32 = 0x0000_0001;
pub const UF2_FLAG_FILE_CONTAINER: u32 = 0x0000_1000;
pub const UF2_FLAG_FAMILY_ID_PRESENT: u32 = 0x0000_2000;
pub const UF2_FLAG_MD5_PRESENT: u32 = 0x0000_4000;
pub const UF2_FLAG_EXTENSION_TAGS_PRESENT: u32 = 0x0000_8000;

const MAGIC_START_0: u32 = 0x0A32_4655;
const MAGIC_START_1: u32 = 0x9E5D_5157;
const MAGIC_END: u32 = 0x0AB1_6F30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Uf2ParseOptions {
    /// Select one family from a multi-family concatenated UF2 file.
    pub family_id: Option<u32>,
    /// Include blocks marked as not targeting main flash in the memory image.
    pub include_not_main_flash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uf2WriteOptions {
    /// Payload bytes in each 512-byte block. The UF2 convention is 256.
    pub payload_size: usize,
    pub family_id: Option<u32>,
}

impl Default for Uf2WriteOptions {
    fn default() -> Self {
        Self {
            payload_size: 256,
            family_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uf2BlockInfo {
    pub flags: u32,
    pub target_address: u32,
    pub payload_size: u32,
    pub block_number: u32,
    pub number_of_blocks: u32,
    pub family_id: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DataFileUf2 {
    pub base: DataFileBase,
    pub blocks: Vec<Uf2BlockInfo>,
    pub family_id: Option<u32>,
    pub skipped_blocks: usize,
}

impl DataFileUf2 {
    pub fn new(source_filename: Option<String>, segments: MemorySegmentList) -> Self {
        Self {
            base: DataFileBase::new(source_filename, segments),
            blocks: Vec::new(),
            family_id: None,
            skipped_blocks: 0,
        }
    }

    pub fn parse(data: &[u8], segments: MemorySegmentList) -> Result<Self> {
        Self::parse_with_options(data, segments, Uf2ParseOptions::default())
    }

    pub fn parse_with_options(
        data: &[u8],
        segments: MemorySegmentList,
        options: Uf2ParseOptions,
    ) -> Result<Self> {
        let mut file = Self::new(None, segments);
        file.load_bytes(data, options)?;
        Ok(file)
    }

    pub fn from_file(path: &Path, segments: MemorySegmentList) -> Result<Self> {
        let data = std::fs::read(path)?;
        let mut file = Self::parse(&data, segments)?;
        file.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(file)
    }

    pub fn reload(&mut self, options: Uf2ParseOptions) -> Result<()> {
        let path = self
            .base
            .source_filename
            .clone()
            .ok_or_else(|| Error::DataFile {
                line: 0,
                message: "reload without source filename".to_string(),
            })?;
        let data = std::fs::read(&path)?;
        self.load_bytes(&data, options)
    }

    pub fn load_bytes(&mut self, data: &[u8], options: Uf2ParseOptions) -> Result<()> {
        let parsed_blocks = parse_blocks(data)?;
        let families: BTreeSet<u32> = parsed_blocks
            .iter()
            .filter(|block| block.info.flags & UF2_FLAG_FILE_CONTAINER == 0)
            .filter(|block| {
                options.include_not_main_flash || block.info.flags & UF2_FLAG_NOT_MAIN_FLASH == 0
            })
            .filter_map(|block| block.info.family_id)
            .collect();
        let selected_family = match options.family_id {
            Some(requested) => {
                if !families.contains(&requested) {
                    return Err(Error::Value(format!(
                        "UF2 family 0x{requested:08X} is not present"
                    )));
                }
                Some(requested)
            }
            None if families.len() > 1 => {
                return Err(Error::Value(format!(
                    "UF2 contains {} families; select one with Uf2ParseOptions",
                    families.len()
                )));
            }
            None => families.first().copied(),
        };

        let mut image = MemorySegmentList::new();
        let mut blocks = Vec::new();
        let mut skipped_blocks = 0usize;
        let mut block_numbers = HashSet::new();
        let mut declared_block_count = None;
        for block in parsed_blocks {
            let info = block.info;
            let family_matches = match (selected_family, info.family_id) {
                (Some(selected), Some(actual)) => selected == actual,
                (Some(_), None) => options.family_id.is_none(),
                (None, _) => true,
            };
            let not_main_flash = info.flags & UF2_FLAG_NOT_MAIN_FLASH != 0;
            let is_file = info.flags & UF2_FLAG_FILE_CONTAINER != 0;
            if !family_matches || is_file || (not_main_flash && !options.include_not_main_flash) {
                skipped_blocks += 1;
                continue;
            }
            if info.block_number >= info.number_of_blocks {
                return Err(Error::DataFile {
                    line: info.block_number + 1,
                    message: "UF2 block number is outside the declared block count".to_string(),
                });
            }
            if info.payload_size == 0 {
                return Err(Error::DataFile {
                    line: info.block_number + 1,
                    message: "UF2 flash block has an empty payload".to_string(),
                });
            }
            if declared_block_count
                .replace(info.number_of_blocks)
                .is_some_and(|count| count != info.number_of_blocks)
            {
                return Err(Error::DataFile {
                    line: info.block_number + 1,
                    message: "inconsistent UF2 block count".to_string(),
                });
            }
            if !block_numbers.insert(info.block_number) {
                return Err(Error::DataFile {
                    line: info.block_number + 1,
                    message: "duplicate UF2 block number".to_string(),
                });
            }
            image.insert_segment(
                MemorySegment::from_data(
                    info.target_address as u64,
                    block.payload,
                    MemoryPrgType::DATA,
                    true,
                ),
                OverlapPolicy::Reject,
            )?;
            blocks.push(info);
        }
        if blocks.is_empty() {
            return Err(Error::DataFile {
                line: 0,
                message: "UF2 input contains no selected flash payload blocks".to_string(),
            });
        }

        let mut work = self.base.segment_list.clone();
        work.merge_from(
            &image,
            MergeOptions {
                overlap: OverlapPolicy::Overwrite,
                ..MergeOptions::default()
            },
        )?;
        blocks.sort_by_key(|block| block.block_number);
        self.base.segment_list = work;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        self.blocks = blocks;
        self.family_id = selected_family;
        self.skipped_blocks = skipped_blocks;
        Ok(())
    }

    pub fn write_uf2(&mut self, options: Uf2WriteOptions) -> Result<Vec<u8>> {
        if options.payload_size == 0 || options.payload_size > UF2_MAX_PAYLOAD_SIZE {
            return Err(Error::Value(format!(
                "UF2 payload size must be in 1..={UF2_MAX_PAYLOAD_SIZE}"
            )));
        }
        self.base.segment_list.validate_image()?;
        let mut pieces = Vec::new();
        let mut segments: Vec<&MemorySegment> = self
            .base
            .segment_list
            .segments
            .iter()
            .filter(|segment| segment.is_initialized() && segment.size() != 0)
            .collect();
        segments.sort_by_key(|segment| segment.address);
        for segment in segments {
            for (index, payload) in segment.data().chunks(options.payload_size).enumerate() {
                let offset = index.checked_mul(options.payload_size).ok_or_else(|| {
                    Error::Value("UF2 address offset overflows usize".to_string())
                })?;
                let address = segment
                    .address
                    .checked_add(offset as u64)
                    .ok_or_else(|| Error::Value("UF2 target address overflows u64".to_string()))?;
                let address = u32::try_from(address).map_err(|_| {
                    Error::Value("UF2 target address does not fit 32 bits".to_string())
                })?;
                pieces.push((address, payload));
            }
        }
        if pieces.is_empty() {
            return Err(Error::Value(
                "cannot write a UF2 file without initialized data".to_string(),
            ));
        }
        let number_of_blocks = u32::try_from(pieces.len())
            .map_err(|_| Error::Value("UF2 block count does not fit 32 bits".to_string()))?;
        let output_capacity = pieces
            .len()
            .checked_mul(UF2_BLOCK_SIZE)
            .ok_or_else(|| Error::Value("UF2 output size overflows usize".to_string()))?;
        let mut output = Vec::with_capacity(output_capacity);
        let mut blocks = Vec::with_capacity(pieces.len());
        for (index, (address, payload)) in pieces.into_iter().enumerate() {
            let block_number = index as u32;
            let flags = if options.family_id.is_some() {
                UF2_FLAG_FAMILY_ID_PRESENT
            } else {
                0
            };
            let family_or_size = options.family_id.unwrap_or(0);
            let mut block = [0u8; UF2_BLOCK_SIZE];
            put_u32(&mut block, 0, MAGIC_START_0);
            put_u32(&mut block, 4, MAGIC_START_1);
            put_u32(&mut block, 8, flags);
            put_u32(&mut block, 12, address);
            put_u32(&mut block, 16, payload.len() as u32);
            put_u32(&mut block, 20, block_number);
            put_u32(&mut block, 24, number_of_blocks);
            put_u32(&mut block, 28, family_or_size);
            block[32..32 + payload.len()].copy_from_slice(payload);
            put_u32(&mut block, 508, MAGIC_END);
            output.extend_from_slice(&block);
            blocks.push(Uf2BlockInfo {
                flags,
                target_address: address,
                payload_size: payload.len() as u32,
                block_number,
                number_of_blocks,
                family_id: options.family_id,
            });
        }
        self.blocks = blocks;
        self.family_id = options.family_id;
        self.skipped_blocks = 0;
        self.base.changed_values.clear();
        self.base.is_dirty = false;
        Ok(output)
    }

    pub fn save_file(&mut self, path: &Path, options: Uf2WriteOptions) -> Result<()> {
        let output = self.write_uf2(options)?;
        std::fs::write(path, output)?;
        self.base.source_filename = Some(path.to_string_lossy().into_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct ParsedBlock {
    info: Uf2BlockInfo,
    payload: Vec<u8>,
}

fn parse_blocks(data: &[u8]) -> Result<Vec<ParsedBlock>> {
    if data.is_empty() || !data.len().is_multiple_of(UF2_BLOCK_SIZE) {
        return Err(Error::DataFile {
            line: 0,
            message: "UF2 size must be a non-zero multiple of 512 bytes".to_string(),
        });
    }
    let mut result = Vec::with_capacity(data.len() / UF2_BLOCK_SIZE);
    for (index, block) in data.chunks_exact(UF2_BLOCK_SIZE).enumerate() {
        let line = index as u32 + 1;
        if get_u32(block, 0) != MAGIC_START_0
            || get_u32(block, 4) != MAGIC_START_1
            || get_u32(block, 508) != MAGIC_END
        {
            return Err(Error::DataFile {
                line,
                message: "invalid UF2 block magic".to_string(),
            });
        }
        let flags = get_u32(block, 8);
        let payload_size = get_u32(block, 16);
        if payload_size as usize > UF2_MAX_PAYLOAD_SIZE {
            return Err(Error::DataFile {
                line,
                message: format!("UF2 payload exceeds {UF2_MAX_PAYLOAD_SIZE} bytes"),
            });
        }
        let family_id = (flags & UF2_FLAG_FAMILY_ID_PRESENT != 0).then(|| get_u32(block, 28));
        result.push(ParsedBlock {
            info: Uf2BlockInfo {
                flags,
                target_address: get_u32(block, 12),
                payload_size,
                block_number: get_u32(block, 20),
                number_of_blocks: get_u32(block, 24),
                family_id,
            },
            payload: block[32..32 + payload_size as usize].to_vec(),
        });
    }
    Ok(result)
}

fn get_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn has_uf2_magic(data: &[u8]) -> bool {
    data.len() >= UF2_BLOCK_SIZE
        && get_u32(data, 0) == MAGIC_START_0
        && get_u32(data, 4) == MAGIC_START_1
        && get_u32(data, 508) == MAGIC_END
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::AddressRange;

    fn image() -> MemorySegmentList {
        MemorySegmentList {
            segments: vec![
                MemorySegment::from_data(0x1000, (0..=255).collect(), MemoryPrgType::DATA, true),
                MemorySegment::from_data(0x2000, vec![9, 8, 7], MemoryPrgType::DATA, true),
            ],
        }
    }

    #[test]
    fn canonical_uf2_round_trips_sparse_blocks_and_family() {
        let original = image();
        let mut file = DataFileUf2::new(None, original.clone());
        let bytes = file
            .write_uf2(Uf2WriteOptions {
                payload_size: 128,
                family_id: Some(0xE48B_FF56),
            })
            .unwrap();
        assert_eq!(bytes.len(), 3 * UF2_BLOCK_SIZE);
        assert!(has_uf2_magic(&bytes));

        let parsed = DataFileUf2::parse(&bytes, MemorySegmentList::new()).unwrap();
        assert_eq!(parsed.family_id, Some(0xE48B_FF56));
        assert_eq!(parsed.blocks.len(), 3);
        assert_eq!(parsed.base.segment_list, original);
    }

    #[test]
    fn rejects_bad_magic_payload_size_and_overlap() {
        let mut file = DataFileUf2::new(None, image());
        let mut bytes = file.write_uf2(Uf2WriteOptions::default()).unwrap();
        bytes[0] ^= 1;
        assert!(DataFileUf2::parse(&bytes, MemorySegmentList::new()).is_err());

        let mut bytes = file.write_uf2(Uf2WriteOptions::default()).unwrap();
        put_u32(&mut bytes, 16, 477);
        assert!(DataFileUf2::parse(&bytes, MemorySegmentList::new()).is_err());

        let mut bytes = file.write_uf2(Uf2WriteOptions::default()).unwrap();
        let second = UF2_BLOCK_SIZE;
        put_u32(&mut bytes, second + 12, 0x1000);
        assert!(DataFileUf2::parse(&bytes, MemorySegmentList::new()).is_err());
    }

    #[test]
    fn skips_not_main_flash_blocks_by_default() {
        let mut file = DataFileUf2::new(None, image());
        let mut bytes = file.write_uf2(Uf2WriteOptions::default()).unwrap();
        put_u32(&mut bytes, 8, UF2_FLAG_NOT_MAIN_FLASH);
        let parsed = DataFileUf2::parse(&bytes, MemorySegmentList::new()).unwrap();
        assert_eq!(parsed.skipped_blocks, 1);
        assert!(parsed
            .base
            .segment_list
            .read_exact(AddressRange::new(0x2000, 0x2003).unwrap())
            .is_ok());
        assert!(parsed
            .base
            .segment_list
            .read_exact(AddressRange::new(0x1000, 0x1080).unwrap())
            .is_err());
    }

    #[test]
    fn explicit_family_selection_handles_concatenated_files() {
        let mut first = DataFileUf2::new(
            None,
            MemorySegmentList {
                segments: vec![MemorySegment::from_data(
                    0x1000,
                    vec![1, 2],
                    MemoryPrgType::DATA,
                    true,
                )],
            },
        );
        let mut second = DataFileUf2::new(
            None,
            MemorySegmentList {
                segments: vec![MemorySegment::from_data(
                    0x2000,
                    vec![3, 4],
                    MemoryPrgType::DATA,
                    true,
                )],
            },
        );
        let mut bytes = first
            .write_uf2(Uf2WriteOptions {
                family_id: Some(1),
                ..Uf2WriteOptions::default()
            })
            .unwrap();
        bytes.extend(
            second
                .write_uf2(Uf2WriteOptions {
                    family_id: Some(2),
                    ..Uf2WriteOptions::default()
                })
                .unwrap(),
        );
        assert!(DataFileUf2::parse(&bytes, MemorySegmentList::new()).is_err());
        let selected = DataFileUf2::parse_with_options(
            &bytes,
            MemorySegmentList::new(),
            Uf2ParseOptions {
                family_id: Some(2),
                ..Uf2ParseOptions::default()
            },
        )
        .unwrap();
        assert_eq!(selected.family_id, Some(2));
        assert_eq!(
            selected
                .base
                .segment_list
                .read_exact(AddressRange::new(0x2000, 0x2002).unwrap())
                .unwrap(),
            [3, 4]
        );
    }
}
