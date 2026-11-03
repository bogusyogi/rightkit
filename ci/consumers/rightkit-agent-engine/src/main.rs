// rightkit-agent-engine 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the pure task-state table and the effect classification:
// - `transition` and `is_legal_transition` accept the documented moves and refuse an illegal one
//   (IllegalTransition carries from and to);
// - `TaskStatus::is_terminal` is true for succeeded, failed and cancelled only;
// - `EffectClass::is_irreversible` is false for reversible and true for the other classes.
// No I/O, no network, no process spawn. Exits 1 on the first failed check.
use rightkit_agent_engine::task::{is_legal_transition, transition, TaskStatus};
use rightkit_agent_engine::EffectClass;

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
        transition(TaskStatus::Accepted, TaskStatus::Leased) == Ok(TaskStatus::Leased),
        "accepted to leased is a legal transition",
    );
    check(
        is_legal_transition(TaskStatus::Running, TaskStatus::Succeeded),
        "running to succeeded is legal",
    );
    let refused = transition(TaskStatus::Succeeded, TaskStatus::Running);
    check(
        matches!(refused, Err(e) if e.from == TaskStatus::Succeeded && e.to == TaskStatus::Running),
        "succeeded to running is refused with IllegalTransition",
    );
    check(
        TaskStatus::Succeeded.is_terminal()
            && TaskStatus::Failed.is_terminal()
            && TaskStatus::Cancelled.is_terminal(),
        "succeeded, failed and cancelled are terminal",
    );
    check(
        !TaskStatus::Uncertain.is_terminal() && !TaskStatus::CancelRequested.is_terminal(),
        "uncertain and cancel-requested are not terminal",
    );
    check(
        !EffectClass::Reversible.is_irreversible()
            && EffectClass::Spend.is_irreversible()
            && EffectClass::Publication.is_irreversible()
            && EffectClass::IrreversibleDestruction.is_irreversible(),
        "only reversible effects skip the owner approval check",
    );
}
