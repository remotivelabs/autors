# autors-symbols

Shared symbol-to-A2L address-update logic. File-specific parsers produce the
symbol records defined here, and this crate resolves them against editable A2L
objects.

## Highlights

- Parse hierarchical symbol paths, array indexes, and member selectors with
  `parse_symbol_path`.
- Represent ELF and MAP candidates uniformly with `UpdaterSymbols`.
- Resolve address-bearing A2L nodes through `AddressNodeResolver`.
- Describe proposed matches with `UpdateData` and `UpdateType`.
- Apply selected changes and write the resulting A2L through
  `update_and_write_a2l`.

ELF/DWARF parsing lives in `autors-elf`; linker MAP parsing lives in
`autors-map`. Keeping matching and write-back here gives both formats one update
pipeline.

## Development

```text
cargo test -p autors-symbols
cargo bench -p autors-symbols --bench address_resolution
```

See the [workspace README](../../README.md) for the complete symbol toolchain.
