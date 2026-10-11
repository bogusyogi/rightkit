//! RK8 worker wire over loopback TCP. The controller and the GUI worker run in
//! one process: the worker serves on a listener thread, and the controller
//! dials it. Faults are injected by a stream wrapper that stops reads after a
//! chosen number of writes, which models a connection lost mid-exchange.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use rightkit_scenario::worker::BundleStore;
use rightkit_scenario::{
    BundleRef, CancelToken, Capability, Controller, ControllerLink, DriverError, EnrolmentRequest,
    ExecContext, IdempotencyKey, ManualClock, Observation, ObservePolicy, Phase, ReconnectState,
    RemoteDriver, Run, StepDriver, StepRef, StepSpec, TransportError, WorkerEndpoint,
    WorkerTransport,
};
use serde_json::json;

/// A TCP stream that fails reads once `fail_reads_after_writes` writes have
/// happened. Each frame is one `write` call, so the count is per frame.
struct Flaky {
    inner: TcpStream,
    writes: usize,
    fail_reads_after_writes: Option<usize>,
    /// When set, every byte the controller writes is copied here.
    record: Option<Arc<Mutex<Vec<u8>>>>,
}

impl Read for Flaky {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(limit) = self.fail_reads_after_writes {
            if self.writes >= limit {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "injected drop",
                ));
            }
        }
        self.inner.read(buf)
    }
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if let Some(log) = &self.record {
            log.lock().expect("log").extend_from_slice(buf);
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

type Link = ControllerLink<Flaky, Box<dyn FnMut() -> io::Result<Flaky>>>;

/// Counts performs, and reports satisfied once a perform has happened.
#[derive(Clone)]
struct Gui {
    performs: Arc<AtomicUsize>,
    ready: Arc<AtomicBool>,
}

impl Gui {
    fn new() -> Self {
        Self {
            performs: Arc::new(AtomicUsize::new(0)),
            ready: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl StepDriver for Gui {
    fn perform(&mut self, _step: &StepSpec, _key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performs.fetch_add(1, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn observe(
        &mut self,
        _step: &StepSpec,
        _key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(Observation::Satisfied(json!({ "panel": "open" })))
        } else {
            Ok(Observation::Pending)
        }
    }
}

fn capabilities() -> Vec<Capability> {
    vec![Capability {
        name: "gui-click".into(),
        version: 1,
    }]
}

/// Starts a worker on loopback. A thread serves connections one at a time.
fn start_worker(code: &str, gui: Gui) -> (SocketAddr, BundleStore) {
    let endpoint = WorkerEndpoint::awaiting_enrolment("gui-worker", code, gui, capabilities())
        .expect("worker endpoint");
    start_endpoint(endpoint)
}

fn start_endpoint(mut endpoint: WorkerEndpoint<Gui>) -> (SocketAddr, BundleStore) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let bundles = endpoint.bundles();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = endpoint.serve_connection(stream);
        }
    });
    (addr, bundles)
}

/// Each connector call dials the worker. `plan` gives the failure threshold for
/// each successive connection. `None` means no fault, and a missing entry is `None`.
fn connector(addr: SocketAddr, plan: Vec<Option<usize>>) -> Box<dyn FnMut() -> io::Result<Flaky>> {
    connector_recording(addr, plan, None)
}

/// Like [`connector`], and records the controller's writes on connection
/// number `record.0` (zero based) into `record.1`.
fn connector_recording(
    addr: SocketAddr,
    plan: Vec<Option<usize>>,
    record: Option<(usize, Arc<Mutex<Vec<u8>>>)>,
) -> Box<dyn FnMut() -> io::Result<Flaky>> {
    let mut plan = VecDeque::from(plan);
    let mut index = 0usize;
    Box::new(move || {
        let fail = plan.pop_front().flatten();
        let log = match &record {
            Some((n, log)) if *n == index => Some(Arc::clone(log)),
            _ => None,
        };
        index += 1;
        Ok(Flaky {
            inner: TcpStream::connect(addr)?,
            writes: 0,
            fail_reads_after_writes: fail,
            record: log,
        })
    })
}

fn step(id: &str) -> StepSpec {
    StepSpec {
        step_id: id.into(),
        label: format!("step {id}"),
        action: json!({ "click": "open-panel" }),
        expect: json!({ "visible": "panel" }),
    }
}

fn journal_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rk8-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("run.journal");
    let _ = std::fs::remove_file(&path);
    path
}

fn policy() -> ObservePolicy {
    ObservePolicy {
        timeout_ms: 5_000,
        interval_ms: 10,
    }
}

fn enrolment_request(issued: &rightkit_scenario::IssuedEnrolment) -> EnrolmentRequest {
    EnrolmentRequest {
        worker_label: "gui-worker".into(),
        enrolment_id: issued.enrolment_id.clone(),
    }
}

#[test]
fn loopback_enrolment_discovery_and_a_step_over_tcp() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui.clone());

    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");
    assert!(identity.worker_id.starts_with("w-"));
    assert_eq!(link.discover(&identity).expect("discover"), capabilities());

    let path = journal_path("loopback");
    let mut run = Run::open(&path, "run-loopback", ManualClock::new(0)).expect("open");
    let at = StepRef {
        step: "open".into(),
        take: 0,
    };
    let lease = run.acquire(&at, "controller", 60_000).expect("lease");
    link.grant(&at, lease.fence).expect("grant");
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "controller",
        lease_ttl_ms: 60_000,
        policy: policy(),
        cancel: &cancel,
    };
    let spec = step("open");
    let mut driver = RemoteDriver::new(&mut link, at.clone(), lease.fence);
    let phase = run
        .execute(&lease, &spec, &mut driver, &ctx)
        .expect("execute");
    assert_eq!(phase, Phase::Completed);
    assert_eq!(run.value(&at), Some(&json!({ "panel": "open" })));
    assert_eq!(gui.performs.load(Ordering::SeqCst), 1);
}

#[test]
fn dropped_after_dispatch_reconnects_and_the_step_completes_once() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui.clone());

    // Connection 1 is the enrolment. Connection 2 is the session, whose writes
    // are Challenge, Accept, Grant, Perform. Reads fail once the Perform has
    // been written, so the worker performs but the reply is lost. Connection 3
    // is the reconnect and carries no fault.
    let mut link: Link =
        ControllerLink::new(controller, connector(addr, vec![None, Some(4), None, None]));
    link.enrol(&enrolment_request(&issued)).expect("enrol");

    let path = journal_path("drop-after-dispatch");
    let mut run = Run::open(&path, "run-drop", ManualClock::new(0)).expect("open");
    let at = StepRef {
        step: "open".into(),
        take: 0,
    };
    let lease = run.acquire(&at, "controller", 60_000).expect("lease");
    link.grant(&at, lease.fence).expect("grant");

    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "controller",
        lease_ttl_ms: 60_000,
        policy: policy(),
        cancel: &cancel,
    };
    let spec = step("open");
    let mut driver = RemoteDriver::new(&mut link, at.clone(), lease.fence);
    let phase = run
        .execute(&lease, &spec, &mut driver, &ctx)
        .expect("execute");

    assert_eq!(
        phase,
        Phase::Completed,
        "step should complete after reconnect"
    );
    assert_eq!(
        gui.performs.load(Ordering::SeqCst),
        1,
        "perform must run exactly once across the drop"
    );

    // A restart resumes from the durable journal into observe, never perform.
    drop(run);
    let reopened = Run::open(&path, "run-drop", ManualClock::new(0)).expect("reopen");
    assert_eq!(reopened.phase(&at), Phase::Completed);
}

#[test]
fn a_stale_fencing_token_is_rejected_by_the_worker() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui.clone());
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));
    link.enrol(&enrolment_request(&issued)).expect("enrol");

    let at = StepRef {
        step: "open".into(),
        take: 0,
    };
    // The first lease is superseded by a second grant, as after an expiry.
    link.grant(&at, 1).expect("grant 1");
    link.grant(&at, 2).expect("grant 2");
    match link.grant(&at, 1) {
        Err(TransportError::Fenced { current: 2 }) => {}
        other => panic!("a lower fence must be refused, got {other:?}"),
    }

    let spec = step("open");
    let key = IdempotencyKey::new("run-stale", &at);
    let stale = RemoteDriver::new(&mut link, at.clone(), 1).perform(&spec, &key);
    assert!(stale.is_err(), "a stale fence must not perform");
    assert_eq!(
        gui.performs.load(Ordering::SeqCst),
        0,
        "a fenced perform must never reach the driver"
    );

    RemoteDriver::new(&mut link, at.clone(), 2)
        .perform(&spec, &key)
        .expect("the current fence performs");
    assert_eq!(gui.performs.load(Ordering::SeqCst), 1);
}

#[test]
fn bundle_transfer_resumes_after_interruption_with_a_matching_hash() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, bundles) = start_worker(&issued.code, gui);

    // Session writes are Challenge, Accept, BundleBegin, then chunks. Reads
    // fail after the sixth write, so chunk 3's reply is lost although the
    // worker has already applied it.
    let mut link: Link =
        ControllerLink::new(controller, connector(addr, vec![None, Some(6), None, None]))
            .with_chunk_bytes(1_000);
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");

    let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let bundle = BundleRef {
        bundle_id: "bundle-demo".into(),
        blake3: blake3::hash(&payload).to_hex().to_string(),
        bytes: payload.len() as u64,
    };

    let first = link.transfer_bundle(&identity, &bundle, &payload);
    assert!(
        first.is_err(),
        "the injected drop must interrupt the transfer"
    );
    assert!(!link.is_connected());

    let report = link
        .transfer_bundle(&identity, &bundle, &payload)
        .expect("resumed transfer");
    assert!(
        report.resumed_from > 0,
        "the second attempt must resume, not restart (resumed_from={})",
        report.resumed_from
    );
    assert_eq!(report.bytes, payload.len() as u64);

    let held = bundles.lock().expect("lock");
    let stored = held.get("bundle-demo").expect("verified bundle kept");
    assert_eq!(stored, &payload);
    assert_eq!(blake3::hash(stored).to_hex().as_str(), bundle.blake3);
}

#[test]
fn bundle_bytes_that_do_not_match_the_declared_digest_are_not_sent() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui);
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");

    let bundle = BundleRef {
        bundle_id: "bad".into(),
        blake3: blake3::hash(b"expected").to_hex().to_string(),
        bytes: 8,
    };
    assert!(matches!(
        link.transfer_bundle(&identity, &bundle, b"tampered"),
        Err(TransportError::Digest(_))
    ));
}

#[test]
fn a_wrong_enrolment_code_is_refused_and_the_grant_is_burned() {
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    // The operator typed a different code for the same grant.
    let wrong_code = format!("{}:{}", issued.enrolment_id, "11".repeat(32));
    let (addr, _bundles) = start_worker(&wrong_code, Gui::new());
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));

    let request = enrolment_request(&issued);
    assert!(matches!(link.enrol(&request), Err(TransportError::Auth(_))));
    // The same grant cannot be tried again.
    assert!(matches!(link.enrol(&request), Err(TransportError::Auth(_))));
}

#[test]
fn reconnect_reports_the_applied_position_and_detects_lost_work() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui);
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");
    link.discover(&identity).expect("discover");
    let acked = link.acked_seq();
    assert!(acked >= 1);

    let state = link
        .reconnect(
            &identity,
            ReconnectState {
                last_acked_seq: acked,
            },
        )
        .expect("reconnect");
    assert!(state.last_acked_seq >= acked);

    // Requiring more acknowledged work than the worker applied is an error.
    let beyond = link.reconnect(
        &identity,
        ReconnectState {
            last_acked_seq: state.last_acked_seq + 100,
        },
    );
    assert!(matches!(beyond, Err(TransportError::Protocol(_))));
}

fn open_step(link: &mut Link, run_id: &str, fence: u64) -> (StepRef, StepSpec, IdempotencyKey) {
    let at = StepRef {
        step: "open".into(),
        take: 0,
    };
    link.grant(&at, fence).expect("grant");
    (at.clone(), step("open"), IdempotencyKey::new(run_id, &at))
}

#[test]
fn a_recorded_session_replayed_on_a_new_connection_is_rejected() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui.clone());

    // Connection 1 is the enrolment, connection 2 the recorded session.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut link: Link = ControllerLink::new(
        controller,
        connector_recording(addr, vec![None, None], Some((1, Arc::clone(&log)))),
    );
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");
    link.discover(&identity).expect("discover");
    let (at, spec, key) = open_step(&mut link, "run-replay", 1);
    RemoteDriver::new(&mut link, at, 1)
        .perform(&spec, &key)
        .expect("perform");
    assert_eq!(gui.performs.load(Ordering::SeqCst), 1);
    drop(link);

    // An attacker replays every recorded controller byte on a fresh connection.
    let recorded = log.lock().expect("log").clone();
    assert!(recorded.len() > 100);
    let mut raw = TcpStream::connect(addr).expect("connect");
    raw.write_all(&recorded).expect("replay");
    let mut reply = Vec::new();
    let _ = raw.read_to_end(&mut reply);
    // The worker sent only its fresh Identify frame, then closed: it never
    // accepted the stale Accept proof and never answered a sealed command.
    // It closes with replayed bytes still unread, which Windows turns into a connection
    // reset, so the read may end before the whole frame arrived: never more than the frame.
    if reply.len() >= 4 {
        let identify_len = u32::from_be_bytes(reply[0..4].try_into().expect("len")) as usize;
        assert!(
            reply.len() <= 4 + identify_len,
            "worker answered beyond the handshake: {} bytes, frame is {}",
            reply.len(),
            4 + identify_len
        );
    }
    assert_eq!(
        gui.performs.load(Ordering::SeqCst),
        1,
        "replay must not perform"
    );
}

#[test]
fn an_oversized_length_prefix_cannot_wedge_the_worker() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui);

    let mut raw = TcpStream::connect(addr).expect("connect");
    raw.write_all(&0x7fff_ffffu32.to_be_bytes())
        .expect("announce");
    let mut sink = Vec::new();
    let _ = raw.read_to_end(&mut sink);
    assert!(sink.is_empty(), "worker answered an oversized frame");

    // The worker still serves a legitimate controller afterwards.
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));
    let identity = link
        .enrol(&enrolment_request(&issued))
        .expect("enrol after abuse");
    link.discover(&identity).expect("discover");
}

#[test]
fn fences_survive_a_new_session_and_a_cut_handshake_does_not_desynchronise() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui);

    // Connection 2: reads fail after the Grant is written, so the worker
    // applies grant 2 but the controller never sees a reply, and the session
    // is never confirmed. Connection 3 must start a clean session anyway.
    let mut link: Link =
        ControllerLink::new(controller, connector(addr, vec![None, Some(3), None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");
    let at = StepRef {
        step: "open".into(),
        take: 0,
    };
    assert!(link.grant(&at, 2).is_err(), "the reply was cut");

    // A stale lease holder must not be able to re-grant fence 1 in the new
    // session: the high-water mark survives.
    match link.grant(&at, 1) {
        Err(TransportError::Fenced { current: 2 }) => {}
        other => panic!("fence regressed across sessions: {other:?}"),
    }
    link.discover(&identity)
        .expect("no session desync after a cut handshake");
}

#[test]
fn a_perform_stays_deduplicated_across_a_new_session() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _bundles) = start_worker(&issued.code, gui.clone());
    // Connection 2 loses the Perform reply (session unconfirmed after Grant?
    // no: Grant replied, so the session is confirmed). Connection 3 resumes.
    let mut link: Link =
        ControllerLink::new(controller, connector(addr, vec![None, Some(4), None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");
    let (at, spec, key) = open_step(&mut link, "run-dedupe", 1);
    assert!(RemoteDriver::new(&mut link, at.clone(), 1)
        .perform(&spec, &key)
        .is_err());
    link.reconnect(&identity, ReconnectState { last_acked_seq: 1 })
        .expect("reconnect");
    RemoteDriver::new(&mut link, at, 1)
        .perform(&spec, &key)
        .expect("duplicate perform answers from the record");
    assert_eq!(gui.performs.load(Ordering::SeqCst), 1);
}

#[test]
fn a_worker_discards_its_enrolment_code_after_repeated_failures() {
    let gui = Gui::new();
    let mut controller = Controller::new();
    let good = controller.issue_enrolment("gui-worker").expect("issue");
    // The worker holds the right id but a wrong secret, so every proof is
    // rejected. Each attempt needs its own grant on the controller.
    let wrong = format!("{}:{}", good.enrolment_id, "22".repeat(32));
    let endpoint = WorkerEndpoint::awaiting_enrolment("gui-worker", &wrong, gui, capabilities())
        .expect("endpoint");
    let (addr, _b) = start_endpoint(endpoint);
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None; 6]));
    // The controller burns its grant on the first attempt; drive the worker
    // directly with raw challenges for the rest.
    assert!(link.enrol(&enrolment_request(&good)).is_err());
    for _ in 0..2 {
        let mut raw = TcpStream::connect(addr).expect("connect");
        let hello = format!(
            r#"{{"kind":"enrol_challenge","enrolment_id":"{}","challenge":"aa"}}"#,
            good.enrolment_id
        );
        raw.write_all(&(hello.len() as u32).to_be_bytes())
            .expect("len");
        raw.write_all(hello.as_bytes()).expect("hello");
        // The worker sends its proof frame, then we hang up.
        let mut len = [0u8; 4];
        raw.read_exact(&mut len).expect("proof frame");
    }
    // After three failed attempts the worker no longer answers this id.
    let mut raw = TcpStream::connect(addr).expect("connect");
    let hello = format!(
        r#"{{"kind":"enrol_challenge","enrolment_id":"{}","challenge":"bb"}}"#,
        good.enrolment_id
    );
    raw.write_all(&(hello.len() as u32).to_be_bytes())
        .expect("len");
    raw.write_all(hello.as_bytes()).expect("hello");
    let mut buf = Vec::new();
    let _ = raw.read_to_end(&mut buf);
    assert!(
        buf.is_empty(),
        "worker still answers after its attempt limit"
    );
}

#[test]
fn an_expired_enrolment_grant_is_refused() {
    let mut controller = Controller::new().with_enrolment_ttl(std::time::Duration::ZERO);
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, _b) = start_worker(&issued.code, Gui::new());
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None]));
    std::thread::sleep(std::time::Duration::from_millis(5));
    match link.enrol(&enrolment_request(&issued)) {
        Err(TransportError::Auth(reason)) => assert!(reason.contains("expired")),
        other => panic!("expired grant was accepted: {other:?}"),
    }
}

#[test]
fn a_short_enrolment_code_is_refused_by_the_worker() {
    let short = "e-1:abcd";
    assert!(
        WorkerEndpoint::awaiting_enrolment("gui-worker", short, Gui::new(), capabilities())
            .is_err()
    );
}

fn payload(len: usize, salt: u8) -> (BundleRef, Vec<u8>) {
    let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 ^ salt).collect();
    let bundle = BundleRef {
        bundle_id: "shared-id".into(),
        blake3: blake3::hash(&data).to_hex().to_string(),
        bytes: data.len() as u64,
    };
    (bundle, data)
}

#[test]
fn a_different_bundle_cannot_reuse_a_verified_or_partial_bundle_id() {
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let (addr, bundles) = start_worker(&issued.code, Gui::new());
    let mut link: Link =
        ControllerLink::new(controller, connector(addr, vec![None, None])).with_chunk_bytes(512);
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");

    let (a, a_data) = payload(2_000, 0);
    let (b, b_data) = payload(2_000, 0x55);
    link.transfer_bundle(&identity, &a, &a_data)
        .expect("bundle a");

    // The same bundle again is idempotent, not an error.
    let again = link
        .transfer_bundle(&identity, &a, &a_data)
        .expect("repeat");
    assert_eq!(again.resumed_from, a.bytes);

    // Different bytes under the same id are refused and the stored bytes stay.
    assert!(matches!(
        link.transfer_bundle(&identity, &b, &b_data),
        Err(TransportError::Digest(_))
    ));
    assert_eq!(
        bundles.lock().expect("lock").get("shared-id"),
        Some(&a_data)
    );
}

#[test]
fn bundle_size_and_id_limits_are_enforced_before_any_bytes_are_kept() {
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    let endpoint =
        WorkerEndpoint::awaiting_enrolment("gui-worker", &issued.code, Gui::new(), capabilities())
            .expect("endpoint")
            .with_max_bundle_bytes(1_000);
    let (addr, bundles) = start_endpoint(endpoint);
    let mut link: Link = ControllerLink::new(controller, connector(addr, vec![None, None]));
    let identity = link.enrol(&enrolment_request(&issued)).expect("enrol");

    let (big, big_data) = payload(1_001, 0);
    assert!(matches!(
        link.transfer_bundle(&identity, &big, &big_data),
        Err(TransportError::Digest(_))
    ));

    let (mut odd, data) = payload(100, 0);
    odd.bundle_id = "../escape".into();
    assert!(matches!(
        link.transfer_bundle(&identity, &odd, &data),
        Err(TransportError::Digest(_))
    ));
    assert!(bundles.lock().expect("lock").is_empty());
}
