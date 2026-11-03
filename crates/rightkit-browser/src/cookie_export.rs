//! Export a browser's cookies to a private Netscape `cookies.txt` artifact (feature
//! `cookie-export`).
//!
//! [`CookieExport`] keeps only the cookies of declared domains, writes them in the format
//! [`parse_netscape`] reads and publishes the file through rightkit-fs's
//! `AtomicWriter`: owner-only from creation, flushed and replaced atomically. The file is a
//! credential, so the rules are fail-closed:
//!
//! * An export with no cookie in the declared domains, or with a cookie the file format cannot
//!   carry (unless [`CookieExport::skip_invalid`] is set), writes nothing. The previous file stays
//!   byte for byte as it was.
//! * The text is parsed back and compared with the cookies selected before anything is written,
//!   and the staged temp file is read back and compared with the text before it replaces the
//!   artifact. A truncated or altered write is refused, with the old file intact.
//! * Errors and `Debug` output carry counts, field names and paths, never a cookie name or value.
//!
//! The module filters by domain and serialises. It does not read cookies from a browser or web
//! view (the caller does, and builds [`NetscapeCookie`] values with [`NetscapeCookie::new`]), it
//! asks for no consent, and it knows nothing about an engine. [`CookieExport::clear_with`] runs a
//! callback once the artifact is gone, which is where the caller invalidates whatever
//! authentication snapshot it handed to an engine.

use crate::cookies::{
    domain_ok, parse_netscape, NetscapeCookie, MAX_COOKIES, MAX_COOKIES_FILE_BYTES,
};
use rightkit_fs::{AtomicWriteOptions, AtomicWriter, FsError, Privacy, ReplaceMode, RetryPolicy};
use std::fs;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

const HEADER: &str = "# Netscape HTTP Cookie File\n";
const HTTP_ONLY_PREFIX: &str = "#HttpOnly_";

/// Why an export or a clear did not happen. No variant carries a cookie name or value. For every
/// variant except [`CookieExportError::Publish`] with
/// [`target_replaced`](CookieExportError::target_replaced) true, the artifact at the path is
/// exactly what it was before the call.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CookieExportError {
    /// [`CookieExport::new`] was given no domain.
    #[error("no cookie domains were declared")]
    NoDomains,
    /// A declared domain is empty or holds whitespace, control characters, a slash, backslash,
    /// `;` or `,`, or starts with `#`.
    #[error("declared cookie domain {domain:?} is empty or malformed")]
    BadDomain { domain: String },
    /// No cookie was left to write: none matched the declared domains, or (with
    /// [`CookieExport::skip_invalid`]) every match was invalid. Nothing was written.
    #[error(
        "no exportable cookies: {offered} offered, {in_scope} in the declared domains, \
         {invalid} invalid; nothing was written"
    )]
    NothingToExport {
        offered: usize,
        in_scope: usize,
        invalid: usize,
    },
    /// A cookie in the declared domains has a field the file format cannot carry (a tab or line
    /// break in a name or value, a path without a leading slash, an expiry of zero or below).
    /// `index` is its position in the slice offered. Nothing was written.
    #[error(
        "cookie {index} in the declared domains has an invalid {field} field; nothing was written"
    )]
    InvalidCookie { index: usize, field: &'static str },
    /// More cookies matched than a file can hold ([`MAX_COOKIES`]).
    #[error("{count} cookies match; a file holds at most {limit}; nothing was written")]
    TooManyCookies { count: usize, limit: usize },
    /// The file would be over [`MAX_COOKIES_FILE_BYTES`], which [`parse_netscape`] would refuse.
    #[error("the export would be {bytes} bytes; the limit is {limit}; nothing was written")]
    TooLarge { bytes: usize, limit: usize },
    /// The serialised text did not parse back to the cookies selected. Nothing was written.
    #[error("the export did not round-trip through parse_netscape ({0}); nothing was written")]
    NotRoundTrippable(String),
    /// rightkit-fs could not publish the file. Check
    /// [`target_replaced`](CookieExportError::target_replaced).
    #[error("publishing the cookie file failed: {0}")]
    Publish(#[source] FsError),
    /// The artifact could not be removed (or is not a regular file).
    #[error("clearing the cookie file failed: {0}")]
    Clear(#[source] FsError),
}

impl CookieExportError {
    /// True when the failure came after the new file replaced the old one (a directory sync or
    /// temp cleanup failed): the artifact holds the new, complete export. False for every other
    /// error: the old artifact is untouched.
    pub fn target_replaced(&self) -> bool {
        matches!(self, Self::Publish(e) if e.target_replaced())
    }
}

/// What a successful export did. Counts only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportReport {
    /// Cookies written.
    pub exported: usize,
    /// Cookies offered that are outside the declared domains (not written).
    pub out_of_scope: usize,
    /// Cookies in the declared domains that the format cannot carry, left out because
    /// [`CookieExport::skip_invalid`] is set. Always zero otherwise.
    pub invalid_skipped: usize,
}

/// What [`CookieExport::clear`] found. After either value the artifact does not exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClearOutcome {
    /// The file existed and was removed.
    Removed,
    /// There was no file.
    Absent,
}

/// Declared domains and the serialisation choices for one cookie artifact. Holds no cookie data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieExport {
    domains: Vec<String>,
    mark_http_only: bool,
    skip_invalid: bool,
}

fn normalise_declared(raw: &str) -> Option<String> {
    let d = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    domain_ok(&d).then_some(d)
}

impl CookieExport {
    /// Declare the domains to keep. Each is trimmed and lowercased, a leading dot is dropped, and
    /// it covers the domain itself and every subdomain (`instagram.com` keeps `instagram.com`,
    /// `.instagram.com` and `www.instagram.com`, not `notinstagram.com` or
    /// `instagram.com.evil.example`). There is no public-suffix check: declaring `com` keeps
    /// every `.com` cookie, so the caller declares real service domains.
    ///
    /// Defaults: the HttpOnly prefix is not written ([`CookieExport::mark_http_only`]) and a
    /// cookie the format cannot carry fails the export ([`CookieExport::skip_invalid`]).
    pub fn new<I, S>(domains: I) -> Result<Self, CookieExportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out: Vec<String> = Vec::new();
        for raw in domains {
            let raw = raw.as_ref();
            let d = normalise_declared(raw).ok_or_else(|| CookieExportError::BadDomain {
                domain: raw.to_string(),
            })?;
            if !out.contains(&d) {
                out.push(d);
            }
        }
        if out.is_empty() {
            return Err(CookieExportError::NoDomains);
        }
        Ok(Self {
            domains: out,
            mark_http_only: false,
            skip_invalid: false,
        })
    }

    /// Write the `#HttpOnly_` host prefix for HttpOnly cookies (default `false`).
    ///
    /// The prefix is what curl, yt-dlp and Python's `MozillaCookieJar` write and read, and
    /// [`parse_netscape`] understands it. A reader that treats every line starting with `#` as a
    /// comment drops those cookies, and an HttpOnly cookie is usually the session cookie, so a
    /// sign-in would silently break. Turn this on only when every reader of the file handles the
    /// prefix. Off, HttpOnly cookies are still exported, as ordinary lines.
    #[must_use]
    pub fn mark_http_only(mut self, mark: bool) -> Self {
        self.mark_http_only = mark;
        self
    }

    /// Leave out a cookie of a declared domain that the format cannot carry instead of failing the
    /// export (default `false`). The count is in [`ExportReport::invalid_skipped`]. Skipping a
    /// session cookie can produce a file that no longer signs in; the default refuses instead.
    #[must_use]
    pub fn skip_invalid(mut self, skip: bool) -> Self {
        self.skip_invalid = skip;
        self
    }

    /// The normalised declared domains.
    pub fn domains(&self) -> &[String] {
        &self.domains
    }

    /// Whether a cookie domain (leading dot and case ignored) is a declared domain or one of its
    /// subdomains.
    pub fn matches(&self, cookie_domain: &str) -> bool {
        let c = cookie_domain
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        self.domains.iter().any(|d| {
            c == *d
                || (c.len() > d.len()
                    && c.ends_with(d.as_str())
                    && c.as_bytes()[c.len() - d.len() - 1] == b'.')
        })
    }

    /// Filter, serialise and verify in memory. Returns the file text and the report.
    fn prepare(
        &self,
        cookies: &[NetscapeCookie],
    ) -> Result<(String, ExportReport), CookieExportError> {
        let mut selected: Vec<NetscapeCookie> = Vec::new();
        let mut out_of_scope = 0usize;
        let mut in_scope = 0usize;
        let mut invalid = 0usize;
        for (index, cookie) in cookies.iter().enumerate() {
            if !self.matches(&cookie.domain) {
                out_of_scope += 1;
                continue;
            }
            in_scope += 1;
            let mut c = cookie.clone();
            c.domain = c.domain.trim().trim_start_matches('.').to_ascii_lowercase();
            if let Some(field) = c.invalid_field() {
                if self.skip_invalid {
                    invalid += 1;
                    continue;
                }
                return Err(CookieExportError::InvalidCookie { index, field });
            }
            if !self.mark_http_only {
                c.http_only = false;
            }
            selected.push(c);
        }
        if selected.is_empty() {
            return Err(CookieExportError::NothingToExport {
                offered: cookies.len(),
                in_scope,
                invalid,
            });
        }
        if selected.len() > MAX_COOKIES {
            return Err(CookieExportError::TooManyCookies {
                count: selected.len(),
                limit: MAX_COOKIES,
            });
        }
        let text = render(&selected);
        if text.len() > MAX_COOKIES_FILE_BYTES {
            return Err(CookieExportError::TooLarge {
                bytes: text.len(),
                limit: MAX_COOKIES_FILE_BYTES,
            });
        }
        match parse_netscape(&text) {
            Err(e) => {
                return Err(CookieExportError::NotRoundTrippable(e.to_string()));
            }
            Ok(parsed) if parsed != selected => {
                return Err(CookieExportError::NotRoundTrippable(
                    "cookie fields differ".to_string(),
                ));
            }
            Ok(_) => {}
        }
        let report = ExportReport {
            exported: selected.len(),
            out_of_scope,
            invalid_skipped: invalid,
        };
        Ok((text, report))
    }

    /// Keep the cookies of the declared domains and publish them to `path` as a private Netscape
    /// file, replacing any file already there.
    ///
    /// The file is created owner-only (`0600`, parents `0700` when created; an owner-only DACL on
    /// Windows), flushed, and replaced atomically with a bounded retry of a Windows sharing
    /// violation. A pre-existing file with broader permissions is replaced by an owner-only one.
    /// If this returns an error the file at `path` is unchanged, except when
    /// [`CookieExportError::target_replaced`] is true (the new, complete file is in place and
    /// only the directory sync or temp cleanup failed). No temp file is left behind.
    ///
    /// Each kept cookie is written as `domain flag path secure expiry name value`: a cookie that
    /// matches subdomains gets a leading dot and `TRUE`, a host-only cookie neither; a session
    /// cookie has expiry `0`. See [`CookieExport::mark_http_only`] for the HttpOnly flag.
    pub fn export_to(
        &self,
        path: &Path,
        cookies: &[NetscapeCookie],
    ) -> Result<ExportReport, CookieExportError> {
        let (text, report) = self.prepare(cookies)?;
        publish(path, text.as_bytes())?;
        Ok(report)
    }

    /// Remove the artifact at `path`. A missing file is [`ClearOutcome::Absent`], not an error. A
    /// directory is refused, and a symbolic link is removed itself, never followed. The directory
    /// entry is synced best-effort afterwards.
    ///
    /// The caller must also drop anything derived from the file that it handed to an engine (an
    /// authentication snapshot): use [`CookieExport::clear_with`] to run that step only when the
    /// file is really gone.
    pub fn clear(path: &Path) -> Result<ClearOutcome, CookieExportError> {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ClearOutcome::Absent),
            Err(e) => return Err(CookieExportError::Clear(FsError::io(path, e))),
        };
        if meta.is_dir() {
            return Err(CookieExportError::Clear(FsError::NotAFile(
                path.to_path_buf(),
            )));
        }
        match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    let _ = rightkit_fs::try_sync_dir(parent);
                }
                Ok(ClearOutcome::Removed)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ClearOutcome::Absent),
            Err(e) => Err(CookieExportError::Clear(FsError::io(path, e))),
        }
    }

    /// [`CookieExport::clear`], then `invalidate(outcome)` once the artifact is confirmed gone
    /// ([`ClearOutcome::Removed`] or [`ClearOutcome::Absent`]). The callback is where the caller
    /// invalidates its engine authentication snapshot, so the engine never keeps treating a
    /// signed-out state as signed in. It is not called when the clear fails: the file may still
    /// exist, and the error is returned instead.
    pub fn clear_with(
        path: &Path,
        invalidate: impl FnOnce(ClearOutcome),
    ) -> Result<ClearOutcome, CookieExportError> {
        let outcome = Self::clear(path)?;
        invalidate(outcome);
        Ok(outcome)
    }
}

/// The file text for cookies that already passed validation.
fn render(cookies: &[NetscapeCookie]) -> String {
    let mut text = String::from(HEADER);
    for c in cookies {
        if c.http_only {
            text.push_str(HTTP_ONLY_PREFIX);
        }
        if c.include_subdomains {
            text.push('.');
        }
        text.push_str(&c.domain);
        text.push('\t');
        text.push_str(if c.include_subdomains {
            "TRUE"
        } else {
            "FALSE"
        });
        text.push('\t');
        text.push_str(&c.path);
        text.push('\t');
        text.push_str(if c.secure { "TRUE" } else { "FALSE" });
        text.push('\t');
        text.push_str(&c.expires.unwrap_or(0).to_string());
        text.push('\t');
        text.push_str(&c.name);
        text.push('\t');
        text.push_str(c.value());
        text.push('\n');
    }
    text
}

fn publish_options() -> AtomicWriteOptions {
    AtomicWriteOptions {
        private: true,
        create_parents: true,
        durable: true,
        replace: ReplaceMode::Rename,
        retry_sharing_violation: Some(RetryPolicy::new(5, Duration::from_millis(50))),
        privacy: Privacy::OwnerOnly,
    }
}

/// The staged temp file holds exactly the bytes that were meant to be published.
fn verify_staged(temp: &Path, expected: &[u8]) -> std::io::Result<()> {
    let staged = fs::read(temp)?;
    if staged == expected {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "staged cookie file does not match the export",
        ))
    }
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), CookieExportError> {
    let mut writer =
        AtomicWriter::create_with(path, publish_options()).map_err(CookieExportError::Publish)?;
    // A failed write drops the writer, which removes the temp file; the target is untouched.
    writer
        .write_all(bytes)
        .map_err(CookieExportError::Publish)?;
    writer
        .commit_checked(|temp| verify_staged(temp, bytes))
        .map_err(CookieExportError::Publish)
}
