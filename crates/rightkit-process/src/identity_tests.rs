//! Unit tests for the process-group identity rule, with every signal and every reap recorded
//! in order by `unix_child::trace`.

use crate::unix_child::trace::{self, Event};
use crate::unix_child::{gate, LeaderState, SignalTarget};
use crate::{OwnedChild, OwnedCommand, ProcessHandle, WaitOutcome};
use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

fn sh(script: &str) -> OwnedCommand {
    let mut command = OwnedCommand::new("/bin/sh");
    command
        .command_mut()
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn exits_at_once() -> OwnedChild {
    sh("exit 0").spawn().expect("spawn")
}

fn signals(events: &[Event]) -> Vec<(i32, i32)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Signal { target, signal, .. } => Some((*target, *signal)),
            Event::Reaped { .. } | Event::Wait { .. } | Event::Proof { .. } => None,
        })
        .collect()
}

/// A process alone in a group of its own, standing in for an unrelated group that was given
/// a recycled id.
struct Victim(std::process::Child);

impl Victim {
    fn new() -> Self {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("600")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setpgid is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        Self(command.spawn().expect("spawn victim"))
    }
    fn id(&self) -> u32 {
        self.0.id()
    }
    fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Victim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Copy, Debug)]
enum Reap {
    OwnerWait,
    OwnerTryWait,
    OwnerWaitTimeout,
    HandleWait,
    HandleTryWait,
    HandleWaitTimeout,
    AccessorWait,
    AccessorTryWait,
    CloneOnAnotherThread,
}

const EVERY_REAP: [Reap; 9] = [
    Reap::OwnerWait,
    Reap::OwnerTryWait,
    Reap::OwnerWaitTimeout,
    Reap::HandleWait,
    Reap::HandleTryWait,
    Reap::HandleWaitTimeout,
    Reap::AccessorWait,
    Reap::AccessorTryWait,
    Reap::CloneOnAnotherThread,
];

fn poll_until_some<T>(mut step: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = step() {
            return value;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never reaped");
        thread::sleep(Duration::from_millis(2));
    }
}

fn reap(child: &mut OwnedChild, how: Reap) {
    let handle = child.process_handle().expect("handle");
    match how {
        Reap::OwnerWait => {
            child.wait().expect("wait");
        }
        Reap::OwnerTryWait => {
            poll_until_some(|| child.try_wait().expect("try_wait"));
        }
        Reap::OwnerWaitTimeout => {
            poll_until_some(|| {
                child
                    .wait_timeout(Duration::from_millis(50))
                    .expect("wait_timeout")
            });
        }
        Reap::HandleWait => {
            handle.wait().expect("handle wait");
        }
        Reap::HandleTryWait => {
            poll_until_some(|| handle.try_wait().expect("handle try_wait"));
        }
        Reap::HandleWaitTimeout => {
            poll_until_some(|| {
                handle
                    .wait_timeout(Duration::from_millis(50))
                    .expect("handle wait_timeout")
            });
        }
        Reap::AccessorWait => {
            child.child().wait().expect("accessor wait");
        }
        Reap::AccessorTryWait => {
            poll_until_some(|| child.child().try_wait().expect("accessor try_wait"));
        }
        Reap::CloneOnAnotherThread => {
            let clone = handle.try_clone().expect("clone");
            thread::spawn(move || clone.wait().expect("clone wait"))
                .join()
                .expect("join");
        }
    }
}

/// Every termination entry point, called after the leader was reaped.
fn terminate_every_way(child: &mut OwnedChild) {
    let _ = child.terminate_tree();
    let _ = child.wait_or_kill(Duration::from_millis(100));
    let _ = child.terminate_tree_with_options(
        crate::TerminationOptions::default().with_grace(Duration::from_millis(50)),
    );
    let _ = child.terminate_tree_bounded(Duration::from_millis(100));
}

#[test]
fn after_a_reap_by_any_path_no_signal_of_any_kind_is_sent() {
    for how in EVERY_REAP {
        let mut child = exits_at_once();
        let pid = child.id();
        let mark = trace::mark();
        reap(&mut child, how);
        terminate_every_way(&mut child);
        drop(child);
        let events = trace::since(mark, pid);
        assert_eq!(
            events,
            vec![Event::Reaped { leader: pid }],
            "{how:?}: expected one reap and no signal (signal 0 included)"
        );
    }
}

#[test]
fn a_recycled_id_is_never_signalled_after_a_reap_by_any_path() {
    let mut victim = Victim::new();
    for how in EVERY_REAP {
        let mut child = exits_at_once();
        let pid = child.id();
        reap(&mut child, how);
        // From here the kernel could have handed the id to the victim's group.
        child.child().simulate_recycled_id(victim.id());
        terminate_every_way(&mut child);
        drop(child);
        assert!(victim.alive(), "{how:?}: the unrelated group was signalled");
        let _ = pid;
    }
}

/// The control for the test above: the hook really does aim signals at the victim while the
/// leader is unreaped, so the test above is able to fail.
#[test]
fn the_recycled_id_hook_redirects_signals_while_the_leader_is_unreaped() {
    let mut victim = Victim::new();
    let mut child = sh("exec sleep 600").spawn().expect("spawn");
    child.child().simulate_recycled_id(victim.id());
    let started = Instant::now();
    child.child().kill().expect("kill");
    // The kill went to the leader's id as redirected: the victim dies, the leader does not.
    while victim.alive() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "victim not signalled"
        );
        thread::sleep(Duration::from_millis(5));
    }
    child.child().simulate_recycled_id(child.id());
    child.terminate_tree().expect("cleanup");
}

#[test]
fn terminating_before_the_reap_signals_exactly_the_group_then_reaps() {
    let mut child = sh("exec sleep 600").spawn().expect("spawn");
    let pid = child.id();
    let mark = trace::mark();
    child.terminate_tree().expect("terminate");
    assert_eq!(
        trace::since(mark, pid),
        vec![
            Event::Signal {
                leader: pid,
                target: -(pid as i32),
                signal: libc::SIGKILL
            },
            Event::Reaped { leader: pid },
        ]
    );
}

#[test]
fn wait_or_kill_signals_term_then_kill_before_the_reap_and_nothing_after() {
    let mut child = sh("exec sleep 600").spawn().expect("spawn");
    let pid = child.id();
    let mark = trace::mark();
    child
        .wait_or_kill(Duration::from_secs(5))
        .expect("wait_or_kill");
    let events = trace::since(mark, pid);
    assert_eq!(
        events.last(),
        Some(&Event::Reaped { leader: pid }),
        "{events:?}"
    );
    let group = -(pid as i32);
    let sent = signals(&events);
    assert!(
        sent.first() == Some(&(group, libc::SIGTERM))
            && sent.iter().all(|(target, _)| *target == group),
        "{sent:?}"
    );
}

#[test]
fn a_detached_child_is_signalled_by_pid_only_while_unreaped() {
    let mut child = sh("exec sleep 600")
        .spawn_uncontained_detached()
        .expect("spawn");
    let pid = child.id();
    let mark = trace::mark();
    child.terminate_tree().expect("terminate");
    assert_eq!(
        trace::since(mark, pid),
        vec![
            Event::Signal {
                leader: pid,
                target: pid as i32,
                signal: libc::SIGKILL
            },
            Event::Reaped { leader: pid },
        ]
    );

    let mut reaped = sh("exit 0").spawn_uncontained_detached().expect("spawn");
    let pid = reaped.id();
    let mark = trace::mark();
    reaped.wait().expect("wait");
    let _ = reaped.terminate_tree();
    assert_eq!(trace::since(mark, pid), vec![Event::Reaped { leader: pid }]);
}

#[test]
fn a_leader_reaped_behind_our_back_is_never_signalled() {
    let mut child = exits_at_once();
    let pid = child.id();
    thread::sleep(Duration::from_millis(200));
    let mark = trace::mark();
    let mut status = 0;
    // SAFETY: reaps this test's own child, standing in for an unrelated reaper in the process.
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, &mut status, 0) },
        pid as i32
    );
    child.child().kill().expect("kill is a no-op");
    let _ = child.terminate_tree();
    drop(child);
    assert_eq!(signals(&trace::since(mark, pid)), vec![]);
}

/// `while_unreaped` runs the caller's pid-based action while the leader is unreaped (running or
/// an exited zombie, which still pins the pid) and never once it is reaped.
#[test]
fn while_unreaped_runs_the_action_only_until_the_leader_is_reaped() {
    let mut running = sh("exec sleep 600").spawn().expect("spawn");
    let pid = running.id();
    assert_eq!(running.while_unreaped(|id| id).expect("running"), Some(pid));
    running.terminate_tree().expect("terminate");
    let mut ran = false;
    let after = running
        .while_unreaped(|_| ran = true)
        .expect("after the reap");
    assert_eq!(after, None);
    assert!(!ran, "the action ran after the leader was reaped");

    // Exited but not reaped: the zombie still pins the number, so the action still runs.
    let exited = exits_at_once();
    exited
        .process_handle()
        .expect("handle")
        .wait_exited()
        .expect("observe the exit");
    assert_eq!(
        exited.while_unreaped(|id| id).expect("zombie"),
        Some(exited.id())
    );
}

/// One thread reaps through a clone while the owner terminates, released together by a
/// barrier. Whatever the interleaving: at most one KILL, and never a signal after the reap.
fn interleave(script: &str, rounds: usize) {
    for round in 0..rounds {
        let mut child = sh(script).spawn().expect("spawn");
        let pid = child.id();
        let handle = child.process_handle().expect("handle");
        let barrier = Arc::new(Barrier::new(2));
        let mark = trace::mark();
        let reaper = {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                handle.wait().expect("clone wait")
            })
        };
        barrier.wait();
        let _ = child.terminate_tree();
        reaper.join().expect("join");
        let events = trace::since(mark, pid);
        let reaps = events
            .iter()
            .filter(|event| matches!(event, Event::Reaped { .. }))
            .count();
        assert_eq!(reaps, 1, "round {round}: {events:?}");
        let reaped_at = events
            .iter()
            .position(|event| matches!(event, Event::Reaped { .. }))
            .unwrap();
        assert!(
            events[reaped_at..]
                .iter()
                .all(|event| matches!(event, Event::Reaped { .. })),
            "round {round}: a signal followed the reap: {events:?}"
        );
        let sent = signals(&events);
        assert!(
            sent.is_empty() || sent == vec![(-(pid as i32), libc::SIGKILL)],
            "round {round}: {sent:?}"
        );
    }
}

#[test]
fn clones_reaping_and_owners_terminating_never_interleave_a_signal_after_the_reap() {
    interleave("exit 0", 60);
    interleave("sleep 0.01", 40);
    interleave("exec sleep 600", 40);
}

fn temp_file(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rightkit-identity-unit-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn read_pid(file: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "pid file was not written");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Running, and not a zombie that only waits for a reaper.
fn is_running(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    !state.is_empty() && !state.starts_with('Z')
}

/// The leader leaves on TERM. A straggler in the same group ignores TERM, and publishes its pid
/// only after it set the trap, so a TERM sent once the pid is known cannot race the trap.
fn leader_with_term_resistant_straggler(file: &Path) -> OwnedChild {
    let mut command = OwnedCommand::new("/bin/sh");
    command
        .command_mut()
        .args([
            "-c",
            "sh -c 'trap \"\" TERM; echo $$ > \"$0\"; exec sleep 600' \"$0\" & \
             trap 'exit 0' TERM; while :; do sleep 0.05; done",
        ])
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().expect("spawn")
}

/// An exit monitor that watches through a handle clone and does not reap. The monitor has
/// observed the exit before the owner escalates, which is the order that used to lose: the
/// reaping watcher won, group signalling was retired, and the TERM-resistant straggler lived
/// on. Now the leader is still a zombie pinning the group, so the KILL goes out first.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[test]
fn an_exit_monitor_that_does_not_reap_leaves_the_owner_able_to_kill_a_term_resistant_straggler() {
    let file = temp_file("straggler");
    let mut child = leader_with_term_resistant_straggler(&file);
    let pid = child.id();
    let straggler = read_pid(&file);
    assert!(is_running(straggler));
    let mark = trace::mark();
    child
        .child()
        .signal(SignalTarget::Group, libc::SIGTERM)
        .expect("TERM to the group");
    let monitor = child.process_handle().expect("handle");
    monitor.wait_exited().expect("observe the exit");
    // The monitor saw the exit and stopped: nothing was reaped, the zombie still pins the id.
    assert_eq!(
        child.child().leader_state().expect("state"),
        LeaderState::Exited
    );
    assert!(
        !trace::since(mark, pid)
            .iter()
            .any(|event| matches!(event, Event::Reaped { .. })),
        "the monitor reaped the leader"
    );
    assert!(is_running(straggler), "the straggler ignores TERM");

    let outcome = child
        .wait_or_kill(Duration::from_secs(10))
        .expect("wait_or_kill");
    assert!(matches!(outcome, WaitOutcome::Exited(_)), "{outcome:?}");
    let events = trace::since(mark, pid);
    let group = -(pid as i32);
    let sent = signals(&events);
    assert_eq!(sent.first(), Some(&(group, libc::SIGTERM)), "{events:?}");
    assert_eq!(sent.last(), Some(&(group, libc::SIGKILL)), "{events:?}");
    assert_eq!(
        events.last(),
        Some(&Event::Reaped { leader: pid }),
        "the KILL must come before the reap: {events:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while is_running(straggler) {
        assert!(
            Instant::now() < deadline,
            "the straggler outlived the group KILL"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&file);
}

/// The control for the test above: a monitor that reaps ends the owner's ability to kill the
/// straggler. This is what `ProcessHandle::wait` is documented to do.
#[test]
fn a_monitor_that_reaps_retires_the_owner_and_the_straggler_survives() {
    let file = temp_file("reaping-monitor");
    let mut child = leader_with_term_resistant_straggler(&file);
    let pid = child.id();
    let straggler = read_pid(&file);
    child
        .child()
        .signal(SignalTarget::Group, libc::SIGTERM)
        .expect("TERM to the group");
    child
        .process_handle()
        .expect("handle")
        .wait()
        .expect("reap");
    let mark = trace::mark();
    let _ = child.wait_or_kill(Duration::from_millis(100));
    assert_eq!(signals(&trace::since(mark, pid)), vec![]);
    assert!(is_running(straggler));
    // SAFETY: cleanup of this test's own sleeper.
    unsafe { libc::kill(straggler, libc::SIGKILL) };
    let _ = std::fs::remove_file(&file);
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[test]
fn wait_exited_observes_without_reaping_and_its_timeout_form_reports_both_outcomes() {
    // Still running: the timed form says so and nothing is reaped.
    let mut running = sh("exec sleep 600").spawn().expect("spawn");
    let handle = running.process_handle().expect("handle");
    assert!(!handle
        .wait_exited_timeout(Duration::from_millis(30))
        .expect("timed observation"));
    assert!(!handle.wait_exited_timeout(Duration::ZERO).expect("zero"));
    running.terminate_tree().expect("cleanup");

    // Exited: both forms return, and neither reaps.
    let mut child = exits_at_once();
    let pid = child.id();
    let mark = trace::mark();
    let handle = child.process_handle().expect("handle");
    handle.wait_exited().expect("wait_exited");
    assert!(handle
        .wait_exited_timeout(Duration::from_secs(10))
        .expect("timed observation"));
    assert_eq!(
        child.child().leader_state().expect("state"),
        LeaderState::Exited
    );
    assert!(
        !trace::since(mark, pid)
            .iter()
            .any(|event| matches!(event, Event::Reaped { .. })),
        "an observation reaped the leader"
    );
    // The owner reaps afterwards, once, and later observations still return.
    child.wait().expect("owner reaps");
    handle.wait_exited().expect("after the reap");
    assert!(handle
        .wait_exited_timeout(Duration::ZERO)
        .expect("after the reap"));
}

/// Parks the first waiter on `pid` at its pause point: just after it released the child lock,
/// the moment the old blocking wait had read the state and was about to name the pid.
/// Returns a signal that the waiter is parked and the key that lets it go.
fn park_first_waiter(pid: u32) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let arrived_tx = Mutex::new(arrived_tx);
    let resume_rx = Mutex::new(resume_rx);
    let first = AtomicBool::new(true);
    gate::install(pid, move || {
        if first.swap(false, Ordering::SeqCst) {
            let _ = arrived_tx.lock().unwrap().send(());
            let _ = resume_rx.lock().unwrap().recv();
        }
    });
    (arrived_rx, resume_tx)
}

/// A waiter that looked while the child ran is held; another reader then reaps the leader (the
/// kernel is now free to give the pid to anyone); the waiter is released. It must finish from
/// the shared state, with no wait-class system call naming the pid after the reap: a blocking
/// `waitid(pid)` made here would wait on whatever child now holds that number.
fn a_parked_waiter_never_names_the_pid_after_the_reap(
    wait: impl FnOnce(ProcessHandle) + Send + 'static,
) {
    let mut child = sh("exec sleep 600").spawn().expect("spawn");
    let pid = child.id();
    let (arrived, resume) = park_first_waiter(pid);
    let handle = child.process_handle().expect("handle");
    let waiter = thread::spawn(move || wait(handle));
    arrived
        .recv_timeout(Duration::from_secs(10))
        .expect("the waiter reached its pause point");
    let mark = trace::mark();
    child.terminate_tree().expect("kill and reap");
    resume.send(()).expect("release the waiter");
    waiter.join().expect("the waiter finishes");
    gate::remove(pid);
    let events = trace::since_with_waits(mark, pid);
    let reaped_at = events
        .iter()
        .position(|event| matches!(event, Event::Reaped { .. }))
        .unwrap_or_else(|| panic!("no reap recorded: {events:?}"));
    assert!(
        events[reaped_at + 1..].is_empty(),
        "a system call named the pid after the leader was reaped: {events:?}"
    );
}

#[test]
fn a_parked_wait_never_names_the_pid_after_another_reader_reaped_it() {
    a_parked_waiter_never_names_the_pid_after_the_reap(|handle| {
        handle.wait().expect("wait returns the cached status");
    });
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[test]
fn a_parked_wait_exited_never_names_the_pid_after_another_reader_reaped_it() {
    a_parked_waiter_never_names_the_pid_after_the_reap(|handle| {
        handle.wait_exited().expect("wait_exited sees the reap");
    });
}

// ---- A pid that changed hands (fourth read, finding 7) ----------------------------------------
//
// Other code in the process collects our leader and a replacement child takes its number. The
// replacement cannot be given the same number on demand, so the tests change what the child is
// expected to look like (`simulate_replaced`): the kernel then describes the pid differently
// from the leader that was spawned, which is exactly what a replacement looks like. Every
// action that names the pid must stop at the proof: no signal, no wait-class call, no reap.
// The replacement here is a live child, so a stray signal or reap is visible as its death.

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod replaced_pid {
    use super::*;
    use std::io;

    /// A running child standing in for the replacement that now holds the leader's number.
    fn replaced_leader() -> (OwnedChild, u32) {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let pid = child.id();
        // Let `exec` finish so the process is the sleeper, not the shell.
        thread::sleep(Duration::from_millis(100));
        child.child().simulate_replaced();
        (child, pid)
    }

    /// The replacement must still be running and unreaped; then clean it up ourselves.
    fn assert_untouched_then_release(child: OwnedChild, pid: u32, mark: usize) {
        let events = trace::since_with_waits(mark, pid);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, Event::Proof { .. })),
            "something other than a proof named the pid: {events:?}"
        );
        let mut status = 0;
        // SAFETY: WNOHANG probe of this test's own child.
        let probe = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
        assert_eq!(probe, 0, "the replacement was signalled or reaped");
        // SAFETY: cleanup of this test's own child.
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
            libc::waitpid(pid as i32, &mut status, 0);
        }
        drop(child);
    }

    #[test]
    fn terminating_does_not_signal_a_pid_that_changed_hands() {
        let actions: [fn(&mut OwnedChild) -> io::Result<()>; 4] = [
            |c: &mut OwnedChild| c.terminate_tree().map(|_| ()),
            |c: &mut OwnedChild| {
                c.terminate_tree_with_options(crate::TerminationOptions {
                    grace: Duration::from_millis(20),
                    ..Default::default()
                })
                .map(|_| ())
            },
            |c: &mut OwnedChild| {
                c.terminate_tree_bounded(Duration::from_millis(50))
                    .map(|_| ())
            },
            |c: &mut OwnedChild| c.wait_or_kill(Duration::from_millis(20)).map(|_| ()),
        ];
        for terminate in actions {
            let (mut child, pid) = replaced_leader();
            let mark = trace::mark();
            let result = terminate(&mut child);
            // The leader is Lost: the reap answers ECHILD (or the bounded form times out).
            if let Err(error) = result {
                assert_eq!(error.raw_os_error(), Some(libc::ECHILD), "{error}");
            }
            assert_eq!(signals(&trace::since(mark, pid)), vec![]);
            assert_untouched_then_release(child, pid, mark);
        }
    }

    #[test]
    fn waiting_does_not_name_a_pid_that_changed_hands() {
        let (mut child, pid) = replaced_leader();
        let mark = trace::mark();
        let error = child.try_wait().expect_err("not our child any more");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        let (mut child2, pid2) = replaced_leader();
        let mark2 = trace::mark();
        let error = child2.wait().expect_err("not our child any more");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        let error = child2
            .child()
            .wait()
            .expect_err("the public guard obeys the proof too");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        assert_untouched_then_release(child, pid, mark);
        assert_untouched_then_release(child2, pid2, mark2);
    }

    #[test]
    fn probes_and_kills_do_not_name_a_pid_that_changed_hands() {
        let (mut child, pid) = replaced_leader();
        let mark = trace::mark();
        let handle = child.process_handle().expect("handle");
        child
            .child()
            .kill()
            .expect("a lost leader's kill is a no-op");
        assert_eq!(
            child.child().leader_state().expect("state"),
            LeaderState::Reaped
        );
        assert!(!child.child().group_has_other_members().expect("members"));
        assert_eq!(
            child
                .child()
                .signal(SignalTarget::Group, libc::SIGKILL)
                .expect("signal"),
            crate::unix_child::SignalOutcome::Retired
        );
        handle.wait_exited().expect("a lost leader reads as exited");
        assert_untouched_then_release(child, pid, mark);
    }
}

/// The description of a zombie is readable, so the proof holds at the moment of the reap. This
/// is the assumption `unix_child` and the macOS helper reap rest on.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn an_exited_unreaped_leader_is_still_described_as_the_same_process() {
    let mut child = exits_at_once();
    let pid = child.id();
    let before = crate::identity::describe(pid).expect("describe");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.child().leader_state().expect("state") != LeaderState::Exited {
        assert!(Instant::now() < deadline, "the leader never exited");
        thread::sleep(Duration::from_millis(5));
    }
    let after = crate::identity::describe(pid).expect("describe the zombie");
    assert!(after.is_some(), "a zombie must be describable");
    if let Some(before) = before {
        assert_eq!(Some(before), after);
    }
    child.wait().expect("reap");
}

// ---- Waits are loops of non-blocking looks: bounded, cancellable, never holding the lock ---------

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod bounded_waits {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// An unbounded wait on a child that runs for 300 ms returns its status after the child
    /// exits, not before and not never.
    #[test]
    fn an_unbounded_wait_returns_the_status_of_a_child_that_runs_300_ms() {
        let mut child = sh("exec sleep 0.3").spawn().expect("spawn");
        let handle = child.process_handle().expect("handle");
        let started = Instant::now();
        let status = handle.wait().expect("wait");
        assert!(status.success());
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(started.elapsed() < Duration::from_secs(20));
        let _ = child.wait();
    }

    /// Blockers 9 and 10: a bounded wait returns by its deadline while the child is still
    /// running, whatever the exit event did, and reaps nothing.
    #[test]
    fn a_bounded_wait_returns_by_its_deadline_while_the_child_is_still_running() {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let handle = child.process_handle().expect("handle");

        let started = Instant::now();
        assert_eq!(
            handle
                .wait_timeout(Duration::from_millis(300))
                .expect("wait_timeout"),
            None
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(10),
            "wait_timeout(300 ms) took {elapsed:?}"
        );

        let started = Instant::now();
        assert!(!handle
            .wait_exited_timeout(Duration::from_millis(200))
            .expect("wait_exited_timeout"));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(10),
            "wait_exited_timeout(200 ms) took {elapsed:?}"
        );

        // A zero timeout looks once and returns.
        let started = Instant::now();
        assert_eq!(handle.wait_timeout(Duration::ZERO).expect("zero"), None);
        assert!(started.elapsed() < Duration::from_secs(2));

        // Nothing was reaped by any of them: the child is still ours and running.
        assert_eq!(child.try_wait().expect("try_wait"), None);
        child.terminate_tree().expect("cleanup");
    }

    /// Blocker 10: a wait in progress holds no lock, so a cancellation (here `terminate_tree`)
    /// from another thread is not queued behind it, and the wait returns with the status.
    #[test]
    fn a_cancelled_bounded_wait_returns_and_the_cancel_is_not_blocked() {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let handle = child.process_handle().expect("handle");
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let _ = done_tx.send(handle.wait_timeout(Duration::from_secs(120)));
        });
        // Let the waiter get into its wait.
        thread::sleep(Duration::from_millis(200));
        assert!(done_rx.try_recv().is_err(), "the wait returned early");
        let cancelled = Instant::now();
        let killed = child.terminate_tree().expect("terminate during a wait");
        assert!(
            cancelled.elapsed() < Duration::from_secs(10),
            "the cancel waited behind the wait: {:?}",
            cancelled.elapsed()
        );
        let waited = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the cancelled wait returned")
            .expect("wait_timeout")
            .expect("the wait saw the exit");
        waiter.join().expect("waiter");
        assert_eq!(waited.signal(), Some(libc::SIGKILL));
        assert_eq!(killed.signal(), Some(libc::SIGKILL));
    }

    /// The waiter holds no lock while it sleeps, so another thread can kill the child and the
    /// waiter is released with the real status.
    #[test]
    fn a_blocked_wait_is_released_when_another_thread_kills_the_child() {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let pid = child.id();
        let handle = child.process_handle().expect("handle");
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let arrived_tx = Mutex::new(arrived_tx);
        gate::install(pid, move || {
            let _ = arrived_tx.lock().unwrap().send(());
        });
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let _ = done_tx.send(handle.wait());
        });
        arrived_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the waiter reached its sleep");
        // Let it enter its sleep.
        thread::sleep(Duration::from_millis(100));
        assert!(done_rx.try_recv().is_err(), "the wait returned early");
        let killed = child
            .terminate_tree()
            .expect("terminate while a wait is blocked");
        let waited = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the blocked wait was released");
        gate::remove(pid);
        waiter.join().expect("waiter");
        assert_eq!(waited.expect("status").signal(), Some(libc::SIGKILL));
        assert_eq!(killed.signal(), Some(libc::SIGKILL));
    }

    /// Same, through `kill` on the shared child instead of `terminate_tree`: taking the lock
    /// while another thread waits is not blocked.
    #[test]
    fn a_blocked_wait_is_released_by_a_plain_kill_from_another_thread() {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let handle = child.process_handle().expect("handle");
        let waiter = thread::spawn(move || handle.wait());
        thread::sleep(Duration::from_millis(200));
        child
            .child()
            .kill()
            .expect("kill while another thread waits");
        let status = waiter.join().expect("waiter").expect("status");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    /// An exit monitor (`wait_exited`) in progress does not block the owner either.
    #[test]
    fn an_exit_monitor_in_progress_does_not_block_termination() {
        let mut child = sh("exec sleep 600").spawn().expect("spawn");
        let monitor = child.process_handle().expect("handle");
        let waiter = thread::spawn(move || monitor.wait_exited());
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        child
            .terminate_tree()
            .expect("terminate during the monitor");
        assert!(started.elapsed() < Duration::from_secs(10));
        waiter
            .join()
            .expect("monitor")
            .expect("the monitor saw the exit");
    }

    /// `wait_or_kill` returns when the leader exits, not when the grace ends.
    #[test]
    fn wait_or_kill_returns_at_the_exit_not_at_the_end_of_the_grace() {
        let mut child = sh("exec sleep 0.2").spawn().expect("spawn");
        let started = Instant::now();
        let outcome = child
            .wait_or_kill(Duration::from_secs(60))
            .expect("wait_or_kill");
        assert!(started.elapsed() < Duration::from_secs(20));
        // TERM at the start ended `sleep` at once; either way nothing waited out the grace.
        assert!(matches!(
            outcome,
            WaitOutcome::Exited(_) | WaitOutcome::Terminated(_)
        ));
    }

    /// The tree-wide grace is bounded by its length: a member that ignores TERM keeps the group
    /// alive for the whole grace and is then killed; one that exits on TERM does not hold the
    /// call up.
    #[test]
    fn the_tree_grace_waits_for_a_term_resistant_member_and_no_longer() {
        let dir = std::env::temp_dir().join(format!("rk-grace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let pid_file = dir.join("straggler.pid");
        let script = format!(
            "sh -c 'trap \"\" TERM; echo $$ > \"$0\"; exec sleep 600' '{}' &\nexit 0",
            pid_file.display()
        );
        let mut child = sh(&script).spawn().expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(10);
        let straggler: i32 = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the straggler never started");
            thread::sleep(Duration::from_millis(5));
        };
        let started = Instant::now();
        child
            .terminate_tree_with_options(crate::TerminationOptions {
                grace: Duration::from_millis(600),
                ..Default::default()
            })
            .expect("terminate");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(10),
            "the grace took {elapsed:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(straggler) {
            assert!(Instant::now() < deadline, "the straggler survived the KILL");
            thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Running, and not a zombie that only waits for its reaper.
    fn running(pid: i32) -> bool {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|text| {
                    text.rfind(')')
                        .map(|at| !text[at + 1..].trim_start().starts_with('Z'))
                })
                .unwrap_or(false)
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: an all-zero proc_bsdinfo is valid; the call fills at most its size.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            // arg 0 does not describe zombies, so a full record means a live process.
            let written = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            written == size
        }
    }

    #[test]
    fn the_tree_grace_ends_early_when_every_member_exits_on_term() {
        let mut child = sh("sleep 600 &\nsleep 600 &\nwait").spawn().expect("spawn");
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        child
            .terminate_tree_with_options(crate::TerminationOptions {
                grace: Duration::from_secs(30),
                ..Default::default()
            })
            .expect("terminate");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the call waited out the grace: {:?}",
            started.elapsed()
        );
    }
}
