# rightkit-dictation-tauri

Dictation runtime for a desktop shell. It owns the recording state machine over the engine
sidecar (`RuntimeApp`, `EngineRuntime`), history sequencing, the transcribe watchdog, start-sequence
capture liveness, label mapping for capture errors, and the single global runtime lock. Everything
that belongs to the application or the platform is reached through `host::DictationHost`.

Moved into RightKit from HeardRight (`src-tauri`: `runtime/*`, `runtime`, `runtime_lock`,
`start_sequence`, `dictation_error`, `engine_event_consumer`). Part of the `rightkit-dictation`
package, split along the app's module boundaries.

## Host

Install a host once before dictation starts:

```rust,ignore
rightkit_dictation_tauri::host::install_host(std::sync::Arc::new(MyHost));
```

`DictationHost` requires settings, onboarding state and the two history stores (JSON and
encrypted); every other service has an inert default. The host also implements the platform
destination work: freezing the start and stop destinations (`capture_*_destination`, with opaque
`HostPin` focus pins) and choosing where the final transcript goes (`deliver_final`). The runtime
sequences these calls; it never touches an accessibility API itself.

## No environment switches

The runtime is always the supervised engine sidecar with encrypted history. There is no test mode, no fake engine and no environment selector (rightkit#1437).

## Not included

The Tauri command layer, pill and hub window orchestration, tray, hotkey wiring and the
Windows low-level cancel hook stay in the application: they need the shell's windows.

## Session modules (0.3.0)

`state_decisions`, `state_emission` & `focus_coverage` expose session decisions through
host callbacks. `start_destination` captures platform destination proofs; `final_delivery`
applies final-transcript policy through a host-supplied command-host `DeliveryHost`.
Hosts call `final_delivery::deliver_final_for_host` from their `DictationHost::deliver_final`
override. Default final delivery returns clipboard-preserved recovery without insertion.

Manual repaste calls `DictationHost::delivery_backend` once, stores its outcome & returns
success only for `DeliveryOutcome::Pasted`. A refused or unconfirmed outcome stays available
in history for explicit recovery; it causes no second paste or clipboard write. Explicit Copy
uses host last-delivery/history state & remains independent of engine availability.
Hosts own recovery wording, UI triggers & emission after a repaste error.

Last-paste casing requests from the engine reset the session and surface a command
failure through the host. Support requires the destination editor to verify and
replace the prior insertion in one operation. This release leaves the text in
place and retains the original delivery in history.

Streaming hosts return `host::StreamingFinal`: `NothingWritten` allows normal delivery;
`Reconciled { remainder }` inserts only missing text; `Stopped { tag, cause }` records
full-transcript recovery with clipboard preserved, skipping final delivery & Return.
All records retain full transcript after reconciled streaming, including recovery records.

## Shell integration

`shell::DictationShell` owns per-handle operations: renderer events, state mirrors,
recovery surfaces, wake listener, notices & launch callbacks. Pass `shell::Shell`
(`Arc<dyn DictationShell>`) into `engine_events`, `lifecycle`, `hotkey_wiring` &
`dictation_control`. `renderer_routes` validates host manifests & encodes routed payloads.
Hosts retain window/event names, copy, visual cues, hotkey bindings & route manifests.

`DictationHost::history_export_labels` supplies required export wording. Other new host
methods supply onboarding/launch gates, pill cues, shared-model setup, history settings,
platform hook callbacks & focus coverage. macOS hotkey wiring uses `rightkit-hotkeys` 0.1.6;
history exports use `rightkit-fs` 0.4.17 with `fs`; renderer encoding uses `serde` derive
& `rightkit-crypto` 0.1.3.

Partial streaming holds runtime's session mutex through its host callback. Only recording
session's increasing revisions from its acknowledged engine generation reach input; stale
session events never replace a stopped binding. Host callback must not reenter runtime or
acquire backend transition gate. Recovery drafts remain eligible after stop.

## Host policy & renderer adapters (0.3.2)

`asr_compat` lists captured WAVs, reads duration, selects samples & evaluates timed
transcripts using host thresholds. `capture_folder` migrates host-resolved capture
paths. `command_outcome` classifies engine action/detail pairs; hosts supply labels.
`hotkey_gates` applies onboarding/test/launch inputs. `settings_policy::BackendPolicy`
resolves host backend aliases & entitlement policy; language & choice helpers accept
host defaults. These modules need no Tauri feature.

Enable `features = ["tauri"]` on macOS/Windows shell consumers to use
`renderer_events::{RendererEvents, native_drag_drop_payload}`. This optional Tauri 2
dependency disables Tauri default features; hosts select their runtime features.
`RendererEvents::new` accepts static manifest JSON, schema version, allowed window
labels & dispatch channel. `emit` reuses `renderer_routes` validation/encoding,
tries every routed window, skips absent windows & returns first delivery error.
Unknown events or invalid manifests fail before delivery. Native drag/drop payloads
retain tagged enter/over/drop/leave shapes, physical coordinates & lossy UTF-8 paths.
Hosts retain UI-dispatch lock checks, route manifests, window/event names & copy.
