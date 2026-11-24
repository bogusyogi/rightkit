//! An [`ExternalOpener`] that applies the host's destination policy before the hand-off.
//!
//! The shell routes every link click and `window.open` to its `rk_shell_open_external` command; an
//! app that must allow only its own pages registers [`AllowlistOpener`] so renderer text can never
//! become an arbitrary-URL opener. The policy (which destinations are allowed) is the host's
//! predicate; the hand-off is `rightkit_child_process::open_url`, which never goes through a shell.

use crate::opener::ExternalOpener;

/// Refusal text used when the host gives none.
pub const DEFAULT_REFUSAL: &str = "refused: destination is not on the allowlist";

/// Check `url` with `allowed`, then open it in the system browser.
///
/// `allowed` is the host's policy. A refused destination returns `refusal` and nothing is
/// launched; a launch failure returns `could not open link: <error>`.
pub fn open_allowed(
    allowed: impl Fn(&str) -> bool,
    refusal: &str,
    url: &str,
) -> Result<(), String> {
    if !allowed(url) {
        return Err(refusal.to_string());
    }
    rightkit_child_process::open_url(url).map_err(|err| format!("could not open link: {err}"))
}

/// The external-link opener for an app with a destination allowlist.
pub struct AllowlistOpener<F> {
    allowed: F,
    refusal: String,
}

impl<F: Fn(&str) -> bool + Send + Sync + 'static> AllowlistOpener<F> {
    /// An opener that opens only the destinations `allowed` accepts. The host passes its policy
    /// function (for example an exact-URL list); the refusal text is [`DEFAULT_REFUSAL`].
    pub fn new(allowed: F) -> Self {
        Self {
            allowed,
            refusal: DEFAULT_REFUSAL.to_string(),
        }
    }

    /// Replace the refusal text returned to the renderer.
    pub fn with_refusal(mut self, text: impl Into<String>) -> Self {
        self.refusal = text.into();
        self
    }

    /// The policy half: `Ok` when the destination is allowed, the refusal text otherwise.
    pub fn check(&self, url: &str) -> Result<(), String> {
        if (self.allowed)(url) {
            Ok(())
        } else {
            Err(self.refusal.clone())
        }
    }

    /// Check and open `url`, for a host command that opens a fixed page.
    pub fn open_url(&self, url: &str) -> Result<(), String> {
        open_allowed(&self.allowed, &self.refusal, url)
    }
}

impl<F: Fn(&str) -> bool + Send + Sync + 'static> ExternalOpener for AllowlistOpener<F> {
    fn open(&self, target: &str) -> Result<(), String> {
        self.open_url(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_destination_is_not_launched() {
        let opener = AllowlistOpener::new(|url| url == "https://example.test/").with_refusal("no");
        assert_eq!(opener.check("https://other.test/"), Err("no".to_string()));
        assert_eq!(opener.open("https://other.test/"), Err("no".to_string()));
        assert!(opener.check("https://example.test/").is_ok());
    }
}
