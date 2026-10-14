# rightkit-workflow

Shared scenario interpreter and durable state machine for the RightKit scenario runtime (RK8). `perform` runs separately from `observe_until`. Each step has an idempotency key. An append-only, fsynced journal records every transition. Leases carry fencing tokens. Cancellation is cooperative and has a deadline. A retake replays a step and compares its postcondition.

- Scenario model (`Scenario`, `StepSpec`, `IdempotencyKey`). `validate` requires non-empty, unique step ids.
- Step driver (`StepDriver`, `Observation`, `DriverError`). The machine calls `perform` at most once per `(run, step, take)`. It polls `observe` until the postcondition holds, the deadline passes, or the step is cancelled.
- Durable journal (`Journal`, `Record`, `Event`). A record is acknowledged only after `sync_all`. On open, bytes after the last newline (a torn, unacknowledged write) are truncated.
- Run and leases (`Run`, `Lease`, `Phase`, `ExecContext`). `acquire` and `renew` issue leases. Every write carries its fencing token. A superseded or expired lease is rejected.
- Retake (`Run::retake`, `RetakeReport`). A differing postcondition is flagged. It is never spliced into the original take.
- Remote worker boundary (`WorkerTransport`, `EnrolmentRequest`, `WorkerIdentity`, `Capability`, `BundleRef`, `ReconnectState`). `WorkerTransport` is a trait (`src/transport.rs`); `controller::ControllerLink` implements it over any `Read + Write` stream (`src/controller.rs:338`), `worker::WorkerEndpoint` is the worker side, and `net` holds loopback-first TCP helpers that refuse non-loopback addresses unless the caller opts in (`src/net.rs:1-25`, `:54`).

Catalog capability: `scenario-runtime`. The catalog entry is declaration-only (`rightkit-ownership.json`). Workers implement `StepDriver`. `ControllerLink` implements `WorkerTransport` today; other transports may follow in later slices.

## When to use it

- Use it to run an ordered set of steps where a repeated action would be harmful, such as a click that must not fire twice.
- Use it to resume a run after a crash. A step that was dispatched resumes into observe. It is not performed again.

Do not use it for GUI driving, capture, or networking. The `Run` machine performs no I/O beyond its journal file (`src/lib.rs:23-24`); the `net`, `controller` and `worker` modules do socket and stream I/O for remote workers. Do not use it as a general workflow engine or queue.

## Cargo features

None. The crate has no `[features]` table.

## Platform support

- Unix: the journal calls `sync_all` on the file and on its parent directory.
- Other platforms: the parent directory sync is a no-op. The file still gets `sync_all`.
- The crate has no GUI or OS-specific driver code. Workers provide those through `StepDriver`.

## Usage

Taken from `tests/machine.rs`. The test uses `ManualClock` so time can be controlled. Production code can use `SystemClock`.

```rust
use rightkit_workflow::{
    CancelToken, ExecContext, ManualClock, ObservePolicy, Run, StepRef, StepSpec,
};
use serde_json::Value;

let mut run = Run::open(&journal_path, "run-1", ManualClock::new(0))?;
let lease = run.acquire(
    &StepRef { step: "s0".into(), take: 0 },
    "w1",
    60_000,
)?;

let cancel = CancelToken::new();
let ctx = ExecContext {
    worker: "w1",
    lease_ttl_ms: 60_000,
    policy: ObservePolicy { timeout_ms: 5_000, interval_ms: 100 },
    cancel: &cancel,
};
let spec = StepSpec {
    step_id: "s0".into(),
    label: "step s0".into(),
    action: Value::Null,
    expect: Value::Null,
};
let phase = run.execute(&lease, &spec, &mut driver, &ctx)?;
```

`driver` is any value that implements `StepDriver`. The test defines a scripted `FakeDriver`. A production `Run::open` returns `Result` from `rightkit_workflow::Result`.

## Tests

- `tests/machine.rs`: perform runs at most once, delayed postconditions resume into observe, restart after dispatch, lease fencing, cancellation, and retake comparison. Nothing is mocked except the step driver.

## Version

Crate version: see [INDEX.md](../../INDEX.md) and `CHANGELOG.md`. 0.1.0 is staged, not published, in the registry record (`../../packages/release/rightkit-versions.json`).
