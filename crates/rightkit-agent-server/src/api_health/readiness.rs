#[cfg(test)]
pub(crate) use rightkit_agent_engine_api_platform::readiness::ReadinessState;
pub(crate) use rightkit_agent_engine_api_platform::readiness::{
    ReadinessRow, ReadinessSnapshot, ReadinessVerdict,
};

async fn with_readiness_timeout(
    id: &'static str,
    label: &'static str,
    f: impl std::future::Future<Output = ReadinessRow>,
) -> ReadinessRow {
    match tokio::time::timeout(std::time::Duration::from_millis(50), f).await {
        Ok(row) => row,
        Err(_) => ReadinessRow::degraded(id, label, "check timed out"),
    }
}

fn row_provider_key(state: &DaemonState) -> ReadinessRow {
    if let Some(cfg) = &state.engine_cfg {
        if !cfg.api_key.is_empty() {
            return ReadinessRow::ready(
                "provider-key",
                "Provider API key",
                "key configured at startup",
            );
        }
    }
    let found = rightkit_llm::agent_providers::PROVIDERS
        .iter()
        .any(|p| rightkit_llm::agent_providers::load_api_key(p.id).is_some());
    if found {
        ReadinessRow::ready(
            "provider-key",
            "Provider API key",
            "key found for known provider",
        )
    } else {
        ReadinessRow::blocked(
            "provider-key",
            "Provider API key",
            "no provider API key configured",
            "Set OPENROUTER_API_KEY or add an OpenRouter key in Settings > Providers",
        )
    }
}

fn row_default_model(state: &DaemonState) -> ReadinessRow {
    if let Some(cfg) = &state.engine_cfg {
        if !cfg.model_id.is_empty() {
            return ReadinessRow::ready(
                "default-model",
                "Default model",
                format!("model: {}", cfg.model_id),
            );
        }
    }
    let model = state
        .settings
        .lock()
        .unwrap()
        .primary_orchestrator_model_id
        .clone();
    ReadinessRow::ready(
        "default-model",
        "Default model",
        format!("conductor routes to {model}"),
    )
}

fn row_local_backend(state: &DaemonState) -> ReadinessRow {
    let registry = lock_recover(&state.execution_backends, "state.execution_backends");
    match registry.backends.iter().find(|b| b.id == "local") {
        Some(b) if b.enabled => ReadinessRow::ready(
            "local-backend",
            "Local backend",
            "local execution available",
        ),
        Some(_) => ReadinessRow::blocked(
            "local-backend",
            "Local backend",
            "local backend disabled",
            "Enable local backend in Settings > Execution",
        ),
        None => ReadinessRow::degraded(
            "local-backend",
            "Local backend",
            "local backend not in registry",
        ),
    }
}

fn row_telegram_remote(state: &DaemonState) -> ReadinessRow {
    if state.telegram_status.token_configured {
        ReadinessRow::ready("telegram-remote", "Telegram remote", "bot token configured")
    } else {
        ReadinessRow::unavailable(
            "telegram-remote",
            "Telegram remote",
            "not configured (optional)",
        )
    }
}

fn row_schedules(state: &DaemonState) -> ReadinessRow {
    if state.has_license_feature("schedules") {
        ReadinessRow::ready("schedules", "Schedules", "schedule execution enabled")
    } else {
        ReadinessRow::unavailable("schedules", "Schedules", "requires Pro license")
    }
}

fn row_mcp(state: &DaemonState) -> ReadinessRow {
    match crate::api_mcp::read_mcp_config_file(&state.mcp_user_config_path) {
        Ok(cfg) if !cfg.mcp_servers.is_empty() => {
            let n = cfg.mcp_servers.len();
            ReadinessRow::ready(
                "mcp",
                "MCP servers",
                format!("{n} server{} configured", if n == 1 { "" } else { "s" }),
            )
        }
        Ok(_) => {
            ReadinessRow::unavailable("mcp", "MCP servers", "no MCP servers configured (optional)")
        }
        Err(_) => ReadinessRow::degraded("mcp", "MCP servers", "could not read MCP config"),
    }
}

fn row_license(state: &DaemonState) -> ReadinessRow {
    let features = state.current_license_features();
    if features.is_empty() {
        ReadinessRow::warn(
            "license",
            "License",
            "Free tier — advanced features locked",
            "Activate a Pro license via Settings > License",
        )
    } else {
        ReadinessRow::ready(
            "license",
            "License",
            format!(
                "{} feature{} enabled",
                features.len(),
                if features.len() == 1 { "" } else { "s" }
            ),
        )
    }
}

fn row_permission_rules(state: &DaemonState) -> ReadinessRow {
    match crate::api_stores::refresh_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
    ) {
        Some(error) => ReadinessRow::blocked(
            "permission-rules",
            "Permission rules",
            error.degraded_reason(),
            "Repair or delete the permission rules file, then retry",
        ),
        None => ReadinessRow::ready(
            "permission-rules",
            "Permission rules",
            "persisted rules loaded",
        ),
    }
}

fn row_updater() -> ReadinessRow {
    // The Tauri app embeds the updater pubkey at build time (tauri.conf.json); the
    // daemon doesn't have direct access to it but can trust it is present in packaged builds.
    ReadinessRow::ready("updater", "Auto-update", "pubkey configured at build time")
}

pub(crate) async fn readiness_snapshot(state: &DaemonState) -> ReadinessSnapshot {
    let rows = vec![
        with_readiness_timeout("provider-key", "Provider API key", async {
            row_provider_key(state)
        })
        .await,
        with_readiness_timeout("default-model", "Default model", async {
            row_default_model(state)
        })
        .await,
        with_readiness_timeout("local-backend", "Local backend", async {
            row_local_backend(state)
        })
        .await,
        with_readiness_timeout("telegram-remote", "Telegram remote", async {
            row_telegram_remote(state)
        })
        .await,
        with_readiness_timeout("schedules", "Schedules", async { row_schedules(state) }).await,
        with_readiness_timeout("mcp", "MCP servers", async { row_mcp(state) }).await,
        with_readiness_timeout("license", "License", async { row_license(state) }).await,
        with_readiness_timeout("permission-rules", "Permission rules", async {
            row_permission_rules(state)
        })
        .await,
        with_readiness_timeout("updater", "Auto-update", async { row_updater() }).await,
    ];

    let verdict = if rows.iter().any(|r| r.verdict == ReadinessVerdict::Block) {
        ReadinessVerdict::Block
    } else if rows.iter().any(|r| r.verdict == ReadinessVerdict::Warn) {
        ReadinessVerdict::Warn
    } else {
        ReadinessVerdict::Pass
    };

    ReadinessSnapshot { verdict, rows }
}

async fn health_readiness(State(state): State<DaemonState>) -> Json<ReadinessSnapshot> {
    Json(readiness_snapshot(&state).await)
}
