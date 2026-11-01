async fn run_skill_proposal_benchmark_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillProposalBenchmarkRunRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let id = normalize_skill_slug(&id).ok_or(StatusCode::BAD_REQUEST)?;
    let proposal = {
        let path = skill_governance_store_path(&state.skill_dirs)?;
        let store = load_skill_governance_store(&path);
        store
            .proposals
            .iter()
            .find(|proposal| proposal.id == id)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let gate_mode = body
        .gate_mode
        .or_else(|| proposal_gate_mode(&state.skill_dirs, &proposal.slug))
        .unwrap_or(rightkit_agent_host_traits::skill_host::SkillGateMode::Hard);
    let threshold = body
        .threshold
        .unwrap_or_else(|| default_skill_benchmark_threshold(gate_mode));
    let kind = body
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("role");

    let (benchmark_id, score, runner_passed, runner_payload) = match kind {
        "role" => {
            let session_id = required_trimmed(body.session_id.as_deref())?;
            let role = required_trimmed(body.role.as_deref())?;
            let goal = required_trimmed(body.goal.as_deref())?;
            if state.get_session(&session_id).is_none() {
                state.create_session(&session_id, None, "", "Skill benchmark", None);
            }
            let model = body.model.clone().unwrap_or_else(|| {
                state
                    .agent_registry
                    .get(&role)
                    .map(|descriptor| descriptor.model_id.clone())
                    .unwrap_or_default()
            });
            if model.trim().is_empty() {
                return Err(StatusCode::BAD_REQUEST);
            }
            let repeats = body.repeats.unwrap_or(1).max(1);
            let report = state
                .benchmark_role(&role, &model, &goal, repeats, &session_id)
                .await;
            crate::api_benchmarks::record_benchmark_history(&state, &report).await;
            let score = report.success_rate();
            let benchmark_id = body
                .benchmark_id
                .unwrap_or_else(|| format!("role:{}:{}:{}", role, model, report.benchmark_suite));
            (
                benchmark_id,
                score,
                report.successes == report.repeats && report.repeats > 0,
                serde_json::json!({
                    "runner": "runtimeBenchmark",
                    "role": report.role,
                    "model": report.model,
                    "suite": report.benchmark_suite,
                    "repeats": report.repeats,
                    "successes": report.successes,
                    "successRate": score,
                    "goal": goal,
                    "sessionId": session_id,
                }),
            )
        }
        "terminalBench" | "terminal-bench" | "tbench" => {
            let session_id = required_trimmed(body.session_id.as_deref())?;
            let task_id = required_trimmed(body.task_id.as_deref())?;
            let agent = required_trimmed(body.agent.as_deref().or(body.role.as_deref()))?;
            let report = crate::api_benchmarks::run_terminal_bench_task(
                &state,
                crate::api_benchmarks::TerminalBenchRunRequest {
                    task_id: task_id.clone(),
                    agent: agent.clone(),
                    session_id: session_id.clone(),
                },
            )
            .await?;
            let score = report["successRate"].as_f64().unwrap_or(0.0);
            let runner_passed = report["passed"].as_bool().unwrap_or(false);
            let benchmark_id = body
                .benchmark_id
                .unwrap_or_else(|| format!("terminal-bench:{task_id}:{agent}"));
            (
                benchmark_id,
                score,
                runner_passed,
                serde_json::json!({
                    "runner": "terminalBench",
                    "report": report,
                }),
            )
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let passed = skill_benchmark_gate_passed(gate_mode, score, runner_passed, threshold);
    let body = SkillBenchmarkRequest {
        slug: Some(proposal.slug.clone()),
        benchmark_id,
        gate_mode: Some(gate_mode),
        score_continuous: Some(score),
        passed,
        verdict: Some(serde_json::json!({
            "proposalId": id,
            "slug": proposal.slug,
            "gateMode": gate_mode,
            "threshold": threshold,
            "scoreContinuous": score,
            "passed": passed,
            "runnerPassed": runner_passed,
            "runnerPayload": runner_payload,
        })),
        reason: body
            .reason
            .or_else(|| Some("recorded from benchmark runner".to_string())),
    };
    let recorded = record_skill_proposal_benchmark(&state, &id, body)?;
    Ok(Json(serde_json::json!({
        "ran": true,
        "gateMode": gate_mode,
        "threshold": threshold,
        "scoreContinuous": score,
        "runnerPassed": runner_passed,
        "record": recorded,
    })))
}
