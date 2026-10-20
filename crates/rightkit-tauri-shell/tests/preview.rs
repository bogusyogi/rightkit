//! The preview scheme (Orthic-Labs/rightkit#386): the app's kind-to-policy map, random bounded tokens
//! that expire or are revoked, and the response served for exactly a live token. Driven through
//! `serve_preview_request_at`, the path the registered custom protocol takes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use rightkit_tauri_shell::preview::{
    preview_url, serve_preview_request, serve_preview_request_at, valid_csp, valid_kind,
    PreviewLimits, PreviewStore, DENY_CSP, MAX_TTL, TOKEN_HEX_LEN,
};
use tauri::http::{header, Method, Request, Response, StatusCode};

const SCRIPTY: &str = "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' blob:";
const PLAIN: &str = "default-src 'none'; img-src data:";

fn store() -> PreviewStore {
    PreviewStore::new([("scripty", SCRIPTY), ("plain", PLAIN)]).unwrap()
}

fn get(token: &str) -> Request<Vec<u8>> {
    Request::builder()
        .uri(format!("demo-preview://localhost/{token}"))
        .body(Vec::new())
        .unwrap()
}

fn csp_of(response: &Response<Vec<u8>>) -> String {
    response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_policy_map_is_the_apps_and_is_validated() {
    let s = store();
    assert_eq!(s.csp("scripty"), Some(SCRIPTY));
    assert_eq!(s.csp("plain"), Some(PLAIN));
    assert_eq!(s.csp("other"), None);
    assert_eq!(s.kinds(), ["plain", "scripty"]);

    assert!(PreviewStore::new(Vec::<(String, String)>::new()).is_err());
    let long_kind = "k".repeat(33);
    for bad_kind in ["", "Upper", "has space", "a/b", "-lead", long_kind.as_str()] {
        assert!(!valid_kind(bad_kind), "{bad_kind:?}");
        assert!(
            PreviewStore::new([(bad_kind, PLAIN)]).is_err(),
            "{bad_kind:?}"
        );
    }
    for bad_csp in [
        "",
        "   ",
        "default-src 'none'\r\nSet-Cookie: x=1",
        "script-src \u{e9}",
    ] {
        assert!(!valid_csp(bad_csp), "{bad_csp:?}");
        assert!(PreviewStore::new([("k", bad_csp)]).is_err(), "{bad_csp:?}");
    }
    assert!(valid_csp(PLAIN));
    // The same kind twice keeps the last policy.
    let twice = PreviewStore::new([("k", "default-src 'none'"), ("k", PLAIN)]).unwrap();
    assert_eq!(twice.csp("k"), Some(PLAIN));
}

#[test]
fn tokens_are_long_random_and_distinct() {
    let s = PreviewStore::new([("plain", PLAIN)]).unwrap();
    let mut seen = HashSet::new();
    for _ in 0..40 {
        let token = s.register("plain", "<p>x</p>".into()).unwrap();
        assert_eq!(token.len(), TOKEN_HEX_LEN);
        assert!(token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert!(seen.insert(token), "a token repeated");
    }
    assert_eq!(s.len(), 40);
}

#[test]
fn only_a_live_exact_token_is_served_under_its_kinds_policy() {
    let s = store();
    let a = s.register("scripty", "<p>hi</p>".into()).unwrap();
    let b = s.register("plain", "other".into()).unwrap();

    let ok = serve_preview_request(&s, &get(&a));
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.body(), b"<p>hi</p>");
    assert_eq!(csp_of(&ok), SCRIPTY);
    assert_eq!(ok.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(
        ok.headers().get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    assert_eq!(
        ok.headers().get(header::REFERRER_POLICY).unwrap(),
        "no-referrer"
    );
    let windows_style = Request::builder()
        .uri(format!("http://demo-preview.localhost/{b}"))
        .body(Vec::new())
        .unwrap();
    let other = serve_preview_request(&s, &windows_style);
    assert_eq!(csp_of(&other), PLAIN, "another kind, another policy");

    let head = Request::builder()
        .method(Method::HEAD)
        .uri(format!("demo-preview://localhost/{a}"))
        .body(Vec::new())
        .unwrap();
    let head = serve_preview_request(&s, &head);
    assert_eq!(head.status(), StatusCode::OK);
    assert!(head.body().is_empty());

    // A token one digit off is another string.
    let off_by_one = format!(
        "{}{}",
        &a[..TOKEN_HEX_LEN - 1],
        if a.ends_with('0') { '1' } else { '0' }
    );
    for uri in [
        "demo-preview://localhost/".to_owned(),
        "demo-preview://localhost/not-a-token".to_owned(),
        format!("demo-preview://localhost/{a}/extra"),
        format!("demo-preview://localhost/../{a}"),
        format!("demo-preview://localhost/{}", a.to_uppercase()),
        format!("demo-preview://localhost/{off_by_one}"),
    ] {
        let request = Request::builder().uri(&uri).body(Vec::new()).unwrap();
        let refused = serve_preview_request(&s, &request);
        assert_eq!(refused.status(), StatusCode::NOT_FOUND, "{uri}");
        assert!(refused.body().is_empty());
        assert_eq!(csp_of(&refused), DENY_CSP);
    }
    // A trailing slash is tolerated.
    let slash = Request::builder()
        .uri(format!("demo-preview://localhost/{a}/"))
        .body(Vec::new())
        .unwrap();
    assert_eq!(serve_preview_request(&s, &slash).status(), StatusCode::OK);

    let post = Request::builder()
        .method(Method::POST)
        .uri(format!("demo-preview://localhost/{a}"))
        .body(Vec::new())
        .unwrap();
    let refused = serve_preview_request(&s, &post);
    assert_eq!(refused.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(csp_of(&refused), DENY_CSP);
}

#[test]
fn a_revoked_token_is_gone_and_never_reused() {
    let s = store();
    let a = s.register("plain", "a".into()).unwrap();
    s.revoke(&a);
    s.revoke("never-registered");
    let gone = serve_preview_request(&s, &get(&a));
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    assert_eq!(csp_of(&gone), DENY_CSP);
    let again = s.register("plain", "b".into()).unwrap();
    assert_ne!(again, a);
    let c = s.register("plain", "c".into()).unwrap();
    s.revoke_all();
    assert!(s.is_empty());
    for token in [again, c] {
        assert_eq!(
            serve_preview_request(&s, &get(&token)).status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[test]
fn a_token_expires_after_its_ttl() {
    let ttl = Duration::from_secs(60);
    let s = store()
        .with_limits(PreviewLimits {
            ttl,
            ..PreviewLimits::default()
        })
        .unwrap();
    let t0 = Instant::now();
    let token = s.register_at("plain", "doc".into(), t0).unwrap();

    let before = serve_preview_request_at(&s, &get(&token), t0 + ttl - Duration::from_millis(1));
    assert_eq!(before.status(), StatusCode::OK);
    assert_eq!(s.len_at(t0 + ttl - Duration::from_millis(1)), 1);

    let after = serve_preview_request_at(&s, &get(&token), t0 + ttl);
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
    assert_eq!(csp_of(&after), DENY_CSP);
    assert_eq!(s.len(), 0, "the expired entry is dropped on lookup");
    // Gone for good, even if the clock were to run backwards.
    assert_eq!(
        serve_preview_request_at(&s, &get(&token), t0).status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn expired_documents_free_their_room_and_can_be_purged() {
    let limits = PreviewLimits {
        max_previews: 2,
        ttl: Duration::from_secs(10),
        ..PreviewLimits::default()
    };
    let s = store().with_limits(limits).unwrap();
    let t0 = Instant::now();
    s.register_at("plain", "a".into(), t0).unwrap();
    s.register_at("plain", "b".into(), t0).unwrap();
    assert!(
        s.register_at("plain", "c".into(), t0 + Duration::from_secs(5))
            .is_err(),
        "full while both are live"
    );
    assert!(
        s.register_at("plain", "c".into(), t0 + Duration::from_secs(10))
            .is_ok(),
        "the expired two no longer count"
    );
    assert_eq!(s.len(), 1);
    let d = s
        .register_at("plain", "d".into(), t0 + Duration::from_secs(11))
        .unwrap();
    // `c` (registered at +10 s) expires at +20 s; `d` (at +11 s) lives until +21 s.
    assert_eq!(s.purge_expired(t0 + Duration::from_secs(20)), 1);
    assert_eq!(s.len(), 1);
    assert_eq!(
        serve_preview_request_at(&s, &get(&d), t0 + Duration::from_secs(20)).status(),
        StatusCode::OK
    );
}

#[test]
fn registration_is_bounded() {
    let s = PreviewStore::new([("plain", PLAIN)])
        .unwrap()
        .with_limits(PreviewLimits {
            max_bytes: 10,
            max_total_bytes: 25,
            max_previews: 3,
            ttl: Duration::from_secs(60),
        })
        .unwrap();
    assert!(
        s.register("plain", "x".repeat(11)).is_err(),
        "one document too large"
    );
    assert!(s.is_empty());
    assert!(s.register("nope", "x".into()).is_err(), "unknown kind");
    let a = s.register("plain", "x".repeat(10)).unwrap();
    s.register("plain", "x".repeat(10)).unwrap();
    assert!(
        s.register("plain", "x".repeat(6)).is_err(),
        "26 bytes exceed the 25 byte total"
    );
    assert_eq!(s.total_bytes(), 20);
    s.register("plain", "x".repeat(5)).unwrap();
    assert!(
        s.register("plain", "x".into()).is_err(),
        "a fourth document"
    );
    // Revoking returns the room.
    s.revoke(&a);
    assert!(s.register("plain", "x".repeat(10)).is_ok());
}

#[test]
fn limits_are_validated() {
    let ok = PreviewLimits::default();
    assert!(store().with_limits(ok).is_ok());
    for bad in [
        PreviewLimits { max_bytes: 0, ..ok },
        PreviewLimits {
            max_previews: 0,
            ..ok
        },
        PreviewLimits {
            ttl: Duration::ZERO,
            ..ok
        },
        PreviewLimits {
            ttl: MAX_TTL + Duration::from_secs(1),
            ..ok
        },
        PreviewLimits {
            max_bytes: 100,
            max_total_bytes: 99,
            ..ok
        },
    ] {
        assert!(store().with_limits(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn preview_urls_follow_the_platform_convention() {
    let url = preview_url("demo-preview", "abc");
    if cfg!(windows) {
        assert_eq!(url, "http://demo-preview.localhost/abc");
    } else {
        assert_eq!(url, "demo-preview://localhost/abc");
    }
}

// ---- qualification: kind lookup, kind revocation, preview documents, the service (PK08-5/8) ----

use std::sync::Arc;

use rightkit_tauri_shell::preview::{is_preview_url, PreviewService};

#[test]
fn kind_of_names_only_live_tokens_and_revoke_kind_drops_one_kind() {
    let s = store();
    let t0 = Instant::now();
    let a1 = s.register_at("scripty", "<a>".into(), t0).unwrap();
    let a2 = s.register_at("scripty", "<b>".into(), t0).unwrap();
    let p1 = s.register_at("plain", "<c>".into(), t0).unwrap();
    assert_eq!(s.kind_of_at(&a1, t0).as_deref(), Some("scripty"));
    assert_eq!(s.kind_of_at(&p1, t0).as_deref(), Some("plain"));
    assert_eq!(s.kind_of_at("0".repeat(TOKEN_HEX_LEN).as_str(), t0), None);
    assert_eq!(s.kind_of_at("", t0), None);
    // Dropping a kind removes exactly its documents.
    assert_eq!(s.revoke_kind("scripty"), 2);
    assert_eq!(s.kind_of_at(&a1, t0), None);
    assert_eq!(s.kind_of_at(&a2, t0), None);
    assert_eq!(s.kind_of_at(&p1, t0).as_deref(), Some("plain"));
    assert_eq!(s.revoke_kind("scripty"), 0);
    assert_eq!(s.revoke_kind("unknown-kind"), 0);
    // A revoked token is not served either.
    assert!(s.lookup_at(&a1, t0).is_none());
}

#[test]
fn an_expired_token_has_no_kind() {
    let s = store();
    let t0 = Instant::now();
    let token = s.register_at("plain", "<p>".into(), t0).unwrap();
    assert_eq!(s.kind_of_at(&token, t0).as_deref(), Some("plain"));
    assert_eq!(
        s.kind_of_at(&token, t0 + Duration::from_secs(3 * 60 * 60)),
        None
    );
    assert_eq!(s.kind_of_at(&token, t0), None, "and it is gone for good");
}

#[test]
fn a_preview_document_url_is_recognised_in_both_platform_forms_and_nothing_else() {
    let schemes = vec!["demo-preview".to_owned(), "other-preview".to_owned()];
    let yes = |u: &str| is_preview_url(&schemes, &tauri::Url::parse(u).unwrap());
    assert!(yes("demo-preview://localhost/abc"));
    assert!(yes("other-preview://localhost/abc"));
    assert!(yes("http://demo-preview.localhost/abc"));
    assert!(yes("https://other-preview.localhost/abc?x=1#y"));
    assert!(!yes("tauri://localhost/"));
    assert!(!yes("http://tauri.localhost/"));
    assert!(!yes("http://demo-preview.example/abc"));
    assert!(!yes("http://sub.demo-preview.localhost/abc"));
    assert!(!yes("ftp://demo-preview.localhost/abc"));
    assert!(!yes("https://example.com/demo-preview.localhost"));
    assert!(!is_preview_url(
        &[],
        &tauri::Url::parse("demo-preview://localhost/").unwrap()
    ));
}

#[test]
fn the_service_answers_a_url_built_from_its_own_scheme_and_revokes() {
    let store = Arc::new(store());
    let service = PreviewService::new(Arc::clone(&store), "demo-preview").unwrap();
    assert_eq!(service.scheme(), "demo-preview");
    let handle = service.register("plain", "<p>hi</p>".into()).unwrap();
    assert_eq!(handle.url, preview_url("demo-preview", &handle.token));
    assert_eq!(handle.token.len(), TOKEN_HEX_LEN);
    // The document the URL names is the one registered, under its kind's policy.
    let response = serve_preview_request(&store, &get(&handle.token));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(csp_of(&response), PLAIN);
    // A kind the app never declared is refused, and nothing is registered for it.
    assert!(service.register("script-everything", "x".into()).is_err());
    assert_eq!(store.len(), 1);
    // Revoke one, then all.
    let second = service.register("plain", "<p>2</p>".into()).unwrap();
    service.revoke(&handle.token);
    service.revoke("not-a-token");
    assert!(store.lookup_at(&handle.token, Instant::now()).is_none());
    assert!(store.lookup_at(&second.token, Instant::now()).is_some());
    service.revoke_all();
    assert!(store.is_empty());
    // The scheme must be a valid internal scheme.
    assert!(PreviewService::new(Arc::clone(&store), "https").is_err());
    assert!(PreviewService::new(store, "").is_err());
}

#[test]
fn a_service_handle_serialises_to_the_token_and_url_only() {
    let store = Arc::new(store());
    let service = PreviewService::new(Arc::clone(&store), "demo-preview").unwrap();
    let handle = service.register("plain", "<p>hi</p>".into()).unwrap();
    let json = serde_json::to_value(&handle).unwrap();
    let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["token", "url"]);
}
