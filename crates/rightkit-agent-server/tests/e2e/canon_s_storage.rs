//! S:E3 / S:E11 public-runtime qualification.

use crate::support::daemon_process;
use crate::support::e2e_api::*;
use crate::support::scripted_provider::Step;
use rightkit_db::rusqlite::Connection;
use serde_json::json;

#[test]
fn compaction_archive_reopens_and_materializes_original_history() {
    let mut rig = Rig::start(vec![
        Step::text("first durable answer"),
        Step::text("second durable answer"),
        // Current summarizer contract validates all seven canonical sections
        // (default_compaction.rs -> SessionStateSummary::parse_and_validate).
        Step::text("## Goal\nPreserve durable history\n## Decisions\n(none)\n## Constraints\n(none)\n## Files touched\n(none)\n## Current state\nTwo durable turns completed\n## Open questions\n(none)\n## Next steps\n- Reopen original history"),
    ]);
    rig.session("s-e3", json!({"permissionMode":"bypass"}));
    // The typed no-gain guard refuses a compaction whose replacement is not smaller
    // than the history, so the fixture history must out-weigh the seven-section summary.
    let filler = "durable history filler sentence. ".repeat(160);
    assert_eq!(
        rig.send("s-e3", &format!("E3-FIRST-PROMPT {filler}")).0,
        200
    );
    rig.wait_assistant("s-e3", "first durable answer");
    assert_eq!(
        rig.send("s-e3", &format!("E3-SECOND-PROMPT {filler}")).0,
        200
    );
    rig.wait_assistant("s-e3", "second durable answer");

    let (status, compacted) = rig.post("/sessions/s-e3/compact", json!({}));
    assert_eq!(status, 200, "manual compaction: {compacted}");
    assert_eq!(compacted["compacted"], true, "{compacted}");
    let archive_id = compacted["archive"]["id"].as_str().expect("archive id");
    assert!(compacted["archive"]["messageCount"].as_u64().unwrap_or(0) > 0);

    rig.restart();
    let (status, archive) = rig.get(&format!("/sessions/s-e3/compaction-archives/{archive_id}"));
    assert_eq!(status, 200, "archive after restart: {archive}");
    let body = archive.to_string();
    assert!(body.contains("E3-FIRST-PROMPT"), "{body}");
    assert!(body.contains("E3-SECOND-PROMPT"), "{body}");
}

#[test]
fn malformed_durable_event_tail_is_quarantined_before_public_replay() {
    let mut rig = Rig::start(vec![Step::text("prefix answer")]);
    rig.session("s-e11", json!({"permissionMode":"bypass"}));
    assert_eq!(rig.send("s-e11", "E11-PREFIX").0, 200);
    rig.wait_assistant("s-e11", "prefix answer");
    rig.wait_event("s-e11", "sessionTurnFinished");
    let prefix_len = rig.history("s-e11").len();
    assert!(
        prefix_len > 1,
        "expected durable events before fault injection"
    );

    let db = rig.daemon.startup.database_path.clone();
    let _ = rig.daemon.child.kill();
    let _ = rig.daemon.child.wait();
    let conn = Connection::open(&db).expect("open stopped daemon database");
    let tail_seq: i64 = conn
        .query_row(
            "SELECT MAX(seq) FROM events WHERE session_id = ?1",
            ["s-e11"],
            |row| row.get(0),
        )
        .expect("read durable tail seq");
    conn.execute(
        "UPDATE events SET payload_json = ?1 WHERE session_id = ?2 AND seq = ?3",
        rightkit_db::rusqlite::params!["{torn-json", "s-e11", tail_seq],
    )
    .expect("inject a torn durable record");
    drop(conn);
    rig.daemon = daemon_process::start_daemon(rig.home.path(), &[]);

    let events = rig.history("s-e11");
    assert_eq!(
        events.len(),
        prefix_len - 1,
        "bad tail must leave good prefix replayable: {events:?}"
    );
    assert!(!events
        .iter()
        .any(|event| event.to_string().contains("torn-json")));
}
