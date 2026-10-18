//! Finding and stopping processes by executable path and command line. Children are spawned
//! with plain `std` and carry a unique marker in their command line, so a sweep can only reach
//! them; every query that could touch other processes is a dry run.

use rightkit_child_process::{
    find_processes, sweep_matching, AdoptedIdentity, MatchSweepOptions, ProcessQuery,
};
use std::{
    io::{self, ErrorKind, Read},
    num::NonZeroU32,
    ops::{Deref, DerefMut},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc, Mutex, Once,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static COUNTER: AtomicU32 = AtomicU32::new(0);
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);
/// Hard ceiling for this whole test binary (it normally finishes in seconds). Every fixture
/// child blocks on a stdin pipe that only this process holds open, so a test that wedged would
/// otherwise leave the suite, and the children, waiting for ever.
const SUITE_DEADLINE: Duration = Duration::from_secs(180);

/// Serializes `spawn`. On macOS std creates a pipe and then marks it close-on-exec in two steps,
/// so a fork on another test thread between them can hand one fixture the write end of
/// another's stdin pipe; the pair then keep each other's stdin open and never see EOF.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());
/// Pids of fixture children that have not been reaped, for the watchdog.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static WATCHDOG: Once = Once::new();

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    // SAFETY: SIGKILL to a fixture child this binary spawned and has not reaped.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Starts (once) the thread that ends a wedged run: after [`SUITE_DEADLINE`] it kills every
/// fixture child still alive and exits the binary with a failure, instead of hanging.
fn arm_watchdog() {
    WATCHDOG.call_once(|| {
        thread::spawn(|| {
            thread::sleep(SUITE_DEADLINE);
            let pids = lock(&LIVE).clone();
            for pid in &pids {
                kill_pid(*pid);
            }
            eprintln!(
                "sweep_matching: still running after {SUITE_DEADLINE:?}; killed fixture children {pids:?}"
            );
            std::process::exit(101);
        });
    });
}

/// Retain ownership through assertions too, so a failed scan cannot leak its fixture.
struct TestChild(Child);

impl Deref for TestChild {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let pid = self.id();
        let _ = self.kill();
        if matches!(wait_bounded(self, Duration::from_secs(2)), Ok(Some(_))) {
            lock(&LIVE).retain(|live| *live != pid);
        }
    }
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A command-line token no other process has.
fn marker() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "rkmatch-{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A process that waits (on stdin, or in a sleep) with `marker` in its command line.
fn spawn_marked(marker: &str) -> TestChild {
    #[cfg(unix)]
    let mut command = {
        // `read` is a builtin, so the shell stays the process and keeps its arguments; the
        // marker is `$0`. The piped stdin is never written, so the shell waits until it is killed.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf R; read line", marker])
            .stdin(Stdio::piped());
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("[Console]::Out.Write('R'); Start-Sleep -Seconds 600 # {marker}"),
        ]);
        command.stdin(Stdio::null());
        command
    };
    arm_watchdog();
    let spawned = {
        let _spawning = lock(&SPAWN_LOCK);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a marked child")
    };
    lock(&LIVE).push(spawned.id());
    let mut child = TestChild(spawned);
    // spawn() only proves the initial exec succeeded. macOS /bin/sh can re-exec its
    // actual shell afterwards, temporarily making command-line inspection unavailable.
    // Wait for code in that final process before testing the one-shot process scan.
    let mut stdout = child.stdout.take().expect("readiness pipe");
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut ready = [0];
        let result = stdout.read_exact(&mut ready).and_then(|()| {
            if ready == *b"R" {
                Ok(())
            } else {
                Err(io::Error::other("unexpected child readiness byte"))
            }
        });
        let _ = ready_tx.send(result);
    });
    ready_rx
        .recv_timeout(CHILD_TIMEOUT)
        .expect("marked child did not become ready within 10 seconds")
        .expect("marked child readiness failed");
    child
}

fn stop(child: TestChild) {
    drop(child);
}

fn identity(child: &Child) -> AdoptedIdentity {
    AdoptedIdentity::capture(NonZeroU32::new(child.id()).unwrap())
        .expect("capture")
        .expect("the child is running")
}

#[test]
fn a_marker_finds_exactly_the_marked_child_without_signalling_it() {
    let marker = marker();
    let mut child = spawn_marked(&marker);
    let query = ProcessQuery::new().arg_containing(marker.clone());
    let found = find_processes(&query).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].pid(), child.id());
    assert!(found[0]
        .args()
        .unwrap()
        .iter()
        .any(|arg| arg.to_string_lossy().contains(&marker)));

    let report = sweep_matching(&query, &MatchSweepOptions::new().dry_run(true)).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.matched.len(), 1);
    assert_eq!(report.sweep, Default::default());
    assert!(
        child.try_wait().unwrap().is_none(),
        "a dry run must not signal"
    );
    stop(child);
}

#[test]
fn the_executable_path_and_the_marker_must_both_hold() {
    let marker = marker();
    let child = spawn_marked(&marker);
    // The executable path is compared as the OS reports it, and a launcher such as the macOS
    // `/bin/sh` may re-exec after spawn, so a path read once at spawn can go stale. Each pass
    // re-reads the child's path and queries with that, so both sides come from the same moment;
    // the child is long-lived, so the pair must agree within a bounded wait.
    let deadline = Instant::now() + Duration::from_secs(10);
    let (exe, by_exe) = loop {
        let exe = identity(&child)
            .exe()
            .expect("an executable path")
            .to_owned();
        let both = ProcessQuery::new().exe(&exe).arg_containing(marker.clone());
        let found = find_processes(&both).unwrap();
        let by_exe = find_processes(&ProcessQuery::new().exe(&exe)).unwrap();
        let ok = found.len() == 1
            && found[0].pid() == child.id()
            && by_exe.iter().any(|found| found.pid() == child.id());
        if ok || Instant::now() >= deadline {
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].pid(), child.id());
            break (exe, by_exe);
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        by_exe.iter().any(|found| found.pid() == child.id()),
        "child {} exe {exe:?} not among {} exe matches: {:?}",
        child.id(),
        by_exe.len(),
        by_exe.iter().map(|found| found.pid()).collect::<Vec<_>>()
    );
    assert!(by_exe.iter().all(|found| found.pid() != std::process::id()));

    let wrong_exe = ProcessQuery::new()
        .exe(exe.with_file_name("rk-no-such-program"))
        .arg_containing(marker.clone());
    assert!(find_processes(&wrong_exe).unwrap().is_empty());
    let wrong_marker = ProcessQuery::new()
        .exe(&exe)
        .arg_containing(format!("{marker}-not"));
    assert!(find_processes(&wrong_marker).unwrap().is_empty());
    stop(child);
}

#[cfg(unix)]
#[test]
fn an_exact_argument_token_matches_and_a_prefix_does_not() {
    let marker = marker();
    let child = spawn_marked(&marker);
    let found = find_processes(&ProcessQuery::new().arg(marker.clone())).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].pid(), child.id());
    let prefix = &marker[..marker.len() - 1];
    assert!(find_processes(&ProcessQuery::new().arg(prefix))
        .unwrap()
        .is_empty());
    stop(child);
}

#[test]
fn a_sweep_stops_the_marked_child_and_leaves_an_unmarked_one() {
    let (marker_a, marker_b) = (marker(), marker());
    let mut target = spawn_marked(&marker_a);
    let bystander = spawn_marked(&marker_b);
    let target_pid = target.id();
    // `Child::wait` closes a piped stdin first, which would make the shell read EOF and exit
    // before the sweep signals it. Keep the pipe open until the sweep is done.
    let _target_stdin = target.stdin.take();
    // The target is our own child: reap it as soon as it dies, as its parent would.
    let reaper = thread::spawn(move || wait_bounded(&mut target, CHILD_TIMEOUT));

    let report = sweep_matching(
        &ProcessQuery::new().arg_containing(marker_a),
        &MatchSweepOptions::new().grace(Duration::from_secs(5)),
    )
    .unwrap();
    assert!(
        reaper.join().unwrap().unwrap().is_some(),
        "sweep did not stop child {target_pid} within 10 seconds: {report:?}"
    );

    assert!(!report.dry_run);
    assert_eq!(report.matched.len(), 1, "{report:?}");
    assert_eq!(report.sweep.terminated, vec![target_pid], "{report:?}");
    assert!(
        report.sweep.refused.is_empty()
            && report.sweep.survivors.is_empty()
            && report.sweep.failed.is_empty(),
        "{report:?}"
    );
    let mut bystander = bystander;
    assert!(
        bystander.try_wait().unwrap().is_none(),
        "a process the query does not match must keep running"
    );
    stop(bystander);
}

#[test]
fn nothing_matching_is_an_empty_result_and_signals_nothing() {
    let query = ProcessQuery::new().arg_containing(format!("{}-nobody-has-this", marker()));
    assert!(find_processes(&query).unwrap().is_empty());
    let report = sweep_matching(&query, &MatchSweepOptions::new()).unwrap();
    assert!(report.matched.is_empty());
    assert!(!report.dry_run);
    assert_eq!(report.sweep, Default::default());
}

#[test]
fn a_query_without_criteria_is_refused_not_treated_as_everything() {
    let error = sweep_matching(&ProcessQuery::new(), &MatchSweepOptions::new()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    let error = find_processes(&ProcessQuery::new().arg_containing("")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn this_process_and_its_ancestors_are_never_matched() {
    let this = std::process::id();
    let exe = std::env::current_exe().unwrap();
    // This test binary runs from a parent (cargo or a shell) whose executable differs; a query
    // for this very executable must still never list this process.
    let found = find_processes(&ProcessQuery::new().exe(&exe)).unwrap();
    assert!(found.iter().all(|found| found.pid() != this), "{found:?}");

    // A query that this process's own command line satisfies (its executable name appears in
    // argument zero) must not list it either. Ancestors are excluded by the same table walk;
    // `src/foreign/scan.rs` unit-tests that walk.
    let name = exe.file_name().unwrap().to_string_lossy().into_owned();
    let found = find_processes(&ProcessQuery::new().arg_containing(name)).unwrap();
    assert!(found.iter().all(|found| found.pid() != this), "{found:?}");
}
