//! QA never launches an installed app. [`refuse_installed`] refuses any app bundle or executable
//! under an installed-apps location unless installed-QA mode is on.
//!
//! Locations: `/Applications` and `~/Applications` (macOS); `ProgramFiles`, `ProgramFiles(x86)`,
//! `ProgramW6432` and `%LOCALAPPDATA%\Programs` (Windows), read from the environment the way
//! rightkit-fs `AppDirs` reads the per-user bases. Paths are canonicalised first, so a symlink or
//! `..` cannot bypass the check.
//!
//! Installed-QA mode is the signal `right-release qa --stage installed` already sets for the runner
//! it starts: `RIGHT_RELEASE_QA_STAGE=installed` ([`STAGE_ENV`]). There is no second switch.

use crate::util::{canonical_plain, err, Result};
use std::path::{Path, PathBuf};

/// The environment variable `right-release qa --stage <stage>` sets for the QA runner.
pub const STAGE_ENV: &str = "RIGHT_RELEASE_QA_STAGE";
/// The `STAGE_ENV` value that is installed-QA mode.
pub const STAGE_INSTALLED: &str = "installed";

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

/// The installed-app locations of `os`, as text. `lookup` reads the environment; `home` is the
/// user's home folder. Unset, empty or relative values are skipped.
pub fn installed_roots(os: Os, lookup: &dyn Fn(&str) -> Option<String>, home: &str) -> Vec<String> {
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
                roots.push(format!("{}\\Programs", v.trim_end_matches(['\\', '/'])));
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
    let roots: Vec<String> = installed_roots(os, &|k| std::env::var(k).ok(), &home)
        .into_iter()
        .map(|r| canonical_text(Path::new(&r)))
        .collect();
    decide(os, what, &canonical_text(path), &roots, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win_env(k: &str) -> Option<String> {
        match k {
            "ProgramFiles" => Some(r"C:\Program Files".into()),
            "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)".into()),
            "LOCALAPPDATA" => Some(r"C:\Users\a\AppData\Local".into()),
            _ => None,
        }
    }

    #[test]
    fn mac_roots_and_classification() {
        let roots = installed_roots(Os::Mac, &|_| None, "/Users/a");
        assert_eq!(roots, ["/Applications", "/Users/a/Applications"]);
        let hit = |p: &str| installed_root_of(Os::Mac, p, &roots);
        assert_eq!(
            hit("/Applications/CutRight.app/Contents/MacOS/CutRight"),
            Some("/Applications")
        );
        assert_eq!(hit("/applications/CutRight.app"), Some("/Applications"));
        assert_eq!(
            hit("/Users/a/Applications/X.app"),
            Some("/Users/a/Applications")
        );
        assert_eq!(
            hit("/Users/a/dev/../../../Applications/X.app"),
            Some("/Applications")
        );
        assert_eq!(hit("/ApplicationsExtra/X.app"), None);
        assert_eq!(hit("/Users/a/dev/target/debug/X.app"), None);
    }

    #[test]
    fn windows_roots_and_classification() {
        let roots = installed_roots(Os::Windows, &win_env, r"C:\Users\a");
        assert_eq!(
            roots,
            [
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\Users\a\AppData\Local\Programs"
            ]
        );
        let hit = |p: &str| installed_root_of(Os::Windows, p, &roots);
        assert_eq!(
            hit(r"C:\Program Files\CutRight\cutright.exe"),
            Some(r"C:\Program Files")
        );
        assert_eq!(
            hit("c:/program files (x86)/CutRight/x.exe"),
            Some(r"C:\Program Files (x86)")
        );
        assert_eq!(
            hit(r"\\?\C:\Users\a\AppData\Local\Programs\X\x.exe"),
            Some(r"C:\Users\a\AppData\Local\Programs")
        );
        assert_eq!(
            hit(r"C:\work\..\Program Files\X\x.exe"),
            Some(r"C:\Program Files")
        );
        assert_eq!(hit(r"C:\Program Files Extra\x.exe"), None);
        assert_eq!(hit(r"C:\Users\a\AppData\Local\Temp\x.exe"), None);
    }

    #[test]
    fn unset_or_relative_windows_values_give_no_root() {
        assert!(installed_roots(Os::Windows, &|_| Some("relative".into()), "").is_empty());
        assert!(installed_roots(Os::Windows, &|_| None, "").is_empty());
    }

    #[test]
    fn installed_qa_mode_reads_the_release_stage_signal() {
        let with = |v: Option<&'static str>| {
            installed_mode_with(&move |k| (k == STAGE_ENV).then(|| v.map(String::from)).flatten())
        };
        assert!(with(Some("installed")));
        assert!(with(Some(" Installed ")));
        assert!(!with(Some("source")));
        assert!(!with(None));
    }

    #[test]
    fn refuses_installed_path_unless_opted_in_and_names_path_and_opt_in() {
        let roots = vec!["/Applications".to_owned()];
        let e = decide(
            Os::Mac,
            "UI app bundle",
            "/Applications/CutRight.app",
            &roots,
            false,
        )
        .unwrap_err();
        assert!(e.0.contains("/Applications/CutRight.app"), "{e}");
        assert!(e.0.contains("RIGHT_RELEASE_QA_STAGE=installed"), "{e}");
        assert!(decide(
            Os::Mac,
            "UI app bundle",
            "/Applications/CutRight.app",
            &roots,
            true
        )
        .is_ok());
        assert!(decide(
            Os::Mac,
            "UI app bundle",
            "/tmp/dev/CutRight.app",
            &roots,
            false
        )
        .is_ok());
    }
}
