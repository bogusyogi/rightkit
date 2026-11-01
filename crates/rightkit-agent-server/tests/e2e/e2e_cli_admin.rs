//! Admin verbs of the `coderight` CLI that edit daemon-owned state: `mcp`
//! (add-json/get/list/status/login/remove) and `plugin`
//! (install/list/disable/enable/reload), plus the "no daemon" failure contract.
//! Every command is a real child process talking to a REAL `coderight serve`.
//!
//! Retires `main_runtime_cli_management_tests`, `main_runtime_mcp_catalog_tests`
//! and the mcp/plugin clap tests in `cli_definitions_tests_*`.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};

fn text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn err(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn diag(out: &std::process::Output) -> String {
    format!(
        "code={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        text(out),
        err(out)
    )
}

fn server_row<'a>(list: &'a Value, name: &str) -> Option<&'a Value> {
    list["servers"]
        .as_array()?
        .iter()
        .find(|s| s["name"] == name)
}

#[test]
fn mcp_add_get_list_login_remove_round_trip_persists_to_the_user_config() {
    let rig = Rig::start(vec![]);
    let spec = rig.home.path().join("server-spec.json");
    std::fs::write(
        &spec,
        json!({
            "scope": "user",
            "name": "fixture-srv",
            "config": {"type": "stdio", "command": "node", "args": ["server.js", "--flag"]}
        })
        .to_string(),
    )
    .unwrap();

    let add = rig.cli(&["mcp", "add-json", spec.to_str().unwrap()]);
    assert_eq!(add.status.code(), Some(0), "{}", diag(&add));
    let saved = stdout_json(&add);
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["scope"], "user");
    assert_eq!(saved["name"], "fixture-srv");
    let config_path = std::path::PathBuf::from(saved["path"].as_str().expect("config path"));
    assert!(
        config_path.starts_with(rig.home.path()),
        "config lives in the isolated home: {config_path:?}"
    );
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(on_disk["mcpServers"]["fixture-srv"]["command"], "node");

    let list = stdout_json(&rig.cli(&["mcp", "list"]));
    let row = server_row(&list, "fixture-srv").expect("listed");
    assert_eq!(row["scope"], "user");
    assert_eq!(row["type"], "stdio");
    assert_eq!(row["config"]["args"], json!(["server.js", "--flag"]));
    assert!(
        row["trust"]["preset"].is_string(),
        "trust preset is inferred or recorded: {row}"
    );

    let get = rig.cli(&["mcp", "get", "fixture-srv"]);
    assert_eq!(get.status.code(), Some(0), "{}", diag(&get));
    assert_eq!(stdout_json(&get)["config"]["command"], "node");

    // A stdio server without OAuth config needs no login; the CLI says so and exits 0.
    let login = rig.cli(&["mcp", "login", "fixture-srv"]);
    assert_eq!(login.status.code(), Some(0), "{}", diag(&login));
    assert!(
        text(&login).contains("does not require login"),
        "{}",
        diag(&login)
    );
    let unknown_login = rig.cli(&["mcp", "login", "ghost"]);
    assert_eq!(
        unknown_login.status.code(),
        Some(1),
        "{}",
        diag(&unknown_login)
    );
    assert!(
        err(&unknown_login).contains("no such server configured: ghost"),
        "{}",
        diag(&unknown_login)
    );

    let remove = rig.cli(&["mcp", "remove", "fixture-srv"]);
    assert_eq!(remove.status.code(), Some(0), "{}", diag(&remove));
    let removed = stdout_json(&remove);
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["name"], "fixture-srv");
    assert!(server_row(&stdout_json(&rig.cli(&["mcp", "list"])), "fixture-srv").is_none());
    // Removing an absent server is idempotent: success with removed=false.
    assert_eq!(
        stdout_json(&rig.cli(&["mcp", "remove", "fixture-srv"]))["removed"],
        false
    );

    let missing = rig.cli(&["mcp", "get", "fixture-srv"]);
    assert_eq!(missing.status.code(), Some(1), "{}", diag(&missing));
    assert!(
        err(&missing).contains("No MCP server named fixture-srv."),
        "{}",
        diag(&missing)
    );
}

#[test]
fn mcp_rejects_bad_scope_and_unsafe_names_with_400_and_leaves_config_untouched() {
    let rig = Rig::start(vec![]);
    for (label, body) in [
        (
            "scope",
            json!({"scope": "galaxy", "name": "ok-name", "config": {"type": "stdio", "command": "node"}}),
        ),
        (
            "name",
            json!({"scope": "user", "name": "bad name!", "config": {"type": "stdio", "command": "node"}}),
        ),
    ] {
        let spec = rig.home.path().join(format!("bad-{label}.json"));
        std::fs::write(&spec, body.to_string()).unwrap();
        let out = rig.cli(&["mcp", "add-json", spec.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1), "{label}: {}", diag(&out));
        assert!(
            err(&out).contains("400") && err(&out).contains("mcp add"),
            "{label}: {}",
            diag(&out)
        );
    }
    let list = stdout_json(&rig.cli(&["mcp", "list"]));
    assert!(
        list["servers"].as_array().unwrap().is_empty(),
        "no partial writes: {list}"
    );

    let (status, _) = rig.post("/mcp/servers", json!({"scope": "user", "name": "x", "config": {"type": "stdio", "command": "n"}, "trustPreset": "not-a-preset"}));
    assert_eq!(status, 400, "unknown trust presets are refused");

    let unreadable = rig.cli(&[
        "mcp",
        "add-json",
        rig.home.path().join("missing.json").to_str().unwrap(),
    ]);
    assert_eq!(unreadable.status.code(), Some(1), "{}", diag(&unreadable));
}

#[test]
fn mcp_status_reports_the_stdio_server_tool_partition() {
    let rig = Rig::start(vec![]);
    let status = stdout_json(&rig.cli(&["mcp", "status"]));
    assert_eq!(status["command"], "coderight mcp-server");
    assert_eq!(status["defaultMode"], "read_only");
    assert_eq!(status["mutatingRequiresOptIn"], true);
    assert_eq!(
        status["mutatingCommand"],
        "coderight mcp-server --allow-mutate"
    );
    let ro = status["readOnlyTools"].as_array().unwrap();
    let mu = status["mutatingTools"].as_array().unwrap();
    assert_eq!(
        status["readOnlyToolCount"].as_u64().unwrap() as usize,
        ro.len()
    );
    assert_eq!(
        status["mutatingToolCount"].as_u64().unwrap() as usize,
        mu.len()
    );
    assert_eq!(
        status["totalToolCount"].as_u64().unwrap() as usize,
        ro.len() + mu.len()
    );
    assert!(ro.iter().any(|t| t == "read_file"));
    assert!(mu.iter().any(|t| t == "write_file"));
    assert!(
        !ro.iter().any(|t| t == "write_file"),
        "mutating tools never appear in the read-only set"
    );
}

fn plugin_source(root: &std::path::Path, id: &str, version: &str) -> std::path::PathBuf {
    let dir = root.join(format!("src-{id}-{version}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        json!({
            "id": id, "name": id, "version": version, "description": "cli admin fixture",
            "entry": "plugin.js", "provides_tools": [], "provides_commands": ["adm"], "enabled": true
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("plugin.js"),
        "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>console.log(JSON.stringify({content:'ok',isError:false})));",
    )
    .unwrap();
    dir
}

#[test]
fn plugin_install_list_disable_enable_reload_and_overwrite_rules() {
    let rig = Rig::start(vec![]);
    let src = plugin_source(rig.home.path(), "adm-plugin", "1.0.0");

    let install = rig.cli(&["plugin", "install", src.to_str().unwrap()]);
    assert_eq!(install.status.code(), Some(0), "{}", diag(&install));
    let v = stdout_json(&install);
    assert_eq!(v["installed"], true);
    assert_eq!(v["overwritten"], false);
    assert_eq!(
        v["signatureStatus"], "unsigned",
        "local installs carry no publisher signature"
    );
    assert_eq!(v["plugin"]["id"], "adm-plugin");
    let installed = std::path::PathBuf::from(v["installedPath"].as_str().unwrap());
    assert!(
        installed.join("plugin.json").is_file(),
        "plugin tree copied to {installed:?}"
    );
    assert!(installed.starts_with(rig.home.path()));

    // Same id again without --overwrite is a conflict, not a silent replace.
    let dup = rig.cli(&["plugin", "install", src.to_str().unwrap()]);
    assert_eq!(dup.status.code(), Some(1), "{}", diag(&dup));
    assert!(err(&dup).contains("409"), "{}", diag(&dup));
    let v2_src = plugin_source(rig.home.path(), "adm-plugin", "2.0.0");
    let over = rig.cli(&["plugin", "install", v2_src.to_str().unwrap(), "--overwrite"]);
    assert_eq!(over.status.code(), Some(0), "{}", diag(&over));
    assert_eq!(stdout_json(&over)["overwritten"], true);

    let list = stdout_json(&rig.cli(&["plugin", "list"]));
    let plugins = list["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1, "{list}");
    assert_eq!(plugins[0]["id"], "adm-plugin");
    assert_eq!(
        plugins[0]["version"], "2.0.0",
        "overwrite replaced the tree"
    );
    assert_eq!(plugins[0]["enabled"], true);

    let off = stdout_json(&rig.cli(&["plugin", "disable", "adm-plugin"]));
    assert_eq!(off["id"], "adm-plugin");
    assert_eq!(off["enabled"], false);
    assert_eq!(
        stdout_json(&rig.cli(&["plugin", "list"]))["plugins"][0]["enabled"],
        false
    );
    let on = stdout_json(&rig.cli(&["plugin", "enable", "adm-plugin"]));
    assert_eq!(on["enabled"], true);

    let reload = stdout_json(&rig.cli(&["plugin", "reload"]));
    assert_eq!(reload["reloaded"], true);
    assert_eq!(reload["count"], 1);

    let bad = rig.cli(&[
        "plugin",
        "install",
        rig.home.path().join("does-not-exist").to_str().unwrap(),
    ]);
    assert_eq!(bad.status.code(), Some(1), "{}", diag(&bad));
    assert!(err(&bad).contains("400"), "{}", diag(&bad));
    let ghost = rig.cli(&["plugin", "enable", "ghost-plugin"]);
    assert_eq!(ghost.status.code(), Some(1), "{}", diag(&ghost));
}

#[test]
fn daemon_verbs_fail_with_a_clear_message_and_exit_1_when_no_daemon_is_discoverable() {
    // No daemon is started: an isolated home with autostart disabled.
    let home = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    for args in [
        vec!["kill", "any"],
        vec!["export", "any"],
        vec!["mcp", "list"],
        vec!["session", "set", "any", "--title", "t"],
    ] {
        let out = isolated_cli(home.path(), cwd.path())
            .args(&args)
            .output()
            .expect("run coderight");
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", diag(&out));
        assert!(
            err(&out).contains("No running daemon found"),
            "{args:?}: {}",
            diag(&out)
        );
        assert!(
            text(&out).trim().is_empty(),
            "{args:?}: failures keep stdout clean"
        );
    }
}
