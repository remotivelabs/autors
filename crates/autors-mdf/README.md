# autors-mdf

Reading and writing of ASAM MDF measurement files across the MDF 3.x and 4.x
families.

## Highlights

- `MdfReader::parse` and `MdfReader::open` detect the input version and expose
  the corresponding parsed file.
- `MdfReader::write` and `save` serialize an edited measurement file.
- `MdfWriter` creates channel groups, numeric or text-table measurements, raw
  samples, timestamps, annotations, and source information.
- MDF 3.x support includes the block structures and conversion data used by
  `.dat` and `.mdf` files.
- MDF 4.x support models ASAM MDF 4.10 blocks, absolute links, aligned layouts,
  data lists, and compressed data blocks used by `.mf4` files.
- Shared conversion and channel metadata make recorded values usable alongside
  A2L conversion definitions.

The low-level `v3` and `v4` modules are public for applications that need block
access; most callers can start with `MdfReader` or `MdfWriter` in `base`.

## Development

```text
cargo test -p autors-mdf
cargo doc -p autors-mdf --no-deps
```

See the [workspace README](../../README.md) for BLF, A2L, and formula support.
