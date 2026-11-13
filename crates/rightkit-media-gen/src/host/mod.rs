//! Host facts the generation layer reads: the app data root, the caller identity, the outbound
//! HTTP policy, diagnostics, the OS credential store, ffmpeg discovery, asset facts and approval
//! receipts.
//!
//! Names come from the host configuration in `rightkit-local-gen`
//! ([`rightkit_local_gen::host::configure`]): the app identifier names the per-user data folder and
//! the credential store service, and the environment prefix names every override
//! (`<PREFIX>_APP_DATA`, `<PREFIX>_CALLER`, ...).

use std::path::PathBuf;

pub use rightkit_local_gen::host::{config, configure, env_name, env_var, HostConfig};
pub use rightkit_local_gen::http;

pub mod approval;
pub mod asset_facts;
pub mod diag;
pub mod ffmpeg;
pub mod keychain;

/// The app data root: `<PREFIX>_APP_DATA` when set and non-empty, else the per-user data folder
/// named after the host app identifier.
pub fn app_data_root() -> PathBuf {
    app_data_root_for(env_var("APP_DATA").map(Into::into))
}

/// The data folder the caller chose explicitly (`<PREFIX>_APP_DATA`, which an engine's
/// `--data-dir <dir>` also sets), or `None` when the per-user default would be used.
pub fn explicit_data_root() -> Option<PathBuf> {
    env_var("APP_DATA")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// The per-user default data folder, ignoring `<PREFIX>_APP_DATA`.
pub fn user_data_root() -> PathBuf {
    app_data_root_for(None)
}

/// `env_override` when non-empty, else the per-user data folder (`<base>/<app identifier>`), else a
/// folder of that name under the temporary directory.
pub fn app_data_root_for(env_override: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(path) = env_override.filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    per_user_dirs()
        .map(|d| d.data)
        .unwrap_or_else(|| std::env::temp_dir().join(&config().app_identifier))
}

fn per_user_dirs() -> Option<rightkit_fs::AppDirs> {
    let app = rightkit_fs::AppId::new(config().app_identifier.as_str()).without_org();
    let opts = rightkit_fs::AppDirsOptions::new(rightkit_fs::AppDirsOs::current()).dev(false);
    rightkit_fs::AppDirs::resolve_with_options(app, &opts).ok()
}

/// Caller used when an invocation names none.
pub const DEFAULT_CALLER: &str = "cli";
/// Sidecar beside a library file that records which caller produced it: `<file>.caller.json`.
pub const CALLER_SIDECAR_SUFFIX: &str = ".caller.json";
const MAX_CALLER_LEN: usize = 40;

/// A caller identity: a stable `slug` a gallery filters on and the `label` it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Lower-case slug (spaces become `-`).
    pub slug: String,
    /// The name as given, trimmed.
    pub label: String,
}

impl Caller {
    /// The default caller (`cli`).
    pub fn cli() -> Self {
        Self {
            slug: DEFAULT_CALLER.to_string(),
            label: DEFAULT_CALLER.to_string(),
        }
    }
}

/// `My App` -> slug `my-app`, label `My App`. At most 40 characters of letters, digits, `-`, `_`
/// and inner spaces. Anything else is refused, so a caller name can never carry a path separator
/// into a file name or JSON field.
pub fn parse_caller(raw: &str) -> Result<Caller, String> {
    let label = raw.trim();
    if label.is_empty() {
        return Err("caller must not be empty".to_string());
    }
    if label.chars().count() > MAX_CALLER_LEN {
        return Err(format!(
            "caller must be at most {MAX_CALLER_LEN} characters"
        ));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '))
    {
        return Err(format!(
            "caller {label:?} may contain only letters, digits, '-', '_' and spaces"
        ));
    }
    let slug: String = label.to_ascii_lowercase().replace(' ', "-");
    if !slug.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("caller must contain a letter or digit".to_string());
    }
    Ok(Caller {
        slug,
        label: label.to_string(),
    })
}

/// The caller for this process: the flag when given, else `<PREFIX>_CALLER`, else `cli`.
pub fn resolve_caller(flag: Option<&str>) -> Result<Caller, String> {
    resolve_caller_from(flag, env_var("CALLER").as_deref())
}

/// Environment-free core of [`resolve_caller`]. An empty value counts as unset.
pub fn resolve_caller_from(flag: Option<&str>, env: Option<&str>) -> Result<Caller, String> {
    match flag.or(env).filter(|v| !v.trim().is_empty()) {
        Some(raw) => parse_caller(raw),
        None => Ok(Caller::cli()),
    }
}

/// Names and literals specific to this crate that an embedding app may set once at startup, next
/// to [`configure`]. Every default is RightKit-neutral; a host with shipped users passes the
/// literals it already uses so file names, user-visible text and the HTTP identity do not change.
/// Build it with `..Default::default()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaHostConfig {
    /// The app's display name for user-visible messages ("this app uploads ...", "built into
    /// ..."). `None` keeps the neutral wording ("this app", "the host", "the catalogue").
    pub app_name: Option<String>,
    /// Prefix of diagnostic lines this crate writes to stderr (`<prefix>: ...`).
    pub log_prefix: String,
    /// `User-Agent` of the stock-search and asset-download requests.
    pub user_agent: String,
    /// File name, under the locks folder, of the lock that serialises attaching the spend ledger
    /// to the job database. Hosts that already have lock files in place pass the name they use.
    pub spend_attach_lock_name: String,
}

impl Default for MediaHostConfig {
    fn default() -> Self {
        Self {
            app_name: None,
            log_prefix: "rightkit-media-gen".into(),
            user_agent: "RightKit-media-gen/0.1 (asset acquisition)".into(),
            spend_attach_lock_name: "spend-attach.lock".into(),
        }
    }
}

static MEDIA_CONFIG: std::sync::OnceLock<MediaHostConfig> = std::sync::OnceLock::new();

/// Set the crate-specific host options once, before first use. Returns the rejected value when
/// options are already set (or were already read, which fixes the defaults).
pub fn configure_media(config: MediaHostConfig) -> Result<(), Box<MediaHostConfig>> {
    MEDIA_CONFIG.set(config).map_err(Box::new)
}

/// The options in force (the defaults until [`configure_media`] is called).
pub fn media_config() -> &'static MediaHostConfig {
    MEDIA_CONFIG.get_or_init(MediaHostConfig::default)
}

/// The host's display name, or `neutral` when the host named none. For sentence-initial use pass
/// the capitalised neutral form ("This app").
pub fn app_or(neutral: &str) -> &str {
    media_config().app_name.as_deref().unwrap_or(neutral)
}
