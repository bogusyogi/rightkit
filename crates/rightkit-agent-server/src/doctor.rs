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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Block,
}

impl Status {
    fn symbol(self) -> &'static str {
        match self {
            Status::Pass => "\u{2713}",
            Status::Warn => "\u{26a0}",
            Status::Block => "\u{2717}",
        }
    }
    fn rank(self) -> u8 {
        match self {
            Status::Pass => 0,
            Status::Warn => 1,
            Status::Block => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
    /// What to do about it (empty for a clean Pass).
    pub fix: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsReport {
    pub verdict: Status,
    pub checks: Vec<Check>,
}

impl DiagnosticsReport {
    /// Process exit code: 0 ready, 1 warnings, 2 blocked.
    pub fn exit_code(&self) -> i32 {
        self.verdict.rank() as i32
    }

    pub fn render_text(&self) -> String {
        let mut out = String::from("RightKit doctor\n\n");
        for c in &self.checks {
            out.push_str(&format!(
                "  {} {} \u{2014} {}\n",
                c.status.symbol(),
                c.label,
                c.detail
            ));
            if !c.fix.is_empty() && c.status != Status::Pass {
                out.push_str(&format!("      \u{2192} {}\n", c.fix));
            }
        }
        let verdict = match self.verdict {
            Status::Pass => "READY",
            Status::Warn => "READY WITH WARNINGS",
            Status::Block => "BLOCKED",
        };
        out.push_str(&format!("\nVerdict: {verdict}\n"));
        out
    }
}

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
include!("doctor_clipboard.rs");

#[cfg(test)]
#[path = "doctor_dimensions_tests.rs"]
mod doctor_dimensions_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn all_pass_facts() -> Facts {
        Facts {
            provider: ProviderReadiness::OpenRouterFallback { key: true },
            coderight_dir_writable: true,
            evolution_db_ok: true,
            can_bind_local: true,
            workspace_writable: true,
            git_available: true,
            daemon_port_free: true,
            skills_dir_present: true,
            mcp_server_count: Some(1),
            sandbox_tier_env: None,
            sandbox_tier_source: None,
            sandbox_os_supported: true,
        }
    }

    #[test]
    fn all_pass_is_ready_exit_0() {
        let r = evaluate(&all_pass_facts());
        assert_eq!(r.verdict, Status::Pass);
        assert_eq!(r.exit_code(), 0);
        assert!(r.render_text().contains("READY"));
    }

    #[test]
    fn missing_key_blocks_with_fix() {
        let f = Facts {
            provider: ProviderReadiness::OpenRouterFallback { key: false },
            ..all_pass_facts()
        };
        let r = evaluate(&f);
        assert_eq!(r.verdict, Status::Block);
        assert_eq!(r.exit_code(), 2);
        let key = r.checks.iter().find(|c| c.id == "provider_key").unwrap();
        assert_eq!(key.status, Status::Block);
        assert!(key.fix.contains("OPENROUTER_API_KEY"));
        assert!(key.fix.contains("OS keychain"));
        assert!(!key.fix.contains("PLAINTEXT"));
        assert!(!key.fix.contains("create ~/.coderight/openrouter.key"));
        assert!(!key.fix.contains("GROQ_API_KEY"));
    }

    fn provider_check(id: &str, auth: RouteAuth) -> Check {
        let f = Facts {
            provider: ProviderReadiness::Route {
                id: id.to_string(),
                auth,
            },
            ..all_pass_facts()
        };
        evaluate(&f)
            .checks
            .into_iter()
            .find(|c| c.id == "provider_key")
            .unwrap()
    }

    #[test]
    fn key_free_route_passes_without_openrouter() {
        for auth in [
            RouteAuth::NotRequired,
            RouteAuth::Key,
            RouteAuth::ChatGptSignIn,
        ] {
            let c = provider_check("local", auth);
            assert_eq!(c.status, Status::Pass, "{auth:?}");
            assert!(c.detail.contains("local route"), "{}", c.detail);
            assert!(!c.detail.contains("OpenRouter"), "{}", c.detail);
        }
    }

    #[test]
    fn route_missing_required_key_blocks_naming_route() {
        let c = provider_check("nvidia", RouteAuth::MissingKey);
        assert_eq!(c.status, Status::Block);
        assert!(c.detail.contains("nvidia route"), "{}", c.detail);
        assert!(
            c.fix.contains("provider-routes.nvidia.api-key-env"),
            "{}",
            c.fix
        );
        assert!(c.fix.contains("coderight onboard"), "{}", c.fix);
        assert!(!c.fix.contains("OPENROUTER_API_KEY"), "{}", c.fix);
    }

    #[test]
    fn route_without_base_url_blocks() {
        let c = provider_check("nvidia", RouteAuth::MissingBaseUrl);
        assert_eq!(c.status, Status::Block);
        assert!(
            c.fix.contains("provider-routes.nvidia.base-url"),
            "{}",
            c.fix
        );
    }

    #[test]
    fn default_route_auth_follows_engine_route_resolution() {
        let mut cfg = rightkit_app_settings::agent_config::RightKitConfig::default();
        cfg.model.default_model = "qwen2.5-coder:7b".to_string();
        let route = rightkit_app_settings::agent_config::ProviderRouteConfig {
            display_name: "Local".to_string(),
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            api_key_required: false,
            api_key_env: None,
            approved_private: false,
        };
        cfg.provider_routes
            .insert("doctor-test-local".to_string(), route.clone());
        assert_eq!(
            crate::main_runtime::default_route_auth(&cfg),
            Some(("doctor-test-local".to_string(), RouteAuth::NotRequired))
        );

        cfg.provider_routes.insert(
            "doctor-test-local".to_string(),
            rightkit_app_settings::agent_config::ProviderRouteConfig {
                base_url: String::new(),
                ..route
            },
        );
        assert_eq!(
            crate::main_runtime::default_route_auth(&cfg),
            Some(("doctor-test-local".to_string(), RouteAuth::MissingBaseUrl))
        );

        cfg.model.default_model = "unrouted-model".to_string();
        assert_eq!(crate::main_runtime::default_route_auth(&cfg), None);
    }

    #[test]
    fn db_problem_only_warns() {
        let f = Facts {
            evolution_db_ok: false,
            ..all_pass_facts()
        };
        let r = evaluate(&f);
        assert_eq!(r.verdict, Status::Warn);
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn json_is_valid_and_carries_verdict() {
        let r = evaluate(&all_pass_facts());
        let json = serde_json::to_string_pretty(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["verdict"], "pass");
        assert!(v["checks"].as_array().unwrap().len() >= 4);
    }

    #[test]
    fn cli_json_adds_schema_version_without_changing_report_json() {
        let r = evaluate(&all_pass_facts());
        let report_json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&r).unwrap()).unwrap();
        assert!(report_json.get("schema_version").is_none());

        let cli_value: serde_json::Value =
            serde_json::from_str(&super::cli_json(&r, false)).unwrap();
        assert_eq!(cli_value["schema_version"], "coderight.cli.doctor.v1");
        assert_eq!(cli_value["verdict"], "pass");
    }

    #[test]
    fn no_git_warns() {
        let f = Facts {
            git_available: false,
            ..all_pass_facts()
        };
        let r = evaluate(&f);
        assert_eq!(r.verdict, Status::Warn);
        let c = r.checks.iter().find(|c| c.id == "git").unwrap();
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn diagnostics_report_core_sandbox_posture() {
        let r = evaluate(&all_pass_facts());
        let c = r.checks.iter().find(|c| c.id == "core_sandbox").unwrap();
        assert_eq!(c.label, "Core tool sandbox");
        assert!(c.detail.contains("Landlock/seccomp"));
    }

    #[test]
    fn daemon_port_in_use_warns() {
        let f = Facts {
            daemon_port_free: false,
            ..all_pass_facts()
        };
        assert_eq!(
            evaluate(&f)
                .checks
                .iter()
                .find(|c| c.id == "daemon_port")
                .unwrap()
                .status,
            Status::Warn
        );
    }

    #[test]
    fn no_mcp_config_warns() {
        let f = Facts {
            mcp_server_count: None,
            ..all_pass_facts()
        };
        assert_eq!(
            evaluate(&f)
                .checks
                .iter()
                .find(|c| c.id == "mcp_config")
                .unwrap()
                .status,
            Status::Warn
        );
    }
}
