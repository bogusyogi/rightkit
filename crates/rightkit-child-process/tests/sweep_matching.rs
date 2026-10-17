//! Finding and stopping processes by executable path and command line. Children are spawned
//! with plain `std` and carry a unique marker in their command line, so a sweep can only reach
//! them; every query that could touch other processes is a dry run.

use rightkit_child_process::{
    find_processes, sweep_matching, AdoptedIdentity, MatchSweepOptions, ProcessQuery,
};
use std::{
    io::ErrorKind,
    num::NonZeroU32,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

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
fn spawn_marked(marker: &str) -> Child {
    #[cfg(unix)]
    let mut command = {
        // `read` is a builtin, so the shell stays the process and keeps its arguments; the
        // marker is `$0`. The piped stdin is never written, so the shell waits until it is killed.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "read line", marker])
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
            &format!("Start-Sleep -Seconds 600 # {marker}"),
        ]);
        command.stdin(Stdio::null());
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a marked child")
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
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
    let reaper = thread::spawn(move || target.wait());

    let report = sweep_matching(
        &ProcessQuery::new().arg_containing(marker_a),
        &MatchSweepOptions::new().grace(Duration::from_secs(5)),
    )
    .unwrap();
    reaper.join().unwrap().unwrap();

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
