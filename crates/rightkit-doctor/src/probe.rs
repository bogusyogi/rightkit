//! Neutral probes: tools on the search path, writable folders, bounded child processes, free
//! ports and the system clipboard. A probe never reads a secret, sends a request or changes
//! user data.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The first file named `command` (with `.exe`, `.cmd` and `.bat` tried on Windows) in a
/// directory of `PATH`.
pub fn command_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            for extension in [".exe", ".cmd", ".bat"] {
                let candidate = directory.join(format!("{command}{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// A child of a windowless parent opens no console window on Windows (`CREATE_NO_WINDOW`); a no-op elsewhere.
fn hidden(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

/// The first line `<executable> --version` prints, trimmed; `None` when it cannot run, exits
/// non-zero or prints nothing.
pub fn tool_version(executable: &Path) -> Option<String> {
    let mut command = Command::new(executable);
    hidden(&mut command);
    let output = command.arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    (!version.is_empty()).then_some(version)
}

/// Which of `names` run on this computer: `{"state": "ready" | "unproven", "tools": [{name,
/// executable, version}]}`. `unproven` means none could be run.
pub fn toolchains(names: &[&str]) -> Value {
    let tools: Vec<Value> = names
        .iter()
        .filter_map(|name| {
            let executable = command_path(name)?;
            let version = tool_version(&executable)?;
            Some(json!({ "name": name, "executable": executable, "version": version }))
        })
        .collect();
    if tools.is_empty() {
        return json!({ "state": "unproven", "tools": [] });
    }
    json!({ "state": "ready", "tools": tools })
}

/// Whether `dir` can be created and written: a probe file is written and removed.
pub fn dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Whether `dir` could be created: the nearest existing ancestor of `dir` gets a file named
/// `<tag>-<pid>` that is removed immediately. Creates nothing that persists.
pub fn parent_writable(dir: &Path, tag: &str) -> Result<(), String> {
    let mut probe_dir = dir.parent();
    while let Some(p) = probe_dir {
        if p.is_dir() {
            let probe = p.join(format!("{tag}-{}", std::process::id()));
            let _ = std::fs::remove_file(&probe);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)
                .map_err(|e| e.to_string())?;
            return std::fs::remove_file(&probe).map_err(|e| e.to_string());
        }
        probe_dir = p.parent();
    }
    Err("no existing ancestor directory".into())
}

/// How a bounded child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bounded {
    /// Exited successfully after `ms` milliseconds.
    Succeeded {
        /// Operating-system process id.
        pid: u32,
        /// Wall-clock milliseconds.
        ms: u128,
    },
    /// Exited with a failure status (`None` when killed by a signal).
    Failed {
        /// Operating-system process id.
        pid: u32,
        /// Exit code.
        code: Option<i32>,
    },
    /// Still running at the deadline; it was killed and reaped.
    TimedOut {
        /// Operating-system process id.
        pid: u32,
    },
}

/// Spawn `program args...` with null stdio, wait at most `timeout`, and kill and reap it on
/// timeout. `Err` is a spawn or wait failure.
pub fn run_bounded(program: &Path, args: &[&str], timeout: Duration) -> Result<Bounded, String> {
    let started = Instant::now();
    let mut command = Command::new(program);
    hidden(&mut command);
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;
    let pid = child.id();
    loop {
        match child
            .try_wait()
            .map_err(|e| format!("wait child {pid}: {e}"))?
        {
            Some(status) if status.success() => {
                return Ok(Bounded::Succeeded {
                    pid,
                    ms: started.elapsed().as_millis(),
                })
            }
            Some(status) => {
                return Ok(Bounded::Failed {
                    pid,
                    code: status.code(),
                })
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Bounded::TimedOut { pid });
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Whether a TCP listener can bind `addr` (a loopback `:0` proves the machine can serve at all;
/// a fixed port proves it is free). The listener is dropped at once.
pub fn can_bind(addr: SocketAddr) -> bool {
    TcpListener::bind(addr).is_ok()
}

/// The clipboard tools to try, in order, for this platform: `(program, args)`.
pub fn clipboard_candidates() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip.exe", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// Best effort: pipe `text` to the first clipboard tool that accepts it and return its name.
/// `Err` carries the last failure; callers note it on stderr and keep their exit code.
pub fn copy_to_clipboard(text: &str) -> Result<String, String> {
    copy_with(clipboard_candidates(), text)
}

fn copy_with(candidates: &[(&str, &[&str])], text: &str) -> Result<String, String> {
    let mut last = String::from("no clipboard candidates configured for this target");
    for (bin, args) in candidates {
        let mut command = Command::new(bin);
        hidden(&mut command);
        let mut child = match command
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                last = format!("{bin}: spawn error: {e}");
                continue;
            }
        };
        if let Some(stdin) = child.stdin.as_mut() {
            // The tool may exit before the write finishes (wl-copy without a compositor).
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(status) if status.success() => return Ok((*bin).to_string()),
            Ok(status) => last = format!("{bin}: exited with {status}"),
            Err(e) => last = format!("{bin}: wait error: {e}"),
        }
    }
    Err(last)
}
