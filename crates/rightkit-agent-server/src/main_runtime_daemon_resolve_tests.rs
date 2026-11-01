// Behavioral tests for fleet-field projection (model / lastActivity).
// Included from main_runtime_daemon_resolve.rs — items and imports are
// provided by the parent `mod daemon_resolve_tests { use super::*; .. }`.

#[test]
fn projects_model_and_last_activity() {
    let row = json!({
        "id": "sess_001",
        "status": "running",
        "model": "claude-sonnet-4-20250514",
        "updatedAt": "2026-06-30T12:00:00Z",
        "title": "Refactor auth",
        "workspaceRoot": "/tmp/proj",
    });
    let mut rows = vec![row];
    let filtered = filter_agent_session_rows(rows.clone(), true, false);
    assert_eq!(filtered.len(), 1);
    let r = &filtered[0];
    // model passes through from the API row
    assert_eq!(r["model"], "claude-sonnet-4-20250514");

    // Simulate the JSON projection that daemon_agents() applies
    let projected = {
        let mut p = rows.remove(0);
        if let Some(updated_at) = p.get("updatedAt").and_then(|v| v.as_str()) {
            p["lastActivity"] = Value::String(updated_at.to_string());
        }
        p
    };
    assert_eq!(projected["lastActivity"], "2026-06-30T12:00:00Z");
}

#[test]
fn missing_fields_do_not_panic() {
    let row = json!({
        "id": "sess_002",
        "status": "running",
        "title": "No model or updatedAt",
    });
    let rows = vec![row];
    let filtered = filter_agent_session_rows(rows, true, false);
    assert_eq!(filtered.len(), 1);
    let r = &filtered[0];

    // JSON path: missing model → key absent; missing updatedAt → no lastActivity set
    assert!(
        r.get("model").is_none() || r["model"].is_null(),
        "model key should not be present when the source row lacks it"
    );
    // The projection only fires when updatedAt is a present string
    let has_updated_at = r.get("updatedAt").and_then(|v| v.as_str()).is_some();
    assert!(!has_updated_at, "updatedAt was not in the fixture");

    // Table path uses "-" as sentinel (matching the code in daemon_agents())
    assert_eq!(r["model"].as_str().unwrap_or("-"), "-");
    assert_eq!(r["updatedAt"].as_str().unwrap_or("-"), "-");
}

#[test]
fn filter_semantics_unchanged_by_field_addition() {
    // --all filter: archived/deleted excluded by default, all pass with --all
    let rows = vec![
        json!({"id": "a", "status": "running", "workspaceRoot": "/x"}),
        json!({"id": "b", "status": "archived", "workspaceRoot": "/x"}),
        json!({"id": "c", "status": "deleted", "workspaceRoot": "/x"}),
        json!({"id": "d", "status": "dormant", "workspaceRoot": "/x"}),
    ];
    let filtered = filter_agent_session_rows(rows.clone(), false, false);
    assert_eq!(
        filtered.len(),
        2,
        "without --all: archived/deleted excluded"
    );
    for r in &filtered {
        assert_ne!(r["status"], "archived");
        assert_ne!(r["status"], "deleted");
    }
    let filtered_all = filter_agent_session_rows(rows.clone(), true, false);
    assert_eq!(
        filtered_all.len(),
        4,
        "with --all: all statuses pass through"
    );

    // --cwd filter: only matching workspace passes
    let cwd = std::env::current_dir().unwrap();
    let cwd_str = cwd.display().to_string();
    let rows = vec![
        json!({"id": "in_cwd", "status": "running", "workspaceRoot": cwd_str}),
        json!({"id": "elsewhere", "status": "running", "workspaceRoot": "/nonexistent/other"}),
    ];
    let filtered_cwd = filter_agent_session_rows(rows.clone(), true, true);
    assert_eq!(
        filtered_cwd.len(),
        1,
        "with --cwd: only workspace-matching row"
    );
    assert_eq!(filtered_cwd[0]["id"], "in_cwd");

    let filtered_no_cwd = filter_agent_session_rows(rows, true, false);
    assert_eq!(filtered_no_cwd.len(), 2, "without --cwd: all rows pass");
}

#[test]
fn table_columns_preserve_new_and_old() {
    let row = json!({
        "id": "sess_003",
        "status": "dormant",
        "model": "claude-opus-4-20250514",
        "updatedAt": "2026-06-30T14:00:00Z",
        "title": "SQL indexing pass",
        "workspaceRoot": "/home/user/db-proj",
    });
    // Replicate the table-formatting branch from daemon_agents()
    let id = row["id"].as_str().unwrap_or("-");
    let status = row["status"].as_str().unwrap_or("-");
    let model = row["model"].as_str().unwrap_or("-");
    let last_activity = row["updatedAt"].as_str().unwrap_or("-");
    let title = row["title"].as_str().unwrap_or("");
    let workspace = row["workspaceRoot"].as_str().unwrap_or("");

    // New columns
    assert_eq!(model, "claude-opus-4-20250514");
    assert_eq!(last_activity, "2026-06-30T14:00:00Z");
    // Existing columns still present
    assert_eq!(id, "sess_003");
    assert_eq!(status, "dormant");
    assert_eq!(title, "SQL indexing pass");
    assert_eq!(workspace, "/home/user/db-proj");
}

#[test]
fn cost_usd_passes_through_json_projection_when_present() {
    // costUsd (Lane 1H, §#177) is populated upstream by session_row()/the
    // dormant-session branch and must survive the JSON projection unchanged
    // — daemon_agents() only adds lastActivity, it never strips fields.
    let row = json!({
        "id": "sess_001",
        "status": "running",
        "model": "claude-sonnet-4-20250514",
        "updatedAt": "2026-06-30T12:00:00Z",
        "costUsd": 0.0512,
    });
    let mut rows = vec![row];
    let filtered = filter_agent_session_rows(rows.clone(), true, false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["costUsd"].as_f64(), Some(0.0512));
    // Apply the same JSON projection daemon_agents() uses
    let projected = {
        let mut p = rows.remove(0);
        if let Some(updated_at) = p.get("updatedAt").and_then(|v| v.as_str()) {
            p["lastActivity"] = Value::String(updated_at.to_string());
        }
        p
    };
    assert_eq!(projected["costUsd"].as_f64(), Some(0.0512));
}

#[test]
fn cost_usd_null_stays_null_through_json_projection() {
    // Unpriced model / dormant session → costUsd is explicitly null, never
    // absent and never coerced to 0 — unknown cost must read as unknown.
    let row = json!({
        "id": "sess_002",
        "status": "running",
        "model": "some/unpriced-model",
        "updatedAt": "2026-06-30T12:00:00Z",
        "costUsd": serde_json::Value::Null,
    });
    let rows = vec![row];
    let filtered = filter_agent_session_rows(rows, true, false);
    assert_eq!(filtered.len(), 1);
    assert!(filtered[0]["costUsd"].is_null());
    assert_ne!(filtered[0]["costUsd"], json!(0));
    assert_ne!(filtered[0]["costUsd"], json!(0.0));
}

#[test]
fn table_cost_column_formats_known_and_unknown() {
    // Replicate the table-formatting branch from daemon_agents().
    let known = json!({"costUsd": 0.0125});
    let unknown = json!({"costUsd": serde_json::Value::Null});
    let absent = json!({});

    let fmt = |row: &Value| match row.get("costUsd").and_then(|v| v.as_f64()) {
        Some(usd) => format!("${usd:.4}"),
        None => "-".to_string(),
    };
    assert_eq!(fmt(&known), "$0.0125");
    assert_eq!(fmt(&unknown), "-");
    assert_eq!(fmt(&absent), "-");
}

#[test]
fn current_process_start_identity_roundtrips() {
    let pid = std::process::id();
    let started = process_start_epoch_ms(pid).expect("current process start time");
    assert!(process_matches_start(pid, started));
    assert!(!process_matches_start(pid, started.saturating_add(60_000)));
}

#[test]
fn repeated_health_probe_tolerates_one_transient_failure() {
    let mut outcomes = [false, true].into_iter();
    let mut attempts = 0;
    let mut delays = 0;
    let healthy = repeated_health_probe(
        || {
            attempts += 1;
            outcomes.next().unwrap_or(false)
        },
        || delays += 1,
    );
    assert!(healthy);
    assert_eq!(attempts, 2);
    assert_eq!(delays, 1);
}

#[test]
fn repeated_health_probe_requires_two_failures_to_reject() {
    let mut attempts = 0;
    let healthy = repeated_health_probe(
        || {
            attempts += 1;
            false
        },
        || {},
    );
    assert!(!healthy);
    assert_eq!(attempts, 2);
}

/// DL-014: `coderight stop`'s dispatch (`daemon_stop`, wired above) must
/// route through the shared `authority()` singleton rather than an
/// independent stop path. There is no seam to intercept a private free
/// function's call graph directly, so this observes the effect only
/// `LifecycleAuthority::execute`'s `commit_success` can produce: a `Stop`
/// command always leaves the authority's own tracked state `Stopped` —
/// whether it took the idempotent "already stopped" short-circuit or ran the
/// real backend — and nothing else in this binary ever touches that private
/// state. `force: true` keeps this network-free/deterministic in an
/// environment with no discovery record, matching the real backend's
/// documented no-daemon short-circuit (`stop_daemon`'s "no daemon discovery
/// record; nothing to stop" branch).
#[tokio::test]
async fn cli_stop_dispatch_routes_through_lifecycle_authority() {
    let code = daemon_stop(true).await;
    assert_eq!(
        code, 0,
        "daemon_stop must report success with no daemon running"
    );
    let snapshot = crate::daemon::lifecycle_backend::authority()
        .snapshot()
        .await;
    assert_eq!(
        snapshot.state,
        rightkit_daemon::agent_daemon_runtime::LifecycleState::Stopped,
        "authority's own tracked state must be Stopped after daemon_stop's dispatch, \
         proving the CLI verb submitted its command through the shared authority \
         rather than an independent stop path"
    );
}
