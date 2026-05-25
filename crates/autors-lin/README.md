# autors-lin

Hardware-independent LIN bus support with typed frames, configuration,
checksums, logging queues, and optional vendor adapters.

## Highlights

- `LinDevice` defines the async device contract for open, send, request,
  receive, and close operations.
- `LinConfiguration` models channel, LIN version, baud rate, role, checksum,
  and scheduling-related settings.
- `LinFrame` handles protected identifiers, classic/enhanced checksums,
  timestamps, direction, and payload presentation.
- `LinFrameQueue` provides bounded frame collection and optional send logging.
- `BlockingDevice` exposes a synchronous wrapper over any `LinDevice`.

## Features

| Feature | Effect |
| --- | --- |
| `vendor-kvaser` | Enables the Kvaser adapter. |
| `vendor-peak` | Enables the PEAK adapter. |
| `vendor-vector` | Enables the Vector adapter. |
| `all-vendors` | Enables all three vendor adapters. |
| `runtime-tokio` | Uses Tokio-backed runtime services. |
| `blocking` | Exposes the synchronous facade. |

Vendor adapters are disabled by default and require the corresponding native
driver at runtime.

## Development

```text
cargo test -p autors-lin
cargo check -p autors-lin --all-features
```

See the [workspace README](../../README.md) for LDF and LIN diagnostic transport.
