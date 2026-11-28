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
    let default_source = rightkit_agent_config::env_name("SANDBOX_TIER");
    let source = f
        .sandbox_tier_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_source.as_str());
    if configured.is_some() && rightkit_agent_policy::SandboxTier::parse_alias(raw_tier).is_none() {
        return check(
            "core_sandbox",
            "Core tool sandbox",
            Status::Warn,
            &format!("{source}={raw_tier} is invalid; sandbox failed closed to landlock-seccomp"),
            &format!("Set {default_source} to landlock-seccomp, landlock, or none."),
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
        _ => match (f.sandbox_unavailable_reason, f.sandbox_backend) {
            (Some(reason), _) => check(
                "core_sandbox",
                "Core tool sandbox",
                Status::Block,
                reason,
                "Use Linux kernel 5.13 or newer with landlock in the lsm= boot list (CONFIG_SECURITY_LANDLOCK).",
            ),
            (None, Some(backend)) => check(
                "core_sandbox",
                "Core tool sandbox",
                Status::Pass,
                &format!("{source}={raw_tier}; default {backend} sandbox active for local shell commands"),
                "",
            ),
            (None, None) => check(
                "core_sandbox",
                "Core tool sandbox",
                Status::Warn,
                &format!("{source}={raw_tier}; no local shell sandbox backend exists for this OS"),
                "Use Linux, macOS or Windows for local process isolation.",
            ),
        },
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
    let _external = catalog
        .iter()
        .find(|entry| {
            entry.class == rightkit_agent_plugins::ExtensionTrustClass::ExternalProcessMcp
        })
        .expect("external extension trust class");
    let _isolated = catalog
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
        "declarative/built-in: data/no arbitrary code; trusted native in-process: the host process authority, not sandboxed by registration; external process/MCP: governed process boundary, isolation requires backend evidence; future isolated: unavailable",
        "",
    )
}

/// Check the provider the default model actually resolves to. No key value is ever echoed.
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
                if crate::host_options::provider_keys_from_environment() {
                    "Set OPENROUTER_API_KEY or enter a key in onboarding so it can be stored in the OS keychain."
                } else {
                    "Enter an OpenRouter key in onboarding or Settings > Providers so it can be stored in the OS keychain."
                },
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
            &if crate::host_options::provider_keys_from_environment() {
                format!(
                    "Set the environment variable named by provider-routes.{id}.api-key-env, or run `{}` to store a key for {id} in the OS keychain.",
                    crate::host_identity::command("onboard")
                )
            } else {
                format!(
                    "Run `{}` to store a key for {id} in the OS keychain.",
                    crate::host_identity::command("onboard")
                )
            },
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

/// Audit 2026-11-05 round 2 M2: an ABAC policy that does not load makes every
/// session deny every tool call, so doctor names the file and the error.
fn abac_policy_check(policies: &[AbacPolicyFact]) -> Check {
    const LABEL: &str = "ABAC policy";
    let broken: Vec<&AbacPolicyFact> = policies.iter().filter(|p| p.result.is_err()).collect();
    if let Some(first) = broken.first() {
        let detail = broken
            .iter()
            .map(|p| {
                let scope = if p.applies_when_trusted_only {
                    " (applies when the workspace is trusted)"
                } else {
                    ""
                };
                format!(
                    "{}{scope} does not load: {}",
                    p.source,
                    p.result
                        .as_ref()
                        .err()
                        .map(String::as_str)
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return check(
            "abac_policy",
            LABEL,
            Status::Block,
            &format!("{detail}; every tool call in a new session is denied"),
            &format!(
                "Fix {} (or remove it), then start a new session: the policy is read only when a session starts.",
                first.source
            ),
        );
    }
    let detail = if policies.is_empty() {
        "no ABAC policy configured".to_string()
    } else {
        policies
            .iter()
            .map(|p| {
                format!(
                    "{}: {} rule(s)",
                    p.source,
                    p.result.as_ref().copied().unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    check("abac_policy", LABEL, Status::Pass, &detail, "")
}

/// The ABAC policies a session started in `workspace` would load, in the
/// order `load_session_abac_pdp` consults them. Only a definite "not found"
/// means a file is absent; any other stat or read error is reported.
fn gather_abac_policies(
    inline_yaml: Option<&str>,
    workspace: &std::path::Path,
) -> Vec<AbacPolicyFact> {
    let parse = |text: &str| {
        rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(text).map(|pdp| pdp.len())
    };
    let mut facts = Vec::new();
    if let Some(yaml) = inline_yaml.filter(|value| !value.trim().is_empty()) {
        let mut files: Vec<String> = rightkit_agent_config::state_root()
            .map(|root| root.join("config.toml").display().to_string())
            .into_iter()
            .collect();
        files.push(
            workspace
                .join(rightkit_agent_config::state_dir_name())
                .join("config.toml")
                .display()
                .to_string(),
        );
        facts.push(AbacPolicyFact {
            source: format!("permissions.abac_yaml in {}", files.join(" or ")),
            result: parse(yaml),
            applies_when_trusted_only: false,
        });
        // The inline policy wins; the repository file is not loaded.
        return facts;
    }
    let path = workspace
        .join(rightkit_agent_config::state_dir_name())
        .join("policy.abac.yaml");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => facts.push(AbacPolicyFact {
            source: path.display().to_string(),
            result: std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| parse(&text)),
            applies_when_trusted_only: true,
        }),
    }
    facts
}

/// A config file that failed to parse is ignored (defaults apply): a warning, never a block.
fn unknown_config_keys_check(layers: &[rightkit_agent_config::UnknownKeys]) -> Option<Check> {
    if layers.is_empty() {
        return None;
    }
    let detail = layers
        .iter()
        .map(|l| format!("{} ({}): {}", l.path.display(), l.layer, l.keys.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    Some(check(
        "config_unknown_keys",
        "Config keys",
        Status::Warn,
        &format!("unknown key(s) ignored: {detail}"),
        "Fix the spelling of the named key, or remove it; nothing reads it today.",
    ))
}

/// Longest the probe waits for the provider to answer.
const REACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// Clock difference worth a warning: signed requests and TLS start failing past a few minutes.
const CLOCK_SKEW_WARN_SECS: i64 = 300;

fn provider_reach_check(fact: &ReachFact) -> Check {
    const ID: &str = "provider_reach";
    const LABEL: &str = "Provider reachable";
    match &fact.outcome {
        Err(error) => check(
            ID,
            LABEL,
            Status::Warn,
            &format!("{} is unreachable: {error}", fact.url),
            "Check the base-url, DNS, proxy and network; a run will fail until it answers.",
        ),
        Ok(status) => match fact.clock_skew_secs {
            Some(skew) if skew.abs() > CLOCK_SKEW_WARN_SECS => check(
                ID,
                LABEL,
                Status::Warn,
                &format!(
                    "{} answered (HTTP {status}) but its clock differs from this machine's by {skew} s",
                    fact.url
                ),
                "Set this machine's clock from a network time source; TLS and signed requests fail on a large skew.",
            ),
            _ => check(
                ID,
                LABEL,
                Status::Pass,
                &format!("{} answered (HTTP {status})", fact.url),
                "",
            ),
        },
    }
}

/// Seconds since the epoch for an HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn parse_http_date(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else {
        return None;
    };
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let mut clock = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    // Days from 1970-01-01 (civil-from-days inverse).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// `GET <base-url>/models` with no credential: any HTTP answer shows the host resolves, accepts
/// TLS and speaks HTTP; only a transport failure is "unreachable". Runs on its own thread and
/// runtime so the (synchronous) doctor works inside or outside an async caller.
fn probe_provider(base_url: &str) -> ReachFact {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let probe_url = url.clone();
    let joined = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async {
            let client =
                rightkit_agent_daemon::http_client::endpoint_client(&probe_url, REACH_TIMEOUT)?;
            let response = rightkit_agent_daemon::http_client::send_any(
                &client,
                &rightkit_http::Request::get(probe_url.clone()),
            )
            .await?;
            let skew = response
                .header("date")
                .and_then(parse_http_date)
                .and_then(|theirs| {
                    let ours = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs() as i64;
                    Some(theirs - ours)
                });
            Ok::<_, String>((response.status, skew))
        })
    })
    .join();
    match joined {
        Ok(Ok((status, skew))) => ReachFact {
            url,
            outcome: Ok(status),
            clock_skew_secs: skew,
        },
        Ok(Err(error)) => ReachFact {
            url,
            outcome: Err(error),
            clock_skew_secs: None,
        },
        Err(_) => ReachFact {
            url,
            outcome: Err("the probe stopped unexpectedly".to_string()),
            clock_skew_secs: None,
        },
    }
}

fn ignored_config_check(layers: &[rightkit_agent_config::IgnoredLayer]) -> Check {
    if layers.is_empty() {
        return check(
            "config_files",
            "Config files",
            Status::Pass,
            "parse cleanly",
            "",
        );
    }
    let detail = layers
        .iter()
        .map(|l| {
            format!(
                "{} config {} ignored: {}{}",
                l.layer,
                l.path.display(),
                l.error,
                l.moved_to
                    .as_ref()
                    .map(|p| format!(" (moved to {})", p.display()))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    check(
        "config_files",
        "Config files",
        Status::Warn,
        &format!("{detail}; permission rules and deny globs from it are not in effect"),
        "Fix the TOML syntax error in the named file; defaults apply until then.",
    )
}

/// Pure verdict logic over gathered facts.
pub(crate) fn evaluate(f: &Facts) -> DiagnosticsReport {
    let mut checks = Vec::new();

    checks.push(provider_key_check(&f.provider));

    let state_dir_check_id = format!("{}_dir", crate::host_identity::app());
    checks.push(if f.state_dir_writable {
        check(
            &state_dir_check_id,
            "State directory writable",
            Status::Pass,
            "ok",
            "",
        )
    } else {
        check(
            &state_dir_check_id,
            "State directory writable",
            Status::Block,
            "cannot create or write the state directory",
            "Check the permissions of the data directory; the daemon stores keys and state there.",
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
            "could not open evolution.db in the state directory",
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
                "Run {} from a directory you own and can write to.",
                crate::host_identity::app()
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

    checks.push(if f.skills_dir_present {
        check(
            "skills_dir",
            "Skills directory",
            Status::Pass,
            "skills directory present",
            "",
        )
    } else {
        check(
            "skills_dir",
            "Skills directory",
            Status::Warn,
            "skills directory not found",
            "Create a skills folder in the state directory to add custom skills.",
        )
    });

    match f.mcp_server_count {
        None => checks.push(check(
            "mcp_config",
            "MCP config",
            Status::Warn,
            &format!(
                "no MCP config found (~/.config/{}/mcp.json)",
                crate::host_identity::config_dir()
            ),
            &format!(
                "Create ~/.config/{}/mcp.json to configure MCP servers (optional).",
                crate::host_identity::config_dir()
            ),
        )),
        Some(0) => checks.push(check(
            "mcp_config",
            "MCP config",
            Status::Warn,
            "MCP config present but no servers configured",
            &format!(
                "Add at least one server to ~/.config/{}/mcp.json to enable MCP tools.",
                crate::host_identity::config_dir()
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
    checks.push(abac_policy_check(&f.abac_policies));
    checks.push(ignored_config_check(&f.ignored_config_layers));
    checks.extend(unknown_config_keys_check(&f.unknown_config_keys));
    checks.extend(f.provider_reach.as_ref().map(provider_reach_check));

    let verdict = checks
        .iter()
        .map(|c| c.status)
        .max_by_key(|s| s.rank())
        .unwrap_or(Status::Pass);
    DiagnosticsReport { verdict, checks }
}

/// Return whether `path` exists/is a directory or could be created under its
/// existing parent. Doctor must not create probe paths.
fn directory_is_writable(path: &std::path::Path) -> bool {
    let mut candidate = path;
    loop {
        match std::fs::metadata(candidate) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return false;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    let Ok(c_path) = std::ffi::CString::new(candidate.as_os_str().as_bytes())
                    else {
                        return false;
                    };
                    // SAFETY: `c_path` is a valid NUL-terminated string; access(2) only reads it.
                    return unsafe { libc::access(c_path.as_ptr(), libc::W_OK | libc::X_OK) } == 0;
                }
                #[cfg(not(unix))]
                return !metadata.permissions().readonly();
            }
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

    let merged_config = crate::main_runtime::load_current_merged_config();
    let abac_policies = gather_abac_policies(
        merged_config.permissions.abac_yaml.as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );
    let provider = match crate::main_runtime::default_route_auth(&merged_config) {
        Some((id, auth)) => ProviderReadiness::Route { id, auth },
        None => ProviderReadiness::OpenRouterFallback {
            key: rightkit_llm::agent_providers::load_api_key("openrouter").is_some(),
        },
    };

    // Only a route a turn could use is probed: the key is present (or not needed) and the route
    // names an endpoint. A missing key or URL is already a block of its own.
    let provider_reach = match &provider {
        ProviderReadiness::Route {
            id,
            auth: RouteAuth::Key | RouteAuth::NotRequired,
        } => merged_config
            .provider_routes
            .get(id)
            .map(|route| route.base_url.trim())
            .filter(|url| !url.is_empty())
            .map(probe_provider),
        ProviderReadiness::OpenRouterFallback { key: true } => {
            rightkit_llm::agent_providers::find_provider("openrouter")
                .map(|info| probe_provider(info.base_url))
        }
        _ => None,
    };

    let state_dir_writable = state_dir
        .as_ref()
        .map(|dir| directory_is_writable(dir))
        .unwrap_or(false);

    // A fresh install has no evolution.db yet; the first run creates it in the
    // writable state directory, so absence is not a fault.
    let evolution_db_ok = state_dir
        .as_ref()
        .map(|dir| {
            let path = dir.join("evolution.db");
            if path.exists() {
                evolution_db_is_readable(&path)
            } else {
                state_dir_writable
            }
        })
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
    let sandbox_tier_name = rightkit_agent_config::env_name("SANDBOX_TIER");
    let sandbox_tier_env = std::env::var(&sandbox_tier_name).ok();
    let sandbox_tier_source = sandbox_tier_env.as_ref().map(|_| sandbox_tier_name.clone());
    // The backend the shell tool actually enforces on this OS (same mapping as
    // `tools::shell::result_sandbox` uses for denial evidence).
    // On Linux the shell tool fails closed when Landlock is unavailable, so probe the
    // same syscall it uses (landlock_create_ruleset, VERSION flag) instead of assuming.
    #[cfg(target_os = "linux")]
    let sandbox_unavailable_reason: Option<&'static str> = {
        // SAFETY: a NULL attr with size 0 and the VERSION flag only queries the ABI version.
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0usize,
                1u32,
            )
        };
        // The shell decides enforcement per workspace, so only a probe failure that
        // the shell would actually act on is reported (probe-only if cwd is unknown).
        let enforced = std::env::current_dir()
            .map(|dir| rightkit_agent_coding_tools::shell::os_sandbox_enforced(&dir))
            .unwrap_or(true);
        if abi >= 1 || !enforced {
            None
        } else {
            Some("Landlock unavailable on this kernel; every shell command is refused")
        }
    };
    #[cfg(not(target_os = "linux"))]
    let sandbox_unavailable_reason: Option<&'static str> = None;
    let sandbox_backend = if cfg!(target_os = "linux") {
        Some("Landlock/seccomp")
    } else if cfg!(target_os = "macos") {
        Some("Seatbelt")
    } else if cfg!(windows) {
        Some("AppContainer")
    } else {
        None
    };

    Facts {
        provider,
        state_dir_writable,
        evolution_db_ok,
        can_bind_local,
        workspace_writable,
        git_available,
        skills_dir_present,
        mcp_server_count,
        sandbox_tier_env,
        sandbox_tier_source,
        sandbox_backend,
        sandbox_unavailable_reason,
        abac_policies,
        // Read after the config load above, in this same process.
        ignored_config_layers: rightkit_agent_config::ConfigLoader::ignored_layers(),
        unknown_config_keys: rightkit_agent_config::ConfigLoader::unknown_keys(),
        provider_reach,
    }
}
