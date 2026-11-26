# Changelog

## 0.2.8

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.2.7 requires 0.4.9. The 0.2.7 manifest on crates.io stays as published; this version carries the new requirements.

## 0.2.7

- Keep the last configured disk settings when an open fails with a transient I/O error. A later successful read replaces the retained settings.

## 0.2.6

- `settings::configure_disk_settings` accepts host store options for read-only settings with per-field repair. Native runtime configuration takes precedence over disk reads, including a push that arrives during a read.

## 0.2.5 (2026-11-16)

- Requires rightkit-llm 0.7.0. No behaviour change.

## 0.2.4

HeardRight's cleanup transport is compared with this crate and every difference in its output is taken; nothing changes for a host that makes no call below.

- `l3_cleanup::configure_wire_identity(WireIdentity)` (once per process; returns `false` when already installed) sets the names this crate puts on the wire: the `User-Agent` of provider calls, the prompt-version request header and the prefix of the one-line JSON runtime diagnostic written to stderr. `wire_identity()` returns the ones in force. `WireIdentity::DEFAULT` is the old behaviour (`rightkit-dictation/0.1`, `X-RightKit-AI-Prompt-Version`, `RIGHTKIT_RUNTIME_DIAGNOSTIC_JSON=`). HeardRight's are `heardright-engine/0.1`, `X-HeardRight-AI-Prompt-Version`, `HR_RUNTIME_DIAGNOSTIC_JSON=`.
- Compared and kept: HeardRight's `l3_cleanup/overrides.rs` answers "no override" for every environment lever in a release build, so the provider roster (Groq only), timeouts, circuit breaker and OpenRouter routing it leaves are the constants this crate already uses; its live-evaluation hooks are release stubs.

## 0.2.3

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.2

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.1

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-logs` 0.3 -> 0.4. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Removed `l3_cleanup::overrides`, the live-evaluation stand-in (`LiveEvalLane`, `run_live_evaluation`, `render_prompt_convergence_payloads_json`), the roster/NVIDIA/OpenRouter opt-in routing, the environment key fallback and the `RIGHTKIT_*` reads for language and polish gates; fixed product values remain (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-text-pipeline` (tauri-app-next).
