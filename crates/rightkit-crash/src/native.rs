//! Best-effort native capture next to the structured report. Local-only:
//! native artifacts can contain memory, so they are never queued for upload and
//! enter a support bundle only when the user opts in explicitly.
//!
//! * macOS: `/usr/bin/sample <pid>` of the crashing process (HeardRight donor),
//!   with the text output scrubbed of home directories. `sample` writes to a hidden raw file
//!   (see [`RAW_PREFIX`]) that no reader of the crash folder lists; only the scrubbed bytes are
//!   published under the final `sample_*.txt` name, and a failed scrub publishes nothing.
//! * Windows: out-of-process minidump through the dump helper (default
//!   `rightkit-dump-helper.exe` next to the app executable; see [`set_dump_helper`]),
//!   so DbgHelp never runs inside the crashing process (HeardRight donor).
//! * elsewhere: no native capture.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

/// Name prefix of the unscrubbed file `sample` writes to. It starts with a dot and not with
/// `sample_`, `dump_` or `crash_`, so the support bundle, [`crate::prune`]'s listings and
/// [`crate::list_pending`] never match it.
pub(crate) const RAW_PREFIX: &str = ".sample_raw_";
/// Name suffix of the raw file.
pub(crate) const RAW_SUFFIX: &str = ".raw";
/// A raw file is never needed after `capture` returns, so a sweep removes any older than this.
pub(crate) const RAW_MAX_AGE: Duration = Duration::from_secs(300);

/// Whether `name` is an unscrubbed raw sample (never bundle-eligible).
pub(crate) fn is_raw_name(name: &str) -> bool {
    name.starts_with(RAW_PREFIX) && name.ends_with(RAW_SUFFIX)
}

/// Result of one native capture attempt: the kept artifact, if any, and a fixed-vocabulary note
/// when something went wrong (no paths, no user content; a leftover raw file is named by file
/// name only).
#[derive(Debug, Default)]
pub(crate) struct NativeOutcome {
    pub artifact: Option<PathBuf>,
    pub note: Option<String>,
}

impl NativeOutcome {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn nothing() -> Self {
        Self::default()
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn from_option(artifact: Option<PathBuf>) -> Self {
        Self {
            artifact,
            note: None,
        }
    }
}

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
/// `None` when the platform has no capture, the helper is missing, a capture is in flight, or
/// (macOS) the sample could not be scrubbed and published: an unscrubbed sample is never kept,
/// and the reason is written to stderr as one `rightkit-crash:` line. The artifact is
/// local-only, like the panic hook's.
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
    capture_detailed(dir, kind, stamp).artifact
}

/// [`capture`] plus the note the panic hook records on the report. Also writes the note to
/// stderr (best effort: a closed stderr must not panic inside a panic hook).
pub(crate) fn capture_detailed(dir: &Path, kind: &str, stamp: u64) -> NativeOutcome {
    #[cfg(target_os = "macos")]
    let outcome = platform::capture(dir, kind, stamp);
    #[cfg(not(target_os = "macos"))]
    let outcome = NativeOutcome::from_option(platform::capture(dir, kind, stamp));
    if let Some(note) = &outcome.note {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("rightkit-crash: native capture: {note}\n"),
        );
    }
    outcome
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{NativeOutcome, RAW_PREFIX, RAW_SUFFIX};
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;
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

    pub(super) fn capture(dir: &Path, kind: &str, stamp: u64) -> NativeOutcome {
        if IN_FLIGHT.swap(true, Ordering::AcqRel) {
            return NativeOutcome::nothing();
        }
        let _guard = Guard;
        crate::retention::sweep_raw(dir);
        let pid = std::process::id();
        let final_path = dir.join(format!("sample_{kind}_{stamp}_{pid}.txt"));
        let raw = dir.join(format!("{RAW_PREFIX}{kind}_{stamp}_{pid}{RAW_SUFFIX}"));
        // `sample` writes the unscrubbed text here, owner-only from the first byte. The name is
        // invisible to the bundle, retention and the pending list.
        if create_raw(&raw).is_err() {
            return NativeOutcome {
                artifact: None,
                note: Some("sample_raw_create_failed".into()),
            };
        }
        let spawned = Command::new("/usr/bin/sample")
            .arg(pid.to_string())
            .arg(SAMPLE_SECS)
            .arg("-file")
            .arg(&raw)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(_) => return discard(&raw),
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let bytes = std::fs::metadata(&raw).map(|m| m.len()).unwrap_or(0);
                    if status.success() && bytes > 0 {
                        // Belt and braces: whatever mode `sample` left, make it owner-only now.
                        let _ =
                            std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o600));
                        return finish(&raw, &final_path);
                    }
                    return discard(&raw);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return discard(&raw);
                }
            }
        }
    }

    /// Creates the raw file empty and owner-only (`0600`), never following an existing name.
    fn create_raw(raw: &Path) -> std::io::Result<()> {
        let _ = std::fs::remove_file(raw);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(raw)
            .map(drop)
    }

    /// Removes the raw file. `Some(note)` names it when it could not be removed.
    fn remove_raw(raw: &Path) -> Option<String> {
        match std::fs::remove_file(raw) {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(format!(
                "sample_raw_not_removed {}",
                raw.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }

    /// `sample` did not produce a usable file: remove whatever it left, keep nothing.
    fn discard(raw: &Path) -> NativeOutcome {
        NativeOutcome {
            artifact: None,
            note: remove_raw(raw),
        }
    }

    /// Publish-or-nothing. The scrubbed bytes appear under `final_path` only when the raw file was
    /// read, scrubbed and atomically published. The raw file is removed on every path. On any
    /// failure the outcome has no artifact and says why; a raw file that cannot be removed is
    /// named in the note (its name is not bundle-eligible).
    pub(super) fn finish(raw: &Path, final_path: &Path) -> NativeOutcome {
        let published = publish_scrubbed(raw, final_path);
        let leftover = remove_raw(raw);
        match published {
            Ok(()) => NativeOutcome {
                artifact: Some(final_path.to_path_buf()),
                note: leftover,
            },
            Err(code) => NativeOutcome {
                artifact: None,
                note: Some(match leftover {
                    Some(left) => format!("{code}; {left}"),
                    None => code.to_string(),
                }),
            },
        }
    }

    pub(super) fn publish_scrubbed(raw: &Path, final_path: &Path) -> Result<(), &'static str> {
        let bytes = std::fs::read(raw).map_err(|_| "sample_read_failed")?;
        let text = String::from_utf8_lossy(&bytes);
        // Same owner-only, atomic publish as the panic report. Runs after the sampler exits,
        // in ordinary code, never in a signal handler.
        let scrubbed = rightkit_logs::scrub_text(&text);
        let options = rightkit_fs::AtomicWriteOptions {
            private: true,
            ..rightkit_fs::AtomicWriteOptions::default()
        };
        rightkit_fs::atomic_write_with_options(final_path, options, |file| {
            file.write_all(scrubbed.as_bytes())
        })
        .map_err(|_| "sample_publish_failed")
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::platform::{finish, publish_scrubbed};
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SAMPLE_TEXT: &str =
        "Call graph:\n  1 main  (in app) + 4  [/Users/adalovelace/Documents/private/app]\n";

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-crash-native-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Names in `dir` the support bundle would copy or retention would count.
    fn bundle_eligible(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("sample_") || n.starts_with("dump_"))
            .collect()
    }

    fn raw_path(dir: &Path) -> PathBuf {
        dir.join(format!("{RAW_PREFIX}panic_1_2{RAW_SUFFIX}"))
    }

    #[test]
    fn the_raw_name_is_not_bundle_eligible() {
        let name = raw_path(Path::new("x"))
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(is_raw_name(&name));
        assert!(
            !name.starts_with("sample_")
                && !name.starts_with("dump_")
                && !name.starts_with("crash_")
        );
        assert!(!is_raw_name("sample_panic_1_2.txt"));
    }

    #[test]
    fn a_good_sample_is_published_scrubbed_owner_only_and_the_raw_file_is_gone() {
        let dir = temp_dir("ok");
        let raw = raw_path(&dir);
        std::fs::write(&raw, SAMPLE_TEXT).unwrap();
        let final_path = dir.join("sample_panic_1_2.txt");
        let outcome = finish(&raw, &final_path);
        assert_eq!(outcome.artifact.as_deref(), Some(final_path.as_path()));
        assert!(outcome.note.is_none(), "{:?}", outcome.note);
        let text = std::fs::read_to_string(&final_path).unwrap();
        assert!(!text.contains("adalovelace"), "{text}");
        assert!(text.contains("Call graph"));
        let mode = std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!raw.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_raw_file_publishes_nothing_and_reports_the_failure() {
        let dir = temp_dir("read");
        let raw = raw_path(&dir); // never created: the read fails
        let final_path = dir.join("sample_panic_1_2.txt");
        let outcome = finish(&raw, &final_path);
        assert!(outcome.artifact.is_none());
        assert_eq!(outcome.note.as_deref(), Some("sample_read_failed"));
        assert!(bundle_eligible(&dir).is_empty());
        assert!(!raw.exists());

        // A directory in the raw slot: the read fails (publish_scrubbed alone), and the failed
        // removal is named by file name only, under a name the bundle ignores.
        std::fs::create_dir(&raw).unwrap();
        assert_eq!(
            publish_scrubbed(&raw, &final_path),
            Err("sample_read_failed")
        );
        let outcome = finish(&raw, &final_path);
        assert!(outcome.artifact.is_none());
        let note = outcome.note.unwrap();
        assert!(
            note.starts_with("sample_read_failed; sample_raw_not_removed .sample_raw_"),
            "{note}"
        );
        assert!(!note.contains(dir.to_str().unwrap()), "{note}");
        assert!(bundle_eligible(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_publish_returns_nothing_and_removes_the_raw_file() {
        let dir = temp_dir("publish");
        let raw = raw_path(&dir);
        std::fs::write(&raw, SAMPLE_TEXT).unwrap();
        // The final path's parent is a regular file, so the atomic publish cannot succeed
        // (this holds for root as well, unlike a read-only directory).
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let final_path = blocker.join("sample_panic_1_2.txt");
        let outcome = finish(&raw, &final_path);
        assert!(outcome.artifact.is_none());
        assert_eq!(outcome.note.as_deref(), Some("sample_publish_failed"));
        assert!(!raw.exists(), "raw file must be deleted on failure");
        assert!(bundle_eligible(&dir).is_empty());
        assert!(!final_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
