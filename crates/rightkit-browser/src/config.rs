use crate::error::{BrowserError, Result};
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

#[derive(Clone, Debug)]
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
        }
    }
}

impl LaunchOptions {
    pub fn headed(mut self) -> Self {
        self.headless = false;
        self
    }
    pub fn named_profile(mut self, root: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        self.profile = ProfileSpec::Named { root: root.into(), name: name.into() };
        self
    }
}

pub(crate) fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(BrowserError::Profile(format!("invalid profile name '{name}'")))
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
        &["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/microsoft-edge"]
    };
    candidates.iter().map(PathBuf::from).find(|p| Path::new(p).exists())
}
