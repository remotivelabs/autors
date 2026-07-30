# examplecar-seedkey

A deliberately non-production Seed & Key plugin for the public `ExampleCar`
flashing demonstration.

## What it demonstrates

- Exports the native `GenerateKeyEx` symbol consumed by `autors-native`.
- Validates the security level, seed length, output buffer, and required out
  parameter.
- Returns clear native status codes, including the required output size when a
  buffer is too small.
- Prevents Rust panics from crossing the ABI boundary.
- Produces the 32-byte record expected by the bundled virtual ECU from a
  16-byte seed at example security level `0x11`.

## Security warning

The transformation is intentionally transparent and contains no production
key material. It exists only to exercise dynamic loading and ABI handling. Do
not use it to protect a real ECU and do not place confidential algorithms or
keys in this public example.

## Build and test

```text
cargo build -p examplecar-seedkey
cargo test -p examplecar-seedkey
```

See the [Security Flasher README](../../security-flasher/README.md) for the host
workflow and the [workspace README](../../../README.md) for the full library.
