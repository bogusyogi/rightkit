//! Best-effort native capture next to the structured report. Local-only:
//! native artifacts can contain memory, so they are never queued for upload and
//! enter a support bundle only when the user opts in explicitly.
//!
//! * macOS: `/usr/bin/sample <pid>` of the crashing process (HeardRight donor),
//!   with the text output scrubbed. `sample` writes to a private `0700` directory in the OS temp
//!   directory, never inside the crash folder; only the scrubbed bytes are published under the
//!   final `sample_*.txt` name, and a failed scrub publishes the short marker
//!   `sample withheld: scrub failed` instead. The raw directory is deleted on every path.
//! * Windows: out-of-process minidump through the dump helper (default
//!   `rightkit-dump-helper.exe` next to the app executable; see [`set_dump_helper`]),
//!   so DbgHelp never runs inside the crashing process (HeardRight donor).
//! * elsewhere: no native capture.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

/// Name prefix of the unscrubbed file releases up to 0.3.0 wrote inside the crash folder. Nothing
/// writes it any more; a sweep still removes leftovers. It starts with a dot and not with
/// `sample_`, `dump_` or `crash_`, so the support bundle and listings never matched it.
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
/// (macOS) the sample could not be published. An unscrubbed sample is never written to `dir`: a
/// sample that cannot be scrubbed is replaced by a short withheld marker, and the reason is
/// written to stderr as one `rightkit-crash:` line. The artifact is
/// local-only, like the panic hook's.
pub fn capture_native(dir: &Path, kind: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    capture(dir, &crate::report::sanitize_kind(kind), stamp)
}

/// Native capture of another process, `pid`, named by the caller (a supervised helper that hangs
/// or misbehaves while still alive). Same platform tools and the same scrub-before-publish path
/// as [`capture_native`]; `secrets` are removed from the macOS sample text.
pub(crate) fn capture_pid_detailed(
    dir: &Path,
    kind: &str,
    stamp: u64,
    pid: u32,
    secrets: &[String],
) -> NativeOutcome {
    capture_detailed_for(dir, kind, stamp, pid, secrets)
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
    capture_detailed(dir, kind, stamp, &[]).artifact
}

/// [`capture`] plus the note the panic hook records on the report. Also writes the note to
/// stderr (best effort: a closed stderr must not panic inside a panic hook).
pub(crate) fn capture_detailed(
    dir: &Path,
    kind: &str,
    stamp: u64,
    secrets: &[String],
) -> NativeOutcome {
    capture_detailed_for(dir, kind, stamp, std::process::id(), secrets)
}

fn capture_detailed_for(
    dir: &Path,
    kind: &str,
    stamp: u64,
    pid: u32,
    secrets: &[String],
) -> NativeOutcome {
    #[cfg(target_os = "macos")]
    let outcome = platform::capture(dir, kind, stamp, pid, secrets);
    #[cfg(not(target_os = "macos"))]
    let outcome = NativeOutcome::from_option(platform::capture(dir, kind, stamp, pid, secrets));
    if let Some(note) = &outcome.note {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("rightkit-crash: native capture: {note}\n"),
        );
    }
    outcome
}

/// Scrub-before-publish core of the macOS sample path. Not platform-gated, so the scrubbing,
/// marker and cleanup behaviour is tested on every host; only macOS calls it outside tests.
///
/// Rule: no unscrubbed byte is ever written to the crash folder (the folder a support bundle is
/// built from). Raw `sample` output goes to a private `0700` directory in the OS temp directory,
/// outside the crash folder, and that directory is deleted whatever happens. The crash folder
/// receives only scrubbed text, or a short marker when scrubbing failed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod sample {
    use super::{NativeOutcome, RAW_MAX_AGE};
    use std::io::Write;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Content of the published file when the sample could not be scrubbed.
    pub(crate) const WITHHELD_MARKER: &str = "sample withheld: scrub failed\n";
    /// Name prefix of the private raw directory in the OS temp directory.
    pub(crate) const TEMP_RAW_PREFIX: &str = "rightkit-crash-raw-";
    const RAW_FILE: &str = "sample.raw";

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// The production scrubber: known secrets first, then the pattern scrub.
    pub(crate) fn default_scrub(secrets: &[String]) -> impl Fn(&str) -> String + '_ {
        move |text| rightkit_logs::scrub_text(&crate::report::redact_known(text, secrets))
    }

    /// Scrubs `bytes`. A panic in the scrubber, or a known secret still present in the result,
    /// is a failure: the caller must then publish [`WITHHELD_MARKER`], never the input.
    pub(crate) fn scrub_checked(
        bytes: &[u8],
        secrets: &[String],
        scrub: &dyn Fn(&str) -> String,
    ) -> Result<String, &'static str> {
        let text = String::from_utf8_lossy(bytes);
        let out = catch_unwind(AssertUnwindSafe(|| scrub(&text)))
            .map_err(|_| "sample_scrub_failed")?;
        if secrets.iter().any(|s| !s.is_empty() && out.contains(s.as_str())) {
            return Err("sample_scrub_failed");
        }
        Ok(out)
    }

    /// Scrubbed text for a native sample read from the crash folder at bundle time, or the marker.
    /// Defence in depth for a `sample_*` file an older release left unscrubbed.
    pub(crate) fn scrub_for_bundle(bytes: &[u8]) -> String {
        scrub_checked(bytes, &[], &default_scrub(&[])).unwrap_or_else(|_| WITHHELD_MARKER.into())
    }

    /// Creates a fresh private directory under `base` (never inside `crash_dir`) holding an empty
    /// owner-only raw file. Returns `(directory, raw file)`.
    pub(crate) fn create_raw_dir(base: &Path, crash_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!(
            "{TEMP_RAW_PREFIX}{}-{nanos}-{n}",
            std::process::id()
        ));
        let inside = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a.starts_with(b),
            _ => a.starts_with(b),
        };
        if inside(base, crash_dir) {
            return Err(std::io::Error::other("raw directory would be inside the crash folder"));
        }
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        builder.create(&dir)?;
        let raw = dir.join(RAW_FILE);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        if let Err(e) = options.open(&raw) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok((dir, raw))
    }

    /// Deletes the raw directory. `Some(note)` names it (file name only) when it survived.
    pub(crate) fn remove_raw_dir(dir: &Path) -> Option<String> {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(format!(
                "sample_raw_not_removed {}",
                dir.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }

    /// Removes stale raw directories (older than [`RAW_MAX_AGE`]) a crashed capture left in `base`.
    pub(crate) fn sweep_temp_raw_in(base: &Path) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(TEMP_RAW_PREFIX) {
                continue;
            }
            let stale = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > RAW_MAX_AGE);
            if stale {
                let path = entry.path();
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    }

    /// Scrubs the raw file and publishes the result under `final_path`; deletes `raw_dir` on every
    /// path. A read or scrub failure publishes [`WITHHELD_MARKER`] instead of any raw text and
    /// says why in the note. Only when even the marker cannot be written is there no artifact.
    pub(crate) fn finish(
        raw_dir: &Path,
        raw: &Path,
        final_path: &Path,
        secrets: &[String],
        scrub: &dyn Fn(&str) -> String,
    ) -> NativeOutcome {
        let (content, failure) = match std::fs::read(raw) {
            Err(_) => (WITHHELD_MARKER.to_string(), Some("sample_read_failed")),
            Ok(bytes) => match scrub_checked(&bytes, secrets, scrub) {
                Ok(text) => (text, None),
                Err(code) => (WITHHELD_MARKER.to_string(), Some(code)),
            },
        };
        let published = write_private(final_path, &content);
        let leftover = remove_raw_dir(raw_dir);
        let (artifact, failure) = match published {
            Ok(()) => (Some(final_path.to_path_buf()), failure),
            Err(()) => (None, Some("sample_publish_failed")),
        };
        let note = match (failure, leftover) {
            (None, None) => None,
            (Some(code), None) => Some(code.to_string()),
            (None, Some(left)) => Some(left),
            (Some(code), Some(left)) => Some(format!("{code}; {left}")),
        };
        NativeOutcome { artifact, note }
    }

    /// Same owner-only, atomic publish as the panic report.
    fn write_private(path: &Path, text: &str) -> Result<(), ()> {
        let options = rightkit_fs::AtomicWriteOptions {
            private: true,
            ..rightkit_fs::AtomicWriteOptions::default()
        };
        rightkit_fs::atomic_write_with_options(path, options, |file| file.write_all(text.as_bytes()))
            .map_err(|_| ())
    }
}

#[cfg_attr(not(feature = "support-bundle"), allow(unused_imports))]
pub(crate) use sample::{scrub_for_bundle, WITHHELD_MARKER};

/// Removes stale raw sample directories from the OS temp directory (best effort).
pub(crate) fn sweep_temp_raw() {
    sample::sweep_temp_raw_in(&std::env::temp_dir());
}

#[cfg(target_os = "macos")]
mod platform {
    use super::sample::{self, default_scrub};
    use super::NativeOutcome;
    use std::os::unix::fs::PermissionsExt;
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

    pub(super) fn capture(
        dir: &Path,
        kind: &str,
        stamp: u64,
        pid: u32,
        secrets: &[String],
    ) -> NativeOutcome {
        if IN_FLIGHT.swap(true, Ordering::AcqRel) {
            return NativeOutcome::nothing();
        }
        let _guard = Guard;
        crate::retention::sweep_raw(dir);
        let final_path = dir.join(format!("sample_{kind}_{stamp}_{pid}.txt"));
        // `sample` writes the unscrubbed text into a private directory outside the crash folder.
        // Nothing raw is ever written under `dir`.
        let Ok((raw_dir, raw)) = sample::create_raw_dir(&std::env::temp_dir(), dir) else {
            return NativeOutcome {
                artifact: None,
                note: Some("sample_raw_create_failed".into()),
            };
        };
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
            Err(_) => return discard(&raw_dir),
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
                        return sample::finish(
                            &raw_dir,
                            &raw,
                            &final_path,
                            secrets,
                            &default_scrub(secrets),
                        );
                    }
                    return discard(&raw_dir);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return discard(&raw_dir);
                }
            }
        }
    }

    /// `sample` did not produce a usable file: remove its directory, keep nothing.
    fn discard(raw_dir: &Path) -> NativeOutcome {
        NativeOutcome {
            artifact: None,
            note: sample::remove_raw_dir(raw_dir),
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

    /// `pid` is the process to dump (this process, or a child the parent named). The minidump is
    /// binary and is never scrubbed, so `_secrets` has nothing to act on.
    pub(super) fn capture(
        dir: &Path,
        kind: &str,
        stamp: u64,
        pid: u32,
        _secrets: &[String],
    ) -> Option<PathBuf> {
        if IN_FLIGHT.swap(true, Ordering::AcqRel) {
            return None;
        }
        let _guard = Guard;
        let helper = super::dump_helper_path()?;
        if !helper.is_file() {
            return None;
        }
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

    pub(super) fn capture(
        _dir: &Path,
        _kind: &str,
        _stamp: u64,
        _pid: u32,
        _secrets: &[String],
    ) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::sample::{
        create_raw_dir, default_scrub, finish, remove_raw_dir, scrub_checked, scrub_for_bundle,
        sweep_temp_raw_in, TEMP_RAW_PREFIX, WITHHELD_MARKER,
    };
    use super::*;

    // Fixture assembled at run time so no literal token sits in the source.
    fn fixture() -> String {
        let token = format!("{}{}", "ghp_", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        format!(
            "Call graph:\n  1 main  (in app) + 4  [/Users/adalovelace/Documents/private/app]\n  \
             mail ada.lovelace@example.com token {token}\n  \
             Authorization: Bearer abcdefghijklmnop1234567890\n  \
             home C:\\Users\\adalovelace\\AppData\\x\n"
        )
    }

    const RAW_NEEDLES: [&str; 4] = [
        "adalovelace",
        "ada.lovelace@example.com",
        "ghp_A1b2C3d4E5f6",
        "abcdefghijklmnop1234567890",
    ];

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

    /// Every file under `dir`, recursively, as (path, bytes).
    fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    out.push((path, bytes));
                }
            }
        }
        out
    }

    fn assert_dir_has_no_raw(dir: &Path) {
        for (path, bytes) in all_files(dir) {
            let text = String::from_utf8_lossy(&bytes);
            for needle in RAW_NEEDLES {
                assert!(
                    !text.contains(needle),
                    "{} holds raw data ({needle})",
                    path.display()
                );
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!name.ends_with(".raw") && !name.starts_with(".sample_raw_"), "{name}");
        }
    }

    /// A raw directory next to (not inside) the crash folder, with the fixture in its raw file.
    fn staged(label: &str, text: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = temp_dir(&format!("{label}-tmp"));
        let crash = temp_dir(&format!("{label}-crash"));
        let (raw_dir, raw) = create_raw_dir(&base, &crash).unwrap();
        std::fs::write(&raw, text).unwrap();
        (base, crash, raw_dir, raw)
    }

    #[test]
    fn a_fixture_sample_is_scrubbed_before_it_reaches_the_crash_folder() {
        let (base, crash, raw_dir, raw) = staged("ok", &fixture());
        let final_path = crash.join("sample_panic_1_2.txt");
        let outcome = finish(&raw_dir, &raw, &final_path, &[], &default_scrub(&[]));
        assert_eq!(outcome.artifact.as_deref(), Some(final_path.as_path()));
        assert!(outcome.note.is_none(), "{:?}", outcome.note);
        let text = std::fs::read_to_string(&final_path).unwrap();
        assert!(text.contains("Call graph"), "{text}");
        assert!(!text.contains(WITHHELD_MARKER.trim()), "{text}");
        for needle in RAW_NEEDLES {
            assert!(!text.contains(needle), "{needle} survived: {text}");
        }
        assert!(!raw_dir.exists(), "raw directory must be deleted");
        assert_dir_has_no_raw(&crash);
        assert_dir_has_no_raw(&base);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn known_secrets_are_removed_from_a_published_sample() {
        let (base, crash, raw_dir, raw) =
            staged("secret", "frame key=ZZ-not-a-pattern-4821 at /Users/adalovelace/x\n");
        let secrets = vec!["ZZ-not-a-pattern-4821".to_string()];
        let final_path = crash.join("sample_panic_1_2.txt");
        let outcome = finish(&raw_dir, &raw, &final_path, &secrets, &default_scrub(&secrets));
        assert!(outcome.artifact.is_some(), "{:?}", outcome.note);
        let text = std::fs::read_to_string(&final_path).unwrap();
        assert!(!text.contains("ZZ-not-a-pattern-4821"), "{text}");
        assert!(!text.contains("adalovelace"), "{text}");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn a_scrubber_panic_yields_the_marker_not_the_raw_text() {
        let (base, crash, raw_dir, raw) = staged("panic", &fixture());
        let final_path = crash.join("sample_panic_1_2.txt");
        let boom = |_: &str| -> String { panic!("scrubber failed") };
        let outcome = finish(&raw_dir, &raw, &final_path, &[], &boom);
        assert_eq!(outcome.artifact.as_deref(), Some(final_path.as_path()));
        assert_eq!(outcome.note.as_deref(), Some("sample_scrub_failed"));
        assert_eq!(std::fs::read_to_string(&final_path).unwrap(), WITHHELD_MARKER);
        assert!(!raw_dir.exists());
        assert_dir_has_no_raw(&crash);
        assert_dir_has_no_raw(&base);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn a_scrubber_that_leaves_a_known_secret_yields_the_marker() {
        let (base, crash, raw_dir, raw) = staged("leaky", "frame key=ZZ-not-a-pattern-4821\n");
        let secrets = vec!["ZZ-not-a-pattern-4821".to_string()];
        let final_path = crash.join("sample_panic_1_2.txt");
        let identity = |t: &str| t.to_string(); // a scrubber that does nothing
        let outcome = finish(&raw_dir, &raw, &final_path, &secrets, &identity);
        assert_eq!(outcome.note.as_deref(), Some("sample_scrub_failed"));
        assert_eq!(std::fs::read_to_string(&final_path).unwrap(), WITHHELD_MARKER);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn an_unreadable_raw_file_yields_the_marker_and_the_directory_is_removed() {
        let base = temp_dir("read-tmp");
        let crash = temp_dir("read-crash");
        let (raw_dir, raw) = create_raw_dir(&base, &crash).unwrap();
        std::fs::remove_file(&raw).unwrap(); // the read now fails
        let final_path = crash.join("sample_panic_1_2.txt");
        let outcome = finish(&raw_dir, &raw, &final_path, &[], &default_scrub(&[]));
        assert_eq!(outcome.note.as_deref(), Some("sample_read_failed"));
        assert_eq!(std::fs::read_to_string(&final_path).unwrap(), WITHHELD_MARKER);
        assert!(!raw_dir.exists());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn a_failed_publish_returns_nothing_and_leaves_no_raw_text_in_the_crash_folder() {
        let (base, crash, raw_dir, raw) = staged("publish", &fixture());
        // The final path's parent is a regular file, so the atomic publish cannot succeed.
        let blocker = crash.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let final_path = blocker.join("sample_panic_1_2.txt");
        let outcome = finish(&raw_dir, &raw, &final_path, &[], &default_scrub(&[]));
        assert!(outcome.artifact.is_none());
        assert_eq!(outcome.note.as_deref(), Some("sample_publish_failed"));
        assert!(!raw_dir.exists(), "raw directory must be deleted on failure");
        assert!(!final_path.exists());
        assert_dir_has_no_raw(&crash);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn the_raw_directory_is_never_inside_the_crash_folder_and_is_private() {
        let crash = temp_dir("inside");
        // A base inside the crash folder is refused outright.
        let inner = crash.join("tmp");
        std::fs::create_dir_all(&inner).unwrap();
        assert!(create_raw_dir(&inner, &crash).is_err());
        assert!(create_raw_dir(&crash, &crash).is_err());
        assert!(all_files(&crash).is_empty());

        let base = temp_dir("inside-base");
        let (raw_dir, raw) = create_raw_dir(&base, &crash).unwrap();
        let _ = &raw;
        assert!(!raw_dir.starts_with(&crash));
        assert!(raw_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(TEMP_RAW_PREFIX));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&raw_dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&raw).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(remove_raw_dir(&raw_dir).is_none());
        assert!(!raw_dir.exists());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn a_fresh_raw_directory_survives_the_temp_sweep() {
        let base = temp_dir("sweep");
        let crash = temp_dir("sweep-crash");
        let (fresh, _) = create_raw_dir(&base, &crash).unwrap();
        sweep_temp_raw_in(&base);
        assert!(fresh.exists(), "a fresh raw directory belongs to a capture in flight");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&crash);
    }

    #[test]
    fn scrub_for_bundle_scrubs_text_and_never_returns_raw_on_failure() {
        let out = scrub_for_bundle(fixture().as_bytes());
        for needle in RAW_NEEDLES {
            assert!(!out.contains(needle), "{needle} survived: {out}");
        }
        // Invalid UTF-8 is scrubbed lossily, never passed through.
        let mut bytes = fixture().into_bytes();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        let out = scrub_for_bundle(&bytes);
        assert!(!out.contains("adalovelace"), "{out}");
        // A panicking scrubber is a failure.
        let boom = |_: &str| -> String { panic!("x") };
        assert!(scrub_checked(b"raw", &[], &boom).is_err());
    }
}
