//! Mastodon and X provider presets. Registration is covered by unit tests in
//! `src/server/presets.rs` (it needs a plain-HTTP mock, the public entry point is https-only).
#![cfg(feature = "server")]
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, Secret};
use rightkit_oauth::server::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const REDIRECT: &str = "https://postright.orthiclabs.com/oauth/callback";

fn flow(p: ServerProvider) -> ServerAuthFlow {
    ServerAuthFlow::new(
        Client::new(loopback_config()),
        [p],
        Arc::new(MemoryPendingStateStore::default()),
        Duration::from_secs(300),
    )
    .unwrap()
}
fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[test]
fn mastodon_uses_instance_endpoints_scopes_and_pkce() {
    let p =
        ServerProvider::mastodon("https://mastodon.example/", "cid", "csecret", REDIRECT).unwrap();
    assert_eq!(p.id, "mastodon:mastodon.example");
    assert_eq!(
        p.config.authorization_url,
        "https://mastodon.example/oauth/authorize"
    );
    assert_eq!(p.config.token_url, "https://mastodon.example/oauth/token");
    let port = ServerProvider::mastodon("https://m.example:8443", "c", "s", REDIRECT).unwrap();
    assert_eq!(port.id, "mastodon:m.example:8443");
    assert_eq!(port.config.token_url, "https://m.example:8443/oauth/token");
    let f = flow(p);
    let req = f.begin("mastodon:mastodon.example", "acct", &[]).unwrap();
    assert!(req
        .authorize_url
        .starts_with("https://mastodon.example/oauth/authorize?"));
    let q = query(&req.authorize_url);
    assert_eq!(q["scope"], "read:accounts write:statuses write:media");
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["code_challenge_method"], "S256");
    let custom = f
        .begin("mastodon:mastodon.example", "acct", &["read".into()])
        .unwrap();
    assert_eq!(query(&custom.authorize_url)["scope"], "read");
}

#[test]
fn mastodon_rejects_unsafe_instance_urls() {
    for bad in [
        "http://mastodon.example",
        "https://u:p@mastodon.example",
        "https://mastodon.example/sub",
        "https://mastodon.example?x=1",
        "mastodon.example",
        "",
    ] {
        assert!(
            ServerProvider::mastodon(bad, "c", "s", REDIRECT).is_err(),
            "{bad} accepted"
        );
    }
}

#[test]
fn x_confidential_authorizes_with_pkce_and_exchanges_with_basic_auth() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"a1","refresh_token":"r1","expires_in":7200}),
        )
    });
    let mut p = ServerProvider::x_confidential("xid", "xsecret", REDIRECT);
    assert_eq!(p.config.token_url, X_TOKEN_URL);
    p.config.token_url = server.url("/2/oauth2/token");
    let f = flow(p);
    let req = f.begin("x", "acct", &[]).unwrap();
    assert!(req
        .authorize_url
        .starts_with(&format!("{X_AUTHORIZATION_URL}?")));
    let q = query(&req.authorize_url);
    assert_eq!(
        q["scope"],
        "tweet.read tweet.write users.read offline.access"
    );
    assert_eq!(q["code_challenge_method"], "S256");
    f.complete(CallbackQuery {
        state: Some(req.state.clone()),
        code: Some("code".into()),
        error: None,
    })
    .unwrap();
    let r = &server.requests()[0];
    let auth = r.header("authorization").unwrap();
    // base64("xid:xsecret")
    assert_eq!(auth, "Basic eGlkOnhzZWNyZXQ=");
    let form: HashMap<_, _> = r.form().into_iter().collect();
    assert!(!form.contains_key("client_secret"));
    assert!(form.contains_key("code_verifier"));
}

#[test]
fn x_public_sends_client_id_without_secret_and_refresh_rotates() {
    let server = MockServer::start(|request, n| {
        let form: HashMap<_, _> = request.form().into_iter().collect();
        assert!(request.header("authorization").is_none());
        assert!(!form.contains_key("client_secret"));
        assert_eq!(form["client_id"], "xid");
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], format!("r{n}"));
        MockResponse::json(
            200,
            &json!({"access_token":"a","refresh_token":format!("r{}", n + 1),"expires_in":7200}),
        )
    });
    let mut p = ServerProvider::x_public("xid", REDIRECT);
    p.config.token_url = server.url("/2/oauth2/token");
    let f = flow(p);
    let one = f.refresh_token("x", &Secret::new("r0")).unwrap();
    assert_eq!(one.refresh_token.as_ref().unwrap().expose(), "r1");
    let two = f.refresh_token("x", &Secret::new("r1")).unwrap();
    assert_eq!(two.refresh_token.as_ref().unwrap().expose(), "r2");
}

#[test]
fn with_default_scopes_replaces_defaults() {
    let p = ServerProvider::x_public("xid", REDIRECT).with_default_scopes(["tweet.read"]);
    let req = flow(p).begin("x", "acct", &[]).unwrap();
    assert_eq!(query(&req.authorize_url)["scope"], "tweet.read");
}
