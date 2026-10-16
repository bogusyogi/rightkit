//! `HttpBackend` and `HttpAuth` against an in-process fake of the `rightkit-sync-http/1`
//! server (see tests/common/fake_server.rs and HTTP-PROTOCOL.md).

mod common;

use std::sync::{Arc, Mutex};

use common::fake_server::FakeServer;
use common::*;
use rightkit_http::testing::loopback_config;
use rightkit_http::Client;
use rightkit_sync::http::*;
use rightkit_sync::*;

struct Tok {
    token: Mutex<Option<String>>,
    scope: Mutex<Option<String>>,
}

impl Tok {
    fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            token: Mutex::new(Some(token.to_string())),
            scope: Mutex::new(Some("scope-1".into())),
        })
    }
}

impl TokenSource for Tok {
    fn bearer(&self) -> SyncResult<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }
    fn account_scope(&self) -> Option<String> {
        self.scope.lock().unwrap().clone()
    }
}

fn backend_for(base_url: &str, tok: Arc<Tok>, zone: &str) -> HttpBackend {
    HttpBackend::new(
        Client::new(loopback_config()),
        tok,
        HttpBackendConfig::new(base_url, zone),
    )
}

fn backend(server: &FakeServer, token: &str, zone: &str) -> HttpBackend {
    backend_for(&server.base_url(), Tok::new(token), zone)
}

fn sealed(name: &str, updated: i64, body: &[u8]) -> RawRecord {
    let mut r = RawRecord::new("Note", name);
    r.set("updatedAt", FieldValue::Date(updated));
    r.set("enc", FieldValue::Bytes(body.to_vec()));
    r
}

fn ok_all(outcomes: &[RecordOutcome]) -> bool {
    outcomes.iter().all(|o| o.result.is_ok())
}

#[test]
fn health_prepare_save_and_paged_changes_round_trip() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let mut b = backend(&server, "tok", "notes");
    b.health().unwrap();
    assert_eq!(
        b.fetch_changes(None).unwrap_err(),
        SyncError::ZoneMissing,
        "a zone that was never prepared"
    );
    b.prepare().unwrap();
    b.prepare().unwrap();

    let records: Vec<RawRecord> = (0..5)
        .map(|i| sealed(&format!("n{i}"), 100 + i, &[i as u8, 0, 255]))
        .collect();
    let outcomes = b.save_records(&records, SavePolicy::Overwrite).unwrap();
    assert_eq!(outcomes.len(), 5);
    assert!(ok_all(&outcomes));

    // Rebuild with a small page limit to walk several pages.
    let mut cfg = HttpBackendConfig::new(server.base_url(), "notes");
    cfg.page_limit = 2;
    b = HttpBackend::new(Client::new(loopback_config()), Tok::new("tok"), cfg);
    let mut since: Option<Vec<u8>> = None;
    let mut names = Vec::new();
    let mut pages = 0;
    loop {
        let page = b.fetch_changes(since.as_deref()).unwrap();
        pages += 1;
        assert!(page.deleted_record_names.is_empty());
        for r in &page.records {
            names.push(r.record_name.clone());
            assert_eq!(r.record_type, "Note");
            assert!(r.change_tag.is_some());
        }
        since = page.new_token.clone();
        if !page.more_coming {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(names, vec!["n0", "n1", "n2", "n3", "n4"]);

    // Caught up: nothing since the final cursor.
    let page = b.fetch_changes(since.as_deref()).unwrap();
    assert!(page.records.is_empty() && !page.more_coming);

    // The body and the dates survive the wire exactly.
    let r = b.fetch_record("n3").unwrap().unwrap();
    assert_eq!(r.date_ms("updatedAt"), Some(103));
    assert_eq!(r.get("enc"), Some(&FieldValue::Bytes(vec![3, 0, 255])));
    assert_eq!(b.fetch_record("nope").unwrap(), None);
    assert_eq!(b.max_batch(), 40);
}

#[test]
fn conditional_saves_report_conflicts_with_the_server_record() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let b = backend(&server, "tok", "notes");
    b.prepare().unwrap();

    let one = |r: RawRecord, p| b.save_records(&[r], p).unwrap().remove(0);

    // Insert: no tag means the record must not exist.
    let o = one(sealed("n1", 100, b"v1"), SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Ok(()));
    let o = one(sealed("n1", 100, b"again"), SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    let server_rec = o.server_record.expect("conflict carries the server record");
    assert_eq!(server_rec.change_tag.as_deref(), Some("1"));
    assert_eq!(
        server_rec.get("enc"),
        Some(&FieldValue::Bytes(b"v1".to_vec()))
    );

    // Update with the tag we saw.
    let mut upd = sealed("n1", 200, b"v2");
    upd.change_tag = Some("1".into());
    assert_eq!(one(upd, SavePolicy::IfServerUnchanged).result, Ok(()));
    assert_eq!(
        b.fetch_record("n1").unwrap().unwrap().change_tag.as_deref(),
        Some("2")
    );

    // A stale tag conflicts and returns the newer record.
    let mut stale = sealed("n1", 300, b"v3");
    stale.change_tag = Some("1".into());
    let o = one(stale, SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    assert_eq!(o.server_record.unwrap().change_tag.as_deref(), Some("2"));

    // A tag for a record that does not exist: conflict, no server record.
    let mut ghost = sealed("ghost", 1, b"x");
    ghost.change_tag = Some("5".into());
    let o = one(ghost, SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    assert!(o.server_record.is_none());

    // Overwrite ignores tags (tombstones use it).
    let mut tomb = sealed("n1", 400, b"t");
    tomb.set("deletedAt", FieldValue::Date(400));
    assert_eq!(one(tomb, SavePolicy::Overwrite).result, Ok(()));
    let r = b.fetch_record("n1").unwrap().unwrap();
    assert_eq!(r.date_ms("deletedAt"), Some(400));
}

#[test]
fn plaintext_fields_and_unversioned_records_are_refused_before_anything_is_sent() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let b = backend(&server, "tok", "notes");
    b.prepare().unwrap();
    let sent = server.server.request_count();

    let mut leaky = sealed("n1", 100, b"v");
    leaky.set("text", FieldValue::Text("secret transcript".into()));
    let e = b.save_records(&[leaky], SavePolicy::Overwrite).unwrap_err();
    assert!(matches!(e, SyncError::Persistence(_)), "{e:?}");
    assert!(
        !e.to_string().contains("secret transcript"),
        "the message names the field, never its value"
    );

    let mut undated = RawRecord::new("Note", "n2");
    undated.set("enc", FieldValue::Bytes(vec![1]));
    let e = b
        .save_records(&[undated], SavePolicy::Overwrite)
        .unwrap_err();
    assert!(matches!(e, SyncError::Persistence(_)), "{e:?}");

    assert_eq!(server.server.request_count(), sent, "nothing went out");
    assert!(server.records("acct", "notes").is_empty());
}

#[test]
fn server_failures_map_to_the_engine_error_kinds() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let b = backend(&server, "tok", "notes");
    b.prepare().unwrap();

    server.fail_next(429, &[("Retry-After", "7")], "rate_limited");
    match b.fetch_changes(None).unwrap_err() {
        SyncError::RetryAfter { secs, .. } => assert_eq!(secs, 7.0),
        other => panic!("{other:?}"),
    }

    server.fail_next(503, &[], "unavailable");
    match b.fetch_changes(None).unwrap_err() {
        SyncError::RetryAfter { secs, .. } => assert_eq!(secs, 30.0),
        other => panic!("{other:?}"),
    }

    server.fail_next(401, &[], "session_expired");
    assert!(matches!(
        b.fetch_changes(None).unwrap_err(),
        SyncError::Unavailable(_)
    ));

    server.fail_next(403, &[], "app_not_allowed");
    assert!(matches!(
        b.prepare().unwrap_err(),
        SyncError::Unavailable(_)
    ));

    server.fail_next(500, &[], "internal");
    assert!(matches!(
        b.fetch_changes(None).unwrap_err(),
        SyncError::Transient(_)
    ));

    // 410 token_expired, from a cursor the server rejects.
    assert_eq!(
        b.fetch_changes(Some(&b"not-a-cursor"[..])).unwrap_err(),
        SyncError::TokenExpired
    );
    // A cursor that is not even text never reaches the wire.
    let sent = server.server.request_count();
    assert_eq!(
        b.fetch_changes(Some(&[0xffu8, 0xfe][..])).unwrap_err(),
        SyncError::TokenExpired
    );
    assert_eq!(server.server.request_count(), sent);

    // A revoked token.
    server.revoke("tok");
    assert!(matches!(
        b.fetch_changes(None).unwrap_err(),
        SyncError::Unavailable(_)
    ));

    // Nothing listening: a network failure, retried on the network cadence.
    let down = backend_for("http://127.0.0.1:1", Tok::new("tok"), "notes");
    match down.prepare().unwrap_err() {
        SyncError::RetryAfter { secs, .. } => assert_eq!(secs, 15.0),
        other => panic!("{other:?}"),
    }

    // Signed out: nothing is sent.
    let tok = Tok::new("tok");
    *tok.token.lock().unwrap() = None;
    let signed_out = backend_for(&server.base_url(), tok, "notes");
    let sent = server.server.request_count();
    assert!(matches!(
        signed_out.prepare().unwrap_err(),
        SyncError::Unavailable(_)
    ));
    assert_eq!(server.server.request_count(), sent);
}

#[test]
fn requests_carry_the_bearer_and_accounts_are_isolated_and_erasable() {
    let server = FakeServer::start(&[("tok-1", "acct-1"), ("tok-2", "acct-2")]);
    let (b1, b2) = (
        backend(&server, "tok-1", "notes"),
        backend(&server, "tok-2", "notes"),
    );
    b1.prepare().unwrap();
    b2.prepare().unwrap();
    b1.save_records(&[sealed("n1", 1, b"one")], SavePolicy::Overwrite)
        .unwrap();
    b2.save_records(&[sealed("n1", 2, b"two")], SavePolicy::Overwrite)
        .unwrap();
    assert_eq!(
        b1.fetch_record("n1").unwrap().unwrap().date_ms("updatedAt"),
        Some(1)
    );
    assert_eq!(
        b2.fetch_record("n1").unwrap().unwrap().date_ms("updatedAt"),
        Some(2)
    );

    let reqs = server.server.requests();
    let save = reqs.iter().find(|r| r.target.ends_with("/save")).unwrap();
    assert_eq!(save.method, "POST");
    assert_eq!(save.header("authorization"), Some("Bearer tok-1"));
    assert_eq!(save.header("accept"), Some("application/json"));
    assert_eq!(save.path(), "/v1/sync/zones/notes/save");
    assert!(save.body_text().contains("\"policy\":\"overwrite\""));

    b1.erase_account().unwrap();
    let erase = server.server.requests().into_iter().last().unwrap();
    assert_eq!(erase.method, "DELETE");
    assert_eq!(erase.header("x-rightkit-confirm"), Some("erase-account"));
    assert_eq!(b1.fetch_changes(None).unwrap_err(), SyncError::ZoneMissing);
    assert!(
        b2.fetch_record("n1").unwrap().is_some(),
        "other account untouched"
    );
}

#[test]
fn zone_and_record_names_are_percent_encoded_in_the_path() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let b = backend(&server, "tok", "my zone/1");
    b.prepare().unwrap();
    b.save_records(&[sealed("a:b@c", 1, b"x")], SavePolicy::Overwrite)
        .unwrap();
    assert!(b.fetch_record("a:b@c").unwrap().is_some());
    let reqs = server.server.requests();
    assert!(reqs
        .iter()
        .any(|r| r.path() == "/v1/sync/zones/my%20zone%2F1"));
    assert!(server.record("acct", "my zone/1", "a:b@c").is_some());
}

#[test]
fn auth_follows_the_session_and_the_toggle() {
    let tok = Tok::new("tok");
    let auth = HttpAuth::new(tok.clone());
    assert!(auth.enabled());
    assert_eq!(auth.availability(), Ok(()));
    assert_eq!(auth.account_scope().as_deref(), Some("scope-1"));
    assert_eq!(auth.bearer(), Ok(Some("tok".to_string())));
    let g = auth.generation();
    auth.set_enabled(false);
    assert!(!auth.enabled());
    assert!(auth.generation() > g, "disabling cancels an in-flight pass");

    *tok.token.lock().unwrap() = None;
    assert!(matches!(
        auth.availability(),
        Err(SyncError::Unavailable(_))
    ));
}

#[test]
fn account_scope_binds_app_zone_account_and_key_id() {
    let base = account_scope("heardright", "history", "acct-1", "key-1");
    assert_eq!(base.len(), 64);
    assert!(base.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        base,
        account_scope("heardright", "history", "acct-1", "key-1")
    );
    for other in [
        account_scope("other", "history", "acct-1", "key-1"),
        account_scope("heardright", "other", "acct-1", "key-1"),
        account_scope("heardright", "history", "acct-2", "key-1"),
        account_scope("heardright", "history", "acct-1", "key-2"),
    ] {
        assert_ne!(base, other);
    }
    assert!(sync_config().require_consent_on_scope_change);
}

#[test]
fn an_engine_over_a_bare_http_backend_fails_closed_instead_of_sending_plaintext() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let b = Arc::new(backend(&server, "tok", "notes"));
    let mut d = device(b, false);
    d.local.put("n1", "secret transcript", 100, 100);
    let r = d.pass();
    assert!(
        matches!(r.outcome, PassOutcome::Failed(_)),
        "{:?}",
        r.outcome
    );
    assert!(server.records("acct", "notes").is_empty());
}
