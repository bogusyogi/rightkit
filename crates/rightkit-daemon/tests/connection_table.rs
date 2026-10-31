//! The Unix connection table: slots kept for the supervisor's health probe, one `TableFull` event
//! per fill with occupancy by state, and the handshake deadline that frees slots held by clients
//! that connected and said nothing (rightkit#975, pulse#481). Unix only: the table cap and the
//! kernel-reported peer pid are Unix behaviours, and the fill needs raw sockets that never speak.
#![cfg(unix)]

use rightkit_daemon::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Echo;

impl Handler for Echo {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

type Events = Arc<Mutex<Vec<ServiceEvent>>>;

fn start(options: HostOptions) -> (tempfile::TempDir, Suite, Hosted, Events) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let events: Events = Arc::default();
    let mut cfg = ServiceConfig::new(
        "table",
        "engine",
        1,
        "1.0",
        Allowlist::new().allow("app", ["echo"]),
    );
    let sink = events.clone();
    cfg.on_event = Some(Arc::new(move |e: &ServiceEvent| {
        sink.lock().unwrap().push(e.clone())
    }));
    let hosted = host_with_options(&suite, cfg, Arc::new(Echo), options).expect("host starts");
    (dir, suite, hosted, events)
}

/// Small table, nobody quiet: no deadline unless a test asks for one.
fn options() -> HostOptions {
    HostOptions::new()
        .max_connections(2)
        .reserved_slots(1)
        .handshake_timeout(None)
}

/// A pid that is not this process's.
fn other_pid() -> u32 {
    std::process::id().wrapping_add(1)
}

/// A client that connected and sent nothing.
fn stall(hosted: &Hosted) -> UnixStream {
    UnixStream::connect(&hosted.record().endpoint).expect("raw connect")
}

/// Read without setting socket timeouts: an immediately refused peer may already be closed, and
/// macOS can reject SO_RCVTIMEO on that socket. Nonblocking mode only changes our descriptor.
fn read_byte_with_timeout(stream: &mut UnixStream, timeout: Duration) -> std::io::Result<usize> {
    stream.set_nonblocking(true)?;
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        match stream.read(&mut [0u8; 1]) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(
                    until
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            result => return result,
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "timed out waiting for a byte or EOF",
    ))
}

fn connect(suite: &Suite) -> Result<Client, ServiceError> {
    Client::connect_with(suite, "table", "app", Some(Duration::from_secs(5)))
}

fn pings(client: &Client) -> bool {
    client
        .call("ping", json!({}))
        .is_ok_and(|v| v["pong"] == true)
}

fn count(events: &Events, f: impl Fn(&ServiceEvent) -> bool) -> usize {
    events.lock().unwrap().iter().filter(|e| f(e)).count()
}

fn table_fulls(events: &Events) -> Vec<ServiceEvent> {
    let all = events.lock().unwrap();
    all.iter()
        .filter(|e| matches!(e, ServiceEvent::TableFull { .. }))
        .cloned()
        .collect()
}

/// Connections turned away because the table was full.
fn refusals(events: &Events) -> usize {
    count(events, |e| match e {
        ServiceEvent::PeerRejected { reason } => reason.contains("connection limit"),
        _ => false,
    })
}

/// Connections the host closed because `hello` did not arrive in time.
fn deadlines(events: &Events) -> usize {
    count(events, |e| match e {
        ServiceEvent::ConnectionRejected { app, reason } => {
            app.is_empty() && reason.starts_with("handshake deadline")
        }
        _ => false,
    })
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Is the host still holding `stream` open and quiet? A closed one answers EOF at once.
fn held_open(stream: &mut UnixStream) -> bool {
    match read_byte_with_timeout(stream, Duration::from_millis(100)) {
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => true,
        Err(e) => panic!("could not inspect stalled connection: {e}"),
        Ok(read) => {
            // The host never sends before `hello`: anything it gives back is the close.
            assert_eq!(read, 0, "the host wrote to a connection that sent nothing");
            false
        }
    }
}

/// Fill a two-slot table with stalled connections, then try `attempts` clients; every one must be
/// refused. Returns the host's events once each refusal has been recorded.
fn fill_and_refuse(options: HostOptions, attempts: usize) -> Events {
    let (_dir, suite, hosted, events) = start(options);
    let _stalled = [stall(&hosted), stall(&hosted)];
    for _ in 0..attempts {
        assert!(
            connect(&suite).is_err(),
            "a connection past the table must be refused"
        );
    }
    wait_for("every refusal to be recorded", || {
        refusals(&events) == attempts
    });
    events
}

#[test]
fn defaults_reserve_two_slots_and_bound_the_handshake() {
    let d = HostOptions::default();
    assert_eq!(d.get_reserved_slots(), 2);
    assert_eq!(DEFAULT_RESERVED_SLOTS, 2);
    assert_eq!(d.get_handshake_timeout(), Some(Duration::from_secs(5)));
    assert_eq!(DEFAULT_HANDSHAKE_TIMEOUT, Duration::from_secs(5));
    // Left alone, the reserved slots belong to the host's parent: the supervisor of a child.
    assert_eq!(
        d.get_supervisor_pid(),
        Some(std::os::unix::process::parent_id())
    );
    assert_eq!(
        HostOptions::new().supervisor_pid(None).get_supervisor_pid(),
        None
    );
}

#[test]
fn a_table_full_of_stalled_connections_still_admits_the_supervisors_probe() {
    let (_dir, suite, hosted, events) = start(options().supervisor_pid(Some(std::process::id())));
    let _stalled = [stall(&hosted), stall(&hosted)];

    // The supervisor's probe (this process) connects and pings through the reserved slot.
    let probe = connect(&suite).expect("the supervisor must get a reserved slot");
    assert!(pings(&probe));
    assert_eq!(refusals(&events), 0);
    assert!(table_fulls(&events).is_empty(), "nothing was refused");

    // One reserved slot: while the probe holds it, a second connection is refused.
    assert!(connect(&suite).is_err());
    wait_for("the refusal", || refusals(&events) == 1);
    assert_eq!(table_fulls(&events).len(), 1);

    // Once the probe lets go, the slot frees and the next probe gets in.
    drop(probe);
    wait_for("the reserved slot to free", || {
        connect(&suite).is_ok_and(|c| pings(&c))
    });
}

#[test]
fn a_peer_that_is_not_the_supervisor_is_refused_and_one_table_full_is_recorded() {
    let events = fill_and_refuse(options().supervisor_pid(Some(other_pid())), 3);
    assert_eq!(
        table_fulls(&events),
        vec![ServiceEvent::TableFull {
            limit: 2,
            reserved: 1,
            pre_handshake: 2,
            attached: 0,
            peer_pid: Some(std::process::id()),
        }],
        "three refusals in one fill are one event"
    );
}

#[test]
fn naming_no_supervisor_reserves_the_slots_for_nobody() {
    let events = fill_and_refuse(options().supervisor_pid(None), 1);
    assert_eq!(table_fulls(&events).len(), 1);
}

#[test]
fn an_unnamed_supervisor_is_the_hosts_parent_not_this_process() {
    // The host runs in this process, so its parent is the test runner. The client here is this
    // process, not the supervisor, and by default gets no reserved slot.
    let events = fill_and_refuse(options(), 1);
    assert_eq!(table_fulls(&events).len(), 1);
}

#[test]
fn table_full_counts_attached_and_pre_handshake_apart_and_rearms_after_a_drop() {
    let (_dir, suite, hosted, events) = start(options().supervisor_pid(None));
    // One attached client and one stalled connection fill the two slots.
    let attached = connect(&suite).expect("room for one");
    let mut stalled = Some(stall(&hosted));
    assert!(connect(&suite).is_err());
    wait_for("the refusal", || refusals(&events) == 1);
    assert_eq!(
        table_fulls(&events),
        vec![ServiceEvent::TableFull {
            limit: 2,
            reserved: 1,
            pre_handshake: 1,
            attached: 1,
            peer_pid: Some(std::process::id()),
        }]
    );

    // More refusals in the same fill add nothing.
    assert!(connect(&suite).is_err());
    wait_for("the second refusal", || refusals(&events) == 2);
    assert_eq!(table_fulls(&events).len(), 1);

    // A connection that ends before hello frees its slot; take it again with another stalled one.
    drop(stalled.take());
    let mut refill = None;
    wait_for("the slot to free", || {
        let mut s = stall(&hosted);
        let admitted = held_open(&mut s);
        if admitted {
            refill = Some(s);
        }
        admitted
    });
    // The table is full again, so the next refusal opens a second episode. Its pre-handshake count
    // is one: the connection that ended before hello was counted out, not left behind.
    assert!(connect(&suite).is_err());
    wait_for("a second table-full", || table_fulls(&events).len() == 2);
    let all = table_fulls(&events);
    assert!(
        matches!(
            &all[1],
            ServiceEvent::TableFull {
                pre_handshake: 1,
                attached: 1,
                ..
            }
        ),
        "{:?}",
        all[1]
    );
    assert!(pings(&attached));
    drop(refill);
}

#[test]
fn stalled_connections_are_closed_after_the_handshake_deadline() {
    let deadline = Duration::from_millis(300);
    let (_dir, suite, hosted, events) = start(
        options()
            .supervisor_pid(None)
            .handshake_timeout(Some(deadline)),
    );
    let began = Instant::now();
    // Two stalled connections fill the table; a third arrives at once and is turned away.
    let mut stalled = [stall(&hosted), stall(&hosted)];
    let mut turned_away = stall(&hosted);
    assert_eq!(
        read_byte_with_timeout(&mut turned_away, Duration::from_secs(5)).unwrap(),
        0,
        "a connection past the table is closed unread"
    );
    assert!(
        began.elapsed() < deadline,
        "the table was full for the refusal"
    );

    // The host closes each stalled connection once the deadline passes: the read is EOF.
    for s in &mut stalled {
        assert_eq!(
            read_byte_with_timeout(s, Duration::from_secs(5)).unwrap(),
            0,
            "closed by the host, not by us"
        );
    }
    let waited = began.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "closed after {waited:?}, before the deadline"
    );
    assert!(waited < Duration::from_secs(5), "closed after {waited:?}");
    wait_for("both deadlines to be recorded", || deadlines(&events) == 2);
    assert!(events.lock().unwrap().iter().any(|e| matches!(e,
        ServiceEvent::ConnectionRejected { app, reason }
            if app.is_empty() && reason == "handshake deadline 300 ms passed")));

    // The table had been full, and a normal client connects and pings now.
    assert_eq!(refusals(&events), 1);
    let client = connect(&suite).expect("the slots were freed by the deadline");
    assert!(pings(&client));
}

#[test]
fn the_deadline_ends_with_hello_and_leaves_no_watcher_after_garbage() {
    let deadline = Duration::from_millis(300);
    let (_dir, suite, hosted, events) = start(options().handshake_timeout(Some(deadline)));
    let client = connect(&suite).expect("connects");

    // A connection that sends garbage and closes ends the handshake early (the "probe or garbage"
    // return): its watcher must not fire later.
    let mut garbage = stall(&hosted);
    garbage.write_all(b"not json\n").unwrap();
    drop(garbage);

    // Well past the deadline: the established connection is untouched and nothing was cut off.
    std::thread::sleep(deadline * 3);
    assert!(
        pings(&client),
        "the deadline must not apply once hello has succeeded"
    );
    assert_eq!(
        client.call("echo", json!({"a": 1})).unwrap(),
        json!({"a": 1})
    );
    assert_eq!(deadlines(&events), 0);
}
