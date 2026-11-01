#[test]
fn claim_due_schedules_prevents_duplicate_same_minute() {
    let dir = test_dir("claim_due_schedules_prevents_duplicate_same_minute");
    write_schedule(&dir, "daily", "* * * * *", "ping");

    let first = claim_due_schedules(&dir, 42).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].prompt, "ping");

    let second = claim_due_schedules(&dir, 42).unwrap();
    assert!(second.is_empty(), "same minute must not fire twice");

    let third = claim_due_schedules(&dir, 43).unwrap();
    assert_eq!(third.len(), 1, "next due minute can fire again");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn claim_due_schedules_skips_paused_jobs() {
    let dir = test_dir("claim_due_schedules_skips_paused_jobs");
    write_schedule(&dir, "daily", "* * * * *", "ping");
    let path = schedules_path(&dir);
    let mut schedules: HashMap<String, ScheduleEntry> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    schedules.get_mut("daily").unwrap().paused = true;
    std::fs::write(&path, serde_json::to_string_pretty(&schedules).unwrap()).unwrap();

    let due = claim_due_schedules(&dir, 42).unwrap();

    assert!(due.is_empty());
    assert!(!runs_path(&dir).exists());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn claim_due_schedules_records_claim_history() {
    let dir = test_dir("claim_due_schedules_records_claim_history");
    write_schedule(&dir, "daily", "* * * * *", "ping");
    let path = schedules_path(&dir);
    let mut schedules: HashMap<String, ScheduleEntry> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = schedules.get_mut("daily").unwrap();
    entry.mode = "scriptOnly".to_string();
    entry.toolset = Some("cron-safe".to_string());
    entry.execution_backend = Some("local".to_string());
    entry.workdir = Some(dir.to_string_lossy().to_string());
    entry.skills = vec!["reviewer".to_string()];
    entry.delivery = vec![DeliveryTarget {
        kind: "desktop_notification".to_string(),
        target: None,
        allow_private_network: false,
    }];
    entry.source_recipe_id = Some("recipe-daily".to_string());
    std::fs::write(&path, serde_json::to_string_pretty(&schedules).unwrap()).unwrap();

    let due = claim_due_schedules(&dir, 42).unwrap();
    assert_eq!(due.len(), 1);

    let raw = std::fs::read_to_string(runs_path(&dir)).unwrap();
    let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["history"][0]["scheduleId"], "daily");
    assert_eq!(state["history"][0]["epochMinute"], 42);
    assert_eq!(state["history"][0]["status"], "claimed");
    assert_eq!(state["history"][0]["sessionId"], "schedule-daily");
    assert_eq!(state["history"][0]["mode"], "scriptOnly");
    assert_eq!(state["history"][0]["toolset"], "cron-safe");
    assert_eq!(state["history"][0]["executionBackend"], "local");
    assert_eq!(
        state["history"][0]["workdir"],
        serde_json::json!(dir.to_string_lossy())
    );
    assert_eq!(
        state["history"][0]["skills"],
        serde_json::json!(["reviewer"])
    );
    assert_eq!(
        state["history"][0]["deliveryKinds"],
        serde_json::json!(["desktop_notification"])
    );
    assert_eq!(state["history"][0]["sourceRecipeId"], "recipe-daily");

    let _ = std::fs::remove_dir_all(dir);
}
