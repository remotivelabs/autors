# autors-asc

Reading and writing Vector ASCII (`.asc`) CAN trace files.

## Highlights

- Parses classic CAN and CAN FD data, remote, and error frames.
- Supports standard and extended identifiers, Rx/Tx direction, hexadecimal or
  decimal number bases, and absolute or relative timestamp modes.
- Preserves timestamped non-frame events and CAN FD metadata columns.
- Converts data frames to and from the shared `autors-can::CanFrame` type.
- Writes CANoe/CANalyzer-compatible trigger blocks.

## Development

```text
cargo test -p autors-asc
cargo clippy -p autors-asc --all-targets -- -D warnings
```
