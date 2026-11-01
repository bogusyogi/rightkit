//! Registry consumer for rightkit-document-workspace 0.1.0: builds against the published API only.
use rightkit_document_workspace::Db;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let dir = std::env::temp_dir().join(format!("rightkit-docws-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (db, degraded) = Db::open_path(&dir.join("workspace.db")).expect("open store");
    check(
        degraded.is_none(),
        "a fresh store opens without degradation",
    );
    db.record_recent("/docs/a.md", "a.md", "markdown")
        .expect("record");
    check(
        db.list_recent(10).map(|r| r.len() == 1).unwrap_or(false),
        "a recorded file is listed as recent",
    );
    check(db.purge_user_paths().is_ok(), "stored paths purge");
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-document-workspace consumer: all checks passed");
}
