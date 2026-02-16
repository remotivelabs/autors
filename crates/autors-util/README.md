# autors-util

Safe utility building blocks shared by the `autors` workspace. This crate
forbids unsafe code and collects reusable algorithms that do not belong to a
specific automotive format or protocol.

## Highlights

- Endian conversion, alignment, bit masks, binary extraction, and numeric text
  formatting.
- Adler-32 computation and byte/string conversion helpers.
- Data types, limits, data points, parser events, progress values, and compact
  memory-range management.
- XML namespace cleanup and invariant text-file writing.
- Process-relative timing and DAQ time tracking.
- A lightweight logging facade built on the `log` crate.

The main APIs live in `helpers` and `logging`. Frequently used types include
`Adler32Computer`, `BitOperation`, `DataType`, `MemoryRangeList`, `TimeBase`,
`Logger`, and `LogManager`.

## Development

```text
cargo test -p autors-util
cargo doc -p autors-util --no-deps
```

See the [workspace README](../../README.md) for the complete library map.
