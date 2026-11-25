//! The host app's own update feed: a GitHub `releases/latest` answer read into a release to
//! offer, with the rules that decide whether it may be offered at all.
//!
//! No network and no file access happens here. The host fetches the feed over https (so the
//! pinned client and its timeouts stay the host's), hands the status and body to
//! `status_reason` and `parse_release`, downloads the asset, and checks the signed version
//! resource of the installer with `check_binding`. Failure reasons are short stable codes
//! (`&'static str`) the host words for its own people.
//!
//! Rules carried from Pulse's Windows updater (bogusyogi/pulse#360, #513):
//!  * the feed URL must be `https://host/path`; nothing is ever fetched over plain http;
//!  * a release is offered only when its version is strictly newer than the running one;
//!  * the installer asset must be named exactly as the host asks, its download URL must be on the
//!    one allowed host, and the feed must state its SHA-256 (`digest` = `sha256:<64 hex>`): a
//!    release without one is refused (`no_digest`);
//!  * a feed answer over 512 KiB is not read.

use serde_json::Value;

use super::release_triple;

/// The most of a feed answer that is read.
pub const MAX_FEED_BYTES: usize = 512 * 1024;

/// A newer release with an installer the host may download.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    /// The asset's size in bytes as the feed states it; 0 when it does not.
    pub size: u64,
    /// Lower-case hex SHA-256 from the feed.
    pub sha256: String,
    /// The download path on the allowed host, with its leading `/`.
    pub path: String,
}

/// What the host expects of the feed.
pub struct FeedRules<'a> {
    /// The installer asset's exact name.
    pub asset_name: &'a str,
    /// The only host the asset may be downloaded from (`github.com`).
    pub download_host: &'a str,
    /// The running version, `major.minor.patch`.
    pub current_version: &'a str,
}

/// `https://host/path` as its host and path (the path keeps its leading `/`). Anything else is
/// refused, so the update source is never fetched over plain http.
pub fn split_feed(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    (!host.is_empty()).then_some((host, path))
}

/// `Ok` for a 200 answer; otherwise the reason code: `no_release` for 404 (`releases/latest`
/// answers 404 while the repository has no published release at all), `rate_limited` for 403 and
/// 429, `bad_status` for any other.
pub fn status_reason(status: u16) -> Result<(), &'static str> {
    match status {
        200 => Ok(()),
        404 => Err("no_release"),
        403 | 429 => Err("rate_limited"),
        _ => Err("bad_status"),
    }
}

/// `Ok(Some)` for a newer release with an installer; `Ok(None)` when up to date. Errors:
/// `unreadable` (not JSON, or over [`MAX_FEED_BYTES`]), `no_tag`, `bad_version`, `no_installer`,
/// `foreign_host`, `no_digest`.
pub fn parse_release(body: &[u8], rules: &FeedRules<'_>) -> Result<Option<Release>, &'static str> {
    if body.len() > MAX_FEED_BYTES {
        return Err("unreadable");
    }
    let root: Value = serde_json::from_slice(body).map_err(|_| "unreadable")?;
    let tag = root
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("no_tag")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let (Some(latest), Some(current)) = (
        release_triple(version),
        release_triple(rules.current_version),
    ) else {
        return Err("bad_version");
    };
    if latest <= current {
        return Ok(None);
    }
    let asset = root
        .get("assets")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .find(|a| a.get("name").and_then(Value::as_str) == Some(rules.asset_name))
        .ok_or("no_installer")?;
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or("no_installer")?;
    let path = url
        .strip_prefix(&format!("https://{}/", rules.download_host))
        .ok_or("foreign_host")?;
    let sha256 = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or("no_digest")?;
    Ok(Some(Release {
        version: version.to_string(),
        notes: root
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        size: asset
            .get("size")
            .and_then(Value::as_f64)
            .map_or(0, |s| s as u64),
        sha256,
        path: format!("/{path}"),
    }))
}

/// Markdown release notes as one plain line of at most `most` characters.
pub fn notes_summary(notes: &str, most: usize) -> String {
    let plain: String = notes
        .chars()
        .map(|c| {
            if matches!(c, '#' | '*' | '`' | '_' | '>') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(most).collect()
}

/// What survives a restart: when the feed was last asked and which version was last offered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersistedCheck {
    /// Unix seconds of the last attempt; 0 when none.
    pub last_attempt: u64,
    /// The version last offered on the card, so a scheduled check offers each version once.
    pub offered: Option<String>,
}

impl PersistedCheck {
    /// Reads the two-line `last_attempt=<secs>` / `offered=<version>` text (the first eight lines
    /// only; an `offered` value of 64 bytes or more is ignored).
    pub fn parse(text: &str) -> Self {
        let mut state = Self::default();
        for line in text.lines().take(8) {
            match line.split_once('=') {
                Some(("last_attempt", v)) => state.last_attempt = v.trim().parse().unwrap_or(0),
                Some(("offered", v)) if v.len() < 64 => state.offered = Some(v.trim().to_string()),
                _ => {}
            }
        }
        state
    }

    /// The text [`PersistedCheck::parse`] reads.
    pub fn format(&self) -> String {
        format!(
            "last_attempt={}\noffered={}\n",
            self.last_attempt,
            self.offered.as_deref().unwrap_or("")
        )
    }
}

/// Why an installer's version resource was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingError {
    /// The version resource or its `ProductVersion` could not be read.
    Unreadable,
    /// A binding string differs: not this product, platform or channel.
    WrongProduct,
    /// The `ProductVersion` is not the release's.
    VersionMismatch,
}

/// Checks an installer's signed version resource: every `(key, expected)` of `binding` must read
/// back exactly, and `ProductVersion` must equal the release's. A signed older installer
/// re-published under a newer tag, or an installer for another product, platform or channel,
/// never passes. `read` returns one string of the resource, or `None` when it has none;
/// `resource_readable` is false when the resource itself could not be read at all.
pub fn check_binding(
    resource_readable: bool,
    read: &dyn Fn(&str) -> Option<String>,
    binding: &[(&str, &str)],
    release_version: &str,
) -> Result<(), BindingError> {
    if !resource_readable {
        return Err(BindingError::Unreadable);
    }
    if binding
        .iter()
        .any(|(key, want)| read(key).as_deref() != Some(*want))
    {
        return Err(BindingError::WrongProduct);
    }
    let stated = read("ProductVersion").ok_or(BindingError::Unreadable)?;
    let parsed = release_triple(stated.trim());
    if parsed.is_none() || parsed != release_triple(release_version) {
        return Err(BindingError::VersionMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_must_be_https() {
        assert_eq!(
            split_feed("https://api.github.com/repos/o/r/releases/latest"),
            Some(("api.github.com", "/repos/o/r/releases/latest"))
        );
        assert_eq!(split_feed("http://x/y"), None);
        assert_eq!(split_feed("https://x"), None);
        assert_eq!(split_feed("https:///y"), None);
    }

    #[test]
    fn persisted_round_trip() {
        let state = PersistedCheck {
            last_attempt: 12,
            offered: Some("1.2.3".into()),
        };
        assert_eq!(PersistedCheck::parse(&state.format()), state);
    }
}
