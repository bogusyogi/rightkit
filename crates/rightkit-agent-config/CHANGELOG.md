# Changelog

## 0.1.5

Additive: a strict superset of the CodeRight private crate `coderight-config`. No existing public item is removed, renamed or re-typed, and **defaults unchanged**: every behaviour below that differs from before is selected by a once-per-process host setter (each returns `false` if already set), so a host that calls none of them behaves exactly as 0.1.4. New fields of the config structs are `#[serde(default)]` with the values CodeRight has, so existing config files read as before; a config the host saves now also writes the new keys at their defaults (a 0.1.4 reader ignores them). `ConfigLoader::load_files_for_workspace` without the hardened loader takes `[hooks]`, `[compaction]`, `tui.quick-slots` and `tui.statusline-command` from the user file only, never from a workspace file, as 0.1.4 (which did not know them) did. One change applies to every host: the `AccountStore` writers hold a lock file for the read-change-write and report a lock that cannot be taken as an I/O error. No new dependency.

Host setters (call once at start-up, before anything loads configuration):

- `set_budget_defaults(BudgetDefaults) -> bool`, `budget_defaults() -> BudgetDefaults`, `BudgetDefaults` (`Default` = the 0.1.4 values, `BudgetDefaults::no_ceiling()` = every dollar key `0.0`, wall clock 10 minutes): the `Default` of `BudgetConfig` and the serde default of every budget key read them. With a `0.0` default for a dollar key, `HostConfig::validate` accepts `0.0` for that key (zero means no cap).
- `set_hardened_config_loader() -> bool`: `ConfigLoader::load_files_for_workspace` and `load_files_default` read layers as `load_files_for_workspace_granted` does (1 MiB cap, regular files only, workspace link refused, parse errors name a line and column and never the text, invalid user layer moved aside, invalid workspace layer ignored and left in place, linked worktree uses its project's config, untrusted workspace layer reduced to `[prompt]` and cosmetic `[tui]`, explicitly written scalars stand at their default, summarizer consent never from a workspace file, `-c` overrides); `ConfigLoader::update_user` starts from `load_user_file`; `ConfigSource::effective_for_workspace` reads layers the same way.
- `set_hardened_instructions() -> bool`: `InstructionLoader::discover` and `discover_gated` use the project walk, the trust gate on every project file kind, `AGENTS.override.md`, name-sorted rule folders, the per-scope (32 KiB) and total (96 KiB) caps with one cap marker entry, `--ignore-rules`, and one logged line per unreadable file.
- `set_nearest_ancestor_trust() -> bool`: `workspace_trust_key` normalises (no trailing separator, Windows case fold); `is_workspace_trusted_in` lets the nearest recorded ancestor decide; `is_workspace_trusted` also honours `trust_workspace_for_this_process`, reads `workspace_trust_store_file()` (no environment override, no working-directory fallback) and trusts nothing without a state root. A rule set with `set_workspace_trust_rule` still replaces it.

Licence environment: `is_licence_env_name` now also refuses `<host env prefix>_LICENSE_`, `<host env prefix>_SIGNED_LICENSE_` and `<host env prefix>_DEVICE_` (from `env_prefix()`, so a host named `CODERIGHT` gets the `CODERIGHT_` ones without a call); the `RIGHTKIT_` ones stay refused for every host and the list never shrinks. For the default prefix the answer is unchanged. New: `licence_env::ENV_LICENSE_OWNER_EXE`, `ENV_LICENSE_OWNER_TEAM`, `ENV_LICENSE_OWNER_SIGNER` (the `RIGHTKIT_LICENSE_OWNER_*` names) and `licence_env::host_license_env_name(suffix)` (`<prefix>_LICENSE_<suffix>`).

New public items:

- Modules: `cli_flags` (`set_ignore_user_config`, `ignore_user_config`, `set_ignore_rules`, `ignore_rules`, `set_strict_config`, `strict_config`, `parse_override`, `set_overrides`, `add_override`), `definition_pins` (`PinCheck`, `pin_store_path`, `digest_of`, `digest_of_bytes`, `check_or_pin`, `clear_scope`, `clear_scopes_under`; store schema `<host slug>.definition-pins.v1`), `features` (`FeatureFlagRow`, `FeatureConfig::flag_rows`), `schema` (`config_json_schema`, `config_json_schema_text`, `config_schema_file()`; `$id`, title and description from the host naming).
- Root: `ensure_host_names(env_prefix, state_dir, app_name) -> Result<(), String>` (the three naming setters in one idempotent call: an unset name is set, an equal one accepted, a different one an error), `host_slug()` (the env prefix in lower case), `state_root_required()`, `os_home_dir_required()`, `IgnoredLayer`, `UnknownKeys`, `ConfigLoader::ignored_layers()`, `ConfigLoader::unknown_keys()`, `ConfigLoader::load_files_for_workspace_granted(workspace, granted)`, `ConfigLoader::load_user_file()`, `project_root_for_config`, `find_project_root`, `instruction_cap_marker_path()`, `InstructionLoader::add_project_walk`, `instructions::{INSTRUCTION_ROOT_CAP_BYTES, INSTRUCTION_TOTAL_CAP_BYTES, AGENTS_OVERRIDE_FILE}`.
- Workspace trust: `normalize_trust_key`, `trust_lookup_keys`, `trust_lookup_keys_with`, `nearest_trust_record`, `trust_workspace_for_this_process`, `is_workspace_registered`, `workspace_trust_store_file()` (no fallback; `workspace_trust_store_path() -> PathBuf` keeps its signature).
- Accounts: `LinkedProfile`, `LinkedProfilesFile`, `LinkedProfileStore`, `LINKABLE_PROVIDERS`, `AccountsError::InvalidPath`, `AccountsError::LabelInUse`; every `AccountStore` writer now holds a cross-process lock (`accounts.json.lock`) for its read-change-write.
- Presets: `ModelProfile`, `model_profile_from`, `model_profile_for`.
- Config types: `HostConfig::{model_limits, turn, loop_detector, hooks, compaction}`, `ModelLimitsConfig`, `ContextLimits`, `HostConfig::context_limits`, `HostConfig::context_limits_for`, `TurnConfig`, `LoopDetectorSettings`, `HooksConfig`, `CompactionConfig`, `TuiConfig::{quick_slots, statusline_command}`, `ModelConfig::{small_model, plan_model}`, `ProviderRouteConfig::{prompt_cache_key_mode, requires_non_null_assistant_content, tool_calls, parallel_tool_calls, protocol}` with `native_tool_calls()` and `wire_protocol()`, `ToolsConfig::eager_tools`, `FeatureConfig::{self_review, advisor}` with `self_review_enabled()` and `advisor_enabled()`, `AgentOverride::{effort, toolset, spawnable_roles, omit_instructions, compact_window_tokens}`, `OrchestrationConfig::{sibling_start_hold_ms, advisor_model}` with `sibling_start_hold_ms()` and `SIBLING_START_HOLD_MS_DEFAULT`. Adding fields to `ProviderRouteConfig` and `AgentOverride` breaks a struct literal that names every field: build them with `..Default::default()`.
- `ConfigLoader::load_str` drops out-of-range `[model-limits]` values (a key that did not exist before); `ConfigError` text for a missing state root names `app_name()` (unchanged for the default `Agent`).

## 0.1.4

- Publish the dependency requirements main already declares: `rightkit-roots` 0.3.0, where the published 0.1.3 requires 0.2.0; `rightkit-fs` 0.5.0, where the published 0.1.3 requires 0.4.9. The 0.1.3 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.3 (2026-11-16)

- `set_workspace_trust_rule(fn(&Path) -> bool)` (once per process, returns `false` if already set): a host with no trust-store file supplies the workspace-trust decision, which replaces the persisted-store lookup in `is_workspace_trusted`. Default unchanged. `is_workspace_trusted_in` still reads the store it is given.

## 0.1.2

- Requires `rightkit-agent-policy` 0.2.0 (dependency requirement raised; no API change here).

## 0.1.1

- Host naming options, defaults unchanged: `set_state_dir_name` / `state_dir_name()` (default `.rightkit`; the workspace config, presets, agents folder, trust-store fallback) and `set_env_prefix` / `env_prefix()` / `env_name(suffix)` (default `RIGHTKIT`; the model, permission-mode, web-search, Telegram and CLI-acquisition overrides in `apply_env`), beside the existing `set_app_name`. Call once at start-up. The licence and trust-store variables keep their `RIGHTKIT_` names.

## 0.1.0 (unpublished)

- New crate. The agent host configuration (layered TOML `HostConfig`, `ConfigLoader`, state root and app name, workspace trust store, instructions, presets, accounts, licence environment names, session secrets, OKF bundles) moved here from `rightkit-app-settings` 0.3.5, where it was the `agent_config` module behind the `agent_config` feature (added in 0.3.1, moved from CodeRight `coderight-config`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-shell-ui.md) found it is agent product configuration, not app settings, and that it pulled `rightkit-daemon` and `rightkit-agent-policy` into a crate every desktop app adopts.
- Paths: `rightkit_app_settings::agent_config::X` is now `rightkit_agent_config::X`; the module tree under it is unchanged (for example `rightkit_agent_config::state_root::state_root`).
- Feature: `agent_config-host-env` is now `host-env`. There is no `agent_config` feature; the whole crate is the configuration.
- Dependencies: `rightkit-roots` 0.1.0 replaces `rightkit-daemon` for `Suite` and `SuiteError`, which moved there in rightkit-daemon 0.8.0; nothing else from the daemon was used (2026-11-11 shape review, docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md). The state root is unchanged.
- No behaviour change.
