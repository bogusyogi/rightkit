use super::*;

#[test]
fn command_probe_rotation_moves_full_log_to_sidecar() {
    let path = std::env::temp_dir().join(format!(
        "rightkit-command-probe-rotation-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    let rotated = path.with_extension("jsonl.1");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&rotated);

    let file = std::fs::File::create(&path).expect("create rotation fixture");
    file.set_len(COMMAND_EVENTS_JSONL_MAX_BYTES)
        .expect("fill rotation fixture");
    drop(file);

    write_event_at(&path, &serde_json::json!({"event": "test_rotation"}))
        .expect("production writer rotates full log");

    assert!(path.exists());
    assert_eq!(
        std::fs::metadata(&rotated).expect("rotated log").len(),
        COMMAND_EVENTS_JSONL_MAX_BYTES
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&rotated);
}

#[test]
fn command_probe_rollover_rotates_before_next_line_overflows_limit() {
    let path = std::env::temp_dir().join(format!(
        "rightkit-command-probe-rollover-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    let rotated = path.with_extension("jsonl.1");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&rotated);

    let file = std::fs::File::create(&path).expect("create rollover fixture");
    file.set_len(COMMAND_EVENTS_JSONL_MAX_BYTES - 1)
        .expect("almost full log");
    drop(file);
    write_event_at(&path, &serde_json::json!({"event": "test_rollover"}))
        .expect("production writer rotates before next line");

    assert_eq!(
        std::fs::metadata(&rotated).expect("rotated log").len(),
        COMMAND_EVENTS_JSONL_MAX_BYTES - 1
    );
    let line: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).expect("new log"))
        .expect("complete JSON line");
    assert_eq!(line["event"], "test_rollover");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&rotated);
}
