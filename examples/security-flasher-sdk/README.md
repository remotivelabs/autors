# autors-security-flasher-sdk

The versioned C ABI shared by the `autors` Security Flasher host and its flow
and file-loader plugins.

## ABI surface

- `FlowPluginV1` identifies a flow plugin and exposes its synchronous `execute`
  entry point.
- `FlowContextV1` gives the flow access to configuration, UDS requests, image
  loading, Seed & Key calculation, progress, logs, errors, and cancellation.
- `FileLoaderPluginV1` identifies supported files and streams their addressable
  segments into a host-owned `SegmentSinkV1`.
- Status constants distinguish success, error, invalid arguments, insufficient
  buffers, cancellation, and unsupported operations.
- Loader and component constants distinguish separate images from packaged
  application/driver components.
- `ABI_VERSION_V1`, structure sizes, and fixed entry-symbol names allow the
  host to reject incompatible plugins before execution.

Only C-compatible values cross the boundary. Plugin calls are synchronous, and
plugins must not retain host pointers after a callback returns. Rust plugins can
depend on this crate directly; plugins written in other native languages should
mirror the declared `repr(C)` layouts and calling convention.

## Development

```text
cargo test -p autors-security-flasher-sdk
```

See the [Security Flasher README](../security-flasher/README.md) for the host
workflow and the [workspace README](../../README.md) for the full library map.
