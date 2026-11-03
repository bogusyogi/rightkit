async fn fire_script_only_schedule(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
) -> ScheduleFireOutcome {
    // S:N3a/S:N4 — script occurrences get the same isolation namespace and
    // causal receipt chain as agent occurrences.
    let isolation = TaskIsolationV1::fresh(session_id).ok();
    let trigger = trigger_receipt(&entry.id, session_id, current_epoch_millis());
    let command = entry.prompt.trim();
    if command.is_empty() {
        return record_schedule_failure(root, entry, session_id, "scriptOnly command is empty");
    }
    if !entry
        .script_allowlist
        .iter()
        .any(|allowed| allowed.trim() == command)
    {
        return record_schedule_failure(
            root,
            entry,
            session_id,
            "scriptOnly command is not in scriptAllowlist",
        );
    }
    let backend = match script_only_execution_backend(entry) {
        Ok(backend) => backend,
        Err(message) => {
            return record_schedule_failure(root, entry, session_id, &message);
        }
    };

    let workdir = match script_workdir(root, entry) {
        Ok(workdir) => workdir,
        Err(message) => {
            return record_schedule_failure(root, entry, session_id, &message);
        }
    };

    let timeout_duration = entry
        .max_runtime_seconds
        .filter(|v| *v > 0)
        .map(Duration::from_secs);

    let _backend = backend;
    let spawned_ms = current_epoch_millis();
    let output = run_local_script_only_schedule(&workdir, command, timeout_duration).await;
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            return record_schedule_failure(
                root,
                entry,
                session_id,
                &format!("script failed to start: {e}"),
            );
        }
    };

    let exit_code = output.exit_code;
    let artifact_path = match write_script_output_artifact(root, entry, command, &output) {
        Ok(path) => path,
        Err(message) => {
            return record_schedule_failure(root, entry, session_id, &message);
        }
    };
    let status = if output.success {
        "scriptCompleted"
    } else {
        "scriptFailed"
    };
    // Hop chain: trigger -> run (script execution) -> child task (the command
    // process) -> result (exit outcome). Each hop is captured at its own time.
    let run_hop = run_receipt(&trigger, session_id, session_id, spawned_ms);
    let child_hop = child_task_receipt(
        &run_hop,
        &format!("{session_id}:script"),
        session_id,
        spawned_ms,
    );
    let result_hop = result_receipt(
        &child_hop,
        &format!(
            "outcome:{}:exit-{}",
            status,
            output
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
        session_id,
        current_epoch_millis(),
    );
    let chain = [trigger, run_hop, child_hop, result_hop];
    let delivery = deliver_schedule_result(
        state,
        root,
        entry,
        session_id,
        status,
        Some(&artifact_path),
        &format!("script output artifact: {}", artifact_path.display()),
    )
    .await;
    let mut event = schedule_run_event(
        entry,
        current_epoch_minute(),
        status,
        session_id,
        &causal_message_with_isolation(
            &format!("script output artifact: {}", artifact_path.display()),
            &chain,
            isolation.as_ref(),
        ),
    );
    event.exit_code = exit_code;
    event.artifact_path = Some(artifact_path.to_string_lossy().to_string());
    apply_script_only_execution_audit(&mut event, output.audit);
    event.delivery_status = delivery.status;
    event.delivery_errors = delivery.errors;
    if let Err(e) = append_run_event(root, event) {
        warn!(schedule_id = %entry.id, error = %e, "failed to record script run");
    }
    ScheduleFireOutcome {
        status: status.to_string(),
        message: format!("script output artifact: {}", artifact_path.display()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ScriptOnlyExecutionBackend;

#[derive(Clone, Debug)]
struct ScriptOnlyOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    success: bool,
    audit: ScriptOnlyExecutionAudit,
}

async fn run_local_script_only_schedule(
    workdir: &Path,
    command: &str,
    timeout_duration: Option<Duration>,
) -> Result<ScriptOnlyOutput, String> {
    let (program, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    use rightkit_sandbox::execution::{
        CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext,
    };
    #[cfg(test)]
    let adapter = rightkit_agent_tools::coding_tools::process_launch::LocalProcessLaunchAdapter;
    #[cfg(not(test))]
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let mut child = adapter
        .spawn(CommandLaunchSpec {
            program: program.to_string(),
            argv: vec![flag.to_string(), command.to_string()],
            cwd: workdir.to_string_lossy().into_owned(),
            context: ProcessLaunchContext {
                workspace_root: workdir.to_string_lossy().into_owned(),
                sandbox_profile: Some("workspace".to_string()),
                sandbox_required: true,
                network_required: false,
            },
            env: Vec::new(),
        })
        .await
        .map_err(|error| format!("managed script launch failed: {error:?}"))?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "managed script did not provide stdout".to_string())?;
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| "managed script did not provide stderr".to_string())?;
    let collect = async {
        let (stdout, stderr) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.map(|_| bytes)
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.map(|_| bytes)
            }
        );
        let code = child
            .wait()
            .await
            .map_err(|error| format!("managed script wait failed: {error:?}"))?;
        Ok::<_, String>((
            stdout.map_err(|error| format!("read script stdout: {error}"))?,
            stderr.map_err(|error| format!("read script stderr: {error}"))?,
            code,
        ))
    };
    let (stdout, stderr, code) = match timeout_duration {
        Some(duration) => match tokio::time::timeout(duration, collect).await {
            Ok(result) => result?,
            Err(_) => {
                // `timeout` drops `collect`, but managed children are not
                // kill-on-drop.  Cancel & reap explicitly so timeout keeps
                // its old bounded-process semantics instead of leaving
                // shell descendants or an unreaped owner behind.
                let _ = child.cancel().await;
                let _ = child.wait().await;
                return Err(format!(
                    "scriptOnly exceeded max_runtime_seconds ({})",
                    duration.as_secs()
                ));
            }
        },
        None => collect.await?,
    };
    Ok(ScriptOnlyOutput {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code: Some(code),
        success: code == 0,
        audit: ScriptOnlyExecutionAudit {
            resolved_execution_backend: Some("local".to_string()),
            ..Default::default()
        },
    })
}

fn script_workdir(root: &Path, entry: &ScheduleEntry) -> Result<PathBuf, String> {
    let Some(workdir) = entry
        .workdir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(root.to_path_buf());
    };
    let path = PathBuf::from(workdir);
    if !path.is_absolute() {
        return Err("scriptOnly workdir must be absolute".to_string());
    }
    if !normalize_lexical(&path).starts_with(normalize_lexical(root)) {
        return Err("scriptOnly workdir must stay inside the workspace".to_string());
    }
    Ok(path)
}

pub(crate) fn script_only_execution_backend(
    entry: &ScheduleEntry,
) -> Result<ScriptOnlyExecutionBackend, String> {
    let Some(backend) = entry
        .execution_backend
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(ScriptOnlyExecutionBackend);
    };
    if backend.eq_ignore_ascii_case("local") {
        return Ok(ScriptOnlyExecutionBackend);
    }
    Err(format!(
        "scriptOnly executionBackend '{backend}' is not available for command execution; supported backends: local"
    ))
}

fn write_script_output_artifact(
    root: &Path,
    entry: &ScheduleEntry,
    command: &str,
    output: &ScriptOnlyOutput,
) -> Result<PathBuf, String> {
    let dir = root.join(".coderight").join("schedule-artifacts");
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "failed to create schedule artifact dir '{}': {e}",
            dir.display()
        )
    })?;
    let path = dir.join(format!(
        "{}-{}.txt",
        sanitize_id(&entry.id),
        current_epoch_millis()
    ));
    let content = format!(
        "scheduleId: {}\nmode: scriptOnly\ncommand: {}\nexitCode: {}\n\n--- stdout ---\n{}\n--- stderr ---\n{}",
        entry.id,
        command,
        output
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        output.stdout,
        output.stderr
    );
    std::fs::write(&path, content).map_err(|e| {
        format!(
            "failed to write schedule artifact '{}': {e}",
            path.display()
        )
    })?;
    Ok(path)
}
