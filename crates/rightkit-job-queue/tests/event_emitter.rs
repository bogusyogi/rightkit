//! `EventEmitter`: unsolicited event frames on the connection `serve` is serving.
//! Written without running Cargo; not yet run.

use std::io::{BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_job_queue::*;
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl Write for SharedBuf {
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
        .map(|l| serde_json::from_str(l).expect("every line is one whole JSON frame"))
        .collect()
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A served connection whose input the test controls: dropping `input` is EOF.
struct Served {
    input: std::io::PipeWriter,
    out: SharedBuf,
    done: std::thread::JoinHandle<()>,
}

fn serve_in_background(d: Dispatcher) -> Served {
    let (reader, input) = std::io::pipe().unwrap();
    let out = SharedBuf::default();
    let done = {
        let out = out.clone();
        std::thread::spawn(move || {
            serve(Arc::new(d), BufReader::new(reader), out, false).unwrap();
        })
    };
    Served { input, out, done }
}

impl Served {
    fn finish(self) -> Vec<Value> {
        drop(self.input);
        self.done.join().unwrap();
        frames(&self.out)
    }
}

#[test]
fn unbound_emitter_refuses_and_does_not_queue() {
    let d = Dispatcher::new("t", "1");
    let emitter = d.event_emitter();
    assert!(!emitter.is_connected());
    let err = emitter.emit(json!({"n": 1})).unwrap_err();
    assert_eq!(err.code, "not_serving");
}

#[test]
fn event_follows_hello_has_no_id_and_ends_with_the_connection() {
    let d = Dispatcher::new("t", "1");
    let emitter = d.event_emitter();
    let served = serve_in_background(d);
    wait_until("the connection to bind", || emitter.is_connected());
    emitter
        .emit(json!({"event": "model.ready", "n": 1}))
        .unwrap();
    wait_until("the event frame", || frames(&served.out).len() >= 2);
    let all = served.finish();
    assert_eq!(all[0]["type"], "hello", "hello stays the first frame");
    let event = &all[1];
    assert_eq!(event["type"], "event");
    assert!(
        event.get("id").is_none(),
        "an unsolicited event carries no id"
    );
    assert_eq!(event["event"], json!({"event": "model.ready", "n": 1}));
    assert!(!emitter.is_connected());
    assert_eq!(
        emitter.emit(json!({"late": true})).unwrap_err().code,
        "not_serving"
    );
}

#[test]
fn events_and_responses_interleave_as_whole_frames() {
    let d = Dispatcher::new("t", "1");
    let emitter = d.event_emitter();
    let mut served = serve_in_background(d);
    wait_until("the connection to bind", || emitter.is_connected());
    let mut senders = Vec::new();
    for t in 0..4 {
        let emitter = emitter.clone();
        senders.push(std::thread::spawn(move || {
            for n in 0..50 {
                emitter.emit(json!({"t": t, "n": n})).unwrap();
            }
        }));
    }
    for id in 1..=20u64 {
        writeln!(
            served.input,
            "{}",
            json!({"type": "request", "id": id, "method": "ping"})
        )
        .unwrap();
    }
    for s in senders {
        s.join().unwrap();
    }
    wait_until("every response", || {
        frames(&served.out)
            .iter()
            .filter(|f| f["type"] == "response")
            .count()
            == 20
    });
    let all = served.finish();
    assert_eq!(all[0]["type"], "hello");
    let events: Vec<&Value> = all.iter().filter(|f| f["type"] == "event").collect();
    assert_eq!(events.len(), 200);
    for t in 0..4 {
        let seen: Vec<i64> = events
            .iter()
            .filter(|f| f["event"]["t"] == t)
            .map(|f| f["event"]["n"].as_i64().unwrap())
            .collect();
        assert_eq!(seen, (0..50).collect::<Vec<_>>(), "per-thread order kept");
    }
}

#[test]
fn an_oversized_event_is_refused_and_writes_nothing() {
    let d = Dispatcher::new("t", "1").with_frame_limit(1024);
    let emitter = d.event_emitter();
    let served = serve_in_background(d);
    wait_until("the connection to bind", || emitter.is_connected());
    let err = emitter.emit(json!({"blob": "x".repeat(4096)})).unwrap_err();
    assert_eq!(err.code, "frame_too_large");
    emitter.emit(json!({"ok": true})).unwrap();
    wait_until("the small event", || frames(&served.out).len() >= 2);
    let all = served.finish();
    let events: Vec<&Value> = all.iter().filter(|f| f["type"] == "event").collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], json!({"ok": true}));
}

#[test]
fn the_same_emitter_serves_the_next_connection() {
    let d = Arc::new(Dispatcher::new("t", "1"));
    let emitter = d.event_emitter();
    for round in 0..2 {
        let (reader, input) = std::io::pipe().unwrap();
        let out = SharedBuf::default();
        let done = {
            let (d, out) = (Arc::clone(&d), out.clone());
            std::thread::spawn(move || {
                serve(d, BufReader::new(reader), out, false).unwrap();
            })
        };
        wait_until("the connection to bind", || emitter.is_connected());
        emitter.emit(json!({"round": round})).unwrap();
        wait_until("the event frame", || frames(&out).len() >= 2);
        drop(input);
        done.join().unwrap();
        assert!(!emitter.is_connected(), "round {round}: unbound at the end");
        let all = frames(&out);
        assert_eq!(all[1]["event"], json!({"round": round}));
    }
}
