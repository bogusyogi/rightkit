//! Platform-scoped sign-in state for X and TikTok.
//!
//! The host's [`AuthStateSnapshot`] carries ONE platform at a time, whichever was pushed last
//! (it replaces, it does not merge). So every lookup here falls back to the canonical on-disk
//! locations the host's auth helpers write:
//!
//! - browser sign-in profile: `<data_root>/auth/<platform>` (Chromium `--user-data-dir`; yt-dlp
//!   reads it natively via `--cookies-from-browser`)
//! - exported Netscape cookies: `<data_root>/auth/<platform>_cookies.txt`
//!
//! The data root and the browser-kind marker file name come from the host through
//! [`AuthLayout`].

use std::path::PathBuf;

use crate::protocol::AuthStateSnapshot;

/// Where the host keeps sign-in state on disk.
#[derive(Debug, Clone)]
pub struct AuthLayout {
    /// The app data root; sign-in state lives under `<data_root>/auth`.
    pub data_root: PathBuf,
    /// Name of the file inside a sign-in profile that records the browser kind yt-dlp reads
    /// cookies from (absent means chrome).
    pub browser_marker: String,
}

impl AuthLayout {
    /// `<data_root>/auth`, the folder holding every platform's sign-in state.
    pub fn auth_dir(&self) -> PathBuf {
        self.data_root.join("auth")
    }
}

/// A source that can carry a signed-in session. Covers X and TikTok;
/// the Instagram/Threads equivalents already exist next to their call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePlatform {
    X,
    TikTok,
}

impl SourcePlatform {
    /// Wire label carried in `needs_auth` and `auth-signed-in`. Must equal
    /// `canonical_platform_name` in `src-tauri/src/commands_auth_helpers.rs`
    /// and the string the frontend compares (`useEngineEvents` retries jobs
    /// whose `job.needs_auth === payload`).
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::TikTok => "TikTok",
        }
    }

    /// `auth/<sub>` profile dir segment and `<sub>_cookies.txt` file stem —
    /// matches `auth_subdir` in `src-tauri/src/commands_auth_helpers.rs`.
    pub fn subdir(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::TikTok => "tiktok",
        }
    }

    /// yt-dlp error signatures that mean "sign in, then retry" for this
    /// platform — the X/TikTok counterpart of
    /// the Instagram auth-recoverable check. Kept
    /// deliberately narrow: a false positive yields a Sign In button whose
    /// retry just fails again.
    pub fn auth_recoverable(self, msg: &str) -> bool {
        let m = msg.to_ascii_lowercase();
        // Shared: yt-dlp telling us cookies would fix it, and the locked
        // profile-cookie-DB case (close the sign-in browser, retry).
        let shared = m.contains("use --cookies")
            || m.contains("login required")
            || m.contains("sign in to confirm")
            || m.contains("rate-limit reached")
            || (m.contains("could not copy") && m.contains("cookie database"));
        let platform = match self {
            // X logged out: JSON-API 400s/401s/403s and protected accounts.
            Self::X => {
                m.contains("unable to download json metadata")
                    || m.contains("http error 401")
                    || m.contains("http error 403")
                    || m.contains("not authorized")
                    || m.contains("this post is protected")
                    || m.contains("guest token")
            }
            // TikTok logged out/geo-gated: webpage 403s, captcha/verify
            // walls, and content-region blocks a session can clear.
            Self::TikTok => {
                m.contains("unable to download webpage")
                    || m.contains("http error 401")
                    || m.contains("http error 403")
                    || m.contains("forbidden")
                    || m.contains("requested content is not available")
                    || m.contains("video not available")
                    || m.contains("captcha")
                    || m.contains("verify your identity")
            }
        };
        shared || platform
    }
}

/// Browser sign-in profile dir for `platform` — the pushed snapshot's
/// `profile_dir` when it names this platform, else the canonical
/// `<app_data>/auth/<platform>` on-disk location (the snapshot is a single
/// slot, so a later sign-in to a different platform must not strand this
/// one).
pub fn platform_profile_dir(
    layout: &AuthLayout,
    auth: &AuthStateSnapshot,
    platform: SourcePlatform,
) -> Option<PathBuf> {
    if auth
        .platform
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(platform.subdir()))
    {
        if let Some(dir) = auth
            .profile_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
        {
            return Some(PathBuf::from(dir));
        }
    }
    Some(layout.auth_dir().join(platform.subdir())).filter(|dir| dir.exists())
}

/// yt-dlp `--cookies-from-browser <kind>:<profile>` spec for `platform`,
/// mirroring the Instagram block of the audio pipeline: only when the profile's cookie store
/// exists, with the browser kind recorded in the layout's marker file (defaulting to chrome).
pub fn platform_cookies_from_browser(
    layout: &AuthLayout,
    auth: &AuthStateSnapshot,
    platform: SourcePlatform,
) -> Option<String> {
    let dir = platform_profile_dir(layout, auth, platform)?;
    if !dir.join("Default").join("Network").join("Cookies").exists() {
        return None;
    }
    let kind = std::fs::read_to_string(dir.join(&layout.browser_marker))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "chrome".to_string());
    Some(format!("{kind}:{}", dir.display()))
}

/// Exported Netscape cookies file for `platform` (`auth/x_cookies.txt`,
/// `auth/tiktok_cookies.txt`), plus the single-slot snapshot's exported path
/// when it happens to be carrying this platform. Returns `None` when no
/// platform cookies exist — the caller then falls back to the user's global
/// `settings.cookies_path`.
pub fn platform_cookies_file(
    layout: &AuthLayout,
    auth: &AuthStateSnapshot,
    platform: SourcePlatform,
) -> Option<PathBuf> {
    let disk = layout
        .auth_dir()
        .join(format!("{}_cookies.txt", platform.subdir()));
    if disk.exists() {
        return Some(disk);
    }
    if auth
        .platform
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(platform.subdir()))
        && auth.exported_cookies_present
    {
        return auth
            .exported_cookies_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.exists());
    }
    None
}

/// True when `url`'s host is `x.com`/`twitter.com` (any subdomain).
pub fn is_x_host(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    host == "x.com"
        || host == "twitter.com"
        || host.ends_with(".x.com")
        || host.ends_with(".twitter.com")
}

/// True when `url`'s host is `tiktok.com` (any subdomain — covers `vm.`/`vt.`
/// shortlinks for code paths that run before yt-dlp resolves them).
pub fn is_tiktok_host(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    host == "tiktok.com" || host.ends_with(".tiktok.com")
}

fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .map(|host| {
            host.strip_prefix("www.")
                .or_else(|| host.strip_prefix("m."))
                .unwrap_or(&host)
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(root: &std::path::Path) -> AuthLayout {
        AuthLayout {
            data_root: root.to_path_buf(),
            browser_marker: ".browser".to_string(),
        }
    }

    #[test]
    fn hosts_are_matched_with_subdomains_only() {
        assert!(is_x_host("https://x.com/a/status/1"));
        assert!(is_x_host("https://mobile.twitter.com/a"));
        assert!(!is_x_host("https://notx.com/"));
        assert!(is_tiktok_host("https://vm.tiktok.com/abc"));
        assert!(!is_tiktok_host("https://tiktok.com.evil.example/"));
    }

    #[test]
    fn snapshot_profile_wins_for_its_own_platform() {
        let root = std::env::temp_dir().join("rightkit-auth-layout-test");
        let auth = AuthStateSnapshot {
            platform: Some("X".to_string()),
            profile_dir: Some(" /p ".to_string()),
            ..AuthStateSnapshot::default()
        };
        assert_eq!(
            platform_profile_dir(&layout(&root), &auth, SourcePlatform::X),
            Some(PathBuf::from("/p"))
        );
        // Another platform falls back to the disk location, which does not exist here.
        assert_eq!(
            platform_profile_dir(&layout(&root), &auth, SourcePlatform::TikTok),
            None
        );
    }

    #[test]
    fn cookies_from_browser_needs_the_cookie_store() {
        let root =
            std::env::temp_dir().join(format!("rightkit-auth-cookies-{}", std::process::id()));
        let profile = root.join("auth").join("x");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(profile.join("Default").join("Network")).unwrap();
        let layout = layout(&root);
        let auth = AuthStateSnapshot::default();
        assert_eq!(
            platform_cookies_from_browser(&layout, &auth, SourcePlatform::X),
            None
        );
        std::fs::write(profile.join("Default").join("Network").join("Cookies"), b"").unwrap();
        std::fs::write(profile.join(".browser"), "edge\n").unwrap();
        let spec = platform_cookies_from_browser(&layout, &auth, SourcePlatform::X).unwrap();
        assert!(spec.starts_with("edge:"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auth_recoverable_signatures_are_platform_scoped() {
        assert!(SourcePlatform::X.auth_recoverable("HTTP Error 403: Forbidden"));
        assert!(SourcePlatform::TikTok.auth_recoverable("Unable to download webpage"));
        assert!(!SourcePlatform::X.auth_recoverable("Unable to download webpage"));
        assert!(SourcePlatform::X.auth_recoverable("use --cookies for the authentication"));
    }
}
