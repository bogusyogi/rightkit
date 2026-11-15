# Changelog

## 0.2.1

HeardRight's runtime is compared with this crate and every difference in its output is taken; nothing changes for a host that makes no call below.

- `host::set_product_name(&str)` (once per process; returns `false` when already set) names the product in the pill label shown when Windows refuses microphone access ("Microphone access is off for `<name>` in Windows Settings"; default "this app"). `host::product_name()` returns it.
- `host::set_engine_name(&str)` (once per process) names the engine process in the message logged when it crashes ("`<name>` crashed"; default "engine crashed").
- Compared and kept: the final-delivery policy, focus pins and start-field capture that HeardRight's runtime holds inline stay in the host (`DictationHost::deliver_final` and the `capture_*` methods).

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-dictation-app-runtime` 0.1 -> 0.2, `rightkit-dictation-command-host` 0.1 -> 0.2, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-models` 0.1 -> 0.2, `rightkit-dictation-telemetry` 0.1 -> 0.2. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Removed (breaking): the `*_for_test` hooks (`apply_sidecar_control_tail_for_test`, `watchdog_progress_event_for_test`, `transcribe_watchdog_due_for_test`, `state_emission_permitted_for_test`, `stale_error_expiry_rejected_for_test`, `shared_state_clone_shares_for_test`, `lock_free_facades_absent_for_test`) and the `controller` re-export (the module is gone from rightkit-dictation-core 0.2.3). Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore."
- Removed (breaking, rightkit#1437): the `test-mode` feature (and its forwarding to `rightkit-dictation-core` and `rightkit-dictation-app-runtime`); the runtime-lock depth assertions; the `RIGHTKIT_DICTATION_ENGINE`, `RIGHTKIT_SIDECAR_AUTORESTART` and `RIGHTKIT_HISTORY` switches; and the in-process `RuntimeApp::Fake` runtime with its fixed "Test transcript" engine. `RuntimeApp` is the engine runtime only. Release behaviour is unchanged. A build that enables `rightkit-dictation-tauri/test-mode` no longer resolves: drop the feature.
- Why: Adrian, 2026-11-03: "no debug assertions"; "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight. Platform delivery, focus capture, history stores,
  settings, vocabulary, sync and media control are reached through `host::DictationHost`.
- Environment switches renamed to `RIGHTKIT_DICTATION_ENGINE`, `RIGHTKIT_SIDECAR_AUTORESTART`, `RIGHTKIT_HISTORY`.
- `watchdog_tick` takes the state-emission closure instead of an app handle.
- The microphone-denied pill label no longer names the product.
