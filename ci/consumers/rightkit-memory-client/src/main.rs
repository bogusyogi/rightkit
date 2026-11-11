// rightkit-memory-client 0.1.0 (published as rightkit-memory until 0.1.1) consumed by exact registry
// pin: write screening, scope slugs, the membrane schema check and the engine's tier type at the
// backend boundary. No external membrane binary is run.
use rightkit_memory_client::backend::types::path_to_scope;
use rightkit_memory_client::membrane::{check_schema, SUPPORTED_MAJOR};
use rightkit_memory_client::backend::MemoryTier;
use rightkit_memory_client::screen;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(!screen("").is_admit(), "empty memory is rejected");
    check(
        screen("The MyApp desktop UI lives in apps/myapp-desktop.").is_admit(),
        "a durable fact is admitted",
    );
    check(
        path_to_scope("D:/work/app") == "D--work-app",
        "path_to_scope slug",
    );
    let doc = serde_json::json!({ "schema": format!("membrane.status.v{SUPPORTED_MAJOR}") });
    check(
        check_schema(&doc, &["membrane.status"]).is_ok(),
        "supported schema accepted",
    );
    check(
        MemoryTier::parse("SEMANTIC") == Some(MemoryTier::Semantic)
            && MemoryTier::Semantic.as_str() == "Semantic",
        "the backend tier is the engine's, with its wire names",
    );
    check(
        MemoryTier::Semantic.rank() > MemoryTier::Working.rank(),
        "the engine's tier ranks are available through the client",
    );
}
