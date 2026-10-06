//! Outcome-level proofs: two devices converge through one shared zone, and
//! the offline / partial-failure / replay paths resume without loss.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_sync::*;

#[derive(Clone, Debug, PartialEq)]
struct Note {
    id: String,
    text: String,
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
        ItemHeader { id: n.id.clone(), created_at_ms: n.created, updated_at_ms: n.updated, origin_device: None }
    }
    fn encode(&self, n: &Note, r: &mut RawRecord) {
        r.set("text", FieldValue::Text(n.text.clone()));
    }
    fn decode(&self, r: &RawRecord) -> Result<Note, String> {
        Ok(Note {
            id: r.text("id").ok_or("no id")?,
            text: r.text("text").ok_or("no text")?,
            created: r.date_ms("createdAt").ok_or("no createdAt")?,
            updated: r.date_ms("updatedAt").ok_or("no updatedAt")?,
        })
    }
}

#[derive(Default, Clone)]
struct Local(Arc<Mutex<BTreeMap<String, Note>>>);
impl Local {
    fn put(&self, id: &str, text: &str, created: i64, updated: i64) {
        self.0.lock().unwrap().insert(
            id.into(),
            Note { id: id.into(), text: text.into(), created, updated },
        );
    }
    fn text(&self, id: &str) -> Option<String> {
        self.0.lock().unwrap().get(id).map(|n| n.text.clone())
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

struct Device {
    local: Local,
    engine: SyncEngine<NoteMapper>,
    storage: MemoryStorage,
    clock: Arc<TestClock>,
}

fn device(backend: &MemoryBackend, infer: bool) -> Device {
    let local = Local::default();
    let storage = MemoryStorage::default();
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let engine = build(backend, &local, &storage, &clock, infer);
    Device { local, engine, storage, clock }
}

fn build(
    backend: &MemoryBackend,
    local: &Local,
    storage: &MemoryStorage,
    clock: &Arc<TestClock>,
    infer: bool,
) -> SyncEngine<NoteMapper> {
    SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(backend.clone()),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .clock(clock.clone())
    .config(SyncConfig { infer_local_deletions: infer, ..SyncConfig::default() })
    .build()
    .unwrap()
}

impl Device {
    fn pass(&mut self) -> PassReport {
        let r = self.engine.run_pass();
        // Step past any backoff window so tests drive passes explicitly.
        self.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
        r
    }
}

#[test]
fn two_devices_converge_edits_lww_and_tombstone_wins() {
    let zone = MemoryBackend::new();
    zone.set_page_size(2); // force multi-page delta fetches
    let (mut a, mut b) = (device(&zone, false), device(&zone, false));
    for i in 0..5 {
        a.local.put(&format!("n{i}"), "v1", 100 + i, 100 + i);
    }
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
    assert_eq!(b.pass().downloaded, 5);
    assert_eq!(b.local.len(), 5);

    // Concurrent edit of n0: B newer than A. LWW picks B everywhere.
    a.local.put("n0", "from-a", 100, 500);
    b.local.put("n0", "from-b", 100, 600);
    a.pass();
    b.pass();
    a.pass();
    assert_eq!(a.local.text("n0").as_deref(), Some("from-b"));
    assert_eq!(b.local.text("n0").as_deref(), Some("from-b"));

    // A deletes n1 while B concurrently edits it: tombstone wins on both,
    // and B must not re-upload it.
    a.local.0.lock().unwrap().remove("n1");
    a.engine.note_deleted("n1").unwrap();
    b.local.put("n1", "late-edit", 101, 9_999);
    a.pass();
    let before = zone.saved_records();
    b.pass();
    assert!(b.local.text("n1").is_none());
    assert_eq!(zone.saved_records(), before, "blocked id must not be re-uploaded");
    let tomb = zone.record("n1").unwrap();
    assert!(tomb.date_ms("deletedAt").is_some());
    assert!(tomb.get("text").is_none(), "tombstone clears content");
    assert_eq!(a.pass().outcome, PassOutcome::Synced);
}

#[test]
fn offline_queue_partial_batch_and_restart_resume_without_duplicates() {
    let zone = MemoryBackend::new();
    let mut a = device(&zone, false);
    for i in 0..4 {
        a.local.put(&format!("n{i}"), "v", 10 + i, 10 + i);
    }
    // Offline: pass fails, backoff window opens, nothing lost.
    zone.set_offline(true);
    let r = a.engine.run_pass();
    assert!(matches!(r.outcome, PassOutcome::Failed(_)), "{:?}", r.outcome);
    assert_eq!(r.retry_after_secs, Some(15.0), "server hint honoured");
    assert!(matches!(a.engine.run_pass().outcome, PassOutcome::Deferred { .. }));
    a.clock.0.fetch_add(3_600_000, Ordering::SeqCst);

    // Back online but the zone accepts only 2 records: partial success.
    zone.set_offline(false);
    zone.fail_after_saves(Some(2));
    let r = a.pass();
    assert_eq!(r.uploaded, 2);
    assert!(matches!(r.outcome, PassOutcome::Pending(_)));
    assert_eq!(zone.record_count(), 2);

    // "Restart": new engine over the same durable ledger; zone healthy.
    zone.fail_after_saves(None);
    let saves_before = zone.saved_records();
    let mut a2 = build(&zone, &a.local, &a.storage, &a.clock, false);
    let r = a2.run_pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(zone.saved_records() - saves_before, 2, "only the unacknowledged remainder uploads");
    assert_eq!(zone.record_count(), 4);
    assert!(a2.ledger().change_token.is_some() && a2.ledger().pending_uploads.is_empty());
}

#[test]
fn token_expiry_replays_once_and_bad_records_are_quarantined() {
    let zone = MemoryBackend::new();
    let (mut a, mut b) = (device(&zone, false), device(&zone, false));
    a.local.put("ok", "v", 1, 1);
    a.pass();
    // A malformed record in the zone must not block the page.
    let mut junk = RawRecord::new("Note", "junk");
    junk.set("id", FieldValue::Text("junk".into()));
    zone.put_record(junk);
    b.pass();
    zone.expire_token_once();
    let r = b.pass();
    assert_eq!(r.outcome, PassOutcome::Synced);
    assert_eq!(b.local.len(), 1);
    assert_eq!(r.quarantined, 1, "replay re-sees the junk record, counted not fatal");
    assert!(b.engine.ledger().quarantined_total >= 2);
}

#[test]
fn desktop_style_local_deletion_inference_writes_one_tombstone() {
    let zone = MemoryBackend::new();
    let (mut a, mut b) = (device(&zone, true), device(&zone, false));
    a.local.put("x", "v", 1, 1);
    a.pass();
    b.pass();
    assert_eq!(b.local.len(), 1);
    a.local.0.lock().unwrap().remove("x"); // app deleted without telling the engine
    let r = a.pass();
    assert_eq!(r.tombstones_written, 1);
    b.pass();
    assert_eq!(b.local.len(), 0);
}
