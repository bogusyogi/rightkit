// rightkit-prompt-recipes 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the command table and the embedded catalogue lookups:
// - commands() lists the catalogue's studio command names, including studio.catalog.intents;
// - catalog::COMMANDS is a non-empty static table;
// - catalog::reference_limit is a positive count;
// - catalog::intent_model refuses an unknown intent name.
// No recipe is dispatched: dispatch needs a media-generation Context and would call the generator.
// No network, no file, no process spawn. Exits 1 on the first failed check.
use rightkit_prompt_recipes::catalog;
use rightkit_prompt_recipes::commands;

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
        commands().contains(&"studio.catalog.intents"),
        "commands() lists studio.catalog.intents",
    );
    check(!catalog::COMMANDS.is_empty(), "the catalogue command table is non-empty");
    check(catalog::reference_limit() > 0, "a reference limit is a positive count");
    check(
        catalog::intent_model("no-such-intent-rightkit").is_none(),
        "an unknown intent has no model",
    );
    println!("OK rightkit-prompt-recipes 0.1.0 (registry consumer)");
}
