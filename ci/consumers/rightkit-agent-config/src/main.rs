// rightkit-agent-config 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Parses and merges configuration layers, reads a trust store and presets from explicit files in a
// per-process temp directory, and checks the licence-variable filter and session secrets.
// `state_root`, `save_user`, `update_user*`, `save_workspace`, `is_workspace_trusted` and `load_presets`
// use the real state root and are never called.
use std::collections::HashMap;

use rightkit_agent_config::licence_env::is_licence_env_name;
use rightkit_agent_config::presets::{load_presets_from, resolve_preset};
use rightkit_agent_config::session_secrets::{SessionSecretError, SessionSecrets};
use rightkit_agent_config::{
    extend_dedup, is_workspace_trusted_in, workspace_trust_key, ConfigLoader, HostConfig,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    let loader = ConfigLoader::new();

    // One TOML layer.
    let user = loader
        .load_str("[model]\ndefault-model = \"model-a\"\nfallback-models = [\"b\", \"c\"]\nmax-tokens = 1000\n")
        .unwrap_or_default();
    check(
        "toml layer parses",
        user.model.default_model == "model-a"
            && user.model.fallback_models == ["b", "c"]
            && user.model.max_tokens == 1000,
    );
    check(
        "invalid toml is an error",
        loader.load_str("[model\n").is_err(),
    );

    // Layers fold over the defaults: later scalars win, lists extend without duplicates.
    let workspace = loader
        .load_str("[model]\ndefault-model = \"model-w\"\nfallback-models = [\"c\", \"d\"]\n")
        .unwrap_or_default();
    let merged = loader.load_layered(vec![user.clone(), workspace]);
    check(
        "later layer wins a scalar",
        merged.model.default_model == "model-w",
    );
    check(
        "default overlay keeps the base scalar",
        merged.model.max_tokens == 1000,
    );
    check(
        "lists extend without duplicates",
        merged.model.fallback_models == ["b", "c", "d"],
    );
    check(
        "no layers is the default",
        loader.load_layered(Vec::new()) == HostConfig::default(),
    );
    check(
        "extend_dedup keeps first-seen order",
        extend_dedup(vec![1, 2, 2], vec![3, 1, 4]) == vec![1, 2, 3, 4],
    );

    // Workspace trust from an explicit store file.
    let dir = std::env::temp_dir().join(format!(
        "rightkit-agent-config-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create consumer temp dir");
    check(
        "trust key",
        workspace_trust_key("  C:\\work\\repo ") == "C:/work/repo",
    );
    let store = dir.join("workspace-trust.json");
    std::fs::write(
        &store,
        r#"{"projects":{"/work/trusted":{"trust":"Trusted"},"/work/other":{"trust":"untrusted"}}}"#,
    )
    .expect("write trust store");
    check(
        "trusted workspace",
        is_workspace_trusted_in(&store, "/work/trusted"),
    );
    check(
        "untrusted and unknown workspaces fail closed",
        !is_workspace_trusted_in(&store, "/work/other")
            && !is_workspace_trusted_in(&store, "/work/unknown")
            && !is_workspace_trusted_in(&dir.join("missing.json"), "/work/trusted"),
    );

    // Presets from explicit files.
    let user_presets = dir.join("user-presets.toml");
    let project_presets = dir.join("project-presets.toml");
    std::fs::write(
        &user_presets,
        "[preset.review]\nmodel = \"m1\"\ntools = [\"read_file\"]\n",
    )
    .expect("write user presets");
    std::fs::write(
        &project_presets,
        "[preset.fast]\nmodel = \"m2\"\npermission_mode = \"bypass\"\n",
    )
    .expect("write project presets");
    let untrusted =
        load_presets_from(Some(&user_presets), Some(&project_presets), false).unwrap_or_default();
    check(
        "untrusted project presets are ignored",
        untrusted.len() == 1 && untrusted.contains_key("review"),
    );
    let trusted =
        load_presets_from(Some(&user_presets), Some(&project_presets), true).unwrap_or_default();
    let fast = resolve_preset(&trusted, "fast").unwrap_or_default();
    check(
        "trusted project preset loads without its bypass mode",
        fast.model.as_deref() == Some("m2") && fast.permission_mode.is_none(),
    );
    check(
        "unknown preset is an error",
        resolve_preset(&trusted, "missing").is_err(),
    );

    // Licence and device variables never reach a tool process.
    check(
        "licence variable names",
        is_licence_env_name("RIGHTKIT_LICENSE_DIR")
            && is_licence_env_name("rightkit_device_account")
            && !is_licence_env_name("RIGHTKIT_LOG"),
    );

    // Session secrets: validated keys, names only in Debug, injected into an environment map.
    let mut secrets = SessionSecrets::new();
    check(
        "invalid key refused",
        secrets.set("1BAD", "x") == Err(SessionSecretError::InvalidKey("1BAD".to_string())),
    );
    check(
        "valid key set",
        secrets.set("API_TOKEN", "s3cret-value").is_ok(),
    );
    check(
        "debug prints no value",
        !format!("{secrets:?}").contains("s3cret-value"),
    );
    let mut env = HashMap::from([("API_TOKEN".to_string(), "inherited".to_string())]);
    secrets.inject_into(&mut env);
    check(
        "session secret overrides the inherited value",
        env.get("API_TOKEN").map(String::as_str) == Some("s3cret-value")
            && secrets.key_names() == ["API_TOKEN"],
    );

    let _ = std::fs::remove_dir_all(&dir);
    check("temp directory removed", !dir.exists());

    println!("OK rightkit-agent-config 0.1.0 (registry consumer, public API)");
}
