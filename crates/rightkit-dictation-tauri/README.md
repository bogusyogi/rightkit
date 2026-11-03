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
