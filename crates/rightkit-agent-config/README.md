# rightkit-agent-config

Layered TOML configuration for an agent host app (CodeRight and the RightKit agent crates). It resolves one `HostConfig` from, in order of precedence (later overrides earlier for scalars; lists extend):

1. built-in defaults
2. user config: `<state root>/config.toml`, where the state root is the rightkit-roots `AppRoot` (see `state_root`)
3. workspace config: `<workspace>/<state-dir>/config.toml`
4. environment overrides (`RIGHTKIT_*`)
5. session overrides (a map passed in)

It also owns the host's state root and app name (`state_root`, `set_app_name`), the workspace trust store (`is_workspace_trusted`, `workspace_trust_store_path`), instruction files (`InstructionLoader`), presets and model profiles, subscription accounts and linked profile folders, licence environment names, command-line config flags (`cli_flags`), trust-on-first-use definition pins (`definition_pins`), the feature-flag table (`features`), the JSON Schema derived from `HostConfig` (`schema`), session-scoped secrets (never serialized) and the OKF knowledge bundle format.

Behaviours that differ from the defaults are chosen once per process by the host at start-up (`set_hardened_config_loader`, `set_hardened_instructions`, `set_nearest_ancestor_trust`, `set_budget_defaults`, `set_retired_config_keys`); a host that calls none of them gets the defaults described in CHANGELOG.md. `set_retired_config_keys` retires configuration keys the host does not have: a retired key is not read, not written, not in the schema, not validated and reported as an unknown key.

**Use it** for an agent host's product configuration. **Do not use it** for an app's own settings file: that is `rightkit-app-settings`, which this crate uses for its atomic writes.

## Features

- `host-env`: read host environment overrides, such as `RIGHTKIT_WORKSPACE_TRUST_STORE` for the trust store location. Off by default.

## Origin

The code first moved here from `rightkit-app-settings` 0.3.x, where it was the `agent_config` module behind the `agent_config` and `agent_config-host-env` features, and has grown since (see CHANGELOG.md for each version).

## License

MIT OR Apache-2.0.
