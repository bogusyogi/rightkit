// rightkit-evidence-effects consumed by the exact registry pin, outside the workspace.
// Proves: effect errors map to stable execution states and display stable codes; sandbox modes are distinct.
use rightkit_evidence_effects::sandbox::SandboxMode;
use rightkit_evidence_effects::{EffectError, ExecutionState};

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
        EffectError::Timeout.state() == ExecutionState::Timeout,
        "timeout maps to its execution state",
    );
    check(
        EffectError::MissingExecutable("x".into()).to_string() == "missing_executable: x",
        "stable error code text",
    );
    check(
        SandboxMode::DenyNetwork != SandboxMode::AllowNetworkDenyProjectWrite,
        "sandbox modes are distinct",
    );
}
