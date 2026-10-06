//! End-to-end: a real engine process hosts the `job.*` contract on a
//! `rightkit-service` endpoint; the test is a real client over the socket.
//! Covers: interactive preempts batch at step boundaries, progress events,
//! cancel on disconnect, stall timeout without a total limit, and retryable
//! handoff to a higher-ranked host.

use rightkit_service::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "job_engine.exe"
        } else {
            "job_engine"
        })
}

struct Engine {
    child: Child,
    lines: std::sync::mpsc::Receiver<String>,
    data: PathBuf,
}
impl Engine {
    fn start(suite: &Path, data: &Path, role: &str, rank: u8, stall_ms: u64) -> Engine {
        let mut child = Command::new(bin())
            .args([
                suite.to_str().unwrap(),
                data.to_str().unwrap(),
                role,
                &rank.to_string(),
                &stall_ms.to_string(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn engine");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        let e = Engine {
            child,
            lines: rx,
            data: data.to_path_buf(),
        };
        assert_eq!(e.line(30), "READY");
        e
    }
    fn line(&self, secs: u64) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(secs))
            .unwrap_or_default()
    }
    fn log(&self) -> Vec<(u128, String, String, i64)> {
        std::fs::read_to_string(self.data.join("steps.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split(' ').collect();
                Some((
                    p[0].parse().ok()?,
                    p[1].into(),
                    p[2].into(),
                    p[3].parse().ok()?,
                ))
            })
            .collect()
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Rig {
    _d: tempfile::TempDir,
    suite: Suite,
    root: PathBuf,
}
fn rig() -> Rig {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().to_path_buf();
    let suite = Suite::at(root.join("suite")).unwrap();
    Rig { _d: d, suite, root }
}

fn submit(kind: &str, rid: &str, request: Value, extra: Value) -> Value {
    let mut p = json!({"kind": kind, "request_id": rid, "request": request, "run": true});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        p[k] = v;
    }
    p
}

#[test]
fn interactive_preempts_batch_at_step_boundaries_with_progress() {
    let r = rig();
    let eng = Engine::start(r.suite.root(), &r.root.join("a"), "headless", 2, 0);
    let stages: Arc<Mutex<Vec<String>>> = Arc::default();
    let (s2, st2) = (r.suite.clone(), stages.clone());
    let batch = std::thread::spawn(move || {
        let c = Client::connect(&s2, "jobs-demo", "alpha").unwrap();
        c.call_events(
            "job.submit",
            submit(
                "windows",
                "file-1",
                json!({"steps": 10, "step_ms": 120}),
                json!({}),
            ),
            &mut |ev| {
                st2.lock()
                    .unwrap()
                    .push(ev["stage"].as_str().unwrap_or("").to_string())
            },
        )
    });
    std::thread::sleep(Duration::from_millis(450));
    let c2 = Client::connect(&r.suite, "jobs-demo", "alpha").unwrap();
    let dict = c2
        .call(
            "job.submit",
            submit(
                "dictation",
                "dict-1",
                json!({"priority": "interactive", "ms": 800}),
                json!({}),
            ),
        )
        .unwrap();
    assert_eq!(dict["status"], "complete");
    let b = batch.join().unwrap().unwrap();
    assert_eq!(b["status"], "complete");
    assert_eq!(b["attempts"], 1, "preemption is a pause, not a retry");

    let log = eng.log();
    let t = |what: &str| log.iter().find(|e| e.1 == what).unwrap().0;
    let (d0, d1) = (t("dictation_start"), t("dictation_end"));
    let starts_during: Vec<_> = log
        .iter()
        .filter(|e| e.1 == "step_start" && e.0 > d0 + 30 && e.0 < d1)
        .collect();
    assert!(
        starts_during.is_empty(),
        "no batch step may begin while interactive work runs: {starts_during:?}"
    );
    let steps_done = log.iter().filter(|e| e.1 == "step_end").count();
    assert_eq!(steps_done, 10, "batch resumed and finished every window");
    let st = stages.lock().unwrap().clone();
    for want in ["started", "step", "preempted", "resumed", "complete"] {
        assert!(
            st.iter().any(|s| s == want),
            "progress stream lacks {want}: {st:?}"
        );
    }
    assert!(st.iter().position(|s| s == "preempted") < st.iter().position(|s| s == "resumed"));

    let beta = Client::connect(&r.suite, "jobs-demo", "beta").unwrap();
    assert_eq!(
        beta.call("job.submit", json!({"kind": "windows"}))
            .unwrap_err()
            .code,
        ERROR_NOT_ALLOWED,
        "per-app allowlist covers job methods"
    );
    assert_eq!(
        beta.call("job.status", json!({"request_id": "file-1"}))
            .unwrap()["status"],
        "complete"
    );
}

#[test]
fn disconnect_cancels_a_job_that_asked_for_it() {
    let r = rig();
    let eng = Engine::start(r.suite.root(), &r.root.join("a"), "headless", 2, 0);
    let c = Arc::new(Client::connect(&r.suite, "jobs-demo", "alpha").unwrap());
    let c2 = c.clone();
    let t = std::thread::spawn(move || {
        c2.call(
            "job.submit",
            submit(
                "windows",
                "long",
                json!({"steps": 200, "step_ms": 50}),
                json!({"cancel_on_disconnect": true}),
            ),
        )
    });
    std::thread::sleep(Duration::from_millis(500));
    c.abort();
    assert_eq!(t.join().unwrap().unwrap_err().code, ERROR_CONNECTION_CLOSED);

    let obs = Client::connect(&r.suite, "jobs-demo", "alpha").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut status = String::new();
    while Instant::now() < until {
        status = obs
            .call("job.status", json!({"request_id": "long"}))
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .into();
        if status == "cancelled" {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(status, "cancelled");
    std::thread::sleep(Duration::from_millis(400));
    let n1 = eng.log().len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        n1,
        eng.log().len(),
        "the handler stopped working after the disconnect"
    );
    let evs = obs
        .call("job.events", json!({"request_id": "long"}))
        .unwrap();
    assert!(evs["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["stage"] == "cancelled"));
}

#[test]
fn stall_timeout_pauses_a_silent_job_but_never_caps_total_duration() {
    let r = rig();
    let _eng = Engine::start(r.suite.root(), &r.root.join("a"), "headless", 2, 700);
    let c = Client::connect(&r.suite, "jobs-demo", "alpha").unwrap();

    let t0 = Instant::now();
    let hung = c
        .call("job.submit", submit("hang", "hang-1", json!({}), json!({})))
        .unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "stall detected near the timeout, not after a total cap"
    );
    assert_eq!(hung["error"]["code"], "stalled");
    assert_eq!(hung["error"]["retryable"], true);
    assert_eq!(
        hung["recoverable"], true,
        "a stalled run pauses; it is recoverable"
    );

    let t0 = Instant::now();
    let steady = c
        .call(
            "job.submit",
            submit("steady", "steady-1", json!({}), json!({})),
        )
        .unwrap();
    assert_eq!(steady["status"], "complete");
    assert!(
        t0.elapsed() > Duration::from_millis(2500),
        "ran 3s under a 0.7s stall timeout because it kept making progress"
    );
    assert_eq!(steady["attempts"], 1);
}

#[test]
fn higher_ranked_host_takes_over_and_the_client_resubmits() {
    let r = rig();
    let mut a = Engine::start(r.suite.root(), &r.root.join("a"), "headless", 2, 0);
    let s2 = r.suite.clone();
    let job = std::thread::spawn(move || {
        Client::call_retrying(
            &s2,
            "jobs-demo",
            "alpha",
            "job.submit",
            submit(
                "windows",
                "file-h",
                json!({"steps": 30, "step_ms": 100}),
                json!({}),
            ),
            RetryPolicy::default(),
            &mut |_| {},
        )
    });
    std::thread::sleep(Duration::from_millis(700));
    let b = Engine::start(r.suite.root(), &r.root.join("b"), "shell", 0, 0);
    let res = job
        .join()
        .unwrap()
        .expect("handoff is retryable; the retry completes on the new host");
    assert_eq!(res["status"], "complete");
    let result = Client::connect(&r.suite, "jobs-demo", "alpha")
        .unwrap()
        .call("job.result", json!({"request_id": "file-h"}))
        .unwrap();
    assert_eq!(
        result["result"]["host"], "shell",
        "the job finished on the higher-ranked host"
    );
    assert_eq!(a.line(10), "RELEASED");
    assert!(a.child.wait().unwrap().success());
    let a_steps = a.log().iter().filter(|e| e.1 == "step_end").count();
    assert!(
        a_steps > 0 && a_steps < 30,
        "the old host stopped at a step boundary ({a_steps} windows)"
    );
    assert_eq!(
        b.log().iter().filter(|e| e.1 == "step_end").count(),
        30,
        "the new host reran the whole request"
    );
}
