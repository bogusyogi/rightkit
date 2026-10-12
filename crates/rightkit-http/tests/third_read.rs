//! Regression tests for the third independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-http-third-read.md). Each test names its finding.

use std::time::Duration;

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        max_redirects: 3,
        ..loopback_config()
    }
}

// ------------------------------------------------------------------ finding 4
#[test]
fn f4_secret_form_fields_are_scrubbed_from_status_errors() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(400)
            .header("Content-Type", "application/json")
            .body(
                br#"{"error":"invalid_request","error_description":"rejected single-use"}"#
                    .to_vec(),
            )
    });
    let req = Request::post(server.url("/token")).secret_form(
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", "single-use"),
        ],
        &["refresh_token"],
    );
    let err = Client::new(cfg()).send(&req).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains("single-use"), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    assert_eq!(
        req.scrub("rejected single-use, grant refresh_token"),
        "rejected [redacted], grant refresh_token"
    );
}
