# autors-prm

Parsing and execution of INCA ProF-style PRM/CNF ECU flash procedures.

## Highlights

- `PrmFile::open` and `parse_str` load `.prm` procedures, `#define` values,
  included `.pri` files, command sections, and the referenced `.cnf` file.
- `CnfFile` exposes controller configuration and loads `SOURCE_MEM_AREA`
  segments from supported program-data files.
- `script::execute` interprets command sets, procedures, state-based branches,
  and all recognized PRM instructions with a configurable safety step limit.
- `Executor` supplies the instruction actions and coordinates injected UDS,
  CCP, XCP, CAN, checksum, and Seed & Key implementations. Interpreter options
  can suppress waits and transport I/O for deterministic preflight runs.
- Trait-based dependencies (`UdsOps`, `CcpOps`, `XcpOps`, and `CanSend`) make
  production adapters and deterministic test doubles interchangeable.
- Message callbacks, progress updates, variable substitution, and cooperative
  cancellation are built into the execution path.
- `BlockingExecutor` exposes the same workflow synchronously.

The supported syntax is the subset implemented by the parser and interpreter;
scripts are interpreted directly rather than compiled into host code. The
default features enable `runtime-tokio` and `blocking`.

## Development

```text
cargo test -p autors-prm
cargo check -p autors-prm --no-default-features
```

See the [workspace README](../../README.md) for program-data and protocol crates.
