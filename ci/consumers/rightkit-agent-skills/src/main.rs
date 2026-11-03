// rightkit-agent-skills 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the pure routing, trigger and argument rules:
// - PathAffinityRouter maps a glob to a skill slug, routes a matching path, refuses a bad glob;
// - evaluate_trigger evaluates a quoted equality over a TriggerContext;
// - substitute_arguments fills $1..$n and $ARGUMENTS and leaves $0 and a bare $ alone;
// - split_args honours simple quotes (the quoted group is one argument).
// No I/O, no process spawn, no network. Exits 1 on the first failed check.
use rightkit_agent_skills::custom_commands::{split_args, substitute_arguments};
use rightkit_agent_skills::{evaluate_trigger, PathAffinityRouter, TriggerContext};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let mut router = PathAffinityRouter::new();
    check(
        router.add_rule("**/*.rs", "rust-review").is_ok(),
        "a glob rule registers",
    );
    check(
        router.route("src/main.rs") == vec!["rust-review".to_string()],
        "a Rust path routes to its skill",
    );
    check(
        router.route("README.md").is_empty(),
        "a non-matching path routes nowhere",
    );
    check(
        router.rules().count() == 1,
        "rules() lists the one registered pair",
    );
    check(
        router.add_rule("[", "broken").is_err(),
        "an unclosed glob class is refused",
    );

    let mut ctx = TriggerContext::new();
    ctx.set("event", "PreToolUse");
    check(
        matches!(evaluate_trigger("event == \"PreToolUse\"", &ctx), Ok(true)),
        "equality over the event matches",
    );
    check(
        matches!(evaluate_trigger("event == \"Stop\"", &ctx), Ok(false)),
        "a different event does not match",
    );

    check(
        substitute_arguments("$1-$2", "a b") == "a-b",
        "positional arguments fill $1 and $2",
    );
    check(
        substitute_arguments("[$1]", "") == "[]",
        "a missing positional becomes empty",
    );
    check(
        substitute_arguments("$0 $ x", "a") == "$0 $ x",
        "$0 and a bare $ are left alone",
    );
    check(
        split_args("one \"two words\"") == vec!["one".to_string(), "two words".to_string()],
        "a quoted group is one argument",
    );
    println!("OK rightkit-agent-skills 0.1.0 (registry consumer)");
}
