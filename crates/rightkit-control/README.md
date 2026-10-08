# rightkit-control

Shared native desktop control APIs for Right Suite applications. Provides JSON helpers, keyboard input, WebDriver bridging, and platform-specific accessibility or embedded control modules.

### Hidden macOS pointer input

Embedded Tauri targets receive native `NSEvent` down/drag/up calls on the
WKWebView responder, on AppKit's main thread. This bypasses window first-mouse
handling without making a window key, showing it, or activating its app.
WebKit reads `+[NSEvent pressedMouseButtons]` to build Pointer Events, so the
embedded path supplies a thread-local button mask during dispatch. A second
scoped hook bypasses WKWebView's asynchronous pointer text-input interception;
both Objective-C hooks forward to their original implementations outside that
dispatch. No DOM input events or global HID events are synthesized.

`pointer {phase:"down"|"drag"|"up"|"move",x,y,button?,modifiers?}` supports
scrubs split across requests. `click`, `move`, and composite `drag` share native
delivery; x/y are viewport CSS pixels. Pointer calls retain effect admission,
allowlists, and optional input-lease fencing. `rightkit-qa::Control::pointer`
uses this service for hidden launches. Non-embedded callers keep the per-pid
CGEvent fallback, which cannot guarantee first-mouse delivery to inactive views.

`examples/control-test-app/proof.sh` proves trusted Pointer Events, capture,
scrub value changes, modifier clicks, hidden/non-key readback, and foreground
stability throughout the run. `TARGET` names the workspace managed debug
directory; `APP_TARGET` names the standalone fixture's managed debug directory.

The `tauri-plugin` feature enables the embedded Tauri control server.

Publish through the RightKit release workflow.

## Control-plane guarantees (0.1.2)

- **EFF-001 admission** (`admission::EffectGate`): every effectful request is
  validated, approved by a host `AdmissionHook` (trait or closure), executed,
  and settled as `ok | denied | failed | cancelled`. A denial (or a panicking
  hook) happens before any side effect. `webdriver::Server::with_gate`
  (click, send keys, screenshot), the `tauri-plugin` `Control::gate`,
  `mac::Gated` and the `rightkit-control` CLI route every effectful entry
  point through it; the default gate is allow-all, so existing behaviour is
  unchanged.
- **PTY-002 input lease** (`lease::InputLease`, `lease::ControlSession`):
  acquire bumps the epoch and fences the previous holder; input carries
  `(epoch, sequence)`; stale epochs are rejected, acknowledged duplicates are
  deduplicated without re-execution, gaps are rejected, and ambiguous
  (unacknowledged) input must be explicitly reconciled before new input.
  `Control::input_lease` makes the plugin's input methods require
  `leaseEpoch` + `inputSequence` and adds `lease_*` methods.
- **PTY-002 durable state** (`lease::InputLease::open`, `lease_store`):
  optional `LeaseStore` backing (in-memory stays the default) with an atomic
  `FileLeaseStore` (temp file, fsync, rename, directory fsync, checksum).
  Every change is saved before it takes effect: an epoch is granted only once
  durable, and an input sequence is reserved durably before its executor
  runs. Each lease has a durable `ExecutionIdentity` and an
  `ExecutionState` (`Running | Exited | Unknown`); input is accepted only
  while `Running`. On restart the epoch is fenced forward (never backwards),
  the lease is detached, `Running` comes back `Unknown`, and input that was
  unacknowledged at the crash comes back as `Unknown` input
  (`unknown_input`, `input_status`) that must be resolved explicitly with
  `acknowledge_input(epoch, seq)`, `reconcile` or `acquire_reconciling`.
  A corrupt or foreign record fails closed rather than resetting.
- **Lifecycle events** (`events::ControlEvent`): content-free
  start/stop/crash/denied and lease acquired/fenced/released/rejected events
  via an `EventSink` callback.

## CLI admission

`rightkit-control [--policy FILE] [--events] <command> ...` admits every
effectful command (`launch click hover drag scroll click-text press keywin
keyraw type key shot`, and `serve`'s click / send keys / screenshot) through
one gate. Without a policy the gate is allow-all (output unchanged); with
`--policy FILE` or `RIGHTKIT_CONTROL_POLICY` it applies `cli::Policy`
(`default allow|deny`, then first-match `allow|deny <kind|*> [method|*]`
lines). An unreadable or invalid policy exits 2; a denial exits 3 and does
nothing. `--events` / `RIGHTKIT_CONTROL_EVENTS=stderr` prints content-free
lifecycle events to stderr. Read-only commands are not effects.

## Known gaps

Closed in 0.1.2:

- PTY-002 durable lease state and execution identity/state across host
  restart or crash (previously in-memory only).
- Ungated paths: the effectful `mac` primitives are now crate-private behind
  `mac::Gated`, reachable ungated only through the explicitly named
  `mac::unsafe_ungated` opt-in; the CLI admits through a gate with an optional
  policy; the WebDriver screenshot is now admitted as a `capture` effect.

Remaining:

- **Breaking for direct `mac` callers:** `mac::click`, `type_text`, `key`,
  `ax_press`, `click_node`, `launch_hidden`, `capture_window`, `ax_make_key`,
  `key_without_raise`, `hover`, `drag`, `scroll` moved to `mac::Gated` (or
  `mac::unsafe_ungated`). Reads (`windows`, `main_window`, `frontmost_*`,
  `ax_dump`, `ax_find`, `ax_text`, `accessibility_trusted`) are unchanged.
- `webdriver::macos::AxBackend` is an adapter whose `Backend` methods act
  directly, and `Server::backend` is a public field; only calls through
  `Server::handle`/`serve` are admitted.
- `FileLeaseStore` assumes one owning process per file (no cross-process
  lock). If the post-execute acknowledgement save fails, the durable record
  stays conservative (that input recovers as `Unknown`) and
  `InputLease::store_degraded()` reports it.
- `acquire_reconciling` panics if a durable store refuses the save; durable
  callers should use `try_acquire_reconciling`.
- The holder is identified by its epoch only (no holder label is stored).
- Recovery happens in `InputLease::open`, before an event sink is attached,
  so no recovery event is emitted; inspect `record()` after opening.
- `LeaseError` gained variants (`ExecutionNotRunning`, `UnsentSequence`,
  `NonMonotonicAck`, `InvalidExecutionTransition`, `Store`); exhaustive
  external matches need a new arm.
- The `tauri-plugin` lease methods do not yet expose durable state or
  `acknowledge_input` over the wire.
