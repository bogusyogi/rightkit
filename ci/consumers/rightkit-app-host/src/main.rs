//! Registry consumer for rightkit-app-host 0.1.0: builds against the published API only.
use rightkit_app_host::{host, install, setting_bool, AppHost, NeutralHost};
use std::path::PathBuf;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

struct Consumer;
impl AppHost for Consumer {
    fn state_root(&self) -> Option<PathBuf> {
        Some(std::env::temp_dir())
    }
    fn product_name(&self) -> &str {
        "ConsumerCheck"
    }
}

fn main() {
    check(
        NeutralHost.state_root().is_none(),
        "the neutral host keeps no state folder",
    );
    check(
        install(Box::new(Consumer)),
        "the app installs its host once",
    );
    check(!install(Box::new(NeutralHost)), "a second host is refused");
    check(
        host().product_name() == "ConsumerCheck",
        "the installed host answers",
    );
    check(setting_bool("absent", true), "absent switches default on");
    println!("rightkit-app-host consumer: all checks passed");
}
