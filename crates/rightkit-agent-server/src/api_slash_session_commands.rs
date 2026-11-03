use super::*;

pub(crate) async fn command_usage(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let session_id = args
        .iter()
        .find(|arg| !arg.trim().is_empty())
        .map(|arg| arg.trim())
        .or_else(|| {
            active_session_id
                .map(str::trim)
                .filter(|arg| !arg.is_empty())
        });
    let Some(session_id) = session_id else {
        return CommandResult {
            success: false,
            output: "Usage: /usage in an active session, or /usage <session-id>.".into(),
            action: CommandAction::None,
        };
    };

    let events = match session_events_for_usage(state, session_id).await {
        Ok(events) => events,
        Err(err) => {
            return CommandResult {
                success: false,
                output: err,
                action: CommandAction::None,
            };
        }
    };

    // Prompt-cache breaks recorded in session history.
    let mut expected_breaks = 0usize;
    let mut unexpected_breaks = 0usize;
    let mut unexpected_tokens_lost = 0i64;
    let mut unexpected_cost_lost = 0.0f64;
    for event in &events {
        if let ProtocolEvent::PromptCacheBreak {
            expected,
            tokens_lost,
            estimated_cost_lost,
            ..
        } = event
        {
            if *expected {
                expected_breaks += 1;
            } else {
                unexpected_breaks += 1;
                unexpected_tokens_lost += *tokens_lost;
                unexpected_cost_lost += estimated_cost_lost.unwrap_or(0.0);
            }
        }
    }

    let latest = events.into_iter().rev().find_map(|event| match event {
        ProtocolEvent::UsageUpdated {
            prompt_tokens,
            completion_tokens,
            cost_usd,
            context_tokens,
            context_percent,
            cache_read_tokens,
            cache_creation_tokens,
            ..
        } => Some((
            prompt_tokens,
            completion_tokens,
            cost_usd,
            context_tokens,
            context_percent,
            cache_read_tokens,
            cache_creation_tokens,
        )),
        _ => None,
    });

    let Some((
        prompt_tokens,
        completion_tokens,
        cost_usd,
        context_tokens,
        context_percent,
        cache_read_tokens,
        cache_creation_tokens,
    )) = latest
    else {
        return CommandResult {
            success: true,
            output: format!("**Usage:** {session_id}\nNo usage events recorded yet."),
            action: CommandAction::None,
        };
    };

    let cache_hit_pct = rightkit_agent_engine_core::usage::cache_hit_rate_pct(
        u64::try_from(prompt_tokens).unwrap_or(0),
        u64::try_from(cache_read_tokens).unwrap_or(0),
        u64::try_from(cache_creation_tokens).unwrap_or(0),
    );
    let cache_breaks = if expected_breaks + unexpected_breaks == 0 {
        "none".to_string()
    } else if unexpected_cost_lost > 0.0 {
        format!(
            "{unexpected_breaks} unexpected ({unexpected_tokens_lost} tokens, ~${unexpected_cost_lost:.4} lost), {expected_breaks} expected"
        )
    } else {
        format!(
            "{unexpected_breaks} unexpected ({unexpected_tokens_lost} tokens lost), {expected_breaks} expected"
        )
    };
    CommandResult {
        success: true,
        output: format!(
            "**Usage:** {session_id}\n\
Prompt tokens: {prompt_tokens}\n\
Completion tokens: {completion_tokens}\n\
Context tokens: {context_tokens} ({:.1}%)\n\
Cache read tokens: {cache_read_tokens}\n\
Cache creation tokens: {cache_creation_tokens}\n\
Cache hit rate: {cache_hit_pct:.1}%\n\
Cache breaks: {cache_breaks}\n\
Estimated cost: ${cost_usd:.6}",
            context_percent * 100.0
        ),
        action: CommandAction::None,
    }
}

async fn session_events_for_usage(
    state: &DaemonState,
    session_id: &str,
) -> Result<Vec<ProtocolEvent>, String> {
    if let Some(live) = state.get_session(session_id) {
        return Ok(live.conduit.events_after(0));
    }
    let Some(storage) = state.storage.clone() else {
        return Err(format!("Session not found: {session_id}"));
    };
    let session_id = session_id.to_string();
    let lookup_session_id = session_id.clone();
    let rows = tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store
            .events_after(&lookup_session_id, 0)
            .ok()
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        return Err(format!("Session not found: {session_id}"));
    }
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .collect())
}

pub(crate) async fn command_model(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let Some((session_id, requested_model)) =
        resolve_model_command_target(state, args, active_session_id).await
    else {
        return CommandResult {
            success: false,
            output: "Usage: /model [model] in an active session, or /model <session-id> [model]."
                .into(),
            action: CommandAction::None,
        };
    };

    if requested_model.is_empty() {
        return match read_session_model(state, session_id).await {
            Ok(row) => {
                let display_model = if row.model.is_empty() {
                    "(default model from daemon settings)".to_string()
                } else {
                    format!("`{}`", row.model)
                };
                CommandResult {
                    success: true,
                    output: format!(
                        "**Session model:** {session_id}\nCurrent: {display_model}\nStatus: {}",
                        row.status
                    ),
                    action: CommandAction::None,
                }
            }
            Err(StatusCode::NOT_FOUND) => CommandResult {
                success: false,
                output: format!("Session not found: {session_id}"),
                action: CommandAction::None,
            },
            Err(_) => CommandResult {
                success: false,
                output: format!("Failed to read model for session: {session_id}"),
                action: CommandAction::None,
            },
        };
    }

    match update_session_model(state, session_id, &requested_model).await {
        Ok(applied_live) => CommandResult {
            success: true,
            output: format!(
                "**Session model updated:** {session_id}\nModel: `{}`\nApplies to: {}",
                requested_model,
                if applied_live {
                    "live session"
                } else {
                    "next resume"
                }
            ),
            action: CommandAction::None,
        },
        Err(StatusCode::BAD_REQUEST) => CommandResult {
            success: false,
            output: "Usage: /model [model] in an active session, or /model <session-id> [model]."
                .into(),
            action: CommandAction::None,
        },
        Err(StatusCode::CONFLICT) => CommandResult {
            success: false,
            output: format!(
                "Model switch refused for session: {session_id} (sub-agent route is settings-only, or a turn is in flight/queued)"
            ),
            action: CommandAction::None,
        },
        Err(StatusCode::NOT_FOUND) => CommandResult {
            success: false,
            output: format!("Session not found: {session_id}"),
            action: CommandAction::None,
        },
        Err(_) => CommandResult {
            success: false,
            output: format!("Failed to update model for session: {session_id}"),
            action: CommandAction::None,
        },
    }
}

async fn resolve_model_command_target<'a>(
    state: &DaemonState,
    args: &'a [String],
    active_session_id: Option<&'a str>,
) -> Option<(&'a str, String)> {
    let active_session_id = active_session_id
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty());

    let first = args
        .first()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty());
    let Some(first) = first else {
        return active_session_id.map(|session_id| (session_id, String::new()));
    };

    if active_session_id.is_some() && read_session_model(state, first).await.is_err() {
        let model = args
            .iter()
            .map(|arg| arg.trim())
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return active_session_id.map(|session_id| (session_id, model));
    }

    let model = args
        .iter()
        .skip(1)
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some((first, model))
}

/// `/effort [low|medium|high]` — the daemon executor for the shared
/// registry row. Effort-only updates go through the SAME atomic route
/// authority every other surface uses (`update_session_effort`, a delegate
/// of `update_session_route`): the session's committed model and provider
/// identity are read inside the route gate (or verbatim from the dormant
/// row), so an effort change can neither overwrite a concurrent /model
/// commit nor re-resolve away a provider pin — and model + provider +
/// effort still move as one config_json rewrite with live-route rollback.
/// There is no second effort writer.
pub(crate) async fn command_effort(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let Some((session_id, requested_effort)) =
        resolve_model_command_target(state, args, active_session_id).await
    else {
        return CommandResult {
            success: false,
            output: "Usage: /effort [low|medium|high] in an active session, or /effort <session-id> [low|medium|high]."
                .into(),
            action: CommandAction::None,
        };
    };

    if requested_effort.is_empty() {
        return match read_session_model(state, session_id).await {
            Ok(row) => {
                let display_effort = if row.effort.is_empty() {
                    "(default effort from daemon settings)".to_string()
                } else {
                    format!("`{}`", row.effort)
                };
                CommandResult {
                    success: true,
                    output: format!(
                        "**Session effort:** {session_id}\nCurrent: {display_effort}\nStatus: {}",
                        row.status
                    ),
                    action: CommandAction::None,
                }
            }
            Err(StatusCode::NOT_FOUND) => CommandResult {
                success: false,
                output: format!("Session not found: {session_id}"),
                action: CommandAction::None,
            },
            Err(_) => CommandResult {
                success: false,
                output: format!("Failed to read effort for session: {session_id}"),
                action: CommandAction::None,
            },
        };
    }

    // Normalization BEFORE any state access: an invalid level must never
    // reach the route updater or durable store.
    let effort = match normalize_effort(Some(requested_effort.clone())) {
        Ok(Some(effort)) => effort,
        Ok(None) => {
            return CommandResult {
                success: false,
                output: "Usage: /effort [low|medium|high] in an active session, or /effort <session-id> [low|medium|high]."
                    .into(),
                action: CommandAction::None,
            };
        }
        Err(reason) => {
            return CommandResult {
                success: false,
                output: reason,
                action: CommandAction::None,
            };
        }
    };

    // The effort-only delegate preserves the session's committed model and
    // provider identity inside the route gate itself — no pre-gate model
    // read here, so nothing stale can be written back over a concurrent
    // /model commit.
    match update_session_effort(state, session_id, &effort).await {
        Ok(applied_live) => CommandResult {
            success: true,
            output: format!(
                "**Session effort updated:** {session_id}\nEffort: `{effort}`\nApplies to: {}",
                if applied_live {
                    "live session"
                } else {
                    "next resume"
                }
            ),
            action: CommandAction::None,
        },
        Err(StatusCode::BAD_REQUEST) => CommandResult {
            success: false,
            output: format!(
                "Effort update refused for session: {session_id} (no committed model, or the session's route is qualified and cannot take a legacy patch)"
            ),
            action: CommandAction::None,
        },
        Err(StatusCode::CONFLICT) => CommandResult {
            success: false,
            output: format!(
                "Effort switch refused for session: {session_id} (sub-agent route is settings-only, or a turn is in flight/queued)"
            ),
            action: CommandAction::None,
        },
        Err(StatusCode::NOT_FOUND) => CommandResult {
            success: false,
            output: format!("Session not found: {session_id}"),
            action: CommandAction::None,
        },
        Err(_) => CommandResult {
            success: false,
            output: format!("Failed to update effort for session: {session_id}"),
            action: CommandAction::None,
        },
    }
}

pub(crate) struct SessionModelRow {
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) status: String,
}

pub(crate) async fn read_session_model(
    state: &DaemonState,
    session_id: &str,
) -> Result<SessionModelRow, StatusCode> {
    if let Some(live) = state.get_session(session_id) {
        return Ok(SessionModelRow {
            model: lock_recover(&live.model, "session.model").clone(),
            effort: lock_recover(&live.effort, "session.effort")
                .clone()
                .unwrap_or_default(),
            status: "running".to_string(),
        });
    }

    let Some(storage) = state.storage.clone() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let session_id = session_id.to_string();
    let session_id_for_log = session_id.clone();
    tokio::task::spawn_blocking(move || {
        let s = storage.blocking_lock();
        let row = s.get_session(&session_id)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let config: serde_json::Value =
            serde_json::from_str(&row.config_json).unwrap_or(serde_json::Value::Null);
        Ok::<Option<SessionModelRow>, rightkit_agent_engine_storage::StorageError>(Some(
            SessionModelRow {
                model: config
                    .get("model")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                effort: config
                    .get("effort")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                status: if row.status.is_empty() {
                    "stored".to_string()
                } else {
                    row.status
                },
            },
        ))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        tracing::warn!(session_id = %session_id_for_log, error = %e, "read_session_model failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! `/effort` daemon-executor coverage — authored under the native TUI
    //! absorption lane; execution is parent-owned. Run with:
    //! `rustright cargo test -p coderight --bin coderight api_slash_session_commands`
    use super::*;
    use crate::api::{EngineConfig, SharedStorage};

    fn engine_cfg() -> EngineConfig {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("route fixture process authority");
        EngineConfig {
            api_key: "test-key".into(),
            model_id: "engine-default-model".into(),
            base_url: "https://engine.invalid/v1".into(),
            fallback_models: Vec::new(),
        }
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join("coderight-test-tmp")
            .join(format!("coderight-{label}-{nanos}"))
    }

    async fn live_session(
        label: &str,
    ) -> (
        DaemonState,
        SharedStorage,
        std::path::PathBuf,
        std::sync::Arc<crate::api::LiveSession>,
    ) {
        let dir = unique_temp_dir(label);
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            &format!("{label}-session"),
            None,
            dir.to_str().unwrap(),
            "Effort test",
            None,
        );
        // The initial row write is fire-and-forget; wait for it so the
        // durable half of the update is exercised rather than skipped.
        for _ in 0..50 {
            if storage
                .lock()
                .await
                .get_session(&format!("{label}-session"))
                .ok()
                .flatten()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        (state, storage, dir, live)
    }

    async fn stored_config(storage: &SharedStorage, id: &str) -> serde_json::Value {
        let row = {
            let s = storage.lock().await;
            s.get_session(id).expect("session row readable")
        }
        .expect("session row exists");
        serde_json::from_str(&row.config_json).expect("config_json parses")
    }

    /// A committed `/effort` must land the SAME effort in live state and in
    /// config_json — the atomic durable rewrite is what resume consumes,
    /// and the session's model must survive untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_commits_live_and_durable_effort() {
        let (state, storage, dir, live) = live_session("effort-commit").await;
        let session_id = format!("{}-session", "effort-commit");
        let before_model = lock_recover(&live.model, "session.model").clone();

        let result = command_effort(&state, &["high".to_string()], Some(&session_id)).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            lock_recover(&live.effort, "session.effort").as_deref(),
            Some("high")
        );
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before_model.as_str(),
            "an effort-only update must not move the committed model"
        );

        let cfg = stored_config(&storage, &session_id).await;
        assert_eq!(cfg["effort"], "high");
        assert_eq!(cfg["model"].as_str().unwrap_or(""), before_model.as_str());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An invalid level is rejected by normalization before any route
    /// mutation — the live effort must not move.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_rejects_invalid_level_before_effects() {
        let (state, _storage, dir, live) = live_session("effort-invalid").await;
        let session_id = "effort-invalid-session".to_string();

        let result = command_effort(&state, &["maximum".to_string()], Some(&session_id)).await;
        assert!(!result.success);
        assert!(
            result.output.contains("Invalid effort"),
            "{}",
            result.output
        );
        assert!(lock_recover(&live.effort, "session.effort").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A queued-but-unconsumed admission is busy: `/effort` must refuse
    /// rather than land between admission and the first provider request.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_refuses_while_session_busy() {
        let (state, _storage, dir, live) = live_session("effort-busy").await;
        let session_id = "effort-busy-session".to_string();

        live.pending_admissions
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let result = command_effort(&state, &["low".to_string()], Some(&session_id)).await;
        assert!(!result.success);
        assert!(result.output.contains("refused"), "{}", result.output);
        live.pending_admissions
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown session id is a clean not-found, never a panic or a
    /// silent no-op.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_reports_missing_session() {
        let dir = unique_temp_dir("effort-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage)
            .with_engine(engine_cfg());

        let result = command_effort(
            &state,
            &["effort-missing-session".to_string(), "low".to_string()],
            None,
        )
        .await;
        assert!(!result.success);
        assert!(
            result.output.contains("Session not found"),
            "{}",
            result.output
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bare `/effort` reports the committed selection honestly — including
    /// the "no explicit effort" default — without mutating anything.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_read_reports_current_selection() {
        let (state, _storage, dir, _live) = live_session("effort-read").await;
        let session_id = "effort-read-session".to_string();

        let result = command_effort(&state, &[], Some(&session_id)).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Session effort"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("default effort"),
            "{}",
            result.output
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dormant session's stored route is preserved verbatim: `/effort`
    /// must rewrite only `effort`, never re-resolve or clear the stored
    /// `providerId`/`model` (provider_id=None would otherwise remove the
    /// pin from config_json).
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_preserves_dormant_stored_provider_and_model() {
        let dir = unique_temp_dir("effort-dormant");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        {
            let s = storage.lock().await;
            s.create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "effort-dormant-session".to_string(),
                title: "Dormant effort".to_string(),
                workspace_root: dir.to_str().unwrap().to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                parent_session_id: None,
                role: None,
                status: "stored".to_string(),
                config_json: serde_json::json!({
                    "model": "openrouter/some-model",
                    "providerId": "openrouter",
                    "effort": "low"
                })
                .to_string(),
                tags: "{}".to_string(),
                recovery_marker: None,
            })
            .expect("dormant row written");
        }
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());

        let result = command_effort(
            &state,
            &["effort-dormant-session".to_string(), "high".to_string()],
            None,
        )
        .await;
        assert!(result.success, "{}", result.output);

        let cfg = stored_config(&storage, "effort-dormant-session").await;
        assert_eq!(cfg["model"], "openrouter/some-model");
        assert_eq!(
            cfg["providerId"], "openrouter",
            "stored provider pin must survive an effort-only update"
        );
        assert_eq!(cfg["effort"], "high");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live session's provider pin is read under the route gate and
    /// re-persisted verbatim — an effort-only update must not re-resolve
    /// the provider from the model name or clear the pin.
    #[tokio::test(flavor = "multi_thread")]
    async fn effort_command_preserves_live_provider_pin() {
        let (state, storage, dir, live) = live_session("effort-pin").await;
        let session_id = "effort-pin-session".to_string();
        *lock_recover(&live.model, "session.model") = "minimax/minimax-m2".to_string();
        *lock_recover(&live.provider_id, "session.provider_id") = Some("minimax".to_string());

        let result = command_effort(&state, &["high".to_string()], Some(&session_id)).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            "minimax/minimax-m2"
        );
        assert_eq!(
            lock_recover(&live.provider_id, "session.provider_id").as_deref(),
            Some("minimax"),
            "live provider pin must survive an effort-only update"
        );
        assert_eq!(
            lock_recover(&live.effort, "session.effort").as_deref(),
            Some("high")
        );

        let cfg = stored_config(&storage, &session_id).await;
        assert_eq!(cfg["model"], "minimax/minimax-m2");
        assert_eq!(cfg["providerId"], "minimax");
        assert_eq!(cfg["effort"], "high");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
