//! Installed app discovery and single-winner spawn coordination.
//!
//! Discovery is deliberately installation-bound: it scans known application
//! bundles (macOS) or uninstall registry entries (Windows), never `PATH` or a
//! checkout. Spawn claims use `create_new`; discovery is daemon-owned and is
//! never removed by a losing launcher.

use crate::record::{
    read_live_record, try_read_live_record, try_read_record, write_takeover, ServiceRecord,
    TakeoverRequest, RANK_UNKNOWN,
};
use crate::transport::{pid_alive, probe_endpoint};
use rightkit_roots::lock::{try_lock_exclusive, LockedFile};
use rightkit_roots::{make_private_dir, Suite};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Installed Right Suite applications and their engine executable.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledApp {
    pub app: String,
    pub app_path: String,
    pub engine_path: String,
    pub version: Option<String>,
    pub source: String,
    /// Whether the engine executable was found. Always [`Completeness::Complete`] unless
    /// [`DiscoverOptions::include_incomplete`] was set.
    pub completeness: Completeness,
}

/// Whether an installed app is usable as a service host.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    /// The app bundle and its engine executable are both present.
    #[default]
    Complete,
    /// The app is installed but its engine executable is missing (a broken or partial
    /// install). `engine_path` names where the engine was expected.
    EngineMissing,
}

/// Options for discovery. `DiscoverOptions::default()` reproduces the original behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoverOptions {
    /// Also return an app whose bundle is installed but whose engine is missing, marked
    /// [`Completeness::EngineMissing`]. A complete install in any root always wins over an
    /// incomplete one. Default `false`. Honoured on macOS ([`discover_macos_with`]) and on
    /// Windows ([`discover_windows_with`], where an entry whose install directory exists
    /// but whose engine file is missing is reported).
    pub include_incomplete: bool,
}

/// The original Right Suite table. Deprecated: the crate hard-coded one product
/// suite's names. Products pass their own `(app name, engine executable)` registry to
/// [`discover_apps_in`]. Kept with identical contents so existing callers behave the same
/// in this release.
#[deprecated(
    since = "0.1.2",
    note = "pass your own (app, engine) registry to discover_apps_in"
)]
pub const KNOWN_APPS: &[(&str, &str)] = SUITE_APPS;

/// Built-in table behind the deprecated [`KNOWN_APPS`] and [`discover_apps`].
const SUITE_APPS: &[(&str, &str)] = &[
    ("HeardRight", "heardright-engine"),
    ("ScrapeRight", "scraperight-engine"),
    ("GenRight", "genright-engine"),
    ("CutRight", "cutright-engine"),
];

/// Discover installed apps from the caller's `(app bundle or display name, engine executable)`
/// registry. The first complete match per app wins.
pub fn discover_apps_in(registry: &[(&str, &str)]) -> Vec<InstalledApp> {
    discover_apps_in_with(registry, &DiscoverOptions::default())
}

/// [`discover_apps_in`] with explicit [`DiscoverOptions`].
pub fn discover_apps_in_with(
    registry: &[(&str, &str)],
    options: &DiscoverOptions,
) -> Vec<InstalledApp> {
    #[cfg(target_os = "macos")]
    {
        discover_macos_with(&default_macos_roots(), registry, options)
    }
    #[cfg(windows)]
    {
        discover_windows_with(
            &read_windows_uninstall_entries(),
            registry,
            |p| Path::new(p).is_file(),
            |p| Path::new(p).is_dir(),
            options,
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (registry, options);
        Vec::new()
    }
}

/// Untrusted locations from one installed CLI discovery probe.
#[derive(Debug, Default)]
pub struct InstalledCliDiscovery {
    /// Every observed install entry, including incomplete ones, in discovery order.
    pub candidates: Vec<InstalledCliCandidate>,
    /// Failed probes that could have hidden other candidates. A nonempty list cannot
    /// establish that an install is absent or unique.
    pub failures: Vec<InstalledCliProbeFailure>,
}

/// An install entry whose executable still needs independent authentication.
#[derive(Debug)]
pub struct InstalledCliCandidate {
    /// Installation directory or app bundle reported by discovery.
    pub installation: PathBuf,
    /// Expected helper path, formed using the caller's relative path.
    pub executable: PathBuf,
    /// Whether discovery observed a complete filesystem entry.
    pub state: InstalledCliCandidateState,
}

/// Filesystem observation only. This carries no signing or launch guarantees.
#[derive(Debug)]
#[non_exhaustive]
pub enum InstalledCliCandidateState {
    /// A directory and regular executable file were observed at the expected paths.
    Present,
    /// The install entry exists, but a required directory or file was missing or of
    /// the wrong type (including a symbolic link at a probed path).
    Incomplete,
    /// Discovery observed the installation entry but could not finish probing it.
    ProbeFailed(InstalledCliProbeFailure),
}

/// An installation discovery probe that could not be completed.
#[derive(Debug)]
pub struct InstalledCliProbeFailure {
    /// Filesystem path or registry key that could not be probed, when available.
    pub path: Option<PathBuf>,
    /// Underlying probe error.
    pub error: io::Error,
}

/// Probe installed CLI locations without selecting a winner. `app_name` is one macOS
/// bundle name component without `.app`, or the exact Windows uninstall display name.
/// `relative_executable` is the expected path inside the installation, including
/// `Contents` on macOS. Both come from the caller's app catalogue.
/// Unsupported platforms return [`io::ErrorKind::Unsupported`]. Indeterminate probes
/// on a supported platform are preserved alongside any observed candidates.
pub fn discover_installed_cli_candidates(
    app_name: &str,
    relative_executable: &Path,
) -> io::Result<InstalledCliDiscovery> {
    #[cfg(target_os = "macos")]
    {
        let invalid_relative = relative_executable
            .as_os_str()
            .as_encoded_bytes()
            .split(|&byte| byte == b'/')
            .any(|part| matches!(part, b"" | b"." | b"..") || part.contains(&0));
        if app_name.is_empty()
            || app_name.ends_with(".app")
            || app_name.as_bytes().contains(&0)
            || Path::new(app_name).file_name().and_then(|name| name.to_str()) != Some(app_name)
            || invalid_relative
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an app name and a relative CLI path",
            ));
        }

        let mut discovery = InstalledCliDiscovery::default();
        let bundle = format!("{app_name}.app");
        probe_macos_cli_root(
            Path::new("/Applications"),
            &bundle,
            relative_executable,
            &mut discovery,
        );
        match rightkit_roots::passwd_home_dir() {
            Ok(home) if home.is_absolute() => probe_macos_cli_root(
                &home.join("Applications"),
                &bundle,
                relative_executable,
                &mut discovery,
            ),
            Ok(home) => discovery.failures.push(InstalledCliProbeFailure {
                path: Some(home),
                error: io::Error::new(io::ErrorKind::InvalidData, "account home is not absolute"),
            }),
            Err(error) => discovery.failures.push(InstalledCliProbeFailure {
                path: None,
                error: io::Error::other(error),
            }),
        }
        Ok(discovery)
    }
    #[cfg(windows)]
    {
        windows_cli_discovery::discover(app_name, relative_executable)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (app_name, relative_executable);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict installed CLI discovery is unavailable on this platform",
        ))
    }
}

#[cfg(target_os = "macos")]
fn probe_macos_cli_root(
    root: &Path,
    bundle_name: &str,
    relative_executable: &Path,
    discovery: &mut InstalledCliDiscovery,
) {
    use std::fs::symlink_metadata;

    let root_error = match symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => None,
        Ok(_) => Some(io::Error::new(
            io::ErrorKind::InvalidData,
            "application root is not a directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => Some(error),
    };
    if let Some(error) = root_error {
        discovery.failures.push(InstalledCliProbeFailure {
            path: Some(root.to_path_buf()),
            error,
        });
        return;
    }

    let installation = root.join(bundle_name);
    let bundle = match symlink_metadata(&installation) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            discovery.failures.push(InstalledCliProbeFailure {
                path: Some(installation),
                error,
            });
            return;
        }
    };
    let executable = installation.join(relative_executable);
    let state = if bundle.is_dir() {
        probe_cli_path(&installation, relative_executable)
    } else {
        InstalledCliCandidateState::Incomplete
    };
    discovery.candidates.push(InstalledCliCandidate {
        installation,
        executable,
        state,
    });
}

#[cfg(any(target_os = "macos", windows))]
fn probe_cli_path(installation: &Path, relative_executable: &Path) -> InstalledCliCandidateState {
    use InstalledCliCandidateState::{Incomplete, Present, ProbeFailed};

    let mut path = installation.to_path_buf();
    let mut components = relative_executable.components().peekable();
    while let Some(component) = components.next() {
        path.push(component.as_os_str());
        let is_leaf = components.peek().is_none();
        match std::fs::symlink_metadata(&path) {
            #[cfg(windows)]
            Ok(metadata) if windows_cli_discovery::is_reparse_point(&metadata) => {
                return Incomplete;
            }
            Ok(metadata) if is_leaf && metadata.is_file() => return Present,
            Ok(metadata) if !is_leaf && metadata.is_dir() => {}
            Ok(_) => return Incomplete,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Incomplete,
            Err(error) => {
                return ProbeFailed(InstalledCliProbeFailure {
                    path: Some(path),
                    error,
                });
            }
        }
    }
    Incomplete
}

#[cfg(target_os = "macos")]
fn default_macos_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    // The user's own Applications folder comes from the account database (`getpwuid_r`), never
    // from `HOME`: whoever launches the process controls `HOME`, and this root decides which
    // installed apps are trusted as siblings.
    if let Ok(home) = rightkit_roots::passwd_home_dir() {
        roots.push(home.join("Applications"));
    }
    roots
}

/// Discover the built-in Right Suite apps. Deprecated: use [`discover_apps_in`].
#[deprecated(since = "0.1.2", note = "use discover_apps_in with your own registry")]
pub fn discover_apps() -> Vec<InstalledApp> {
    #[allow(deprecated)]
    discover_apps_in(KNOWN_APPS)
}

/// Scan roots in order; first complete bundle wins per app.
pub fn discover_macos(roots: &[PathBuf], apps: &[(&str, &str)]) -> Vec<InstalledApp> {
    discover_macos_with(roots, apps, &DiscoverOptions::default())
}

/// [`discover_macos`] with explicit [`DiscoverOptions`]. With `include_incomplete`, an app
/// that has a bundle directory but no engine in any root is returned once, from the first
/// root that has its bundle, as [`Completeness::EngineMissing`].
pub fn discover_macos_with(
    roots: &[PathBuf],
    apps: &[(&str, &str)],
    options: &DiscoverOptions,
) -> Vec<InstalledApp> {
    let mut found = Vec::new();
    for (name, engine) in apps {
        let mut incomplete: Option<InstalledApp> = None;
        let mut complete: Option<InstalledApp> = None;
        for root in roots {
            let bundle = root.join(format!("{name}.app"));
            let binary = bundle.join("Contents/MacOS").join(engine);
            let present = binary.is_file();
            if !present && (!options.include_incomplete || incomplete.is_some() || !bundle.is_dir())
            {
                continue;
            }
            let app = InstalledApp {
                app: (*name).to_string(),
                app_path: bundle.to_string_lossy().into_owned(),
                engine_path: binary.to_string_lossy().into_owned(),
                version: read_plist_version(&bundle.join("Contents/Info.plist")),
                source: "macos-bundle".into(),
                completeness: if present {
                    Completeness::Complete
                } else {
                    Completeness::EngineMissing
                },
            };
            if present {
                complete = Some(app);
                break;
            }
            incomplete = Some(app);
        }
        found.extend(complete.or(incomplete));
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

/// Discover complete Windows installs only. Equivalent to [`discover_windows_with`] with
/// [`DiscoverOptions::default`].
pub fn discover_windows(
    entries: &[UninstallEntry],
    apps: &[(&str, &str)],
    exists: impl Fn(&str) -> bool,
) -> Vec<InstalledApp> {
    discover_windows_with(
        entries,
        apps,
        exists,
        |_| false,
        &DiscoverOptions::default(),
    )
}

/// Windows discovery with explicit options. `exists` checks the engine file and `dir_exists`
/// checks the install directory. With `include_incomplete`, an entry whose directory exists but
/// whose engine is missing is returned as [`Completeness::EngineMissing`]. A complete install
/// from any entry of the same app always wins.
pub fn discover_windows_with(
    entries: &[UninstallEntry],
    apps: &[(&str, &str)],
    exists: impl Fn(&str) -> bool,
    dir_exists: impl Fn(&str) -> bool,
    options: &DiscoverOptions,
) -> Vec<InstalledApp> {
    let mut found = Vec::new();
    for (name, engine) in apps {
        let mut incomplete: Option<InstalledApp> = None;
        let mut complete: Option<InstalledApp> = None;
        for entry in entries.iter().filter(|entry| entry.display_name == *name) {
            let Some(dir) = resolve_install_dir(entry) else {
                continue;
            };
            let engine_path = format!(r"{dir}\{engine}.exe");
            let present = exists(&engine_path);
            if !present
                && (!options.include_incomplete || incomplete.is_some() || !dir_exists(&dir))
            {
                continue;
            }
            let app = InstalledApp {
                app: (*name).to_string(),
                app_path: dir,
                engine_path,
                version: entry
                    .display_version
                    .clone()
                    .filter(|v| !v.trim().is_empty()),
                source: "windows-uninstall".into(),
                completeness: if present {
                    Completeness::Complete
                } else {
                    Completeness::EngineMissing
                },
            };
            if present {
                complete = Some(app);
                break;
            }
            incomplete = Some(app);
        }
        if let Some(app) = complete.or(incomplete) {
            found.push(app);
        }
    }
    found
}

#[cfg(windows)]
mod windows_cli_discovery {
    use super::{
        probe_cli_path, InstalledCliCandidate, InstalledCliCandidateState, InstalledCliDiscovery,
        InstalledCliProbeFailure,
    };
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::fs::Metadata;
    use std::io;
    use std::os::windows::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::ptr::{null, null_mut};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, FILE_ATTRIBUTE_REPARSE_POINT};
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_WOW64_32KEY,
        KEY_WOW64_64KEY, REG_SZ,
    };

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    const MAX_ROWS: u32 = 8192;
    const MAX_TEXT_UNITS: usize = 32_768;
    const DRIVE_FIXED: u32 = 3;

    pub(super) fn discover(
        app_name: &str,
        relative_executable: &Path,
    ) -> io::Result<InstalledCliDiscovery> {
        if app_name.trim().is_empty()
            || app_name.as_bytes().contains(&0)
            || !valid_relative_path(relative_executable)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an uninstall display name and a relative CLI path",
            ));
        }

        let mut discovery = InstalledCliDiscovery::default();
        let mut seen = BTreeSet::new();
        for (hive, view, source) in [
            (HKEY_CURRENT_USER, 0, "HKCU"),
            (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, "HKLM (64-bit)"),
            (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, "HKLM (32-bit)"),
        ] {
            let registry_path = PathBuf::from(format!(r"{source}\{UNINSTALL}"));
            if let Err(error) = probe_view(
                hive,
                view,
                &registry_path,
                app_name,
                relative_executable,
                &mut discovery,
                &mut seen,
            ) {
                discovery.failures.push(InstalledCliProbeFailure {
                    path: Some(registry_path),
                    error,
                });
            }
        }
        Ok(discovery)
    }

    fn probe_view(
        hive: HKEY,
        view: u32,
        registry_path: &Path,
        app_name: &str,
        relative_executable: &Path,
        discovery: &mut InstalledCliDiscovery,
        seen: &mut BTreeSet<PathBuf>,
    ) -> io::Result<()> {
        let uninstall: Vec<_> = UNINSTALL.encode_utf16().chain([0]).collect();
        let key = match Key::open(hive, &uninstall, KEY_ENUMERATE_SUB_KEYS | view) {
            Ok(key) => key,
            Err(error)
                if error.raw_os_error().is_some_and(|status| {
                    matches!(status as u32, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
                }) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut value_buffer = vec![0u16; MAX_TEXT_UNITS];
        let mut name = [0u16; 256];
        for index in 0..=MAX_ROWS {
            let mut len = name.len() as u32;
            // SAFETY: `name` has room for `len` units and unused out-pointers are null.
            let status = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            match status {
                ERROR_NO_MORE_ITEMS => return Ok(()),
                ERROR_SUCCESS => {}
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
            let len = len as usize;
            if index == MAX_ROWS || len == 0 || len >= name.len() {
                return Err(invalid_data(
                    "uninstall registry enumeration exceeded its bounds",
                ));
            }
            name[len] = 0;
            let result = Key::open(key.0, &name[..=len], KEY_QUERY_VALUE | view)
                .and_then(|row| row.location_for(app_name, &mut value_buffer))
                .and_then(|location| {
                    location.map_or(Ok(None), |path| candidate(&path, relative_executable, seen))
                });
            match result {
                Ok(Some(candidate)) => discovery.candidates.push(candidate),
                Ok(None) => {}
                Err(error) => discovery.failures.push(InstalledCliProbeFailure {
                    path: Some(registry_path.join(String::from_utf16_lossy(&name[..len]))),
                    error,
                }),
            }
        }
        Ok(())
    }

    struct Key(HKEY);

    impl Key {
        fn open(parent: HKEY, name: &[u16], access: u32) -> io::Result<Self> {
            let mut key = null_mut();
            // SAFETY: caller supplies a NUL-terminated key name & a live parent key.
            let status = unsafe { RegOpenKeyExW(parent, name.as_ptr(), 0, access, &mut key) };
            if status != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            Ok(Self(key))
        }

        fn location_for(&self, app_name: &str, buffer: &mut [u16]) -> io::Result<Option<PathBuf>> {
            if self.text(windows_sys::core::w!("DisplayName"), buffer)?.as_deref()
                != Some(app_name)
            {
                return Ok(None);
            }
            if let Some(location) = self.text(windows_sys::core::w!("InstallLocation"), buffer)? {
                let location = location
                    .strip_prefix('"')
                    .and_then(|unquoted| unquoted.strip_suffix('"'))
                    .unwrap_or(&location);
                if !location.is_empty() {
                    return Ok(Some(PathBuf::from(location)));
                }
            }
            Err(invalid_data("matching uninstall row requires InstallLocation"))
        }

        fn text(&self, field: *const u16, buffer: &mut [u16]) -> io::Result<Option<String>> {
            let mut bytes = std::mem::size_of_val(buffer) as u32;
            let mut kind = 0;
            // SAFETY: `field` is a static NUL-terminated name; `buffer` is writable for `bytes`.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    field,
                    null(),
                    &mut kind,
                    buffer.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            match status {
                ERROR_FILE_NOT_FOUND => return Ok(None),
                ERROR_SUCCESS => {}
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
            if kind != REG_SZ
                || !bytes.is_multiple_of(2)
                || bytes as usize > std::mem::size_of_val(buffer)
            {
                return Err(invalid_data(
                    "uninstall registry value must be bounded literal text",
                ));
            }
            let units = &buffer[..bytes as usize / 2];
            let units = units.strip_suffix(&[0]).unwrap_or(units);
            if units.contains(&0) {
                return Err(invalid_data("uninstall registry value contains an embedded NUL"));
            }
            let text = String::from_utf16(units)
                .map_err(|_| invalid_data("uninstall registry value contains invalid UTF-16"))?;
            Ok(Some(text))
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `self.0` belongs to this Key, which closes it exactly once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn valid_relative_path(path: &Path) -> bool {
        path.as_os_str()
            .as_encoded_bytes()
            .split(|&byte| matches!(byte, b'\\' | b'/'))
            .all(|part| !matches!(part, b"" | b"." | b".."))
            && path.components().all(|part| {
                matches!(part, Component::Normal(name) if valid_path_component(name))
            })
    }

    fn valid_path_component(component: &OsStr) -> bool {
        let bytes = component.as_encoded_bytes();
        bytes.last().is_some_and(|&byte| !matches!(byte, b'.' | b' '))
            && !bytes.iter().any(|&byte| {
                byte < b' '
                    || matches!(
                        byte,
                        b'/' | b'\\' | b':' | b'"' | b'<' | b'>' | b'|' | b'?' | b'*'
                    )
            })
    }

    fn candidate(
        location: &Path,
        relative_executable: &Path,
        seen: &mut BTreeSet<PathBuf>,
    ) -> io::Result<Option<InstalledCliCandidate>> {
        let (drive, relative) = local_drive_path(location).ok_or_else(|| {
            invalid_data("matching uninstall row has no absolute drive location")
        })?;
        // Use a verbatim DOS path so file probes cannot reinterpret a component as a device.
        let root = PathBuf::from(format!(r"\\?\{}:\", char::from(drive)));
        let installation = root.join(&relative);
        if !seen.insert(installation.clone()) {
            return Ok(None);
        }
        let relative = relative.join(relative_executable);
        let executable = root.join(&relative);
        let drive_root = [u16::from(drive), u16::from(b':'), u16::from(b'\\'), 0];
        // SAFETY: the fixed-size buffer is a NUL-terminated Windows drive root.
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(drive_root.as_ptr())) };
        let state = if drive_type == DRIVE_FIXED {
            probe_cli_path(&root, &relative)
        } else {
            InstalledCliCandidateState::ProbeFailed(InstalledCliProbeFailure {
                path: Some(root),
                error: invalid_data("install location requires a fixed local drive"),
            })
        };
        Ok(Some(InstalledCliCandidate {
            installation,
            executable,
            state,
        }))
    }

    fn local_drive_path(location: &Path) -> Option<(u8, PathBuf)> {
        let mut components = location.components();
        let Component::Prefix(prefix) = components.next()? else {
            return None;
        };
        let drive = match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive.to_ascii_uppercase(),
            _ => return None,
        };
        if !matches!(components.next(), Some(Component::RootDir)) {
            return None;
        }
        let mut relative = PathBuf::new();
        for part in components {
            let Component::Normal(name) = part else {
                return None;
            };
            if !valid_path_component(name) {
                return None;
            }
            relative.push(name);
        }
        Some((drive, relative))
    }

    pub(super) fn is_reparse_point(metadata: &Metadata) -> bool {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }

    fn invalid_data(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }
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
/// always locks the same inode; Drop unlocks and closes only this claim's own
/// handle.
pub struct SpawnClaim {
    _file: LockedFile,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOrSpawn {
    Adopt(ServiceRecord),
    Spawn,
}

/// Adopt only a live, reachable record whose PID generation still matches.
///
/// A `service.json` that cannot be read is `Spawn` here, as an absent one is; the enum cannot say
/// "unknown". Use [`try_adopt_or_spawn`] where that must not start a second host.
pub fn adopt_or_spawn(suite: &Suite, service: &str) -> AdoptOrSpawn {
    try_adopt_or_spawn(suite, service).unwrap_or(AdoptOrSpawn::Spawn)
}

/// [`adopt_or_spawn`] with `Err` when `service.json` exists but could not be read: whether a
/// live host exists is then unknown, so look again rather than spawn.
pub fn try_adopt_or_spawn(suite: &Suite, service: &str) -> io::Result<AdoptOrSpawn> {
    Ok(match try_read_record(suite, service)? {
        Some(record) if record_process_matches(&record) && probe_endpoint(&record.endpoint) => {
            AdoptOrSpawn::Adopt(record)
        }
        _ => AdoptOrSpawn::Spawn,
    })
}

/// Wait for the winner of a competing claim to publish a matching live record.
pub fn wait_for_claim_winner(
    suite: &Suite,
    service: &str,
    timeout: Duration,
) -> Option<ServiceRecord> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // An unreadable record is "not yet": keep waiting, never stop early on it.
        if let Ok(AdoptOrSpawn::Adopt(record)) = try_adopt_or_spawn(suite, service) {
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
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    // Win32 writes into the caller's UTF-16 buffer. A successful length excludes the NUL.
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Vec::new();
    }
    let system_directory = PathBuf::from(OsString::from_wide(&buffer[..length]));
    if !system_directory.is_absolute() {
        return Vec::new();
    }
    let reg_path = system_directory.join("reg.exe");
    let mut all = String::new();
    for key in [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let mut reg = Command::new(&reg_path);
        reg.args(["query", key, "/s"]);
        reg.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: never flash a console from a GUI host
        if let Ok(output) = reg.output() {
            all.push_str(&String::from_utf8_lossy(&output.stdout));
            all.push('\n');
        }
    }
    parse_reg_query(&all)
}

// ---------------------------------------------------------------------------
// Host planning, takeover and detached start. Every service client plans through
// these; product crates map their own role names onto ranks (`known_roles`) and
// keep their own wire file names.
// ---------------------------------------------------------------------------

/// Rank of a live record or takeover request: its own `rank` when it writes one,
/// else `known_roles` by its `role`. `None` when neither resolves; such a peer is
/// never taken over from and never obliges a handoff.
pub fn resolve_rank(role: &str, rank: u8, known_roles: &BTreeMap<String, u8>) -> Option<u8> {
    if rank != RANK_UNKNOWN {
        Some(rank)
    } else {
        known_roles.get(role).copied()
    }
}

/// Outcome of [`plan_host`] for a process of a given rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPlan {
    /// A live host of equal or better rank serves the service.
    UseRunning,
    /// A live host of lower rank serves it and this process outranks it: request takeover.
    TakeOver { from_role: String },
    /// Nothing live and nothing to start: host in this process.
    HostMyself,
    /// Nothing live: start this installed engine, then re-discover.
    StartInstalled(PathBuf),
}

/// Decide how a process of rank `my_rank` gets a host for `service`. A live record whose
/// rank cannot be resolved through `known_roles` counts as absent. `installed_engine` is
/// the executable the caller may start when nothing is live; it is used only when it exists.
pub fn plan_host(
    suite: &Suite,
    service: &str,
    my_rank: u8,
    known_roles: &BTreeMap<String, u8>,
    installed_engine: Option<&Path>,
) -> HostPlan {
    plan_from(
        read_live_record(suite, service),
        my_rank,
        known_roles,
        installed_engine,
    )
}

/// [`plan_host`] with `Err` when `service.json` exists but could not be read. Whether a live host
/// exists is then unknown, so no plan is made: `plan_host` would answer `HostMyself` or
/// `StartInstalled`, which starts a second host beside a live one.
pub fn try_plan_host(
    suite: &Suite,
    service: &str,
    my_rank: u8,
    known_roles: &BTreeMap<String, u8>,
    installed_engine: Option<&Path>,
) -> io::Result<HostPlan> {
    Ok(plan_from(
        try_read_live_record(suite, service)?,
        my_rank,
        known_roles,
        installed_engine,
    ))
}

fn plan_from(
    live: Option<ServiceRecord>,
    my_rank: u8,
    known_roles: &BTreeMap<String, u8>,
    installed_engine: Option<&Path>,
) -> HostPlan {
    if let Some(record) = live {
        if let Some(their) = resolve_rank(&record.role, record.rank, known_roles) {
            return if my_rank < their {
                HostPlan::TakeOver {
                    from_role: record.role,
                }
            } else {
                HostPlan::UseRunning
            };
        }
    }
    match installed_engine {
        Some(engine) if engine.exists() => HostPlan::StartInstalled(engine.to_path_buf()),
        _ => HostPlan::HostMyself,
    }
}

/// Takeover is requested only against a live, lower-ranked host held by another process.
/// `incumbent` is the live record's `(resolved rank, pid)`.
pub fn should_request_takeover(my_rank: u8, incumbent: Option<(u8, u32)>, own_pid: u32) -> bool {
    incumbent.is_some_and(|(their, pid)| my_rank < their && pid != own_pid)
}

/// Does a pending takeover request `(resolved rank, pid)` oblige a host of rank `my_rank`
/// to yield? Only a live, other-process, outranking requester does.
pub fn takeover_obliges_yield(
    my_rank: u8,
    request: Option<(u8, u32)>,
    own_pid: u32,
    alive: &dyn Fn(u32) -> bool,
) -> bool {
    request.is_some_and(|(their, pid)| pid != own_pid && their < my_rank && alive(pid))
}

/// Publish `takeover.json` for this process as `role` (with `rank`), atomically.
pub fn publish_takeover(suite: &Suite, service: &str, role: &str, rank: u8) -> io::Result<()> {
    write_takeover(
        suite,
        service,
        &TakeoverRequest {
            role: role.into(),
            rank,
            pid: std::process::id(),
            requested_at_ms: now_ms(),
        },
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Start `program` detached: no inherited stdio, its own process group on Unix, and on
/// Windows no console window (`CREATE_NO_WINDOW | DETACHED_PROCESS`) so a GUI host never
/// flashes one. The child is not waited for.
pub fn spawn_detached(program: &Path, args: &[String], env: &[(String, String)]) -> io::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    drop(cmd.spawn()?);
    Ok(())
}

/// Poll `ready` every 100 ms until it holds (`true`) or `wait` elapses (`false`).
/// Checks before the first sleep.
pub fn poll_until(wait: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
