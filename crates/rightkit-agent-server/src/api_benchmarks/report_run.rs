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
        rightkit_agent_orchestration::coding::PromotionDecision::Promote {
            role,
            from_model,
            to_model,
            delta,
        } => serde_json::json!({
            "promoted": true, "role": role, "fromModel": from_model,
            "toModel": to_model, "delta": delta,
        }),
        rightkit_agent_orchestration::coding::PromotionDecision::Reject { reason } => {
            serde_json::json!({
                "promoted": false, "reason": reason,
            })
        }
    };
    Ok(Json(json))
}

async fn get_benchmarks(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(benchmark_history_json(&state).await)
}

async fn get_benchmark_proof(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(benchmark_proof_json(&state).await)
}

async fn load_benchmark_rows(
    state: &DaemonState,
) -> Vec<rightkit_agent_engine_storage::BenchmarkRow> {
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
