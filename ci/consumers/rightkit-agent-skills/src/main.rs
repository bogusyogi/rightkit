// rightkit-agent-skills 0.1.0 consumed by exact registry pin: the shared command grammar. No I/O.
use rightkit_agent_skills::commands::{entries_for_surface, resolve_interactive_effect};

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
        resolve_interactive_effect("clear").map(|s| s.name) == Some("new"),
        "/clear resolves to the new effect",
    );
    check(
        resolve_interactive_effect("nope").is_none(),
        "unknown verbs do not resolve",
    );
    check(
        entries_for_surface(false).count() > 0,
        "interactive surface lists commands",
    );
}
