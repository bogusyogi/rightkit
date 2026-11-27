//! Layered TOML configuration for an agent host app. See README.md.
//!
//! Merge precedence (later overrides earlier for scalars; lists extend):
//! 1. built-in defaults
//! 2. user config (`<state root>/config.toml`, the rightkit-roots AppRoot, see [`state_root`](fn@state_root))
//! 3. workspace config (`<workspace>/<state-dir>/config.toml`)
//! 4. env var overrides (`RIGHTKIT_*`, or the host's own prefix, see [`env_prefix`])
//! 5. session overrides (a passed-in map)
//!
//! A host may retire configuration keys it does not have ([`set_retired_config_keys`]).

pub mod accounts;
pub mod agent_config;
pub mod cli_flags;
pub mod definition_pins;
pub mod features;
pub mod instructions;
pub mod licence_env;
mod loader;
mod merge;
pub mod okf;
pub mod presets;
mod retired;
pub mod schema;
pub mod session_secrets;
pub mod state_root;
mod types;
pub mod workspace_trust;

pub use agent_config::{find_agents_dir, AgentMcpConfig, AgentMcpServer};
pub use instructions::{
    find_project_root, instruction_cap_marker_path, set_hardened_instructions, Frontmatter,
    InstructionFile, InstructionLoader, InstructionScope,
};
pub use loader::{
    project_root_for_config, set_hardened_config_loader, ConfigError, ConfigLoader, IgnoredLayer,
    UnknownKeys,
};
pub use merge::{
    extend_dedup, merge, merge_with_report, validate_config, MergeConflict, MergeReport, TypedMerge,
};
pub use okf::{
    compress_prose, emit_bundle as emit_okf_bundle, parse_bundle as parse_okf_bundle, OkfBundle,
    OkfBundleStats, OkfConcept, OkfEmitOptions, OkfError, OkfLink, OKF_VERSION,
};
pub use retired::{retired_config_keys, set_retired_config_keys, RETIRABLE_CONFIG_KEYS};
pub use state_root::{
    app_name, ensure_host_names, env_name, env_prefix, host_slug, os_home_dir,
    os_home_dir_required, set_app_name, set_env_prefix, set_state_dir_name, state_dir_name,
    state_root, state_root_is_isolated, state_root_required, APP_NAME, DEFAULT_ENV_PREFIX,
    DEFAULT_STATE_DIR_NAME,
};
pub use types::{
    budget_defaults, set_budget_defaults, AgentOverride, BooleanSetting, BudgetConfig,
    BudgetDefaults, ChannelProfileConfig, CompactionConfig, ConfigSource, ContextLimits,
    FeatureConfig, HooksConfig, HostConfig, LoopDetectorSettings, MemoryConfig, ModelConfig,
    ModelLimitsConfig, ModelToolInterfaceConfig, OrchestrationConfig, PermissionsConfig,
    PreGateCheckConfig, PromptConfig, Provenance, ProviderRouteConfig, RemoteConfig, RoutingConfig,
    SessionConfig, ToolsConfig, ToolsetConfig, TuiConfig, TurnConfig, ValidationError,
    WorkflowImportTrustedPublicKey,
};
pub use workspace_trust::{
    is_workspace_registered, is_workspace_trusted, is_workspace_trusted_in, nearest_trust_record,
    normalize_trust_key, set_nearest_ancestor_trust, set_workspace_trust_rule, trust_lookup_keys,
    trust_lookup_keys_with, trust_workspace_for_this_process, workspace_trust_key,
    workspace_trust_store_file, workspace_trust_store_path, WorkspaceTrustRule,
};
