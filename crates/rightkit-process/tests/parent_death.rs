#![cfg(target_os = "linux")]

//! `kill_on_parent_exit` integration tests (Linux only: `PR_SET_PDEATHSIG`; other hosts refuse
//! the option, see the unit tests in `src/owned.rs`). The "parent" is this test binary
//! re-executed with `ROLE_ENV` set. It spawns a sleeping child and prints its
//! pid. The outer test SIGKILLs the parent, which runs no cleanup code, then
//! checks whether the child is gone.

use std::{
    fs,
    io::{self, BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rightkit_process::OwnedCommand;

const ROLE_ENV: &str = "RIGHTKIT_PROCESS_PARENT_DEATH_ROLE";
const MARKER: &str = "CHILD_PID=";
/// A tied child dies on the first TERM, so this window is the outer bound with scheduling
/// slack.
const GONE_WITHIN: Duration = Duration::from_secs(5);

/// Re-exec entry point. Without `ROLE_ENV` it returns at once, so a normal test
/// run is unaffected.
#[test]
fn parent_death_role() {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return;
    };
    let tied = role != "untied";
    let mut command = OwnedCommand::new("sleep");
    command.command_mut().arg("600");
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.kill_on_parent_exit(tied);
    let child = command.spawn().expect("spawn child");
    println!("{MARKER}{}", child.id());
    // Never returns. The outer test SIGKILLs this process, so no destructor
    // runs and the child is left to the parent-death tie.
    let _child = child;
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

struct Role {
    process: Child,
    child_pid: u32,
}

fn start_role(mode: &str) -> Role {
    let mut command = Command::new(std::env::current_exe().expect("test binary path"));
    command
        .args([
            "--exact",
            "parent_death_role",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = command.spawn().expect("spawn role");
    let stdout = process.stdout.take().expect("role stdout is piped");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // libtest may print its own "test ... " prefix on the same line.
            if let Some(index) = line.find(MARKER) {
                let digits: String = line[index + MARKER.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if let Ok(pid) = digits.parse::<u32>() {
                    let _ = sender.send(pid);
                }
            }
        }
    });
    let child_pid = receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("role published its child pid");
    Role { process, child_pid }
}

fn sigkill(pid: u32) {
    // SAFETY: plain kill(2) on a pid this test created.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// SIGKILL the whole process group led by `pid`, to clean up after a failing test.
fn group_kill(pid: u32) {
    // SAFETY: a negative pid targets only the group this test spawned.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

/// Reads /proc for the state character.
fn process_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.trim_start().chars().next()
}

/// True once the pid no longer names a live process. A zombie counts as gone
/// because it only waits for a reaper.
fn is_gone(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence and permission.
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    process_state(pid) == Some('Z')
}

/// True once no process remains in the group `pgid`. A zombie that only waits for a
/// reaper is not counted; `ps` lists the members that remain.
fn group_is_gone(pgid: u32) -> bool {
    // SAFETY: signal 0 only checks existence of the group.
    if unsafe { libc::kill(-(pgid as i32), 0) } != 0 {
        return io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    let output = Command::new("ps")
        .args(["-A", "-o", "pgid=,stat="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    !output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().and_then(|g| g.parse::<u32>().ok()) == Some(pgid)
            && !fields.next().is_some_and(|stat| stat.starts_with('Z'))
    })
}

fn wait_until_group_gone(pgid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if group_is_gone(pgid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if is_gone(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn tied_child_dies_when_its_parent_is_sigkilled() {
    let mut role = start_role("tied");
    assert!(
        !is_gone(role.child_pid),
        "child must be running before its parent is killed"
    );
    sigkill(role.process.id());
    role.process.wait().expect("reap killed role");
    let gone = wait_until_gone(role.child_pid, GONE_WITHIN);
    // The child leads its own process group; the whole group must be gone too.
    let group_gone = wait_until_group_gone(role.child_pid, GONE_WITHIN);
    if !(gone && group_gone) {
        group_kill(role.child_pid);
    }
    assert!(
        gone,
        "tied child {} outlived its SIGKILLed parent",
        role.child_pid
    );
    assert!(
        group_gone,
        "process group {} outlived its SIGKILLed parent",
        role.child_pid
    );
}

/// The tie must already be armed when `spawn` returns: killing the parent at once, with
/// no settling time, still takes the child down. Repeated to catch a registration race.
#[test]
fn tied_child_dies_when_its_parent_is_killed_right_after_spawn() {
    for round in 0..4 {
        let mut role = start_role("tied");
        sigkill(role.process.id());
        role.process.wait().expect("reap killed role");
        let gone = wait_until_gone(role.child_pid, GONE_WITHIN);
        if !gone {
            group_kill(role.child_pid);
        }
        assert!(
            gone,
            "round {round}: child {} outlived its parent killed right after spawn",
            role.child_pid
        );
    }
}

#[test]
fn untied_child_survives_its_parent_being_sigkilled() {
    let mut role = start_role("untied");
    sigkill(role.process.id());
    role.process.wait().expect("reap killed role");
    // Longer than the tied path's grace, so a delayed kill would also show up here.
    thread::sleep(GONE_WITHIN);
    let survived = !is_gone(role.child_pid);
    group_kill(role.child_pid);
    assert!(
        survived,
        "untied child {} died with its parent without the option",
        role.child_pid
    );
}
