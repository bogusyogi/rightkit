//! Driver process lifecycle, event ingestion, and status projection.
//!
//! Decomposed out of `api_drivers.rs` (ra-016): this module owns the
//! start/prompt/interrupt/resume/stop/status handlers for driver-hosted CLI
//! sessions, driver discovery/model listing, the normalized driver event
//! consumer, and lifecycle/status-projection helpers (including Codex
//! harness verification support). `api_drivers.rs` re-exports everything
//! here (`pub(crate) use driver_lifecycle::*;`) and keeps calling these
//! functions unqualified exactly as before the split, so route paths, wire
//! formats, and authorization boundaries are unchanged — only the item's
//! source file moved.

use super::*;

pub(crate) async fn start_driver(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<StartDriverRequest>,
) -> Result<Json<Value>, StatusCode> {
    let _authority = state.driver_authority_lock.lock().await;
    let (lease, _) = authorize_lease(&state, &session_id, &headers).await?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let mut binding = storage
        .lock()
        .await
        .get_driver_binding(&body.binding_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if binding.session_id != session_id || binding.id != lease.binding_id {
        return Err(StatusCode::FORBIDDEN);
    }
    let recovery_at = unix_key(0);
    let claimed = storage
        .lock()
        .await
        .list_claimed_driver_events(&binding.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !claimed.is_empty() {
        if !body.recover_claimed {
            binding.status = "recovery_required".into();
            binding.updated_at = recovery_at.clone();
            storage
                .lock()
                .await
                .upsert_driver_binding(&binding)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            return Err(StatusCode::CONFLICT);
        }
        storage
            .lock()
            .await
            .abandon_claimed_driver_events(&binding.id, &recovery_at)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let executable = binding
        .executable_path
        .clone()
        .ok_or(StatusCode::CONFLICT)?;
    if body
        .executable
        .as_deref()
        .is_some_and(|value| value != executable)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let live_digest =
        rightkit_agents::driver_host::cli_runtime::recovery::compute_executable_digest(
            std::path::Path::new(&executable),
        )
        .map_err(|_| StatusCode::CONFLICT)?;
    if binding.executable_sha256.as_deref() != Some(live_digest.as_str()) {
        return Err(StatusCode::CONFLICT);
    }
    let provider = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(
        binding
            .provider_id
            .clone()
            .unwrap_or_else(|| binding.driver_kind.clone()),
    );
    if !state.cli_registry.is_ready(&provider, &live_digest) {
        return Err(StatusCode::CONFLICT);
    }
    if runtime_contains_binding(&state, &binding.id).await {
        return Err(StatusCode::CONFLICT);
    }
    let current_exe = std::env::current_exe().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mcp_args = vec![
        "mcp-server".to_string(),
        "--session-id".to_string(),
        session_id.clone(),
    ];
    let mcp_config = json!({
        "mcpServers": {
            (rightkit_agent_config::host_slug()): {
                "type": "stdio",
                "command": current_exe,
                "args": mcp_args,
            }
        }
    })
    .to_string();
    let mut claude_account_identity: Option<rightkit_agents::driver_host::ClaudeAccountIdentity> =
        None;
    let mut spec = match binding.driver_kind.as_str() {
        "claude" => {
            let options = ClaudeLaunchOptions {
                executable: executable.clone().into(),
                cwd: PathBuf::from(&live.workspace_root),
                mcp_config,
                session_id: None,
                resume_session_id: binding.vendor_session_id.clone(),
                model: body.model.clone(),
            };
            // PL-005: enforce Claude-root delegation at the live daemon
            // launch seam, before the process can be admitted. This keeps
            // the real Claude CLI bound to host MCP/policy controls.
            rightkit_agents::driver_host::claude_harness::enforce_delegation_launch_policy_for_legacy(
                &options,
            )
            .map_err(|_| StatusCode::CONFLICT)?;
            let mut spec = options.launch_spec().map_err(|_| StatusCode::BAD_REQUEST)?;
            // #336: persistent host answers can_use_tool over stdio through host approvals.
            rightkit_agents::driver_host::enable_stdio_permission_prompt_for_spec(&mut spec);
            // Explicit account selection only (session, else project default). A selected
            // account that cannot be honoured fails the launch rather than falling back.
            match crate::main_runtime::resolve_claude_account(&session_id, &live.workspace_root) {
                Ok(Some(resolved)) => {
                    rightkit_agents::driver_host::apply_claude_account_env(
                        &mut spec,
                        Some(&resolved.launch),
                    );
                    // #344: credential precedence guard (also strips ANTHROPIC_AUTH_TOKEN).
                    crate::main_runtime::apply_credential_precedence_guard(
                        &mut spec,
                        "claude",
                        Some(&resolved.launch),
                        &|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()),
                    );
                    claude_account_identity = Some(resolved.identity);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(session_id, error = %error, "selected Claude account unavailable; launch refused");
                    return Err(StatusCode::CONFLICT);
                }
            }
            spec
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    spec.env.insert(
        rightkit_agent_config::env_name("DAEMON_URL"),
        format!("http://127.0.0.1:{}", state.bound_port),
    );
    let capability_token = headers
        .get(CAPABILITY_HEADER.as_str())
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    spec.env.insert(
        rightkit_agent_config::env_name("GATEWAY_TOKEN"),
        capability_token.to_string(),
    );
    let process_instance_secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let process_instance_hash = token_hash(&process_instance_secret);
    if !storage
        .lock()
        .await
        .set_session_capability_lease_process_instance(&lease.id, Some(&process_instance_hash))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    spec.env.insert(
        rightkit_agent_config::env_name("DRIVER_INSTANCE_SECRET"),
        process_instance_secret,
    );

    let driver_kind = DriverKind::Claude;
    // Return the launch policy as evidence from the actual process spec, not
    // merely from the requested driver kind. QA can therefore prove that the
    // driver was given exactly one host MCP server and no native shell
    // route before it sends a conductor prompt.
    let disabled_features = spec
        .args
        .windows(2)
        .filter(|pair| pair[0] == "--disable")
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    // Claude Code's own tools are switched off with `--tools ""`; the host's MCP server is then
    // the only route to any effect.
    let native_mutation_routes_disabled = spec
        .args
        .windows(2)
        .any(|pair| pair[0] == "--tools" && pair[1].is_empty());
    let started = match state
        .driver_host
        .start_with_spec_for_account(
            binding.id.clone(),
            driver_kind,
            spec,
            DriverCursor {
                input_offset: 0,
                output_offset: binding
                    .last_event_cursor
                    .as_deref()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                last_event_key: binding.last_event_key.clone(),
            },
            claude_account_identity,
        )
        .await
    {
        Ok(started) => started,
        Err(error) => {
            let _ = storage
                .lock()
                .await
                .set_session_capability_lease_process_instance(&lease.id, None);
            tracing::warn!(session_id, error = %error, "driver start failed");
            return Err(StatusCode::BAD_GATEWAY);
        }
    };
    binding.binary_version = started.info.binary_version.clone();
    if driver_kind == DriverKind::Claude {
        binding.schema_hash = protocol_schema_hash("claude", &binding.binary_version);
    }
    binding.status = "starting".into();
    binding.updated_at = unix_key(0);
    if storage
        .lock()
        .await
        .upsert_driver_binding(&binding)
        .is_err()
    {
        if let Err(error) = state.driver_host.shutdown(&binding.id).await {
            tracing::warn!(binding_id = %binding.id, %error, "driver stop failed after a rejected binding upsert; the driver process may still be running");
        }
        let _ = storage
            .lock()
            .await
            .set_session_capability_lease_process_instance(&lease.id, None);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let info = started.info.clone();
    spawn_driver_event_consumer(state.clone(), binding, started.events);
    Ok(Json(json!({
        "started": true,
        "bindingId": info.binding_id,
        "driverKind": binding_driver_label(info.driver),
        "pid": info.pid,
        "binaryVersion": info.binary_version,
        "launchEvidence": {
            "mcpServerCount": 1,
            "nativeMutationRoutesDisabled": native_mutation_routes_disabled,
            "disabledFeatures": disabled_features,
        },
    })))
}

pub(crate) async fn prompt_driver(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DriverPromptRequest>,
) -> Result<Json<Value>, StatusCode> {
    let (lease, _) = authorize_lease(&state, &session_id, &headers).await?;
    if lease.binding_id != body.binding_id {
        return Err(StatusCode::FORBIDDEN);
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let binding = storage
        .lock()
        .await
        .get_driver_binding(&body.binding_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    // Claude emits system/init only after its first stream-json user message;
    // that first governed prompt is its observed readiness handshake.
    // An interrupted or failed turn leaves the vendor process alive; only a
    // dead process makes those statuses final.
    let turn_ended_alive = matches!(binding.status.as_str(), "interrupted" | "failed")
        && state.driver_host.contains(&body.binding_id).await;
    if binding.status != "ready"
        && binding.status != "running"
        && binding.status != "completed"
        && !turn_ended_alive
        && !(binding.driver_kind == "claude" && binding.status == "starting")
    {
        return Err(StatusCode::CONFLICT);
    }
    let request_id = state
        .driver_host
        .send_prompt(&body.binding_id, &body.vendor_session_id, &body.text)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if let Some(live) = state.get_session(&session_id) {
        let created_at = now_iso();
        let visible_text = body
            .request_label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(&body.text)
            .to_string();
        lock_recover(&live.transcript, "session.transcript").push(
            rightkit_agent_engine_protocol::TranscriptMessage::User {
                id: format!("driver-user-{}", now_millis()),
                created_at: created_at.clone(),
                content: visible_text.clone(),
                trust: None,
                pinned: false,
                attachments: Vec::new(),
            },
        );
        let seq = live.conduit.next_seq();
        live.conduit.publish(
            rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
                session_id: session_id.clone(),
                seq,
                created_at,
                text: visible_text,
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            },
        );
    }
    Ok(Json(json!({"accepted": true, "requestId": request_id})))
}

pub(crate) async fn interrupt_driver(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DriverInterruptRequest>,
) -> Result<Json<Value>, StatusCode> {
    let _authority = state.driver_authority_lock.lock().await;
    authorize_binding_request(&state, &session_id, &body.binding_id, &headers).await?;
    // Break host-side approval/tool waits as well as the vendor turn.
    if let Some(live) = state.get_session(&session_id) {
        if let Some(interrupt) = &live.interrupt_tx {
            let _ = interrupt.send(true);
        }
        lock_recover(&live.pending_approvals, "session.pending_approvals").clear();
        for sender in lock_recover(&live.cancel_tokens, "session.cancel_tokens").values() {
            let _ = sender.send(true);
        }
    }
    let request_id = state
        .driver_host
        .interrupt(
            &body.binding_id,
            &body.vendor_session_id,
            body.vendor_turn_id.as_deref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(session_id, %error, "driver interrupt failed");
            StatusCode::BAD_GATEWAY
        })?;
    Ok(Json(json!({"interrupted": true, "requestId": request_id})))
}

pub(crate) async fn resume_driver(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DriverBindingRequest>,
) -> Result<Json<Value>, StatusCode> {
    authorize_binding_request(&state, &session_id, &body.binding_id, &headers).await?;
    let vendor_session_id = body.vendor_session_id.ok_or(StatusCode::BAD_REQUEST)?;
    // A live process is attached to exactly one vendor session: resuming another is a conflict.
    if let Some(storage) = state.storage.as_ref() {
        let binding = storage
            .lock()
            .await
            .get_driver_binding(&body.binding_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(stored) = binding.and_then(|binding| binding.vendor_session_id) {
            if !stored.is_empty() && stored != vendor_session_id {
                return Err(StatusCode::CONFLICT);
            }
        }
    }
    let request_id = state
        .driver_host
        .resume(&body.binding_id, &vendor_session_id)
        .await
        .map_err(|error| {
            tracing::warn!(session_id, %error, "driver resume failed");
            StatusCode::BAD_GATEWAY
        })?;
    Ok(Json(json!({"resumed": true, "requestId": request_id})))
}

pub(crate) async fn stop_driver(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DriverBindingRequest>,
) -> Result<Json<Value>, StatusCode> {
    let _authority = state.driver_authority_lock.lock().await;
    let (lease, _) = authorize_lease(&state, &session_id, &headers).await?;
    if lease.binding_id != body.binding_id {
        return Err(StatusCode::FORBIDDEN);
    }
    state
        .driver_host
        .shutdown(&body.binding_id)
        .await
        .map_err(|error| {
            tracing::warn!(session_id, %error, "driver shutdown failed");
            StatusCode::BAD_GATEWAY
        })?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    storage
        .lock()
        .await
        .update_driver_binding_progress(
            &body.binding_id,
            None,
            None,
            None,
            "stopped",
            None,
            None,
            &unix_key(0),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    storage
        .lock()
        .await
        .revoke_session_capability_lease(&lease.id, &unix_key(0))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"stopped": true})))
}

/// Read the `driverKind` an operator selected for a session from its
/// persisted `config_json`, defaulting to `"native"` exactly like
/// `session_runtime_options_from_config` does for a resumed session. Kept
/// local (rather than reusing that private helper from another module) since
/// this handler only needs the one field for a read-only status response.
pub(crate) fn driver_kind_from_config_json(config_json: &str) -> String {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|cfg| {
            cfg.get("driverKind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|kind| kind.trim().to_ascii_lowercase())
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| "native".to_string())
}

/// Per-binding rate-limit availability (#346). Held in process memory: it is telemetry about the
/// live driver, rebuilt from the next `rate_limit_event`, so it is not persisted.
static DRIVER_RATE_LIMITS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<String, rightkit_agents::driver_host::RateLimitState>,
    >,
> = std::sync::OnceLock::new();

fn driver_rate_limits() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<String, rightkit_agents::driver_host::RateLimitState>,
> {
    DRIVER_RATE_LIMITS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a driver kind ever reports usage windows. Antigravity does not, so probing it is
/// pointless and its state is `Unsupported` rather than `NoData`.
fn driver_reports_rate_limits(driver_kind: &str) -> bool {
    driver_kind == "claude"
}

/// Fold a normalized event into the binding's rate-limit state; returns whether it was one.
pub(crate) fn observe_driver_rate_limit(binding_id: &str, kind: &NormalizedEventKind) -> bool {
    driver_rate_limits()
        .entry(binding_id.to_string())
        .or_default()
        .observe(kind)
}

/// A failed attempt to learn the binding's usage (driver died or spoke out of protocol). Keeps
/// the last snapshot so the UI can show it as stale; the next rate-limit event recovers to
/// `Known`.
pub(crate) fn record_driver_rate_limit_probe_failure(binding_id: &str, message: &str) {
    driver_rate_limits()
        .entry(binding_id.to_string())
        .or_default()
        .probe_failed(message);
}

/// Current state for the status surface, with `Unsupported` for kinds that never report usage.
pub(crate) fn driver_rate_limit_state(
    binding_id: &str,
    driver_kind: &str,
) -> rightkit_agents::driver_host::RateLimitState {
    let mut state = driver_rate_limits()
        .get(binding_id)
        .cloned()
        .unwrap_or_default();
    if !driver_reports_rate_limits(driver_kind) {
        state.mark_unsupported(format!("{driver_kind} does not report rate limits"));
    }
    state
}

pub(crate) async fn driver_status(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bindings = storage
        .lock()
        .await
        .list_driver_bindings_for_session(&session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let now = unix_key(0);
    let mut rows = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let auth_required = driver_auth_required(&binding.driver_kind).await;
        // Schema first: if the driver cannot be spoken to at all, saying the
        // lease expired would send the user to re-authorize something that
        // still would not work.
        let failure_reason = match binding_schema_drifted(
            &binding.driver_kind,
            &binding.binary_version,
            &binding.schema_hash,
        ) {
            Some(true) => Some("unsupported_schema"),
            _ => storage
                .lock()
                .await
                .get_latest_session_capability_lease_for_binding(&binding.id)
                .ok()
                .flatten()
                .filter(|lease| lease.expires_at.as_str() <= now.as_str())
                .map(|_| "lease_expired"),
        };
        rows.push(json!({
            "bindingId": binding.id,
            "driverKind": binding.driver_kind,
            "lifecycle": driver_lifecycle(&binding.status),
            "authRequired": auth_required,
            "diagnostic": driver_diagnostic(&binding.status),
            // Lets the desktop strip name an unresumable session instead of
            // showing a generic failure. Null when nothing is known to be wrong,
            // and null (not false) when this kind's schema cannot be rechecked
            // for free — see binding_schema_drifted.
            "failureReason": match failure_reason {
                Some(reason) => Value::from(reason),
                None => Value::Null,
            },
            "capabilities": driver_capabilities_json(),
            "version": {
                "executable": binding.driver_kind,
                "installed": true,
                "binaryVersion": binding.binary_version,
                "protocolVersion": driver_protocol_version(&binding.driver_kind),
                "schemaHash": binding.schema_hash,
            },
            "vendorSessionId": binding.vendor_session_id,
            "vendorTurnId": binding.vendor_turn_id,
            "running": runtime_contains_binding(&state, &binding.id).await,
            "gatewayState": if binding.status == "failed" { json!({"state":"failed", "message":"Driver gateway failed closed"}) } else if matches!(binding.status.as_str(), "ready" | "running" | "completed") { json!({"state":"ready"}) } else { json!({"state":"starting"}) },
            "processGeneration": binding.last_event_cursor,
            "rateLimit": serde_json::to_value(driver_rate_limit_state(&binding.id, &binding.driver_kind))
                .unwrap_or(Value::Null),
        }));
    }

    // A session already resumed into memory is the common, fully-live case.
    if let Some(live) = state.get_session(&session_id) {
        let live_driver_kind = lock_recover(&live.driver_kind, "session.driver_kind").clone();
        let selected_auth_required = if live_driver_kind == "native" {
            false
        } else {
            driver_auth_required(&live_driver_kind).await
        };
        let lifecycle = if live_driver_kind == "native" {
            "native"
        } else if selected_auth_required {
            "authentication_required"
        } else {
            "ready"
        };
        // #347: the active auth method (and any env override warning) for this session.
        let auth = match live_driver_kind.as_str() {
            "claude" => Some(crate::main_runtime::session_auth_report(
                "claude",
                &session_id,
                &live.workspace_root,
            )),
            _ => None,
        };
        return Ok(Json(json!({
            "sessionId": session_id,
            "driverKind": live_driver_kind,
            "authRequired": selected_auth_required,
            "lifecycle": lifecycle,
            "auth": auth.map(|report| report.to_json()),
            "drivers": rows,
        })));
    }

    // Not live, but a session restored from Recents still exists in storage —
    // it just hasn't been resumed into memory yet. That is a truthful, calm
    // state, not a 404: report it without side effects (no resume here; this
    // is a polling read, and resuming belongs to an explicit action).
    let dormant = storage
        .lock()
        .await
        .get_session(&session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(json!({
        "sessionId": session_id,
        "driverKind": driver_kind_from_config_json(&dormant.config_json),
        "authRequired": false,
        "lifecycle": "not_resumed",
        "drivers": rows,
    })))
}

pub(crate) async fn discover_driver(Path(kind): Path<String>) -> Result<Json<Value>, StatusCode> {
    let driver_kind = kind.trim().to_ascii_lowercase();
    if driver_kind != "claude" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let executable = driver_kind.clone();
    let version = capture_driver_version(&executable).await;
    let installed = version.is_ok();
    let schema_hash = version
        .as_ref()
        .ok()
        .map(|version| protocol_schema_hash(&driver_kind, version));
    let auth_required = driver_auth_required(&executable).await;
    let diagnostic = version.as_ref().err().cloned();
    Ok(Json(json!({
        "driverKind": driver_kind,
        "installed": installed,
        "authRequired": auth_required,
        "lifecycle": if !installed { "unavailable" } else if auth_required { "authentication_required" } else { "ready" },
        "diagnostic": diagnostic,
        "capabilities": driver_capabilities_json(),
        "version": {
            "executable": executable,
            "installed": installed,
            "binaryVersion": version.ok(),
            "protocolVersion": driver_protocol_version(&driver_kind),
            "schemaHash": schema_hash,
        },
    })))
}

/// `GET /drivers/{kind}/models` — the model catalogue offered when picking a model for a
/// driver-hosted session. Claude Code
/// has no introspection command for this, so its list is a static marker for the frontend to fill in
/// from documentation rather than a fabricated catalogue.
pub(crate) async fn list_driver_models(
    Path(kind): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let driver_kind = kind.trim().to_ascii_lowercase();
    match driver_kind.as_str() {
        "claude" => Ok(Json(json!({ "models": [], "source": "static" }))),
        _ => Err(driver_models_error(
            StatusCode::BAD_REQUEST,
            format!("unknown driver kind: {driver_kind}"),
        )),
    }
}

pub(crate) fn driver_models_error(
    status: StatusCode,
    message: String,
) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

pub(crate) async fn authorize_binding_request(
    state: &DaemonState,
    session_id: &str,
    binding_id: &str,
    headers: &HeaderMap,
) -> Result<(), StatusCode> {
    let (lease, _) = authorize_lease(state, session_id, headers).await?;
    if lease.binding_id != binding_id {
        return Err(StatusCode::FORBIDDEN);
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let binding = storage
        .lock()
        .await
        .get_driver_binding(binding_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    (binding.session_id == session_id)
        .then_some(())
        .ok_or(StatusCode::FORBIDDEN)
}

pub(crate) fn spawn_driver_event_consumer(
    state: DaemonState,
    binding: rightkit_agent_engine_storage::DriverBindingRow,
    mut events: crate::driver_host_runtime::DriverEventReceiver,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                crate::driver_host_runtime::DriverRuntimeEvent::Normalized(event) => {
                    if let Err(error) = apply_normalized_driver_event(
                        &state,
                        &binding.id,
                        &binding.session_id,
                        event,
                    )
                    .await
                    {
                        tracing::warn!(binding_id = %binding.id, error = %error, "driver event rejected");
                        if let Err(error) = state.driver_host.shutdown(&binding.id).await {
                            tracing::warn!(binding_id = %binding.id, %error, "driver stop failed after a rejected event; the driver process may still be running");
                        }
                        break;
                    }
                }
                crate::driver_host_runtime::DriverRuntimeEvent::ServerRequest {
                    id,
                    method,
                    params,
                } => {
                    if method == "can_use_tool" {
                        // #336: answer through the host's own approval flow. The wait can
                        // last minutes, so it runs on its own task and never blocks this loop.
                        let state = state.clone();
                        let binding_id = binding.id.clone();
                        let session_id = binding.session_id.clone();
                        tokio::spawn(async move {
                            answer_can_use_tool_request(
                                &state,
                                &binding_id,
                                &session_id,
                                id,
                                &params,
                            )
                            .await;
                        });
                        continue;
                    }
                    tracing::warn!(binding_id = %binding.id, method, params = %params, "driver server request denied by native-effect policy");
                    // Native effects are disabled. A vendor request here is a
                    // policy violation, so decline it explicitly and keep the
                    // host gateway as the only effect path.
                    let _ = state.driver_host.respond_server_request(
                        &binding.id,
                        id,
                        json!({"decision": "decline", "reason": format!("{method} must use the {} capability gateway", crate::host_identity::app())}),
                    ).await;
                }
                crate::driver_host_runtime::DriverRuntimeEvent::ProtocolError { message } => {
                    record_driver_rate_limit_probe_failure(&binding.id, &message);
                    mark_driver_failed(&state, &binding.id).await;
                    publish_driver_failure(&state, &binding.session_id, message);
                    break;
                }
                crate::driver_host_runtime::DriverRuntimeEvent::Exited { success, code }
                    if !success =>
                {
                    record_driver_rate_limit_probe_failure(
                        &binding.id,
                        &format!("driver exited with code {code:?}"),
                    );
                    mark_driver_failed(&state, &binding.id).await;
                    publish_driver_failure(
                        &state,
                        &binding.session_id,
                        format!("driver exited with code {code:?}"),
                    );
                    break;
                }
                _ => {}
            }
        }
    });
}

/// Route one Claude `can_use_tool` request through the approval flow and write the answer back.
pub(super) async fn answer_can_use_tool_request(
    state: &DaemonState,
    binding_id: &str,
    session_id: &str,
    id: Value,
    params: &Value,
) {
    let answer =
        crate::api_sessions::approvals::answer_claude_can_use_tool(state, session_id, params).await;
    if let Err(error) = state
        .driver_host
        .respond_server_request(binding_id, id, answer)
        .await
    {
        tracing::warn!(binding_id = %binding_id, error = %error, "can_use_tool answer could not be written");
    }
}

/// Put one driver event on the chat's own event stream: text as it arrives, the finished
/// message, the end of the turn, a failure, and account telemetry. No storage is touched, so
/// both launch paths can call it: the driver routes through `apply_normalized_driver_event`,
/// and the agent-runtime routes through the bridge's event tap (`ensure_runtime_session_events`).
pub(crate) fn publish_driver_event_to_session(
    state: &DaemonState,
    binding_id: &str,
    session_id: &str,
    live: &crate::api::LiveSession,
    kind: NormalizedEventKind,
    event_key: &str,
    now: String,
) {
    match kind {
        NormalizedEventKind::AssistantDelta { text } => {
            live.conduit.emit(|seq| {
                rightkit_agent_engine_protocol::ProtocolEvent::AssistantDelta {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now,
                    text,
                    is_reasoning: false,
                }
            });
        }
        NormalizedEventKind::AssistantCompleted { text } => {
            if text.trim().is_empty() {
                return;
            }
            let seq = live.conduit.next_seq();
            let message = rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                id: format!("driver-{seq}"),
                created_at: now.clone(),
                content: text,
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
                origin: None,
            };
            live.conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::AssistantMessageCompleted {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now.clone(),
                    message,
                    phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
                },
            );
        }
        NormalizedEventKind::TurnCompleted | NormalizedEventKind::TurnCompletedWithUsage { .. } => {
            // Issue 450 (W9T-03): a vendor driver's turn ends on `sessionTurnFinished`,
            // which clients wait for; the driver's own completion carries no engine publish.
            crate::api_sessions::messages::publish_turn_finished(
                &live.conduit,
                session_id,
                &format!("driver-turn-{event_key}"),
                "completed",
            );
        }
        NormalizedEventKind::TurnInterrupted => {
            let seq = live.conduit.next_seq();
            live.conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::RunInterrupted {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now,
                    reason: Some("vendor driver interrupted".into()),
                },
            );
        }
        NormalizedEventKind::TurnFailed { message } => {
            publish_driver_failure(state, session_id, message)
        }
        NormalizedEventKind::ToolResult {
            call_id,
            is_error,
            content,
        } => {
            tracing::warn!(binding_id, call_id, is_error, content = %content, "driver MCP tool call completed");
        }
        // Account telemetry becomes a durable session stream event so the UI can show
        // "account X capped until T" and offer a manual continue-on-another-account.
        kind @ (NormalizedEventKind::RateLimit { .. }
        | NormalizedEventKind::TurnCostReported { .. }) => {
            observe_driver_rate_limit(binding_id, &kind);
            let seq = live.conduit.next_seq();
            if let Some(protocol_event) = account_protocol_event(session_id, seq, &now, &kind) {
                live.conduit.publish(protocol_event);
            }
        }
        // Gateway execution emits the canonical host tool lifecycle.
        // Vendor tool echoes are retained in the driver inbox only.
        _ => {}
    }
}

/// Send the agent-runtime path's driver events to the chats they belong to. That path's events
/// were only read by the executor to work out the final answer, so nothing reached the chat's
/// event stream: a desktop chat on Claude Code showed the question and no reply. Installed once.
pub(crate) fn ensure_runtime_session_events(state: &DaemonState) {
    let Some(bridge) = state.cli_bridge.as_ref() else {
        return;
    };
    bridge.session_events().get_or_init(|| {
        let (sender, mut receiver) =
            tokio::sync::mpsc::unbounded_channel::<(String, NormalizedEvent)>();
        let state = state.clone();
        tokio::spawn(async move {
            let mut sessions: HashMap<String, String> = HashMap::new();
            while let Some((binding_id, event)) = receiver.recv().await {
                let session_id = match sessions.get(&binding_id) {
                    Some(session_id) => session_id.clone(),
                    None => {
                        let Some(storage) = state.storage.as_ref() else {
                            continue;
                        };
                        let Ok(Some(binding)) =
                            storage.lock().await.get_driver_binding(&binding_id)
                        else {
                            continue;
                        };
                        sessions.insert(binding_id.clone(), binding.session_id.clone());
                        binding.session_id
                    }
                };
                let Some(live) = state.get_session(&session_id) else {
                    continue;
                };
                publish_driver_event_to_session(
                    &state,
                    &binding_id,
                    &session_id,
                    &live,
                    event.kind,
                    &event.event_key,
                    crate::api::now_iso(),
                );
            }
        });
        sender
    });
}

pub(crate) async fn mark_driver_failed(state: &DaemonState, binding_id: &str) {
    mark_driver_lifecycle(state, binding_id, "failed").await;
}

pub(crate) async fn mark_driver_lifecycle(state: &DaemonState, binding_id: &str, lifecycle: &str) {
    let Some(storage) = state.storage.as_ref() else {
        return;
    };
    let Ok(Some(binding)) = storage.lock().await.get_driver_binding(binding_id) else {
        return;
    };
    let _ = storage.lock().await.update_driver_binding_progress(
        binding_id,
        binding.vendor_session_id.as_deref(),
        binding.vendor_thread_id.as_deref(),
        binding.vendor_turn_id.as_deref(),
        lifecycle,
        binding.last_event_key.as_deref(),
        binding.last_event_cursor.as_deref(),
        &unix_key(0),
    );
}

pub(crate) async fn apply_normalized_driver_event(
    state: &DaemonState,
    binding_id: &str,
    session_id: &str,
    event: NormalizedEvent,
) -> Result<(), String> {
    let storage = state.storage.as_ref().ok_or("storage unavailable")?;
    let inbox = rightkit_agent_engine_storage::DriverEventInboxRow {
        binding_id: binding_id.to_string(),
        event_key: event.event_key.clone(),
        event_cursor: Some(event.source_offset.to_string()),
        event_kind: event.vendor_method.clone(),
        payload_json: serde_json::to_string(&event).map_err(|error| error.to_string())?,
        status: "claimed".into(),
        received_at: unix_key(0),
        processed_at: None,
        result_json: None,
    };
    {
        let mut store = storage.lock().await;
        if matches!(
            store
                .claim_driver_event(&inbox)
                .map_err(|error| error.to_string())?,
            rightkit_agent_engine_storage::DriverEventClaim::Duplicate(_)
        ) {
            return Ok(());
        }
    }
    let live = state
        .get_session(session_id)
        .ok_or("session no longer live")?;
    let now = crate::api::now_iso();
    let existing = storage
        .lock()
        .await
        .get_driver_binding(binding_id)
        .map_err(|error| error.to_string())?
        .ok_or("driver binding missing")?;
    let (vendor_session, vendor_turn) = match &event.kind {
        NormalizedEventKind::SessionStarted {
            vendor_session_id, ..
        } => (Some(vendor_session_id.as_str()), None),
        NormalizedEventKind::TurnStarted { vendor_turn_id } => {
            (None, Some(vendor_turn_id.as_str()))
        }
        _ => (None, None),
    };
    let status = driver_status_after_event(event.driver, &existing.status, &event.kind);
    {
        storage
            .lock()
            .await
            .update_driver_binding_progress(
                binding_id,
                vendor_session.or(existing.vendor_session_id.as_deref()),
                vendor_session.or(existing.vendor_thread_id.as_deref()),
                vendor_turn.or(existing.vendor_turn_id.as_deref()),
                status,
                Some(&event.event_key),
                Some(&event.source_offset.to_string()),
                &unix_key(0),
            )
            .map_err(|error| error.to_string())?;
    }
    publish_driver_event_to_session(
        state,
        binding_id,
        session_id,
        &live,
        event.kind,
        &event.event_key,
        now,
    );
    storage
        .lock()
        .await
        .mark_driver_event_processed(binding_id, &event.event_key, "processed", &unix_key(0))
        .map_err(|error| error.to_string())?;
    Ok(())
}

struct AccountStamp {
    provider: String,
    display: String,
    account_id: Option<String>,
    kind: Option<String>,
    email: Option<String>,
    organization: Option<String>,
}

/// Project account-scoped vendor telemetry onto the session stream. `None` for every other kind.
pub(crate) fn account_protocol_event(
    session_id: &str,
    seq: i64,
    created_at: &str,
    kind: &NormalizedEventKind,
) -> Option<rightkit_agent_engine_protocol::ProtocolEvent> {
    let label =
        |account: &Option<rightkit_agents::driver_host::ClaudeAccountIdentity>| match account {
            Some(who) => AccountStamp {
                provider: who.provider_id.clone(),
                display: who.display(),
                account_id: who.account_id.clone(),
                kind: who.kind.clone(),
                email: who.email.clone(),
                organization: who.organization.clone(),
            },
            None => AccountStamp {
                provider: "claude".to_string(),
                display: "unknown".to_string(),
                account_id: None,
                kind: None,
                email: None,
                organization: None,
            },
        };
    match kind {
        NormalizedEventKind::RateLimit {
            account,
            status,
            rate_limit_type,
            resets_at,
            utilization,
            overage_status,
            error_code,
        } => {
            let who = label(account);
            Some(
                rightkit_agent_engine_protocol::ProtocolEvent::AccountRateLimited {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: created_at.to_string(),
                    provider: who.provider,
                    account: who.display,
                    account_id: who.account_id,
                    account_kind: who.kind,
                    email: who.email,
                    organization: who.organization,
                    status: status.clone(),
                    rate_limit_type: rate_limit_type.clone(),
                    resets_at: *resets_at,
                    utilization: *utilization,
                    overage_status: overage_status.clone(),
                    error_code: error_code.clone(),
                },
            )
        }
        NormalizedEventKind::TurnCostReported {
            account,
            total_cost_usd,
            duration_ms,
            num_turns,
            model_usage,
        } => {
            let who = label(account);
            Some(
                rightkit_agent_engine_protocol::ProtocolEvent::AccountUsageReported {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: created_at.to_string(),
                    provider: who.provider,
                    account: who.display,
                    account_id: who.account_id,
                    account_kind: who.kind,
                    email: who.email,
                    total_cost_usd: *total_cost_usd,
                    duration_ms: *duration_ms,
                    num_turns: *num_turns,
                    input_tokens: model_usage.iter().map(|row| row.input_tokens).sum(),
                    output_tokens: model_usage.iter().map(|row| row.output_tokens).sum(),
                    cache_read_tokens: model_usage
                        .iter()
                        .map(|row| row.cache_read_input_tokens)
                        .sum(),
                    cache_creation_tokens: model_usage
                        .iter()
                        .map(|row| row.cache_creation_input_tokens)
                        .sum(),
                    models: model_usage.iter().map(|row| row.model.clone()).collect(),
                },
            )
        }
        _ => None,
    }
}

/// Telemetry events never change binding status; keep the prior value.
fn retained_status(previous: &str) -> &'static str {
    match previous {
        "starting" => "starting",
        "ready" => "ready",
        "completed" => "completed",
        "failed" => "failed",
        "interrupted" => "interrupted",
        "waiting_for_approval" => "waiting_for_approval",
        _ => "running",
    }
}

pub(crate) fn driver_status_after_event(
    driver: DriverKind,
    previous: &str,
    kind: &NormalizedEventKind,
) -> &'static str {
    match kind {
        NormalizedEventKind::RateLimit { .. } | NormalizedEventKind::TurnCostReported { .. } => {
            retained_status(previous)
        }
        NormalizedEventKind::TurnFailed { .. } => "failed",
        NormalizedEventKind::TurnInterrupted => "interrupted",
        NormalizedEventKind::TurnCompleted | NormalizedEventKind::TurnCompletedWithUsage { .. } => {
            "completed"
        }
        NormalizedEventKind::GatewayReady => "ready",
        NormalizedEventKind::SessionStarted { .. } if driver == DriverKind::Claude => "starting",
        NormalizedEventKind::SessionStarted { .. } => "ready",
        NormalizedEventKind::ToolCallStarted { tool_name, .. }
            if driver == DriverKind::Claude
                && tool_name
                    .starts_with(&rightkit_agents::driver_host::host_names::mcp_tool_scope()) =>
        {
            "ready"
        }
        // Claude --print cannot expose its stream-json init until its first
        // controlled input arrives. Keep every pre-gateway echo non-ready so
        // a second prompt cannot bypass the host MCP confirmation.
        _ if driver == DriverKind::Claude && previous == "starting" => "starting",
        NormalizedEventKind::TurnStarted { .. } => "running",
        NormalizedEventKind::ApprovalRequested { .. } => "waiting_for_approval",
        _ => "running",
    }
}

pub(crate) fn publish_driver_failure(state: &DaemonState, session_id: &str, error: String) {
    let Some(live) = state.get_session(session_id) else {
        return;
    };
    let seq = live.conduit.next_seq();
    live.conduit
        .publish(rightkit_agent_engine_protocol::ProtocolEvent::RunFailed {
            session_id: session_id.to_string(),
            seq,
            created_at: crate::api::now_iso(),
            error,
            kind: "provider_error".into(),
            hint: "Driver protocol failed closed; inspect the recorded version/schema.".into(),
        });
}

pub(crate) fn publish_verification_result(
    state: &DaemonState,
    session_id: &str,
    result: &ToolResult,
) {
    let Some(live) = state.get_session(session_id) else {
        return;
    };
    let seq = live.conduit.next_seq();
    live.conduit.publish(
        rightkit_agent_engine_protocol::ProtocolEvent::VerificationFinished {
            session_id: session_id.to_string(),
            seq,
            created_at: crate::api::now_iso(),
            summary: result.content.clone(),
            passed: !result.is_error,
            tool: "shell".into(),
            issue_count: if result.is_error { 1 } else { 0 },
            issues: Vec::new(),
        },
    );
}

pub(crate) fn binding_driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Claude => "claude",
        DriverKind::Antigravity => "agy",
        // Not a driver the host runs; the label is the one the driver host gives its events.
        DriverKind::Codex => "codex",
    }
}

pub(crate) fn driver_protocol_version(driver_kind: &str) -> &'static str {
    match driver_kind {
        "claude" => "stream-json",
        _ => "native",
    }
}

pub(crate) fn protocol_schema_hash(driver_kind: &str, binary_version: &str) -> String {
    let digest = rightkit_fs::digest::sha256_bytes(
        format!(
            "{driver_kind}:{}:{binary_version}",
            driver_protocol_version(driver_kind)
        )
        .as_bytes(),
    );
    format!("sha256:{digest}")
}

pub(crate) fn driver_lifecycle(status: &str) -> &'static str {
    match status {
        "leased" => "leased",
        "starting" => "starting",
        "ready" => "ready",
        "running" => "running",
        "waiting_for_approval" => "waiting_for_approval",
        "interrupted" => "interrupted",
        "completed" => "completed",
        "recovery_required" => "recovery_required",
        "failed" => "failed",
        "stopped" => "stopped",
        "connected" => "ready",
        "resuming" => "starting",
        "waiting_for_child" => "running",
        _ => "failed",
    }
}

/// Did this binding's recorded schema stop matching what this build speaks?
///
/// A binding stores the schema hash computed when it was created. For kinds
/// whose hash is derived from the protocol constant, recomputing it is free and
/// a mismatch means exactly one thing: the session was bound under a protocol
/// this build no longer emits, so resuming it would talk past the driver. The
/// desktop strip needs that distinguished from a generic failure to name it.
pub(crate) fn binding_schema_drifted(
    driver_kind: &str,
    binary_version: &str,
    stored: &str,
) -> Option<bool> {
    Some(protocol_schema_hash(driver_kind, binary_version) != stored)
}

pub(crate) fn driver_diagnostic(status: &str) -> Option<&'static str> {
    match status {
        "failed" => {
            Some("Driver failed closed; inspect the recorded version and schema fingerprint.")
        }
        "recovery_required" => Some(
            "A prior driver process stopped after claiming work. Review the durable claim and explicitly recover before resuming.",
        ),
        _ => None,
    }
}

pub(crate) fn driver_capabilities_json() -> Value {
    json!({
        "transport": crate::host_identity::prefixed("mcp"),
        "requiresLease": true,
        "tools": DRIVER_CAPABILITIES.iter().map(|id| json!({
            "id": id,
            "requiresApproval": matches!(*id, "verify"),
        })).collect::<Vec<_>>(),
    })
}

/// Upper bound for a vendor `--version` probe (providers contract R19).
const DRIVER_VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Upper bound for the other vendor probes (`auth status`, `login status`, schema generation).
const DRIVER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) async fn capture_driver_version(executable: &str) -> Result<String, String> {
    let (status, stdout, _stderr) =
        managed_driver_probe_within(executable, &["--version"], DRIVER_VERSION_PROBE_TIMEOUT)
            .await
            .map_err(|error| format!("{executable} --version: {error}"))?;
    if status != 0 {
        return Err(format!("{executable} --version exited {status}"));
    }
    let version = stdout.trim().to_string();
    if version.is_empty() {
        return Err(format!("{executable} --version returned no version"));
    }
    Ok(version)
}

pub(crate) async fn driver_auth_required(executable: &str) -> bool {
    match executable {
        "claude" => {
            let Ok((status, stdout, _stderr)) =
                managed_driver_probe("claude", &["auth", "status"]).await
            else {
                return true;
            };
            if status != 0 {
                return true;
            }
            serde_json::from_slice::<Value>(stdout.as_bytes())
                .ok()
                .and_then(|value| value.get("loggedIn").and_then(Value::as_bool))
                .is_none_or(|logged_in| !logged_in)
        }
        _ => false,
    }
}

pub(crate) async fn managed_driver_probe(
    executable: &str,
    args: &[&str],
) -> Result<(i32, String, String), String> {
    managed_driver_probe_within(executable, args, DRIVER_PROBE_TIMEOUT).await
}

/// One sandboxed vendor probe whose spawn, read and wait together fit in `timeout`; on expiry the
/// child is cancelled (killed and reaped). A hung CLI can never stall discovery or the readiness
/// paths that share this probe (providers contract R19: no unbounded read of child output).
pub(crate) async fn managed_driver_probe_within(
    executable: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<(i32, String, String), String> {
    let adapter = rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter;
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.display().to_string(),
        sandbox_profile: Some("driver-probe".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.into(),
        argv: args.iter().map(|a| (*a).into()).collect(),
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    let started = std::time::Instant::now();
    let mut child = tokio::time::timeout(
        timeout,
        rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec),
    )
    .await
    .map_err(|_| "driver probe timed out".to_string())?
    .map_err(|e| format!("{e:?}"))?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "driver stdout unavailable".to_string())?;
    let mut bytes = Vec::new();
    // The vendor probe must answer inside the request budget; a hung CLI
    // cannot stall `driver/status` (or the readiness paths that share this
    // probe) indefinitely.
    let probed = tokio::time::timeout(timeout.saturating_sub(started.elapsed()), async {
        let read = tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| format!("{e:?}"))?;
        Ok::<(i32, usize), String>((status, read))
    })
    .await;
    match probed {
        Ok(Ok((status, _))) => Ok((
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
            String::new(),
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            let _ = child.cancel().await;
            Err("driver probe timed out".to_string())
        }
    }
}
