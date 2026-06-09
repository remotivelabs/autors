# autors-scheduler

Cooperative CAN and LIN remaining-bus simulation driven by DBC and LDF files.

## Highlights

- `CanScheduler` reads `GenMsgCycleTime`, `GenMsgStartDelayTime`, and
  `GenSigStartValue`, then sends selected messages at their DBC cycle times.
- CAN messages can be selected individually or through their transmitter node;
  message overrides, payloads, periods, frame types, and one-shot triggers can
  all change while the scheduler is running.
- `LinScheduler` executes named LDF schedule tables. It sends stored payloads
  for the LDF master and simulated nodes, and issues header-only requests for
  physical slave nodes.
- LIN schedule changes, simulated-node selection, payloads, frame overrides,
  sporadic pending state, and one-shot triggers are runtime operations.
- Ordered before-send and after-send hooks can mutate payloads and observe the
  adapter result. Hooks are suitable for rolling counters, checksums, message
  authentication, and encryption, and can be removed by registration token.
- Both schedulers are explicitly advanced through `poll`; no background task is
  created. The `blocking` feature exposes synchronous wrappers over the same
  state machines.

## Minimal CAN example

```rust,no_run
use autors_dbc::dbc::DBCFile;
use autors_scheduler::CanScheduler;

# fn configure(dbc: &DBCFile) -> autors_scheduler::Result<CanScheduler> {
let mut scheduler = CanScheduler::from_dbc(dbc)?;
scheduler.set_node_enabled("BodyController", true)?;
scheduler.add_before_hook(0x123, |frame, _context| {
    frame.data[0] = frame.data[0].wrapping_add(1);
    Ok(())
})?;
# Ok(scheduler)
# }
```

Applications call `scheduler.poll(&mut device).await` from their existing event
loop. Use `next_deadline` to integrate the scheduler with an external timer.
