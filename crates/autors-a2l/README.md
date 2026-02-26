# autors-a2l

An editable A2L object model, parser, and writer for ASAM MCD-2 MC ECU
description files.

## Highlights

- `Project::parse_str` and `Project::parse_file` build a typed project model.
- Dedicated nodes cover modules, measurements, characteristics, axes,
  conversion methods, record layouts, functions, groups, typedefs, variants,
  annotations, and common CANape extensions.
- Unknown or not-yet-typed content is retained as tokens where possible so it
  can survive a read/write round trip.
- `WriterOptions` controls indentation, alignment, sorting, and A2ML output.
- The tokenizer, block tree, parameter cursor, and `Node` trait are public for
  applications that need lower-level access.

## Example

```rust
use autors_a2l::Project;

fn main() -> autors_a2l::Result<()> {
    let project = Project::parse_file("input.a2l")?;
    for module in project.modules() {
        println!("{}", module.name);
    }
    project.save("output.a2l")?;
    Ok(())
}
```

## Development

```text
cargo test -p autors-a2l
cargo doc -p autors-a2l --no-deps
```

See the [workspace README](../../README.md) for calibration, symbol, and
protocol crates that consume the model.
