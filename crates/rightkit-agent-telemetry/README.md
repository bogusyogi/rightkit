# rightkit-agent-telemetry

Agent runtime telemetry for an agent host (CodeRight and the RightKit agent crates). It keeps bounded, in-process recorders and the contracts that render them, and never fabricates a number: a metric that was not observed stays `None` and renders as "not measured".

- `resource`: turn latency percentiles, provider round trips per task, SQLite op latency, event-loop delay and daemon wakeups (`record_*`, `*_summary`, `reset_resource_telemetry`).
- `prompt_overhead`: estimated tokens of the system prompt and tool prelude per provider request, and cache-prefix events.
- `context_ceiling`: the remaining-context ceiling record (`RemainingContextCeilingV1`).
- `evidence`: tool schema, procedural asset and evaluation outcome observations with provenance.
- `repair`: validator repair counters.
- `measurement`: a measurement receipt for one scenario run (`MeasurementReceipt`, `ResourceMeasurement`).
- `resource_report`: the OPS-009 resource-report contract that keeps host-only figures apart from whole-system ones.
- `agent_observability`: per-agent start, end and failure records (`AgentObservationV1`). `baseline`: the S0/S1/S10/S50 baseline table instrument.
- `init`: installs `rightkit-logs` stderr tracing from `RUST_LOG` (default `info`).

**Use it** in an agent host to record and report its own runtime cost. **Do not use it** for app logs or diagnostics: that is `rightkit-logs`, which this crate uses for its tracing install.

## Features

None. The whole crate is the telemetry.

## Origin

The code moved here unchanged from `rightkit-logs` 0.3.x, where it was the `agent_telemetry` module behind the `agent_telemetry` feature (see CHANGELOG.md).

## License

MIT OR Apache-2.0.
