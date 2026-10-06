use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("no Chrome, Chromium, or Edge executable found")]
    ChromeNotFound,
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
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BrowserError>;

pub(crate) fn cdp<E: std::fmt::Display>(e: E) -> BrowserError {
    BrowserError::Cdp(e.to_string())
}
