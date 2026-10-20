#![cfg(feature = "host-services")]
//! The host services (Orthic-Labs/rightkit PK04): the settings bridge over a real `Document`, the
//! write-only secret slots over fake credential stores, and the caller policy that fronts both.
//!
//! Everything runs on the plain-Rust cores (`SettingsBridge`, `SecretsService`, `HostServices`)
//! with `CallerFacts` built by hand, exactly what the Tauri commands build from the invoking
//! webview, so no webview is needed. The command wrappers add only the facts and a thread hop.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_app_settings::{
    Document, Edit, Error as StoreError, OnCorrupt, Schema, SecretRef, SettingsBackend,
    StoreOptions, Stored,
};
use rightkit_tauri_shell::caller_gate::CallerFacts;
use rightkit_tauri_shell::host::secrets::secrets_for;
use rightkit_tauri_shell::host::settings::settings_for;
use rightkit_tauri_shell::host::{
    ApiKeySlot, HostError, HostServices, SecretPresence, SecretStore, SecretStoreError,
    SecretValue, SecretWriter, SecretsError, SecretsService, ServiceId, ServiceTrust,
    SettingsBridge, SettingsChange, SettingsError, SettingsHostService, SettingsSnapshot,
    SettingsSubscription, StaticSchema, WriteIntent,
};
use rightkit_tauri_shell::NavPolicy;
use serde_json::{json, Value};

const NS: &str = "demo";

fn doc_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

fn open(dir: &Path) -> Document {
    Document::open_path(doc_path(dir), NS).unwrap()
}

fn schema() -> StaticSchema {
    StaticSchema::new(1)
}

fn bridge(dir: &Path, schema: StaticSchema) -> SettingsBridge {
    SettingsBridge::new(open(dir), schema)
}

type Seen = Arc<Mutex<Vec<SettingsChange>>>;

fn record(bridge: &SettingsBridge) -> (Seen, SettingsSubscription) {
    let seen: Seen = Arc::default();
    let sink = Arc::clone(&seen);
    let sub = bridge.subscribe(move |c| sink.lock().unwrap().push(c.clone()));
    (seen, sub)
}

fn values(snapshot: &SettingsSnapshot) -> &serde_json::Map<String, Value> {
    snapshot.values.as_object().unwrap()
}

// ---- settings bridge ------------------------------------------------------------------------

#[test]
fn an_empty_store_reads_as_revision_zero_and_a_patch_commits_revision_one() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    let first = b.get().unwrap();
    assert_eq!(first.revision, 0);
    assert!(values(&first).is_empty());
    let saved = b.patch(&json!({"theme": "dark"}), Some(0)).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(values(&saved)["theme"], "dark");
    assert_eq!(b.get().unwrap(), saved);
}

#[test]
fn a_stale_revision_is_a_typed_conflict_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    b.patch(&json!({"a": 1}), None).unwrap();
    let before = fs::read(doc_path(dir.path())).unwrap();
    let err = b.patch(&json!({"a": 2}), Some(0)).unwrap_err();
    assert_eq!(
        err,
        SettingsError::Conflict {
            expected: 0,
            actual: 1
        }
    );
    assert_eq!(fs::read(doc_path(dir.path())).unwrap(), before);
    let wire = HostError::from(err);
    assert_eq!(wire.code, "revision_conflict");
    assert_eq!(wire.details.unwrap(), json!({"expected": 0, "actual": 1}));
}

#[test]
fn a_patch_that_fails_validation_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(
        dir.path(),
        schema().validate_with(
            |candidate| match candidate.get("theme").and_then(Value::as_str) {
                None | Some("light") | Some("dark") => Ok(()),
                Some(_) => Err("theme must be light or dark".into()),
            },
        ),
    );
    b.patch(&json!({"theme": "dark"}), None).unwrap();
    let before = fs::read(doc_path(dir.path())).unwrap();
    let err = b.patch(&json!({"theme": "neon"}), None).unwrap_err();
    assert_eq!(err.code(), "validation_failed");
    assert_eq!(err.to_string(), "theme must be light or dark");
    assert_eq!(fs::read(doc_path(dir.path())).unwrap(), before);
    assert_eq!(b.get().unwrap().revision, 1);
}

#[test]
fn validation_sees_the_complete_candidate_not_just_the_patch() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(
        dir.path(),
        schema().validate_with(|candidate| {
            if candidate.contains_key("lo") && candidate.contains_key("hi") {
                let lo = candidate["lo"].as_i64().unwrap();
                let hi = candidate["hi"].as_i64().unwrap();
                if lo > hi {
                    return Err("lo exceeds hi".into());
                }
            }
            Ok(())
        }),
    );
    b.patch(&json!({"lo": 1, "hi": 5}), None).unwrap();
    // Each key alone is fine; together with the stored one the document is not.
    assert_eq!(
        b.patch(&json!({"lo": 9}), None).unwrap_err().code(),
        "validation_failed"
    );
    assert_eq!(b.get().unwrap().revision, 1);
}

#[test]
fn a_document_from_a_newer_build_is_refused_for_reads_and_writes_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .put(9, json!({"theme": "future"}), None)
        .unwrap();
    let before = fs::read(doc_path(dir.path())).unwrap();
    let b = bridge(dir.path(), schema());
    let expected = SettingsError::NewerSchema {
        found: 9,
        supported: 1,
    };
    assert_eq!(b.get().unwrap_err(), expected);
    assert_eq!(
        b.patch(&json!({"theme": "dark"}), None).unwrap_err(),
        expected
    );
    assert_eq!(b.reset(&["theme".into()], None).unwrap_err(), expected);
    let described = b.describe();
    assert!(!described.writable);
    assert_eq!(described.refusal, Some("newer_schema"));
    assert_eq!(fs::read(doc_path(dir.path())).unwrap(), before);
}

#[test]
fn an_oversized_value_or_document_is_refused_before_it_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(
        dir.path(),
        schema().max_value_bytes(16).max_document_bytes(64),
    );
    let err = b
        .patch(&json!({"note": "x".repeat(100)}), None)
        .unwrap_err();
    assert!(
        matches!(err, SettingsError::ValueTooLarge { limit: 16, .. }),
        "{err:?}"
    );
    assert_eq!(HostError::from(err).code, "value_too_large");
    assert!(
        open(dir.path()).get().unwrap().is_none(),
        "nothing was written"
    );

    // Each value fits; the document does not.
    b.patch(&json!({"a": "0123456789"}), None).unwrap();
    b.patch(&json!({"b": "0123456789"}), None).unwrap();
    let err = b
        .patch(&json!({"c": "0123456789", "d": "0123456789"}), None)
        .unwrap_err();
    assert!(
        matches!(err, SettingsError::DocumentTooLarge { .. }),
        "{err:?}"
    );
    assert_eq!(b.get().unwrap().revision, 2);
}

#[test]
fn keys_the_schema_does_not_allow_and_malformed_requests_are_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema().keys(["theme"]));
    for bad in [json!({"other": 1}), json!({"": 1}), json!({"a\nb": 1})] {
        assert_eq!(b.patch(&bad, None).unwrap_err().code(), "invalid", "{bad}");
    }
    for not_an_object in [json!([1]), json!("x"), json!(null), json!(3)] {
        assert_eq!(
            b.patch(&not_an_object, None).unwrap_err().code(),
            "invalid",
            "{not_an_object}"
        );
    }
    assert_eq!(b.reset(&[], None).unwrap_err().code(), "invalid");
    assert_eq!(
        b.reset(&["other".into()], None).unwrap_err().code(),
        "invalid"
    );
    assert!(open(dir.path()).get().unwrap().is_none());
}

#[test]
fn null_removes_a_key_and_reset_returns_it_to_the_frontends_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    b.patch(&json!({"a": 1, "b": 2, "c": 3}), None).unwrap();
    let after_null = b.patch(&json!({"a": null}), None).unwrap();
    assert!(!values(&after_null).contains_key("a"));
    let after_reset = b
        .reset(&["b".into(), "missing".into()], Some(after_null.revision))
        .unwrap();
    assert_eq!(after_reset.values, json!({"c": 3}));
    assert_eq!(after_reset.revision, after_null.revision + 1);
    // Resetting nothing that is stored changes nothing and commits no revision.
    let again = b.reset(&["b".into()], None).unwrap();
    assert_eq!(again.revision, after_reset.revision);
}

#[test]
fn reset_cannot_remove_what_validation_requires() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(
        dir.path(),
        schema().validate_with(|c| {
            if c.contains_key("required") {
                Ok(())
            } else {
                Err("required is missing".into())
            }
        }),
    );
    b.patch(&json!({"required": true, "x": 1}), None).unwrap();
    assert_eq!(
        b.reset(&["required".into()], None).unwrap_err().code(),
        "validation_failed"
    );
    assert!(b.reset(&["x".into()], None).is_ok());
}

#[test]
fn subscribers_hear_a_commit_only_after_it_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    let path = doc_path(dir.path());
    let stored_at_notice: Arc<Mutex<Vec<(SettingsChange, u64)>>> = Arc::default();
    let sink = Arc::clone(&stored_at_notice);
    let _sub = b.subscribe(move |change| {
        // A second handle on the same file sees the commit already.
        let revision = Document::open_path(&path, NS)
            .unwrap()
            .get()
            .unwrap()
            .map_or(0, |s| s.revision);
        sink.lock().unwrap().push((change.clone(), revision));
    });
    let saved = b.patch(&json!({"theme": "dark", "n": 1}), None).unwrap();
    let seen = stored_at_notice.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.keys, ["n", "theme"]);
    assert_eq!(seen[0].0.revision, saved.revision);
    assert_eq!(
        seen[0].1, saved.revision,
        "the stored revision when the subscriber ran"
    );
}

#[test]
fn an_unchanged_patch_notifies_nobody_and_commits_no_revision() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    b.patch(&json!({"a": 1}), None).unwrap();
    let (seen, _sub) = record(&b);
    let same = b.patch(&json!({"a": 1}), None).unwrap();
    assert_eq!(same.revision, 1);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn an_external_edit_reaches_subscribers_once_and_own_writes_are_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    b.patch(&json!({"theme": "light", "keep": 1}), None)
        .unwrap();
    let (seen, _sub) = record(&b);

    // Another process (here a second handle) edits the file.
    open(dir.path())
        .patch(1, &json!({"theme": "dark", "added": true}), None)
        .unwrap();
    let change = b.poll_external().unwrap().expect("an external change");
    assert_eq!(change.keys, ["added", "theme"]);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap()[0], change);
    assert_eq!(b.poll_external().unwrap(), None, "reported once");

    // The bridge's own write is announced by the write, not again by the next poll.
    b.patch(&json!({"keep": 2}), None).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(b.poll_external().unwrap(), None);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn the_first_poll_only_records_a_baseline() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path()).put(1, json!({"a": 1}), None).unwrap();
    let b = bridge(dir.path(), schema());
    let (seen, _sub) = record(&b);
    assert_eq!(b.poll_external().unwrap(), None);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_write_that_follows_an_external_edit_reports_both_changes() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    b.patch(&json!({"a": 1}), None).unwrap();
    let (seen, _sub) = record(&b);
    open(dir.path()).patch(1, &json!({"ext": 1}), None).unwrap();
    b.patch(&json!({"mine": 1}), None).unwrap();
    let all: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|c| c.keys.clone())
        .collect();
    assert!(
        all.contains(&"ext".to_owned()) && all.contains(&"mine".to_owned()),
        "{all:?}"
    );
}

#[test]
fn a_dropped_subscription_stops_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    let (seen, sub) = record(&b);
    b.patch(&json!({"a": 1}), None).unwrap();
    drop(sub);
    b.patch(&json!({"a": 2}), None).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_panicking_subscriber_neither_fails_the_commit_nor_silences_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(dir.path(), schema());
    let _bad = b.subscribe(|_| panic!("subscriber bug"));
    let (seen, _good) = record(&b);
    let saved = b.patch(&json!({"a": 1}), None).expect("the commit stands");
    assert_eq!(saved.revision, 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(b.notify_failures(), 1);
    assert_eq!(open(dir.path()).get().unwrap().unwrap().revision, 1);
}

#[test]
fn a_read_only_bridge_reads_but_refuses_every_mutation() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path()).put(1, json!({"a": 1}), None).unwrap();
    let before = fs::read(doc_path(dir.path())).unwrap();
    let b = bridge(dir.path(), schema()).read_only(true);
    assert_eq!(b.get().unwrap().revision, 1);
    assert!(!b.describe().writable);
    for err in [
        b.patch(&json!({"a": 2}), None).unwrap_err(),
        b.reset(&["a".into()], None).unwrap_err(),
    ] {
        assert_eq!(err.code(), "read_only");
    }
    assert_eq!(fs::read(doc_path(dir.path())).unwrap(), before);
}

/// A backend that holds one document and refuses every write, as a read-only volume or store does.
struct ReadOnlyBackend;

impl SettingsBackend for ReadOnlyBackend {
    fn load(&self) -> Result<Option<Stored>, StoreError> {
        Ok(None)
    }

    fn patch(
        &self,
        _edit: &mut dyn FnMut(Option<&Stored>) -> Result<Edit, StoreError>,
    ) -> Result<Option<u64>, StoreError> {
        Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "read-only volume C:\\secret\\place",
        )))
    }
}

#[test]
fn a_read_only_backend_is_a_typed_read_only_error_without_the_os_message() {
    let b = SettingsBridge::new(
        Document::with_backend(ReadOnlyBackend, NS).unwrap(),
        schema(),
    );
    assert_eq!(b.get().unwrap().revision, 0);
    let err = b.patch(&json!({"a": 1}), None).unwrap_err();
    assert_eq!(err.code(), "read_only");
    let wire = serde_json::to_string(&HostError::from(err)).unwrap();
    assert!(!wire.contains("secret"), "{wire}");
    assert!(!wire.contains("C:"), "{wire}");
}

#[test]
fn a_damaged_settings_file_is_a_storage_error_that_names_no_path() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(doc_path(dir.path()), b"{ not json").unwrap();
    // `Refuse` leaves the file alone and reports it, with the path in the store's own message.
    let options = StoreOptions::new(Schema::new(NS, 1)).on_corrupt(OnCorrupt::Refuse);
    let document = Document::open_path_with(doc_path(dir.path()), &options).unwrap();
    let b = SettingsBridge::new(document, schema());
    let err = b.get().unwrap_err();
    assert_eq!(err.code(), "storage");
    let wire = serde_json::to_string(&HostError::from(err)).unwrap();
    assert!(!wire.contains("settings.json"), "{wire}");
    let temp_name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(!wire.contains(&temp_name), "{wire}");
    assert_eq!(fs::read(doc_path(dir.path())).unwrap(), b"{ not json");
}

#[test]
fn describe_carries_the_apps_descriptor_limits_and_the_live_revision() {
    let dir = tempfile::tempdir().unwrap();
    let b = bridge(
        dir.path(),
        schema()
            .descriptor(json!({"sections": ["general"]}))
            .max_value_bytes(1000),
    );
    b.patch(&json!({"a": 1}), None).unwrap();
    let d = b.describe();
    assert_eq!(d.schema_version, 1);
    assert_eq!(d.revision, 1);
    assert_eq!(d.descriptor, json!({"sections": ["general"]}));
    assert_eq!(d.limits.max_value_bytes, 1000);
    assert!(d.writable);
    assert_eq!(d.refusal, None);
    let wire = serde_json::to_value(&d).unwrap();
    assert_eq!(wire["schemaVersion"], 1);
    assert_eq!(wire["limits"]["maxValueBytes"], 1000);
    assert!(wire.get("refusal").is_none());
}

#[test]
fn the_frontends_schema_version_is_stamped_and_never_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path()).put(1, json!({"a": 1}), None).unwrap();
    let v2 = SettingsBridge::new(open(dir.path()), StaticSchema::new(2));
    v2.patch(&json!({"b": 2}), None).unwrap();
    assert_eq!(open(dir.path()).get().unwrap().unwrap().version, 2);
    // The older build now sees a newer document and refuses it.
    let v1 = bridge(dir.path(), schema());
    assert_eq!(v1.get().unwrap_err().code(), "newer_schema");
}

// ---- secrets ---------------------------------------------------------------------------------

#[derive(Default)]
struct FakeInner {
    present: Mutex<BTreeSet<(String, String)>>,
    calls: AtomicUsize,
    /// What the fake writer was handed. Test-only: the service API has no way to read it back.
    stored: Mutex<Vec<String>>,
    fail_with: Mutex<Option<SecretStoreError>>,
    unavailable: Mutex<bool>,
}

#[derive(Clone, Default)]
struct Fake(Arc<FakeInner>);

impl Fake {
    fn calls(&self) -> usize {
        self.0.calls.load(Ordering::SeqCst)
    }

    fn seed(&self, secret: &SecretRef) {
        self.0
            .present
            .lock()
            .unwrap()
            .insert((secret.service.clone(), secret.account.clone()));
    }

    fn holds(&self, secret: &SecretRef) -> bool {
        self.0
            .present
            .lock()
            .unwrap()
            .contains(&(secret.service.clone(), secret.account.clone()))
    }
}

impl SecretStore for Fake {
    fn exists(&self, secret: &SecretRef) -> Result<bool, SecretStoreError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if *self.0.unavailable.lock().unwrap() {
            return Err(SecretStoreError::Unavailable("keychain locked".into()));
        }
        Ok(self.holds(secret))
    }

    fn delete(&self, secret: &SecretRef) -> Result<(), SecretStoreError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        self.0
            .present
            .lock()
            .unwrap()
            .remove(&(secret.service.clone(), secret.account.clone()));
        Ok(())
    }
}

impl SecretWriter for Fake {
    fn store(
        &self,
        secret: &SecretRef,
        value: &SecretValue,
        overwrite: bool,
    ) -> Result<(), SecretStoreError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.0.fail_with.lock().unwrap().clone() {
            return Err(e);
        }
        if !overwrite && self.holds(secret) {
            return Err(SecretStoreError::AlreadyExists);
        }
        self.seed(secret);
        self.0
            .stored
            .lock()
            .unwrap()
            .push(value.expose().to_owned());
        Ok(())
    }
}

fn slot_ref() -> SecretRef {
    SecretRef::new("myapp", "openai")
}

fn service(fake: &Fake) -> SecretsService {
    SecretsService::new(fake.clone(), [ApiKeySlot::new(slot_ref()).max_bytes(32)])
        .with_writer(fake.clone())
}

fn key(text: &str) -> SecretValue {
    SecretValue::from(text.to_owned())
}

const SECRET: &str = "sk-live-0123456789abcdef";

#[test]
fn only_declared_slots_can_be_probed_set_or_cleared() {
    let fake = Fake::default();
    let svc = service(&fake);
    let other = SecretRef::new("myapp", "somebody-elses-token");
    assert_eq!(
        svc.presence(&[slot_ref(), other.clone()]),
        Err(SecretsError::SlotNotDeclared)
    );
    assert_eq!(svc.clear(&other), Err(SecretsError::SlotNotDeclared));
    assert_eq!(
        svc.set(&other, &key(SECRET), WriteIntent::Replace),
        Err(SecretsError::SlotNotDeclared)
    );
    // A near-miss is not the slot.
    let near = SecretRef::new("myapp", "OPENAI");
    assert_eq!(svc.clear(&near), Err(SecretsError::SlotNotDeclared));
    assert_eq!(fake.calls(), 0, "the store was never reached");
}

#[test]
fn presence_reports_present_absent_and_unknown_never_a_value() {
    let fake = Fake::default();
    let svc = service(&fake);
    assert_eq!(
        svc.presence(&[slot_ref()]).unwrap(),
        [SecretPresence::Absent]
    );
    fake.seed(&slot_ref());
    assert_eq!(
        svc.presence(&[slot_ref()]).unwrap(),
        [SecretPresence::Present]
    );
    *fake.0.unavailable.lock().unwrap() = true;
    assert_eq!(
        svc.presence(&[slot_ref()]).unwrap(),
        [SecretPresence::Unknown]
    );
    let wire = serde_json::to_value(svc.presence(&[slot_ref()]).unwrap()).unwrap();
    assert_eq!(wire, json!(["unknown"]));
}

#[test]
fn set_states_its_intent_create_refuses_to_replace_and_replace_overwrites() {
    let fake = Fake::default();
    let svc = service(&fake);
    svc.set(&slot_ref(), &key(SECRET), WriteIntent::Create)
        .unwrap();
    assert!(fake.holds(&slot_ref()));
    assert_eq!(
        svc.set(&slot_ref(), &key("another-key"), WriteIntent::Create),
        Err(SecretsError::Exists)
    );
    svc.set(&slot_ref(), &key("another-key"), WriteIntent::Replace)
        .unwrap();
    assert_eq!(
        fake.0.stored.lock().unwrap().as_slice(),
        [SECRET, "another-key"]
    );
    // The intent has no default: a request without one does not parse.
    assert!(serde_json::from_value::<WriteIntent>(json!("overwrite")).is_err());
    assert_eq!(
        serde_json::from_value::<WriteIntent>(json!("replace")).unwrap(),
        WriteIntent::Replace
    );
}

#[test]
fn a_bad_value_never_reaches_the_writer() {
    let fake = Fake::default();
    let svc = service(&fake);
    let too_long = "k".repeat(33);
    for bad in [
        "",
        too_long.as_str(),
        "line\nbreak",
        "tab\tbed",
        "nul\0byte",
    ] {
        let r = svc.set(&slot_ref(), &key(bad), WriteIntent::Replace);
        assert!(
            matches!(r, Err(SecretsError::BadValue(_))),
            "{bad:?}: {r:?}"
        );
    }
    assert_eq!(fake.calls(), 0);
    assert!(svc
        .set(&slot_ref(), &key(&"k".repeat(32)), WriteIntent::Replace)
        .is_ok());
}

#[test]
fn clear_removes_a_declared_slot_and_is_idempotent() {
    let fake = Fake::default();
    let svc = service(&fake);
    fake.seed(&slot_ref());
    svc.clear(&slot_ref()).unwrap();
    assert!(!fake.holds(&slot_ref()));
    svc.clear(&slot_ref()).unwrap();
}

#[test]
fn set_is_unavailable_without_a_writer() {
    let fake = Fake::default();
    let svc = SecretsService::new(fake.clone(), [ApiKeySlot::new(slot_ref())]);
    assert_eq!(
        svc.set(&slot_ref(), &key(SECRET), WriteIntent::Replace),
        Err(SecretsError::WriteUnavailable)
    );
    // Presence and clear still work.
    assert!(svc.presence(&[slot_ref()]).is_ok());
    assert_eq!(
        HostError::from(SecretsError::WriteUnavailable).code,
        "unsupported"
    );
}

#[test]
fn nothing_a_secrets_call_answers_carries_the_secret_not_even_a_store_error() {
    let fake = Fake::default();
    let svc = service(&fake);
    let mut answers: Vec<String> = Vec::new();
    let mut push = |label: &str, text: String| answers.push(format!("{label}: {text}"));

    push(
        "set ok",
        format!(
            "{:?}",
            svc.set(&slot_ref(), &key(SECRET), WriteIntent::Create)
        ),
    );
    push(
        "set exists",
        format!(
            "{:?}",
            svc.set(&slot_ref(), &key(SECRET), WriteIntent::Create)
        ),
    );
    let exists_again = svc
        .set(&slot_ref(), &key(SECRET), WriteIntent::Create)
        .map_err(HostError::from);
    push(
        "set exists wire",
        serde_json::to_string(&exists_again).unwrap(),
    );
    push(
        "presence",
        serde_json::to_string(&svc.presence(&[slot_ref()]).unwrap()).unwrap(),
    );
    push("clear", format!("{:?}", svc.clear(&slot_ref())));

    // A writer that puts the secret into its own error text: the renderer never sees it.
    *fake.0.fail_with.lock().unwrap() = Some(SecretStoreError::Failed(format!(
        "write of {SECRET} failed"
    )));
    let failed = svc
        .set(&slot_ref(), &key(SECRET), WriteIntent::Replace)
        .unwrap_err();
    assert_eq!(failed, SecretsError::StoreFailed);
    push(
        "store failure wire",
        serde_json::to_string(&HostError::from(failed)).unwrap(),
    );
    *fake.0.fail_with.lock().unwrap() = Some(SecretStoreError::Unavailable(format!(
        "locked while writing {SECRET}"
    )));
    let unavailable = svc
        .set(&slot_ref(), &key(SECRET), WriteIntent::Replace)
        .unwrap_err();
    assert_eq!(unavailable, SecretsError::StoreUnavailable);
    push(
        "store unavailable wire",
        serde_json::to_string(&HostError::from(unavailable)).unwrap(),
    );
    push("value debug", format!("{:?}", key(SECRET)));

    for answer in &answers {
        assert!(!answer.contains(SECRET), "{answer}");
        assert!(!answer.contains("write of"), "{answer}");
        assert!(!answer.contains("locked while"), "{answer}");
    }
}

// ---- the caller policy in front of both -------------------------------------------------------

struct Rig {
    host: HostServices,
    fake: Fake,
}

fn rig(settings_labels: &[&str], secrets_labels: &[&str]) -> (Rig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let host = HostServices::builder()
        .settings(
            SettingsHostService::new(bridge(dir.path(), schema())),
            ServiceTrust::for_labels(settings_labels.iter().copied()),
        )
        .secrets(
            service(&fake),
            ServiceTrust::for_labels(secrets_labels.iter().copied()),
        )
        .build()
        .unwrap();
    (Rig { host, fake }, dir)
}

struct Caller {
    label: String,
    content: Vec<String>,
    url: Option<tauri::Url>,
    nav: NavPolicy,
}

impl Caller {
    fn new(label: &str, url: &str) -> Self {
        Self {
            label: label.into(),
            content: Vec::new(),
            url: Some(tauri::Url::parse(url).unwrap()),
            nav: NavPolicy::default(),
        }
    }

    fn content_window(mut self) -> Self {
        self.content.push(self.label.clone());
        self
    }

    fn facts(&self) -> CallerFacts<'_> {
        CallerFacts {
            label: &self.label,
            is_app_window: true,
            content_labels: &self.content,
            url: self.url.as_ref(),
            nav: &self.nav,
        }
    }
}

const APP_PAGE: &str = "tauri://localhost/settings.html";

#[test]
fn the_trusted_settings_window_reaches_settings_and_secrets() {
    let (rig, _dir) = rig(&["settings"], &["settings"]);
    let caller = Caller::new("settings", APP_PAGE);
    for action in ["describe", "get", "patch", "reset", "subscribe"] {
        assert!(
            settings_for(&rig.host, action, &caller.facts()).is_ok(),
            "{action}"
        );
    }
    for action in ["presence", "set", "clear"] {
        assert!(
            secrets_for(&rig.host, action, &caller.facts()).is_ok(),
            "{action}"
        );
    }
}

#[test]
fn a_content_window_is_refused_for_every_settings_and_secrets_action() {
    let (rig, _dir) = rig(&["settings", "html-content"], &["settings", "html-content"]);
    // The content webview's label is even listed as trusted: the shell's content registry wins.
    let hostile = Caller::new("html-content", APP_PAGE).content_window();
    for action in ["describe", "get", "patch", "reset", "subscribe"] {
        let e = settings_for(&rig.host, action, &hostile.facts())
            .err()
            .expect(action);
        assert_eq!(e.code, "permission_denied", "{action}");
    }
    for action in ["presence", "set", "clear"] {
        let e = secrets_for(&rig.host, action, &hostile.facts())
            .err()
            .expect(action);
        assert_eq!(e.code, "permission_denied", "{action}");
    }
    assert_eq!(rig.fake.calls(), 0);
}

#[test]
fn a_content_origin_page_in_any_window_is_refused() {
    let (rig, _dir) = rig(&["settings"], &["settings"]);
    for page in [
        "https://evil.example/",
        "http://tauri.localhost.evil.test/",
        "about:blank",
        "blob:https://evil.example/1",
        "data:text/html,<p>x</p>",
        "file:///C:/Users/me/page.html",
    ] {
        let hostile = Caller::new("settings", page);
        assert!(
            settings_for(&rig.host, "patch", &hostile.facts()).is_err(),
            "{page}"
        );
        assert!(
            secrets_for(&rig.host, "set", &hostile.facts()).is_err(),
            "{page}"
        );
        assert!(
            secrets_for(&rig.host, "clear", &hostile.facts()).is_err(),
            "{page}"
        );
    }
    let mut unreadable = Caller::new("settings", APP_PAGE);
    unreadable.url = None;
    assert!(secrets_for(&rig.host, "set", &unreadable.facts()).is_err());
}

#[test]
fn another_app_window_is_not_trusted_just_for_being_the_apps() {
    let (rig, _dir) = rig(&["settings"], &["settings"]);
    let main = Caller::new("main", "tauri://localhost/");
    assert!(settings_for(&rig.host, "get", &main.facts()).is_err());
    assert!(secrets_for(&rig.host, "presence", &main.facts()).is_err());
}

#[test]
fn a_window_that_is_not_an_app_webview_is_refused_before_the_service_rules() {
    let (rig, _dir) = rig(&["settings"], &["settings"]);
    let mut child = Caller::new("settings", APP_PAGE);
    let mut facts = child.facts();
    facts.is_app_window = false;
    assert!(settings_for(&rig.host, "get", &facts).is_err());
    child.label = "other".into();
    assert!(secrets_for(&rig.host, "set", &child.facts()).is_err());
}

#[test]
fn a_secret_is_written_only_from_a_caller_the_settings_service_also_trusts() {
    // "keys" is trusted for secrets alone, "settings" for both.
    let (rig, _dir) = rig(&["settings"], &["settings", "keys"]);
    let keys = Caller::new("keys", APP_PAGE);
    assert!(secrets_for(&rig.host, "presence", &keys.facts()).is_ok());
    assert!(secrets_for(&rig.host, "clear", &keys.facts()).is_ok());
    let refused = secrets_for(&rig.host, "set", &keys.facts())
        .err()
        .expect("set refused");
    assert_eq!(refused.code, "permission_denied");
    let settings = Caller::new("settings", APP_PAGE);
    assert!(secrets_for(&rig.host, "set", &settings.facts()).is_ok());
}

#[test]
fn a_secret_set_is_refused_when_no_settings_service_is_registered() {
    let fake = Fake::default();
    let host = HostServices::builder()
        .secrets(service(&fake), ServiceTrust::for_labels(["settings"]))
        .build()
        .unwrap();
    let caller = Caller::new("settings", APP_PAGE);
    assert!(secrets_for(&host, "set", &caller.facts()).is_err());
    assert!(secrets_for(&host, "presence", &caller.facts()).is_ok());
}

#[test]
fn enabled_actions_narrow_what_a_trusted_window_may_do() {
    let dir = tempfile::tempdir().unwrap();
    let host = HostServices::builder()
        .settings(
            SettingsHostService::new(bridge(dir.path(), schema())),
            ServiceTrust::for_labels(["settings"]).with_actions(["describe", "get"]),
        )
        .build()
        .unwrap();
    let caller = Caller::new("settings", APP_PAGE);
    assert!(settings_for(&host, "get", &caller.facts()).is_ok());
    assert!(settings_for(&host, "patch", &caller.facts()).is_err());
}

#[test]
fn services_the_app_never_registered_refuse_every_caller() {
    let (rig, _dir) = rig(&["settings"], &["settings"]);
    let caller = Caller::new("settings", APP_PAGE);
    for id in [
        ServiceId::LICENSE,
        ServiceId::UPDATE,
        ServiceId::MODELS,
        ServiceId::CRASH,
    ] {
        let e = rig
            .host
            .service_for::<u8>(id, "any", &caller.facts())
            .err()
            .expect("refused");
        assert_eq!(e.code, "permission_denied", "{id}");
    }
}

// ---- no response path carries a secret ---------------------------------------------------------

const SECRETS_SOURCE: &str = include_str!("../src/host/secrets.rs");

/// The text of the declaration that starts at `marker`, up to its first `{`.
fn declaration_head<'a>(source: &'a str, marker: &str) -> &'a str {
    let at = source
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} not found"));
    let head_start = source[..at].rfind("\n\n").map_or(0, |i| i + 2);
    &source[head_start..at + marker.len()]
}

#[test]
fn the_secret_value_type_cannot_be_serialized_cloned_or_displayed() {
    let head = declaration_head(SECRETS_SOURCE, "pub struct SecretValue(String);");
    for forbidden in ["Serialize", "Clone", "Display"] {
        assert!(
            !head.contains(forbidden),
            "SecretValue derives {forbidden}:\n{head}"
        );
    }
    for forbidden in [
        "impl Clone for SecretValue",
        "impl std::fmt::Display for SecretValue",
        "impl serde::Serialize for SecretValue",
        "impl Serialize for SecretValue",
    ] {
        assert!(!SECRETS_SOURCE.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_credential_traits_have_no_read_method() {
    for marker in [
        "pub trait SecretStore: Send + Sync {",
        "pub trait SecretWriter: Send + Sync {",
    ] {
        let at = SECRETS_SOURCE.find(marker).expect(marker);
        let body = &SECRETS_SOURCE[at..];
        let body = &body[..body.find("\n}\n").expect("trait end")];
        for line in body.lines().filter(|l| l.trim_start().starts_with("fn ")) {
            for reader in ["get", "read", "load", "fetch", "export", "reveal", "value"] {
                let name = line.trim_start().trim_start_matches("fn ");
                assert!(
                    !name.starts_with(reader),
                    "{marker} has a reading method: {line}"
                );
            }
        }
        assert!(
            !body.contains("Option<String>") && !body.contains("-> Result<String"),
            "{marker} returns text"
        );
    }
}

#[test]
fn every_secrets_command_answers_a_presence_list_or_nothing() {
    for (command, answer) in [
        (
            "pub async fn rightkit_secrets_presence(",
            "Vec<SecretPresence>",
        ),
        ("pub async fn rightkit_secrets_set(", "()"),
        ("pub async fn rightkit_secrets_clear(", "()"),
    ] {
        let at = SECRETS_SOURCE.find(command).expect(command);
        let rest = &SECRETS_SOURCE[at..];
        let ret = rest.find("-> Result<").expect("return type") + "-> Result<".len();
        let end = rest[ret..].find(", HostError>").expect("error type");
        assert_eq!(&rest[ret..ret + end], answer, "{command}");
    }
}

#[test]
fn the_host_module_has_no_generic_credential_surface() {
    for (name, source) in [
        ("secrets", SECRETS_SOURCE),
        ("settings", include_str!("../src/host/settings.rs")),
        ("mod", include_str!("../src/host/mod.rs")),
    ] {
        for command in source
            .lines()
            .filter(|l| l.contains("pub async fn rightkit_") || l.contains("pub fn rightkit_"))
        {
            for banned in [
                "credential",
                "keychain",
                "export",
                "reveal",
                "header",
                "path",
                "sql",
                "shell",
            ] {
                assert!(!command.contains(banned), "{name}: {command}");
            }
        }
    }
}
