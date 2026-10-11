//! Best-effort native capture next to the structured report. Local-only:
//! native artifacts can contain memory, so they are never queued for upload and
//! enter a support bundle only when the user opts in explicitly.
//!
//! * macOS: `/usr/bin/sample <pid>` of the crashing process (HeardRight donor),
//!   with the text output scrubbed of home directories.
//! * Windows: out-of-process minidump through the dump helper (default
//!   `rightkit-dump-helper.exe` next to the app executable; see [`set_dump_helper`]),
//!   so DbgHelp never runs inside the crashing process (HeardRight donor).
//! * elsewhere: no native capture.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The Windows helper looked up next to the app executable unless [`set_dump_helper`] says otherwise.
pub const DEFAULT_DUMP_HELPER: &str = "rightkit-dump-helper.exe";

static DUMP_HELPER: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Selects the Windows dump helper. A bare file name (for example `heardright-dump-helper.exe`)
/// is resolved next to the app executable; an absolute path is used as given. Ignored elsewhere.
pub fn set_dump_helper(helper: impl Into<PathBuf>) {
    if let Ok(mut slot) = DUMP_HELPER.write() {
        *slot = Some(helper.into());
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn dump_helper_path() -> Option<PathBuf> {
    let configured = DUMP_HELPER.read().ok().and_then(|slot| slot.clone());
    let helper = configured.unwrap_or_else(|| PathBuf::from(DEFAULT_DUMP_HELPER));
    if helper.is_absolute() {
        return Some(helper);
    }
    Some(std::env::current_exe().ok()?.parent()?.join(helper))
}

/// Captures a native sample (macOS) or minidump (Windows) of this process into `dir` now, for
/// hang, UI-stall or sidecar-crash paths that are not panics. Returns the artifact path, or
/// `None` when the platform has no capture, the helper is missing, or a capture is in flight.
/// The artifact is local-only, like the panic hook's.
pub fn capture_native(dir: &Path, kind: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    capture(dir, &crate::report::sanitize_kind(kind), stamp)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeCapture {
    /// Structured report only (default; fastest, nothing memory-bearing).
    #[default]
    Off,
    /// Also capture a native sample/minidump when the platform supports it.
    Auto,
}

pub(crate) fn capture(dir: &Path, kind: &str, stamp: u64) -> Option<PathBuf> {
    platform::capture(dir, kind, stamp)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    const SAMPLE_SECS: &str = "1";
    const TIMEOUT: Duration = Duration::from_secs(6);

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            IN_FLIGHT.store(false, Ordering::Release);
        }
    }

    pub(super) fn capture(dir: &Path, kind: &str, stamp: u64) -> Option<PathBuf> {
        if IN_FLIGHT.swap(true, Ordering::AcqRel) {
            return None;
        }
        let _guard = Guard;
        let pid = std::process::id();
        let path = dir.join(format!("sample_{kind}_{stamp}_{pid}.txt"));
        let mut child = Command::new("/usr/bin/sample")
            .arg(pid.to_string())
            .arg(SAMPLE_SECS)
            .arg("-file")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    if status.success() && bytes > 0 {
                        scrub_in_place(&path);
                        return Some(path);
                    }
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
            }
        }
    }

    fn scrub_in_place(path: &Path) {
        if let Ok(bytes) = std::fs::read(path) {
            let text = String::from_utf8_lossy(&bytes);
            // Same owner-only, atomic publish as the panic report. Runs after the sampler exits,
            // in ordinary code, never in a signal handler.
            let scrubbed = rightkit_logs::scrub_text(&text);
            let options = rightkit_fs::AtomicWriteOptions {
                private: true,
                ..rightkit_fs::AtomicWriteOptions::default()
            };
            let _ = rightkit_fs::atomic_write_with_options(path, options, |file| {
                std::io::Write::write_all(file, scrubbed.as_bytes())
            });
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TIMEOUT: Duration = Duration::from_secs(12);

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            IN_FLIGHT.store(false, Ordering::Release);
        }
    }

    pub(super) fn capture(dir: &Path, kind: &str, stamp: u64) -> Option<PathBuf> {
        if IN_FLIGHT.swap(true, Ordering::AcqRel) {
            return None;
        }
        let _guard = Guard;
        let helper = super::dump_helper_path()?;
        if !helper.is_file() {
            return None;
        }
        let pid = std::process::id();
        let final_path = dir.join(format!("dump_{kind}_{stamp}_{pid}.dmp"));
        let partial = dir.join(format!(".dump_{kind}_{stamp}_{pid}.dmp.partial"));
        let _ = std::fs::remove_file(&partial);
        let mut child = Command::new(&helper)
            .arg("--pid")
            .arg(pid.to_string())
            .arg("--output")
            .arg(&partial)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let bytes = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
                    if status.success()
                        && bytes > 0
                        && std::fs::rename(&partial, &final_path).is_ok()
                    {
                        return Some(final_path);
                    }
                    let _ = std::fs::remove_file(&partial);
                    return None;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&partial);
                    return None;
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use std::path::{Path, PathBuf};

    pub(super) fn capture(_dir: &Path, _kind: &str, _stamp: u64) -> Option<PathBuf> {
        None
    }
}
