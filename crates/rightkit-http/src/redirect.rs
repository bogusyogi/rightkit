//! Redirect rules shared by the blocking and async clients. The clients follow redirects
//! themselves, one hop at a time, so the address policy runs on every hop.
//!
//! Rules match `ureq`'s own: 301/302/303 (and any other 3xx except 304) switch to `GET`
//! unless the method is `GET` or `HEAD`; 307/308 keep the method but are not followed for
//! methods that carry a body (`POST`, `PUT`, `PATCH`) or for `DELETE`, in which case the 3xx
//! response is returned as-is. A 3xx without `Location` is returned as-is.

use ureq::http::Uri;

use crate::client::Method;
use crate::netpolicy::{ApprovedOrigins, RedirectRefusal};

const BODY_HEADERS: [&str; 6] = [
    "content-type",
    "content-length",
    "content-encoding",
    "content-language",
    "content-location",
    "transfer-encoding",
];

/// Header-name fragments that mark a credential. Matched on lowercase names.
const SECRET_NAME_PARTS: [&str; 8] = [
    "authorization",
    "authentication",
    "cookie",
    "token",
    "secret",
    "api-key",
    "apikey",
    "password",
];

/// `https_only` rule for one URL (the first request when `redirect` is false, otherwise a
/// redirect target). Runs before anything is sent to the URL.
pub(crate) fn require_https(
    https_only: bool,
    scheme: &str,
    host: &str,
    redirect: bool,
) -> Result<(), crate::error::HttpError> {
    if !https_only || scheme.eq_ignore_ascii_case("https") {
        return Ok(());
    }
    Err(crate::error::HttpError::HttpsRequired {
        host: host.to_string(),
        scheme: scheme.to_ascii_lowercase(),
        redirect,
    })
}

pub(crate) fn is_redirect_status(status: u16) -> bool {
    (300..400).contains(&status) && status != 304
}

/// Method for the next hop, or `None` when the redirect must not be followed.
pub(crate) fn redirect_method(status: u16, method: Method) -> Option<Method> {
    if matches!(status, 307 | 308) {
        return match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => None,
            other => Some(other),
        };
    }
    Some(match method {
        Method::Get | Method::Head => method,
        _ => Method::Get,
    })
}

/// True for header names that carry credentials or cookies (including `set-cookie`).
pub(crate) fn is_secret_header_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_PARTS.iter().any(|p| lower.contains(p))
}

/// Drop headers that must not follow a request to another origin: credential-named
/// headers, and any header whose value contains a recorded secret value.
pub(crate) fn strip_cross_origin(headers: &mut Vec<(String, String)>, secrets: &[String]) {
    headers.retain(|(name, value)| {
        let by_name = is_secret_header_name(name);
        let by_value = secrets
            .iter()
            .any(|s| !s.is_empty() && value.contains(s.as_str()));
        !(by_name || by_value)
    });
}

/// Shape and origin rules for one resolved redirect target, on strings only: the URL must be
/// an absolute http(s) URL without whitespace, control characters or backslashes, must not
/// carry userinfo, and, when the request has an approved set, must be at the origin of
/// `request_url` or in the set. The check the clients and
/// [`crate::netpolicy::check_redirect_target`] share.
pub(crate) fn vet_target(
    request_url: &str,
    next_url: &str,
    approved: Option<&ApprovedOrigins>,
) -> Result<(), RedirectRefusal> {
    let (next, userinfo) =
        crate::netpolicy::url_origin_parts(next_url).ok_or(RedirectRefusal::Malformed)?;
    if userinfo {
        return Err(RedirectRefusal::Credentials);
    }
    let Some(approved) = approved else {
        return Ok(());
    };
    let own_origin = crate::netpolicy::url_origin_parts(request_url).map(|(origin, _)| origin);
    if own_origin.as_ref() == Some(&next) || approved.allows(next_url) {
        return Ok(());
    }
    Err(RedirectRefusal::UnapprovedOrigin)
}

/// [`vet_target`] for the clients: the refusal as a typed [`crate::error::HttpError`]. The
/// error names the target's host only, never its path, query or userinfo.
pub(crate) fn vet_redirect(
    request_url: &str,
    next_url: &str,
    approved: Option<&ApprovedOrigins>,
) -> Result<(), crate::error::HttpError> {
    vet_target(request_url, next_url, approved).map_err(|reason| {
        crate::error::HttpError::RedirectRefused {
            host: host_of(next_url).unwrap_or_default(),
            reason,
        }
    })
}

/// Drop body-describing headers when a redirect turns the request into a bodiless `GET`.
pub(crate) fn strip_body_headers(headers: &mut Vec<(String, String)>) {
    headers.retain(|(name, _)| !BODY_HEADERS.iter().any(|b| name.eq_ignore_ascii_case(b)));
}

fn default_port(scheme: &str) -> u16 {
    if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    }
}

/// `(scheme, host, port)` of an absolute http(s) URL, lowercased, or `None`.
pub(crate) fn origin(url: &str) -> Option<(String, String, u16)> {
    let uri: Uri = url.parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let host = crate::netpolicy::normalize_host(uri.host()?);
    let port = uri.port_u16().unwrap_or_else(|| default_port(&scheme));
    Some((scheme, host, port))
}

/// Scheme of an absolute URL, lowercased.
pub(crate) fn scheme_of(url: &str) -> Option<String> {
    let uri: Uri = url.parse().ok()?;
    Some(uri.scheme_str()?.to_ascii_lowercase())
}

/// Host of an absolute URL exactly as written (case, trailing dot and legacy IPv4 spelling
/// kept), for the non-canonical spelling check.
#[cfg(feature = "async")]
pub(crate) fn raw_host_of(url: &str) -> Option<String> {
    let uri: Uri = url.parse().ok()?;
    Some(uri.host()?.to_string())
}

/// Host of an absolute URL, as `ureq` will connect to it.
pub(crate) fn host_of(url: &str) -> Option<String> {
    let uri: Uri = url.parse().ok()?;
    Some(crate::netpolicy::normalize_host(uri.host()?))
}

/// Resolve a `Location` value against the URL of the request that produced it. Returns
/// `None` for an empty, unparsable or non-http(s) target. The fragment is dropped.
pub(crate) fn resolve_location(base: &str, location: &str) -> Option<String> {
    let location = location.trim().split('#').next().unwrap_or("");
    if location.is_empty() {
        return None;
    }
    let base_uri: Uri = base.parse().ok()?;
    let scheme = base_uri.scheme_str()?;
    let authority = base_uri.authority()?.as_str();
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    let first_segment = location.split(['/', '?']).next().unwrap_or("");
    let absolute = if first_segment.contains(':') {
        location.to_string()
    } else if let Some(rest) = location.strip_prefix("//") {
        format!("{scheme}://{rest}")
    } else if location.starts_with('/') {
        format!("{scheme}://{host_port}{location}")
    } else if location.starts_with('?') {
        format!("{scheme}://{host_port}{}{location}", base_uri.path())
    } else {
        let dir = match base_uri.path().rfind('/') {
            Some(i) => &base_uri.path()[..=i],
            None => "/",
        };
        format!("{scheme}://{host_port}{dir}{location}")
    };
    let absolute = normalize_dot_segments(&absolute);
    let parsed: Uri = absolute.parse().ok()?;
    match parsed.scheme_str() {
        Some(s) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https") => {}
        _ => return None,
    }
    parsed.authority()?;
    Some(absolute)
}

fn normalize_dot_segments(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let path_start = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(path_start);
    let (path, query) = match tail.find('?') {
        Some(i) => tail.split_at(i),
        None => (tail, ""),
    };
    if !path.contains("/.") {
        return url.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/').skip(1) {
        match seg {
            "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let trailing_slash = path.ends_with("/.") || path.ends_with("/..");
    let mut joined = format!("/{}", out.join("/"));
    if trailing_slash && !joined.ends_with('/') {
        joined.push('/');
    }
    format!("{scheme}://{authority}{joined}{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_forms_resolve() {
        let base = "https://u:p@a.example:8443/x/y/z?q=1";
        assert_eq!(
            resolve_location(base, "/abs?k=v").as_deref(),
            Some("https://a.example:8443/abs?k=v")
        );
        assert_eq!(
            resolve_location(base, "rel").as_deref(),
            Some("https://a.example:8443/x/y/rel")
        );
        assert_eq!(
            resolve_location(base, "../up").as_deref(),
            Some("https://a.example:8443/x/up")
        );
        assert_eq!(
            resolve_location(base, "//b.example/p").as_deref(),
            Some("https://b.example/p")
        );
        assert_eq!(
            resolve_location(base, "http://c.example/").as_deref(),
            Some("http://c.example/")
        );
        assert_eq!(
            resolve_location(base, "?only=1").as_deref(),
            Some("https://a.example:8443/x/y/z?only=1")
        );
        assert_eq!(resolve_location(base, "ftp://c.example/"), None);
        assert_eq!(resolve_location(base, "javascript:alert(1)"), None);
        assert_eq!(resolve_location(base, ""), None);
    }

    #[test]
    fn origin_compares_scheme_host_port() {
        assert_eq!(
            origin("https://A.example/p"),
            Some(("https".into(), "a.example".into(), 443))
        );
        assert_ne!(origin("http://a.example/"), origin("https://a.example/"));
        assert_eq!(origin("http://a.example:80/"), origin("http://a.example/"));
    }

    #[test]
    fn method_rules_match_ureq() {
        assert_eq!(redirect_method(302, Method::Post), Some(Method::Get));
        assert_eq!(redirect_method(303, Method::Head), Some(Method::Head));
        assert_eq!(redirect_method(307, Method::Get), Some(Method::Get));
        assert_eq!(redirect_method(307, Method::Post), None);
        assert_eq!(redirect_method(308, Method::Delete), None);
        assert!(!is_redirect_status(304));
        assert!(is_redirect_status(301));
    }

    #[test]
    fn cross_origin_strip_removes_credentials() {
        let mut headers = vec![
            ("Authorization".to_string(), "Bearer t".to_string()),
            ("X-Api-Key".to_string(), "k".to_string()),
            ("X-Custom".to_string(), "has-the-hidden-value".to_string()),
            ("Accept".to_string(), "text/plain".to_string()),
        ];
        strip_cross_origin(&mut headers, &["hidden-value".to_string()]);
        assert_eq!(
            headers,
            vec![("Accept".to_string(), "text/plain".to_string())]
        );
    }

    #[test]
    fn https_only_refuses_every_non_https_scheme_and_names_the_hop() {
        use crate::error::HttpError;
        assert!(require_https(false, "http", "h", false).is_ok());
        assert!(require_https(true, "https", "h", false).is_ok());
        assert!(require_https(true, "HTTPS", "h", true).is_ok());
        for scheme in ["http", "HTTP", "ftp", "ws", ""] {
            for hop in [false, true] {
                match require_https(true, scheme, "h", hop) {
                    Err(HttpError::HttpsRequired {
                        scheme: s,
                        redirect,
                        host,
                    }) => {
                        assert_eq!(s, scheme.to_ascii_lowercase());
                        assert_eq!(redirect, hop);
                        assert_eq!(host, "h");
                    }
                    other => panic!("{scheme:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn vet_target_refuses_userinfo_and_unsafe_shapes() {
        let req = "https://a.example/start";
        assert_eq!(vet_target(req, "https://a.example/next", None), Ok(()));
        assert_eq!(vet_target(req, "https://b.example/next", None), Ok(()));
        assert_eq!(
            vet_target(req, "https://user:pw@b.example/", None),
            Err(RedirectRefusal::Credentials)
        );
        assert_eq!(
            vet_target(req, "https://a.example@evil.example/", None),
            Err(RedirectRefusal::Credentials)
        );
        for bad in [
            "https://a.example\\@evil.example/",
            "https://a.example/pa th",
            "https://a.example/\r\nX: y",
            "https:///no-host",
            "https://a.example:notaport/",
            "ftp://a.example/",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                vet_target(req, bad, None),
                Err(RedirectRefusal::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn vet_target_enforces_the_approved_set_and_always_admits_the_request_origin() {
        let req = "https://a.example/start";
        let empty = ApprovedOrigins::new();
        let cdn = ApprovedOrigins::parse(["https://cdn.example:8443"]).unwrap();
        // Same origin, canonical forms folded.
        for next in [
            "https://a.example/other",
            "https://A.EXAMPLE:443/other",
            "https://a.example./other",
        ] {
            assert_eq!(vet_target(req, next, Some(&empty)), Ok(()), "{next}");
        }
        // Another origin, a scheme change and a port change are all outside the empty set.
        for next in [
            "https://b.example/x",
            "http://a.example/x",
            "https://a.example:8443/x",
            "https://sub.a.example/x",
        ] {
            assert_eq!(
                vet_target(req, next, Some(&empty)),
                Err(RedirectRefusal::UnapprovedOrigin),
                "{next}"
            );
        }
        assert_eq!(
            vet_target(req, "https://cdn.example:8443/f?x=1", Some(&cdn)),
            Ok(())
        );
        assert_eq!(
            vet_target(req, "https://cdn.example/f", Some(&cdn)),
            Err(RedirectRefusal::UnapprovedOrigin)
        );
        // Shape rules still apply to an approved origin.
        assert_eq!(
            vet_target(req, "https://u:p@cdn.example:8443/", Some(&cdn)),
            Err(RedirectRefusal::Credentials)
        );
    }

    #[test]
    fn vet_redirect_names_the_host_only() {
        use crate::error::HttpError;
        match vet_redirect(
            "https://a.example/",
            "https://b.example/secret/path?key=SECRET-1",
            Some(&ApprovedOrigins::new()),
        ) {
            Err(HttpError::RedirectRefused { host, reason }) => {
                assert_eq!(host, "b.example");
                assert_eq!(reason, RedirectRefusal::UnapprovedOrigin);
            }
            other => panic!("{other:?}"),
        }
        match vet_redirect("https://a.example/", "https://u:SECRET-2@b.example/", None) {
            Err(HttpError::RedirectRefused { host, reason }) => {
                assert_eq!(host, "b.example");
                assert_eq!(reason, RedirectRefusal::Credentials);
            }
            other => panic!("{other:?}"),
        }
    }
}
