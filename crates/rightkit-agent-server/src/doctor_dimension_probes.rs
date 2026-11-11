// Five security / runtime dimension probe functions + helpers.
// Included from `doctor.rs` via include! (the dispatcher in
// `doctor_dimensions.rs` references these).
//
// Each probe pulls a real runtime source (construct an instance of the engine
// the dimension represents, or read a known config path) and reports one of:
//   pass / warn / block / unknown
// `unknown` is reserved for dimensions where no client-configurable accessor
// is reachable from the doctor CLI yet — never a fake pass.

/// Probe the policy ABAC layer availability.
///
/// The in-code default-constructible PDP proves the ABAC engine is linked from
/// this build, but per the spec ("If a dimension has no accessor yet, surface
/// it as status='unknown', not a fake pass."), a default-only PDP without an
/// operator-visible config file is reported as `unknown`. When
/// `<state root>/policy.toml` exists we use the public `from_yaml_like`
/// accessor; success => `pass`, parse failure => `block`.
pub(crate) fn probe_policy(verbose: bool) -> Dimension {
    use rightkit_agent_policy::PolicyDecisionPoint;

    let mut evidence: Vec<Evidence> = Vec::new();

    let home = match coderight_home() {
        Some(h) => h,
        None => {
            evidence.push(Evidence {
                id: "pdp_default".to_string(),
                label: "PolicyDecisionPoint::new()".to_string(),
                status: "pass",
                detail: "ABAC engine constructible".to_string(),
            });
            let detail =
                "policy: ABAC engine constructible but no state directory to inspect".to_string();
            return if verbose {
                dim_with_evidence("unknown", detail, evidence)
            } else {
                dim("unknown", detail)
            };
        }
    };

    let path = home.join("policy.toml");
    match std::fs::read_to_string(&path) {
        Err(_) => {
            evidence.push(Evidence {
                id: "pdp_default".to_string(),
                label: "PolicyDecisionPoint::new()".to_string(),
                status: "pass",
                detail: "ABAC engine constructible".to_string(),
            });
            let detail = format!(
                "policy: ABAC engine constructible but no operator policy.toml at {}",
                path.display()
            );
            if verbose {
                dim_with_evidence("unknown", detail, evidence)
            } else {
                dim("unknown", detail)
            }
        }
        Ok(text) => match PolicyDecisionPoint::from_yaml_like(&text) {
            Ok(_pdp) => {
                evidence.push(Evidence {
                    id: "pdp_yaml".to_string(),
                    label: "policy.toml parses via from_yaml_like".to_string(),
                    status: "pass",
                    detail: format!("loaded from {}", path.display()),
                });
                let detail = format!(
                    "policy: operator policy.toml parses cleanly at {}",
                    path.display()
                );
                if verbose {
                    dim_with_evidence("pass", detail, evidence)
                } else {
                    dim("pass", detail)
                }
            }
            Err(err) => {
                evidence.push(Evidence {
                    id: "pdp_yaml".to_string(),
                    label: "policy.toml parses via from_yaml_like".to_string(),
                    status: "block",
                    detail: format!("parse error: {err} at {}", path.display()),
                });
                let detail = format!(
                    "policy: operator policy.toml at {} failed to parse: {err}",
                    path.display()
                );
                if verbose {
                    dim_with_evidence("block", detail, evidence)
                } else {
                    dim("block", detail)
                }
            }
        },
    }
}

/// Probe workspace trust by reading `RIGHTKIT_WORKSPACE_TRUST` and parsing
/// via the canonical `parse_workspace_trust` helper.
pub(crate) fn probe_workspace_trust(verbose: bool) -> Dimension {
    use rightkit_agent_policy::{parse_workspace_trust, WorkspaceTrust};

    let raw = std::env::var("RIGHTKIT_WORKSPACE_TRUST").ok();
    let parsed = parse_workspace_trust(raw.as_deref());
    let (status, detail) = match parsed {
        WorkspaceTrust::Trusted => (
            "pass",
            "workspace-trust: Trusted (default for fully-trusted repos)".to_string(),
        ),
        WorkspaceTrust::Sandboxed => (
            "pass",
            "workspace-trust: Sandboxed (read/write sandbox enforced)".to_string(),
        ),
        WorkspaceTrust::Untrusted => (
            "warn",
            "workspace-trust: Untrusted (operator input rejected)".to_string(),
        ),
        WorkspaceTrust::Locked => (
            "block",
            "workspace-trust: Locked (all writes denied)".to_string(),
        ),
    };

    if verbose {
        let evidence = vec![Evidence {
            id: "workspace_trust_env".to_string(),
            label: "RIGHTKIT_WORKSPACE_TRUST".to_string(),
            status,
            detail: format!(
                "raw={:?}, parsed={:?}",
                raw,
                format!("{:?}", parsed).to_ascii_lowercase()
            ),
        }];
        dim_with_evidence(status, detail, evidence)
    } else {
        dim(status, detail)
    }
}

/// Probe hook-deny readiness: confirm the hooks crate's destructive guard is
/// constructible AND the engine-side destructive-guard config is on disk.
pub(crate) fn probe_hooks(verbose: bool) -> Dimension {
    use rightkit_hooks::agent_hooks::DestructiveGuard;

    // Constructable in code → the hook-deny engine is wired into this build.
    let _ = DestructiveGuard::default();
    let mut evidence = vec![Evidence {
        id: "destructive_guard_default".to_string(),
        label: "DestructiveGuard::default()".to_string(),
        status: "pass",
        detail: "destructive shell guard constructible".to_string(),
    }];

    // Try to locate assets/hooks/destructive-guard.toml relative to a few
    // candidate roots. This is best-effort: missing config is "warn", not
    // "block", because the in-code defaults still protect operators.
    let probe_paths = hook_config_probe_paths();
    let found = probe_paths.iter().find(|p| p.is_file()).cloned();
    let candidate_list = probe_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    match found {
        Some(path) => {
            evidence.push(Evidence {
                id: "destructive_guard_toml".to_string(),
                label: "destructive-guard.toml".to_string(),
                status: "pass",
                detail: format!("config present at {}", path.display()),
            });
            let detail = format!(
                "hooks: destructive guard constructible, config at {}",
                path.display()
            );
            if verbose {
                dim_with_evidence("pass", detail, evidence)
            } else {
                dim("pass", detail)
            }
        }
        None => {
            evidence.push(Evidence {
                id: "destructive_guard_toml".to_string(),
                label: "destructive-guard.toml".to_string(),
                status: "warn",
                detail: format!("config not found in: {candidate_list}"),
            });
            let detail = "hooks: destructive guard constructible (in-code defaults), no destructive-guard.toml probed"
                .to_string();
            if verbose {
                dim_with_evidence("warn", detail, evidence)
            } else {
                dim("warn", detail)
            }
        }
    }
}

/// Probe the CSPRNG source: read 32 bytes via `getrandom::getrandom` (the same
/// crate the daemon's startup token uses) and confirm it succeeded.
pub(crate) fn probe_csprng(verbose: bool) -> Dimension {
    let mut bytes = [0u8; 32];
    match getrandom::fill(&mut bytes) {
        Ok(()) => {
            let all_zero = bytes.iter().all(|b| *b == 0);
            let status = if all_zero { "block" } else { "pass" };
            let detail = if all_zero {
                "csprng: 32-byte sample is all zero — not a real CSPRNG".to_string()
            } else {
                format!(
                    "csprng: 32-byte sample from getrandom OK (first byte 0x{:02x})",
                    bytes[0]
                )
            };
            let evidence = vec![Evidence {
                id: "getrandom_32".to_string(),
                label: "getrandom::fill([u8; 32])".to_string(),
                status,
                detail: format!("{} bytes produced, all_zero={}", bytes.len(), all_zero),
            }];
            if verbose {
                dim_with_evidence(status, detail, evidence)
            } else {
                dim(status, detail)
            }
        }
        Err(err) => {
            let status = "block";
            let detail = format!("csprng: getrandom::getrandom failed: {err}");
            let evidence = vec![Evidence {
                id: "getrandom_32".to_string(),
                label: "getrandom::fill([u8; 32])".to_string(),
                status,
                detail: err.to_string(),
            }];
            if verbose {
                dim_with_evidence(status, detail, evidence)
            } else {
                dim(status, detail)
            }
        }
    }
}

/// Probe learning/self-improve state: try to load `<state root>/instincts.json`
/// via the canonical `InstinctStore::load_from_json` accessor. Missing file
/// is reported as `unknown` (no live store surface is reachable from the
/// doctor CLI today).
pub(crate) fn probe_learning(verbose: bool) -> Dimension {
    use rightkit_agent_learning::InstinctStore;

    let Some(home) = coderight_home() else {
        let detail =
            "learning: no state directory — cannot locate the RightKit state directory".to_string();
        return if verbose {
            dim_with_evidence("unknown", detail, Vec::new())
        } else {
            dim("unknown", detail)
        };
    };

    let path = home.join("instincts.json");
    match std::fs::read_to_string(&path) {
        Err(_) => {
            let detail =
                "learning: no instincts.json in the RightKit state directory — no live learning surface reachable from this CLI".to_string();
            if verbose {
                dim_with_evidence("unknown", detail, Vec::new())
            } else {
                dim("unknown", detail)
            }
        }
        Ok(text) => match InstinctStore::load_from_json(&path.to_string_lossy()) {
            Ok(store) => {
                let active = store.active().len();
                let total = store.len();
                let status = "pass";
                let detail = format!(
                    "learning: {active} active / {total} total instinct(s) at {}",
                    path.display()
                );
                let evidence = vec![Evidence {
                    id: "instincts_load".to_string(),
                    label: "InstinctStore::load_from_json".to_string(),
                    status,
                    detail: format!("active={active}, total={total}, path={}", path.display()),
                }];
                if verbose {
                    dim_with_evidence(status, detail, evidence)
                } else {
                    dim(status, detail)
                }
            }
            Err(_) => {
                // Tolerate a raw JSON-array payload (operators sometimes edit a
                // simpler list). Reuse the `text` we already read — no double
                // file-read.
                let arr_count = serde_json::from_str::<Vec<serde_json::Value>>(&text)
                    .map(|v| v.len())
                    .unwrap_or(0);
                let detail = if arr_count > 0 {
                    format!(
                        "learning: instincts.json at {} is a raw JSON array ({arr_count} entries); cannot decode as InstinctStore",
                        path.display()
                    )
                } else {
                    "learning: instincts.json in the RightKit state directory present but invalid InstinctStore JSON".to_string()
                };
                let status = if arr_count > 0 { "warn" } else { "block" };
                let evidence = vec![Evidence {
                    id: "instincts_load".to_string(),
                    label: "InstinctStore::load_from_json".to_string(),
                    status,
                    detail: if arr_count > 0 {
                        format!("raw JSON array with {arr_count} entries")
                    } else {
                        format!("path={}, decode failed", path.display())
                    },
                }];
                if verbose {
                    dim_with_evidence(status, detail, evidence)
                } else {
                    dim(status, detail)
                }
            }
        },
    }
}

/// Helper: locate the RightKit state directory (the rightkit-daemon AppRoot).
pub(crate) fn coderight_home() -> Option<PathBuf> {
    rightkit_agent_config::state_root()
}

/// Helper: candidate locations for `destructive-guard.toml`.
pub(crate) fn hook_config_probe_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(h) = coderight_home() {
        paths.push(h.join("hooks").join("destructive-guard.toml"));
        paths.push(h.join("destructive-guard.toml"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(
            cwd.join("assets")
                .join("hooks")
                .join("destructive-guard.toml"),
        );
        paths.push(
            cwd.join(".coderight")
                .join("hooks")
                .join("destructive-guard.toml"),
        );
    }
    paths
}
