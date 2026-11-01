// rightkit-media-pipeline 0.1.0 consumed by exact registry pin (default features), outside the workspace.
// Pure, embedded-data commands only: no provider keys, no network, no ffmpeg.
use rightkit_media_pipeline::{commands, handle, Context};
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
    let all = commands();
    check(
        all.contains(&"studio.pipelines.templates") && all.contains(&"studio.production.templates"),
        "command list",
    );
    let ctx = Context::new(std::env::temp_dir().join("rightkit-media-pipeline-consumer"));
    check(handle(&ctx, "nope.unknown", &json!({})).is_none(), "unknown command not handled");
    let t = handle(&ctx, "studio.pipelines.templates", &json!({}));
    check(
        matches!(&t, Some(Ok(v)) if v["templates"].is_array() || v["templates"].is_object()),
        "pipeline templates from embedded data",
    );
    let p = handle(&ctx, "studio.production.templates", &json!({}));
    check(matches!(&p, Some(Ok(v)) if v.is_object()), "production templates from embedded data");
}
