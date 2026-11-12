// rightkit-agent-server 0.1.2 consumed by exact registry pin (`=0.1.2`), outside the workspace.
//
// `run` is the host binary entry. Calling it starts the agent server, installs crash hooks and
// reads licence locators, and it needs the product's identity, skill catalog and skill host. So
// this consumer names `run` as a function item without calling it, and builds a second app's
// `HostIdentity` (pure values, nothing installed). No I/O, no network, no process spawn. Exits 1
// on a failed check.
use rightkit_agent_server::{HarnessRadarData, HostIdentity};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let entry = rightkit_agent_server::run;
    check(
        std::mem::size_of_val(&entry) == 0,
        "run resolves as a zero-sized function item",
    );

    let derived = HostIdentity::new("example-app");
    check(derived.app() == "example-app", "app name kept");
    check(derived.home_dir() == ".example-app", "state folder derives from the app name");
    check(derived.config_dir() == "example-app", "config folder derives from the app name");
    check(derived.license_app() == "example-app", "licence app derives from the app name");
    check(
        derived.credential_namespace() == "example-app",
        "credential namespace derives from the app name",
    );
    check(
        derived.github_client_id().is_none() && derived.homepage().is_none(),
        "no Github client id or homepage unless given",
    );
    check(derived.harness_radar().is_none(), "no watch-list unless given");

    let set = HostIdentity::new("example-app")
        .with_home_dir(".example")
        .with_config_dir("example")
        .with_license_app("example-suite")
        .with_credential_namespace("example-creds")
        .with_github_client_id("client-id")
        .with_homepage("https://example.invalid")
        .with_harness_radar(HarnessRadarData {
            sources_toml: String::new(),
            snapshot_yaml: String::new(),
            previous_yaml: String::new(),
        });
    check(
        set.home_dir() == ".example"
            && set.config_dir() == "example"
            && set.license_app() == "example-suite"
            && set.credential_namespace() == "example-creds"
            && set.github_client_id() == Some("client-id")
            && set.homepage() == Some("https://example.invalid")
            && set.harness_radar().is_some(),
        "every name can be set",
    );

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let refused = std::panic::catch_unwind(|| HostIdentity::new("Bad Name"));
    std::panic::set_hook(previous_hook);
    check(refused.is_err(), "an app name that is not a lowercase slug is refused");

    println!("OK rightkit-agent-server 0.1.2 (registry consumer, entry not invoked)");
}
