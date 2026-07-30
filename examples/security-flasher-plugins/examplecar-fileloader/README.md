# examplecar-fileloader

A public, non-production file-loader plugin for the `autors` Security Flasher
example.

## What it demonstrates

- Exports the versioned `autors_security_file_loader_v1` entry symbol.
- Validates the host's ABI version and reports failures through the supplied
  `SegmentSinkV1` callbacks.
- Loads Intel HEX and Motorola S-record images through `autors-datafile`.
- Loads raw `.bin` files at the base address supplied by the configuration.
- Streams initialized address/data segments back to the host without retaining
  host-owned pointers.

The advertised extensions are `.hex`, `.s19`, `.s28`, `.s37`, `.srec`, `.mot`,
and `.bin`. This example supports `LOADER_MODE_SEPARATE` components only; it is
not a package/archive loader.

## Build and test

From the workspace root:

```text
cargo build -p examplecar-fileloader
cargo test -p examplecar-fileloader
```

The resulting dynamic library is loaded by the path in the selected flasher
configuration. See the [SDK README](../../security-flasher-sdk/README.md) for
the ABI and the [workspace README](../../../README.md) for the full example.
