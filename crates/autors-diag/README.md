# autors-diag

Automotive diagnostic session and message layers for UDS, KWP2000, and DoIP.

## Highlights

- `UdsClient<T: UdsTransport>` implements typed UDS services for sessions,
  reset, security, data identifiers, memory, DTCs, routines, transfer, I/O
  control, communication control, and related operations.
- Typed response structures decode positive and negative responses while
  retaining transport state.
- `IsoTpTransport` and `LinTpTransport` connect UDS or KWP-style requests to CAN
  ISO-TP and LIN diagnostic transport.
- `kwp` defines KWP2000 service and negative-response enumerations;
  `ifdata_kwp` parses and writes typed A2L `IF_DATA ASAP1B_KWP2000` blocks.
- `doip` implements ISO 13400-2 frame codecs, and `DoIpClient` handles UDP
  vehicle identification (broadcast, EID, or VIN), TCP connection, routing
  activation, diagnostic exchange, NACKs, and timeouts.
- `doip_capture` reads pcapng captures, decodes common Ethernet/raw-IP/Linux
  cooked link types, and extracts clear-text DoIP over UDP or reassembled TCP.
  Recoverable packet truncation, unsupported link types, TCP gaps, and partial
  DoIP frames are retained as typed capture issues.
- Blocking wrappers cover UDS transports/clients and DoIP.

Discovery returns a response-arrival-ordered entity registry with duplicate
announcements removed. The default features enable `runtime-tokio` and
`blocking`.

## Reading a DoIP pcapng capture

```rust
use autors_diag::doip_capture::DoIpCapture;

let capture = DoIpCapture::open_pcapng("vehicle-session.pcapng")?;
for item in &capture.frames {
    println!("{} -> {}: {}", item.source, item.destination, item.frame);
}
if !capture.issues.is_empty() {
    eprintln!("{} packet-level issues were recovered", capture.issues.len());
}
# Ok::<(), autors_diag::Error>(())
```

The default filter is clear-text DoIP port 13400. TLS-encrypted DoIP traffic is
not decrypted. Use `DoIpCaptureOptions` to select additional clear-text ports
or adjust parser and stream limits. The `doip_pcapng` example prints capture
statistics without dumping diagnostic payloads:

```text
cargo run -p autors-diag --example doip_pcapng -- capture.pcapng
```

## Development

```text
cargo test -p autors-diag
cargo check -p autors-diag --no-default-features
```

See the [workspace README](../../README.md) for ODX, ISO-TP, CAN, and LIN crates.
