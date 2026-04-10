# autors-dcm

Calibration conservation data for DCM, MATLAB `.m`, and CANape PAR files.

## Highlights

- `DcmFile`, `MatlabFile`, and `ParFile` expose path-based and in-memory parsing
  and writing APIs.
- `DataConservation` stores imported calibration values, metadata, and parse
  diagnostics in one format-independent container.
- `ModuleRefs` indexes the A2L module objects needed to interpret
  characteristics, axes, record layouts, conversions, functions, and EPK data.
- `ConservationValue` formats and converts scalar, curve, map, and axis values
  against their A2L definitions.
- Ordered storage keeps emitted calibration values predictable.

Parsing is A2L-aware: construct `ModuleRefs` for the target module and pass it
to the selected format handler.

## Development

```text
cargo test -p autors-dcm
cargo doc -p autors-dcm --no-deps
```

See the [workspace README](../../README.md) for related A2L, CDF, and value
crates.
