# autors-ccp

A CAN Calibration Protocol master with typed command/response codecs, DAQ,
programming operations, and A2L `IF_DATA ASAP1B_CCP` support.

## Highlights

- `CcpMaster<D: CanDevice>` provides connect/disconnect, upload/download,
  memory transfer, calibration pages, checksums, Seed & Key, DAQ setup, and
  programming workflows over an injected CAN device.
- Typed command and response structures expose the CCP wire format without raw
  indexing in application code.
- `CcpFrame` provides protocol-aware formatting and DAQ/error classification.
- `CcpIfData` parses and writes CCP transport blobs, DAQ/raster data, memory
  pages, checksum settings, and unknown A2L blocks.
- `BlockingCcpMaster` mirrors the async master through the shared runtime.
- Polling remains cooperative through `CcpMaster::poll`.

The default feature set contains `runtime-tokio` and `blocking`; either can be
selected independently for a narrower build.

## Development

```text
cargo test -p autors-ccp
cargo check -p autors-ccp --no-default-features
```

See the [workspace README](../../README.md) for A2L, CAN, and shared DAQ support.
