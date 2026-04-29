# autors-map

Linker MAP file parsing for symbol lookup and A2L address synchronization.

## Highlights

- `MapFile::open` reads a MAP file from disk.
- `MapFile::open_str` parses in-memory text and accepts optional source
  metadata.
- `MapSymbolValue` stores parsed symbol information in a format suitable for
  matching.
- `get_values_to_synchronize` compares the symbol table with an A2L project and
  returns shared update records from `autors-symbols`.

This crate owns MAP-specific parsing only. Symbol path resolution, update
selection, and A2L write-back remain centralized in `autors-symbols`.

## Development

```text
cargo test -p autors-map
cargo bench -p autors-map --bench map_parsing
```

See the [workspace README](../../README.md) for the complete symbol toolchain.
