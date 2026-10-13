//! The outbound link (`rk-link/1`) and durable tasks, M7 lane D.
//!
//! Two layers. The host logic (`Daemon::link_accept`, `link_frame`, `enqueue_task`, leases) is
//! driven in process with a manual clock, so lease expiry and epoch fencing are exact. The
//! socket tests run the real blocking client against a TLS test daemon: kill a connection
//! mid-task and resume, and refuse plaintext.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use rightkit_daemon::link::client::{LinkClient, LinkConfig, LinkTask, TaskError, TaskHandler};
use rightkit_daemon::link::frame::{c2s_line, open_s2c, sign_c2s};
use rightkit_daemon::remote::client::DeviceKey;
use rightkit_daemon::remote::testing::TestDaemon;
use rightkit_daemon::remote::{unhex, Accepted, FrameOutcome, Out, TaskSpec, TaskStatus};
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};

// ---- in-process host logic -----------------------------------------------------------------

struct LinkDev {
    seed: [u8; 32],
    id: String,
    dev: Dev,
}

fn enrol_link_device(h: &Harness, seed: u8) -> LinkDev {
    enrol_with(h, seed, &["link"])
}

fn enrol_with(h: &Harness, seed: u8, scopes: &[&str]) -> LinkDev {
    let seed = [seed; 32];
    let kp = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let code = h.daemon.issue_enrolment_code(scopes, 600).unwrap();
    let first = Dev {
        id: "none".into(),
        kp: Arc::new(kp),
    };
    let resp = h.call(first.enrol_request(&code, h.now(), &next_nonce()));
    assert_eq!(resp.status, 201, "{:?}", resp.body);
    let id = resp.body["deviceId"].as_str().unwrap().to_string();
    LinkDev {
        seed,
        id: id.clone(),
        dev: Dev { id, kp: first.kp },
    }
}

fn upgrade_request(h: &Harness, d: &LinkDev) -> rightkit_daemon::remote::Request {
    d.dev.request_at(
        "GET",
        "/v1/link",
        &[],
        h.now(),
        &next_nonce(),
        &[("upgrade", "rk-link/1"), ("connection", "Upgrade")],
    )
}

/// The machine's end of one link connection, driven by hand.
struct Peer {
    key: DeviceKey,
    id: String,
    link_id: String,
    s2c_key: [u8; 32],
    n: u64,
    acc: Accepted,
}

fn connect(h: &Harness, d: &LinkDev) -> Peer {
    let acc = match h.daemon.link_accept(&upgrade_request(h, d)) {
        Ok(a) => a,
        Err(r) => panic!("upgrade refused: {} {:?}", r.status, r.body),
    };
    Peer {
        key: DeviceKey::from_seed(&d.seed).unwrap(),
        id: d.id.clone(),
        link_id: acc.link_id.clone(),
        s2c_key: unhex(&acc.key_hex).unwrap().try_into().unwrap(),
        n: 0,
        acc,
    }
}

impl Peer {
    fn line(&mut self, mut frame: Value) -> String {
        self.n += 1;
        frame["n"] = json!(self.n);
        c2s_line(&self.key, &self.link_id, &frame)
    }

    fn send(&mut self, h: &Harness, frame: Value) -> FrameOutcome {
        let line = self.line(frame);
        h.daemon.link_frame(&self.acc.session, &line)
    }

    fn hello(&mut self, h: &Harness, resume_seq: u64) -> FrameOutcome {
        let f = json!({ "t": "hello", "device": self.id, "resume_seq": resume_seq });
        self.send(h, f)
    }

    fn result(&mut self, h: &Harness, id: &str, epoch: u64) -> FrameOutcome {
        let f = json!({ "t": "result", "id": id, "epoch": epoch, "ok": true, "result": {"v": 1} });
        self.send(h, f)
    }

    fn outs(&mut self) -> Vec<Out> {
        let mut v = Vec::new();
        while let Some(o) = self.acc.try_next() {
            v.push(o);
        }
        v
    }

    fn frames(&mut self) -> Vec<Value> {
        self.outs()
            .into_iter()
            .filter_map(|o| match o {
                Out::Frame(f) => Some(f),
                Out::Close(_) => None,
            })
            .collect()
    }
}

fn types(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .map(|f| f["t"].as_str().unwrap_or("?").to_string())
        .collect()
}

fn settled(frames: &[Value]) -> Vec<(u64, String, Option<String>)> {
    frames
        .iter()
        .filter(|f| f["t"] == "settled")
        .map(|f| {
            (
                f["epoch"].as_u64().unwrap(),
                f["state"].as_str().unwrap().to_string(),
                f["code"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn work(h: &Harness, d: &LinkDev) -> String {
    h.daemon
        .enqueue_task(TaskSpec::new("work", json!({"k": 1})).target(&d.id))
        .unwrap()
}

#[test]
fn hello_welcomes_and_leases_a_queued_task() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 1);
    // Queued before the device is linked: nothing is sent, nothing is leased.
    let id = work(&h, &d);
    let t = h.daemon.task(&id).unwrap();
    assert_eq!((t.status, t.epoch, t.owner), (TaskStatus::Queued, 0, None));

    let mut p = connect(&h, &d);
    assert_eq!(p.hello(&h, 0), FrameOutcome::Continue);
    let frames = p.frames();
    assert_eq!(types(&frames), ["welcome", "task"]);
    assert_eq!(frames[1]["id"], json!(id));
    assert_eq!(frames[1]["epoch"], json!(1));
    assert_eq!(frames[1]["seq"], json!(1));
    assert_eq!(frames[1]["method"], json!("work"));
    let t = h.daemon.task(&id).unwrap();
    assert_eq!(t.status, TaskStatus::Leased);
    assert_eq!(t.owner.as_deref(), Some(d.id.as_str()));
    assert_eq!(h.events("TASK_LEASED").len(), 1);
    assert_eq!(h.events("LINK_UP").len(), 1);

    // Host frames carry numbers and a tag that verifies under the issued session key.
    let line = p.acc.session.encode(json!({ "t": "ping" }));
    let v = open_s2c(&p.s2c_key, &p.link_id, &line).unwrap();
    assert_eq!((v["n"].as_u64(), v["t"].as_str()), (Some(1), Some("ping")));
    let line = p.acc.session.encode(json!({ "t": "ping" }));
    assert_eq!(
        open_s2c(&p.s2c_key, &p.link_id, &line).unwrap()["n"],
        json!(2)
    );
}

#[test]
fn a_stale_epoch_result_is_refused_and_the_current_one_accepted() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 2);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let id = work(&h, &d);
    assert_eq!(types(&p.frames()), ["welcome", "task"]);

    // The owner goes silent past the lease (default 120 s): it lapses once and is leased
    // again with epoch 2.
    h.advance(200);
    assert_eq!(h.daemon.tasks_sweep().unwrap(), 1);
    assert_eq!(h.daemon.tasks_sweep().unwrap(), 0);
    let frames = p.frames();
    assert_eq!(types(&frames), ["task"]);
    assert_eq!(frames[0]["epoch"], json!(2));
    assert_eq!(h.daemon.task(&id).unwrap().epoch, 2);

    // The late epoch-1 result is fenced; the epoch-2 result is accepted; a repeat is a no-op.
    assert_eq!(p.result(&h, &id, 1), FrameOutcome::Continue);
    assert_eq!(
        settled(&p.frames()),
        [(1, "refused".to_string(), Some("lease_lost".to_string()))]
    );
    assert_eq!(h.daemon.task(&id).unwrap().status, TaskStatus::Leased);

    p.result(&h, &id, 2);
    assert_eq!(settled(&p.frames()), [(2, "accepted".to_string(), None)]);
    let t = h.daemon.task(&id).unwrap();
    assert_eq!(t.status, TaskStatus::Done);
    assert_eq!(t.outcome.unwrap()["result"], json!({"v": 1}));

    p.result(&h, &id, 2);
    assert_eq!(settled(&p.frames()), [(2, "duplicate".to_string(), None)]);
    assert_eq!(h.events("TASK_DONE").len(), 1);
    let stale = h
        .events("TASK_LEASE_LOST")
        .into_iter()
        .filter(|e| e["reason"] == "stale_result")
        .count();
    assert_eq!(stale, 1);
}

#[test]
fn lease_expiry_requeues_exactly_once_even_under_concurrent_sweeps() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 3);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let id = work(&h, &d);
    p.frames();
    // The device drops off. Disconnecting does not release the lease.
    h.daemon.link_closed(&p.acc.session, "gone");
    let t = h.daemon.task(&id).unwrap();
    assert_eq!((t.status, t.epoch), (TaskStatus::Leased, 1));
    assert!(h.events("LINK_DOWN").len() == 1);

    h.advance(200);
    let lapsed: usize = std::thread::scope(|s| {
        let joins: Vec<_> = (0..8)
            .map(|_| s.spawn(|| h.daemon.tasks_sweep().unwrap()))
            .collect();
        joins.into_iter().map(|j| j.join().unwrap()).sum()
    });
    assert_eq!(lapsed, 1, "the lease lapses once, however many sweeps race");
    let t = h.daemon.task(&id).unwrap();
    assert_eq!((t.status, t.owner, t.epoch), (TaskStatus::Queued, None, 1));
    let lost = h
        .events("TASK_LEASE_LOST")
        .into_iter()
        .filter(|e| e["reason"] == "expired")
        .count();
    assert_eq!(lost, 1);

    // When the device comes back it is leased once more: epoch 2, and only one task frame.
    let mut q = connect(&h, &d);
    q.hello(&h, 0);
    let frames = q.frames();
    assert_eq!(types(&frames), ["welcome", "task"]);
    assert_eq!(frames[1]["epoch"], json!(2));
    assert_eq!(h.daemon.task(&id).unwrap().epoch, 2);
}

#[test]
fn a_newer_connection_supersedes_and_the_old_one_completes_nothing() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 4);
    let mut old = connect(&h, &d);
    old.hello(&h, 0);
    let id = work(&h, &d);
    old.frames();

    let mut new = connect(&h, &d);
    // The older connection is told it was superseded and is closed.
    assert!(old
        .outs()
        .iter()
        .any(|o| *o == Out::Close("superseded".into())));
    assert_eq!(h.daemon.linked_devices(), Vec::<String>::new()); // new has not said hello yet
    assert_eq!(
        h.events("LINK_DOWN")
            .iter()
            .filter(|e| e["reason"] == "superseded")
            .count(),
        1
    );

    // The old connection's result is refused and the lease is untouched.
    assert_eq!(
        old.result(&h, &id, 1),
        FrameOutcome::Close("superseded".into())
    );
    assert_eq!(h.daemon.task(&id).unwrap().status, TaskStatus::Leased);

    // The new connection resumes the same lease (same epoch) and completes it.
    new.hello(&h, 0);
    let frames = new.frames();
    assert_eq!(types(&frames), ["welcome", "task"]);
    assert_eq!(frames[1]["epoch"], json!(1));
    new.result(&h, &id, 1);
    assert_eq!(settled(&new.frames()), [(1, "accepted".to_string(), None)]);
    assert_eq!(h.daemon.task(&id).unwrap().status, TaskStatus::Done);
    assert_eq!(h.daemon.linked_devices(), vec![d.id.clone()]);
}

#[test]
fn resume_seq_skips_tasks_the_device_already_holds() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 5);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let first = work(&h, &d);
    let second = work(&h, &d);
    let frames = p.frames();
    assert_eq!(types(&frames), ["welcome", "task", "task"]);
    p.send(&h, json!({ "t": "ack", "id": first, "epoch": 1 }));
    assert!(h.daemon.task(&first).unwrap().acked);
    h.daemon.link_closed(&p.acc.session, "killed");

    // Reconnect saying it durably holds seq 1: only seq 2 is sent again, with its epoch.
    let mut q = connect(&h, &d);
    q.hello(&h, 1);
    let frames = q.frames();
    assert_eq!(types(&frames), ["welcome", "task"]);
    assert_eq!(frames[1]["id"], json!(second));
    assert_eq!(frames[1]["epoch"], json!(1));
    // The held task finishes over the new connection with its original epoch.
    q.result(&h, &first, 1);
    assert_eq!(settled(&q.frames()), [(1, "accepted".to_string(), None)]);
}

#[test]
fn unpinned_tasks_go_to_the_least_loaded_linked_device() {
    let h = Harness::new();
    let a = enrol_link_device(&h, 6);
    let b = enrol_link_device(&h, 7);
    let (mut pa, mut pb) = (connect(&h, &a), connect(&h, &b));
    pa.hello(&h, 0);
    pb.hello(&h, 0);
    pa.frames();
    pb.frames();
    for _ in 0..4 {
        h.daemon
            .enqueue_task(TaskSpec::new("any", json!({})))
            .unwrap();
    }
    assert_eq!(pa.frames().len(), 2);
    assert_eq!(pb.frames().len(), 2);
}

#[test]
fn every_frame_is_authenticated_and_numbered() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 8);
    let mut p = connect(&h, &d);

    // Nothing but hello before hello.
    assert_eq!(
        p.send(&h, json!({ "t": "ping" })),
        FrameOutcome::Close("expected_hello".into())
    );
    let mut p = connect(&h, &d);
    assert_eq!(p.hello(&h, 0), FrameOutcome::Continue);
    let ping = p.line(json!({ "t": "ping" }));
    assert_eq!(
        h.daemon.link_frame(&p.acc.session, &ping),
        FrameOutcome::Continue
    );
    // The same line again is a replay.
    assert_eq!(
        h.daemon.link_frame(&p.acc.session, &ping),
        FrameOutcome::Close("replayed".into())
    );

    // A forged tag, a tag from another connection, a tag from another key, a gap.
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let good = p.line(json!({ "t": "ping" }));
    let (_, payload) = good.trim_end().split_once(' ').unwrap();
    let forged = format!("{} {}\n", "AAAA", payload);
    assert_eq!(
        h.daemon.link_frame(&p.acc.session, &forged),
        FrameOutcome::Close("bad_tag".into())
    );

    // A frame signed for another connection (another link id) does not verify here.
    let p = connect(&h, &d);
    let hello = json!({ "t": "hello", "device": p.id, "resume_seq": 0, "n": 1 });
    let foreign = format!(
        "{} {}\n",
        sign_c2s(&p.key, "some-other-link", &hello.to_string()),
        hello
    );
    assert_eq!(
        h.daemon.link_frame(&p.acc.session, &foreign),
        FrameOutcome::Close("bad_tag".into())
    );

    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    p.n += 3; // frames 2..4 never arrive
    assert_eq!(
        p.send(&h, json!({ "t": "ping" })),
        FrameOutcome::Close("sequence_gap".into())
    );

    let rejected = h.events("LINK_FRAME_REJECTED");
    let why: Vec<&str> = rejected
        .iter()
        .map(|e| e["reason"].as_str().unwrap())
        .collect();
    for expected in ["expected_hello", "replayed", "bad_tag", "sequence_gap"] {
        assert!(why.contains(&expected), "{expected} in {why:?}");
    }
}

#[test]
fn upgrade_needs_a_signed_device_with_the_link_scope() {
    let h = Harness::new();
    let no_scope = enrol_with(&h, 9, &["venture"]);
    match h.daemon.link_accept(&upgrade_request(&h, &no_scope)) {
        Ok(_) => panic!("a device without the link scope was linked"),
        Err(r) => assert_eq!(
            (r.status, r.body["error"].as_str()),
            (403, Some("forbidden"))
        ),
    }

    let d = enrol_link_device(&h, 10);
    // Unknown device, bad signature, replayed nonce, missing upgrade headers.
    let mut ghost = enrol_link_device(&h, 11);
    ghost.dev.id = "dev_0000000000000000".into();
    let r = h
        .daemon
        .link_accept(&upgrade_request(&h, &ghost))
        .err()
        .unwrap();
    assert_eq!(r.status, 401);

    let signed = upgrade_request(&h, &d);
    let mut bad = signed.clone();
    bad.headers
        .insert("x-rk-signature".into(), "AAAAAAAA".into());
    assert_eq!(h.daemon.link_accept(&bad).err().unwrap().status, 401);
    assert!(h.daemon.link_accept(&signed).is_ok());
    assert_eq!(h.daemon.link_accept(&signed).err().unwrap().status, 401); // nonce replay

    let mut plain = upgrade_request(&h, &d);
    plain.headers.remove("upgrade");
    assert_eq!(h.daemon.link_accept(&plain).err().unwrap().status, 400);
    let mut post = upgrade_request(&h, &d);
    post.method = "POST".into();
    assert_eq!(h.daemon.link_accept(&post).err().unwrap().status, 400);
}

#[test]
fn a_revoked_device_is_cut_off_and_its_lease_lapses() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 12);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let id = work(&h, &d);
    p.frames();
    assert!(h.daemon.revoke_device(&d.id).unwrap());
    assert!(p.outs().iter().any(|o| *o == Out::Close("revoked".into())));
    assert_eq!(
        p.result(&h, &id, 1),
        FrameOutcome::Close("superseded".into())
    );
    assert!(h
        .daemon
        .enqueue_task(TaskSpec::new("x", json!({})).target(&d.id))
        .is_err());
    h.advance(200);
    assert_eq!(h.daemon.tasks_sweep().unwrap(), 1);
    assert_eq!(h.daemon.task(&id).unwrap().status, TaskStatus::Queued);
}

#[test]
fn leases_and_the_queue_survive_a_host_restart() {
    let mut h = Harness::new();
    let d = enrol_link_device(&h, 13);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let leased = work(&h, &d);
    p.frames();
    h.daemon.close_all_links("restart");
    h.restart();
    let t = h.daemon.task(&leased).unwrap();
    assert_eq!((t.status, t.epoch), (TaskStatus::Leased, 1));
    assert_eq!(t.owner.as_deref(), Some(d.id.as_str()));
    // The device reconnects to the new host and finishes the lease it already held.
    let mut q = connect(&h, &d);
    q.hello(&h, 1);
    assert_eq!(types(&q.frames()), ["welcome"]);
    q.result(&h, &leased, 1);
    assert_eq!(settled(&q.frames()), [(1, "accepted".to_string(), None)]);
    // Sequence numbers keep growing after a restart.
    let next = work(&h, &d);
    assert_eq!(h.daemon.task(&next).unwrap().seq, 2);
}

#[test]
fn a_machine_request_frame_runs_the_device_contract() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 14);
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    p.frames();
    // This host has no method table (`Daemon::with_calls`), exactly like `POST /v1/call`.
    p.send(
        &h,
        json!({ "t": "request", "id": "r1", "method": "echo", "params": {} }),
    );
    let frames = p.frames();
    assert_eq!(types(&frames), ["response"]);
    assert_eq!(
        (frames[0]["id"].as_str(), frames[0]["status"].as_u64()),
        (Some("r1"), Some(404))
    );
}

#[test]
fn status_counts_links_and_queue_depth() {
    let h = Harness::new();
    let d = enrol_link_device(&h, 15);
    let _ = work(&h, &d);
    let m = h.daemon.metrics();
    assert_eq!((m.links, m.tasks_queued, m.tasks_leased), (0, 1, 0));
    let mut p = connect(&h, &d);
    p.hello(&h, 0);
    let m = h.daemon.metrics();
    assert_eq!((m.links, m.tasks_queued, m.tasks_leased), (1, 0, 1));
}

// ---- real client over TLS --------------------------------------------------------------------

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn enrolled_session(td: &TestDaemon) -> rightkit_daemon::remote::client::DeviceSession {
    let key = DeviceKey::from_pkcs8(&DeviceKey::generate_pkcs8().unwrap()).unwrap();
    td.client()
        .unwrap()
        .enrol(&td.issue_code(&["link"]).unwrap(), key, Some("link-test"))
        .unwrap()
}

fn fast_config(dir: &std::path::Path) -> LinkConfig {
    LinkConfig {
        journal: Some(dir.join("journal.jsonl")),
        backoff_min: Duration::from_millis(20),
        backoff_max: Duration::from_millis(100),
        ..LinkConfig::default()
    }
}

#[test]
fn killing_the_connection_mid_task_lets_the_same_device_resume() {
    let td = TestDaemon::start().unwrap();
    let session = enrolled_session(&td);
    let device = session.device_id().to_string();
    let dir = tempfile::tempdir().unwrap();

    let (started_tx, started_rx) = mpsc::channel::<(String, u64)>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let runs = Arc::new(AtomicU64::new(0));
    let handler: Arc<dyn TaskHandler> = {
        let runs = runs.clone();
        Arc::new(move |t: &LinkTask| -> Result<Value, TaskError> {
            runs.fetch_add(1, Ordering::SeqCst);
            started_tx
                .lock()
                .unwrap()
                .send((t.id.clone(), t.epoch))
                .unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .map_err(|_| TaskError::new("timeout", "never released"))?;
            Ok(json!({ "ran": t.id }))
        })
    };
    let handle = LinkClient::start(session, fast_config(dir.path()), handler).unwrap();
    wait_until("the first link", || handle.connected());

    let id = td
        .daemon
        .enqueue_task(
            TaskSpec::new("work", json!({"k": 1}))
                .target(&device)
                .lease_ttl(300),
        )
        .unwrap();
    let (got, epoch) = started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!((got.as_str(), epoch), (id.as_str(), 1));

    // Kill the connection while the task runs.
    let before = handle.connections();
    assert!(td.daemon.close_link(&device, "test_kill"));
    wait_until("the link to come back", || {
        handle.connections() > before && handle.connected()
    });
    // Same lease, same epoch, nothing re-run, nothing re-queued.
    let t = td.daemon.task(&id).unwrap();
    assert_eq!((t.status, t.epoch), (TaskStatus::Leased, 1));

    release_tx.send(()).unwrap();
    wait_until("the result", || {
        td.daemon.task(&id).unwrap().status == TaskStatus::Done
    });
    wait_until("the result to be settled", || handle.pending_results() == 0);
    let t = td.daemon.task(&id).unwrap();
    assert_eq!(t.epoch, 1);
    assert_eq!(t.outcome.unwrap()["result"], json!({ "ran": id }));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the task ran once");
    let events = td.daemon.events();
    let count = |name: &str| events.iter().filter(|e| e["event"] == name).count();
    assert_eq!(count("TASK_LEASED"), 1);
    assert_eq!(count("TASK_LEASE_LOST"), 0);
    assert!(count("LINK_UP") >= 2);
    handle.stop();
}

#[test]
fn a_result_finished_while_the_link_is_down_is_delivered_after_reconnect() {
    let td = TestDaemon::start().unwrap();
    let session = enrolled_session(&td);
    let device = session.device_id().to_string();
    let dir = tempfile::tempdir().unwrap();

    let (started_tx, started_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let handler: Arc<dyn TaskHandler> = Arc::new(move |_t: &LinkTask| {
        started_tx.lock().unwrap().send(()).unwrap();
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20));
        Ok::<Value, TaskError>(json!("late"))
    });
    // A long backoff keeps the link down while the task finishes.
    let mut cfg = fast_config(dir.path());
    cfg.backoff_min = Duration::from_millis(600);
    cfg.backoff_max = Duration::from_millis(600);
    let handle = LinkClient::start(session, cfg, handler).unwrap();
    wait_until("the link", || handle.connected());
    let id = td
        .daemon
        .enqueue_task(
            TaskSpec::new("work", json!({}))
                .target(&device)
                .lease_ttl(300),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    let before = handle.connections();
    td.daemon.close_link(&device, "cut");
    wait_until("the link to drop", || !handle.connected());
    release_tx.send(()).unwrap();
    wait_until("pending result", || handle.pending_results() == 1);
    wait_until("reconnect", || handle.connections() > before);
    wait_until("done", || {
        td.daemon.task(&id).unwrap().status == TaskStatus::Done
    });
    assert_eq!(td.daemon.task(&id).unwrap().epoch, 1);
    handle.stop();
}

#[test]
fn the_link_never_falls_back_to_plaintext() {
    let td = TestDaemon::start().unwrap();
    let mut s = TcpStream::connect(td.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = s.write_all(
        b"GET /v1/link HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: rk-link/1\r\n\r\n",
    );
    let mut buf = [0u8; 512];
    let n = s.read(&mut buf).unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(!text.starts_with("HTTP/"), "answered in plaintext: {text}");
    assert!(!text.contains("101"), "{text}");
    assert!(td.daemon.linked_devices().is_empty());
}
