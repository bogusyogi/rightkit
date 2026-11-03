//! `coderight doctor` — a preflight that previews whether the engine is ready
//! to run, without executing models or tools.
//!
//! See the included siblings for the implementation:
//!   - `doctor_impl`             — pure verdict logic over `Facts`
//!   - `doctor_runtime`          — daemon/CLI entry points + JSON serialisation
//!   - `doctor_dimensions`       — `Dimension`/`Evidence` types + dispatcher
//!   - `doctor_dimension_probes` — the 5 security/runtime probes
//!   - `doctor_clipboard`        — tiny platform-aware clipboard helper
//!
//! Every check and every dimension yields a **verdict**
//! (`Pass` / `Warn` / `Block` / `Unknown`), the whole thing rolls up to one
//! overall verdict, it renders as text *or* JSON (`--json`, for the onboarding
//! UI), and the process **exit code** encodes the verdict
//! (0 ready / 1 warnings / 2 blocked) so it's scriptable in CI and launchers.
//!
//! `--verbose` adds per-dimension evidence; `--clipboard` copies the rendered
//! report to the OS clipboard via the local selection tool.

use serde::Serialize;
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
    /// `~/.coderight` exists or can be created (writable).
    pub coderight_dir_writable: bool,
    /// The evolution DB opens cleanly.
    pub evolution_db_ok: bool,
    /// A local TCP socket can be bound (the daemon can serve).
    pub can_bind_local: bool,
    /// Current working directory is writable (can create files).
    pub workspace_writable: bool,
    /// `git` binary is available on PATH.
    pub git_available: bool,
    /// Default daemon port (5057) is not already in use.
    pub daemon_port_free: bool,
    /// `~/.coderight/skills/` directory exists (optional, warns if missing).
    pub skills_dir_present: bool,
    /// Number of MCP servers found in config (None = no config file).
    pub mcp_server_count: Option<usize>,
    /// Raw RIGHTKIT_SANDBOX_TIER value, if the operator selected a local sandbox tier.
    pub sandbox_tier_env: Option<String>,
    /// Human-readable source for the selected sandbox tier.
    pub sandbox_tier_source: Option<String>,
    /// Whether this platform can enforce the default Landlock/seccomp sandbox.
    pub sandbox_os_supported: bool,
}

include!("doctor_impl.rs");
include!("doctor_dimension_probes.rs");
include!("doctor_dimensions.rs");
include!("doctor_runtime.rs");
use rightkit_doctor::probe::{
    clipboard_candidates as candidate_clipboard_bins, copy_to_clipboard as try_copy_to_clipboard,
};
