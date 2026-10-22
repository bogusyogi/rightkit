//! The daemon-api v1 route table. A conformance test pins this table to the
//! contract's concrete route list (the schema example and the valid fixture,
//! which must agree), so the code and the contract cannot drift silently.

use rightkit_contracts::types::daemon_api::{AuthClassId, Scope};

#[derive(Debug, Clone, Copy)]
pub struct RouteDef {
    pub id: &'static str,
    pub method: &'static str,
    pub pattern: &'static str,
    pub class: AuthClassId,
    pub mutation: bool,
    pub cas: bool,
    pub scopes: &'static [Scope],
    pub responses: &'static [u16],
}

use AuthClassId::{
    DeviceAuthenticatedMutation as Dev, DeviceCall, OauthCallback as Oauth, SignedWebhook as Hook,
};
use Scope::{Account, Channel, Venture};

macro_rules! route {
    ($id:expr, $m:expr, $p:expr, $c:expr, $mu:expr, $cas:expr, $sc:expr, $r:expr) => {
        RouteDef {
            id: $id,
            method: $m,
            pattern: $p,
            class: $c,
            mutation: $mu,
            cas: $cas,
            scopes: $sc,
            responses: $r,
        }
    };
}

pub const ROUTES: &[RouteDef] = &[
    route!(
        "device.enrol",
        "POST",
        "/v1/devices",
        Dev,
        true,
        false,
        &[Venture],
        &[201, 400, 401, 409]
    ),
    route!(
        "upload.init",
        "POST",
        "/v1/uploads",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[201, 400, 401, 403]
    ),
    route!(
        "upload.chunk",
        "PUT",
        "/v1/uploads/{uploadId}/chunks/{index}",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[204, 400, 401, 403, 404, 409, 416]
    ),
    route!(
        "upload.status",
        "GET",
        "/v1/uploads/{uploadId}",
        Dev,
        false,
        false,
        &[Venture, Channel],
        &[200, 401, 403, 404]
    ),
    route!(
        "upload.complete",
        "POST",
        "/v1/uploads/{uploadId}/complete",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[200, 400, 401, 403, 404, 409]
    ),
    route!(
        "job.update",
        "PATCH",
        "/v1/publish-jobs/{jobId}",
        Dev,
        true,
        true,
        &[Venture, Channel, Account],
        &[200, 400, 401, 403, 409, 422, 428]
    ),
    route!(
        "oauth.callback",
        "GET",
        "/v1/oauth/{provider}/callback",
        Oauth,
        false,
        false,
        &[Account],
        &[302, 400, 502]
    ),
    route!(
        "webhook.inbound",
        "POST",
        "/v1/webhooks/{provider}",
        Hook,
        false,
        false,
        &[Account],
        &[202, 401, 409, 429]
    ),
];

/// Route id of `POST /v1/call/{method}` (daemon-api 1.1.0, class `device_call`).
pub const CALL_ROUTE_ID: &str = "device.call";

/// The additive 1.1.0 route. Not in [`ROUTES`], which stays the exact 1.0.0 table the v1
/// fixture pins; [`routes_v1_1`] iterates both. Its scopes name the scopes the contract knows; which
/// scope a method needs is the service's method table, which the daemon checks per call.
pub static CALL_ROUTE: RouteDef = RouteDef {
    id: CALL_ROUTE_ID,
    method: "POST",
    pattern: "/v1/call/{method}",
    class: DeviceCall,
    mutation: true,
    cas: false,
    scopes: &[Venture, Channel, Account],
    responses: &[200, 400, 401, 403, 409, 500, 502, 503, 504],
};

/// Route id of the outbound-link upgrade `GET /v1/link` (`rk-link/1`, scope `link`).
pub const LINK_ROUTE_ID: &str = "device.link";

/// The link upgrade. Like [`CALL_ROUTE`] it is not in [`ROUTES`] and is not matched by
/// [`match_route`]: the HTTP adapter routes `GET /v1/link` to the link handshake before the
/// route table is consulted, so the daemon-api route lists and the conformance fixtures are
/// unchanged. It exists to name the route in `AUTH_REJECTED` events.
pub static LINK_ROUTE: RouteDef = RouteDef {
    id: LINK_ROUTE_ID,
    method: "GET",
    pattern: "/v1/link",
    class: DeviceCall,
    mutation: false,
    cas: false,
    scopes: &[],
    responses: &[101, 400, 401, 403],
};

/// Every route a 1.1.0 daemon serves: [`ROUTES`] then [`CALL_ROUTE`].
pub fn routes_v1_1() -> impl Iterator<Item = &'static RouteDef> {
    ROUTES.iter().chain(std::iter::once(&CALL_ROUTE))
}

pub enum Matched {
    Route(&'static RouteDef, Vec<String>),
    MethodNotAllowed,
    NotFound,
}

/// The `{provider}` path-parameter rule: `^[a-z0-9][a-z0-9-]{0,63}$`.
/// Mirrors `$defs/providerSlug` in `daemon-api.v1.schema.json`.
pub fn is_provider_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn capture(pattern: &str, path: &str) -> Option<Vec<String>> {
    let mut params = Vec::new();
    let mut p = pattern.split('/');
    let mut s = path.split('/');
    loop {
        match (p.next(), s.next()) {
            (None, None) => return Some(params),
            (Some(a), Some(b)) => {
                if a.starts_with('{') {
                    if b.is_empty() || (a == "{provider}" && !is_provider_slug(b)) {
                        // A provider outside the slug rule is not a route: 404.
                        return None;
                    }
                    params.push(b.to_string());
                } else if a != b {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

pub fn match_route(method: &str, path: &str) -> Matched {
    let mut path_known = false;
    for r in routes_v1_1() {
        if let Some(params) = capture(r.pattern, path) {
            if r.method == method {
                return Matched::Route(r, params);
            }
            path_known = true;
        }
    }
    if path_known {
        Matched::MethodNotAllowed
    } else {
        Matched::NotFound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_slug_rule() {
        for ok in ["instagram-login", "meta", "a", "0x", &"a".repeat(64)] {
            assert!(is_provider_slug(ok), "{ok}");
        }
        for bad in [
            "Instagram",
            "a_b",
            "",
            "-meta",
            "me ta",
            "méta",
            "a.b",
            &"a".repeat(65),
        ] {
            assert!(!is_provider_slug(bad), "{bad}");
        }
    }

    #[test]
    fn invalid_provider_is_not_a_route() {
        let long = format!("/v1/webhooks/{}", "a".repeat(65));
        for (m, p) in [
            ("POST", "/v1/webhooks/Instagram"),
            ("POST", "/v1/webhooks/a_b"),
            ("POST", "/v1/webhooks/"),
            ("POST", long.as_str()),
            ("GET", "/v1/oauth/Instagram/callback"),
            ("GET", "/v1/oauth/a_b/callback"),
            ("GET", "/v1/oauth//callback"),
        ] {
            assert!(matches!(match_route(m, p), Matched::NotFound), "{m} {p}");
        }
        assert!(matches!(
            match_route("POST", "/v1/webhooks/instagram-login"),
            Matched::Route(r, ref p) if r.id == "webhook.inbound" && p[0] == "instagram-login"
        ));
        assert!(matches!(
            match_route("GET", "/v1/oauth/instagram-login/callback"),
            Matched::Route(r, _) if r.id == "oauth.callback"
        ));
    }
}
