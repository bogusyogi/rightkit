#![cfg(feature = "engine-gate")]
//! Engine gate against the loopback entitlement fake. No network. Covers the headless cases:
//! persisted cache from the shell app, no bearer token available, offline, and enforcement.

use std::sync::atomic::{AtomicI64, AtomicU16, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, RetryPolicy};
use rightkit_license::{
    DenyReason, EngineGate, Enforcement, MemorySubjectCache, SubjectEntitlementConfig,
    SubjectEntitlementSource, SubjectSource, TokenError, Verdict, EXIT_ALLOWED, EXIT_CONFIG,
    EXIT_DENIED, EXIT_UNAVAILABLE, SUBJECT_OFFLINE_GRACE_SECS,
};
use serde_json::{json, Value};

const APP: &str = "cutright";
/// 2026-10-16T00:00:00Z
const T0: i64 = 1_792_108_800;
const DAY: i64 = 86_400;

struct Fake {
    server: MockServer,
    status: Arc<AtomicU16>,
    hits: Arc<AtomicUsize>,
}

fn answer(pro: bool, expires: Option<&str>) -> Value {
    json!({
        "app": APP,
        "pro": pro,
        "source": "license",
        "expires_at": expires,
        "checked_at": "2026-10-16T00:00:00Z",
    })
}

impl Fake {
    fn new(body: Value) -> Self {
        let status = Arc::new(AtomicU16::new(200));
        let body = Arc::new(Mutex::new(body));
        let hits = Arc::new(AtomicUsize::new(0));
        let (s, b, h) = (status.clone(), body.clone(), hits.clone());
        let server = MockServer::start(move |_request, _index| {
            h.fetch_add(1, SeqCst);
            match s.load(SeqCst) {
                200 => MockResponse::json(200, &b.lock().unwrap()),
                other => MockResponse::new(other),
            }
        });
        Self {
            server,
            status,
            hits,
        }
    }

    fn set_status(&self, code: u16) {
        self.status.store(code, SeqCst);
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

fn source_with(
    config: SubjectEntitlementConfig,
    store: &Arc<MemorySubjectCache>,
    clock: &Arc<AtomicI64>,
    tokens: impl Fn(bool) -> Result<String, TokenError> + Send + Sync + 'static,
) -> SubjectEntitlementSource {
    let clock = clock.clone();
    SubjectEntitlementSource::new(config, tokens)
        .with_client(client())
        .with_store(store.clone())
        .with_clock(move || clock.load(SeqCst))
}

fn ok_tokens() -> impl Fn(bool) -> Result<String, TokenError> + Send + Sync + 'static {
    |_| Ok("id-token".to_string())
}

fn signed_out() -> impl Fn(bool) -> Result<String, TokenError> + Send + Sync + 'static {
    |_| Err(TokenError::new("no signed-in account"))
}

fn clock() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(T0))
}

fn store() -> Arc<MemorySubjectCache> {
    Arc::new(MemorySubjectCache::new())
}

fn gate(fake: &Fake, store: &Arc<MemorySubjectCache>, clock: &Arc<AtomicI64>) -> EngineGate {
    EngineGate::new(
        source_with(fake.config(), store, clock, ok_tokens()),
        Enforcement::Enforce,
    )
}

#[test]
fn pro_account_is_allowed_and_silent() {
    let fake = Fake::new(answer(true, None));
    let outcome = gate(&fake, &store(), &clock()).check();
    assert_eq!(
        outcome.verdict,
        Verdict::Allowed {
            source: SubjectSource::License,
            expires_at: None
        }
    );
    assert!(outcome.permits());
    assert_eq!(outcome.exit_code(), EXIT_ALLOWED);
    assert!(!outcome.must_report());
    assert!(!outcome.message().is_empty());
}

#[test]
fn free_account_is_denied_and_reported() {
    let fake = Fake::new(answer(false, None));
    let outcome = gate(&fake, &store(), &clock()).check();
    assert_eq!(outcome.verdict, Verdict::Denied(DenyReason::NotEntitled));
    assert!(!outcome.permits());
    assert_eq!(outcome.exit_code(), EXIT_DENIED);
    let mut out = Vec::new();
    outcome.emit(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Pro entitlement") && text.contains("will not run"), "{text}");
}

#[test]
fn lapsed_entitlement_is_denied_as_expired() {
    let fake = Fake::new(answer(true, Some("2026-10-15T00:00:00Z")));
    let outcome = gate(&fake, &store(), &clock()).check();
    assert_eq!(
        outcome.verdict,
        Verdict::Denied(DenyReason::Expired {
            expired_at: T0 - DAY
        })
    );
    assert_eq!(outcome.exit_code(), EXIT_DENIED);
    assert!(outcome.message().contains("2026-10-15T00:00:00Z"));
}

#[test]
fn service_down_inside_grace_admits_with_a_report() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    assert!(gate(&fake, &store, &clock).check().permits());

    fake.set_status(503);
    clock.store(T0 + 2 * DAY, SeqCst);
    let outcome = gate(&fake, &store, &clock).check();
    assert_eq!(
        outcome.verdict,
        Verdict::Grace {
            source: SubjectSource::License,
            expires_at: None,
            grace_ends_at: T0 + SUBJECT_OFFLINE_GRACE_SECS
        }
    );
    assert!(outcome.permits());
    assert_eq!(outcome.exit_code(), EXIT_ALLOWED);
    assert!(outcome.must_report(), "grace is never silent");
    assert!(outcome.message().contains("2026-10-23T00:00:00Z"));
}

#[test]
fn service_down_past_grace_is_unavailable() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check();

    fake.set_status(503);
    clock.store(T0 + SUBJECT_OFFLINE_GRACE_SECS, SeqCst);
    let outcome = gate(&fake, &store, &clock).check();
    assert!(matches!(
        outcome.verdict,
        Verdict::Denied(DenyReason::Unavailable(_))
    ));
    assert_eq!(outcome.exit_code(), EXIT_UNAVAILABLE);
}

#[test]
fn offline_with_no_cache_is_unavailable_not_allowed() {
    let fake = Fake::new(answer(true, None));
    let config = fake.config();
    drop(fake); // port closed
    let outcome = EngineGate::new(
        source_with(config, &store(), &clock(), ok_tokens()),
        Enforcement::Enforce,
    )
    .check();
    assert!(matches!(
        outcome.verdict,
        Verdict::Denied(DenyReason::Unavailable(_))
    ));
    assert!(!outcome.permits());
    assert_eq!(outcome.exit_code(), EXIT_UNAVAILABLE);
}

#[test]
fn rejected_account_is_denied_with_no_grace_and_clears_the_cache() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check();

    fake.set_status(403);
    clock.store(T0 + 2 * DAY, SeqCst);
    let outcome = gate(&fake, &store, &clock).check();
    assert_eq!(outcome.verdict, Verdict::Denied(DenyReason::Unauthorized));
    assert_eq!(outcome.exit_code(), EXIT_DENIED);
    // The cache is gone: even offline, the offline read grants nothing.
    assert!(!gate(&fake, &store, &clock).check_offline().permits());
}

#[test]
fn unknown_app_is_a_config_refusal() {
    let fake = Fake::new(answer(true, None));
    fake.set_status(404);
    let outcome = gate(&fake, &store(), &clock()).check();
    assert!(matches!(
        outcome.verdict,
        Verdict::Denied(DenyReason::Misconfigured(_))
    ));
    assert_eq!(outcome.exit_code(), EXIT_CONFIG);
}

#[test]
fn headless_with_a_fresh_shell_cache_is_allowed_without_a_token() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check(); // the shell app populated the cache
    let hits = fake.hits.load(SeqCst);

    clock.store(T0 + 3600, SeqCst);
    let headless = EngineGate::new(
        source_with(fake.config(), &store, &clock, signed_out()),
        Enforcement::Enforce,
    );
    let outcome = headless.check();
    assert!(matches!(outcome.verdict, Verdict::Allowed { .. }));
    assert_eq!(fake.hits.load(SeqCst), hits, "served from cache, no request");
}

#[test]
fn headless_with_a_stale_cache_and_no_token_gets_grace() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check();
    let hits = fake.hits.load(SeqCst);

    clock.store(T0 + 3 * DAY, SeqCst);
    let headless = EngineGate::new(
        source_with(fake.config(), &store, &clock, signed_out()),
        Enforcement::Enforce,
    );
    let outcome = headless.check();
    assert!(matches!(outcome.verdict, Verdict::Grace { .. }), "{outcome}");
    assert!(outcome.permits() && outcome.must_report());
    assert_eq!(fake.hits.load(SeqCst), hits, "no token, so no request");

    clock.store(T0 + SUBJECT_OFFLINE_GRACE_SECS, SeqCst);
    let outcome = headless.check();
    assert!(matches!(
        outcome.verdict,
        Verdict::Denied(DenyReason::SignedOut(_))
    ));
    assert_eq!(outcome.exit_code(), EXIT_DENIED);
}

#[test]
fn headless_with_no_cache_and_no_token_is_signed_out() {
    let fake = Fake::new(answer(true, None));
    let outcome = EngineGate::new(
        source_with(fake.config(), &store(), &clock(), signed_out()),
        Enforcement::Enforce,
    )
    .check();
    match outcome.verdict {
        Verdict::Denied(DenyReason::SignedOut(ref m)) => assert!(m.contains("no signed-in")),
        ref other => panic!("{other:?}"),
    }
    assert_eq!(outcome.exit_code(), EXIT_DENIED);
    assert_eq!(fake.hits.load(SeqCst), 0);
}

#[test]
fn lapsed_cache_and_no_token_is_still_denied() {
    let fake = Fake::new(answer(true, Some("2026-10-17T00:00:00Z")));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check();

    clock.store(T0 + 2 * DAY, SeqCst);
    let headless = EngineGate::new(
        source_with(fake.config(), &store, &clock, signed_out()),
        Enforcement::Enforce,
    );
    let outcome = headless.check();
    assert_eq!(
        outcome.verdict,
        Verdict::Denied(DenyReason::Expired {
            expired_at: T0 + DAY
        })
    );
}

#[test]
fn check_offline_never_asks_for_a_token_or_the_network() {
    let fake = Fake::new(answer(true, None));
    let (store, clock) = (store(), clock());
    gate(&fake, &store, &clock).check();
    let hits = fake.hits.load(SeqCst);

    let panicking = |_: bool| -> Result<String, TokenError> { panic!("token requested") };
    let offline = EngineGate::new(
        source_with(fake.config(), &store, &clock, panicking),
        Enforcement::Enforce,
    );
    assert!(matches!(
        offline.check_offline().verdict,
        Verdict::Allowed { .. }
    ));
    clock.store(T0 + 2 * DAY, SeqCst);
    assert!(matches!(
        offline.check_offline().verdict,
        Verdict::Grace { .. }
    ));
    clock.store(T0 + 8 * DAY, SeqCst);
    let outcome = offline.check_offline();
    assert!(matches!(
        outcome.verdict,
        Verdict::Denied(DenyReason::SignedOut(_))
    ));
    assert_eq!(fake.hits.load(SeqCst), hits);
}

#[test]
fn advisory_mode_runs_but_reports_the_refusal() {
    let fake = Fake::new(answer(false, None));
    let outcome = EngineGate::new(
        source_with(fake.config(), &store(), &clock(), ok_tokens()),
        Enforcement::from_switch(false),
    )
    .check();
    assert!(matches!(outcome.verdict, Verdict::Denied(_)));
    assert!(outcome.permits());
    assert_eq!(outcome.exit_code(), EXIT_ALLOWED);
    let mut out = Vec::new();
    outcome.emit(&mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("not enforced"));
}

#[test]
fn messages_carry_no_token() {
    let fake = Fake::new(answer(false, None));
    let outcome = gate(&fake, &store(), &clock()).check();
    let text = format!("{outcome} {outcome:?}");
    assert!(!text.contains("id-token"), "{text}");
}
