use super::*;

pub(crate) fn untrusted_policy_action(trust: &TrustEnvelope, content: &str) -> serde_json::Value {
    let (action, approval_authority, policy_change_authority, reason) = match trust.trust {
        ContentTrust::RemoteAgentUntrusted => (
            "review_required",
            false,
            false,
            "Remote-agent content is context only; it cannot approve tools, change policy, or grant permissions.",
        ),
        ContentTrust::ToolUntrusted => (
            "treat_as_data",
            false,
            false,
            "Tool output is untrusted data; contained instructions cannot approve tools or change policy.",
        ),
        _ if trust.instructions_are_data => (
            "treat_as_data",
            false,
            false,
            "This content is marked instructions-as-data and cannot authorize actions.",
        ),
        _ => (
            "none",
            true,
            true,
            "Trusted content does not require an untrusted-context policy action.",
        ),
    };
    serde_json::json!({
        "action": action,
        "approvalAuthority": approval_authority,
        "policyChangeAuthority": policy_change_authority,
        "instructionsAreData": trust.instructions_are_data,
        "blockedAuthorityRequests": blocked_authority_requests(content),
        "reason": reason,
    })
}

fn blocked_authority_requests(content: &str) -> Vec<serde_json::Value> {
    let lower = content.to_ascii_lowercase();
    let mut blocked = Vec::new();
    if contains_any(
        &lower,
        &[
            "approve",
            "approval",
            "allow tool",
            "grant permission",
            "permission granted",
            "bypass approval",
        ],
    ) {
        blocked.push(serde_json::json!({
            "kind": "approval_authority",
            "blocked": true,
            "reason": "Untrusted content cannot approve tools or grant permissions.",
        }));
    }
    if contains_any(
        &lower,
        &[
            "change policy",
            "ignore policy",
            "ignore your policy",
            "approval policy",
            "tool policy",
            "permission mode",
            "allowed tools",
            "disallowed tools",
            "disable safety",
        ],
    ) {
        blocked.push(serde_json::json!({
            "kind": "policy_change_authority",
            "blocked": true,
            "reason": "Untrusted content cannot change RightKit tool or permission policy.",
        }));
    }
    blocked
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

pub(crate) async fn transcript_for_session(
    state: &DaemonState,
    id: &str,
) -> Result<Vec<TranscriptMessage>, StatusCode> {
    if let Some(live) = state.get_session(id) {
        return Ok(lock_recover(&live.transcript, "session.transcript").clone());
    }
    let storage = state.storage.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let storage2 = storage.clone();
    let id2 = id.to_string();
    let events = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store.all_events(&id2)
    })
    .await
    .map_err(|_| StatusCode::NOT_FOUND)?
    .map_err(|_| StatusCode::NOT_FOUND)?;
    if events.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(crate::rehydrate::rehydrate_transcript(&events))
}

pub(crate) fn transcript_created_at(message: &TranscriptMessage) -> Option<String> {
    match message {
        TranscriptMessage::System { created_at, .. }
        | TranscriptMessage::User { created_at, .. }
        | TranscriptMessage::Assistant { created_at, .. }
        | TranscriptMessage::Tool { created_at, .. }
        | TranscriptMessage::Compact { created_at, .. }
        | TranscriptMessage::Fold { created_at, .. } => Some(created_at.clone()),
    }
}

pub(crate) fn away_summary_line(message: &TranscriptMessage) -> Option<String> {
    match message {
        TranscriptMessage::User { content, .. } => {
            Some(format!("User: {}", summarize_text(content, 140)))
        }
        TranscriptMessage::Assistant { content, .. } if !content.trim().is_empty() => {
            Some(format!("RightKit: {}", summarize_text(content, 180)))
        }
        TranscriptMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => Some(format!(
            "RightKit requested tools: {}",
            tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        TranscriptMessage::Tool {
            summary, is_error, ..
        } => Some(format!(
            "{}: {}",
            if *is_error { "Tool error" } else { "Tool" },
            summarize_text(summary, 160)
        )),
        TranscriptMessage::Compact {
            summary,
            dropped_count,
            ..
        } => Some(format!(
            "Compacted {dropped_count} older messages: {}",
            summarize_text(summary, 160)
        )),
        TranscriptMessage::Fold {
            summary,
            subtask_id,
            dropped_count,
            ..
        } => Some(format!(
            "Folded subtask {subtask_id} ({dropped_count} messages): {}",
            summarize_text(summary, 160)
        )),
        TranscriptMessage::System { .. } => None,
        TranscriptMessage::Assistant { .. } => None,
    }
}

pub(crate) fn summarize_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    out.push('…');
    out
}

pub(crate) fn kind_matches(data: &str, filter: &Option<String>) -> bool {
    match filter {
        None => true,
        Some(k) => serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|v| v.get("$kind")?.as_str().map(|s| s == k.as_str()))
            .unwrap_or(false),
    }
}

pub(crate) async fn update_session_model(
    state: &DaemonState,
    id: &str,
    model: &str,
) -> Result<bool, StatusCode> {
    update_session_route(state, id, model, None, None).await
}

/// True while a native route update cannot commit safely: a provider request
/// is in flight, a turn is admitted/running, or a user turn is queued in the
/// engine channel. Checked while holding `route_update_gate` so a queued
/// admission cannot slip between the check and the commit.
pub(crate) fn session_route_update_busy(live: &LiveSession) -> Option<&'static str> {
    if live
        .inference_active_requests
        .load(std::sync::atomic::Ordering::Acquire)
        > 0
    {
        return Some("provider request in flight");
    }
    if live.live_turn_handle.is_active() {
        return Some("turn in flight");
    }
    if live
        .pending_admissions
        .load(std::sync::atomic::Ordering::Acquire)
        > 0
    {
        return Some("admitted turn queued for engine");
    }
    // Admission paths that do not take the route gate (internal producers)
    // still occupy channel slots; a non-empty queue means an unconsumed turn.
    if live
        .msg_tx
        .as_ref()
        .is_some_and(|tx| tx.capacity() < crate::api::api_state::SESSION_MSG_CHANNEL_CAPACITY)
    {
        return Some("turn queued for engine");
    }
    None
}

/// Switch a session's model, optionally pinning the provider route and the
/// reasoning effort. An explicit provider binds that configured/native route
/// exactly; without one the route is still inferred from the model name.
///
/// Ordering contract: the target route is fully validated before any live or
/// durable state changes. For a live session the update runs under
/// `route_update_gate`, refuses while a turn is admitted/active/queued, and
/// publishes live route fields only after the atomic durable write commits,
/// so memory and `config_json` can never diverge on the provider a request
/// actually uses.
pub(crate) async fn update_session_route(
    state: &DaemonState,
    id: &str,
    model: &str,
    provider_id: Option<&str>,
    effort: Option<&str>,
) -> Result<bool, StatusCode> {
    update_session_route_inner(state, id, Some(model), provider_id, effort).await
}

/// Effort-only sibling of [`update_session_route`] on the SAME authority,
/// `route_update_gate`, and atomic durable rewrite. It carries no
/// caller-supplied model or provider: the committed model and provider
/// identity are read inside the gate for a live session, or verbatim from
/// the stored `config_json` for a dormant one, so an effort change can
/// never re-resolve away a provider pin or overwrite a concurrently
/// committed model.
pub(crate) async fn update_session_effort(
    state: &DaemonState,
    id: &str,
    effort: &str,
) -> Result<bool, StatusCode> {
    update_session_route_inner(state, id, None, None, Some(effort)).await
}

#[allow(clippy::too_many_lines)]
async fn update_session_route_inner(
    state: &DaemonState,
    id: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
    effort: Option<&str>,
) -> Result<bool, StatusCode> {
    // `preserve_route` is the effort-only form: model + provider identity
    // come from the session's own committed state, resolved under the gate
    // (live) or from the durable row (dormant) — never from a caller.
    let preserve_route = model.is_none();
    let mut model = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    if !preserve_route && model.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let provider_id = provider_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let effort = match effort.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            normalize_effort(Some(value.to_string())).map_err(|_| StatusCode::BAD_REQUEST)?
        }
        None => None,
    };

    let live = state.get_session(id);
    let mut exists = live.is_some();
    let mut locked_to_settings = false;
    // Durable provider identity actually consumed by this update. An explicit
    // pin is written verbatim; a model-only update persists the provider the
    // resolution path consumed (or removes a stale pin when the route falls
    // back to the configured engine default, which has no durable identity).
    let mut durable_provider_id = provider_id.clone();

    // A dormant session still has a durable route authority: fetch the row so
    // the same qualified-route rejection and explicit-provider validation run
    // before the write, not after it.
    let dormant_row = if live.is_none() {
        match state.storage.clone() {
            Some(store) => {
                let id_for_lookup = id.to_string();
                tokio::task::spawn_blocking(move || {
                    store.blocking_lock().get_session(&id_for_lookup)
                })
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            }
            None => None,
        }
    } else {
        None
    };
    if let Some(row) = &dormant_row {
        exists = true;
        if row.parent_session_id.is_some() {
            locked_to_settings = true;
        } else {
            let cfg: serde_json::Value =
                serde_json::from_str(&row.config_json).unwrap_or(serde_json::Value::Null);
            if cfg
                .get("inferenceSelection")
                .is_some_and(|value| !value.is_null())
            {
                // Same rule as the live path: the consumer for this row is a
                // qualified route; a legacy model/provider write cannot be
                // committed to it safely.
                return Err(StatusCode::BAD_REQUEST);
            }
            if preserve_route {
                // Effort-only: carry the stored model and providerId
                // verbatim — re-resolving or clearing them here would let an
                // effort change silently rewrite the durable route.
                let stored_model = cfg
                    .get("model")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if stored_model.is_empty() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                model = stored_model.to_string();
                durable_provider_id = cfg
                    .get("providerId")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            } else {
                if let Some(provider) = provider_id.as_deref() {
                    let workspace_config = rightkit_agent_config::ConfigLoader::new()
                        .load_files_for_workspace(FsPath::new(row.workspace_root.trim()));
                    let known = workspace_config.provider_routes.contains_key(provider)
                        || rightkit_llm::agent_providers::find_provider(provider).is_some();
                    if !known {
                        return Err(StatusCode::BAD_REQUEST);
                    }
                }
                if provider_id.is_none() {
                    let workspace_config = rightkit_agent_config::ConfigLoader::new()
                        .load_files_for_workspace(FsPath::new(row.workspace_root.trim()));
                    durable_provider_id =
                        state.resolved_provider_id_for_model(&workspace_config, &model);
                }
            }
        }
    }

    // ── Validate before effects ──────────────────────────────────────────
    // `next_route` is Some whenever the live session can consume a provider
    // route update; computing it here means an unresolvable explicit provider
    // is rejected before anything mutates.
    let _route_gate = match &live {
        Some(session) if !locked_to_settings => Some(session.route_update_gate.lock().await),
        _ => None,
    };
    let mut next_route: Option<crate::provider_client::ProviderRoute> = None;
    if let Some(session) = &live {
        if session.parent_session_id.is_some() {
            locked_to_settings = true;
        } else if lock_recover(&session.inference_selection, "session.inference_selection")
            .is_some()
        {
            // Qualified sessions consume `ResolvedInferenceRoute` through
            // `inference_route_tx`, never the shared ProviderRoute — a legacy
            // model/provider patch cannot be committed to that consumer, so
            // refuse it before effects instead of leaving state split-brained.
            return Err(StatusCode::BAD_REQUEST);
        } else {
            let workspace_config = rightkit_agent_config::ConfigLoader::new()
                .load_files_for_workspace(FsPath::new(session.workspace_root.trim()));
            if preserve_route {
                // Effort-only: read the committed model and provider pin
                // under the route gate. A pre-gate read could clobber a
                // concurrent /model commit, and re-deriving the provider
                // would drop an existing pin.
                model = lock_recover(&session.model, "session.model").clone();
                if model.is_empty() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                durable_provider_id =
                    lock_recover(&session.provider_id, "session.provider_id").clone();
            }
            if let Some(route) = &session.model_route {
                let (engine_cfg, inherited_fallbacks, current_effort) = {
                    let current = lock_recover(route, "session.model_route");
                    let engine_cfg = state.engine_cfg.clone().unwrap_or_else(|| EngineConfig {
                        api_key: current.api_key.clone(),
                        model_id: current.model_id.clone(),
                        base_url: current.base_url.clone(),
                        fallback_models: current.fallback_models.clone(),
                    });
                    (
                        engine_cfg,
                        current.fallback_models.clone(),
                        current.effort.clone(),
                    )
                };
                let next_effort = effort
                    .clone()
                    .or(current_effort)
                    .or_else(|| lock_recover(&session.effort, "session.effort").clone());
                // For an effort-only update the preserved provider identity
                // is the route's provider: resolving it explicitly keeps the
                // live route on the same provider instead of re-deriving one
                // from the model name.
                let route_provider = if preserve_route {
                    durable_provider_id.as_deref()
                } else {
                    provider_id.as_deref()
                };
                let mut resolved = match route_provider {
                    Some(provider) => state
                        .explicit_provider_route(
                            &workspace_config,
                            provider,
                            &model,
                            &engine_cfg,
                            next_effort,
                            session.thinking_budget_tokens,
                            session.disable_thinking,
                        )
                        .ok_or(StatusCode::BAD_REQUEST)?,
                    None => state.provider_route_for_model(
                        &workspace_config,
                        &model,
                        &engine_cfg,
                        next_effort,
                        session.thinking_budget_tokens,
                        session.disable_thinking,
                    ),
                };
                if route_provider.is_none() && !inherited_fallbacks.is_empty() {
                    resolved.fallback_models = inherited_fallbacks;
                }
                if provider_id.is_none() && !preserve_route {
                    durable_provider_id =
                        state.resolved_provider_id_for_model(&workspace_config, &model);
                }
                next_route = Some(resolved);
            } else if let Some(provider) = provider_id.as_deref() {
                // No live route/engine to validate against: still reject an
                // unknown explicit provider instead of silently persisting a
                // pin nothing can serve.
                let known = workspace_config.provider_routes.contains_key(provider)
                    || rightkit_llm::agent_providers::find_provider(provider).is_some();
                if !known {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
        }
    }

    // ── Busy refusal, then durable-first commit under the route gate ────
    // The gate serializes against prompt admission: it is held from the busy
    // check through the durable write AND the live publish, so no turn can be
    // admitted between the commit decision and its persistence — a turn
    // admitted after the gate releases provably runs on the committed route.
    // Live fields are published only AFTER the atomic durable write succeeds,
    // so concurrent snapshot/footer/API reads can never observe a route that
    // was never committed.
    if let Some(session) = &live {
        if _route_gate.is_some() {
            if let Some(reason) = session_route_update_busy(session) {
                tracing::info!(session_id = %id, %reason, "route update refused while session busy");
                return Err(StatusCode::CONFLICT);
            }
        }
    }

    // ── Atomic durable commit ────────────────────────────────────────────
    // The durable write records the effort actually committed to the live
    // route (resolved `next_route.effort`); a dormant update keeps the stored
    // value when the request carried none.
    let effective_effort = next_route
        .as_ref()
        .map(|resolved| resolved.effort.clone())
        .unwrap_or_else(|| effort.clone());
    // The effort value actually written to config_json: the requested/
    // resolved effort, or the stored effort the row preserved when the
    // request omitted one. Live effort is published from this so memory can
    // never drift from the durable value.
    let mut persisted_effort: Option<String> = None;
    let mut storage_failed = false;
    if let Some(store) = state.storage.clone() {
        let id_for_lookup = id.to_string();
        let model_for_storage = model.clone();
        let provider_for_storage = durable_provider_id.clone();
        let effort_for_storage = effective_effort.clone();
        let now = now_iso();
        let storage_outcome = tokio::task::spawn_blocking(move || {
            let s = store.blocking_lock();
            let Some(session) = s.get_session(&id_for_lookup)? else {
                return Ok((false, false, None));
            };
            if session.parent_session_id.is_some() {
                return Ok((true, true, None));
            }
            // One config_json rewrite: model + providerId + effort move
            // together so a partial write can never pin a model to a stale
            // provider. For a dormant session, effort=None preserves the
            // stored value; the live path always passes the effective effort.
            let persisted_effort = match effort_for_storage.as_deref() {
                Some(effort) => Some(effort.to_string()),
                None => serde_json::from_str::<serde_json::Value>(&session.config_json)
                    .ok()
                    .and_then(|cfg| {
                        cfg.get("effort")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    }),
            };
            s.update_session_route(
                &id_for_lookup,
                &model_for_storage,
                provider_for_storage.as_deref(),
                persisted_effort.as_deref(),
                &now,
            )?;
            Ok::<(bool, bool, Option<String>), rightkit_agent_engine_storage::StorageError>((
                true,
                false,
                persisted_effort,
            ))
        })
        .await;
        match storage_outcome {
            Ok(Ok((storage_updated, storage_locked, effort_written))) => {
                exists |= storage_updated;
                locked_to_settings |= storage_locked;
                persisted_effort = effort_written;
                // A live-only update cannot claim durable success when its row vanished.
                storage_failed |= live.is_some() && !storage_updated;
            }
            Ok(Err(e)) => {
                tracing::warn!(session_id = %id, error = %e, "update_session_route persist failed");
                storage_failed = true;
            }
            Err(_) => {
                storage_failed = true;
            }
        }
    }
    if storage_failed || locked_to_settings {
        // Nothing durable committed (or the durable row is settings-locked):
        // live state was never touched, so memory and storage cannot diverge.
        if locked_to_settings {
            return Err(StatusCode::CONFLICT);
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── Live publish — gate still held, durable write already committed ──
    let mut applied_live = false;
    if let Some(session) = &live {
        if _route_gate.is_some() {
            // Snapshot readers retain this guard across the route tuple.
            // Keep it until every published field reflects the durable row.
            let mut model_guard = lock_recover(&session.model, "session.model");
            *model_guard = model.clone();
            if let (Some(route_cell), Some(resolved)) = (&session.model_route, &next_route) {
                *lock_recover(route_cell, "session.model_route") = resolved.clone();
            }
            // Publish the effort actually persisted; when storage is absent
            // only overwrite when an effort was resolved/requested.
            if state.storage.is_some() {
                *lock_recover(&session.effort, "session.effort") = persisted_effort.clone();
            } else if effective_effort.is_some() {
                *lock_recover(&session.effort, "session.effort") = effective_effort.clone();
            }
            *lock_recover(&session.provider_id, "session.provider_id") =
                durable_provider_id.clone();
            drop(model_guard);
            applied_live = next_route.is_some();
        }
    }

    if applied_live {
        if let Some(session) = &live {
            let capacity_model = next_route
                .as_ref()
                .map(|resolved| resolved.model_id.as_str())
                .unwrap_or(model.as_str());
            if let Some(compaction_policy_tx) = &session.compaction_policy_tx {
                if compaction_policy_tx
                    .send(crate::provider_client::compaction_policy_for_model(
                        capacity_model,
                    ))
                    .is_err()
                {
                    tracing::warn!(session_id = %id, "active engine dropped compaction-policy channel");
                }
            }
        }
    }
    if !exists {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(applied_live)
}
