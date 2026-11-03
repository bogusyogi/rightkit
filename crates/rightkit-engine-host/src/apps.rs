//! Installed-app discovery: locate the apps of a suite and resolve `<app>-engine` inside the
//! installed app only. No `PATH`, no environment overrides, no checkout or dev-tree fallbacks.
//!
//! The host passes the apps it knows as [`AppEntry`] values. All filesystem and registry access goes
//! through [`Host`] so both platform layouts are unit-testable on any machine. Paths are plain
//! strings so the Windows layout can be exercised from a macOS test run.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_daemon::{
    discover_macos_with, discover_windows_with, Completeness, DiscoverOptions, UninstallEntry,
};
use rightkit_job_queue::{DoctorProbe, JobError};
use serde_json::{json, Value};

/// One app a host can discover: its id and product name. The engine file stem is `<id>-engine`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Lowercase app id (`alpha`).
    pub id: String,
    /// Product name as the OS lists it (`Alpha`).
    pub product: String,
}

impl AppEntry {
    /// An entry from an app id and its product name.
    pub fn new(id: impl Into<String>, product: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            product: product.into(),
        }
    }

    /// Engine executable name for the app: `<id>-engine`.
    pub fn engine_name(&self) -> String {
        format!("{}-engine", self.id)
    }
}

/// One Windows uninstall registry entry (only the fields discovery reads).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegEntry {
    pub display_name: String,
    pub install_location: Option<String>,
    pub display_icon: Option<String>,
    pub uninstall_string: Option<String>,
}

/// Filesystem and registry access, injectable for tests.
pub trait Host {
    /// Whether `path` is an existing file.
    fn is_file(&self, path: &str) -> bool;
    /// Uninstall entries in lookup order: HKCU, HKLM, HKLM WOW6432Node.
    fn registry_entries(&self) -> Vec<RegEntry>;
}

/// Where one app was found.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    pub app: String,
    pub product: String,
    pub installed: bool,
    pub app_path: Option<String>,
    pub engine_path: Option<String>,
}

impl Located {
    /// Whether the app's engine executable was found.
    pub fn engine_found(&self) -> bool {
        self.engine_path.is_some()
    }

    fn not_installed(entry: &AppEntry) -> Self {
        Self {
            app: entry.id.clone(),
            product: entry.product.clone(),
            installed: false,
            app_path: None,
            engine_path: None,
        }
    }

    fn from_found(entry: &AppEntry, app_path: String, engine_path: String, complete: bool) -> Self {
        Self {
            app: entry.id.clone(),
            product: entry.product.clone(),
            installed: true,
            app_path: Some(app_path),
            engine_path: complete.then_some(engine_path),
        }
    }
}

/// macOS install roots: the system Applications folder, then the user's.
pub fn macos_roots(home: Option<&str>) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(h) = home {
        roots.push(PathBuf::from(format!(
            "{}/Applications",
            h.trim_end_matches('/')
        )));
    }
    roots
}

/// A Windows uninstall entry in the daemon's shape.
pub fn to_uninstall_entry(entry: RegEntry) -> UninstallEntry {
    UninstallEntry {
        display_name: entry.display_name,
        install_location: entry.install_location,
        display_icon: entry.display_icon,
        uninstall_string: entry.uninstall_string,
        display_version: None,
    }
}

/// Locate one app among macOS bundles under `roots`.
pub fn locate_macos(roots: &[PathBuf], entry: &AppEntry) -> Located {
    let engine = entry.engine_name();
    // A bundle without its engine comes back as `EngineMissing`, so the apps report still
    // lists the app as installed (`engine_missing`), with no engine path.
    let options = DiscoverOptions {
        include_incomplete: true,
    };
    match discover_macos_with(
        roots,
        &[(entry.product.as_str(), engine.as_str())],
        &options,
    )
    .into_iter()
    .next()
    {
        Some(found) => {
            let complete = found.completeness == Completeness::Complete;
            Located::from_found(entry, found.app_path, found.engine_path, complete)
        }
        None => Located::not_installed(entry),
    }
}

/// Locate one app among Windows uninstall entries.
pub fn locate_windows(entries: &[UninstallEntry], host: &dyn Host, entry: &AppEntry) -> Located {
    let engine = entry.engine_name();
    // Rows for this product only, matched trimmed and case-insensitively.
    let rows: Vec<UninstallEntry> = entries
        .iter()
        .filter(|e| e.display_name.trim().eq_ignore_ascii_case(&entry.product))
        .map(|e| UninstallEntry {
            display_name: entry.product.clone(),
            ..e.clone()
        })
        .collect();
    // A row that names an install directory counts as installed even when its engine is missing
    // (`engine_missing` in the apps report). The directory is not probed, so a stale uninstall
    // row still reports installed.
    let options = DiscoverOptions {
        include_incomplete: true,
    };
    match discover_windows_with(
        &rows,
        &[(entry.product.as_str(), engine.as_str())],
        |p| host.is_file(p),
        |_| true,
        &options,
    )
    .into_iter()
    .next()
    {
        Some(found) => {
            let complete = found.completeness == Completeness::Complete;
            Located::from_found(entry, found.app_path, found.engine_path, complete)
        }
        None => Located::not_installed(entry),
    }
}

/// Locate every app for `os` ("macos" or "windows"; other platforms report every app as not
/// installed), with explicit macOS `roots` and an injectable [`Host`].
pub fn discover_with(
    os: &str,
    roots: &[PathBuf],
    host: &dyn Host,
    apps: &[AppEntry],
) -> Vec<Located> {
    let uninstall: Vec<UninstallEntry> = if os == "windows" {
        host.registry_entries()
            .into_iter()
            .map(to_uninstall_entry)
            .collect()
    } else {
        Vec::new()
    };
    apps.iter()
        .map(|entry| match os {
            "macos" => locate_macos(roots, entry),
            "windows" => locate_windows(&uninstall, host, entry),
            _ => Located::not_installed(entry),
        })
        .collect()
}

/// Locate every app on this machine through the daemon's per-OS install locations.
pub fn discover_installed(apps: &[AppEntry]) -> Vec<Located> {
    let engines: Vec<String> = apps.iter().map(AppEntry::engine_name).collect();
    let registry: Vec<(&str, &str)> = apps
        .iter()
        .zip(&engines)
        .map(|(entry, engine)| (entry.product.as_str(), engine.as_str()))
        .collect();
    let found = rightkit_daemon::discover_apps_in_with(
        &registry,
        &DiscoverOptions {
            include_incomplete: true,
        },
    );
    apps.iter()
        .map(|entry| {
            let installed = found.iter().find(|f| f.app == entry.product);
            Located {
                app: entry.id.clone(),
                product: entry.product.clone(),
                installed: installed.is_some(),
                app_path: installed.map(|f| f.app_path.clone()),
                engine_path: installed
                    .filter(|f| f.completeness == Completeness::Complete)
                    .map(|f| f.engine_path.clone()),
            }
        })
        .collect()
}

/// The real filesystem. It reads no registry: [`discover_installed`] is the real discovery.
pub struct RealHost;

impl Host for RealHost {
    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn registry_entries(&self) -> Vec<RegEntry> {
        Vec::new()
    }
}

fn doctor_probe() -> &'static DoctorProbe {
    static PROBE: OnceLock<DoctorProbe> = OnceLock::new();
    PROBE.get_or_init(DoctorProbe::default)
}

/// The `apps` command payload: every app in `apps`, located on this machine, with a doctor probe of
/// each found engine unless `dry_run`.
pub fn apps_report(apps: &[AppEntry], dry_run: bool) -> Value {
    report_locations(discover_installed(apps), dry_run)
}

/// The `apps` command payload for already located apps.
pub fn report_locations(located: Vec<Located>, dry_run: bool) -> Value {
    let apps: Vec<Value> = located
        .into_iter()
        .map(|l| {
            let (doctor, error) = match (&l.engine_path, l.installed, dry_run) {
                (Some(engine), _, false) => match doctor_probe().probe(engine) {
                    Ok(d) => (Some(d), None),
                    Err(e) => (None, Some(e)),
                },
                (Some(_), _, true) => (None, None),
                (None, true, _) => (
                    None,
                    Some(JobError::new(
                        "engine_missing",
                        format!("{} is installed but has no {}-engine", l.product, l.app),
                    )),
                ),
                (None, false, _) => (None, None),
            };
            json!({
                "app": l.app,
                "product": l.product,
                "installed": l.installed,
                "app_path": l.app_path,
                "engine_path": l.engine_path,
                "engine_found": l.engine_found(),
                "doctor": doctor,
                "error": error.map(|e| json!({"code": e.code, "message": e.message})),
            })
        })
        .collect();
    json!({"apps": apps})
}
