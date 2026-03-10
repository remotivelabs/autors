# autors-cdf

ASAM CDF (Calibration Data Format) XML reading, writing, and object modeling.

## Highlights

- `CdfFile::parse_str` and `CdfFile::load` deserialize CDF XML into `Msrsw`.
- `CdfFile::write_string` and `CdfFile::save` serialize edited documents.
- Typed structures represent systems, instance specifications, instance trees,
  calibration instances, axes, collections, and physical values.
- Constructors for the main container types make it possible to create CDF
  documents as well as consume them.
- `Msrsw::all_instances` provides a convenient flattened view of calibration
  instances across systems and trees.

The crate handles CDF XML itself. Mapping those instances to A2L runtime values
is provided by `autors-values::cdf_link`.

## Development

```text
cargo test -p autors-cdf
cargo doc -p autors-cdf --no-deps
```

See the [workspace README](../../README.md) for the full calibration stack.
