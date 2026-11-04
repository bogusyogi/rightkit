//! host_config — layered TOML configuration for the host app.
//!
//! Merge precedence (later overrides earlier for scalars; lists extend):
//! 1. built-in defaults
//! 2. user config (`<state root>/config.toml`, the rightkit-daemon AppRoot, see [`state_root`](fn@state_root))
//! 3. workspace config (`<workspace>/<state-dir>/config.toml`)
//! 4. env var overrides (`RIGHTKIT_*`)
//! 5. session overrides (a passed-in map)

pub mod accounts;
#[allow(clippy::module_inception)] // public path agent_config::agent_config is part of the API
pub mod agent_config;
pub mod instructions;
pub mod licence_env;
mod loader;
mod merge;
pub mod okf;
pub mod presets;
pub mod session_secrets;
pub mod state_root;
mod types;
pub mod workspace_trust;

pub use agent_config::{find_agents_dir, AgentMcpConfig, AgentMcpServer};
pub use instructions::{Frontmatter, InstructionFile, InstructionLoader, InstructionScope};
pub use loader::{ConfigError, ConfigLoader};
pub use merge::{
    extend_dedup, merge, merge_with_report, validate_config, MergeConflict, MergeReport, TypedMerge,
};
pub use okf::{
    compress_prose, emit_bundle as emit_okf_bundle, parse_bundle as parse_okf_bundle, OkfBundle,
    OkfBundleStats, OkfConcept, OkfEmitOptions, OkfError, OkfLink, OKF_VERSION,
};
pub use state_root::{
    app_name, os_home_dir, set_app_name, state_root, state_root_is_isolated, APP_NAME,
};
pub use types::{
    AgentOverride, BooleanSetting, BudgetConfig, ChannelProfileConfig, ConfigSource, FeatureConfig,
    HostConfig, MemoryConfig, ModelConfig, ModelToolInterfaceConfig, OrchestrationConfig,
    PermissionsConfig, PreGateCheckConfig, PromptConfig, Provenance, ProviderRouteConfig,
    RemoteConfig, RoutingConfig, SessionConfig, ToolsConfig, ToolsetConfig, TuiConfig,
    ValidationError, WorkflowImportTrustedPublicKey,
};
pub use workspace_trust::{
    is_workspace_trusted, is_workspace_trusted_in, workspace_trust_key, workspace_trust_store_path,
};
