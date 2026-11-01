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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_job_queue::{parse_doctor, run_doctor};
    use std::collections::HashSet;
    use std::time::Duration;

    #[derive(Default)]
    struct Fake {
        files: HashSet<String>,
        reg: Vec<RegEntry>,
    }

    impl Host for Fake {
        fn is_file(&self, p: &str) -> bool {
            self.files.contains(p)
        }
        fn registry_entries(&self) -> Vec<RegEntry> {
            self.reg.clone()
        }
    }

    fn apps() -> Vec<AppEntry> {
        vec![
            AppEntry::new("alpha", "Alpha"),
            AppEntry::new("delta", "Delta"),
            AppEntry::new("beta", "Beta"),
            AppEntry::new("gamma", "Gamma"),
        ]
    }

    fn find<'a>(v: &'a [Located], app: &str) -> &'a Located {
        v.iter().find(|l| l.app == app).unwrap()
    }

    /// Create an empty file, and its parent directories, under a temp root.
    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    /// Create an app bundle directory without an engine binary.
    fn bundle(root: &Path, product: &str) -> PathBuf {
        let b = root.join(format!("{product}.app"));
        std::fs::create_dir_all(&b).unwrap();
        b
    }

    #[test]
    fn mac_finds_complete_bundle_in_system_root() {
        let d = tempfile::tempdir().unwrap();
        let sys = d.path().join("Applications");
        let engine = sys.join("Beta.app/Contents/MacOS/beta-engine");
        touch(&engine);
        let r = discover_with(
            "macos",
            &[sys.clone(), d.path().join("home/Applications")],
            &RealHost,
            &apps(),
        );
        let g = find(&r, "beta");
        assert!(g.installed && g.engine_found());
        assert_eq!(g.engine_path.as_deref().map(PathBuf::from), Some(engine));
        assert_eq!(
            g.app_path.as_deref().map(PathBuf::from),
            Some(sys.join("Beta.app"))
        );
        assert!(!find(&r, "gamma").installed);
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn mac_user_applications_and_missing_engine() {
        let d = tempfile::tempdir().unwrap();
        let sys = d.path().join("Applications");
        let user = d.path().join("home/Applications");
        touch(&user.join("Delta.app/Contents/MacOS/delta-engine"));
        bundle(&sys, "Alpha");
        let r = discover_with("macos", &[sys.clone(), user], &RealHost, &apps());
        assert!(find(&r, "delta").engine_found());
        let hr = find(&r, "alpha");
        assert!(hr.installed && !hr.engine_found());
        assert_eq!(
            hr.app_path.as_deref().map(PathBuf::from),
            Some(sys.join("Alpha.app"))
        );
    }

    #[test]
    fn mac_prefers_root_with_engine() {
        let d = tempfile::tempdir().unwrap();
        let sys = d.path().join("Applications");
        let user = d.path().join("home/Applications");
        bundle(&sys, "Gamma");
        touch(&user.join("Gamma.app/Contents/MacOS/gamma-engine"));
        let r = discover_with("macos", &[sys, user.clone()], &RealHost, &apps());
        assert_eq!(
            find(&r, "gamma").app_path.as_deref().map(PathBuf::from),
            Some(user.join("Gamma.app"))
        );
    }

    #[test]
    fn mac_never_uses_path_or_checkout() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("usr/local/bin/beta-engine"));
        touch(&d.path().join("repo/target/debug/beta-engine"));
        // Only the given install roots are ever consulted.
        let r = discover_with(
            "macos",
            &[d.path().join("Applications")],
            &RealHost,
            &apps(),
        );
        assert!(r.iter().all(|l| !l.installed && !l.engine_found()));
    }

    fn entry(name: &str, loc: Option<&str>, icon: Option<&str>, un: Option<&str>) -> RegEntry {
        RegEntry {
            display_name: name.into(),
            install_location: loc.map(Into::into),
            display_icon: icon.map(Into::into),
            uninstall_string: un.map(Into::into),
        }
    }

    #[test]
    fn windows_install_location_with_quotes() {
        let mut h = Fake::default();
        h.reg.push(entry(
            "Beta",
            Some("\"C:\\Users\\a\\AppData\\Local\\Beta\""),
            None,
            None,
        ));
        h.files
            .insert("C:\\Users\\a\\AppData\\Local\\Beta\\beta-engine.exe".into());
        let r = discover_with("windows", &[], &h, &apps());
        let g = find(&r, "beta");
        assert_eq!(
            g.engine_path.as_deref(),
            Some("C:\\Users\\a\\AppData\\Local\\Beta\\beta-engine.exe")
        );
        assert_eq!(
            g.app_path.as_deref(),
            Some("C:\\Users\\a\\AppData\\Local\\Beta")
        );
    }

    #[test]
    fn windows_falls_back_to_icon_then_uninstall_dir() {
        let mut h = Fake::default();
        h.reg.push(entry(
            "Alpha",
            None,
            Some("C:\\Apps\\Alpha\\alpha_next_bin.exe,0"),
            None,
        ));
        h.reg.push(entry(
            "Delta",
            Some(""),
            None,
            Some("\"C:\\Apps\\Delta\\uninstall.exe\" /S"),
        ));
        h.files.insert("C:\\Apps\\Alpha\\alpha-engine.exe".into());
        h.files.insert("C:\\Apps\\Delta\\delta-engine.exe".into());
        let r = discover_with("windows", &[], &h, &apps());
        assert!(find(&r, "alpha").engine_found());
        assert!(find(&r, "delta").engine_found());
    }

    #[test]
    fn windows_unquoted_uninstall_with_args() {
        let mut h = Fake::default();
        h.reg.push(entry(
            "Gamma",
            None,
            None,
            Some("C:\\Program Files\\Gamma\\unins000.exe /SILENT"),
        ));
        h.files
            .insert("C:\\Program Files\\Gamma\\gamma-engine.exe".into());
        let r = discover_with("windows", &[], &h, &apps());
        let c = find(&r, "gamma");
        assert!(c.engine_found());
        assert_eq!(c.app_path.as_deref(), Some("C:\\Program Files\\Gamma"));
    }

    #[test]
    fn windows_missing_engine_and_not_installed() {
        let mut h = Fake::default();
        h.reg.push(entry("Gamma", Some("C:\\Gamma"), None, None));
        h.reg
            .push(entry("Gamma Helper", Some("C:\\Other"), None, None));
        let r = discover_with("windows", &[], &h, &apps());
        let c = find(&r, "gamma");
        assert!(c.installed && !c.engine_found());
        assert_eq!(c.app_path.as_deref(), Some("C:\\Gamma"));
        assert!(c.engine_path.is_none());
        assert!(!find(&r, "beta").installed);
    }

    #[test]
    fn windows_ignores_path_style_engines() {
        let mut h = Fake::default();
        h.files.insert("C:\\tools\\beta-engine.exe".into());
        let r = discover_with("windows", &[], &h, &apps());
        assert!(!find(&r, "beta").installed);
    }

    #[test]
    fn windows_first_entry_with_engine_wins() {
        let mut h = Fake::default();
        h.reg.push(entry("Beta", Some("C:\\Stale"), None, None));
        h.reg.push(entry("Beta", Some("C:\\Good"), None, None));
        h.files.insert("C:\\Good\\beta-engine.exe".into());
        let r = discover_with("windows", &[], &h, &apps());
        assert_eq!(find(&r, "beta").app_path.as_deref(), Some("C:\\Good"));
    }

    #[test]
    fn unsupported_os_reports_nothing_installed() {
        let r = discover_with("linux", &[], &Fake::default(), &apps());
        assert!(r.iter().all(|l| !l.installed));
    }

    #[test]
    fn parse_doctor_extracts_identity() {
        let d = parse_doctor(
            r#"{"app":"Beta","app_version":"0.3.1","contract_version":1,
                "capabilities":[{"name":"doctor","version":1}],
                "readiness":{"status":"ready"}}"#,
        )
        .unwrap();
        assert_eq!(d["app_version"], "0.3.1");
        assert_eq!(d["contract_version"], 1);
        assert_eq!(d["capabilities"][0]["name"], "doctor");
        assert_eq!(d["readiness"]["status"], "ready");
    }

    #[test]
    fn parse_doctor_rejects_bad_output() {
        assert_eq!(parse_doctor("nope").unwrap_err().code, "doctor_bad_json");
        assert_eq!(parse_doctor("{}").unwrap_err().code, "doctor_bad_shape");
    }

    #[test]
    fn run_doctor_reports_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let e = run_doctor(dir.path().join("missing-engine"), Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.code, "spawn_failed");
    }

    #[test]
    fn apps_report_shape_dry_run_and_missing_engine() {
        let d = tempfile::tempdir().unwrap();
        let sys = d.path().join("Applications");
        bundle(&sys, "Alpha");
        let r = report_locations(discover_with("macos", &[sys], &RealHost, &apps()), true);
        let apps = r["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 4);
        let hr = apps.iter().find(|a| a["app"] == "alpha").unwrap();
        assert_eq!(hr["installed"], true);
        assert_eq!(hr["engine_found"], false);
        assert_eq!(hr["error"]["code"], "engine_missing");
        assert!(hr["doctor"].is_null());
        let cr = apps.iter().find(|a| a["app"] == "gamma").unwrap();
        assert_eq!(cr["installed"], false);
        assert!(cr["error"].is_null() && cr["engine_path"].is_null());
    }
}
