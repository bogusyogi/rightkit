//! Shared fixtures for the cipher, bridge and HTTP tests: a note mapper, an in-memory local
//! store and a device (engine + store + ledger + clock).
#![allow(dead_code)]

pub mod fake_server;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_sync::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub text: String,
    pub created: i64,
    pub updated: i64,
}

pub struct NoteMapper;
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
pub struct Local(pub Arc<Mutex<BTreeMap<String, Note>>>);
impl Local {
    pub fn put(&self, id: &str, text: &str, created: i64, updated: i64) {
        self.0.lock().unwrap().insert(
            id.into(),
            Note {
                id: id.into(),
                text: text.into(),
                created,
                updated,
            },
        );
    }
    pub fn text(&self, id: &str) -> Option<String> {
        self.0.lock().unwrap().get(id).map(|n| n.text.clone())
    }
    pub fn delete(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    /// `(id, text)` pairs in id order.
    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.0
            .lock()
            .unwrap()
            .values()
            .map(|n| (n.id.clone(), n.text.clone()))
            .collect()
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

pub struct TestClock(pub AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Device {
    pub local: Local,
    pub engine: SyncEngine<NoteMapper>,
    pub storage: MemoryStorage,
    pub clock: Arc<TestClock>,
}

pub fn build(
    backend: Arc<dyn Backend>,
    local: &Local,
    storage: &MemoryStorage,
    clock: &Arc<TestClock>,
    config: SyncConfig,
) -> SyncEngine<NoteMapper> {
    SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        backend,
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .clock(clock.clone())
    .config(config)
    .build()
    .unwrap()
}

/// A device with its own store, ledger and clock.
pub fn device(backend: Arc<dyn Backend>, infer: bool) -> Device {
    device_on(backend, &Local::default(), infer)
}

/// A device whose local store is `local` (several engines can share one store).
pub fn device_on(backend: Arc<dyn Backend>, local: &Local, infer: bool) -> Device {
    let storage = MemoryStorage::default();
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let engine = build(
        backend,
        local,
        &storage,
        &clock,
        SyncConfig {
            infer_local_deletions: infer,
            ..SyncConfig::default()
        },
    );
    Device {
        local: local.clone(),
        engine,
        storage,
        clock,
    }
}

impl Device {
    pub fn pass(&mut self) -> PassReport {
        let r = self.engine.run_pass();
        // Step past any backoff window so tests drive passes explicitly.
        self.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
        r
    }
}
