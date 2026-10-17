//! The paged local listing (`LocalStore::list_page`) gives the same pass results as the full
//! `all()` listing: same reports, same ledger, same local and remote state, at every page size,
//! including page boundaries that split related changes (an upload next to a download, inferred
//! deletions on both sides of a boundary). Also: a store whose pages do not advance fails the pass
//! instead of looping.

mod common;

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{Note, NoteMapper, TestClock};
use rightkit_sync::*;

#[derive(Clone)]
struct PagedLocal {
    notes: Arc<Mutex<BTreeMap<String, Note>>>,
    /// `false`: `list_page` answers `None`, so the engine uses `all()` (the old full pass).
    paged: bool,
    page_calls: Arc<AtomicUsize>,
}

impl PagedLocal {
    fn new(paged: bool) -> Self {
        Self {
            notes: Arc::default(),
            paged,
            page_calls: Arc::default(),
        }
    }
    fn put(&self, id: &str, text: &str, created: i64, updated: i64) {
        self.notes.lock().unwrap().insert(
            id.into(),
            Note {
                id: id.into(),
                text: text.into(),
                created,
                updated,
            },
        );
    }
    fn delete(&self, id: &str) {
        self.notes.lock().unwrap().remove(id);
    }
    fn snapshot(&self) -> Vec<Note> {
        self.notes.lock().unwrap().values().cloned().collect()
    }
}

impl LocalStore<Note> for PagedLocal {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(self.snapshot())
    }
    fn list_page(&self, after: Option<&str>, limit: usize) -> SyncResult<Option<Vec<Note>>> {
        if !self.paged {
            return Ok(None);
        }
        self.page_calls.fetch_add(1, Ordering::SeqCst);
        let lower = match after {
            Some(a) => Bound::Excluded(a.to_string()),
            None => Bound::Unbounded,
        };
        let m = self.notes.lock().unwrap();
        Ok(Some(
            m.range((lower, Bound::Unbounded))
                .take(limit)
                .map(|(_, n)| n.clone())
                .collect(),
        ))
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.delete(id);
        Ok(())
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        let mut m = self.notes.lock().unwrap();
        for n in items {
            m.insert(n.id.clone(), n.clone());
        }
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
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

fn remote_tombstone(zone: &MemoryBackend, id: &str, at: i64) {
    let mut r = RawRecord::new("Note", id);
    write_tombstone(&mut r, id, at, &[]);
    zone.put_record(r);
}

struct Run {
    reports: Vec<PassReport>,
    ledgers: Vec<Vec<u8>>,
    local: Vec<Note>,
    remote: Vec<Option<RawRecord>>,
    page_calls: usize,
}

fn id(i: usize) -> String {
    format!("n{i:02}")
}

/// Three passes over a 14-id zone. Local ids (n01..n04, n06..n08, n12, n13) are listed in pages of
/// `page` items, so with `page == 2` the boundary between [n01 n02] and [n03 n04] separates a
/// download (n02: remote newer) from an upload (n03: local newer), and so on.
fn scenario(paged: bool, page: usize) -> Run {
    let zone = MemoryBackend::new();
    for i in 0..12 {
        remote_note(&zone, &id(i), "remote", i as i64, 100);
    }
    let local = PagedLocal::new(paged);
    local.put("n01", "local-newer", 1, 200); // upload
    local.put("n02", "local-older", 2, 50); // download
    local.put("n03", "local-newer", 3, 300); // upload, right after a download
    local.put("n04", "local-equal", 4, 100); // neither
    local.put("n06", "local-equal", 6, 100); // neither
    local.put("n07", "local-newer", 7, 400); // upload
    local.put("n08", "local-older", 8, 10); // download
    local.put("n12", "local-new", 12, 120); // never published: new
    local.put("n13", "local-new", 13, 130); // never published: new

    let storage = MemoryStorage::default();
    let clock = Arc::new(TestClock(std::sync::atomic::AtomicI64::new(1_000_000)));
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(zone.clone()),
        Arc::new(local.clone()),
        Box::new(storage),
    )
    .clock(clock)
    .config(SyncConfig {
        infer_local_deletions: true,
        ..SyncConfig::default()
    })
    .local_page_size(page)
    .build()
    .unwrap();

    let mut reports = Vec::new();
    let mut ledgers = Vec::new();
    let mut pass = |engine: &mut SyncEngine<NoteMapper>| {
        let r = engine.run_pass();
        assert_eq!(r.outcome, PassOutcome::Synced, "{r:?}");
        reports.push(r);
        ledgers.push(serde_json::to_vec(engine.ledger()).unwrap());
    };

    pass(&mut engine);

    // Between passes: local deletions on both sides of a page boundary (inferred), a local edit
    // queued next to a remote edit, a remote tombstone for a held item, a remote-only new item.
    local.delete("n05");
    local.delete("n06");
    local.put("n00", "local-edit", 0, 300);
    engine.note_changed("n00").unwrap();
    remote_note(&zone, "n02", "remote-edit", 2, 500);
    remote_note(&zone, "n03", "remote-edit", 3, 200); // older than local n03 (already uploaded 300)
    remote_tombstone(&zone, "n11", 600);
    remote_note(&zone, "n14", "remote-new", 14, 140);
    pass(&mut engine);

    // Third pass: nothing left to move.
    pass(&mut engine);
    let last = reports.last().unwrap();
    assert_eq!((last.uploaded, last.downloaded), (0, 0), "{last:?}");

    let remote = (0..16).map(|i| zone.record(&id(i))).collect();
    Run {
        reports,
        ledgers,
        local: local.snapshot(),
        remote,
        page_calls: local.page_calls.load(Ordering::SeqCst),
    }
}

#[test]
fn paged_pass_matches_the_full_pass_at_every_page_size() {
    let full = scenario(false, 1000);
    assert_eq!(full.page_calls, 0);
    for page in [1usize, 2, 3, 4, 100, 1000] {
        let run = scenario(true, page);
        assert!(run.page_calls >= 3, "page {page}: {} calls", run.page_calls);
        assert_eq!(run.reports, full.reports, "page size {page}: reports");
        assert_eq!(run.ledgers, full.ledgers, "page size {page}: ledgers");
        assert_eq!(run.local, full.local, "page size {page}: local store");
        assert_eq!(run.remote, full.remote, "page size {page}: zone");
    }
}

#[test]
fn small_pages_really_split_the_listing() {
    let run = scenario(true, 2);
    // 9 local ids in pages of 2 is 5 calls on the first pass alone.
    assert!(run.page_calls >= 15, "{} calls", run.page_calls);
}

#[test]
fn a_store_whose_pages_do_not_advance_fails_the_pass() {
    struct Stuck(Vec<Note>);
    impl LocalStore<Note> for Stuck {
        fn all(&self) -> SyncResult<Vec<Note>> {
            Ok(self.0.clone())
        }
        fn list_page(&self, _after: Option<&str>, limit: usize) -> SyncResult<Option<Vec<Note>>> {
            Ok(Some(self.0.iter().take(limit).cloned().collect()))
        }
        fn remove(&self, _id: &str) -> SyncResult<()> {
            Ok(())
        }
        fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
            Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
        }
    }
    let notes = (0..3)
        .map(|i| Note {
            id: id(i),
            text: "x".into(),
            created: i as i64,
            updated: 10,
        })
        .collect();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(MemoryBackend::new()),
        Arc::new(Stuck(notes)),
        Box::new(MemoryStorage::default()),
    )
    .local_page_size(2)
    .build()
    .unwrap();
    let r = engine.run_pass();
    assert!(matches!(r.outcome, PassOutcome::Failed(_)), "{r:?}");
}
