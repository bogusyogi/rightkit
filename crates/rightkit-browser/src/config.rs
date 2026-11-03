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
    /// Named persistent profile at `<root>/<name>` seeded, with the user's consent, from the
    /// sign-ins of a Chromium-family browser profile (see [`crate::ImportSpec`]). Locked like
    /// [`Self::Named`]; the browser's own files are only read, never written. Every action of
    /// the session is confined to the consented domains and stops when consent is revoked.
    ImportedFrom {
        root: PathBuf,
        name: String,
        import: crate::real_session::ImportSpec,
    },
    /// Persistent profile kept as one authenticated-encrypted file at rest (feature
    /// `profile-vault`): decrypted into a private working directory when the session starts,
    /// re-sealed and wiped when it stops, recovered after a crash. See [`crate::ProfileVault`].
    #[cfg(feature = "profile-vault")]
    Encrypted(crate::vault::ProfileVault),
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
    /// Show the browser window (`false` is the default headless mode). The action API is the
    /// same in both modes.
    pub fn headless(mut self, on: bool) -> Self {
        self.headless = on;
        self
    }
    /// Same as `headless(false)`.
    pub fn visible(self) -> Self {
        self.headless(false)
    }
    /// Seed a named persistent profile from the user's own browser, with consent. Pair with
    /// the same browser's executable ([`crate::browser_executable`]) so its sign-in encryption
    /// key applies; [`crate::BrowserSession::attach_user_browser`] does both.
    pub fn imported_profile(
        mut self,
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        import: crate::real_session::ImportSpec,
    ) -> Self {
        self.profile = ProfileSpec::ImportedFrom {
            root: root.into(),
            name: name.into(),
            import,
        };
        self
    }
    /// Use an encrypted persistent profile (feature `profile-vault`); see [`crate::ProfileVault`].
    #[cfg(feature = "profile-vault")]
    pub fn encrypted_profile(mut self, vault: crate::vault::ProfileVault) -> Self {
        self.profile = ProfileSpec::Encrypted(vault);
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

/// Override the RightKit-managed Chrome for Testing root (the folder that holds `current`).
pub const CHROME_FOR_TESTING_ROOT_ENV: &str = "RIGHTKIT_CHROME_FOR_TESTING_ROOT";

/// Folder name of the one managed install inside the managed root.
pub const MANAGED_CURRENT_DIR: &str = "current";
/// Folder name of the single rollback copy kept beside [`MANAGED_CURRENT_DIR`].
pub const MANAGED_PREVIOUS_DIR: &str = "current.previous";
/// Folder name the installer extracts into before swapping it into `current`.
#[cfg_attr(not(feature = "install"), allow(dead_code))]
pub const MANAGED_STAGING_DIR: &str = "current.staging";

/// How to get Chrome for Testing; part of [`BrowserError::ChromeForTestingNotFound`].
pub const CHROME_FOR_TESTING_INSTALL_HINT: &str = "Chrome for Testing is put in place by the app's installer or updater step, which calls \
rightkit_browser::install::ensure_chrome_for_testing (cargo feature `install`; pinned version, SHA-256 checked) \
into <app data>/Orthic Labs/shared/chrome-for-testing/current. For development, point \
RIGHTKIT_CHROME_FOR_TESTING at an existing Chrome for Testing executable, or run \
`rightkit-qa-chrome install` from @rightkit/qa-chrome. Regular Google Chrome is used only with \
LaunchOptions::system_chrome(): on macOS each killed instance leaks a code-sign clone.";

/// The RightKit-managed Chrome for Testing folder and its fixed layout: one `current`
/// install, one `current.previous` rollback copy, and `current.staging` while an install runs.
/// No per-version folders and no new id: every app resolves the same path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRoot {
    root: PathBuf,
}

#[cfg_attr(not(feature = "install"), allow(dead_code))]
impl ManagedRoot {
    /// The suite-wide root (`<app data>/Orthic Labs/shared/chrome-for-testing`), or
    /// `RIGHTKIT_CHROME_FOR_TESTING_ROOT` when set.
    pub fn resolve() -> Self {
        let home = home_dir().unwrap_or_default();
        let env_path = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            root: managed_chrome_for_testing_root(&home, &env_path),
        }
    }

    /// An explicit root (tests, relocated installs).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root folder itself.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `<root>/current`: the installed Chrome for Testing.
    pub fn current(&self) -> PathBuf {
        self.root.join(MANAGED_CURRENT_DIR)
    }

    /// `<root>/current.previous`: the one rollback copy.
    pub fn previous(&self) -> PathBuf {
        self.root.join(MANAGED_PREVIOUS_DIR)
    }

    /// `<root>/current.staging`: where an install extracts before it swaps into `current`.
    pub fn staging(&self) -> PathBuf {
        self.root.join(MANAGED_STAGING_DIR)
    }

    /// The Chrome executable inside `current` for this platform.
    pub fn executable(&self) -> PathBuf {
        executable_in(&self.current())
    }
}

/// Chrome for Testing's platform key for this build, as Google names it.
pub(crate) fn platform_key() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "mac-arm64"
        } else {
            "mac-x64"
        }
    } else if cfg!(target_os = "windows") {
        "win64"
    } else {
        "linux64"
    }
}

/// The Chrome executable inside one extracted Chrome for Testing folder, for this platform.
pub(crate) fn executable_in(dir: &Path) -> PathBuf {
    let sub = format!("chrome-{}", platform_key());
    if cfg!(target_os = "macos") {
        dir.join(sub).join(MAC_CFT_APP)
    } else if cfg!(target_os = "windows") {
        dir.join(sub).join("chrome.exe")
    } else {
        dir.join(sub).join("chrome")
    }
}

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

/// Dotted numeric version folders of the managed root, newest first (qa-chrome's layout);
/// staging/metadata entries are ignored.
fn managed_version_dirs(root: &Path) -> Vec<PathBuf> {
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
        .collect()
}

/// Managed candidates, best first: `current`, then `current.previous` (only reached when an
/// interrupted swap left no `current`), then legacy per-version folders (qa-chrome before the
/// fixed layout), newest first.
fn managed_chrome_for_testing_candidates(root: &Path) -> Vec<PathBuf> {
    let managed = ManagedRoot::at(root);
    let mut out = vec![
        executable_in(&managed.current()),
        executable_in(&managed.previous()),
    ];
    out.extend(
        managed_version_dirs(root)
            .into_iter()
            .map(|dir| executable_in(&dir)),
    );
    out
}

/// Windows fixture only: `chrome-headless-shell` of the same Chrome for Testing version, which
/// never starts the crashpad handler (see [`chrome_headless_shell_path`]).
#[cfg(windows)]
pub(crate) const HEADLESS_SHELL_ENV: &str = "RIGHTKIT_CHROME_HEADLESS_SHELL";
#[cfg(windows)]
const HEADLESS_SHELL_WIN64: &str = "chrome-headless-shell-win64/chrome-headless-shell.exe";

/// Candidate `chrome-headless-shell.exe` paths for the Windows fixture, in resolution order: the
/// sibling of the resolved Chrome (`<version>/chrome-win64/chrome.exe` ->
/// `<dir>/chrome-headless-shell-win64/`), then the managed `current` and `current.previous`, then
/// each legacy managed version, newest first (`RIGHTKIT_CHROME_FOR_TESTING_ROOT` overrides the
/// root, as for Chrome).
#[cfg(windows)]
fn chrome_headless_shell_candidates(chrome: &Path, managed_root: &Path) -> Vec<PathBuf> {
    let managed = ManagedRoot::at(managed_root);
    let mut out = Vec::new();
    if let Some(version_dir) = chrome.parent().and_then(Path::parent) {
        out.push(version_dir.join(HEADLESS_SHELL_WIN64));
    }
    out.push(managed.current().join(HEADLESS_SHELL_WIN64));
    out.push(managed.previous().join(HEADLESS_SHELL_WIN64));
    out.extend(
        managed_version_dirs(managed_root)
            .into_iter()
            .map(|dir| dir.join(HEADLESS_SHELL_WIN64)),
    );
    out
}

/// Resolve `chrome-headless-shell.exe` for the Windows fixture: `RIGHTKIT_CHROME_HEADLESS_SHELL`
/// (must exist), then [`chrome_headless_shell_candidates`]. Chrome's browser process starts the
/// crashpad handler unconditionally, which cannot create its named pipe inside an AppContainer;
/// the headless shell never starts it on Windows. Production launches never call this.
#[cfg(windows)]
pub(crate) fn chrome_headless_shell_path(chrome: &Path) -> Result<PathBuf> {
    let missing = |what: String| {
        BrowserError::ChromeForTestingNotFound(format!(
            "{what}\nThe Windows fixture runs chrome-headless-shell from the same Chrome for Testing \
             version (chrome-headless-shell-win64.zip, unpacked beside chrome-win64, or {HEADLESS_SHELL_ENV})"
        ))
    };
    if let Some(p) = std::env::var_os(HEADLESS_SHELL_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return if p.exists() {
            Ok(p)
        } else {
            Err(missing(format!(
                "{HEADLESS_SHELL_ENV} points at a missing file: {}",
                p.display()
            )))
        };
    }
    let home = home_dir().unwrap_or_default();
    let env_path = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let root = managed_chrome_for_testing_root(&home, &env_path);
    let candidates = chrome_headless_shell_candidates(chrome, &root);
    if let Some(p) = candidates.iter().find(|p| p.exists()) {
        return Ok(p.clone());
    }
    let searched: Vec<String> = candidates
        .iter()
        .map(|p| format!("  {}", p.display()))
        .collect();
    Err(missing(format!(
        "chrome-headless-shell is not installed; searched:\n{}",
        searched.join("\n")
    )))
}

/// Known Chrome for Testing install locations, in resolution order: the RightKit-managed
/// `current` (then `current.previous`, then legacy per-version folders;
/// `RIGHTKIT_CHROME_FOR_TESTING_ROOT` overrides the root), app installs, then
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
    let per_user: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
        ]
    } else {
        &[]
    };
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
        .chain(windows_local_app_candidates(per_user))
        .find(|p| Path::new(p).exists())
}

/// Per-user Windows installs: each relative path under `%LOCALAPPDATA%`, in order. Empty when
/// `LOCALAPPDATA` is unset, and on other platforms (no per-user install layout there).
pub(crate) fn windows_local_app_candidates(relative: &[&str]) -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(local) = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    relative.iter().map(|rel| local.join(rel)).collect()
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
