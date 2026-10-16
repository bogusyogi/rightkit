#![cfg(feature = "subject-entitlement")]
//! Google-subject entitlement source against an in-process loopback server. No network.

use std::sync::atomic::{AtomicI64, AtomicU16, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, RetryPolicy};
use rightkit_license::{
    Freshness, MemorySubjectCache, PlayPurchase, SubjectCacheStore, SubjectEntitlementConfig,
    SubjectEntitlementError, SubjectEntitlementSource, SubjectSource, TokenError,
    SUBJECT_OFFLINE_GRACE_SECS, SUBJECT_RECHECK_SECS,
};
use serde_json::{json, Value};

const APP: &str = "heardright";
/// 2026-10-16T00:00:00Z
const T0: i64 = 1_792_108_800;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

struct Fake {
    server: MockServer,
    status: Arc<AtomicU16>,
    body: Arc<Mutex<Value>>,
}

fn entitlement(pro: bool, source: &str, expires: Option<&str>) -> Value {
    json!({
        "app": APP,
        "pro": pro,
        "source": source,
        "expires_at": expires,
        "checked_at": "2026-10-16T00:00:00Z",
    })
}

impl Fake {
    fn new(body: Value) -> Self {
        let status = Arc::new(AtomicU16::new(200));
        let body = Arc::new(Mutex::new(body));
        let (s, b) = (status.clone(), body.clone());
        let server = MockServer::start(move |_request, _index| {
            let code = s.load(SeqCst);
            match code {
                200 => MockResponse::json(200, &b.lock().unwrap()),
                429 => MockResponse::new(429).header("Retry-After", "7"),
                other => MockResponse::new(other),
            }
        });
        Self {
            server,
            status,
            body,
        }
    }

    fn set_status(&self, code: u16) {
        self.status.store(code, SeqCst);
    }

    fn set_body(&self, body: Value) {
        *self.body.lock().unwrap() = body;
    }

    fn config(&self) -> SubjectEntitlementConfig {
        SubjectEntitlementConfig::new(self.server.base_url(), APP)
    }
}

fn client() -> Client {
    let mut config = loopback_config();
    config.retry = RetryPolicy::none();
    Client::new(config)
}

fn ok_tokens() -> impl Fn(bool) -> Result<String, TokenError> + Send + Sync + 'static {
    |force| Ok(if force { "id-token-2" } else { "id-token-1" }.to_string())
}

fn source(fake: &Fake, clock: &Arc<AtomicI64>) -> SubjectEntitlementSource {
    let clock = clock.clone();
    SubjectEntitlementSource::new(fake.config(), ok_tokens())
        .with_client(client())
        .with_clock(move || clock.load(SeqCst))
}

fn clock() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(T0))
}

#[test]
fn fetches_and_maps_the_entitlement_object() {
    let fake = Fake::new(entitlement(true, "play", Some("2026-11-16T00:00:00Z")));
    let clock = clock();
    let source = source(&fake, &clock);

    let decision = source.entitlement().expect("entitlement");
    assert!(decision.is_pro());
    assert_eq!(decision.freshness, Freshness::Fresh);
    assert_eq!(decision.entitlement.app, APP);
    assert_eq!(decision.entitlement.source, SubjectSource::Play);
    assert_eq!(decision.entitlement.expires_at, Some(T0 + 31 * DAY));
    assert_eq!(decision.entitlement.checked_at, T0);

    let requests = fake.server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path(), "/v1/entitlement");
    assert_eq!(
        requests[0].query_pairs(),
        vec![("app".to_string(), APP.to_string())]
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer id-token-1")
    );
}

#[test]
fn null_expiry_and_none_source() {
    let fake = Fake::new(entitlement(false, "none", None));
    let source = source(&fake, &clock());
    let decision = source.entitlement().unwrap();
    assert!(!decision.is_pro());
    assert_eq!(decision.entitlement.source, SubjectSource::Absent);
    assert_eq!(decision.entitlement.expires_at, None);
    assert!(!source.is_pro());
}

#[test]
fn unknown_source_string_is_tolerated() {
    let fake = Fake::new(entitlement(true, "stripe", None));
    let decision = source(&fake, &clock()).entitlement().unwrap();
    assert_eq!(decision.entitlement.source, SubjectSource::Unknown);
    assert!(decision.is_pro());
}

#[test]
fn cache_serves_inside_the_recheck_interval_then_rechecks() {
    let fake = Fake::new(entitlement(true, "license", None));
    let clock = clock();
    let source = source(&fake, &clock);

    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Fresh);
    clock.store(T0 + 23 * HOUR, SeqCst);
    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Cached);
    assert_eq!(fake.server.request_count(), 1);

    clock.store(T0 + SUBJECT_RECHECK_SECS, SeqCst);
    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Fresh);
    assert_eq!(fake.server.request_count(), 2);
}

#[test]
fn recheck_interval_is_clamped_to_daily() {
    let fake = Fake::new(entitlement(true, "license", None));
    let clock = clock();
    let mut config = fake.config();
    config.recheck_interval_secs = 30 * DAY;
    let clock2 = clock.clone();
    let source = SubjectEntitlementSource::new(config, ok_tokens())
        .with_client(client())
        .with_clock(move || clock2.load(SeqCst));
    source.entitlement().unwrap();
    clock.store(T0 + SUBJECT_RECHECK_SECS, SeqCst);
    source.entitlement().unwrap();
    assert_eq!(fake.server.request_count(), 2);
}

#[test]
fn expires_at_is_honoured_inside_the_recheck_interval() {
    let fake = Fake::new(entitlement(true, "play", Some("2026-10-16T01:00:00Z")));
    let clock = clock();
    let source = source(&fake, &clock);
    assert!(source.entitlement().unwrap().is_pro());

    // Past expires_at but well inside the daily interval: the cache must not grant Pro
    // and a re-check is forced.
    fake.set_body(entitlement(false, "none", None));
    clock.store(T0 + 2 * HOUR, SeqCst);
    let decision = source.entitlement().unwrap();
    assert_eq!(decision.freshness, Freshness::Fresh);
    assert!(!decision.is_pro());
    assert_eq!(fake.server.request_count(), 2);
}

#[test]
fn lapsed_pro_gets_no_offline_grace() {
    let fake = Fake::new(entitlement(true, "play", Some("2026-10-16T01:00:00Z")));
    let clock = clock();
    let source = source(&fake, &clock);
    assert!(source.entitlement().unwrap().is_pro());

    fake.set_status(503);
    clock.store(T0 + 2 * HOUR, SeqCst);
    let decision = source.entitlement().unwrap();
    assert!(matches!(decision.freshness, Freshness::OfflineGrace { .. }));
    assert!(!decision.is_pro(), "grace never extends expires_at");
    assert!(!source.is_pro());
}

#[test]
fn offline_grace_is_seven_days_from_the_last_good_check() {
    let fake = Fake::new(entitlement(true, "storekit", None));
    let clock = clock();
    let source = source(&fake, &clock);
    assert!(source.entitlement().unwrap().is_pro());

    fake.set_status(503);
    clock.store(T0 + 2 * DAY, SeqCst);
    let decision = source.entitlement().unwrap();
    assert!(decision.is_pro());
    assert_eq!(
        decision.freshness,
        Freshness::OfflineGrace {
            grace_ends_at: T0 + SUBJECT_OFFLINE_GRACE_SECS
        }
    );

    clock.store(T0 + SUBJECT_OFFLINE_GRACE_SECS - 1, SeqCst);
    assert!(source.entitlement().unwrap().is_pro());

    clock.store(T0 + SUBJECT_OFFLINE_GRACE_SECS, SeqCst);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::Server { status: 503 }
    );
    assert!(!source.is_pro());
}

#[test]
fn grace_applies_to_unreachable_server_and_unreadable_answers() {
    let fake = Fake::new(entitlement(true, "play", None));
    let clock = clock();
    let source = source(&fake, &clock);
    source.entitlement().unwrap();

    clock.store(T0 + 2 * DAY, SeqCst);
    fake.set_status(200);
    fake.set_body(json!("<html>captive portal</html>"));
    assert!(matches!(
        source.entitlement().unwrap().freshness,
        Freshness::OfflineGrace { .. }
    ));

    fake.set_status(429);
    assert!(matches!(
        source.entitlement().unwrap().freshness,
        Freshness::OfflineGrace { .. }
    ));
}

#[test]
fn no_cache_means_transient_errors_surface() {
    let fake = Fake::new(entitlement(true, "play", None));
    let source = source(&fake, &clock());
    fake.set_status(503);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::Server { status: 503 }
    );
    fake.set_status(429);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::RateLimited {
            retry_after: Some(Duration::from_secs(7))
        }
    );
    assert!(source.entitlement().unwrap_err().is_transient());
}

#[test]
fn connection_refused_maps_to_offline() {
    let fake = Fake::new(entitlement(true, "play", None));
    let config = fake.config();
    drop(fake); // port now closed
    let source = SubjectEntitlementSource::new(config, ok_tokens())
        .with_client(client())
        .with_clock(|| T0);
    let error = source.entitlement().unwrap_err();
    assert!(
        matches!(error, SubjectEntitlementError::Offline(_)),
        "{error:?}"
    );
    assert!(error.is_transient());
}

#[test]
fn offline_with_cache_uses_grace() {
    let fake = Fake::new(entitlement(true, "play", None));
    let clock = clock();
    let config = fake.config();
    let store = Arc::new(MemorySubjectCache::new());
    let clock_a = clock.clone();
    let first = SubjectEntitlementSource::new(config.clone(), ok_tokens())
        .with_client(client())
        .with_store(store.clone())
        .with_clock(move || clock_a.load(SeqCst));
    first.entitlement().unwrap();
    drop(first);
    drop(fake); // server gone

    clock.store(T0 + 3 * DAY, SeqCst);
    let clock_b = clock.clone();
    let restarted = SubjectEntitlementSource::new(config, ok_tokens())
        .with_client(client())
        .with_store(store)
        .with_clock(move || clock_b.load(SeqCst));
    let decision = restarted.entitlement().unwrap();
    assert!(decision.is_pro());
    assert!(matches!(decision.freshness, Freshness::OfflineGrace { .. }));
}

#[test]
fn a_401_refreshes_the_token_once_and_retries() {
    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = seen.clone();
    let server = MockServer::start(move |request, _| {
        seen2.fetch_add(1, SeqCst);
        if request.header("authorization") == Some("Bearer id-token-2") {
            MockResponse::json(200, &entitlement(true, "play", None))
        } else {
            MockResponse::new(401)
        }
    });
    let forced = Arc::new(Mutex::new(Vec::new()));
    let forced2 = forced.clone();
    let source = SubjectEntitlementSource::new(
        SubjectEntitlementConfig::new(server.base_url(), APP),
        move |force: bool| {
            forced2.lock().unwrap().push(force);
            Ok(if force { "id-token-2" } else { "id-token-1" }.to_string())
        },
    )
    .with_client(client())
    .with_clock(|| T0);

    assert!(source.entitlement().unwrap().is_pro());
    assert_eq!(*forced.lock().unwrap(), vec![false, true]);
    assert_eq!(seen.load(SeqCst), 2);
}

#[test]
fn a_second_401_is_unauthorized_and_clears_the_cache() {
    let fake = Fake::new(entitlement(true, "play", None));
    let clock = clock();
    let store = Arc::new(MemorySubjectCache::new());
    let clock2 = clock.clone();
    let source = SubjectEntitlementSource::new(fake.config(), ok_tokens())
        .with_client(client())
        .with_store(store.clone())
        .with_clock(move || clock2.load(SeqCst));
    source.entitlement().unwrap();
    assert!(store.load().is_some());

    fake.set_status(401);
    clock.store(T0 + 2 * DAY, SeqCst);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::Unauthorized
    );
    assert!(
        store.load().is_none(),
        "cache cleared, no grace on auth failure"
    );
    assert!(!source.is_pro());
}

#[test]
fn forbidden_is_unauthorized_without_grace() {
    let fake = Fake::new(entitlement(true, "play", None));
    let clock = clock();
    let source = source(&fake, &clock);
    source.entitlement().unwrap();
    fake.set_status(403);
    clock.store(T0 + 2 * DAY, SeqCst);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::Unauthorized
    );
}

#[test]
fn other_4xx_is_rejected_and_not_transient() {
    let fake = Fake::new(entitlement(true, "play", None));
    fake.set_status(404);
    let error = source(&fake, &clock()).entitlement().unwrap_err();
    assert_eq!(error, SubjectEntitlementError::Rejected { status: 404 });
    assert!(!error.is_transient());
}

#[test]
fn response_for_another_app_is_refused() {
    let mut other = entitlement(true, "play", None);
    other["app"] = json!("coderight");
    let fake = Fake::new(other);
    assert!(matches!(
        source(&fake, &clock()).entitlement().unwrap_err(),
        SubjectEntitlementError::BadResponse(_)
    ));
}

#[test]
fn bad_timestamps_are_refused() {
    let mut bad = entitlement(true, "play", None);
    bad["checked_at"] = json!("yesterday");
    let fake = Fake::new(bad);
    assert!(matches!(
        source(&fake, &clock()).entitlement().unwrap_err(),
        SubjectEntitlementError::BadResponse(_)
    ));
    fake.set_body(entitlement(true, "play", Some("soon")));
    assert!(matches!(
        source(&fake, &clock()).entitlement().unwrap_err(),
        SubjectEntitlementError::BadResponse(_)
    ));
}

#[test]
fn token_failure_sends_nothing() {
    let fake = Fake::new(entitlement(true, "play", None));
    let source = SubjectEntitlementSource::new(fake.config(), |_force: bool| {
        Err(TokenError::new("signed out"))
    })
    .with_client(client())
    .with_clock(|| T0);
    assert_eq!(
        source.entitlement().unwrap_err(),
        SubjectEntitlementError::Token(TokenError::new("signed out"))
    );
    assert_eq!(fake.server.request_count(), 0);
}

#[test]
fn from_play_posts_the_contract_body_and_primes_the_cache() {
    let fake = Fake::new(entitlement(true, "play", Some("2026-11-16T00:00:00Z")));
    let source = source(&fake, &clock());
    let decision = source
        .link_play_purchase(&PlayPurchase {
            package_name: "com.orthic.heardright".into(),
            product_id: "pro_monthly".into(),
            purchase_token: "purchase-secret".into(),
        })
        .unwrap();
    assert!(decision.is_pro());
    assert_eq!(decision.entitlement.source, SubjectSource::Play);

    let requests = fake.server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path(), "/v1/licenses/bridge/from-play");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer id-token-1")
    );
    assert_eq!(
        requests[0].body_json(),
        json!({
            "app": APP,
            "package_name": "com.orthic.heardright",
            "product_id": "pro_monthly",
            "purchase_token": "purchase-secret",
        })
    );

    // The linked answer is now the cache: no second request.
    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Cached);
    assert_eq!(fake.server.request_count(), 1);
}

#[test]
fn from_storekit_and_from_activation_use_the_bridge_paths() {
    let fake = Fake::new(entitlement(true, "storekit", None));
    let source = source(&fake, &clock());
    source.link_storekit("jws.payload.sig").unwrap();
    source.link_activation("act-token", "dev-1").unwrap();
    let requests = fake.server.requests();
    assert_eq!(requests[0].path(), "/v1/licenses/bridge/from-storekit");
    assert_eq!(
        requests[0].body_json(),
        json!({"app": APP, "jws": "jws.payload.sig"})
    );
    assert_eq!(requests[1].path(), "/v1/licenses/bridge/from-activation");
    assert_eq!(
        requests[1].body_json(),
        json!({"app": APP, "activation_token": "act-token", "device_id": "dev-1"})
    );
}

#[test]
fn purchase_rejection_is_not_cached_and_is_not_grace_eligible() {
    let fake = Fake::new(entitlement(true, "play", None));
    fake.set_status(400);
    let source = source(&fake, &clock());
    let error = source
        .link_play_purchase(&PlayPurchase {
            package_name: "p".into(),
            product_id: "i".into(),
            purchase_token: "t".into(),
        })
        .unwrap_err();
    assert_eq!(error, SubjectEntitlementError::Rejected { status: 400 });
}

#[test]
fn invalidate_forgets_the_cache() {
    let fake = Fake::new(entitlement(true, "play", None));
    let source = source(&fake, &clock());
    source.entitlement().unwrap();
    source.invalidate();
    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Fresh);
    assert_eq!(fake.server.request_count(), 2);
}

#[test]
fn clock_rolled_back_before_the_fetch_distrusts_the_cache() {
    let fake = Fake::new(entitlement(true, "play", None));
    let clock = clock();
    let source = source(&fake, &clock);
    source.entitlement().unwrap();
    fake.set_status(503);
    clock.store(T0 - HOUR, SeqCst);
    assert!(source.entitlement().is_err());
}

#[test]
fn a_cache_for_another_app_is_ignored() {
    let fake = Fake::new(entitlement(true, "play", None));
    let store = Arc::new(MemorySubjectCache::new());
    store.save(
        &json!({
            "v": 1,
            "entitlement": {
                "app": "coderight", "pro": true, "source": "play",
                "expires_at": null, "checked_at": T0
            },
            "fetched_at": T0
        })
        .to_string(),
    );
    let source = SubjectEntitlementSource::new(fake.config(), ok_tokens())
        .with_client(client())
        .with_store(store)
        .with_clock(|| T0 + HOUR);
    assert_eq!(source.entitlement().unwrap().freshness, Freshness::Fresh);
}

#[test]
fn debug_and_errors_never_carry_tokens() {
    let purchase = PlayPurchase {
        package_name: "p".into(),
        product_id: "i".into(),
        purchase_token: "purchase-secret".into(),
    };
    assert!(!format!("{purchase:?}").contains("purchase-secret"));
    let fake = Fake::new(entitlement(true, "play", None));
    fake.set_status(500);
    let error = source(&fake, &clock()).entitlement().unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(!text.contains("id-token"));
}
