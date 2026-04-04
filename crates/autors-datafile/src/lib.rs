//! Sparse ECU memory-image inspection, editing, comparison, and conversion.
//!
//! Supported inputs include Intel HEX, Motorola S-record, mixed HEX/S-record,
//! hexadecimal ASCII, TI-TXT, UF2, raw binary, and raw-block Ford/Volvo VBF
//! 2.x. Content-
//! first detection is available through [`datafile::DataFile::open_auto`] and
//! [`datafile::DataFile::parse_auto`]. Recognized but unsupported OEM
//! containers return [`Error::UnsupportedFormat`] instead of being silently
//! interpreted as raw bytes.
//!
//! [`datafile::MemorySegmentList`] is the common sparse representation.
//! [`AddressRange`] and its image methods provide exact/gap-filled reads,
//! search, comparison, transactional merge policies, erase, fill, alignment,
//! splitting, and relocation. [`calculate_checksum`] and the export functions
//! operate on that same representation. [`DataProcessor`] provides atomic
//! byte transformations, while [`EditSession`] groups edits into bounded undo
//! and redo history.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use autors_datafile::{
//!     ChecksumAlgorithm, ChecksumOptions,
//!     datafile::{DataFile, MemorySegmentList},
//! };
//!
//! # fn main() -> autors_datafile::Result<()> {
//! let (file, detected) = DataFile::open_auto(
//!     Path::new("firmware.vbf"),
//!     MemorySegmentList::new(),
//!     0,
//!     None,
//! )?;
//! let summary = file.base().segment_list.image_summary()?;
//! let crc = file.base().checksum(
//!     ChecksumAlgorithm::Crc32IsoHdlc,
//!     &ChecksumOptions::default(),
//! )?;
//! println!("{}: {} bytes, CRC={:08X}", detected.format.name(), summary.byte_count, crc.value);
//! # Ok(())
//! # }
//! ```

pub mod ascii_hex;
pub mod checksum;
pub mod datafile;
pub mod error;
pub mod export;
pub mod ford_ihex;
pub mod format;
pub mod history;
pub mod image;
pub mod mixed;
pub mod processing;
pub mod search;
pub mod ti_txt;
pub mod uf2;
pub mod vbf;

pub use ascii_hex::{DataFileHexAscii, HexAsciiOptions};
pub use checksum::{
    calculate_checksum, ByteOrder, ChecksumAlgorithm, ChecksumOptions, ChecksumValue,
};
pub use datafile::{DataFile, DataFileBase, DataFileType, MemorySegment, MemorySegmentList};
pub use error::{Error, Result};
pub use export::{
    export_base64, export_binary, export_binary_blocks, export_c_arrays, BinaryBlock,
    BinaryExportOptions, CArrayOptions,
};
pub use ford_ihex::{DataFileFordIHex, FordIHexHeader, FordIHexHeaderField};
pub use format::{detect_format, DataFormat, DetectionConfidence, FormatDetection};
pub use history::{EditOutcome, EditSession, HistoryEvent};
pub use image::{
    parse_address_ranges, AddressRange, DifferenceKind, ImageDifference, ImageSummary,
    MergeOptions, MergeReport, OverlapPolicy,
};
pub use mixed::DataFileMixed;
pub use processing::{
    arle_compress, arle_decompress, ArleCompressor, ArleDecompressor, DataProcessor, NoopProcessor,
    ProcessingOptions, ProcessingReport, SwapProcessor, XorProcessor,
};
pub use search::{BytePattern, ReplaceReport, SearchOptions};
pub use ti_txt::DataFileTiTxt;
pub use uf2::{
    DataFileUf2, Uf2BlockInfo, Uf2ParseOptions, Uf2WriteOptions, UF2_BLOCK_SIZE,
    UF2_FLAG_EXTENSION_TAGS_PRESENT, UF2_FLAG_FAMILY_ID_PRESENT, UF2_FLAG_FILE_CONTAINER,
    UF2_FLAG_MD5_PRESENT, UF2_FLAG_NOT_MAIN_FLASH, UF2_MAX_PAYLOAD_SIZE,
};
pub use vbf::{
    DataFileVbf, VbfBlockInfo, VbfHeader, VbfHeaderField, VbfParseOptions, VbfWriteOptions,
};
