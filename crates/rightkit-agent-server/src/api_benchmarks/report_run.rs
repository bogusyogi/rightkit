#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkBody {
    pub session_id: String,
    pub role: String,
    /// Model to benchmark; defaults to the role's current (champion) model.
    #[serde(default)]
    pub model: Option<String>,
    pub goal: String,
    #[serde(default = "default_repeats")]
    pub repeats: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteBody {
    pub session_id: String,
    pub role: String,
    pub challenger_model: String,
    pub goal: String,
    #[serde(default = "default_repeats")]
    pub repeats: u32,
}

fn default_repeats() -> u32 {
    3
}

/// Run a role benchmark (k repeats, each verified) and return the report.
/// Synchronous: benchmarks make several model calls, so this can take a while.
async fn run_benchmark_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<BenchmarkBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    state
        .get_session(&body.session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let model = body.model.clone().unwrap_or_else(|| {
        state
            .agent_registry
            .get(&body.role)
            .map(|d| d.model_id.clone())
            .unwrap_or_default()
    });
    let report = state
        .benchmark_role(
            &body.role,
            &model,
            &body.goal,
            body.repeats,
            &body.session_id,
        )
        .await;
    record_benchmark_history(&state, &report).await;
    Ok(Json(serde_json::json!({
        "role": report.role,
        "model": report.model,
        "repeats": report.repeats,
        "successes": report.successes,
        "successRate": report.success_rate(),
    })))
}

/// Run a champion/challenger comparison; on a clear win, persist the promoted
/// model. Returns the decision.
async fn run_promotion_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<PromoteBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    state
        .get_session(&body.session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let decision = state
        .run_promotion(
            &body.role,
            &body.challenger_model,
            &body.goal,
            body.repeats,
            &body.session_id,
        )
        .await;
    let json = match decision {
        rightkit_agent_orchestration::PromotionDecision::Promote {
            role,
            from_model,
            to_model,
            delta,
        } => serde_json::json!({
            "promoted": true, "role": role, "fromModel": from_model,
            "toModel": to_model, "delta": delta,
        }),
        rightkit_agent_orchestration::PromotionDecision::Reject { reason } => serde_json::json!({
            "promoted": false, "reason": reason,
        }),
    };
    Ok(Json(json))
}

async fn get_benchmarks(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(benchmark_history_json(&state).await)
}

async fn get_benchmark_proof(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(benchmark_proof_json(&state).await)
}

async fn load_benchmark_rows(state: &DaemonState) -> Vec<rightkit_agent_engine_storage::BenchmarkRow> {
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        tokio::task::spawn_blocking(move || {
            let s = storage2.blocking_lock();
            s.list_benchmarks().unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    } else {
        vec![]
    }
}

pub(crate) async fn benchmark_history_json(state: &DaemonState) -> serde_json::Value {
    let rows = load_benchmark_rows(state).await;
    let json: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "role": r.role,
                "model": r.model,
                "suite": r.suite,
                "repeats": r.repeats,
                "successes": r.successes,
                "successRate": if r.repeats > 0 { r.successes as f64 / r.repeats as f64 } else { 0.0 },
                "taskId": r.task_id,
                "passed": r.passed,
                "agentOutput": r.agent_output,
                "verifyOutput": r.verify_output,
                "verifyError": r.verify_error,
                "createdAt": r.created_at,
            })
        })
        .collect();
    serde_json::json!(json)
}

pub(crate) async fn benchmark_proof_json(state: &DaemonState) -> serde_json::Value {
    let rows = load_benchmark_rows(state).await;
    benchmark_proof_from_rows(&rows)
}

pub(crate) fn benchmark_proof_from_rows(
    rows: &[rightkit_agent_engine_storage::BenchmarkRow],
) -> serde_json::Value {
    let mut suite_totals: std::collections::BTreeMap<String, (u32, u32, u32)> =
        std::collections::BTreeMap::new();
    let mut role_model_totals: std::collections::BTreeMap<String, (String, String, u32, u32, u32)> =
        std::collections::BTreeMap::new();
    let mut terminal_reports = 0_u32;
    let mut terminal_passed = 0_u32;

    let mut total_repeats = 0_u32;
    let mut total_successes = 0_u32;
    for row in rows {
        total_repeats = total_repeats.saturating_add(row.repeats);
        total_successes = total_successes.saturating_add(row.successes);

        let suite_entry = suite_totals.entry(row.suite.clone()).or_insert((0, 0, 0));
        suite_entry.0 = suite_entry.0.saturating_add(1);
        suite_entry.1 = suite_entry.1.saturating_add(row.repeats);
        suite_entry.2 = suite_entry.2.saturating_add(row.successes);

        let role_model_key = format!("{}\u{1f}{}", row.role, row.model);
        let role_model_entry = role_model_totals.entry(role_model_key).or_insert((
            row.role.clone(),
            row.model.clone(),
            0,
            0,
            0,
        ));
        role_model_entry.2 = role_model_entry.2.saturating_add(1);
        role_model_entry.3 = role_model_entry.3.saturating_add(row.repeats);
        role_model_entry.4 = role_model_entry.4.saturating_add(row.successes);

        if row.task_id.is_some() {
            terminal_reports = terminal_reports.saturating_add(1);
            if row
                .passed
                .unwrap_or(row.successes >= row.repeats && row.repeats > 0)
            {
                terminal_passed = terminal_passed.saturating_add(1);
            }
        }
    }

    let suites: Vec<serde_json::Value> = suite_totals
        .into_iter()
        .map(|(suite, (reports, repeats, successes))| {
            json!({
                "suite": suite,
                "reports": reports,
                "repeats": repeats,
                "successes": successes,
                "successRate": ratio(successes, repeats),
            })
        })
        .collect();

    let role_models: Vec<serde_json::Value> = role_model_totals
        .into_values()
        .map(|(role, model, reports, repeats, successes)| {
            json!({
                "role": role,
                "model": model,
                "reports": reports,
                "repeats": repeats,
                "successes": successes,
                "successRate": ratio(successes, repeats),
            })
        })
        .collect();

    json!({
        "generatedAt": now_iso(),
        "totalReports": rows.len(),
        "totalRepeats": total_repeats,
        "totalSuccesses": total_successes,
        "successRate": ratio(total_successes, total_repeats),
        "suites": suites,
        "roleModels": role_models,
        "terminalBench": {
            "reports": terminal_reports,
            "passed": terminal_passed,
            "passRate": ratio(terminal_passed, terminal_reports),
        }
    })
}

fn ratio(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod report_run_unit_tests {
    use super::*;

    fn row(
        role: &str,
        model: &str,
        suite: &str,
        repeats: u32,
        successes: u32,
        task_id: Option<&str>,
        passed: Option<bool>,
    ) -> rightkit_agent_engine_storage::BenchmarkRow {
        rightkit_agent_engine_storage::BenchmarkRow {
            id: format!("bench-{role}-{model}"),
            role: role.to_string(),
            model: model.to_string(),
            suite: suite.to_string(),
            repeats,
            successes,
            task_id: task_id.map(str::to_string),
            passed,
            agent_output: None,
            verify_output: None,
            verify_error: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn ratio_guards_against_division_by_zero() {
        assert_eq!(ratio(0, 0), 0.0);
        assert_eq!(ratio(3, 0), 0.0);
        assert_eq!(ratio(1, 2), 0.5);
    }

    #[test]
    fn benchmark_proof_from_rows_is_empty_for_no_rows() {
        let proof = benchmark_proof_from_rows(&[]);
        assert_eq!(proof["totalReports"], 0);
        assert_eq!(proof["totalRepeats"], 0);
        assert_eq!(proof["successRate"], 0.0);
        assert_eq!(proof["suites"].as_array().unwrap().len(), 0);
        assert_eq!(proof["terminalBench"]["reports"], 0);
        assert_eq!(proof["terminalBench"]["passRate"], 0.0);
    }

    #[test]
    fn benchmark_proof_from_rows_aggregates_suites_role_models_and_terminal_bench() {
        let rows = vec![
            row(
                "coder",
                "model-a",
                "suite-x",
                3,
                2,
                Some("task-1"),
                Some(true),
            ),
            row("coder", "model-a", "suite-x", 3, 1, None, None),
            row("coder", "model-b", "suite-y", 4, 4, Some("task-2"), None),
        ];
        let proof = benchmark_proof_from_rows(&rows);

        assert_eq!(proof["totalReports"], 3);
        assert_eq!(proof["totalRepeats"], 10);
        assert_eq!(proof["totalSuccesses"], 7);
        assert_eq!(proof["successRate"], 0.7);

        let suites = proof["suites"].as_array().unwrap();
        assert_eq!(suites.len(), 2, "suite-x and suite-y");
        let suite_x = suites.iter().find(|s| s["suite"] == "suite-x").unwrap();
        assert_eq!(suite_x["reports"], 2);
        assert_eq!(suite_x["repeats"], 6);
        assert_eq!(suite_x["successes"], 3);

        let role_models = proof["roleModels"].as_array().unwrap();
        assert_eq!(role_models.len(), 2, "distinct role+model pairs");
        let coder_a = role_models
            .iter()
            .find(|rm| rm["role"] == "coder" && rm["model"] == "model-a")
            .unwrap();
        assert_eq!(coder_a["reports"], 2);
        assert_eq!(coder_a["repeats"], 6);
        assert_eq!(coder_a["successes"], 3);

        // Only rows with a task_id count as terminal-bench reports; the second
        // row (task_id: None) does not.
        assert_eq!(proof["terminalBench"]["reports"], 2);
        // task-1 explicitly passed=true; task-2 has passed=None and falls back
        // to `successes >= repeats && repeats > 0`, which holds (4 >= 4).
        assert_eq!(proof["terminalBench"]["passed"], 2);
        assert_eq!(proof["terminalBench"]["passRate"], 1.0);
    }

    #[test]
    fn benchmark_proof_from_rows_falls_back_to_success_ratio_when_passed_is_absent() {
        let rows = vec![row(
            "coder",
            "model-a",
            "suite-x",
            3,
            2,
            Some("task-1"),
            None,
        )];
        let proof = benchmark_proof_from_rows(&rows);
        // successes (2) < repeats (3) with passed=None => not counted as passed.
        assert_eq!(proof["terminalBench"]["reports"], 1);
        assert_eq!(proof["terminalBench"]["passed"], 0);
    }
}
