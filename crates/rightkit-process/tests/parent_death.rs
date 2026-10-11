#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

//! `kill_on_parent_exit` integration tests. The "parent" is this test binary
//! re-executed with `ROLE_ENV` set. It spawns a sleeping child and prints its
//! pid. The outer test SIGKILLs the parent, which runs no cleanup code, then
//! checks whether the child is gone.

use std::{
    fs,
    io::{self, BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::path::PathBuf;

use rightkit_process::OwnedCommand;

const ROLE_ENV: &str = "RIGHTKIT_PROCESS_PARENT_DEATH_ROLE";
const TREE_FILE_ENV: &str = "RIGHTKIT_PROCESS_PARENT_DEATH_TREE_FILE";
const MARKER: &str = "CHILD_PID=";
/// The macOS path waits a 2 s grace before SIGKILL. A tied child dies on the first TERM,
/// so this window is the outer bound with scheduling slack.
const GONE_WITHIN: Duration = Duration::from_secs(5);

/// Re-exec entry point. Without `ROLE_ENV` it returns at once, so a normal test
/// run is unaffected.
#[test]
fn parent_death_role() {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return;
    };
    let tied = role != "untied";
    let mut command = if role == "tied-tree" {
        let tree_file = std::env::var_os(TREE_FILE_ENV).expect("tree file path is set");
        let mut command = OwnedCommand::new("/bin/sh");
        command
            .command_mut()
            .args(["-c", "sleep 600 & echo $! > \"$0\"; wait"])
            .arg(tree_file);
        command
    } else if role == "tied-orphan" {
        // The engine leaves at once, leaving a grandchild in its group.
        let tree_file = std::env::var_os(TREE_FILE_ENV).expect("tree file path is set");
        let mut command = OwnedCommand::new("/bin/sh");
        command
            .command_mut()
            .args(["-c", "sleep 600 & echo $! > \"$0\""])
            .arg(tree_file);
        command
    } else {
        let mut command = OwnedCommand::new("sleep");
        command.command_mut().arg("600");
        command
    };
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.kill_on_parent_exit(tied);
    let mut child = command.spawn().expect("spawn child");
    if role == "tied-orphan" {
        // Reap the engine: the supervisor still owns the tie, though the engine is gone.
        child.wait().expect("reap the engine");
    }
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

fn start_role(mode: &str, tree_file: Option<&Path>) -> Role {
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
    if let Some(path) = tree_file {
        command.env(TREE_FILE_ENV, path);
    }
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

/// Linux reads /proc. macOS asks ps; both report the state character.
#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.trim_start().chars().next()
}

#[cfg(target_os = "macos")]
fn process_state(pid: u32) -> Option<char> {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .chars()
        .next()
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

#[cfg(target_os = "macos")]
fn temp_path(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rightkit-process-parent-death-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    path
}

#[cfg(target_os = "macos")]
fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "grandchild pid file was not written"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn tied_child_dies_when_its_parent_is_sigkilled() {
    let mut role = start_role("tied", None);
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
        let mut role = start_role("tied", None);
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
    let mut role = start_role("untied", None);
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

#[cfg(target_os = "macos")]
#[test]
fn tied_kill_reaches_grandchildren_in_the_process_group() {
    let tree_file = temp_path("tree");
    let mut role = start_role("tied-tree", Some(tree_file.as_path()));
    let grandchild = wait_for_pid_file(&tree_file);
    sigkill(role.process.id());
    role.process.wait().expect("reap killed role");
    let child_gone = wait_until_gone(role.child_pid, GONE_WITHIN);
    let grandchild_gone = wait_until_gone(grandchild, GONE_WITHIN);
    let group_gone = wait_until_group_gone(role.child_pid, GONE_WITHIN);
    if !(child_gone && grandchild_gone && group_gone) {
        group_kill(role.child_pid);
        sigkill(grandchild);
    }
    let _ = fs::remove_file(&tree_file);
    assert!(
        child_gone && grandchild_gone && group_gone,
        "tied group {} left child, grandchild {grandchild} or another member alive",
        role.child_pid
    );
}

/// The tie outlives the engine: while the supervisor still holds the `OwnedChild`, a
/// grandchild that outlived the engine dies with the supervisor. macOS only; Linux's
/// `PR_SET_PDEATHSIG` names the direct child alone, so nothing here is promised there.
#[cfg(target_os = "macos")]
#[test]
fn tied_kill_reaches_a_grandchild_that_outlived_the_engine() {
    let tree_file = temp_path("orphan");
    let mut role = start_role("tied-orphan", Some(tree_file.as_path()));
    let grandchild = wait_for_pid_file(&tree_file);
    assert!(
        !is_gone(grandchild),
        "the grandchild must outlive the engine"
    );
    assert!(is_gone(role.child_pid), "the engine has exited");
    sigkill(role.process.id());
    role.process.wait().expect("reap killed role");
    let gone = wait_until_gone(grandchild, GONE_WITHIN);
    if !gone {
        sigkill(grandchild);
    }
    let _ = fs::remove_file(&tree_file);
    assert!(
        gone,
        "grandchild {grandchild} outlived the SIGKILLed supervisor that still held its OwnedChild"
    );
}

#[test]
fn detached_spawn_rejects_kill_on_parent_exit() {
    let mut command = OwnedCommand::new("/bin/sh");
    command.command_mut().args(["-c", "true"]);
    command.kill_on_parent_exit(true);
    let Err(error) = command.spawn_uncontained_detached() else {
        panic!("a detached spawn must not accept the parent-death tie");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
