//! Installed app discovery and single-winner spawn coordination.
//!
//! Discovery is deliberately installation-bound: it scans known application
//! bundles (macOS) or uninstall registry entries (Windows), never `PATH` or a
//! checkout. Spawn claims use `create_new`; discovery is daemon-owned and is
//! never removed by a losing launcher.

use crate::record::{read_record, ServiceRecord};
use crate::suite::{make_private_dir, Suite};
use crate::transport::{pid_alive, probe_endpoint};
use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Installed Right Suite applications and their engine executable.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledApp {
    pub app: String,
    pub app_path: String,
    pub engine_path: String,
    pub version: Option<String>,
    pub source: String,
}

/// Known app table supplied by the product. Keeping it caller-visible makes
/// discovery reusable by products with a smaller or extended suite.
pub const KNOWN_APPS: &[(&str, &str)] = &[
    ("HeardRight", "heardright-engine"),
    ("ScrapeRight", "scraperight-engine"),
    ("GenRight", "genright-engine"),
    ("CutRight", "cutright-engine"),
];

pub fn discover_apps() -> Vec<InstalledApp> {
    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Applications"));
        }
        discover_macos(&roots, KNOWN_APPS)
    }
    #[cfg(windows)]
    {
        discover_windows(&read_windows_uninstall_entries(), KNOWN_APPS, |p| {
            Path::new(p).is_file()
        })
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// Scan roots in order; first complete bundle wins per app.
pub fn discover_macos(roots: &[PathBuf], apps: &[(&str, &str)]) -> Vec<InstalledApp> {
    let mut found = Vec::new();
    for (name, engine) in apps {
        for root in roots {
            let bundle = root.join(format!("{name}.app"));
            let binary = bundle.join("Contents/MacOS").join(engine);
            if !binary.is_file() {
                continue;
            }
            found.push(InstalledApp {
                app: (*name).to_string(),
                app_path: bundle.to_string_lossy().into_owned(),
                engine_path: binary.to_string_lossy().into_owned(),
                version: read_plist_version(&bundle.join("Contents/Info.plist")),
                source: "macos-bundle".into(),
            });
            break;
        }
    }
    found
}

fn read_plist_version(path: &Path) -> Option<String> {
    parse_plist_string(
        &std::fs::read_to_string(path).ok()?,
        "CFBundleShortVersionString",
    )
}

/// Parse a plain XML plist without requiring a plist dependency. Binary plist
/// files return `None`, while the app remains discoverable.
pub fn parse_plist_string(xml: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let after = &xml[xml.find(&marker)? + marker.len()..];
    let string_start = after.find("<string>")? + "<string>".len();
    if after[..string_start].contains("<key>") {
        return None;
    }
    let end = after[string_start..].find("</string>")? + string_start;
    let value = after[string_start..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// One uninstall registry row, independent of the Windows registry API so it
/// can be tested from captured `reg query` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallEntry {
    pub display_name: String,
    pub install_location: Option<String>,
    pub display_icon: Option<String>,
    pub uninstall_string: Option<String>,
    pub display_version: Option<String>,
}

/// Parse `reg query ... /s` output. Unknown values and malformed rows are
/// ignored; keys are grouped by their registry-key heading.
pub fn parse_reg_query(text: &str) -> Vec<UninstallEntry> {
    let mut out = Vec::new();
    let mut current = UninstallEntry::default();
    let mut active = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.starts_with("HKEY_") {
            if active && !current.display_name.is_empty() {
                out.push(current);
            }
            current = UninstallEntry::default();
            active = true;
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let Some(name) = fields.next() else { continue };
        let Some(_kind) = fields.next() else { continue };
        let value = fields.collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            continue;
        }
        match name {
            "DisplayName" => current.display_name = value,
            "InstallLocation" => current.install_location = Some(value),
            "DisplayIcon" => current.display_icon = Some(value),
            "UninstallString" => current.uninstall_string = Some(value),
            "DisplayVersion" => current.display_version = Some(value),
            _ => {}
        }
    }
    if active && !current.display_name.is_empty() {
        out.push(current);
    }
    out
}

fn strip_quotes(value: &str) -> &str {
    value.trim().trim_matches('"').trim()
}

fn parent(path: &str) -> Option<String> {
    let i = path.rfind(['\\', '/'])?;
    (!path[..i].is_empty()).then(|| path[..i].to_string())
}

fn executable_from_value(value: &str) -> Option<String> {
    let value = value.trim();
    let path = if let Some(rest) = value.strip_prefix('"') {
        rest.split('"').next()?.to_string()
    } else {
        let lower = value.to_ascii_lowercase();
        let end = lower.find(".exe").map(|i| i + 4).unwrap_or(value.len());
        value[..end].to_string()
    };
    (!path.is_empty()).then_some(path)
}

pub fn resolve_install_dir(entry: &UninstallEntry) -> Option<String> {
    if let Some(loc) = entry.install_location.as_deref() {
        let loc = strip_quotes(loc).trim_end_matches(['\\', '/']);
        if !loc.is_empty() {
            return Some(loc.to_string());
        }
    }
    entry
        .display_icon
        .as_deref()
        .and_then(executable_from_value)
        .and_then(|p| parent(&p))
        .or_else(|| {
            entry
                .uninstall_string
                .as_deref()
                .and_then(executable_from_value)
                .and_then(|p| parent(&p))
        })
}

pub fn discover_windows(
    entries: &[UninstallEntry],
    apps: &[(&str, &str)],
    exists: impl Fn(&str) -> bool,
) -> Vec<InstalledApp> {
    let mut found = Vec::new();
    for (name, engine) in apps {
        for entry in entries.iter().filter(|entry| entry.display_name == *name) {
            let Some(dir) = resolve_install_dir(entry) else {
                continue;
            };
            let engine_path = format!(r"{dir}\{engine}.exe");
            if !exists(&engine_path) {
                continue;
            }
            found.push(InstalledApp {
                app: (*name).to_string(),
                app_path: dir,
                engine_path,
                version: entry
                    .display_version
                    .clone()
                    .filter(|v| !v.trim().is_empty()),
                source: "windows-uninstall".into(),
            });
            break;
        }
    }
    found
}

#[derive(Debug)]
pub enum SpawnClaimError {
    AlreadyHeld(PathBuf),
    Io(io::Error),
}
impl std::fmt::Display for SpawnClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyHeld(path) => write!(f, "spawn claim is held: {}", path.display()),
            Self::Io(e) => write!(f, "spawn claim: {e}"),
        }
    }
}
impl std::error::Error for SpawnClaimError {}
impl From<io::Error> for SpawnClaimError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The claim around one process spawn attempt, backed by an OS exclusive lock
/// on `spawn.claim`. The lock lives as long as the open file: the kernel
/// releases it when the owner dies (any signal), and there is no time-based
/// eviction of a live owner. The file is never unlinked, so every contender
/// always locks the same inode; Drop closes only this claim's own handle.
pub struct SpawnClaim {
    _file: std::fs::File,
    path: PathBuf,
}

impl SpawnClaim {
    pub fn path(suite: &Suite, service: &str) -> PathBuf {
        suite.service_dir(service).join("spawn.claim")
    }

    pub fn acquire(suite: &Suite, service: &str) -> Result<Self, SpawnClaimError> {
        make_private_dir(&suite.service_dir(service))
            .map_err(|e| SpawnClaimError::Io(io::Error::other(e.to_string())))?;
        let path = Self::path(suite, service);
        match try_lock_exclusive(&path) {
            Ok(Some(file)) => Ok(Self { _file: file, path }),
            Ok(None) => Err(SpawnClaimError::AlreadyHeld(path)),
            Err(e) => Err(SpawnClaimError::Io(e)),
        }
    }

    /// Path of the claim file (for diagnostics).
    pub fn claim_path(&self) -> &Path {
        &self.path
    }
}

/// Open `path` and take a non-blocking exclusive lock. `Ok(None)` = held elsewhere.
#[cfg(unix)]
fn try_lock_exclusive(path: &Path) -> io::Result<Option<std::fs::File>> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    loop {
        // SAFETY: valid open fd; flock has no memory effects.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(c) if c == libc::EWOULDBLOCK || c == libc::EAGAIN => return Ok(None),
            Some(c) if c == libc::EINTR => continue,
            _ => return Err(e),
        }
    }
    // Informational only (we hold the lock, so the write is ours).
    let _ = f.set_len(0);
    let _ = f.write_all(std::process::id().to_string().as_bytes());
    Ok(Some(f))
}

/// Windows: open with share mode 0 (no sharing). A second open fails with
/// ERROR_SHARING_VIOLATION (32) while the first handle lives; the kernel closes
/// the handle on process death. Not compiled on this host; std-only API
/// (`OpenOptionsExt::share_mode`), checked by reasoning against its docs.
#[cfg(windows)]
fn try_lock_exclusive(path: &Path) -> io::Result<Option<std::fs::File>> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(mut f) => {
            let _ = f.write_all(std::process::id().to_string().as_bytes());
            Ok(Some(f))
        }
        Err(e) if e.raw_os_error() == Some(32) => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOrSpawn {
    Adopt(ServiceRecord),
    Spawn,
}

/// Adopt only a live, reachable record whose PID generation still matches.
pub fn adopt_or_spawn(suite: &Suite, service: &str) -> AdoptOrSpawn {
    match read_record(suite, service) {
        Some(record) if record_process_matches(&record) && probe_endpoint(&record.endpoint) => {
            AdoptOrSpawn::Adopt(record)
        }
        _ => AdoptOrSpawn::Spawn,
    }
}

/// Wait for the winner of a competing claim to publish a matching live record.
pub fn wait_for_claim_winner(
    suite: &Suite,
    service: &str,
    timeout: Duration,
) -> Option<ServiceRecord> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let AdoptOrSpawn::Adopt(record) = adopt_or_spawn(suite, service) {
            return Some(record);
        }
        std::thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    None
}

/// Check both liveness and process generation, protecting discovery from PID reuse.
pub fn record_process_matches(record: &ServiceRecord) -> bool {
    record_process_matches_inner(record)
}

fn record_process_matches_inner(record: &ServiceRecord) -> bool {
    if !pid_alive(record.pid) {
        return false;
    }
    // Legacy / wave-1 records carry no process birth time: PID liveness is all
    // we can know. `started_ms` is publication time and never validates a PID.
    if record.process_started_ms == 0 {
        return true;
    }
    match process_start_ms(record.pid) {
        Some(started) => started.abs_diff(record.process_started_ms as u128) <= 2_000,
        // Cannot read the birth time right now: the pid is alive, keep the record.
        None => true,
    }
}

/// Start time (ms since epoch) of this process, as `process_start_ms` reports it.
pub(crate) fn own_start_ms() -> Option<u64> {
    process_start_ms(std::process::id()).map(|v| v as u64)
}

fn process_start_ms(pid: u32) -> Option<u128> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(if pid == std::process::id() {
            "/proc/self/stat".into()
        } else {
            format!("/proc/{pid}/stat")
        })
        .ok()?;
        let ticks = stat
            .rsplit(')')
            .next()?
            .split_whitespace()
            .nth(19)?
            .parse::<u128>()
            .ok()?;
        let boot = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .parse::<u128>()
            .ok()?;
        return Some(boot * 1000 + ticks * 1000 / 100);
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let value = text.trim();
        let parsed = std::process::Command::new("date")
            .args(["-j", "-f", "%a %b %e %H:%M:%S %Y", value, "+%s"])
            .output()
            .ok()?;
        let seconds = String::from_utf8(parsed.stdout)
            .ok()?
            .trim()
            .parse::<u128>()
            .ok()?;
        return Some(seconds * 1000);
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn GetProcessTimes(
                handle: *mut c_void,
                creation: *mut FileTime,
                exit: *mut FileTime,
                kernel: *mut FileTime,
                user: *mut FileTime,
            ) -> i32;
            fn CloseHandle(handle: *mut c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut creation = FileTime { low: 0, high: 0 };
        let mut exit = FileTime { low: 0, high: 0 };
        let mut kernel = FileTime { low: 0, high: 0 };
        let mut user = FileTime { low: 0, high: 0 };
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
                != 0;
        unsafe {
            CloseHandle(handle);
        }
        if !ok {
            return None;
        }
        let ticks = ((creation.high as u128) << 32) | creation.low as u128;
        return Some(ticks / 10_000 - 11_644_473_600_000);
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(windows)]
fn read_windows_uninstall_entries() -> Vec<UninstallEntry> {
    let mut all = String::new();
    for key in [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let mut reg = std::process::Command::new("reg");
        reg.args(["query", key, "/s"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            reg.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: never flash a console from a GUI host
        }
        if let Ok(output) = reg.output()
        {
            all.push_str(&String::from_utf8_lossy(&output.stdout));
            all.push('\n');
        }
    }
    parse_reg_query(&all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn mac_discovery_uses_known_bundle_and_first_root() {
        let d = tempfile::tempdir().unwrap();
        let bundle = d.path().join("GenRight.app/Contents/MacOS");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("genright-engine"), b"engine").unwrap();
        fs::write(
            d.path().join("GenRight.app/Contents/Info.plist"),
            b"<key>CFBundleShortVersionString</key><string>1.2</string>",
        )
        .unwrap();
        let apps = discover_macos(
            &[d.path().to_path_buf()],
            &[("GenRight", "genright-engine")],
        );
        assert_eq!(apps[0].version.as_deref(), Some("1.2"));
    }

    #[test]
    fn registry_parser_and_windows_projection_are_deterministic() {
        let text = "HKEY_CURRENT_USER\\x\n    DisplayName    REG_SZ    GenRight\n    InstallLocation    REG_SZ    C:\\Apps\\GenRight\n    DisplayVersion    REG_SZ    2.0\n";
        let rows = parse_reg_query(text);
        assert_eq!(rows.len(), 1);
        let apps = discover_windows(&rows, &[("GenRight", "genright-engine")], |p| {
            p.ends_with("genright-engine.exe")
        });
        assert_eq!(apps[0].version.as_deref(), Some("2.0"));
    }

    #[test]
    fn spawn_claim_is_single_winner_and_released() {
        let d = tempfile::tempdir().unwrap();
        let s = Suite::at(d.path()).unwrap();
        let c = SpawnClaim::acquire(&s, "demo").unwrap();
        assert!(matches!(
            SpawnClaim::acquire(&s, "demo"),
            Err(SpawnClaimError::AlreadyHeld(_))
        ));
        drop(c);
        assert!(SpawnClaim::acquire(&s, "demo").is_ok());
    }
}
