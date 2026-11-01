// rightkit-agent-engine-storage 0.1.0 consumed by exact registry pin: in-memory database with
// migrations, session listing, SQLite observer install and the op-kind labels. No files are written.
use rightkit_agent_engine_storage::{set_sqlite_op_observer, SqliteOpKind, Storage};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn observe(_kind: SqliteOpKind, _elapsed: std::time::Duration) {}

fn main() {
    let storage = Storage::open_in_memory();
    check(storage.is_ok(), "open_in_memory runs every migration");
    let storage = storage.expect("storage");
    check(
        storage
            .list_sessions()
            .map(|s| s.is_empty())
            .unwrap_or(false),
        "fresh database has no sessions",
    );
    check(
        storage
            .all_events("missing")
            .map(|e| e.is_empty())
            .unwrap_or(false),
        "unknown session has no events",
    );
    check(set_sqlite_op_observer(observe), "observer installs once");
    check(
        SqliteOpKind::AppendEvent.label() == "append_event",
        "op kind labels are stable",
    );
}
