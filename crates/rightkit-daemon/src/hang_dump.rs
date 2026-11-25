//! Stack samples of a hung child, kept private and bounded (Pulse #468).
//!
//! A supervisor that is about to stop an unhealthy child calls [`capture_sample`] first, in its
//! own callback and before the child is reaped, so the pid cannot have been reused. macOS only
//! (`/usr/bin/sample`, one second, bounded): Windows and other hosts capture nothing, because a
//! `comsvcs.dll` MiniDump is the pattern antivirus flags as credential dumping and a full dump
//! writes the child's memory to disk.
//!
//! A sample can contain file paths, process metadata and symbol names, so it is never logged or
//! committed: the folder is owned by this user, a real directory (never a symlink), mode 0700
//! (`private_dir`); the file is created 0600 without following a symlink and filled in place by
//! `sample`; a failure to make either private fails the capture. The capture checks that the pid is
//! alive and that its executable is the one the supervisor launched, and is abandoned otherwise. A
//! failed or timed-out sample is killed and its file deleted.
//!
//! Retention ([`prune`]): the newest `kept` files, none older than `max_age`; a host applies it
//! when the supervisor starts, on a timer, and ([`capture_sample`] does) at the start and end of
//! each capture, so the age limit holds with no new capture.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

/// Keeps the newest `kept` plain files in `dir` and deletes the rest and any older than
/// `max_age`. A missing folder is not an error.
pub fn prune(dir: &Path, kept: usize, max_age: Duration) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut files: Vec<_> = read
        .flatten()
        .filter_map(|e| {
            let m = std::fs::symlink_metadata(e.path()).ok()?;
            m.is_file().then_some((m.modified().ok()?, e.path()))
        })
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (n, (modified, path)) in files.into_iter().enumerate() {
        let old = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if n >= kept || old {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `parent/name` as a private folder: created 0700 when absent, and proved to be a plain
/// directory (never a symlink) owned by this user with mode 0700 (made 0700 when it is not).
/// Any failure to make or prove that is an error.
#[cfg(unix)]
pub fn private_dir(parent: &Path, name: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { geteuid() };
    let dir = parent.join(name);
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| format!("create {name}: {e}"))?;
        }
        Err(e) => return Err(format!("stat {name}: {e}")),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("stat {name}: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{name} is not a plain directory"));
    }
    if meta.uid() != euid {
        return Err(format!("{name} is not owned by this user"));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod {name}: {e}"))?;
        let again = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if again.permissions().mode() & 0o777 != 0o700 {
            return Err(format!("{name} mode is not 0700"));
        }
    }
    Ok(dir)
}

/// What to keep and how long a capture may take.
#[derive(Clone, Copy, Debug)]
pub struct CaptureOptions {
    /// The longest the `sample` run may take.
    pub bound: Duration,
    /// Files kept in the folder ([`prune`]).
    pub kept: usize,
    /// Files older than this are deleted ([`prune`]).
    pub max_age: Duration,
}

/// Captures a one-second stack sample of process `pid` into `dir` (from `private_dir`), named
/// `<prefix>-<pid>-<millis>.sample.txt`; the file name on success. `expected_exe` is the executable
/// the child was launched from: a pid running anything else is refused. Contents are never read
/// or logged.
#[cfg(target_os = "macos")]
pub fn capture_sample(
    pid: u32,
    expected_exe: &Path,
    dir: &Path,
    prefix: &str,
    options: CaptureOptions,
) -> Result<String, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::time::Instant;
    const O_NOFOLLOW: i32 = 0x0100;
    extern "C" {
        fn geteuid() -> u32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffersize: u32) -> i32;
    }
    // Identity: still there, and running the executable the supervisor launched. The supervisor
    // has not reaped the child (the stop comes after this call), so the pid is not reused.
    let ipid = i32::try_from(pid).map_err(|_| "pid out of range".to_string())?;
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { kill(ipid, 0) } != 0 {
        return Err("child already gone".into());
    }
    let mut buffer = [0u8; 4096];
    // SAFETY: the buffer is 4096 writable bytes and its size is passed.
    let n = unsafe { proc_pidpath(ipid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if n <= 0 {
        return Err("child path unreadable".into());
    }
    let running = PathBuf::from(String::from_utf8_lossy(&buffer[..n as usize]).into_owned());
    let same = match (running.canonicalize(), expected_exe.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => running == expected_exe,
    };
    if !same {
        return Err("pid is not the child executable".into());
    }
    prune(dir, options.kept, options.max_age);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!("{prefix}-{pid}-{stamp}.sample.txt");
    let path = dir.join(&name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("create dump file: {e}"))?;
    let result = (|| {
        let mut tool = std::process::Command::new("/usr/bin/sample")
            .arg(pid.to_string())
            .arg("1")
            .arg("-file")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let until = Instant::now() + options.bound;
        loop {
            match tool.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => return Err(format!("dump tool exited {status}")),
                Ok(None) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Ok(None) => {
                    let _ = tool.kill();
                    let _ = tool.wait();
                    return Err(format!("dump tool over {} s", options.bound.as_secs()));
                }
                Err(e) => {
                    let _ = tool.kill();
                    let _ = tool.wait();
                    return Err(e.to_string());
                }
            }
        }
        // The file sample wrote must still be our own plain file, private, and not empty.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        // SAFETY: geteuid takes no arguments and cannot fail.
        if !meta.is_file() || meta.uid() != unsafe { geteuid() } {
            return Err("dump file is not our plain file".into());
        }
        if meta.permissions().mode() & 0o777 != 0o600 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("chmod dump file: {e}"))?;
            let again = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if again.permissions().mode() & 0o777 != 0o600 {
                return Err("dump file mode is not 0600".into());
            }
        }
        if meta.len() == 0 {
            return Err("dump tool wrote nothing".into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            prune(dir, options.kept, options.max_age);
            Ok(name)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
}

/// Windows and other hosts capture nothing.
#[cfg(not(target_os = "macos"))]
pub fn capture_sample(
    _pid: u32,
    _expected_exe: &Path,
    _dir: &Path,
    _prefix: &str,
    _options: CaptureOptions,
) -> Result<String, String> {
    Err("no dump on this platform".into())
}
