# autors-elf

ELF and DWARF parsing for calibration-oriented symbol discovery and target data
access.

## Highlights

- Parse ELF headers, sections, symbol tables, machine types, bindings, and data
  endianness with `ElfFile`.
- Read initialized bytes from addressable sections with `ElfFile::get_data`.
- Parse DWARF compilation units, attributes, types, arrays, structures, and
  variables into a navigable symbol tree.
- Expand typed symbols and produce `UpdaterSymbols` values for A2L address
  synchronization.
- Open from a path or an in-memory byte vector, with DWARF processing selected
  explicitly.

The crate focuses on parsing. Shared matching and A2L write-back are provided by
`autors-symbols`.

## Development

```text
cargo test -p autors-elf
cargo doc -p autors-elf --no-deps
```

See the [workspace README](../../README.md) for related symbol and A2L crates.
