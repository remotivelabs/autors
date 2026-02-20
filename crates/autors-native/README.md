# autors-native

Native dynamic-library support for integrations that must cross an operating
system ABI boundary.

## Highlights

- `DllWrapper` loads a shared library with `libloading` and retains its source
  path and optional version metadata.
- `NativeSkDll` loads Seed & Key providers on Windows and supports the CCP, XCP,
  and UDS calculation entry points used by the protocol crates.
- `SkType`, `ResultSk`, and `VKeyGenResultEx` model the native provider contract
  without leaking raw status values into callers.
- Missing libraries and symbols are returned through the crate's typed `Error`.

`NativeSkDll` is Windows-only; the general dynamic-library wrapper and shared
Seed & Key types remain available on other targets.

## Development

```text
cargo test -p autors-native
cargo doc -p autors-native --no-deps
```

See the [workspace README](../../README.md) for related protocol and FFI crates.
