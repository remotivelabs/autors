# autors-cli

`autors-cli` is a CANoe-inspired terminal engineering workbench for the whole
`autors` workspace. It uses Ratatui to place workspace discovery, network
descriptions, traces, calibration data, diagnostics, simulation, and platform
integration behind one keyboard-driven interface.

## Current workbench surfaces

- The capability center discovers every `autors-*` package through Cargo
  metadata. Its descriptions, features, internal dependencies, paths, and
  engineering workflows therefore track workspace changes instead of being a
  fixed package list.
- Native A2L inspection summarizes projects, modules, measurements,
  characteristics, functions, conversions, layouts, and type definitions. A
  retained A2L session also drives a measurement/calibration workbench:
  module-qualified objects expose addresses, types, shapes, units, limits,
  read/write policy, physical values, editable virtual ECU memory, DAQ
  selection, and timestamped 10 Hz acquisition. Physical/raw conversion uses
  `autors-values`; DAQ packing and sample buffers use `autors-comm`.
  Captured physical samples are retained beyond the small recent-value display;
  `w` writes them through `autors-mdf` as a round-trippable MDF 3.30 recording
  with timestamps, module-qualified channels, units, limits, and descriptions.
- Native DBC inspection shows messages, transmitters, DLCs, signal details,
  units, scaling, and plausibility results.
- Native CANoe ASC inspection shows classic CAN, CAN FD, errors, events,
  timestamp metadata, and per-frame details.
- A DBC remains attached to the workbench session: opening an ASC or BLF trace
  afterwards adds symbolic message names and decoded physical signal values,
  including multiplex-branch selection and value-table labels.
- The CAN/CAN FD workbench provides manual transmit, a bounded live trace,
  bus-load statistics, DBC symbolic decoding, and remaining-bus simulation.
  Its always-available virtual adapter adds receive injection and loopback;
  optional Vector, Kvaser, and PEAK drivers run the same scheduler and trace
  pipeline on real channels. Scheduled messages can be enabled, triggered
  once, and edited for payload and cycle time while simulation is running.
  `w` exports the bounded live trace as a CANoe/CANalyzer-compatible ASC file.
- The LIN workbench loads LDF frame and schedule definitions, executes
  master/slave remaining-bus behavior (including header-only subscriber
  requests), switches schedule tables at runtime, edits and triggers frames,
  and decodes received application signals into the live trace. The same
  feature-gated Vector, Kvaser, and PEAK families can replace the virtual LIN
  channel without changing the scheduling workflow. `w` records full frames
  and header-only slave requests in a round-trippable PLIN-View LTRC file.
- The protocol lab builds and decodes UDS/KWP PDUs, wraps and validates DoIP
  diagnostic frames, and exposes typed CCP and XCP command/response codecs.
  Named command templates and raw hexadecimal input share one inspectable
  history. A separate live action sends UDS through the real ISO-TP state
  machine, CCP/XCP through native CAN request-response frames on the selected
  adapter, or UDS over a routed DoIP TCP client. Command/response frames remain
  visible in both protocol history and the CAN trace; the virtual CAN adapter
  supplies deterministic ECU responses for hardware-free workflow tests. The
  DoIP page broadcasts vehicle-identification requests, deduplicates every
  answering entity, shows VIN/EID/GID/action/synchronization data, and can use
  the selected responder's endpoint and logical address for routing activation.
- Native LDF and LTRC views cover LIN nodes, unconditional frames, signals,
  schedule inventory, trace frames, checksums, missing bytes, and bus events.
- Native BLF inspection inventories every typed/raw object and decodes common
  CAN, CAN FD, and LIN rows into trace columns.
- ASC, BLF, and LTRC tables have timestamp-driven play/pause, reset, and
  0.125×-16× playback controls for offline trace review.
- Native MDF, CDF, and ODX views expose measurement channels, calibration
  instances, diagnostic variants, services, protocols, and DTC totals.
- Opening an ODX file also retains a database-driven diagnostic session (`d`).
  It merges inherited ECU/base variants, expands identifier-specific services,
  generates SID/subfunction/identifier request prefixes, lists dynamic request
  parameters, switches between ISO-TP/CAN and DoIP, and decodes live positive
  responses back into named physical values. ODX communication parameters can
  populate the physical CAN request/response IDs without retyping them.
- DCM, CANape PAR, and MATLAB calibration files gain an A2L-aware typed view
  after an A2L is retained. The workbench tries every module, selects the best
  object match, and shows scalar/array shape, physical preview, units,
  function ownership, axes, and explicit reasons for skipped values.
- Native PRM/CNF views expose flash-procedure command graphs, protocol mode,
  branches, controller addresses, transport settings, and source/erase/
  destination memory segments. The retained PRM preflight workbench uses the
  shared `autors-prm` script interpreter to traverse command sets, procedures,
  state branches, and instruction semantics while explicitly suppressing
  waits, transport I/O, flashing, and Seed & Key calls.
- Native program-image views use content-first detection for Intel HEX,
  Motorola S-record, VBF, Ford I-HEX, TI-TXT, UF2, hexadecimal ASCII, mixed
  record streams, and raw binary files. They expose sparse address segments,
  holes, initialized byte counts, detection confidence, previews, and CRC-32.
- Native ELF and linker MAP views list sections and symbols with addresses,
  sizes, types, bindings, and the metadata needed for the shared A2L address
  synchronization workflow. After retaining an A2L, opening either source
  computes typed object matches in the symbol workbench (`y`): safe
  address-only changes are preselected, size mismatches require an explicit
  policy opt-in, bit-mask handling is visible, address multipliers can be
  recomputed, and `s` writes selected updates to an explicit A2L copy.
- Every other text or binary format can be opened in a safe universal line or
  hex inspector while its dedicated engineering view is being integrated.

## Run

```text
cargo run -p autors-cli
cargo run -p autors-cli -- network.dbc
cargo run -p autors-cli -- --workspace C:\work\auto_rs capture.asc
cargo run -p autors-cli --features hardware-all
```

Native drivers are opt-in and loaded dynamically at runtime. Use
`hardware-vector`, `hardware-kvaser`, or `hardware-peak` for one adapter
family, or `hardware-all` for all three. A missing DLL is reported in the TUI
instead of preventing the normal virtual-only build from starting.

Use arrow keys or `h/j/k/l` to navigate. Press `o` to open a file, `/` to
filter, `Enter` to open a selected crate's README, `F5` to rescan the workspace,
`b` to enter the CAN workbench, `n` to enter the LIN workbench, `Esc` to go
back, and `?` for the complete keyboard reference. Press `a` to enter the A2L
measurement/calibration workbench after opening an A2L file; `Tab` changes the
object view, `Enter` arms a measurement, `Space` runs DAQ, and `e` edits a
writable physical value. Press `w` in this workbench to save the captured
physical samples as MDF. In either bus workbench,
use `c` to connect, `s` to transmit, `i` to inject a received frame, and
`Space` to run or pause scheduling.
Use `a` to cycle through compiled adapters, `e` to enumerate CAN channels when
the selected backend exposes discovery, `,` and `.` to select a channel, and
`v` to edit the zero-based channel and vendor hardware-type number directly.
Receive injection is intentionally restricted to virtual adapters.
The message table also supports `Enter` (enable), `t` (one-shot trigger), `p`
(payload), and `m` (CAN cycle time). Use `[` and `]` to switch LDF schedule
tables.

Press `g` for the protocol lab. Use `Tab` or `Shift-Tab` (or keys `1`-`4`)
to select UDS/KWP, DoIP, CCP, or XCP, then press `e` to enter a named template
or raw wire bytes. The left panel documents the commands accepted by the
selected codec. Press `c` to configure CAN IDs/FD mode or a DoIP endpoint and
`r` to execute the template on that live transport. CAN protocol execution uses
the connection selected in the CAN workbench. On DoIP, `d` discovers entities
over UDP and `[`/`]` selects the endpoint used by the next routing activation.

After opening an ODX file, press `d` for the diagnostic database workbench.
`Tab` selects an ECU/base variant, arrow keys select an expanded service row,
`t` switches CAN/DoIP, and `c` applies resolvable ODX physical CAN IDs. `e`
encodes the selected service for offline inspection; `r` executes it live.
Both prompts accept optional raw bytes for dynamic request parameters after the
ODX-generated fixed prefix.

After opening a PRM file, press `f` for the flash preflight workbench. `Tab`
selects the entry command set and `Space`/`Enter` runs the no-I/O interpreter;
the report shows each executed semantic step, suppressed I/O step, state, and
branch target.

For address synchronization, open the A2L and then its ELF/AXF or linker MAP,
and press `y`. `Enter` selects individual changes, `m` changes the address
multiplier, `z` explicitly allows size-mismatch updates, `p` controls bit-mask
preservation, and `s` writes a separate synchronized A2L target.

## Development

```text
cargo test -p autors-cli
cargo test -p autors-cli --features hardware-all
cargo clippy -p autors-cli --all-targets --no-deps -- -D warnings
```
