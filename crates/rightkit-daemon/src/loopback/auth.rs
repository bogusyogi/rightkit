//! Who may talk to the loopback host, decided inside the owner: the bearer token and its
//! carriers, owner-declared capability routes, the Host and Origin policy, and the CSRF rules.
//!
//! The order of the checks on one request (see [`Policy::check_context`] then
//! [`Policy::authenticate`]):
//!
//! 1. **Host.** The `Host` header must name a loopback host (`127.0.0.1`, `localhost`, `[::1]`,
//!    any port, since a tunnel forwards another port) or a host the owner listed. A page on
//!    another name that was rebound to 127.0.0.1 sends its own name and is refused (421).
//! 2. **Origin.** A request with an `Origin` header (a browser) needs an origin the owner
//!    allowed, or the host's own origin; anything else, `null` included, is 403. A request
//!    without `Origin` is a non-browser client and goes on to authentication; a state-changing
//!    one that a browser marks `Sec-Fetch-Site: cross-site` is refused even without it.
//! 3. **Credentials**, by the route's class: everything is bearer unless the owner declared the
//!    route [`Public`](RouteClass::Public) (a handler verifies its own signature, as a signed
//!    webhook does) or a [`Capability`](RouteClass::Capability) route (a capability presented in
//!    a header, a path segment or a cookie and checked by the owner's [`CapabilityVerifier`]).
//!    Route matching is exact: a lookalike path (a suffix, a longer path, another method) is a
//!    bearer route again.
//! 4. **Body type.** A state-changing request with a body must be `application/json` unless the
//!    route allows a form body, so a cross-site form post is not a request the owner sees.
//!    A capability carried in a **cookie** is ambient authority, so a state-changing request on
//!    such a route must carry an allowed `Origin`.
//!
//! The bearer token is compared in constant time. The carriers mirror CodeRight's wire: the
//! owner's legacy token headers first, then `Authorization: Bearer <token>`, and the
//! `access_token` query parameter only on routes declared [`RouteClass::BearerQueryToken`] (event
//! streams, which cannot set headers).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde_json::json;
use subtle::ConstantTimeEq;

use super::http::{Method, Request, Response};
use crate::error::{ServiceError, ERROR_UNAUTHENTICATED};

/// Shortest token the host accepts from its owner.
pub const MIN_TOKEN_LEN: usize = 16;
/// Longest token the host accepts from its owner.
pub const MAX_TOKEN_LEN: usize = 256;

/// Why a token could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The OS random source failed.
    Random,
    /// Not between [`MIN_TOKEN_LEN`] and [`MAX_TOKEN_LEN`] bytes.
    Length,
    /// A byte outside visible ASCII (no spaces, no control bytes).
    Charset,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::Random => f.write_str("the OS random source failed"),
            TokenError::Length => write!(f, "a token is {MIN_TOKEN_LEN} to {MAX_TOKEN_LEN} bytes"),
            TokenError::Charset => f.write_str("a token is visible ASCII with no spaces"),
        }
    }
}

impl std::error::Error for TokenError {}

/// The per-launch bearer token. Never printed; wiped from memory on drop (best effort: copies
/// made to build a response are not wiped).
pub struct LoopbackToken {
    text: String,
}

impl fmt::Debug for LoopbackToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoopbackToken(<redacted>)")
    }
}

impl Drop for LoopbackToken {
    fn drop(&mut self) {
        // SAFETY: overwriting every byte with ASCII zero keeps the string valid UTF-8.
        unsafe {
            for byte in self.text.as_bytes_mut() {
                std::ptr::write_volatile(byte, b'0');
            }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl LoopbackToken {
    /// A fresh token: 16 bytes from the OS random source as 32 lowercase hex digits (the shape
    /// CodeRight's daemon issues; clients treat it as opaque).
    pub fn generate() -> Result<Self, TokenError> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|_| TokenError::Random)?;
        let mut text = String::with_capacity(32);
        for byte in raw {
            text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            text.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        Ok(Self { text })
    }

    /// A token the owner chose (for example to keep a value across a reload).
    pub fn from_string(text: impl Into<String>) -> Result<Self, TokenError> {
        let text = text.into();
        if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&text.len()) {
            return Err(TokenError::Length);
        }
        if !text.bytes().all(|b| (0x21..0x7f).contains(&b)) {
            return Err(TokenError::Charset);
        }
        Ok(Self { text })
    }

    /// The token text, for the discovery record the owner writes. Do not log it.
    pub fn expose(&self) -> &str {
        &self.text
    }

    /// Constant-time check of a presented value. The length is not a secret (as in CodeRight's
    /// own check), so a different length returns early.
    pub fn verify(&self, presented: &str) -> bool {
        let (ours, theirs) = (self.text.as_bytes(), presented.as_bytes());
        ours.len() == theirs.len() && bool::from(ours.ct_eq(theirs))
    }
}

// ---------------------------------------------------------------------------------------------
// Route patterns and classes
// ---------------------------------------------------------------------------------------------

/// A pattern did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(pub String);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "route pattern: {}", self.0)
    }
}

impl std::error::Error for PatternError {}

/// `/literal/{wildcard}/more`: segments are literal text or `{name}` (any one non-empty
/// segment). Matching is exact: the same number of segments, literals equal, case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePattern {
    raw: String,
    /// `None` is a wildcard.
    segments: Vec<Option<String>>,
}

fn split_path(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    Some(rest.split('/').collect())
}

impl RoutePattern {
    pub fn parse(pattern: &str) -> Result<Self, PatternError> {
        let Some(parts) = split_path(pattern) else {
            return Err(PatternError(format!("`{pattern}` must start with `/`")));
        };
        let mut segments = Vec::with_capacity(parts.len());
        for part in parts {
            if part.is_empty() {
                return Err(PatternError(format!("`{pattern}` has an empty segment")));
            }
            if part.len() >= 2 && part.starts_with('{') && part.ends_with('}') {
                let name = &part[1..part.len() - 1];
                if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(PatternError(format!("`{part}` is not a wildcard name")));
                }
                segments.push(None);
            } else if part.contains(['{', '}']) {
                return Err(PatternError(format!(
                    "`{part}` mixes text and a wildcard; a wildcard is a whole segment"
                )));
            } else {
                segments.push(Some(part.to_string()));
            }
        }
        Ok(Self {
            raw: pattern.to_string(),
            segments,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, path: &str) -> bool {
        let Some(parts) = split_path(path) else {
            return false;
        };
        parts.len() == self.segments.len()
            && parts
                .iter()
                .zip(&self.segments)
                .all(|(part, segment)| match segment {
                    Some(literal) => literal.as_str() == *part,
                    None => !part.is_empty(),
                })
    }

    /// The text of the `index`th wildcard in `path` (0 is the first), when `path` matches.
    pub fn wildcard(&self, path: &str, index: usize) -> Option<String> {
        if !self.matches(path) {
            return None;
        }
        let parts = split_path(path)?;
        parts
            .iter()
            .zip(&self.segments)
            .filter(|(_, segment)| segment.is_none())
            .nth(index)
            .map(|(part, _)| (*part).to_string())
    }
}

/// Where a capability is presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilitySource {
    /// A request header (the name is lowercased).
    Header(String),
    /// The `index`th wildcard of the route pattern (`/join/{capability}` is `PathSegment(0)`).
    PathSegment(usize),
    /// A cookie of this name. Ambient authority: a state-changing request must carry an allowed
    /// `Origin`.
    Cookie(String),
}

/// How a route is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteClass {
    /// No host-side authentication: the owner's handler verifies its own proof (a signed
    /// webhook, say) and must fail closed.
    Public,
    /// A capability presented as `source`, verified by the [`CapabilityVerifier`].
    Capability(CapabilitySource),
    /// Bearer, and the token may also ride the `access_token` query parameter (GET only): for an
    /// event stream opened by a browser `EventSource`, which cannot set headers.
    BearerQueryToken,
}

/// One declared route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    /// `None` matches every method.
    pub method: Option<Method>,
    pub pattern: String,
    pub class: RouteClass,
    /// A form post is a legitimate body here (see the module docs, step 4).
    pub allow_form_body: bool,
}

impl RouteSpec {
    /// A route without host-side authentication, for one method.
    pub fn public(method: Method, pattern: &str) -> Self {
        Self {
            method: Some(method),
            pattern: pattern.to_string(),
            class: RouteClass::Public,
            allow_form_body: false,
        }
    }

    /// A route authenticated by a capability.
    pub fn capability(method: Option<Method>, pattern: &str, source: CapabilitySource) -> Self {
        let source = match source {
            CapabilitySource::Header(name) => CapabilitySource::Header(name.to_ascii_lowercase()),
            other => other,
        };
        Self {
            method,
            pattern: pattern.to_string(),
            class: RouteClass::Capability(source),
            allow_form_body: false,
        }
    }

    /// A GET route whose bearer token may be sent as `?access_token=`.
    pub fn bearer_query_token(pattern: &str) -> Self {
        Self {
            method: Some(Method::Get),
            pattern: pattern.to_string(),
            class: RouteClass::BearerQueryToken,
            allow_form_body: false,
        }
    }

    #[must_use]
    pub fn allow_form_body(mut self) -> Self {
        self.allow_form_body = true;
        self
    }
}

/// What a verified capability grants. The owner decides what the fields mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    pub subject: String,
    pub scopes: Vec<String>,
}

/// The owner's check of a presented capability. Runs inside the owner, before the handler.
/// An `Err` with code `unauthenticated` is 401 and counts as a failed authentication; any other
/// error is answered with its status (for example `forbidden`, 403).
pub trait CapabilityVerifier: Send + Sync + 'static {
    /// `route` is the matched pattern, `presented` the capability text, `request` the request.
    fn verify(
        &self,
        route: &str,
        presented: &str,
        request: &Request,
    ) -> Result<CapabilityGrant, ServiceError>;
}

/// How a request was admitted, handed to the owner's handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authenticated {
    /// The bearer token verified.
    Bearer,
    /// A capability verified.
    Capability(CapabilityGrant),
    /// A [`RouteClass::Public`] route: nothing was verified here.
    Public,
}

/// A refusal, before any handler ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// A credential was presented and was wrong: counts against the failed-authentication limiter.
    pub auth_failure: bool,
    /// Answer with this plain text instead of the JSON error (CodeRight answers `unauthorized`).
    pub plain: Option<&'static str>,
}

impl Reject {
    fn new(status: u16, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.to_string(),
            auth_failure: false,
            plain: None,
        }
    }

    fn unauthorized(presented_wrong: bool) -> Self {
        Self {
            status: 401,
            code: ERROR_UNAUTHENTICATED.to_string(),
            message: "unauthorized".to_string(),
            auth_failure: presented_wrong,
            plain: Some("unauthorized"),
        }
    }

    fn from_error(error: &ServiceError) -> Self {
        Self {
            status: crate::contract::status_for_error(error),
            code: error.code.clone(),
            message: error.message.clone(),
            auth_failure: error.code == ERROR_UNAUTHENTICATED,
            plain: None,
        }
    }

    pub fn into_response(self) -> Response {
        match self.plain {
            Some(text) => Response::text(self.status, text),
            None => Response::json(
                self.status,
                &json!({"error": {"code": self.code, "message": self.message, "retryable": false}}),
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Origin and host policy
// ---------------------------------------------------------------------------------------------

/// The browser-facing rules. The default allows no browser origin but the host's own, requires
/// JSON for state-changing bodies, and answers preflights for `authorization` and `content-type`.
#[derive(Debug, Clone)]
pub struct OriginPolicy {
    allowed: BTreeSet<String>,
    extra_hosts: BTreeSet<String>,
    request_headers: BTreeSet<String>,
    require_json_body: bool,
}

impl Default for OriginPolicy {
    fn default() -> Self {
        Self {
            allowed: BTreeSet::new(),
            extra_hosts: BTreeSet::new(),
            request_headers: ["authorization", "content-type"]
                .iter()
                .map(|h| (*h).to_string())
                .collect(),
            require_json_body: true,
        }
    }
}

impl OriginPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow a browser origin, exactly as the browser sends it (`tauri://localhost`,
    /// `http://localhost:5173`). No wildcard exists.
    #[must_use]
    pub fn allow_origin(mut self, origin: &str) -> Self {
        self.allowed.insert(origin.to_string());
        self
    }

    /// Allow a `Host` header value beyond the loopback names (for example a tunnel's name).
    #[must_use]
    pub fn allow_host(mut self, host: &str) -> Self {
        self.extra_hosts.insert(host.to_ascii_lowercase());
        self
    }

    /// A request header a preflight may ask to send (lowercased). `authorization` and
    /// `content-type` are always allowed.
    #[must_use]
    pub fn allow_request_header(mut self, name: &str) -> Self {
        self.request_headers.insert(name.to_ascii_lowercase());
        self
    }

    /// Whether a state-changing request with a body must be `application/json` (default true).
    #[must_use]
    pub fn require_json_body(mut self, required: bool) -> Self {
        self.require_json_body = required;
        self
    }

    pub fn allowed_origins(&self) -> impl Iterator<Item = &str> {
        self.allowed.iter().map(String::as_str)
    }

    pub fn allowed_request_headers(&self) -> impl Iterator<Item = &str> {
        self.request_headers.iter().map(String::as_str)
    }
}

const LOOPBACK_NAMES: &[&str] = &["127.0.0.1", "localhost", "::1"];

fn host_is_loopback(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    let (name, port) = if let Some(rest) = value.strip_prefix('[') {
        match rest.split_once(']') {
            Some((name, tail)) => (name.to_string(), tail.strip_prefix(':')),
            None => return false,
        }
    } else {
        match value.rsplit_once(':') {
            Some((name, port)) => (name.to_string(), Some(port)),
            None => (value.clone(), None),
        }
    };
    if let Some(port) = port {
        if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    LOOPBACK_NAMES.contains(&name.as_str())
}

// ---------------------------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------------------------

struct Compiled {
    spec: RouteSpec,
    pattern: RoutePattern,
}

/// Everything the host decides about a request before the owner's handler: built once at start.
pub(crate) struct Policy {
    token: LoopbackToken,
    legacy_headers: Vec<String>,
    origin: OriginPolicy,
    port: u16,
    routes: Vec<Compiled>,
    capability: Option<Arc<dyn CapabilityVerifier>>,
}

impl Policy {
    pub(crate) fn new(
        token: LoopbackToken,
        legacy_headers: Vec<String>,
        origin: OriginPolicy,
        port: u16,
        routes: Vec<RouteSpec>,
        capability: Option<Arc<dyn CapabilityVerifier>>,
    ) -> Result<Self, PatternError> {
        let mut compiled = Vec::with_capacity(routes.len());
        let mut needs_verifier = false;
        for spec in routes {
            let pattern = RoutePattern::parse(&spec.pattern)?;
            if let RouteClass::Capability(source) = &spec.class {
                needs_verifier = true;
                if let CapabilitySource::PathSegment(index) = source {
                    let wildcards = pattern.segments.iter().filter(|s| s.is_none()).count();
                    if *index >= wildcards {
                        return Err(PatternError(format!(
                            "`{}` has no wildcard number {index} to carry the capability",
                            spec.pattern
                        )));
                    }
                }
            }
            compiled.push(Compiled { spec, pattern });
        }
        if needs_verifier && capability.is_none() {
            return Err(PatternError(
                "a capability route needs a CapabilityVerifier".to_string(),
            ));
        }
        Ok(Self {
            token,
            legacy_headers: legacy_headers
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            origin,
            port,
            routes: compiled,
            capability,
        })
    }

    pub(crate) fn token(&self) -> &LoopbackToken {
        &self.token
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.origin.allowed.contains(origin)
            || origin == format!("http://127.0.0.1:{}", self.port)
            || origin == format!("http://localhost:{}", self.port)
    }

    fn host_allowed(&self, host: &str) -> bool {
        host_is_loopback(host) || self.origin.extra_hosts.contains(&host.to_ascii_lowercase())
    }

    /// Steps 1 and 2: the Host and Origin rules. Returns the origin to echo in CORS headers
    /// (the request's own, once allowed), if the request had one.
    pub(crate) fn check_context(&self, request: &Request) -> Result<Option<String>, Reject> {
        let host = request.header("host").unwrap_or("");
        if !self.host_allowed(host) {
            return Err(Reject::new(
                421,
                "host_not_allowed",
                "the Host header does not name this loopback service",
            ));
        }
        if let Some(origin) = request.header("origin") {
            if !self.origin_allowed(origin) {
                return Err(Reject::new(
                    403,
                    "origin_not_allowed",
                    "this origin may not use the service",
                ));
            }
            return Ok(Some(origin.to_string()));
        }
        if request.method.is_unsafe()
            && request
                .header("sec-fetch-site")
                .is_some_and(|site| site.eq_ignore_ascii_case("cross-site"))
        {
            return Err(Reject::new(
                403,
                "cross_site_request",
                "a cross-site state-changing request is refused",
            ));
        }
        Ok(None)
    }

    /// The CORS answer to a preflight whose `Origin` passed [`check_context`](Self::check_context).
    pub(crate) fn preflight(&self, request: &Request, origin: Option<&str>) -> Response {
        let Some(origin) = origin else {
            return Response::empty(204);
        };
        if let Some(asked) = request.header("access-control-request-headers") {
            let all_allowed = asked
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .all(|h| {
                    self.origin.request_headers.contains(&h)
                        || self.legacy_headers.iter().any(|legacy| *legacy == h)
                });
            if !all_allowed {
                return Response::empty(204);
            }
        }
        let headers: Vec<&str> = self
            .origin
            .request_headers
            .iter()
            .map(String::as_str)
            .chain(self.legacy_headers.iter().map(String::as_str))
            .collect();
        Response::empty(204)
            .header("access-control-allow-origin", origin)
            .header(
                "access-control-allow-methods",
                "GET, POST, PUT, DELETE, PATCH",
            )
            .header("access-control-allow-headers", &headers.join(", "))
            .header("access-control-max-age", "600")
            .header(
                "vary",
                "origin, access-control-request-method, access-control-request-headers",
            )
    }

    fn match_route(&self, request: &Request) -> Option<&Compiled> {
        self.routes.iter().find(|route| {
            route.spec.method.map_or(true, |m| m == request.method)
                && route.pattern.matches(&request.path)
        })
    }

    fn body_ok(&self, request: &Request, allow_form: bool) -> Result<(), Reject> {
        if !self.origin.require_json_body
            || allow_form
            || !request.method.is_unsafe()
            || request.body.is_empty()
        {
            return Ok(());
        }
        let is_json = request.header("content-type").is_some_and(|value| {
            let essence = value.split(';').next().unwrap_or("").trim();
            essence.eq_ignore_ascii_case("application/json")
        });
        if is_json {
            Ok(())
        } else {
            Err(Reject::new(
                415,
                "unsupported_media_type",
                "a state-changing request with a body must be application/json",
            ))
        }
    }

    /// The token as a request presents it: the owner's legacy headers, then
    /// `Authorization: Bearer`, then (only where `query_ok`) `?access_token=`.
    fn presented_token(&self, request: &Request, query_ok: bool) -> Option<String> {
        for name in &self.legacy_headers {
            if let Some(value) = request.header(name) {
                return Some(value.to_string());
            }
        }
        if let Some(value) = request
            .header("authorization")
            .and_then(|a| a.strip_prefix("Bearer "))
        {
            return Some(value.to_string());
        }
        if query_ok && request.method == Method::Get {
            return request.query.as_deref().and_then(|query| {
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("access_token="))
                    .map(str::to_string)
            });
        }
        None
    }

    fn authenticate_bearer(
        &self,
        request: &Request,
        query_ok: bool,
        allow_form: bool,
    ) -> Result<Authenticated, Reject> {
        match self.presented_token(request, query_ok) {
            Some(presented) if self.token.verify(&presented) => {
                self.body_ok(request, allow_form)?;
                Ok(Authenticated::Bearer)
            }
            Some(_) => Err(Reject::unauthorized(true)),
            None => Err(Reject::unauthorized(false)),
        }
    }

    fn authenticate_capability(
        &self,
        request: &Request,
        route: &Compiled,
        source: &CapabilitySource,
    ) -> Result<Authenticated, Reject> {
        let Some(verifier) = self.capability.as_ref() else {
            return Err(Reject::unauthorized(false));
        };
        let presented = match source {
            CapabilitySource::Header(name) => request.header(name).map(str::to_string),
            CapabilitySource::PathSegment(index) => route.pattern.wildcard(&request.path, *index),
            CapabilitySource::Cookie(name) => request.header("cookie").and_then(|cookies| {
                cookies.split(';').find_map(|pair| {
                    let (k, v) = pair.trim().split_once('=')?;
                    (k == name).then(|| v.to_string())
                })
            }),
        };
        let Some(presented) = presented.filter(|p| !p.is_empty()) else {
            return Err(Reject::unauthorized(false));
        };
        if matches!(source, CapabilitySource::Cookie(_))
            && request.method.is_unsafe()
            && request.header("origin").is_none()
        {
            // `check_context` already refused a bad Origin; a cookie needs one.
            return Err(Reject::new(
                403,
                "origin_required",
                "a state-changing request authenticated by a cookie must carry an Origin",
            ));
        }
        match verifier.verify(route.pattern.as_str(), &presented, request) {
            Ok(grant) => {
                self.body_ok(request, route.spec.allow_form_body)?;
                Ok(Authenticated::Capability(grant))
            }
            // An unknown or wrong capability is the same plain 401 as a wrong token.
            Err(error) if error.code == ERROR_UNAUTHENTICATED => Err(Reject::unauthorized(true)),
            Err(error) => Err(Reject::from_error(&error)),
        }
    }

    /// Step 3 and 4: credentials by the route's class, then the body-type rule.
    pub(crate) fn authenticate(&self, request: &Request) -> Result<Authenticated, Reject> {
        match self.match_route(request) {
            Some(route) => match &route.spec.class {
                RouteClass::Public => Ok(Authenticated::Public),
                RouteClass::Capability(source) => {
                    self.authenticate_capability(request, route, source)
                }
                RouteClass::BearerQueryToken => {
                    self.authenticate_bearer(request, true, route.spec.allow_form_body)
                }
            },
            None => self.authenticate_bearer(request, false, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    struct Verifier;

    impl CapabilityVerifier for Verifier {
        fn verify(
            &self,
            route: &str,
            presented: &str,
            _request: &Request,
        ) -> Result<CapabilityGrant, ServiceError> {
            match presented {
                "good-cap" => Ok(CapabilityGrant {
                    subject: route.to_string(),
                    scopes: vec!["read".into()],
                }),
                "no-scope" => Err(ServiceError::new(crate::error::ERROR_FORBIDDEN, "no scope")),
                _ => Err(ServiceError::new(
                    ERROR_UNAUTHENTICATED,
                    "unknown capability",
                )),
            }
        }
    }

    fn policy() -> Policy {
        Policy::new(
            LoopbackToken::from_string(TOKEN).unwrap(),
            vec!["X-Legacy-Token".to_string()],
            OriginPolicy::new().allow_origin("tauri://localhost"),
            4000,
            vec![
                RouteSpec::bearer_query_token("/sessions/{id}/events"),
                RouteSpec::public(Method::Post, "/triggers/{id}/deliver/github"),
                RouteSpec::capability(
                    Some(Method::Get),
                    "/join/{capability}",
                    CapabilitySource::PathSegment(0),
                ),
                RouteSpec::capability(
                    None,
                    "/hook",
                    CapabilitySource::Header("X-Capability".into()),
                ),
                RouteSpec::capability(
                    Some(Method::Post),
                    "/room/say",
                    CapabilitySource::Cookie("room".into()),
                )
                .allow_form_body(),
            ],
            Some(Arc::new(Verifier)),
        )
        .unwrap()
    }

    fn req(method: Method, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Request {
        let mut all = vec![("host", "127.0.0.1:4000")];
        all.extend_from_slice(headers);
        Request::for_test(method, target, &all, body)
    }

    fn bearer() -> (&'static str, String) {
        ("authorization", format!("Bearer {TOKEN}"))
    }

    #[test]
    fn tokens_are_made_and_checked_in_the_documented_shape() {
        let a = LoopbackToken::generate().unwrap();
        let b = LoopbackToken::generate().unwrap();
        assert_eq!(a.expose().len(), 32);
        assert!(a.expose().bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.expose(), b.expose());
        assert!(a.verify(a.expose()));
        assert!(!a.verify(b.expose()));
        assert!(!a.verify(""));
        assert!(!a.verify(&a.expose()[..31]));
        assert!(!format!("{a:?}").contains(a.expose()));
        assert_eq!(
            LoopbackToken::from_string("short").err(),
            Some(TokenError::Length)
        );
        assert_eq!(
            LoopbackToken::from_string("has a space in the token!!").err(),
            Some(TokenError::Charset)
        );
        assert!(LoopbackToken::from_string("x".repeat(257)).is_err());
        assert!(LoopbackToken::from_string("x".repeat(16)).is_ok());
    }

    #[test]
    fn patterns_are_exact() {
        let p = RoutePattern::parse("/sessions/{id}/events").unwrap();
        assert!(p.matches("/sessions/abc/events"));
        assert!(!p.matches("/sessions/abc/events/history"));
        assert!(!p.matches("/sessions//events"));
        assert!(!p.matches("/sessions/abc/events/"));
        assert!(!p.matches("/Sessions/abc/events"));
        assert_eq!(
            p.wildcard("/sessions/abc/events", 0).as_deref(),
            Some("abc")
        );
        assert_eq!(p.wildcard("/sessions/abc/events", 1), None);
        assert!(RoutePattern::parse("/").unwrap().matches("/"));
        assert!(RoutePattern::parse("relative").is_err());
        assert!(RoutePattern::parse("/a//b").is_err());
        assert!(RoutePattern::parse("/a/x{id}").is_err());
        assert!(RoutePattern::parse("/a/{i d}").is_err());
    }

    #[test]
    fn a_capability_route_needs_a_verifier_and_a_real_wildcard() {
        let token = || LoopbackToken::from_string(TOKEN).unwrap();
        assert!(Policy::new(
            token(),
            vec![],
            OriginPolicy::new(),
            1,
            vec![RouteSpec::capability(
                None,
                "/x",
                CapabilitySource::Header("h".into())
            )],
            None,
        )
        .is_err());
        assert!(Policy::new(
            token(),
            vec![],
            OriginPolicy::new(),
            1,
            vec![RouteSpec::capability(
                None,
                "/x/{a}",
                CapabilitySource::PathSegment(1)
            )],
            Some(Arc::new(Verifier)),
        )
        .is_err());
    }

    #[test]
    fn the_bearer_carriers_and_their_order() {
        let p = policy();
        let (name, value) = bearer();
        assert_eq!(
            p.authenticate(&req(
                Method::Get,
                "/anything",
                &[(name, value.as_str())],
                b""
            )),
            Ok(Authenticated::Bearer)
        );
        // The legacy header is read first.
        assert_eq!(
            p.authenticate(&req(
                Method::Get,
                "/anything",
                &[("x-legacy-token", TOKEN)],
                b""
            )),
            Ok(Authenticated::Bearer)
        );
        let wrong_first = req(
            Method::Get,
            "/anything",
            &[("x-legacy-token", "nope"), (name, value.as_str())],
            b"",
        );
        assert_eq!(p.authenticate(&wrong_first).unwrap_err().status, 401);
        // Wrong scheme, wrong token, no token: all 401; only a wrong token is a guess.
        let basic = p.authenticate(&req(
            Method::Get,
            "/a",
            &[("authorization", "Basic abc")],
            b"",
        ));
        assert!(!basic.unwrap_err().auth_failure);
        let wrong = p
            .authenticate(&req(
                Method::Get,
                "/a",
                &[("authorization", "Bearer nope")],
                b"",
            ))
            .unwrap_err();
        assert_eq!((wrong.status, wrong.auth_failure), (401, true));
        assert_eq!(wrong.plain, Some("unauthorized"));
        let none = p
            .authenticate(&req(Method::Get, "/a", &[], b""))
            .unwrap_err();
        assert_eq!((none.status, none.auth_failure), (401, false));
        let short = p
            .authenticate(&req(
                Method::Get,
                "/a",
                &[("authorization", "Bearer ")],
                b"",
            ))
            .unwrap_err();
        assert_eq!(short.status, 401);
    }

    #[test]
    fn the_query_token_is_for_the_declared_stream_route_only() {
        let p = policy();
        let query = format!("access_token={TOKEN}");
        let stream = format!("/sessions/abc/events?{query}");
        assert_eq!(
            p.authenticate(&req(Method::Get, &stream, &[], b"")),
            Ok(Authenticated::Bearer)
        );
        for target in [
            format!("/sessions/abc/events/history?{query}"),
            format!("/sessions/abc/messages?{query}"),
            format!("/health2?{query}"),
        ] {
            assert_eq!(
                p.authenticate(&req(Method::Get, &target, &[], b""))
                    .unwrap_err()
                    .status,
                401,
                "{target}"
            );
        }
        // Only for GET.
        let post = req(Method::Post, &stream, &[], b"");
        assert_eq!(p.authenticate(&post).unwrap_err().status, 401);
    }

    #[test]
    fn a_public_route_is_exact_and_lookalikes_stay_behind_the_token() {
        let p = policy();
        assert_eq!(
            p.authenticate(&req(
                Method::Post,
                "/triggers/t1/deliver/github",
                &[],
                b"{}"
            )),
            Ok(Authenticated::Public)
        );
        for (method, path) in [
            (Method::Get, "/triggers/t1/deliver/github"),
            (Method::Post, "/triggers/t1/deliver/github/extra"),
            (Method::Post, "/triggers/t1/deliver"),
            (Method::Post, "/triggers/t1/deliver/other"),
        ] {
            assert_eq!(
                p.authenticate(&req(method, path, &[], b""))
                    .unwrap_err()
                    .status,
                401,
                "{path}"
            );
        }
    }

    #[test]
    fn capabilities_come_from_the_declared_place_and_are_verified_by_the_owner() {
        let p = policy();
        let ok = p.authenticate(&req(Method::Get, "/join/good-cap", &[], b""));
        match ok {
            Ok(Authenticated::Capability(grant)) => {
                assert_eq!(grant.subject, "/join/{capability}");
                assert_eq!(grant.scopes, vec!["read".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        let bad = p
            .authenticate(&req(Method::Get, "/join/guess", &[], b""))
            .unwrap_err();
        assert_eq!((bad.status, bad.auth_failure), (401, true));
        let forbidden = p
            .authenticate(&req(Method::Get, "/join/no-scope", &[], b""))
            .unwrap_err();
        assert_eq!((forbidden.status, forbidden.auth_failure), (403, false));
        // The route is GET only; the bearer token does not stand in for a capability.
        assert_eq!(
            p.authenticate(&req(Method::Post, "/join/good-cap", &[], b""))
                .unwrap_err()
                .status,
            401
        );
        assert_eq!(
            p.authenticate(&req(
                Method::Post,
                "/hook",
                &[
                    ("x-capability", "good-cap"),
                    ("content-type", "application/json")
                ],
                b"{}"
            )),
            Ok(Authenticated::Capability(CapabilityGrant {
                subject: "/hook".into(),
                scopes: vec!["read".into()],
            }))
        );
        assert_eq!(
            p.authenticate(&req(Method::Post, "/hook", &[], b"{}"))
                .unwrap_err()
                .status,
            401
        );
    }

    #[test]
    fn a_cookie_capability_needs_an_allowed_origin_to_change_state() {
        let p = policy();
        let cookie = [
            ("cookie", "other=1; room=good-cap"),
            ("content-type", "application/x-www-form-urlencoded"),
        ];
        // Form bodies are fine on this route, but a state change without an Origin is not.
        let no_origin = p
            .authenticate(&req(Method::Post, "/room/say", &cookie, b"text=hi"))
            .unwrap_err();
        assert_eq!(
            (no_origin.status, no_origin.code.as_str()),
            (403, "origin_required")
        );
        let mut with_origin: Vec<(&str, &str)> = cookie.to_vec();
        with_origin.push(("origin", "http://127.0.0.1:4000"));
        let r = req(Method::Post, "/room/say", &with_origin, b"text=hi");
        assert!(p.check_context(&r).is_ok());
        assert!(matches!(
            p.authenticate(&r),
            Ok(Authenticated::Capability(_))
        ));
    }

    #[test]
    fn host_and_origin_rules() {
        let p = policy();
        let ctx = |headers: &[(&str, &str)], method: Method| {
            p.check_context(&Request::for_test(method, "/x", headers, b""))
        };
        // Loopback names on any port; a rebound foreign name is refused.
        for host in [
            "127.0.0.1:4000",
            "localhost:4000",
            "LOCALHOST:9999",
            "[::1]:4000",
            "127.0.0.1",
        ] {
            assert!(ctx(&[("host", host)], Method::Get).is_ok(), "{host}");
        }
        for host in [
            "evil.example:4000",
            "127.0.0.1.evil.example:4000",
            "localhost:evil",
            "127.0.0.1:",
            "",
        ] {
            assert_eq!(
                ctx(&[("host", host)], Method::Get).unwrap_err().status,
                421,
                "{host}"
            );
        }
        // Origins: the owner's list and the host's own; nothing else, `null` included.
        for origin in [
            "tauri://localhost",
            "http://127.0.0.1:4000",
            "http://localhost:4000",
        ] {
            assert_eq!(
                ctx(
                    &[("host", "127.0.0.1:4000"), ("origin", origin)],
                    Method::Post
                )
                .unwrap(),
                Some(origin.to_string())
            );
        }
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1:4001",
            "tauri://localhost/",
        ] {
            assert_eq!(
                ctx(
                    &[("host", "127.0.0.1:4000"), ("origin", origin)],
                    Method::Post
                )
                .unwrap_err()
                .status,
                403,
                "{origin}"
            );
        }
        // No Origin: a non-browser client, unless the browser says cross-site on a state change.
        assert_eq!(
            ctx(&[("host", "127.0.0.1:4000")], Method::Post).unwrap(),
            None
        );
        let cross = [("host", "127.0.0.1:4000"), ("sec-fetch-site", "cross-site")];
        assert_eq!(ctx(&cross, Method::Post).unwrap_err().status, 403);
        assert!(ctx(&cross, Method::Get).is_ok());
        let same = [
            ("host", "127.0.0.1:4000"),
            ("sec-fetch-site", "same-origin"),
        ];
        assert!(ctx(&same, Method::Post).is_ok());
        // An owner-listed extra host (a tunnel) is allowed.
        let tunnelled = Policy::new(
            LoopbackToken::from_string(TOKEN).unwrap(),
            vec![],
            OriginPolicy::new().allow_host("Tunnel.Internal:80"),
            4000,
            vec![],
            None,
        )
        .unwrap();
        assert!(tunnelled
            .check_context(&Request::for_test(
                Method::Get,
                "/",
                &[("host", "tunnel.internal:80")],
                b""
            ))
            .is_ok());
    }

    #[test]
    fn preflights_allow_only_listed_headers_for_allowed_origins() {
        let p = policy();
        let allowed = Request::for_test(
            Method::Options,
            "/x",
            &[
                ("host", "127.0.0.1:4000"),
                ("origin", "tauri://localhost"),
                (
                    "access-control-request-headers",
                    "Authorization, Content-Type, X-Legacy-Token",
                ),
            ],
            b"",
        );
        let response = p.preflight(&allowed, Some("tauri://localhost"));
        let header = |r: &Response, name: &str| {
            r.headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(response.status, 204);
        assert_eq!(
            header(&response, "access-control-allow-origin").as_deref(),
            Some("tauri://localhost")
        );
        assert!(header(&response, "access-control-allow-headers")
            .unwrap()
            .contains("authorization"));
        let asks_more = Request::for_test(
            Method::Options,
            "/x",
            &[
                ("host", "127.0.0.1:4000"),
                ("origin", "tauri://localhost"),
                ("access-control-request-headers", "x-anything-else"),
            ],
            b"",
        );
        assert!(header(
            &p.preflight(&asks_more, Some("tauri://localhost")),
            "access-control-allow-origin"
        )
        .is_none());
        assert!(header(&p.preflight(&allowed, None), "access-control-allow-origin").is_none());
    }

    #[test]
    fn a_state_changing_body_must_be_json_unless_the_route_allows_a_form() {
        let p = policy();
        let (name, value) = bearer();
        let json = [
            (name, value.as_str()),
            ("content-type", "Application/JSON; charset=utf-8"),
        ];
        assert!(p
            .authenticate(&req(Method::Post, "/x", &json, b"{}"))
            .is_ok());
        let text = [(name, value.as_str()), ("content-type", "text/plain")];
        assert_eq!(
            p.authenticate(&req(Method::Post, "/x", &text, b"{}"))
                .unwrap_err()
                .status,
            415
        );
        let none = [(name, value.as_str())];
        assert_eq!(
            p.authenticate(&req(Method::Post, "/x", &none, b"{}"))
                .unwrap_err()
                .status,
            415
        );
        // No body (a heartbeat) and safe methods are not subject to it.
        assert!(p.authenticate(&req(Method::Post, "/x", &none, b"")).is_ok());
        assert!(p.authenticate(&req(Method::Get, "/x", &text, b"")).is_ok());
        // The rule can be switched off by the owner.
        let lax = Policy::new(
            LoopbackToken::from_string(TOKEN).unwrap(),
            vec![],
            OriginPolicy::new().require_json_body(false),
            4000,
            vec![],
            None,
        )
        .unwrap();
        assert!(lax
            .authenticate(&req(Method::Post, "/x", &text, b"{}"))
            .is_ok());
    }
}
