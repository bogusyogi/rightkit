//! Registry consumer for rightkit-prompt-recipes 0.1.0: public API only, no network, no assets.
use rightkit_prompt_recipes::{brands, commands, dispatch, Context};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let names = commands();
    check(
        names.contains(&"studio.brands.list") && names.contains(&"studio.recipes.compose"),
        "the command list names the brand and recipe commands",
    );
    check(
        brands::infer_brand_code("/nowhere/at/all") == "NA",
        "an unknown location gets the default brand code",
    );
    let dir = std::env::temp_dir().join(format!(
        "rightkit-prompt-recipes-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let ctx = Context::new(dir.clone());
    let list = dispatch(&ctx, "studio.brands.list", &serde_json::json!({}));
    check(
        matches!(list, Some(Ok(_))),
        "brands.list answers from the embedded catalogue",
    );
    check(
        dispatch(&ctx, "studio.nothing.here", &serde_json::json!({})).is_none(),
        "an unknown command is not routed",
    );
    let _ = std::fs::remove_dir_all(dir);
}
