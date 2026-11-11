# Changelog

## 0.1.0 (unpublished)

- New crate. The agent runtime telemetry (resource metrics, turn latency, round trips, SQLite op latency, wakeups, prompt overhead, context ceiling, evidence, repair counters, measurement receipts, the resource-report contract and the baseline instrument) moved here from `rightkit-logs` 0.3.10, where it was the `agent_telemetry` module behind the `agent_telemetry` feature (moved from CodeRight `coderight-telemetry`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md) found it is CodeRight agent product code, not logging, inside a crate every desktop app adopts.
- Paths: `rightkit_logs::agent_telemetry::X` is now `rightkit_agent_telemetry::X`; the module tree under it is unchanged (for example `rightkit_agent_telemetry::resource::record_turn_latency`).
- Features: there is no `agent_telemetry` feature; the whole crate is the telemetry. `init` needs `rightkit-logs`' `tracing` feature, which this crate enables (the old feature relied on a consumer enabling it).
- No behaviour change.
