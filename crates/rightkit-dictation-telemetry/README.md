# rightkit-dictation-telemetry

Dictation telemetry: local utterance timelines, telemetry writer, crash dump capture, feedback and HTTP client for diagnostics, redacted before anything is written.

Moved into RightKit from `heardright-telemetry` (tauri-app-next) on 2026-11-01.

Part of the `rightkit-dictation` package. The host configures the feedback endpoint
(`feedback::configure_feedback_endpoint`) and, on Windows, the dump helper name
(`crash_dump::configure_dump_helper_name`); the data root comes from
`rightkit_dictation_core::app_paths`.
