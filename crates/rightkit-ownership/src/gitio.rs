//! Bounded child-process runner for `git`. Every spawn is windowless on Windows
//! (`CREATE_NO_WINDOW`), has no stdin unless input is supplied, and is held to a wall
//! clock timeout and an output cap, as Node's `spawnSync` with `timeout` and `maxBuffer`.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Failure to run a process to completion. `code` is the Node error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnError {
    pub code: &'static str,
}

#[derive(Debug)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Native git for protected reads. Windows uses Git for Windows, never PATH.
pub fn native_git() -> &'static str {
    if cfg!(windows) {
        "C:\\Program Files\\Git\\cmd\\git.exe"
    } else {
        "/usr/bin/git"
    }
}

/// `os.devNull`.
pub fn dev_null() -> &'static str {
    if cfg!(windows) {
        "\\\\.\\nul"
    } else {
        "/dev/null"
    }
}

/// Environment for protected git reads: every `GIT_*` variable dropped, then the
/// config isolation variables set.
pub fn protected_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.to_ascii_uppercase().starts_with("GIT_"))
        .collect();
    for (key, value) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        (
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        ),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        env.push((key.to_string(), value.to_string()));
    }
    env
}

/// Runs `program args` and waits for it. `env` replaces the whole environment when given.
pub fn run<S: AsRef<OsStr>>(
    program: &str,
    args: &[S],
    env: Option<&[(String, String)]>,
    input: Option<&[u8]>,
    timeout: Duration,
    max_output: usize,
) -> Result<Output, SpawnError> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(env) = env {
        command.env_clear();
        command.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| SpawnError {
        code: if error.kind() == std::io::ErrorKind::NotFound {
            "ENOENT"
        } else {
            "EACCES"
        },
    })?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        let bytes = bytes.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, ()>>();
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    buffer.extend_from_slice(&chunk[..count]);
                    if buffer.len() > max_output {
                        let _ = tx.send(Err(()));
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Ok(buffer));
    });
    let deadline = Instant::now() + timeout;
    loop {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(Ok(buffer)) => {
                let status = wait(&mut child, deadline)?;
                return Ok(Output {
                    status,
                    stdout: buffer,
                });
            }
            Ok(Err(())) => {
                kill(&mut child);
                return Err(SpawnError { code: "ENOBUFS" });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    kill(&mut child);
                    return Err(SpawnError { code: "ETIMEDOUT" });
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                kill(&mut child);
                return Err(SpawnError { code: "EPIPE" });
            }
        }
    }
}

fn wait(child: &mut Child, deadline: Instant) -> Result<Option<i32>, SpawnError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill(child);
                    return Err(SpawnError { code: "ETIMEDOUT" });
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                kill(child);
                return Err(SpawnError { code: "EPIPE" });
            }
        }
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Ordinary `git -C root ...` through PATH with the caller's environment, as Node's
/// plain `spawnSync("git", ...)` calls.
pub fn plain_git(root: &Path, args: &[&str], timeout: Duration) -> Result<Output, SpawnError> {
    let mut full: Vec<&OsStr> = vec![OsStr::new("-C"), root.as_os_str()];
    full.extend(args.iter().map(OsStr::new));
    run("git", &full, None, None, timeout, 4 * 1024 * 1024)
}
