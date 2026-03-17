# autors-ldf

LIN Description File parsing, validation, writing, and payload codecs.

## Highlights

- `Ldf::parse_str` and `Ldf::read` load an editable LIN network model.
- `Ldf::validate`, `write_string`, and `write` validate and emit the model.
- Typed objects cover nodes, signals, unconditional/sporadic/event-triggered
  frames, diagnostic frames, schedules, encodings, and node composition.
- Frame codecs encode and decode named signal values with configurable padding
  and LIN checksum handling.
- Diagnostic helpers build and decode the standard LIN node-configuration
  services and expose the master-request and slave-response identifiers.

The parser implementation is private; applications work through the stable
model, codec, and diagnostic modules.

## Development

```text
cargo test -p autors-ldf
cargo doc -p autors-ldf --no-deps
```

See the [workspace README](../../README.md) for live LIN and diagnostic
transport crates.
