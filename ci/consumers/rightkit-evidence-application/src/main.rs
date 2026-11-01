// rightkit-evidence-application consumed by the exact registry pin, outside the workspace.
// Proves: host catalog adapters validate and count entries, and the default application identity
// is neutral. Nothing is composed or executed.
use rightkit_evidence_application::{
    application_identity, CapabilityCatalog, EmptyCatalog, JsonCatalog,
};

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
        EmptyCatalog.validate().is_ok() && EmptyCatalog.entry_count() == 0,
        "empty catalog",
    );
    let catalog = JsonCatalog(serde_json::json!({"entries": [{"id": "a"}, {"id": "b"}]}));
    check(
        catalog.validate().is_ok() && catalog.entry_count() == 2,
        "json catalog counts entries",
    );
    check(
        JsonCatalog(serde_json::json!({})).validate().is_err(),
        "catalog without entries refused",
    );
    check(
        application_identity().agent_id == "rightkit",
        "neutral default agent id",
    );
}
