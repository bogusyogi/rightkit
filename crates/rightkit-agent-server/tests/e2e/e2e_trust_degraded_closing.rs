//! D2 closing e2e: `coderight trust [path]` persists trust in the canonical
//! workspace-trust store used by the engine and API.

use crate::support::e2e_cli;
use e2e_cli::*;

#[test]
fn trust_command_persists_only_selected_workspace_in_existing_store() {
    let provider = Provider::start(vec![]);
    let cli = Cli::new(&provider);
    let workspace = cli.ws().canonicalize().unwrap();
    let workspace_arg = workspace.to_string_lossy().to_string();

    let out = cli.exec(&["trust", &workspace_arg], &[]);
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("Trusted workspace"), "{}", out.stdout);

    let trust_path = crate::common::app_root(cli.home.path()).join("workspace-trust.json");
    let store: Value = serde_json::from_slice(&std::fs::read(&trust_path).unwrap()).unwrap();
    let projects = store["projects"].as_object().expect("workspace records");
    let key = workspace_arg.replace('\\', "/");
    assert_eq!(
        projects.len(),
        1,
        "unrelated workspace records are preserved/absent: {store}"
    );
    let record = projects.get(&key).expect("canonical workspace record");
    assert_eq!(record["trust"], "trusted");
    assert_eq!(record["approvedBy"], "user");
    assert!(record["updatedAt"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(provider.count(), 0, "trust command does not invoke a model");
}
