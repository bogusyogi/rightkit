//! Profile-aware home folder for apps that are sometimes started from a shell whose `HOME`
//! (`USERPROFILE`, `APPDATA`, `LOCALAPPDATA` on Windows) points into a secondary-profile folder
//! under the account, such as `<account>/<profiles>/<name>/home`. Such a process must still use the
//! account's own folders, while any other `HOME` (a test's fixture folder included) is honoured.
//!
//! Unlike [`crate::app_dirs::account_home`], which never reads the environment, these functions
//! honour the environment except inside the named profiles folder.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn env_home() -> PathBuf {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The account folder that holds `path` inside its `profiles_dir` folder (`C:\Users\a` for
/// `C:\Users\a\<profiles_dir>\b\home\AppData\Local`), else `None`. Case-insensitive.
pub fn profiles_owner(path: &Path, profiles_dir: &str) -> Option<PathBuf> {
    let mut owner = PathBuf::new();
    for part in path.components() {
        if part.as_os_str().eq_ignore_ascii_case(profiles_dir) {
            return (owner.components().count() > 1).then_some(owner);
        }
        owner.push(part);
    }
    None
}

/// The account's home folder: `HOME` (`USERPROFILE` on Windows), unless it lies inside the
/// account's `profiles_dir` folder (or is `/` on Unix), where the account's real home is used.
pub fn profile_aware_home(profiles_dir: &str) -> PathBuf {
    let env = env_home();
    #[cfg(windows)]
    if let Some(dir) = profiles_owner(&env, profiles_dir) {
        return dir;
    }
    #[cfg(unix)]
    if let Some(dir) = crate::app_dirs::account_home()
        .ok()
        .filter(|dir| env == Path::new("/") || env.starts_with(dir.join(profiles_dir)))
    {
        return dir;
    }
    let _ = profiles_dir;
    env
}

/// An environment variable as a per-app folder resolver should read it: on Unix `HOME` is
/// [`profile_aware_home`]; on Windows `USERPROFILE`/`HOME`, `APPDATA` and `LOCALAPPDATA` that lie
/// inside a profiles folder are mapped back to the account's own folders. Pass it as the
/// environment reader of [`crate::app_dirs::AppDirs::resolve_with`].
pub fn profile_aware_env_var(key: &str, profiles_dir: &str) -> Option<OsString> {
    #[cfg(unix)]
    if key == "HOME" {
        return Some(profile_aware_home(profiles_dir).into_os_string());
    }
    let value = std::env::var_os(key);
    #[cfg(windows)]
    if let Some(account) = value
        .as_ref()
        .and_then(|v| profiles_owner(Path::new(v), profiles_dir))
    {
        match key.to_ascii_uppercase().as_str() {
            "USERPROFILE" | "HOME" => return Some(account.into_os_string()),
            "APPDATA" => return Some(account.join("AppData").join("Roaming").into_os_string()),
            "LOCALAPPDATA" => return Some(account.join("AppData").join("Local").into_os_string()),
            _ => {}
        }
    }
    let _ = profiles_dir;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_is_found_above_the_profiles_folder() {
        let base = std::env::temp_dir().join("acct");
        let inside = base.join("Profiles").join("b").join("home");
        assert_eq!(profiles_owner(&inside, "profiles"), Some(base.clone()));
        assert_eq!(profiles_owner(&base, "profiles"), None);
    }
}
