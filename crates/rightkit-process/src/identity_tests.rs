//! Unit tests for the process-group identity rule, with every signal and every reap recorded
//! in order by `unix_child::trace`.

use crate::unix_child::trace::{self, Event};
use crate::{OwnedChild, OwnedCommand};
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{Arc, Barrier},
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
            Event::Reaped { .. } => None,
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
