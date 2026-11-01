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
pub mod h264;
pub mod keychain;

/// The app data root: `<PREFIX>_APP_DATA` when set and non-empty, else the per-user data folder
/// named after the host app identifier.
pub fn app_data_root() -> PathBuf {
    app_data_root_for(env_var("APP_DATA").map(Into::into))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_parsing() {
        let c = parse_caller("My App").unwrap();
        assert_eq!(c.slug, "my-app");
        assert!(parse_caller("../x").is_err());
        assert_eq!(resolve_caller_from(None, Some(" ")).unwrap(), Caller::cli());
    }

    #[test]
    fn explicit_root_wins() {
        let dir = std::env::temp_dir().join("rk-media-gen-root");
        assert_eq!(app_data_root_for(Some(dir.clone().into())), dir);
    }
}
