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
            Event::Reaped { .. } | Event::Wait { .. } => None,
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
