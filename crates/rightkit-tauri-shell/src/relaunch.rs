//! PID-ordered relaunch of the running app (update restart and ordinary restart).
//!
//! `tauri::process::restart` spawns the new binary BEFORE the old one exits, so for a moment both
//! processes are alive. The new one runs the single-instance setup, notifies the still-running old
//! instance, concludes another instance owns the app and exits; the old process then exits too and
//! nothing is left running. This module avoids that race: a private instance of this executable
//! waits (bounded) for the old PID to exit and only then launches the replacement. The old process
//! releases its single-instance claim before it exits, so even a helper that wins the race early
//! finds no owner.
//!
//! macOS bundles are launched through LaunchServices (`/usr/bin/open -a`); development executables
//! and Windows launch directly. Process launch goes through `rightkit-child-process`; no shell, no
//! caller-supplied program or argv.
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// Host-supplied settings of the private restart protocol. Both the process that asks for a
/// restart and the helper it spawns must be built with the same `flag_prefix`.
#[derive(Clone, Debug)]
pub struct Protocol {
    /// Reserved argv prefix of the helper invocation, for example `--<app>-update-relaunch-helper`.
    /// The helper flag is `<flag_prefix>-v1`. Every invocation whose argv contains an argument that
    /// starts with this prefix is consumed by [`dispatch`].
    flag_prefix: String,
    helper_arg: String,
    /// How long the helper waits for the old process to exit before it launches anyway.
    parent_wait: Duration,
    event_sink: Option<fn(&str, &str)>,
}

impl Protocol {
    /// Default parent wait is ten seconds.
    pub fn new(flag_prefix: &str) -> Self {
        Self {
            flag_prefix: flag_prefix.to_owned(),
            helper_arg: format!("{flag_prefix}-v1"),
            parent_wait: Duration::from_secs(10),
            event_sink: None,
        }
    }

    pub fn parent_wait(mut self, wait: Duration) -> Self {
        self.parent_wait = wait;
        self
    }

    /// Receives `(event, detail)` diagnostics from [`relaunch_self`]. Without one they are dropped.
    pub fn event_sink(mut self, sink: fn(&str, &str)) -> Self {
        self.event_sink = Some(sink);
        self
    }

    fn event(&self, event: &str, detail: &str) {
        if let Some(sink) = self.event_sink {
            sink(event, detail);
        }
    }
}

pub fn spawn(protocol: &Protocol) -> io::Result<()> {
    if !cfg!(any(target_os = "macos", windows)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "update relaunch requires macOS or Windows",
        ));
    }
    let executable = std::env::current_exe()?;
    let mut command = rightkit_child_process::OwnedCommand::new(&executable);
    command
        .command_mut()
        .arg(&protocol.helper_arg)
        .arg(std::process::id().to_string());
    command.spawn_uncontained_detached_strict()?;
    Ok(())
}

/// None means ordinary startup. Every reserved-protocol invocation exits here,
/// including malformed invocations; none may reach Tauri or single-instance setup.
/// Call it first in `main`, before any app state is read.
pub fn dispatch(protocol: &Protocol) -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if !args.iter().any(|arg| {
        arg.to_string_lossy()
            .starts_with(protocol.flag_prefix.as_str())
    }) {
        return None;
    }
    let pid = match args.as_slice() {
        [flag, pid] if flag == protocol.helper_arg.as_str() => pid.to_str().and_then(|text| {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            text.parse::<u32>()
                .ok()
                .filter(|pid| *pid != 0 && *pid != std::process::id())
        }),
        _ => None,
    };
    let Some(pid) = pid else {
        eprintln!("invalid update relaunch helper arguments");
        return Some(64);
    };
    Some(match relaunch(protocol, pid) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("update relaunch helper failed: {error}");
            1
        }
    })
}

fn relaunch(protocol: &Protocol, pid: u32) -> io::Result<()> {
    // No caller-controlled program, argv, bundle, or shell fragment is accepted.
    let executable = std::env::current_exe()?;
    wait_for_parent(pid, protocol.parent_wait)?;
    let command = launch_command(executable);
    command.spawn_uncontained_detached_strict()?;
    Ok(())
}

/// Spawn the helper, release the single-instance claim and exit this process: the shared sequence
/// for an update relaunch, a retry from a startup-error screen, or any ordinary restart.
///
/// `marker` is an optional restart marker: a file written (atomically, parent directories created)
/// BEFORE the helper spawns, so the incoming process can tell this restart is the host's own and
/// consume it with [`crate::launch_origin::consume_marker`]. Best-effort: a marker that cannot be
/// written costs one slow launch, never a broken one. It is removed again if the spawn fails.
/// `reason` is written to the event sink.
pub fn relaunch_self(
    app: &tauri::AppHandle,
    protocol: &Protocol,
    reason: &str,
    marker: Option<&Path>,
) -> Result<(), String> {
    if let Some(marker) = marker {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rightkit_fs::atomic::atomic_write(marker, b"1");
    }
    let spawned = spawn(protocol).map_err(|err| format!("spawn relaunch helper: {err}"));
    if spawned.is_err() {
        if let Some(marker) = marker {
            let _ = std::fs::remove_file(marker);
        }
    }
    spawned?;
    protocol.event("relaunch_self", &format!("reason={reason} helper=spawned"));
    // Release the single-instance socket explicitly before exiting, so even a
    // helper that wins the race early finds no owner.
    crate::single_instance::SingleInstance::destroy();
    app.exit(0);
    Ok(())
}

fn launch_command(executable: PathBuf) -> rightkit_child_process::OwnedCommand {
    #[cfg(target_os = "macos")]
    {
        let bundle = executable
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))
            .and_then(|dir| dir.parent())
            .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
            .and_then(|dir| dir.parent())
            .filter(|dir| {
                dir.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            });
        if let Some(bundle) = bundle {
            let mut command = rightkit_child_process::OwnedCommand::new("/usr/bin/open");
            command.command_mut().arg("-a").arg(bundle);
            return command;
        }
    }
    rightkit_child_process::OwnedCommand::new(executable)
}

#[cfg(target_os = "macos")]
fn wait_for_parent(pid: u32, wait: Duration) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid parent PID"))?;
    let started = std::time::Instant::now();
    loop {
        // Signal 0 checks existence without delivering any signal. Positive PID only.
        if unsafe { libc::kill(pid, 0) } != 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => return Ok(()),
                Some(libc::EPERM) => {}
                _ => return Err(error),
            }
        }
        let remaining = wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100).min(remaining));
    }
    // Launch after the bound even if the PID persists.
    Ok(())
}

#[cfg(windows)]
fn wait_for_parent(pid: u32, wait: Duration) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    // A retained process handle avoids PID reuse during this bounded wait.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(()) // Parent already exited before helper acquired its handle.
        } else {
            Err(error)
        };
    }
    // DWORD::MAX is INFINITE, not a finite timeout. Reserve that sentinel.
    let millis = wait.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    let result = unsafe { WaitForSingleObject(handle, millis) };
    let error = io::Error::last_os_error();
    unsafe { CloseHandle(handle) };
    match result {
        WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
        _ => Err(error),
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn wait_for_parent(_pid: u32, _wait: Duration) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "update relaunch requires macOS or Windows",
    ))
}
