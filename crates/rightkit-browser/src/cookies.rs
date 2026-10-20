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

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "s3cr3t-VALUE-9f2a";

    fn line(f: [&str; 7]) -> String {
        f.join("\t")
    }

    #[test]
    fn parses_normal_lines() {
        let text = format!(
            "# Netscape HTTP Cookie File\n{}\n{}\n",
            line([
                ".Example.com",
                "TRUE",
                "/",
                "true",
                "1900000000",
                "sid",
                SECRET
            ]),
            line([
                "host.example.com",
                "false",
                "/app",
                "FALSE",
                "0",
                "pref",
                "dark"
            ]),
        );
        let c = parse_netscape(&text).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].domain, "example.com");
        assert!(c[0].include_subdomains && c[0].secure && !c[0].http_only);
        assert_eq!(c[0].path, "/");
        assert_eq!(c[0].expires, Some(1_900_000_000));
        assert_eq!(c[0].name, "sid");
        assert_eq!(c[0].value(), SECRET);
        assert_eq!(c[1].domain, "host.example.com");
        assert!(!c[1].include_subdomains && !c[1].secure);
        assert_eq!(c[1].path, "/app");
        assert_eq!(c[1].expires, None);
    }

    #[test]
    fn leading_dot_implies_subdomains_even_with_false_flag() {
        let text = line([".a.com", "FALSE", "/", "FALSE", "0", "n", "v"]);
        assert!(parse_netscape(&text).unwrap()[0].include_subdomains);
    }

    #[test]
    fn http_only_prefix_sets_flag_and_is_stripped() {
        let text = format!(
            "#HttpOnly_{}\n#HttpOnly_.b.com\tTRUE\t/\tTRUE\t0\tn\tv\n",
            line(["a.com", "FALSE", "/", "TRUE", "0", "tok", SECRET])
        );
        let c = parse_netscape(&text).unwrap();
        assert_eq!(c.len(), 2);
        assert!(c[0].http_only);
        assert_eq!(c[0].domain, "a.com");
        assert!(c[1].http_only);
        assert_eq!(c[1].domain, "b.com");
    }

    #[test]
    fn comments_blank_lines_crlf_and_bom_are_tolerated() {
        let text = format!(
            "\u{feff}# comment\r\n\r\n   \r\n#another\r\n{}\r\n",
            line(["a.com", "TRUE", "/", "FALSE", "5", "n", "v"])
        );
        let c = parse_netscape(&text).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].value(), "v");
    }

    #[test]
    fn empty_value_and_empty_path_are_accepted() {
        let text = line(["a.com", "TRUE", "", "FALSE", "0", "n", ""]);
        let c = parse_netscape(&text).unwrap();
        assert_eq!(c[0].value(), "");
        assert_eq!(c[0].path, "/");
    }

    #[test]
    fn session_expiry_zero_is_none() {
        let text = line(["a.com", "TRUE", "/", "FALSE", "0", "n", "v"]);
        assert_eq!(parse_netscape(&text).unwrap()[0].expires, None);
    }

    #[test]
    fn bad_field_count_reports_line_number_without_value() {
        // Six fields, the secret sitting in the first one.
        let text = format!("# c\n\n{SECRET}\ta.com\tTRUE\t/\tFALSE\t0\n");
        let err = parse_netscape(&text).unwrap_err();
        assert_eq!(err, CookieParseError::BadFieldCount { line: 3, found: 6 });
        let msg = err.to_string();
        assert!(msg.contains("line 3"), "{msg}");
        assert!(!msg.contains(SECRET), "{msg}");
        assert!(!format!("{err:?}").contains(SECRET));

        let short = format!("a.com\t{SECRET}\n");
        let err = parse_netscape(&short).unwrap_err();
        assert_eq!(err, CookieParseError::BadFieldCount { line: 1, found: 2 });
        assert!(!err.to_string().contains(SECRET));

        // Eight fields: a value cannot contain a tab.
        let long = format!("a.com\tTRUE\t/\tFALSE\t0\tn\t{SECRET}\textra");
        assert_eq!(
            parse_netscape(&long).unwrap_err(),
            CookieParseError::BadFieldCount { line: 1, found: 8 }
        );
    }

    #[test]
    fn bad_flag_expiry_and_fields_error_with_line_number_only() {
        let bad_flag = line(["a.com", "MAYBE", "/", "FALSE", "0", "n", SECRET]);
        let e = parse_netscape(&bad_flag).unwrap_err();
        assert_eq!(
            e,
            CookieParseError::BadFlag {
                line: 1,
                field: "include-subdomains"
            }
        );
        assert!(!e.to_string().contains(SECRET));

        let bad_secure = line(["a.com", "TRUE", "/", "yes", "0", "n", SECRET]);
        assert_eq!(
            parse_netscape(&bad_secure).unwrap_err(),
            CookieParseError::BadFlag {
                line: 1,
                field: "secure"
            }
        );

        for exp in ["abc", "-5", ""] {
            let t = format!(
                "# x\n{}",
                line(["a.com", "TRUE", "/", "FALSE", exp, "n", SECRET])
            );
            let e = parse_netscape(&t).unwrap_err();
            assert_eq!(e, CookieParseError::BadExpires { line: 2 });
            assert!(!e.to_string().contains(SECRET));
        }

        let no_name = line(["a.com", "TRUE", "/", "FALSE", "0", "", SECRET]);
        assert_eq!(
            parse_netscape(&no_name).unwrap_err(),
            CookieParseError::BadField {
                line: 1,
                field: "name"
            }
        );
        let no_domain = line(["", "TRUE", "/", "FALSE", "0", "n", SECRET]);
        assert_eq!(
            parse_netscape(&no_domain).unwrap_err(),
            CookieParseError::BadField {
                line: 1,
                field: "domain"
            }
        );
        let bad_path = line(["a.com", "TRUE", "app", "FALSE", "0", "n", SECRET]);
        assert_eq!(
            parse_netscape(&bad_path).unwrap_err(),
            CookieParseError::BadField {
                line: 1,
                field: "path"
            }
        );
    }

    #[test]
    fn size_limit_is_enforced() {
        let at_limit = " ".repeat(MAX_COOKIES_FILE_BYTES);
        assert!(parse_netscape(&at_limit).unwrap().is_empty());
        let over = " ".repeat(MAX_COOKIES_FILE_BYTES + 1);
        assert_eq!(
            parse_netscape(&over).unwrap_err(),
            CookieParseError::InputTooLarge {
                bytes: MAX_COOKIES_FILE_BYTES + 1,
                limit: MAX_COOKIES_FILE_BYTES
            }
        );
    }

    #[test]
    fn cookie_count_limit_is_enforced() {
        let one = line(["a.com", "TRUE", "/", "FALSE", "0", "n", "v"]);
        let ok = vec![one.as_str(); MAX_COOKIES].join("\n");
        assert_eq!(parse_netscape(&ok).unwrap().len(), MAX_COOKIES);
        let over = vec![one.as_str(); MAX_COOKIES + 1].join("\n");
        assert_eq!(
            parse_netscape(&over).unwrap_err(),
            CookieParseError::TooManyCookies { limit: MAX_COOKIES }
        );
    }

    #[test]
    fn debug_redacts_the_value() {
        let text = line(["a.com", "TRUE", "/", "FALSE", "0", "sid", SECRET]);
        let c = parse_netscape(&text).unwrap();
        let one = format!("{:?}", c[0]);
        let many = format!("{c:?}");
        let pretty = format!("{:#?}", c[0]);
        for s in [&one, &many, &pretty] {
            assert!(!s.contains(SECRET), "{s}");
            assert!(s.contains("<redacted>"), "{s}");
            assert!(s.contains("sid"), "{s}");
        }
    }

    #[test]
    fn cdp_mapping_applies_dot_semantics_and_fields() {
        let text = format!(
            "{}\n#HttpOnly_{}\n",
            line([".a.com", "TRUE", "/p", "TRUE", "1900000000", "one", "v1"]),
            line(["b.com", "FALSE", "/", "FALSE", "0", "two", "v2"]),
        );
        let params = to_cdp_params(&parse_netscape(&text).unwrap());
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "one");
        assert_eq!(params[0].value, "v1");
        assert_eq!(params[0].domain.as_deref(), Some(".a.com"));
        assert_eq!(params[0].path.as_deref(), Some("/p"));
        assert_eq!(params[0].secure, Some(true));
        assert_eq!(params[0].http_only, Some(false));
        assert_eq!(
            params[0].expires.as_ref().map(|t| *t.inner()),
            Some(1_900_000_000.0)
        );
        assert_eq!(params[1].domain.as_deref(), Some("b.com"));
        assert_eq!(params[1].secure, Some(false));
        assert_eq!(params[1].http_only, Some(true));
        assert!(params[1].expires.is_none());
        assert!(params[0].url.is_none());
    }

    #[test]
    fn builder_normalises_and_defaults() {
        let c = NetscapeCookie::new(" .Example.COM ", "sid", SECRET).unwrap();
        assert_eq!(c.domain, "example.com");
        assert!(c.include_subdomains && !c.secure && !c.http_only);
        assert_eq!((c.path.as_str(), c.expires), ("/", None));
        assert_eq!(c.value(), SECRET);
        let host_only = NetscapeCookie::new("example.com", "sid", "v").unwrap();
        assert!(!host_only.include_subdomains);
        let full = host_only
            .with_include_subdomains(true)
            .with_secure(true)
            .with_http_only(true)
            .with_path("/app")
            .unwrap()
            .with_expires(Some(1_900_000_000))
            .unwrap();
        assert!(full.include_subdomains && full.secure && full.http_only);
        assert_eq!(
            (full.path.as_str(), full.expires),
            ("/app", Some(1_900_000_000))
        );
    }

    #[test]
    fn builder_refuses_what_the_file_cannot_carry_without_echoing_values() {
        for d in ["", ".", "a b.com", "a/b", "a;b", "a,b", "a\tb", "#c.com"] {
            let e = NetscapeCookie::new(d, "n", SECRET).unwrap_err();
            assert_eq!(e.field, "domain", "{d:?}");
        }
        for n in ["", "a=b", "a;b", "a\nb", "a\tb"] {
            assert_eq!(
                NetscapeCookie::new("a.com", n, SECRET).unwrap_err().field,
                "name"
            );
        }
        for v in ["a\tb", "a\nb", "a\rb", "a\u{0}b"] {
            let e = NetscapeCookie::new("a.com", "n", v).unwrap_err();
            assert_eq!(e.field, "value");
            assert!(!e.to_string().contains('b'), "{e}");
        }
        let ok = NetscapeCookie::new("a.com", "n", "v").unwrap();
        assert_eq!(ok.clone().with_path("app").unwrap_err().field, "path");
        assert_eq!(ok.clone().with_path("/a;b").unwrap_err().field, "path");
        assert_eq!(
            ok.clone().with_expires(Some(0)).unwrap_err().field,
            "expires"
        );
        assert_eq!(ok.with_expires(Some(-1)).unwrap_err().field, "expires");
    }

    #[test]
    fn invalid_field_catches_public_field_edits() {
        let mut c = NetscapeCookie::new("a.com", "n", "v").unwrap();
        assert_eq!(c.invalid_field(), None);
        c.path = "no-slash".to_string();
        assert_eq!(c.invalid_field(), Some("path"));
        c.path = "/".to_string();
        c.name = "a\tb".to_string();
        assert_eq!(c.invalid_field(), Some("name"));
        c.name = "n".to_string();
        c.expires = Some(0);
        assert_eq!(c.invalid_field(), Some("expires"));
    }

    #[test]
    fn admission_detail_and_scrub_leak_no_values() {
        let text = format!(
            "{}\n{}\n",
            line(["a.com", "TRUE", "/", "FALSE", "0", "sid", SECRET]),
            line(["b.com", "TRUE", "/", "FALSE", "0", "tok", "xy"]),
        );
        let c = parse_netscape(&text).unwrap();
        let d = admission_detail(&c);
        assert_eq!(d, "import_cookies count=2 domains=a.com,b.com");
        assert!(!d.contains(SECRET) && !d.contains("sid"));
        let scrubbed = scrub(&format!("Invalid cookie {SECRET} fields"), &c);
        assert_eq!(scrubbed, "Invalid cookie [redacted] fields");
    }
}
