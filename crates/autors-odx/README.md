# autors-odx

ODX (Open Diagnostic Data Exchange) parsing, editing, reference resolution, and
flash-container support.

## Highlights

- `OdxFile::parse_str`, `open`, `write_string`, and `save` provide single-file
  XML I/O.
- The object model covers diagnostic layers, variants, services, parameters,
  data object properties, communication parameters, DTCs, vehicle information,
  and flash memory descriptions.
- `OdxRoot` builds ID indexes, resolves references, merges inherited layers,
  inspects protocols and DTCs, and analyzes diagnostic services.
- `OdxDocumentSet` loads linked documents from directories or PDX archives and
  resolves cross-file `DOCREF` references on demand.
- `PdxPackage` exposes package members and the `index.xml` catalog.
- Parser events and failed-reference tracking preserve useful diagnostics for
  imperfect data sets.

## Development

```text
cargo test -p autors-odx
cargo doc -p autors-odx --no-deps
```

See the [workspace README](../../README.md) for UDS, DoIP, and flashing crates.
