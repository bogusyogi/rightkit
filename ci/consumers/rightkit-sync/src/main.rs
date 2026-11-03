// rightkit-sync 0.2.3 consumed by exact registry pin (default features: helper, process-usage), outside
// the workspace.
//
// Compile-only (Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data."; "My decision was no
// unit tests. Only e2e."). The crate no longer ships an in-process backend, and the published package never
// shipped a helper stand-in, so nothing here runs a sync. This program proves that the sealed archive installs
// through the registry and that the public surface an app links still type-checks: the record mapper and
// local store traits, `SyncEngineBuilder` and `SyncEngine`, `FileStorage`, and the real CloudKit helper types
// (`CloudKitHelper`, `HelperConfig`, `HelperBackend`, `HelperAuth`). The real run is the crate's
// `tests/helper_e2e.rs` and `benches/budget.rs` against the signed helper and the app's real CloudKit
// container, which need a Mac, an entitled helper and an iCloud account.
use rightkit_sync::helper::{CloudKitHelper, HelperConfig};
use rightkit_sync::{
    ApplyOutcome, FieldValue, FileStorage, ItemHeader, LocalStore, NullObserver, PassOutcome,
    RawRecord, RecordMapper, SyncEngineBuilder, SyncResult,
};
use std::path::PathBuf;
use std::sync::Arc;

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

struct Local;

impl LocalStore<Note> for Local {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(Vec::new())
    }
    fn remove(&self, _id: &str) -> SyncResult<()> {
        Ok(())
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}

/// Type-checked, never called: it would start the real helper.
fn the_public_surface_compiles(helper_program: PathBuf, ledger: PathBuf) -> Result<(), String> {
    let helper = CloudKitHelper::new(
        HelperConfig::new(helper_program)
            .arg("--container")
            .arg("iCloud.example")
            .arg("--zone")
            .arg("zone")
            .arg("--subscription")
            .arg("subscription"),
        Arc::new(NullObserver),
    );
    let mut engine = SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(helper.backend()),
        Arc::new(Local),
        Box::new(FileStorage::new(ledger)),
    )
    .auth(Arc::new(helper.auth()))
    .build()
    .map_err(|e| e.to_string())?;
    engine.note_changed("n1").map_err(|e| e.to_string())?;
    let report = engine.run_pass();
    if report.outcome != PassOutcome::Synced {
        return Err(format!("{:?}", report.outcome));
    }
    helper.shutdown();
    Ok(())
}

const _: fn(PathBuf, PathBuf) -> Result<(), String> = the_public_surface_compiles;

fn main() {
    println!("rightkit-sync consumer: the public surface compiles; nothing was run");
}
