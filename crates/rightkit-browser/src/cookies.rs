//! Netscape `cookies.txt` parsing and the CDP mapping that imports the cookies into the
//! current browser profile ([`BrowserPage::import_cookies`](crate::BrowserPage::import_cookies)).
//!
//! Cookie values are credentials. They are held in a redacting wrapper, never appear in
//! `Debug` output, never appear in a [`CookieParseError`] (errors carry a line number only) and
//! are scrubbed from the error text of a failed import. This module filters nothing by domain
//! and asks for no consent: which cookies reach the browser, and whether the user agreed, is
//! the caller's decision.

use chromiumoxide::cdp::browser_protocol::network::{CookieParam, TimeSinceEpoch};
use std::fmt;
use thiserror::Error;

/// Largest `cookies.txt` text [`parse_netscape`] accepts, in bytes (4 MiB).
pub const MAX_COOKIES_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Most cookies [`parse_netscape`] returns and
/// [`BrowserPage::import_cookies`](crate::BrowserPage::import_cookies) sets in one call.
pub const MAX_COOKIES: usize = 10_000;

/// A cookie value. `Debug` prints a fixed placeholder; there is no `Display` or `Serialize`.
#[derive(Clone, PartialEq, Eq)]
struct SecretValue(String);

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// One cookie from a Netscape `cookies.txt` file.
///
/// `Debug` prints every field except the value, which is shown as `<redacted>`. Read the value
/// with [`NetscapeCookie::value`] only to hand it to the browser.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NetscapeCookie {
    /// Host, ASCII-lowercased, without a leading dot (see `include_subdomains`).
    pub domain: String,
    /// The cookie also matches subdomains (flag column `TRUE`, or a leading dot on the host).
    pub include_subdomains: bool,
    /// Cookie path; `/` when the file leaves it empty.
    pub path: String,
    pub secure: bool,
    /// Set by the `#HttpOnly_` host prefix.
    pub http_only: bool,
    /// Expiry in seconds since the Unix epoch; `None` is a session cookie (file value `0`).
    pub expires: Option<i64>,
    pub name: String,
    value: SecretValue,
}

/// A cookie field refused by [`NetscapeCookie::new`] or one of its builder methods. Names the
/// field only, never its text, because cookie values are secrets.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("the {field} field is empty or malformed")]
#[non_exhaustive]
pub struct CookieFieldError {
    pub field: &'static str,
}

fn field_error(field: &'static str) -> CookieFieldError {
    CookieFieldError { field }
}

/// A host the cookie file can carry: non-empty, no whitespace, control characters, slash,
/// backslash, `;` or `,`, and not a comment marker.
pub(crate) fn domain_ok(domain: &str) -> bool {
    !domain.is_empty()
        && !domain.starts_with('#')
        && !domain
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | ';' | ','))
}

fn path_ok(path: &str) -> bool {
    path.starts_with('/') && !path.chars().any(|c| c.is_control() || c == ';')
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_control() || c == ';' || c == '=')
}

fn value_ok(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
}

impl NetscapeCookie {
    /// The cookie value. A credential: do not log it.
    pub fn value(&self) -> &str {
        &self.value.0
    }

    /// Build a cookie to export (see `CookieExport`, feature `cookie-export`) from a browser's
    /// cookie store.
    ///
    /// `domain` is trimmed and ASCII-lowercased; a leading dot is dropped and turns
    /// `include_subdomains` on (without one it starts `false`, a host-only cookie). The path
    /// starts as `/`, `secure` and `http_only` as `false` and the cookie as a session cookie.
    /// Set the rest with the `with_*` methods. Every field is checked against the same rules
    /// [`parse_netscape`] applies, so a cookie built here round-trips through the file format.
    /// The error names the field, never its text.
    pub fn new(domain: &str, name: &str, value: &str) -> Result<Self, CookieFieldError> {
        let host = domain.trim();
        let had_dot = host.starts_with('.');
        let domain = host.trim_start_matches('.').to_ascii_lowercase();
        if !domain_ok(&domain) {
            return Err(field_error("domain"));
        }
        if !name_ok(name) {
            return Err(field_error("name"));
        }
        if !value_ok(value) {
            return Err(field_error("value"));
        }
        Ok(Self {
            domain,
            include_subdomains: had_dot,
            path: "/".to_string(),
            secure: false,
            http_only: false,
            expires: None,
            name: name.to_string(),
            value: SecretValue(value.to_string()),
        })
    }

    /// Match subdomains of the domain (`true`, written with a leading dot) or only the exact host.
    #[must_use]
    pub fn with_include_subdomains(mut self, include_subdomains: bool) -> Self {
        self.include_subdomains = include_subdomains;
        self
    }

    /// Set the cookie path. It must start with `/` and hold no control character or `;`.
    pub fn with_path(mut self, path: &str) -> Result<Self, CookieFieldError> {
        if !path_ok(path) {
            return Err(field_error("path"));
        }
        self.path = path.to_string();
        Ok(self)
    }

    #[must_use]
    pub fn with_secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    #[must_use]
    pub fn with_http_only(mut self, http_only: bool) -> Self {
        self.http_only = http_only;
        self
    }

    /// Set the expiry in seconds since the Unix epoch; `None` is a session cookie. Zero or below
    /// is refused (the file format reserves `0` for a session cookie).
    pub fn with_expires(mut self, expires: Option<i64>) -> Result<Self, CookieFieldError> {
        if matches!(expires, Some(e) if e <= 0) {
            return Err(field_error("expires"));
        }
        self.expires = expires;
        Ok(self)
    }

    /// The first field the file format cannot carry, if any. The public fields can be changed
    /// after construction, so an export checks again.
    #[cfg_attr(not(feature = "cookie-export"), allow(dead_code))]
    pub(crate) fn invalid_field(&self) -> Option<&'static str> {
        if !domain_ok(&self.domain) {
            Some("domain")
        } else if !path_ok(&self.path) {
            Some("path")
        } else if !name_ok(&self.name) {
            Some("name")
        } else if !value_ok(&self.value.0) {
            Some("value")
        } else if matches!(self.expires, Some(e) if e <= 0) {
            Some("expires")
        } else {
            None
        }
    }
}

impl fmt::Debug for NetscapeCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetscapeCookie")
            .field("domain", &self.domain)
            .field("include_subdomains", &self.include_subdomains)
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("http_only", &self.http_only)
            .field("expires", &self.expires)
            .field("name", &self.name)
            .field("value", &self.value)
            .finish()
    }
}

/// Why [`parse_netscape`] refused its input. Line numbers are 1-based; no variant carries any
/// text from the file, because cookie values are secrets.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CookieParseError {
    #[error("cookies file is {bytes} bytes; the limit is {limit}")]
    InputTooLarge { bytes: usize, limit: usize },
    #[error("cookies file holds more than {limit} cookies")]
    TooManyCookies { limit: usize },
    #[error("line {line}: expected 7 tab-separated fields, found {found}")]
    BadFieldCount { line: usize, found: usize },
    #[error("line {line}: the {field} field must be TRUE or FALSE")]
    BadFlag { line: usize, field: &'static str },
    #[error("line {line}: the expiry field must be a non-negative integer")]
    BadExpires { line: usize },
    #[error("line {line}: the {field} field is empty or malformed")]
    BadField { line: usize, field: &'static str },
}

const HTTP_ONLY_PREFIX: &str = "#HttpOnly_";

fn parse_flag(raw: &str, line: usize, field: &'static str) -> Result<bool, CookieParseError> {
    if raw.eq_ignore_ascii_case("TRUE") {
        Ok(true)
    } else if raw.eq_ignore_ascii_case("FALSE") {
        Ok(false)
    } else {
        Err(CookieParseError::BadFlag { line, field })
    }
}

fn bad_field(line: usize, field: &'static str) -> CookieParseError {
    CookieParseError::BadField { line, field }
}

/// Parse Netscape `cookies.txt` text.
///
/// Each cookie line has seven tab-separated fields: host, include-subdomains flag, path, secure
/// flag, expiry (Unix seconds, `0` for a session cookie), name, value. Lines starting with `#`
/// are comments except the `#HttpOnly_` host prefix, which marks the cookie HttpOnly. Blank
/// lines are skipped and CRLF is accepted. Flags are case-insensitive. Input over
/// [`MAX_COOKIES_FILE_BYTES`] or more than [`MAX_COOKIES`] cookies is refused.
pub fn parse_netscape(text: &str) -> Result<Vec<NetscapeCookie>, CookieParseError> {
    if text.len() > MAX_COOKIES_FILE_BYTES {
        return Err(CookieParseError::InputTooLarge {
            bytes: text.len(),
            limit: MAX_COOKIES_FILE_BYTES,
        });
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    for (idx, raw_line) in text.split('\n').enumerate() {
        let line_no = idx + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let (line, http_only) = match line.strip_prefix(HTTP_ONLY_PREFIX) {
            Some(rest) => (rest, true),
            None if line.starts_with('#') => continue,
            None => (line, false),
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(CookieParseError::BadFieldCount {
                line: line_no,
                found: fields.len(),
            });
        }
        if out.len() >= MAX_COOKIES {
            return Err(CookieParseError::TooManyCookies { limit: MAX_COOKIES });
        }
        let host = fields[0].trim();
        let had_dot = host.starts_with('.');
        let domain = host.trim_start_matches('.').to_ascii_lowercase();
        if !domain_ok(&domain) {
            return Err(bad_field(line_no, "domain"));
        }
        let flag = parse_flag(fields[1].trim(), line_no, "include-subdomains")?;
        let path = match fields[2] {
            "" => "/".to_string(),
            p if path_ok(p) => p.to_string(),
            _ => return Err(bad_field(line_no, "path")),
        };
        let secure = parse_flag(fields[3].trim(), line_no, "secure")?;
        let expires = match fields[4].trim().parse::<i64>() {
            Ok(0) => None,
            Ok(n) if n > 0 => Some(n),
            _ => return Err(CookieParseError::BadExpires { line: line_no }),
        };
        let name = fields[5];
        if !name_ok(name) {
            return Err(bad_field(line_no, "name"));
        }
        let value = fields[6];
        if !value_ok(value) {
            return Err(bad_field(line_no, "value"));
        }
        out.push(NetscapeCookie {
            domain,
            include_subdomains: flag || had_dot,
            path,
            secure,
            http_only,
            expires,
            name: name.to_string(),
            value: SecretValue(value.to_string()),
        });
    }
    Ok(out)
}

/// Map parsed cookies to CDP `Network.CookieParam`s. A subdomain-matching cookie gets a leading
/// dot on its domain; a host-only cookie gets none. The result holds the values in clear text
/// and its `Debug` prints them: pass it straight to the browser and never format it.
pub(crate) fn to_cdp_params(cookies: &[NetscapeCookie]) -> Vec<CookieParam> {
    cookies
        .iter()
        .map(|c| {
            let mut p = CookieParam::new(c.name.clone(), c.value().to_string());
            p.domain = Some(if c.include_subdomains {
                format!(".{}", c.domain)
            } else {
                c.domain.clone()
            });
            p.path = Some(c.path.clone());
            p.secure = Some(c.secure);
            p.http_only = Some(c.http_only);
            p.expires = c.expires.map(|e| TimeSinceEpoch::new(e as f64));
            p
        })
        .collect()
}

/// Admission detail for an import: the cookie count and the distinct domains. Names and values
/// are left out.
pub(crate) fn admission_detail(cookies: &[NetscapeCookie]) -> String {
    let mut domains: Vec<&str> = cookies.iter().map(|c| c.domain.as_str()).collect();
    domains.sort_unstable();
    domains.dedup();
    format!(
        "import_cookies count={} domains={}",
        cookies.len(),
        domains.join(",")
    )
}

/// Replace every cookie value of four or more characters found in `message` with
/// `[redacted]`, so an error text from the browser cannot echo a credential.
pub(crate) fn scrub(message: &str, cookies: &[NetscapeCookie]) -> String {
    let mut out = message.to_string();
    for c in cookies {
        let v = c.value();
        if v.chars().count() >= 4 {
            out = out.replace(v, "[redacted]");
        }
    }
    out
}
