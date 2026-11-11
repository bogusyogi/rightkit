// rightkit-prompt-recipes 0.2.0 consumed by exact registry pin (`=0.2.0`), outside the workspace.
//
// Proves, through the public API of the default (pure library) build:
// - catalog::reference_limit is a positive count and an unknown intent has no model;
// - contracts::list_contracts names the shipped contracts;
// - the crate ships no brands: catalog::brands() is only `NA` until the host sets its own;
// - after brands::set_brands, the host's code is a catalogue brand and a video venture, and an unknown one is refused.
// No engine, no network, no file, no process spawn. Exits 1 on the first failed check.
use rightkit_prompt_recipes::{brands, catalog, contracts, video_templates};

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
        catalog::reference_limit() > 0,
        "a reference limit is a positive count",
    );
    check(
        catalog::intent_model("no-such-intent-rightkit").is_none(),
        "an unknown intent has no model",
    );
    check(
        !contracts::list_contracts().is_empty(),
        "the shipped prompt contracts are listed",
    );
    check(
        catalog::brands() == vec!["NA".to_string()],
        "no brands ship: only NA",
    );
    brands::set_brands(brands::Brands {
        codes: vec!["acme".into()],
        defaults: serde_json::json!({}),
    })
    .expect("brands set once");
    check(
        catalog::brands().contains(&"ACME".to_string()) && brands::is_known("Acme"),
        "the host's brand is accepted",
    );
    check(
        video_templates::normalize_venture("ACME").ok().as_deref() == Some("acme")
            && video_templates::normalize_venture("zz").is_err(),
        "a video venture is a host brand",
    );
    println!("OK rightkit-prompt-recipes 0.2.0 (registry consumer)");
}
