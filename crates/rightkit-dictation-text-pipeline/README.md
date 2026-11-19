# rightkit-dictation-text-pipeline

Dictation text pipeline: LLM cleanup (L1-L3) with provider transports and policy, provider keys, runtime settings, screen-context vocabulary and the text polish orchestration.

Moved into RightKit from `heardright-text-pipeline` (tauri-app-next) on 2026-11-01.

Call `settings::configure_disk_settings` at startup to read with the same store
options the host uses for writes. Engine IPC calls `settings::replace_runtime_config`
for live changes. The native configuration takes precedence once received.
