//! Registry consumer for rightkit-context 0.1.0: builds against the published API only.
//! It does not start the resident or touch any store.
use rightkit_context::{
    protocol, runtime, EXIT_OK, EXIT_RESIDENT_UNREACHABLE, SCHEMA_CONTEXT_PULL,
    SCHEMA_MEMORY_SEARCH,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        EXIT_OK == 0 && EXIT_RESIDENT_UNREACHABLE != EXIT_OK,
        "exit codes are distinct",
    );
    check(
        SCHEMA_CONTEXT_PULL.ends_with(".v1") && SCHEMA_MEMORY_SEARCH.ends_with(".v1"),
        "output schema ids are versioned",
    );
    check(
        !protocol::SHAPES.is_empty(),
        "the protocol re-export carries the contract shapes",
    );
    check(
        runtime::digest::digest_str("x").starts_with("sha256:"),
        "the runtime re-export is usable",
    );
    println!("rightkit-context consumer: all checks passed");
}
