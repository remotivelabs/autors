use std::env;
use std::path::Path;

use autors_datafile::{ChecksumAlgorithm, ChecksumOptions, DataFile, MemorySegmentList};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filename = env::args().nth(1).ok_or("usage: inspect <data-file>")?;
    let (file, detected) =
        DataFile::open_auto(Path::new(&filename), MemorySegmentList::new(), 0, None)?;
    let summary = file.base().segment_list.image_summary()?;
    let crc = file
        .base()
        .checksum(ChecksumAlgorithm::Crc32IsoHdlc, &ChecksumOptions::default())?;

    println!("format: {}", detected.format.name());
    println!(
        "confidence: {:?} ({})",
        detected.confidence, detected.reason
    );
    println!("segments: {}", summary.segment_count);
    println!("initialized bytes: {}", summary.byte_count);
    if let Some(range) = summary.address_range {
        println!("address range: 0x{:X}..0x{:X}", range.start, range.end);
    }
    println!("holes: {}", summary.holes.len());
    println!("CRC-32/ISO-HDLC: 0x{:08X}", crc.value);
    Ok(())
}
