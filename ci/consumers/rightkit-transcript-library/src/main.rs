// rightkit-transcript-library consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values only.
use rightkit_transcript_library::delivery::draft_delivery_id;
use rightkit_transcript_library::history::{HistoryQuery, HistoryStore, MemoryHistoryStore};
use rightkit_transcript_library::net::host_of_url;
use rightkit_transcript_library::export::{format_history, ExportLabels, HistoryExportFormat};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    store_api_surface();
    let mut all = true;
    let mut store = MemoryHistoryStore::default();
    let upserted = store.upsert_draft("s1", "hello world".to_string()).is_ok();
    let found = store.find(&draft_delivery_id("s1"));
    all &= check(
        upserted && found.is_some_and(|r| r.transcript == "hello world"),
        "draft upserts under its deterministic id",
    );
    let page = store.query(&HistoryQuery::default());
    all &= check(
        page.is_ok_and(|p| p.items.is_empty()),
        "default history query hides drafts",
    );
    all &= check(
        host_of_url("https://API.Example.COM:8443/v1").as_deref() == Some("api.example.com"),
        "host_of_url lower-cases and strips the port",
    );
    let exported = format_history(&[], HistoryExportFormat::PlainText, &ExportLabels::default());
    all &= check(exported.is_ok_and(|text| text.contains("Transcript history")), "history export formats empty history");
    if !all {
        std::process::exit(1);
    }
}

fn store_api_surface() {
    use rightkit_transcript_library::store::DeliveryHistoryStore;
    use rightkit_transcript_library::history::HistoryStore;
    let _open_connection = DeliveryHistoryStore::open_connection;
    let _connection = DeliveryHistoryStore::connection;
    let _push = HistoryStore::push;
    let _push_batch = HistoryStore::push_batch;
    let _update = HistoryStore::update_transcript;
    let _delete_matching = HistoryStore::delete_matching;
    let _prune = HistoryStore::prune_before;
    let _clear = HistoryStore::clear;
}
