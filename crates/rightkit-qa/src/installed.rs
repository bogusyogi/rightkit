//! QA never launches an installed app. [`refuse_installed`] refuses any app bundle or executable
//! under an installed-apps location unless installed-QA mode is on.
//!
//! Locations: `/Applications` and `~/Applications` (macOS); `ProgramFiles`, `ProgramFiles(x86)`,
//! `ProgramW6432`, `%LOCALAPPDATA%\Programs` and `%LOCALAPPDATA%\<Product>` (Windows), read from
//! the environment the way rightkit-fs `AppDirs` reads the per-user bases. Paths are canonicalised
//! first, so a symlink or `..` cannot bypass the check.
//!
//! `right-release` dev-installs every Windows app into `%LOCALAPPDATA%\<Product>`, so each product
//! folder is a root. `%LOCALAPPDATA%` itself is never a root: QA homes, temp dirs and caches live
//! there. The product folder names arrive two ways, merged and deduplicated case-insensitively:
//! [`declare_product_dirs`] (the config loader declares `[ui] installed_dirs` or the app's
//! `productName`) and the `;`-separated [`INSTALLED_DIRS_ENV`] list (`right-release` or an app's
//! own launcher sets it). A name that is not a single folder name ([`product_dir_name_ok`]) is
//! ignored, so no value can widen a root to `%LOCALAPPDATA%` or leave it.
//!
//! Installed-QA mode is the signal `right-release qa --stage installed` already sets for the runner
//! it starts: `RIGHT_RELEASE_QA_STAGE=installed` ([`STAGE_ENV`]). There is no second switch.

use crate::util::{canonical_plain, err, Result};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The environment variable `right-release qa --stage <stage>` sets for the QA runner.
pub const STAGE_ENV: &str = "RIGHT_RELEASE_QA_STAGE";
/// The `STAGE_ENV` value that is installed-QA mode.
pub const STAGE_INSTALLED: &str = "installed";
/// A `;`-separated list of product folder names under `%LOCALAPPDATA%` that each hold an installed
/// app (Windows only). `right-release` and an app's own launcher may set it; QA reads it next to
/// the names given to [`declare_product_dirs`].
pub const INSTALLED_DIRS_ENV: &str = "RIGHTKIT_QA_INSTALLED_DIRS";

/// Which OS's installed-app locations to use (injected in tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Other,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

/// True when `lookup(STAGE_ENV)` is `installed` (any case, trimmed).
pub fn installed_mode_with(lookup: &dyn Fn(&str) -> Option<String>) -> bool {
    lookup(STAGE_ENV).is_some_and(|v| v.trim().eq_ignore_ascii_case(STAGE_INSTALLED))
}

/// Installed-QA mode in the process environment.
pub fn installed_mode() -> bool {
    installed_mode_with(&|k| std::env::var(k).ok())
}

fn absolute_text(os: Os, text: &str) -> bool {
    match os {
        Os::Windows => {
            let b = text.as_bytes();
            (b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'\\' || b[2] == b'/'))
                || text.starts_with("\\\\")
        }
        _ => text.starts_with('/'),
    }
}

/// True when `name` (trimmed) is one folder name: non-empty, not `.` or `..`, no `\`, `/` or `:`,
/// no NUL or other control character, at most 255 UTF-16 units (the NTFS component limit). Only
/// such a name may become `%LOCALAPPDATA%\<name>`; anything else could widen the root.
pub fn product_dir_name_ok(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.encode_utf16().count() <= 255
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':'))
}

/// The valid names (trimmed) in a `;`-separated list; invalid and empty entries are dropped.
fn parse_product_dirs(list: &str) -> Vec<String> {
    list.split(';')
        .map(str::trim)
        .filter(|n| product_dir_name_ok(n))
        .map(str::to_owned)
        .collect()
}

/// Process-wide product folder names declared through [`declare_product_dirs`].
static DECLARED_PRODUCT_DIRS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The declared list; a poisoned lock still holds a valid list, so it is used as is.
fn declared_lock() -> MutexGuard<'static, Vec<String>> {
    DECLARED_PRODUCT_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Declare product folder names under `%LOCALAPPDATA%` that hold an installed app (Windows only),
/// for the rest of the process. Names are trimmed; invalid ones ([`product_dir_name_ok`]) are
/// ignored; a name already declared, in any letter case, is not added again.
pub fn declare_product_dirs<I, S>(names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut declared = declared_lock();
    for name in names {
        let name = name.as_ref().trim();
        if !product_dir_name_ok(name) {
            continue;
        }
        let key = name.to_lowercase();
        if !declared.iter().any(|d| d.to_lowercase() == key) {
            declared.push(name.to_owned());
        }
    }
}

/// The product folder names declared so far, in declaration order.
pub fn declared_product_dirs() -> Vec<String> {
    declared_lock().clone()
}

/// The installed-app locations of `os`, as text, with product folder names from
/// [`INSTALLED_DIRS_ENV`] (read through `lookup`) only. See [`installed_roots_with`].
pub fn installed_roots(os: Os, lookup: &dyn Fn(&str) -> Option<String>, home: &str) -> Vec<String> {
    installed_roots_with(os, lookup, home, &[])
}

/// The installed-app locations of `os`, as text. `lookup` reads the environment; `home` is the
/// user's home folder. Unset, empty or relative values are skipped. On Windows, with an absolute
/// `LOCALAPPDATA`, each valid name in `product_dirs` and in the [`INSTALLED_DIRS_ENV`] list adds
/// the root `%LOCALAPPDATA%\<name>` (deduplicated case-insensitively); `%LOCALAPPDATA%` itself is
/// never a root. Other systems ignore product folder names.
pub fn installed_roots_with(
    os: Os,
    lookup: &dyn Fn(&str) -> Option<String>,
    home: &str,
    product_dirs: &[String],
) -> Vec<String> {
    let mut roots = Vec::new();
    match os {
        Os::Mac => {
            roots.push("/Applications".to_owned());
            if absolute_text(os, home) {
                roots.push(format!("{}/Applications", home.trim_end_matches('/')));
            }
        }
        Os::Windows => {
            for key in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
                if let Some(v) = lookup(key).filter(|v| absolute_text(os, v)) {
                    roots.push(v);
                }
            }
            if let Some(v) = lookup("LOCALAPPDATA").filter(|v| absolute_text(os, v)) {
                let base = v.trim_end_matches(['\\', '/']);
                roots.push(format!("{base}\\Programs"));
                let from_env = lookup(INSTALLED_DIRS_ENV)
                    .map(|list| parse_product_dirs(&list))
                    .unwrap_or_default();
                // `Programs` is already a root; later spellings of any name are dropped.
                let mut seen = vec!["programs".to_owned()];
                for name in product_dirs
                    .iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|n| product_dir_name_ok(n))
                    .chain(from_env.iter().map(String::as_str))
                {
                    let key = name.to_lowercase();
                    if !seen.contains(&key) {
                        seen.push(key);
                        roots.push(format!("{base}\\{name}"));
                    }
                }
            }
        }
        Os::Other => {}
    }
    roots
}

/// Lexical form for comparison: verbatim prefix dropped, separators unified (Windows), `.` and
/// `..` resolved, case folded (both macOS and Windows file systems are case-insensitive by default).
fn fold(os: Os, text: &str) -> Vec<String> {
    let text = match os {
        Os::Windows => crate::util::plain_path_text(text).replace('\\', "/"),
        _ => text.to_owned(),
    };
    let mut parts: Vec<String> = Vec::new();
    for seg in text.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(if os == Os::Other {
                s.to_owned()
            } else {
                s.to_lowercase()
            }),
        }
    }
    parts
}

/// Pure classification: the root `candidate` sits under (or equals), if any.
pub fn installed_root_of<'a>(os: Os, candidate: &str, roots: &'a [String]) -> Option<&'a str> {
    let c = fold(os, candidate);
    roots.iter().map(String::as_str).find(|root| {
        let r = fold(os, root);
        !r.is_empty() && c.len() >= r.len() && c[..r.len()] == r[..]
    })
}

/// Refusal decision with everything injected: `Err` when `candidate` is under a root and
/// installed-QA mode is off. The message names the path and the way to opt in.
pub fn decide(
    os: Os,
    what: &str,
    candidate: &str,
    roots: &[String],
    installed_qa: bool,
) -> Result<()> {
    if installed_qa {
        return Ok(());
    }
    match installed_root_of(os, candidate, roots) {
        None => Ok(()),
        Some(root) => err(format!(
            "QA refuses {what} {candidate}: it is inside the installed-apps location {root}. QA drives dev or QA builds, never an installed app. \
             To run against an installed app on purpose, use installed-QA mode: run `right-release qa --stage installed`, which sets {STAGE_ENV}={STAGE_INSTALLED}; nothing was launched"
        )),
    }
}

fn canonical_text(path: &Path) -> String {
    // Resolve symlinks in the longest existing prefix and append the rest.
    let mut base = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = canonical_plain(&base) {
            let mut out: PathBuf = real;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out.display().to_string();
        }
        match (
            base.file_name().map(|n| n.to_owned()),
            base.parent().map(Path::to_path_buf),
        ) {
            (Some(name), Some(parent)) if !parent.as_os_str().is_empty() => {
                tail.push(name);
                base = parent;
            }
            _ => return path.display().to_string(),
        }
    }
}

/// Refuse `path` (a bundle or executable about to be launched) when it is under an installed-apps
/// location and installed-QA mode is off. `what` names the thing in the message.
pub fn refuse_installed(path: &Path, what: &str) -> Result<()> {
    if installed_mode() {
        return Ok(());
    }
    let os = Os::current();
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_default();
    let roots: Vec<String> = installed_roots_with(
        os,
        &|k| std::env::var(k).ok(),
        &home,
        &declared_product_dirs(),
    )
    .into_iter()
    .map(|r| canonical_text(Path::new(&r)))
    .collect();
    decide(os, what, &canonical_text(path), &roots, false)
}
