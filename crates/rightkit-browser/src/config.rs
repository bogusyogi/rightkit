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
}

#[derive(Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    /// Passes `--mute-audio`. Default true.
    pub mute_audio: bool,
    pub profile: ProfileSpec,
    /// Explicit browser executable. When `None`, launch resolves Chrome for Testing with
    /// [`chrome_for_testing_path`].
    pub chrome_path: Option<PathBuf>,
    /// Opt-in: when Chrome for Testing is not installed, fall back to the system browser
    /// ([`find_chrome`]). Off by default: regular Google Chrome on macOS leaves an APFS
    /// code-sign clone of its bundle behind for every instance that does not shut down cleanly.
    pub allow_system_chrome: bool,
    pub viewport: (u32, u32),
    pub launch_timeout: Duration,
    /// Extra Chrome flags, with or without leading `--`.
    pub extra_args: Vec<String>,
    /// Defaults to `<profile>/downloads` for temporary profiles and
    /// `<profile>/rightkit-downloads` for named ones.
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
            viewport: (1280, 800),
            launch_timeout: Duration::from_secs(60),
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

/// Known Chrome for Testing install locations, in resolution order: app installs, then
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
    let mut out = Vec::new();
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

/// The executable a launch uses: `opts.chrome_path` as given, else Chrome for Testing, else
/// (only with [`LaunchOptions::system_chrome`]) the system browser.
pub(crate) fn resolve_executable(opts: &LaunchOptions) -> Result<PathBuf> {
    if let Some(p) = &opts.chrome_path {
        return Ok(p.clone());
    }
    match chrome_for_testing_path(None) {
        Ok(p) => Ok(p),
        Err(e) if opts.allow_system_chrome => find_chrome().ok_or(e),
        Err(e) => Err(e),
    }
}

/// System Chrome, Chromium, then Edge. `CHROME` env overrides. Launch uses this only with
/// [`LaunchOptions::system_chrome`]; prefer [`chrome_for_testing_path`].
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
