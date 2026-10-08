#![cfg(any(unix, windows))]

use rightkit_process::OwnedCommand;
use std::{
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

// Reexec this test executable; stdin keeps the child alive without a shell
// accidentally closing unknown descriptors before we can observe inheritance.
#[test]
#[ignore]
fn inheritance_child() {
    if let Some(missing) = std::env::var_os("RIGHTKIT_MISSING_ENV") {
        assert!(std::env::var_os(missing).is_none());
        assert_eq!(std::env::var("RIGHTKIT_KEPT_ENV").unwrap(), "Kept");
        assert!(std::env::var_os("RIGHTKIT_STRIPPED_ENV").is_none());
        let cwd = std::env::var_os("RIGHTKIT_EXPECTED_CWD").unwrap();
        assert_eq!(
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            std::fs::canonicalize(cwd).unwrap()
        );
    }
    println!("RIGHTKIT_READY");
    let mut byte = [0];
    let _ = std::io::stdin().read(&mut byte);
}

fn command() -> OwnedCommand {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "inheritance_child", "--ignored", "--nocapture"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let owned = OwnedCommand::from_command(command);
    #[cfg(windows)]
    let owned = {
        use rightkit_process::{WindowsSpawnConfig, WindowsStdio};
        let mut owned = owned;
        owned.windows_spawn_config(WindowsSpawnConfig {
            stdin: WindowsStdio::Piped,
            stdout: WindowsStdio::Piped,
            stderr: WindowsStdio::Piped,
            inherit_environment: true,
            raw_arguments: Vec::new(),
        });
        owned
    };
    owned
}

fn assert_inheritance(allow: bool, cloexec: bool) {
    let (mut read, write) = pipe(cloexec);
    let mut command = command();
    if allow {
        command.inherit_handles_only([raw(&write)]);
    } else {
        command.inherit_handles_only([]);
    }
    let mut child = command.spawn().unwrap();
    assert!(
        child.take_stderr().is_some(),
        "native stderr pipe is exposed"
    );
    assert_eq!(inheritable(&write), !cloexec, "parent flags changed");
    let mut stdout = BufReader::new(child.take_stdout().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        if line.trim() == "RIGHTKIT_READY" {
            break;
        }
    }
    drop(write);
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut byte = [0];
        sender.send(read.read(&mut byte)).unwrap();
    });
    if allow {
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    } else {
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            0
        );
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "child must still be alive"
    );
    child.terminate_tree().unwrap();
    assert!(child.wait_timeout(Duration::ZERO).unwrap().is_some());
    if allow {
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            0
        );
    }
    reader.join().unwrap();
}

#[test]
fn unlisted_inheritable_pipe_is_not_inherited() {
    assert_inheritance(false, false);
}

#[test]
fn listed_inheritable_pipe_is_inherited() {
    assert_inheritance(true, false);
}

#[test]
fn listed_noninheritable_pipe_is_inherited_without_changing_parent_flags() {
    assert_inheritance(true, true);
}

#[cfg(unix)]
fn raw(file: &std::fs::File) -> rightkit_process::RawHandleOrFd {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(unix)]
fn inheritable(file: &std::fs::File) -> bool {
    unsafe { nix::libc::fcntl(raw(file), nix::libc::F_GETFD) & nix::libc::FD_CLOEXEC == 0 }
}

#[cfg(unix)]
fn pipe(cloexec: bool) -> (std::fs::File, std::fs::File) {
    use std::os::fd::FromRawFd;
    let mut fds = [-1; 2];
    assert_eq!(unsafe { nix::libc::pipe(fds.as_mut_ptr()) }, 0);
    let files = unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    };
    assert_eq!(
        unsafe { nix::libc::fcntl(fds[0], nix::libc::F_SETFD, nix::libc::FD_CLOEXEC) },
        0
    );
    if cloexec {
        assert_eq!(
            unsafe { nix::libc::fcntl(fds[1], nix::libc::F_SETFD, nix::libc::FD_CLOEXEC) },
            0
        );
    }
    files
}

#[cfg(windows)]
fn raw(file: &std::fs::File) -> rightkit_process::RawHandleOrFd {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle()
}

#[cfg(windows)]
fn inheritable(file: &std::fs::File) -> bool {
    let mut flags = 0;
    assert_ne!(unsafe { GetHandleInformation(raw(file), &mut flags) }, 0);
    flags & 1 != 0
}

#[cfg(windows)]
fn pipe(cloexec: bool) -> (std::fs::File, std::fs::File) {
    use std::os::windows::io::FromRawHandle;
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    assert_ne!(
        unsafe { CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 0) },
        0
    );
    let files = unsafe {
        (
            std::fs::File::from_raw_handle(read),
            std::fs::File::from_raw_handle(write),
        )
    };
    if !cloexec {
        assert_ne!(unsafe { SetHandleInformation(write, 1, 1) }, 0);
    }
    files
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreatePipe(
        read: *mut *mut std::ffi::c_void,
        write: *mut *mut std::ffi::c_void,
        attributes: *mut std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
    fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
}

#[cfg(unix)]
#[test]
fn inheritance_filter_preserves_exec_failure_reporting() {
    // Existing absolute path without execute permission: exec fails with EACCES after
    // pre_exec has run. (An executable file with a bad format is not usable here:
    // execvp answers ENOEXEC by re-running it through /bin/sh, so spawn succeeds.)
    let path = std::env::temp_dir().join(format!("rightkit-bad-exec-{}", std::process::id()));
    std::fs::write(&path, b"not executable").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut command = OwnedCommand::new(&path);
    command.inherit_handles_only([]);
    let result = command.spawn();
    std::fs::remove_file(path).unwrap();
    assert!(result.unwrap_err().raw_os_error().is_some());
}

#[cfg(unix)]
#[test]
fn invalid_allowlisted_fd_fails_before_spawn() {
    let mut command = command();
    command.inherit_handles_only([-1]);
    assert_eq!(
        command.spawn().unwrap_err().raw_os_error(),
        Some(nix::libc::EBADF)
    );
}

#[cfg(windows)]
#[test]
fn native_spawn_honors_environment_filters_cwd_and_stdio() {
    let missing = std::env::vars_os()
        .map(|(key, _)| key)
        .find(|key| !key.to_string_lossy().starts_with("RIGHTKIT_"))
        .unwrap();
    let cwd = std::env::temp_dir();
    let mut command = command();
    command
        .inherit_handles_only([])
        .env_allowlist(std::iter::empty::<&str>());
    command
        .command_mut()
        .env("RIGHTKIT_MISSING_ENV", missing)
        .env("RIGHTKIT_KEPT_ENV", "Kept")
        .env("RIGHTKIT_STRIPPED_ENV", "remove")
        .env("RIGHTKIT_EXPECTED_CWD", &cwd)
        .current_dir(&cwd);
    command.env_strip(["RIGHTKIT_STRIPPED_ENV"]);
    let mut child = command.spawn().unwrap();
    assert!(child.creation_time_ticks().is_some());
    let reader = child.process_handle().unwrap();
    let mut stdout = BufReader::new(child.take_stdout().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        if line.trim() == "RIGHTKIT_READY" {
            break;
        }
    }
    assert!(child.wait_timeout(Duration::ZERO).unwrap().is_none());
    assert!(child
        .wait_timeout(Duration::from_nanos(1))
        .unwrap()
        .is_none());
    drop(child.take_stdin());
    assert!(child.wait().unwrap().success());
    assert_eq!(reader.wait().unwrap(), child.wait().unwrap());
}

#[cfg(windows)]
#[test]
fn missing_native_configuration_fails_without_spawning() {
    let mut command = OwnedCommand::new("cmd.exe");
    command.command_mut().args(["/c", "exit 0"]);
    command.inherit_handles_only([]);
    assert_eq!(
        command.spawn().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(windows)]
#[test]
fn native_spawn_failure_restores_handle_flags() {
    let (_read, write) = pipe(true);
    let mut command = command();
    // Resolve program & create stdio successfully, then CreateProcessW rejects cwd.
    command.command_mut().current_dir(
        std::env::temp_dir()
            .join(format!("rightkit-no-cwd-{}", std::process::id()))
            .join("missing"),
    );
    command.inherit_handles_only([raw(&write)]);
    assert!(command.spawn().unwrap_err().raw_os_error().is_some());
    assert!(!inheritable(&write));
}

#[test]
fn independent_blocking_wait_keeps_stdio_and_timeout_accessible() {
    let mut command = command();
    command.inherit_handles_only([]);
    let mut child = command.spawn().unwrap();
    let mut stdout = BufReader::new(child.take_stdout().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        if line.trim() == "RIGHTKIT_READY" {
            break;
        }
    }
    let reader = child.process_handle().unwrap();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        sender.send(()).unwrap();
        reader.wait().unwrap()
    });
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(child
        .wait_timeout(Duration::from_millis(50))
        .unwrap()
        .is_none());
    let stdin = child
        .take_stdin()
        .expect("independent wait keeps stdin open");
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(waiter.join().unwrap(), status);
    assert_eq!(child.child().wait().unwrap(), status);
}
