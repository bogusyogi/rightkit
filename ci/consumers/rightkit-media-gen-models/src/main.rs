// rightkit-media-gen-models 0.1.0 consumed by exact registry pin (default features), outside the workspace.
// The command table and the route settings on a temp Context; no download, no network, no model.
use rightkit_media_gen::Context;
use rightkit_media_gen_models::{handle, COMMANDS};
use serde_json::json;

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
        COMMANDS.contains(&"studio.models.catalog")
            && COMMANDS.contains(&"studio.models.install")
            && COMMANDS.contains(&"studio.models.governor.status")
            && COMMANDS.contains(&"studio.models.route.get"),
        "the local model commands",
    );
    check(
        !COMMANDS.iter().any(|c| {
            matches!(
                *c,
                "studio.models.workers"
                    | "studio.models.rental.search"
                    | "studio.models.worker.action"
                    | "studio.models.rentals"
                    | "studio.models.rentals_enforce"
            )
        }),
        "no rented-worker command (rightkit-remote-compute answers those)",
    );

    let dir = std::env::temp_dir().join(format!(
        "rightkit-media-gen-models-consumer-{}",
        std::process::id()
    ));
    let ctx = Context::new(dir.clone());
    let routes = handle(&ctx, "studio.models.route.get", &json!({})).expect("route.get");
    check(
        routes["routes"] == json!({}),
        "no route is set in a new data folder",
    );
    check(
        routes["valid_routes"] == json!(["auto", "local", "api", "rented"]),
        "the valid routes",
    );
    let refused = handle(&ctx, "studio.models.workers", &json!({}));
    check(
        refused.is_err_and(|e| e.code == "unknown_command"),
        "a rented-worker command is not answered here",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
