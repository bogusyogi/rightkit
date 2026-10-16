//! The full client stack: `SyncEngine` -> `EncryptingBackend` -> `HttpBackend` -> fake
//! `rightkit-sync-http/1` server. The server must only ever hold ciphertext and ordering
//! metadata, and the engine must still converge, delete, replay after compaction and pause
//! on an account change.

mod common;

use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};

use common::fake_server::FakeServer;
use common::*;
use rightkit_http::testing::loopback_config;
use rightkit_http::Client;
use rightkit_sync::cipher::*;
use rightkit_sync::http::*;
use rightkit_sync::wire::b64_decode;
use rightkit_sync::*;

struct Tok {
    token: String,
    scope: Mutex<String>,
}

impl Tok {
    fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            token: token.to_string(),
            scope: Mutex::new("scope-1".into()),
        })
    }
}

impl TokenSource for Tok {
    fn bearer(&self) -> SyncResult<Option<String>> {
        Ok(Some(self.token.clone()))
    }
    fn account_scope(&self) -> Option<String> {
        Some(self.scope.lock().unwrap().clone())
    }
}

fn stack(server: &FakeServer, tok: Arc<Tok>, keys: Arc<dyn KeyProvider>) -> Arc<EncryptingBackend> {
    let http = HttpBackend::new(
        Client::new(loopback_config()),
        tok,
        HttpBackendConfig::new(server.base_url(), "notes"),
    );
    Arc::new(EncryptingBackend::new(Arc::new(http), "notes", keys))
}

fn keys() -> Arc<dyn KeyProvider> {
    Arc::new(StaticKeys::new(1, CipherKey::from_bytes([42; 32])))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn devices_converge_over_http_and_the_server_holds_only_ciphertext() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let mut a = device(stack(&server, Tok::new("tok"), keys()), false);
    let mut b = device(stack(&server, Tok::new("tok"), keys()), false);
    a.local.put("n1", "secret transcript one", 100, 100);
    a.local.put("n2", "secret transcript two", 101, 101);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);

    let stored = server.records("acct", "notes");
    assert_eq!(stored.len(), 2);
    for rec in &stored {
        let obj = rec.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["change_tag", "deleted_at", "enc", "name", "seq", "type", "updated_at"]
        );
        let body = b64_decode(rec["enc"].as_str().expect("sealed body")).unwrap();
        assert!(!contains(&body, b"secret transcript"));
        assert!(!rec.to_string().contains("secret transcript"));
        assert!(rec["deleted_at"].is_null());
    }

    assert_eq!(b.pass().downloaded, 2);
    assert_eq!(b.local.text("n2").as_deref(), Some("secret transcript two"));

    // Edit on B, delete on A: both reach both.
    b.local.put("n1", "edited secret", 100, 900);
    a.local.delete("n2");
    a.engine.note_deleted("n2").unwrap();
    b.pass();
    a.pass();
    b.pass();
    assert_eq!(a.local.text("n1").as_deref(), Some("edited secret"));
    assert!(b.local.text("n2").is_none());
    let tomb = server.record("acct", "notes", "n2").unwrap();
    assert!(tomb["deleted_at"].is_i64() || tomb["deleted_at"].is_u64());
    assert!(tomb["enc"].is_string());
}

#[test]
fn a_replay_after_server_compaction_that_keeps_enc_is_an_authenticated_deletion() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let eb = stack(&server, Tok::new("tok"), keys());
    let mut a = device(stack(&server, Tok::new("tok"), keys()), false);
    let mut b = device(eb.clone(), false);
    a.local.put("n1", "one", 100, 100);
    a.local.put("n2", "two", 101, 101);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 2);

    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    // The server compacts: it raises its floor above B's cursor and keeps the sealed `enc`.
    server.compact_tombstone("acct", "notes", "n1");
    let compacted = server.record("acct", "notes", "n1").unwrap();
    assert!(compacted["enc"].is_string());

    // B's cursor is now below the floor (410 token_expired): the engine replays from the
    // start, and the tombstone authenticates and applies as a deletion.
    let r = b.pass();
    assert_eq!(r.outcome, PassOutcome::Synced, "{r:?}");
    assert_eq!(r.quarantined, 0);
    assert_eq!(b.local.snapshot(), vec![("n2".to_string(), "two".to_string())]);
    assert_eq!(eb.stats().unauthenticated_tombstones, 0);

    // A fresh device replays the same history and ends in the same place.
    let mut c = device(stack(&server, Tok::new("tok"), keys()), false);
    let r = c.pass();
    assert_eq!(r.quarantined, 0);
    assert_eq!(c.local.snapshot(), vec![("n2".to_string(), "two".to_string())]);
}

#[test]
fn a_tombstone_served_without_enc_is_quarantined_and_the_local_record_survives() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let eb = stack(&server, Tok::new("tok"), keys());
    let mut a = device(stack(&server, Tok::new("tok"), keys()), false);
    let mut b = device(eb.clone(), false);
    a.local.put("n1", "one", 100, 100);
    a.local.put("n2", "two", 101, 101);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 2);

    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    // A server that strips the sealed body of a tombstone forges an unauthenticated deletion.
    server.strip_tombstone_enc("acct", "notes", "n1");
    assert!(server.record("acct", "notes", "n1").unwrap()["enc"].is_null());

    let r = b.pass();
    assert_eq!(r.outcome, PassOutcome::Synced, "{r:?}");
    assert_eq!(r.quarantined, 1);
    assert_eq!(r.tombstones_applied, 0);
    assert_eq!(
        b.local.snapshot(),
        vec![
            ("n1".to_string(), "one".to_string()),
            ("n2".to_string(), "two".to_string())
        ]
    );
    assert_eq!(eb.stats().unauthenticated_tombstones, 1);
}

#[test]
fn an_account_change_pauses_for_consent_when_the_app_uses_sync_config() {
    let server = FakeServer::start(&[("tok", "acct")]);
    let tok = Tok::new("tok");
    let local = Local::default();
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        stack(&server, tok.clone(), keys()),
        Arc::new(local.clone()),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(HttpAuth::new(tok.clone())))
    .clock(clock.clone())
    .config(sync_config())
    .build()
    .unwrap();

    local.put("n1", "one", 100, 100);
    assert_eq!(engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(server.records("acct", "notes").len(), 1);

    // The signed-in account (or the key id inside the scope) changes.
    *tok.scope.lock().unwrap() = account_scope("app", "notes", "acct-2", "key-1");
    clock
        .0
        .fetch_add(3_600_000, std::sync::atomic::Ordering::SeqCst);
    local.put("n2", "two", 101, 101);
    let r = engine.run_pass();
    assert!(
        matches!(r.outcome, PassOutcome::ConsentRequired(_)),
        "{:?}",
        r.outcome
    );
    assert_eq!(
        server.records("acct", "notes").len(),
        1,
        "nothing was published into the new account"
    );

    let to = engine.consent_request().unwrap().to;
    engine.grant_consent(&to, ConsentChoice::Merge).unwrap();
    assert_eq!(engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(server.records("acct", "notes").len(), 2);
}
