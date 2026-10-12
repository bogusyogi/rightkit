//! Regression tests for the independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-rightkit-http-0.3.0.md). Each test names its finding and
//! was written from the finding's stated sequence before the fix.

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;
use std::time::Duration;

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

// ------------------------------------------------------------------ finding 8
#[test]
fn f8_response_debug_hides_configured_and_request_secrets() {
    let server = MockServer::start(|_, _| {
        MockResponse::text(200, "body")
            .header("X-Echo", "hunter2-config")
            .header("X-Echo2", "hunter2-request")
    });
    let c = Client::new(ClientConfig {
        redact_values: vec!["hunter2-config".into()],
        ..cfg()
    });
    let req = Request::get(server.url("/")).secret_header("X-Credential", "hunter2-request");
    let resp = c.send(&req).unwrap();
    let shown = format!("{resp:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    let head = c.send_headers(&req).unwrap();
    let shown = format!("{head:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    // Nothing else is lost: the real header values are still readable.
    assert_eq!(resp.header("x-echo"), Some("hunter2-config"));
}
