//! Sign-in always opens the user's DEFAULT browser, never an in-app webview.
//!
//! Users stay signed into their own browser; an app that shows its own webview forces them to
//! sign in again. This module is the one place Right Suite apps open an authorization URL:
//!
//! - Desktop (macOS, Windows, Linux): [`SystemBrowser`] hands the URL to the OS default
//!   handler (`/usr/bin/open`, `rundll32 url.dll,FileProtocolHandler` which is the
//!   `ShellExecute` default-browser path, `xdg-open`). Argument vector only, never a shell.
//!   The flow completes through an RFC 8252 loopback redirect
//!   ([`crate::OAuthClient::sign_in_with_browser`]: `127.0.0.1`, ephemeral port, exact
//!   redirect path, constant-time `state` check, single use, bounded timeout), always with
//!   PKCE `S256`. A registered custom-scheme deep link is the alternative for apps whose
//!   provider registration requires one; the app's URL hub (`rightkit-tauri-shell`
//!   `open_url`) delivers the redirect and the app passes it to
//!   [`crate::OAuthClient::exchange_code`] after checking `state`.
//! - Embedded webviews (WKWebView, WebView2, WebKitGTK, bundled Chromium) are refused with
//!   [`OAuthError::EmbeddedWebviewRefused`]. Google also blocks them for OAuth.
//!
//! # Sign in with Apple
//!
//! macOS desktop: the web flow (`https://appleid.apple.com/auth/authorize`, `response_mode`
//! `form_post` or a deep link) opens in the default browser through [`SystemBrowser`]. Apple's
//! native `AuthenticationServices` sheet (`ASAuthorizationController`) is used only where Apple
//! requires it: iOS/iPadOS apps that offer other third-party sign-ins (App Store Review
//! Guideline 4.8) and macOS apps that choose the system credential sheet. It is a system
//! surface, not an app webview, and it is never replaced by one. Apple's `form_post` redirect
//! cannot reach a loopback listener, so an Apple web flow needs the app's own HTTPS redirect
//! (relay) handing back to a custom-scheme deep link.
//!
//! # Mobile (the only allowed surfaces)
//!
//! - Android: Chrome Custom Tabs on the user's default browser ([`SignInSurface::CustomTab`]).
//!   Never an Android `WebView`.
//! - iOS: `ASWebAuthenticationSession` ([`SignInSurface::AuthSession`]), required by App Review
//!   and sharing Safari's state. Never `WKWebView`. Limit: it shares Safari, not the user's
//!   other browsers.
//!
//! This crate has no mobile UI code; the host app implements [`BrowserOpener`] on those
//! surfaces and reports the matching [`SignInSurface`].

use std::process::{Command, Stdio};

use crate::OAuthError;

const MAX_URL_LEN: usize = 8 * 1024;

/// Where a sign-in page is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInSurface {
    /// The OS default browser (desktop).
    DefaultBrowser,
    /// Android Chrome Custom Tabs on the default browser.
    CustomTab,
    /// iOS `ASWebAuthenticationSession`.
    AuthSession,
    /// An in-app webview. Always refused.
    EmbeddedWebview,
}

impl SignInSurface {
    /// `Ok` for the three allowed surfaces, [`OAuthError::EmbeddedWebviewRefused`] otherwise.
    pub fn require_allowed(self) -> Result<(), OAuthError> {
        match self {
            Self::EmbeddedWebview => Err(OAuthError::EmbeddedWebviewRefused),
            _ => Ok(()),
        }
    }
}

/// Shows an authorization URL to the user.
pub trait BrowserOpener {
    /// The surface this opener uses. Hosts with a webview-backed opener must not claim
    /// another surface; the flow refuses [`SignInSurface::EmbeddedWebview`].
    fn surface(&self) -> SignInSurface {
        SignInSurface::DefaultBrowser
    }

    /// Show `url` (already checked by [`check_authorization_url`]).
    fn open(&self, url: &str) -> Result<(), OAuthError>;
}

/// Closures open in the default browser surface.
impl<F: Fn(&str) -> Result<(), OAuthError>> BrowserOpener for F {
    fn open(&self, url: &str) -> Result<(), OAuthError> {
        self(url)
    }
}

/// Opens the URL with the operating system's default-browser handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), OAuthError> {
        check_authorization_url(url)?;
        let mut cmd;
        #[cfg(target_os = "macos")]
        {
            cmd = Command::new("/usr/bin/open");
            cmd.arg(url);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd = Command::new("rundll32.exe");
            cmd.arg("url.dll,FileProtocolHandler").arg(url);
            // CREATE_NO_WINDOW: no console flash.
            cmd.creation_flags(0x0800_0000);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            cmd = Command::new("xdg-open");
            cmd.arg(url);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| OAuthError::Callback(format!("open default browser: {e}")))?;
        // Reap without blocking the caller.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Only `https` URLs (or `http` to a loopback host, for local test providers) without
/// whitespace, control characters or quotes may reach the OS handler. Refusing others keeps a
/// crafted configuration from launching a local file or another scheme.
pub fn check_authorization_url(url: &str) -> Result<(), OAuthError> {
    let refuse = |why: &str| Err(OAuthError::UnsafeAuthorizationUrl(why.into()));
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return refuse("empty or too long");
    }
    if url
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
    {
        return refuse("contains whitespace, control or quote characters");
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("");
        if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(());
        }
    }
    refuse("not an https URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_loopback_http_pass() {
        assert!(check_authorization_url("https://accounts.google.com/o/oauth2/v2/auth?a=b").is_ok());
        assert!(check_authorization_url("http://127.0.0.1:8080/authorize").is_ok());
        assert!(check_authorization_url("http://localhost/authorize").is_ok());
    }

    #[test]
    fn other_schemes_and_hosts_refused() {
        for bad in [
            "http://example.com/a",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "https://a.test/x y",
            "https://a.test/\"x",
            "https://a.test/\n",
            "",
            "ms-msdt:/id",
        ] {
            assert!(
                matches!(
                    check_authorization_url(bad),
                    Err(OAuthError::UnsafeAuthorizationUrl(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn embedded_webview_is_refused() {
        assert!(matches!(
            SignInSurface::EmbeddedWebview.require_allowed(),
            Err(OAuthError::EmbeddedWebviewRefused)
        ));
        for ok in [
            SignInSurface::DefaultBrowser,
            SignInSurface::CustomTab,
            SignInSurface::AuthSession,
        ] {
            assert!(ok.require_allowed().is_ok());
        }
    }
}
