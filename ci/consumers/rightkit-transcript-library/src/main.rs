// rightkit-transcript-library consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values only.
use rightkit_transcript_library::delivery::draft_delivery_id;
use rightkit_transcript_library::history::{HistoryQuery, HistoryStore, MemoryHistoryStore};
use rightkit_transcript_library::net::host_of_url;

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
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
    if !all {
        std::process::exit(1);
    }
}
