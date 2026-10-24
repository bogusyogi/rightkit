// rightkit-sync 0.2.2 consumed by exact registry pin (default features: helper, process-usage), outside
// the workspace.
//
// Proves the sync engine through its public API against the in-process MemoryBackend and, for the helper
// module, against a real child process: this program, started again with `--fixture-helper`, speaks the
// `rightkit-sync-helper/1` line-JSON protocol over an on-disk zone, which is what the macOS CloudKit
// helper does for an app. The fake-helper binary of the source repository is not part of the published
// package, so the stand-in lives here.
// Covered: SyncEngineBuilder and SyncEngine (run_pass, note_changed, note_deleted, request_full_replay_once,
// ledger, metrics, status), two devices converging through one zone with paging (last-writer-wins, tombstone
// wins and is not re-uploaded, a replacement from another zone), offline queue and partial-batch resume over
// a durable ledger, token expiry, quarantine of undecodable records, inferred local deletion, pass hooks,
// account-generation fencing, the consent gate (Merge and StartFresh, stale answers), conditional saves
// against a concurrent writer (newer remote, older remote, remote tombstone), custom ConflictPolicy,
// deferred applies, paged local listing, FileStorage and LedgerStore, sync_once with SyncBudget and
// SchedulePolicy and the pure scheduling helpers, BackoffPolicy, SyncError, RawRecord, FieldValue, the
// tombstone convention, the wire module (base64, WireRecord, WireError, Request, ok_line, err_line), and the
// helper module (CloudKitHelper, HelperConfig, HelperBackend, HelperAuth, HelperProcess) over the stand-in
// helper process: two engines on one zone, multi-page feed, tags and an asset-sized byte field, a crashed
// helper that is respawned, a throttled helper whose hint beats the base backoff, a signed-out helper, an
// account switch, and a missing helper program.
// Does not prove: the `cipher`, `http` and `drive` features (not enabled; apps use the defaults), the real
// Swift CloudKit helper and real CloudKit (macOS and an iCloud account), Android scheduling, and a ledger
// of the size the runtime budget names (100,000 records).
// State: a per-process directory under the OS temp directory, removed at the end; one short-lived child
// process (this program) per simulated device. No network, no credentials, no real user data.
use rightkit_sync::helper::{
    CloudKitHelper, HelperAuth, HelperBackend, HelperConfig, HelperProcess,
};
use rightkit_sync::wire::{
    self, b64_decode, b64_encode, err_line, ok_line, Request, Response, WireError, WireRecord,
    WireValue, PROTOCOL_VERSION,
};
use rightkit_sync::{
    field, next_delay, read_tombstone, should_continue, sync_once, work_outcome, write_tombstone,
    AlwaysAvailable, ApplyOutcome, AuthProvider, Backend, BackoffPolicy, ChangePage, Clock,
    ConflictPolicy, ConsentChoice, ConsentRequest, Extras, FieldValue, FileStorage, ItemHeader,
    LastWriterWins, Ledger, LedgerStorage, LedgerStore, LocalStore, MemoryBackend, MemoryStorage,
    NoHooks, NullObserver, PassHooks, PassOutcome, PassReport, RawRecord, RecordMapper,
    RecordOutcome, RemoteVersion, SavePolicy, SchedulePolicy, SyncBudget, SyncConfig, SyncEngine,
    SyncEngineBuilder, SyncError, SyncMetrics, SyncObserver, SyncReport, SyncResult, SystemClock,
    WorkOutcome,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---- the domain the engine syncs --------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Note {
    id: String,
    text: String,
    tags: Vec<String>,
    body: Vec<u8>,
    created: i64,
    updated: i64,
}

struct NoteMapper;

impl RecordMapper for NoteMapper {
    type Item = Note;
    fn record_type(&self) -> &str {
        "Note"
    }
    fn header(&self, n: &Note) -> ItemHeader {
        ItemHeader {
            id: n.id.clone(),
            created_at_ms: n.created,
            updated_at_ms: n.updated,
            origin_device: None,
        }
    }
    fn encode(&self, n: &Note, r: &mut RawRecord) {
        r.set("text", FieldValue::Text(n.text.clone()));
        r.set("tags", FieldValue::Strings(n.tags.clone()));
        r.set("body", FieldValue::Bytes(n.body.clone()));
    }
    fn decode(&self, r: &RawRecord) -> Result<Note, String> {
        Ok(Note {
            id: r.text(field::ID).ok_or("no id")?,
            text: r.text("text").ok_or("no text")?,
            tags: r
                .get("tags")
                .and_then(FieldValue::as_strings)
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            body: match r.get("body") {
                Some(FieldValue::Bytes(b)) => b.clone(),
                _ => Vec::new(),
            },
            created: r.date_ms(field::CREATED_AT).ok_or("no createdAt")?,
            updated: r.date_ms(field::UPDATED_AT).ok_or("no updatedAt")?,
        })
    }
}

#[derive(Default, Clone)]
struct Local(Arc<Mutex<BTreeMap<String, Note>>>);

impl Local {
    fn put(&self, id: &str, text: &str, created: i64, updated: i64) {
        self.put_full(id, text, vec![], vec![], created, updated);
    }
    fn put_full(&self, id: &str, text: &str, tags: Vec<String>, body: Vec<u8>, c: i64, u: i64) {
        self.0.lock().unwrap().insert(
            id.into(),
            Note {
                id: id.into(),
                text: text.into(),
                tags,
                body,
                created: c,
                updated: u,
            },
        );
    }
    fn get(&self, id: &str) -> Option<Note> {
        self.0.lock().unwrap().get(id).cloned()
    }
    fn text(&self, id: &str) -> Option<String> {
        self.get(id).map(|n| n.text)
    }
    fn delete(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    fn ids(&self) -> Vec<String> {
        self.0.lock().unwrap().keys().cloned().collect()
    }
}

impl LocalStore<Note> for Local {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        let mut m = self.0.lock().unwrap();
        for n in items {
            m.insert(n.id.clone(), n.clone());
        }
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}

/// The same store, but listing in pages and counting its calls.
struct Paged {
    inner: Local,
    calls: AtomicUsize,
    stuck: bool,
}

impl LocalStore<Note> for Paged {
    fn all(&self) -> SyncResult<Vec<Note>> {
        self.inner.all()
    }
    fn list_page(&self, after: Option<&str>, limit: usize) -> SyncResult<Option<Vec<Note>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let all = self.inner.all()?;
        if self.stuck {
            // A store whose pages never advance past their cursor.
            return Ok(Some(all.into_iter().take(limit).collect()));
        }
        Ok(Some(
            all.into_iter()
                .filter(|n| after.is_none_or(|a| n.id.as_str() > a))
                .take(limit)
                .collect(),
        ))
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.inner.remove(id)
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        self.inner.apply_remote(items)
    }
}

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<(String, String)>>);

impl SyncObserver for Events {
    fn event(&self, name: &'static str, detail: &str) {
        self.0
            .lock()
            .unwrap()
            .push((name.to_string(), detail.to_string()));
    }
}

impl Events {
    fn count(&self, name: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .count()
    }
    fn names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }
}

struct Device {
    local: Local,
    engine: SyncEngine<NoteMapper>,
    storage: MemoryStorage,
    clock: Arc<TestClock>,
    events: Arc<Events>,
}

fn new_clock() -> Arc<TestClock> {
    Arc::new(TestClock(AtomicI64::new(1_000_000)))
}

fn engine_over(
    backend: Arc<dyn Backend>,
    local: Arc<dyn LocalStore<Note>>,
    storage: Box<dyn LedgerStorage>,
    clock: &Arc<TestClock>,
    events: &Arc<Events>,
    config: SyncConfig,
) -> SyncEngine<NoteMapper> {
    SyncEngineBuilder::new(Arc::new(NoteMapper), backend, local, storage)
        .clock(clock.clone())
        .observer(events.clone())
        .config(config)
        .build()
        .expect("engine builds")
}

fn device(zone: &MemoryBackend, infer: bool) -> Device {
    let local = Local::default();
    let storage = MemoryStorage::default();
    let clock = new_clock();
    let events = Arc::new(Events::default());
    let engine = engine_over(
        Arc::new(zone.clone()),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
        &clock,
        &events,
        SyncConfig {
            infer_local_deletions: infer,
            ..SyncConfig::default()
        },
    );
    Device {
        local,
        engine,
        storage,
        clock,
        events,
    }
}

impl Device {
    /// One pass, then past any backoff window so the scenario drives passes explicitly.
    fn pass(&mut self) -> PassReport {
        let report = self.engine.run_pass();
        self.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
        report
    }
    fn stored_ledger(&self) -> Ledger {
        serde_json::from_slice(&self.storage.load().unwrap().unwrap()).unwrap()
    }
}

fn remote_note(zone: &MemoryBackend, id: &str, text: &str, created: i64, updated: i64) {
    let mut r = RawRecord::new("Note", id);
    r.set(field::ID, FieldValue::Text(id.into()));
    r.set("text", FieldValue::Text(text.into()));
    r.set(field::CREATED_AT, FieldValue::Date(created));
    r.set(field::UPDATED_AT, FieldValue::Date(updated));
    zone.put_record(r);
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let root = std::env::temp_dir().join(format!("rk-sync-consumer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create consumer temp dir");
        Scratch(root)
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).expect("create scratch subdirectory");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- pure surface -------------------------------------------------------------

fn backoff_and_errors() {
    let policy = BackoffPolicy::default();
    assert_eq!(
        (policy.base_secs, policy.max_secs, policy.pending_floor_secs),
        (15.0, 900.0, 30.0)
    );
    for (failures, expected) in [
        (0, 15.0),
        (1, 15.0),
        (2, 30.0),
        (3, 60.0),
        (4, 120.0),
        (6, 480.0),
    ] {
        assert_eq!(
            policy.delay_secs(failures, None),
            expected,
            "failure {failures}"
        );
    }
    for failures in [7, 8, 30, u32::MAX] {
        assert_eq!(
            policy.delay_secs(failures, None),
            900.0,
            "capped at {failures}"
        );
    }
    assert_eq!(
        policy.delay_secs(1, Some(120.0)),
        120.0,
        "a larger server hint wins"
    );
    assert_eq!(
        policy.delay_secs(1, Some(16.0)),
        16.0,
        "any hint above the backoff wins, even by a little"
    );
    assert_eq!(
        policy.delay_secs(2, Some(30.0)),
        30.0,
        "a hint equal to the backoff changes nothing"
    );
    assert_eq!(
        policy.delay_secs(4, Some(5.0)),
        120.0,
        "a smaller hint never shortens it"
    );
    assert_eq!(
        policy.delay_secs(9, Some(2000.0)),
        2000.0,
        "a hint is honoured over the cap"
    );
    assert_eq!(policy.delay_secs(1, Some(f64::NAN)), 15.0);
    assert_eq!(policy.delay_secs(1, Some(f64::INFINITY)), 15.0);
    assert_eq!(policy.clone(), policy);

    let retry = SyncError::RetryAfter {
        secs: 12.5,
        detail: "slow down".into(),
    };
    assert_eq!(retry.retry_hint_secs(), Some(12.5));
    for secs in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let odd = SyncError::RetryAfter {
            secs,
            detail: String::new(),
        };
        assert_eq!(odd.retry_hint_secs(), None, "{secs} is not a usable hint");
    }
    assert_eq!(SyncError::Transient("x".into()).retry_hint_secs(), None);
    assert_eq!(SyncError::TokenExpired.retry_hint_secs(), None);
    for (error, text) in [
        (SyncError::Unavailable("signed out".into()), "signed out"),
        (SyncError::Transient("hiccup".into()), "hiccup"),
        (SyncError::Persistence("disk full".into()), "disk full"),
        (retry.clone(), "slow down"),
        (SyncError::TokenExpired, "change token expired"),
        (SyncError::ZoneMissing, "sync zone missing"),
        (SyncError::Cancelled, "sync cancelled"),
        (SyncError::Conflict, "server record changed"),
    ] {
        assert_eq!(error.to_string(), text);
        let as_dyn: &dyn std::error::Error = &error;
        assert_eq!(as_dyn.to_string(), text);
    }
    assert_eq!(SyncError::Conflict.clone(), SyncError::Conflict);
    assert_ne!(SyncError::Conflict, SyncError::Cancelled);
}

fn records_and_tombstones() {
    assert_eq!(
        (
            field::ID,
            field::CREATED_AT,
            field::UPDATED_AT,
            field::DELETED_AT
        ),
        ("id", "createdAt", "updatedAt", "deletedAt")
    );
    let mut record = RawRecord::new("Note", "n1");
    assert_eq!(
        (record.record_type.as_str(), record.record_name.as_str()),
        ("Note", "n1")
    );
    assert!(record.fields.is_empty() && record.change_tag.is_none());
    assert!(record.modified_ms.is_none() && record.created_ms.is_none());
    record.set("text", FieldValue::Text("hello".into()));
    record.set("when", FieldValue::Date(1234));
    record.set("tags", FieldValue::Strings(vec!["a".into(), "b".into()]));
    assert_eq!(record.text("text").as_deref(), Some("hello"));
    assert_eq!(record.date_ms("when"), Some(1234));
    assert_eq!(record.text("when"), None, "a date is not text");
    assert_eq!(record.date_ms("text"), None);
    assert!(record.get("missing").is_none());
    record.set_opt("text", None);
    assert!(
        record.get("text").is_none(),
        "set_opt(None) clears the field"
    );
    record.set_opt("text", Some(FieldValue::Text("again".into())));
    assert_eq!(record.text("text").as_deref(), Some("again"));
    assert_eq!(record.clone(), record);

    assert_eq!(FieldValue::Text("t".into()).as_str(), Some("t"));
    assert_eq!(FieldValue::I64(1).as_str(), None);
    assert_eq!(FieldValue::Date(7).as_date_ms(), Some(7));
    assert_eq!(FieldValue::I64(7).as_date_ms(), None);
    assert_eq!(
        FieldValue::Strings(vec!["x".into()]).as_strings(),
        Some(&["x".to_string()][..])
    );
    assert_eq!(FieldValue::Text("x".into()).as_strings(), None);
    assert_eq!(FieldValue::F64(1.5).as_f64(), Some(1.5));
    assert_eq!(FieldValue::I64(3).as_f64(), Some(3.0));
    assert_eq!(FieldValue::Text("1".into()).as_f64(), None);
    assert_eq!(
        FieldValue::Text("inline".into())
            .as_text_or_bytes()
            .as_deref(),
        Some("inline")
    );
    assert_eq!(
        FieldValue::Bytes("asset".as_bytes().to_vec())
            .as_text_or_bytes()
            .as_deref(),
        Some("asset")
    );
    assert_eq!(
        FieldValue::Bytes(vec![0xff, 0xfe]).as_text_or_bytes(),
        None,
        "invalid UTF-8 is not text"
    );
    assert_eq!(FieldValue::Date(1).as_text_or_bytes(), None);

    // The tombstone convention: id, updatedAt and deletedAt stay, everything else goes except `keep`.
    let mut live = RawRecord::new("Note", "n1");
    live.set(field::ID, FieldValue::Text("n1".into()));
    live.set("text", FieldValue::Text("secret".into()));
    live.set("folder", FieldValue::Text("work".into()));
    live.set(field::CREATED_AT, FieldValue::Date(10));
    assert_eq!(
        read_tombstone(&live),
        None,
        "a live record is not a tombstone"
    );
    write_tombstone(&mut live, "n1", 99, &["folder"]);
    assert_eq!(live.text("text"), None, "content is cleared");
    assert_eq!(
        live.text("folder").as_deref(),
        Some("work"),
        "a kept field survives"
    );
    assert_eq!(
        live.date_ms(field::CREATED_AT),
        None,
        "createdAt is cleared unless kept"
    );
    assert_eq!(live.date_ms(field::UPDATED_AT), Some(99));
    assert_eq!(live.date_ms(field::DELETED_AT), Some(99));
    assert_eq!(read_tombstone(&live), Some(("n1".to_string(), 99)));
    let mut legacy = RawRecord::new("Note", "record-name");
    legacy.set(field::DELETED_AT, FieldValue::Date(5));
    assert_eq!(
        read_tombstone(&legacy),
        Some(("record-name".to_string(), 5)),
        "a legacy tombstone without an id falls back to the record name"
    );
}

fn schedule_helpers() {
    let policy = SchedulePolicy::default();
    assert_eq!(policy.backoff, BackoffPolicy::default());
    assert_eq!(
        (policy.idle_secs, policy.min_secs, policy.cancelled_secs),
        (900.0, 15.0, 30.0)
    );
    let budget = SyncBudget::default();
    assert_eq!(budget.max_passes, 1);
    assert_eq!(budget.max_elapsed, Duration::from_secs(60));
    assert_eq!(budget.max_inline_delay_secs, 0.0);
    let config = SyncConfig::default();
    assert_eq!((config.batch_size, config.tombstone_batch), (200, 50));
    assert!(config.ordered_new_publication);
    assert!(!config.infer_local_deletions && !config.require_consent_on_scope_change);
    assert_eq!(config.backoff, BackoffPolicy::default());

    let consent = ConsentRequest {
        from: "a".into(),
        to: "b".into(),
    };
    let pending = PassOutcome::Pending("queued".into());
    let failed = PassOutcome::Failed("boom".into());
    // next_delay
    assert_eq!(
        next_delay(&PassOutcome::Synced, None, 0, &policy),
        Some(900.0)
    );
    assert_eq!(next_delay(&PassOutcome::Disabled, None, 0, &policy), None);
    assert_eq!(
        next_delay(
            &PassOutcome::ConsentRequired(consent.clone()),
            Some(60.0),
            0,
            &policy
        ),
        None
    );
    assert_eq!(
        next_delay(&pending, Some(45.0), 0, &policy),
        Some(45.0),
        "the engine figure"
    );
    assert_eq!(
        next_delay(&pending, None, 0, &policy),
        Some(30.0),
        "else the pending floor"
    );
    assert_eq!(
        next_delay(&PassOutcome::Deferred { secs: 100.0 }, None, 0, &policy),
        Some(100.0)
    );
    assert_eq!(
        next_delay(&PassOutcome::Deferred { secs: 2.0 }, None, 0, &policy),
        Some(15.0),
        "never below the minimum"
    );
    assert_eq!(
        next_delay(&PassOutcome::Deferred { secs: f64::NAN }, None, 0, &policy),
        Some(15.0)
    );
    assert_eq!(
        next_delay(&PassOutcome::Cancelled, None, 0, &policy),
        Some(30.0)
    );
    assert_eq!(next_delay(&failed, Some(600.0), 2, &policy), Some(600.0));
    for (failures, expected) in [(0, 15.0), (1, 15.0), (2, 30.0), (4, 120.0), (50, 900.0)] {
        assert_eq!(
            next_delay(&failed, None, failures, &policy),
            Some(expected),
            "{failures} failures"
        );
    }
    for engine in [f64::INFINITY, -5.0, 0.0] {
        assert_eq!(
            next_delay(&failed, Some(engine), 2, &policy),
            Some(30.0),
            "ignored {engine}"
        );
    }
    let lifted = SchedulePolicy {
        min_secs: 120.0,
        ..SchedulePolicy::default()
    };
    assert_eq!(
        next_delay(&PassOutcome::Synced, None, 0, &lifted),
        Some(900.0)
    );
    assert_eq!(
        next_delay(&pending, Some(45.0), 0, &lifted),
        Some(120.0),
        "a larger minimum lifts every delay"
    );
    // work_outcome
    assert_eq!(work_outcome(&PassOutcome::Synced), WorkOutcome::Success);
    for retry in [
        pending.clone(),
        PassOutcome::Deferred { secs: 1.0 },
        PassOutcome::Unavailable("signed out".into()),
        PassOutcome::Cancelled,
        failed.clone(),
    ] {
        assert_eq!(work_outcome(&retry), WorkOutcome::Retry, "{retry:?}");
    }
    assert_eq!(work_outcome(&PassOutcome::Disabled), WorkOutcome::Failure);
    assert_eq!(
        work_outcome(&PassOutcome::ConsentRequired(consent)),
        WorkOutcome::Failure
    );
    // should_continue
    let drain = SyncBudget {
        max_passes: 3,
        max_elapsed: Duration::from_secs(10),
        max_inline_delay_secs: 1.0,
    };
    let go = |outcome: &PassOutcome, hint: Option<f64>, progress: usize, run: u32, elapsed: u64| {
        should_continue(
            outcome,
            hint,
            progress,
            run,
            3,
            Duration::from_secs(elapsed),
            &drain,
        )
    };
    assert!(
        go(&pending, Some(0.5), 2, 1, 1),
        "pending with progress and a short wait"
    );
    assert!(go(&pending, None, 2, 1, 1), "pending with no wait at all");
    assert!(
        !go(&pending, Some(30.0), 2, 1, 1),
        "the engine wants a longer wait than the budget allows"
    );
    assert!(!go(&pending, Some(0.5), 0, 1, 1), "no progress");
    assert!(!go(&pending, Some(0.5), 2, 3, 1), "passes used up");
    assert!(!go(&pending, Some(0.5), 2, 1, 10), "time used up");
    assert!(!go(&PassOutcome::Synced, None, 2, 1, 0));
    assert!(!go(&failed, None, 2, 1, 0));
    assert!(!go(&pending, Some(f64::NAN), 2, 1, 1));
    assert!(
        !should_continue(
            &pending,
            Some(30.0),
            5,
            1,
            1,
            Duration::ZERO,
            &SyncBudget::default()
        ),
        "the default budget is one pass"
    );
}

fn wire_format() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(wire::PROTOCOL_VERSION, 1);
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(b64_encode(plain.as_bytes()), encoded);
        assert_eq!(b64_decode(encoded).unwrap(), plain.as_bytes());
    }
    assert_eq!(
        b64_decode("Zm9v\nYmFy\r\n").unwrap(),
        b"foobar",
        "line breaks and spaces are skipped"
    );
    assert_eq!(b64_decode("Zm 9v").unwrap(), b"foo");
    assert_eq!(
        b64_decode("Zg==Zm9v").unwrap(),
        b"f",
        "decoding stops at padding"
    );
    assert_eq!(b64_decode("Zm9!").unwrap_err(), "invalid base64 byte 0x21");
    let every: Vec<u8> = (0..=255u8).collect();
    assert_eq!(b64_decode(&b64_encode(&every)).unwrap(), every);

    // A record crosses the wire with every field kind.
    let mut record = RawRecord::new("Note", "n1");
    record.set("text", FieldValue::Text("hi".into()));
    record.set("count", FieldValue::I64(-4));
    record.set("ratio", FieldValue::F64(0.25));
    record.set("at", FieldValue::Date(1_700_000_000_000));
    record.set("body", FieldValue::Bytes(vec![0, 1, 2]));
    record.set("tags", FieldValue::Strings(vec!["a".into(), "b".into()]));
    record.change_tag = Some("tag-9".into());
    record.modified_ms = Some(5);
    let wired = WireRecord::from(&record);
    let json = serde_json::to_value(&wired).unwrap();
    assert_eq!(
        json,
        json!({
            "type": "Note", "name": "n1",
            "fields": {
                "text": {"t": "text", "v": "hi"},
                "count": {"t": "i64", "v": -4},
                "ratio": {"t": "f64", "v": 0.25},
                "at": {"t": "date", "v": 1_700_000_000_000i64},
                "body": {"t": "bytes", "v": "AAEC"},
                "tags": {"t": "strings", "v": ["a", "b"]}
            },
            "modified_ms": 5, "change_tag": "tag-9"
        })
    );
    let back: WireRecord = serde_json::from_value(json).unwrap();
    assert_eq!(back, wired);
    assert_eq!(back.into_raw().unwrap(), record, "the round trip is exact");
    let broken = WireRecord {
        record_type: "Note".into(),
        name: "bad".into(),
        fields: BTreeMap::from([("body".to_string(), WireValue::Bytes("Zm9!".into()))]),
        modified_ms: None,
        created_ms: None,
        change_tag: None,
    };
    assert!(
        broken.into_raw().unwrap_err().contains("body"),
        "the field is named"
    );

    // Error kinds map to engine errors and back.
    let map =
        |kind: &str, hint: Option<f64>| WireError::new(kind, "detail", hint).into_sync_error();
    for kind in [
        "unavailable",
        "not_authenticated",
        "permission_denied",
        "quota_exceeded",
    ] {
        assert_eq!(
            map(kind, None),
            SyncError::Unavailable("detail".into()),
            "{kind}"
        );
    }
    assert_eq!(map("token_expired", None), SyncError::TokenExpired);
    assert_eq!(map("zone_missing", None), SyncError::ZoneMissing);
    assert_eq!(map("scope_changed", None), SyncError::Cancelled);
    assert_eq!(map("conflict", None), SyncError::Conflict);
    assert_eq!(
        map("network", None),
        SyncError::RetryAfter {
            secs: 15.0,
            detail: "detail".into()
        }
    );
    assert_eq!(
        map("busy", None),
        SyncError::RetryAfter {
            secs: 30.0,
            detail: "detail".into()
        }
    );
    assert_eq!(
        map("retry_after", None),
        SyncError::RetryAfter {
            secs: 30.0,
            detail: "detail".into()
        }
    );
    assert_eq!(
        map("busy", Some(90.0)),
        SyncError::RetryAfter {
            secs: 90.0,
            detail: "detail".into()
        },
        "a server hint wins"
    );
    assert_eq!(
        map("something-new", None),
        SyncError::Transient("detail".into()),
        "unknown kinds are transient"
    );
    assert_eq!(
        map("something-new", Some(7.0)),
        SyncError::RetryAfter {
            secs: 7.0,
            detail: "detail".into()
        }
    );
    assert_eq!(
        map("something-new", Some(f64::NAN)),
        SyncError::Transient("detail".into())
    );
    for (error, kind) in [
        (SyncError::Unavailable("u".into()), "unavailable"),
        (SyncError::TokenExpired, "token_expired"),
        (SyncError::ZoneMissing, "zone_missing"),
        (SyncError::Cancelled, "scope_changed"),
        (SyncError::Conflict, "conflict"),
        (SyncError::Persistence("p".into()), "transient"),
        (SyncError::Transient("t".into()), "transient"),
    ] {
        assert_eq!(WireError::from_sync_error(&error).kind, kind, "{error:?}");
    }
    let sent = WireError::from_sync_error(&SyncError::RetryAfter {
        secs: 20.0,
        detail: "wait".into(),
    });
    assert_eq!(
        (sent.kind.as_str(), sent.retry_after_secs),
        ("retry_after", Some(20.0))
    );

    // Request and response lines.
    let request: Request = serde_json::from_str(
        r#"{"v":1,"id":7,"cmd":"fetch_changes","since":null,"expected_scope":"s"}"#,
    )
    .unwrap();
    assert_eq!(
        (request.v, request.id, request.cmd.as_str()),
        (1, 7, "fetch_changes")
    );
    assert_eq!(request.params["expected_scope"], "s");
    assert!(request.params.contains_key("since") && !request.params.contains_key("cmd"));
    let ok: Value = serde_json::from_str(&ok_line(
        7,
        Map::from_iter([("scope".to_string(), json!("s"))]),
    ))
    .unwrap();
    assert_eq!(ok, json!({"id": 7, "ok": true, "scope": "s"}));
    let refusal: Value =
        serde_json::from_str(&err_line(8, &WireError::new("busy", "slow", Some(12.0)))).unwrap();
    assert_eq!(
        refusal,
        json!({"id": 8, "ok": false, "error": {"kind": "busy", "message": "slow", "retry_after_secs": 12.0}})
    );
    let refusal_plain: Value =
        serde_json::from_str(&err_line(9, &WireError::new("network", "down", None))).unwrap();
    assert!(refusal_plain["error"].get("retry_after_secs").is_none());
    let response: Response = serde_json::from_str(&ok_line(
        3,
        Map::from_iter([("records".to_string(), json!([]))]),
    ))
    .unwrap();
    assert!(response.ok && response.id == 3 && response.error.is_none());
    assert_eq!(response.rest["records"], json!([]));
    let failed: Response =
        serde_json::from_str(&err_line(4, &WireError::new("zone_missing", "gone", None))).unwrap();
    assert!(!failed.ok);
    assert_eq!(failed.error.unwrap().kind, "zone_missing");
}

// ---- the engine over the in-process zone -----------------------------------

fn two_devices_converge() {
    let zone = MemoryBackend::new();
    zone.set_page_size(2); // force multi-page delta fetches
    let (mut a, mut b) = (device(&zone, false), device(&zone, false));
    for i in 0..5 {
        a.local.put(&format!("n{i}"), "v1", 100 + i, 100 + i);
    }
    let first = a.pass();
    assert_eq!(first.outcome, PassOutcome::Synced);
    assert_eq!(first.uploaded, 5);
    assert_eq!(zone.record_count(), 5);
    let pulled = b.pass();
    assert_eq!(
        (pulled.outcome.clone(), pulled.downloaded),
        (PassOutcome::Synced, 5)
    );
    assert_eq!(b.local.ids(), ["n0", "n1", "n2", "n3", "n4"]);
    assert_eq!(b.engine.ledger().remote_versions.len(), 5);
    assert!(b.engine.ledger().has_synced && b.engine.ledger().change_token.is_some());

    // A concurrent edit of n0: B is newer, last-writer-wins picks B on both devices.
    a.local.put("n0", "from-a", 100, 500);
    b.local.put("n0", "from-b", 100, 600);
    a.pass();
    b.pass();
    a.pass();
    assert_eq!(a.local.text("n0").as_deref(), Some("from-b"));
    assert_eq!(b.local.text("n0").as_deref(), Some("from-b"));

    // A deletes n1 while B concurrently edits it: the tombstone wins and is not uploaded again.
    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    b.local.put("n1", "late-edit", 101, 9_999);
    a.pass();
    let before = zone.saved_records();
    let after_delete = b.pass();
    assert!(
        b.local.get("n1").is_none(),
        "the tombstone removed the late edit"
    );
    assert_eq!(after_delete.tombstones_applied, 1);
    assert_eq!(
        zone.saved_records(),
        before,
        "a blocked id is not uploaded again"
    );
    let tomb = zone.record("n1").expect("tombstone in the zone");
    assert!(tomb.date_ms(field::DELETED_AT).is_some());
    assert!(tomb.get("text").is_none(), "a tombstone clears the content");
    assert_eq!(read_tombstone(&tomb).map(|t| t.0).as_deref(), Some("n1"));
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert!(b.engine.ledger().tombstones.contains_key("n1"));

    // Counters after three passes on B: the first download, the edit upload, the tombstone.
    let metrics: SyncMetrics = b.engine.metrics();
    assert_eq!(metrics.passes, 3);
    assert_eq!(metrics.failed_passes, 0);
    assert_eq!((metrics.downloaded_total, metrics.uploaded_total), (5, 1));
    assert_eq!(
        (
            metrics.tombstones_applied_total,
            metrics.tombstones_written_total
        ),
        (1, 0)
    );
    assert_eq!(metrics.tombstones, 1);
    assert_eq!(
        (
            metrics.pending_uploads,
            metrics.pending_tombstones,
            metrics.pending_apply
        ),
        (0, 0, 0)
    );
    assert_eq!(
        b.engine.status().metrics,
        metrics,
        "status carries the same counters"
    );
    let _usage = b.engine.status().process; // present or absent by host; reading it is the point
}

fn offline_queue_partial_batch_and_durable_restart(scratch: &Scratch) {
    let zone = MemoryBackend::new();
    let dir = scratch.dir("ledger");
    let ledger_path = dir.join("nested").join("ledger.json");
    let local = Local::default();
    let clock = new_clock();
    let events = Arc::new(Events::default());
    let build = |local: &Local| {
        engine_over(
            Arc::new(zone.clone()),
            Arc::new(local.clone()),
            Box::new(FileStorage::new(&ledger_path)),
            &clock,
            &events,
            SyncConfig::default(),
        )
    };
    let mut engine = build(&local);
    for i in 0..4 {
        local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }

    // Offline: the pass fails, the backoff window opens, nothing is lost.
    zone.set_offline(true);
    let report = engine.run_pass();
    assert!(
        matches!(report.outcome, PassOutcome::Failed(_)),
        "{:?}",
        report.outcome
    );
    assert_eq!(
        report.retry_after_secs,
        Some(15.0),
        "the backend's hint is honoured"
    );
    assert_eq!(events.count("pass_failed"), 1);
    let deferred = engine.run_pass();
    assert!(
        matches!(deferred.outcome, PassOutcome::Deferred { secs } if secs > 0.0 && secs <= 15.0),
        "{:?}",
        deferred.outcome
    );
    assert_eq!(engine.metrics().failed_passes, 1);
    assert_eq!(engine.ledger().consecutive_failures, 1);
    clock.0.fetch_add(3_600_000, Ordering::SeqCst);

    // Back online, but the zone accepts only two records: a partial batch.
    zone.set_offline(false);
    zone.fail_after_saves(Some(2));
    let report = engine.run_pass();
    clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    assert_eq!(report.uploaded, 2);
    assert!(
        matches!(report.outcome, PassOutcome::Pending(_)),
        "{:?}",
        report.outcome
    );
    assert_eq!(zone.record_count(), 2);
    assert_eq!(
        engine.ledger().remote_versions.len(),
        2,
        "only the acknowledged records are recorded"
    );

    // "Restart": a new engine over the same durable ledger file; only the remainder uploads.
    zone.fail_after_saves(None);
    let saved_before = zone.saved_records();
    let mut restarted = build(&local);
    assert_eq!(
        restarted.ledger().remote_versions.len(),
        2,
        "the acknowledgements survived the restart"
    );
    let report = restarted.run_pass();
    assert_eq!(report.outcome, PassOutcome::Synced);
    assert_eq!(
        zone.saved_records() - saved_before,
        2,
        "only the unacknowledged remainder uploads"
    );
    assert_eq!(zone.record_count(), 4);
    assert!(
        restarted.ledger().change_token.is_some() && restarted.ledger().pending_uploads.is_empty()
    );

    // The durable file holds the ledger as JSON, replaced atomically and owner-only on Unix.
    let stored: Ledger = serde_json::from_slice(&std::fs::read(&ledger_path).unwrap()).unwrap();
    assert!(
        stored.has_synced && stored.change_token.is_some() && stored.pending_uploads.is_empty()
    );
    assert_eq!(stored.remote_versions.len(), 4);
    let names: Vec<String> = std::fs::read_dir(ledger_path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["ledger.json"], "no temporary file is left behind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ledger_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o077, 0, "the ledger is owner-only, mode {mode:o}");
    }

    // FileStorage on its own: a missing file loads as nothing, a save replaces.
    let alone = FileStorage::new(dir.join("alone").join("l.json"));
    assert_eq!(alone.load().unwrap(), None);
    alone.save(b"one").unwrap();
    alone.save(b"two").unwrap();
    assert_eq!(alone.load().unwrap().as_deref(), Some(&b"two"[..]));

    // A ledger that cannot be decoded restarts empty (a full replay), and says so.
    std::fs::write(&ledger_path, b"not json").unwrap();
    let store = LedgerStore::open(Box::new(FileStorage::new(&ledger_path))).unwrap();
    assert!(store.recovered_from_corruption);
    assert!(!store.ledger().has_synced);
}

fn ledger_store_semantics() {
    let storage = MemoryStorage::default();
    let mut store = LedgerStore::open(Box::new(storage.clone())).unwrap();
    assert!(!store.recovered_from_corruption && storage.load().unwrap().is_none());
    store
        .mutate(|l| {
            l.pending_uploads.insert("a".into());
            l.bury("gone", 7);
            l.change_token = Some(vec![1, 2, 3]);
            l.remote_versions.insert(
                "a".into(),
                RemoteVersion {
                    record_name: "a".into(),
                    updated_at_ms: 4,
                },
            );
        })
        .unwrap();
    assert!(
        storage.load().unwrap().is_some(),
        "a mutation is persisted before it returns"
    );
    assert_eq!(store.ledger().tombstones.get("gone"), Some(&7));
    // bury forgets queued work for the id and blocks it.
    store
        .mutate(|l| {
            l.pending_uploads.insert("gone".into());
            l.bury("gone", 8);
        })
        .unwrap();
    assert!(!store.ledger().pending_uploads.contains("gone"));
    assert_eq!(store.ledger().tombstones.get("gone"), Some(&8));
    let reopened = LedgerStore::open(Box::new(storage.clone())).unwrap();
    assert_eq!(
        reopened.ledger().change_token.as_deref(),
        Some(&[1u8, 2, 3][..])
    );
    assert!(reopened.ledger().pending_uploads.contains("a"));
    assert_eq!(
        reopened.ledger().remote_versions["a"],
        RemoteVersion {
            record_name: "a".into(),
            updated_at_ms: 4
        }
    );

    // A failed save restores the exact prior state.
    struct Full;
    impl LedgerStorage for Full {
        fn load(&self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn save(&self, _: &[u8]) -> Result<(), String> {
            Err("disk full".into())
        }
    }
    let mut failing = LedgerStore::open(Box::new(Full)).unwrap();
    let error = failing.mutate(|l| l.has_synced = true).unwrap_err();
    assert_eq!(error, SyncError::Persistence("disk full".into()));
    assert!(
        !failing.ledger().has_synced,
        "the in-memory change was rolled back"
    );
    let _: Extras = Extras::new();
    let _: BTreeSet<String> = Ledger::default().pending_uploads;
}

fn token_expiry_quarantine_and_replay() {
    let zone = MemoryBackend::new();
    let (mut a, mut b) = (device(&zone, false), device(&zone, false));
    a.local.put("ok", "v", 1, 1);
    a.pass();
    // A malformed record in the zone must not block the page.
    let mut junk = RawRecord::new("Note", "junk");
    junk.set(field::ID, FieldValue::Text("junk".into()));
    zone.put_record(junk);
    b.pass();
    zone.expire_token_once();
    let report = b.pass();
    assert_eq!(report.outcome, PassOutcome::Synced);
    assert_eq!(b.local.len(), 1);
    assert_eq!(
        report.quarantined, 1,
        "the replay sees the junk record again: counted, not fatal"
    );
    assert!(b.engine.ledger().quarantined_total >= 2);

    // One-shot repair: a flagged replay re-reads the whole zone once.
    let downloads = b.engine.metrics().downloaded_total;
    b.engine.request_full_replay_once("repair-1");
    let replay = b.pass();
    assert_eq!(replay.outcome, PassOutcome::Synced);
    assert!(
        b.engine.ledger().flags.contains("repair-1"),
        "the flag is recorded"
    );
    assert!(b.engine.metrics().downloaded_total >= downloads);
    let quiet = b.pass();
    assert_eq!(
        (quiet.outcome, quiet.uploaded, quiet.downloaded),
        (PassOutcome::Synced, 0, 0)
    );
}

fn inferred_local_deletion() {
    let zone = MemoryBackend::new();
    let (mut a, mut b) = (device(&zone, true), device(&zone, false));
    a.local.put("x", "v", 1, 1);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 1);
    a.local.delete("x"); // the app deleted without telling the engine
    let report = a.pass();
    assert_eq!(report.tombstones_written, 1);
    b.pass();
    assert_eq!(b.local.len(), 0);
    assert_eq!(a.engine.metrics().tombstones_written_total, 1);
}

struct RequestHooks;

impl PassHooks for RequestHooks {
    fn outbound(&self, extras: &mut Extras) -> Vec<RawRecord> {
        if extras.contains_key("announced") {
            return Vec::new();
        }
        extras.insert("announced".into(), json!(true));
        let mut device = RawRecord::new("Device", "device-1");
        device.set("name", FieldValue::Text("fixture".into()));
        vec![device]
    }
    fn on_foreign_records(&self, records: &[RawRecord], extras: &mut Extras) -> Vec<RawRecord> {
        let mut out = Vec::new();
        let answered = extras.entry("answered".into()).or_insert_with(|| json!([]));
        for r in records.iter().filter(|r| r.record_type == "Req") {
            if answered
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == r.record_name.as_str())
            {
                continue;
            }
            answered
                .as_array_mut()
                .unwrap()
                .push(r.record_name.clone().into());
            let mut resp = RawRecord::new("Resp", &format!("resp-{}", r.record_name));
            resp.set("answer", FieldValue::Text("ok".into()));
            out.push(resp);
        }
        out
    }
}

fn pass_hooks_acknowledge_only_after_the_write() {
    let zone = MemoryBackend::new();
    let local = Local::default();
    let storage = MemoryStorage::default();
    let clock = new_clock();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .clock(clock.clone())
    .hooks(Arc::new(RequestHooks))
    .build()
    .unwrap();
    zone.put_record(RawRecord::new("Req", "req-1"));

    // The response write fails: the pass fails, the feed position does not advance, nothing is
    // acknowledged, so the request replays.
    zone.fail_after_saves(Some(0));
    let report = engine.run_pass();
    assert!(
        matches!(report.outcome, PassOutcome::Failed(_)),
        "{:?}",
        report.outcome
    );
    assert!(
        engine.ledger().change_token.is_none(),
        "the feed advanced past an unanswered request"
    );
    assert!(
        !engine.ledger().extras.contains_key("answered"),
        "marked answered without a response"
    );
    assert!(zone.record("resp-req-1").is_none());

    zone.fail_after_saves(None);
    clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    assert_eq!(engine.run_pass().outcome, PassOutcome::Synced);
    let response = zone
        .record("resp-req-1")
        .expect("the response lands after the retry");
    assert_eq!(response.text("answer").as_deref(), Some("ok"));
    assert!(
        zone.record("device-1").is_some(),
        "outbound hook records are written at the start of a pass"
    );
    assert!(engine.ledger().change_token.is_some());
    assert_eq!(engine.ledger().extras["answered"], json!(["req-1"]));

    // The default hooks do nothing.
    let quiet = NoHooks;
    assert!(quiet.outbound(&mut Extras::new()).is_empty());
    assert!(quiet.on_foreign_records(&[], &mut Extras::new()).is_empty());
    quiet.on_account_switch("a", "b");
}

struct Generation(Arc<AtomicU64>);

impl AuthProvider for Generation {
    fn availability(&self) -> SyncResult<()> {
        Ok(())
    }
    fn generation(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A local store that switches the account while the pass is applying downloads.
struct SwitchOnApply(Local, Arc<AtomicU64>);

impl LocalStore<Note> for SwitchOnApply {
    fn all(&self) -> SyncResult<Vec<Note>> {
        self.0.all()
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.0.remove(id)
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.apply_remote(items)
    }
}

fn account_switch_during_io_never_commits_feed_progress() {
    let zone = MemoryBackend::new();
    let mut other = device(&zone, false);
    other.local.put("n1", "hello", 1, 1);
    other.pass();

    let generation = Arc::new(AtomicU64::new(0));
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(SwitchOnApply(Local::default(), generation.clone())),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(Generation(generation.clone())))
    .build()
    .unwrap();
    let report = engine.run_pass();
    assert_eq!(report.outcome, PassOutcome::Cancelled);
    assert!(
        engine.ledger().change_token.is_none(),
        "a stale pass advanced the feed position"
    );
    assert!(!engine.ledger().has_synced);
    assert_eq!(engine.run_pass().outcome, PassOutcome::Synced);
    assert!(engine.ledger().change_token.is_some());

    // Disabling sync is reported, not an error; the always-available provider never disables.
    struct Off;
    impl AuthProvider for Off {
        fn enabled(&self) -> bool {
            false
        }
        fn availability(&self) -> SyncResult<()> {
            Ok(())
        }
    }
    let mut disabled = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(Local::default()),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(Off))
    .build()
    .unwrap();
    assert_eq!(disabled.run_pass().outcome, PassOutcome::Disabled);
    let always = AlwaysAvailable;
    assert!(always.enabled() && always.availability().is_ok());
    assert_eq!(
        (
            always.generation(),
            always.account_scope(),
            always.bearer().unwrap()
        ),
        (0, None, None)
    );

    // A provider that is signed out: Unavailable, with the backoff window opened.
    struct SignedOut;
    impl AuthProvider for SignedOut {
        fn availability(&self) -> SyncResult<()> {
            Err(SyncError::Unavailable("signed out".into()))
        }
    }
    let mut out = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(Local::default()),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(SignedOut))
    .build()
    .unwrap();
    assert_eq!(
        out.run_pass().outcome,
        PassOutcome::Unavailable("signed out".into())
    );
    assert!(matches!(
        out.run_pass().outcome,
        PassOutcome::Deferred { .. }
    ));
    assert_eq!(out.metrics().failed_passes, 1);
}

// ---- consent gate ----------------------------------------------------------

#[derive(Clone, Default)]
struct Account {
    index: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
}

impl Account {
    fn sign_in(&self, index: usize) {
        self.index.store(index, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    fn scope(&self) -> String {
        format!("acct-{}", self.index.load(Ordering::SeqCst))
    }
}

impl AuthProvider for Account {
    fn availability(&self) -> SyncResult<()> {
        Ok(())
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn account_scope(&self) -> Option<String> {
        Some(self.scope())
    }
}

/// Routes every call to the zone of the signed-in account.
struct Zones {
    zones: Vec<MemoryBackend>,
    account: Account,
}

impl Zones {
    fn current(&self) -> &MemoryBackend {
        &self.zones[self.account.index.load(Ordering::SeqCst)]
    }
}

impl Backend for Zones {
    fn prepare(&self) -> SyncResult<()> {
        self.current().prepare()
    }
    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        self.current().fetch_changes(since)
    }
    fn fetch_record(&self, name: &str) -> SyncResult<Option<RawRecord>> {
        self.current().fetch_record(name)
    }
    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        self.current().save_records(records, policy)
    }
}

#[derive(Default)]
struct Switched(Mutex<Vec<(String, String)>>);

impl PassHooks for Switched {
    fn on_account_switch(&self, from: &str, to: &str) {
        self.0.lock().unwrap().push((from.into(), to.into()));
    }
}

struct Rig {
    local: Local,
    storage: MemoryStorage,
    account: Account,
    zones: Vec<MemoryBackend>,
    events: Arc<Events>,
    switched: Arc<Switched>,
    engine: SyncEngine<NoteMapper>,
}

fn rig_engine(r: &Rig, gate: bool) -> SyncEngine<NoteMapper> {
    SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(Zones {
            zones: r.zones.clone(),
            account: r.account.clone(),
        }),
        Arc::new(r.local.clone()),
        Box::new(r.storage.clone()),
    )
    .auth(Arc::new(r.account.clone()))
    .observer(r.events.clone())
    .hooks(r.switched.clone())
    .config(SyncConfig {
        require_consent_on_scope_change: gate,
        ..SyncConfig::default()
    })
    .build()
    .unwrap()
}

/// Account 0 has synced note `a`; account 1 (holding its own note `z`) is then signed in.
fn synced_then_switched(gate: bool) -> Rig {
    let zones = vec![MemoryBackend::new(), MemoryBackend::new()];
    remote_note(&zones[1], "z", "theirs", 5, 5);
    let placeholder = Rig {
        local: Local::default(),
        storage: MemoryStorage::default(),
        account: Account::default(),
        zones,
        events: Arc::new(Events::default()),
        switched: Arc::new(Switched::default()),
        engine: SyncEngineBuilder::new(
            Arc::new(NoteMapper),
            Arc::new(MemoryBackend::new()),
            Arc::new(Local::default()),
            Box::new(MemoryStorage::default()),
        )
        .build()
        .unwrap(),
    };
    let mut rig = placeholder;
    rig.engine = rig_engine(&rig, gate);
    rig.local.put("a", "mine", 10, 10);
    assert_eq!(rig.engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(rig.zones[0].record_count(), 1);
    rig.account.sign_in(1);
    rig
}

fn consent_gate() {
    // Pause: nothing reaches the new account until consent is recorded.
    let mut r = synced_then_switched(true);
    r.local.put("b", "new while switched", 20, 20);
    r.engine.note_changed("b").unwrap();
    let before = (r.zones[1].save_calls(), r.zones[1].record_count());
    let report = r.engine.run_pass();
    let request = ConsentRequest {
        from: "acct-0".into(),
        to: "acct-1".into(),
    };
    assert_eq!(
        report.outcome,
        PassOutcome::ConsentRequired(request.clone())
    );
    assert_eq!((report.uploaded, report.downloaded), (0, 0));
    assert_eq!(
        (r.zones[1].save_calls(), r.zones[1].record_count()),
        before,
        "nothing may reach the new account"
    );
    assert!(r.local.get("z").is_none(), "nothing downloaded either");
    assert_eq!(r.engine.consent_request(), Some(request.clone()));
    assert_eq!(r.engine.ledger().scope.as_deref(), Some("acct-0"));
    assert!(r.switched.0.lock().unwrap().is_empty());
    assert!(matches!(
        r.engine.run_pass().outcome,
        PassOutcome::ConsentRequired(_)
    ));
    assert_eq!(
        r.events.count("consent_required"),
        1,
        "the event is raised once per request"
    );
    let sync_report: SyncReport = sync_once(
        &mut r.engine,
        &SyncBudget::default(),
        &SchedulePolicy::default(),
    );
    assert_eq!(sync_report.consent_required, Some(request.clone()));
    assert_eq!(
        (sync_report.work, sync_report.next_delay_secs),
        (WorkOutcome::Failure, None)
    );
    // The request and the bound identity survive a restart.
    let restarted = rig_engine(&r, true);
    assert_eq!(restarted.ledger().scope.as_deref(), Some("acct-0"));
    assert_eq!(restarted.consent_request(), Some(request));

    // Stale or unsolicited answers are refused.
    assert!(
        r.engine
            .grant_consent("acct-9", ConsentChoice::Merge)
            .is_err(),
        "another account"
    );
    r.account.index.store(0, Ordering::SeqCst);
    assert!(
        r.engine
            .grant_consent("acct-1", ConsentChoice::Merge)
            .is_err(),
        "the account moved on again"
    );
    assert_eq!(r.zones[1].record_count(), 1, "still nothing published");
    // Signing back in to the bound account clears the pause without consent.
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(r.engine.consent_request(), None);

    // Merge publishes local history into the new account.
    let mut r = synced_then_switched(true);
    assert!(
        r.engine
            .grant_consent("acct-1", ConsentChoice::Merge)
            .is_err(),
        "nothing pending before a pass noticed"
    );
    r.engine.run_pass();
    r.engine
        .grant_consent("acct-1", ConsentChoice::Merge)
        .unwrap();
    assert_eq!(r.engine.consent_request(), None);
    assert_eq!(
        *r.switched.0.lock().unwrap(),
        vec![("acct-0".to_string(), "acct-1".to_string())]
    );
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert!(
        r.zones[1].record("a").is_some(),
        "the local note was merged in"
    );
    assert!(
        r.local.get("z").is_some(),
        "the new account's content downloaded"
    );
    assert_eq!(r.engine.ledger().scope.as_deref(), Some("acct-1"));
    assert_eq!(
        r.zones[0].record_count(),
        1,
        "the old account's zone is untouched"
    );

    // StartFresh keeps what exists locally out of the new account, and deletes nothing.
    let mut r = synced_then_switched(true);
    r.engine.run_pass();
    r.engine
        .grant_consent("acct-1", ConsentChoice::StartFresh)
        .unwrap();
    assert!(r.engine.ledger().withheld.contains("a"));
    r.local.put("c", "after consent", 30, 30);
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert!(
        r.zones[1].record("a").is_none(),
        "the pre-existing item is withheld"
    );
    assert!(
        r.zones[1].record("c").is_some(),
        "later items sync normally"
    );
    assert!(r.local.get("a").is_some() && r.local.get("z").is_some());

    // With the gate off (the default) the account change resets the ledger silently.
    let mut r = synced_then_switched(false);
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert!(r.zones[1].record("a").is_some());
    assert_eq!(r.events.count("consent_required"), 0);
    assert_eq!(ConsentChoice::Merge, ConsentChoice::Merge);
    assert_ne!(ConsentChoice::Merge, ConsentChoice::StartFresh);
}

// ---- conditional saves, policies, deferred applies, paged listing -----------

fn concurrent_writers_never_lose_an_update() {
    let zone = MemoryBackend::new();
    let (mut a, mut b) = (device(&zone, false), device(&zone, false));
    for n in ["n0", "n1", "n2"] {
        a.local.put(n, "v1", 100, 100);
    }
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 3);
    let live = |id: &str, text: &str, updated: i64| {
        let mut r = RawRecord::new("Note", id);
        r.set(field::ID, FieldValue::Text(id.into()));
        r.set("text", FieldValue::Text(text.into()));
        r.set(field::CREATED_AT, FieldValue::Date(100));
        r.set(field::UPDATED_AT, FieldValue::Date(updated));
        r
    };

    // 1. Another device writes a NEWER version between B's fetch and B's save: B's conditional save
    //    is refused, B reads the server record, the remote wins, and nothing is overwritten.
    b.local.put("n0", "from-b", 100, 600);
    zone.concurrent_write_before_next_save(live("n0", "from-a-newer", 900));
    let report = b.pass();
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert_eq!(report.uploaded, 0);
    assert_eq!(
        zone.record("n0").unwrap().text("text").as_deref(),
        Some("from-a-newer"),
        "no lost update"
    );
    assert_eq!(b.local.text("n0").as_deref(), Some("from-a-newer"));
    assert!(b.events.count("save_conflict") >= 1);
    a.pass();
    assert_eq!(a.local.text("n0").as_deref(), Some("from-a-newer"));

    // 2. The concurrent write is OLDER than B's edit: B saves again on top of the server's change tag.
    b.local.put("n1", "from-b-newest", 100, 700);
    zone.concurrent_write_before_next_save(live("n1", "stale-concurrent", 550));
    let report = b.pass();
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert_eq!(report.uploaded, 1);
    assert_eq!(
        zone.record("n1").unwrap().text("text").as_deref(),
        Some("from-b-newest")
    );
    a.pass();
    assert_eq!(a.local.text("n1").as_deref(), Some("from-b-newest"));

    // 3. The concurrent write is a deletion: the tombstone wins over B's edit.
    b.local.put("n2", "edited-while-deleted", 100, 800);
    let mut tombstone = RawRecord::new("Note", "n2");
    write_tombstone(&mut tombstone, "n2", 750, &[]);
    zone.concurrent_write_before_next_save(tombstone);
    let report = b.pass();
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert!(
        b.local.get("n2").is_none(),
        "the tombstone was applied locally"
    );
    assert!(zone
        .record("n2")
        .unwrap()
        .date_ms(field::DELETED_AT)
        .is_some());
    assert!(b.stored_ledger().tombstones.contains_key("n2"));
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
}

/// A policy under which the remote always wins and the local never uploads over it.
struct RemoteAlways;

impl ConflictPolicy<Note> for RemoteAlways {
    fn upload_local_over_remote(&self, _: &ItemHeader, _: &Note, _: &ItemHeader, _: &Note) -> bool {
        false
    }
    fn accept_remote_over_local(&self, _: &ItemHeader, _: &ItemHeader) -> bool {
        true
    }
}

fn conflict_policy_is_pluggable() {
    // Default: last writer wins, ties change nothing.
    let lww = LastWriterWins;
    let h = |updated: i64| ItemHeader {
        id: "x".into(),
        created_at_ms: 1,
        updated_at_ms: updated,
        origin_device: None,
    };
    let note = Note {
        id: "x".into(),
        text: String::new(),
        tags: vec![],
        body: vec![],
        created: 1,
        updated: 1,
    };
    assert!(lww.upload_local_over_remote(&h(2), &note, &h(1), &note));
    assert!(!lww.upload_local_over_remote(&h(1), &note, &h(1), &note));
    assert!(ConflictPolicy::<Note>::accept_remote_over_local(
        &lww,
        &h(1),
        &h(2)
    ));
    assert!(!ConflictPolicy::<Note>::accept_remote_over_local(
        &lww,
        &h(2),
        &h(2)
    ));
    assert!(ConflictPolicy::<Note>::upload_local_over_remote(
        &lww,
        &h(3),
        &note,
        &h(2),
        &note
    ));

    let zone = MemoryBackend::new();
    remote_note(&zone, "n1", "remote", 1, 100);
    let local = Local::default();
    local.put("n1", "local-newer", 1, 500);
    let clock = new_clock();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(local.clone()),
        Box::new(MemoryStorage::default()),
    )
    .clock(clock.clone())
    .policy(Box::new(RemoteAlways))
    .build()
    .unwrap();
    let report = engine.run_pass();
    assert_eq!(report.outcome, PassOutcome::Synced);
    assert_eq!(
        local.text("n1").as_deref(),
        Some("remote"),
        "the custom policy let the remote replace a newer local item"
    );
    assert_eq!(
        zone.record("n1").unwrap().text("text").as_deref(),
        Some("remote")
    );
    assert_eq!(report.uploaded, 0);
}

/// A store that cannot land the first download yet.
struct Deferring {
    inner: Local,
    ready: Arc<AtomicUsize>,
}

impl LocalStore<Note> for Deferring {
    fn all(&self) -> SyncResult<Vec<Note>> {
        self.inner.all()
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.inner.remove(id)
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        if self.ready.load(Ordering::SeqCst) == 0 {
            return Ok(items
                .iter()
                .map(|_| ApplyOutcome::Deferred("dependency missing".into()))
                .collect());
        }
        self.inner.apply_remote(items)
    }
}

fn deferred_applies_are_kept_and_retried() {
    let zone = MemoryBackend::new();
    remote_note(&zone, "n1", "from-remote", 1, 10);
    let inner = Local::default();
    let ready = Arc::new(AtomicUsize::new(0));
    let clock = new_clock();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(Deferring {
            inner: inner.clone(),
            ready: ready.clone(),
        }),
        Box::new(MemoryStorage::default()),
    )
    .clock(clock.clone())
    .build()
    .unwrap();
    let first = engine.run_pass();
    clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    assert!(
        matches!(first.outcome, PassOutcome::Pending(_)),
        "{:?}",
        first.outcome
    );
    assert!(inner.get("n1").is_none());
    assert_eq!(
        engine.metrics().pending_apply,
        1,
        "the raw record is kept durably"
    );
    assert_eq!(engine.ledger().pending_apply.len(), 1);
    // The change feed never replays it, so the engine retries it on every pass until it lands.
    ready.store(1, Ordering::SeqCst);
    let second = engine.run_pass();
    assert_eq!(second.outcome, PassOutcome::Synced, "{second:?}");
    assert_eq!(inner.text("n1").as_deref(), Some("from-remote"));
    assert_eq!(engine.metrics().pending_apply, 0);
}

fn paged_local_listing() {
    // The same results at every page size, and the store is listed in pages.
    let mut reports = Vec::new();
    for page in [1usize, 2, 3, 100] {
        let zone = MemoryBackend::new();
        for i in 0..6 {
            remote_note(&zone, &format!("r{i}"), "remote", i, 100);
        }
        let inner = Local::default();
        for i in 0..7 {
            inner.put(&format!("l{i}"), "local", i, 100);
        }
        inner.put("r2", "local-newer", 2, 500);
        let paged = Arc::new(Paged {
            inner: inner.clone(),
            calls: AtomicUsize::new(0),
            stuck: false,
        });
        let clock = new_clock();
        let events = Arc::new(Events::default());
        let mut engine = SyncEngineBuilder::new(
            Arc::new(NoteMapper),
            Arc::new(zone.clone()),
            paged.clone(),
            Box::new(MemoryStorage::default()),
        )
        .clock(clock.clone())
        .observer(events)
        .local_page_size(page)
        .build()
        .unwrap();
        let report = engine.run_pass();
        assert_eq!(report.outcome, PassOutcome::Synced, "page {page}");
        assert!(
            paged.calls.load(Ordering::SeqCst) >= 7usize.div_ceil(page),
            "page {page}: the store was listed in pages"
        );
        assert_eq!(
            zone.record("r2").unwrap().text("text").as_deref(),
            Some("local-newer")
        );
        assert_eq!(zone.record_count(), 6 + 7, "page {page}");
        assert_eq!(
            inner.len(),
            7 + 6,
            "page {page}: seven local notes plus six remote ones"
        );
        reports.push((report.uploaded, report.downloaded));
    }
    assert!(
        reports.windows(2).all(|w| w[0] == w[1]),
        "every page size gives the same result: {reports:?}"
    );

    // A store whose pages never advance fails the pass instead of looping.
    let zone = MemoryBackend::new();
    let inner = Local::default();
    for i in 0..4 {
        inner.put(&format!("l{i}"), "x", i, i);
    }
    let stuck = Arc::new(Paged {
        inner,
        calls: AtomicUsize::new(0),
        stuck: true,
    });
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone),
        stuck,
        Box::new(MemoryStorage::default()),
    )
    .local_page_size(2)
    .build()
    .unwrap();
    let report = engine.run_pass();
    assert!(
        matches!(&report.outcome, PassOutcome::Failed(why) if why.contains("did not advance")),
        "{:?}",
        report.outcome
    );
}

fn sync_once_reports_what_the_host_needs() {
    let zone = MemoryBackend::new();
    let mut a = device(&zone, false);
    for i in 0..3 {
        a.local.put(&format!("n{i}"), "v", i, i);
    }
    // Default budget: one pass; a clean pass is a success and the next run is the idle period.
    let report = sync_once(
        &mut a.engine,
        &SyncBudget::default(),
        &SchedulePolicy::default(),
    );
    assert_eq!(
        (report.passes, report.uploaded, report.work),
        (1, 3, WorkOutcome::Success)
    );
    assert_eq!(report.outcome, PassOutcome::Synced);
    assert_eq!(report.next_delay_secs, Some(900.0));
    assert!(report.consent_required.is_none());
    assert!(report.elapsed_ms < 60_000);

    // A failure is a retry with the backend's hint; the call never sleeps.
    zone.set_offline(true);
    a.local.put("n9", "v", 9, 9);
    a.engine.note_changed("n9").unwrap();
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    let failed = sync_once(
        &mut a.engine,
        &SyncBudget::default(),
        &SchedulePolicy::default(),
    );
    assert!(matches!(failed.outcome, PassOutcome::Failed(_)));
    assert_eq!(
        (failed.work, failed.next_delay_secs),
        (WorkOutcome::Retry, Some(15.0))
    );
    zone.set_offline(false);

    // The very next call is inside the window: a deferral that ends the call.
    let deferred = sync_once(
        &mut a.engine,
        &SyncBudget {
            max_passes: 5,
            ..SyncBudget::default()
        },
        &SchedulePolicy::default(),
    );
    assert!(
        matches!(deferred.outcome, PassOutcome::Deferred { .. }),
        "{:?}",
        deferred.outcome
    );
    assert_eq!((deferred.passes, deferred.work), (1, WorkOutcome::Retry));

    // A drain loop: pending work with progress and no wait runs another pass inside the budget.
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    zone.fail_after_saves(Some(1));
    for i in 10..13 {
        a.local.put(&format!("n{i}"), "v", i, i);
    }
    let single = sync_once(
        &mut a.engine,
        &SyncBudget::default(),
        &SchedulePolicy::default(),
    );
    assert!(
        matches!(single.outcome, PassOutcome::Pending(_)),
        "{:?}",
        single.outcome
    );
    assert_eq!((single.passes, single.work), (1, WorkOutcome::Retry));
    assert!(single.next_delay_secs.is_some_and(|s| s >= 15.0));
    zone.fail_after_saves(None);
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    let drained = sync_once(
        &mut a.engine,
        &SyncBudget {
            max_passes: 4,
            max_elapsed: Duration::from_secs(30),
            max_inline_delay_secs: 3600.0,
        },
        &SchedulePolicy::default(),
    );
    assert_eq!(drained.outcome, PassOutcome::Synced, "{drained:?}");
    assert_eq!(zone.record_count(), 7, "n0..n2, n9 and n10..n12");
    let _ = (SystemClock.now_ms() > 0, NullObserver);
}

// ---- the helper module over a real child process ----------------------------

/// The flag that turns this program into the stand-in for the CloudKit helper.
const HELPER_FLAG: &str = "--fixture-helper";

fn read_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}

fn write_json(path: &Path, value: &Value) {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec(value).expect("serialize")).expect("write state");
    std::fs::rename(&temp, path).expect("rename state");
}

fn zone_put(zone: &mut Value, record: Value) {
    let seq = zone["seq"].as_u64().unwrap_or(0) + 1;
    let mut record = record;
    record["modified_ms"] = json!(seq);
    record["change_tag"] = json!(seq.to_string());
    let name = record["name"].as_str().unwrap_or_default().to_string();
    zone["seq"] = json!(seq);
    zone["records"][name.as_str()] = json!([seq, record]);
}

/// One request of the `rightkit-sync-helper/1` protocol against the zone files. Faults come from
/// `<zone>.faults.json` and are read on every request, so a scenario can change them between passes.
fn helper_handle(
    zone_file: &Path,
    faults_path: &Path,
    req: &Request,
) -> Result<Map<String, Value>, WireError> {
    let mut faults = read_json(faults_path);
    let account = faults["account"]
        .as_str()
        .unwrap_or("fixture-account-A")
        .to_string();
    let zone_path = PathBuf::from(format!("{}.{account}.json", zone_file.display()));
    let mut zone = read_json(&zone_path);
    let params = Value::Object(req.params.clone());
    let mut body = Map::new();
    body.insert("scope".into(), account.clone().into());

    if req.cmd == "status" {
        match faults["unavailable"].as_str() {
            Some(detail) => {
                body.insert("status".into(), "unavailable".into());
                body.insert("detail".into(), detail.into());
            }
            None => {
                body.insert("status".into(), "ready".into());
            }
        }
        return Ok(body);
    }
    if req.cmd == "shutdown" {
        return Ok(body);
    }
    // Account isolation: a request bound to another account is refused before anything is read or written.
    match params.get("expected_scope").and_then(Value::as_str) {
        None => return Err(WireError::new("transient", "missing expected scope", None)),
        Some(scope) if scope != account => {
            return Err(WireError::new(
                "scope_changed",
                "the account changed during the sync request",
                None,
            ))
        }
        _ => {}
    }
    if let Some(secs) = faults["throttle_secs"].as_f64() {
        return Err(WireError::new("busy", "request rate limited", Some(secs)));
    }
    let exists = zone["exists"].as_bool().unwrap_or(false);
    let result = match req.cmd.as_str() {
        "prepare" => {
            zone["exists"] = json!(true);
            Ok(body)
        }
        "fetch_changes" => {
            faults["fetch_calls"] = json!(faults["fetch_calls"].as_u64().unwrap_or(0) + 1);
            if faults["zone_missing_once"].as_bool().unwrap_or(false) {
                faults["zone_missing_once"] = json!(false);
                write_json(&zone_path, &json!({}));
                write_json(faults_path, &faults);
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let mut after = 0u64;
            if let Some(since) = params.get("since").and_then(Value::as_str) {
                if faults["expire_token_once"].as_bool().unwrap_or(false) {
                    faults["expire_token_once"] = json!(false);
                    write_json(faults_path, &faults);
                    return Err(WireError::new(
                        "token_expired",
                        "change token expired",
                        None,
                    ));
                }
                match <[u8; 8]>::try_from(b64_decode(since).unwrap_or_default().as_slice()) {
                    Ok(bytes) => after = u64::from_le_bytes(bytes),
                    Err(_) => return Err(WireError::new("token_expired", "bad token", None)),
                }
            }
            if !exists {
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let mut changed: Vec<(u64, Value)> = zone["records"]
                .as_object()
                .map(|m| {
                    m.values()
                        .map(|v| (v[0].as_u64().unwrap_or(0), v[1].clone()))
                        .collect()
                })
                .unwrap_or_default();
            changed.retain(|(seq, _)| *seq > after);
            changed.sort_by_key(|(seq, _)| *seq);
            let limit = match faults["page_size"].as_u64() {
                Some(0) | None => usize::MAX,
                Some(n) => n as usize,
            };
            let more = changed.len() > limit;
            changed.truncate(limit);
            let seq = zone["seq"].as_u64().unwrap_or(0);
            let cursor = if more {
                changed.last().map(|(s, _)| *s).unwrap_or(after)
            } else {
                seq
            };
            let deleted: Vec<String> = zone["deletions"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|d| d[0].as_u64().is_some_and(|s| s > after && s <= cursor))
                        .filter_map(|d| d[1].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            body.insert(
                "records".into(),
                Value::Array(changed.into_iter().map(|(_, r)| r).collect()),
            );
            body.insert("deleted".into(), json!(deleted));
            body.insert("token".into(), b64_encode(&cursor.to_le_bytes()).into());
            body.insert("more_coming".into(), more.into());
            body.insert("failed_records".into(), json!([]));
            body.insert("undecodable_records".into(), json!([]));
            Ok(body)
        }
        "fetch_record" => {
            let name = params
                .get("record_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            body.insert("record".into(), zone["records"][name][1].clone());
            Ok(body)
        }
        "save_records" => {
            faults["save_calls"] = json!(faults["save_calls"].as_u64().unwrap_or(0) + 1);
            if !exists {
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let records: Vec<Value> = params["records"].as_array().cloned().unwrap_or_default();
            let conditional =
                params.get("policy").and_then(Value::as_str) == Some("if_server_unchanged");
            let mut outcomes = Vec::new();
            for record in records {
                let name = record["name"].as_str().unwrap_or_default().to_string();
                if conditional {
                    let server = zone["records"][name.as_str()][1].clone();
                    let stale = if server.is_null() {
                        !record["change_tag"].is_null()
                    } else {
                        server["change_tag"] != record["change_tag"]
                    };
                    if stale {
                        let kind = if server.is_null() {
                            "transient"
                        } else {
                            "conflict"
                        };
                        faults["conflicts"] = json!(faults["conflicts"].as_u64().unwrap_or(0) + 1);
                        outcomes.push(json!({
                            "name": name, "ok": false,
                            "error": WireError::new(kind, "server record changed", None),
                            "server_record": server,
                        }));
                        continue;
                    }
                }
                zone_put(&mut zone, record);
                faults["saved_records"] = json!(faults["saved_records"].as_u64().unwrap_or(0) + 1);
                outcomes.push(json!({"name": name, "ok": true}));
            }
            body.insert("outcomes".into(), json!(outcomes));
            Ok(body)
        }
        other => Err(WireError::new(
            "transient",
            format!("unknown command {other}"),
            None,
        )),
    };
    write_json(&zone_path, &zone);
    write_json(faults_path, &faults);
    result
}

fn helper_child(zone_file: PathBuf) {
    let faults_path = PathBuf::from(format!("{}.faults.json", zone_file.display()));
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(
                    out,
                    "{}",
                    err_line(0, &WireError::new("transient", e.to_string(), None))
                );
                let _ = out.flush();
                continue;
            }
        };
        let mut faults = read_json(&faults_path);
        if faults["crash_once"].as_bool().unwrap_or(false) {
            faults["crash_once"] = json!(false);
            write_json(&faults_path, &faults);
            eprintln!("fixture helper: injected crash");
            std::process::exit(1);
        }
        let reply = match helper_handle(&zone_file, &faults_path, &req) {
            Ok(body) => ok_line(req.id, body),
            Err(e) => err_line(req.id, &e),
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            break;
        }
        if req.cmd == "shutdown" {
            break;
        }
    }
}

struct HelperZone {
    dir: PathBuf,
}

impl HelperZone {
    fn file(&self) -> PathBuf {
        self.dir.join("zone")
    }
    fn faults_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.faults.json", self.file().display()))
    }
    fn faults(&self, edit: impl FnOnce(&mut Map<String, Value>)) {
        let mut value = read_json(&self.faults_path());
        edit(value.as_object_mut().expect("faults are an object"));
        write_json(&self.faults_path(), &value);
    }
    fn counter(&self, key: &str) -> u64 {
        read_json(&self.faults_path())[key].as_u64().unwrap_or(0)
    }
    fn records(&self, account: &str) -> BTreeMap<String, Value> {
        let zone = read_json(&PathBuf::from(format!(
            "{}.{account}.json",
            self.file().display()
        )));
        zone["records"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v[1].clone())).collect())
            .unwrap_or_default()
    }
    fn config(&self, timeout: Duration) -> HelperConfig {
        HelperConfig::new(std::env::current_exe().expect("this program's path"))
            .arg(HELPER_FLAG)
            .arg(self.file().display().to_string())
            .timeout(timeout)
    }
}

struct HelperDevice {
    local: Local,
    engine: SyncEngine<NoteMapper>,
    storage: MemoryStorage,
    clock: Arc<TestClock>,
    helper: CloudKitHelper,
    events: Arc<Events>,
}

fn helper_device(zone: &HelperZone, timeout: Duration, config: SyncConfig) -> HelperDevice {
    let events = Arc::new(Events::default());
    let helper = CloudKitHelper::new(zone.config(timeout), events.clone());
    let local = Local::default();
    let storage = MemoryStorage::default();
    let clock = new_clock();
    let backend: HelperBackend = helper.backend();
    let auth: HelperAuth = helper.auth();
    let engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(backend),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .auth(Arc::new(auth))
    .clock(clock.clone())
    .observer(events.clone())
    .config(config)
    .build()
    .unwrap();
    HelperDevice {
        local,
        engine,
        storage,
        clock,
        helper,
        events,
    }
}

impl HelperDevice {
    fn pass(&mut self) -> PassReport {
        let report = self.engine.run_pass();
        self.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
        report
    }
    fn ledger(&self) -> Ledger {
        serde_json::from_slice(&self.storage.load().unwrap().unwrap()).unwrap()
    }
}

fn helper_config_and_process(scratch: &Scratch) {
    let config = HelperConfig::new("prog");
    assert_eq!(config.request_timeout, Duration::from_secs(120));
    assert!(config.args.is_empty() && config.envs.is_empty());
    let config = config
        .arg("one")
        .env("KEY", "VALUE")
        .timeout(Duration::from_secs(5))
        .cloudkit("iCloud.example.fixture", "Items", "items-sub");
    assert_eq!(config.program, PathBuf::from("prog"));
    assert_eq!(
        config.args,
        [
            "one",
            "--container",
            "iCloud.example.fixture",
            "--zone",
            "Items",
            "--subscription",
            "items-sub"
        ]
    );
    assert_eq!(config.envs, [("KEY".to_string(), "VALUE".to_string())]);
    assert_eq!(config.request_timeout, Duration::from_secs(5));
    assert_eq!(config.clone().program, config.program);

    // The raw process: one long-lived child, one response per request, helper errors are data.
    let zone = HelperZone {
        dir: scratch.dir("helper-process"),
    };
    let process = HelperProcess::new(zone.config(Duration::from_secs(30)));
    assert_eq!(
        process.spawn_count(),
        0,
        "nothing starts until the first call"
    );
    let status = process.call("status", Map::new()).expect("status");
    assert!(status.ok && status.error.is_none());
    assert_eq!(status.rest["status"], "ready");
    assert_eq!(status.rest["scope"], "fixture-account-A");
    assert_eq!(process.spawn_count(), 1);
    let unknown = process
        .call(
            "no-such-command",
            Map::from_iter([("expected_scope".to_string(), json!("fixture-account-A"))]),
        )
        .expect("a refusal is a response");
    assert!(!unknown.ok);
    assert_eq!(unknown.error.expect("error").kind, "transient");
    let second = process.call("status", Map::new()).unwrap();
    assert!(second.id > status.id, "request ids count up");
    assert_eq!(process.spawn_count(), 1, "the same child answered");
    process.shutdown();
    process
        .call("status", Map::new())
        .expect("a call after shutdown starts a new child");
    assert_eq!(process.spawn_count(), 2);
    drop(process);
}

fn two_devices_through_helper_processes(scratch: &Scratch) {
    let zone = HelperZone {
        dir: scratch.dir("helper-converge"),
    };
    zone.faults(|f| {
        f.insert("page_size".into(), 2.into());
    });
    let (mut a, mut b) = (
        helper_device(&zone, Duration::from_secs(30), SyncConfig::default()),
        helper_device(&zone, Duration::from_secs(30), SyncConfig::default()),
    );
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    for i in 0..5 {
        a.local.put(&format!("n{i}"), "v1", 100 + i, 100 + i);
    }
    a.local.put_full(
        "rich",
        "has tags and an asset body",
        vec!["alpha".into(), "beta".into()],
        big.clone(),
        90,
        90,
    );
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    let report = b.pass();
    assert_eq!(
        (report.outcome.clone(), report.downloaded),
        (PassOutcome::Synced, 6),
        "{report:?}"
    );
    let rich = b.local.get("rich").expect("the rich note downloaded");
    assert_eq!(rich.tags, ["alpha", "beta"]);
    assert_eq!(
        rich.body, big,
        "an asset-sized byte field survives the wire"
    );
    assert_eq!(
        a.helper.spawn_count(),
        1,
        "one long-lived helper per device"
    );
    assert!(
        zone.counter("fetch_calls") >= 4,
        "page size 2 forced several fetches"
    );
    assert_eq!(
        b.ledger().scope.as_deref(),
        Some("fixture-account-A"),
        "the ledger is bound to the helper's scope"
    );

    // Last-writer-wins across the processes: B is newer.
    a.local.put("n0", "from-a", 100, 500);
    b.local.put("n0", "from-b", 100, 600);
    a.pass();
    b.pass();
    a.pass();
    assert_eq!(a.local.text("n0").as_deref(), Some("from-b"));
    assert_eq!(b.local.text("n0").as_deref(), Some("from-b"));

    // Delete against a concurrent edit: the tombstone wins, clears the content and is not uploaded again.
    a.local.delete("n1");
    a.engine.note_deleted("n1").unwrap();
    b.local.put("n1", "late-edit", 101, 9_999);
    a.pass();
    let saved = zone.counter("saved_records");
    b.pass();
    assert!(b.local.get("n1").is_none());
    assert_eq!(
        zone.counter("saved_records"),
        saved,
        "a blocked id is not uploaded again"
    );
    let tomb = &zone.records("fixture-account-A")["n1"];
    assert!(tomb["fields"].get("deletedAt").is_some());
    assert!(
        tomb["fields"].get("text").is_none(),
        "a tombstone clears the content"
    );
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert_eq!(b.local.len(), 5);

    // A changed record carries its change tag back, and a stale tag is refused with the server record.
    let conflicts = zone.counter("conflicts");
    a.local.put("n2", "a-edit", 102, 7_000);
    a.pass();
    b.local.put("n2", "b-older-edit", 102, 6_000);
    let report = b.pass();
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert_eq!(
        b.local.text("n2").as_deref(),
        Some("a-edit"),
        "the newer edit wins on both sides"
    );
    assert!(
        zone.counter("conflicts") >= conflicts,
        "conflicts are counted by the helper"
    );

    // Shutting the helper down is not an error: the next pass starts a fresh child.
    a.helper.shutdown();
    a.local.put("n9", "after shutdown", 300, 300);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert_eq!(a.helper.spawn_count(), 2);
    assert!(zone.records("fixture-account-A").contains_key("n9"));
}

fn helper_faults_recover(scratch: &Scratch) {
    let zone = HelperZone {
        dir: scratch.dir("helper-faults"),
    };
    let mut a = helper_device(&zone, Duration::from_secs(30), SyncConfig::default());
    a.local.put("n0", "v", 10, 10);

    // A crash mid-pass fails the pass retryably; the next pass starts a new helper and nothing is lost.
    zone.faults(|f| {
        f.insert("crash_once".into(), true.into());
    });
    let report = a.pass();
    assert!(
        matches!(report.outcome, PassOutcome::Failed(_)),
        "{:?}",
        report.outcome
    );
    assert_eq!(a.helper.spawn_count(), 1);
    assert!(a.events.count("pass_failed") >= 1);

    // Throttled: the server hint (77 s) beats the 15 s base backoff.
    zone.faults(|f| {
        f.insert("throttle_secs".into(), 77.0.into());
    });
    let report = a.engine.run_pass();
    assert!(
        matches!(report.outcome, PassOutcome::Failed(_)),
        "{:?}",
        report.outcome
    );
    assert!(report.retry_after_secs.unwrap() >= 77.0, "{report:?}");
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);

    // Signed out: Unavailable with the helper's detail.
    zone.faults(|f| {
        f.remove("throttle_secs");
        f.insert(
            "unavailable".into(),
            "Sign in to iCloud on this Mac.".into(),
        );
    });
    let report = a.pass();
    assert_eq!(
        report.outcome,
        PassOutcome::Unavailable("Sign in to iCloud on this Mac.".into())
    );
    zone.faults(|f| {
        f.remove("unavailable");
    });
    let report = a.pass();
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert!(
        a.helper.spawn_count() >= 2,
        "the crashed helper was replaced"
    );
    assert!(zone.records("fixture-account-A").contains_key("n0"));

    // The user's toggle: disabling stops passes without touching the zone, enabling resumes them.
    let auth = a.helper.auth();
    assert!(auth.enabled());
    auth.set_enabled(false);
    assert!(!auth.enabled());
    a.local.put("n1", "while off", 11, 11);
    assert_eq!(a.pass().outcome, PassOutcome::Disabled);
    assert!(!zone.records("fixture-account-A").contains_key("n1"));
    auth.set_enabled(true);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert!(zone.records("fixture-account-A").contains_key("n1"));

    // A token the zone no longer honours, and a zone that was deleted: replay without losing local data.
    let mut b = helper_device(&zone, Duration::from_secs(30), SyncConfig::default());
    b.pass();
    assert_eq!(b.local.len(), 2);
    zone.faults(|f| {
        f.insert("expire_token_once".into(), true.into());
    });
    assert_eq!(b.pass().outcome, PassOutcome::Synced);
    assert!(b.events.names().contains(&"token_reset".to_string()));
    assert_eq!(b.local.len(), 2);
    zone.faults(|f| {
        f.insert("zone_missing_once".into(), true.into());
    });
    let report = a.pass();
    assert!(
        matches!(
            report.outcome,
            PassOutcome::Synced | PassOutcome::Pending(_)
        ),
        "{report:?}"
    );
    assert!(a.events.names().contains(&"zone_recreated".to_string()));
    assert_eq!(a.local.len(), 2);
    a.pass();
    assert_eq!(
        zone.records("fixture-account-A").len(),
        2,
        "the library was republished"
    );

    // A helper program that does not exist is Unavailable, not a crash.
    let events = Arc::new(Events::default());
    let missing = CloudKitHelper::new(HelperConfig::new(zone.dir.join("no-such-helper")), events);
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(missing.backend()),
        Arc::new(Local::default()),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(missing.auth()))
    .build()
    .unwrap();
    let report = engine.run_pass();
    assert!(
        matches!(&report.outcome, PassOutcome::Unavailable(why) if why.contains("could not start")),
        "{:?}",
        report.outcome
    );
    assert_eq!(missing.spawn_count(), 0);
}

fn helper_account_switch_isolates_zones(scratch: &Scratch) {
    let zone = HelperZone {
        dir: scratch.dir("helper-account"),
    };
    let mut a = helper_device(&zone, Duration::from_secs(30), SyncConfig::default());
    for i in 0..3 {
        a.local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert_eq!(a.ledger().scope.as_deref(), Some("fixture-account-A"));
    assert_eq!(zone.records("fixture-account-A").len(), 3);

    // The user deletes n0 locally, then signs in to another account before the tombstone reached the
    // zone: it must never be written to the new account.
    a.local.delete("n0");
    a.engine.note_deleted("n0").unwrap();
    zone.faults(|f| {
        f.insert("account".into(), "fixture-account-B".into());
    });
    let first = a.pass();
    // The pass that notices the switch is fenced; the next one binds the new account.
    assert!(
        matches!(first.outcome, PassOutcome::Cancelled | PassOutcome::Synced),
        "{first:?}"
    );
    let report = if first.outcome == PassOutcome::Cancelled {
        a.pass()
    } else {
        first
    };
    assert_eq!(report.outcome, PassOutcome::Synced, "{report:?}");
    assert_eq!(a.ledger().scope.as_deref(), Some("fixture-account-B"));
    assert!(a
        .events
        .names()
        .contains(&"account_scope_changed".to_string()));
    let new_zone = zone.records("fixture-account-B");
    assert!(
        !new_zone.contains_key("n0"),
        "the old account's pending deletion must not leak"
    );
    assert_eq!(
        new_zone.len(),
        2,
        "the surviving local items were published to the new account"
    );
    assert!(
        zone.records("fixture-account-A")["n0"]["fields"]
            .get("deletedAt")
            .is_none(),
        "the old zone is untouched"
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some(HELPER_FLAG) {
        helper_child(PathBuf::from(args.next().expect("the zone file path")));
        return;
    }
    let scratch = Scratch::new();
    backoff_and_errors();
    records_and_tombstones();
    schedule_helpers();
    wire_format();
    two_devices_converge();
    offline_queue_partial_batch_and_durable_restart(&scratch);
    ledger_store_semantics();
    token_expiry_quarantine_and_replay();
    inferred_local_deletion();
    pass_hooks_acknowledge_only_after_the_write();
    account_switch_during_io_never_commits_feed_progress();
    consent_gate();
    concurrent_writers_never_lose_an_update();
    conflict_policy_is_pluggable();
    deferred_applies_are_kept_and_retried();
    paged_local_listing();
    sync_once_reports_what_the_host_needs();
    helper_config_and_process(&scratch);
    two_devices_through_helper_processes(&scratch);
    helper_faults_recover(&scratch);
    helper_account_switch_isolates_zones(&scratch);
    println!("OK rightkit-sync 0.2.2 (registry consumer)");
}
