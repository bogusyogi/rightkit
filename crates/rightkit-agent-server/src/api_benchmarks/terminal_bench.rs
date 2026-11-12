/// Get the RightKit internal task pack used for promotion targeting.
async fn get_terminal_bench_tasks(
    State(_state): State<DaemonState>,
) -> Json<Vec<serde_json::Value>> {
    use rightkit_agent_orchestration::coding::TbenchCatalog;
    let tasks = TbenchCatalog::all_tasks();
    let json: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "category": format!("{:?}", t.category).to_lowercase(),
                "difficulty": format!("{:?}", t.difficulty).to_lowercase(),
                "prompt": t.prompt,
                "setupFiles": t.setup_files.iter()
                    .map(|(name, content)| serde_json::json!({ "name": name, "content": content }))
                    .collect::<Vec<_>>(),
                "verifyCommand": t.verify_command,
                "expectedPassRate": t.expected_pass_rate,
            })
        })
        .collect();
    Json(json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalBenchRunRequest {
    /// Task ID (e.g. "tb-001")
    pub task_id: String,
    /// Agent role to run the task (e.g. "coder")
    pub agent: String,
    /// Session ID for context
    pub session_id: String,
}

/// Run a RightKit internal task-pack task against a named agent and return a benchmark report.
/// The handler creates an isolated workspace, invokes the requested agent role,
/// verifies the resulting files, and records the report in benchmark history.
async fn run_terminal_bench_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<TerminalBenchRunRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    run_terminal_bench_task(&state, body).await.map(Json)
}

pub(crate) async fn run_terminal_bench_task(
    state: &DaemonState,
    body: TerminalBenchRunRequest,
) -> Result<serde_json::Value, StatusCode> {
    use rightkit_agent_coding_tools::SubagentSpawner;
    use rightkit_agent_orchestration::coding::TbenchCatalog;

    let tasks = TbenchCatalog::all_tasks();
    let task = tasks
        .iter()
        .find(|t| t.id == body.task_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let model_id = state
        .agent_registry
        .get(&body.agent)
        .ok_or(StatusCode::BAD_REQUEST)?
        .model_id
        .clone();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let workspace_root =
        std::env::temp_dir().join(format!("tbench-{}-{}", body.task_id, timestamp));

    std::fs::create_dir_all(&workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for (name, content) in &task.setup_files {
        let path = workspace_root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    let bench_session_id = format!("tbench-sess-{timestamp}");
    let parent = state
        .get_session(&body.session_id)
        .map(|_| body.session_id.clone());
    let _live = state.create_session(
        &bench_session_id,
        Some(&body.agent),
        &workspace_root.to_string_lossy(),
        &format!("RightKit task pack {}", body.task_id),
        parent.as_deref(),
    );

    let agent_output = match state
        .spawn(
            &body.agent,
            &task.prompt,
            &bench_session_id,
            rightkit_agent_coding_tools::SubagentSpawnOptions::default(),
        )
        .await
    {
        Ok(out) => out,
        Err(e) => format!("(agent error: {e})"),
    };

    let (status, verify_output) =
        super::managed_verification_shell_command(&task.verify_command, &workspace_root)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let success = status == 0;
    let verify_error = String::new();

    let cleanup_workspace = workspace_root.clone();
    tokio::spawn(async move {
        let _ = std::fs::remove_dir_all(cleanup_workspace);
    });

    let report = rightkit_agent_orchestration::coding::BenchmarkReport::new(
        &body.agent,
        &model_id,
        1,
        if success { 1 } else { 0 },
    )
    .with_suite(TbenchCatalog::suite_id());
    let detail = rightkit_agent_engine_storage::BenchmarkDetail {
        task_id: Some(body.task_id.clone()),
        passed: Some(success),
        agent_output: Some(bounded_benchmark_output(agent_output.clone())),
        verify_output: Some(bounded_benchmark_output(verify_output.clone())),
        verify_error: Some(bounded_benchmark_output(verify_error.clone())),
    };
    record_benchmark_history_with_detail(state, &report, Some(detail)).await;

    Ok(serde_json::json!({
        "role": report.role,
        "model": report.model,
        "repeats": report.repeats,
        "successes": report.successes,
        "successRate": report.success_rate(),
        "benchmarkSuite": report.benchmark_suite,
        "taskId": body.task_id,
        "passed": success,
        "agentOutput": agent_output,
        "verifyOutput": verify_output,
        "verifyError": verify_error,
    }))
}
