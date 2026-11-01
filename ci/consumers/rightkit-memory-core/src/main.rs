//! Registry consumer for rightkit-memory-core 0.1.0: builds against the published API only.
use rightkit_memory_core::{estimate_tokens, Embedder, HashEmbedder, MemoryEntry, MemoryTier};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(estimate_tokens("") == 0, "empty text costs no tokens");
    check(estimate_tokens("a few words of text") > 0, "text costs tokens");
    let e = HashEmbedder::new();
    check(e.embed("same input") == e.embed("same input"), "the hash embedder is deterministic");
    let _ = std::mem::size_of::<MemoryEntry>() + std::mem::size_of::<MemoryTier>();
    println!("rightkit-memory-core consumer: all checks passed");
}
