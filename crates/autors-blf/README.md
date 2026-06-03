# autors-blf

Reading, editing, and writing Vector BLF (Binary Logging Format) files.

## Format coverage

- Buffered APIs (`BlfFile`) and incremental APIs (`BlfReader`/`BlfWriter`).
- Compressed (zlib, method 2) and uncompressed (method 0) log containers,
  including objects split across container boundaries.
- The 72-byte minimum file header, the common 144-byte header, extended file
  headers, and version-1/version-2 object headers.
- Every registered object ID from 0 through 131. CAN and CAN FD are fully
  typed; LIN data frames, Ethernet frames/status/errors/statistics, application
  metadata, environment/system variables, GPS, diagnostics, triggers, data-loss
  events, and restore points also have typed representations.
- Registered objects without a family-specific model use `BlfObject::Raw`.
  Future numeric IDs use `BlfObject::Unknown`. Both retain their complete
  header and payload and can be written back without data loss.
- Resource limits for headers, containers, decompression, and objects when
  reading untrusted files.
- Conversion to the shared `autors-can::CanFrame` and `autors-lin::LinFrame`
  types, channel/ID filters, object statistics, and timestamp conversion.

The BLF specification is not publicly published. Compatibility is therefore
defined against observed files and the public Vector BLF reference
implementation. The repository's `blf_inspect` example can recursively verify
external fixture collections by parsing and logically round-tripping them.

## Example

```rust,no_run
use std::fs::File;
use std::io::{BufReader, BufWriter};

use autors_blf::{BlfReader, BlfWriter, Result};

fn copy_blf(source: &str, destination: &str) -> Result<()> {
    let mut reader = BlfReader::new(BufReader::new(File::open(source)?))?;
    let header = reader.header().clone();
    let mut writer = BlfWriter::new(BufWriter::new(File::create(destination)?), header)?;
    for object in &mut reader {
        writer.write_object(&object?)?;
    }
    writer.finish()?;
    Ok(())
}
```

## Development

```text
cargo test -p autors-blf
cargo clippy -p autors-blf --all-targets --no-deps -- -D warnings
cargo doc -p autors-blf --no-deps
cargo run -p autors-blf --example blf_inspect -- <fixture-directory>
```

See the [workspace README](../../README.md) for CAN access and other measurement
formats.
