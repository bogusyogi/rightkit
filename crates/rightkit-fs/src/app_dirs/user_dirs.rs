//! The user's own folders (home, Desktop, Documents, Downloads), the values the `dirs` crate
//! answers for `home_dir`, `desktop_dir`, `document_dir` and `download_dir`.
//!
//! These are the real account folders, not the app folders of [`super::AppDirs`]. Each returns
//! `None` when the platform has no such folder or it is disabled.
//!
//! * **Windows:** `SHGetKnownFolderPath` for the Profile, Desktop, Documents and Downloads known
//!   folders; the environment is never consulted and a relocated folder is followed.
//! * **macOS:** `$HOME` when it is an absolute path, else the account database
//!   ([`super::account_home`]), plus `Desktop`, `Documents` and `Downloads`.
//! * **Linux and other Unix:** `$XDG_CONFIG_HOME/user-dirs.dirs` (else `~/.config/user-dirs.dirs`),
//!   the file `xdg-user-dirs` writes. A missing file, a missing or relative entry, and a disabled
//!   entry (`"$HOME/"`) give `None`. `home` is `$HOME` when absolute, else the account database.
//! * **Android and other targets:** only `home` can answer (the account home, when there is one).

use std::path::PathBuf;

/// The user's home folder.
pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        super::known::known_folder(&super::known::PROFILE).filter(|p| p.is_absolute())
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| super::account_home().ok())
    }
}

/// The Desktop folder.
pub fn desktop() -> Option<PathBuf> {
    folder(Kind::Desktop)
}

/// The Documents folder.
pub fn documents() -> Option<PathBuf> {
    folder(Kind::Documents)
}

/// The Downloads folder.
pub fn downloads() -> Option<PathBuf> {
    folder(Kind::Downloads)
}

#[derive(Clone, Copy)]
enum Kind {
    Desktop,
    Documents,
    Downloads,
}

#[cfg(windows)]
fn folder(kind: Kind) -> Option<PathBuf> {
    let id = match kind {
        Kind::Desktop => &super::known::DESKTOP,
        Kind::Documents => &super::known::DOCUMENTS,
        Kind::Downloads => &super::known::DOWNLOADS,
    };
    super::known::known_folder(id).filter(|p| p.is_absolute())
}

#[cfg(target_os = "macos")]
fn folder(kind: Kind) -> Option<PathBuf> {
    let name = match kind {
        Kind::Desktop => "Desktop",
        Kind::Documents => "Documents",
        Kind::Downloads => "Downloads",
    };
    home().map(|h| h.join(name))
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn folder(kind: Kind) -> Option<PathBuf> {
    let key = match kind {
        Kind::Desktop => "XDG_DESKTOP_DIR",
        Kind::Documents => "XDG_DOCUMENTS_DIR",
        Kind::Downloads => "XDG_DOWNLOAD_DIR",
    };
    let home = home()?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_user_dirs(&text, key, &home)
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )
)))]
fn folder(_kind: Kind) -> Option<PathBuf> {
    None
}

/// The value of `key` in the text of a `user-dirs.dirs` file: `KEY="$HOME/Sub"` or
/// `KEY="/absolute/path"`. `"$HOME/"` (disabled), a relative value and a missing key are `None`.
#[cfg_attr(
    not(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )),
    allow(dead_code)
)]
fn parse_user_dirs(text: &str, key: &str, home: &std::path::Path) -> Option<PathBuf> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        // The last assignment wins, as when the shell file is sourced.
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        found = Some(value.to_string());
    }
    let value = found?;
    if let Some(rest) = value.strip_prefix("$HOME/") {
        return if rest.is_empty() {
            None
        } else {
            Some(home.join(rest))
        };
    }
    let path = PathBuf::from(&value);
    path.is_absolute().then_some(path)
}
