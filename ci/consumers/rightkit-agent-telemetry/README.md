# rightkit-agent-telemetry 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-telemetry` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency, no committed `Cargo.lock`
and no other dependency.

Exercises the in-process recorders: turn latency percentiles (nearest rank), round trips per task, wakeups,
conservative token estimates and prompt overhead aggregates, repair counters and the content-free validator
error kinds, and a measurement receipt (an empty run id and a reversed window are refused; a receipt issued
from an observed run is marked observed and its digest checks). Every recorder is reset before and after.

State: process memory only. No file, no network, no process spawn, no real user data. `init` installs a global
tracing subscriber and is never called.

Proof row: public API surface of the new crate (moved from `rightkit-logs` 0.3.10 `agent_telemetry`). Each
check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
