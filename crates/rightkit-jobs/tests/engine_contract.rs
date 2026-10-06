use std::io::{BufReader, Cursor};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_jobs::*;
use serde_json::{json, Value};

/// Writes `out.txt` under the job scratch dir; `{"fail":"retry"}` pauses,
/// `{"cancel":true}` cancels itself mid-run.
struct Handler {
    scratch: PathBuf,
    runs: Arc<AtomicUsize>,
    store: Mutex<Option<Arc<JobStore>>>,
}
impl JobHandler for Handler {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        ctl.progress("work", Some(0.5), None);
        if job.request["fail"] == "retry" && job.attempts == 1 {
            return Err(JobError::retryable("offline", "no network"));
        }
        if job.request["cancel"] == true {
            let s = self.store.lock().unwrap().clone().unwrap();
            s.cancel(ctl.job_id())?;
        }
        let dir = self.scratch.join(&job.job_id);
        std::fs::create_dir_all(&dir)?;
        let f = dir.join("out.txt");
        std::fs::write(&f, b"hello")?;
        ctl.add_artifact(&f, "text/plain")?;
        Ok(json!({"ok": true}))
    }
}

struct Env {
    _dir: tempfile::TempDir,
    runner: Arc<Runner>,
    runs: Arc<AtomicUsize>,
    scratch: PathBuf,
}
fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JobStore::open(dir.path().join("jobs.db")).unwrap());
    let runs = Arc::new(AtomicUsize::new(0));
    let scratch = dir.path().join("scratch");
    let h = Arc::new(Handler {
        scratch: scratch.clone(),
        runs: runs.clone(),
        store: Mutex::new(Some(store.clone())),
    });
    let runner = Arc::new(Runner::new(
        store,
        JobLocks::new(dir.path().join("locks")),
        h,
    ));
    Env {
        _dir: dir,
        runner,
        runs,
        scratch,
    }
}
fn new(rid: Option<&str>, request: Value) -> NewJob {
    NewJob {
        request_id: rid.map(Into::into),
        kind: "t".into(),
        request,
    }
}

#[test]
fn dedup_recovery_artifacts_and_cancel() {
    let e = env();
    let store = e.runner.store().clone();
    let (a, d1) = store.submit(&new(Some("r1"), json!({}))).unwrap();
    let (b, d2) = store.submit(&new(Some("r1"), json!({}))).unwrap();
    assert!(!d1 && d2 && a.job_id == b.job_id, "request_id dedups");

    // Passive reads never run or revive anything.
    store.get(&a.job_id).unwrap();
    store.list(&ListFilter::default()).unwrap();
    assert_eq!(e.runner.recoverable_ids().unwrap(), vec![a.job_id.clone()]);
    assert_eq!(e.runs.load(Ordering::SeqCst), 0);

    // A live holder of the lock is not recoverable.
    let held = e.runner.locks().try_lock(&a.job_id).unwrap().unwrap();
    assert!(e.runner.recoverable_ids().unwrap().is_empty());
    assert!(matches!(
        e.runner.run(&a.job_id).unwrap(),
        RunOutcome::LockedElsewhere(_)
    ));
    drop(held);

    // Simulated crash: attempt began, never finished. Explicit resume completes once.
    store.begin_attempt(&a.job_id).unwrap();
    let outs = e.runner.resume_recoverable().unwrap();
    let RunOutcome::Finished(done) = &outs[0] else {
        panic!("{outs:?}")
    };
    assert_eq!(done.status, COMPLETE);
    assert_eq!(e.runs.load(Ordering::SeqCst), 1);
    assert!(e.runner.recoverable_ids().unwrap().is_empty());

    // Artifact carries sha256 (of "hello") and detects tampering.
    let art = store.artifacts(&a.job_id).unwrap().remove(0);
    assert_eq!(
        art.sha256,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert!(store.verify_artifact(&art).unwrap());
    std::fs::write(&art.path, b"HELLO").unwrap();
    assert!(!store.verify_artifact(&art).unwrap());
    assert!(store
        .events_after(&a.job_id, 0)
        .unwrap()
        .iter()
        .any(|e| e.stage == "complete"));

    // Retryable error pauses (recoverable); second attempt completes.
    let (p, _) = store.submit(&new(None, json!({"fail": "retry"}))).unwrap();
    assert!(matches!(
        e.runner.run(&p.job_id).unwrap(),
        RunOutcome::Paused(_)
    ));
    assert_eq!(e.runner.recoverable_ids().unwrap(), vec![p.job_id.clone()]);
    assert!(
        matches!(e.runner.run(&p.job_id).unwrap(), RunOutcome::Finished(j) if j.status == COMPLETE)
    );

    // Cancel during a run wins over completion.
    let (c, _) = store.submit(&new(None, json!({"cancel": true}))).unwrap();
    assert!(
        matches!(e.runner.run(&c.job_id).unwrap(), RunOutcome::Finished(j) if j.status == CANCELLED)
    );

    // Orphan scratch of a finished job is swept; live jobs keep theirs.
    assert!(e.scratch.join(&a.job_id).exists());
    let removed = e.runner.sweep_orphan_scratch(&e.scratch).unwrap();
    assert!(removed.contains(&a.job_id) && !e.scratch.join(&a.job_id).exists());
}

#[test]
fn stdio_contract_streams_events_and_resyncs() {
    let e = env();
    let d = Arc::new(
        Dispatcher::new("test-engine", "1.0.0").with(Arc::new(JobCommands::new(e.runner.clone()))),
    );
    let input = [
        "this is not json",
        r#"{"type":"request","id":1,"method":"ping"}"#,
        r#"{"type":"request","id":2,"method":"job.submit","params":{"kind":"t","request_id":"x","request":{},"run":true}}"#,
        r#"{"type":"request","id":3,"method":"job.status","params":{"request_id":"x"}}"#,
        r#"{"type":"request","id":4,"method":"nope"}"#,
        r#"{"type":"request","id":5,"method":"shutdown"}"#,
    ]
    .join("\n");
    let out = SharedBuf::default();
    serve(d, BufReader::new(Cursor::new(input)), out.clone(), false).unwrap();
    let frames: Vec<Value> = String::from_utf8(out.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(frames[0]["type"], "hello");
    assert_eq!(frames[0]["contract_version"], CONTRACT_VERSION);
    let resp = |id: u64| {
        frames
            .iter()
            .find(|f| f["type"] == "response" && f["id"] == id)
            .unwrap()
    };
    assert_eq!(
        resp(0)["error"]["code"],
        "bad_request",
        "bad frame answered, stream continues"
    );
    assert_eq!(resp(1)["result"]["pong"], true);
    assert_eq!(resp(2)["result"]["status"], COMPLETE);
    let pos = |pred: &dyn Fn(&Value) -> bool| frames.iter().position(pred).unwrap();
    let last_event = frames
        .iter()
        .rposition(|f| f["type"] == "event" && f["id"] == 2)
        .unwrap();
    assert!(
        last_event < pos(&|f| f["type"] == "response" && f["id"] == 2),
        "events precede response"
    );
    // Long requests run concurrently: id 3 may answer before id 2 finishes, but always answers.
    assert!(resp(3)["ok"] == true || resp(3)["error"]["code"] == "job_not_found");
    assert_eq!(
        e.runner.store().get_by_request("x").unwrap().status,
        COMPLETE
    );
    assert_eq!(resp(4)["error"]["code"], "unknown_method");
    assert_eq!(resp(5)["ok"], true);
}

#[test]
fn client_cli_roundtrip_over_pipes() {
    let e = env();
    let d = Arc::new(
        Dispatcher::new("test-engine", "1.0.0").with(Arc::new(JobCommands::new(e.runner.clone()))),
    );
    let (req_r, req_w) = std::io::pipe().unwrap();
    let (res_r, res_w) = std::io::pipe().unwrap();
    let dd = d.clone();
    let server =
        std::thread::spawn(move || serve(dd, BufReader::new(req_r), res_w, false).unwrap());
    let client = EngineClient::connect(BufReader::new(res_r), req_w).unwrap();
    assert_eq!(client.hello()["engine"], "test-engine");
    let events = Arc::new(AtomicUsize::new(0));
    let ev = events.clone();
    let r = client
        .call_with_events(
            "job.submit",
            json!({"kind": "t", "run": true}),
            Some(Box::new(move |_| {
                ev.fetch_add(1, Ordering::SeqCst);
            })),
        )
        .unwrap();
    assert_eq!(r["status"], COMPLETE);
    assert!(events.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        client
            .call("job.result", json!({"job_id": r["job_id"]}))
            .unwrap()["artifacts"][0]["verified"],
        true
    );
    assert_eq!(
        client.call("job.nope", json!({})).unwrap_err().code,
        "unknown_method"
    );
    client.shutdown(std::time::Duration::from_secs(1));
    server.join().unwrap();

    // CLI: same dispatcher, request-id dedup, error envelope, stdin untouched.
    let run = |args: &[&str]| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli(
            &d,
            args.iter().map(|s| s.to_string()),
            &mut std::io::empty(),
            &mut out,
            &mut err,
        );
        (code, serde_json::from_slice::<Value>(&out).unwrap())
    };
    let (c1, a) = run(&[
        "job",
        "submit",
        "--input",
        r#"{"kind":"t"}"#,
        "--request-id",
        "q",
    ]);
    let (_, b) = run(&[
        "job",
        "submit",
        "--input",
        r#"{"kind":"t"}"#,
        "--request-id",
        "q",
    ]);
    assert_eq!(
        (c1, a["job_id"].clone(), b["deduplicated"].clone()),
        (0, b["job_id"].clone(), json!(true))
    );
    let (c, v) = run(&["job", "status", "missing"]);
    assert_eq!((c, v["error"]["code"].as_str()), (1, Some("job_not_found")));
    assert_eq!(run(&["bogus"]).0, 2);
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An event callback may call back into the client: callbacks run on the
/// caller's thread, not under the pending-map lock or on the reader.
#[test]
fn event_callback_can_reenter_the_client() {
    let e = env();
    let d = Arc::new(
        Dispatcher::new("test-engine", "1.0.0").with(Arc::new(JobCommands::new(e.runner.clone()))),
    );
    let (req_r, req_w) = std::io::pipe().unwrap();
    let (res_r, res_w) = std::io::pipe().unwrap();
    let dd = d.clone();
    let server =
        std::thread::spawn(move || serve(dd, BufReader::new(req_r), res_w, false).unwrap());
    let client = Arc::new(EngineClient::connect(BufReader::new(res_r), req_w).unwrap());
    let inner = client.clone();
    let pongs = Arc::new(AtomicUsize::new(0));
    let p = pongs.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let c2 = client.clone();
    std::thread::spawn(move || {
        let r = c2.call_with_events(
            "job.submit",
            json!({"kind": "t", "run": true}),
            Some(Box::new(move |_| {
                if inner
                    .call_timeout("ping", json!({}), None, std::time::Duration::from_secs(5))
                    .is_ok()
                {
                    p.fetch_add(1, Ordering::SeqCst);
                }
            })),
        );
        tx.send(r).unwrap();
    });
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("re-entrant callback deadlocked")
        .unwrap();
    assert_eq!(r["status"], COMPLETE);
    assert!(
        pongs.load(Ordering::SeqCst) >= 2,
        "callback re-entered the client"
    );
    client.shutdown(std::time::Duration::from_secs(1));
    server.join().unwrap();
}

/// A real child that says hello, then never answers and ignores SIGTERM.
#[cfg(unix)]
#[test]
fn shutdown_of_a_hung_engine_is_bounded_and_kills_the_child() {
    use rightkit_process::OwnedCommand;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!(
        r#"trap '' TERM; echo $$ > '{}'; echo '{{"type":"hello","engine":"hung","version":"0","contract_version":1}}'; while :; do sleep 1; done"#,
        pidfile.display()
    );
    let mut cmd = OwnedCommand::new("sh");
    cmd.command_mut().args(["-c", &script]);
    let client = EngineClient::spawn(cmd).unwrap();
    assert_eq!(client.hello()["engine"], "hung");
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(pid));

    // A bounded call against the hung engine times out instead of hanging.
    let t = Instant::now();
    let err = client
        .call_timeout("job.status", json!({}), None, Duration::from_millis(200))
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(t.elapsed() < Duration::from_secs(3));

    // Shutdown reaches the kill deadline even though the RPC never completes.
    let t = Instant::now();
    client.shutdown(Duration::from_millis(300));
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "shutdown took {:?}",
        t.elapsed()
    );
    let mut gone = false;
    for _ in 0..50 {
        if !alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(gone, "hung engine pid {pid} still alive after shutdown");
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Real child that never sends hello: spawn gives up, kills it, typed error.
#[cfg(unix)]
#[test]
fn spawn_bounds_the_hello_wait_and_kills_a_silent_child() {
    use rightkit_process::OwnedCommand;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!(
        "trap '' TERM; echo $$ > '{}'; while :; do sleep 1; done",
        pidfile.display()
    );
    let mut cmd = OwnedCommand::new("sh");
    cmd.command_mut().args(["-c", &script]);
    let t = Instant::now();
    let err = EngineClient::spawn_with(cmd, Duration::from_millis(400))
        .err()
        .expect("must fail");
    assert_eq!(err.code, "hello_timeout");
    assert!(t.elapsed() < Duration::from_secs(5));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("silent engine {pid} survived");
}

/// Real child that says hello but never reads stdin: a request bigger than the
/// pipe buffer blocks the write; call_timeout must fail and kill the child.
#[cfg(unix)]
#[test]
fn call_timeout_covers_a_write_blocked_on_a_full_pipe() {
    use rightkit_process::OwnedCommand;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!(
        r#"echo $$ > '{}'; echo '{{"type":"hello","engine":"deaf","version":"0","contract_version":1}}'; while :; do sleep 1; done"#,
        pidfile.display()
    );
    let mut cmd = OwnedCommand::new("sh");
    cmd.command_mut().args(["-c", &script]);
    let client = EngineClient::spawn(cmd).unwrap();
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let big = "x".repeat(900_000); // under the frame limit, far over the pipe buffer
    let t = Instant::now();
    let err = client
        .call_timeout(
            "job.submit",
            json!({"blob": big}),
            None,
            Duration::from_millis(400),
        )
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "took {:?}",
        t.elapsed()
    );
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("deaf engine {pid} survived");
}
