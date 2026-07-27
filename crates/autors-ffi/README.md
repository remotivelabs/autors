# autors-ffi

A panic-safe C ABI for the `autors-a2l` project model and `autors-values`
conversion functions.

## Capabilities

- Create, parse, serialize, save, and free A2L project handles.
- Enumerate modules and measurements, and look up measurements or
  characteristics by name.
- Read names, descriptions, addresses, data types, conversions, and record
  layouts through opaque borrowed handles.
- Convert measurement values between raw and physical representations.
- Update measurement addresses.
- Retrieve a thread-local error message after a failed call.

Every exported entry point catches Rust panics. Return code `0` means success;
other codes distinguish invalid arguments, missing objects, parse failures, and
internal errors. Strings returned as owned pointers must be released with
`autors_string_free`. Borrowed measurement and characteristic handles become
invalid when their owning project is freed or structurally modified.

## Generated and example artifacts

- [`include/autors.h`](include/autors.h) is generated with `cbindgen`.
- [`examples/c_demo.c`](examples/c_demo.c) demonstrates the C API and is built
  by the native smoke test.
- [`examples/AutorsPInvoke.cs`](examples/AutorsPInvoke.cs) demonstrates C#
  P/Invoke ownership and error handling.
- ABI tests verify the generated header and both examples.

## Development

```text
cargo test -p autors-ffi
cargo build -p autors-ffi
```

See the [workspace README](../../README.md) for the native Rust APIs.
