# autors-isotp

Diagnostic transport for ISO-TP over CAN (ISO 15765-2) and segmented LIN
diagnostics (ISO 17987-2).

## Highlights

- `IsoTpFsm` is a pure byte-level CAN segmentation/reassembly state machine for
  single, first, consecutive, and flow-control frames.
- `IsoTp` connects the state machine to any `autors-can::CanDevice`, including
  timeout, abort, addressing, fill-byte, and CAN FD behavior.
- `LinTpPdu` encodes and decodes the fixed eight-byte LIN diagnostic frames.
- `LinTpFsm` and `LinTp` implement LIN message segmentation, scheduling, and
  response collection over any `autors-lin::LinDevice`.
- `BlockingIsoTp` and `BlockingLinTp` provide synchronous wrappers.

The default features enable `runtime-tokio` and `blocking`. Async-only and
standard-thread fallback builds are available by disabling default features.

## Development

```text
cargo test -p autors-isotp
cargo check -p autors-isotp --no-default-features
```

See the [workspace README](../../README.md) for CAN, LIN, UDS, and KWP layers.
