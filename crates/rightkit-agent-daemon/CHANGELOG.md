# Changelog

## 0.1.0 (unpublished)

- New crate. The agent host's daemon primitives moved here from `rightkit-daemon` 0.5.4, where they were two modules behind two features (moved from CodeRight `coderight-daemon-core` and `coderight-daemon-runtime`): `agent_daemon_core` (the `x-rightkit-agent-source-trust` authority gate, `DaemonSettingsStore`, `EngineConfig`, `LiveTurnHandle` and `LiveTurnGuard`) and `agent_daemon_runtime` (the `lifecycle` module: `LifecycleCommand` parsing, `LifecycleAuthority` and the `LifecycleBackend` trait). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md) found it is CodeRight's daemon code and pulled `axum`, `tokio`, `async-trait` and `rightkit-agent-engine-protocol` into a crate every desktop app adopts.
- Paths: `rightkit_daemon::agent_daemon_core::X` and `rightkit_daemon::agent_daemon_runtime::X` are now `rightkit_agent_daemon::X`; the lifecycle items are also under `rightkit_agent_daemon::lifecycle::X` (for example `rightkit_agent_daemon::lifecycle::LifecycleAuthority`).
- Features: there are no `agent_daemon_core` or `agent_daemon_runtime` features. The only consumer, `rightkit-agent-server`, enabled both, so the whole crate is always compiled.
- No behaviour change.
