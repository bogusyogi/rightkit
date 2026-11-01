// rightkit-apple-ops consumed by the exact registry pin, outside the workspace.
// Proves: the operation catalog is offline and names the backend; preflight listing runs without
// processes or network; non-object arguments are refused.
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
    let catalog = rightkit_apple_ops::catalog();
    check(
        catalog["backend"] == "rightkit-apple-ops",
        "catalog names the backend",
    );
    check(
        catalog["operations"]["preflight"].is_string(),
        "catalog lists preflight",
    );
    let listed =
        rightkit_apple_ops::preflight::invoke(&json!({"list": true})).expect("offline list");
    check(listed["tools"].is_array(), "preflight list is offline");
    check(
        rightkit_apple_ops::preflight::invoke(&json!({"list": "yes"})).is_err(),
        "malformed list refused",
    );
    let (targets, _) = rightkit_apple_ops::gate::bound_targets(
        "project.build",
        &json!({"project": "App.xcodeproj"}),
        None,
    )
    .expect("targets");
    check(targets == ["App.xcodeproj"], "gate binds project targets");
    let effects =
        rightkit_apple_ops::gate::required_effects("project.build", &json!({"execute": true}))
            .expect("effects");
    check(
        effects.contains(&rightkit_apple_ops::gate::Effect::ProcessSpawn)
            && effects.contains(&rightkit_apple_ops::gate::Effect::FileWrite),
        "gate derives build effects",
    );
    check(
        rightkit_apple_ops::cli::read_input(Some("[]"), None, false).is_err(),
        "cli input must be an object",
    );
}
