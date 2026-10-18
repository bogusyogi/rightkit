//! End-to-end: real files, real child processes, real HTTP over loopback.

use rightkit_logs::testing::MockServer;
use rightkit_logs::{
    AsyncWriter, ConsentStore, EnqueueError, EventRecord, FlushState, HttpTransport, JsonlStore,
    Level, Outbox, OutboxConfig, Scheduler, Telemetry, UploadConfig, Uploader,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHILD_ENV: &str = "RK_LOGS_E2E_CHILD_DIR";
const SECRET_VALUE: &str = "hunter2-the-real-secret";
const EMAIL: &str = "ada.lovelace@example.org";
const USER_PATH: &str = "/Users/adalovelace/Documents/private/notes.txt";
const DICTATED: &str = "send the quarterly numbers to the board";

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-e2e-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn open(dir: &Path, url: &str) -> Telemetry {
    let mut config = UploadConfig::new(url, "ExampleApp", "9.9.9");
    config.base_backoff_secs = 1;
    Telemetry::open(
        dir,
        config,
        Box::new(HttpTransport::new("rightkit-logs-e2e")),
        OutboxConfig::default(),
    )
    .expect("open telemetry")
}

/// Runs inside a separate OS process (spawned by the tests below). Does nothing
/// when invoked as an ordinary test.
#[test]
fn child_grants_consent_and_enqueues() {
    let Ok(dir) = std::env::var(CHILD_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let telemetry = open(&dir, "http://127.0.0.1:9/unused");
    telemetry.consent.grant("2026-10").expect("grant consent");
    for index in 0..3 {
        telemetry
            .outbox
            .enqueue(
                "usage",
                json!({
                    "index": index,
                    "transcript": DICTATED,
                    "api_key": SECRET_VALUE,
                    "note": format!("failed for {EMAIL} reading {USER_PATH}"),
                    "nested": { "clipboard": DICTATED, "count": 7 },
                }),
            )
            .expect("enqueue");
    }
    // Exit without any flush: persistence must be on disk already.
}

fn run_child(dir: &Path) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_grants_consent_and_enqueues",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, dir)
        .status()
        .expect("spawn child");
    assert!(status.success(), "child process failed");
}

#[test]
fn consent_gates_upload_outbox_persists_across_restart_and_backoff_survives() {
    let dir = temp_dir("restart");
    let server = MockServer::start();

    // Before consent: enqueue refuses, flush sends nothing, nothing is written.
    {
        let telemetry = open(&dir, &server.url);
        assert_eq!(
            telemetry.outbox.enqueue("usage", json!({"a": 1})),
            Err(EnqueueError::NoConsent)
        );
        assert_eq!(telemetry.uploader.flush().state, FlushState::NoConsent);
        assert!(
            !telemetry.outbox.path().exists(),
            "no ledger may exist without consent"
        );
    }
    assert_eq!(server.count(), 0);

    // A different process consents and queues events, then exits.
    run_child(&dir);

    // Restart: the queue is on disk and still nothing has been uploaded.
    let ledger_text =
        std::fs::read_to_string(dir.join("telemetry-outbox.json")).expect("ledger on disk");
    for forbidden in [SECRET_VALUE, EMAIL, "adalovelace", DICTATED] {
        assert!(
            !ledger_text.contains(forbidden),
            "ledger leaked {forbidden:?}:\n{ledger_text}"
        );
    }
    assert!(ledger_text.contains("[redacted:email]"));
    assert!(ledger_text.contains("/Users/<user>/Documents/private/notes.txt"));
    assert_eq!(server.count(), 0);

    let telemetry = open(&dir, &server.url);
    assert_eq!(telemetry.outbox.len(), 3, "events persisted across restart");

    // Revoked consent blocks upload even with events queued.
    telemetry.consent.revoke().unwrap();
    assert_eq!(telemetry.uploader.flush_now().state, FlushState::NoConsent);
    assert_eq!(server.count(), 0);
    telemetry.consent.grant("2026-10").unwrap();

    // Server asks us to back off: events stay queued, failure is counted.
    server.script(&[(503, Some(1))]);
    let report = telemetry.uploader.flush();
    assert!(
        matches!(
            report.state,
            FlushState::Retrying {
                status: Some(503),
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.remaining, 3);
    assert_eq!(server.count(), 1);

    // Backoff is persisted: a restarted process still waits.
    drop(telemetry);
    let telemetry = open(&dir, &server.url);
    let report = telemetry.uploader.flush();
    assert!(
        matches!(report.state, FlushState::Backoff { .. }),
        "{report:?}"
    );
    assert_eq!(server.count(), 1, "no request during backoff");

    // After the wait the batch goes through and the ledger empties on disk.
    thread::sleep(Duration::from_millis(1_300));
    let report = telemetry.uploader.flush();
    assert_eq!(report.state, FlushState::Drained, "{report:?}");
    assert_eq!((report.sent, report.remaining), (3, 0));
    assert_eq!(server.count(), 2);

    let body = server.bodies().pop().unwrap();
    assert_eq!(body["schema_version"], 1);
    assert_eq!(
        body["app"],
        json!({"name": "ExampleApp", "version": "9.9.9"})
    );
    assert!(body.get("install_id").is_none(), "install id is opt-in");
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["type"], "usage");
    let wire = body.to_string();
    for forbidden in [SECRET_VALUE, EMAIL, "adalovelace", DICTATED] {
        assert!(
            !wire.contains(forbidden),
            "wire body leaked {forbidden:?}: {wire}"
        );
    }
    assert_eq!(events[0]["payload"]["nested"]["count"], 7);

    let reopened = open(&dir, &server.url);
    assert_eq!(
        reopened.outbox.len(),
        0,
        "acknowledged events are gone from disk"
    );
}

#[test]
fn revoke_purges_outbox_and_scheduler_uploads_only_while_consented() {
    let dir = temp_dir("revoke");
    let server = MockServer::start();
    let telemetry = open(&dir, &server.url);
    telemetry.consent.grant("2026-10").unwrap();
    telemetry.outbox.enqueue("usage", json!({"k": 1})).unwrap();
    telemetry.outbox.enqueue("usage", json!({"k": 2})).unwrap();

    let scheduler =
        Scheduler::spawn(telemetry.uploader.clone(), Duration::from_millis(100)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.count() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.count(), 1, "scheduler delivered the queued batch");

    // Queue more while the server is unavailable, then the user revokes.
    server.script(&[(500, Some(1)); 8]);
    telemetry.outbox.enqueue("usage", json!({"k": 3})).unwrap();
    telemetry.revoke().unwrap();
    assert!(telemetry.outbox.is_empty());
    assert!(
        !telemetry.outbox.path().exists(),
        "ledger deleted on revoke"
    );
    assert_eq!(
        telemetry.outbox.enqueue("usage", json!({"k": 4})),
        Err(EnqueueError::NoConsent)
    );
    let before = server.count();
    thread::sleep(Duration::from_millis(600));
    assert!(
        server.count() <= before + 1,
        "at most one in-flight request after revoke"
    );
    scheduler.stop();
}

#[test]
fn permanent_rejection_drops_events_and_older_policy_version_blocks_upload() {
    let dir = temp_dir("reject");
    let server = MockServer::start();
    let consent = ConsentStore::new(dir.join("consent.json"));
    let outbox = Arc::new(Outbox::open(
        dir.join("outbox.json"),
        consent.clone(),
        OutboxConfig::default(),
    ));
    let mut config = UploadConfig::new(&server.url, "ExampleApp", "1.0.0");
    config.policy_version = Some("v2".into());
    config.install_id = Some("11111111-2222-4333-8444-555555555555".into());
    let uploader =
        Uploader::new(outbox.clone(), Box::new(HttpTransport::new("e2e")), config).unwrap();

    consent.grant("v1").unwrap();
    outbox.enqueue("usage", json!({"x": 1})).unwrap();
    assert_eq!(
        uploader.flush().state,
        FlushState::NoConsent,
        "stale wording version is not consent"
    );
    assert_eq!(server.count(), 0);

    consent.grant("v2").unwrap();
    server.script(&[(422, None)]);
    let report = uploader.flush();
    assert_eq!(
        (report.sent, report.rejected, report.remaining),
        (0, 1, 0),
        "{report:?}"
    );
    assert_eq!(
        server.bodies()[0]["install_id"],
        "11111111-2222-4333-8444-555555555555"
    );
}

#[test]
fn endpoint_must_be_https_or_loopback() {
    let dir = temp_dir("endpoint");
    let consent = ConsentStore::new(dir.join("c.json"));
    let outbox = Arc::new(Outbox::open(
        dir.join("o.json"),
        consent,
        OutboxConfig::default(),
    ));
    for bad in [
        "http://telemetry.example.com/ingest",
        "ftp://x/y",
        "",
        "http://127.0.0.1.evil.com/x",
    ] {
        let result = Uploader::new(
            outbox.clone(),
            Box::new(HttpTransport::new("e2e")),
            UploadConfig::new(bad, "a", "1"),
        );
        assert!(result.is_err(), "{bad:?} must be rejected");
    }
    for good in [
        "https://telemetry.example.com/ingest",
        "http://localhost:8080/x",
        "http://[::1]:9/x",
        "http://127.0.0.1:1/x",
    ] {
        let result = Uploader::new(
            outbox.clone(),
            Box::new(HttpTransport::new("e2e")),
            UploadConfig::new(good, "a", "1"),
        );
        assert!(result.is_ok(), "{good:?} must be accepted");
    }
}

#[test]
fn corrupt_ledger_is_quarantined_and_purge_removes_quarantine() {
    let dir = temp_dir("corrupt");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let ledger = dir.join("outbox.json");
    std::fs::write(&ledger, b"{ not json \xff\xfe").unwrap();

    let outbox = Outbox::open(&ledger, consent, OutboxConfig::default());
    outbox
        .enqueue("usage", json!({"ok": true}))
        .expect("recovers from corruption");
    assert_eq!(outbox.len(), 1);
    let quarantined: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .collect();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(
        std::fs::read(quarantined[0].path()).unwrap(),
        b"{ not json \xff\xfe"
    );

    outbox.purge().unwrap();
    let leftovers = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            name.to_string_lossy().contains("outbox") && name != "outbox.json.lock"
        })
        .count();
    assert_eq!(leftovers, 0, "purge removes ledger and quarantined bytes");
}

#[test]
fn oversized_event_is_refused_not_truncated() {
    let dir = temp_dir("oversize");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let config = OutboxConfig {
        max_event_bytes: 1024,
        ..OutboxConfig::default()
    };
    let outbox = Outbox::open(dir.join("outbox.json"), consent, config);
    let error = outbox
        .enqueue("usage", json!({"blob": "x".repeat(4096)}))
        .unwrap_err();
    assert!(matches!(error, EnqueueError::TooLarge { .. }));
    assert!(outbox.is_empty());
}

#[test]
fn dedupe_receipt_survives_ack_and_restart_until_retired() {
    let dir = temp_dir("dedupe");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let ledger = dir.join("outbox.json");
    let outbox = Outbox::open(&ledger, consent.clone(), OutboxConfig::default());

    assert!(outbox.enqueue_deduped("", "usage", json!({})).is_err());
    let first = outbox
        .enqueue_deduped("report-1", "usage", json!({"n": 1}))
        .unwrap();
    let repeat = outbox
        .enqueue_deduped("report-1", "usage", json!({"n": 2}))
        .unwrap();
    assert_eq!(
        repeat, first,
        "a repeated key returns the original acceptance"
    );
    assert_eq!(outbox.len(), 1);

    // Upload ack removes the event, never the receipt.
    outbox.acknowledge(&[first]).unwrap();
    assert_eq!(outbox.len(), 0);
    drop(outbox);

    // After a restart the receipt is still durable in the ledger.
    let outbox = Outbox::open(&ledger, consent, OutboxConfig::default());
    let after_ack = outbox
        .enqueue_deduped("report-1", "usage", json!({"n": 3}))
        .unwrap();
    assert_eq!(after_ack, first);
    assert_eq!(outbox.len(), 0, "no duplicate is stored after ack");

    assert!(outbox.retire_dedupe("report-1").unwrap());
    assert!(!outbox.retire_dedupe("report-1").unwrap());
    let fresh = outbox
        .enqueue_deduped("report-1", "usage", json!({"n": 4}))
        .unwrap();
    assert_ne!(fresh, first);
    assert_eq!(outbox.len(), 1);
}

#[test]
fn dedupe_holds_across_two_instances_sharing_one_ledger() {
    let dir = temp_dir("dedupe-instances");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let ledger = dir.join("outbox.json");
    let open_instance = || {
        Arc::new(Outbox::open(
            &ledger,
            ConsentStore::new(dir.join("consent.json")),
            OutboxConfig::default(),
        ))
    };
    // Two instances are the cross-process stand-in: the sidecar file lock plus
    // reload-under-lock is what serialises them.
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let instance = open_instance();
            thread::spawn(move || {
                let mut ids = Vec::new();
                for _ in 0..8 {
                    ids.push(
                        instance
                            .enqueue_deduped("shared", "usage", json!({"x": 1}))
                            .unwrap(),
                    );
                }
                ids
            })
        })
        .collect();
    let mut all_ids: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    all_ids.dedup();
    assert_eq!(all_ids.len(), 1, "every caller got the one acceptance");
    assert_eq!(open_instance().len(), 1, "exactly one event was stored");
}

#[test]
fn permanent_rejection_with_failed_ack_retains_pending_without_spinning() {
    let dir = temp_dir("reject-ack-fails");
    let server = MockServer::start();
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let ledger = dir.join("outbox.json");
    let outbox = Arc::new(Outbox::open(&ledger, consent, OutboxConfig::default()));
    let mut config = UploadConfig::new(&server.url, "ExampleApp", "1.0.0");
    config.base_backoff_secs = 1;
    let uploader =
        Uploader::new(outbox.clone(), Box::new(HttpTransport::new("e2e")), config).unwrap();
    outbox.enqueue("usage", json!({"x": 1})).unwrap();

    // The ledger can no longer be replaced (a directory sits at its path), so
    // the ack persist fails after the server's permanent 422.
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    server.script(&[(422, None); 4]);

    let report = uploader.flush();
    assert!(
        matches!(
            report.state,
            FlushState::Retrying {
                status: Some(422),
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(
        report.rejected, 0,
        "no rejected counter without a durable ack"
    );
    assert_eq!(report.remaining, 1, "the event stays pending");
    assert_eq!(
        server.count(),
        1,
        "the failed ack did not re-post in a spin"
    );
}

#[test]
fn async_writer_never_blocks_and_accounts_for_every_event() {
    let dir = temp_dir("writer");
    let store = Arc::new(JsonlStore::new(dir.join("events.jsonl"), 0));
    let writer = Arc::new(AsyncWriter::spawn(store.clone(), 64).unwrap());
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let writer = writer.clone();
            thread::spawn(move || {
                for i in 0..250 {
                    writer.append(
                        EventRecord::new(i, Level::Info, "tick").with_field("thread", json!(t)),
                    );
                }
            })
        })
        .collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    assert!(writer.flush(Duration::from_secs(10)));
    let text = std::fs::read_to_string(store.path()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    for line in &lines {
        let value: Value = serde_json::from_str(line).expect("every line is whole JSON");
        assert_eq!(value["event"], "tick");
    }
    assert_eq!(lines.len() as u64 + writer.dropped(), 1000);
}

#[test]
fn outstanding_acceptances_backpressure_instead_of_evicting_receipts() {
    let dir = temp_dir("receipt-cap");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let path = dir.join("outbox.json");
    let config = OutboxConfig {
        max_events: 1,
        ..OutboxConfig::default()
    };
    let outbox = Outbox::open(&path, consent.clone(), config.clone());
    let first = outbox.enqueue_deduped("a", "crash", json!({})).unwrap();
    assert!(outbox.enqueue("usage", json!({})).is_err());
    assert_eq!(outbox.pending(10, i64::MAX)[0].id, first);
    outbox.acknowledge(&[first]).unwrap();
    let second = outbox.enqueue_deduped("b", "crash", json!({})).unwrap();
    outbox.acknowledge(&[second]).unwrap();
    assert!(outbox.enqueue_deduped("c", "crash", json!({})).is_err());
    drop(outbox);
    let outbox = Outbox::open(&path, consent, config);
    assert_eq!(
        outbox.enqueue_deduped("a", "crash", json!({})).unwrap(),
        first
    );
    assert!(outbox.is_empty());
    outbox.retire_dedupe("b").unwrap();
    let third = outbox.enqueue_deduped("c", "crash", json!({})).unwrap();
    outbox.acknowledge(&[third]).unwrap();
    assert_eq!(
        outbox.enqueue_deduped("a", "crash", json!({})).unwrap(),
        first
    );
    assert!(outbox.is_empty());
}

fn wait_for_lock_fixture(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "lock fixture timed out: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn child_purge_lock_waiter() {
    let Ok(dir) = std::env::var("RK_PURGE_LOCK_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let waiter = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("outbox.json.lock"))
        .unwrap();
    std::fs::write(dir.join("opened"), b"").unwrap();
    waiter.lock().unwrap();
    std::fs::write(dir.join("acquired"), b"").unwrap();
    wait_for_lock_fixture(&dir.join("unlock"));
    waiter.unlock().unwrap();
    std::fs::write(dir.join("unlocked"), b"").unwrap();
    // Retain the exact handle that was blocked before purge.
    wait_for_lock_fixture(&dir.join("relock"));
    waiter.lock().unwrap();
    std::fs::write(dir.join("held"), b"").unwrap();
    wait_for_lock_fixture(&dir.join("release"));
}

#[test]
fn purge_keeps_blocked_process_waiter_on_same_coordination_object() {
    let dir = temp_dir("purge-lock");
    let consent = ConsentStore::new(dir.join("consent.json"));
    consent.grant("v1").unwrap();
    let outbox = Outbox::open(dir.join("outbox.json"), consent, OutboxConfig::default());
    outbox.enqueue("usage", json!({})).unwrap();
    let original = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("outbox.json.lock"))
        .unwrap();
    original.lock().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_purge_lock_waiter", "--test-threads=1"])
        .env("RK_PURGE_LOCK_CHILD", &dir)
        .spawn()
        .unwrap();
    wait_for_lock_fixture(&dir.join("opened"));
    thread::sleep(Duration::from_millis(100));
    assert!(
        !dir.join("acquired").exists(),
        "waiter must block on original lock"
    );
    drop(original);
    wait_for_lock_fixture(&dir.join("acquired"));
    std::fs::write(dir.join("unlock"), b"").unwrap();
    wait_for_lock_fixture(&dir.join("unlocked"));
    outbox.purge().unwrap();
    std::fs::write(dir.join("relock"), b"").unwrap();
    wait_for_lock_fixture(&dir.join("held"));
    let newcomer = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("outbox.json.lock"))
        .unwrap();
    let same_lock = newcomer.try_lock().is_err();
    std::fs::write(dir.join("release"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(
        same_lock,
        "purge split the blocked waiter's coordination inode"
    );
    drop(newcomer);
}
