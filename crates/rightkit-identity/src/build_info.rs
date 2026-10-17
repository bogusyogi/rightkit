//! Shared build identity (Orthic-Labs/rightkit#503).
//!
//! In the app: `static BUILD: ... = rightkit_identity::build_identity!();` after a `build.rs` that
//! calls [`build_script::emit`] (or sets the same variables another way). Unset variables become
//! `unknown`.

use serde::{Deserialize, Serialize};

pub const ENV_GIT_REV: &str = "RIGHTKIT_GIT_REV";
pub const ENV_GIT_DIRTY: &str = "RIGHTKIT_GIT_DIRTY";
pub const ENV_PROFILE: &str = "RIGHTKIT_BUILD_PROFILE";
pub const ENV_CHANNEL: &str = "RIGHTKIT_CHANNEL";
pub const ENV_UTC: &str = "RIGHTKIT_BUILD_UTC";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    pub git_rev: String,
    pub dirty: bool,
    pub profile: String,
    pub channel: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`, or `unknown`.
    pub built_utc: String,
}

impl BuildIdentity {
    /// Build from compile-time `option_env!` values (use [`build_identity!`](crate::build_identity)).
    pub fn from_compile_time(
        git_rev: Option<&str>,
        dirty: Option<&str>,
        profile: Option<&str>,
        channel: Option<&str>,
        built_utc: Option<&str>,
        version: &str,
    ) -> Self {
        let fallback_profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        BuildIdentity {
            version: version.to_string(),
            git_rev: git_rev.unwrap_or("unknown").to_string(),
            dirty: parse_bool(dirty),
            profile: profile.unwrap_or(fallback_profile).to_string(),
            channel: channel.unwrap_or("unknown").to_string(),
            built_utc: built_utc.unwrap_or("unknown").to_string(),
        }
    }

    /// Apply a run-time override. `get` looks up the same variable names as the compile-time
    /// path (for example `std::env::var(k).ok()`); a present value wins.
    pub fn with_runtime_override(mut self, get: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(v) = get(ENV_GIT_REV) {
            self.git_rev = v;
        }
        if let Some(v) = get(ENV_GIT_DIRTY) {
            self.dirty = parse_bool(Some(&v));
        }
        if let Some(v) = get(ENV_PROFILE) {
            self.profile = v;
        }
        if let Some(v) = get(ENV_CHANNEL) {
            self.channel = v;
        }
        if let Some(v) = get(ENV_UTC) {
            self.built_utc = v;
        }
        self
    }

    /// `1.2.3+abc1234-dirty (release, beta, 2026-10-18T09:00:00Z)`.
    pub fn display(&self) -> String {
        let short: String = self.git_rev.chars().take(7).collect();
        format!(
            "{}+{}{} ({}, {}, {})",
            self.version,
            short,
            if self.dirty { "-dirty" } else { "" },
            self.profile,
            self.channel,
            self.built_utc
        )
    }
}

fn parse_bool(v: Option<&str>) -> bool {
    matches!(
        v.map(str::trim),
        Some("1") | Some("true") | Some("yes") | Some("dirty")
    )
}

/// Expands in the calling crate, so `env!("CARGO_PKG_VERSION")` is the app's version.
#[macro_export]
macro_rules! build_identity {
    () => {
        $crate::BuildIdentity::from_compile_time(
            option_env!("RIGHTKIT_GIT_REV"),
            option_env!("RIGHTKIT_GIT_DIRTY"),
            option_env!("RIGHTKIT_BUILD_PROFILE"),
            option_env!("RIGHTKIT_CHANNEL"),
            option_env!("RIGHTKIT_BUILD_UTC"),
            env!("CARGO_PKG_VERSION"),
        )
    };
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_stamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// For a `build.rs`: prints `cargo:rustc-env` lines for the five variables. A variable already
/// set in the environment wins over the value computed here. Runs `git` when it is on PATH.
pub mod build_script {
    use super::*;
    use std::process::Command;

    fn git(args: &[&str]) -> Option<String> {
        // windowless: runs inside build.rs (cargo build script), never from a GUI process
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn emit() {
        let rev = std::env::var(ENV_GIT_REV)
            .ok()
            .or_else(|| git(&["rev-parse", "HEAD"]))
            .unwrap_or_else(|| "unknown".into());
        let dirty = std::env::var(ENV_GIT_DIRTY).ok().unwrap_or_else(|| {
            match git(&["status", "--porcelain"]) {
                Some(s) if !s.is_empty() => "1".into(),
                _ => "0".into(),
            }
        });
        let profile = std::env::var(ENV_PROFILE)
            .ok()
            .or_else(|| std::env::var("PROFILE").ok())
            .unwrap_or_else(|| "unknown".into());
        let channel = std::env::var(ENV_CHANNEL).unwrap_or_else(|_| "dev".into());
        let utc = std::env::var(ENV_UTC).unwrap_or_else(|_| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            utc_stamp(now)
        });
        println!("cargo:rustc-env={ENV_GIT_REV}={rev}");
        println!("cargo:rustc-env={ENV_GIT_DIRTY}={dirty}");
        println!("cargo:rustc-env={ENV_PROFILE}={profile}");
        println!("cargo:rustc-env={ENV_CHANNEL}={channel}");
        println!("cargo:rustc-env={ENV_UTC}={utc}");
        for v in [
            ENV_GIT_REV,
            ENV_GIT_DIRTY,
            ENV_PROFILE,
            ENV_CHANNEL,
            ENV_UTC,
        ] {
            println!("cargo:rerun-if-env-changed={v}");
        }
        watch_git();
    }

    /// Resolve the real git directory (a worktree or submodule `.git` is a file) and watch HEAD,
    /// the ref HEAD names, `packed-refs` and the index, so a commit on an unchanged branch and
    /// working-tree status changes rerun the script.
    fn watch_git() {
        let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]).map(std::path::PathBuf::from)
        else {
            println!("cargo:rerun-if-changed=.git/HEAD");
            return;
        };
        let common = git(&["rev-parse", "--git-common-dir"])
            .map(std::path::PathBuf::from)
            .map(|p| if p.is_absolute() { p } else { git_dir.join(p) })
            .unwrap_or_else(|| git_dir.clone());
        let head = git_dir.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Some(r) = std::fs::read_to_string(&head)
            .ok()
            .and_then(|t| t.trim().strip_prefix("ref:").map(|r| r.trim().to_string()))
        {
            println!("cargo:rerun-if-changed={}", common.join(&r).display());
            println!("cargo:rerun-if-changed={}", git_dir.join(&r).display());
        }
        println!(
            "cargo:rerun-if-changed={}",
            common.join("packed-refs").display()
        );
        println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
        // Unstaged edits change `dirty` without touching the index.
        for p in ["src", "Cargo.toml", "build.rs"] {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_known_values() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_760_000_000), "2025-10-09T08:53:20Z");
    }

    #[test]
    fn defaults_and_override() {
        let b = BuildIdentity::from_compile_time(None, Some("1"), None, None, None, "1.0.0");
        assert_eq!(b.git_rev, "unknown");
        assert!(b.dirty);
        let b = b.with_runtime_override(|k| (k == ENV_CHANNEL).then(|| "beta".to_string()));
        assert_eq!(b.channel, "beta");
        assert!(b.display().starts_with("1.0.0+unknown-dirty"));
    }
}
