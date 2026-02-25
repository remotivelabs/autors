# autors-runtime

The small runtime abstraction used by the asynchronous `autors` I/O stack. It
keeps protocol implementations independent from a mandatory executor while
providing a shared bridge for synchronous facades.

## Core API

- `sleep` suspends an async task for a duration.
- `timeout` races a future against a deadline and returns `Elapsed` on expiry.
- `spawn_blocking` moves blocking driver or serial operations off the executor.
- `block_on` drives a future on the current thread and powers the workspace's
  `blocking` modules.

## Features

| Feature | Effect |
| --- | --- |
| `runtime-tokio` | Uses a shared Tokio runtime, timers, and blocking pool. |
| `blocking` | Enables the feature convention used for synchronous facades. |
| default | Enables both `runtime-tokio` and `blocking`. |

Without `runtime-tokio`, timers and blocking work use a standard-thread
fallback. Do not call `block_on` from inside the shared runtime's async context.

## Development

```text
cargo test -p autors-runtime
cargo test -p autors-runtime --no-default-features
```

See the [workspace README](../../README.md) for the crates built on this layer.
