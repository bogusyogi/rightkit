// rightkit-mail-store 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the mutation-class vocabulary and the schema constants:
// - safety::MutationClass has two distinct classes, safe and reversible;
// - safety::DESTRUCTIVE_FLAG names the remote-destruction switch that stays off by default;
// - db_schema::SCHEMA_V13 is a non-empty schema script.
// safety::guard, attach_cache::clear_account and the database open paths are not called: they
// write audit rows and delete cached attachments on disk. No network, no process spawn.
// Exits 1 on the first failed check.
use rightkit_mail_store::db_schema::SCHEMA_V13;
use rightkit_mail_store::safety::{MutationClass, DESTRUCTIVE_FLAG};

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
        MutationClass::SafeMutation != MutationClass::ReversibleMutation,
        "safe and reversible mutations are distinct classes",
    );
    check(
        DESTRUCTIVE_FLAG == "destructive_remote_actions_enabled",
        "the destructive remote-action flag has its reviewed name",
    );
    check(!SCHEMA_V13.trim().is_empty(), "schema version 13 is a non-empty script");
    println!("OK rightkit-mail-store 0.1.0 (registry consumer)");
}
