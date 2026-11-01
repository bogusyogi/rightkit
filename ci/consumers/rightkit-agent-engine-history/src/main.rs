// rightkit-agent-engine-history 0.1.0 consumed by exact registry pin: unified diffs and the snapshot store
// in a per-process directory under the OS temp directory.
use rightkit_agent_engine_history::{unified_diff, HistoryStore};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let d = unified_diff("a\nb\n", "a\nc\n", "f.txt");
    check(
        d.contains("-b") && d.contains("+c"),
        "unified_diff marks removed and added lines",
    );
    let root = std::env::temp_dir().join(format!("rk-history-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    let store = HistoryStore::new(&root, ".rightkit");
    let file = root.join("x.txt");
    store.snapshot(&file, "before").expect("snapshot");
    let restored = store.restore_latest(&file).expect("restore");
    check(
        restored.as_deref() == Some("before"),
        "snapshot then restore_latest returns the content",
    );
    check(
        root.join(".rightkit").join(".gitignore").exists(),
        "state dir is git-ignored",
    );
    let _ = std::fs::remove_dir_all(&root);
}
