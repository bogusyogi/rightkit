# Changelog

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
