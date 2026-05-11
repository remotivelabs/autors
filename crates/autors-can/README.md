# autors-can

A cross-platform CAN/CAN FD hardware abstraction with shared frames,
configuration, listener dispatch, and optional device backends.

## Highlights

- `CanDevice` defines async open, close, send, receive, polling, channel
  discovery, and listener behavior.
- `CanFrame` supports classic CAN and CAN FD payloads, 11-bit and 29-bit IDs,
  timestamps, CSV/clipboard formatting, and J1939 views.
- `CanConfiguration` models predefined or custom nominal/data-phase bit rates,
  channels, filters, serial settings, and CAN standards.
- `DeviceCore` supplies unique bus IDs, listener registration, dispatch, and
  bus-load statistics to backend implementations.
- Reception is cooperative through `poll_once`; `start_dispatch` is an explicit
  opt-in background helper.
- `BlockingDevice` exposes synchronous methods over the same async core.

## Backends and features

- `socketcan` enables the Linux SocketCAN backend and is enabled by default on
  Linux targets.
- Seventeen `vendor-*` features select adapters for Advantech, CAN Analyst,
  EB/EL, 8devices, ELM327, ESD, ETAS, I+ME ACTIA, Intrepid, IXXAT, Kvaser,
  LAWICEL, MHS, NI, PEAK, TOSUN, and Vector hardware.
- `all-vendors` enables every vendor adapter.
- `runtime-tokio` and `blocking` select runtime services and the synchronous
  facade; both are in the default feature set.

Vendor drivers are loaded at runtime where applicable, so compiling an adapter
does not guarantee that its native driver or hardware is installed.

## Development

```text
cargo test -p autors-can
cargo check -p autors-can --all-features
```

See the [workspace README](../../README.md) for DBC, BLF, ISO-TP, and protocol
layers.
