use super::artifacts::{changed_artifacts, format_repl_artifacts, snapshot_workspace_artifacts};
use super::helpers::{node_command, read_frame_bounded, NODE_HELPER, PYTHON_HELPER};
use super::session_tool::{ReplSession, ReplTool, MAX_REPL_FRAME_BYTES};
use crate::context::ToolContext;
use crate::tool_trait::ToolResult;
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};

impl ReplTool {
    pub(super) async fn start_session(&self, language: &str, ctx: &ToolContext) -> ToolResult {
        match self.spawn_session(language, None, ctx).await {
            Ok((session_id, session)) => {
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.clone(), session);

                ToolResult::ok(format!("Started {} REPL session: {}", language, session_id))
            }
            Err(error) => error,
        }
    }

    pub(super) async fn spawn_session(
        &self,
        language: &str,
        session_id: Option<String>,
        ctx: &ToolContext,
    ) -> Result<(String, ReplSession), ToolResult> {
        let workspace_root = ctx.workspace_root.clone();
        let (cmd, args): (String, Vec<&str>) = match language {
            "python" if cfg!(windows) => ("python".to_string(), vec!["-u", "-c", PYTHON_HELPER]),
            "python" => ("python3".to_string(), vec!["-u", "-c", PYTHON_HELPER]),
            "node" => (node_command(), vec!["-e", NODE_HELPER]),
            _ => {
                return Err(ToolResult::error(format!(
                    "Unsupported language: {}",
                    language
                )))
            }
        };
        // The governed adapter is injected by the engine's process-resource
        // authority (admission + sandbox policy). REPL code is arbitrary
        // model code, so it must never own a launcher of its own.
        let governed = ctx.process_launch_adapter.is_some();
        let adapter = ctx.process_launch_adapter.clone();
        let Some(adapter) = adapter else {
            return Err(ToolResult::error(
                "resource_limits_missing: process authority was not injected",
            ));
        };

        let base_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut context = ctx.process_launch_context.clone();
        context.workspace_root = workspace_root.display().to_string();
        context.sandbox_profile = Some("repl".into());
        // The REPL executes arbitrary model-authored code: confinement is a
        // hard requirement, not a preference.
        context.sandbox_required = true;
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: cmd,
            argv: base_args,
            cwd: workspace_root.display().to_string(),
            context,
            env: Vec::new(),
        };
        match adapter.spawn(spec).await {
            Ok(mut process) => {
                // Fail closed: a governed launch that claimed required
                // confinement but returned an unconfined child (for example a
                // platform sandbox setup fallback) must not run model code.
                // The test seam is exempt — it deliberately runs unconfined.
                if governed
                    && process
                        .sandbox_proof()
                        .map(|proof| !proof.enforced)
                        .unwrap_or(true)
                {
                    let _ = process.cancel().await;
                    return Err(ToolResult::error(format!(
                        "{language} REPL requires confinement but the launch is not sandboxed"
                    )));
                }
                let session_id = session_id.unwrap_or_else(|| {
                    format!(
                        "repl_{}_{}",
                        language,
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    )
                });
                let stdin = match process.take_stdin() {
                    Some(stdin) => stdin,
                    None => {
                        let _ = process.cancel().await;
                        return Err(ToolResult::error(format!("{language} REPL missing stdin")));
                    }
                };
                let stdout = match process.take_stdout() {
                    Some(stdout) => BufReader::new(stdout),
                    None => {
                        let _ = process.cancel().await;
                        return Err(ToolResult::error(format!("{language} REPL missing stdout")));
                    }
                };
                if let Some(mut stderr) = process.take_stderr() {
                    tokio::spawn(async move {
                        let mut buffer = [0u8; 8192];
                        loop {
                            match tokio::io::AsyncReadExt::read(&mut stderr, &mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    });
                }

                let session = ReplSession {
                    id: session_id.clone(),
                    language: language.to_string(),
                    workspace_root: workspace_root.to_path_buf(),
                    process,
                    stdin,
                    stdout,
                    next_request_id: 0,
                };

                Ok((session_id, session))
            }
            Err(e) => Err(ToolResult::error(format!(
                "Failed to start {} REPL: {}",
                language, e
            ))),
        }
    }

    /// One eval round-trip. The session is taken out of the map for the
    /// duration and only reinserted on a correlated response: on timeout or
    /// protocol desync the helper may still be executing, so the child is
    /// killed — a late frame must never be consumed by the next evaluation.
    pub(super) async fn eval_code(&self, session_id: &str, code: &str) -> ToolResult {
        let mut session = {
            let mut sessions = self.sessions.lock().await;
            match sessions.remove(session_id) {
                Some(session) => session,
                None => {
                    return ToolResult::error(format!("Session '{}' not found", session_id));
                }
            }
        };

        match self.eval_round_trip(&mut session, code).await {
            Ok(result) => {
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.to_string(), session);
                result
            }
            Err(mut result) => {
                let _ = session.stdin.shutdown().await;
                let _ = session.process.cancel().await;
                if !result.content.contains("terminated") {
                    result.content = format!(
                        "{} (session '{}' was terminated)",
                        result.content, session_id
                    );
                }
                result
            }
        }
    }

    async fn eval_round_trip(
        &self,
        session: &mut ReplSession,
        code: &str,
    ) -> Result<ToolResult, ToolResult> {
        let request_id = session.next_request_id;
        session.next_request_id += 1;

        let before_artifacts = snapshot_workspace_artifacts(&session.workspace_root);

        let payload = match serde_json::to_string(&json!({ "id": request_id, "code": code })) {
            Ok(p) => p,
            Err(e) => {
                return Err(ToolResult::error(format!(
                    "Failed to encode REPL request: {e}"
                )))
            }
        };

        if let Err(e) = session.stdin.write_all(payload.as_bytes()).await {
            return Err(ToolResult::error(format!(
                "Failed to write to REPL '{}': {e}",
                session.id
            )));
        }
        if let Err(e) = session.stdin.write_all(b"\n").await {
            return Err(ToolResult::error(format!(
                "Failed to flush REPL '{}': {e}",
                session.id
            )));
        }
        if let Err(e) = session.stdin.flush().await {
            return Err(ToolResult::error(format!(
                "Failed to flush REPL '{}': {e}",
                session.id
            )));
        }

        let frame = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_frame_bounded(&mut session.stdout, MAX_REPL_FRAME_BYTES),
        )
        .await
        {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => {
                return Err(ToolResult::error(format!(
                    "REPL '{}' exited before returning output",
                    session.id
                )))
            }
            Ok(Err(e)) => {
                return Err(ToolResult::error(format!(
                    "Failed to read REPL '{}': {e}",
                    session.id
                )))
            }
            Err(_) => {
                return Err(ToolResult::error(format!(
                    "REPL '{}' timed out after 10s",
                    session.id
                )))
            }
        };

        let line = match String::from_utf8(frame) {
            Ok(line) => line,
            Err(_) => {
                return Err(ToolResult::error(format!(
                    "REPL '{}' returned a non-UTF-8 frame",
                    session.id
                )))
            }
        };

        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                return Err(ToolResult::error(format!(
                    "Invalid REPL response from '{}': {e}: {line}",
                    session.id
                )))
            }
        };
        if value.get("id").and_then(Value::as_u64) != Some(request_id) {
            return Err(ToolResult::error(format!(
                "REPL '{}' returned an uncorrelated response",
                session.id
            )));
        }

        let stdout = value["stdout"].as_str().unwrap_or("").trim_end();
        let stderr = value["stderr"].as_str().unwrap_or("").trim_end();
        let result = value["result"].as_str().unwrap_or("").trim_end();
        let error = value["error"].as_str().unwrap_or("").trim_end();
        let after_artifacts = snapshot_workspace_artifacts(&session.workspace_root);
        let artifacts =
            changed_artifacts(&session.workspace_root, &before_artifacts, &after_artifacts);

        if value["ok"].as_bool().unwrap_or(false) {
            let mut parts = Vec::new();
            if !stdout.is_empty() {
                parts.push(format!("stdout:\n{stdout}"));
            }
            if !stderr.is_empty() {
                parts.push(format!("stderr:\n{stderr}"));
            }
            if !result.is_empty() {
                parts.push(format!("result:\n{result}"));
            }
            if !artifacts.is_empty() {
                parts.push(format_repl_artifacts(&artifacts));
            }
            Ok(ToolResult::ok(if parts.is_empty() {
                "(no output)".to_string()
            } else {
                parts.join("\n")
            }))
        } else {
            let mut message = String::new();
            if !stdout.is_empty() {
                message.push_str(&format!("stdout:\n{stdout}\n"));
            }
            if !stderr.is_empty() {
                message.push_str(&format!("stderr:\n{stderr}\n"));
            }
            if !error.is_empty() {
                message.push_str(error);
            }
            // A helper-reported failure (traceback etc.) is a valid
            // correlated round-trip: the session stays alive.
            Ok(ToolResult::error(if message.trim().is_empty() {
                "REPL evaluation failed".to_string()
            } else {
                message.trim().to_string()
            }))
        }
    }

    pub(super) async fn status(&self, session_id: Option<&str>) -> ToolResult {
        let sessions = self.sessions.lock().await;
        if let Some(session_id) = session_id {
            let Some(session) = sessions.get(session_id) else {
                return ToolResult::error(format!("Session '{}' not found", session_id));
            };
            return ToolResult::ok(format!(
                "Session '{}' language={} pid={}",
                session.id,
                session.language,
                session
                    .process
                    .pid()
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ));
        }

        if sessions.is_empty() {
            return ToolResult::ok("No active REPL sessions.".to_string());
        }

        let mut rows = sessions
            .values()
            .map(|session| {
                format!(
                    "- {} language={} pid={}",
                    session.id,
                    session.language,
                    session
                        .process
                        .pid()
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "unknown".into())
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        ToolResult::ok(format!("Active REPL sessions:\n{}", rows.join("\n")))
    }

    pub(super) async fn reset_session(&self, session_id: &str, ctx: &ToolContext) -> ToolResult {
        let mut sessions = self.sessions.lock().await;
        let Some(mut old_session) = sessions.remove(session_id) else {
            return ToolResult::error(format!("Session '{}' not found", session_id));
        };
        let language = old_session.language.clone();
        drop(sessions);

        let _ = old_session.stdin.shutdown().await;
        let _ = old_session.process.cancel().await;

        match self
            .spawn_session(&language, Some(session_id.to_string()), ctx)
            .await
        {
            Ok((new_id, new_session)) => {
                self.sessions
                    .lock()
                    .await
                    .insert(new_id.clone(), new_session);
                ToolResult::ok(format!("Reset {} REPL session: {}", language, new_id))
            }
            Err(error) => error,
        }
    }

    pub(super) async fn close_session(&self, session_id: &str) -> ToolResult {
        let mut sessions = self.sessions.lock().await;

        if let Some(mut session) = sessions.remove(session_id) {
            drop(sessions);
            let _ = session.stdin.shutdown().await;
            let _ = session.process.cancel().await;
            ToolResult::ok(format!("Closed session '{}'", session_id))
        } else {
            ToolResult::error(format!("Session '{}' not found", session_id))
        }
    }
}
