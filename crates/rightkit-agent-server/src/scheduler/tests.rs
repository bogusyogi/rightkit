use super::delivery::{telegram_chat_id, truncate_telegram_text};
use super::execution::{
    fire_schedule, mark_local_execution_resolved, record_schedule_failure, schedule_run_event,
};
use super::lifecycle::scheduled_workspace_roots;
use super::persistence::{
    append_run_event, load_run_state, replay_causal_receipts, runs_path, schedules_path,
};
use rightkit_workflow::control_plane::trigger::{CausalReceipt, CausalReceiptKind};
use rightkit_workflow::control_plane::waits::TaskIsolationV1;
use serde_json::json;

#[test]
fn bot_profile_v1_validates_and_fresh_task_state_is_unique() {
    let profile = super::model::BotProfileV1 {
        schema_version: 1,
        id: "reviewer".into(),
        name: "Reviewer".into(),
        role_pack: "reviewer.v1".into(),
        model: "catalog/reviewer".into(),
        project: Some("current".into()),
        glyph: "R".into(),
        avatar: None,
        description: "review".into(),
        version: 1,
        fallback_models: vec![],
        instructions: "review".into(),
        memory_policy: "none".into(),
        default_project: None,
        task_templates: vec![],
        tools: vec!["read_file".into()],
        tool_grants: vec![],
        permissions: vec!["fs.read".into()],
        browser: super::model::BrowserPolicy {
            mode: "disabled".into(),
            profile: None,
            allow_network: false,
        },
        budget: super::model::BotBudget::default(),
        verifier: super::model::VerifierBinding {
            role_pack: Some("verifier.v1".into()),
            model: None,
            required: true,
        },
        triggers: vec![],
        concurrency: "queue-one".into(),
        concurrency_cap: 1,
        enabled: true,
    };
    profile.validate().expect("profile");
    let isolation = TaskIsolationV1::fresh("schedule-reviewer-1").expect("isolation");
    assert_ne!(
        isolation.transcript_id,
        TaskIsolationV1::fresh("schedule-reviewer-2")
            .unwrap()
            .transcript_id
    );
}

#[test]
fn schedule_failures_carry_trigger_receipt_and_isolation() {
    let dir = test_dir("schedule_failures_carry_trigger_receipt_and_isolation");
    let entry = base_entry("fails-fast");
    let outcome = record_schedule_failure(
        &dir,
        &entry,
        "schedule-fails-fast-1",
        "rejected before submission",
    );
    assert_eq!(outcome.status, "failed");
    let chains = replay_causal_receipts(&dir, "fails-fast").expect("replay");
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].len(), 1);
    assert!(matches!(
        &chains[0][0].kind,
        CausalReceiptKind::Trigger { trigger_id } if trigger_id == "fails-fast"
    ));
    let state = load_run_state(&dir).expect("run state");
    let event = state.history.last().expect("failure row");
    let parsed: serde_json::Value = serde_json::from_str(&event.message).expect("structured");
    assert_eq!(parsed["taskIsolation"]["taskId"], "schedule-fails-fast-1");
    let receipts = parsed["causalReceipts"].as_array().expect("receipts");
    assert_eq!(receipts.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scheduler_runtime_replays_causal_receipts_from_run_history() {
    let dir = test_dir("scheduler_runtime_replays_causal_receipts_from_run_history");
    let entry = super::model::ScheduleEntry {
        id: "causal".into(),
        cron_expr: "* * * * *".into(),
        prompt: "causal".into(),
        time_zone: "UTC".into(),
        skills: Vec::new(),
        workdir: None,
        toolset: None,
        execution_backend: None,
        delivery: Vec::new(),
        script_allowlist: Vec::new(),
        mode: "agent".into(),
        paused: false,
        source_recipe_id: None,
        recipe_params: Default::default(),
        max_runtime_seconds: None,
    };
    let chain = vec![
        CausalReceipt {
            id: "t".into(),
            sequence: 0,
            kind: CausalReceiptKind::Trigger {
                trigger_id: "causal".into(),
            },
            created_at_unix_ms: 1,
        },
        CausalReceipt {
            id: "r".into(),
            sequence: 1,
            kind: CausalReceiptKind::Run {
                run_id: "r".into(),
                trigger_receipt_id: "t".into(),
            },
            created_at_unix_ms: 1,
        },
        CausalReceipt {
            id: "c".into(),
            sequence: 2,
            kind: CausalReceiptKind::ChildTask {
                task_id: "c".into(),
                run_receipt_id: "r".into(),
            },
            created_at_unix_ms: 1,
        },
        CausalReceipt {
            id: "x".into(),
            sequence: 3,
            kind: CausalReceiptKind::Result {
                result_id: "x".into(),
                task_receipt_id: "c".into(),
            },
            created_at_unix_ms: 1,
        },
    ];
    let mut event = schedule_run_event(&entry, 1, "completed", "schedule-causal-1", "done");
    event.message = super::persistence::causal_message("done", &chain);
    append_run_event(&dir, event).expect("persist");
    assert_eq!(replay_causal_receipts(&dir, "causal").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scheduler_runtime_supports_once_interval_monitor_specs_and_queue_release() {
    assert!(matches!(
        super::model::ScheduleSpec::from_legacy("once:60000", "UTC").unwrap(),
        super::model::ScheduleSpec::Once { at_unix_ms: 60000 }
    ));
    assert!(matches!(
        super::model::ScheduleSpec::from_legacy("interval:30:1000", "UTC").unwrap(),
        super::model::ScheduleSpec::Interval {
            every_seconds: 30,
            start_unix_ms: Some(1000)
        }
    ));
    assert!(matches!(
        super::model::ScheduleSpec::from_legacy("monitor:deploy:prod", "UTC").unwrap(),
        super::model::ScheduleSpec::Monitor { ref topic, ref filter }
            if topic == "deploy" && filter.as_deref() == Some("prod")
    ));

    let dir = test_dir("scheduler_runtime_supports_queue_release");
    write_schedule(&dir, "queued", "* * * * *", "queued");
    let mut state = super::persistence::load_run_state(&dir).unwrap();
    state.last_claimed_minute.insert("queued".into(), 7);
    state.history.push(schedule_run_event(
        &base_entry("queued"),
        7,
        "queued",
        "schedule-queued",
        "queued occurrence",
    ));
    super::persistence::save_run_state(&dir, &state).unwrap();
    assert!(super::persistence::release_queued_occurrence(&dir, "queued").unwrap());
    let state = super::persistence::load_run_state(&dir).unwrap();
    assert!(!state.last_claimed_minute.contains_key("queued"));
    assert_eq!(state.history[0].status, "dequeued");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scheduler_runtime_once_is_single_claim_and_idempotency_is_exact() {
    let dir = test_dir("scheduler_runtime_once_is_single_claim");
    write_schedule(&dir, "once", "once:60000", "once");
    assert_eq!(
        super::persistence::claim_due_schedules(&dir, 1)
            .unwrap()
            .len(),
        1
    );
    assert!(super::persistence::claim_due_schedules(&dir, 2)
        .unwrap()
        .is_empty());
    let retry = test_dir("scheduler_runtime_once_bounded_retry");
    write_schedule(&retry, "once-retry", "once:60000", "once-retry");
    let path = schedules_path(&retry);
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["once-retry"]["lifecycle"] =
        json!({"paused":false,"overlap":"skip","misfire":"skip","retryLimit":1});
    std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&retry, 1)
            .unwrap()
            .len(),
        1
    );
    let mut state = super::persistence::load_run_state(&retry).unwrap();
    state.history.push(schedule_run_event(
        &base_entry("once-retry"),
        1,
        "failed",
        "once-retry-1",
        "failed",
    ));
    super::persistence::save_run_state(&retry, &state).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&retry, 2)
            .unwrap()
            .len(),
        1
    );

    let idem = test_dir("scheduler_runtime_idempotency_is_exact");
    write_schedule(&idem, "idem", "* * * * *", "idem");
    let path = schedules_path(&idem);
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["idem"]["lifecycle"] = json!({"paused":false,"overlap":"skip","misfire":"skip","idempotencyKey":"evt-1","retryLimit":0});
    std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&idem, 10)
            .unwrap()
            .len(),
        1
    );
    let mut state = super::persistence::load_run_state(&idem).unwrap();
    state.last_claimed_minute.clear();
    super::persistence::save_run_state(&idem, &state).unwrap();
    assert!(super::persistence::claim_due_schedules(&idem, 10)
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(retry);
    let _ = std::fs::remove_dir_all(idem);
}

#[test]
fn scheduler_runtime_overlap_and_bounded_parallel_respect_active_runs() {
    for (name, policy, expected_queued) in [("skip", "skip", false), ("queue", "queue-one", true)] {
        let dir = test_dir(&format!("scheduler_runtime_overlap_{name}"));
        write_schedule(&dir, "active", "* * * * *", "active");
        let path = schedules_path(&dir);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        raw["active"]["lifecycle"] =
            json!({"paused":false,"overlap":policy,"misfire":"skip","retryLimit":0});
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
        assert_eq!(
            super::persistence::claim_due_schedules(&dir, 10)
                .unwrap()
                .len(),
            1
        );
        let mut state = super::persistence::load_run_state(&dir).unwrap();
        state.history.push(schedule_run_event(
            &base_entry("active"),
            10,
            "submitted",
            "live-10",
            "active",
        ));
        super::persistence::save_run_state(&dir, &state).unwrap();
        let next = super::persistence::claim_due_schedules(&dir, 11).unwrap();
        assert!(next.is_empty());
        if expected_queued {
            assert!(super::persistence::load_run_state(&dir)
                .unwrap()
                .history
                .iter()
                .any(|event| event.status == "queued"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    let dir = test_dir("scheduler_runtime_bounded_parallel_cap");
    write_schedule(&dir, "bounded", "* * * * *", "bounded");
    let path = schedules_path(&dir);
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["bounded"]["lifecycle"] =
        json!({"paused":false,"overlap":"bounded-parallel","misfire":"skip","retryLimit":0});
    std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&dir, 10)
            .unwrap()
            .len(),
        1
    );
    let mut state = super::persistence::load_run_state(&dir).unwrap();
    state.history.push(schedule_run_event(
        &base_entry("bounded"),
        10,
        "submitted",
        "live-10",
        "active",
    ));
    super::persistence::save_run_state(&dir, &state).unwrap();
    assert!(super::persistence::claim_due_schedules(&dir, 11)
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(dir);

    let dir = test_dir("scheduler_runtime_bounded_parallel_terminal_clears");
    write_schedule(&dir, "bounded-terminal", "* * * * *", "bounded-terminal");
    let path = schedules_path(&dir);
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["bounded-terminal"]["lifecycle"] =
        json!({"paused":false,"overlap":"bounded-parallel","misfire":"skip","retryLimit":0});
    std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&dir, 10)
            .unwrap()
            .len(),
        1
    );
    let mut state = super::persistence::load_run_state(&dir).unwrap();
    state.history.push(schedule_run_event(
        &base_entry("bounded-terminal"),
        10,
        "submitted",
        "same-session",
        "active",
    ));
    state.history.push(schedule_run_event(
        &base_entry("bounded-terminal"),
        12,
        "completed",
        "same-session",
        "done",
    ));
    super::persistence::save_run_state(&dir, &state).unwrap();
    assert_eq!(
        super::persistence::claim_due_schedules(&dir, 13)
            .unwrap()
            .len(),
        1
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scheduler_runtime_misfire_policy_is_explicit_and_bounded() {
    for (policy, expected) in [("skip", 0), ("coalesce", 1), ("catch_up", 1)] {
        let dir = test_dir(&format!("scheduler_runtime_misfire_{policy}"));
        write_schedule(&dir, "interval", "interval:120:0", "interval");
        let path = schedules_path(&dir);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        raw["interval"]["lifecycle"] =
            json!({"paused":false,"overlap":"skip","misfire":policy,"retryLimit":0});
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
        assert_eq!(
            super::persistence::claim_due_schedules(&dir, 3)
                .unwrap()
                .len(),
            expected
        );
        assert!(super::persistence::claim_due_schedules(&dir, 3)
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
use super::time::cron_matches_in_time_zone;
use super::*;
use crate::api::DaemonState;
use rightkit_agent_engine_protocol::{default_job_mode, ScheduleRunEvent};
use rightkit_agent_engine_runtime_middleware::MiddlewarePhase;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

include!("test_parts/support.rs");
include!("test_parts/cron.rs");
include!("test_parts/claim.rs");
include!("test_parts/runtime_delivery.rs");
include!("test_parts/run_records.rs");
