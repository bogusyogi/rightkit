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

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "s3cr3t-VALUE-9f2a";

    fn ck(domain: &str, name: &str, value: &str) -> NetscapeCookie {
        NetscapeCookie::new(domain, name, value).unwrap()
    }

    fn export(domains: &[&str]) -> CookieExport {
        CookieExport::new(domains).unwrap()
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn old_artifact(dir: &Path) -> (std::path::PathBuf, Vec<u8>) {
        let path = dir.join("auth").join("x_cookies.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = b"# Netscape HTTP Cookie File\n.x.com\tTRUE\t/\tTRUE\t0\tauth_token\tOLD-GOOD\n"
            .to_vec();
        rightkit_fs::atomic_write_private(&path, &old).unwrap();
        (path, old)
    }

    #[test]
    fn declared_domains_are_validated_and_normalised() {
        assert!(matches!(
            CookieExport::new(Vec::<&str>::new()),
            Err(CookieExportError::NoDomains)
        ));
        for bad in ["", ".", "  ", "a b.com", "a/b.com", "a;b", "#x.com"] {
            let e = CookieExport::new([bad]).unwrap_err();
            assert!(matches!(e, CookieExportError::BadDomain { .. }), "{bad:?}");
        }
        let e = export(&[" .Instagram.COM ", "instagram.com", "Threads.net"]);
        assert_eq!(e.domains(), ["instagram.com", "threads.net"]);
    }

    #[test]
    fn matching_covers_subdomains_and_host_only_but_not_lookalikes() {
        let e = export(&["instagram.com"]);
        for yes in [
            "instagram.com",
            ".instagram.com",
            "INSTAGRAM.com",
            "www.instagram.com",
            ".i.instagram.com",
        ] {
            assert!(e.matches(yes), "{yes}");
        }
        for no in [
            "notinstagram.com",
            "instagram.com.evil.example",
            "instagram.co",
            "com",
            "",
            "x.com",
            "xinstagram.com",
        ] {
            assert!(!e.matches(no), "{no}");
        }
        // A declared subdomain does not widen to its parent.
        let sub = export(&["www.instagram.com"]);
        assert!(sub.matches("www.instagram.com") && sub.matches("a.www.instagram.com"));
        assert!(!sub.matches("instagram.com") && !sub.matches("api.instagram.com"));
    }

    fn mixed() -> Vec<NetscapeCookie> {
        vec![
            ck(".instagram.com", "sessionid", SECRET)
                .with_secure(true)
                .with_http_only(true)
                .with_expires(Some(1_900_000_000))
                .unwrap(),
            ck("www.instagram.com", "csrftoken", "tok")
                .with_path("/app")
                .unwrap(),
            ck("instagram.com", "mid", "m1").with_include_subdomains(true),
            ck("notinstagram.com", "other", "leak-1"),
            ck(".x.com", "auth_token", "leak-2"),
            ck("instagram.com.evil.example", "evil", "leak-3"),
        ]
    }

    #[test]
    fn exports_exactly_the_declared_subset_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth").join("instagram_cookies.txt");
        let report = export(&["instagram.com"])
            .mark_http_only(true)
            .export_to(&path, &mixed())
            .unwrap();
        assert_eq!(
            report,
            ExportReport {
                exported: 3,
                out_of_scope: 3,
                invalid_skipped: 0
            }
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Netscape HTTP Cookie File\n"));
        assert!(!text.contains("leak-"), "out-of-scope cookie written");
        assert!(text.contains(SECRET));
        let parsed = parse_netscape(&text).unwrap();
        let expected: Vec<NetscapeCookie> = mixed().into_iter().take(3).collect();
        // The first cookie is HttpOnly and the prefix is on, so it must survive.
        assert!(expected[0].http_only);
        assert_eq!(parsed, expected);
        // Exact line shapes.
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1],
            format!("#HttpOnly_.instagram.com\tTRUE\t/\tTRUE\t1900000000\tsessionid\t{SECRET}")
        );
        assert_eq!(
            lines[2],
            "www.instagram.com\tFALSE\t/app\tFALSE\t0\tcsrftoken\ttok"
        );
        assert_eq!(lines[3], ".instagram.com\tTRUE\t/\tFALSE\t0\tmid\tm1");
        // Only the artifact exists: no temp file left.
        assert_eq!(dir_names(path.parent().unwrap()), ["instagram_cookies.txt"]);
    }

    #[test]
    fn http_only_prefix_is_off_by_default_and_the_cookie_is_still_exported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        export(&["instagram.com"])
            .export_to(&path, &mixed())
            .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("#HttpOnly_"));
        // A reader that skips every `#` line still sees the session cookie.
        let visible: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(visible.iter().any(|l| l.contains("\tsessionid\t")));
        let parsed = parse_netscape(&text).unwrap();
        assert!(parsed.iter().all(|c| !c.http_only));
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].value(), SECRET);
        assert!(parsed[0].secure);
        assert_eq!(parsed[0].expires, Some(1_900_000_000));
    }

    #[test]
    fn session_host_only_secure_and_path_fields_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        let cookies = vec![
            ck("a.com", "s", "").with_secure(true),
            ck("b.a.com", "t", "x y").with_path("/p/q").unwrap(),
        ];
        export(&["a.com"]).export_to(&path, &cookies).unwrap();
        let parsed = parse_netscape(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed, cookies);
        assert!(!parsed[0].include_subdomains && parsed[0].expires.is_none());
        assert_eq!(parsed[1].value(), "x y");
    }

    #[test]
    fn caller_edits_to_public_fields_are_normalised_or_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        let mut shouty = ck("a.com", "n", "v");
        shouty.domain = ".A.COM".to_string();
        export(&["a.com"]).export_to(&path, &[shouty]).unwrap();
        assert_eq!(
            parse_netscape(&fs::read_to_string(&path).unwrap()).unwrap()[0].domain,
            "a.com"
        );

        let (old_path, old) = old_artifact(dir.path());
        let mut broken = ck("a.com", "n", SECRET);
        broken.name = "a\tb".to_string();
        let e = export(&["a.com"])
            .export_to(&old_path, &[broken])
            .unwrap_err();
        assert!(matches!(
            e,
            CookieExportError::InvalidCookie {
                index: 0,
                field: "name"
            }
        ));
        assert_eq!(fs::read(&old_path).unwrap(), old);
    }

    #[test]
    fn empty_or_unmatched_export_leaves_the_old_artifact_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = old_artifact(dir.path());
        let before = dir_names(path.parent().unwrap());

        for cookies in [
            Vec::new(),
            vec![ck("other.example", "n", SECRET)],
            vec![
                ck("notx.com", "n", SECRET),
                ck("x.com.evil.example", "m", SECRET),
            ],
        ] {
            let e = export(&["x.com"]).export_to(&path, &cookies).unwrap_err();
            assert!(
                matches!(e, CookieExportError::NothingToExport { .. }),
                "{e}"
            );
            assert!(!e.target_replaced());
            assert!(!e.to_string().contains(SECRET));
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(dir_names(path.parent().unwrap()), before);
        }
    }

    #[test]
    fn invalid_cookie_fails_the_export_by_default_and_leaves_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = old_artifact(dir.path());
        let good = ck("x.com", "auth_token", "new");
        let mut bad = ck("x.com", "ct0", SECRET);
        bad.path = "relative".to_string();

        let e = export(&["x.com"])
            .export_to(&path, &[good.clone(), bad.clone()])
            .unwrap_err();
        assert!(matches!(
            e,
            CookieExportError::InvalidCookie {
                index: 1,
                field: "path"
            }
        ));
        assert!(!e.to_string().contains(SECRET) && !format!("{e:?}").contains(SECRET));
        assert_eq!(fs::read(&path).unwrap(), old);

        // Opt-in: the valid cookie is exported, the invalid one counted.
        let report = export(&["x.com"])
            .skip_invalid(true)
            .export_to(&path, &[good, bad.clone()])
            .unwrap();
        assert_eq!(
            report,
            ExportReport {
                exported: 1,
                out_of_scope: 0,
                invalid_skipped: 1
            }
        );
        assert_ne!(fs::read(&path).unwrap(), old);

        // Opt-in but nothing valid left: still refuses and keeps what is there.
        let kept = fs::read(&path).unwrap();
        let e = export(&["x.com"])
            .skip_invalid(true)
            .export_to(&path, &[bad])
            .unwrap_err();
        assert!(matches!(
            e,
            CookieExportError::NothingToExport {
                offered: 1,
                in_scope: 1,
                invalid: 1
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), kept);
    }

    #[test]
    fn too_many_cookies_is_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = old_artifact(dir.path());
        let one = ck("x.com", "n", "v");
        let many = vec![one; MAX_COOKIES + 1];
        let e = export(&["x.com"]).export_to(&path, &many).unwrap_err();
        assert!(matches!(
            e,
            CookieExportError::TooManyCookies {
                limit: MAX_COOKIES,
                ..
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), old);
    }

    #[test]
    fn oversized_export_is_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = old_artifact(dir.path());
        let big = "v".repeat(4000);
        let cookies: Vec<NetscapeCookie> = (0..1100)
            .map(|i| ck("x.com", &format!("n{i}"), &big))
            .collect();
        let e = export(&["x.com"]).export_to(&path, &cookies).unwrap_err();
        assert!(matches!(e, CookieExportError::TooLarge { .. }), "{e}");
        assert_eq!(fs::read(&path).unwrap(), old);
    }

    #[test]
    fn staged_file_mismatch_aborts_the_publish() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        fs::write(&staged, b"abc").unwrap();
        assert!(verify_staged(&staged, b"abc").is_ok());
        let e = verify_staged(&staged, b"abd").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(verify_staged(&staged, b"ab").is_err());
        assert!(verify_staged(&dir.path().join("missing"), b"abc").is_err());
    }

    /// A publish that fails part-way leaves the old artifact and no temp file behind.
    ///
    /// The failure is injected with the `rightkit-fs` `atomic::seams` write budget (dev-dependency
    /// feature `test-seams`), which is deterministic and the same on every platform. A read-only
    /// parent (`chmod 0500`) cannot force the failure: the export publishes with
    /// `private: true, create_parents: true`, and `rightkit-fs` re-applies `0700` to an existing
    /// parent directory on that path, which makes it writable again, so the publish succeeds. Root
    /// and Windows ignore directory permissions as well. The seam is per thread and `export_to`
    /// runs on the calling thread, so the injection reaches the staged write.
    #[test]
    fn failed_publish_leaves_the_old_artifact_and_no_temp_file() {
        use rightkit_fs::atomic::seams::fail_write_after;
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = old_artifact(dir.path());
        let parent = path.parent().unwrap().to_path_buf();
        let result = {
            let _guard = fail_write_after(8, std::io::ErrorKind::StorageFull);
            export(&["x.com"]).export_to(&path, &[ck("x.com", "n", SECRET)])
        };
        let e = result.unwrap_err();
        assert!(matches!(e, CookieExportError::Publish(_)), "{e}");
        assert!(!e.target_replaced());
        assert!(!e.to_string().contains(SECRET));
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(dir_names(&parent), ["x_cookies.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn artifact_and_created_parents_are_private_even_over_a_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth").join("deeper").join("c.txt");
        export(&["x.com"])
            .export_to(&path, &[ck("x.com", "n", SECRET)])
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        // An existing 0644 file is replaced by an owner-only one.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        export(&["x.com"])
            .export_to(&path, &[ck("x.com", "n", "second")])
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert!(fs::read_to_string(&path).unwrap().contains("second"));
    }

    #[test]
    fn second_export_replaces_the_first_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        let e = export(&["x.com"]);
        e.export_to(&path, &[ck("x.com", "a", "1"), ck("x.com", "b", "2")])
            .unwrap();
        e.export_to(&path, &[ck("x.com", "c", "3")]).unwrap();
        let parsed = parse_netscape(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "c");
        assert_eq!(dir_names(dir.path()), ["c.txt"]);
    }

    #[test]
    fn clear_removes_the_artifact_and_reports_absence() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _old) = old_artifact(dir.path());
        assert_eq!(CookieExport::clear(&path).unwrap(), ClearOutcome::Removed);
        assert!(!path.exists());
        assert_eq!(CookieExport::clear(&path).unwrap(), ClearOutcome::Absent);
        assert_eq!(
            CookieExport::clear(&dir.path().join("never-existed")).unwrap(),
            ClearOutcome::Absent
        );
    }

    #[test]
    fn clear_with_invalidates_only_after_the_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _old) = old_artifact(dir.path());
        let mut calls = Vec::new();
        let r = CookieExport::clear_with(&path, |o| calls.push(o)).unwrap();
        assert_eq!(r, ClearOutcome::Removed);
        assert_eq!(calls, [ClearOutcome::Removed]);
        assert!(!path.exists());

        // Already absent still invalidates: the snapshot must not outlive the artifact.
        CookieExport::clear_with(&path, |o| calls.push(o)).unwrap();
        assert_eq!(calls, [ClearOutcome::Removed, ClearOutcome::Absent]);

        // A path that is a directory is refused, the callback does not run, nothing is removed.
        let sub = dir.path().join("a-directory");
        fs::create_dir(&sub).unwrap();
        let mut ran = false;
        let e = CookieExport::clear_with(&sub, |_| ran = true).unwrap_err();
        assert!(matches!(e, CookieExportError::Clear(_)), "{e}");
        assert!(!ran);
        assert!(sub.is_dir());
    }

    #[test]
    fn debug_of_export_and_report_holds_no_cookie_data() {
        let e = export(&["x.com"]).mark_http_only(true);
        let s = format!("{e:?}");
        assert!(s.contains("x.com"));
        let dir = tempfile::tempdir().unwrap();
        let r = e
            .export_to(&dir.path().join("c.txt"), &[ck("x.com", "n", SECRET)])
            .unwrap();
        assert!(!format!("{r:?}").contains(SECRET));
    }
}
