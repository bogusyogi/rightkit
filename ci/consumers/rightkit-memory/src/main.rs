// rightkit-memory 0.1.0 consumed by exact registry pin: write screening, scope slugs and the membrane
// schema check. No external membrane binary is run.
use rightkit_memory::backend::types::path_to_scope;
use rightkit_memory::membrane::{check_schema, SUPPORTED_MAJOR};
use rightkit_memory::screen;

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
}
