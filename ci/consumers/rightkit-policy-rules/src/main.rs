// rightkit-policy-rules consumed by the exact registry pin, outside the workspace.
// Proves: the policy schema version, the fixed evaluation order length and that it starts and
// ends with distinct stages.
use rightkit_policy_rules::model::POLICY_SCHEMA_VERSION;
use rightkit_policy_rules::rules::precedence::EVALUATION_ORDER;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(POLICY_SCHEMA_VERSION == 1, "policy schema version 1");
    check(EVALUATION_ORDER.len() == 11, "eleven evaluation stages");
    check(
        EVALUATION_ORDER.first() != EVALUATION_ORDER.last(),
        "first and last stages differ",
    );
}
