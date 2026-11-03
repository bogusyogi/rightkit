// rightkit-evidence-application 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the configuration refusal and the catalog value types:
// - NativeApplicationConfig::new builds a default configuration value without I/O;
// - from_versioned_json refuses text that is not a versioned JSON document;
// - from_versioned_source refuses inline JSON that is not a versioned document (no file read);
// - CatalogHostError and EmptyCatalog are constructible public types.
// No application is built, no catalog is loaded, no audit or report is run, no file is read.
// No network, no child process. Exits 1 on the first failed check.
use rightkit_evidence_application::{CatalogHostError, EmptyCatalog, NativeApplicationConfig};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let _default = NativeApplicationConfig::new();
    check(true, "NativeApplicationConfig::new builds a default value");

    check(
        NativeApplicationConfig::from_versioned_json("not json").is_err(),
        "text that is not JSON is refused",
    );
    check(
        NativeApplicationConfig::from_versioned_source("{").is_err(),
        "truncated inline JSON is refused",
    );
    let host_error = CatalogHostError("catalog offline".to_string());
    check(host_error.0 == "catalog offline", "CatalogHostError carries its message");
    let _empty = EmptyCatalog;
    check(true, "EmptyCatalog is constructible");
    println!("OK rightkit-evidence-application 0.1.0 (registry consumer)");
}
