//! `{app} doctor` — a preflight that previews whether the engine is ready
//! to run, without executing models or tools.
//!
//! See the included siblings for the implementation:
//!   - `doctor_impl`             — pure verdict logic over `Facts`
//!   - `doctor_runtime`          — daemon/CLI entry points + JSON serialisation
//!   - `doctor_dimensions`       — `Dimension`/`Evidence` types + dispatcher
//!   - `doctor_dimension_probes` — the 5 security/runtime probes
//!
//! Every check and every dimension yields a **verdict**
//! (`Pass` / `Warn` / `Block` / `Unknown`), the whole thing rolls up to one
//! overall verdict, it renders as text *or* JSON (`--json`, for the onboarding
//! UI), and the process **exit code** encodes the verdict
//! (0 ready / 1 warnings / 2 blocked) so it's scriptable in CI and launchers.
//!
//! `--verbose` adds per-dimension evidence; `--clipboard` copies the rendered
//! report to the OS clipboard via the local selection tool.

use std::path::PathBuf;

pub use rightkit_doctor::verdict::{Check, Status, VerdictReport as DiagnosticsReport};

/// How the configured route selected by the default model authenticates.
/// Presence only — never a credential value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAuth {
    /// A key resolved from the route's env var or the saved key store.
    Key,
    /// The route declares `api-key-required = false`.
    NotRequired,
    /// Native OpenAI route without a saved key: ChatGPT sign-in is resolved
    /// when a turn builds its client, not by this preflight.
    ChatGptSignIn,
    /// The route requires a key and none resolved.
    MissingKey,
    /// The route has no base-url, so no turn can use it.
    MissingBaseUrl,
}

/// The provider a turn uses when none is requested, resolved the same way the
/// engine resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderReadiness {
    /// The default model selects configured `provider-routes.<id>`.
    Route { id: String, auth: RouteAuth },
    /// No configured route matches the default model, so a turn falls back to
    /// OpenRouter; `key` is whether an OpenRouter key is present
    /// (environment, OS keychain, or an opted-in degraded plaintext store).
    OpenRouterFallback { key: bool },
}

impl Default for ProviderReadiness {
    fn default() -> Self {
        Self::OpenRouterFallback { key: false }
    }
}

/// Gathered environment facts the verdict is computed from. Separated from the
/// check logic so the logic is pure and testable.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Provider the default model resolves to, and whether it can authenticate.
    pub provider: ProviderReadiness,
    /// `~/.{app}` exists or can be created (writable).
    pub state_dir_writable: bool,
    /// The evolution DB opens cleanly.
    pub evolution_db_ok: bool,
    /// A local TCP socket can be bound (the daemon can serve).
    pub can_bind_local: bool,
    /// Current working directory is writable (can create files).
    pub workspace_writable: bool,
    /// `git` binary is available on PATH.
    pub git_available: bool,
    /// Why the OS sandbox cannot run here (the shell tool then refuses path-naming commands).
    pub sandbox_unavailable_reason: Option<&'static str>,
    /// `~/.{app}/skills/` directory exists (optional, warns if missing).
    pub skills_dir_present: bool,
    /// Number of MCP servers found in config (None = no config file).
    pub mcp_server_count: Option<usize>,
    /// Raw sandbox-tier environment value (`<PREFIX>_SANDBOX_TIER`), if the operator selected a local sandbox tier.
    pub sandbox_tier_env: Option<String>,
    /// Human-readable source for the selected sandbox tier.
    pub sandbox_tier_source: Option<String>,
    /// The OS sandbox backend the shell tool enforces here (None: no backend for this OS).
    pub sandbox_backend: Option<&'static str>,
    /// Every ABAC policy a session in this workspace would load, with its load result.
    pub abac_policies: Vec<AbacPolicyFact>,
    /// Config layers this process found invalid and ignored (defaults apply instead).
    pub ignored_config_layers: Vec<rightkit_agent_config::IgnoredLayer>,
    /// Config keys that no setting reads (typos), per layer file.
    pub unknown_config_keys: Vec<rightkit_agent_config::UnknownKeys>,
    /// Whether the default provider's endpoint answers (`None`: no route to probe).
    pub provider_reach: Option<ReachFact>,
}

/// One reachability probe of a provider endpoint: a `GET <base-url>/models`, no completion, so
/// nothing is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachFact {
    pub url: String,
    /// The HTTP status of the answer (any status is an answer), or why there was none.
    pub outcome: Result<u16, String>,
    /// Seconds the provider's `Date` header is ahead of this machine's clock.
    pub clock_skew_secs: Option<i64>,
}

/// One configured ABAC policy and whether it loads (audit 2026-11-05 round 2, M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbacPolicyFact {
    /// Where the policy comes from: a file path, or `permissions.abac_yaml`.
    pub source: String,
    /// `Ok(rule count)`, or the read/parse error a session would fail closed on.
    pub result: Result<usize, String>,
    /// The repository file applies only in a trusted workspace.
    pub applies_when_trusted_only: bool,
}

/// Best effort: pipe `text` to the first system clipboard tool that accepts it.
pub use rightkit_doctor::probe::copy_to_clipboard as try_copy_to_clipboard;

/// The process exit code of a doctor run. The shared report exits 0 ready, 1 warnings and
/// 2 blocked; a host that reserves 2 for a refused or misused command (the host option
/// `doctor_block_exits_with_one`) exits 1 for a block as well, so exit 2 never means "blocked".
pub fn exit_code(report: &DiagnosticsReport) -> i32 {
    if report.verdict == Status::Block && crate::host_options::doctor_block_exits_with_one() {
        1
    } else {
        report.exit_code()
    }
}

include!("doctor_impl.rs");
include!("doctor_dimensions.rs");
include!("doctor_runtime.rs");
// Holds the `hooks_probe_tests` test module, so it must stay the last include.
include!("doctor_dimension_probes.rs");
