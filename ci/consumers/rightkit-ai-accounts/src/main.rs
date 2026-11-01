//! Registry consumer for rightkit-ai-accounts 0.1.0: builds against the published API only.
use rightkit_ai_accounts::{cli_accounts, desktop_roots};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        cli_accounts::valid_name("work") && !cli_accounts::valid_name(".hidden"),
        "account names are validated",
    );
    check(
        desktop_roots::profile_label("claude-b-home") == "Claude B",
        "a profile folder gets a readable label",
    );
    check(
        cli_accounts::default_base().is_err(),
        "no host state folder means no account storage",
    );
    println!("rightkit-ai-accounts consumer: all checks passed");
}
