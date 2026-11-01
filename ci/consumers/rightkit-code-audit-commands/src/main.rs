// rightkit-code-audit-commands consumed by the exact registry pin, outside the workspace.
// Proves: the embedded provider registry parses with providers, the installed share directory is
// host-declared once, and command errors share the evidence-commands codes. No audit is run.
use rightkit_code_audit_commands::commands::coverage::PROVIDER_REGISTRY;
use rightkit_code_audit_commands::commands::{
    installed_share_dir, set_installed_share_dir, CommandError,
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
    let registry: serde_json::Value =
        serde_json::from_str(PROVIDER_REGISTRY).expect("registry JSON");
    check(
        registry["providers"]
            .as_array()
            .is_some_and(|p| !p.is_empty()),
        "embedded provider registry",
    );
    check(
        installed_share_dir().is_none(),
        "no installed share directory by default",
    );
    check(
        set_installed_share_dir(std::env::temp_dir()) && installed_share_dir().is_some(),
        "share directory set once",
    );
    check(CommandError::usage("x").code == 4, "shared usage exit code");
}
