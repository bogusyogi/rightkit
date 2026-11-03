# rightkit-dictation-core

Dictation core: engine wire protocol and validation, provider key handoff, recording session state
machine, wake word state, mic selection and audio conditioning.

Moved into RightKit from HeardRight (`heardright_core`: `engine`, `state`, `wake`,
`mic_selection`, `audio_conditioning`) on 2026-11-01. First part of the `rightkit-dictation`
package, split along the app's crate boundaries.

- `engine`: `EngineFrame` and payloads of the versioned shell/engine JSON protocol,
  `validate_frame`, the launch frame, recording delivery policy, `SecretString`, and the provider
  key handoff (`provider_handoff`) that scrubs ambient key variables from the engine process.
- `state`: the recording session state machine.
- `settings`, `app_paths`: the settings document and the data root for the folder name the host
  configures with `app_paths::configure_app_data_dir_name`.
- `redact_diagnostic_event` / `redact_payload_*`.
- `wake`, `mic_selection`, `audio_conditioning`: wake listener state, capture device choice and
  PCM conditioning.

GUI-free and process-free; the engine process, OS capture and delivery stay in the host. The
launch frame keeps its existing wire id (`heardright.engine.launch`) so shells and engines already
in the field stay compatible.
