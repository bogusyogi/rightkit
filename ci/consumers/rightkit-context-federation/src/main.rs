//! Registry consumer for rightkit-context-federation 0.1.0: builds against the published API only.
//! It does not start providers; the crate's own tests drive fixtures.
use rightkit_context_federation::{FederationConfig, FederationEngine, FEDERATION_CONFIG_SCHEMA_VERSION};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let config = FederationConfig::default();
    check(config.schema_version == FEDERATION_CONFIG_SCHEMA_VERSION, "the default config is the current schema version");
    check(config.validate().is_ok(), "the default config validates");
    let _ = std::mem::size_of::<FederationEngine>();
    println!("rightkit-context-federation consumer: all checks passed");
}
