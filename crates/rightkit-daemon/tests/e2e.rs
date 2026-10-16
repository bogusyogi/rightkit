//! End-to-end: real host processes, real client connections over the
//! current-user-only socket, discovery through service.json.

use rightkit_daemon::*;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn host_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "svc_host.exe"
        } else {
            "svc_host"
        })
}

struct Host {
    child: Child,
    lines: std::sync::mpsc::Receiver<String>,
}
impl Host {
    fn start(root: &std::path::Path, role: &str, rank: u8, marker: &std::path::Path) -> Host {
        let mut child = Command::new(host_bin())
            .args([
                root.to_str().unwrap(),
                "demo",
                role,
                &rank.to_string(),
                marker.to_str().unwrap(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn host (build examples with cargo test)");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        Host { child, lines: rx }
    }
    fn line(&self, secs: u64) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(secs))
            .unwrap_or_default()
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn setup() -> (tempfile::TempDir, Suite) {
    let d = tempfile::tempdir().unwrap();
    let s = Suite::at(d.path().join("suite")).unwrap();
    (d, s)
}

#[test]
fn discovery_allowlist_and_unknown_apps() {
    let (d, suite) = setup();
    let a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));

    let rec = read_record(&suite, "demo").expect("service.json published");
    assert_eq!((rec.role.as_str(), rec.rank), ("headless", 2));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&suite.service_dir("demo")),
            0o700,
            "service dir is owner-only"
        );
        assert_eq!(
            mode(std::path::Path::new(&rec.endpoint)),
            0o600,
            "socket is owner-only"
        );
    }

    let alpha = Client::connect(&suite, "demo", "alpha").unwrap();
    assert_eq!(
        alpha.call("echo", json!({"x": 1})).unwrap(),
        json!({"x": 1})
    );
    assert_eq!(alpha.call("whoami", json!({})).unwrap()["role"], "headless");
    assert!(alpha.hello()["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "slow"));

    let beta = Client::connect(&suite, "demo", "beta").unwrap();
    assert_eq!(beta.call("echo", json!("ok")).unwrap(), json!("ok"));
    let denied = beta.call("whoami", json!({})).unwrap_err();
    assert_eq!(denied.code, ERROR_NOT_ALLOWED);
    assert!(!denied.retryable);
    let unlisted = alpha.call("secret", json!({})).unwrap_err();
    assert_eq!(
        unlisted.code, ERROR_NOT_ALLOWED,
        "a method nobody listed is never callable"
    );
    assert_eq!(
        beta.call("ping", json!({})).unwrap()["pong"],
        true,
        "ping is always allowed"
    );

    let gamma = Client::connect(&suite, "demo", "gamma")
        .err()
        .expect("unknown app rejected at hello");
    assert_eq!(gamma.code, ERROR_NOT_ALLOWED);
}

#[test]
fn higher_rank_takes_over_with_a_retryable_handoff() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));

    // An in-flight job on the incumbent, retried transparently across the handoff. The incumbent
    // looks for a takeover request from another process once a second, so the job must outlast
    // that look (and the second host's start-up) with room to spare: it ends only at the handoff.
    let s2 = suite.clone();
    let job = std::thread::spawn(move || {
        let mut events = 0;
        let r = Client::call_retrying(
            &s2,
            "demo",
            "alpha",
            "slow",
            json!({"ms": 5000}),
            RetryPolicy::default(),
            &mut |_| events += 1,
        );
        (r, events)
    });
    std::thread::sleep(Duration::from_millis(300));

    // Equal or lower rank cannot take the service.
    let mut c = Host::start(suite.root(), "cutright", 3, d.path());
    assert_eq!(c.line(20), "HELD headless");
    assert_eq!(c.child.wait().unwrap().code(), Some(3));

    // A higher-ranked host takes over; the incumbent releases and exits.
    let b = Host::start(suite.root(), "shell", 0, d.path());
    assert!(
        b.line(30).starts_with("READY"),
        "takeover host must come up"
    );
    assert_eq!(a.line(10), "RELEASED");
    assert!(a.child.wait().unwrap().success());
    assert!(
        read_takeover(&suite, "demo").is_none(),
        "requester removes takeover.json"
    );
    assert_eq!(read_record(&suite, "demo").unwrap().role, "shell");

    let (r, events) = job.join().unwrap();
    let r = r.expect("handoff error was retryable and the retry reached the new host");
    assert_eq!(r["role"], "shell", "the job completed on the new host");
    assert!(events > 0);
}

#[test]
fn client_disconnect_is_seen_mid_request() {
    let (d, suite) = setup();
    let h = Host::start(suite.root(), "headless", 2, d.path());
    assert!(h.line(20).starts_with("READY"));
    let client = std::sync::Arc::new(Client::connect(&suite, "demo", "alpha").unwrap());
    let c2 = client.clone();
    let t = std::thread::spawn(move || c2.call("slow", json!({"ms": 20000})));
    std::thread::sleep(Duration::from_millis(300));
    client.abort();
    let err = t.join().unwrap().unwrap_err();
    assert_eq!(err.code, ERROR_CONNECTION_CLOSED);
    assert!(err.retryable);
    let marker = d.path().join("cancelled");
    let until = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        marker.exists(),
        "host handler observed peer_closed() while the request was running"
    );
}

#[test]
fn stale_record_from_a_killed_host_is_replaced() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));
    a.child.kill().unwrap();
    a.child.wait().unwrap();
    assert!(
        read_record(&suite, "demo").is_some(),
        "kill -9 leaves service.json behind"
    );
    assert!(
        Client::connect(&suite, "demo", "alpha").is_err(),
        "stale record is unreachable"
    );
    let b = Host::start(suite.root(), "cutright", 3, d.path());
    assert!(
        b.line(20).starts_with("READY"),
        "a dead incumbent never blocks a new host"
    );
    assert_eq!(
        Client::connect(&suite, "demo", "alpha")
            .unwrap()
            .call("whoami", json!({}))
            .unwrap()["role"],
        "cutright"
    );
}

#[cfg(feature = "test-mode")]
#[test]
fn app_data_root_fails_closed_never_cwd_relative() {
    let cwd = tempfile::tempdir().unwrap();
    let out = Command::new(host_bin())
        .arg("--resolve")
        .current_dir(cwd.path())
        .env_clear()
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("ERR no app-data root"), "got {text:?}");
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(
        std::fs::read_dir(cwd.path()).unwrap().count(),
        0,
        "nothing created in the working directory"
    );

    // A relative override is refused too.
    let out = Command::new(host_bin())
        .arg("--resolve")
        .env_clear()
        .env(ENV_SUITE_ROOT, "relative/dir")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ERR suite root must be absolute"));

    // The explicit per-run alternative is a private temp directory that cleans up after itself.
    let kept;
    {
        let s = Suite::per_run_temp("e2e").unwrap();
        kept = s.root().to_path_buf();
        assert!(kept.is_absolute() && kept.exists());
    }
    assert!(!kept.exists());
}

#[cfg(unix)]
fn mute_bin() -> PathBuf {
    host_bin().with_file_name(if cfg!(windows) {
        "svc_mute.exe"
    } else {
        "svc_mute"
    })
}

#[cfg(unix)]
fn mute_host(root: &std::path::Path, mode: &str) -> Child {
    let mut c = Command::new(mute_bin())
        .args([root.to_str().unwrap(), "demo", mode])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mute host");
    let mut line = String::new();
    BufReader::new(c.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("READY"), "mute host ready: {line:?}");
    c
}

/// A host that accepts the connection and never answers hello, and one that
/// listens but never accepts: connect must come back with the typed timeout.
#[cfg(unix)]
#[test]
fn connect_deadline_against_silent_and_never_accepting_hosts() {
    for mode in ["accept", "noaccept"] {
        let (d, suite) = setup();
        let mut h = mute_host(suite.root(), mode);
        let t = Instant::now();
        let err = Client::connect_with(&suite, "demo", "alpha", Some(Duration::from_millis(600)))
            .err()
            .expect("must time out");
        assert_eq!(err.code, ERROR_TIMEOUT, "{mode}: {err}");
        assert!(!err.retryable, "{mode}: a timeout is not auto-retryable");
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(500) && took < Duration::from_secs(5),
            "{mode}: took {took:?}"
        );
        let _ = h.kill();
        let _ = h.wait();
        drop(d);
    }
}

/// A real host that accepts the request and never replies: the call aborts
/// the connection (the host sees the disconnect), returns the typed timeout, and
/// the request is submitted exactly once even through call_retrying.
#[test]
fn request_deadline_aborts_and_never_replays() {
    let (d, suite) = setup();
    let h = Host::start(suite.root(), "headless", 2, d.path());
    assert!(h.line(20).starts_with("READY"));

    let c =
        Client::connect_with(&suite, "demo", "alpha", Some(Duration::from_millis(700))).unwrap();
    let t = Instant::now();
    let err = c.call("hang", json!({})).unwrap_err();
    assert_eq!(err.code, ERROR_TIMEOUT, "{err}");
    assert!(!err.retryable);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "returned promptly: {:?}",
        t.elapsed()
    );
    assert_eq!(
        c.call("echo", json!({"a": 1})).unwrap_err().code,
        ERROR_CONNECTION_CLOSED,
        "aborted connection is dead"
    );
    let aborted = d.path().join("hang_aborted");
    let until = Instant::now() + Duration::from_secs(5);
    while !aborted.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(aborted.exists(), "host observed the abort");

    // call_retrying must not replay the timed-out request.
    let policy = RetryPolicy {
        max_attempts: 5,
        backoff: Duration::from_millis(50),
        request_deadline: Some(Duration::from_millis(700)),
        ..RetryPolicy::default()
    };
    let err = Client::call_retrying(
        &suite,
        "demo",
        "alpha",
        "hang",
        json!({}),
        policy,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, ERROR_TIMEOUT);
    let calls = std::fs::read(d.path().join("hang_calls")).unwrap();
    assert_eq!(
        calls.len(),
        2,
        "one invocation per explicit submit (direct call + one retrying call), none replayed"
    );
}

/// The shared convention: HeardRight, ScrapeRight and CutRight already root their shared
/// store and speech discovery at `<app data>/Orthic Labs/shared`.
#[cfg(all(target_os = "macos", feature = "test-mode"))]
#[test]
fn default_root_is_the_shared_directory_the_apps_already_use() {
    let out = Command::new(host_bin())
        .arg("--resolve")
        .env_clear()
        .env("HOME", "/Users/someone")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "ROOT /Users/someone/Library/Application Support/Orthic Labs/shared"
    );
    // HeardRight's override spellings still win, in HeardRight's order.
    let out = Command::new(host_bin())
        .arg("--resolve")
        .env_clear()
        .env("ORTHIC_SHARED_DIR", "/a")
        .env("RIGHT_SUITE_DIR", "/b")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ROOT /b");
}

#[test]
fn pid_liveness_never_signals_process_groups_and_reads_peer_records_without_a_rank() {
    assert!(pid_alive(std::process::id()));
    for bad in [0, u32::MAX, u32::MAX - 1, i32::MAX as u32 + 1] {
        assert!(!pid_alive(bad), "{bad} must not be probed");
    }
    // A HeardRight-written record (no service/rank/version/started_ms) still parses.
    let (_d, suite) = setup();
    std::fs::create_dir_all(suite.service_dir("speech-service")).unwrap();
    let hr = json!({"protocol_major": 1, "protocol_minor": 0, "role": "heard_right_shell", "pid": std::process::id(), "endpoint": "e", "engine_version": "0.9"});
    std::fs::write(
        suite.service_dir("speech-service").join("service.json"),
        hr.to_string(),
    )
    .unwrap();
    let rec = read_live_record(&suite, "speech-service").expect("live record");
    assert_eq!((rec.rank, rec.host_version()), (RANK_UNKNOWN, "0.9"));
    std::fs::write(suite.service_dir("speech-service").join("service.json"), json!({"protocol_major":1,"protocol_minor":0,"role":"x","pid":4000000000u32,"endpoint":"e","engine_version":"1"}).to_string()).unwrap();
    assert!(
        read_live_record(&suite, "speech-service").is_none(),
        "dead pid is stale"
    );
}

#[test]
fn legacy_record_published_long_after_start_is_live_and_dead_spawn_claim_is_recovered() {
    // Wave-1 / HeardRight host: `started_ms` is publication time (here far from
    // the process birth time) and there is no `process_started_ms`.
    let (d, suite) = setup();
    let a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));
    let rec = read_record(&suite, "demo").unwrap();
    assert!(rec.started_ms > 0 && rec.process_started_ms > 0);
    let mut legacy = serde_json::to_value(&rec).unwrap();
    let obj = legacy.as_object_mut().unwrap();
    obj.remove("process_started_ms");
    obj.insert("started_ms".into(), json!(rec.started_ms + 10_000));
    std::fs::write(record_path(&suite, "demo"), legacy.to_string()).unwrap();
    let live = read_live_record(&suite, "demo")
        .expect("legacy record from a host that published >2s after start is live");
    assert_eq!(live.process_started_ms, 0);
    assert!(record_process_matches(&live));
    assert!(matches!(
        adopt_or_spawn(&suite, "demo"),
        AdoptOrSpawn::Adopt(_)
    ));
    // A second host must see it as held, never delete it.
    let b = Host::start(suite.root(), "cutright", 3, d.path());
    assert!(
        !b.line(20).starts_with("READY"),
        "a live compatible record is never deleted on timestamp mismatch"
    );
    assert!(read_record(&suite, "demo").is_some());
    // A modern record whose process birth time disagrees is a reused pid.
    let mut reused = rec.clone();
    reused.process_started_ms += 3_600_000;
    assert!(!record_process_matches(&reused));

    // A leftover claim file from a dead launcher (no lock holder) is reclaimable;
    // a held claim is never taken.
    let claim = SpawnClaim::path(&suite, "demo");
    std::fs::write(&claim, "4000000000 1").unwrap();
    let got = SpawnClaim::acquire(&suite, "demo").expect("unlocked leftover claim recovered");
    assert!(matches!(
        SpawnClaim::acquire(&suite, "demo"),
        Err(SpawnClaimError::AlreadyHeld(_))
    ));
    drop(got);
    SpawnClaim::acquire(&suite, "demo").expect("released on drop");
}

fn claim_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "svc_claim.exe"
        } else {
            "svc_claim"
        })
}

#[test]
fn competing_recoverer_processes_yield_exactly_one_owner_and_killed_owner_is_reclaimable() {
    let (d, suite) = setup();
    let go = d.path().join("go");
    // Seed a stale-looking claim (dead pid, ancient timestamp) that every racer will see.
    std::fs::create_dir_all(suite.service_dir("demo")).unwrap();
    std::fs::write(SpawnClaim::path(&suite, "demo"), "4000000000 1").unwrap();
    let mut racers: Vec<Host> = Vec::new();
    for _ in 0..8 {
        let mut child = Command::new(claim_bin())
            .args([
                suite.root().to_str().unwrap(),
                "demo",
                go.to_str().unwrap(),
                "4000",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn claim racer");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        racers.push(Host { child, lines: rx });
    }
    std::thread::sleep(Duration::from_millis(500)); // all racers are polling for the go file
    std::fs::write(&go, "go").unwrap();
    let results: Vec<String> = racers.iter().map(|r| r.line(20)).collect();
    let owners: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("OWNER"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        owners.len(),
        1,
        "exactly one owner among racers: {results:?}"
    );
    assert_eq!(
        results.iter().filter(|l| l.starts_with("HELD")).count(),
        7,
        "{results:?}"
    );
    // Owner is live and still holds: this process cannot take it.
    assert!(matches!(
        SpawnClaim::acquire(&suite, "demo"),
        Err(SpawnClaimError::AlreadyHeld(_))
    ));
    // SIGKILL the owner: the claim becomes reclaimable immediately.
    let mut owner = racers.swap_remove(owners[0]);
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    drop(racers);
    let t = Instant::now();
    loop {
        match SpawnClaim::acquire(&suite, "demo") {
            Ok(_) => break,
            Err(SpawnClaimError::AlreadyHeld(_)) if t.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => panic!("killed owner's claim not reclaimable: {e}"),
        }
    }
}

fn race_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "svc_race.exe"
        } else {
            "svc_race"
        })
}

fn spawn_racer(suite: &Suite, go: &std::path::Path, marker: &std::path::Path, id: &str) -> Host {
    let mut child = Command::new(race_bin())
        .args([
            suite.root().to_str().unwrap(),
            "demo",
            go.to_str().unwrap(),
            id,
            marker.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn race racer (build examples with cargo test)");
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    Host { child, lines: rx }
}

/// Start `ids` racers at the same instant. Returns the racers and the index of the one
/// that printed `OWNER`; every other racer must print `FORWARDED`.
fn race(suite: &Suite, marker: &std::path::Path, ids: &[&str]) -> (Vec<Host>, usize) {
    let go = marker.join("go");
    let racers: Vec<Host> = ids
        .iter()
        .map(|id| spawn_racer(suite, &go, marker, id))
        .collect();
    std::thread::sleep(Duration::from_millis(500)); // every racer is waiting for the go file
    std::fs::write(&go, "go").unwrap();
    let lines: Vec<String> = racers.iter().map(|r| r.line(30)).collect();
    let owners: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("OWNER"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        owners.len(),
        1,
        "exactly one primary among racers: {lines:?}"
    );
    for (i, l) in lines.iter().enumerate() {
        if i != owners[0] {
            assert_eq!(
                l,
                &format!("FORWARDED {}", ids[i]),
                "loser {i} forwards its payload"
            );
        }
    }
    (racers, owners[0])
}

#[test]
fn eight_simultaneous_launches_elect_one_primary_and_forward_the_rest() {
    let (d, suite) = setup();
    let ids = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"];
    let (mut racers, owner_idx) = race(&suite, d.path(), &ids);
    let mut forwarded: Vec<String> = Vec::new();
    for (i, r) in racers.iter_mut().enumerate() {
        if i == owner_idx {
            continue;
        }
        let status = r.child.wait().unwrap();
        assert!(status.success(), "loser {i} must exit cleanly");
        forwarded.push(ids[i].to_string());
    }
    assert_eq!(forwarded.len(), 7);
    // Every loser's payload reached the single primary exactly once.
    let mut received: Vec<String> = std::fs::read_to_string(d.path().join("received"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    received.sort();
    forwarded.sort();
    assert_eq!(received, forwarded);
    // Release the primary and check it shuts down cleanly.
    std::fs::write(d.path().join("stop"), b"1").unwrap();
    let owner = &mut racers[owner_idx];
    assert!(owner.child.wait().unwrap().success());
}

#[test]
fn stale_record_with_dead_pid_is_replaced_without_waiting_for_takeover() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));
    let rec = read_record(&suite, "demo").unwrap();
    a.child.kill().unwrap();
    a.child.wait().unwrap();
    // Leave the socket file and rewrite the record as if its owner died long ago.
    let mut stale = serde_json::to_value(&rec).unwrap();
    stale["pid"] = json!(4_000_000_000u64);
    stale["process_started_ms"] = json!(1);
    std::fs::write(record_path(&suite, "demo"), stale.to_string()).unwrap();
    let started = Instant::now();
    let b = Host::start(suite.root(), "cutright", 3, d.path());
    assert!(
        b.line(5).starts_with("READY"),
        "a dead-pid record must not hold the service"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "takeover of a stale record must not wait for the deadline"
    );
    let now = read_record(&suite, "demo").unwrap();
    assert_eq!(now.role, "cutright");
    assert_ne!(now.pid, 4_000_000_000u32);
}

#[test]
fn dead_primary_is_taken_over_by_a_simultaneous_race_with_one_winner() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));
    a.child.kill().unwrap();
    a.child.wait().unwrap();
    let ids = ["s0", "s1", "s2", "s3"];
    let (mut racers, owner_idx) = race(&suite, d.path(), &ids);
    let mut forwarded = 0;
    for (i, r) in racers.iter_mut().enumerate() {
        if i != owner_idx {
            assert!(r.child.wait().unwrap().success(), "loser {i} exits cleanly");
            forwarded += 1;
        }
    }
    assert_eq!(forwarded, 3);
    std::fs::write(d.path().join("stop"), b"1").unwrap();
    assert!(racers[owner_idx].child.wait().unwrap().success());
}

/// One monotonic deadline covers waiting for the connection: a call queued behind a request that
/// never answers times out at its own deadline instead of waiting for the holder's, and it does
/// not abort the connection the holder is still using.
#[test]
fn queued_call_deadline_includes_the_mutex_wait() {
    let (d, suite) = setup();
    let h = Host::start(suite.root(), "headless", 2, d.path());
    assert!(h.line(20).starts_with("READY"));

    let c = std::sync::Arc::new(
        Client::connect_with(&suite, "demo", "alpha", Some(Duration::from_secs(6))).unwrap(),
    );
    let holder = {
        let c = c.clone();
        std::thread::spawn(move || c.call("hang", json!({})))
    };
    // Let the holder take the connection and write its request.
    std::thread::sleep(Duration::from_millis(500));
    let t = Instant::now();
    let err = c
        .call_events_until(
            "echo",
            json!({"a": 1}),
            Some(Instant::now() + Duration::from_millis(400)),
            &mut |_| {},
        )
        .unwrap_err();
    assert_eq!(err.code, ERROR_TIMEOUT, "{err}");
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "bounded by the queued call's own deadline, not the holder's 6 s: {:?}",
        t.elapsed()
    );
    // The holder still owns a live connection: its own deadline ends it, not the queued call.
    let held = holder.join().unwrap().unwrap_err();
    assert_eq!(held.code, ERROR_TIMEOUT, "{held}");
}

/// Release build (no `test-mode`): `--resolve` reads no variable; an empty environment and a
/// hostile one give the same account-database root.
#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    not(feature = "test-mode")
))]
#[test]
fn release_resolve_ignores_the_environment() {
    let want = format!(
        "ROOT {}",
        Suite::per_user_default().unwrap().root().display()
    );
    for env in [
        vec![],
        vec![
            (ENV_SUITE_ROOT, "/evil"),
            ("RIGHT_SUITE_DIR", "/evil"),
            ("ORTHIC_SHARED_DIR", "/evil"),
            ("HOME", "/evil"),
            ("XDG_DATA_HOME", "/evil"),
        ],
    ] {
        let out = Command::new(host_bin())
            .arg("--resolve")
            .env_clear()
            .envs(env)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), want);
    }
}
