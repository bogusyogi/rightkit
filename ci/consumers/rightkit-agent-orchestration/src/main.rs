// rightkit-agent-orchestration 0.1.0 consumed by exact registry pin: builtin agent roster and catalog. No I/O.
use rightkit_agent_orchestration::catalog::roster_entries;
use rightkit_agent_orchestration::registry::AgentRegistry;

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
        roster_entries(&AgentRegistry::new()).is_empty(),
        "an empty registry has no roster",
    );
    check(
        !roster_entries(&AgentRegistry::with_builtins()).is_empty(),
        "builtin agents are listed",
    );
}
