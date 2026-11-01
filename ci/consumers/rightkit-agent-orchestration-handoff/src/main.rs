// rightkit-agent-orchestration-handoff consumed by the exact registry pin, outside the workspace.
// Proves: the neutral handoff host builds script commands, a host cannot be replaced once read,
// and the character tokenizer counts deterministically.
use rightkit_agent_orchestration_handoff::{
    handoff_host, set_handoff_host, CharTokenizer, HandoffHost, Tokenizer,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let host = handoff_host();
    check(
        host.script_command("handoff/validate-handoff")
            == "rightkit script handoff/validate-handoff",
        "neutral script command",
    );
    check(
        !set_handoff_host(HandoffHost::default()),
        "handoff host is fixed after first use",
    );
    check(
        CharTokenizer.count("abcd") == 4,
        "character tokenizer counts characters",
    );
}
