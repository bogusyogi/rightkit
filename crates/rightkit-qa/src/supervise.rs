//! Runner side of the runner <-> worker contract (ADR-0001 item 3, rulings D-a, D-c, D-g; Sage C7, C10).
//!
//! [`run_worker`] runs one scenario in one supervised worker: it writes `scenario.json`, starts
//! `<exe> worker <world dir>` (the same `rightkit-qa` executable, see [`crate::worker_main`]) inside a
//! containment the runner alone holds, enforces the wall-clock deadline, records every process the
//! scenario left behind **before** any cleanup, then terminates the containment.
//!
//! * Windows: a fresh kill-on-close Job Object that allows no breakaway (`WindowsJob`). A descendant
//!   asking for `CREATE_BREAKAWAY_FROM_JOB` is refused by the OS (access denied) and never exists, so
//!   the job's process-id list is every live member. A process created outside the job tree is not
//!   seen here (a Residual risk in the ADR; the world-marker scan of D-c is the second net).
//! * Unix: a new process group. Members are the processes in that group plus descendants of the worker
//!   found by parent links. A descendant that leaves the group (`setsid`, double fork) is still found
//!   when it was seen alive while the scenario ran: the supervisor samples the process table while it
//!   waits and keeps each sample's identity (pid plus start time), so a recycled pid is never mistaken
//!   for one. One that is created and leaves between two samples, after the worker exited, is the
//!   same Residual risk.
//! * macOS `open` launches are not in either: launchd starts them. They are tracked by bundle path and
//!   start time through [`SuperviseOptions::app_bundles`] (see [`app_bundles_of`] for where the list
//!   comes from) and, independently, by the world marker below.
//! * World marker (D-c, Unix): every process of the world carries the marker variable the runner puts
//!   in the worker's environment, and `open` passes it on with `--env`. The scan step lists the
//!   processes of the current user, reads each one's environment (macOS `KERN_PROCARGS2` through
//!   `sysctl`, Linux `/proc/<pid>/environ`) and records every match that the containment did not already
//!   name as a [`SurvivorOwner::WorldMarker`] survivor, before any kill. It finds a process that left
//!   the group and the parent chain between two samples (`setsid`, double fork). A process that scrubs
//!   its environment escapes it (a Residual risk in the ADR) unless it descends from a registered root.
//!   After the cleanup the scan runs again and a process that still carries the marker is reported.
//!   The Windows Job Object needs no scan: it lists every member itself.
//!
//! Nothing here kills by name. Every kill is a Job Object, a process group, or an identity-pinned
//! signal to a pid the supervisor recorded. A survivor is always recorded in the outcome before it is
//! killed; the runner never cleans up silently.
use crate::evidence::CheckStatus;
use crate::util::{err, tail, Error, Result};
use crate::worker::{
    self, EnvMarker, ScenarioSpec, Survivor, SurvivorOwner, WorkerOutcome, OUTCOME_FILE,
};
use crate::worker_main::{errored_outcome, LOG_FILE, START_LINE, WORKER_SUBCOMMAND};
use rightkit_child_process::{process_table, AdoptedIdentity, AdoptedSignal};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// How long after the worker exits a member still alive is given to finish exiting before it counts
/// as a survivor (a process the worker stopped a moment ago may still be tearing down).
const SURVIVOR_GRACE: Duration = Duration::from_millis(500);
/// How long the supervisor waits for the containment to empty after it killed it.
const KILL_CONFIRM: Duration = Duration::from_secs(5);
/// Poll interval of the wait loop.
const POLL: Duration = Duration::from_millis(15);
/// Longest wall clock a worker is ever given, whatever the deadlines say (guards the addition).
const MAX_WALL: Duration = Duration::from_secs(24 * 60 * 60);
/// Slack when comparing a process start time with the moment the worker was started (clock
/// granularity of the host's start times).
const START_SLACK: Duration = Duration::from_secs(1);

/// What the caller can add to a supervised run.
#[derive(Debug, Clone, Default)]
pub struct SuperviseOptions {
    /// Application bundles (`.app` directories) the scenario launches through `open`. Any process whose
    /// executable lies inside one of them and that started after the worker did is a survivor candidate
    /// (macOS `open` launches leave both the group and the job). Empty means none are scanned.
    pub app_bundles: Vec<PathBuf>,
}

/// The application bundles a scenario may launch through `open`, for [`run_worker`].
///
/// Two sources, both read without running anything:
/// * `appBundles` in the resolved scenario (`spec.scenario`), an array of absolute `.app` paths, for a
///   launch the runner's own staging does not produce;
/// * macOS only: every `<name>.app` the runner staged for QA launches, at the fixed per-app location
///   `<home base>/rightkit-qa-apps/<app>/<name>.app` (`control::stage_bundle`). The path is fixed per
///   app, not per run, so an entry here can also be another scenario's or the user's instance; the
///   scan only takes a process that started after this worker did and that does not carry another
///   world's marker (see [`scan_bundles`]).
pub fn app_bundles_of(spec: &ScenarioSpec) -> Vec<PathBuf> {
    let mut bundles: Vec<PathBuf> = spec
        .scenario
        .get("appBundles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.extension().is_some_and(|e| e == "app"))
        .collect();
    #[cfg(target_os = "macos")]
    bundles.extend(staged_bundles(
        &crate::storage::home_base().join("rightkit-qa-apps"),
    ));
    bundles.sort();
    bundles.dedup();
    bundles
}

/// `<root>/<app>/<name>.app` directories (one level of `<app>`); an unreadable root has none.
#[cfg(any(target_os = "macos", test))]
fn staged_bundles(root: &Path) -> Vec<PathBuf> {
    let Ok(apps) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for app in apps.flatten() {
        let Ok(inner) = std::fs::read_dir(app.path()) else {
            continue;
        };
        for entry in inner.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "app") && path.is_dir() {
                found.push(path);
            }
        }
    }
    found
}

/// Run `spec` in a supervised worker started from `exe` (normally `std::env::current_exe()`).
///
/// `Err` means the supervisor itself could not do its job (the world cannot be written, the worker
/// cannot be started or contained): the caller reports that scenario `errored`. Everything the worker
/// does or fails to do is an `Ok` outcome: a missing or refused `outcome.json`, a non-zero exit, a
/// blown deadline and any survivor all come back as `errored`, never as a pass.
pub fn run_worker(spec: &ScenarioSpec, exe: &Path) -> Result<WorkerOutcome> {
    run_worker_with(
        spec,
        exe,
        &SuperviseOptions {
            app_bundles: app_bundles_of(spec),
        },
    )
}

/// [`run_worker`] with explicit [`SuperviseOptions`].
pub fn run_worker_with(
    spec: &ScenarioSpec,
    exe: &Path,
    options: &SuperviseOptions,
) -> Result<WorkerOutcome> {
    let launch = Launch {
        program: exe.to_path_buf(),
        args: vec![
            OsString::from(WORKER_SUBCOMMAND),
            spec.world.dir.clone().into_os_string(),
        ],
        env: Vec::new(),
    };
    supervise(spec, &launch, options)
}

/// What to execute as the worker. Always `<exe> worker <dir>` outside tests; the tests run the test
/// binary as a helper through the same path, with extra environment to select its behaviour.
struct Launch {
    program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(String, String)>,
}

/// How the wait for the worker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    Exited { success: bool, how: String },
    Deadline { limit_ms: u64 },
    WaitFailed(String),
}

/// What the supervisor saw of the containment: processes left, and anything that stops it from
/// claiming there were none (a failed query, a kill that did not empty the containment).
#[derive(Debug, Default)]
struct Scan {
    survivors: Vec<Survivor>,
    problems: Vec<String>,
}

fn supervise(
    spec: &ScenarioSpec,
    launch: &Launch,
    options: &SuperviseOptions,
) -> Result<WorkerOutcome> {
    spec.validate()?;
    let dir = &spec.world.dir;
    // A file from an earlier attempt must never be read as this worker's answer.
    match std::fs::remove_file(dir.join(OUTCOME_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return err(format!(
                "cannot clear the previous {OUTCOME_FILE} in {}: {e}",
                dir.display()
            ))
        }
    }
    worker::write_scenario(spec)?;
    let log_path = dir.join(LOG_FILE);
    let log = File::create(&log_path)
        .map_err(|e| Error(format!("cannot create {}: {e}", log_path.display())))?;
    let log_err = log
        .try_clone()
        .map_err(|e| Error(format!("cannot share {}: {e}", log_path.display())))?;

    let mut containment = contain::Containment::new()?;
    // windowless: `containment.prepare` sets CREATE_NO_WINDOW on Windows.
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        // D-c: every process of the world carries the marker, so a same-user scan can find strays.
        .env(&spec.world.env_marker.name, &spec.world.env_marker.value);
    for (key, value) in &launch.env {
        cmd.env(key, value);
    }
    containment.prepare(&mut cmd);

    let not_before = SystemTime::now();
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        Error(format!(
            "cannot start the worker {}: {e}",
            launch.program.display()
        ))
    })?;
    let worker_pid = child.id();
    // Contain first, release second: the worker reads its start line before it does anything, so
    // nothing it creates can exist outside the containment.
    if let Err(e) = containment.attach(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return err(format!("cannot contain the worker (pid {worker_pid}): {e}"));
    }
    if let Some(mut stdin) = child.stdin.take() {
        // A failed write means the worker is already gone; the wait below reports that.
        let _ = writeln!(stdin, "{START_LINE}").and_then(|()| stdin.flush());
    }

    let limit = Duration::from_millis(
        spec.deadlines
            .scenario_ms
            .saturating_add(spec.deadlines.teardown_ms),
    )
    .min(MAX_WALL);
    let deadline = started + limit;
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break Ended::Exited {
                    success: status.success(),
                    how: status.to_string(),
                }
            }
            Ok(None) => {}
            Err(e) => break Ended::WaitFailed(e.to_string()),
        }
        if Instant::now() >= deadline {
            break Ended::Deadline {
                limit_ms: limit.as_millis() as u64,
            };
        }
        containment.observe(worker_pid);
        std::thread::sleep(POLL);
    };
    let worker_exited = matches!(ended, Ended::Exited { .. });

    // Record first. After a normal exit the survivors are what is left once the worker has finished
    // its own teardown; on a deadline they are what was still running when the tree was killed.
    let mut scan = if worker_exited {
        settle_scan(&mut containment, worker_pid, SURVIVOR_GRACE)
    } else {
        containment.scan(worker_pid)
    };
    // The world-marker scan (D-c) runs next, still before any kill: it names what left the group and
    // the parent chain. What the containment already named is not named twice.
    let marker = &spec.world.env_marker;
    let mut skip: Vec<u32> = scan.survivors.iter().map(|s| s.pid).collect();
    skip.push(worker_pid);
    let (mut hits, marker_problems) = scan_world_marker(marker, &skip);
    scan.problems.extend(marker_problems);
    let (bundle_hits, bundle_problems) = scan_bundles(&options.app_bundles, not_before, marker);
    scan.problems.extend(bundle_problems);
    for hit in bundle_hits {
        let pid = hit.survivor.pid;
        if pid != worker_pid && !skip.contains(&pid) && !hits.iter().any(|h| h.survivor.pid == pid)
        {
            hits.push(hit);
        }
    }
    scan.survivors
        .extend(hits.iter().map(|h| h.survivor.clone()));

    // Then clean up, and say so if cleanup did not finish.
    scan.problems.extend(containment.kill(worker_pid));
    for hit in &hits {
        let _ = hit.identity.signal(AdoptedSignal::Force);
    }
    if !worker_exited {
        let _ = child.kill();
        scan.problems.extend(reap(&mut child));
    }
    scan.problems.extend(containment.confirm_gone());
    for hit in &hits {
        match hit.identity.wait_confirmed_gone(KILL_CONFIRM) {
            Ok(true) => {}
            Ok(false) => scan.problems.push(format!(
                "pid {} ({}) was still running after it was killed",
                hit.survivor.pid, hit.survivor.name
            )),
            Err(e) => scan.problems.push(format!(
                "cannot confirm pid {} ({}) is gone: {e}",
                hit.survivor.pid, hit.survivor.name
            )),
        }
    }

    // Last look: anything that still carries the marker after the cleanup (a process started while
    // the kill ran, or one the kill could not remove) is a problem, never a silent pass.
    let (left, left_problems) = scan_world_marker(marker, &[worker_pid]);
    scan.problems.extend(left_problems);
    for hit in &left {
        scan.problems.push(format!(
            "pid {} ({}) still carries the world marker after the cleanup",
            hit.survivor.pid, hit.survivor.name
        ));
    }

    let read = if worker_exited {
        worker::read_outcome(dir, spec)
    } else {
        err("the worker did not finish")
    };
    let log_tail = std::fs::read(&log_path)
        .map(|b| tail(String::from_utf8_lossy(&b).trim(), 600))
        .unwrap_or_default();
    Ok(settle_outcome(spec, &ended, read, scan, &log_tail))
}

/// Re-scan until the containment holds no process other than the worker or `grace` runs out; the last
/// scan is the record.
fn settle_scan(c: &mut contain::Containment, worker_pid: u32, grace: Duration) -> Scan {
    let until = Instant::now() + grace;
    loop {
        let scan = c.scan(worker_pid);
        if scan.survivors.is_empty() || Instant::now() >= until {
            return scan;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait up to five seconds for a killed worker to be reaped.
fn reap(child: &mut Child) -> Vec<String> {
    let until = Instant::now() + KILL_CONFIRM;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Vec::new(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                return vec![format!(
                    "the worker (pid {}) was not reaped within {} s of the kill",
                    child.id(),
                    KILL_CONFIRM.as_secs()
                )]
            }
            Err(e) => return vec![format!("cannot reap the worker: {e}")],
        }
    }
}

fn join(message: &mut String, add: &str) {
    if !message.is_empty() {
        message.push_str("; ");
    }
    message.push_str(add);
}

fn describe_survivors(survivors: &[Survivor]) -> String {
    let shown: Vec<String> = survivors
        .iter()
        .take(8)
        .map(|s| format!("{} ({})", s.pid, s.name))
        .collect();
    let more = survivors.len().saturating_sub(shown.len());
    format!(
        "{} process(es) outlived the scenario and were killed by the supervisor: {}{}",
        survivors.len(),
        shown.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    )
}

/// The verdict of one supervised run, from what the worker reported and what the supervisor saw.
/// Pure: no process or file is touched here, so every rule below has a unit test.
///
/// * A deadline or a failed wait is `errored` with a message that starts `deadline` / `could not wait`.
/// * A worker that exited without a usable outcome is `errored`; so is a pass from a worker that
///   exited non-zero.
/// * Any survivor turns a pass into `errored`; a non-pass keeps its status and gains the survivors.
/// * Anything that stops the supervisor from proving the containment empty (a failed query, a kill
///   that did not finish) also turns a pass into `errored`.
fn settle_outcome(
    spec: &ScenarioSpec,
    ended: &Ended,
    read: Result<WorkerOutcome>,
    scan: Scan,
    log_tail: &str,
) -> WorkerOutcome {
    let with_log = |mut text: String| {
        if !log_tail.is_empty() {
            text.push_str(&format!(" [worker log tail: {log_tail}]"));
        }
        text
    };
    let mut out = match ended {
        Ended::Deadline { limit_ms } => errored_outcome(
            spec,
            with_log(format!(
                "deadline: the worker did not finish within {limit_ms} ms (scenario {} ms + teardown {} ms); its process tree was killed",
                spec.deadlines.scenario_ms, spec.deadlines.teardown_ms
            )),
        ),
        Ended::WaitFailed(e) => errored_outcome(
            spec,
            with_log(format!(
                "could not wait for the worker: {e}; its process tree was killed"
            )),
        ),
        Ended::Exited { success, how } => match read {
            Ok(mut outcome) => {
                if !*success && outcome.status == CheckStatus::Passed {
                    outcome.status = CheckStatus::Errored;
                    join(
                        &mut outcome.message,
                        &format!("the worker exited with {how} after it reported a pass"),
                    );
                }
                outcome
            }
            Err(e) => errored_outcome(
                spec,
                with_log(format!(
                    "the worker ({how}) left no usable outcome: {e}"
                )),
            ),
        },
    };

    for found in scan.survivors {
        if !out.survivors.iter().any(|s| s.pid == found.pid) {
            out.survivors.push(found);
        }
    }
    let mut note = String::new();
    if !out.survivors.is_empty() {
        note = describe_survivors(&out.survivors);
    }
    for problem in &scan.problems {
        join(&mut note, problem);
    }
    if !note.is_empty() {
        if out.status == CheckStatus::Passed {
            out.status = CheckStatus::Errored;
        }
        join(&mut out.message, &note);
    }
    out
}

// ------------------------------------------------------------ world marker and macOS `open` bundles

/// A process found outside the containment (by world marker or app bundle): what to record and the
/// identity that makes a later signal safe.
struct BundleHit {
    survivor: Survivor,
    identity: AdoptedIdentity,
}

/// Raw bytes of an `environ` block (`NAME=value` entries separated by NUL); empty entries are dropped.
#[cfg(any(unix, test))]
fn parse_environ_block(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split(|b| *b == 0).filter(|e| !e.is_empty()).collect()
}

/// The environment of a `KERN_PROCARGS2` reply: `int argc`, the executable path, NUL padding, `argc`
/// argument strings, then the environment strings (one `NAME=value` each, NUL terminated). `None` when
/// the layout does not hold (short buffer, negative argc, an argument that is not terminated).
/// Known limit: an empty first argument is indistinguishable from the padding after the path.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<&[u8]>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let mut pos = 4;
    pos += buf[pos..].iter().position(|b| *b == 0)?;
    while pos < buf.len() && buf[pos] == 0 {
        pos += 1;
    }
    for _ in 0..argc {
        pos += buf.get(pos..)?.iter().position(|b| *b == 0)? + 1;
    }
    Some(parse_environ_block(buf.get(pos..)?))
}

/// Size of the buffer for one `KERN_PROCARGS2` read, given the size the kernel reported for the
/// process (`needed`) and the ceiling it accepts (`cap`, at most the kernel's `ARG_MAX`): a quarter
/// plus a little slack over `needed` so a process that grew between the probe and the read still
/// fits, never more than `cap`, never less than the argc word.
#[cfg(any(target_os = "macos", test))]
fn procargs_buffer_len(needed: usize, cap: usize) -> usize {
    needed
        .saturating_add(needed / 4)
        .saturating_add(64)
        .min(cap)
        .max(8)
}

/// The value of `name` in `entries` (the first entry spelled `name=...`).
#[cfg(any(unix, test))]
fn entry_value<'a>(entries: &[&'a [u8]], name: &str) -> Option<&'a [u8]> {
    let name = name.as_bytes();
    entries.iter().find_map(|e| {
        e.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(b"="))
    })
}

/// One row of `ps -A -o pid= -o uid= -o stat= -o comm=`.
#[cfg(any(unix, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserRow {
    pid: u32,
    uid: u32,
    zombie: bool,
    comm: String,
}

#[cfg(any(unix, test))]
fn parse_user_rows(text: &str) -> Vec<UserRow> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let uid = parts.next()?.parse().ok()?;
            let stat = parts.next()?;
            let comm = parts.collect::<Vec<_>>().join(" ");
            Some(UserRow {
                pid,
                uid,
                zombie: stat.starts_with('Z'),
                comm,
            })
        })
        .collect()
}

/// A failure to read one process's environment that only means "not ours to read or already gone":
/// the process exited (`ESRCH`, `ENOENT`, macOS `EINVAL` for a pid that no longer exists) or the
/// system refuses to show it to this user (`EPERM`, `EACCES`). Anything else is reported.
#[cfg(any(unix, test))]
fn is_benign_read_failure(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied)
        || matches!(e.raw_os_error(), Some(3 | 22))
}

/// Same-user processes whose environment holds the world marker, other than those in `skip` and this
/// process.
///
/// Known limit (macOS): the kernel hides the environment of Apple platform binaries (`/bin/sh`,
/// `/bin/sleep`, ...) from a same-user `KERN_PROCARGS2` reader (SIP; verified on macOS 27). An
/// escaped platform binary that carries the marker is therefore invisible here, so D-c on macOS
/// covers app and toolchain binaries, not Apple system binaries. Those are covered by the
/// containment: the process-group scan and the parent-chain sample (`contain`), which do not
/// depend on the environment. There is deliberately no "nothing was readable" check: most
/// same-user processes may be platform binaries, so an empty read is normal; the own-environment
/// canary in `EnvReader::new` is what proves the read path works.
///
/// Each hit is captured with its identity so a later signal cannot reach a recycled pid. The
/// problems say what could not be proven (a failed listing, an environment that could not be read).
#[cfg(unix)]
fn scan_world_marker(marker: &EnvMarker, skip: &[u32]) -> (Vec<BundleHit>, Vec<String>) {
    let procs = match marker_scan::same_user_processes() {
        Ok(p) => p,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!(
                    "cannot scan for processes carrying the world marker: {e}"
                )],
            )
        }
    };
    let mut reader = match marker_scan::EnvReader::new() {
        Ok(r) => r,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!(
                    "cannot read process environments for the world marker scan: {e}"
                )],
            )
        }
    };
    let want = marker.value.as_bytes();
    let mut hits = Vec::new();
    let mut problems = Vec::new();
    let mut unreadable = 0usize;
    let mut first_unreadable = String::new();
    for p in procs {
        if skip.contains(&p.pid) {
            continue;
        }
        match reader.value_of(p.pid, &marker.name) {
            Ok(Some(v)) if v == want => {}
            Ok(_) => continue,
            Err(e) if is_benign_read_failure(&e) => continue,
            Err(e) => {
                unreadable += 1;
                if first_unreadable.is_empty() {
                    first_unreadable = format!("pid {} ({}): {e}", p.pid, p.comm);
                }
                continue;
            }
        }
        let Some(pid) = NonZeroU32::new(p.pid) else {
            continue;
        };
        match AdoptedIdentity::capture(pid) {
            Ok(Some(identity)) => hits.push(BundleHit {
                survivor: Survivor {
                    pid: p.pid,
                    name: file_name(Path::new(&p.comm)),
                    owner: SurvivorOwner::WorldMarker,
                    detail: format!(
                        "carries the world marker {} but is outside the worker's process group and parent chain",
                        marker.name
                    ),
                },
                identity,
            }),
            // It exited between the read and now: not a survivor.
            Ok(None) => {}
            Err(e) => problems.push(format!(
                "pid {} ({}) carries the world marker but its identity cannot be read, so it cannot be killed safely: {e}",
                p.pid, p.comm
            )),
        }
    }
    if unreadable > 0 {
        problems.push(format!(
            "the world marker scan could not read the environment of {unreadable} process(es), first {first_unreadable}"
        ));
    }
    (hits, problems)
}

/// Windows: the Job Object lists every member itself, so there is nothing to scan.
#[cfg(not(unix))]
fn scan_world_marker(_marker: &EnvMarker, _skip: &[u32]) -> (Vec<BundleHit>, Vec<String>) {
    (Vec::new(), Vec::new())
}

/// `true` when the process has a world marker variable with another value: it belongs to another
/// scenario's world (an app bundle path is shared by every scenario of the same app).
#[cfg(unix)]
fn carries_other_world(marker: &EnvMarker, pid: u32) -> bool {
    let Ok(mut reader) = marker_scan::EnvReader::new() else {
        return false;
    };
    matches!(
        reader.value_of(pid, &marker.name),
        Ok(Some(v)) if v != marker.value.as_bytes()
    )
}

#[cfg(not(unix))]
fn carries_other_world(_marker: &EnvMarker, _pid: u32) -> bool {
    false
}

#[cfg(unix)]
mod marker_scan {
    use super::*;
    use crate::process::{output_bounded, KILL_HELPER_TIMEOUT};
    use std::io;

    /// A live process of the current user.
    pub(super) struct Process {
        pub pid: u32,
        pub comm: String,
    }

    /// Live (non-zombie) processes of the user this process runs as, other than this one.
    pub(super) fn same_user_processes() -> std::result::Result<Vec<Process>, String> {
        let mut ps = Command::new("ps");
        ps.args([
            "-A", "-o", "pid=", "-o", "uid=", "-o", "stat=", "-o", "comm=",
        ]);
        let text = match output_bounded(&mut ps, KILL_HELPER_TIMEOUT) {
            Ok(Some(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(Some(o)) => return Err(format!("ps exited with {}", o.status)),
            Ok(None) => return Err("ps timed out".to_string()),
            Err(e) => return Err(format!("cannot run ps: {e}")),
        };
        let rows = parse_user_rows(&text);
        let me = std::process::id();
        let uid = rows
            .iter()
            .find(|r| r.pid == me)
            .map(|r| r.uid)
            .ok_or_else(|| format!("ps did not list this process (pid {me})"))?;
        Ok(rows
            .into_iter()
            .filter(|r| r.uid == uid && r.pid != me && !r.zombie)
            .map(|r| Process {
                pid: r.pid,
                comm: r.comm,
            })
            .collect())
    }

    /// Reads the value of one environment variable of another process.
    pub(super) struct EnvReader {
        /// macOS: a buffer reused for every process, sized per read from what the kernel reports.
        #[cfg(target_os = "macos")]
        buf: Vec<u8>,
        /// macOS: the largest read the kernel accepts (`KERN_ARGMAX`, never above its `ARG_MAX`).
        #[cfg(target_os = "macos")]
        cap: usize,
    }

    #[cfg(target_os = "macos")]
    impl EnvReader {
        pub(super) fn new() -> io::Result<Self> {
            let mut reader = Self {
                buf: Vec::new(),
                cap: macos::argmax()?.min(macos::ARG_MAX),
            };
            // Canary: a process can always read its own environment. When this fails the read
            // path itself is broken, and every other process would look like "not ours to read".
            reader.value_of(std::process::id(), "PATH").map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("cannot read this process's own KERN_PROCARGS2: {e}"),
                )
            })?;
            Ok(reader)
        }

        /// `Ok(None)`: the environment was read and `name` is not in it.
        pub(super) fn value_of(&mut self, pid: u32, name: &str) -> io::Result<Option<Vec<u8>>> {
            let n = macos::procargs2(pid, &mut self.buf, self.cap)?;
            let entries = parse_procargs2(&self.buf[..n]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unreadable KERN_PROCARGS2 reply",
                )
            })?;
            Ok(entry_value(&entries, name).map(<[u8]>::to_vec))
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    impl EnvReader {
        pub(super) fn new() -> io::Result<Self> {
            Ok(Self {})
        }

        /// `Ok(None)`: the environment was read and `name` is not in it.
        pub(super) fn value_of(&mut self, pid: u32, name: &str) -> io::Result<Option<Vec<u8>>> {
            let bytes = std::fs::read(format!("/proc/{pid}/environ"))?;
            let entries = parse_environ_block(&bytes);
            Ok(entry_value(&entries, name).map(<[u8]>::to_vec))
        }
    }

    /// Other Unix systems have no reader here: the scan reports that it cannot run, never "clean".
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    impl EnvReader {
        pub(super) fn new() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "reading another process's environment is implemented for macOS and Linux only",
            ))
        }

        pub(super) fn value_of(&mut self, _pid: u32, _name: &str) -> io::Result<Option<Vec<u8>>> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "unsupported"))
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use std::ffi::{c_int, c_uint, c_void};
        use std::io;

        extern "C" {
            // libSystem, always linked by std on macOS.
            fn sysctl(
                name: *mut c_int,
                namelen: c_uint,
                oldp: *mut c_void,
                oldlenp: *mut usize,
                newp: *mut c_void,
                newlen: usize,
            ) -> c_int;
        }

        const CTL_KERN: c_int = 1;
        const KERN_ARGMAX: c_int = 8;
        const KERN_PROCARGS2: c_int = 49;
        /// The kernel's `ARG_MAX` (sys/syslimits.h): `KERN_PROCARGS2` answers `EINVAL` to a read
        /// whose buffer, less the argc word, is larger than this, whatever the process is.
        pub(super) const ARG_MAX: usize = 1024 * 1024;

        /// Largest argument plus environment block the kernel hands out.
        pub(super) fn argmax() -> io::Result<usize> {
            let mut mib = [CTL_KERN, KERN_ARGMAX];
            let mut value: c_int = 0;
            let mut len = std::mem::size_of::<c_int>();
            // SAFETY: `mib` has two elements as `namelen` says; `value` and `len` are valid for the
            // `c_int` the kernel writes; no new value is passed.
            let rc = unsafe {
                sysctl(
                    mib.as_mut_ptr(),
                    2,
                    (&mut value as *mut c_int).cast::<c_void>(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            usize::try_from(value)
                .ok()
                .filter(|n| *n >= 4096)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "implausible KERN_ARGMAX")
                })
        }

        /// `KERN_PROCARGS2` of `pid` into `buf` (grown as needed, at most `cap` bytes); returns the
        /// number of bytes written. The kernel refuses a process of another user and one that no
        /// longer exists (both `EINVAL`). The size is asked first (`oldp` NULL returns the size the
        /// reply needs) so the read never passes a buffer the kernel might reject as oversized.
        pub(super) fn procargs2(pid: u32, buf: &mut Vec<u8>, cap: usize) -> io::Result<usize> {
            let pid = c_int::try_from(pid)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
            let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
            let mut needed: usize = 0;
            // SAFETY: `mib` has three elements as `namelen` says; with a NULL `oldp` the kernel
            // only stores the required size in `needed`; no new value is passed.
            let rc = unsafe {
                sysctl(
                    mib.as_mut_ptr(),
                    3,
                    std::ptr::null_mut(),
                    &mut needed,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            buf.resize(super::super::procargs_buffer_len(needed, cap), 0);
            let mut len = buf.len();
            // SAFETY: `mib` has three elements as `namelen` says; `buf` is valid for `len` bytes and
            // the kernel writes at most `len`, updating it to the bytes written; no new value is passed.
            let rc = unsafe {
                sysctl(
                    mib.as_mut_ptr(),
                    3,
                    buf.as_mut_ptr().cast::<c_void>(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(len.min(buf.len()))
        }
    }
}

/// Processes whose executable lies inside one of `bundles` and that started no earlier than
/// `not_before` (one second of slack). One started earlier is the user's own instance, not ours; one
/// that carries another world's marker belongs to a concurrent scenario of the same app.
/// A process whose start time cannot be read is recorded (fail closed).
fn scan_bundles(
    bundles: &[PathBuf],
    not_before: SystemTime,
    marker: &EnvMarker,
) -> (Vec<BundleHit>, Vec<String>) {
    if bundles.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let table = match process_table() {
        Ok(table) => table,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("cannot scan for app bundle processes: {e}")],
            )
        }
    };
    let me = std::process::id();
    let floor = not_before.checked_sub(START_SLACK).unwrap_or(not_before);
    let mut hits = Vec::new();
    for entry in table {
        let Some(pid) = NonZeroU32::new(entry.pid).filter(|p| p.get() != me) else {
            continue;
        };
        let Ok(Some(identity)) = AdoptedIdentity::capture(pid) else {
            continue;
        };
        let Some(exe) = identity.exe() else { continue };
        let Some(bundle) = bundles.iter().find(|b| exe.starts_with(b)) else {
            continue;
        };
        if !identity.is_running().unwrap_or(true) {
            continue;
        }
        if carries_other_world(marker, pid.get()) {
            continue;
        }
        let start = identity.start_time();
        if start.is_some_and(|s| s < floor) {
            continue;
        }
        hits.push(BundleHit {
            survivor: Survivor {
                pid: pid.get(),
                name: file_name(exe),
                owner: SurvivorOwner::RegisteredRoot,
                detail: format!(
                    "app bundle {} started after the worker{}",
                    bundle.display(),
                    if start.is_none() {
                        " (start time unreadable, recorded to fail closed)"
                    } else {
                        ""
                    }
                ),
            },
            identity,
        });
    }
    (hits, Vec::new())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// -------------------------------------------------------------------------------- containment

#[cfg(windows)]
mod contain {
    use super::*;
    use rightkit_child_process::WindowsJob;
    use std::os::windows::process::CommandExt;

    /// `CREATE_NO_WINDOW`: never flash a console from a GUI host.
    const NO_WINDOW: u32 = 0x0800_0000;

    /// A fresh kill-on-close Job Object that allows no breakaway.
    pub(super) struct Containment {
        job: WindowsJob,
    }

    impl Containment {
        pub(super) fn new() -> Result<Self> {
            let job = WindowsJob::builder()
                .allow_breakaway(false)
                .build()
                .map_err(|e| Error(format!("cannot create the worker's Job Object: {e}")))?;
            Ok(Self { job })
        }

        pub(super) fn prepare(&self, cmd: &mut Command) {
            cmd.creation_flags(NO_WINDOW);
        }

        pub(super) fn attach(&mut self, child: &Child) -> std::result::Result<(), String> {
            self.job
                .assign_child(child)
                .map_err(|e| format!("assigning it to the Job Object failed: {e}"))
        }

        /// The job knows its members exactly; nothing to sample.
        pub(super) fn observe(&mut self, _worker: u32) {}

        /// Every job member other than the worker.
        pub(super) fn scan(&mut self, worker: u32) -> Scan {
            match self.job.process_ids() {
                Err(e) => Scan {
                    survivors: Vec::new(),
                    problems: vec![format!("cannot list the Job Object's processes: {e}")],
                },
                Ok(ids) => Scan {
                    survivors: ids
                        .into_iter()
                        .filter(|pid| *pid != worker && *pid != 0)
                        .filter_map(describe)
                        .collect(),
                    problems: Vec::new(),
                },
            }
        }

        /// Kill every process in the job.
        pub(super) fn kill(&mut self, _worker: u32) -> Vec<String> {
            match self.job.terminate() {
                Ok(()) => Vec::new(),
                Err(e) => vec![format!("terminating the worker's Job Object failed: {e}")],
            }
        }

        pub(super) fn confirm_gone(&mut self) -> Vec<String> {
            match self.job.wait_empty(KILL_CONFIRM) {
                Ok(true) => Vec::new(),
                Ok(false) => vec![format!(
                    "processes were still alive in the Job Object {} s after it was terminated",
                    KILL_CONFIRM.as_secs()
                )],
                Err(e) => vec![format!("cannot confirm the Job Object is empty: {e}")],
            }
        }
    }

    /// `None` when the process is already gone (it left between the listing and now).
    fn describe(pid: u32) -> Option<Survivor> {
        let nz = NonZeroU32::new(pid)?;
        let (name, detail) = match AdoptedIdentity::capture(nz) {
            Ok(Some(identity)) => match identity.exe() {
                Some(exe) => (
                    file_name(exe),
                    format!("member of the worker's Job Object: {}", exe.display()),
                ),
                None => ("unknown".into(), "member of the worker's Job Object".into()),
            },
            Ok(None) => return None,
            Err(e) => (
                "unknown".into(),
                format!("member of the worker's Job Object (identity unreadable: {e})"),
            ),
        };
        Some(Survivor {
            pid,
            name,
            owner: SurvivorOwner::RegisteredRoot,
            detail,
        })
    }
}

#[cfg(unix)]
mod contain {
    use super::*;
    use crate::process::{output_bounded, status_bounded, KILL_HELPER_TIMEOUT};
    use rightkit_child_process::wait_process_group_empty;
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::unix::process::CommandExt;

    /// How often the wait loop samples the process table.
    const SAMPLE_EVERY: Duration = Duration::from_millis(500);

    /// One row of `ps`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct PsRow {
        pub pid: u32,
        pub ppid: u32,
        pub pgid: u32,
        pub zombie: bool,
        pub comm: String,
    }

    /// Parse `ps -A -o pid= -o ppid= -o pgid= -o stat= -o comm=`; rows that do not parse are skipped.
    pub(super) fn parse_ps(text: &str) -> Vec<PsRow> {
        text.lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let pid = parts.next()?.parse().ok()?;
                let ppid = parts.next()?.parse().ok()?;
                let pgid = parts.next()?.parse().ok()?;
                let stat = parts.next()?;
                let comm = parts.collect::<Vec<_>>().join(" ");
                Some(PsRow {
                    pid,
                    ppid,
                    pgid,
                    zombie: stat.starts_with('Z'),
                    comm,
                })
            })
            .collect()
    }

    fn snapshot() -> std::result::Result<Vec<PsRow>, String> {
        let mut ps = Command::new("ps");
        ps.args([
            "-A", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "stat=", "-o", "comm=",
        ]);
        match output_bounded(&mut ps, KILL_HELPER_TIMEOUT) {
            Ok(Some(o)) if o.status.success() => Ok(parse_ps(&String::from_utf8_lossy(&o.stdout))),
            Ok(Some(o)) => Err(format!("ps exited with {}", o.status)),
            Ok(None) => Err("ps timed out".to_string()),
            Err(e) => Err(format!("cannot run ps: {e}")),
        }
    }

    /// Live (non-zombie) processes in group `pgid` or below `worker` by parent links, other than the
    /// worker and this process, each with how it was found.
    pub(super) fn members(rows: &[PsRow], pgid: u32, worker: u32) -> Vec<(PsRow, &'static str)> {
        let me = std::process::id();
        let mut below: BTreeSet<u32> = BTreeSet::from([worker]);
        loop {
            let before = below.len();
            for r in rows {
                if below.contains(&r.ppid) {
                    below.insert(r.pid);
                }
            }
            if below.len() == before {
                break;
            }
        }
        rows.iter()
            .filter(|r| r.pid != worker && r.pid != me && !r.zombie)
            .filter_map(|r| {
                if r.pgid == pgid {
                    Some((r.clone(), "member of the worker's process group"))
                } else if below.contains(&r.pid) {
                    Some((
                        r.clone(),
                        "descendant of the worker outside its process group",
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    /// A new process group, plus the identity of everything seen in it while the worker ran.
    pub(super) struct Containment {
        pgid: u32,
        seen: BTreeMap<u32, AdoptedIdentity>,
        last_sample: Option<Instant>,
    }

    impl Containment {
        pub(super) fn new() -> Result<Self> {
            Ok(Self {
                pgid: 0,
                seen: BTreeMap::new(),
                last_sample: None,
            })
        }

        /// The worker leads a new process group; its pid is the group id.
        pub(super) fn prepare(&self, cmd: &mut Command) {
            cmd.process_group(0);
        }

        pub(super) fn attach(&mut self, child: &Child) -> std::result::Result<(), String> {
            self.pgid = child.id();
            Ok(())
        }

        /// Sample the table at most every half second and remember each member's identity, so a
        /// member that later leaves the group (or whose parent exits) is still known.
        pub(super) fn observe(&mut self, worker: u32) {
            if self.last_sample.is_some_and(|t| t.elapsed() < SAMPLE_EVERY) {
                return;
            }
            self.last_sample = Some(Instant::now());
            let Ok(rows) = snapshot() else { return };
            for (row, _) in members(&rows, self.pgid, worker) {
                if self.seen.contains_key(&row.pid) {
                    continue;
                }
                if let Some(pid) = NonZeroU32::new(row.pid) {
                    if let Ok(Some(identity)) = AdoptedIdentity::capture(pid) {
                        self.seen.insert(row.pid, identity);
                    }
                }
            }
        }

        pub(super) fn scan(&mut self, worker: u32) -> Scan {
            let mut found: BTreeMap<u32, Survivor> = BTreeMap::new();
            let mut problems = Vec::new();
            match snapshot() {
                Ok(rows) => {
                    for (row, how) in members(&rows, self.pgid, worker) {
                        found.insert(
                            row.pid,
                            Survivor {
                                pid: row.pid,
                                name: file_name(Path::new(&row.comm)),
                                owner: SurvivorOwner::RegisteredRoot,
                                detail: how.to_string(),
                            },
                        );
                    }
                }
                Err(e) => problems.push(format!("cannot list the worker's processes: {e}")),
            }
            for (pid, identity) in &self.seen {
                if *pid == worker || found.contains_key(pid) {
                    continue;
                }
                // Fail closed: a state that cannot be read counts as running.
                if identity.is_running().unwrap_or(true) {
                    found.insert(
                        *pid,
                        Survivor {
                            pid: *pid,
                            name: identity
                                .exe()
                                .map(file_name)
                                .unwrap_or_else(|| "unknown".into()),
                            owner: SurvivorOwner::RegisteredRoot,
                            detail: "seen in the worker's tree while the scenario ran, now outside its process group (setsid or double fork), still running".to_string(),
                        },
                    );
                }
            }
            Scan {
                survivors: found.into_values().collect(),
                problems,
            }
        }

        /// Kill the whole group, then each recorded identity that is outside it.
        pub(super) fn kill(&mut self, worker: u32) -> Vec<String> {
            let mut problems = Vec::new();
            // `kill -- -0` would signal the caller's own group; a group id is only ever a spawned
            // worker's pid, so refuse an unattached containment outright.
            if self.pgid == 0 {
                return vec!["the worker was never attached to a process group".to_string()];
            }
            // Name everything now: killing the group reparents what is left of the tree.
            let mut targets: Vec<AdoptedIdentity> = self.seen.values().cloned().collect();
            match snapshot() {
                Ok(rows) => {
                    for (row, _) in members(&rows, self.pgid, worker) {
                        if targets.iter().any(|t| t.pid() == row.pid) {
                            continue;
                        }
                        if let Some(pid) = NonZeroU32::new(row.pid) {
                            if let Ok(Some(identity)) = AdoptedIdentity::capture(pid) {
                                targets.push(identity);
                            }
                        }
                    }
                }
                Err(e) => problems.push(format!("cannot list the worker's processes to kill: {e}")),
            }
            let mut kill = Command::new("kill");
            kill.args(["-KILL", "--", &format!("-{}", self.pgid)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // ESRCH (nobody left in the group) is a non-zero exit and is fine.
            if let Err(e) = status_bounded(&mut kill, KILL_HELPER_TIMEOUT) {
                problems.push(format!(
                    "cannot run kill for process group {}: {e}",
                    self.pgid
                ));
            }
            for identity in &targets {
                if let Err(e) = identity.signal(AdoptedSignal::Force) {
                    problems.push(format!("cannot signal pid {}: {e}", identity.pid()));
                }
            }
            problems
        }

        /// The group is empty and every recorded identity is gone. The worker must be reaped first:
        /// an unreaped leader keeps the group alive.
        pub(super) fn confirm_gone(&mut self) -> Vec<String> {
            let mut problems = Vec::new();
            if let Some(pgid) = NonZeroU32::new(self.pgid) {
                match wait_process_group_empty(pgid, KILL_CONFIRM) {
                    Ok(true) => {}
                    Ok(false) => problems.push(format!(
                        "process group {} was not empty {} s after the kill",
                        self.pgid,
                        KILL_CONFIRM.as_secs()
                    )),
                    Err(e) => problems.push(format!(
                        "cannot confirm process group {} is empty: {e}",
                        self.pgid
                    )),
                }
            }
            for (pid, identity) in &self.seen {
                match identity.wait_confirmed_gone(Duration::from_secs(2)) {
                    Ok(true) => {}
                    Ok(false) => {
                        problems.push(format!("pid {pid} was still running after the kill"))
                    }
                    Err(e) => problems.push(format!("cannot confirm pid {pid} is gone: {e}")),
                }
            }
            problems
        }
    }
}

// -------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::is_alive;
    use crate::worker::{SCENARIO_FILE, SCHEMA_VERSION};
    use crate::worker_main;
    use crate::worker_main::test_support;
    use crate::worker_main::test_support::*;

    /// `test_support::spec_in` with a marker value of its own. The shared helper spells the same
    /// marker for every world named `world-1`, and tests run in parallel: a survivor of one test
    /// would otherwise be found, recorded and killed by the marker scan of another.
    fn spec_in(root: &Path, name: &str, limits: crate::worker::Deadlines) -> ScenarioSpec {
        let mut spec = test_support::spec_in(root, name, limits);
        spec.world.env_marker.value = format!("w-{name}-{}", crate::util::new_id());
        spec
    }

    fn survivor(pid: u32) -> Survivor {
        Survivor {
            pid,
            name: "stray".into(),
            owner: SurvivorOwner::RegisteredRoot,
            detail: String::new(),
        }
    }

    fn exited_ok() -> Ended {
        Ended::Exited {
            success: true,
            how: "exit code: 0".into(),
        }
    }

    fn spec() -> ScenarioSpec {
        let root = run_root("settle");
        spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000))
    }

    // ---- pure verdict rules ----

    #[test]
    fn a_clean_pass_stays_a_pass() {
        let spec = spec();
        let out = settle_outcome(
            &spec,
            &exited_ok(),
            Ok(passed_outcome(&spec)),
            Scan::default(),
            "",
        );
        assert_eq!(out.status, CheckStatus::Passed);
        assert!(out.survivors.is_empty());
        assert!(out.message.is_empty());
    }

    #[test]
    fn a_survivor_turns_a_pass_into_errored_and_is_recorded() {
        let spec = spec();
        let scan = Scan {
            survivors: vec![survivor(4242)],
            problems: vec![],
        };
        let out = settle_outcome(&spec, &exited_ok(), Ok(passed_outcome(&spec)), scan, "");
        assert_eq!(out.status, CheckStatus::Errored);
        assert_eq!(out.survivors.len(), 1);
        assert_eq!(out.survivors[0].pid, 4242);
        assert!(
            out.message.contains("outlived the scenario"),
            "{}",
            out.message
        );
        out.validate_for(&spec).unwrap();
    }

    #[test]
    fn a_survivor_on_a_failure_keeps_the_failure() {
        let spec = spec();
        let mut failed = passed_outcome(&spec);
        failed.status = CheckStatus::Failed;
        failed.message = "check x failed".into();
        let scan = Scan {
            survivors: vec![survivor(7)],
            problems: vec![],
        };
        let out = settle_outcome(&spec, &exited_ok(), Ok(failed), scan, "");
        assert_eq!(out.status, CheckStatus::Failed);
        assert!(out.message.starts_with("check x failed"), "{}", out.message);
        assert!(out.message.contains("outlived"), "{}", out.message);
        assert_eq!(out.survivors.len(), 1);
    }

    #[test]
    fn a_survivor_the_worker_already_named_is_not_listed_twice() {
        let spec = spec();
        let mut failed = passed_outcome(&spec);
        failed.status = CheckStatus::Failed;
        failed.survivors.push(survivor(9));
        let scan = Scan {
            survivors: vec![survivor(9), survivor(10)],
            problems: vec![],
        };
        let out = settle_outcome(&spec, &exited_ok(), Ok(failed), scan, "");
        let pids: Vec<u32> = out.survivors.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![9, 10]);
    }

    #[test]
    fn a_deadline_is_errored_and_says_deadline() {
        let spec = spec();
        let ended = Ended::Deadline { limit_ms: 6_000 };
        let out = settle_outcome(
            &spec,
            &ended,
            err("the worker did not finish"),
            Scan::default(),
            "",
        );
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.starts_with("deadline"), "{}", out.message);
        assert!(out.message.contains("6000 ms"), "{}", out.message);
    }

    #[test]
    fn no_outcome_is_errored_with_the_worker_log() {
        let spec = spec();
        let ended = Ended::Exited {
            success: false,
            how: "exit code: 2".into(),
        };
        let out = settle_outcome(
            &spec,
            &ended,
            err("worker wrote no outcome.json"),
            Scan::default(),
            "refused: wrong world",
        );
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.contains("no usable outcome"), "{}", out.message);
        assert!(out.message.contains("exit code: 2"), "{}", out.message);
        assert!(
            out.message.contains("refused: wrong world"),
            "{}",
            out.message
        );
    }

    #[test]
    fn a_pass_from_a_worker_that_exited_badly_is_not_a_pass() {
        let spec = spec();
        let ended = Ended::Exited {
            success: false,
            how: "exit code: 4".into(),
        };
        let out = settle_outcome(
            &spec,
            &ended,
            Ok(passed_outcome(&spec)),
            Scan::default(),
            "",
        );
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.contains("exit code: 4"), "{}", out.message);
    }

    #[test]
    fn a_failed_scan_or_kill_stops_a_pass_but_not_a_failure() {
        let spec = spec();
        let scan = || Scan {
            survivors: vec![],
            problems: vec!["cannot list the Job Object's processes: denied".into()],
        };
        let out = settle_outcome(&spec, &exited_ok(), Ok(passed_outcome(&spec)), scan(), "");
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.contains("denied"), "{}", out.message);

        let mut failed = passed_outcome(&spec);
        failed.status = CheckStatus::Failed;
        let out = settle_outcome(&spec, &exited_ok(), Ok(failed), scan(), "");
        assert_eq!(out.status, CheckStatus::Failed);
    }

    #[cfg(unix)]
    #[test]
    fn ps_rows_parse_and_zombies_are_not_members() {
        use super::contain::{members, parse_ps};
        let rows = parse_ps(
            "  500     1   500 Ss   /usr/bin/worker --flag\n  501   500   500 S    sleep\n  502   500   502 Ss   setsid-child\n  503   500   500 Z+   (defunct)\n  600     1   600 S    unrelated\nnot a row\n",
        );
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].comm, "/usr/bin/worker --flag");
        assert!(rows[3].zombie);
        let found: Vec<(u32, &str)> = members(&rows, 500, 500)
            .into_iter()
            .map(|(r, how)| (r.pid, how))
            .collect();
        assert_eq!(
            found,
            vec![
                (501, "member of the worker's process group"),
                (502, "descendant of the worker outside its process group"),
            ]
        );
    }

    // ---- fixtures on real OS processes ----
    //
    // The helper is this test binary run again as the worker: it reads the start line, reads the
    // spec, and behaves as `RIGHTKIT_QA_SUPERVISE_HELPER` says. It is started by `supervise` itself,
    // so the real spawn, containment, start line, deadline and cleanup paths are what run.

    const HELPER_MODE: &str = "RIGHTKIT_QA_SUPERVISE_HELPER";
    const HELPER_WORLD: &str = "RIGHTKIT_QA_SUPERVISE_WORLD";

    #[cfg(unix)]
    const MARKER_SLEEP: &str = "RIGHTKIT_QA_SUPERVISE_MARKER_SLEEP";
    #[cfg(unix)]
    const MARKER_SLEEP_TEST: &str = "supervise::tests::marker_sleep_helper";

    /// Not a test by itself: it returns at once unless `RIGHTKIT_QA_SUPERVISE_MARKER_SLEEP` is set,
    /// and then sleeps far longer than any fixture. The world-marker fixtures run it from the test
    /// executable: a non-platform binary, whose environment macOS lets a same-user reader see.
    #[cfg(unix)]
    #[test]
    fn marker_sleep_helper() {
        if std::env::var_os(MARKER_SLEEP).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_secs(120));
    }

    /// The marker fixtures' child: this test executable in sleep mode (never `/bin/*`).
    #[cfg(unix)]
    fn marker_sleeper() -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([
            "--exact",
            MARKER_SLEEP_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARKER_SLEEP, "1");
        c
    }

    /// A command that runs far longer than any fixture.
    fn sleeper() -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("ping");
            c.args(["-n", "120", "127.0.0.1"]);
            c
        }
        #[cfg(unix)]
        {
            let mut c = Command::new("sleep");
            c.arg("120");
            c
        }
    }

    /// Start a detached sleeper (null stdio, never waited on) and write its pid to `file`.
    // The orphan is the point of the fixture: the supervisor must find and record it as a survivor.
    #[allow(clippy::zombie_processes)]
    fn leave_a_sleeper(file: &Path) {
        let child = sleeper()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the sleeper");
        std::fs::write(file, child.id().to_string()).unwrap();
        // Dropping a `Child` neither waits for it nor kills it: it is now an orphan of this worker.
    }

    /// Leave a sleeper that is neither in the worker's process group nor below it by parent links
    /// (`setsid` and double fork do the same to a real scenario): `sh` starts in a group of its own,
    /// starts the sleeper in the background and exits at once, so the sleeper is orphaned and its
    /// group is not the worker's. It lives for the whole run, but it was in the worker's tree for
    /// a few milliseconds only, far less than one 500 ms sample. Its pid goes to `file`.
    #[cfg(unix)]
    fn escape_the_tree(file: &Path) {
        use std::os::unix::process::CommandExt;
        // The escapee is this test executable in sleep mode, not `/bin/sleep`: on macOS the
        // environment of an Apple platform binary is hidden from the marker scan.
        let exe = std::env::current_exe().unwrap();
        let script = format!(
            "'{}' --exact {MARKER_SLEEP_TEST} --nocapture --test-threads=1 </dev/null >/dev/null 2>&1 & echo $! > '{}'",
            exe.display(),
            file.display()
        );
        let mut sh = Command::new("sh");
        sh.env(MARKER_SLEEP, "1")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        sh.spawn()
            .expect("start sh")
            .wait()
            .expect("wait for sh, which exits as soon as it started the sleeper");
    }

    /// Not a test by itself: it does nothing unless `RIGHTKIT_QA_SUPERVISE_HELPER` is set, which only
    /// `helper_launch` does.
    #[test]
    fn supervise_helper_child() {
        let Ok(mode) = std::env::var(HELPER_MODE) else {
            return;
        };
        let world = PathBuf::from(std::env::var(HELPER_WORLD).unwrap());
        let code = if mode == "wrong-world" {
            // Point the worker at a directory that is not the spec's world.
            let other = world.parent().unwrap().join("other-world");
            std::fs::create_dir_all(&other).unwrap();
            std::fs::copy(world.join(SCENARIO_FILE), other.join(SCENARIO_FILE)).unwrap();
            worker_main::main(&other, |s| Ok(passed_outcome(s)))
        } else {
            let world_for_files = world.clone();
            worker_main::main(&world, move |s| {
                match mode.as_str() {
                    "clean" => {}
                    #[cfg(unix)]
                    "escape" => escape_the_tree(&world_for_files.join("escaped.pid")),
                    "leak" => leave_a_sleeper(&world_for_files.join("leaked.pid")),
                    "hang" => {
                        leave_a_sleeper(&world_for_files.join("hung.pid"));
                        loop {
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                    other => panic!("unknown helper mode {other}"),
                }
                Ok(passed_outcome(s))
            })
        };
        std::process::exit(code);
    }

    fn helper_launch(mode: &str, spec: &ScenarioSpec) -> Launch {
        Launch {
            program: std::env::current_exe().unwrap(),
            args: [
                "--exact",
                "supervise::tests::supervise_helper_child",
                "--nocapture",
                "--test-threads=1",
            ]
            .iter()
            .map(OsString::from)
            .collect(),
            env: vec![
                (HELPER_MODE.into(), mode.into()),
                (HELPER_WORLD.into(), spec.world.dir.display().to_string()),
            ],
        }
    }

    fn pid_in(spec: &ScenarioSpec, file: &str) -> u32 {
        std::fs::read_to_string(spec.world.dir.join(file))
            .unwrap_or_else(|e| panic!("the helper did not write {file}: {e}"))
            .trim()
            .parse()
            .unwrap()
    }

    fn assert_gone_soon(pid: u32) {
        let until = Instant::now() + Duration::from_secs(10);
        while is_alive(pid) {
            assert!(
                Instant::now() < until,
                "pid {pid} is still alive after the run"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_clean_worker_passes_with_no_survivors() {
        let root = run_root("clean");
        let spec = spec_in(&root, "world-1", deadlines(30_000, 60_000, 10_000));
        let out = supervise(
            &spec,
            &helper_launch("clean", &spec),
            &SuperviseOptions::default(),
        )
        .unwrap();
        assert_eq!(out.status, CheckStatus::Passed, "{}", out.message);
        assert!(out.survivors.is_empty(), "{:?}", out.survivors);
        assert_eq!(out.schema_version, SCHEMA_VERSION);
        assert!(spec.world.dir.join(LOG_FILE).exists());
    }

    /// Fixture: a detached descendant (Windows: a Job Object member; Unix: a process group member)
    /// that outlives the worker is recorded as a survivor, fails the scenario although the worker
    /// reported a pass, and is killed after it was recorded.
    #[test]
    fn a_detached_descendant_is_recorded_as_a_survivor_and_fails_the_scenario() {
        let root = run_root("leak");
        let spec = spec_in(&root, "world-1", deadlines(30_000, 60_000, 10_000));
        let out = supervise(
            &spec,
            &helper_launch("leak", &spec),
            &SuperviseOptions::default(),
        )
        .unwrap();
        let leaked = pid_in(&spec, "leaked.pid");
        assert_eq!(out.status, CheckStatus::Errored, "{}", out.message);
        assert!(
            out.survivors.iter().any(|s| s.pid == leaked),
            "pid {leaked} missing from {:?}",
            out.survivors
        );
        assert!(
            out.message.contains("outlived the scenario"),
            "{}",
            out.message
        );
        out.validate_for(&spec).unwrap();
        assert_gone_soon(leaked);
    }

    /// Acceptance (D-c, Oracle item 2): a child that left the process group and the parent chain
    /// (the `setsid` plus double fork shape) within 500 ms is found by the world-marker scan, recorded
    /// as a `world-marker` survivor, fails the scenario although the worker reported a pass, and is
    /// killed after it was recorded. The containment cannot name it: it is not in the group, not
    /// below the worker, and no sample ever saw it in the tree.
    #[cfg(unix)]
    #[test]
    fn a_child_that_escapes_the_tree_is_a_world_marker_survivor() {
        let root = run_root("escape");
        let spec = spec_in(&root, "world-1", deadlines(30_000, 60_000, 10_000));
        let out = supervise(
            &spec,
            &helper_launch("escape", &spec),
            &SuperviseOptions::default(),
        )
        .unwrap();
        let escaped = pid_in(&spec, "escaped.pid");
        assert_eq!(out.status, CheckStatus::Errored, "{}", out.message);
        let found = out
            .survivors
            .iter()
            .find(|s| s.pid == escaped)
            .unwrap_or_else(|| panic!("pid {escaped} missing from {:?}", out.survivors));
        assert_eq!(found.owner, SurvivorOwner::WorldMarker, "{found:?}");
        assert!(
            out.message.contains("outlived the scenario"),
            "{}",
            out.message
        );
        out.validate_for(&spec).unwrap();
        assert_gone_soon(escaped);
    }

    /// The marker is readable from outside on this OS (on the Mac: `KERN_PROCARGS2`), and only the
    /// value of this world matches.
    #[cfg(unix)]
    #[test]
    fn the_world_marker_of_a_same_user_child_is_readable_and_exact() {
        let marker = EnvMarker {
            name: "RIGHTKIT_QA_WORLD".into(),
            value: format!("w-readable-{}", crate::util::new_id()),
        };
        let mut child = marker_sleeper()
            .env(&marker.name, &marker.value)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the sleeper");
        let pid = child.id();
        let (hits, _problems) = scan_world_marker(&marker, &[]);
        let other = EnvMarker {
            name: marker.name.clone(),
            value: format!("{}-other", marker.value),
        };
        let (other_hits, _) = scan_world_marker(&other, &[]);
        let (skipped, _) = scan_world_marker(&marker, &[pid]);
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            hits.iter()
                .any(|h| h.survivor.pid == pid && h.survivor.owner == SurvivorOwner::WorldMarker),
            "pid {pid} not found among {:?}",
            hits.iter().map(|h| h.survivor.pid).collect::<Vec<_>>()
        );
        assert!(other_hits.iter().all(|h| h.survivor.pid != pid));
        assert!(skipped.iter().all(|h| h.survivor.pid != pid));
    }

    // ---- world-marker parsing (pure) ----

    #[test]
    fn environ_blocks_and_entry_values_parse() {
        let block = b"A=1\0RIGHTKIT_QA_WORLD=w-1\0\0EMPTY=\0NOEQ\0";
        let entries = parse_environ_block(block);
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entry_value(&entries, "RIGHTKIT_QA_WORLD"),
            Some(&b"w-1"[..])
        );
        assert_eq!(entry_value(&entries, "EMPTY"), Some(&b""[..]));
        // A name that is only a prefix of another is not that variable.
        assert_eq!(entry_value(&entries, "RIGHTKIT_QA"), None);
        assert_eq!(entry_value(&entries, "NOEQ"), None);
    }

    #[test]
    fn a_kern_procargs2_reply_yields_only_the_environment() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&2i32.to_ne_bytes());
        buf.extend_from_slice(b"/Applications/X.app/Contents/MacOS/x\0\0\0\0");
        buf.extend_from_slice(b"x\0RIGHTKIT_QA_WORLD=decoy\0");
        buf.extend_from_slice(b"HOME=/h\0RIGHTKIT_QA_WORLD=w-9\0\0\0");
        let env = parse_procargs2(&buf).expect("parses");
        assert_eq!(env, vec![&b"HOME=/h"[..], &b"RIGHTKIT_QA_WORLD=w-9"[..]]);
        // The argument that spells the marker is not the environment.
        assert_eq!(entry_value(&env, "RIGHTKIT_QA_WORLD"), Some(&b"w-9"[..]));
        // Too short, negative argc, and arguments that run off the end are refused.
        assert!(parse_procargs2(b"\x01").is_none());
        assert!(parse_procargs2(&(-1i32).to_ne_bytes()).is_none());
        let mut cut = Vec::new();
        cut.extend_from_slice(&3i32.to_ne_bytes());
        cut.extend_from_slice(b"/bin/x\0\0a\0b\0");
        assert!(parse_procargs2(&cut).is_none());
    }

    #[test]
    fn the_procargs2_buffer_is_sized_from_the_probe_and_never_above_the_cap() {
        let cap = 1024 * 1024;
        assert_eq!(procargs_buffer_len(1000, cap), 1000 + 250 + 64);
        assert_eq!(procargs_buffer_len(0, cap), 64);
        assert_eq!(procargs_buffer_len(cap, cap), cap);
        assert_eq!(procargs_buffer_len(usize::MAX, cap), cap);
        assert_eq!(procargs_buffer_len(10, 4), 8);
    }

    #[test]
    fn ps_rows_with_a_user_parse() {
        let rows = parse_user_rows(
            "  500   501 Ss   /usr/bin/worker --flag\n  502     0 Z+   (defunct)\nnot a row\n  7 8\n",
        );
        assert_eq!(
            rows,
            vec![
                UserRow {
                    pid: 500,
                    uid: 501,
                    zombie: false,
                    comm: "/usr/bin/worker --flag".into()
                },
                UserRow {
                    pid: 502,
                    uid: 0,
                    zombie: true,
                    comm: "(defunct)".into()
                },
            ]
        );
    }

    #[test]
    fn unreadable_or_vanished_processes_are_not_reported_but_other_failures_are() {
        use std::io::{Error as IoError, ErrorKind};
        assert!(is_benign_read_failure(&IoError::from(ErrorKind::NotFound)));
        assert!(is_benign_read_failure(&IoError::from(
            ErrorKind::PermissionDenied
        )));
        assert!(is_benign_read_failure(&IoError::from_raw_os_error(3)));
        assert!(is_benign_read_failure(&IoError::from_raw_os_error(22)));
        assert!(!is_benign_read_failure(&IoError::from(
            ErrorKind::InvalidData
        )));
        assert!(!is_benign_read_failure(&IoError::from_raw_os_error(12)));
    }

    #[test]
    fn staged_bundles_are_the_app_directories_one_level_down() {
        let root = run_root("bundles");
        let staged = root.join("rightkit-qa-apps");
        std::fs::create_dir_all(staged.join("alpha").join("alpha.app")).unwrap();
        std::fs::create_dir_all(staged.join("alpha").join("alpha.app.previous")).unwrap();
        std::fs::create_dir_all(staged.join("beta").join("beta.app")).unwrap();
        std::fs::write(staged.join("beta").join("note.app"), "a file").unwrap();
        let mut found = staged_bundles(&staged);
        found.sort();
        assert_eq!(
            found,
            vec![
                staged.join("alpha").join("alpha.app"),
                staged.join("beta").join("beta.app"),
            ]
        );
        assert!(staged_bundles(&root.join("missing")).is_empty());
    }

    #[test]
    fn declared_app_bundles_come_from_the_scenario() {
        let mut spec = spec();
        spec.scenario =
            serde_json::json!({"appBundles": ["relative.app", 7, "/x/ok.app", "/x/not-a-bundle"]});
        let bundles = app_bundles_of(&spec);
        assert!(bundles.contains(&PathBuf::from("/x/ok.app")) || cfg!(windows));
        assert!(bundles
            .iter()
            .all(|b| b.extension().is_some_and(|e| e == "app")));
        assert!(!bundles.contains(&PathBuf::from("relative.app")));
    }

    /// Fixture: a worker that never finishes is killed at scenario + teardown, with its whole tree,
    /// and the scenario is `errored` with reason `deadline`.
    #[test]
    fn a_deadline_overrun_is_errored_and_the_tree_is_killed() {
        let root = run_root("deadline");
        let spec = spec_in(&root, "world-1", deadlines(2_000, 8_000, 2_000));
        let began = Instant::now();
        let out = supervise(
            &spec,
            &helper_launch("hang", &spec),
            &SuperviseOptions::default(),
        )
        .unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "the deadline must bound the run"
        );
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.starts_with("deadline"), "{}", out.message);
        let hung = pid_in(&spec, "hung.pid");
        assert!(
            out.survivors.iter().any(|s| s.pid == hung),
            "the descendant alive at the deadline must be recorded: {:?}",
            out.survivors
        );
        assert_gone_soon(hung);
        out.validate_for(&spec).unwrap();
    }

    /// Fixture: a worker started with a world dir that does not match the spec refuses and writes
    /// nothing, so the runner sees no usable outcome and reports `errored`.
    #[test]
    fn a_worker_run_on_the_wrong_world_dir_is_refused_and_errored() {
        let root = run_root("wrong-world");
        let spec = spec_in(&root, "world-1", deadlines(30_000, 60_000, 10_000));
        let out = supervise(
            &spec,
            &helper_launch("wrong-world", &spec),
            &SuperviseOptions::default(),
        )
        .unwrap();
        assert_eq!(out.status, CheckStatus::Errored);
        assert!(out.message.contains("no usable outcome"), "{}", out.message);
        assert!(
            out.message.contains("refused"),
            "the worker's own refusal is in its log: {}",
            out.message
        );
        assert!(!spec.world.dir.join(OUTCOME_FILE).exists());
    }
}
