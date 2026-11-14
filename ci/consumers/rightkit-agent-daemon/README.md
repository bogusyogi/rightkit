# rightkit-agent-daemon 0.1.1 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-daemon` by exact registry pin
(`=0.1.1`) with default features. It has no path, Git, workspace or patch dependency and no committed
`Cargo.lock`. Extra dependencies: `async-trait = "0.1"` (to implement `LifecycleBackend`) and `http = "1"`
(to build the request headers the authority gate reads).

Exercises the source-trust gate (an absent header is trusted, `untrusted` and `tool-untrusted` are lowered and
refused with 403, an unknown value is ignored), the `DaemonSettingsStore` defaults, the live-turn generation fence
(a stale guard dropped after a newer turn began leaves the session active), the lifecycle command parser (start,
stop, attach, schedule add and cancel, ps, and its refusals), and one `LifecycleAuthority` over an in-memory
backend: start, the idempotent second start with no backend call, attach, schedule refused while stopped, a
failing restart that leaves the state `Failed`, and a shared clone that sees the same snapshot. Async calls run on
a minimal `std` executor; there is no tokio runtime.

State: process memory only. No file, no network, no process spawn, no real user data.

Proof row: public API surface of the new crate (moved from `rightkit-daemon` 0.5.4 `agent_daemon_core` and
`agent_daemon_runtime`). Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
