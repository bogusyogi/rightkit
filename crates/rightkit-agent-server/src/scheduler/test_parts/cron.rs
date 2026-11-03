#[test]
fn cron_matches_every_five_minutes() {
    assert!(cron_matches_in_time_zone("*/5 * * * *", 5, "UTC").unwrap());
    assert!(!cron_matches_in_time_zone("*/5 * * * *", 6, "UTC").unwrap());
}

#[test]
fn script_only_schedule_rejects_nonlocal_backend() {
    let entry = ScheduleEntry {
        id: "nonlocal-job".to_string(),
        cron_expr: "* * * * *".to_string(),
        prompt: "echo nonlocal".to_string(),
        time_zone: "UTC".to_string(),
        skills: Vec::new(),
        workdir: None,
        toolset: None,
        execution_backend: Some("missing-backend".to_string()),
        delivery: Vec::new(),
        script_allowlist: vec!["echo nonlocal".to_string()],
        mode: "scriptOnly".to_string(),
        paused: false,
        source_recipe_id: None,
        recipe_params: BTreeMap::new(),
        max_runtime_seconds: None,
    };

    let error = script_only_execution_backend(&entry).unwrap_err();
    assert!(error.contains("supported backends: local"));
}

#[test]
fn cron_matches_daily_hour_and_minute() {
    // 1970-01-01 09:00 UTC.
    assert!(cron_matches_in_time_zone("0 9 * * *", 9 * 60, "UTC").unwrap());
    assert!(!cron_matches_in_time_zone("1 9 * * *", 9 * 60, "UTC").unwrap());
}

#[test]
fn next_due_epoch_minute_is_exclusive_and_utc() {
    assert_eq!(
        next_due_epoch_minute_in_time_zone("0 9 * * *", "UTC", 8 * 60 + 59, 1440).unwrap(),
        Some(9 * 60)
    );
    assert_eq!(
        next_due_epoch_minute_in_time_zone("0 9 * * *", "UTC", 9 * 60, 1440 * 2).unwrap(),
        Some(33 * 60)
    );
}

#[test]
fn next_due_epoch_minute_respects_fixed_utc_offset_time_zone() {
    assert_eq!(
        next_due_epoch_minute_in_time_zone("0 9 * * *", "UTC+05:30", 3 * 60 + 29, 1440).unwrap(),
        Some(3 * 60 + 30)
    );
}

#[test]
fn cron_matches_month_day_and_weekday() {
    // 1970-01-01 was Thursday; weekday 4 when Sunday=0.
    assert!(cron_matches_in_time_zone("0 0 1 1 4", 0, "UTC").unwrap());
    assert!(!cron_matches_in_time_zone("0 0 1 1 5", 0, "UTC").unwrap());
}

#[test]
fn claim_due_schedules_matches_cron_in_schedule_time_zone() {
    let dir = test_dir("claim_due_schedules_matches_cron_in_schedule_time_zone");
    write_schedule_with_time_zone(&dir, "morning", "0 9 * * *", "ping", "UTC+05:30");

    let due = claim_due_schedules(&dir, 3 * 60 + 30).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, "morning");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn claim_due_schedules_matches_iana_time_zone_with_dst() {
    let dir = test_dir("claim_due_schedules_matches_iana_time_zone_with_dst");
    write_schedule_with_time_zone(
        &dir,
        "new-york-morning",
        "0 9 * * *",
        "ping",
        "America/New_York",
    );

    let summer_9am_new_york = epoch_minute_from_civil(2026, 7, 15, 13, 0);
    let due = claim_due_schedules(&dir, summer_9am_new_york).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, "new-york-morning");

    let summer_10am_new_york = epoch_minute_from_civil(2026, 7, 16, 14, 0);
    let not_due = claim_due_schedules(&dir, summer_10am_new_york).unwrap();
    assert!(not_due.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}
