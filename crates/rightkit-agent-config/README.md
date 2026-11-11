# rightkit-agent-config

Layered TOML configuration for an agent host app (CodeRight and the RightKit agent crates). It resolves one `HostConfig` from, in order of precedence (later overrides earlier for scalars; lists extend):

1. built-in defaults
2. user config: `<state root>/config.toml`, where the state root is the rightkit-roots `AppRoot` (see `state_root`)
3. workspace config: `<workspace>/<state-dir>/config.toml`
4. environment overrides (`RIGHTKIT_*`)
5. session overrides (a map passed in)

It also owns the host's state root and app name (`state_root`, `set_app_name`), the workspace trust store (`is_workspace_trusted`, `workspace_trust_store_path`), instruction files (`InstructionLoader`), presets, subscription accounts, licence environment names, session-scoped secrets (never serialized) and the OKF knowledge bundle format.

**Use it** for an agent host's product configuration. **Do not use it** for an app's own settings file: that is `rightkit-app-settings`, which this crate uses for its atomic writes.

## Features

- `host-env`: read host environment overrides, such as `RIGHTKIT_WORKSPACE_TRUST_STORE` for the trust store location. Off by default.

## Origin

The code moved here unchanged from `rightkit-app-settings` 0.3.x, where it was the `agent_config` module behind the `agent_config` and `agent_config-host-env` features (see CHANGELOG.md).

## License

MIT OR Apache-2.0.
