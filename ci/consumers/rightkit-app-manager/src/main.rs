//! Registry consumer for rightkit-app-manager, built against the candidate registry archive.
use rightkit_app_manager::apps::{compare_versions, valid_bundle_id, VersionComparison};
use rightkit_app_manager::{set_account_home_resolver, set_host_identity, HostIdentity};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        valid_bundle_id("com.example.app") && !valid_bundle_id("bad..id"),
        "bundle ids are validated",
    );
    check(
        compare_versions("1.2.0", "1.10.0") == VersionComparison::Newer,
        "1.10.0 is newer than 1.2.0",
    );
    let me = HostIdentity {
        bundle_id_prefix: "com.example.consumer".into(),
        name_prefix: "ConsumerCheck".into(),
        display_name: "ConsumerCheck".into(),
    };
    check(
        set_host_identity(me.clone()),
        "the host app names itself once",
    );
    check(!set_host_identity(me), "a second identity is refused");
    check(
        set_account_home_resolver(std::path::PathBuf::new),
        "the account-home resolver can be registered on this host",
    );
    println!("rightkit-app-manager consumer: all checks passed");
}
