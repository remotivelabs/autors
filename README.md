# autors - The Ultimate Automobile Library

`autors` is a modular Rust workspace for automotive calibration, measurement,
diagnostics, bus access, ECU communication, flashing, and symbol processing. It
brings common engineering file formats and live-vehicle protocols into one
coherent, strongly typed library family.

The workspace is designed for applications that need to move smoothly between
description data, binary images, measurement files, bus hardware, protocol
clients, and foreign-language integrations without rebuilding the same
infrastructure for every tool.

## Why autors?

- **End-to-end automotive workflows.** Read A2L, CDF, DCM, DBC, LDF, ASC, BLF,
  LTRC, MDF, ODX, ELF, MAP, Intel HEX, Motorola S-record, VBF, TI-TXT, UF2,
  and raw binary data, then connect
  those descriptions to calibration, diagnostics, measurement, or flashing.
- **Modular by design.** Use one focused crate or compose the complete stack.
  File-format crates remain independent of hardware and protocol layers.
- **Async core with synchronous facades.** Communication crates implement their
  state machines once and expose both async APIs and optional blocking wrappers.
- **Cross-platform foundations.** Linux SocketCAN is available out of the box,
  while native CAN and LIN adapters are selected with explicit vendor features.
- **Typed, editable models.** Parsers produce domain objects that applications
  can inspect, modify, validate, and write back.
- **Deterministic and testable I/O.** Ordered collections, explicit byte order,
  pure codecs, injectable transports, and cooperative polling make behavior
  predictable in tools and tests.
- **Native integration.** A stable C ABI, generated C header, C example, and C#
  P/Invoke example make the A2L and calibration-value APIs accessible outside
  Rust.

## Library map

### Foundations

| Crate | Purpose |
| --- | --- |
| [`autors-util`](crates/autors-util/README.md) | Safe helper algorithms, binary/value formatting, memory ranges, time, and logging. |
| [`autors-native`](crates/autors-native/README.md) | Dynamic-library loading and native Seed & Key integration. |
| [`autors-runtime`](crates/autors-runtime/README.md) | Shared runtime primitives and the blocking bridge used by I/O crates. |

### Description and data formats

| Crate | Purpose |
| --- | --- |
| [`autors-a2l`](crates/autors-a2l/README.md) | A2L / ASAM MCD-2 MC object model, parser, and writer. |
| [`autors-cdf`](crates/autors-cdf/README.md) | ASAM CDF calibration-data XML model and I/O. |
| [`autors-dcm`](crates/autors-dcm/README.md) | DCM, MATLAB `.m`, and CANape PAR calibration conservation files. |
| [`autors-datafile`](crates/autors-datafile/README.md) | Sparse image editing, undo/redo, processing, and conversion for Intel HEX, S-record, VBF, TI-TXT, UF2, HEX ASCII, mixed records, and raw BIN. |
| [`autors-dbc`](crates/autors-dbc/README.md) | Editable CAN DBC databases and signal decoding. |
| [`autors-ldf`](crates/autors-ldf/README.md) | LIN Description Files, payload codecs, and diagnostic helpers. |
| [`autors-asc`](crates/autors-asc/README.md) | Vector ASC trace reading and writing for classic CAN and CAN FD. |
| [`autors-blf`](crates/autors-blf/README.md) | Vector BLF log reading and writing for CAN, CAN FD, and LIN frames. |
| [`autors-ltrc`](crates/autors-ltrc/README.md) | PEAK PLIN-View Pro LTRC 1.0-1.2 LIN trace reading. |
| [`autors-mdf`](crates/autors-mdf/README.md) | ASAM MDF 3.x and 4.x measurement files. |
| [`autors-odx`](crates/autors-odx/README.md) | ODX diagnostic data, flash descriptions, multi-file sets, and PDX packages. |

### Values, formulas, and symbols

| Crate | Purpose |
| --- | --- |
| [`autors-formula`](crates/autors-formula/README.md) | A2L formulas, table/rational conversions, and checksum algorithms. |
| [`autors-values`](crates/autors-values/README.md) | Runtime calibration values, ECU memory I/O, and CDF import/export. |
| [`autors-symbols`](crates/autors-symbols/README.md) | Shared symbol matching and A2L address-update workflow. |
| [`autors-elf`](crates/autors-elf/README.md) | ELF and DWARF parsing for symbols, types, and target data. |
| [`autors-map`](crates/autors-map/README.md) | Linker MAP parsing and A2L synchronization candidates. |

### Bus, transport, and protocols

| Crate | Purpose |
| --- | --- |
| [`autors-can`](crates/autors-can/README.md) | CAN/CAN FD abstraction, frames, SocketCAN, and optional vendor adapters. |
| [`autors-lin`](crates/autors-lin/README.md) | LIN devices, frames, checksums, configuration, and vendor adapters. |
| [`autors-scheduler`](crates/autors-scheduler/README.md) | DBC/LDF-driven CAN and LIN remaining-bus simulation with runtime selection and transmission hooks. |
| [`autors-isotp`](crates/autors-isotp/README.md) | ISO-TP over CAN and ISO 17987-2 diagnostic transport over LIN. |
| [`autors-comm`](crates/autors-comm/README.md) | Protocol-independent master state, DAQ lists, buffers, and polling. |
| [`autors-ccp`](crates/autors-ccp/README.md) | CCP master plus typed A2L `IF_DATA` support. |
| [`autors-xcp`](crates/autors-xcp/README.md) | XCP master for CAN, Ethernet, and SxI plus typed A2L `IF_DATA`. |
| [`autors-diag`](crates/autors-diag/README.md) | UDS, KWP2000, DoIP, pcapng DoIP extraction, and CAN/LIN diagnostic adapters. |
| [`autors-prm`](crates/autors-prm/README.md) | INCA ProF-style PRM/CNF flash-script parsing and execution. |

### Integration

| Crate | Purpose |
| --- | --- |
| [`autors-cli`](crates/autors-cli/README.md) | CANoe-inspired Ratatui workbench with dynamic all-crate discovery, native engineering-file inspectors, A2L calibration/DAQ-to-MDF and ELF/MAP synchronization, Vector/Kvaser/PEAK CAN/LIN scheduling, ASC/LTRC trace recording/playback, live UDS/ISO-TP/CCP/XCP, DoIP discovery/routing, ODX-driven diagnostics, and PRM preflight. |
| [`autors-ffi`](crates/autors-ffi/README.md) | Panic-safe C ABI for A2L projects, measurements, characteristics, and conversions. |

## Example applications and plugins

- [`autors-security-flasher`](examples/security-flasher/README.md) is a Slint
  desktop example that discovers configurations, connects to CAN hardware or a
  virtual ECU, and executes a plugin-defined UDS flashing flow.
- [`autors-security-flasher-sdk`](examples/security-flasher-sdk/README.md)
  defines the versioned C ABI shared by the flasher and its plugins.
- [`examplecar-fileloader`](examples/security-flasher-plugins/examplecar-fileloader/README.md)
  demonstrates an image-loader plugin for HEX, S-record, and BIN files.
- [`examplecar-flow`](examples/security-flasher-plugins/examplecar-flow/README.md)
  demonstrates a configuration-driven UDS programming sequence.
- [`examplecar-seedkey`](examples/security-flasher-plugins/examplecar-seedkey/README.md)
  demonstrates the native `GenerateKeyEx` boundary with non-production logic.

## Getting started

Install a current stable Rust toolchain, clone the repository, and work from the
workspace root:

```text
cargo check --workspace
cargo test --workspace
cargo doc --workspace --no-deps
```

Workspace packages are currently path-based and are not published. A crate in
the same checkout can depend on only the component it needs:

```toml
[dependencies]
autors-a2l = { path = "crates/autors-a2l" }
autors-values = { path = "crates/autors-values" }
```

For example, parsing and writing an A2L file is centered on `Project`:

```rust
use autors_a2l::Project;

fn main() -> autors_a2l::Result<()> {
    let project = Project::parse_file("example.a2l")?;
    println!("modules: {}", project.modules().count());
    project.save("example-copy.a2l")?;
    Ok(())
}
```

## Runtime and feature model

The communication stack uses an async core. Its default feature set enables
Tokio-backed runtime services and synchronous wrappers:

- `runtime-tokio` uses Tokio for timers and blocking-task dispatch.
- `blocking` exposes synchronous facade modules backed by the same async state
  machines.
- `--no-default-features` uses the standard-library runtime fallback and omits
  the blocking facade unless explicitly requested.

The bus crates keep native dependencies optional. `autors-can` enables Linux
SocketCAN by default and offers 17 `vendor-*` adapter features;
`autors-lin` offers Kvaser, PEAK, and Vector adapters. Use `all-vendors` only
when an application intentionally wants every adapter compiled in.

Communication is cooperatively driven: devices and protocol masters do not
silently start receive threads. Applications call `poll`, `poll_once`, or
`CommKernel::poll_clients`; the explicit CAN dispatch helper is available when
a background dispatcher is desired.

## Typical composition

A calibration or flashing tool can combine the crates without collapsing their
boundaries:

1. Load ECU metadata with `autors-a2l` or `autors-odx`.
2. Load program data with `autors-datafile` and symbols with `autors-elf` or
   `autors-map`.
3. Select CAN or LIN hardware through the bus abstraction.
4. Add ISO-TP, UDS, CCP, or XCP according to the ECU workflow.
5. Convert raw memory through `autors-formula` and `autors-values`.
6. Record or exchange data through ASC, BLF, LTRC, MDF, CDF, DCM, or the C ABI.

## Repository status and license

The workspace is currently version `0.1.0`, marked `publish = false`, and is
under active development. The main workspace packages use the proprietary
license declared in the root `Cargo.toml`. The public Security Flasher example,
SDK, and example plugins declare `MIT OR Apache-2.0` independently. Review the
relevant package manifest before redistribution.
