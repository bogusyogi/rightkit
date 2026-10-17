//! Opens a URL with its registered handler and launches an installed app by its identifier,
//! through the desktop launcher (`open` on macOS, `rundll32` and `explorer.exe` on Windows,
//! `xdg-open` on other Unix). The launcher is detached and never killed by the call, the same
//! as [`reveal`](crate::reveal): it hands the work to the desktop and may stay up.
//!
//! Inputs are checked before anything starts. A URL needs a scheme and no whitespace or control
//! characters, and `file:` is refused (opening a file URL can run a local program). An app
//! identifier is a macOS bundle identifier (`com.example.App`, `-b`) or a Windows
//! AppUserModelID (`PackageFamilyName!AppId`, `shell:AppsFolder`).

use crate::{reveal::LAUNCH_WAIT, OwnedCommand};
use std::{io, process::Stdio};

/// Opens `url` with the handler registered for its scheme (for example `claude://`).
pub fn open_url(url: &str) -> io::Result<()> {
    if !valid_url(url) {
        return Err(invalid(format!(
            "refusing to open {url:?}: not an allowed URL"
        )));
    }
    if cfg!(target_os = "macos") {
        hand_off("open", &[url])
    } else if cfg!(windows) {
        hand_off("rundll32.exe", &["url.dll,FileProtocolHandler", url])
    } else {
        hand_off("xdg-open", &[url])
    }
}

/// Launches the installed app `id`: a macOS bundle identifier on macOS, a Windows AppUserModelID
/// on Windows. Other Unix hosts return `Unsupported`.
pub fn launch_app(id: &str) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        if !valid_bundle_id(id) {
            return Err(invalid(format!("{id:?} is not a bundle identifier")));
        }
        hand_off("open", &["-b", id])
    } else if cfg!(windows) {
        if !valid_app_user_model_id(id) {
            return Err(invalid(format!("{id:?} is not an AppUserModelID")));
        }
        let target = format!("shell:AppsFolder\\{id}");
        hand_off("explorer.exe", &[target.as_str()])
    } else {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// A scheme (letter first; letters, digits, `+`, `-`, `.`), then `:`, and no whitespace or
/// control characters anywhere. `file:` is refused.
fn valid_url(url: &str) -> bool {
    let Some((scheme, _rest)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let scheme_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    scheme_ok
        && !scheme.eq_ignore_ascii_case("file")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Reverse-DNS form: at least one dot, letters, digits, `.` and `-` only.
fn valid_bundle_id(id: &str) -> bool {
    id.contains('.')
        && !id.starts_with('.')
        && !id.ends_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// `PackageFamilyName!AppId`: exactly one `!`, and letters, digits, `.`, `-`, `_` only.
fn valid_app_user_model_id(id: &str) -> bool {
    id.matches('!').count() == 1
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '!'))
}

/// Starts the launcher detached and waits up to [`LAUNCH_WAIT`] for it to report. A launcher
/// that is still running is handing off to the desktop and counts as success.
fn hand_off(program: &str, args: &[&str]) -> io::Result<()> {
    let mut command = OwnedCommand::new(program);
    command
        .command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.windows_hide();
    let mut child = command.spawn_uncontained_detached()?;
    match child.wait_timeout(LAUNCH_WAIT)? {
        // `explorer.exe` reports failure with exit code 1 even when it succeeded.
        Some(status) if !status.success() && !cfg!(windows) => Err(io::Error::other(format!(
            "the launcher exited with {status}"
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_need_a_scheme_and_no_whitespace() {
        assert!(valid_url("claude://chat/abc"));
        assert!(valid_url("https://example.com/a?b=c"));
        assert!(!valid_url("example.com"));
        assert!(!valid_url("file:///etc/passwd"));
        assert!(!valid_url("FILE:/x"));
        assert!(!valid_url("https://example.com/a b"));
        assert!(!valid_url("https://example.com/\nx"));
        assert!(!valid_url("1abc://x"));
    }

    #[test]
    fn bundle_ids_and_app_user_model_ids_are_checked_per_shape() {
        assert!(valid_bundle_id("com.apple.Safari"));
        assert!(!valid_bundle_id("Safari"));
        assert!(!valid_bundle_id("com.example/../x"));
        assert!(valid_app_user_model_id(
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        ));
        assert!(!valid_app_user_model_id("NoBang"));
        assert!(!valid_app_user_model_id("A!B!C"));
        assert!(!valid_app_user_model_id("A!B C"));
    }

    #[test]
    fn a_refused_url_starts_nothing() {
        let error = open_url("file:///tmp/x").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
