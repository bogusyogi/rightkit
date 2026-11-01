use super::*;
use crate::engine::loop_detector_config::LoopDetectorConfig;

pub struct SessionEngineConfig {
    pub session_id: String,
    pub permission_mode: PermissionMode,
    pub workspace_trust: WorkspaceTrust,
    pub workspace_root: std::path::PathBuf,
    pub max_iterations: usize,
    /// Auto post-edit LSP diagnostics (default true). See `SessionEngine`.
    pub auto_post_edit_diagnostics: bool,
    /// Maximum worker-spawning tool calls accepted from one LLM response.
    /// 0 means unlimited.
    pub max_worker_tool_calls: usize,
    pub budget_ceiling_usd: f64,
    /// Reasoning effort hint passed to the provider: "low" | "medium" | "high".
    /// Maps to thinking budget tokens: low→1024, medium→8192, high→32768.
    /// None means no explicit effort constraint is sent.
    pub effort: Option<String>,
    pub permission_rules: Option<Arc<Mutex<RuleStore>>>,
    pub skill_dirs: Vec<std::path::PathBuf>,
    pub execution_backend: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub execution_workspace_path: Option<std::path::PathBuf>,
    pub execution_network_profile: Option<String>,
    pub execution_resource_limits:
        Option<rightkit_agent_tools::coding_tools::ExecutionResourceLimits>,
    #[cfg(feature = "host")]
    pub execution_backend_registry:
        Option<Arc<rightkit_agent_tools::coding_tools::ExecutionBackendRegistry>>,
    pub mcp_channel: String,
    pub toolset: Option<String>,
    pub runtime_middleware: Option<Arc<Mutex<RuntimeMiddlewareRegistry>>>,
    #[cfg(feature = "host")]
    pub background_shells: Option<rightkit_agent_tools::coding_tools::background_shell::BgStore>,
    #[cfg(feature = "host")]
    pub browser_sessions: Option<rightkit_agent_tools::coding_tools::browser::BrowserStore>,
    pub task_store: Option<rightkit_agent_tools::coding_tools::task_tools::TaskStore>,
    /// UTC day bucket (`YYYY-MM-DD`) used to derive the stable
    /// `cache:prompt:stable:{hash}` cache key. Empty string means the
    /// engine constructor will fill it from `cache::today_day()` at session
    /// start. Tests should pre-set this field via `..cfg.with_day_key(...)`
    /// (or by direct field assignment) so test paths stay free of wall-clock
    /// reads. See `engine::cache::StableCacheKey`.
    pub day_key: String,
    /// Enable context folding for subtask completion. When enabled, the engine
    /// will fold sub-trajectories into summaries when subtasks complete.
    /// OFF by default until measured. Additive; complementary to the 70% compactor.
    pub context_fold_enabled: bool,
    /// Directory for archived sub-trajectories. If not set, uses `tool_result_dir`.
    pub fold_archive_dir: Option<std::path::PathBuf>,
    /// Unified threshold config for the loop-guard family
    /// (`MopDetector`, `ToolLoopDetector`, and the new idempotency
    /// axis from `tool_loop_idempotency`). All thresholds for all
    /// three live in one block — see `LoopDetectorConfig`. Defaults
    /// are byte-identical to the pre-P-3 hard-coded call sites.
    pub loop_detector_config: LoopDetectorConfig,
    pub denial_tracker:
        Option<std::sync::Arc<std::sync::Mutex<rightkit_agent_policy::DenialTracker>>>,
    /// Optional ABAC PDP (FM-026). When `Some`, `permission_verdict_for` consults
    /// it before the `RuleStore` (`permission_rules` seam). Built from
    /// `PermissionsConfig::abac_yaml` via `PolicyDecisionPoint::from_yaml_like`
    /// or from `<workspace>/<state-dir>/policy.abac.yaml`; first-match-wins.
    pub abac_pdp: Option<std::sync::Arc<rightkit_agent_policy::PolicyDecisionPoint>>,
    /// Defaults plus user `[permissions] deny-read-globs/deny-write-globs`.
    /// `None` keeps the built-in default deny lists.
    pub deny_globs: Option<std::sync::Arc<rightkit_agent_policy::DenyGlobSet>>,
    /// Daemon-owned model resolver for compaction-stage route isolation.
    /// `None` keeps legacy dynamic sessions on their existing fallback.
    pub compaction_route_resolver: Option<Arc<dyn crate::CompactionModelRouteResolver>>,
}

impl Default for SessionEngineConfig {
    fn default() -> Self {
        Self {
            session_id: "s1".into(),
            permission_mode: PermissionMode::Bypass,
            workspace_trust: WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            max_iterations: 16,
            auto_post_edit_diagnostics: true,
            max_worker_tool_calls: 4,
            budget_ceiling_usd: 0.0,
            effort: None,
            permission_rules: None,
            skill_dirs: Vec::new(),
            execution_backend: None,
            execution_workspace_id: None,
            execution_workspace_path: None,
            execution_network_profile: None,
            execution_resource_limits: None,
            #[cfg(feature = "host")]
            execution_backend_registry: None,
            mcp_channel: "desktop".into(),
            toolset: None,
            runtime_middleware: None,
            #[cfg(feature = "host")]
            background_shells: None,
            #[cfg(feature = "host")]
            browser_sessions: None,
            task_store: None,
            // Empty → the constructor fills it from `cache::today_day()`
            // without invoking the wall clock here. Tests override via
            // `cfg.day_key = "..."` to keep test paths clock-free.
            day_key: String::new(),
            // OFF by default until measured. Additive; complementary to compactor.
            context_fold_enabled: false,
            fold_archive_dir: None,
            loop_detector_config: LoopDetectorConfig::default(),
            denial_tracker: None,
            abac_pdp: None,
            deny_globs: None,
            compaction_route_resolver: None,
        }
    }
}

impl SessionEngineConfig {
    /// Convenience builder: pre-set the day bucket for deterministic tests or
    /// for replaying a session against a captured day identifier. Production
    /// code that wants the live day may simply leave `day_key` empty and the
    /// engine constructor will fill it from `cache::today_day()`.
    pub fn with_day_key(mut self, day: impl Into<String>) -> Self {
        self.day_key = day.into();
        self
    }
}

pub(super) fn normalize_mcp_channel(channel: String) -> String {
    let normalized = channel.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        "desktop".to_string()
    } else {
        normalized
    }
}
