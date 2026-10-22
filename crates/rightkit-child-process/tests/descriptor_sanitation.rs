//! Orthic-Labs/rightkit#835: a spawned child inherits no descriptor above stderr unless the
//! caller names it. The parent opens a pipe and a socketpair (low numbers) and duplicates one
//! end of each far above them, spawns this test executable again through every path the crate
//! offers, and the child reports for each of those numbers whether `fcntl(F_GETFD)` succeeds.
//!
//! The async-signal-safety of the hook is a code-review invariant, documented on
//! `Inheritance::apply` in `src/process_unix.rs`: `fcntl` and `close_range` only, nothing that
//! allocates or locks runs between `fork` and `exec`. No runtime test can prove it.
#![cfg(unix)]

use rightkit_child_process::{
    command_std, run_command, OwnedCommand, RunOptions, SanitizeDescriptors,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::fd::RawFd,
    process::Stdio,
    time::Duration,
};

const PROBE_ENV: &str = "RIGHTKIT_PROBE_FDS";

/// The child: reports each descriptor named in the environment, then echoes stdin. Re-run by the
/// tests below with `--ignored`; does nothing when the variable is absent.
#[test]
#[ignore]
fn descriptor_probe_child() {
    let Some(list) = std::env::var_os(PROBE_ENV) else {
        return;
    };
    // Leading newline: libtest may have printed `test name ... ` without one before this.
    let mut report = String::from("\n");
    for fd in list.to_str().unwrap().split(',') {
        let number: RawFd = fd.parse().unwrap();
        let present = unsafe { libc::fcntl(number, libc::F_GETFD) } >= 0;
        if std::env::var_os("RIGHTKIT_PROBE_NO_INHERIT").is_some() {
            assert!(!present, "terminal child inherited fd {number}");
        }
        report.push_str(&format!(
            "FD {number} {}\n",
            if present { "present" } else { "absent" }
        ));
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    report.push_str(&format!("STDIN {}\n", input.trim()));
    // One write: libtest's own output on the same stream cannot interleave with it.
    std::io::stdout().write_all(report.as_bytes()).unwrap();
}

/// Open, non-close-on-exec descriptors of the parent, as a leaking parent would hold them.
struct Held {
    all: Vec<RawFd>,
    pipe_read: RawFd,
    pipe_write: RawFd,
    socket_a: RawFd,
    high_pipe_write: RawFd,
    high_socket_a: RawFd,
}

impl Held {
    fn open() -> Self {
        let mut pipe = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let mut sockets = [-1; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
            0
        );
        let high = |fd: RawFd| {
            let moved = unsafe { libc::fcntl(fd, libc::F_DUPFD, 100) };
            assert!(moved >= 100, "dup above 100");
            moved
        };
        let high_pipe_write = high(pipe[1]);
        let high_socket_a = high(sockets[0]);
        let all = vec![
            pipe[0],
            pipe[1],
            sockets[0],
            sockets[1],
            high_pipe_write,
            high_socket_a,
        ];
        for &fd in &all {
            assert!(!cloexec(fd), "test descriptors must be inheritable");
        }
        Self {
            all,
            pipe_read: pipe[0],
            pipe_write: pipe[1],
            socket_a: sockets[0],
            high_pipe_write,
            high_socket_a,
        }
    }

    fn list(&self) -> String {
        self.all
            .iter()
            .map(RawFd::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        for &fd in &self.all {
            unsafe { libc::close(fd) };
        }
    }
}

fn cloexec(fd: RawFd) -> bool {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "fd {fd} is open in the parent");
    flags & libc::FD_CLOEXEC != 0
}

fn exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

const ARGS: [&str; 4] = [
    "--exact",
    "descriptor_probe_child",
    "--ignored",
    "--nocapture",
];

/// `fd -> present in the child`, and the echoed stdin, from the child's stdout.
fn parse(stdout: &[u8]) -> (BTreeMap<RawFd, bool>, String) {
    let text = String::from_utf8_lossy(stdout);
    let mut fds = BTreeMap::new();
    let mut stdin = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("FD ") {
            let (fd, state) = rest.split_once(' ').unwrap();
            fds.insert(fd.parse().unwrap(), state == "present");
        } else if let Some(rest) = line.strip_prefix("STDIN ") {
            stdin = rest.to_string();
        }
    }
    assert!(!fds.is_empty(), "the child reported nothing: {text}");
    (fds, stdin)
}

fn assert_only(report: &BTreeMap<RawFd, bool>, held: &Held, kept: &[RawFd]) {
    for &fd in &held.all {
        assert_eq!(
            report[&fd],
            kept.contains(&fd),
            "fd {fd}: present in child must equal 'caller listed it' (kept {kept:?}, report {report:?})"
        );
    }
}

fn owned(held: &Held) -> OwnedCommand {
    let mut command = OwnedCommand::new(exe());
    command.command_mut().args(ARGS).env(PROBE_ENV, held.list());
    command
}

fn run_owned(command: OwnedCommand) -> (BTreeMap<RawFd, bool>, String) {
    let output = run_command(
        command,
        Some(b"hello stdin\n"),
        &RunOptions::new(Duration::from_secs(60)),
    )
    .expect("run_command");
    assert!(output.status.success(), "child failed: {:?}", output.stderr);
    parse(&output.stdout.bytes)
}

fn run_std(mut command: std::process::Command) -> (BTreeMap<RawFd, bool>, String) {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello stdin\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    parse(&output.stdout)
}

fn std_command(held: &Held) -> std::process::Command {
    let mut command = command_std(exe());
    command.args(ARGS).env(PROBE_ENV, held.list());
    command
}

// ---- stale registration-time bound --------------------------------------------------------

/// A command registered under a low soft limit, with the limit raised and an inheritable
/// descriptor opened far above everything open at registration, then spawned through
/// `refresh_sanitized`: the descriptor is absent. (A bare `spawn()` is documented to use the
/// registration-time bound plus margin, so on macOS it may miss such a descriptor; Linux's
/// `close_range` has no bound.)
#[test]
fn a_refreshed_command_sweeps_a_descriptor_opened_above_the_registration_bound() {
    let held = Held::open();
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let original = limit;
    limit.rlim_cur = 512.min(limit.rlim_max);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut command = std_command(&held);
    // Raise the limit, then open a descriptor beyond registration bound (highest + 256).
    limit.rlim_cur = 8192.min(original.rlim_max);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let far = unsafe { libc::fcntl(held.pipe_read, libc::F_DUPFD, 4000) };
    let restore = || unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) };
    if far < 0 {
        restore();
        panic!("could not open a descriptor above 4000");
    }
    command.env(PROBE_ENV, format!("{},{far}", held.list()));
    command.refresh_sanitized();
    let (report, _) = run_std(command);
    unsafe { libc::close(far) };
    restore();
    assert_only(&report, &held, &[]);
    assert_eq!(report.get(&far), Some(&false), "far descriptor swept");
}

// ---- OwnedCommand ----------------------------------------------------------------------

#[test]
fn owned_command_default_drops_every_inherited_descriptor_and_stdio_works() {
    let held = Held::open();
    let (report, stdin) = run_owned(owned(&held));
    assert_only(&report, &held, &[]);
    assert_eq!(stdin, "hello stdin", "piped stdin and stdout still work");
}

#[test]
fn owned_command_spawn_with_caller_pipes_drops_inherited_descriptors() {
    let held = Held::open();
    let mut command = owned(&held);
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    let mut stdout = Vec::new();
    child
        .take_stdout()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let (report, stdin) = parse(&stdout);
    assert_only(&report, &held, &[]);
    assert_eq!(stdin, "");
}

#[test]
fn owned_command_detached_spawn_drops_inherited_descriptors() {
    let held = Held::open();
    let mut command = owned(&held);
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let mut child = command.spawn_uncontained_detached().expect("spawn");
    let mut stdout = Vec::new();
    child
        .take_stdout()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let (report, _) = parse(&stdout);
    assert_only(&report, &held, &[]);
    let _ = child.wait();
}

#[test]
fn inherit_fd_keeps_only_the_named_descriptors_and_leaves_the_parent_flags() {
    let held = Held::open();
    let mut command = owned(&held);
    command
        .inherit_fd(held.socket_a)
        .inherit_fds(&[held.high_pipe_write, held.pipe_read]);
    let (report, _) = run_owned(command);
    assert_only(
        &report,
        &held,
        &[held.socket_a, held.high_pipe_write, held.pipe_read],
    );
    for &fd in &held.all {
        assert!(!cloexec(fd), "the parent's flags are unchanged (fd {fd})");
    }
}

#[test]
fn inherit_fd_is_added_to_inherit_handles_only() {
    let held = Held::open();
    let mut command = owned(&held);
    command.inherit_handles_only([held.pipe_write]);
    command.inherit_fd(held.high_socket_a);
    let (report, _) = run_owned(command);
    assert_only(&report, &held, &[held.pipe_write, held.high_socket_a]);
}

#[test]
fn inherit_all_descriptors_opts_out_of_the_sweep() {
    let held = Held::open();
    let mut command = owned(&held);
    command.inherit_all_descriptors(true);
    let (report, _) = run_owned(command);
    assert_only(&report, &held, &held.all);
}

#[test]
fn inherit_all_descriptors_conflicts_with_a_list_instead_of_guessing() {
    let held = Held::open();
    let mut command = owned(&held);
    command
        .inherit_all_descriptors(true)
        .inherit_fd(held.pipe_read);
    assert_eq!(
        command.spawn().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    let mut command = owned(&held);
    command
        .inherit_all_descriptors(true)
        .inherit_handles_only([]);
    assert_eq!(
        command.spawn().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_listed_descriptor_that_is_not_open_fails_before_spawn() {
    let held = Held::open();
    let mut command = owned(&held);
    command.inherit_fd(-1);
    assert_eq!(
        command.spawn().unwrap_err().raw_os_error(),
        Some(libc::EBADF)
    );
    let mut command = owned(&held);
    command.inherit_fd(999);
    assert_eq!(
        command.spawn().unwrap_err().raw_os_error(),
        Some(libc::EBADF)
    );
}

#[test]
fn the_default_sweep_preserves_exec_failure_reporting() {
    // An existing file without execute permission fails with EACCES at exec, after the hook
    // ran. Sweeping must not close std's close-on-exec error pipe, or spawn would look like a
    // success.
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("rightkit-835-bad-exec-{}", std::process::id()));
    std::fs::write(&path, b"not executable").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let result = OwnedCommand::new(&path).spawn();
    std::fs::remove_file(path).unwrap();
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EACCES));
}

// ---- std::process::Command -------------------------------------------------------------

#[test]
fn terminal_status_drops_inherited_descriptors() {
    let held = Held::open();
    let mut command = std::process::Command::new(exe());
    command
        .args(ARGS)
        .env(PROBE_ENV, held.list())
        .env("RIGHTKIT_PROBE_NO_INHERIT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    assert!(
        rightkit_child_process::status_inherited_terminal(&mut command)
            .unwrap()
            .success()
    );
}

#[test]
fn command_std_drops_every_inherited_descriptor_and_stdio_works() {
    let held = Held::open();
    let (report, stdin) = run_std(std_command(&held));
    assert_only(&report, &held, &[]);
    assert_eq!(stdin, "hello stdin");
}

#[test]
fn a_plain_std_command_is_swept_by_the_trait() {
    let held = Held::open();
    let mut command = std::process::Command::new(exe());
    command.args(ARGS).env(PROBE_ENV, held.list());
    command.sanitize_descriptors(&[]);
    let (report, _) = run_std(command);
    assert_only(&report, &held, &[]);
}

#[test]
fn a_later_keep_list_restores_only_the_named_descriptors_on_a_std_command() {
    let held = Held::open();
    let mut command = std_command(&held);
    command.sanitize_descriptors(&[held.high_pipe_write, held.socket_a]);
    let (report, _) = run_std(command);
    assert_only(&report, &held, &[held.high_pipe_write, held.socket_a]);
}

#[test]
fn an_unopened_keep_descriptor_fails_a_std_spawn_with_ebadf() {
    let held = Held::open();
    for fd in [-1, i32::MAX] {
        let mut command = std_command(&held);
        command.sanitize_descriptors(&[fd]);
        assert_eq!(
            command.spawn().unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    }
}

// ---- tokio::process::Command -----------------------------------------------------------

#[cfg(feature = "tokio")]
mod with_tokio {
    use super::*;
    use rightkit_child_process::tokio_support;

    #[tokio::test]
    async fn invalid_keep_descriptors_fail_tokio_spawn_with_ebadf() {
        for fd in [-1, i32::MAX] {
            let mut command = tokio_support::command(exe());
            command.sanitize_descriptors(&[fd]);
            assert_eq!(
                command.spawn().unwrap_err().raw_os_error(),
                Some(libc::EBADF)
            );
        }
    }

    /// The dev-dependency `tokio` has no `io-util`, so stdin is not written: it is piped and
    /// closed, which the child sees as an empty input; stdout is read back through the pipe.
    async fn run_tokio(mut command: tokio::process::Command) -> (BTreeMap<RawFd, bool>, String) {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        drop(child.stdin.take());
        let output = child.wait_with_output().await.expect("wait");
        assert!(output.status.success());
        parse(&output.stdout)
    }

    fn tokio_command(held: &Held) -> tokio::process::Command {
        let mut command = tokio_support::command(exe());
        command.args(ARGS).env(PROBE_ENV, held.list());
        command
    }

    #[tokio::test]
    async fn tokio_command_drops_every_inherited_descriptor_and_stdio_works() {
        let held = Held::open();
        let (report, stdin) = run_tokio(tokio_command(&held)).await;
        assert_only(&report, &held, &[]);
        assert_eq!(stdin, "", "piped stdin closes and piped stdout still works");
    }

    #[tokio::test]
    async fn a_plain_tokio_command_is_swept_by_the_trait() {
        let held = Held::open();
        let mut command = tokio::process::Command::new(exe());
        command.args(ARGS).env(PROBE_ENV, held.list());
        command.sanitize_descriptors(&[]);
        let (report, _) = run_tokio(command).await;
        assert_only(&report, &held, &[]);
    }

    #[tokio::test]
    async fn a_later_keep_list_restores_only_the_named_descriptors_on_a_tokio_command() {
        let held = Held::open();
        let mut command = tokio_command(&held);
        command.sanitize_descriptors(&[held.pipe_write, held.high_socket_a]);
        let (report, _) = run_tokio(command).await;
        assert_only(&report, &held, &[held.pipe_write, held.high_socket_a]);
    }
}
