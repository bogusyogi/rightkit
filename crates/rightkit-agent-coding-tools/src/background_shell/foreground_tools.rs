/// Record a foreground process after it has exited, timed out, or been
/// cancelled. The local runner still executes synchronously, but the process
/// table can now show durable metadata for completed foreground shell work.
pub fn record_foreground_process(
    process_id: Option<String>,
    command: String,
    output: String,
    status: BgStatus,
    store: &BgStore,
    execution_target: BgExecutionTarget,
) -> String {
    let row_id = process_id
        .as_deref()
        .map(|pid| format!("fg-{pid}"))
        .unwrap_or_else(|| format!("fg-{}", next_shell_sequence()));
    let mut bounded_output = BoundedOutput::default();
    bounded_output.push(output.as_bytes());
    let shell = Arc::new(BgShell {
        command,
        process_kind: "foregroundShell".to_string(),
        execution_target,
        launch_proof: None,
        pid: Mutex::new(
            process_id
                .as_deref()
                .and_then(|pid| pid.parse::<u32>().ok()),
        ),
        started_at_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        output: Mutex::new(bounded_output),
        cursor: Mutex::new(0),
        status: Mutex::new(status),
        kill_tx: Mutex::new(None),
        stdin: tokio::sync::Mutex::new(None),
        channel: Mutex::new(rightkit_sandbox::execution::ExecutionChannel::Pipe),
        control_tx: None,
        persistence: ProcessPersistence::Inline,
        lease: tokio::sync::Mutex::new(ProcessLease {
            epoch: 1,
            next_input_sequence: 0,
            last_acked_input_sequence: 0,
            attached: true,
        }),
        human: Default::default(),
    });
    lock_recover(store, "background_shell.store").insert(row_id.clone(), shell);
    notify_shell_event();
    row_id
}

// ── process lifecycle inventory ─────────────────────────────────────────────

/// One typed surface for durable process inventory and control. Legacy
/// `get_shell_output`/`kill_shell` remain registered for compatibility; this
/// tool exposes the complete list/get/output/input/stop/reattach contract.
pub struct ProcessTool;

#[async_trait::async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "List/get/output/input/stop/reattach daemon-owned process handles with bounded output and typed exit state."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": { "type": "string", "enum": ["list", "get", "output", "input", "resize", "eof", "stop", "attach", "detach", "ack", "reattach"] },
                "process_id": { "type": "string", "description": "Daemon process handle returned by shell" },
                "input": { "type": "string" },
                "input_base64": { "type": "string", "description": "Raw input bytes; mutually exclusive with input" },
                "size": { "type": "object", "properties": { "rows": { "type": "integer", "minimum": 1, "maximum": 32767 }, "cols": { "type": "integer", "minimum": 1, "maximum": 32767 } }, "required": ["rows", "cols"] },
                "lease_epoch": { "type": "integer", "minimum": 1 },
                "input_sequence": { "type": "integer", "minimum": 0 },
                "acknowledge_sequence": { "type": "integer", "minimum": 0 },
                "reconcile_unacknowledged_input": { "type": "string", "enum": ["delivered", "lost"] }
            }
        })
    }

    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        matches!(args["operation"].as_str(), Some("input" | "eof"))
            .then_some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
    }

    fn capability_class_for(&self, args: &Value) -> Option<rightkit_agent_policy::CapabilityClass> {
        match args["operation"].as_str() {
            Some("list" | "get" | "output" | "attach" | "reattach") => {
                Some(rightkit_agent_policy::CapabilityClass::ReadFile)
            }
            _ => Some(rightkit_agent_policy::CapabilityClass::ProcessControl),
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(operation) => operation,
            None => return ToolResult::error("Missing required argument: operation"),
        };
        let Some(store) = &ctx.background_shells else {
            return ToolResult::error("Process inventory is not available in this context.");
        };
        if operation == "list" {
            let shells = lock_recover(store, "background_shell.store");
            let mut rows: Vec<Value> = shells
                .iter()
                .map(|(id, shell)| process_row(id, shell))
                .collect();
            rows.sort_by(|left, right| {
                left["processId"].as_str().cmp(&right["processId"].as_str())
            });
            return ToolResult::ok(json!({"processes": rows}).to_string());
        }
        let process_id = match args["process_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id,
            _ => return ToolResult::error(format!("process {operation} requires 'process_id'.")),
        };
        let shell = lock_recover(store, "background_shell.store")
            .get(process_id)
            .cloned();
        let Some(shell) = shell else {
            return ToolResult::error(format!(
                "No daemon-owned process handle '{process_id}'; it cannot be reattached."
            ));
        };
        match operation {
            "get" => ToolResult::ok(process_row(process_id, &shell).to_string()),
            "output" => {
                let (bytes, truncated) = shell.drain_new_payload();
                ToolResult::ok(
                    json!({
                        "process": process_row(process_id, &shell),
                        "output": String::from_utf8_lossy(&bytes),
                        "output_base64": rightkit_crypto::base64_encode(&bytes),
                        "truncated": truncated,
                    })
                    .to_string(),
                )
            }
            "input" | "eof" => {
                // Rule 55: a confined agent cannot type into an unconfined shell. The operator
                // terminal launches with `sandbox_required = false` and records `enforced: false`.
                if shell.launched_unconfined() {
                    return ToolResult::error(UNCONFINED_PROCESS_INPUT_REFUSAL);
                }
                if !rightkit_agent_policy::durable_write_is_authorized(
                    &ctx.content_provenance,
                    ctx.durable_write_approved,
                ) {
                    return ToolResult::error(
                        "explicit durable-write approval required for process input/EOF",
                    );
                }
                let epoch = match args.get("lease_epoch").and_then(Value::as_u64) {
                    Some(value) => value,
                    None => return ToolResult::error("invalid_request: lease_epoch is required"),
                };
                let sequence = match args.get("input_sequence").and_then(Value::as_u64) {
                    Some(value) => value,
                    None => return ToolResult::error("invalid_request: input_sequence is required"),
                };
                if operation == "eof" {
                    return match shell.close_stdin_with_lease(epoch, sequence).await {
                        Ok(lease) => ToolResult::ok(json!({"processId": process_id, "eof": true, "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence}).to_string()),
                        Err(error) => ToolResult::error(error),
                    };
                }
                let bytes = match decode_process_input(&args) {
                    Ok(bytes) => bytes,
                    Err(error) => return ToolResult::error(error),
                };
                // #291: a human attached to this PTY always wins the input.
                match shell.write_agent_input_with_lease(&bytes, epoch, sequence).await {
                    Ok(AgentInputOutcome::Written(lease)) => ToolResult::ok(json!({"processId": process_id, "bytesWritten": bytes.len(), "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence}).to_string()),
                    Ok(AgentInputOutcome::Queued { lease, queued_bytes }) => ToolResult::ok(json!({"processId": process_id, "queued": true, "bytesQueued": bytes.len(), "queuedBytes": queued_bytes, "note": "A human is typing in this terminal, so your input was NOT sent yet. It is held and will be delivered in order once the human has been idle for 1.5 seconds. Do not resend it.", "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence}).to_string()),
                    Err(error) => ToolResult::error(error),
                }
            }
            "resize" => {
                let size =
                    match serde_json::from_value::<rightkit_sandbox::execution::PtySize>(
                        args["size"].clone(),
                    ) {
                        Ok(size)
                            if size.rows > 0
                                && size.cols > 0
                                && size.rows <= 32767
                                && size.cols <= 32767 =>
                        {
                            size
                        }
                        _ => return ToolResult::error(
                            "invalid_request: process resize requires size rows/cols in 1..32767",
                        ),
                    };
                match shell.resize_pty(size).await {
                    Ok(()) => ToolResult::ok(process_row(process_id, &shell).to_string()),
                    Err(error) => ToolResult::error(error),
                }
            }
            "attach" => {
                // `reconcile_unacknowledged_input`: "delivered" | "lost". Only
                // consulted when `shell.attach()` rejects because an
                // unacknowledged input range is outstanding (PTY-002: a
                // reattach must explicitly reconcile or reject unacknowledged
                // bytes rather than replaying them ambiguously). Omitting it
                // is fine for the common case where nothing is outstanding.
                let reconcile = match args.get("reconcile_unacknowledged_input").and_then(Value::as_str) {
                    Some("delivered") => Some(UnacknowledgedInputDecision::ReconcileAsDelivered),
                    Some("lost") => Some(UnacknowledgedInputDecision::ReconcileAsLost),
                    Some(other) => {
                        return ToolResult::error(format!(
                            "invalid_request: reconcile_unacknowledged_input must be 'delivered' or 'lost', got '{other}'"
                        ))
                    }
                    None => None,
                };
                let lease = match shell.attach().await {
                    Ok(lease) => lease,
                    Err(error) => match reconcile {
                        Some(decision) => shell.reattach_after_reconciling_input(decision).await,
                        None => return ToolResult::error(error),
                    },
                };
                ToolResult::ok(json!({"attached": true, "process": process_row(process_id, &shell), "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence, "unacknowledgedInput": lease.unacknowledged_input}).to_string())
            }
            "detach" => {
                match shell.detach().await {
                    Ok(lease) => ToolResult::ok(json!({"attached": false, "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence}).to_string()),
                    Err(error) => ToolResult::error(error),
                }
            }
            "ack" => {
                let epoch = match args.get("lease_epoch").and_then(Value::as_u64) { Some(value) => value, None => return ToolResult::error("invalid_request: lease_epoch is required") };
                let sequence = match args.get("acknowledge_sequence").and_then(Value::as_u64) { Some(value) => value, None => return ToolResult::error("invalid_request: acknowledge_sequence is required") };
                match shell.acknowledge_input(epoch, sequence).await {
                    Ok(lease) => ToolResult::ok(json!({"processId": process_id, "leaseEpoch": lease.epoch, "nextInputSequence": lease.next_input_sequence, "lastAckedInputSequence": lease.last_acked_input_sequence}).to_string()),
                    Err(error) => ToolResult::error(error),
                }
            }
            "stop" => {
                if shell.kill() {
                    ToolResult::ok(json!({"processId": process_id, "signal": "stop"}).to_string())
                } else {
                    ToolResult::ok(
                        json!({"processId": process_id, "signal": "none", "status": status_label(&shell.status())}).to_string(),
                    )
                }
            }
            "reattach" => {
                if matches!(shell.status(), BgStatus::Running) && shell.pid().is_none() {
                    ToolResult::error(format!(
                        "Process handle '{process_id}' lost its OS identity and cannot be safely reattached."
                    ))
                } else {
                    let reconcile = match args
                        .get("reconcile_unacknowledged_input")
                        .and_then(Value::as_str)
                    {
                        Some("delivered") => Some(UnacknowledgedInputDecision::ReconcileAsDelivered),
                        Some("lost") => Some(UnacknowledgedInputDecision::ReconcileAsLost),
                        Some(other) => return ToolResult::error(format!(
                            "invalid_request: reconcile_unacknowledged_input must be 'delivered' or 'lost', got '{other}'"
                        )),
                        None => None,
                    };
                    let lease = match shell.attach().await {
                        Ok(lease) => lease,
                        Err(error) if reconcile.is_some() && is_unacknowledged_input_error(&error) => {
                            shell.reattach_after_reconciling_input(reconcile.expect("checked above")).await
                        }
                        Err(error) => return ToolResult::error(error),
                    };
                    ToolResult::ok(json!({
                        "reattached": true,
                        "process": process_row(process_id, &shell),
                        "leaseEpoch": lease.epoch,
                        "nextInputSequence": lease.next_input_sequence,
                        "lastAckedInputSequence": lease.last_acked_input_sequence,
                        "unacknowledgedInput": lease.unacknowledged_input,
                    }).to_string())
                }
            }
            other => ToolResult::error(format!("Unknown process operation '{other}'.")),
        }
    }
}

/// Refusal text for agent `process` input or EOF aimed at a process whose launch proof says
/// `enforced: false` (rule 55).
pub(crate) const UNCONFINED_PROCESS_INPUT_REFUSAL: &str = "refused: this process was launched outside the sandbox (launch proof enforced:false, such as the operator terminal), and a sandboxed agent cannot type into an unconfined process";

fn process_row(id: &str, shell: &BgShell) -> Value {
    let target = shell.execution_target();
    json!({
        "processId": id,
        "osPid": shell.pid(),
        "kind": shell.process_kind(),
        "executionChannel": shell.execution_channel(),
        "persistence": shell.persistence().label(),
        "unconfined": shell.launched_unconfined(),
        "attached": shell.lease_snapshot_now().attached,
        "leaseEpoch": shell.lease_snapshot_now().epoch,
        "nextInputSequence": shell.lease_snapshot_now().next_input_sequence,
        "lastAckedInputSequence": shell.lease_snapshot_now().last_acked_input_sequence,
        "command": shell.command,
        "status": status_label(&shell.status()),
        "exit": shell.exit_status(),
        "startedAtNs": shell.started_at_ns().to_string(),
        "outputBytes": lock_recover(&shell.output, "background_shell.output").total_bytes(),
        "executionTarget": {
            "backendId": target.backend_id,
            "workspaceId": target.workspace_id,
            "networkProfile": target.network_profile
        }
    })
}

fn status_label(st: &BgStatus) -> String {
    match st {
        BgStatus::Running => "running".into(),
        BgStatus::Exited(c) => format!("exited (code {c})"),
        BgStatus::Killed => "killed".into(),
        BgStatus::SpawnFailed(e) => format!("spawn failed: {e}"),
        BgStatus::Unknown => "unknown (restart reconciliation required)".into(),
    }
}

// ── get_shell_output ─────────────────────────────────────────────────────────

const DEFAULT_OUTPUT_READ_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_READ_BYTES: usize = 256 * 1024;
const KILL_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct GetShellOutputTool;

#[async_trait::async_trait]
impl Tool for GetShellOutputTool {
    fn name(&self) -> &str {
        "get_shell_output"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::ReadFile)
    }

    fn description(&self) -> &str {
        "Return output from a background shell started with shell(run_in_background=true), \
         along with its run status and typed exit status. By default returns only output \
         produced since the last check; pass `since` (absolute byte offset) to re-read from an \
         offset. Output per call is bounded by `max_bytes`; `next_offset` and `more` tell you \
         how to continue."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["shell_id"],
            "properties": {
                "shell_id": { "type": "string", "description": "Id returned when the background shell was started" },
                "since": { "type": "integer", "minimum": 0, "description": "Absolute byte offset to read from; omit for new output only" },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 262144, "description": "Maximum bytes to return (default 32768)" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let shell_id = match args["shell_id"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: shell_id"),
        };
        let store = match &ctx.background_shells {
            Some(s) => s,
            None => {
                return ToolResult::error("Background shells are not available in this context.")
            }
        };
        let shell = lock_recover(store, "background_shell.store")
            .get(shell_id)
            .cloned();
        match shell {
            Some(sh) => {
                let since = match args.get("since") {
                    None | Some(Value::Null) => None,
                    Some(v) => match v.as_u64() {
                        Some(n) => Some(n as usize),
                        None => {
                            return ToolResult::error(
                                "invalid_request: 'since' must be a non-negative integer",
                            )
                        }
                    },
                };
                let max_bytes = match args.get("max_bytes") {
                    None | Some(Value::Null) => DEFAULT_OUTPUT_READ_BYTES,
                    Some(v) => match v.as_u64() {
                        Some(n) if n >= 1 => (n as usize).min(MAX_OUTPUT_READ_BYTES),
                        _ => {
                            return ToolResult::error(
                                "invalid_request: 'max_bytes' must be a positive integer",
                            )
                        }
                    },
                };
                let window = sh.read_window(since, max_bytes);
                if window.beyond_end {
                    return ToolResult::error(format!(
                        "'since' offset is beyond the end of output ({} bytes so far).",
                        window.total
                    ));
                }
                let status = status_label(&sh.status());
                let exit = sh
                    .exit_status()
                    .map(|e| format!(" exit={}", serde_json::to_string(&e).unwrap_or_default()))
                    .unwrap_or_default();
                let meta = format!(
                    "offset={} next_offset={} total={} more={}",
                    window.start, window.next_offset, window.total, window.more
                );
                let text = String::from_utf8_lossy(&window.bytes);
                let body = if window.bytes.is_empty() {
                    format!("[{status}]{exit} {meta}\n(no new output)")
                } else if window.gap {
                    format!("[{status}]{exit} {meta}\n[retained output was truncated; bounded snapshot follows]\n{text}")
                } else {
                    format!("[{status}]{exit} {meta}\n{text}")
                };
                ToolResult::ok(body)
            }
            None => ToolResult::error(format!("No background shell with id '{shell_id}'.")),
        }
    }
}

// ── kill_shell ───────────────────────────────────────────────────────────────

pub struct KillShellTool;

#[async_trait::async_trait]
impl Tool for KillShellTool {
    fn name(&self) -> &str {
        "kill_shell"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::ProcessControl)
    }

    fn description(&self) -> &str {
        "Terminate a background shell started with shell(run_in_background=true)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["shell_id"],
            "properties": {
                "shell_id": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let shell_id = match args["shell_id"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: shell_id"),
        };
        let store = match &ctx.background_shells {
            Some(s) => s,
            None => {
                return ToolResult::error("Background shells are not available in this context.")
            }
        };
        let shell = lock_recover(store, "background_shell.store")
            .get(shell_id)
            .cloned();
        match shell {
            Some(sh) => {
                let sent = sh.kill();
                // The runner terminates the whole process tree and waits on the
                // child before publishing a terminal status; wait for that.
                // Wait on the shared shell-event signal, not on a timer (#377): the wait costs no
                // wakeups until a shell changes state, and ends at the reap deadline otherwise.
                // Subscribe before the first status read so a change cannot be missed.
                let mut events = subscribe_shell_events();
                let deadline = tokio::time::Instant::now() + KILL_REAP_TIMEOUT;
                while matches!(sh.status(), BgStatus::Running) {
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let _ = tokio::time::timeout(deadline - now, events.changed()).await;
                }
                match sh.exit_status() {
                    Some(exit) => {
                        let verb = if sent { "Killed" } else { "Already finished:" };
                        ToolResult::ok(format!(
                            "{verb} '{shell_id}' [{}] exit={}",
                            exit.label(),
                            serde_json::to_string(&exit).unwrap_or_default()
                        ))
                    }
                    None => ToolResult::error(format!(
                        "'{shell_id}' did not finish reaping within {}s of the kill signal.",
                        KILL_REAP_TIMEOUT.as_secs()
                    )),
                }
            }
            None => ToolResult::error(format!("No background shell with id '{shell_id}'.")),
        }
    }
}

fn decode_process_input(args: &Value) -> Result<Vec<u8>, String> {
    let bytes = match (args.get("input"), args.get("input_base64")) {
        (Some(Value::String(text)), None) if text.len() <= 65536 => text.as_bytes().to_vec(),
        (None, Some(Value::String(encoded))) if encoded.len() <= 87384 => {
            rightkit_crypto::base64_decode(encoded)
                .map_err(|_| "invalid_request: invalid base64 process input".to_string())?
        }
        _ => return Err("invalid_request: provide one bounded input or input_base64 field".into()),
    };
    if bytes.len() > 65536 {
        return Err("invalid_request: process input exceeds 65536 bytes".into());
    }
    Ok(bytes)
}
