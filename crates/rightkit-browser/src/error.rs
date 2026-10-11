use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("no Chrome, Chromium, or Edge executable found")]
    ChromeNotFound,
    /// Chrome for Testing was not found (or an explicit path is missing); carries what was
    /// searched and how to install it.
    #[error("Chrome for Testing not found: {0}")]
    ChromeForTestingNotFound(String),
    /// Windows: Chrome's sandbox cannot read the browser in `directory` (the access list
    /// lacks read and execute for the AppContainer principals in `principals`), so its
    /// network service would crash at startup and navigations would hang or abort. Nothing
    /// was launched. `fix` is the command that grants the access; an installer that unpacks
    /// Chrome into a directory outside `Program Files` runs it once after unpacking.
    #[error(
        "Chrome's sandbox cannot read the browser in {}: no read and execute access for {}. Grant it with: {fix}",
        .directory.display(),
        .principals.join(" and ")
    )]
    ChromeSandboxAccess {
        directory: std::path::PathBuf,
        principals: Vec<&'static str>,
        fix: String,
    },
    #[error("launch failed: {0}")]
    Launch(String),
    #[error("profile error: {0}")]
    Profile(String),
    #[error("profile '{0}' is already in use by another session")]
    ProfileInUse(String),
    #[error("cdp error: {0}")]
    Cdp(String),
    #[error("stale or foreign ref '{0}'; observe again before acting")]
    StaleRef(String),
    #[error("no element matched '{0}'")]
    NoElement(String),
    #[error("selector '{0}' is ambiguous ({1} matches)")]
    Ambiguous(String, u64),
    #[error("element '{0}' is not visible or has no box")]
    NotInteractable(String),
    #[error("unknown page '{0}'")]
    UnknownPage(String),
    #[error("unknown key '{0}'")]
    UnknownKey(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// The caller's admission hook refused the effect before it happened.
    #[error("denied before effect: {0}")]
    Denied(String),
    /// The network (SSRF/host) policy refused a URL before any request left the machine.
    #[error("blocked by network policy: {0}")]
    Blocked(crate::policy::PolicyDenial),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BrowserError>;

pub(crate) fn cdp<E: std::fmt::Display>(e: E) -> BrowserError {
    BrowserError::Cdp(e.to_string())
}
