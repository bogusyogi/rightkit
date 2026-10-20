//! M4 owner-gap coverage (window/CLI protocol): bounded frame limit, bounded disconnect drain,
//! one command table for window frames and CLI (aliases, method metadata).

use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_job_queue::*;
use serde_json::{json, Value};

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

fn frames(out: &SharedBuf) -> Vec<Value> {
    let bytes = out.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `hold` is a long request that runs until released; it sees disconnect cooperatively. `fast` is
/// a long request that answers at once.
struct Hold {
    release: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
}
impl CommandSet for Hold {
    fn methods(&self) -> Vec<String> {
        vec!["hold".into(), "fast".into(), "quick".into()]
    }
    fn is_long(&self, method: &str, _: &Value) -> bool {
        method == "hold" || method == "fast"
    }
    fn call(&self, method: &str, _: &Value, _: bool, _: &Sink) -> JobResult<Value> {
        if method == "hold" {
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.finished.store(true, Ordering::SeqCst);
        }
        Ok(json!({"done": method}))
    }
    fn connection_closed(&self, _conn: u64) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

struct Rig {
    d: Dispatcher,
    release: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
}
fn rig(d: Dispatcher) -> Rig {
    let release = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let set = Hold {
        release: Arc::clone(&release),
        closed: Arc::clone(&closed),
        finished: Arc::clone(&finished),
    };
    Rig {
        d: d.with(Arc::new(set)),
        release,
        closed,
        finished,
    }
}

const HOLD_THEN_EOF: &str = r#"{"type":"request","id":1,"method":"hold"}"#;

#[test]
fn eof_with_no_drain_timeout_still_waits_for_every_running_request() {
    let r = rig(Dispatcher::new("t", "1"));
    let release = Arc::clone(&r.release);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        release.store(true, Ordering::SeqCst);
    });
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(
        Arc::new(r.d),
        BufReader::new(Cursor::new(HOLD_THEN_EOF)),
        out.clone(),
        false,
    )
    .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "default is unchanged: it waits"
    );
    assert_eq!(report, ServeReport { detached: 0 });
    assert!(r.finished.load(Ordering::SeqCst));
    assert!(frames(&out)
        .iter()
        .any(|f| f["type"] == "response" && f["id"] == 1 && f["ok"] == true));
}

#[test]
fn a_drain_timeout_bounds_the_wait_after_eof_and_detaches_what_is_still_running() {
    let r = rig(Dispatcher::new("t", "1").with_drain_timeout(Duration::from_millis(200)));
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(
        Arc::new(r.d),
        BufReader::new(Cursor::new(HOLD_THEN_EOF)),
        out.clone(),
        false,
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "serve must return while the request is still running: {:?}",
        started.elapsed()
    );
    assert_eq!(report.detached, 1);
    // The close hook ran (cancellation is requested) but the work was not stopped by serve.
    assert_eq!(r.closed.load(Ordering::SeqCst), 1);
    assert!(!r.finished.load(Ordering::SeqCst));
    // Let it finish; the detached thread must end cleanly.
    r.release.store(true, Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(3);
    while !r.finished.load(Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(r.finished.load(Ordering::SeqCst));
}

#[test]
fn a_drain_timeout_does_not_detach_requests_that_finish_in_time() {
    let r = rig(Dispatcher::new("t", "1").with_drain_timeout(Duration::from_secs(5)));
    let release = Arc::clone(&r.release);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        release.store(true, Ordering::SeqCst);
    });
    let out = SharedBuf::default();
    let report = serve_report(
        Arc::new(r.d),
        BufReader::new(Cursor::new(HOLD_THEN_EOF)),
        out.clone(),
        false,
    )
    .unwrap();
    assert_eq!(report.detached, 0);
    assert!(frames(&out)
        .iter()
        .any(|f| f["type"] == "response" && f["id"] == 1 && f["ok"] == true));
}

fn input(lines: &[String]) -> BufReader<Cursor<String>> {
    BufReader::new(Cursor::new(lines.join("\n")))
}

fn long_req(id: u64, method: &str) -> String {
    json!({"type": "request", "id": id, "method": method}).to_string()
}

fn answered(out: &SharedBuf, id: u64) -> bool {
    frames(out)
        .iter()
        .any(|f| f["type"] == "response" && f["id"] == id && f["ok"] == true)
}

#[test]
fn eof_with_no_drain_timeout_joins_many_long_requests_and_loses_no_answer() {
    // Reaping finished threads while admitting must not change the default: EOF waits for all.
    let r = rig(Dispatcher::new("t", "1"));
    let release = Arc::clone(&r.release);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        release.store(true, Ordering::SeqCst);
    });
    let mut lines: Vec<String> = (1..=100).map(|i| long_req(i, "fast")).collect();
    lines.push(long_req(1000, "hold"));
    lines.extend((101..=150).map(|i| long_req(i, "fast")));
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(Arc::new(r.d), input(&lines), out.clone(), false).unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "waits for hold"
    );
    assert_eq!(report, ServeReport { detached: 0 });
    assert!(r.finished.load(Ordering::SeqCst));
    assert_eq!(r.closed.load(Ordering::SeqCst), 1);
    for id in (1..=150).chain([1000]) {
        assert!(answered(&out, id), "no answer for {id}");
    }
}

#[test]
fn shutdown_with_no_drain_timeout_joins_running_and_finished_long_requests() {
    let r = rig(Dispatcher::new("t", "1"));
    let release = Arc::clone(&r.release);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        release.store(true, Ordering::SeqCst);
    });
    let mut lines: Vec<String> = (1..=40).map(|i| long_req(i, "fast")).collect();
    lines.push(long_req(1000, "hold"));
    lines.push(long_req(2000, "shutdown"));
    // Never read: the loop stopped at shutdown.
    lines.push(long_req(3000, "fast"));
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(Arc::new(r.d), input(&lines), out.clone(), false).unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "waits for hold"
    );
    assert_eq!(report, ServeReport { detached: 0 });
    assert!(r.finished.load(Ordering::SeqCst));
    assert_eq!(r.closed.load(Ordering::SeqCst), 1);
    for id in (1..=40).chain([1000, 2000]) {
        assert!(answered(&out, id), "no answer for {id}");
    }
    assert!(!frames(&out).iter().any(|f| f["id"] == 3000));
}

#[test]
fn a_bounded_drain_counts_only_running_requests_among_many_finished_ones() {
    // The drain is long enough for 120 cold thread spawns on a loaded host to finish, so only the
    // stuck request can still be running when it passes.
    let r = rig(Dispatcher::new("t", "1").with_drain_timeout(Duration::from_secs(3)));
    let mut lines: Vec<String> = (1..=60).map(|i| long_req(i, "fast")).collect();
    lines.push(long_req(1000, "hold"));
    lines.extend((61..=120).map(|i| long_req(i, "fast")));
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(Arc::new(r.d), input(&lines), out.clone(), false).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "serve must return by the deadline with a stuck request: {:?}",
        started.elapsed()
    );
    assert_eq!(report.detached, 1);
    assert_eq!(r.closed.load(Ordering::SeqCst), 1);
    assert!(!r.finished.load(Ordering::SeqCst));
    assert!(!answered(&out, 1000));
    for id in 1..=120 {
        assert!(answered(&out, id), "no answer for {id}");
    }
    // The detached request is only detached: released, it finishes and answers cleanly.
    r.release.store(true, Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(3);
    while !r.finished.load(Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(r.finished.load(Ordering::SeqCst));
}

#[test]
fn a_bounded_drain_detaches_every_stuck_request_and_never_blocks_past_the_deadline() {
    let r = rig(Dispatcher::new("t", "1").with_drain_timeout(Duration::from_millis(200)));
    let lines: Vec<String> = (1..=3).map(|i| long_req(i, "hold")).collect();
    let out = SharedBuf::default();
    let started = Instant::now();
    let report = serve_report(Arc::new(r.d), input(&lines), out.clone(), false).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.detached, 3);
    r.release.store(true, Ordering::SeqCst);
}

#[test]
fn the_dispatcher_frame_limit_is_bounded_and_a_finite_one_is_kept() {
    assert_eq!(Dispatcher::new("t", "1").frame_limit(), DEFAULT_FRAME_LIMIT);
    assert_eq!(
        Dispatcher::new("t", "1")
            .with_frame_limit(usize::MAX)
            .frame_limit(),
        MAX_FRAME_LIMIT
    );
    assert_eq!(
        Dispatcher::new("t", "1")
            .with_frame_limit(4096)
            .frame_limit(),
        4096
    );
}

/// One command table: `search.files` is the method; `find` is an alias; `meta.doc` is metadata.
struct Table;
impl CommandSet for Table {
    fn methods(&self) -> Vec<String> {
        vec!["search.files".into(), "slow.thing".into()]
    }
    fn is_long(&self, method: &str, _: &Value) -> bool {
        method == "slow.thing"
    }
    fn call(&self, method: &str, p: &Value, _: bool, _: &Sink) -> JobResult<Value> {
        Ok(json!({"method": method, "params": p}))
    }
}

fn table() -> Dispatcher {
    Dispatcher::new("t", "1")
        .with(Arc::new(Table))
        .with_alias("find", "search.files")
        .with_alias("slow", "slow.thing")
        .with_alias("exit", "shutdown")
        .with_alias("ping", "search.files")
        .with_alias("ghost", "no.such.method")
        .with_method_meta(
            "search.files",
            json!({"help": "find files", "group": "search"}),
        )
}

fn window(d: Dispatcher, lines: &[&str]) -> Vec<Value> {
    let out = SharedBuf::default();
    serve(
        Arc::new(d),
        BufReader::new(Cursor::new(lines.join("\n"))),
        out.clone(),
        false,
    )
    .unwrap();
    frames(&out)
}

fn cli(d: &Dispatcher, words: &[&str]) -> (i32, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli(
        d,
        words.iter().map(|w| w.to_string()),
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn an_alias_reaches_the_same_handler_from_window_frames_and_the_cli() {
    let frames = window(
        table(),
        &[
            r#"{"type":"request","id":1,"method":"find","params":{"q":"a"}}"#,
            r#"{"type":"request","id":2,"method":"search.files","params":{"q":"a"}}"#,
        ],
    );
    let by_alias = &frames.iter().find(|f| f["id"] == 1).unwrap()["result"];
    let direct = &frames.iter().find(|f| f["id"] == 2).unwrap()["result"];
    assert_eq!(
        by_alias["method"], "search.files",
        "the handler sees the canonical name"
    );
    assert_eq!(by_alias, direct);
    let (code, text) = cli(&table(), &["find", "--input", r#"{"q":"a"}"#]);
    assert_eq!(code, 0, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["method"], "search.files");
    assert_eq!(v["params"]["q"], "a");
}

#[test]
fn an_alias_keeps_the_target_methods_long_and_shutdown_semantics() {
    // `exit` is `shutdown`: the loop stops, so the request after it is never read.
    let frames = window(
        table(),
        &[
            r#"{"type":"request","id":1,"method":"exit"}"#,
            r#"{"type":"request","id":2,"method":"search.files"}"#,
        ],
    );
    assert!(frames.iter().any(|f| f["id"] == 1 && f["ok"] == true));
    assert!(!frames.iter().any(|f| f["id"] == 2), "{frames:?}");
    // `slow` is a long request: answered from its own thread, still exactly once.
    let frames = window(
        table(),
        &[
            r#"{"type":"request","id":1,"method":"slow"}"#,
            r#"{"type":"request","id":2,"method":"shutdown"}"#,
        ],
    );
    assert_eq!(
        frames
            .iter()
            .filter(|f| f["type"] == "response" && f["id"] == 1)
            .count(),
        1
    );
}

#[test]
fn an_alias_never_shadows_a_served_name_and_a_dangling_alias_is_unknown() {
    // `ping` is a built-in: the alias to `search.files` does not take it over.
    let frames = window(table(), &[r#"{"type":"request","id":1,"method":"ping"}"#]);
    assert_eq!(
        frames.iter().find(|f| f["id"] == 1).unwrap()["result"]["pong"],
        true
    );
    // An alias whose target is not served answers like any unknown method, on both transports.
    let frames = window(table(), &[r#"{"type":"request","id":1,"method":"ghost"}"#]);
    assert_eq!(
        frames.iter().find(|f| f["id"] == 1).unwrap()["error"]["code"],
        "unknown_method"
    );
    assert_eq!(cli(&table(), &["ghost"]).0, 2);
    // Aliases are not methods.
    assert!(!table().methods().iter().any(|m| m == "find"));
}

struct Failing;
impl CommandSet for Failing {
    fn methods(&self) -> Vec<String> {
        vec!["auth".into(), "plain".into()]
    }
    fn call(&self, method: &str, _: &Value, _: bool, _: &Sink) -> JobResult<Value> {
        if method == "auth" {
            Err(JobError::new("needs_auth", "sign in")
                .with_details(json!({"platform": "x", "nested": {"a": [1, 2]}})))
        } else {
            Err(JobError::new("plain_err", "nope"))
        }
    }
}

#[test]
fn error_details_travel_through_dispatcher_frames_and_job_error_json() {
    let plain = JobError::new("needs_auth", "sign in");
    assert!(plain.details().is_none());
    assert!(plain.to_json().get("details").is_none());
    let e = plain.clone().with_details(json!({"platform": "x"}));
    assert_eq!(e.to_json()["details"]["platform"], "x");
    assert_eq!(e.to_json_with(false)["details"]["platform"], "x");
    assert_ne!(e, plain);
    assert!(e.clone().with_details(Value::Null).details().is_none());

    let d = Dispatcher::new("t", "1").with(Arc::new(Failing));
    let frames = window(
        d,
        &[
            r#"{"type":"request","id":1,"method":"auth"}"#,
            r#"{"type":"request","id":2,"method":"plain"}"#,
        ],
    );
    let auth = &frames.iter().find(|f| f["id"] == 1).unwrap()["error"];
    assert_eq!(auth["details"]["platform"], "x");
    assert_eq!(auth["details"]["nested"]["a"], json!([1, 2]));
    let plain = &frames.iter().find(|f| f["id"] == 2).unwrap()["error"];
    assert!(plain.get("details").is_none(), "{plain}");
}

#[test]
fn method_metadata_is_served_in_doctor_and_readable_and_absent_when_unset() {
    let sink: Sink = Arc::new(|_| {});
    let doctor = table().call("doctor", &json!({}), false, &sink).unwrap();
    assert_eq!(doctor["method_meta"]["search.files"]["group"], "search");
    assert_eq!(
        table().method_meta("search.files").unwrap()["help"],
        "find files"
    );
    assert!(table().method_meta("slow.thing").is_none());
    let plain = Dispatcher::new("t", "1")
        .call("doctor", &json!({}), false, &sink)
        .unwrap();
    assert!(plain.get("method_meta").is_none());
}
