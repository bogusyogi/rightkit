// rightkit-crash 0.3.5 consumed by exact registry pin, outside the workspace.
//
// Proves list_pending, the crash-report discovery a host runs before it enqueues: only files named
// crash_*.json count, the count is exact, and unrelated files in the same directory are ignored.
// Public API only.
// Does not prove: capture_native, the panic and process hooks, enqueue_pending (needs an outbox),
// prune (it sweeps raw samples in the OS temp directory), or support bundles.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data.
use rightkit_crash::list_pending;
use std::fs;
use std::path::PathBuf;

fn main() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rk-crash-consumer-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create consumer temp dir");
    assert!(list_pending(&dir).is_empty(), "an empty directory has no pending reports");

    for name in ["crash_1.json", "crash_2.json", "crash_3.json"] {
        fs::write(dir.join(name), format!("{{\"fixture\":\"{name}\"}}")).expect("write report");
    }
    fs::write(dir.join("crash_notes.txt"), b"not a report").expect("write unrelated text");
    fs::write(dir.join("other.json"), b"{}").expect("write unrelated json");

    let pending = list_pending(&dir);
    assert_eq!(pending.len(), 3, "only crash_*.json files are pending");
    assert!(
        pending.iter().all(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json")),
        "every pending entry is a json report"
    );

    fs::remove_dir_all(&dir).expect("remove consumer temp dir");
    println!("OK rightkit-crash 0.3.5 (registry consumer)");
}
