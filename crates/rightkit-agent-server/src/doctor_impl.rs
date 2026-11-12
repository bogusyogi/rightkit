// Helper: build a Check. Included from `doctor.rs`.
pub(crate) fn check(id: &str, label: &str, status: Status, detail: &str, fix: &str) -> Check {
    Check {
        id: id.to_string(),
        label: label.to_string(),
        status,
        detail: detail.to_string(),
        fix: fix.to_string(),
    }
}

fn core_sandbox_check(f: &Facts) -> Check {
    let configured = f
        .sandbox_tier_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let raw_tier = configured.unwrap_or("landlock-seccomp");
    let parsed = rightkit_agent_policy::SandboxTier::parse(raw_tier);
    let source = f
        .sandbox_tier_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("RIGHTKIT_SANDBOX_TIER");
    if configured.is_some() && rightkit_agent_policy::SandboxTier::parse_alias(raw_tier).is_none() {
        return check(
            "core_sandbox",
            "Core tool sandbox",
            Status::Warn,
            &format!("{source}={raw_tier} is invalid; sandbox failed closed to landlock-seccomp"),
            "Set RIGHTKIT_SANDBOX_TIER to landlock-seccomp, landlock, or none.",
        );
    }
    match parsed {
        rightkit_agent_policy::SandboxTier::Disabled => check(
            "core_sandbox",
            "Core tool sandbox",
            Status::Warn,
            &format!("{source}={raw_tier} disables core shell sandboxing"),
            "Choose landlock-seccomp for stronger local shell isolation.",
        ),
        _ if f.sandbox_os_supported => check(
            "core_sandbox",
            "Core tool sandbox",
            Status::Pass,
            &format!(
                "{source}={raw_tier}; default Landlock/seccomp sandbox active for local shell commands"
            ),
            "",
        ),
        _ => check(
            "core_sandbox",
            "Core tool sandbox",
            Status::Warn,
            &format!(
                "{source}={raw_tier}; default Landlock/seccomp sandbox is Linux-only on this host"
            ),
            "Use a Linux runner for stronger local process isolation.",
        ),
    }
}

fn extension_execution_trust_check() -> Check {
    let catalog = rightkit_agent_plugins::extension_trust_catalog();
    let native = catalog
        .iter()
        .find(|entry| {
            entry.class == rightkit_agent_plugins::ExtensionTrustClass::TrustedNativeInProcess
        })
        .expect("native extension trust class");
    let external = catalog
        .iter()
        .find(|entry| {
            entry.class == rightkit_agent_plugins::ExtensionTrustClass::ExternalProcessMcp
        })
        .expect("external extension trust class");
    let isolated = catalog
        .iter()
        .find(|entry| entry.class == rightkit_agent_plugins::ExtensionTrustClass::FutureIsolated)
        .expect("future isolated extension trust class");

    if native.validate_registration_sandbox_claim(true).is_ok() {
        return check(
            "extension_execution_trust",
            "Extension execution trust",
            Status::Block,
            "native capability registration incorrectly grants a sandbox claim",
            "repair extension execution trust classification",
        );
    }

    check(
        "extension_execution_trust",
        "Extension execution trust",
        Status::Pass,
        "declarative/built-in: data/no arbitrary code; trusted native in-process: RightKit process authority, not sandboxed by registration; external process/MCP: governed process boundary, isolation requires backend evidence; future isolated: unavailable",
        "",
    )
}

/// Check the provider the default model actually resolves to. The route's
/// `api-key-env` value is never echoed: the config view treats it as secret.
fn provider_key_check(provider: &ProviderReadiness) -> Check {
    const LABEL: &str = "Provider key";
    let (id, auth) = match provider {
        ProviderReadiness::OpenRouterFallback { key: true } => {
            return check(
                "provider_key",
                LABEL,
                Status::Pass,
                "OpenRouter: configured",
                "",
            );
        }
        ProviderReadiness::OpenRouterFallback { key: false } => {
            return check(
                "provider_key",
                LABEL,
                Status::Block,
                "OpenRouter: not found (no configured route matches the default model)",
                "Set OPENROUTER_API_KEY or enter a key in onboarding so it can be stored in the OS keychain.",
            );
        }
        ProviderReadiness::Route { id, auth } => (id, *auth),
    };
    match auth {
        RouteAuth::Key => check(
            "provider_key",
            LABEL,
            Status::Pass,
            &format!("{id} route: configured"),
            "",
        ),
        RouteAuth::NotRequired => check(
            "provider_key",
            LABEL,
            Status::Pass,
            &format!("{id} route: no key required"),
            "",
        ),
        RouteAuth::ChatGptSignIn => check(
            "provider_key",
            LABEL,
            Status::Pass,
            &format!("{id} route: ChatGPT sign-in, checked when a turn starts"),
            "",
        ),
        RouteAuth::MissingKey => check(
            "provider_key",
            LABEL,
            Status::Block,
            &format!("{id} route: required key not found"),
            &format!(
                "Set the environment variable named by provider-routes.{id}.api-key-env, or run `{app} onboard` to store a key for {id} in the OS keychain.", app = crate::host_identity::app()
            ),
        ),
        RouteAuth::MissingBaseUrl => check(
            "provider_key",
            LABEL,
            Status::Block,
            &format!("{id} route: no base-url configured"),
            &format!("Set provider-routes.{id}.base-url to the route's API endpoint."),
        ),
    }
}

/// Pure verdict logic over gathered facts.
pub(crate) fn evaluate(f: &Facts) -> DiagnosticsReport {
    let mut checks = Vec::new();

    checks.push(provider_key_check(&f.provider));

    checks.push(if f.state_dir_writable {
        check(
            &format!("{app}_dir", app = crate::host_identity::app()),
            &format!("~/{home} writable", home = crate::host_identity::home_dir()),
            Status::Pass,
            "ok",
            "",
        )
    } else {
        check(
            &format!("{app}_dir", app = crate::host_identity::app()),
            &format!("~/{home} writable", home = crate::host_identity::home_dir()),
            Status::Block,
            &format!("cannot create or write ~/{home}", home = crate::host_identity::home_dir()),
            "Check the permissions of the RightKit data directory; the daemon stores keys and state there.",
        )
    });

    checks.push(if f.evolution_db_ok {
        check(
            "evolution_db",
            "Evolution database",
            Status::Pass,
            "opens cleanly",
            "",
        )
    } else {
        check(
            "evolution_db",
            "Evolution database",
            Status::Warn,
            &format!(
                "could not open ~/{home}/evolution.db",
                home = crate::host_identity::home_dir()
            ),
            "Learning/memory will be in-memory only this run; check disk space/permissions.",
        )
    });

    checks.push(if f.can_bind_local {
        check(
            "network",
            "Local socket",
            Status::Pass,
            "can bind 127.0.0.1",
            "",
        )
    } else {
        check(
            "network",
            "Local socket",
            Status::Warn,
            "could not bind a local TCP socket",
            "Another process or a sandbox may be blocking localhost binding.",
        )
    });

    checks.push(if f.workspace_writable {
        check(
            "workspace_writable",
            "Workspace writable",
            Status::Pass,
            "current directory is writable",
            "",
        )
    } else {
        check(
            "workspace_writable",
            "Workspace writable",
            Status::Warn,
            "cannot create files in the current directory",
            &format!(
                "Run {app} from a directory you own and can write to.",
                app = crate::host_identity::app()
            ),
        )
    });

    checks.push(if f.git_available {
        check("git", "git", Status::Pass, "found on PATH", "")
    } else {
        check(
            "git",
            "git",
            Status::Warn,
            "git not found on PATH",
            "Install git (https://git-scm.com/downloads); many tools require it.",
        )
    });

    checks.push(if f.daemon_port_free {
        check("daemon_port", "Daemon port 5057", Status::Pass, "free", "")
    } else {
        check(
            "daemon_port",
            "Daemon port 5057",
            Status::Warn,
            "port 5057 is already in use",
            &format!(
                "Another {app} daemon may be running, or use --port to pick a different port.",
                app = crate::host_identity::app()
            ),
        )
    });

    checks.push(if f.skills_dir_present {
        check(
            "skills_dir",
            "Skills directory",
            Status::Pass,
            &format!(
                "~/{home}/skills/ present",
                home = crate::host_identity::home_dir()
            ),
            "",
        )
    } else {
        check(
            "skills_dir",
            "Skills directory",
            Status::Warn,
            &format!(
                "~/{home}/skills/ not found",
                home = crate::host_identity::home_dir()
            ),
            &format!(
                "Create it to add custom skills: mkdir -p ~/{home}/skills",
                home = crate::host_identity::home_dir()
            ),
        )
    });

    match f.mcp_server_count {
        None => checks.push(check(
            "mcp_config",
            "MCP config",
            Status::Warn,
            &format!(
                "no MCP config found (~/{home}/mcp.json)",
                home = crate::host_identity::home_dir()
            ),
            &format!(
                "Create ~/{home}/mcp.json to configure MCP servers (optional).",
                home = crate::host_identity::home_dir()
            ),
        )),
        Some(0) => checks.push(check(
            "mcp_config",
            "MCP config",
            Status::Warn,
            "MCP config present but no servers configured",
            &format!(
                "Add at least one server to ~/{home}/mcp.json to enable MCP tools.",
                home = crate::host_identity::home_dir()
            ),
        )),
        Some(n) => checks.push(check(
            "mcp_config",
            "MCP config",
            Status::Pass,
            &format!("{n} MCP server(s) configured"),
            "",
        )),
    }

    checks.push(extension_execution_trust_check());
    checks.push(core_sandbox_check(f));

    let verdict = checks
        .iter()
        .map(|c| c.status)
        .max_by_key(|s| s.rank())
        .unwrap_or(Status::Pass);
    DiagnosticsReport { verdict, checks }
}

/// Return whether `path` exists/is a directory or could be created under its
/// existing parent, using metadata only. Doctor must not create probe paths.
fn directory_is_writable(path: &std::path::Path) -> bool {
    let mut candidate = path;
    loop {
        match std::fs::metadata(candidate) {
            Ok(metadata) => return metadata.is_dir() && !metadata.permissions().readonly(),
            Err(_) => {
                let Some(parent) = candidate.parent() else {
                    return false;
                };
                if parent == candidate {
                    return false;
                }
                candidate = parent;
            }
        }
    }
}

/// Check an existing evolution database without creating it or running
/// migrations. Repair/initialisation belongs to an explicit setup path.
fn evolution_db_is_readable(path: &std::path::Path) -> bool {
    let Ok(connection) = rightkit_db::DbOptions::new(path).open_read_only() else {
        return false;
    };
    connection.query_row("SELECT 1", [], |_| Ok(())).is_ok()
}

/// Gather facts from the environment (the only I/O in this module).
pub(crate) fn gather() -> Facts {
    let state_dir = rightkit_agent_config::state_root();

    let provider = match crate::main_runtime::default_route_auth(
        &crate::main_runtime::load_current_merged_config(),
    ) {
        Some((id, auth)) => ProviderReadiness::Route { id, auth },
        None => ProviderReadiness::OpenRouterFallback {
            key: rightkit_llm::agent_providers::load_api_key("openrouter").is_some(),
        },
    };

    let state_dir_writable = state_dir
        .as_ref()
        .map(|dir| directory_is_writable(dir))
        .unwrap_or(false);

    let evolution_db_ok = state_dir
        .as_ref()
        .map(|dir| evolution_db_is_readable(&dir.join("evolution.db")))
        .unwrap_or(false);

    let can_bind_local = std::net::TcpListener::bind(("127.0.0.1", 0)).is_ok();

    let workspace_writable = std::env::current_dir()
        .ok()
        .map(|d| directory_is_writable(&d))
        .unwrap_or(false);

    let git_available = rightkit_child_process::command_std("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let daemon_port_free = std::net::TcpListener::bind(("127.0.0.1", 5057)).is_ok();

    let skills_dir_present = state_dir
        .as_ref()
        .map(|dir| dir.join("skills").is_dir())
        .unwrap_or(false);

    let mcp_server_count = state_dir.as_ref().and_then(|dir| {
        let p = dir.join("mcp.json");
        let text = std::fs::read_to_string(&p).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let count = v
            .as_object()
            .and_then(|o| o.get("mcpServers"))
            .and_then(|s| s.as_object())
            .map(|o| o.len())
            .unwrap_or(0);
        Some(count)
    });
    let sandbox_tier_env = std::env::var("RIGHTKIT_SANDBOX_TIER").ok();
    let sandbox_tier_source = sandbox_tier_env
        .as_ref()
        .map(|_| "RIGHTKIT_SANDBOX_TIER".to_string());
    let sandbox_os_supported = cfg!(target_os = "linux");

    Facts {
        provider,
        state_dir_writable,
        evolution_db_ok,
        can_bind_local,
        workspace_writable,
        git_available,
        daemon_port_free,
        skills_dir_present,
        mcp_server_count,
        sandbox_tier_env,
        sandbox_tier_source,
        sandbox_os_supported,
    }
}
