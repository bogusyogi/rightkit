//! Redirect URL-shape check, typed redirect error and per-request approved origins
//! (`Request::approved_redirect_origins`, `ApprovedOrigins`, `RedirectRefusal`,
//! `check_redirect_target`): a redirect to an origin outside the approved set is refused with
//! `HttpError::RedirectRefused` before anything is sent to it, a target with userinfo is
//! refused, and a hop to another origin drops credential headers. The https to http downgrade
//! refusal is covered end to end in `tls_local.rs` and at policy level in `audit_fixes.rs`.

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn client() -> Client {
    Client::new(ClientConfig {
        max_redirects: 5,
        retry: RetryPolicy::none(),
        ..loopback_config()
    })
}

/// A server that answers every request with a 302 to `target`.
fn redirecting_to(target: String) -> MockServer {
    MockServer::start(move |_, _| MockResponse::new(302).header("Location", &target))
}

fn refused(err: HttpError) -> (String, RedirectRefusal) {
    match err {
        HttpError::RedirectRefused { host, reason } => (host, reason),
        other => panic!("expected RedirectRefused, got {other:?}"),
    }
}

#[test]
fn redirect_to_an_unapproved_origin_is_refused_before_anything_is_sent() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let a = redirecting_to(b.url("/landing"));
    let err = client()
        .send(
            &Request::get(a.url("/start"))
                .bearer("tok-unapproved-1")
                .approved_redirect_origins(ApprovedOrigins::new()),
        )
        .unwrap_err();
    assert_eq!(err.class(), ErrorClass::Invalid);
    let text = format!("{err} {err:?}");
    assert!(!text.contains("tok-unapproved-1"), "{text}");
    assert!(!text.contains("landing"), "{text}");
    let (host, reason) = refused(err);
    assert_eq!(host, "127.0.0.1");
    assert_eq!(reason, RedirectRefusal::UnapprovedOrigin);
    assert_eq!(a.request_count(), 1);
    assert_eq!(
        b.request_count(),
        0,
        "the refused target must never be contacted"
    );
}

#[test]
fn an_approved_origin_is_followed_and_credentials_still_drop_there() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let to_b = b.url("/landing");
    let a = MockServer::start(move |req, _| match req.path() {
        "/cross" => MockResponse::new(302).header("Location", &to_b),
        "/same" => MockResponse::new(302).header("Location", "/landing"),
        _ => MockResponse::text(200, "at-a"),
    });
    let approved = ApprovedOrigins::parse([b.base_url()]).unwrap();
    let build = |path: &str, set: ApprovedOrigins| {
        Request::get(a.url(path))
            .bearer("tok-approved-1")
            .secret_header("X-Service-Key", "service-approved-2")
            .header("Cookie", "sid=3")
            .header("Accept", "text/plain")
            .approved_redirect_origins(set)
    };

    // Another origin that is in the set: followed, but the hop is cross-origin, so the
    // credential headers are dropped before it is sent.
    let resp = client().send(&build("/cross", approved.clone())).unwrap();
    assert_eq!(resp.text(), "at-b");
    let landed = &b.requests()[0];
    assert_eq!(landed.path(), "/landing");
    assert_eq!(landed.header("authorization"), None);
    assert_eq!(landed.header("x-service-key"), None);
    assert_eq!(landed.header("cookie"), None);
    assert_eq!(landed.header("accept"), Some("text/plain"));
    assert_eq!(
        a.requests()[0].header("authorization"),
        Some("Bearer tok-approved-1")
    );

    // The request's own origin needs no entry: even an empty set follows a same-origin hop,
    // and credentials go with it.
    let resp = client()
        .send(&build("/same", ApprovedOrigins::new()))
        .unwrap();
    assert_eq!(resp.text(), "at-a");
    let landed = a
        .requests()
        .into_iter()
        .find(|r| r.path() == "/landing")
        .unwrap();
    assert_eq!(
        landed.header("authorization"),
        Some("Bearer tok-approved-1")
    );
}

#[test]
fn a_later_hop_out_of_the_set_is_refused_too() {
    let c = MockServer::start(|_, _| MockResponse::text(200, "at-c"));
    let b = redirecting_to(c.url("/"));
    let a = redirecting_to(b.url("/"));
    let approved = ApprovedOrigins::parse([b.base_url()]).unwrap();
    let err = client()
        .send(&Request::get(a.url("/")).approved_redirect_origins(approved))
        .unwrap_err();
    let (_, reason) = refused(err);
    assert_eq!(reason, RedirectRefusal::UnapprovedOrigin);
    assert_eq!(a.request_count(), 1);
    assert_eq!(b.request_count(), 1);
    assert_eq!(c.request_count(), 0);
}

#[test]
fn a_request_without_an_approved_set_still_follows_other_origins() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let a = redirecting_to(b.url("/landing"));
    let resp = client()
        .send(&Request::get(a.url("/")).bearer("tok-open-1"))
        .unwrap();
    assert_eq!(resp.text(), "at-b");
    assert_eq!(b.requests()[0].header("authorization"), None);
}

#[test]
fn a_redirect_target_with_userinfo_is_refused_without_naming_it() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let a = redirecting_to(format!("http://user:pw-ui-9@127.0.0.1:{}/x", b.port()));
    let err = client().send(&Request::get(a.url("/"))).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains("pw-ui-9"), "{text}");
    let (host, reason) = refused(err);
    assert_eq!(host, "127.0.0.1");
    assert_eq!(reason, RedirectRefusal::Credentials);
    assert_eq!(b.request_count(), 0);
}

#[test]
fn check_redirect_target_is_a_string_level_guard() {
    let req = "https://api.example.com/v1/start";
    let approved = ApprovedOrigins::parse(["https://downloads.example.com"]).unwrap();
    let check = |location: &str| check_redirect_target(req, req, location, &approved);

    // Accepted: relative, own origin in any canonical spelling, approved origin, fragment dropped.
    assert_eq!(
        check("/v1/next").unwrap(),
        "https://api.example.com/v1/next"
    );
    assert_eq!(
        check("https://API.example.com:443/x").unwrap(),
        "https://API.example.com:443/x"
    );
    assert_eq!(
        check("//downloads.example.com/f.bin#part").unwrap(),
        "https://downloads.example.com/f.bin"
    );

    // Outside the set, including another port or a different subdomain of an approved host.
    for location in [
        "https://evil.example.com/",
        "https://downloads.example.com:8443/",
        "https://sub.downloads.example.com/",
    ] {
        assert_eq!(
            check(location),
            Err(RedirectRefusal::UnapprovedOrigin),
            "{location}"
        );
    }

    // https to http is refused even toward an origin that is approved or the request's own.
    for location in [
        "http://api.example.com/x",
        "http://downloads.example.com/x",
        "http://evil.example.com/x",
    ] {
        assert_eq!(
            check(location),
            Err(RedirectRefusal::SchemeDowngrade),
            "{location}"
        );
    }

    // Userinfo, including the `host@evil` disguise.
    for location in [
        "https://user:pw@downloads.example.com/",
        "https://api.example.com@evil.example.com/",
    ] {
        assert_eq!(
            check(location),
            Err(RedirectRefusal::Credentials),
            "{location}"
        );
    }

    // Shapes that are not a followable URL.
    for location in [
        "",
        "ftp://downloads.example.com/",
        "javascript:alert(1)",
        "https://api.example.com\\@evil.example.com/",
        "https://api.example.com/a b",
        "https:///path",
    ] {
        assert_eq!(
            check(location),
            Err(RedirectRefusal::Malformed),
            "{location:?}"
        );
    }

    // A scheme change that is not a downgrade is another origin.
    let plain = "http://api.example.com/";
    let none = ApprovedOrigins::new();
    assert_eq!(
        check_redirect_target(plain, plain, "https://api.example.com/", &none),
        Err(RedirectRefusal::UnapprovedOrigin)
    );
}

#[test]
fn approved_origins_parse_only_plain_origins() {
    let set = ApprovedOrigins::parse([
        "https://a.example",
        "https://a.example/",
        "http://b.example:8080",
    ])
    .unwrap();
    assert!(set.allows("https://A.EXAMPLE:443/p?q=1"));
    assert!(set.allows("http://b.example:8080/"));
    assert!(!set.allows("http://b.example/"));
    assert!(!set.allows("https://c.example/"));
    assert_eq!(
        set,
        ApprovedOrigins::new()
            .with_origin("https://a.example")
            .and_then(|s| s.with_origin("http://b.example:8080"))
            .unwrap()
    );

    for bad in [
        "",
        "a.example",
        "ftp://a.example",
        "https://u:p@a.example",
        "https://a.example/path",
        "https://a.example?x=1",
        "https://a.example#f",
        "https:///",
        "https://a.example:99999",
    ] {
        assert_eq!(ApprovedOrigins::parse([bad]), None, "{bad:?}");
    }
    // One bad entry refuses the whole set.
    assert_eq!(
        ApprovedOrigins::parse(["https://ok.example", "not-an-origin"]),
        None
    );
}

#[cfg(feature = "async")]
mod async_client {
    use super::{redirecting_to, refused};
    use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
    use rightkit_http::*;

    fn client() -> AsyncClient {
        AsyncClient::new(ClientConfig {
            max_redirects: 5,
            retry: RetryPolicy::none(),
            ..loopback_config()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn redirect_to_an_unapproved_origin_is_refused_before_anything_is_sent() {
        let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
        let a = redirecting_to(b.url("/landing"));
        let err = client()
            .send(
                &Request::get(a.url("/start"))
                    .bearer("tok-async-1")
                    .approved_redirect_origins(ApprovedOrigins::new()),
            )
            .await
            .unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(!text.contains("tok-async-1"), "{text}");
        let (_, reason) = refused(err);
        assert_eq!(reason, RedirectRefusal::UnapprovedOrigin);
        assert_eq!(a.request_count(), 1);
        assert_eq!(b.request_count(), 0);
    }

    #[tokio::test]
    async fn an_approved_origin_is_followed_and_credentials_drop_there() {
        let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
        let a = redirecting_to(b.url("/landing"));
        let approved = ApprovedOrigins::parse([b.base_url()]).unwrap();
        let resp = client()
            .send(
                &Request::get(a.url("/start"))
                    .bearer("tok-async-2")
                    .header("Cookie", "sid=7")
                    .approved_redirect_origins(approved),
            )
            .await
            .unwrap();
        assert_eq!(resp.text(), "at-b");
        assert_eq!(
            a.requests()[0].header("authorization"),
            Some("Bearer tok-async-2")
        );
        let landed = &b.requests()[0];
        assert_eq!(landed.header("authorization"), None);
        assert_eq!(landed.header("cookie"), None);
    }

    #[tokio::test]
    async fn a_redirect_target_with_userinfo_is_refused() {
        let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
        let a = redirecting_to(format!("http://user:pw-async-9@127.0.0.1:{}/x", b.port()));
        let err = client().send(&Request::get(a.url("/"))).await.unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(!text.contains("pw-async-9"), "{text}");
        let (_, reason) = refused(err);
        assert_eq!(reason, RedirectRefusal::Credentials);
        assert_eq!(b.request_count(), 0);
    }
}
