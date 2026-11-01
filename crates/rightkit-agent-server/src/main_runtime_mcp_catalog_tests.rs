// Behavioral tests for the MCP catalog CLI logic.
// Included from main_runtime_mcp_catalog.rs — items and imports are
// provided by the parent `mod mcp_catalog_tests { use super::*; .. }`.

#[test]
fn catalog_lists_curated_entries() {
    let value = mcp_catalog_json();
    let entries = value["catalog"].as_array().expect("catalog array");
    assert!(!entries.is_empty());
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap_or_default())
        .collect();
    assert!(names.contains(&"context7"));
    assert!(names.contains(&"filesystem"));
    assert!(names.contains(&"github"));
    assert!(names.contains(&"memory"));
    assert!(names.contains(&"sequential-thinking"));
}

#[test]
fn catalog_entries_are_stdio_configs() {
    let value = mcp_catalog_json();
    for entry in value["catalog"].as_array().unwrap() {
        assert_eq!(entry["config"]["type"], "stdio");
        assert!(entry["config"]["command"].as_str().is_some());
    }
}

#[test]
fn find_known_entry_succeeds() {
    let entry = find_mcp_catalog_entry("context7");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().name, "context7");
}

#[test]
fn find_unknown_entry_returns_none() {
    assert!(find_mcp_catalog_entry("bogus-server-does-not-exist").is_none());
}

#[test]
fn install_save_body_matches_add_json_shape() {
    let entry = find_mcp_catalog_entry("memory").unwrap();
    let body = mcp_install_save_body(entry, "user", None);
    assert_eq!(body["scope"], "user");
    assert_eq!(body["name"], "memory");
    assert_eq!(body["config"]["type"], "stdio");
    assert_eq!(body["config"]["command"], entry.command);
    assert_eq!(body["workspaceRoot"], "");
}

#[test]
fn install_save_body_carries_workspace_root() {
    let entry = find_mcp_catalog_entry("github").unwrap();
    let body = mcp_install_save_body(entry, "workspace", Some("D:/repo"));
    assert_eq!(body["scope"], "workspace");
    assert_eq!(body["workspaceRoot"], "D:/repo");
}

#[test]
fn catalog_has_no_todo_commands_or_args() {
    for entry in MCP_CATALOG {
        assert_ne!(entry.command, "TODO", "{} has a TODO command", entry.name);
        assert!(
            !entry.command.trim().is_empty(),
            "{} has empty command",
            entry.name
        );
        assert!(
            !entry.description.contains("TODO") && entry.args.iter().all(|a| !a.contains("TODO")),
            "{} still carries TODO text",
            entry.name
        );
    }
}

#[test]
fn github_entry_passes_token_env_at_spawn() {
    let entry = find_mcp_catalog_entry("github").unwrap();
    assert_eq!(entry.command, "github-mcp-server");
    assert_eq!(entry.args, &["stdio"]);
    let body = mcp_install_save_body(entry, "user", None);
    assert_eq!(
        body["config"]["env"]["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
}
