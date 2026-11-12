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
    // Explicit account selection only (session, else project default); a selected account that
    // cannot be honoured fails the launch rather than falling back to another login.
    let mut codex_account: Option<crate::main_runtime::ResolvedClaudeAccount> = None;
    let codex_home = if binding.driver_kind == "codex" {
        codex_account = match crate::main_runtime::resolve_codex_account(
            &session_id,
            &live.workspace_root,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(session_id, error = %error, "selected Codex account unavailable; launch refused");
                return Err(StatusCode::CONFLICT);
            }
        };
        Some(
            prepare_codex_home_for_account(
                &state,
                &binding.id,
                codex_account.as_ref().map(|resolved| &resolved.launch),
            )
            .map_err(|error| {
                tracing::warn!(session_id, error = %error, "isolated Codex home setup failed closed");
                StatusCode::BAD_GATEWAY
            })?,
        )
    } else {
        None
    };
    let current_exe = std::env::current_exe().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Codex's startup status must name exactly the host MCP server
    // (`rightkit_agents::driver_host` rejects any other name).
    const HOST_MCP_SERVER_NAME: &str =
        rightkit_agents::driver_host::agy_harness::HOST_MCP_SERVER_NAME;
    let mcp_args = vec![
        "mcp-server".to_string(),
        "--session-id".to_string(),
        session_id.clone(),
    ];
    // Claude's launch policy pins `--allowedTools` to `mcp__rightkit`
    // (`claude_harness::HOST_MCP_TOOL_SCOPE`), so the Claude config names the server `rightkit`.
    let mcp_config = json!({
        "mcpServers": {
            "rightkit": {
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
        "codex" => {
            let mut spec = CodexLaunchOptions {
                executable: executable.clone().into(),
                cwd: PathBuf::from(&live.workspace_root),
                strict_config: true,
                model: body.model.clone(),
            }
            .launch_spec()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
            let command_toml = serde_json::to_string(&current_exe.to_string_lossy())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let args_toml =
                serde_json::to_string(&mcp_args).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let env_vars_toml = serde_json::to_string(DRIVER_MCP_FORWARDED_ENVIRONMENT)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            for feature in CODEX_DISABLED_VENDOR_FEATURES {
                spec.args
                    .extend(["--disable".to_string(), (*feature).to_string()]);
            }
            spec.args.extend([
                "-c".to_string(),
                format!("mcp_servers.{HOST_MCP_SERVER_NAME}.command={command_toml}"),
                "-c".to_string(),
                format!("mcp_servers.{HOST_MCP_SERVER_NAME}.args={args_toml}"),
                "-c".to_string(),
                format!("mcp_servers.{HOST_MCP_SERVER_NAME}.env_vars={env_vars_toml}"),
                "-c".to_string(),
                format!("mcp_servers.{HOST_MCP_SERVER_NAME}.enabled=true"),
                "-c".to_string(),
                format!("mcp_servers.{HOST_MCP_SERVER_NAME}.required=true"),
                "-c".to_string(),
                format!(
                    "mcp_servers.{HOST_MCP_SERVER_NAME}.default_tools_approval_mode=\"approve\""
                ),
            ]);
            spec.env.insert(
                "CODEX_HOME".to_string(),
                codex_home
                    .as_ref()
                    .expect("Codex home is prepared for Codex bindings")
                    .to_string_lossy()
                    .into_owned(),
            );
            rightkit_agents::driver_host::apply_codex_account_env(
                &mut spec,
                codex_account.as_ref().map(|resolved| &resolved.launch),
            );
            // #344: a managed Codex subscription must not be overridden by an ambient key.
            crate::main_runtime::apply_credential_precedence_guard(
                &mut spec,
                "codex",
                codex_account.as_ref().map(|resolved| &resolved.launch),
                &|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()),
            );
            claude_account_identity = codex_account.take().map(|resolved| resolved.identity);
            spec
        }
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
            // the real Claude CLI bound to RightKit MCP/policy controls.
            rightkit_agents::driver_host::claude_harness::enforce_delegation_launch_policy_for_legacy(
                &options,
            )
            .map_err(|_| StatusCode::CONFLICT)?;
            let mut spec = options.launch_spec().map_err(|_| StatusCode::BAD_REQUEST)?;
            // #336: persistent host answers can_use_tool over stdio through RightKit approvals.
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
        "RIGHTKIT_DAEMON_URL".into(),
        format!("http://127.0.0.1:{}", state.bound_port),
    );
    let capability_token = headers
        .get(CAPABILITY_HEADER.as_str())
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    spec.env.insert(
        "RIGHTKIT_GATEWAY_TOKEN".into(),
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
        "RIGHTKIT_DRIVER_INSTANCE_SECRET".into(),
        process_instance_secret,
    );

    let driver_kind = if binding.driver_kind == "codex" {
        DriverKind::Codex
    } else {
        DriverKind::Claude
    };
    // Return the launch policy as evidence from the actual process spec, not
    // merely from the requested driver kind. QA can therefore prove that the
    // driver was given exactly one RightKit MCP server and no native shell
    // route before it sends a conductor prompt.
    let disabled_features = spec
        .args
        .windows(2)
        .filter(|pair| pair[0] == "--disable")
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    let native_mutation_routes_disabled = CODEX_DISABLED_VENDOR_FEATURES
        .iter()
        .all(|feature| disabled_features.iter().any(|disabled| disabled == feature));
    if driver_kind == DriverKind::Codex {
        binding.schema_hash = match generate_codex_schema_hash(&executable).await {
            Ok(schema_hash) => schema_hash,
            Err(error) => {
                let _ = storage
                    .lock()
                    .await
                    .set_session_capability_lease_process_instance(&lease.id, None);
                tracing::warn!(session_id, error = %error, "Codex schema generation failed closed");
                return Err(StatusCode::BAD_GATEWAY);
            }
        };
    }
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
        let _ = state.driver_host.shutdown(&binding.id).await;
        let _ = storage
            .lock()
            .await
            .set_session_capability_lease_process_instance(&lease.id, None);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let info = started.info.clone();
    let resume_vendor_session_id = binding.vendor_session_id.clone();
    spawn_driver_event_consumer(state.clone(), binding, started.events);

    if driver_kind == DriverKind::Codex {
        let lifecycle = if let Some(vendor_session_id) = resume_vendor_session_id {
            state
                .driver_host
                .resume(&body.binding_id, &vendor_session_id)
                .await
                .map(|_| ())
        } else {
            state
                .driver_host
                .start_codex_thread(&body.binding_id, &live.workspace_root)
                .await
                .map(|_| ())
        };
        if lifecycle.is_err() {
            let _ = state.driver_host.shutdown(&body.binding_id).await;
            let _ = storage
                .lock()
                .await
                .set_session_capability_lease_process_instance(&lease.id, None);
            mark_driver_failed(&state, &body.binding_id).await;
            return Err(StatusCode::BAD_GATEWAY);
        }
    }
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
    if binding.status != "ready"
        && binding.status != "running"
        && binding.status != "completed"
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
    // Break RightKit-side approval/tool waits as well as the vendor turn.
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
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
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
    let request_id = state
        .driver_host
        .resume(&body.binding_id, &vendor_session_id)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
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
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
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
    matches!(driver_kind, "claude" | "codex")
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
            "codex" => Some(crate::main_runtime::session_auth_report(
                "codex",
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
    if !matches!(driver_kind.as_str(), "codex" | "claude") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let executable = driver_kind.clone();
    let version = capture_driver_version(&executable).await;
    let installed = version.is_ok();
    let schema_hash = if driver_kind == "codex" && installed {
        generate_codex_schema_hash(&executable).await.ok()
    } else {
        version
            .as_ref()
            .ok()
            .map(|version| protocol_schema_hash(&driver_kind, version))
    };
    let auth_required = driver_auth_required(&executable).await;
    let diagnostic = version.as_ref().err().cloned().or_else(|| {
        (driver_kind == "codex" && schema_hash.is_none()).then(|| {
            "Codex is installed but its app-server schema fingerprint could not be generated"
                .to_string()
        })
    });
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
/// driver-hosted session. Codex answers from its own `model/list` app-server method; Claude Code
/// has no such introspection command, so its list is a static marker for the frontend to fill in
/// from documentation rather than a fabricated catalogue.
pub(crate) async fn list_driver_models(
    Path(kind): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let driver_kind = kind.trim().to_ascii_lowercase();
    match driver_kind.as_str() {
        "claude" => Ok(Json(json!({ "models": [], "source": "static" }))),
        "codex" => match query_codex_model_catalogue("codex").await {
            Ok(models) => Ok(Json(
                json!({ "models": models, "source": "codex-app-server" }),
            )),
            // The vendor's own message is the whole diagnosis here — a bodyless
            // 502 left "app-server rejected the method" indistinguishable from
            // "codex is not installed", and cost a debugging session.
            Err(error) => {
                tracing::warn!(error = %error, "Codex model/list query failed closed");
                Err(driver_models_error(StatusCode::BAD_GATEWAY, error))
            }
        },
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
                        *event,
                    )
                    .await
                    {
                        tracing::warn!(binding_id = %binding.id, error = %error, "driver event rejected");
                        let _ = state.driver_host.shutdown(&binding.id).await;
                        break;
                    }
                }
                crate::driver_host_runtime::DriverRuntimeEvent::ServerRequest {
                    id,
                    method,
                    params,
                } => {
                    if method == "can_use_tool" {
                        // #336: answer through RightKit's own approval flow. The wait can
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
                    // RightKit gateway as the only effect path.
                    let _ = state.driver_host.respond_server_request(
                        &binding.id,
                        id,
                        json!({"decision": "decline", "reason": format!("{method} must use the RightKit capability gateway")}),
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
    match event.kind {
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
                return Ok(());
            }
            let seq = live.conduit.next_seq();
            let message = rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                id: format!("driver-{seq}"),
                created_at: now.clone(),
                content: text,
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            };
            live.conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::AssistantMessageCompleted {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now.clone(),
                    message,
                },
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
        // Gateway execution emits the canonical RightKit tool lifecycle.
        // Vendor tool echoes are retained in the driver inbox only.
        _ => {}
    }
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
        NormalizedEventKind::TurnCompleted => "completed",
        NormalizedEventKind::GatewayReady => "ready",
        NormalizedEventKind::SessionStarted { .. } if driver == DriverKind::Claude => "starting",
        NormalizedEventKind::SessionStarted { .. } => "ready",
        NormalizedEventKind::ToolCallStarted { tool_name, .. }
            if driver == DriverKind::Claude && tool_name.starts_with("mcp__rightkit") =>
        {
            "ready"
        }
        // Claude --print cannot expose its stream-json init until its first
        // controlled input arrives. Keep every pre-gateway echo non-ready so
        // a second prompt cannot bypass the RightKit MCP confirmation.
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
        DriverKind::Codex => "codex",
        DriverKind::Claude => "claude",
        DriverKind::Antigravity => "agy",
    }
}

pub(crate) fn driver_protocol_version(driver_kind: &str) -> &'static str {
    match driver_kind {
        "codex" => "app-server-jsonrpc",
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
///
/// Codex is deliberately `None`, not `false`: its hash comes from probing the
/// binary (`generate_codex_schema_hash`), so recomputing the protocol-derived
/// hash would mismatch on every healthy binding. Unknown is reported as unknown
/// rather than guessed.
pub(crate) fn binding_schema_drifted(
    driver_kind: &str,
    binary_version: &str,
    stored: &str,
) -> Option<bool> {
    match driver_kind {
        "codex" => None,
        kind => Some(protocol_schema_hash(kind, binary_version) != stored),
    }
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

pub(crate) async fn capture_driver_version(executable: &str) -> Result<String, String> {
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: std::env::current_dir()
            .map_err(|error| error.to_string())?
            .display()
            .to_string(),
        sandbox_profile: Some("driver-version-probe".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.into(),
        argv: vec!["--version".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
        .await
        .map_err(|error| format!("{executable} is unavailable: {error:?}"))?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| format!("{executable} stdout unavailable"))?;
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    let status = child.wait().await.map_err(|error| format!("{error:?}"))?;
    if status != 0 {
        return Err(format!("{executable} --version exited {status}"));
    }
    let version = String::from_utf8_lossy(&bytes).trim().to_string();
    if version.is_empty() {
        return Err(format!("{executable} --version returned no version"));
    }
    Ok(version)
}

pub(crate) async fn driver_auth_required(executable: &str) -> bool {
    match executable {
        // auth.json merely proves a file exists. It can be expired, revoked,
        // corrupt, or belong to another provider, so the CLI is the authority
        // for discovery readiness. Use the executable argument so a configured
        // driver binary is checked rather than an unrelated hard-coded path.
        "codex" => {
            let output = managed_driver_probe(executable, &["login", "status"]).await;
            let Ok((status, stdout, stderr)) = output else {
                return true;
            };
            !codex_login_status_is_authenticated(status == 0, &stdout, &stderr)
        }
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
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
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
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "driver stdout unavailable".to_string())?;
    let mut bytes = Vec::new();
    // The vendor probe must answer inside the request budget; a hung CLI
    // cannot stall `driver/status` (or the readiness paths that share this
    // probe) indefinitely.
    let probed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
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

pub(crate) fn codex_login_status_is_authenticated(
    success: bool,
    stdout: &str,
    stderr: &str,
) -> bool {
    if !success {
        return false;
    }
    // Codex currently prints e.g. "Logged in using ChatGPT". Match that
    // affirmative state rather than inferring it from a successful command:
    // future versions may use a zero exit for a diagnostic/no-login state.
    let output = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    !output.contains("not logged in")
        && !output.contains("unauthenticated")
        && (output.contains("logged in") || output.contains("authenticated"))
}

pub(crate) async fn generate_codex_schema_hash(executable: &str) -> Result<String, String> {
    let output_dir = std::env::temp_dir().join(format!(
        "{app}-codex-schema-{}",
        crate::startup::generate_token(),
        app = crate::host_identity::app()
    ));
    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output_arg = output_dir.to_string_lossy().into_owned();
    let args = [
        "app-server",
        "generate-json-schema",
        "--out",
        output_arg.as_str(),
    ];
    let hash = match managed_driver_probe(executable, &args).await {
        Ok((0, _, _)) => hash_schema_directory(&output_dir),
        Ok((status, _, stderr)) => Err(format!("schema generator exited {status}: {stderr}")),
        Err(error) => Err(error),
    };
    if let Err(error) = std::fs::remove_dir_all(&output_dir) {
        tracing::warn!(path = %output_dir.display(), error = %error, "failed to remove Codex schema cache directory");
    }
    hash
}

pub(crate) fn prepare_codex_home(state: &DaemonState, binding_id: &str) -> Result<PathBuf, String> {
    prepare_codex_home_for_account(state, binding_id, None)
}

/// Run-scoped Codex home for one binding. A selected subscription account seeds `auth.json` from
/// that account's managed `CODEX_HOME` (created by the official `codex login`); a selected API-key
/// account gets a home with no `auth.json` at all; no selection keeps the ambient login.
pub(crate) fn prepare_codex_home_for_account(
    state: &DaemonState,
    binding_id: &str,
    account: Option<&rightkit_agents::driver_host::ClaudeAccountLaunch>,
) -> Result<PathBuf, String> {
    let database = PathBuf::from(&state.database_path);
    let data_root = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "daemon database path has no parent".to_string())?;
    match account {
        Some(rightkit_agents::driver_host::ClaudeAccountLaunch::Subscription { managed_home }) => {
            prepare_codex_home_at(data_root, managed_home, binding_id)
        }
        Some(rightkit_agents::driver_host::ClaudeAccountLaunch::ApiKey { .. }) => {
            prepare_codex_home_inner(data_root, None, binding_id)
        }
        None => {
            let source_home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|value| PathBuf::from(value).join(".codex"))
                })
                .ok_or_else(|| "cannot locate the authenticated Codex home".to_string())?;
            prepare_codex_home_at(data_root, &source_home, binding_id)
        }
    }
}

pub(crate) fn prepare_codex_home_at(
    data_root: &std::path::Path,
    source_home: &std::path::Path,
    binding_id: &str,
) -> Result<PathBuf, String> {
    prepare_codex_home_inner(data_root, Some(source_home), binding_id)
}

pub(crate) fn prepare_codex_home_inner(
    data_root: &std::path::Path,
    source_home: Option<&std::path::Path>,
    binding_id: &str,
) -> Result<PathBuf, String> {
    let binding_hash = token_hash(binding_id);
    let home = data_root
        .join("driver-homes")
        .join("codex")
        .join(binding_hash);
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }

    let target_auth = home.join("auth.json");
    // Always drop any prior link first: the home is reused per binding, so a stale link from a
    // different account must never survive into this launch.
    if std::fs::symlink_metadata(&target_auth).is_ok() {
        std::fs::remove_file(&target_auth).map_err(|error| error.to_string())?;
    }
    if let Some(source_home) = source_home {
        let source_auth = source_home.join("auth.json");
        if !source_auth.is_file() {
            return Err("authenticated Codex auth.json is unavailable".to_string());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source_auth, &target_auth)
            .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        std::fs::hard_link(&source_auth, &target_auth).map_err(|error| error.to_string())?;
    }
    let config = home.join("config.toml");
    if std::fs::symlink_metadata(&config).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("RightKit Codex config path must not be a symlink".to_string());
    }
    std::fs::write(&config, "# RightKit run-scoped Codex configuration.\n")
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(home)
}

pub(crate) fn hash_schema_directory(root: &std::path::Path) -> Result<String, String> {
    fn hash_json_canonical(value: &Value, hasher: &mut rightkit_fs::digest::Sha256Hasher) {
        match value {
            Value::Null => hasher.update(b"null"),
            Value::Bool(value) => hasher.update(if *value { &b"true"[..] } else { &b"false"[..] }),
            Value::Number(value) => hasher.update(value.to_string().as_bytes()),
            Value::String(value) => {
                hasher.update(&serde_json::to_vec(value).unwrap_or_default());
            }
            Value::Array(values) => {
                hasher.update(b"[");
                for value in values {
                    hash_json_canonical(value, hasher);
                    hasher.update(&[0]);
                }
                hasher.update(b"]");
            }
            Value::Object(values) => {
                hasher.update(b"{");
                let mut keys = values.keys().collect::<Vec<_>>();
                keys.sort();
                for key in keys {
                    hasher.update(key.as_bytes());
                    hasher.update(&[0]);
                    hash_json_canonical(&values[key], hasher);
                    hasher.update(&[0]);
                }
                hasher.update(b"}");
            }
        }
    }

    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files)?;
            } else if path.is_file() {
                files.push(
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_path_buf(),
                );
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err("Codex schema generator produced no files".into());
    }
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        let bytes = std::fs::read(root.join(&relative)).map_err(|error| error.to_string())?;
        if relative.extension().and_then(|value| value.to_str()) == Some("json") {
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                format!("invalid generated schema {}: {error}", relative.display())
            })?;
            hash_json_canonical(&value, &mut hasher);
        } else {
            hasher.update(AsRef::<[u8]>::as_ref(&bytes));
        }
        hasher.update(&[0]);
    }
    Ok(format!("sha256:{}", hasher.finalize()))
}
