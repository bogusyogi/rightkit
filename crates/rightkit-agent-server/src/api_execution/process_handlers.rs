async fn list_execution_workspaces(
    Query(query): Query<ExecutionWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    let root = execution_workspace_root(&query);
    let records = load_execution_workspaces(&root)?;
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "storePath": execution_workspace_store_path(&root),
        "workspaces": records,
    })))
}

async fn list_execution_processes(
    State(state): State<DaemonState>,
    Query(query): Query<ExecutionProcessQuery>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    let workspace_filter = query
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let mut processes = live_execution_processes(&state, workspace_filter.as_deref(), false);
    persist_execution_process_snapshots(&processes)?;
    let store_path = workspace_filter
        .as_deref()
        .map(execution_process_store_path);
    if let Some(root) = workspace_filter.as_deref() {
        merge_historical_processes(root, &mut processes)?;
    }
    Ok(Json(serde_json::json!({
        "storePath": store_path,
        "processes": processes,
    })))
}

async fn create_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<CreateExecutionProcessRequest>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    let session_id = body.session_id.trim();
    if session_id.is_empty() {
        return Err(ExecutionError::bad_request("sessionId is required"));
    }
    let live = state
        .sessions
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .ok_or_else(|| ExecutionError::not_found("session not found"))?;
    if !live
        .execution_backend
        .as_deref()
        .unwrap_or("local")
        .eq_ignore_ascii_case("local")
    {
        return Err(ExecutionError::conflict(
            "interactive terminal launch currently supports local sessions only",
        ));
    }
    if live.workspace_root.trim().is_empty() {
        return Err(ExecutionError::conflict(
            "interactive terminal launch requires a bound workspace root",
        ));
    }
    if let Some(profile) = body.resource_profile.as_deref() {
        let profile = rightkit_agent_engine_runtime_resources::ResourceProfile::parse(profile)
            .map_err(ExecutionError::bad_request)?;
        // A caller may restrict a launch to Strict, but cannot expand the
        // composition-owned governor. Admission below still holds its permits.
        if profile == rightkit_agent_engine_runtime_resources::ResourceProfile::Strict {
            return Err(ExecutionError {
                status: StatusCode::FORBIDDEN,
                message: "strict resource profile denies child creation before spawn".into(),
            });
        }
        return Err(ExecutionError::bad_request(
            "resourceProfile only accepts the strict launch restriction; host profile is composition-owned",
        ));
    }
    let workspace_root = PathBuf::from(&live.workspace_root);
    let program = if cfg!(windows) {
        "powershell".to_string()
    } else {
        "sh".to_string()
    };
    let args = if cfg!(windows) {
        vec!["-NoLogo".to_string(), "-NoProfile".to_string()]
    } else {
        Vec::new()
    };
    let persistence = match body.persistence.as_deref().unwrap_or("inline") {
        "inline" => {
            rightkit_agent_tools::coding_tools::background_shell::ProcessPersistence::Inline
        }
        "service" => {
            rightkit_agent_tools::coding_tools::background_shell::ProcessPersistence::Service
        }
        other => {
            return Err(ExecutionError::bad_request(format!(
                "unsupported persistence '{other}'"
            )))
        }
    };
    let channel = match body.execution_channel.as_deref().unwrap_or("pipe") {
        "pipe" => rightkit_sandbox::execution::ExecutionChannel::Pipe,
        "pty" => {
            let size = body
                .pty_size
                .unwrap_or(rightkit_sandbox::execution::PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            if size.rows == 0 || size.cols == 0 || size.rows > 32767 || size.cols > 32767 {
                return Err(ExecutionError::bad_request(
                    "ptySize rows/cols must be in 1..32767",
                ));
            }
            rightkit_sandbox::execution::ExecutionChannel::Pty(size)
        }
        other => {
            return Err(ExecutionError::bad_request(format!(
                "unsupported executionChannel '{other}'"
            )))
        }
    };
    // This interactive-terminal route is the one deliberate exception to
    // deriving launches from `live.process_launch_adapter` (CG-B02's
    // governed/confined seam). It is opened by a human operator through
    // `/execution/processes` and driven by that human's own keystrokes —
    // never model-arbitrary argv the way the `shell` tool is — so it
    // launches through the explicit, unconfined `LocalProcessLaunchAdapter`
    // with `sandbox_required` left `false` instead. Confining an operator's
    // own terminal under Windows AppContainer produces a session that
    // reports `running` while emitting no output at all: a terminal the
    // operator cannot use. Every model-facing and automation launch (the
    // `shell` tool, hooks, scheduled scripts, driver host, worktree
    // tooling) keeps using the governed confined path via
    // `live.process_launch_adapter`; CG-B02's seam is unchanged for all of
    // them.
    let adapter: std::sync::Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter> =
        std::sync::Arc::new(
            rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_tools::coding_tools::process_launch::LocalProcessLaunchAdapter,
                rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
                    .map_err(|error| ExecutionError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: error.to_string(),
                    })?,
                rightkit_agent_engine_runtime_resources::ResourceKind::BackgroundProcess,
                true,
            ),
        );
    let mut context = live.process_launch_context.clone();
    context.workspace_root = workspace_root.display().to_string();
    context.sandbox_profile = Some("terminalSession".into());
    // The documented exception above, made real: an operator-driven terminal
    // is not confined (`live.process_launch_context` inherits the session's
    // sandbox requirement). The launch proof records `enforced: false`.
    context.sandbox_required = false;
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: program.clone(),
        argv: args,
        cwd: workspace_root.display().to_string(),
        context,
        env: Vec::new(),
    };
    let shell_id =
        rightkit_agent_tools::coding_tools::background_shell::spawn_managed_channel_with_kind(
            adapter,
            spec,
            channel,
            program,
            &live.background_shells,
            None,
            rightkit_agent_tools::coding_tools::background_shell::BgExecutionTarget {
                backend_id: Some("local".to_string()),
                workspace_id: live.execution_workspace_id.clone(),
                network_profile: live.execution_network_profile.clone(),
            },
            persistence,
            "terminalSession".into(),
        )
        .await
        .map_err(|error| match error {
            rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing => {
                ExecutionError {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    message: "resource governor denied process creation before spawn".into(),
                }
            }
            other => ExecutionError::conflict(other.to_string()),
        })?;
    let process = live_execution_process(&state, &shell_id, true)?
        .map(|(process, _)| process)
        .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({ "process": process })))
}

async fn get_execution_process(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<ExecutionProcessQuery>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    let process = if let Some(process) = live_execution_processes(&state, None, true)
        .into_iter()
        .find(|process| process.id == id)
    {
        process
    } else {
        historical_execution_process_from_query(&query, &id)?
            .ok_or_else(|| ExecutionError::not_found("execution process not found"))?
    };
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({ "process": process })))
}

async fn get_execution_process_logs(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<ExecutionProcessLogsQuery>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    let live = live_execution_process(&state, &id, false)?;
    let (process, output) = if let Some((process, shell)) = live {
        persist_execution_process_snapshots(std::slice::from_ref(&process))?;
        (process, shell.output_snapshot())
    } else {
        let process = historical_execution_process_from_logs_query(&query, &id)?
            .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
        let output = process.output.clone().unwrap_or_default();
        (process, output)
    };
    // API log tails are textual, so normalize platform line endings before
    // applying byte semantics. Otherwise Windows CRLF can consume one byte of
    // the requested tail and split the final line.
    let output = output.replace("\r\n", "\n").replace('\r', "\n");
    let byte_length = output.len();
    let tail_bytes = query
        .tail_bytes
        .filter(|value| *value > 0)
        .unwrap_or(byte_length)
        .min(256 * 1024);
    let (output, truncated) = tail_output_bytes(&output, tail_bytes);
    Ok(Json(serde_json::json!({
        "process": process,
        "logs": {
            "processId": id,
            "output": output,
            "byteLength": byte_length,
            "tailBytes": tail_bytes,
            "truncated": truncated,
        }
    })))
}

async fn kill_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ExecutionProcessQuery>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    let process = if let Some((process_before_kill, shell)) =
        live_execution_process(&state, &id, true)?
    {
        if !allows_free_terminal_control(&process_before_kill) {
            enforce_execution_backend_license(&state)?;
        }
        let killed = shell.kill();
        if killed {
            if let Some(live) = state.get_session(&process_before_kill.session_id) {
                let seq = live.conduit.next_seq();
                live.conduit.publish(
                    rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
                        session_id: process_before_kill.session_id.clone(),
                        seq,
                        created_at: format!(
                            "unix:{}",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs())
                        ),
                        text: format!(
                            "Operator stopped background shell {}.",
                            process_before_kill.id
                        ),
                        nonce: None,
                        trust: None,
                        attachments: Vec::new(),
                    },
                );
            }
        }
        live_execution_processes(&state, None, true)
            .into_iter()
            .find(|process| process.id == id)
            .unwrap_or(process_before_kill)
    } else {
        let process = historical_execution_process_from_query(&query, &id)?
            .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
        return Err(ExecutionError::conflict(format!(
            "historical execution process '{}' cannot be killed because only live local processes are controllable",
            process.id
        )));
    };
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({ "process": process })))
}

async fn write_execution_process_stdin(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ExecutionProcessQuery>,
    Json(body): Json<WriteExecutionProcessStdinRequest>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    if body.eof
        && (body.input.as_deref().is_some_and(|input| !input.is_empty())
            || body.input_base64.is_some()
            || body.append_newline == Some(true))
    {
        return Err(ExecutionError::bad_request("EOF cannot carry input bytes"));
    }
    if !body.eof && body.input.is_none() && body.input_base64.is_none() {
        return Err(ExecutionError::bad_request(
            "input or inputBase64 is required unless eof is true",
        ));
    }
    let live = live_execution_process(&state, &id, true)?;

    let mut input = if let Some(encoded) = body.input_base64 {
        String::from_utf8(
            rightkit_crypto::base64_decode(&encoded)
                .map_err(|_| ExecutionError::bad_request("invalid inputBase64"))?,
        )
        .map_err(|_| ExecutionError::bad_request("inputBase64 must be UTF-8"))?
    } else {
        body.input.unwrap_or_default()
    };
    let lease_epoch = body
        .lease_epoch
        .ok_or_else(|| ExecutionError::bad_request("leaseEpoch is required"))?;
    let input_sequence = body
        .input_sequence
        .ok_or_else(|| ExecutionError::bad_request("inputSequence is required"))?;
    if body.append_newline.unwrap_or(false) {
        // ConPTY line editors submit with Enter (CR); LF alone leaves the
        // command in the editable buffer. Pipe streams retain their LF framing.
        let terminal_enter = cfg!(windows)
            && live.as_ref().is_some_and(|(_, shell)| {
                matches!(
                    shell.execution_channel(),
                    rightkit_sandbox::execution::ExecutionChannel::Pty(_)
                )
            });
        input.push(if terminal_enter { '\r' } else { '\n' });
    }

    let process = if let Some((process_before_write, shell)) = live {
        if !allows_free_terminal_control(&process_before_write) {
            enforce_execution_backend_license(&state)?;
        }
        if body.eof {
            shell
                .close_stdin_with_lease(lease_epoch, input_sequence)
                .await
        } else {
            // #291: a client typing into the PTY is the human; the agent yields.
            shell.note_human_input();
            shell
                .write_stdin_bytes_with_lease(input.as_bytes(), lease_epoch, input_sequence)
                .await
        }
        .map_err(ExecutionError::conflict)?;
        live_execution_processes(&state, None, true)
            .into_iter()
            .find(|process| process.id == id)
            .ok_or_else(|| ExecutionError::not_found("execution process not found"))?
    } else {
        let process = historical_execution_process_from_query(&query, &id)?
            .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
        return Err(ExecutionError::conflict(format!(
            "historical execution process '{}' cannot receive stdin because only live local processes are controllable",
            process.id
        )));
    };
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({ "process": process })))
}

async fn resize_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(size): Json<rightkit_sandbox::execution::PtySize>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    if size.rows == 0 || size.cols == 0 || size.rows > 32767 || size.cols > 32767 {
        return Err(ExecutionError::bad_request("rows/cols must be in 1..32767"));
    }
    let (process, shell) = live_execution_process(&state, &id, true)?
        .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
    if !allows_free_terminal_control(&process) {
        enforce_execution_backend_license(&state)?;
    }
    shell
        .resize_pty(size)
        .await
        .map_err(ExecutionError::conflict)?;
    let process = live_execution_process(&state, &id, true)?
        .map(|(process, _)| process)
        .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({ "process": process })))
}

/// Request body for `POST /execution/processes/{id}/attach`. Empty/absent
/// body is equivalent to `{}` (no reconciliation decision supplied), which
/// is the common case: most reattaches have nothing unacknowledged and
/// `shell.attach()` succeeds outright. `reconcile_unacknowledged_input` lets
/// a client that already knows how to resolve an outstanding range (per
/// PTY-002: "reconnect explicitly reconciles or rejects unacknowledged
/// bytes rather than replaying them ambiguously") supply that decision
/// up front instead of round-tripping a 409 first.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachExecutionProcessRequest {
    #[serde(default)]
    reconcile_unacknowledged_input: Option<UnacknowledgedInputReconciliationChoice>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum UnacknowledgedInputReconciliationChoice {
    Delivered,
    Lost,
}

impl From<UnacknowledgedInputReconciliationChoice>
    for rightkit_agent_tools::coding_tools::background_shell::UnacknowledgedInputDecision
{
    fn from(choice: UnacknowledgedInputReconciliationChoice) -> Self {
        match choice {
            UnacknowledgedInputReconciliationChoice::Delivered => {
                rightkit_agent_tools::coding_tools::background_shell::UnacknowledgedInputDecision::ReconcileAsDelivered
            }
            UnacknowledgedInputReconciliationChoice::Lost => {
                rightkit_agent_tools::coding_tools::background_shell::UnacknowledgedInputDecision::ReconcileAsLost
            }
        }
    }
}

async fn attach_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(_query): Query<ExecutionProcessQuery>,
    body: Option<Json<AttachExecutionProcessRequest>>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    let Some((_, shell)) = live_execution_process(&state, &id, true)? else {
        return Err(ExecutionError::not_found("execution process not found"));
    };
    let reconcile = body.and_then(|Json(body)| body.reconcile_unacknowledged_input);
    let lease = match shell.attach().await {
        Ok(lease) => lease,
        Err(err) => match reconcile {
            Some(choice) => shell.reattach_after_reconciling_input(choice.into()).await,
            None => return Err(ExecutionError::conflict(err)),
        },
    };
    shell.set_human_attached(true);
    let process = live_execution_processes(&state, None, true)
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| ExecutionError::not_found("execution process not found"))?;
    persist_execution_process_snapshots(std::slice::from_ref(&process))?;
    Ok(Json(serde_json::json!({
        "process": process,
        "leaseEpoch": lease.epoch,
        "nextInputSequence": lease.next_input_sequence,
        "lastAckedInputSequence": lease.last_acked_input_sequence,
        "unacknowledgedInput": lease.unacknowledged_input,
    })))
}

async fn detach_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    let Some((_, shell)) = live_execution_process(&state, &id, true)? else {
        return Err(ExecutionError::not_found("execution process not found"));
    };
    shell.set_human_attached(false);
    let lease = shell.detach().await.map_err(ExecutionError::conflict)?;
    Ok(Json(
        serde_json::json!({ "processId": id, "attached": false, "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence }),
    ))
}

async fn ack_execution_process(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AcknowledgeExecutionProcessRequest>,
) -> Result<Json<serde_json::Value>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    let Some((_, shell)) = live_execution_process(&state, &id, true)? else {
        return Err(ExecutionError::not_found("execution process not found"));
    };
    let lease = shell
        .acknowledge_input(body.lease_epoch, body.sequence)
        .await
        .map_err(ExecutionError::conflict)?;
    Ok(Json(
        serde_json::json!({ "processId": id, "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence }),
    ))
}

async fn create_execution_workspace(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ExecutionWorkspaceQuery>,
    Json(body): Json<CreateExecutionWorkspaceRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    let backend = {
        let registry = lock_recover(&state.execution_backends, "state.execution_backends");
        registry
            .get(body.backend_id.trim())
            .cloned()
            .ok_or_else(|| ExecutionError::not_found("execution backend not found"))?
    };
    let root = execution_workspace_root(&query);
    let workspace_id = clean_workspace_id(body.workspace_id.as_deref())?;
    let now = now_iso();
    let paths = execution_workspace_paths(&root, &backend, &workspace_id)?;
    create_execution_workspace_dirs(&paths)?;

    let mut records = load_execution_workspaces(&root)?;
    if records.iter().any(|record| record.id == workspace_id) {
        return Err(ExecutionError::conflict(format!(
            "execution workspace '{workspace_id}' already exists"
        )));
    }
    let record = ExecutionWorkspaceRecord {
        id: workspace_id,
        backend_id: backend.id,
        name: body
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Execution workspace")
            .to_string(),
        status: "ready".to_string(),
        paths,
        created_at: now.clone(),
        updated_at: now,
    };
    std::fs::write(
        &record.paths.metadata,
        serde_json::to_string_pretty(&record)
            .map_err(|e| ExecutionError::internal(format!("workspace metadata: {e}")))?,
    )
    .map_err(|e| ExecutionError::internal(format!("write workspace metadata: {e}")))?;
    records.push(record.clone());
    records.sort_by(|a, b| a.id.cmp(&b.id));
    save_execution_workspaces(&root, &records)?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "workspaceRoot": root,
            "storePath": execution_workspace_store_path(&root),
            "workspace": record,
        })),
    ))
}

async fn doctor_execution_backend(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BackendDoctorQuery>,
) -> Result<Json<rightkit_sandbox::execution::BackendDoctorReport>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    let workspace = query.workspace_root.as_deref().map(PathBuf::from);
    let registry = lock_recover(&state.execution_backends, "state.execution_backends");
    let report = registry
        .doctor(&id, workspace.as_deref())
        .ok_or_else(|| ExecutionError::not_found("execution backend not found"))?;
    Ok(Json(report))
}
