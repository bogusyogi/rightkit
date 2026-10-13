//! M4 owner-gap coverage (window/CLI protocol): bounded frame limit, bounded disconnect drain,
//! one command table for window frames and CLI (aliases, method metadata).

use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_jobs::*;
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

/// `hold` is a long request that runs until released; it sees disconnect cooperatively.
struct Hold {
    release: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
}
impl CommandSet for Hold {
    fn methods(&self) -> Vec<String> {
        vec!["hold".into(), "quick".into()]
    }
    fn is_long(&self, method: &str, _: &Value) -> bool {
        method == "hold"
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
