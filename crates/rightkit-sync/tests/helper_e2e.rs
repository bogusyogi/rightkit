//! End-to-end proofs of the helper backend: the real `HelperBackend` /
//! `HelperAuth` talk line-JSON to real `rightkit-sync-fake-helper` child
//! processes that share an on-disk zone, so two engines on two helper
//! processes play two Macs on one iCloud zone. Real CloudKit is the manual
//! gate documented in API.md section 4.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_sync::helper::{CloudKitHelper, HelperConfig};
use rightkit_sync::*;
use serde_json::{json, Value};

const FAKE: &str = env!("CARGO_BIN_EXE_rightkit-sync-fake-helper");

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
            id: r.text("id").ok_or("no id")?,
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
            created: r.date_ms("createdAt").ok_or("no createdAt")?,
            updated: r.date_ms("updatedAt").ok_or("no updatedAt")?,
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
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
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
    fn names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }
}

struct Zone {
    dir: PathBuf,
}
impl Zone {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rk-sync-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn file(&self) -> PathBuf {
        self.dir.join("zone")
    }
    fn faults_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.faults.json", self.file().display()))
    }
    /// Read-modify-write the fault file between requests.
    fn faults(&self, edit: impl FnOnce(&mut serde_json::Map<String, Value>)) {
        let mut v: Value = std::fs::read(self.faults_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({}));
        edit(v.as_object_mut().unwrap());
        std::fs::write(self.faults_path(), serde_json::to_vec(&v).unwrap()).unwrap();
    }
    fn counter(&self, key: &str) -> u64 {
        let v: Value = serde_json::from_slice(&std::fs::read(self.faults_path()).unwrap()).unwrap();
        v[key].as_u64().unwrap_or(0)
    }
    /// Live record names currently in the zone of `account`.
    fn records(&self, account: &str) -> BTreeMap<String, Value> {
        let path = PathBuf::from(format!("{}.{account}.json", self.file().display()));
        let Ok(bytes) = std::fs::read(path) else {
            return BTreeMap::new();
        };
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        v["records"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v[1].clone())).collect())
            .unwrap_or_default()
    }
}
impl Drop for Zone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Device {
    local: Local,
    engine: SyncEngine<NoteMapper>,
    storage: MemoryStorage,
    clock: Arc<TestClock>,
    helper: CloudKitHelper,
    events: Arc<Events>,
}

fn device_with(zone: &Zone, timeout: Duration, infer: bool) -> Device {
    let events = Arc::new(Events::default());
    let cfg = HelperConfig::new(FAKE)
        .arg("--zone-file")
        .arg(zone.file().display().to_string())
        .timeout(timeout);
    let helper = CloudKitHelper::new(cfg, events.clone());
    let local = Local::default();
    let storage = MemoryStorage::default();
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(helper.backend()),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .auth(Arc::new(helper.auth()))
    .clock(clock.clone())
    .observer(events.clone())
    .config(SyncConfig {
        infer_local_deletions: infer,
        ..SyncConfig::default()
    })
    .build()
    .unwrap();
    Device {
        local,
        engine,
        storage,
        clock,
        helper,
        events,
    }
}

fn device(zone: &Zone) -> Device {
    device_with(zone, Duration::from_secs(30), false)
}

impl Device {
    fn pass(&mut self) -> PassReport {
        let r = self.engine.run_pass();
        // Step past any backoff window so tests drive passes explicitly.
        self.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
        r
    }
    fn ledger(&self) -> Ledger {
        serde_json::from_slice(&self.storage.load().unwrap().unwrap()).unwrap()
    }
}

#[test]
fn two_macs_converge_through_helper_processes() {
    let zone = Zone::new("converge");
    zone.faults(|f| {
        f.insert("page_size".into(), 2.into());
    });
    let (mut a, mut b) = (device(&zone), device(&zone));
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
    let rep = b.pass();
    assert_eq!(rep.downloaded, 6, "{rep:?}");
    let rich = b.local.get("rich").expect("rich note downloaded");
    assert_eq!(rich.tags, ["alpha", "beta"]);
    assert_eq!(rich.body, big, "asset-sized bytes survive the wire");
    assert_eq!(
        a.helper.spawn_count(),
        1,
        "one long-lived helper per device"
    );
    assert!(
        zone.counter("fetch_calls") >= 4,
        "page_size 2 forced multi-page fetches"
    );

    // Concurrent edit: B newer. LWW converges both.
    a.local.put("n0", "from-a", 100, 500);
    b.local.put("n0", "from-b", 100, 600);
    a.pass();
    b.pass();
    a.pass();
    assert_eq!(a.local.get("n0").unwrap().text, "from-b");
    assert_eq!(b.local.get("n0").unwrap().text, "from-b");

    // Delete vs concurrent edit: tombstone wins, no re-upload, content cleared.
    a.local.0.lock().unwrap().remove("n1");
    a.engine.note_deleted("n1").unwrap();
    b.local.put("n1", "late-edit", 101, 9_999);
    a.pass();
    let saved = zone.counter("saved_records");
    b.pass();
    assert!(b.local.get("n1").is_none());
    assert_eq!(
        zone.counter("saved_records"),
        saved,
        "blocked id not re-uploaded"
    );
    let tomb = &zone.records("fake-account-A")["n1"];
    assert!(tomb["fields"].get("deletedAt").is_some());
    assert!(
        tomb["fields"].get("text").is_none(),
        "tombstone clears content"
    );
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert_eq!(b.local.len(), 5);
}

#[test]
fn per_record_fetch_failure_never_advances_the_cursor() {
    // H13: the page "succeeds" but one record failed to fetch.
    let zone = Zone::new("h13");
    let (mut a, mut b) = (device(&zone), device(&zone));
    for i in 0..3 {
        a.local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }
    a.pass();
    zone.faults(|f| {
        f.insert("failed_fetch_once".into(), json!(["n1"]));
        f.insert("failed_fetch_retry_after".into(), 42.0.into());
    });
    let rep = b.pass();
    assert!(
        matches!(rep.outcome, PassOutcome::Failed(_)),
        "{:?}",
        rep.outcome
    );
    assert_eq!(
        rep.retry_after_secs,
        Some(42.0),
        "server retry hint honoured"
    );
    assert!(b.ledger().change_token.is_none(), "cursor must not move");
    assert_eq!(b.local.len(), 0, "page not half-applied");
    // Next pass replays the page and lands every record, including n1.
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert_eq!(b.local.len(), 3);
    assert!(b.ledger().change_token.is_some());
}

#[test]
fn account_switch_isolates_ledger_and_pending_deletions() {
    let zone = Zone::new("identity");
    let mut a = device(&zone);
    for i in 0..3 {
        a.local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    let scope_a = a
        .ledger()
        .scope
        .clone()
        .expect("ledger bound to account scope");
    assert_eq!(scope_a, "fake-account-A");
    assert_eq!(zone.records("fake-account-A").len(), 3);

    // User deletes n0 locally, then signs into another iCloud account before
    // the tombstone reached the zone: it must never be written to account B.
    a.local.0.lock().unwrap().remove("n0");
    a.engine.note_deleted("n0").unwrap();
    zone.faults(|f| {
        f.insert("account".into(), "fake-account-B".into());
    });
    let rep = a.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert!(a.events.names().contains(&"account_switched".to_string()));
    assert_eq!(a.ledger().scope.as_deref(), Some("fake-account-B"));
    let b_zone = zone.records("fake-account-B");
    assert!(
        !b_zone.contains_key("n0"),
        "old account's pending deletion must not leak into the new account"
    );
    assert_eq!(
        b_zone.len(),
        2,
        "surviving local items published to the new account"
    );
    assert_eq!(
        zone.records("fake-account-A")["n0"]["fields"].get("deletedAt"),
        None,
        "old account's zone untouched"
    );
}

#[test]
fn account_change_during_a_pass_cancels_without_committing() {
    let zone = Zone::new("fence");
    let mut a = device(&zone);
    a.local.put("n0", "v", 10, 10);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    a.local.put("n1", "v", 11, 11);
    // `prepare` hangs; the account flips while it is in flight.
    zone.faults(|f| {
        f.insert("hang_cmd".into(), "prepare".into());
        f.insert("hang_once_ms".into(), 1500.into());
    });
    let flip = zone.faults_path();
    let flipper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        let mut v: Value = serde_json::from_slice(&std::fs::read(&flip).unwrap()).unwrap();
        v["account"] = "fake-account-B".into();
        std::fs::write(&flip, serde_json::to_vec(&v).unwrap()).unwrap();
    });
    let before = a.ledger().change_token.clone();
    let rep = a.pass();
    flipper.join().unwrap();
    assert_eq!(rep.outcome, PassOutcome::Cancelled, "{rep:?}");
    assert_eq!(
        a.ledger().change_token,
        before,
        "cancelled pass commits no cursor"
    );
    assert!(
        zone.records("fake-account-B").is_empty(),
        "nothing written to the new account"
    );
    assert!(!zone.records("fake-account-A").contains_key("n1"));
}

#[test]
fn helper_crash_timeout_throttle_and_signed_out_recover() {
    let zone = Zone::new("faults");
    let mut a = device_with(&zone, Duration::from_millis(1500), false);
    a.local.put("n0", "v", 10, 10);

    // Crash mid-pass: helper respawns on the next pass, nothing lost.
    zone.faults(|f| {
        f.insert("crash_once".into(), true.into());
    });
    assert!(matches!(a.pass().outcome, PassOutcome::Failed(_)));
    assert_eq!(a.helper.spawn_count(), 1);
    // Hung request: deadline kills the helper and the pass fails retryably.
    zone.faults(|f| {
        f.insert("hang_once_ms".into(), 6000.into());
    });
    let rep = a.pass();
    assert!(
        matches!(rep.outcome, PassOutcome::Failed(_)),
        "{:?}",
        rep.outcome
    );
    assert!(a.helper.spawn_count() >= 2);

    // Throttled: the server hint (77 s) beats our 15 s base backoff.
    zone.faults(|f| {
        f.insert("throttle_secs".into(), 77.0.into());
    });
    let rep = a.engine.run_pass();
    assert!(
        matches!(rep.outcome, PassOutcome::Failed(_)),
        "{:?}",
        rep.outcome
    );
    assert!(rep.retry_after_secs.unwrap() >= 77.0, "{rep:?}");
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    zone.faults(|f| {
        f.remove("throttle_secs");
        f.insert(
            "unavailable".into(),
            "Sign in to iCloud on this Mac.".into(),
        );
    });
    let rep = a.pass();
    assert_eq!(
        rep.outcome,
        PassOutcome::Unavailable("Sign in to iCloud on this Mac.".into())
    );
    zone.faults(|f| {
        f.remove("unavailable");
    });
    let rep = a.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert!(zone.records("fake-account-A").contains_key("n0"));
}

#[test]
fn token_expiry_and_zone_loss_replay_without_losing_local_data() {
    let zone = Zone::new("replay");
    let (mut a, mut b) = (device(&zone), device(&zone));
    for i in 0..3 {
        a.local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 3);

    zone.faults(|f| {
        f.insert("expire_token_once".into(), true.into());
    });
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert!(b.events.names().contains(&"token_reset".to_string()));
    assert_eq!(b.local.len(), 3);

    // iCloud storage reset deletes the zone: engine re-prepares once and the
    // local library re-publishes; nothing local is discarded.
    zone.faults(|f| {
        f.insert("zone_missing_once".into(), true.into());
    });
    let rep = a.pass();
    assert!(
        matches!(rep.outcome, PassOutcome::Synced | PassOutcome::Pending(_)),
        "{rep:?}"
    );
    assert!(a.events.names().contains(&"zone_recreated".to_string()));
    assert_eq!(a.local.len(), 3);
    a.pass();
    assert_eq!(
        zone.records("fake-account-A").len(),
        3,
        "library republished"
    );

    // Permanently undecodable record is reported, not a retry loop.
    zone.faults(|f| {
        f.insert("undecodable".into(), json!(["n2"]));
    });
    a.local.put("n2", "edited", 12, 99_999);
    a.pass();
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert!(b
        .events
        .names()
        .contains(&"records_undecodable".to_string()));
}

#[test]
fn helper_missing_is_unavailable_not_a_crash() {
    let zone = Zone::new("missing");
    let events = Arc::new(Events::default());
    let helper = CloudKitHelper::new(
        HelperConfig::new(zone.dir.join("no-such-helper")),
        events.clone(),
    );
    let local = Local::default();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(helper.backend()),
        Arc::new(local),
        Box::new(MemoryStorage::default()),
    )
    .auth(Arc::new(helper.auth()))
    .build()
    .unwrap();
    let rep = engine.run_pass();
    assert!(
        matches!(rep.outcome, PassOutcome::Unavailable(_)),
        "{:?}",
        rep.outcome
    );
}

/// The real Swift helper (memory backend, no iCloud) must speak the protocol
/// exactly as the fake does: same engine, same contract, different process.
/// Build it first: `swift build -c release --product rightkit-cloudkit-helper` in swift/RightKitSwift
/// (or point RIGHTKIT_CLOUDKIT_HELPER at a built binary). Skipped, loudly,
/// when it is not built.
fn swift_helper() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RIGHTKIT_CLOUDKIT_HELPER") {
        return Some(PathBuf::from(p));
    }
    let base =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../swift/RightKitSwift/.build");
    ["release", "debug"]
        .iter()
        .map(|c| base.join(c).join("rightkit-cloudkit-helper"))
        .find(|p| p.exists())
}

#[test]
fn real_swift_helper_memory_backend_converges_two_engines() {
    let Some(bin) = swift_helper() else {
        eprintln!("SKIPPED: rightkit-cloudkit-helper not built (swift build -c release)");
        return;
    };
    let events = Arc::new(Events::default());
    // One helper process = one in-memory iCloud zone shared by both engines.
    let helper = CloudKitHelper::new(
        HelperConfig::new(bin).arg("--backend").arg("memory"),
        events.clone(),
    );
    let mk = |helper: &CloudKitHelper| {
        let local = Local::default();
        let storage = MemoryStorage::default();
        let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
        let engine = SyncEngineBuilder::new(
            Arc::new(NoteMapper),
            Arc::new(helper.backend()),
            Arc::new(local.clone()),
            Box::new(storage.clone()),
        )
        .auth(Arc::new(helper.auth()))
        .clock(clock.clone())
        .build()
        .unwrap();
        (local, engine, storage, clock)
    };
    let (la, mut ea, sa, ca) = mk(&helper);
    let (lb, mut eb, _sb, cb) = mk(&helper);
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    la.put_full(
        "rich",
        "body+tags",
        vec!["x".into(), "y".into()],
        big.clone(),
        50,
        50,
    );
    la.put("n1", "keep-me", 60, 60);
    la.put("n2", "doomed", 61, 61);
    assert_eq!(ea.run_pass().outcome, PassOutcome::Synced);
    let rep = eb.run_pass();
    assert_eq!(rep.downloaded, 3, "{rep:?}");
    let rich = lb.get("rich").unwrap();
    assert_eq!(rich.tags, ["x", "y"]);
    assert_eq!(
        rich.body, big,
        "bytes cross Swift CKAsset-style mapping intact"
    );
    assert_eq!(rich.created, 50, "createdAt immutable origin time survives");

    // Edit + delete: LWW edit lands, tombstone removes remotely and clears content.
    ca.0.fetch_add(3_600_000, Ordering::SeqCst);
    cb.0.fetch_add(3_600_000, Ordering::SeqCst);
    la.put("n1", "edited-on-a", 60, 700);
    la.0.lock().unwrap().remove("n2");
    ea.note_deleted("n2").unwrap();
    assert_eq!(ea.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(eb.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(lb.get("n1").unwrap().text, "edited-on-a");
    assert!(lb.get("n2").is_none());
    let tomb = helper
        .backend()
        .fetch_record("n2")
        .unwrap()
        .expect("tombstone record");
    assert!(tomb.date_ms("deletedAt").is_some());
    assert!(
        tomb.get("text").is_none(),
        "tombstone cleared the content field: {tomb:?}"
    );

    // Ledgers are bound to the helper-reported scope.
    let ledger: Ledger = serde_json::from_slice(&sa.load().unwrap().unwrap()).unwrap();
    assert_eq!(ledger.scope.as_deref(), Some("in-memory-account"));
    assert_eq!(helper.spawn_count(), 1);
    helper.shutdown();
}

fn wire_note(name: &str, text: &str, created: i64, updated: i64) -> Value {
    json!({"type": "Note", "name": name, "fields": {
        "id": {"t": "text", "v": name},
        "text": {"t": "text", "v": text},
        "createdAt": {"t": "date", "v": created},
        "updatedAt": {"t": "date", "v": updated},
    }})
}

fn wire_tombstone(name: &str, at: i64) -> Value {
    json!({"type": "Note", "name": name, "fields": {
        "id": {"t": "text", "v": name},
        "updatedAt": {"t": "date", "v": at},
        "deletedAt": {"t": "date", "v": at},
    }})
}

fn zone_text(zone: &Zone, name: &str) -> Option<String> {
    zone.records("fake-account-A")
        .get(name)
        .and_then(|r| r["fields"]["text"]["v"].as_str().map(str::to_string))
}

#[test]
fn concurrent_write_between_fetch_and_save_never_loses_an_update() {
    let zone = Zone::new("conditional");
    let (mut a, mut b) = (device(&zone), device(&zone));
    for n in ["n0", "n1", "n2"] {
        a.local.put(n, "v1", 100, 100);
    }
    a.local.put("n9-placeholder", "x", 101, 101);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 4);

    // 1. Another device writes a NEWER version in the window between B's
    // fetch and B's save. B's conditional save is refused, B reads the
    // server record, the policy says the remote wins: B adopts it and the
    // server keeps it (the old blind overwrite would have destroyed it).
    b.local.put("n0", "from-b", 100, 600);
    zone.faults(|f| {
        f.insert(
            "inject_before_save".into(),
            wire_note("n0", "from-a-newer", 100, 900),
        );
    });
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert_eq!(zone.counter("conflicts"), 1);
    assert_eq!(
        zone_text(&zone, "n0").as_deref(),
        Some("from-a-newer"),
        "no lost update"
    );
    assert_eq!(b.local.get("n0").unwrap().text, "from-a-newer");
    assert_eq!(rep.uploaded, 0);
    a.pass();
    assert_eq!(a.local.get("n0").unwrap().text, "from-a-newer");

    // 2. The concurrent write is OLDER than B's edit: B re-saves on top of
    // the server's change tag, and its newer edit survives on both devices.
    b.local.put("n1", "from-b-newest", 100, 700);
    zone.faults(|f| {
        f.insert(
            "inject_before_save".into(),
            wire_note("n1", "stale-concurrent", 100, 550),
        );
    });
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert_eq!(zone.counter("conflicts"), 2);
    assert_eq!(rep.uploaded, 1, "{rep:?}");
    assert_eq!(zone_text(&zone, "n1").as_deref(), Some("from-b-newest"));
    a.pass();
    assert_eq!(a.local.get("n1").unwrap().text, "from-b-newest");

    // 3. The concurrent write is a deletion: tombstone wins over B's edit.
    b.local.put("n2", "edited-while-deleted", 100, 800);
    zone.faults(|f| {
        f.insert("inject_before_save".into(), wire_tombstone("n2", 750));
    });
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert!(b.local.get("n2").is_none(), "tombstone applied locally");
    assert!(zone.records("fake-account-A")["n2"]["fields"]
        .get("deletedAt")
        .is_some());
    assert!(b.ledger().tombstones.contains_key("n2"));

    // 4. First publication of a name another device just created: the insert
    // is refused (record exists), the server record is judged, nothing is lost.
    b.local.put("n7", "b-first", 200, 1_000);
    zone.faults(|f| {
        f.insert(
            "inject_before_save".into(),
            wire_note("n7", "a-first-newer", 200, 2_000),
        );
    });
    let rep = b.pass();
    assert_eq!(rep.outcome, PassOutcome::Synced, "{rep:?}");
    assert_eq!(zone_text(&zone, "n7").as_deref(), Some("a-first-newer"));
    assert_eq!(b.local.get("n7").unwrap().text, "a-first-newer");
    assert_eq!(zone.counter("conflicts"), 4);
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
}

#[test]
fn real_swift_helper_refuses_stale_conditional_save_and_reports_server_record() {
    let Some(bin) = swift_helper() else {
        eprintln!("SKIPPED: rightkit-cloudkit-helper not built (swift build -c release)");
        return;
    };
    let helper = CloudKitHelper::new(
        HelperConfig::new(bin).arg("--backend").arg("memory"),
        Arc::new(Events::default()),
    );
    helper.auth().availability().unwrap();
    let backend = helper.backend();
    backend.prepare().unwrap();
    let mapper = NoteMapper;
    let mut base = RawRecord::new("Note", "n0");
    for (k, v) in [
        ("id", FieldValue::Text("n0".into())),
        ("createdAt", FieldValue::Date(1)),
        ("updatedAt", FieldValue::Date(10)),
    ] {
        base.set(k, v);
    }
    mapper.encode(
        &Note {
            id: "n0".into(),
            text: "v1".into(),
            tags: vec![],
            body: vec![],
            created: 1,
            updated: 10,
        },
        &mut base,
    );
    // Insert (no tag) succeeds; inserting the same name again is refused.
    let r = backend
        .save_records(&[base.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert!(r[0].result.is_ok());
    let r = backend
        .save_records(&[base.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert_eq!(r[0].result, Err(SyncError::Conflict));
    let fetched = backend.fetch_record("n0").unwrap().unwrap();
    let tag = fetched
        .change_tag
        .clone()
        .expect("server change tag travels to Rust");

    // Another device writes: the tag is now stale.
    helper
        .backend_debug_write(wire_note("n0", "from-other-device", 1, 99))
        .unwrap();
    let mut mine = fetched.clone();
    mine.set("text", FieldValue::Text("mine-stale".into()));
    let r = backend
        .save_records(&[mine.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert_eq!(r[0].result, Err(SyncError::Conflict));
    let server = r[0]
        .server_record
        .clone()
        .expect("conflict carries the server record");
    assert_eq!(server.text("text").as_deref(), Some("from-other-device"));
    assert_ne!(server.change_tag.as_deref(), Some(tag.as_str()));
    // On top of the server's tag the save lands.
    let mut retry = server.clone();
    retry.set("text", FieldValue::Text("merged".into()));
    let r = backend
        .save_records(&[retry], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert!(r[0].result.is_ok(), "{:?}", r[0].result);
    assert_eq!(
        backend
            .fetch_record("n0")
            .unwrap()
            .unwrap()
            .text("text")
            .as_deref(),
        Some("merged")
    );
    helper.shutdown();
}
