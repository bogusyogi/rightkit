// rightkit-code-audit-provider-sdk consumed by the exact registry pin, outside the workspace.
// Proves: the environment secret provider never prints its value and refuses an empty name.
use rightkit_code_audit_provider_sdk::EnvironmentSecretProvider;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let provider = EnvironmentSecretProvider::new("RIGHTKIT_CONSUMER_SECRET").expect("valid name");
    let debug = format!("{provider:?}");
    check(
        debug.contains("RIGHTKIT_CONSUMER_SECRET"),
        "debug names the variable",
    );
    check(debug.contains("[REDACTED]"), "debug redacts the value");
    check(
        EnvironmentSecretProvider::new("").is_err(),
        "empty variable name refused",
    );
}
