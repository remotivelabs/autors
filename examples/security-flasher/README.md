# autors-security-flasher

This example is a Slint desktop host for configuration-driven ECU flashing. The
repository ships exactly one public configuration, `Conf/ExampleCar`, but the
host is not tied to that flow. Each configuration selects three independent
dynamic libraries:

- `Flow.DllPath` implements the diagnostic sequence.
- `Loader.DllPath` parses separate images or extracts components from a package.
- `SeedKey.DllPath` provides the Seed & Key calculation.

The host discovers configurations from `Conf/<name>/<name>.toml`, validates all
paths, opens the selected CAN adapter, and supplies transport, image, progress,
cancellation, and Seed & Key callbacks to the configured Flow DLL. Flow and
FileLoader plugins use the versioned C ABI documented by the
[`autors-security-flasher-sdk`](../security-flasher-sdk/README.md). SeedKey plugins use the native
`GenerateKeyEx` boundary supported by `autors-native`.

The bundled configuration is deliberately marked `DemoOnly = true`. It can
only connect to the in-process virtual ECU. Its firmware, flash driver,
signature function, and Seed & Key implementation are synthetic examples and
contain no production secrets.

## Build and run

From the workspace root:

```powershell
cargo build -p examplecar-fileloader -p examplecar-flow -p examplecar-seedkey
cargo run -p autors-security-flasher
```

Cargo places the example DLLs beside the executable in `target/debug`. For a
packaged deployment, place each configured DLL either beside the executable or
inside its configuration directory.

In the UI, keep `ExampleCar`, scan the `Virtual ECU`, connect, and start with
the preselected `SampleApplication.hex` image.

## Add another configuration outside the public example

Create `Conf/<name>/<name>.toml` and its resources in a private deployment. The
host discovers it without source changes. Set the three `DllPath` values to the
desired plugins and choose `Loader.Mode = "separate"` or `"package"`. In
separate mode, add a `FlashDriver` section containing the image path and load
address. CAN IDs, bit rates, timing, padding, and transfer block size are also
configuration values.

A replacement Flow or FileLoader DLL must export the corresponding versioned
entry symbol declared by the SDK. The ABI intentionally contains only
C-compatible data and synchronous callbacks, so plugins can be implemented in
Rust or another language capable of exporting a native C ABI. A replacement
SeedKey DLL must export `GenerateKeyEx` with the signature demonstrated by the
example plugin.

Do not add confidential firmware, keys, signatures, or internal configuration
folders to the public repository.

## Verification

Build the three example DLLs before running the ignored end-to-end test:

```powershell
cargo build -p examplecar-fileloader -p examplecar-flow -p examplecar-seedkey
cargo test -p autors-security-flasher configured_plugins_complete_a_virtual_flash -- --ignored
```

The test loads all three library paths from `ExampleCar.toml` and completes the
full flow against the virtual ECU.

See the [workspace README](../../README.md) for the libraries used by this
example and the READMEs beside each example plugin for their individual scope.
