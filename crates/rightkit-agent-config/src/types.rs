//! Configuration data shapes for the host app.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConfigSource {
    Default,
    User,
    Workspace,
    Session,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanSetting {
    SessionDisableThinking,
    RemoteWebEnabled,
    ConductorRouting,
    CapabilityRouter,
}

impl ConfigSource {
    /// Which layer set `setting` for `workspace`. After [`crate::set_hardened_config_loader`] the
    /// layers are read as the hardened loader reads them (1 MiB cap, regular files only, a
    /// workspace link refused) and an untrusted workspace's layer is reduced exactly as the loader
    /// reduces it, so it is never reported as the source of a value it does not provide.
    pub fn effective_for_workspace(workspace: &Path, setting: BooleanSetting) -> Self {
        let loader = crate::ConfigLoader::new();
        let (user, workspace) = if crate::loader::hardened_loader_selected() {
            let user = crate::state_root::state_root()
                .map(|root| root.join("config.toml"))
                .and_then(|path| {
                    crate::loader::read_layer_bytes(&path, crate::loader::LayerKind::User)
                        .ok()
                        .flatten()
                })
                .and_then(|content| loader.load_str(&content).ok());
            let trusted = crate::is_workspace_trusted(workspace);
            let workspace = crate::loader::read_layer_bytes(
                &workspace.join(crate::state_dir_name()).join("config.toml"),
                crate::loader::LayerKind::Workspace,
            )
            .ok()
            .flatten()
            .and_then(|content| loader.load_str(&content).ok())
            .map(|config| {
                if trusted {
                    config
                } else {
                    crate::loader::untrusted_workspace_layer(config)
                }
            });
            (user, workspace)
        } else {
            let user = crate::state_root::state_root()
                .map(|root| root.join("config.toml"))
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|content| loader.load_str(&content).ok());
            let workspace = std::fs::read_to_string(
                workspace.join(crate::state_dir_name()).join("config.toml"),
            )
            .ok()
            .and_then(|content| loader.load_str(&content).ok());
            (user, workspace)
        };

        if workspace
            .as_ref()
            .is_some_and(|config| setting.is_set(config))
        {
            Self::Workspace
        } else if user.as_ref().is_some_and(|config| setting.is_set(config)) {
            Self::User
        } else {
            Self::Default
        }
    }
}

impl BooleanSetting {
    fn is_set(self, config: &HostConfig) -> bool {
        match self {
            Self::SessionDisableThinking => config.session.disable_thinking.is_some(),
            Self::RemoteWebEnabled => config.remote.web_enabled.is_some(),
            Self::ConductorRouting => config.orchestration.conductor_routing.is_some(),
            Self::CapabilityRouter => config.orchestration.routing.capability_router.is_some(),
        }
    }
}

/// Top-level the host app configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct HostConfig {
    pub model: ModelConfig,
    pub provider_routes: HashMap<String, ProviderRouteConfig>,
    /// Context limits keyed by provider-route key or model id (`[model-limits]`).
    pub model_limits: HashMap<String, ModelLimitsConfig>,
    pub session: SessionConfig,
    pub permissions: PermissionsConfig,
    pub tools: ToolsConfig,
    pub features: FeatureConfig,
    pub toolsets: HashMap<String, ToolsetConfig>,
    pub channel_profiles: HashMap<String, ChannelProfileConfig>,
    pub remote: RemoteConfig,
    pub orchestration: OrchestrationConfig,
    pub prompt: PromptConfig,
    /// `[memory]`. A host that retires it ([`crate::set_retired_config_keys`]) neither reads nor
    /// writes the table.
    #[serde(
        deserialize_with = "crate::retired::read_memory",
        skip_serializing_if = "crate::retired::skip_memory"
    )]
    pub memory: MemoryConfig,
    pub tui: TuiConfig,
    /// Root-turn limits (`[turn]`).
    pub turn: TurnConfig,
    /// Repeated-call guard thresholds (`[loop-detector]`).
    pub loop_detector: LoopDetectorSettings,
    /// Lifecycle hooks (`[hooks]`).
    pub hooks: HooksConfig,
    /// Compaction summarizer routing (`[compaction]`).
    pub compaction: CompactionConfig,
}

/// Compaction summarizer routing (`[compaction]` in `config.toml`).
///
/// ```toml
/// [compaction]
/// builtin-summarizer-routes = true   # let /compact send the transcript to the built-in third-party summarizer models
/// ```
///
/// Unset or `false`: the summary is written by the session's own model on every connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct CompactionConfig {
    pub builtin_summarizer_routes: bool,
}

/// Lifecycle hooks (`[hooks]` in `config.toml`). One table keyed by event name, each value an
/// ordered list of entries:
///
/// ```toml
/// [hooks]
/// PreToolUse = [{ matcher = "shell|write_file", command = "audit.sh", timeout-secs = 10 }]
/// PostToolUse = [{ type = "url", command = "https://example.test/hook" }]
/// ```
///
/// Event names stay in their own case. The workspace layer's `[hooks]` is honoured only in a
/// trusted workspace; the user layer always.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct HooksConfig(pub serde_json::Map<String, serde_json::Value>);

impl HooksConfig {
    /// The table as one JSON document, or `None` when no hook is configured.
    pub fn as_document(&self) -> Option<serde_json::Value> {
        (!self.0.is_empty()).then(|| serde_json::Value::Object(self.0.clone()))
    }
}

/// Root-turn limits (`[turn]` in `config.toml`).
///
/// ```toml
/// [turn]
/// max-wall-clock-minutes = 90   # optional; unset means no wall-clock cap
/// ```
///
/// No peer caps a turn by wall clock, so there is no default: the idle and
/// no-progress limits and the repeated-call bound end a hung turn. This is a
/// different key from `orchestration.budget.max-wall-clock-minutes`, the
/// sub-agent run-length bound.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct TurnConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wall_clock_minutes: Option<u32>,
    /// Longest a tool that waits on a peer (tool servers, web fetch and search, hosted APIs)
    /// may run before it is cancelled with a timeout error, in seconds. Unset keeps the
    /// deadline derived from the turn's iteration budget (60 to 900 s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_timeout_secs: Option<u32>,
}

/// Repeated-call guard thresholds (`[loop-detector]` in `config.toml`). Unset keys keep the
/// built-in values.
///
/// ```toml
/// [loop-detector]
/// exact-stop-threshold = 5   # the same call (same arguments) this often in the window stops it
/// exact-warn-threshold = 3
/// kind-stop-threshold = 40   # one tool this often in the window stops it
/// kind-warn-threshold = 30
/// window-size = 40
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct LoopDetectorSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_warn_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_stop_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_warn_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_stop_threshold: Option<u32>,
}

/// Terminal UI preferences (`[tui]` in `config.toml`).
///
/// ```toml
/// [tui]
/// theme = "dark"          # auto | dark | light (dark base stays the neutral brand ramp)
/// notify = "osc9"         # bel | osc9 | off
/// vim = false
/// statusline = ["cwd", "branch", "model", "ctx", "cache", "cost", "goal"]
///
/// [tui.keymap]
/// sidebar = "ctrl+b"
/// projects = "ctrl+p"
/// config = "f2"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct TuiConfig {
    pub theme: String,
    pub notify: String,
    pub vim: Option<bool>,
    /// Use an alternate-screen chat surface with mouse interaction.
    pub fullscreen: Option<bool>,
    /// Copy transcript text automatically after terminal selection.
    pub copy_on_select: Option<bool>,
    /// Copy complete assistant response instead of opening code-block choices.
    pub always_copy_full_response: Option<bool>,
    /// Open live agents view when the TUI starts.
    pub agents_view_default: Option<bool>,
    pub statusline: Vec<String>,
    pub keymap: HashMap<String, String>,
    /// Numbered quick slots (`[tui.quick-slots]`, keys `1` to `9`): the text Alt+`<digit>` submits on an
    /// empty composer, or the slash command it runs when it starts with `/`.
    pub quick_slots: HashMap<String, String>,
    /// A program the terminal runs in the background for the footer item `script`: JSON on its stdin,
    /// the first line it prints shown in the footer. From the user config or a trusted workspace only.
    pub statusline_command: String,
}

/// Model selection and generation parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ModelConfig {
    pub default_model: String,
    pub fallback_models: Vec<String>,
    /// Cheaper model id on the session's own connection for assist calls (the goal progress audit).
    /// Empty uses the session's model.
    pub small_model: String,
    /// Model id on the session's own connection that answers while the session is in plan mode;
    /// the session's model answers again once the plan is approved. Empty keeps one model.
    pub plan_model: String,
    pub max_tokens: u32,
    pub temperature: f64,
}

/// Workspace/user-defined OpenAI-compatible provider route.
///
/// ```toml
/// [provider-routes.ollama-local]
/// display-name = "Local: qwen2.5-coder:7b"
/// base-url = "http://127.0.0.1:11434/v1"
/// model = "qwen2.5-coder:7b"
/// api-key-required = false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ProviderRouteConfig {
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub api_key_required: bool,
    /// Recorded approval decision for a `base_url` that resolves to a private/
    /// reserved network target (see `rightkit_agent_policy::net_guard`). Loopback
    /// (127.0.0.1, localhost) never sets this — it is always allowed for local
    /// model servers. `true` only when the caller explicitly approved a
    /// non-loopback private target at registration time.
    pub approved_private: bool,
    /// When this route sends `prompt_cache_key`: `auto` (only to api.openai.com, the default),
    /// `always` (to any host: OpenAI-compatible proxies that route on it) or `never`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key_mode: Option<String>,
    /// Some OpenAI-compatible proxies reject an assistant message whose `content` is null:
    /// with this set, an assistant message that carries tool calls and no text sends `""`.
    pub requires_non_null_assistant_content: bool,
    /// `false` marks a model with no native tool calling: the tools are described in the
    /// prompt and the reply's `$ command` lines or fenced JSON become the tool calls.
    /// Unset means `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<bool>,
    /// `false` keeps the `parallel_tool_calls` request parameter off this route (a gateway that
    /// rejects it). Unset means `true`: the parameter goes out with the tools, as it always did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Wire protocol of the route. Unset or `"openai"` is OpenAI-compatible chat completions
    /// (unchanged behaviour); `"anthropic"` is Anthropic Messages (`POST <base-url>/messages`,
    /// `x-api-key` header, `cache_control`, thinking) for a gateway or proxy on a host other than
    /// `api.anthropic.com`. A route on `api.anthropic.com` always uses Messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

impl ProviderRouteConfig {
    /// The route's compat flags in the shape the provider client takes.
    pub fn native_tool_calls(&self) -> bool {
        self.tool_calls.unwrap_or(true)
    }

    /// The route's wire protocol: `"openai"` (unset, empty or `openai`) or `"anthropic"`.
    /// Matching ignores surrounding whitespace and ASCII case. Any other value is an error
    /// carrying the trimmed original text.
    pub fn wire_protocol(&self) -> Result<&'static str, String> {
        let raw = self.protocol.as_deref().unwrap_or("").trim();
        match raw.to_ascii_lowercase().as_str() {
            "" | "openai" => Ok("openai"),
            "anthropic" => Ok("anthropic"),
            _ => Err(raw.to_owned()),
        }
    }
}

/// Context limits, keyed in `[model-limits]` by a provider-route key or a model id.
///
/// ```toml
/// [model-limits.local]              # provider-route key
/// context-window = 32768
/// [model-limits."qwen2.5-coder:7b"] # model id
/// context-window = 16384
/// compaction-trigger-percent = 70
/// ```
///
/// `context-window` is in tokens (at least 1024); `compaction-trigger-percent` is the share of
/// the window at which automatic compaction starts (1..=95). Within one resolved config the
/// active route's entry overrides the model-id entry field by field. Across layers (user, then
/// workspace) a later layer's entry for the same key replaces the earlier entry as a whole.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ModelLimitsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_trigger_percent: Option<u8>,
}

/// Effective context limits for one model, resolved by [`HostConfig::context_limits`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextLimits {
    pub context_window: Option<u64>,
    pub compaction_trigger_percent: Option<u8>,
}

/// Defaults applied when a new daemon/API session is created without explicit
/// request overrides.
///
/// ```toml
/// [session]
/// allowed-tools = ["read_file", "search"]
/// disallowed-tools = ["shell"]
/// effort = "high"
/// thinking-budget-tokens = 8192
/// disable-thinking = false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct SessionConfig {
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub effort: String,
    pub thinking_budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_thinking: Option<bool>,
}

impl SessionConfig {
    pub const DISABLE_THINKING_DEFAULT: bool = false;

    pub fn disable_thinking(&self) -> bool {
        self.disable_thinking
            .unwrap_or(Self::DISABLE_THINKING_DEFAULT)
    }
}

/// Permission / sandboxing configuration.
///
/// `abac_yaml` holds an inline YAML-like ABAC policy consumed via
/// `PolicyDecisionPoint::from_yaml_like` (FM-026). When present, the daemon
/// builds a PDP from this text and consults it **before** the `RuleStore`
/// glob rules — first-match-wins, surfacing `policy_source: "abac:<rule-id>"`.
/// The file-backed alternative is `<workspace>/<state-dir>/policy.abac.yaml`
/// (or `~/<state-dir>/policy.abac.yaml`) which the daemon also loads via
/// `from_yaml_like` when the field is empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct PermissionsConfig {
    pub mode: String,
    pub deny_read_globs: Vec<String>,
    pub deny_write_globs: Vec<String>,
    /// Inline YAML-like ABAC policy (FM-026). Empty or `None` means no ABAC
    /// layer; the PDP is constructed via `PolicyDecisionPoint::from_yaml_like`.
    #[serde(default)]
    pub abac_yaml: Option<String>,
}

/// Tool configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ToolsConfig {
    pub web_search_provider: Option<String>,
    /// Prefer env var override; do not commit this field with a real value.
    pub web_search_api_key: Option<String>,
    /// Deferred/lazy tool loading (par §#157): when true, only the always-on
    /// core toolset (read/edit/shell/search/tool_search) ships its schema in
    /// the default provider tool block; everything else is discovered via
    /// `tool_search` and bound only once scoped by name. A deferred tool
    /// remains fully dispatchable by name regardless of this flag — this
    /// only changes what's serialized into the prelude.
    ///
    /// Defaults to ON. It shipped defaulting to off, so every live request
    /// carried all 68 tool schemas — the token tax this feature exists to
    /// remove, paid on every turn of every session. Set `lazy-tool-loading =
    /// false` to restore the full prelude.
    ///
    /// `None` means "not set in any layer" — read it through
    /// [`ToolsConfig::lazy_tool_loading`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lazy_tool_loading: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_interface: Option<ModelToolInterfaceConfig>,
    /// Deferred tools whose schema ships in every request from the first turn, without a
    /// `tool_search` first (`eager-tools = ["web_search"]`; `run --search` sets it for one call).
    /// Each named tool adds its schema to every request, so name only what the work needs.
    pub eager_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelToolInterfaceConfig {
    #[default]
    LegacyPromoted,
    FixedV1,
}

impl ToolsConfig {
    /// Default for [`Self::lazy_tool_loading`] when no layer sets it.
    pub const LAZY_TOOL_LOADING_DEFAULT: bool = true;

    /// Effective value of `lazy-tool-loading`.
    pub fn lazy_tool_loading(&self) -> bool {
        self.lazy_tool_loading
            .unwrap_or(Self::LAZY_TOOL_LOADING_DEFAULT)
    }

    pub fn model_interface(&self) -> ModelToolInterfaceConfig {
        self.model_interface.unwrap_or_default()
    }
}

/// Packaged feature flags. `None` means use the daemon's product default.
///
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct FeatureConfig {
    pub cli_acquisition_v1: Option<bool>,
    /// When `true`, the daemon starts the autonomous workflow control plane
    /// supervisor (WP-03) and exposes the workflow REST/SSE surface (WP-09).
    /// When `None` or `false` — the V1 default — no workflow supervisor starts,
    /// no `/api/workflows` endpoints are mounted, and the desktop Automations
    /// surface stays hidden. See ADR-0021.
    ///
    /// A host that retires `features.workflows-enabled` ([`crate::set_retired_config_keys`])
    /// neither reads nor writes it.
    #[serde(
        deserialize_with = "crate::retired::read_workflows_enabled",
        skip_serializing_if = "crate::retired::skip_workflows_enabled"
    )]
    pub workflows_enabled: Option<bool>,
    /// Ed25519 trust roots for signed workflow package imports. Empty is
    /// intentional & fail-closed; values are public keys, never secrets.
    ///
    /// A host that retires `features.workflow-import-trusted-public-keys`
    /// ([`crate::set_retired_config_keys`]) neither reads nor writes it.
    #[serde(
        deserialize_with = "crate::retired::read_workflow_import_trusted_public_keys",
        skip_serializing_if = "crate::retired::skip_workflow_import_trusted_public_keys"
    )]
    pub workflow_import_trusted_public_keys: Vec<WorkflowImportTrustedPublicKey>,
    /// Opt-in per-turn git checkpoint: snapshot the workspace (private index,
    /// HEAD and refs untouched) at the start of every turn so a fork or resume
    /// can restore it with `workspace_checkpoint restore_turn`. `None` or
    /// `false` (the default) takes no snapshots. Honoured only for a trusted
    /// workspace.
    pub turn_checkpoints: Option<bool>,
    /// Opt-in auto-commit on exit: when the session ends, commit pending
    /// workspace changes to the current branch. `None` or `false` (the
    /// default) never commits. Honoured only for a trusted workspace.
    pub auto_commit_on_exit: Option<bool>,
    /// The background self-review fork costs one full-transcript completion
    /// every fifth run. `None` or `true` (the default) keeps it; `false`
    /// turns it off.
    pub self_review: Option<bool>,
    /// Opt-in `advisor` tool: the working model may ask a stronger model
    /// (`[orchestration] advisor-model`) for a verdict. Each call is one paid
    /// completion the model itself chooses, so it is off unless `true`.
    pub advisor: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct WorkflowImportTrustedPublicKey {
    pub key_id: String,
    pub public_key_base64: String,
}

impl FeatureConfig {
    /// V1 default: workflows remain **disabled** until the WP-08 release gate
    /// (reference-workflow golden fixtures + installed-Tauri smoke) passes.
    pub const WORKFLOWS_ENABLED_DEFAULT: bool = false;

    /// Resolves the workflows feature gate to a concrete boolean, applying the
    /// V1 default when the config leaves the field unset.
    pub fn workflows_enabled(&self) -> bool {
        self.workflows_enabled
            .unwrap_or(Self::WORKFLOWS_ENABLED_DEFAULT)
    }

    /// Per-turn checkpoints are opt-in: off unless the config says `true`.
    pub fn turn_checkpoints_enabled(&self) -> bool {
        self.turn_checkpoints.unwrap_or(false)
    }

    /// Auto-commit on exit is opt-in: off unless the config says `true`.
    pub fn auto_commit_on_exit_enabled(&self) -> bool {
        self.auto_commit_on_exit.unwrap_or(false)
    }

    /// Self-review is on unless the config says `false`.
    pub fn self_review_enabled(&self) -> bool {
        self.self_review.unwrap_or(true)
    }

    /// The `advisor` tool is opt-in: off unless the config says `true`.
    pub fn advisor_enabled(&self) -> bool {
        self.advisor.unwrap_or(false)
    }
}

/// Named toolset profile that can be defined in user or workspace config.
///
/// ```toml
/// [toolsets.telegram-remote]
/// name = "Telegram remote"
/// description = "Remote-safe Telegram profile"
/// allowed-tools = ["read_file", "search"]
/// disallowed-tools = ["shell"]
/// read-only = true
/// default-behavioral-skills = ["cr-baseline-behavior"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ToolsetConfig {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub read_only: bool,
    pub default_behavioral_skills: Vec<String>,
}

/// Channel profile tying a remote/control surface to a named toolset and
/// trust/delivery policy. This is intentionally transport-neutral so Telegram,
/// schedules, and future remote adapters can share the same config shape.
///
/// ```toml
/// [channel-profiles.telegram]
/// toolset = "telegram-remote"
/// delivery-policy = "approval-required"
/// trust = "remoteAgentUntrusted"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ChannelProfileConfig {
    pub toolset: String,
    pub delivery_policy: String,
    pub trust: String,
}

/// Remote access configuration (Telegram, web UI).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct RemoteConfig {
    /// Prefer env var override; do not commit this field with a real value.
    pub telegram_token: Option<String>,
    pub telegram_allowed_users: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_enabled: Option<bool>,
}

impl RemoteConfig {
    pub const WEB_ENABLED_DEFAULT: bool = false;

    pub fn web_enabled(&self) -> bool {
        self.web_enabled.unwrap_or(Self::WEB_ENABLED_DEFAULT)
    }
}

/// Per-agent model override. Replaces the built-in default for one agent.
/// `provider_id` defaults to "openrouter" when empty.
///
/// ```toml
/// [agents.fast-code-worker]
/// model-id = "qwen/qwen2.5-coder-32b-instruct"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct AgentOverride {
    pub model_id: String,
    pub provider_id: String,
    /// Reasoning effort for this role ("low" | "medium" | "high" | "xhigh" | "max"); empty leaves
    /// the provider's default. A spawn that names its own effort wins.
    pub effort: String,
    /// Tool profile this role runs with (a built-in id such as `reader` or `auditor`, or a
    /// `[toolsets.<id>]` entry); empty keeps the full tool surface. A spawn that names its own
    /// toolset or tool lists wins.
    pub toolset: String,
    /// Roles this role may spawn; empty leaves spawning unrestricted (the tier and depth checks
    /// still apply). An auditor limited to `["reader"]` cannot start a coder.
    pub spawnable_roles: Vec<String>,
    /// Leave the project's instruction files (AGENTS.md, CLAUDE.md, rules) out of this role's prompt.
    /// Skills, persona and the environment note stay. A spawn's `bare: true` leaves out far more.
    pub omit_instructions: bool,
    /// The window, in tokens, this role's automatic compaction counts against; 0 keeps the model's own
    /// window. It only ever lowers the window (a child that should compact early), never raises it past
    /// what the model can hold.
    pub compact_window_tokens: u64,
}

/// Orchestration settings — the boss model and per-agent model overrides.
///
/// Persisted to `<state-dir>/orchestration.toml` (workspace) or
/// `~/<state-dir>/orchestration.toml` (user-global).
///
/// ```toml
/// boss-model = "qwen/qwen3-235b-a22b"
///
/// [agents.fast-code-worker]
/// model-id = "qwen/qwen2.5-coder-32b-instruct"
///
/// [agents.research-investigator]
/// model-id = "moonshotai/kimi-k2.6"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct OrchestrationConfig {
    /// Override for the primary-orchestrator model. Empty = use built-in default.
    pub boss_model: String,
    /// `single` makes every role inherit the conductor model. `split` enables
    /// per-role models only when the daemon has the matching Pro entitlement.
    pub role_model_mode: String,
    /// Per-agent overrides keyed by agent name, e.g. "fast-code-worker".
    pub agents: HashMap<String, AgentOverride>,
    /// Budget ceilings and orchestration limits. Persisted so the Settings
    /// panel survives a daemon restart.
    pub budget: BudgetConfig,
    /// Deterministic pre-gate checks (build/test/fmt). Run before the conductor
    /// spins up the expensive jury; a failing check short-circuits the run (F3).
    pub pre_gate_checks: Vec<PreGateCheckConfig>,
    /// Conductor trigger-routing (the agent-catalog/trigger-rule harness). When
    /// off (default), the conductor uses today's plan→graph→victory-gate path.
    /// When on, it first matches the task to an agent description and records the
    /// routing decision (DirectEdit vs ArchitectPlan) as a session artifact.
    ///
    /// `None` means "not set in any layer" — read it through
    /// [`OrchestrationConfig::conductor_routing`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conductor_routing: Option<bool>,
    /// Eval-hack advisory detector on the victory gate. When enabled (default),
    /// the gate inspects the run's diff for scoring exploitation patterns
    /// (test rewrite-to-pass, hardcoded expected outputs, check-config
    /// mutation) and attaches structured warnings to the gate result.
    /// Advisory-only — never blocks a run.
    ///
    /// `None` means "not set in any layer" — read it through
    /// [`OrchestrationConfig::eval_hack_advisory`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_hack_advisory: Option<bool>,
    /// Capability / cost-model router — Lane R1a-L3 (ADR
    /// `docs/canon/daemon-master-canon-atoms.md` §D2 seam 1).
    /// When `capability_router` is true, the daemon consults
    /// `host_router::triage()` at per-sub-agent spawn to pick the
    /// model_id — honoring pin precedence (per-turn pin > per-agent
    /// override > session pin > router > default) and the
    /// conductor-stable cache-key invariant. Default OFF (no behavior
    /// change). The `aggressiveness` knob picks which named policy
    /// preset (`conservative` | `balanced` | `aggressive`); only takes
    /// effect when `capability_router` is true. Backend-only here; the
    /// UI toggle is R1a-L4's.
    pub routing: RoutingConfig,
    /// Hold between sibling sub-agent starts that share a prompt prefix: the first starts at
    /// once and the others wait for its first streamed byte (so they read the prompt cache it
    /// wrote) or this many milliseconds, whichever comes first. `None` = unset (5000);
    /// `0` turns the hold off.
    ///
    /// Read it through [`OrchestrationConfig::sibling_start_hold_ms`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sibling_start_hold_ms: Option<u64>,
    /// Model the `advisor` tool asks (`[features] advisor = true`). Empty = the tool has no model
    /// to ask and says so.
    pub advisor_model: String,
}

/// Subset of `OrchestrationConfig` consumed by the per-sub-agent capability
/// router at spawn time.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct RoutingConfig {
    /// Master switch for the capability router. Default OFF — when false,
    /// today's descriptor-driven model selection runs byte-identically.
    ///
    /// `None` means "not set in any layer" — read it through
    /// [`RoutingConfig::capability_router`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_router: Option<bool>,
    /// Named policy preset — `conservative` | `balanced` | `aggressive`.
    /// Unknown values default to `conservative` at the router (fail-safe).
    pub aggressiveness: String,
}

impl OrchestrationConfig {
    /// Default for [`Self::eval_hack_advisory`] when no layer sets it.
    pub const EVAL_HACK_ADVISORY_DEFAULT: bool = true;
    /// Default for [`Self::conductor_routing`] when no layer sets it.
    pub const CONDUCTOR_ROUTING_DEFAULT: bool = false;

    /// Effective value of `eval-hack-advisory`.
    pub fn eval_hack_advisory(&self) -> bool {
        self.eval_hack_advisory
            .unwrap_or(Self::EVAL_HACK_ADVISORY_DEFAULT)
    }

    /// Effective value of `conductor-routing`.
    pub fn conductor_routing(&self) -> bool {
        self.conductor_routing
            .unwrap_or(Self::CONDUCTOR_ROUTING_DEFAULT)
    }

    /// Default for [`Self::sibling_start_hold_ms`] when no layer sets it.
    pub const SIBLING_START_HOLD_MS_DEFAULT: u64 = 5000;

    /// Effective value of `sibling-start-hold-ms` (`0` = no hold).
    pub fn sibling_start_hold_ms(&self) -> u64 {
        self.sibling_start_hold_ms
            .unwrap_or(Self::SIBLING_START_HOLD_MS_DEFAULT)
    }
}

impl RoutingConfig {
    /// Default for [`Self::capability_router`] when no layer sets it.
    pub const CAPABILITY_ROUTER_DEFAULT: bool = false;

    /// Effective value of `capability-router`.
    pub fn capability_router(&self) -> bool {
        self.capability_router
            .unwrap_or(Self::CAPABILITY_ROUTER_DEFAULT)
    }
}

/// One declarative pre-gate check. Mapped to a `host_tools::CheckSpec` by
/// the daemon before each conductor run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct PreGateCheckConfig {
    pub id: String,
    pub command: String,
    pub timeout_ms: u64,
    pub max_attempts: u32,
    pub backoff_ms: u64,
    /// "fail-fast" | "retry" | "continue"
    pub on_failure: String,
}

impl Default for PreGateCheckConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            command: String::new(),
            timeout_ms: 120_000,
            max_attempts: 1,
            backoff_ms: 0,
            on_failure: "fail-fast".into(),
        }
    }
}

/// Budget ceilings and orchestration safety limits. Defaults mirror the
/// daemon's in-memory defaults so an empty config still yields sane values.
///
/// A host may retire `global-monthly-max-usd`, `warning-at`, `max-jury-calls-per-run` and
/// `max-escalation-depth` ([`crate::set_retired_config_keys`]): it then neither reads nor writes
/// them, and the fields keep their defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct BudgetConfig {
    pub per_task_max_usd: f64,
    pub per_goal_max_usd: f64,
    pub per_session_max_usd: f64,
    pub per_agent_call_max_usd: f64,
    #[serde(
        deserialize_with = "crate::retired::read_global_monthly_max_usd",
        skip_serializing_if = "crate::retired::skip_global_monthly_max_usd"
    )]
    pub global_monthly_max_usd: f64,
    #[serde(
        deserialize_with = "crate::retired::read_warning_at",
        skip_serializing_if = "crate::retired::skip_warning_at"
    )]
    pub warning_at: f64,
    #[serde(
        deserialize_with = "crate::retired::read_max_jury_calls_per_run",
        skip_serializing_if = "crate::retired::skip_max_jury_calls_per_run"
    )]
    pub max_jury_calls_per_run: u32,
    #[serde(
        deserialize_with = "crate::retired::read_max_escalation_depth",
        skip_serializing_if = "crate::retired::skip_max_escalation_depth"
    )]
    pub max_escalation_depth: u32,
    pub max_retries_per_role: u32,
    pub max_wall_clock_minutes: u32,
    pub max_iterations: u32,
    pub max_workers: u32,
}

/// The budget values a [`BudgetConfig`] starts from when a config file does not set them: the
/// dollar ceilings and the wall-clock bound. `Default` is the values RightKit has always used.
/// A host with other defaults installs them once with [`set_budget_defaults`]. A ceiling of `0.0`
/// means no ceiling (nothing is gated), and [`HostConfig::validate`] then accepts `0.0` for that
/// key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetDefaults {
    /// Default of `per-task-max-usd`.
    pub per_task_max_usd: f64,
    /// Default of `per-goal-max-usd`.
    pub per_goal_max_usd: f64,
    /// Default of `per-session-max-usd`.
    pub per_session_max_usd: f64,
    /// Default of `per-agent-call-max-usd`.
    pub per_agent_call_max_usd: f64,
    /// Default of `global-monthly-max-usd`.
    pub global_monthly_max_usd: f64,
    /// Default of `max-wall-clock-minutes`, the sub-agent run-length bound.
    pub max_wall_clock_minutes: u32,
}

impl Default for BudgetDefaults {
    fn default() -> Self {
        Self {
            per_task_max_usd: 0.1,
            per_goal_max_usd: 5.0,
            per_session_max_usd: 10.0,
            per_agent_call_max_usd: 0.1,
            global_monthly_max_usd: 100.0,
            max_wall_clock_minutes: 60,
        }
    }
}

impl BudgetDefaults {
    /// Every dollar key defaults to `0.0` (no ceiling, nothing gated) and the sub-agent
    /// wall-clock bound is 10 minutes. A user may still set any key.
    pub fn no_ceiling() -> Self {
        Self {
            per_task_max_usd: 0.0,
            per_goal_max_usd: 0.0,
            per_session_max_usd: 0.0,
            per_agent_call_max_usd: 0.0,
            global_monthly_max_usd: 0.0,
            max_wall_clock_minutes: 10,
        }
    }
}

static HOST_BUDGET_DEFAULTS: OnceLock<BudgetDefaults> = OnceLock::new();

/// Set the host's [`BudgetDefaults`] (once per process; returns `false` if they were already
/// set). Call at start-up before any config is loaded or [`BudgetConfig::default`] is used: the
/// `Default` impl and the serde default of every budget key read them. Until it is called the
/// defaults are [`BudgetDefaults::default`], as before.
pub fn set_budget_defaults(defaults: BudgetDefaults) -> bool {
    HOST_BUDGET_DEFAULTS.set(defaults).is_ok()
}

/// The [`BudgetDefaults`] in effect.
pub fn budget_defaults() -> BudgetDefaults {
    HOST_BUDGET_DEFAULTS.get().copied().unwrap_or_default()
}

impl Default for BudgetConfig {
    fn default() -> Self {
        let defaults = budget_defaults();
        Self {
            per_task_max_usd: defaults.per_task_max_usd,
            per_goal_max_usd: defaults.per_goal_max_usd,
            per_session_max_usd: defaults.per_session_max_usd,
            per_agent_call_max_usd: defaults.per_agent_call_max_usd,
            global_monthly_max_usd: defaults.global_monthly_max_usd,
            warning_at: 0.8,
            max_jury_calls_per_run: 10,
            max_escalation_depth: 3,
            max_retries_per_role: 2,
            max_wall_clock_minutes: defaults.max_wall_clock_minutes,
            max_iterations: 100,
            max_workers: 4,
        }
    }
}

/// Prompt assembly defaults. All fields are opt-out at the daemon level;
/// workspace config can override per-key.
///
/// ```toml
/// [prompt]
/// brief-default = false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct PromptConfig {
    /// Inject the terse-output directive into every session and sub-agent
    /// prompt. Default `true`. Set `false` to disable.
    ///
    /// `None` means "not set in any layer" — read it through
    /// [`PromptConfig::brief_default`], never as a raw `Option`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_default: Option<bool>,
}

impl PromptConfig {
    /// Default for [`Self::brief_default`] when no layer sets it.
    pub const BRIEF_DEFAULT_DEFAULT: bool = true;

    /// Effective value of `brief-default`.
    pub fn brief_default(&self) -> bool {
        self.brief_default.unwrap_or(Self::BRIEF_DEFAULT_DEFAULT)
    }
}

/// Memory retrieval routing configuration (FM-071).
///
/// Controls the three-tier `RoutingPolicy` bandit: `Low` → lexical only,
/// `Mid` → hybrid, `High` → full hybrid with wider candidate set.
/// The bandit adapts per-tier rewards from run outcomes; when the embedder
/// is unavailable the router falls back to lexical (`Low`).
///
/// A host that retires `memory` ([`crate::set_retired_config_keys`]) neither reads nor writes the
/// table, and [`HostConfig::validate`] checks none of its fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct MemoryConfig {
    /// Default retrieval tier when bandit is disabled: `low` | `mid` | `high` | `auto`.
    /// `auto` (default) lets the bandit choose.
    pub retrieval_tier: String,
    /// Whether the bandit is enabled. When false, `retrieval_tier` is used verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandit_enabled: Option<bool>,
    /// Epsilon for epsilon-greedy exploration in [0, 0.5]. Default 0.10.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandit_epsilon: Option<f64>,
    /// EMA learning rate in (0, 1). Default 0.30.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandit_alpha: Option<f64>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            retrieval_tier: "auto".to_string(),
            bandit_enabled: None,
            bandit_epsilon: None,
            bandit_alpha: None,
        }
    }
}

impl MemoryConfig {
    pub const BANDIT_ENABLED_DEFAULT: bool = true;
    pub const BANDIT_EPSILON_DEFAULT: f64 = 0.10;
    pub const BANDIT_ALPHA_DEFAULT: f64 = 0.30;

    pub fn bandit_enabled(&self) -> bool {
        self.bandit_enabled.unwrap_or(Self::BANDIT_ENABLED_DEFAULT)
    }

    pub fn bandit_epsilon(&self) -> f64 {
        self.bandit_epsilon.unwrap_or(Self::BANDIT_EPSILON_DEFAULT)
    }

    pub fn bandit_alpha(&self) -> f64 {
        self.bandit_alpha.unwrap_or(Self::BANDIT_ALPHA_DEFAULT)
    }

    /// Resolve an explicit tier string to a `Tier` when bandit is disabled.
    /// Returns `None` for `auto` or unknown values (caller should use bandit).
    pub fn explicit_tier(&self) -> Option<String> {
        let v = self.retrieval_tier.trim().to_ascii_lowercase();
        match v.as_str() {
            "low" | "mid" | "high" => Some(v),
            _ => None,
        }
    }
}

/// Provenance for a single config value — where it was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: ConfigSource,
    pub file: Option<PathBuf>,
    pub key: String,
}

impl Provenance {
    pub fn new(source: ConfigSource, key: impl Into<String>) -> Self {
        Self {
            source,
            file: None,
            key: key.into(),
        }
    }

    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }
}

/// Validation error for a single field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

impl HostConfig {
    /// Context window and compaction trigger for `model_id`. Starts from the `[model-limits]`
    /// entry keyed by the model id; the entry keyed by `route_key` (the active provider route,
    /// passed by the caller) then overrides field by field. Without a route key only the
    /// model-id entry applies.
    pub fn context_limits(&self, route_key: Option<&str>, model_id: &str) -> ContextLimits {
        let mut limits = ContextLimits::default();
        for entry in [Some(model_id), route_key]
            .into_iter()
            .flatten()
            .filter_map(|key| self.model_limits.get(key))
        {
            limits.context_window = entry.context_window.or(limits.context_window);
            limits.compaction_trigger_percent = entry
                .compaction_trigger_percent
                .or(limits.compaction_trigger_percent);
        }
        limits
    }

    /// [`Self::context_limits`] for a model reference as the runtime holds it: `route::model`
    /// names its route outright; a bare model id takes the route key of the one configured
    /// `[provider-routes]` entry that serves that model (none, or an ambiguous match, leaves only
    /// the model-id entry to apply).
    ///
    /// The bare-id route guess can misattribute, because the caller holds no route key:
    /// - A bare model id served by a built-in provider AND by exactly one custom
    ///   `[provider-routes]` entry picks up that custom route's limits even when the session uses
    ///   the built-in provider.
    /// - Two routes serving one model are ambiguous, so route-only limits on both are lost (only
    ///   a `[model-limits]` entry keyed by the model id applies).
    ///
    /// Callers that know the active route should pass `route::model` or call
    /// [`Self::context_limits`] with the route key; the guess stays for callers that only hold
    /// the model id (daemon session start, the per-send policy publish).
    pub fn context_limits_for(&self, model_ref: &str) -> ContextLimits {
        let model_ref = model_ref.trim();
        if let Some((route_key, model_id)) = model_ref.split_once("::") {
            return self.context_limits(Some(route_key), model_id);
        }
        let mut serving = self
            .provider_routes
            .iter()
            .filter(|(_, route)| route.model.trim().eq_ignore_ascii_case(model_ref));
        let route_key = match (serving.next(), serving.next()) {
            (Some((key, _)), None) => Some(key.as_str()),
            _ => None,
        };
        self.context_limits(route_key, model_ref)
    }

    /// Validate the entire config, returning all field errors.
    /// Governance: every field that has a bounded domain is checked here
    /// so invalid configs fail closed at load time, not at use site.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // The `[memory]` checks, unless the host retired the table.
        if !crate::retired::is_retired("memory") {
            // Memory bandit epsilon in [0, 0.5]
            if let Some(eps) = self.memory.bandit_epsilon {
                if !(0.0..=0.5).contains(&eps) || !eps.is_finite() {
                    errors.push(ValidationError {
                        field: "memory.bandit_epsilon".into(),
                        message: format!("must be in [0, 0.5], got {eps}"),
                    });
                }
            }
            // Memory bandit alpha in (0, 1)
            if let Some(alpha) = self.memory.bandit_alpha {
                if !(0.0..1.0).contains(&alpha) || !alpha.is_finite() {
                    errors.push(ValidationError {
                        field: "memory.bandit_alpha".into(),
                        message: format!("must be in (0, 1), got {alpha}"),
                    });
                }
            }
            // retrieval_tier must be one of low|mid|high|auto
            let tier = self.memory.retrieval_tier.trim().to_ascii_lowercase();
            if !matches!(tier.as_str(), "low" | "mid" | "high" | "auto") {
                errors.push(ValidationError {
                    field: "memory.retrieval_tier".into(),
                    message: format!(
                        "must be one of low|mid|high|auto, got '{}'",
                        self.memory.retrieval_tier
                    ),
                });
            }
        }

        // Budget ceilings must be positive and finite. A key whose host default is `0.0` (the
        // host set no ceiling with `set_budget_defaults`) may also be `0.0`: zero means no cap.
        let budget = &self.orchestration.budget;
        let defaults = budget_defaults();
        for (field, value, default) in [
            (
                "orchestration.budget.per_task_max_usd",
                budget.per_task_max_usd,
                defaults.per_task_max_usd,
            ),
            (
                "orchestration.budget.per_goal_max_usd",
                budget.per_goal_max_usd,
                defaults.per_goal_max_usd,
            ),
            (
                "orchestration.budget.per_session_max_usd",
                budget.per_session_max_usd,
                defaults.per_session_max_usd,
            ),
            (
                "orchestration.budget.per_agent_call_max_usd",
                budget.per_agent_call_max_usd,
                defaults.per_agent_call_max_usd,
            ),
            (
                "orchestration.budget.global_monthly_max_usd",
                budget.global_monthly_max_usd,
                defaults.global_monthly_max_usd,
            ),
        ] {
            if crate::retired::is_retired(field) {
                continue;
            }
            if default == 0.0 {
                if !value.is_finite() || value < 0.0 {
                    errors.push(ValidationError {
                        field: field.into(),
                        message: format!("must be finite and >= 0 (0 = no cap), got {value}"),
                    });
                }
            } else if !value.is_finite() || value <= 0.0 {
                errors.push(ValidationError {
                    field: field.into(),
                    message: format!("must be finite and > 0, got {value}"),
                });
            }
        }
        if !crate::retired::is_retired("orchestration.budget.warning-at")
            && (!(0.0..=1.0).contains(&budget.warning_at) || !budget.warning_at.is_finite())
        {
            errors.push(ValidationError {
                field: "orchestration.budget.warning_at".into(),
                message: format!("must be in [0, 1], got {}", budget.warning_at),
            });
        }

        // Routing aggressiveness
        let agg = self
            .orchestration
            .routing
            .aggressiveness
            .trim()
            .to_ascii_lowercase();
        if !agg.is_empty() && !matches!(agg.as_str(), "conservative" | "balanced" | "aggressive") {
            errors.push(ValidationError {
                field: "orchestration.routing.aggressiveness".into(),
                message: format!(
                    "must be conservative|balanced|aggressive, got '{}'",
                    self.orchestration.routing.aggressiveness
                ),
            });
        }

        // Permission mode must be known.
        let mode = self.permissions.mode.trim().to_ascii_lowercase();
        if !mode.is_empty() && !matches!(mode.as_str(), "default" | "bypass" | "plan" | "read-only")
        {
            errors.push(ValidationError {
                field: "permissions.mode".into(),
                message: format!("unknown permission mode '{}'", self.permissions.mode),
            });
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Typed provenance for all boolean settings — which layer set them.
    pub fn provenance_map(&self) -> HashMap<String, Provenance> {
        let mut map = HashMap::new();
        for setting in [
            BooleanSetting::SessionDisableThinking,
            BooleanSetting::RemoteWebEnabled,
            BooleanSetting::ConductorRouting,
            BooleanSetting::CapabilityRouter,
        ] {
            let key = format!("{setting:?}");
            let source = if setting.is_set(self) {
                // Heuristic: if is_set, it came from user or workspace; we
                // report as Workspace when set, otherwise Default. Full file
                // provenance requires the loader's MergeReport.
                ConfigSource::Workspace
            } else {
                ConfigSource::Default
            };
            map.insert(key.clone(), Provenance::new(source, key));
        }
        map
    }
}
