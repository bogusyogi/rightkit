// rightkit-local-gen 0.1.3 consumed by exact registry pin (default features), outside the workspace.
// Cheap checks on the pure public surface; no model files, no sd-server, no network.
use rightkit_local_gen::{host, memory, model_store, profiles, roots};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let cfg = host::HostConfig::default();
    check(
        cfg.env_prefix == "RIGHTKIT",
        "default host config is product-neutral",
    );
    check(
        host::env_name("MODEL_ROOT") == "RIGHTKIT_MODEL_ROOT",
        "env_name prefixes",
    );

    check(
        matches!(memory::parse_requested("auto"), Ok(None)),
        "memory auto parses to None",
    );
    check(
        memory::Mode::ALL
            .iter()
            .all(|m| memory::parse_requested(m.as_str()) == Ok(Some(*m))),
        "memory modes round-trip",
    );
    check(
        memory::parse_requested("turbo").is_err(),
        "unknown memory mode refused",
    );

    check(
        model_store::sdcpp_revision(3) == "r3",
        "sdcpp revision label",
    );
    check(
        model_store::sdcpp_quant("x-q6") == "Q6_K",
        "sdcpp quant label",
    );

    let env = roots::EnvValues {
        workspace_root: Some("/ws".into()),
        ..Default::default()
    };
    check(
        roots::workspace_root(&env) == "/ws",
        "workspace root from env values",
    );
    check(
        roots::content_root(&env) == "/ws/Content",
        "content root default",
    );
    check(roots::tools_root(&env) == "/ws/tools", "tools root default");

    let shipped: serde_json::Value =
        serde_json::from_str(profiles::CATALOGUE_JSON).expect("the shipped catalogue parses");
    let parsed = profiles::catalogue().expect("catalogue");
    check(
        shipped["profiles"].as_array().map(Vec::len) == Some(parsed.profiles.len())
            && !parsed.profiles.is_empty(),
        "CATALOGUE_JSON is the catalogue profiles::catalogue parses",
    );
}
