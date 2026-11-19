# rightkit-dictation-engine

Dictation engine: the speech engine sidecar binary and library facade over the dictation runtime, inference, text pipeline and command host, with engine contract tests and fixtures.

Moved into RightKit from `heardright-engine` (tauri-app-next) on 2026-11-01.

For a host-branded sidecar, call `configure_host` with the app's configuration
before starting the runtime. Handle a configuration error as a startup failure.

`guardrail_replay --input <path> --output <path> [--strict]` checks transcript
pairs against the production output acceptance rules. Supply the transcript
and candidate output as JSONL. This replay starts after vocabulary bias and
homophone repair, so it uses the same input for any host.
