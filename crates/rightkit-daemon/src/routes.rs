//! The daemon-api v1 route table. A conformance test pins this table to the
//! contract fixture, so the code and the contract cannot drift silently.

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
    DeviceAuthenticatedMutation as Dev, OauthCallback as Oauth, SignedWebhook as Hook,
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
        &[201, 401, 409]
    ),
    route!(
        "upload.init",
        "POST",
        "/v1/uploads",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[201, 401, 403]
    ),
    route!(
        "upload.chunk",
        "PUT",
        "/v1/uploads/{uploadId}/chunks/{index}",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[204, 401, 409, 416]
    ),
    route!(
        "upload.status",
        "GET",
        "/v1/uploads/{uploadId}",
        Dev,
        false,
        false,
        &[Venture, Channel],
        &[200, 401, 404]
    ),
    route!(
        "upload.complete",
        "POST",
        "/v1/uploads/{uploadId}/complete",
        Dev,
        true,
        false,
        &[Venture, Channel],
        &[200, 400, 401]
    ),
    route!(
        "job.update",
        "PATCH",
        "/v1/publish-jobs/{jobId}",
        Dev,
        true,
        true,
        &[Venture, Channel, Account],
        &[200, 401, 403, 409]
    ),
    route!(
        "oauth.callback",
        "GET",
        "/v1/oauth/{provider}/callback",
        Oauth,
        false,
        false,
        &[Account],
        &[302, 400]
    ),
    route!(
        "webhook.inbound",
        "POST",
        "/v1/webhooks/{provider}",
        Hook,
        false,
        false,
        &[Account],
        &[202, 401, 409]
    ),
];

pub enum Matched {
    Route(&'static RouteDef, Vec<String>),
    MethodNotAllowed,
    NotFound,
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
                    if b.is_empty() {
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
    for r in ROUTES {
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
