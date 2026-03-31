# autors-datafile

`autors-datafile` is a library for inspecting, editing, comparing, and
converting sparse ECU program images. Its address-space operations are
format-independent, so the same workflow can be used for Intel HEX, Motorola
S-record, VBF, TI-TXT, UF2, hexadecimal ASCII, and binary inputs.

## Format support

| Format | Detect | Read | Write | Notes |
| --- | --- | --- | --- | --- |
| Intel HEX | Yes | Yes | Yes | Extended segment and extended linear addresses |
| Motorola S-record | Yes | Yes | Yes | S1, S2, and S3 data records |
| Mixed Intel HEX/S-record | Yes | Yes | No | Rejects ambiguous overlapping records |
| Hexadecimal ASCII | By explicit extension | Yes | Yes | One- or two-digit tokens and uninterrupted byte pairs |
| Raw binary | Fallback/signature | Yes | Yes | Optional A2L/EPK-based address mapping |
| Ford/Volvo VBF 2.x | Yes | Yes | Yes | Raw blocks, block CRC-16 and file CRC-32; compressed/encrypted blocks are rejected |
| Ford I-HEX container | Yes | Yes | No | Preserves OEM header fields and parses the Intel HEX payload |
| Texas Instruments TI-TXT | Yes | Yes | Yes | Multiple addressed sections, canonical 16-byte lines, mandatory terminator |
| Microsoft UF2 | Yes | Yes | Yes | 512-byte blocks, family IDs, concatenated-family selection, non-main-flash filtering |
| GM, Fiat PRM, GAC, Chery CBF | Yes | No | No | Recognized explicitly instead of being misread as raw BIN |

Additional exports include concatenated or gap-filled binary data, one binary
file per address block, sparse C arrays with an address descriptor table, and
RFC 4648 Base64.

The TI-TXT reader follows the [Texas Instruments object-format definition](https://downloads.ti.com/docs/esd/SLAU131T/ti-txt-hex-format-ti-txt-option-stdz0795656.html).
UF2 block layout and flag handling follow the [Microsoft UF2 specification](https://microsoft.github.io/uf2/).

## Image operations

- Content-first automatic format detection with confidence and evidence.
- Exact or gap-filled reads over half-open 64-bit address ranges.
- Transactional writes and merges with reject, preserve, or overwrite policies.
- Address-range clipping, signed relocation, erase, repeating-pattern fill,
  block alignment, block splitting, and remapping.
- Byte and ASCII-pattern search across adjacent blocks without crossing holes.
- Exact, full-byte wildcard, and nibble-wildcard pattern search with range,
  alignment, overlap, and result-limit controls; atomic same-size replacement
  preserves segment metadata.
- Sparse image comparison returning changed, left-only, and right-only spans.
- Image summaries with byte counts, bounds, and hole ranges.
- XOR, byte/word sums, CRC-16/CCITT-FALSE, and CRC-32/ISO-HDLC with selectable
  ranges, exclusions, gap fill, output byte order, and atomic insertion.
- HexView-style range strings such as `0x190,0x20:0x9020-0x903f`.
- Extensible transactional data processors through the `DataProcessor` trait,
  including repeating XOR/invert, word or longword byte swaps, and bounded
  ARLE compression/decompression.
- Bounded `EditSession` history with named atomic edits, undo/redo, dirty
  checkpoints, branch invalidation, and commit back to `DataFileBase`.

## Example

```rust,no_run
use std::path::Path;

use autors_datafile::{
    ChecksumAlgorithm, ChecksumOptions, DataFile, MemorySegmentList,
};

fn main() -> autors_datafile::Result<()> {
    let (image, detected) = DataFile::open_auto(
        Path::new("firmware.vbf"),
        MemorySegmentList::new(),
        0,
        None,
    )?;
    let summary = image.base().segment_list.image_summary()?;
    let crc = image
        .base()
        .checksum(ChecksumAlgorithm::Crc32IsoHdlc, &ChecksumOptions::default())?;

    println!(
        "{} ({:?}): {} block(s), {} byte(s), CRC32={:08X}",
        detected.format.name(),
        detected.confidence,
        summary.segment_count,
        summary.byte_count,
        crc.value,
    );
    Ok(())
}
```

See [`examples/inspect.rs`](examples/inspect.rs) for a complete command-line
inspection example and [`examples/edit.rs`](examples/edit.rs) for grouped data
processing with undo and redo.

## Current boundaries

VBF compression, encryption, signatures, and verification structures are
preserved as header values but are not transformed. OEM-specific GM, Fiat,
GAC, and CBF container payloads are detected but not parsed yet. Ford I-HEX is
intentionally read-only until all mandatory OEM checksums can be regenerated
safely.
The crate provides the library layer; a GUI, native DLL plug-in ABI, and
flash-trace reconstruction are separate future layers. The Rust
`DataProcessor` trait is the in-process extension point.

## Development

```text
cargo test -p autors-datafile
cargo clippy -p autors-datafile --all-targets -- -D warnings
cargo doc -p autors-datafile --no-deps
```

See the [workspace README](../../README.md) for the rest of the automotive
toolkit.
