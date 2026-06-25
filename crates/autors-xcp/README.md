# autors-xcp

A hardware-agnostic XCP master, transport codecs, and typed A2L `IF_DATA` model.

## Highlights

- `XcpMaster` and `XcpMasterBase` implement calibration, memory upload/download,
  DAQ, clock, checksum, Seed & Key, and programming operations.
- Transports cover XCP on CAN, UDP, TCP, and SxI serial framing; external code
  can implement the open `XcpTransport` trait for additional media.
- More than forty typed commands and their response types encode byte order and
  protocol fields explicitly, including selected XCP-on-CAN 1.5 operations.
- `XcpReceiveBuffer` handles transport headers, alignment, counters, and frame
  reassembly.
- `XcpIfData` parses and writes typed XCP/XCPplus protocol, DAQ, PAG, PGM,
  event, timing, media, and endpoint blocks while preserving generic nodes.
- Blocking wrappers are available for transports, the base master, and the
  high-level master.

Callbacks raised during a mutable exchange are queued and delivered afterward
by the master, preventing re-entrant mutable access while preserving order.
Receive processing remains cooperative through `poll`.

## Features and development

The default feature set enables `runtime-tokio` and `blocking`.

```text
cargo test -p autors-xcp
cargo check -p autors-xcp --no-default-features
```

See the [workspace README](../../README.md) for CAN, A2L, and shared DAQ support.
