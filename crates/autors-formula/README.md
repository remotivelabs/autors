# autors-formula

Evaluation of A2L conversion formulas and automotive checksum algorithms.

## Highlights

- `A2LFormula` parses forward formulas and optional inverse formulas, including
  named system constants.
- `FormulaDict` builds and indexes a set of named formulas with configurable
  strict parsing.
- `RationalCoeffsEval` evaluates A2L rational conversions in both directions.
- `TabCoeffs` handles numeric table interpolation and reverse lookup.
- `Checksum` provides CRC-32, CRC-16, CRC-16/CCITT, and the checksum dispatcher
  used by flashing and calibration workflows.

This crate provides conversion algorithms. Runtime characteristic shapes and
ECU-memory layout handling live in `autors-values`.

## Development

```text
cargo test -p autors-formula
cargo doc -p autors-formula --no-deps
```

See the [workspace README](../../README.md) for the complete calibration stack.
