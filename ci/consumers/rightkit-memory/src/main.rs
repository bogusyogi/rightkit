// rightkit-memory 0.2.0 consumed by exact registry pin: the memory engine as one crate. The root is
// rightkit-memory-core (tiers, token accounting, the hash embedder), `store` is rightkit-memory-store
// (the SQLite memory store); the store is opened in memory only. No file is written.
use rightkit_memory::store::MemDb;
use rightkit_memory::{estimate_tokens, Embedder, HashEmbedder, MemoryTier};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        MemoryTier::parse("episodic") == Some(MemoryTier::Episodic),
        "tier names parse in any case",
    );
    check(
        MemoryTier::Semantic.rank() > MemoryTier::Episodic.rank(),
        "semantic ranks above episodic",
    );
    check(estimate_tokens("") == 0, "empty text costs no tokens");
    let embedder = HashEmbedder::new();
    check(
        embedder.embed("same input") == embedder.embed("same input"),
        "the hash embedder is deterministic",
    );
    // Opening migrates the schema; it panics (and the consumer fails) if that does not work.
    let db = MemDb::open_in_memory();
    drop(db);
    check(true, "the engine's store opens and migrates in memory");
}
