# rightkit-agent-daemon

Daemon primitives for an agent host (CodeRight and the RightKit agent server). They do not depend on the daemon binary, so the server and any extracted domain crate can share them.

- `enforce_trusted_source_authority` and `lower_source_trust`: the `x-rightkit-agent-source-trust` gate. A caller already past the daemon's bearer token may mark a request as lower trust (`ContentTrust::RemoteAgentUntrusted` or `ToolUntrusted`); a marked request is refused with `403`. An absent header means trusted (see the decision matrix in `src/authority.rs`).
- `DaemonSettingsStore` and `EngineConfig`: daemon-wide spend caps, retry, wall-clock, iteration and worker bounds, and the engine's model and endpoint. In memory only; persistence stays with the host.
- `LiveTurnHandle` and `LiveTurnGuard`: whether a session owns an engine turn right now, fenced by a turn generation so a late guard from an old turn cannot mark a live session idle.
- `lifecycle`: `parse_lifecycle_command` (start, attach, detach, stop, restart, update, schedule and process-list commands), and one `LifecycleAuthority` that serialises every command and runs it through a host-supplied `LifecycleBackend`.

**Use it** in an agent host daemon. **Do not use it** for a Right Suite local service, discovery or the loopback transport: that is `rightkit-daemon`.

## Features

None. The whole crate is the agent daemon layer.

## Origin

The code moved here unchanged from `rightkit-daemon` 0.5.x, where it was the `agent_daemon_core` and `agent_daemon_runtime` modules behind the features of the same names (see CHANGELOG.md).

## License

MIT OR Apache-2.0.
