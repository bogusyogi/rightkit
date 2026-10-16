//! Record cipher: construction properties (round trip, binding, tampering, key versions) and
//! the `EncryptingBackend` wrapper under real engines over a `MemoryBackend` zone.
//!
//! There is no externally computed known-answer vector here: the primitives (ChaCha20-Poly1305,
//! HKDF-SHA256) come from `ring`, which carries its own; these tests pin the envelope layout,
//! determinism under a fixed salt, and every rejection path.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use rightkit_sync::cipher::*;
use rightkit_sync::*;

fn key(b: u8) -> CipherKey {
    CipherKey::from_bytes([b; 32])
}

fn keys(version: u8, b: u8) -> Arc<StaticKeys> {
    Arc::new(StaticKeys::new(version, key(b)))
}

fn note_record() -> RawRecord {
    let mut r = RawRecord::new("Note", "n1");
    r.set("id", FieldValue::Text("n1".into()));
    r.set("createdAt", FieldValue::Date(100));
    r.set("updatedAt", FieldValue::Date(500));
    r.set("text", FieldValue::Text("secret transcript".into()));
    r.set("tags", FieldValue::Strings(vec!["a".into(), "b\u{e9}".into()]));
    r.set("empty", FieldValue::Strings(vec![]));
    r.set("ratio", FieldValue::F64(1.5));
    r.set("count", FieldValue::I64(-7));
    r.set("blob", FieldValue::Bytes(vec![0, 1, 2, 255]));
    r.change_tag = Some("7".into());
    r
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn enc_bytes(r: &RawRecord) -> Vec<u8> {
    match r.get(ENC_FIELD) {
        Some(FieldValue::Bytes(b)) => b.clone(),
        other => panic!("no sealed field: {other:?}"),
    }
}

#[test]
fn seal_open_round_trip_keeps_only_ordering_fields_in_clear() {
    let c = RecordCipher::new("notes", keys(3, 1));
    let rec = note_record();
    let sealed = c.seal(&rec).unwrap();
    let names: Vec<&str> = sealed.fields.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["enc", "updatedAt"]);
    assert_eq!(sealed.record_name, "n1");
    assert_eq!(sealed.record_type, "Note");
    assert_eq!(sealed.change_tag.as_deref(), Some("7"));
    assert_eq!(sealed.date_ms("updatedAt"), Some(500));
    let enc = enc_bytes(&sealed);
    assert!(!contains(&enc, b"secret transcript"));
    assert_eq!(c.open(&sealed), Ok(rec));
}

#[test]
fn tombstone_keeps_deleted_at_in_clear_and_restores_id() {
    let c = RecordCipher::new("notes", keys(1, 1));
    let mut rec = RawRecord::new("Note", "n1");
    write_tombstone(&mut rec, "n1", 900, &[]);
    let sealed = c.seal(&rec).unwrap();
    assert_eq!(sealed.date_ms("deletedAt"), Some(900));
    assert_eq!(sealed.date_ms("updatedAt"), Some(900));
    assert!(sealed.get("id").is_none());
    let opened = c.open(&sealed).unwrap();
    assert_eq!(read_tombstone(&opened), Some(("n1".to_string(), 900)));
    assert_eq!(opened, rec);
}

#[test]
fn envelope_layout_and_salt_determinism() {
    let c = RecordCipher::new("notes", keys(3, 1));
    let rec = note_record();
    let a = c.seal_with_salt(&rec, [9; 16]).unwrap();
    let b = c.seal_with_salt(&rec, [9; 16]).unwrap();
    let other = c.seal_with_salt(&rec, [8; 16]).unwrap();
    assert_eq!(a, b, "same key, salt and record seal identically");
    assert_ne!(enc_bytes(&a), enc_bytes(&other));
    let env = enc_bytes(&a);
    assert_eq!(env[0], 1, "format");
    assert_eq!(env[1], 3, "key version");
    assert_eq!(&env[2..18], &[9u8; 16], "salt");
    assert!(env.len() > 18 + 16, "ciphertext and tag follow the header");
    let r1 = c.seal(&rec).unwrap();
    let r2 = c.seal(&rec).unwrap();
    assert_ne!(enc_bytes(&r1), enc_bytes(&r2), "random salt per write");
}

#[test]
fn every_tamper_is_refused() {
    let c = RecordCipher::new("notes", keys(1, 1));
    let sealed = c.seal(&note_record()).unwrap();
    let auth = Err(OpenError::Authentication);

    let mut t = sealed.clone();
    let mut env = enc_bytes(&t);
    let last = env.len() - 1;
    env[last] ^= 1;
    t.set(ENC_FIELD, FieldValue::Bytes(env));
    assert_eq!(c.open(&t), auth, "flipped ciphertext bit");

    let mut t = sealed.clone();
    t.record_name = "n2".into();
    assert_eq!(c.open(&t), auth, "moved to another record name");

    let mut t = sealed.clone();
    t.record_type = "Other".into();
    assert_eq!(c.open(&t), auth, "moved to another record type");

    let mut t = sealed.clone();
    t.set("updatedAt", FieldValue::Date(501));
    assert_eq!(c.open(&t), auth, "changed version");

    let mut t = sealed.clone();
    t.fields.remove("updatedAt");
    assert_eq!(c.open(&t), auth, "removed version");

    let mut t = sealed.clone();
    t.set("deletedAt", FieldValue::Date(500));
    assert_eq!(c.open(&t), auth, "live record turned into a tombstone");

    let other_zone = RecordCipher::new("other", keys(1, 1));
    assert_eq!(other_zone.open(&sealed), auth, "moved to another zone");

    let wrong_key = RecordCipher::new("notes", keys(1, 2));
    assert_eq!(wrong_key.open(&sealed), auth, "wrong key, right version");

    let mut t = sealed.clone();
    let mut env = enc_bytes(&t);
    env[1] = 2;
    t.set(ENC_FIELD, FieldValue::Bytes(env));
    assert_eq!(
        c.open(&t),
        Err(OpenError::UnknownKeyVersion(2)),
        "key version is also bound, and unknown to this provider"
    );
}

#[test]
fn a_sealed_tombstone_cannot_be_revived() {
    let c = RecordCipher::new("notes", keys(1, 1));
    let mut rec = RawRecord::new("Note", "n1");
    write_tombstone(&mut rec, "n1", 900, &[]);
    let mut t = c.seal(&rec).unwrap();
    t.fields.remove("deletedAt");
    assert_eq!(c.open(&t), Err(OpenError::Authentication));
}

#[test]
fn malformed_envelopes_and_foreign_fields() {
    let c = RecordCipher::new("notes", keys(1, 1));
    let rec = note_record();
    let sealed = c.seal(&rec).unwrap();

    let plain = note_record();
    assert!(matches!(c.open(&plain), Err(OpenError::Malformed(_))));

    let mut t = sealed.clone();
    t.set(ENC_FIELD, FieldValue::Bytes(vec![1, 1, 2, 3]));
    assert!(matches!(c.open(&t), Err(OpenError::Malformed(_))));

    let mut t = sealed.clone();
    let mut env = enc_bytes(&t);
    env[0] = 2;
    t.set(ENC_FIELD, FieldValue::Bytes(env));
    assert!(matches!(c.open(&t), Err(OpenError::Malformed(_))));

    let mut t = sealed.clone();
    t.set("updatedAt", FieldValue::Text("x".into()));
    assert!(matches!(c.open(&t), Err(OpenError::Malformed(_))));

    // Fields a server adds beside the sealed one never reach the engine.
    let mut t = sealed.clone();
    t.set("text", FieldValue::Text("injected".into()));
    t.set("extra", FieldValue::Text("injected".into()));
    let opened = c.open(&t).unwrap();
    assert_eq!(opened.text("text").as_deref(), Some("secret transcript"));
    assert!(opened.get("extra").is_none());
}

#[test]
fn key_versions_rotate_without_losing_old_records() {
    let old = RecordCipher::new("notes", keys(1, 1));
    let sealed_old = old.seal(&note_record()).unwrap();

    let rotated = Arc::new(StaticKeys::new(2, key(2)).with_key(1, key(1)));
    let c = RecordCipher::new("notes", rotated);
    assert_eq!(c.open(&sealed_old), Ok(note_record()));
    let sealed_new = c.seal(&note_record()).unwrap();
    assert_eq!(enc_bytes(&sealed_new)[1], 2, "new writes use the current version");
    assert_eq!(c.open(&sealed_new), Ok(note_record()));

    let only_new = RecordCipher::new("notes", keys(2, 2));
    assert_eq!(
        only_new.open(&sealed_old),
        Err(OpenError::UnknownKeyVersion(1))
    );
}

/// Key provider whose keys the test can swap.
struct SwitchKeys(Mutex<StaticKeys>);
impl SwitchKeys {
    fn new(k: StaticKeys) -> Arc<Self> {
        Arc::new(Self(Mutex::new(k)))
    }
    fn set(&self, k: StaticKeys) {
        *self.0.lock().unwrap() = k;
    }
}
impl KeyProvider for SwitchKeys {
    fn current(&self) -> SyncResult<(u8, CipherKey)> {
        self.0.lock().unwrap().current()
    }
    fn key_for(&self, version: u8) -> SyncResult<Option<CipherKey>> {
        self.0.lock().unwrap().key_for(version)
    }
}

fn enc_backend(zone: &MemoryBackend, keys: Arc<dyn KeyProvider>) -> Arc<EncryptingBackend> {
    Arc::new(EncryptingBackend::new(Arc::new(zone.clone()), "notes", keys))
}

#[test]
fn two_devices_converge_through_the_cipher_and_the_zone_holds_no_plaintext() {
    let zone = MemoryBackend::new();
    let k = keys(1, 5);
    let (ea, eb) = (enc_backend(&zone, k.clone()), enc_backend(&zone, k.clone()));
    let (mut a, mut b) = (device(ea.clone(), false), device(eb.clone(), false));
    a.local.put("n1", "secret transcript one", 100, 100);
    a.local.put("n2", "secret transcript two", 101, 101);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);

    let stored = zone.record("n1").unwrap();
    let names: Vec<&str> = stored.fields.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["enc", "updatedAt"]);
    assert!(!contains(&enc_bytes(&stored), b"secret transcript"));

    assert_eq!(b.pass().downloaded, 2);
    assert_eq!(b.local.text("n1").as_deref(), Some("secret transcript one"));

    // B edits n1; A deletes n2. Both reach both sides.
    b.local.put("n1", "edited secret", 100, 900);
    a.local.delete("n2");
    a.engine.note_deleted("n2").unwrap();
    b.pass();
    a.pass();
    b.pass();
    assert_eq!(a.local.text("n1").as_deref(), Some("edited secret"));
    assert!(b.local.text("n2").is_none());

    let tomb = zone.record("n2").unwrap();
    assert!(tomb.date_ms("deletedAt").is_some(), "tombstone flag stays in clear");
    let names: Vec<&str> = tomb.fields.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["deletedAt", "enc", "updatedAt"]);
    assert!(ea.stats().sealed >= 3 && eb.stats().opened >= 2);
}

#[test]
fn a_conflict_outcome_carries_a_decrypted_server_record() {
    let zone = MemoryBackend::new();
    let k = keys(1, 5);
    let mut a = device(enc_backend(&zone, k.clone()), false);
    a.local.put("n1", "v1", 100, 100);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);

    // Another device's write lands between A's fetch and A's save.
    let mut other = RawRecord::new("Note", "n1");
    other.set("id", FieldValue::Text("n1".into()));
    other.set("createdAt", FieldValue::Date(100));
    other.set("updatedAt", FieldValue::Date(150));
    other.set("text", FieldValue::Text("other".into()));
    let sealed_other = RecordCipher::new("notes", k.clone()).seal(&other).unwrap();
    zone.concurrent_write_before_next_save(sealed_other);

    a.local.put("n1", "a-edit", 100, 200);
    let r = a.pass();
    assert_eq!(
        r.outcome,
        PassOutcome::Synced,
        "the conflict was resolved from a decrypted server record: {r:?}"
    );

    let mut b = device(enc_backend(&zone, k), false);
    b.pass();
    assert_eq!(b.local.text("n1").as_deref(), Some("a-edit"));
}

#[test]
fn a_missing_key_fails_the_pass_and_never_advances_the_cursor() {
    let zone = MemoryBackend::new();
    let mut a = device(enc_backend(&zone, keys(1, 7)), false);
    a.local.put("n1", "one", 100, 100);
    a.local.put("n2", "two", 101, 101);
    a.pass();

    let provider = SwitchKeys::new(StaticKeys::new(2, key(9)));
    let backend = enc_backend(&zone, provider.clone());
    let mut c = device(backend.clone(), false);
    let r = c.pass();
    assert!(
        matches!(r.outcome, PassOutcome::Unavailable(_)),
        "{:?}",
        r.outcome
    );
    assert!(c.engine.ledger().change_token.is_none(), "cursor stayed put");
    assert_eq!(c.local.len(), 0);
    assert_eq!(c.engine.ledger().quarantined_total, 0, "nothing was quarantined");
    assert!(backend.stats().key_unavailable >= 1);

    // The key arrives; the same pass resumes from the start without any replay request.
    provider.set(StaticKeys::new(2, key(9)).with_key(1, key(7)));
    let r = c.pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(r.downloaded, 2);
    assert_eq!(c.local.text("n2").as_deref(), Some("two"));
}

#[test]
fn a_locked_key_provider_fails_the_pass_too() {
    struct Locked;
    impl KeyProvider for Locked {
        fn current(&self) -> SyncResult<(u8, CipherKey)> {
            Err(SyncError::Unavailable("keychain locked".into()))
        }
        fn key_for(&self, _: u8) -> SyncResult<Option<CipherKey>> {
            Err(SyncError::Unavailable("keychain locked".into()))
        }
    }
    let zone = MemoryBackend::new();
    let mut a = device(enc_backend(&zone, keys(1, 7)), false);
    a.local.put("n1", "one", 100, 100);
    a.pass();
    let mut c = device(enc_backend(&zone, Arc::new(Locked)), false);
    let r = c.pass();
    assert!(matches!(r.outcome, PassOutcome::Unavailable(_)), "{:?}", r.outcome);
    assert!(c.engine.ledger().change_token.is_none());
}

#[test]
fn corrupt_records_are_quarantined_and_never_block_the_page() {
    let zone = MemoryBackend::new();
    let k = keys(1, 7);
    let mut a = device(enc_backend(&zone, k.clone()), false);
    a.local.put("n1", "one", 100, 100);
    a.local.put("n2", "two", 101, 101);
    a.pass();

    let mut bad = zone.record("n1").unwrap();
    let mut env = enc_bytes(&bad);
    let last = env.len() - 1;
    env[last] ^= 1;
    bad.set(ENC_FIELD, FieldValue::Bytes(env));
    zone.put_record(bad);
    // A legacy plaintext record in the same zone.
    let mut plain = RawRecord::new("Note", "n3");
    plain.set("id", FieldValue::Text("n3".into()));
    plain.set("text", FieldValue::Text("plain".into()));
    zone.put_record(plain);

    let backend = enc_backend(&zone, k);
    let mut b = device(backend.clone(), false);
    let r = b.pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(r.quarantined, 2);
    assert_eq!(b.local.snapshot(), vec![("n2".to_string(), "two".to_string())]);
    let s = backend.stats();
    assert_eq!((s.authentication_failed, s.malformed), (1, 1));
}

#[test]
fn wrong_keys_quarantine_and_a_replay_recovers_after_the_keys_are_fixed() {
    let zone = MemoryBackend::new();
    let mut a = device(enc_backend(&zone, keys(1, 7)), false);
    a.local.put("n1", "one", 100, 100);
    a.local.put("n2", "two", 101, 101);
    a.pass();

    let provider = SwitchKeys::new(StaticKeys::new(1, key(66)));
    let mut d = device(enc_backend(&zone, provider.clone()), false);
    let r = d.pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(r.quarantined, 2);
    assert_eq!(d.local.len(), 0);
    assert!(d.engine.ledger().change_token.is_some(), "the cursor moved on");

    provider.set(StaticKeys::new(1, key(7)));
    d.engine.request_full_replay_once("keys-fixed");
    let r = d.pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(r.downloaded, 2);
}

#[test]
fn a_forged_tombstone_on_a_sealed_record_does_not_delete_local_data() {
    let zone = MemoryBackend::new();
    let k = keys(1, 7);
    let mut a = device(enc_backend(&zone, k.clone()), false);
    a.local.put("n1", "one", 100, 100);
    a.pass();
    let mut b = device(enc_backend(&zone, k), false);
    b.pass();
    assert_eq!(b.local.len(), 1);

    let mut forged = zone.record("n1").unwrap();
    forged.set("deletedAt", FieldValue::Date(5_000));
    zone.put_record(forged);
    let r = b.pass();
    assert_eq!(r.quarantined, 1);
    assert_eq!(b.local.text("n1").as_deref(), Some("one"));
}

#[test]
fn a_tombstone_without_enc_is_quarantined_and_the_local_record_survives() {
    let zone = MemoryBackend::new();
    let k = keys(1, 7);
    let mut a = device(enc_backend(&zone, k.clone()), false);
    let backend_b = enc_backend(&zone, k);
    let mut b = device(backend_b.clone(), false);
    a.local.put("n1", "one", 100, 100);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 1);

    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    a.pass();
    // A server strips the sealed field of the tombstone: ordering fields kept, no `enc`.
    let mut stripped = zone.record("n1").unwrap();
    assert!(stripped.date_ms("deletedAt").is_some());
    stripped.fields.remove(ENC_FIELD);
    zone.put_record(stripped);

    let r = b.pass();
    assert_eq!(r.quarantined, 1);
    assert_eq!(r.tombstones_applied, 0);
    assert_eq!(b.local.text("n1").as_deref(), Some("one"), "the local record survives");
    let s = backend_b.stats();
    assert_eq!(s.unauthenticated_tombstones, 1);
    assert_eq!(s.malformed, 0, "counted once, under its own reason");
}

#[test]
fn a_tombstone_that_keeps_enc_through_compaction_is_an_authenticated_deletion() {
    let zone = MemoryBackend::new();
    let k = keys(1, 7);
    let mut a = device(enc_backend(&zone, k.clone()), false);
    let backend_b = enc_backend(&zone, k);
    let mut b = device(backend_b.clone(), false);
    a.local.put("n1", "one", 100, 100);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 1);

    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    a.pass();
    // Compaction keeps the sealed `enc` on the tombstone (only out-of-line bodies go).
    let kept = zone.record("n1").unwrap();
    assert!(kept.date_ms("deletedAt").is_some());
    assert!(kept.fields.contains_key(ENC_FIELD));

    let r = b.pass();
    assert_eq!(r.quarantined, 0);
    assert_eq!(r.tombstones_applied, 1);
    assert_eq!(b.local.len(), 0);
    assert_eq!(backend_b.stats().unauthenticated_tombstones, 0);
}
