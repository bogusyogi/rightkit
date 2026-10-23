//! Runtime budget for rightkit-sync (macOS and Windows).
//!
//! Scenario, through the public engine API (`SyncEngine::run_pass`) over `MemoryBackend`:
//! * a zone of 100,000 `Note` records is seeded in the backend; the device's local store
//!   starts empty, so the first pass is a full pull of all 100,000 records;
//! * then `PASSES` steady-state passes (default 100). Each pass edits 100 remote records
//!   (first half of the id space) and 100 local records (second half, queued through
//!   `note_changed`), so every pass downloads and uploads.
//!
//! After the passes the engine is held idle (ledger of 100,000 records resident, no pass running)
//! for the idle window. Reports resident memory, CPU, wake-ups and disk operations through
//! `rightkit_child_process::process_usage` (the one shared source), pass latency and throughput from
//! `SyncEngine::metrics()`, and the size of the ledger document that every durable write
//! serializes in full.
//!
//! Run: `rustright cargo bench -p rightkit-sync --bench budget`
//! Optional: `-- <records> <passes> <edits_per_side_per_pass>` (defaults 100000, 100, 100); a fourth argument sets the idle seconds (default 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels, ProcessUsage};
use rightkit_sync::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn usage() -> ProcessUsage {
    process_usage().expect("process usage")
}

fn report(label: &str, p: ProcessUsage) {
    println!(
        "{label}: resident {:7.1} MB  footprint {:7.1} MB",
        mb(p.resident_bytes),
        mb(p.footprint_bytes)
    );
}

#[derive(Clone, Debug)]
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
struct Local(Arc<Mutex<BTreeMap<String, Note>>>);

impl LocalStore<Note> for Local {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn list_page(&self, after: Option<&str>, limit: usize) -> SyncResult<Option<Vec<Note>>> {
        use std::ops::Bound;
        let m = self.0.lock().unwrap();
        let lower = match after {
            Some(a) => Bound::Excluded(a.to_string()),
            None => Bound::Unbounded,
        };
        Ok(Some(
            m.range((lower, Bound::Unbounded))
                .take(limit)
                .map(|(_, n)| n.clone())
                .collect(),
        ))
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

fn remote_record(id: &str, text: &str, created: i64, updated: i64) -> RawRecord {
    let mut r = RawRecord::new("Note", id);
    r.set(field::ID, FieldValue::Text(id.into()));
    r.set("text", FieldValue::Text(text.into()));
    r.set(field::CREATED_AT, FieldValue::Date(created));
    r.set(field::UPDATED_AT, FieldValue::Date(updated));
    r
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let n: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let passes: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(100);
    let edits: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(100);
    let idle_s: u64 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(30);
    let half = n / 2;

    let backend = MemoryBackend::new();
    let t = Instant::now();
    for i in 0..n {
        let id = format!("n{i:06}");
        let r = remote_record(
            &id,
            &format!("seed {i}"),
            1_000 + i as i64,
            1_000 + i as i64,
        );
        backend.put_record(r);
    }
    println!(
        "seeded {} remote records in {:.1?}",
        backend.record_count(),
        t.elapsed()
    );

    let local = Local::default();
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(backend.clone()),
        Arc::new(local.clone()),
        Box::new(MemoryStorage::default()),
    )
    .build()
    .unwrap();
    let before = usage();
    report("engine built", before);

    let t = Instant::now();
    let first = engine.run_pass();
    let initial_pull_s = t.elapsed().as_secs_f64();
    assert_eq!(
        first.outcome,
        PassOutcome::Synced,
        "initial pull: {:?}",
        first.outcome
    );
    assert_eq!(
        first.downloaded as u64, n,
        "initial pull must download every record"
    );
    println!(
        "initial pull: {} records in {initial_pull_s:.2} s",
        first.downloaded
    );
    report("after initial pull", usage());

    let u0 = usage();
    let start = Instant::now();
    let mut pass_ms: Vec<f64> = Vec::with_capacity(passes as usize);
    let mut moved = 0u64;
    let mut not_synced = 0u64;
    let mut pending_max = 0u64;
    for p in 0..passes {
        for k in 0..edits {
            let i = (p * edits + k) % half;
            let id = format!("n{i:06}");
            backend.put_record(remote_record(
                &id,
                &format!("remote {p}"),
                1_000 + i as i64,
                now_ms(),
            ));
        }
        for k in 0..edits {
            let i = half + (p * edits + k) % half;
            let id = format!("n{i:06}");
            local.0.lock().unwrap().insert(
                id.clone(),
                Note {
                    id: id.clone(),
                    text: format!("local {p}"),
                    created: 1_000 + i as i64,
                    updated: now_ms(),
                },
            );
            engine.note_changed(&id).unwrap();
        }
        let t0 = Instant::now();
        let rep = engine.run_pass();
        pass_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        moved += (rep.downloaded + rep.uploaded) as u64;
        if rep.outcome != PassOutcome::Synced {
            not_synced += 1;
        }
        pending_max = pending_max.max(engine.metrics().pending_uploads);
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = usage().cpu_seconds - u0.cpu_seconds;
    let m = engine.metrics();
    report("after steady passes", usage());

    let ledger_json = serde_json::to_vec(engine.ledger()).unwrap().len();
    let mean = pass_ms.iter().sum::<f64>() / pass_ms.len().max(1) as f64;
    let max = pass_ms.iter().cloned().fold(0.0, f64::max);
    let end = usage();
    println!(
        "steady passes: records={n} passes={passes} edits_per_side={edits} initial_pull_s={initial_pull_s:.2} pass_ms_mean={mean:.1} pass_ms_max={max:.1} passes_per_s={:.2} records_moved_per_s={:.1} cpu_s={cpu:.2} cpu_percent={:.1} ledger_json_mb={:.2} remote_versions={} pending_uploads_max={pending_max} failed_passes={} not_synced_passes={not_synced} wall_s={wall:.1}",
        passes as f64 / wall,
        moved as f64 / wall,
        cpu / wall * 100.0,
        ledger_json as f64 / (1024.0 * 1024.0),
        m.remote_versions,
        m.failed_passes,
    );
    let warm = end;
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let status = engine.status();
    println!(
        "RESULT pending_uploads_after_idle={} records",
        status.metrics.pending_uploads
    );
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-sync",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "records",
                scale: n,
                what:
                    "records in the ledger after bidirectional passes, engine idle between passes",
                scope: "synthetic"
            }
        )
    );
}
