use crate::error::{BrowserError, Result};
use crate::policy::{AdmissionHook, EventSink, NetworkPolicy};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the browser keeps cookies, storage, and logins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileSpec {
    /// Throwaway profile deleted when the session drops (default).
    Temporary,
    /// Named persistent profile at `<root>/<name>`. One live session per name;
    /// a second launch fails with `ProfileInUse` instead of corrupting state.
    Named { root: PathBuf, name: String },
    /// Caller-owned, existing absolute user-data directory. Never created or deleted
    /// by the session; no named-profile validation or real-profile exclusion applies.
    Directory(PathBuf),
}

#[derive(Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    /// Passes `--mute-audio`. Default true.
    pub mute_audio: bool,
    pub profile: ProfileSpec,
    /// Explicit browser executable. When `None`, launch resolves Chrome for Testing with
    /// [`chrome_for_testing_path`]. Ignored by [`Self::system_chrome_only`].
    pub chrome_path: Option<PathBuf>,
    /// Opt-in: when Chrome for Testing is not installed, fall back to the system browser
    /// ([`find_chrome`]). Off by default: regular Google Chrome on macOS leaves an APFS
    /// code-sign clone of its bundle behind for every instance that does not shut down cleanly.
    pub allow_system_chrome: bool,
    /// Resolve only through [`find_chrome`], ignoring `chrome_path` and Chrome for
    /// Testing discovery. Off by default; enabled by [`Self::system_chrome_only`].
    pub use_system_chrome_only: bool,
    pub viewport: (u32, u32),
    pub launch_timeout: Duration,
    /// Deadline for each CDP request, including `Page.navigate`. Default 30 s (chromiumoxide's).
    /// Raise it for dev servers whose first load is slow (e.g. Vite's cold dependency optimizer).
    pub request_timeout: Duration,
    /// Extra Chrome flags, with or without leading `--`.
    pub extra_args: Vec<String>,
    /// Defaults to `<profile>/downloads` for temporary profiles and
    /// `<profile>/rightkit-downloads` for persistent ones.
    pub download_dir: Option<PathBuf>,
    /// When set, `upload` refuses files outside this root.
    pub upload_root: Option<PathBuf>,
    /// Gates every navigation, input, and script action before it has any effect.
    /// `None` admits everything (trusted callers).
    pub admission: Option<AdmissionHook>,
    /// Network/SSRF policy. Default blocks loopback, link-local, private ranges,
    /// `localhost`, and `file:` unless allowed.
    pub network: NetworkPolicy,
    /// Receives start/stop/crash/denied lifecycle events (session id, reason, time).
    pub on_event: Option<EventSink>,
    /// Windows: the caller's job object (see [`Self::windows_job`]). Chromium and every
    /// helper it starts are bound to it; a clone of these options keeps the duplicate open.
    #[cfg(windows)]
    pub windows_job: Option<std::sync::Arc<std::os::windows::io::OwnedHandle>>,
}

impl std::fmt::Debug for LaunchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchOptions")
            .field("headless", &self.headless)
            .field("profile", &self.profile)
            .field("network", &self.network)
            .field("admission", &self.admission.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            headless: true,
            mute_audio: true,
            profile: ProfileSpec::Temporary,
            chrome_path: None,
            allow_system_chrome: false,
            use_system_chrome_only: false,
            viewport: (1280, 800),
            launch_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(30),
            extra_args: Vec::new(),
            download_dir: None,
            upload_root: None,
            admission: None,
            network: NetworkPolicy::default(),
            on_event: None,
            #[cfg(windows)]
            windows_job: None,
        }
    }
}

impl LaunchOptions {
    /// Opt in to the system Chrome/Chromium/Edge when Chrome for Testing is missing.
    pub fn system_chrome(mut self) -> Self {
        self.allow_system_chrome = true;
        self.use_system_chrome_only = false;
        self
    }
    /// Use only system Chrome/Chromium/Edge via [`find_chrome`]. Ignores explicit
    /// `chrome_path` and Chrome for Testing installs/environment overrides; errors
    /// with [`BrowserError::ChromeNotFound`] when no system browser is found.
    pub fn system_chrome_only(mut self) -> Self {
        self.use_system_chrome_only = true;
        self.allow_system_chrome = false;
        self
    }
    pub fn headed(mut self) -> Self {
        self.headless = false;
        self
    }
    pub fn admission(mut self, hook: AdmissionHook) -> Self {
        self.admission = Some(hook);
        self
    }
    /// Per-request CDP deadline (navigation included).
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
    pub fn network(mut self, policy: NetworkPolicy) -> Self {
        self.network = policy;
        self
    }
    pub fn on_event(
        mut self,
        f: impl Fn(&crate::policy::BrowserEvent) + Send + Sync + 'static,
    ) -> Self {
        self.on_event = Some(std::sync::Arc::new(f));
        self
    }
    /// Windows: bind Chromium to the caller's job object (an in-process host's own job).
    /// Chromium joins it before running its first instruction, beneath RightKit's own
    /// kill-on-close jobs, none of which allow breakaway: terminating or closing the
    /// caller's job kills the browser and all of its helpers. The handle is duplicated; the
    /// session closes its duplicate as soon as Chromium has been bound.
    #[cfg(windows)]
    pub fn windows_job(
        mut self,
        job: std::os::windows::io::BorrowedHandle<'_>,
    ) -> std::io::Result<Self> {
        self.windows_job = Some(std::sync::Arc::new(job.try_clone_to_owned()?));
        Ok(self)
    }
    pub fn named_profile(mut self, root: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        self.profile = ProfileSpec::Named {
            root: root.into(),
            name: name.into(),
        };
        self
    }
    /// Use an existing absolute Chrome user-data directory, including paths with
    /// spaces or other filesystem-supported characters. Validated at launch; never
    /// created or deleted. The caller owns exclusive access to this directory.
    pub fn profile_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.profile = ProfileSpec::Directory(path.into());
        self
    }
}

pub(crate) fn validate_profile_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(BrowserError::Profile(format!(
            "profile directory must be absolute: {}",
            path.display()
        )));
    }
    let metadata = std::fs::metadata(path).map_err(|e| {
        BrowserError::Profile(format!(
            "profile directory must exist and be accessible: {} ({e})",
            path.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(BrowserError::Profile(format!(
            "profile path is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(BrowserError::Profile(format!(
            "invalid profile name '{name}'"
        )))
    }
}

/// Environment variable naming an explicit Chrome for Testing executable.
pub const CHROME_FOR_TESTING_ENV: &str = "RIGHTKIT_CHROME_FOR_TESTING";

/// Override the RightKit-managed Chrome for Testing version root.
pub const CHROME_FOR_TESTING_ROOT_ENV: &str = "RIGHTKIT_CHROME_FOR_TESTING_ROOT";

/// How to get Chrome for Testing; part of [`BrowserError::ChromeForTestingNotFound`].
pub const CHROME_FOR_TESTING_INSTALL_HINT: &str = "Install Chrome for Testing (any one of):\n  \
pnpm dlx @puppeteer/browsers install chrome@stable --path ~/.cache/puppeteer\n  \
pnpm exec playwright install chromium\n\
or point RIGHTKIT_CHROME_FOR_TESTING at its executable. Regular Google Chrome is used only \
with LaunchOptions::system_chrome(): on macOS each killed instance leaks a code-sign clone.";

const MAC_CFT_APP: &str = "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Children of `root` whose names start with one of `prefixes`, newest version first.
fn versioned_children(root: &Path, prefixes: &[&str]) -> Vec<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| prefixes.iter().any(|p| n.starts_with(p)))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by(|a, b| version_key(b).cmp(&version_key(a)).then_with(|| b.cmp(a)));
    names.into_iter().map(|n| root.join(n)).collect()
}

fn managed_chrome_for_testing_root(
    home: &Path,
    env_path: &impl Fn(&str) -> Option<PathBuf>,
) -> PathBuf {
    if let Some(root) = env_path(CHROME_FOR_TESTING_ROOT_ENV) {
        return root;
    }
    let app_data = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        env_path("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"))
    } else {
        env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"))
    };
    app_data.join("Orthic Labs/shared/chrome-for-testing")
}

fn managed_chrome_for_testing_candidates(root: &Path) -> Vec<PathBuf> {
    // Match qa-chrome's dotted numeric version folders; ignore staging/metadata entries.
    versioned_children(root, &[""])
        .into_iter()
        .filter(|dir| {
            dir.is_dir()
                && dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| {
                        name.contains('.')
                            && name.split('.').all(|part| {
                                !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                            })
                    })
        })
        .map(|dir| {
            if cfg!(target_os = "macos") {
                let sub = if cfg!(target_arch = "aarch64") {
                    "chrome-mac-arm64"
                } else {
                    "chrome-mac-x64"
                };
                dir.join(sub).join(MAC_CFT_APP)
            } else if cfg!(target_os = "windows") {
                dir.join("chrome-win64/chrome.exe")
            } else {
                dir.join("chrome-linux64/chrome")
            }
        })
        .collect()
}

/// Known Chrome for Testing install locations, in resolution order: RightKit-managed
/// versions (`RIGHTKIT_CHROME_FOR_TESTING_ROOT` overrides the root), app installs, then
/// Puppeteer's cache (`PUPPETEER_CACHE_DIR`), then Playwright's (`PLAYWRIGHT_BROWSERS_PATH`),
/// newest version first within each cache.
pub fn chrome_for_testing_candidates() -> Vec<PathBuf> {
    let home = home_dir().unwrap_or_default();
    let env_path = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let puppeteer = env_path("PUPPETEER_CACHE_DIR")
        .unwrap_or_else(|| home.join(".cache").join("puppeteer"))
        .join("chrome");
    let managed_root = managed_chrome_for_testing_root(&home, &env_path);
    let mut out = managed_chrome_for_testing_candidates(&managed_root);
    if cfg!(target_os = "macos") {
        out.push(Path::new("/Applications").join(MAC_CFT_APP));
        out.push(home.join("Applications").join(MAC_CFT_APP));
        let subs: [&str; 2] = if cfg!(target_arch = "aarch64") {
            ["chrome-mac-arm64", "chrome-mac-x64"]
        } else {
            ["chrome-mac-x64", "chrome-mac-arm64"]
        };
        let playwright = env_path("PLAYWRIGHT_BROWSERS_PATH")
            .unwrap_or_else(|| home.join("Library/Caches/ms-playwright"));
        for dir in versioned_children(&puppeteer, &["mac_arm-", "mac-"])
            .into_iter()
            .chain(versioned_children(&playwright, &["chromium-"]))
        {
            for sub in subs {
                out.push(dir.join(sub).join(MAC_CFT_APP));
            }
        }
    } else if cfg!(target_os = "windows") {
        let local = env_path("LOCALAPPDATA").unwrap_or_else(|| home.join(r"AppData\Local"));
        for dir in versioned_children(&puppeteer, &["win64-", "win32-"]) {
            out.push(dir.join("chrome-win64").join("chrome.exe"));
            out.push(dir.join("chrome-win32").join("chrome.exe"));
        }
        let playwright =
            env_path("PLAYWRIGHT_BROWSERS_PATH").unwrap_or_else(|| local.join("ms-playwright"));
        for dir in versioned_children(&playwright, &["chromium-"]) {
            out.push(dir.join("chrome-win64").join("chrome.exe"));
        }
    } else {
        for dir in versioned_children(&puppeteer, &["linux-", "linux_arm-"]) {
            out.push(dir.join("chrome-linux64").join("chrome"));
            out.push(dir.join("chrome-linux-arm64").join("chrome"));
        }
        let playwright = env_path("PLAYWRIGHT_BROWSERS_PATH")
            .unwrap_or_else(|| home.join(".cache").join("ms-playwright"));
        for dir in versioned_children(&playwright, &["chromium-"]) {
            out.push(dir.join("chrome-linux64").join("chrome"));
        }
    }
    out
}

/// Resolve Chrome for Testing: `explicit`, then `RIGHTKIT_CHROME_FOR_TESTING`, then
/// [`chrome_for_testing_candidates`]. Errors with install instructions; never returns
/// regular Chrome.
pub fn chrome_for_testing_path(explicit: Option<&Path>) -> Result<PathBuf> {
    let missing = |what: String| {
        BrowserError::ChromeForTestingNotFound(format!("{what}\n{CHROME_FOR_TESTING_INSTALL_HINT}"))
    };
    if let Some(p) = explicit {
        return if p.exists() {
            Ok(p.to_path_buf())
        } else {
            Err(missing(format!(
                "explicit Chrome for Testing executable does not exist: {}",
                p.display()
            )))
        };
    }
    if let Some(p) = std::env::var_os(CHROME_FOR_TESTING_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return if p.exists() {
            Ok(p)
        } else {
            Err(missing(format!(
                "{CHROME_FOR_TESTING_ENV} points at a missing file: {}",
                p.display()
            )))
        };
    }
    let candidates = chrome_for_testing_candidates();
    if let Some(p) = candidates.iter().find(|p| p.exists()) {
        return Ok(p.clone());
    }
    let searched: Vec<String> = candidates
        .iter()
        .map(|p| format!("  {}", p.display()))
        .collect();
    Err(missing(format!(
        "Chrome for Testing is not installed; searched:\n{}",
        if searched.is_empty() {
            "  (nothing)".to_string()
        } else {
            searched.join("\n")
        }
    )))
}

/// System-only selection takes priority; otherwise `chrome_path` as given, then Chrome
/// for Testing, then (only with [`LaunchOptions::system_chrome`]) the system browser.
pub(crate) fn resolve_executable(opts: &LaunchOptions) -> Result<PathBuf> {
    resolve_executable_with(opts, || chrome_for_testing_path(None), find_chrome)
}

fn resolve_executable_with(
    opts: &LaunchOptions,
    cft: impl FnOnce() -> Result<PathBuf>,
    system: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf> {
    if opts.use_system_chrome_only {
        return system().ok_or(BrowserError::ChromeNotFound);
    }
    if let Some(p) = &opts.chrome_path {
        return Ok(p.clone());
    }
    match cft() {
        Ok(p) => Ok(p),
        Err(e) if opts.allow_system_chrome => system().ok_or(e),
        Err(e) => Err(e),
    }
}

/// System Chrome, Chromium, then Edge. `CHROME` env overrides. Launch uses this only with
/// [`LaunchOptions::system_chrome`] or [`LaunchOptions::system_chrome_only`];
/// prefer [`chrome_for_testing_path`] for isolated automation.
pub fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CHROME") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
        ]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| Path::new(p).exists())
}

/// Chrome flags a caller may not pass: they would redirect the profile to the
/// user's real browser data or expose the debugging endpoint outside this session.
pub(crate) fn validate_extra_args(args: &[String]) -> Result<()> {
    const FORBIDDEN: &[&str] = &[
        "user-data-dir",
        "profile-directory",
        "remote-debugging-port",
        "remote-debugging-address",
        "remote-debugging-pipe",
        "remote-allow-origins",
        "disable-web-security",
        "incognito-bypass",
    ];
    for a in args {
        let name = a.trim_start_matches('-').split('=').next().unwrap_or("");
        if FORBIDDEN.contains(&name) {
            return Err(BrowserError::Invalid(format!(
                "chrome flag '--{name}' is managed by the session"
            )));
        }
    }
    Ok(())
}

/// Refuse roots that are (or sit inside) a real browser's own user-data directory.
pub(crate) fn reject_real_browser_profile(root: &Path) -> Result<()> {
    let mut real: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        for rel in [
            "Library/Application Support/Google/Chrome",
            "Library/Application Support/Chromium",
            "Library/Application Support/Microsoft Edge",
            ".config/google-chrome",
            ".config/chromium",
            ".config/microsoft-edge",
        ] {
            real.push(home.join(rel));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for rel in [
            r"Google\Chrome\User Data",
            r"Chromium\User Data",
            r"Microsoft\Edge\User Data",
        ] {
            real.push(local.join(rel));
        }
    }
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    for r in real {
        if let Ok(r) = r.canonicalize() {
            if root.starts_with(&r) {
                return Err(BrowserError::Profile(
                    "refusing to use the user's real browser profile".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    // Serialize resolver tests that change process environment; restore even on panic.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(values: &[(&'static str, Option<&Path>)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = values
                .iter()
                .map(|(key, _)| (*key, std::env::var_os(key)))
                .collect();
            for (key, value) in values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn write_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture, not a browser").unwrap();
    }

    fn managed_executable(root: &Path, version: &str) -> PathBuf {
        let relative = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
            } else {
                "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
            }
        } else if cfg!(target_os = "windows") {
            "chrome-win64/chrome.exe"
        } else {
            "chrome-linux64/chrome"
        };
        root.join(version).join(relative)
    }

    #[test]
    fn system_chrome_only_ignores_explicit_env_and_managed_cft() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("managed CfT");
        let managed = managed_executable(&root, "130.10.0.0");
        let explicit = temp.path().join("explicit CfT");
        let system = temp.path().join("system Chrome");
        for path in [&managed, &explicit, &system] {
            write_executable(path);
        }
        let _env = EnvGuard::new(&[
            (CHROME_FOR_TESTING_ROOT_ENV, Some(&root)),
            (CHROME_FOR_TESTING_ENV, Some(&managed)),
            ("CHROME", Some(&system)),
        ]);
        assert_eq!(chrome_for_testing_path(None).unwrap(), managed);
        let opts = LaunchOptions {
            chrome_path: Some(explicit),
            ..LaunchOptions::default().system_chrome_only()
        };
        assert_eq!(resolve_executable(&opts).unwrap(), system);
        assert_eq!(find_chrome().unwrap(), system);
        // Also prove discovery is never called, regardless of installed CfT versions.
        assert_eq!(
            resolve_executable_with(
                &opts,
                || panic!("CfT discovery called"),
                || Some(system.clone()),
            )
            .unwrap(),
            system
        );
    }

    #[test]
    fn system_chrome_only_errors_when_system_browser_is_missing() {
        let opts = LaunchOptions::default().system_chrome_only();
        let err =
            resolve_executable_with(&opts, || panic!("CfT discovery called"), || None).unwrap_err();
        assert!(matches!(&err, BrowserError::ChromeNotFound));
        assert_eq!(
            err.to_string(),
            "no Chrome, Chromium, or Edge executable found"
        );
    }

    #[test]
    fn defaults_and_system_chrome_preserve_cft_preference_and_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let cft = temp.path().join("CfT");
        let system = temp.path().join("system Chrome");
        write_executable(&cft);
        write_executable(&system);
        let defaults = LaunchOptions::default();
        assert!(defaults.headless && defaults.mute_audio);
        assert!(!defaults.allow_system_chrome && !defaults.use_system_chrome_only);
        for opts in [defaults.clone(), defaults.clone().system_chrome()] {
            assert_eq!(
                resolve_executable_with(
                    &opts,
                    || Ok(cft.clone()),
                    || panic!("system lookup called"),
                )
                .unwrap(),
                cft
            );
        }
        assert!(matches!(
            resolve_executable_with(
                &defaults,
                || Err(BrowserError::ChromeForTestingNotFound("missing".into())),
                || panic!("default must not fall back"),
            ),
            Err(BrowserError::ChromeForTestingNotFound(_))
        ));
        let fallback = defaults.system_chrome_only().system_chrome();
        assert!(!fallback.use_system_chrome_only);
        assert_eq!(
            resolve_executable_with(
                &fallback,
                || Err(BrowserError::ChromeForTestingNotFound("missing".into())),
                || Some(system.clone()),
            )
            .unwrap(),
            system
        );
    }

    #[test]
    fn managed_root_uses_platform_data_directory_and_env_override() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let suffix = Path::new("Orthic Labs/shared/chrome-for-testing");
        let default_data = if cfg!(target_os = "macos") {
            home.join("Library/Application Support")
        } else if cfg!(target_os = "windows") {
            home.join("AppData/Local")
        } else {
            home.join(".local/share")
        };
        assert_eq!(
            managed_chrome_for_testing_root(home, &|_| None),
            default_data.join(suffix)
        );
        let data = home.join("custom app data");
        let with_data = |key: &str| {
            if key == "LOCALAPPDATA" || key == "XDG_DATA_HOME" {
                Some(data.clone())
            } else {
                None
            }
        };
        let expected_data = if cfg!(target_os = "macos") {
            default_data
        } else {
            data.clone()
        };
        assert_eq!(
            managed_chrome_for_testing_root(home, &with_data),
            expected_data.join(suffix)
        );
        let override_root = home.join("custom managed root");
        assert_eq!(
            managed_chrome_for_testing_root(home, &|key| {
                if key == CHROME_FOR_TESTING_ROOT_ENV {
                    Some(override_root.clone())
                } else {
                    None
                }
            }),
            override_root
        );
    }

    #[test]
    fn managed_folder_wins_with_newest_numeric_version_first() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("managed Chrome for Testing");
        let puppeteer = temp.path().join("puppeteer");
        let older = managed_executable(&root, "129.99.0.0");
        let recent = managed_executable(&root, "130.2.0.0");
        let newest = managed_executable(&root, "130.10.0.0");
        let legacy_dir = if cfg!(target_os = "macos") {
            "mac-999.0.0.0"
        } else if cfg!(target_os = "windows") {
            "win64-999.0.0.0"
        } else {
            "linux-999.0.0.0"
        };
        let legacy = managed_executable(&puppeteer.join("chrome"), legacy_dir);
        for path in [&older, &recent, &newest, &legacy] {
            write_executable(path);
        }
        std::fs::create_dir(root.join("999.staging")).unwrap();
        std::fs::write(root.join("999.0.0.0"), "not a version directory").unwrap();
        let _env = EnvGuard::new(&[
            (CHROME_FOR_TESTING_ROOT_ENV, Some(&root)),
            (CHROME_FOR_TESTING_ENV, None),
            ("PUPPETEER_CACHE_DIR", Some(&puppeteer)),
        ]);
        let candidates = chrome_for_testing_candidates();
        assert_eq!(&candidates[..3], &[newest.clone(), recent.clone(), older]);
        assert!(candidates.iter().position(|p| p == &legacy).unwrap() >= 3);
        assert_eq!(chrome_for_testing_path(None).unwrap(), newest);
        std::env::set_var(CHROME_FOR_TESTING_ENV, &legacy);
        assert_eq!(chrome_for_testing_path(None).unwrap(), legacy);
        std::env::remove_var(CHROME_FOR_TESTING_ENV);
        std::fs::remove_file(&newest).unwrap();
        assert_eq!(chrome_for_testing_path(None).unwrap(), recent);
        // Explicit executable selection retains priority over managed discovery.
        assert_eq!(chrome_for_testing_path(Some(&legacy)).unwrap(), legacy);
    }
}
