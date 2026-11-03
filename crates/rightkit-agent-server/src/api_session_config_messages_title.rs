use super::*;

#[derive(Deserialize, serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(rename = "type")]
    pub attachment_type: String,
    pub data: String,
    pub mime_type: String,
}

#[derive(Deserialize)]
pub struct PostMessage {
    pub text: String,
    /// Whether this caller can answer sandbox grant questions. Omitted API
    /// messages preserve the current setting; interactive sessions start true.
    #[serde(default, rename = "sandboxGrantPrompts")]
    pub sandbox_grant_prompts: Option<bool>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
    /// Durable, metadata-only references shown beside the user turn after a
    /// restart. Binary image bytes continue to use `attachments`; this field
    /// mirrors the desktop's typed `ReferenceMeta[]` contract.
    #[serde(default)]
    pub references: Vec<rightkit_agent_engine_protocol::ReferenceMeta>,
    /// Active workspace for the session. Sent so that a session auto-created on
    /// its first message (e.g. the "Default session") binds to the real
    /// workspace instead of an empty root — otherwise file edits silently no-op
    /// and approval cards can't render a diff.
    #[serde(default, rename = "workspaceRoot")]
    pub workspace_root: Option<String>,
    #[serde(default, rename = "clientId")]
    pub client_id: Option<String>,
    #[serde(default, rename = "connectionId")]
    pub connection_id: Option<String>,
}

#[derive(Deserialize)]
pub struct AfterQuery {
    #[serde(default)]
    pub after: i64,
    // optional kind filter, e.g. ?kind=assistantDelta
    #[serde(default)]
    pub kind: Option<String>,
    /// Backwards scrollback paging: return the page of events immediately
    /// *before* this seq instead of everything after `after`. A TUI scrolling
    /// up past its retained window sends the lowest seq it still holds.
    ///
    /// Mutually exclusive with `after` in practice; when both are supplied
    /// this wins, because asking for a bounded page backwards is the more
    /// specific request.
    #[serde(default)]
    pub before: Option<i64>,
    /// Maximum rows to return. Absent means unbounded, preserved so existing
    /// clients that page by cursor alone keep working; a scrollback client
    /// always sends one.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct WorkspaceQuery {
    #[serde(default)]
    #[serde(rename = "workspaceRoot", alias = "workspace_root")]
    pub workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionQuery {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrateBody {
    pub goal: String,
    /// Preview the plan (who would do what) without spawning any agents.
    #[serde(default)]
    pub dry_run: bool,
    /// What the human actually typed, when the goal is an expansion of it.
    ///
    /// `/audit` expands to a ~1kB instruction block. Recording that block as
    /// the user's message makes the transcript read like the agent talking to
    /// itself; recording the label keeps it honest and readable. Falls back to
    /// the goal when absent (a plain API caller has no separate label).
    #[serde(default)]
    pub request_label: Option<String>,
    /// Durable idempotency key (CR3 #57). A retry with the same nonce and body
    /// replays the original response instead of starting a second run.
    #[serde(default)]
    pub nonce: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveApproval {
    pub tool_call_id: String,
    pub approved: bool,
    /// Structured Codex-compatible decision; `approved` remains accepted for
    /// older clients and is used when this is absent.
    #[serde(default)]
    pub decision: Option<rightkit_agent_engine_protocol::ReviewDecision>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// Optional client-supplied note. The current approval event stream only
    /// carries the approval decision, but older UI payloads may still include it.
    #[allow(dead_code)]
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerQuestion {
    pub question_id: String,
    /// Answers payload, typically `{header: answer}` (answer = string or array).
    pub answers: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvePlan {
    pub plan_id: String,
    pub approved: bool,
}

#[derive(Deserialize)]
pub struct PostSteer {
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchSession {
    pub title: Option<String>,
    pub tags: Option<Vec<String>>,
    pub model: Option<String>,
    /// Explicit provider route pin for the legacy/dynamic route path.
    /// Mutually exclusive with `inferenceSelection`; see CreateSession.
    pub provider_id: Option<String>,
    /// Reasoning effort (`low`/`medium`/`high`) for the session route.
    pub effort: Option<String>,
    pub inference_selection: Option<crate::inference_runtime::InferenceSelection>,
    pub status: Option<String>,
    pub driver_kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetGoal {
    pub text: String,
    /// Optional token budget for the F2 continuation loop (0 = unbounded by tokens).
    #[serde(default)]
    pub budget_tokens: i64,
    /// Optional USD budget for the F2 continuation loop (0 = unbounded by cost).
    #[serde(default)]
    pub budget_usd: f64,
    /// Durable idempotency key (CR3 #57). A retry with the same nonce and body
    /// replays the original response instead of resetting the goal again.
    #[serde(default)]
    pub nonce: Option<String>,
}

/// Compose an agent's runtime system prompt: its authored charter plus a
/// generated scaffold naming its role, tier, tool ceiling, and boundaries, and
/// requiring a concise handoff at the end so the Conductor and downstream
/// agents get a usable result.
pub(crate) fn compose_agent_persona(
    d: &rightkit_agent_orchestration::coding::registry::AgentDescriptor,
) -> String {
    let tools = if d.allowed_tools.is_empty() {
        "none — read-only reasoning".to_string()
    } else {
        d.allowed_tools.join(", ")
    };
    // Safety-pack gates for agents that can change files: reproduce before
    // fixing, verify the change actually works, keep docs in sync, and never
    // edit tests just to make them pass.
    let can_write = d
        .allowed_tools
        .iter()
        .any(|t| t == "write_file" || t == "apply_patch");
    let safety_pack = if can_write {
        "\n\nEngineering gates you MUST follow:\n\
         - Repro-before-fix: for a bug, first reproduce it (a failing check) before changing code.\n\
         - Verify-live: after your change, actually run the relevant build/tests and confirm they pass.\n\
         - Docs-sync: if behavior changes, update the affected docs/comments in the same pass.\n\
         - Never weaken or rewrite tests just to make them pass; fix the code, not the test."
    } else {
        ""
    };
    format!(
        "{charter}\n\n---\n\nYou are the '{role}' agent (tier {tier}). Tools available to you: \
         {tools}. {boundary} Stay strictly within your role: if the task needs capabilities you \
         do not have, say so and stop rather than overstepping. End your work with a short \
         handoff — what you did, what you produced, and anything the next agent or the Conductor \
         must know.{safety_pack}",
        charter = d.system_prompt,
        role = d.role,
        tier = d.escalation_tier,
        tools = tools,
        boundary = d.tool_description,
    )
}

pub(crate) fn session_row(s: &LiveSession) -> serde_json::Value {
    let title = lock_recover(&s.title, "session.title").clone();
    // Route publication holds the model guard until provider & effort move.
    // Retain the same guard while reading the tuple so API/TUI snapshots
    // cannot combine fields from opposite sides of a committed update.
    let (model, provider_id, effort) = {
        let model = lock_recover(&s.model, "session.model");
        (
            model.clone(),
            lock_recover(&s.provider_id, "session.provider_id").clone(),
            lock_recover(&s.effort, "session.effort").clone(),
        )
    };
    let updated_at = lock_recover(&s.updated_at, "session.updated_at").clone();
    let spec_id = lock_recover(&s.spec_id, "session.spec_id").clone();
    // `None` covers both "no usage yet" and "active model's pricing is
    // unknown" — either way costUsd must be null, never a guessed number or 0.
    let cost_usd = *lock_recover(&s.cost_usd, "session.cost_usd");
    let has_history = lock_recover(&s.transcript, "session.transcript")
        .iter()
        .any(|message| {
            matches!(
                message,
                TranscriptMessage::User { .. } | TranscriptMessage::Compact { .. }
            )
        });
    serde_json::json!({
        "id": s.id,
        "title": title,
        "model": model,
        "inferenceSelection": lock_recover(&s.inference_selection, "session.inference_selection").clone(),
        "workspaceRoot": api_workspace_root(&s.workspace_root),
        "createdAt": s.created_at,
        "updatedAt": updated_at,
        "hasHistory": has_history,
        "turnActive": s.live_turn_handle.is_active(),
        // Surface the agent tree for live sessions too (not just dormant ones),
        // so the conductor → sub-agent relationship is visible while running.
        "role": s.role,
        "parentSessionId": s.parent_session_id,
        "forkedFromSessionId": s.forked_from_session_id,
        "allowedTools": s.allowed_tools,
        "disallowedTools": s.disallowed_tools,
        "toolset": s.toolset,
        "activeSkills": s.active_skills,
        "executionBackend": s.execution_backend,
        "executionWorkspaceId": s.execution_workspace_id,
        "executionNetworkProfile": s.execution_network_profile,
        "driverKind": *lock_recover(&s.driver_kind, "session.driver_kind"),
        "mcpChannel": s.mcp_channel,
        "specId": spec_id,
        "status": "running",
        "workspaceTrust": workspace_trust_label(s.workspace_trust),
        "effort": effort,
        "providerId": provider_id,
        "thinkingBudgetTokens": s.thinking_budget_tokens,
        "disableThinking": s.disable_thinking,
        "safeMode": s.safe_mode,
        // Tags are set via PATCH which persists to storage; live sessions have none by default.
        "tags": [],
        // Cumulative session spend in USD, or null when unpriced/not-yet-known.
        "costUsd": cost_usd,
    })
}

pub(crate) fn session_runtime_options_from_config(config_json: &str) -> SessionRuntimeOptions {
    let cfg: serde_json::Value =
        serde_json::from_str(config_json).unwrap_or(serde_json::Value::Null);
    let string_field = |key: &str| {
        cfg.get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let string_list = |key: &str| {
        cfg.get(key)
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    SessionRuntimeOptions {
        model: string_field("model"),
        provider_id: string_field("providerId"),
        inference_selection: cfg
            .get("inferenceSelection")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
        inference_fingerprint: string_field("inferenceFingerprint"),
        allowed_tools: string_list("allowedTools"),
        disallowed_tools: string_list("disallowedTools"),
        toolset: string_field("toolset"),
        active_skills: string_list("activeSkills"),
        execution_backend: string_field("executionBackend"),
        execution_workspace_id: string_field("executionWorkspaceId"),
        execution_network_profile: string_field("executionNetworkProfile"),
        driver_kind: string_field("driverKind")
            .and_then(|value| normalize_driver_kind(Some(value)).ok())
            .unwrap_or_else(|| "native".to_string()),
        mcp_channel: string_field("mcpChannel").unwrap_or_else(|| "desktop".to_string()),
        spec_id: string_field("specId"),
        effort: string_field("effort"),
        thinking_budget_tokens: cfg
            .get("thinkingBudgetTokens")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok()),
        disable_thinking: cfg
            .get("disableThinking")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        safe_mode: cfg
            .get("safeMode")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        forked_from_session_id: string_field("forkedFromSessionId"),
        budget_ceiling_usd: cfg
            .get("budgetCeilingUsd")
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite() && *value > 0.0),
        ..SessionRuntimeOptions::default()
    }
}

pub(crate) fn touch_session(s: &LiveSession) {
    *lock_recover(&s.updated_at, "session.updated_at") = now_iso();
}

pub(crate) fn should_auto_title(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("new session")
        || trimmed.eq_ignore_ascii_case("default session")
}

pub(crate) fn should_auto_title_for_workspace(title: &str, workspace_root: &str) -> bool {
    if should_auto_title(title) {
        return true;
    }
    let workspace_name = workspace_root
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim();
    !workspace_name.is_empty() && title.trim().eq_ignore_ascii_case(workspace_name)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_auto_title_task(
    session_id: String,
    title: Arc<Mutex<String>>,
    updated_at: Arc<Mutex<String>>,
    conduit: Arc<Conduit>,
    workspace_root: String,
    storage: Option<SharedStorage>,
    engine_cfg: EngineConfig,
    transcript: Vec<TranscriptMessage>,
) {
    tokio::spawn(async move {
        if !should_auto_title_for_workspace(&lock_recover(&title, "session.title"), &workspace_root)
        {
            return;
        }
        let Some(generated) = generate_session_title(engine_cfg, &transcript).await else {
            return;
        };
        if !should_auto_title_for_workspace(&lock_recover(&title, "session.title"), &workspace_root)
        {
            return;
        }

        let now = now_iso();
        let final_title = if let Some(store) = storage {
            let id = session_id.clone();
            let base = generated.clone();
            let updated = now.clone();
            let session_id_for_log = session_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                let s = store.blocking_lock();
                let taken: std::collections::HashSet<String> = s
                    .list_sessions()
                    .map(|rows| {
                        rows.into_iter()
                            .filter(|row| row.id != id)
                            .map(|row| row.title.trim().to_lowercase())
                            .collect()
                    })
                    .unwrap_or_default();
                let unique = uniquify_title(&base, &taken);
                s.rename_session(&id, &unique, &updated).map(|()| unique)
            })
            .await;
            match result {
                Ok(Ok(unique)) => unique,
                Ok(Err(err)) => {
                    tracing::warn!(session_id = %session_id_for_log, error = %err, "auto-title persist failed");
                    generated
                }
                Err(err) => {
                    tracing::warn!(session_id = %session_id_for_log, error = %err, "auto-title task join failed");
                    generated
                }
            }
        } else {
            generated
        };
        *lock_recover(&title, "session.title") = final_title.clone();
        *lock_recover(&updated_at, "session.updated_at") = now.clone();
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::SessionTitleUpdated {
            session_id,
            seq,
            created_at: now,
            title: final_title,
        });
    });
}

/// Make a generated session title unique against the other sessions' titles
/// (case-insensitive). Collisions get a ` · N` ordinal; after nine takes the
/// ordinal keeps climbing until a free name is found, so the loop always
/// terminates with a distinct title.
pub(crate) fn uniquify_title(
    base: &str,
    taken_lowercase: &std::collections::HashSet<String>,
) -> String {
    let trimmed = base.trim();
    if !taken_lowercase.contains(&trimmed.to_lowercase()) {
        return trimmed.to_string();
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{trimmed} · {n}");
        if !taken_lowercase.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        n += 1;
    }
}

async fn generate_session_title(
    engine_cfg: EngineConfig,
    transcript: &[TranscriptMessage],
) -> Option<String> {
    let prompt = session_title_prompt(transcript)?;
    let messages = vec![
        serde_json::json!({
            "role": "system",
            "content": "Create a short title for this coding session. Return only the title, no quotes, no punctuation at the end, 3 to 8 words."
        }),
        serde_json::json!({
            "role": "user",
            "content": prompt,
        }),
    ];
    let turn = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        crate::provider_client::complete_projection(&engine_cfg, messages),
    )
    .await
    .ok()?
    .ok()?;
    sanitize_generated_session_title(&turn.text)
}

pub(crate) fn session_title_prompt(transcript: &[TranscriptMessage]) -> Option<String> {
    let first_user = transcript.iter().find_map(|message| match message {
        TranscriptMessage::User { content, .. } => Some(scrub_system_reminders(content)),
        _ => None,
    })?;
    let first_assistant = transcript.iter().find_map(|message| match message {
        TranscriptMessage::Assistant { content, .. } if !content.trim().is_empty() => {
            Some(content.trim().to_string())
        }
        _ => None,
    })?;

    Some(format!(
        "User request:\n{}\n\nAssistant response:\n{}",
        first_user.trim().chars().take(1200).collect::<String>(),
        first_assistant
            .trim()
            .chars()
            .take(1200)
            .collect::<String>()
    ))
}

pub(crate) fn sanitize_generated_session_title(raw: &str) -> Option<String> {
    let first_line = raw.lines().find(|line| !line.trim().is_empty())?.trim();
    let unquoted = first_line
        .trim_matches(['"', '\'', '`', '*', '#', '.', ':', '-'])
        .trim();
    let without_prefix = unquoted
        .strip_prefix("Title:")
        .or_else(|| unquoted.strip_prefix("title:"))
        .unwrap_or(unquoted)
        .trim();
    let cleaned = without_prefix
        .trim_matches(['"', '\'', '`', '*', '#', '.', ':', '-'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = cleaned
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_string();

    if should_auto_title(&cleaned) || cleaned.len() < 3 {
        None
    } else {
        Some(cleaned)
    }
}

fn scrub_system_reminders(content: &str) -> String {
    let mut remaining = content;
    let mut out = String::new();
    loop {
        let Some(start) = remaining.find("<system-reminder>") else {
            out.push_str(remaining);
            break;
        };
        out.push_str(&remaining[..start]);
        let after_start = &remaining[start + "<system-reminder>".len()..];
        let Some(end) = after_start.find("</system-reminder>") else {
            break;
        };
        remaining = &after_start[end + "</system-reminder>".len()..];
    }
    out
}

// ── Router ────────────────────────────────────────────────────────────────────
