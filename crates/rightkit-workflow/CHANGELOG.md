# Changelog

## 0.2.0

- New feature `sequence` (off by default, no new dependencies): `Sequence`, `SequenceRun::open` and `run_next`, a linear run over non-GUI steps that reuses the journal (`Dispatched` fsynced before `perform`), per-step idempotency keys and resume. `run_next` runs one step per call and returns `Advanced`, `Done`, `Stopped` or `Unresolved`, so a restart at every boundary is normal.
- `Gate::before_perform(step, key)` runs before a step's first dispatch (for a budget reservation) and can stop the run without writing anything for that step. It is not called when a dispatched step resumes. Implementations must be idempotent on the key.
- `ReplayPolicy::{Unresolved, ReissueOnce}` decides what happens to a step whose outcome was never observed: report it, or reissue it once in a new take.

## 0.1.0

Renamed from rightkit-scenario (never published).

- First published release. Scenario interpreter and durable state machine for the RightKit scenario runtime (RK8): `perform` separated from `observe_until`, per-step idempotency keys, an append-only fsynced journal with torn-write truncation, leases with fencing tokens, cooperative cancellation with a deadline, retake replay with postcondition comparison, and the `WorkerTransport` trait boundary for remote workers.
