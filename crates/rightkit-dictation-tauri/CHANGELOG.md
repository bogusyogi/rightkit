# Changelog

## 0.3.3
- Add `file_transcribe` (`preflight_media_file`, `host_read_check`, `files_settings_url`, `validate_export_path`, `write_text_export`, default extension lists): media-file preflight and guarded transcript export. Host passes extension lists & app name. Additive.
- Add `asr_compat_report` (`run_sample_check`, `report_for`, `CompatReport`, `CompatLabels`, `SampleTranscript`): the serialisable compatibility result and the whole captured-sample check over `asr_compat`; host passes thresholds, profiles & message templates. Additive.
- Add `engine_cache` (`recompile_resident_cache`, `selected_model_missing`, `streaming_final_from`): composed cache-clear-and-restart under the backend-transition gate, selected-model-missing check & `ReconcileOutcome` to `StreamingFinal` conversion. Additive.
- Add `onboarding_gates` (`registration_gate`, `registration_refusal`, `enable_onboarding_dictation_test`) for hotkey registration after onboarding. Additive.
- Add `RendererEvents::emit_on_main_thread` (feature `tauri`) for emits from input-hook threads with a completion callback. Additive.

## 0.3.2

- Add host-configured ASR sample evaluation, capture-folder migration, command-outcome classification, hotkey gates & settings policy.
- Add opt-in `tauri` feature for routed renderer delivery & native drag/drop conversion over existing `renderer_routes`; hosts keep manifests, window/event names & copy.

## 0.3.1

- Pair native release edges with their capture attempt; keep wake word and existing activation routes available.

## 0.3.0

- Serialize partial streaming with session stop/cancel/start; reject stale sessions, duplicate revisions & changed engine generations before host input. Keep post-stop drafts available for recovery.

- Shared repaste control emits stored recovery/history on refusal without another effect; renderer channel names are JSON encoded; new event/hotkey diagnostics classify errors without content.
- Add shared shell/event/lifecycle/hotkey/control/renderer routing paths consumed by current app wrappers. `DictationShell` carries per-handle operations; `DictationHost` adds host policy, platform callbacks & required history-export labels. Copy, theme, event/window names & composition stay in host.

- Breaking host contract: `streaming_take_for_final` returns `StreamingFinal` (`NothingWritten`, `Reconciled { remainder }`, `Stopped { tag, cause }`). Stopped or uncertain prior writes bypass final delivery & Return, preserving full transcript with clipboard untouched. Reconciled streams insert only remainder; recovery/history keep full transcript.
- Add donor `dictation_error::error_log_code` & `history_error_log_code`, already called by current app command/event paths.
- Add host-configured final delivery, destination capture, focus coverage, state decisions & serialized state emission modules. App copy, theme & composition remain host-owned.
- Repaste uses host delivery backend once, persists its returned outcome & succeeds only for a confirmed paste. Copy reads host last-delivery/history state directly. Cancelled, draft, synced & file rows remain excluded from restored manual delivery.
- Default final delivery preserves clipboard & returns a recovery record until host supplies delivery policy. It never calls insertion callback.
- Raise command-host, models & app-runtime floors to 0.3.0, core to 0.3.2, telemetry to 0.2.2 & transcript-library to 0.1.4. Add automation 0.7.1 for platform destination proofs.
- Last-paste casing requests return a command failure. The shell needs an editor-authoritative, conditional replacement of the prior insertion before it can support this operation.

## 0.2.2

- Sidecar state-probe mismatch logs omit reply contents & session identifiers, including transcript-bearing replies from older peers & unknown state strings.

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
