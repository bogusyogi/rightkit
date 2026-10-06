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
    pub chrome_path: Option<PathBuf>,
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
    /// Receives start/stop/denied lifecycle events.
    pub on_event: Option<EventSink>,
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
            viewport: (1280, 800),
            launch_timeout: Duration::from_secs(60),
            extra_args: Vec::new(),
            download_dir: None,
            upload_root: None,
            admission: None,
            network: NetworkPolicy::default(),
            on_event: None,
        }
    }
}

impl LaunchOptions {
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

/// System Chrome, Chromium, then Edge. `CHROME` env overrides.
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
