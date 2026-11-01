//! Registry consumer for rightkit-memory-store 0.1.0: builds against the published API only.
use rightkit_memory_store::MemDb;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let _db = MemDb::open_in_memory();
    println!("ok: an in-memory store opens");
    let dir = std::env::temp_dir().join(format!(
        "rightkit-memory-store-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    check(
        MemDb::open(dir.join("memory.db")).is_ok(),
        "a file store opens under the temp directory",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-memory-store consumer: all checks passed");
}
