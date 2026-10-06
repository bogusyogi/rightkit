//! End-to-end: real host processes, real client connections over the
//! current-user-only socket, discovery through service.json.

use rightkit_service::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn host_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join("examples").join(if cfg!(windows) { "svc_host.exe" } else { "svc_host" })
}

struct Host {
    child: Child,
    lines: std::sync::mpsc::Receiver<String>,
}
impl Host {
    fn start(root: &std::path::Path, role: &str, rank: u8, marker: &std::path::Path) -> Host {
        let mut child = Command::new(host_bin())
            .args([root.to_str().unwrap(), "demo", role, &rank.to_string(), marker.to_str().unwrap()])
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
        self.lines.recv_timeout(Duration::from_secs(secs)).unwrap_or_default()
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
        assert_eq!(mode(&suite.service_dir("demo")), 0o700, "service dir is owner-only");
        assert_eq!(mode(std::path::Path::new(&rec.endpoint)), 0o600, "socket is owner-only");
    }

    let alpha = Client::connect(&suite, "demo", "alpha").unwrap();
    assert_eq!(alpha.call("echo", json!({"x": 1})).unwrap(), json!({"x": 1}));
    assert_eq!(alpha.call("whoami", json!({})).unwrap()["role"], "headless");
    assert!(alpha.hello()["methods"].as_array().unwrap().iter().any(|m| m == "slow"));

    let beta = Client::connect(&suite, "demo", "beta").unwrap();
    assert_eq!(beta.call("echo", json!("ok")).unwrap(), json!("ok"));
    let denied = beta.call("whoami", json!({})).unwrap_err();
    assert_eq!(denied.code, ERROR_NOT_ALLOWED);
    assert!(!denied.retryable);
    let unlisted = alpha.call("secret", json!({})).unwrap_err();
    assert_eq!(unlisted.code, ERROR_NOT_ALLOWED, "a method nobody listed is never callable");
    assert_eq!(beta.call("ping", json!({})).unwrap()["pong"], true, "ping is always allowed");

    let gamma = Client::connect(&suite, "demo", "gamma").err().expect("unknown app rejected at hello");
    assert_eq!(gamma.code, ERROR_NOT_ALLOWED);
}

#[test]
fn higher_rank_takes_over_with_a_retryable_handoff() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));

    // An in-flight job on the incumbent, retried transparently across the handoff.
    let s2 = suite.clone();
    let job = std::thread::spawn(move || {
        let mut events = 0;
        let r = Client::call_retrying(&s2, "demo", "alpha", "slow", json!({"ms": 1500}), RetryPolicy::default(), &mut |_| events += 1);
        (r, events)
    });
    std::thread::sleep(Duration::from_millis(300));

    // Equal or lower rank cannot take the service.
    let mut c = Host::start(suite.root(), "cutright", 3, d.path());
    assert_eq!(c.line(20), "HELD headless");
    assert_eq!(c.child.wait().unwrap().code(), Some(3));

    // A higher-ranked host takes over; the incumbent releases and exits.
    let b = Host::start(suite.root(), "shell", 0, d.path());
    assert!(b.line(30).starts_with("READY"), "takeover host must come up");
    assert_eq!(a.line(10), "RELEASED");
    assert!(a.child.wait().unwrap().success());
    assert!(read_takeover(&suite, "demo").is_none(), "requester removes takeover.json");
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
    assert!(marker.exists(), "host handler observed peer_closed() while the request was running");
}

#[test]
fn stale_record_from_a_killed_host_is_replaced() {
    let (d, suite) = setup();
    let mut a = Host::start(suite.root(), "headless", 2, d.path());
    assert!(a.line(20).starts_with("READY"));
    a.child.kill().unwrap();
    a.child.wait().unwrap();
    assert!(read_record(&suite, "demo").is_some(), "kill -9 leaves service.json behind");
    assert!(Client::connect(&suite, "demo", "alpha").is_err(), "stale record is unreachable");
    let b = Host::start(suite.root(), "cutright", 3, d.path());
    assert!(b.line(20).starts_with("READY"), "a dead incumbent never blocks a new host");
    assert_eq!(Client::connect(&suite, "demo", "alpha").unwrap().call("whoami", json!({})).unwrap()["role"], "cutright");
}

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
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0, "nothing created in the working directory");

    // A relative override is refused too.
    let out = Command::new(host_bin()).arg("--resolve").env_clear().env(ENV_SUITE_ROOT, "relative/dir").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ERR suite root must be absolute"));

    // The explicit per-run alternative is a private temp directory that cleans up after itself.
    let kept;
    {
        let s = Suite::per_run_temp("e2e").unwrap();
        kept = s.root().to_path_buf();
        assert!(kept.is_absolute() && kept.exists());
        let _: Value = json!(null);
    }
    assert!(!kept.exists());
}

fn mute_bin() -> PathBuf {
    host_bin().with_file_name(if cfg!(windows) { "svc_mute.exe" } else { "svc_mute" })
}

#[cfg(unix)]
fn mute_host(root: &std::path::Path, mode: &str) -> Child {
    let mut c = Command::new(mute_bin()).args([root.to_str().unwrap(), "demo", mode]).stdout(Stdio::piped()).spawn().expect("spawn mute host");
    let mut line = String::new();
    BufReader::new(c.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
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
        let err = Client::connect_with(&suite, "demo", "alpha", Some(Duration::from_millis(600))).err().expect("must time out");
        assert_eq!(err.code, ERROR_TIMEOUT, "{mode}: {err}");
        assert!(!err.retryable, "{mode}: a timeout is not auto-retryable");
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(500) && took < Duration::from_secs(5), "{mode}: took {took:?}");
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

    let c = Client::connect_with(&suite, "demo", "alpha", Some(Duration::from_millis(700))).unwrap();
    let t = Instant::now();
    let err = c.call("hang", json!({})).unwrap_err();
    assert_eq!(err.code, ERROR_TIMEOUT, "{err}");
    assert!(!err.retryable);
    assert!(t.elapsed() < Duration::from_secs(5), "returned promptly: {:?}", t.elapsed());
    assert_eq!(c.call("echo", json!({"a": 1})).unwrap_err().code, ERROR_CONNECTION_CLOSED, "aborted connection is dead");
    let aborted = d.path().join("hang_aborted");
    let until = Instant::now() + Duration::from_secs(5);
    while !aborted.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(aborted.exists(), "host observed the abort");

    // call_retrying must not replay the timed-out request.
    let policy = RetryPolicy { max_attempts: 5, backoff: Duration::from_millis(50), request_deadline: Some(Duration::from_millis(700)), ..RetryPolicy::default() };
    let err = Client::call_retrying(&suite, "demo", "alpha", "hang", json!({}), policy, &mut |_| {}).unwrap_err();
    assert_eq!(err.code, ERROR_TIMEOUT);
    let calls = std::fs::read(d.path().join("hang_calls")).unwrap();
    assert_eq!(calls.len(), 2, "one invocation per explicit submit (direct call + one retrying call), none replayed");
}

/// The shared convention: HeardRight, ScrapeRight and CutRight already root their shared
/// store and speech discovery at `<app data>/Orthic Labs/shared`.
#[cfg(target_os = "macos")]
#[test]
fn default_root_is_the_shared_directory_the_apps_already_use() {
    let out = Command::new(host_bin()).arg("--resolve").env_clear().env("HOME", "/Users/someone").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ROOT /Users/someone/Library/Application Support/Orthic Labs/shared");
    // HeardRight's override spellings still win, in HeardRight's order.
    let out = Command::new(host_bin()).arg("--resolve").env_clear().env("ORTHIC_SHARED_DIR", "/a").env("RIGHT_SUITE_DIR", "/b").output().unwrap();
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
    std::fs::write(suite.service_dir("speech-service").join("service.json"), hr.to_string()).unwrap();
    let rec = read_live_record(&suite, "speech-service").expect("live record");
    assert_eq!((rec.rank, rec.host_version()), (RANK_UNKNOWN, "0.9"));
    std::fs::write(suite.service_dir("speech-service").join("service.json"), json!({"protocol_major":1,"protocol_minor":0,"role":"x","pid":4000000000u32,"endpoint":"e","engine_version":"1"}).to_string()).unwrap();
    assert!(read_live_record(&suite, "speech-service").is_none(), "dead pid is stale");
}
