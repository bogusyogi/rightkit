//! Whole-segment credential scope (`Request::credential_scope`): canonical origin
//! plus a decoded path-segment prefix, enforced on the first request and every
//! redirect hop. The address policy still wins over the scope.

use rightkit_http::netpolicy::CredentialScope;
use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

#[test]
fn whole_segment_scope_matching() {
    let scope = CredentialScope::parse("https://api.example.com/api").unwrap();
    for url in [
        "https://api.example.com/api",
        "https://api.example.com/api/",
        "https://api.example.com/api/users?id=1",
        // Canonical origin: case, trailing dot and the default port fold away.
        "https://API.example.com:443/api/users",
        "https://api.example.com./api/users",
    ] {
        assert!(scope.allows(url), "{url}");
    }
    for url in [
        // Not a whole segment.
        "https://api.example.com/apievil",
        "https://api.example.com/apievil/x",
        // Path case differs.
        "https://api.example.com/API/users",
        // Cross-origin in scheme, host, or port.
        "http://api.example.com/api/users",
        "https://other.example.com/api/users",
        "https://api.example.com:8443/api/users",
        // Traversal, raw or encoded.
        "https://api.example.com/api/../admin",
        "https://api.example.com/api/%2e%2e/admin",
        "https://api.example.com/api/./users",
        // Encoded separators and backslashes.
        "https://api.example.com/api/a%2fb",
        "https://api.example.com/api/a%5cb",
        "https://api.example.com/api\\users",
        // Double-encoded traversal and separators.
        "https://api.example.com/api/%252e%252e/admin",
        "https://api.example.com/api/a%252fb",
        "https://api.example.com/api/a%255cb",
        // Malformed escape.
        "https://api.example.com/api/%zz",
    ] {
        assert!(!scope.allows(url), "{url}");
    }
}

#[test]
fn malformed_scopes_do_not_parse() {
    for scope in [
        "https://user:pass@api.example.com/api",
        "https://api.example.com/api?x=1",
        "https://api.example.com/api#frag",
        "https://api.example.com/api\\v1",
        "https://api.example.com/api/../v1",
        "ftp://api.example.com/api",
        "api.example.com/api",
        "https:///api",
    ] {
        assert_eq!(CredentialScope::parse(scope), None, "{scope}");
    }
}

fn redirect_client() -> Client {
    Client::new(ClientConfig {
        max_redirects: 5,
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

#[test]
fn credentials_follow_in_scope_hops_and_stop_outside() {
    let server = MockServer::start(|req, _| match req.path() {
        "/api/start" => MockResponse::new(302).header("Location", "/api/next"),
        "/api/next" => MockResponse::new(302).header("Location", "/outside"),
        _ => MockResponse::text(200, "done"),
    });
    let scope = CredentialScope::parse(&format!("{}/api", server.base_url())).unwrap();
    let resp = redirect_client()
        .send(
            &Request::get(server.url("/api/start"))
                .bearer("tok-123")
                .header("X-Trace", "t-1")
                .credential_scope(scope),
        )
        .unwrap();
    assert_eq!(resp.status, 200);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].header("authorization"), Some("Bearer tok-123"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer tok-123"));
    assert_eq!(requests[2].header("authorization"), None);
    // Non-credential headers are untouched by the scope.
    assert_eq!(requests[2].header("x-trace"), Some("t-1"));
}

#[test]
fn the_first_request_outside_the_scope_sends_no_credentials() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let scope = CredentialScope::parse(&format!("{}/api", server.base_url())).unwrap();
    redirect_client()
        .send(
            &Request::get(server.url("/outside"))
                .bearer("tok-123")
                .credential_scope(scope),
        )
        .unwrap();
    assert_eq!(server.requests()[0].header("authorization"), None);
}

#[test]
fn metadata_denial_wins_over_an_allowing_scope() {
    let scope = CredentialScope::parse("http://169.254.169.254/latest").unwrap();
    assert!(scope.allows("http://169.254.169.254/latest/meta-data"));
    let client = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = client
        .send(
            &Request::get("http://169.254.169.254/latest/meta-data")
                .bearer("tok-123")
                .credential_scope(scope),
        )
        .unwrap_err();
    assert_eq!(err.class(), ErrorClass::AddressDenied, "{err:?}");
}
