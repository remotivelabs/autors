# autors-values

Runtime values for A2L characteristics, including conversion, editing,
ECU-memory layout, and CDF exchange.

## Highlights

- `CharValue`, `BaseValue`, and `ValueData` model scalar, text, curve, map,
  cube, value-block, and axis values.
- `Conversion` applies A2L formula, rational, numeric table, verbal table, and
  range-table conversions between raw and physical values.
- Formatting helpers produce decimal, hexadecimal, binary, and A2L-style value
  text.
- Editing APIs support indexed access, increments, arithmetic modifications,
  comparisons, and clipboard-style scalar/row/block pasting.
- `ecu_io` calculates record-layout offsets and reads or writes values in ECU
  memory buffers with the correct type, byte order, axes, and alignment.
- `cdf_link` imports CDF instances and exports runtime values with configurable
  metadata.
- Memory-range and measurement-access helpers collect sparse ECU data safely.

## Development

```text
cargo test -p autors-values
cargo doc -p autors-values --no-deps
```

See the [workspace README](../../README.md) for A2L, CDF, DCM, and data-file
support.
