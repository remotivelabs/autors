# examplecar-flow

A public, configuration-driven UDS programming-flow plugin for the `autors`
Security Flasher example.

## What it demonstrates

- Exports the versioned `autors_security_flow_v1` entry symbol.
- Loads separate or packaged application and flash-driver components through
  host callbacks.
- Runs a complete example sequence: session changes, precondition checks,
  communication/DTC control, SecurityAccess, fingerprint writing, download and
  transfer, signature routines, dependency checking, reset, and bus recovery.
- Reports progress and logs, validates positive UDS responses, handles response
  pending, and checks cancellation throughout the flow.
- Normalizes sparse application segments and calculates the example signature
  used by the bundled virtual ECU.

## Demo-only scope

Addresses, routine identifiers, image sizes, security levels, signature logic,
and timing choices are intentionally specific to the synthetic `ExampleCar`
configuration. They are not production defaults and contain no OEM secrets.
Create a separate flow plugin for a real ECU instead of adapting these values
silently.

## Build and test

```text
cargo build -p examplecar-flow
cargo test -p examplecar-flow
```

See the [SDK README](../../security-flasher-sdk/README.md) for the callback ABI
and the [Security Flasher README](../../security-flasher/README.md) for the host
workflow.
