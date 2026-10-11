//! Launch policy and cancel outcomes against an in-process fake service. The fake speaks
//! HeardRight's v1 wire from raw JSON (as `examples/hr_fake_host.rs` does) on a real
//! current-user socket, inside this test process: no HeardRight and no models. Unix only,
//! like the other socket suites.
#![cfg(unix)]

use rightkit_speech_client::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the fake knows about one request id.
#[derive(Clone)]
enum Job {
    /// Accepted and queued; never finishes.
    Held,
    /// Finished with this result payload.
    Done(Value),
    /// Finished with this error object.
    Failed(Value),
}

struct Fake {
    minor: u64,
    /// Never answer `file_transcription_status`, so a status query gets no reply.
    silent_status: bool,
    jobs: Mutex<HashMap<String, Job>>,
    /// Request ids of every file request received, in order.
    submitted: Mutex<Vec<String>>,
}

impl Fake {
    fn submitted(&self) -> Vec<String> {
        self.submitted.lock().unwrap().clone()
    }

    /// Answer a file request. A path containing `hold` never finishes, one containing `fail`
    /// finishes with an error, and any other finishes with [`ok_result`]. A request id seen
    /// before replays its job.
    fn file_request(&self, rid: &str, path: &str) -> String {
        self.submitted.lock().unwrap().push(rid.to_string());
        let job = {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs
                .entry(rid.to_string())
                .or_insert_with(|| {
                    if path.contains("hold") {
                        Job::Held
                    } else if path.contains("fail") {
                        Job::Failed(json!({
                            "code": "E_ENGINE",
                            "message": "ffmpeg could not read this file: Invalid data found when processing input",
                            "retryable": false,
                            "diagnostics": null
                        }))
                    } else {
                        Job::Done(ok_result())
                    }
                })
                .clone();
            job
        };
        let queued = if self.minor >= 3 {
            progress(rid, rid, "queued", 0)
        } else {
            String::new()
        };
        match job {
            Job::Held => queued,
            Job::Done(payload) => format!(
                "{queued}{}",
                frame("file_transcription_result", rid, payload, Value::Null)
            ),
            Job::Failed(error) => {
                format!("{queued}{}", frame("engine_error", rid, Value::Null, error))
            }
        }
    }

    fn status(&self, rid: &str, job_id: &str) -> String {
        let job = self.jobs.lock().unwrap().get(job_id).cloned();
        match job {
            None => frame(
                "file_transcription_status",
                rid,
                Value::Null,
                json!({
                    "code": "speech_service_job_unknown",
                    "message": "file job is unknown or expired",
                    "retryable": false,
                    "diagnostics": null
                }),
            ),
            Some(Job::Held) => progress(rid, job_id, "queued", 0),
            Some(Job::Done(payload)) => {
                frame("file_transcription_result", rid, payload, Value::Null)
            }
            Some(Job::Failed(error)) => frame("engine_error", rid, Value::Null, error),
        }
    }
}

fn frame(schema: &str, rid: &str, payload: Value, error: Value) -> String {
    let f = json!({
        "protocol_major": 1, "protocol_minor": 3, "schema_name": schema, "schema_version": 1,
        "engine_version": "in-process-fake/1", "request_id": rid, "session_id": null,
        "trace_id": "in-process", "payload": payload, "error": error
    });
    format!("{f}\n")
}

fn progress(rid: &str, job_id: &str, state: &str, position: u64) -> String {
    frame(
        "file_transcription_progress",
        rid,
        json!({
            "kind": "file_transcription_progress", "job_id": job_id, "state": state,
            "position": position, "reason": "dictation_active", "fraction": null, "message": null
        }),
        Value::Null,
    )
}

fn ok_result() -> Value {
    json!({
        "kind": "file_transcription_result",
        "text": "hello from fake",
        "srt": "1\n00:00:00,000 --> 00:00:00,900\nhello from fake\n\n",
        "vtt": "WEBVTT\n\n00:00:00.000 --> 00:00:00.900\nhello from fake\n\n",
        "words": [
            {"text": "hello", "start_ms": 0, "end_ms": 300},
            {"text": "from", "start_ms": 300, "end_ms": 600},
            {"text": "fake", "start_ms": 600, "end_ms": 900}
        ]
    })
}

fn capabilities(minor: u64) -> Value {
    let mut caps = vec!["file_transcription"];
    if minor >= 2 {
        caps.push("file_transcription_verbatim");
    }
    if minor >= 3 {
        caps.push("file_transcription_queue");
    }
    json!({"kind": "capabilities", "capabilities": caps, "engines": []})
}

fn handle(fake: &Fake, stream: UnixStream) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let rid = request["request_id"].as_str().unwrap_or("").to_string();
        let out = match request["schema_name"].as_str().unwrap_or("") {
            "engine_health" => frame(
                "engine_health",
                &rid,
                json!({"kind": "health", "status": "ok", "diagnostics": null}),
                Value::Null,
            ),
            "engine_capabilities" => frame(
                "engine_capabilities",
                &rid,
                capabilities(fake.minor),
                Value::Null,
            ),
            "file_transcription_request" => {
                fake.file_request(&rid, request["payload"]["path"].as_str().unwrap_or(""))
            }
            "file_transcription_status" => {
                if fake.silent_status {
                    continue;
                }
                fake.status(&rid, request["payload"]["job_id"].as_str().unwrap_or(""))
            }
            other => frame(
                "engine_error",
                &rid,
                Value::Null,
                json!({
                    "code": "speech_service_request_not_allowed",
                    "message": other,
                    "retryable": false,
                    "diagnostics": null
                }),
            ),
        };
        if writer.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// Publish a live record for this process and serve the fake on the real endpoint.
fn start_host(root: &Path, minor: u64, silent_status: bool) -> Arc<Fake> {
    let dir = service_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let endpoint = endpoint_for(root, "in-process");
    let _ = std::fs::remove_file(&endpoint);
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let record = json!({
        "protocol_major": 1,
        "protocol_minor": minor,
        "role": "scrape_right",
        "pid": std::process::id(),
        "endpoint": endpoint,
        "engine_version": "in-process-fake/1"
    });
    std::fs::write(service_record_path(root), record.to_string()).unwrap();
    let fake = Arc::new(Fake {
        minor,
        silent_status,
        jobs: Mutex::new(HashMap::new()),
        submitted: Mutex::new(Vec::new()),
    });
    let served = fake.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let fake = served.clone();
            std::thread::spawn(move || handle(&fake, stream));
        }
    });
    fake
}

fn env() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("s");
    std::fs::create_dir_all(&root).unwrap();
    (dir, root)
}

/// Record an installed engine that exists on disk, so the planner would start it when no
/// host is live. Returns the engine path.
fn installed_engine(root: &Path) -> PathBuf {
    let engine = root.join("engine");
    std::fs::write(&engine, b"").unwrap();
    let record = installed_heardright_path(root);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let installed = InstalledEngine {
        engine_path: engine.clone(),
        engine_version: "1".into(),
    };
    std::fs::write(&record, serde_json::to_vec(&installed).unwrap()).unwrap();
    engine
}

/// A launcher that records each call and starts nothing.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<PathBuf>>,
}

impl Recorder {
    fn calls(&self) -> Vec<PathBuf> {
        self.calls.lock().unwrap().clone()
    }
}

impl HostLauncher for Recorder {
    fn launch(&self, _suite_root: &Path, engine_path: &Path) -> Result<(), ClientError> {
        self.calls.lock().unwrap().push(engine_path.to_path_buf());
        Ok(())
    }
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Launch policy
// ---------------------------------------------------------------------------

#[test]
fn never_with_no_live_host_fails_typed_and_launches_nothing() {
    let (_dir, root) = env();
    installed_engine(&root);
    let launcher = Recorder::default();
    let mut opts = TranscribeOptions::new(Duration::from_secs(5));
    let err = transcribe_via_service_policy(
        &root,
        HostRole::CutRight,
        Path::new("/x/a.wav"),
        &mut opts,
        Launch::Never,
        &launcher,
    )
    .unwrap_err();
    assert_eq!(err, ClientError::NoLiveHost);
    assert!(launcher.calls().is_empty(), "nothing was launched");
    assert!(
        !service_record_path(&root).exists(),
        "no record was written"
    );
    assert!(
        !takeover_request_path(&root).exists(),
        "no takeover was requested"
    );
}

#[test]
fn auto_is_the_default_and_starts_only_through_the_launcher() {
    assert_eq!(Launch::default(), Launch::Auto);
    let (_dir, root) = env();
    let engine = installed_engine(&root);
    let launcher = Recorder::default();
    let mut opts = TranscribeOptions::new(Duration::from_secs(5));
    // The recorder starts nothing, so the single attempt finds no host.
    let err = transcribe_via_service_policy(
        &root,
        HostRole::CutRight,
        Path::new("/x/a.wav"),
        &mut opts,
        Launch::Auto,
        &launcher,
    )
    .unwrap_err();
    assert_eq!(err, ClientError::NoService);
    assert_eq!(launcher.calls(), vec![engine]);
}

#[test]
fn never_uses_a_live_host_as_found_without_launching_or_taking_over() {
    let (_dir, root) = env();
    let fake = start_host(&root, 3, false);
    let launcher = Recorder::default();
    let mut opts = TranscribeOptions::new(Duration::from_secs(5)).request_id("never-1");
    // HeardRight's shell outranks this scrape_right host: Auto would request a takeover here.
    let result = transcribe_via_service_policy(
        &root,
        HostRole::HeardRightShell,
        Path::new("/x/a.wav"),
        &mut opts,
        Launch::Never,
        &launcher,
    )
    .unwrap();
    assert_eq!(result.transcript.text, "hello from fake");
    assert_eq!(
        result.transcript.cues(),
        vec![Cue {
            start_ms: 0,
            end_ms: 900,
            text: "hello from fake".into()
        }]
    );
    assert_eq!(fake.submitted(), vec!["never-1".to_string()]);
    assert!(launcher.calls().is_empty(), "nothing was launched");
    assert!(
        !takeover_request_path(&root).exists(),
        "no takeover was requested"
    );
}

// ---------------------------------------------------------------------------
// Cancel outcomes
// ---------------------------------------------------------------------------

#[test]
fn cancel_reports_a_finished_job_with_its_result() {
    let (_dir, root) = env();
    let _fake = start_host(&root, 3, false);
    let mut opts = TranscribeOptions::new(Duration::from_secs(5)).request_id("done-1");
    transcribe_via_service_with(&root, HostRole::CutRight, Path::new("/x/a.wav"), &mut opts)
        .unwrap();
    let token = CancelToken::new();
    match cancel_request(&root, &token, "done-1", Duration::from_secs(2)) {
        CancelOutcome::Finished(Ok(result)) => {
            assert_eq!(result.transcript.text, "hello from fake")
        }
        other => panic!("expected Finished(Ok), got {other:?}"),
    }
    assert!(token.is_cancelled());
}

#[test]
fn cancel_reports_a_job_that_finished_with_an_error() {
    let (_dir, root) = env();
    let _fake = start_host(&root, 3, false);
    let mut opts = TranscribeOptions::new(Duration::from_secs(5)).request_id("failed-1");
    let err = transcribe_via_service_with(
        &root,
        HostRole::CutRight,
        Path::new("/x/fail.wav"),
        &mut opts,
    )
    .unwrap_err();
    assert!(
        matches!(&err, ClientError::Engine { code, .. } if code.as_str() == "E_ENGINE"),
        "{err}"
    );
    let outcome = cancel_request(
        &root,
        &CancelToken::new(),
        "failed-1",
        Duration::from_secs(2),
    );
    assert!(
        matches!(
            &outcome,
            CancelOutcome::Finished(Err(ClientError::Engine { code, .. })) if code.as_str() == "E_ENGINE"
        ),
        "{outcome:?}"
    );
}

#[test]
fn cancel_reports_a_held_job_as_still_active_because_the_wire_cannot_stop_it() {
    let (_dir, root) = env();
    let fake = start_host(&root, 3, false);
    let token = CancelToken::new();
    let (job_root, job_token) = (root.clone(), token.clone());
    let job = std::thread::spawn(move || {
        let mut opts = TranscribeOptions::new(Duration::from_secs(30))
            .request_id("hold-job")
            .cancel_token(job_token);
        transcribe_via_service_with(
            &job_root,
            HostRole::CutRight,
            Path::new("/x/hold.wav"),
            &mut opts,
        )
    });
    wait_until("the held job to be submitted", || {
        fake.submitted().iter().any(|id| id.as_str() == "hold-job")
    });
    match cancel_request(&root, &token, "hold-job", Duration::from_secs(2)) {
        CancelOutcome::StillActive(progress) => {
            assert_eq!(progress.job_id.as_deref(), Some("hold-job"));
            assert_eq!(progress.state, ProgressState::Queued);
        }
        other => panic!("expected StillActive, got {other:?}"),
    }
    // The in-flight request was closed by the cancel, not completed.
    assert_eq!(job.join().unwrap(), Err(ClientError::Cancelled));
}

#[test]
fn cancel_reports_an_id_the_service_does_not_hold_as_no_longer_active() {
    let (_dir, root) = env();
    let _fake = start_host(&root, 3, false);
    let outcome = cancel_request(
        &root,
        &CancelToken::new(),
        "never-sent",
        Duration::from_secs(2),
    );
    assert_eq!(outcome, CancelOutcome::NoLongerActive);
}

#[test]
fn cancel_without_a_live_service_is_unknown() {
    let (_dir, root) = env();
    let outcome = cancel_request(&root, &CancelToken::new(), "x", Duration::from_secs(2));
    assert_eq!(outcome, CancelOutcome::Unknown(ClientError::NoService));
}

#[test]
fn cancel_with_no_reply_within_the_bound_is_unknown_and_returns_in_time() {
    let (_dir, root) = env();
    let _fake = start_host(&root, 3, true);
    let started = Instant::now();
    let outcome = cancel_request(&root, &CancelToken::new(), "x", Duration::from_millis(300));
    assert_eq!(outcome, CancelOutcome::Unknown(ClientError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "returned in {:?}",
        started.elapsed()
    );
}

#[test]
fn cancel_against_a_host_without_the_queue_contract_is_unknown() {
    let (_dir, root) = env();
    let _fake = start_host(&root, 2, false);
    let outcome = cancel_request(&root, &CancelToken::new(), "x", Duration::from_secs(2));
    assert!(
        matches!(outcome, CancelOutcome::Unknown(ClientError::Unsupported(_))),
        "{outcome:?}"
    );
}

#[test]
fn cancel_needs_a_request_id() {
    let (_dir, root) = env();
    let outcome = cancel_request(&root, &CancelToken::new(), "", Duration::from_secs(2));
    assert!(
        matches!(outcome, CancelOutcome::Unknown(ClientError::Unsupported(_))),
        "{outcome:?}"
    );
}
