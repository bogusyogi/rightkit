// rightkit-media-gen-approvals 0.1.0 consumed by exact registry pin (default features), outside the workspace.
use rightkit_media_gen::{Context, GenResult};
use rightkit_media_gen_approvals::{handle, input_hash, parent_is_app, register_kind};
use serde_json::{json, Value};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn demo(_ctx: &Context, id: &str, _params: &Value) -> GenResult<(Value, String)> {
    Ok((json!({"draft": id}), String::new()))
}

fn main() {
    let dir = std::env::temp_dir().join("rightkit-media-gen-approvals-consumer");
    let engine = dir.join("demo-engine");
    check(!parent_is_app(None, &engine), "unreadable parent refused");
    check(
        !parent_is_app(Some(&dir.join("bash").to_string_lossy()), &engine),
        "shell beside the engine refused",
    );
    check(
        input_hash(&json!({"a": 1})) == input_hash(&json!({"a": 1})),
        "stable input hash",
    );
    check(
        input_hash(&json!({"a": 1})) != input_hash(&json!({"a": 2})),
        "input hash differs",
    );

    let ctx = Context::new(dir.clone());
    let unknown = handle(
        &ctx,
        "studio.approvals.challenge",
        &json!({"kind": "nope", "draft_id": "d1"}),
    );
    check(
        matches!(&unknown, Err(e) if e.code == "invalid_input"),
        "unknown kind refused",
    );
    register_kind("demo", demo);
    let issued = handle(
        &ctx,
        "studio.approvals.challenge",
        &json!({"kind": "demo", "draft_id": "d1"}),
    );
    check(
        matches!(&issued, Ok(v) if v["kind"] == "demo" && v["single_use"] == true),
        "registered kind issues a challenge",
    );
}
