# Changelog

## Unreleased

- Removed `l3_cleanup::overrides`, the live-evaluation stand-in (`LiveEvalLane`, `run_live_evaluation`, `render_prompt_convergence_payloads_json`), the roster/NVIDIA/OpenRouter opt-in routing, the environment key fallback and the `RIGHTKIT_*` reads for language and polish gates; fixed product values remain (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-text-pipeline` (tauri-app-next).
