#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareRequest {
    /// Legacy: prompt for model-based comparison
    pub prompt: Option<String>,
    /// Legacy: models for model-based comparison
    pub models: Option<Vec<String>>,
    /// Agent-based: task/goal to run in both variants
    pub task: Option<String>,
    /// Agent-based: variant A agent role name (e.g. "coder")
    pub variant_a: Option<String>,
    /// Agent-based: variant B agent role name (e.g. "engineer")
    pub variant_b: Option<String>,
    /// Agent-based: workspace path for the task (must exist)
    pub workspace_path: Option<String>,
    /// Agent-based: max turns per variant
    pub max_turns: Option<u32>,
}

/// Run a blind comparison: either between two models answering a prompt, or between
/// two agent roles running a task. The outputs are returned blinded (labelled A/B/…
/// with identities withheld) so the operator can judge without bias.
async fn run_compare_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<CompareRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if body.max_turns == Some(0) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.prompt.is_some() && body.models.is_some() {
        run_model_compare(&state, body).await
    } else if body.task.is_some() && body.variant_a.is_some() && body.variant_b.is_some() {
        run_agent_compare(&state, body).await
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

/// Legacy model-based blind comparison using OpenRouter.
async fn run_model_compare(
    state: &DaemonState,
    body: CompareRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let prompt = body.prompt.ok_or(StatusCode::BAD_REQUEST)?;
    let models = body.models.ok_or(StatusCode::BAD_REQUEST)?;
    if models.len() < 2 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let key = rightkit_llm::agent_providers::load_api_key("openrouter").unwrap_or_default();
    let base = rightkit_llm::agent_providers::find_provider("openrouter")
        .map(|p| p.base_url)
        .unwrap_or("https://openrouter.ai/api/v1");

    let mut candidates = Vec::new();
    for model in &models {
        let client = rightkit_llm::agent_providers::OpenAiCompatClient::new(&key, model, base);
        let msgs = vec![serde_json::json!({ "role": "user", "content": prompt })];
        let output = match client.complete(msgs).await {
            Ok(turn) => turn.text,
            Err(e) => format!("(model error: {e})"),
        };
        candidates.push(rightkit_agent_orchestration::coding::Candidate {
            model_id: model.clone(),
            output,
        });
    }

    let id = format!("cmp-{}", now_millis());
    let seed = now_millis() as u64;
    let comparison = rightkit_agent_orchestration::coding::BlindComparison::new(&id, &prompt, candidates, seed);
    let entries = serde_json::to_value(comparison.blinded()).unwrap_or(serde_json::Value::Null);
    state
        .comparisons
        .lock()
        .unwrap()
        .insert(id.clone(), comparison);
    Ok(Json(serde_json::json!({
        "id": id,
        "prompt": prompt,
        "entries": entries,
    })))
}

/// Agent-based blind comparison: run two agent roles on the same task and compare results.
async fn run_agent_compare(
    state: &DaemonState,
    body: CompareRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let task = body.task.ok_or(StatusCode::BAD_REQUEST)?;
    let variant_a = body.variant_a.ok_or(StatusCode::BAD_REQUEST)?;
    let variant_b = body.variant_b.ok_or(StatusCode::BAD_REQUEST)?;
    let workspace = body.workspace_path.unwrap_or_default();
    let spawn_options = rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
        max_iterations: body.max_turns,
        ..rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default()
    };

    let session_id = format!("compare-{}", now_millis());
    let _live = state.create_session(&session_id, None, &workspace, "Agent compare", None);

    let output_a = match state
        .spawn(&variant_a, &task, &session_id, spawn_options.clone())
        .await
    {
        Ok(output) => output,
        Err(e) => format!("(variant_a error: {e})"),
    };

    let output_b = match state
        .spawn(&variant_b, &task, &session_id, spawn_options)
        .await
    {
        Ok(output) => output,
        Err(e) => format!("(variant_b error: {e})"),
    };

    let candidates = vec![
        rightkit_agent_orchestration::coding::Candidate {
            model_id: variant_a.clone(),
            output: output_a,
        },
        rightkit_agent_orchestration::coding::Candidate {
            model_id: variant_b.clone(),
            output: output_b,
        },
    ];

    let id = format!("cmp-{}", now_millis());
    let seed = now_millis() as u64;
    let comparison = rightkit_agent_orchestration::coding::BlindComparison::new(&id, &task, candidates, seed);
    let entries = serde_json::to_value(comparison.blinded()).unwrap_or(serde_json::Value::Null);
    state
        .comparisons
        .lock()
        .unwrap()
        .insert(id.clone(), comparison);

    Ok(Json(serde_json::json!({
        "id": id,
        "task": task,
        "variantA": variant_a,
        "variantB": variant_b,
        "entries": entries,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeRequest {
    pub id: String,
    pub winner: String,
}

/// Record the winning label for a comparison, reveal the field, and feed the
/// pick to the audit log as model-selection evidence.
async fn judge_compare_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<JudgeRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let (outcome, mapping) = {
        let comps = lock_recover(&state.comparisons, "state.comparisons");
        let c = comps.get(&body.id).ok_or(StatusCode::NOT_FOUND)?;
        let outcome = c.judge(&body.winner).ok_or(StatusCode::BAD_REQUEST)?;
        let mapping: Vec<serde_json::Value> = c
            .reveal()
            .iter()
            .map(|(label, model)| serde_json::json!({ "label": label, "model": model }))
            .collect();
        (outcome, mapping)
    };
    lock_recover(&state.comparisons, "state.comparisons").remove(&body.id);
    state.audit.append(
        "model_comparison",
        &outcome.winner_model,
        &format!("blind winner over {}", outcome.field.join(", ")),
    );
    Ok(Json(serde_json::json!({
        "winnerModel": outcome.winner_model,
        "winnerLabel": body.winner.to_uppercase(),
        "field": outcome.field,
        "mapping": mapping,
    })))
}

const BENCHMARK_OUTPUT_LIMIT: usize = 16 * 1024;

fn bounded_benchmark_output(text: impl Into<String>) -> String {
    let text = text.into();
    if text.len() <= BENCHMARK_OUTPUT_LIMIT {
        return text;
    }
    let mut end = BENCHMARK_OUTPUT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[truncated to {} bytes for benchmark history]",
        &text[..end],
        BENCHMARK_OUTPUT_LIMIT
    )
}

pub(crate) async fn record_benchmark_history(
    state: &DaemonState,
    report: &rightkit_agent_orchestration::coding::BenchmarkReport,
) {
    record_benchmark_history_with_detail(state, report, None).await;
}

async fn record_benchmark_history_with_detail(
    state: &DaemonState,
    report: &rightkit_agent_orchestration::coding::BenchmarkReport,
    detail: Option<rightkit_agent_engine_storage::BenchmarkDetail>,
) {
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        let bid = format!("bench-{}", now_millis());
        let role2 = report.role.clone();
        let model2 = report.model.clone();
        let suite = report.benchmark_suite.clone();
        let repeats2 = report.repeats;
        let successes2 = report.successes;
        let ts = now_iso();
        let _ = tokio::task::spawn_blocking(move || {
            let s = storage2.blocking_lock();
            let _ = s.record_benchmark_with_detail(
                &bid,
                &role2,
                &model2,
                &suite,
                repeats2,
                successes2,
                &ts,
                detail.as_ref(),
            );
        })
        .await;
    }
}

#[cfg(test)]
mod compare_history_unit_tests {
    use super::*;

    #[test]
    fn bounded_benchmark_output_passes_short_text_through_unchanged() {
        let text = "cargo test passed";
        assert_eq!(bounded_benchmark_output(text), text);
    }

    #[test]
    fn bounded_benchmark_output_truncates_and_annotates_oversized_text() {
        let text = "a".repeat(BENCHMARK_OUTPUT_LIMIT + 500);
        let bounded = bounded_benchmark_output(text);
        assert!(
            bounded.len() > BENCHMARK_OUTPUT_LIMIT,
            "annotation adds bytes back"
        );
        assert!(bounded.starts_with(&"a".repeat(BENCHMARK_OUTPUT_LIMIT)));
        assert!(bounded.contains(&format!(
            "[truncated to {BENCHMARK_OUTPUT_LIMIT} bytes for benchmark history]"
        )));
    }

    #[test]
    fn bounded_benchmark_output_respects_the_exact_limit_boundary() {
        let exact = "b".repeat(BENCHMARK_OUTPUT_LIMIT);
        assert_eq!(bounded_benchmark_output(exact.clone()), exact);
    }

    #[test]
    fn bounded_benchmark_output_truncates_on_a_char_boundary_for_multibyte_text() {
        // "a" * (LIMIT - 1) puts the byte-LIMIT cut point one byte into the
        // first following 2-byte 'é', which is not a char boundary — this
        // would panic on a naive `&text[..LIMIT]` slice.
        let mut text = "a".repeat(BENCHMARK_OUTPUT_LIMIT - 1);
        text.push_str(&"é".repeat(10));
        assert!(
            !text.is_char_boundary(BENCHMARK_OUTPUT_LIMIT),
            "test setup must actually straddle a codepoint"
        );

        // Must not panic (would if it sliced mid-codepoint) and must still
        // carry the truncation annotation.
        let bounded = bounded_benchmark_output(text.clone());
        assert!(bounded.contains("truncated to"));
        assert!(bounded.starts_with(&"a".repeat(BENCHMARK_OUTPUT_LIMIT - 1)));
    }
}

pub(crate) fn verification_shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = rightkit_child_process::tokio_support::command("powershell");
        cmd.arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = rightkit_child_process::tokio_support::command("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Adapter-backed verification launch for live benchmark owners. The legacy
/// command builder remains for compatibility tests; production callers should
/// use this managed path so cancellation/reaping stays with execution host.
pub(crate) async fn managed_verification_shell_command(
    command: &str,
    workspace_root: &std::path::Path,
) -> Result<(i32, String), String> {
    #[cfg(test)]
    {
        let mut process = verification_shell_command(command);
        let output = process
            .current_dir(workspace_root)
            .output()
            .await
            .map_err(|error| error.to_string())?;
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&combined).into_owned(),
        ))
    }
    #[cfg(not(test))]
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    #[cfg(not(test))]
    {
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: workspace_root.display().to_string(),
            sandbox_profile: Some("benchmark-verification".into()),
            sandbox_required: true,
            network_required: false,
        };
        let (program, argv) = if cfg!(windows) {
            (
                "powershell",
                vec![
                    "-NoLogo",
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    command,
                ],
            )
        } else {
            ("sh", vec!["-c", command])
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: program.into(),
            argv: argv.into_iter().map(String::from).collect(),
            cwd: context.workspace_root.clone(),
            context,
            env: Vec::new(),
        };
        let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut stdout = child
            .take_stdout()
            .ok_or_else(|| "verification stdout unavailable".to_string())?;
        let mut stderr = child
            .take_stderr()
            .ok_or_else(|| "verification stderr unavailable".to_string())?;
        let (stdout_result, stderr_result) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
                    .await
                    .map(|_| bytes)
            },
            async {
                let mut bytes = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes).await
            }
        );
        let bytes = stdout_result.map_err(|e| e.to_string())?;
        stderr_result.map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| format!("{e:?}"))?;
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }
}

pub(crate) struct ManagedCommandSpec {
    pub program: String,
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub env: Vec<(String, String)>,
}

pub(crate) async fn run_managed_command(spec: ManagedCommandSpec) -> Result<(i32, String), String> {
    #[cfg(test)]
    let adapter = rightkit_agent_tools::coding_tools::process_launch::UnconfinedProcessLaunchAdapter;
    #[cfg(not(test))]
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: spec.cwd.display().to_string(),
        sandbox_profile: Some("benchmark-command".into()),
        sandbox_required: true,
        network_required: false,
    };
    let launch = rightkit_sandbox::execution::CommandLaunchSpec {
        program: spec.program,
        argv: spec.argv,
        cwd: context.workspace_root.clone(),
        context,
        env: spec.env,
    };
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, launch)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut stdout = match child.take_stdout() {
        Some(stream) => stream,
        None => {
            let _ = child.cancel().await;
            return Err("managed command stdout unavailable".to_string());
        }
    };
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| "managed command stderr unavailable".to_string())?;
    let (stdout_result, stderr_result) = tokio::join!(
        async {
            let mut bytes = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
                .await
                .map(|_| bytes)
        },
        async {
            let mut bytes = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes).await
        }
    );
    let bytes = stdout_result.map_err(|e| e.to_string())?;
    stderr_result.map_err(|e| e.to_string())?;
    let status = child.wait().await.map_err(|e| format!("{e:?}"))?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}
