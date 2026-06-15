# autors-comm

Protocol-independent communication and data-acquisition primitives shared by
CCP, XCP, and other ECU clients.

## Highlights

- `CommMaster` stores common connection state, timeouts, callbacks, frames,
  DAQ configuration, and Seed & Key integration.
- `DaqList`, `OdtList`, `OdtEntry`, `DaqDict`, and `DaqValueBuffer` model DAQ
  layouts and sampled values independently of the wire protocol.
- `MeasurementInfo` interprets A2L data types, byte order, bit operations, array
  offsets, and physical conversion.
- `DaqCache` reconstructs multi-part acquisition payloads.
- `CommKernel::poll_clients` cooperatively advances registered masters; no
  implicit receive thread is started.
- `BlockingCommKernel` drives the same polling path synchronously.

The default features enable Tokio-backed runtime services and the blocking
facade. Use `--no-default-features` for an async API with the standard-thread
runtime fallback.

## Development

```text
cargo test -p autors-comm
cargo bench -p autors-comm --bench daq_configuration
```

See the [workspace README](../../README.md) for the CCP and XCP implementations.
