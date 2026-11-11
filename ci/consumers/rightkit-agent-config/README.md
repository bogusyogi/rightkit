# rightkit-agent-config 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-config` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency, no committed `Cargo.lock`
and no other dependency.

Exercises the configuration layers without touching the real user's state: a TOML layer parsed with
`ConfigLoader::load_str`, layers folded with `load_layered` (later scalars win, lists extend without duplicates),
`extend_dedup`, the workspace trust key and a trust store read from an explicit file, `load_presets_from` from
explicit files (an untrusted project's presets are ignored, a trusted project's bypass mode is dropped), the
licence-variable name filter, and `SessionSecrets` (key validation, key names only in `Debug`, injection into an
environment map).

State: a per-process directory under the OS temp directory, removed at the end. No network, no process spawn,
no real user data. `state_root`, `save_user`, `update_user*`, `save_workspace`, the default trust store and
`load_presets` read or write the real state root and are never called.

Proof row: public API surface of the new crate (moved from `rightkit-app-settings` 0.3.5 `agent_config`). Each
check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
