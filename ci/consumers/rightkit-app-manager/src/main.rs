//! Registry consumer for rightkit-app-manager 0.1.0: builds against the published API only.
use rightkit_app_manager::apps::{compare_versions, valid_bundle_id, VersionComparison};
use rightkit_app_manager::{set_host_identity, HostIdentity};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(valid_bundle_id("com.example.app") && !valid_bundle_id("bad..id"), "bundle ids are validated");
    check(compare_versions("1.2.0", "1.10.0") == VersionComparison::Newer, "1.10.0 is newer than 1.2.0");
    let me = HostIdentity {
        bundle_id_prefix: "com.example.consumer".into(),
        name_prefix: "ConsumerCheck".into(),
        display_name: "ConsumerCheck".into(),
    };
    check(set_host_identity(me.clone()), "the host app names itself once");
    check(!set_host_identity(me), "a second identity is refused");
    println!("rightkit-app-manager consumer: all checks passed");
}
