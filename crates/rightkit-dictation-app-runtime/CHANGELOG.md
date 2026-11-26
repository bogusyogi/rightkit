# Changelog

## 0.3.2

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.3.1 requires 0.4.9. The 0.3.1 manifest on crates.io stays as published; this version carries the new requirements.

## 0.3.1 (unpublished)
- Provider keys never gate the capture lifecycle (v6): `EngineSupervisor::ensure_provider_keys_after_start` is replaced by `deliver_provider_keys_background`, meant for a background thread. It delivers the generation's keys but never retires the generation, never shuts the engine down and never spawns one. A failed or uncertain delivery returns `Err` (fixed non-secret reason), settles the generation's gate as done (the recording keeps its engine, which holds no keys and falls back to local polish; no config push or stop can retire it mid-recording through the gate's failure path) and remembers the generation in `keys_retry_generation`; the next `start_without_keys` (no session in flight) retires it so the next generation gets its own delivery. `start`, `call`, `send` and `send_capture_control` are unchanged.
- Allow the `capture_warmup` engine method and treat it as accepted-style (acknowledged at once, completed by an id-less event).
- Add `EngineSupervisor::send_capture_control`, a narrowly typed send for the shortcut-press warm-up and its revoke frame: it accepts only a `CaptureWarmup` frame, only to an engine that is already running (it never spawns or readies one), and it skips the provider-key gate wait that `send` performs, so a microphone resume or revoke never queues behind a key delivery (up to the gate's bound) or config work. A normal start keeps the gate. Requires `rightkit-dictation-core` 0.3.4 (the `CaptureWarmup` payload and the `start_attempt_id` frame field). Used by `rightkit-dictation-tauri` 0.3.1.
- Add `EngineSupervisor::start_without_keys`, `call_without_keys` and `ensure_provider_keys_after_start`: the engine readiness of `start`/`call` split from the provider-key delivery gate (`ensure_ready` is now `ensure_running` plus `ensure_provider_keys`, behavior unchanged for every existing caller). A recording start uses them so the start frame never waits for a key delivery and delivers the keys right after the engine accepted the start.
- Add `EngineSupervisor::send_session_cancel`, a narrowly typed send for the cancel of a start whose RPC ended ambiguously: only a `Cancel` stop frame naming a session, only to a running engine, no key-gate wait.

- Key delivery never retires a capturing generation (v7): `retire_generation` is refused while a capture holds the engine (`hold_capture` guard, or the shell's registered active-session mirror) and is deferred to the next recording start; prewarm and every ordinary call are covered. `claim_provider_keys_for_start` / `deliver_claimed_provider_keys` replace `deliver_provider_keys_background` (the claim is taken synchronously, non-waiting, before the start returns); `send_stop` waits (bounded) on the claim before a stop; `seal_capture` sends the synchronous capture seal; staged `routing.rs` gains `KeyGate::try_claim` and `wait_settled`. `capture_seal` is allowed on the wire (regular call).
## 0.3.0 (unpublished)

- Breaking for Rust callers that destructure `EngineEvent::SessionReset`: it now carries an optional typed `host_command`. Match the new field or use `..` when only handling the reset. Older engine frames still decode with no host command. The existing sidecar generation and session checks remain in the routing path.

## 0.2.2 (unpublished)

- Align the shell supervisor with the 0.3 model-store interface and current dictation, transcript, telemetry and daemon packages. The host supplies app paths and engine identity. (bogusyogi/heardright#349)

## 0.2.1 (unpublished)

- Serialize provider-key requests into a bounded, zeroizing buffer without creating an intermediate JSON value. Engine worker threads now use product-neutral names. (bogusyogi/heardright#349)

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-daemon` 0.5 -> 0.8, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-models` 0.1 -> 0.2, `rightkit-dictation-telemetry` 0.1 -> 0.2, `rightkit-job-queue` 0.5 -> 0.6, `rightkit-logs` 0.3 -> 0.4. 0.1.2 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.2 in place (#1572). No source change in this crate since 0.1.2.

## 0.1.2 (unpublished)

- Removed (rightkit#1437): the `shared_store_smoke` example and its `[[example]]` entry. It ran the shared store against a scratch suite root through `rightkit-daemon`'s `test-mode` and `TEST_CAPABILITY`, which no longer exist (rightkit-daemon 0.5.3): a dummy root is not a test any more. Removed too: the `test-mode` feature and everything it gated: `engine_supervisor::{pass_live_eval_env_for_test, l3_env_keys_for_test, strip_l3_env_for_test, resolve_engine_override_for_test, build_sidecar_command_for_test}`; the `RIGHTKIT_ENGINE_BIN`, `RIGHTKIT_ENGINE_APP_PATH`, `RIGHTKIT_NEXT_ENGINE_ROOT`, `RIGHTKIT_AI_EVAL_*`, `RIGHTKIT_L3_LOG_PAYLOADS`, `RIGHTKIT_OWNER_DIAGNOSTICS*` and `HR_*` lever forwards; the `RIGHTKIT_SUITE_ROOT` / `RIGHT_SUITE_DIR` / `ORTHIC_SHARED_DIR` / `RIGHTKIT_APP_DATA_DIR` forwards to the sidecar; and the source-checkout sidecar lookup (`CARGO_MANIFEST_DIR`). `resolve_engine_bin` looks only next to the executable and in the macOS bundle `Resources`. Release behaviour is unchanged. A build that enables `rightkit-dictation-app-runtime/test-mode` no longer resolves: drop the feature.
- Also: Adrian, 2026-11-03: "no debug assertions".
- Why: Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-app-runtime` (tauri-app-next).
