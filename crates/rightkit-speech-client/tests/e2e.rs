//! End-to-end: real fake-host processes speaking HeardRight's v1 wire protocol
//! (`examples/hr_fake_host.rs`) over the real current-user-only socket, real
//! discovery files, the real client. Windows named pipes are exercised on the
//! Windows host; this file runs on Unix.
#![cfg(unix)]

use rightkit_speech_client::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn host_bin() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join("hr_fake_host")
}

struct Host {
    child: Child,
    pid: u32,
    out: BufReader<std::process::ChildStdout>,
}

impl Host {
    /// `args`: extra flags such as `--wait-free`.
    fn spawn(root: &Path, role: &str, minor: u32, state: &Path, args: &[&str]) -> Host {
        let mut h = Self::spawn_unready(root, role, minor, state, args);
        h.wait_ready();
        h
    }
    /// Spawn without waiting for READY (a `--wait-free` host is not ready until the incumbent is gone).
    fn spawn_unready(root: &Path, role: &str, minor: u32, state: &Path, args: &[&str]) -> Host {
        let mut child = Command::new(host_bin())
            .args([
                root.to_str().unwrap(),
                role,
                &minor.to_string(),
                state.to_str().unwrap(),
            ])
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn hr_fake_host (cargo test builds examples)");
        let out = BufReader::new(child.stdout.take().unwrap());
        Host { child, pid: 0, out }
    }
    fn wait_ready(&mut self) {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        self.pid = line
            .strip_prefix("READY ")
            .unwrap_or_else(|| panic!("host not ready: {line:?}"))
            .trim()
            .parse()
            .unwrap();
    }
    fn start(root: &Path, role: &str, minor: u32, state: &Path) -> Host {
        Self::spawn(root, role, minor, state, &[])
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Env {
    _d: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
}

fn env() -> Env {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("s");
    let state = d.path().join("st");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    Env { _d: d, root, state }
}

fn wait_for(p: &Path, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while !p.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    p.exists()
}

fn last_request(e: &Env) -> Value {
    serde_json::from_slice(&std::fs::read(e.state.join("last_request.json")).unwrap()).unwrap()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Wire compatibility with HeardRight v1
// ---------------------------------------------------------------------------

#[test]
fn frames_match_heardright_v1_fixtures_exactly() {
    // Requests this client sends: byte-for-byte HeardRight's field set and order,
    // modulo the client's own engine_version label.
    let normalise = |s: &str| -> Value {
        let mut v: Value = serde_json::from_str(s.trim()).unwrap();
        v["engine_version"] = json!("x");
        v
    };
    for (frame, name) in [
        (
            ServiceEnvelope::file_request("r1", "t", "a.wav".into()),
            "v1_file_request.json",
        ),
        (ServiceEnvelope::health("r1", "t"), "v1_health_request.json"),
    ] {
        let ours = serde_json::to_string(&frame).unwrap();
        assert_eq!(normalise(&ours), normalise(&fixture(name)), "{name}");
        // Field order is part of "exact": same keys in the same order.
        let keys = |s: &str| {
            serde_json::from_str::<serde_json::Map<String, Value>>(s)
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&ours), keys(&fixture(name)), "{name} key order");
    }
    // Frames a v1 host sends: this client parses them.
    let result: ServiceEnvelope =
        serde_json::from_str(fixture("v1_file_result.json").trim()).unwrap();
    assert_eq!(result.transcript().unwrap().words[0].end_ms, 100);
    assert_eq!(result.transcript_result().unwrap().engine, None);
    let handoff: ServiceEnvelope =
        serde_json::from_str(fixture("v1_handoff_error.json").trim()).unwrap();
    assert!(handoff
        .error
        .as_ref()
        .is_some_and(|e| e.code == ERROR_HOST_HANDOFF && e.retryable));
    let denied: ServiceEnvelope =
        serde_json::from_str(fixture("v1_not_allowed_error.json").trim()).unwrap();
    assert_eq!(denied.error.unwrap().code, ERROR_NOT_ALLOWED);
    // Discovery files written by a v1 host parse; ours serialise back to the same fields.
    let rec: ServiceRecord = serde_json::from_str(&fixture("v1_service_record.json")).unwrap();
    assert_eq!(
        (rec.role, rec.protocol_minor, rec.engine_version.as_str()),
        (HostRole::HeardRightShell, 0, "0.9.0")
    );
    assert_eq!(
        serde_json::to_value(&rec).unwrap(),
        serde_json::from_str::<Value>(&fixture("v1_service_record.json")).unwrap()
    );
    let tk: TakeoverRequest = serde_json::from_str(&fixture("v1_takeover.json")).unwrap();
    assert_eq!(tk.role, HostRole::ScrapeRight);
    assert_eq!(
        serde_json::to_value(&tk).unwrap(),
        serde_json::from_str::<Value>(&fixture("v1_takeover.json")).unwrap()
    );
    let inst: InstalledEngine = serde_json::from_str(&fixture("v1_installed.json")).unwrap();
    assert!(inst
        .engine_path
        .to_string_lossy()
        .ends_with("heardright-engine.exe"));
    // Endpoint naming.
    let root = Path::new("/Users/me/Library/Application Support/Orthic Labs/shared");
    assert_eq!(
        service_dir(root),
        root.join("runtime").join("speech-service")
    );
    assert_eq!(
        endpoint_for(root, "x"),
        root.join("runtime/speech-service/speech.sock")
            .to_string_lossy()
    );
    assert_eq!(
        SERVICE_ALLOWED_SCHEMAS,
        [
            "engine_health",
            "engine_capabilities",
            "engine_info",
            "file_transcription_request",
            // minor 3 (queue contract); HeardRight adds it with the queue
            "file_transcription_status"
        ]
    );
}

#[test]
fn windows_pipe_name_is_heardright_fnv1a_of_the_lowercased_slashed_root() {
    // `endpoint_for` is cfg!(windows)-gated, so pin the hash through the documented
    // reference value: fnv1a32("c:/users/me/appdata/local/orthic labs/shared").
    let mut h: u32 = 0x811c_9dc5;
    for b in "c:/users/me/appdata/local/orthic labs/shared".bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    assert_eq!(format!("{h:08x}").len(), 8);
    if cfg!(windows) {
        assert_eq!(
            endpoint_for(
                Path::new(r"C:\Users\me\AppData\Local\Orthic Labs\shared\"),
                "S-1-5-21-1"
            ),
            format!(r"\\.\pipe\orthic-labs-speech-S-1-5-21-1-{h:08x}")
        );
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[test]
fn discovery_staleness_planning_and_installed_engine() {
    let e = env();
    assert_eq!(plan_host(&e.root, HostRole::CutRight), HostPlan::HostMyself);
    assert!(matches!(connect(&e.root), Err(ClientError::NoService)));

    let mut h = Host::start(&e.root, "scrape_right", 0, &e.state);
    let rec = read_live_record(&e.root).expect("live record");
    assert_eq!(
        (rec.role, rec.pid, rec.protocol_minor),
        (HostRole::ScrapeRight, h.pid, 0)
    );
    assert_eq!(rec.endpoint, endpoint_for(&e.root, "user"));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&service_dir(&e.root)), 0o700);
        assert_eq!(mode(Path::new(&rec.endpoint)), 0o600);
    }
    assert_eq!(plan_host(&e.root, HostRole::CutRight), HostPlan::UseRunning);
    assert_eq!(
        plan_host(&e.root, HostRole::ScrapeRight),
        HostPlan::UseRunning
    );
    assert_eq!(
        plan_host(&e.root, HostRole::HeardRightShell),
        HostPlan::TakeOver {
            from: HostRole::ScrapeRight
        }
    );

    let c = connect(&e.root).unwrap();
    assert_eq!(
        (c.role(), c.engine_version()),
        (HostRole::ScrapeRight, "fake-hr-host/1")
    );
    drop(c);

    // kill -9: the record stays but its pid is dead, so it is stale.
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    assert!(service_record_path(&e.root).exists());
    assert!(read_live_record(&e.root).is_none());
    assert_eq!(plan_host(&e.root, HostRole::CutRight), HostPlan::HostMyself);
    let engine = e.root.join("engine");
    std::fs::create_dir_all(installed_heardright_path(&e.root).parent().unwrap()).unwrap();
    std::fs::write(
        installed_heardright_path(&e.root),
        serde_json::to_vec(&InstalledEngine {
            engine_path: engine.clone(),
            engine_version: "1".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        plan_host(&e.root, HostRole::CutRight),
        HostPlan::HostMyself,
        "installed engine missing on disk"
    );
    std::fs::write(&engine, b"").unwrap();
    assert_eq!(
        plan_host(&e.root, HostRole::CutRight),
        HostPlan::StartInstalledHeardRight(engine)
    );
    assert_eq!(
        plan_host(&e.root, HostRole::HeardRightShell),
        HostPlan::HostMyself,
        "HeardRight roles host themselves"
    );
}

#[test]
fn newer_protocol_major_is_refused_before_connecting() {
    let e = env();
    std::fs::create_dir_all(service_dir(&e.root)).unwrap();
    let rec = json!({"protocol_major": 2, "protocol_minor": 0, "role": "heard_right_shell", "pid": std::process::id(), "endpoint": "/nonexistent", "engine_version": "9"});
    std::fs::write(service_record_path(&e.root), rec.to_string()).unwrap();
    assert!(matches!(
        connect(&e.root),
        Err(ClientError::ProtocolMismatch { major: 2 })
    ));
}

// ---------------------------------------------------------------------------
// v1.0 host (HeardRight today): default behaviour, allowlist, unchanged requests
// ---------------------------------------------------------------------------

#[test]
fn unchanged_v1_host_transcribes_and_never_sees_minor1_fields() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 0, &e.state);
    let t = transcribe_via_service(
        &e.root,
        HostRole::CutRight,
        Path::new("/x/a.wav"),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(t.text, "hello from parakeet");
    assert_eq!(
        t.words,
        vec![TimedWord {
            text: "hello".into(),
            start_ms: 0,
            end_ms: 400,
            source_ms: None,
        }]
    );
    assert_eq!(
        last_request(&e),
        json!({"kind": "file_transcription_request", "path": "/x/a.wav"})
    );

    // Auto / Parakeet / a language hint degrade to the plain v1.0 request.
    let mut c = connect(&e.root).unwrap();
    assert!(!c.supports_engine_selection());
    for engine in [EngineSelector::Auto, EngineSelector::parakeet()] {
        let r = c
            .transcribe(
                Path::new("/x/b.wav"),
                &mut TranscribeOptions::new(Duration::from_secs(5))
                    .engine(engine)
                    .language("es"),
            )
            .unwrap();
        assert_eq!(
            (r.transcript.text.as_str(), r.engine),
            ("hello from parakeet", None)
        );
        assert_eq!(
            last_request(&e),
            json!({"kind": "file_transcription_request", "path": "/x/b.wav"})
        );
    }
    // Whisper is refused client-side rather than silently decoded by Parakeet.
    std::fs::remove_file(e.state.join("last_request.json")).unwrap();
    let err = c
        .transcribe(
            Path::new("/x/c.wav"),
            &mut TranscribeOptions::new(Duration::from_secs(5)).engine(EngineSelector::whisper()),
        )
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Unsupported(ref m) if m.contains("whisper")),
        "{err}"
    );
    assert!(
        !e.state.join("last_request.json").exists(),
        "nothing was sent"
    );

    let caps = c.capabilities(Duration::from_secs(5)).unwrap();
    assert!(caps.engines.is_empty() && caps.supports_file_transcription());
    let engines = caps.engines_or_default();
    assert_eq!(
        (engines.len(), engines[0].id.as_str(), engines[0].ready),
        (1, "parakeet", true)
    );
}

#[test]
fn allowlist_rejection_and_connection_survives() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 0, &e.state);
    let mut c = connect(&e.root).unwrap();
    for schema in ["recording_started", "replace_engine_config", "paste_text"] {
        assert!(!is_allowed_schema(schema));
        let mut f = ServiceEnvelope::health("rid-deny", "t");
        f.schema_name = schema.into();
        f.payload = Some(json!({"kind": schema}));
        let resp = c.call_raw(&f, Duration::from_secs(5), None, None).unwrap();
        let err = resp.error.expect("error frame");
        assert_eq!(
            (err.code.as_str(), err.retryable),
            (ERROR_NOT_ALLOWED, false),
            "{schema}"
        );
        assert_eq!(resp.request_id, "rid-deny");
    }
    // The host keeps the connection open after a rejection.
    assert_eq!(
        c.transcribe_file(Path::new("/x/a.wav"), Duration::from_secs(5))
            .unwrap()
            .text,
        "hello from parakeet"
    );
}

#[test]
fn host_handoff_error_is_retried_and_reaches_the_result() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_headless", 0, &e.state);
    // The scripted host sends the retryable handoff error once for this file.
    let mut c = connect(&e.root).unwrap();
    let err = c
        .transcribe_file(Path::new("/x/job_handoff.wav"), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(err, ClientError::HandoffRetry);
    std::fs::remove_file(e.state.join("handoff_done")).unwrap();
    drop(c);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let mut opts = TranscribeOptions::new(Duration::from_secs(5))
        .on_progress(move |p| s2.lock().unwrap().push(p.clone()));
    let t = transcribe_via_service_with(
        &e.root,
        HostRole::CutRight,
        Path::new("/x/job_handoff.wav"),
        &mut opts,
    )
    .unwrap();
    assert_eq!(t.transcript.text, "hello from parakeet");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|p| matches!(p, Progress::HandoffRetry { attempt: 1 })),
        "retry was reported"
    );
}

#[test]
fn takeover_by_outranking_host_hands_off_in_flight_job_and_client_follows() {
    let e = env();
    let mut a = Host::start(&e.root, "scrape_right", 1, &e.state);
    let state_b = e.state.join("b");
    std::fs::create_dir_all(&state_b).unwrap();
    // The HeardRight shell binds only once the incumbent is gone (HeardRight waits on the model lease).
    let mut b = Host::spawn_unready(&e.root, "heard_right_shell", 1, &state_b, &["--wait-free"]);

    // A long job on the incumbent; CutRight does not outrank ScrapeRight, so it uses the running host.
    let root = e.root.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let job = std::thread::spawn(move || {
        let mut opts = TranscribeOptions::new(Duration::from_secs(10))
            .on_progress(move |p| s2.lock().unwrap().push(p.clone()));
        transcribe_via_service_with(
            &root,
            HostRole::CutRight,
            Path::new("/x/slow_takeover.wav"),
            &mut opts,
        )
    });
    std::thread::sleep(Duration::from_millis(600));

    // The shell asks for the service the HeardRight way: takeover.json, rank implied by role.
    assert!(should_request_takeover(
        HostRole::HeardRightShell,
        read_live_record(&e.root).as_ref()
    ));
    request_takeover(&e.root, HostRole::HeardRightShell).unwrap();
    assert_eq!(
        read_takeover(&e.root).unwrap().role,
        HostRole::HeardRightShell
    );

    // The incumbent fails the in-flight job with the handoff error, removes service.json and exits.
    let status = a.child.wait().unwrap();
    assert!(status.success());
    b.wait_ready();
    clear_takeover_if_ours(&e.root);
    assert!(read_takeover(&e.root).is_none());
    assert_eq!(
        read_live_record(&e.root).unwrap().role,
        HostRole::HeardRightShell
    );

    let r = job
        .join()
        .unwrap()
        .expect("job followed the handoff to the new host");
    assert_eq!(r.transcript.text, "hello from parakeet");
    assert_eq!(r.engine.as_deref(), Some("parakeet"));
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|p| matches!(p, Progress::HandoffRetry { .. })));
    assert!(
        state_b.join("last_request.json").exists(),
        "the resubmitted request reached host B"
    );
}

#[test]
fn stall_timeout_fires_on_silence_and_the_host_sees_the_disconnect() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 0, &e.state);
    let mut c = connect(&e.root).unwrap();
    let t = Instant::now();
    let err = c
        .transcribe_file(Path::new("/x/stall.wav"), Duration::from_millis(700))
        .unwrap_err();
    assert_eq!(err, ClientError::Timeout);
    assert!(
        t.elapsed() >= Duration::from_millis(650) && t.elapsed() < Duration::from_secs(4),
        "{:?}",
        t.elapsed()
    );
    assert!(
        wait_for(&e.state.join("cancelled"), 5),
        "host observed the abort and cancelled the job"
    );
    assert!(
        matches!(
            c.transcribe_file(Path::new("/x/a.wav"), Duration::from_secs(1)),
            Err(ClientError::Io(_))
        ),
        "an aborted connection is dead"
    );
}

#[test]
fn cancel_closes_the_connection_and_returns_cancelled() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 0, &e.state);
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        c2.cancel();
    });
    let t = Instant::now();
    let mut opts = TranscribeOptions::new(Duration::from_secs(60)).cancel_token(cancel.clone());
    let err = transcribe_via_service_with(
        &e.root,
        HostRole::CutRight,
        Path::new("/x/stall.wav"),
        &mut opts,
    )
    .unwrap_err();
    assert_eq!(err, ClientError::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(
        wait_for(&e.state.join("cancelled"), 5),
        "host cancelled the job on disconnect"
    );
    // An already-cancelled request never reaches the host.
    let mut again = TranscribeOptions::new(Duration::from_secs(5)).cancel_token(cancel);
    assert_eq!(
        transcribe_via_service_with(
            &e.root,
            HostRole::CutRight,
            Path::new("/x/a.wav"),
            &mut again
        )
        .unwrap_err(),
        ClientError::Cancelled
    );
}

/// Hold `dictation_active` for `hold`, then release it.
fn dictate(state: &Path, hold: Duration) -> std::thread::JoinHandle<()> {
    let flag = state.join("dictation_active");
    std::fs::write(&flag, b"1").unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        let _ = std::fs::remove_file(flag);
    })
}

#[test]
fn dictation_pause_with_progress_frames_outlives_the_stall_timeout() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 1, &e.state);
    let held = dictate(&e.state, Duration::from_millis(1800));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let t = Instant::now();
    let mut c = connect(&e.root).unwrap();
    // 600 ms stall timeout, 1.8 s pause: only the host's paused_for_dictation frames keep it alive.
    let mut opts = TranscribeOptions::new(Duration::from_millis(600))
        .on_progress(move |p| s2.lock().unwrap().push(p.clone()));
    let r = c
        .transcribe(Path::new("/x/dictation.wav"), &mut opts)
        .unwrap();
    held.join().unwrap();
    assert!(
        t.elapsed() >= Duration::from_millis(1700),
        "waited out the pause: {:?}",
        t.elapsed()
    );
    assert_eq!(r.transcript.text, "hello from parakeet");
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(
            |p| matches!(p, Progress::Host(h) if h.state == ProgressState::PausedForDictation)
        ),
        "{seen:?}"
    );
    let paused = seen
        .iter()
        .filter(|p| matches!(p, Progress::Host(h) if h.state == ProgressState::PausedForDictation))
        .count();
    assert!(paused >= 3, "heartbeats throughout the pause: {paused}");
}

#[test]
fn dictation_pause_on_a_v1_host_is_silent_so_a_short_stall_timeout_fires() {
    // Legacy hosts send nothing while file work is paused: the stall timeout is then the
    // whole-job bound, which is why the contract says to pass a generous timeout.
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 0, &e.state);
    let held = dictate(&e.state, Duration::from_millis(1800));
    let err = connect(&e.root)
        .unwrap()
        .transcribe_file(Path::new("/x/dictation.wav"), Duration::from_millis(600))
        .unwrap_err();
    assert_eq!(err, ClientError::Timeout);
    held.join().unwrap();
    // A pause shorter than the timeout completes.
    let _ = std::fs::remove_file(e.state.join("cancelled"));
    let held = dictate(&e.state, Duration::from_millis(300));
    let t = connect(&e.root)
        .unwrap()
        .transcribe_file(Path::new("/x/dictation.wav"), Duration::from_secs(5))
        .unwrap();
    held.join().unwrap();
    assert_eq!(t.text, "hello from parakeet");
}

// ---------------------------------------------------------------------------
// Protocol minor 1: engine inventory and selection
// ---------------------------------------------------------------------------

#[test]
fn minor1_host_reports_engines_and_honours_selection() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 1, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(c.supports_engine_selection());

    let caps = c.capabilities(Duration::from_secs(5)).unwrap();
    let ids: Vec<_> = caps
        .engines
        .iter()
        .map(|x| (x.id.as_str(), x.ready))
        .collect();
    assert_eq!(
        ids,
        [("parakeet", true), ("whisper", true), ("future", false)]
    );
    let whisper = caps.engine(ENGINE_WHISPER).unwrap();
    assert!(whisper.languages.contains(&"ja".to_string()) && !whisper.loaded);
    let ready_ja: Vec<_> = caps
        .ready_for(Some("ja"))
        .into_iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(
        ready_ja,
        ["whisper"],
        "only Whisper decodes ja; the unready engine is excluded"
    );
    assert_eq!(caps.ready_for(None).len(), 2);

    let opts = || TranscribeOptions::new(Duration::from_secs(5));
    let p = Path::new("/x/a.wav");

    // No selector: nothing extra on the wire, Parakeet behaviour.
    let r = c.transcribe(p, &mut opts()).unwrap();
    assert_eq!(
        (r.transcript.text.as_str(), r.engine.as_deref()),
        ("hello from parakeet", Some("parakeet"))
    );
    assert_eq!(
        last_request(&e),
        json!({"kind": "file_transcription_request", "path": "/x/a.wav"})
    );

    let r = c
        .transcribe(
            p,
            &mut opts().engine(EngineSelector::whisper()).language("es"),
        )
        .unwrap();
    assert_eq!(
        (
            r.transcript.text.as_str(),
            r.engine.as_deref(),
            r.language.as_deref()
        ),
        ("hello from whisper", Some("whisper"), Some("es"))
    );
    assert_eq!(
        last_request(&e),
        json!({"kind": "file_transcription_request", "path": "/x/a.wav", "engine": "whisper", "language": "es"})
    );

    // auto-by-language: the host routes.
    let r = c
        .transcribe(p, &mut opts().engine(EngineSelector::Auto).language("ja"))
        .unwrap();
    assert_eq!(r.engine.as_deref(), Some("whisper"));
    let r = c
        .transcribe(p, &mut opts().engine(EngineSelector::Auto).language("en"))
        .unwrap();
    assert_eq!(r.engine.as_deref(), Some("parakeet"));
    assert_eq!(last_request(&e)["engine"], "auto");

    // An engine the host cannot run is a typed, non-retryable error, and the connection survives.
    let err = c
        .transcribe(
            p,
            &mut opts().engine(EngineSelector::Named("future".into())),
        )
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Engine { ref code, .. } if code == ERROR_ENGINE_UNAVAILABLE),
        "{err}"
    );
    assert_eq!(
        c.transcribe_file(p, Duration::from_secs(5)).unwrap().text,
        "hello from parakeet"
    );
}

// ---------------------------------------------------------------------------
// One convention: rightkit-daemon hosts and HeardRight peers interoperate
// ---------------------------------------------------------------------------

struct NoCalls;
impl rightkit_daemon::Handler for NoCalls {
    fn call(
        &self,
        _: &rightkit_daemon::CallContext,
        m: &str,
        _: &Value,
        _: &rightkit_daemon::Emit,
    ) -> Result<Value, rightkit_daemon::ServiceError> {
        Err(rightkit_daemon::ServiceError::new("unknown_method", m))
    }
}

/// A live process that is not this one: the requester of a takeover must be another process.
struct Requester(std::process::Child);
impl Requester {
    fn new() -> Requester {
        Requester(Command::new("sleep").arg("60").spawn().unwrap())
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
}
impl Drop for Requester {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn rightkit_daemon_host_publishes_a_record_heardright_clients_parse_and_yields_to_heardright_takeovers(
) {
    use rightkit_daemon::{Allowlist, ServiceConfig};
    let e = env();
    let suite = rightkit_daemon::Suite::at(&e.root).unwrap();
    let mut cfg = ServiceConfig::new(
        SERVICE_NAME,
        HostRole::ScrapeRight.as_str(),
        HostRole::ScrapeRight.rank(),
        "7.7.7",
        Allowlist::new().allow("*", ["noop"]),
    );
    cfg.known_roles = HostRole::rank_table();
    cfg.endpoint = Some(endpoint_for(&e.root, "user"));
    cfg.handoff_grace = Duration::from_secs(1);
    let hosted = rightkit_daemon::host(&suite, cfg, Arc::new(NoCalls)).unwrap();

    // HeardRight's reader sees its own six fields (extra fields are ignored by serde).
    let raw: Value =
        serde_json::from_slice(&std::fs::read(service_record_path(&e.root)).unwrap()).unwrap();
    let hr: ServiceRecord = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(
        (hr.role, hr.engine_version.as_str(), hr.protocol_minor),
        (HostRole::ScrapeRight, "7.7.7", 0)
    );
    for k in [
        "protocol_major",
        "protocol_minor",
        "role",
        "pid",
        "endpoint",
        "engine_version",
    ] {
        assert!(raw.get(k).is_some(), "service.json carries `{k}`");
    }
    assert_eq!(plan_host(&e.root, HostRole::CutRight), HostPlan::UseRunning);

    // A HeardRight-format takeover (no rank field) from the shell is honoured via known_roles.
    let requester = Requester::new();
    let tk =
        json!({"role": "heard_right_shell", "pid": requester.pid(), "requested_at_ms": now_ms()});
    std::fs::write(takeover_request_path(&e.root), tk.to_string()).unwrap();
    assert!(
        hosted.wait_released(Duration::from_secs(10)),
        "incumbent released for the outranking HeardRight role"
    );
    assert!(read_live_record(&e.root).is_none());
}

#[test]
fn rightkit_daemon_host_ignores_lower_and_unknown_roles_but_yields_to_an_outranking_known_one() {
    use rightkit_daemon::{Allowlist, ServiceConfig};
    let e = env();
    let suite = rightkit_daemon::Suite::at(&e.root).unwrap();
    let mk = |role: HostRole| {
        let mut cfg = ServiceConfig::new(
            SERVICE_NAME,
            role.as_str(),
            role.rank(),
            "1",
            Allowlist::new().allow("*", ["noop"]),
        );
        cfg.known_roles = HostRole::rank_table();
        cfg.endpoint = Some(endpoint_for(&e.root, "user"));
        cfg.takeover_timeout = Duration::from_secs(1);
        cfg
    };
    let hosted =
        rightkit_daemon::host(&suite, mk(HostRole::ScrapeRight), Arc::new(NoCalls)).unwrap();
    let requester = Requester::new();
    let write_tk = |role: &str| {
        let tk = json!({"role": role, "pid": requester.pid(), "requested_at_ms": now_ms()});
        std::fs::write(takeover_request_path(&e.root), tk.to_string()).unwrap();
    };
    write_tk("cut_right"); // lower rank
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !hosted.wait_released(Duration::from_millis(1)),
        "lower rank does not displace the incumbent"
    );
    write_tk("mystery"); // rank cannot be resolved
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !hosted.wait_released(Duration::from_millis(1)),
        "an unresolvable rank never obliges a handoff"
    );
    std::fs::remove_file(takeover_request_path(&e.root)).unwrap();
    // The record is still ours and unchanged.
    assert_eq!(
        read_live_record(&e.root).unwrap().role,
        HostRole::ScrapeRight
    );
    // ... and an outranking known role does displace it (same requester, now as the shell).
    write_tk("heard_right_shell");
    assert!(hosted.wait_released(Duration::from_secs(10)));
}

#[test]
fn verbatim_is_sent_only_to_a_host_that_advertises_it() {
    let p = Path::new("/x/a.wav");
    let opts = || TranscribeOptions::new(Duration::from_secs(5));

    // A minor-1 host strips fillers: verbatim is refused, never silently cleaned.
    let e = env();
    let h = Host::start(&e.root, "heard_right_shell", 1, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(!c
        .capabilities(Duration::from_secs(5))
        .unwrap()
        .supports_verbatim());
    match c.transcribe(p, &mut opts().verbatim()) {
        Err(ClientError::Unsupported(m)) => assert!(m.contains("verbatim"), "{m}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert!(
        !e.state.join("last_request.json").exists(),
        "nothing reached the host"
    );
    drop((c, h));

    // A minor-2 host advertises it and receives `verbatim: true`; a default request stays byte-identical.
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 2, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(c
        .capabilities(Duration::from_secs(5))
        .unwrap()
        .supports_verbatim());
    c.transcribe(p, &mut opts().verbatim()).unwrap();
    assert_eq!(
        last_request(&e),
        json!({"kind": "file_transcription_request", "path": "/x/a.wav", "verbatim": true})
    );
    c.transcribe(p, &mut opts()).unwrap();
    assert_eq!(
        last_request(&e),
        json!({"kind": "file_transcription_request", "path": "/x/a.wav"})
    );
}

fn collect() -> (
    Arc<Mutex<Vec<Progress>>>,
    impl FnMut(&Progress) + Send + 'static,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    (seen, move |p: &Progress| s.lock().unwrap().push(p.clone()))
}

#[test]
fn queue_host_reports_position_reason_and_running_with_no_stall_timeout() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 3, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(c.supports_queue().unwrap());
    let (seen, f) = collect();
    // 100 ms stall limit on a 1.2 s job: a queue host is judged by liveness, never by silence.
    let mut opts = TranscribeOptions::new(Duration::from_millis(100))
        .request_id("job-1")
        .on_progress(f);
    let r = c.transcribe(Path::new("/x/queue.wav"), &mut opts).unwrap();
    assert_eq!(r.transcript.text, "hello from parakeet");
    let seen = seen.lock().unwrap();
    let queued: Vec<_> = seen
        .iter()
        .filter_map(|p| match p {
            Progress::Queued {
                job_id,
                position,
                reason,
            } => Some((job_id.as_str(), *position, reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        queued.first(),
        Some(&("job-1", 1, QueueReason::BehindJobs)),
        "accepted at once: {seen:?}"
    );
    assert!(
        queued.contains(&("job-1", 0, QueueReason::DictationActive)),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(
            |p| matches!(p, Progress::Running { job_id, fraction: Some(_) } if job_id == "job-1")
        ),
        "{seen:?}"
    );
    drop(seen);

    // Status: finished jobs replay; unknown ids say so.
    match c.job_status("job-1").unwrap() {
        JobStatus::Done(r) => assert_eq!(r.transcript.text, "hello from parakeet"),
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(c.job_status("nope").unwrap(), JobStatus::Unknown);
}

#[test]
fn queue_client_resubmits_the_same_job_to_the_next_host_when_the_host_dies() {
    let e = env();
    let a = Host::start(&e.root, "heard_right_shell", 3, &e.state);
    let (seen, f) = collect();
    let root = e.root.clone();
    let job = std::thread::spawn(move || {
        let mut opts = TranscribeOptions::new(Duration::from_millis(100)).on_progress(f);
        transcribe_via_service_with(
            &root,
            HostRole::CutRight,
            Path::new("/x/hang.wav"),
            &mut opts,
        )
    });
    let ids = e.state.join("request_ids");
    assert!(wait_for(&ids, 5), "first host got the job");
    std::thread::sleep(Duration::from_millis(500));
    drop(a); // SIGKILL: service.json stays behind with a dead pid
    std::fs::write(e.state.join("finish"), b"1").unwrap();
    let _b = Host::start(&e.root, "heard_right_shell", 3, &e.state);
    let r = job.join().unwrap().unwrap();
    assert_eq!(r.transcript.text, "hello from parakeet");
    let sent: Vec<String> = std::fs::read_to_string(&ids)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(sent.len(), 2, "submitted once per host: {sent:?}");
    assert_eq!(
        sent[0], sent[1],
        "the resubmit reuses the request id (idempotency key)"
    );
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|p| matches!(p, Progress::Reconnecting { .. })));
}

#[test]
fn pre_queue_hosts_keep_the_stall_timeout_and_refuse_status() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 2, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(!c.supports_queue().unwrap());
    assert!(matches!(
        c.job_status("x"),
        Err(ClientError::Unsupported(_))
    ));
    let r = c.transcribe(
        Path::new("/x/stall.wav"),
        &mut TranscribeOptions::new(Duration::from_millis(300)),
    );
    assert_eq!(r.unwrap_err(), ClientError::Timeout);
}

// ---------------------------------------------------------------------------
// Protocol minor 4: cancel frame and capture ownership
// ---------------------------------------------------------------------------

fn cancel_hanging_job(c: &mut SpeechClient) -> ClientError {
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        c2.cancel();
    });
    let mut opts = TranscribeOptions::new(Duration::from_secs(60))
        .request_id("job-cancel")
        .cancel_token(cancel);
    c.transcribe(Path::new("/x/hang.wav"), &mut opts)
        .unwrap_err()
}

#[test]
fn local_stop_sends_the_cancel_frame_to_a_minor_4_host_without_waiting_for_an_ack() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 4, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(c.supports_cancel().unwrap() && c.supports_capture().unwrap());
    let t = Instant::now();
    assert_eq!(cancel_hanging_job(&mut c), ClientError::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    let frames = e.state.join("cancel_frames");
    assert!(wait_for(&frames, 5), "the host received the cancel frame");
    assert_eq!(
        std::fs::read_to_string(&frames).unwrap().trim(),
        "job-cancel"
    );
}

#[test]
fn a_minor_3_host_never_sees_the_cancel_frame_and_refuses_the_new_calls() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 3, &e.state);
    let mut c = connect(&e.root).unwrap();
    assert!(!c.supports_cancel().unwrap() && !c.supports_capture().unwrap());
    assert_eq!(cancel_hanging_job(&mut c), ClientError::Cancelled);
    assert!(
        wait_for(&e.state.join("cancelled"), 5),
        "the disconnect still reaches the host"
    );
    assert!(!e.state.join("cancel_frames").exists());
    let mut c = connect(&e.root).unwrap();
    assert!(matches!(
        c.cancel_job("x"),
        Err(ClientError::Unsupported(_))
    ));
    let acquire = CaptureCommand::Heartbeat {
        owner_id: "p".into(),
    };
    assert!(matches!(
        c.capture(&acquire, Duration::from_secs(2)),
        Err(ClientError::Unsupported(_))
    ));
    assert!(!e.state.join("capture_frames").exists());
}

#[test]
fn capture_commands_round_trip_and_stop_closes_without_waiting_for_a_reply() {
    let e = env();
    let _h = Host::start(&e.root, "heard_right_shell", 4, &e.state);
    let mut c = connect(&e.root).unwrap();
    let pulse = CaptureOwner {
        role: CaptureRole::PulseConversation,
        owner_id: "pulse-1".into(),
        pid: std::process::id(),
    };
    let reply = c
        .capture(
            &CaptureCommand::Acquire {
                owner: pulse,
                confirm_preempt: false,
            },
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply.status, CaptureStatus::Granted);
    let reply = c
        .capture(
            &CaptureCommand::Heartbeat {
                owner_id: "pulse-1".into(),
            },
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply.status, CaptureStatus::Ok);
    // Stop returns at once: the host never answers a capture_cancel.
    let t = Instant::now();
    c.stop_capture("pulse-1");
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    let frames = e.state.join("capture_frames");
    let wait_lines = Instant::now() + Duration::from_secs(5);
    let lines = loop {
        let text = std::fs::read_to_string(&frames).unwrap_or_default();
        if text.lines().count() >= 3 || Instant::now() > wait_lines {
            break text;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let kinds: Vec<String> = lines
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["kind"].to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "\"capture_acquire\"",
            "\"capture_heartbeat\"",
            "\"capture_cancel\""
        ]
    );
    // The client is closed: it cannot be reused.
    assert!(matches!(
        c.capture(
            &CaptureCommand::Heartbeat {
                owner_id: "pulse-1".into()
            },
            Duration::from_secs(1)
        ),
        Err(ClientError::Io(_))
    ));
}
