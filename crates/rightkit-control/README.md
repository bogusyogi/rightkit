# rightkit-control

Shared native desktop control APIs for Right Suite applications. Provides JSON helpers, keyboard input, WebDriver bridging, and platform-specific accessibility or embedded control modules.

The `tauri-plugin` feature enables the embedded Tauri control server.

Publish through the RightKit release workflow.

## Control-plane guarantees (0.1.1)

- **EFF-001 admission** (`admission::EffectGate`): every effectful request is
  validated, approved by a host `AdmissionHook` (trait or closure), executed,
  and settled as `ok | denied | failed | cancelled`. A denial (or a panicking
  hook) happens before any side effect. `webdriver::Server::with_gate` and the
  `tauri-plugin` `Control::gate` route effectful methods through it; the
  default gate is allow-all, so existing behaviour is unchanged.
- **PTY-002 input lease** (`lease::InputLease`, `lease::ControlSession`):
  acquire bumps the epoch and fences the previous holder; input carries
  `(epoch, sequence)`; stale epochs are rejected, acknowledged duplicates are
  deduplicated without re-execution, gaps are rejected, and ambiguous
  (unacknowledged) input must be explicitly reconciled before new input.
  `Control::input_lease` makes the plugin's input methods require
  `leaseEpoch` + `inputSequence` and adds `lease_*` methods.
- **Lifecycle events** (`events::ControlEvent`): content-free
  start/stop/crash/denied and lease acquired/fenced/released/rejected events
  via an `EventSink` callback.

The raw `mac` primitives remain ungated building blocks.
