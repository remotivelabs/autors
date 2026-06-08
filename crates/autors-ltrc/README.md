# autors-ltrc

Reader for PEAK PLIN-View Pro LIN trace (`.ltrc`) files.

## Highlights

- Supports the documented LTRC 1.0, 1.1, and 1.2 formats.
- Parses publisher, subscriber, and auto-length subscriber frames.
- Retains unavailable (`--`) data bytes, checksum information, and all defined
  frame error codes.
- Parses the bus sleep, bus wake-up, queue overrun, and overrun events added in
  format 1.2.
- Converts complete trace frames to the shared `autors-lin::LinFrame` type.
- Serializes and saves the typed trace model back to PLIN-View-compatible LTRC,
  including unavailable subscriber bytes, checksums, errors, and bus events.

## Development

```text
cargo test -p autors-ltrc
cargo clippy -p autors-ltrc --all-targets -- -D warnings
```
