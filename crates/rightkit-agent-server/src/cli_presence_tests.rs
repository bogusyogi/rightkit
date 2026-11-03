// Unit tests for cli_presence.rs — included from its `#[cfg(test)] mod tests`
// so items/imports come from the parent `use super::*;`.

// ── autostart_enabled_from_env: pure decision function ──────────────────

#[test]
fn autostart_enabled_by_default_when_env_unset() {
    assert!(autostart_enabled_from_env(None));
}

#[test]
fn autostart_disabled_by_env_1() {
    assert!(!autostart_enabled_from_env(Some("1")));
}

#[test]
fn autostart_disabled_by_env_true_case_insensitive_common_spellings() {
    assert!(!autostart_enabled_from_env(Some("true")));
    assert!(!autostart_enabled_from_env(Some("TRUE")));
}

#[test]
fn autostart_enabled_for_unrecognized_env_values() {
    // Anything other than the documented on-values keeps auto-start on
    // rather than silently disabling it on a typo like "flase".
    assert!(autostart_enabled_from_env(Some("0")));
    assert!(autostart_enabled_from_env(Some("false")));
    assert!(autostart_enabled_from_env(Some("")));
}

#[test]
fn heartbeat_interval_is_clamped_to_protocol_bounds() {
    assert_eq!(clamp_heartbeat_seconds(0), 1);
    assert_eq!(clamp_heartbeat_seconds(15), 15);
    assert_eq!(clamp_heartbeat_seconds(600), 60);
}

#[test]
fn autostart_log_appends_instead_of_truncating() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon-autostart.log");
    std::fs::write(&log, "prior failure\n").unwrap();
    use std::io::Write;
    let capture = autostart_capture(&log);
    let mut file = capture.stderr_file().unwrap();
    file.write_all(b"next launch\n").unwrap();
    drop(file);
    capture.finish().unwrap();
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "prior failure\nnext launch\n"
    );
}

#[test]
fn autostart_log_rotation_retains_exactly_bounded_history() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon-autostart.log");
    let active = std::fs::File::create(&log).unwrap();
    active.set_len(AUTOSTART_LOG_MAX_BYTES).unwrap();
    std::fs::write(log.with_extension("log.1"), "one").unwrap();
    std::fs::write(log.with_extension("log.2"), "two").unwrap();
    std::fs::write(log.with_extension("log.3"), "three").unwrap();
    std::fs::write(log.with_extension("log.4"), "legacy-overflow").unwrap();

    use std::io::Write;
    let capture = autostart_capture(&log);
    let mut file = capture.stderr_file().unwrap();
    file.write_all(b"next launch\n").unwrap();
    drop(file);
    capture.finish().unwrap();

    assert_eq!(
        std::fs::read_to_string(log.with_extension("log.2")).unwrap(),
        "one"
    );
    assert_eq!(
        std::fs::read_to_string(log.with_extension("log.3")).unwrap(),
        "two"
    );
    // RightKit's rotation keeps `.1`..`.N` and never prunes generations beyond N, so a
    // legacy `.4` is left untouched (RightKit-published behaviour; was previously deleted).
    assert_eq!(
        std::fs::read_to_string(log.with_extension("log.4")).unwrap(),
        "legacy-overflow"
    );
}

#[test]
fn autostart_capture_keeps_a_surviving_older_writer_visible() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon-autostart.log");
    // An older daemon survived a failed health probe and still holds the shared raw file.
    let older = autostart_capture(&log);
    let mut older_writer = older.stderr_file().unwrap();
    older_writer.write_all(b"older daemon: first\n").unwrap();

    // A new CLI run starts a replacement: it drains (never finishes) the shared raw file,
    // opens its own handle, and its child may exit early.
    let newer = autostart_capture(&log);
    newer.drain().unwrap();
    let mut newer_writer = newer.stderr_file().unwrap();
    newer_writer.write_all(b"newer daemon: exits\n").unwrap();
    drop(newer_writer);
    newer.drain().unwrap();
    assert!(
        newer.raw_path().exists(),
        "the shared raw file must survive: the older writer still has it open"
    );

    // The older daemon keeps writing after the newer one is gone; its output must still arrive.
    older_writer.write_all(b"older daemon: later\n").unwrap();
    drop(older_writer);
    newer.drain().unwrap();
    let written = std::fs::read_to_string(&log).unwrap();
    assert!(written.contains("older daemon: first"));
    assert!(written.contains("newer daemon: exits"));
    assert!(written.contains("older daemon: later"));
}

// ── PresenceGuard attach/heartbeat/detach against a mock daemon ─────────

#[tokio::test]
async fn attach_returns_guard_on_successful_response() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/clients")
        .match_header("authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"clientId":"c-1","heartbeatSeconds":10,"expirySeconds":30}"#)
        .create_async()
        .await;

    let guard = attach_cli_presence_to(&server.url(), "test-token", "run", 4242).await;
    assert!(guard.is_some());
    let guard = guard.unwrap();
    assert_eq!(guard.client_id, "c-1");
    guard.detach().await;
}

#[tokio::test]
async fn attach_returns_none_on_404_older_daemon() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/clients")
        .with_status(404)
        .create_async()
        .await;

    let guard = attach_cli_presence_to(&server.url(), "tok", "run", 1).await;
    assert!(guard.is_none());
}

#[tokio::test]
async fn attach_returns_none_on_connection_refused() {
    // Port 1 is reserved/unassignable — connection is refused immediately
    // rather than timing out, keeping this test fast and deterministic.
    let guard = attach_cli_presence_to("http://127.0.0.1:1", "tok", "run", 1).await;
    assert!(guard.is_none());
}

#[tokio::test]
async fn attach_returns_none_on_malformed_response_body() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/clients")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"unexpected":"shape"}"#)
        .create_async()
        .await;

    let guard = attach_cli_presence_to(&server.url(), "tok", "run", 1).await;
    assert!(guard.is_none());
}

#[tokio::test]
async fn heartbeat_task_posts_to_the_correct_endpoint() {
    let mut server = mockito::Server::new_async().await;
    let _attach_mock = server
        .mock("POST", "/clients")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"clientId":"c-hb","heartbeatSeconds":1,"expirySeconds":3}"#)
        .create_async()
        .await;
    let heartbeat_mock = server
        .mock("POST", "/clients/c-hb/heartbeat")
        .match_header("authorization", "Bearer hb-token")
        .with_status(204)
        .expect_at_least(1)
        .create_async()
        .await;

    let guard = attach_cli_presence_to(&server.url(), "hb-token", "run", 1)
        .await
        .expect("attach should succeed");
    // heartbeat period is 1s; give the background task time for >=1 tick.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    heartbeat_mock.assert_async().await;
    guard.detach().await;
}

#[tokio::test]
async fn detach_sends_delete_to_the_correct_endpoint() {
    let mut server = mockito::Server::new_async().await;
    let _attach_mock = server
        .mock("POST", "/clients")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"clientId":"c-del","heartbeatSeconds":10,"expirySeconds":30}"#)
        .create_async()
        .await;
    let delete_mock = server
        .mock("DELETE", "/clients/c-del")
        .match_header("authorization", "Bearer del-token")
        .with_status(204)
        .create_async()
        .await;

    let guard = attach_cli_presence_to(&server.url(), "del-token", "run", 1)
        .await
        .expect("attach should succeed");
    guard.detach().await;
    delete_mock.assert_async().await;
}

#[tokio::test]
async fn drop_without_explicit_detach_still_fires_a_delete() {
    let mut server = mockito::Server::new_async().await;
    let _attach_mock = server
        .mock("POST", "/clients")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"clientId":"c-drop","heartbeatSeconds":10,"expirySeconds":30}"#)
        .create_async()
        .await;
    let delete_mock = server
        .mock("DELETE", "/clients/c-drop")
        .with_status(204)
        .expect_at_least(1)
        .create_async()
        .await;

    {
        let _guard = attach_cli_presence_to(&server.url(), "tok", "run", 1)
            .await
            .expect("attach should succeed");
        // guard drops here without an explicit .detach().await
    }
    // Drop's fire-and-forget delete task needs a moment to actually run.
    tokio::time::sleep(Duration::from_millis(300)).await;
    delete_mock.assert_async().await;
}
