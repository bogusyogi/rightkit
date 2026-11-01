//! Journeys: skills, slash-command resolution/precedence, lifecycle hooks and
//! plugin load/disable/reload, through a real `coderight serve` daemon.
//! Retires the skills/hooks/plugins/recipes unit tests.

use crate::support::e2e_orch;
use e2e_orch::*;

use serde_json::json;

fn write(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Plugin whose single command/tool answers with `reply` (node entry, JSON on stdin/stdout).
fn write_plugin(home: &std::path::Path, id: &str, command: &str, tool: &str, reply: &str) {
    let dir = crate::common::app_root(&home).join("plugins").join(id);
    write(
        &dir.join("plugin.json"),
        &json!({
            "id": id, "name": id, "version": "1.0.0", "description": "e2e fixture",
            "entry": "plugin.js", "provides_tools": [tool],
            "provides_commands": [command], "enabled": true
        })
        .to_string(),
    );
    write(
        &dir.join("plugin.js"),
        &format!(
            "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{{\
             const p=JSON.parse(d);\
             console.log(JSON.stringify({{content:'{reply}:'+p.kind+':'+p.name,isError:false}}));}});"
        ),
    );
}

fn command_names(env: &Env) -> Vec<String> {
    let (_, rows) = env.get("/commands");
    rows.as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str().map(String::from))
        .collect()
}

#[test]
fn invoke_skill_resolves_flat_and_folder_skills_and_reports_unknown() {
    let skills_dir = temp_dir("skills-env");
    let skills_env = skills_dir.path().to_string_lossy().to_string();
    let provider = Provider::start(|body, _| match tool_results(body).len() {
        0 => Reply::tool(
            "s1",
            "invoke_skill",
            json!({"skill":"reviewer","args":{"focus":"auth"}}),
        ),
        1 => Reply::tool("s2", "invoke_skill", json!({"skill":"folder-skill"})),
        2 => Reply::tool("s3", "invoke_skill", json!({"skill":"nope-missing"})),
        3 => Reply::tool("s4", "invoke_skill", json!({"skill":"../escape"})),
        _ => Reply::text("skills done"),
    });
    let env = Env::start(
        provider,
        &[("RIGHTKIT_SKILLS_DIR", &skills_env)],
        |home, _| {
            // Env-dir skill and home skill share a slug: the env dir must win (precedence).
            write(
                &std::path::Path::new(&skills_env).join("reviewer.md"),
                "# Reviewer FROM-ENV-DIR",
            );
            write(
                &crate::common::app_root(&home).join("skills/reviewer.md"),
                "# Reviewer FROM-HOME",
            );
            write(
                &crate::common::app_root(&home).join("skills/folder-skill/SKILL.md"),
                "---\nname: folder-skill\ndescription: folder form\n---\n\n# Folder FROM-FOLDER",
            );
        },
    );
    env.create_session_with("sk", json!({"permissionMode":"bypass"}));
    env.say("sk", "use skills");
    assert_eq!(env.wait_turns("sk", 1), "completed");
    let results: Vec<String> = env
        .provider
        .requests()
        .iter()
        .flat_map(tool_results)
        .collect();
    let joined = results.join("\n----\n");
    assert!(
        joined.contains("FROM-ENV-DIR") && !joined.contains("FROM-HOME"),
        "precedence: {joined}"
    );
    assert!(
        joined.contains("\"focus\":\"auth\""),
        "args forwarded: {joined}"
    );
    assert!(
        joined.contains("FROM-FOLDER"),
        "folder SKILL.md form: {joined}"
    );
    assert!(joined.contains("Skill not found: nope-missing"), "{joined}");
    assert!(
        joined.contains("invalid skill slug"),
        "path traversal must be refused: {joined}"
    );
}

#[test]
fn slash_commands_resolve_aliases_builtin_beats_plugin_and_unknown_is_reported() {
    let provider = Provider::start(|_, _| Reply::text("ok"));
    let env = Env::start(provider, &[], |home, _| {
        write_plugin(home, "echoer", "echoit", "echo_tool", "PLUGIN-ANSWER");
        // A plugin that tries to shadow a built-in command.
        write_plugin(home, "shadow", "help", "shadow_tool", "SHADOWED");
    });
    let names = command_names(&env);
    assert!(
        names.contains(&"echoit".to_string()),
        "plugin command listed: {names:?}"
    );

    let (ok, out) = env.slash("/echoit hello");
    assert!(ok, "{out}");
    assert!(out.contains("PLUGIN-ANSWER:command:echoit"), "{out}");

    let (ok, out) = env.slash("/help");
    assert!(
        ok && !out.contains("SHADOWED"),
        "built-in must win over plugin: {out}"
    );

    let (ok, out) = env.slash("/definitely-not-a-command");
    assert!(!ok && out.contains("Unknown command"), "{out}");
    let (ok, out) = env.slash("not a slash command");
    assert!(!ok && out.contains("Not a slash command"), "{out}");
}

#[test]
fn plugin_disable_reload_and_remove_leave_no_stale_commands_or_tools() {
    let provider = Provider::start(|_, _| Reply::text("ok"));
    let env = Env::start(provider, &[], |home, _| {
        write_plugin(home, "echoer", "echoit", "echo_tool", "V1");
    });
    let (_, listed) = env.get("/plugins");
    assert_eq!(listed["plugins"].as_array().unwrap().len(), 1, "{listed}");
    assert!(env.slash("/echoit").1.contains("V1:"));

    // Disable: command disappears from listing and execution immediately.
    let (s, b) = env.post("/plugins/echoer/disable", json!({}));
    assert!((200..300).contains(&s), "{s} {b}");
    assert!(
        !command_names(&env).contains(&"echoit".to_string()),
        "stale command after disable"
    );
    let (ok, out) = env.slash("/echoit");
    assert!(!ok && out.contains("Unknown command"), "{out}");
    let (s, b) = env.post("/plugins/echoer/enable", json!({}));
    assert!((200..300).contains(&s), "{s} {b}");
    assert!(env.slash("/echoit").1.contains("V1:"));

    // Replace the plugin on disk and reload: new code runs, nothing from V1 lingers.
    write_plugin(env.home.path(), "echoer", "echoit2", "echo_tool", "V2");
    let (s, b) = env.post("/plugins/reload", json!({}));
    assert!((200..300).contains(&s), "{s} {b}");
    assert_eq!(b["count"], 1, "{b}");
    let names = command_names(&env);
    assert!(
        names.contains(&"echoit2".to_string()) && !names.contains(&"echoit".to_string()),
        "{names:?}"
    );
    assert!(env.slash("/echoit2").1.contains("V2:"));

    // Remove from disk and reload: plugin is gone entirely.
    std::fs::remove_dir_all(crate::common::app_root(env.home.path()).join("plugins/echoer"))
        .unwrap();
    let (_, b) = env.post("/plugins/reload", json!({}));
    assert_eq!(b["count"], 0, "{b}");
    assert!(!command_names(&env).contains(&"echoit2".to_string()));
    let (_, listed) = env.get("/plugins");
    assert!(listed["plugins"].as_array().unwrap().is_empty(), "{listed}");
}

#[test]
fn plugin_tool_is_discovered_called_and_vanishes_for_new_sessions_after_disable() {
    let provider = Provider::start(|body, _| {
        let results = tool_results(body);
        match results.len() {
            0 => Reply::tool("d1", "tool_search", json!({"query":"echo_tool"})),
            1 => Reply::tool("p1", "echo_tool", json!({"x":1})),
            _ => Reply::text("plugin tool done"),
        }
    });
    let env = Env::start(provider, &[], |home, _| {
        write_plugin(home, "echoer", "echoit", "echo_tool", "TOOL-V1");
    });
    env.create_session_with("pt", json!({"permissionMode":"bypass"}));
    env.say("pt", "use the plugin tool");
    assert_eq!(env.wait_turns("pt", 1), "completed");
    let seen: Vec<String> = env
        .provider
        .requests()
        .iter()
        .flat_map(tool_results)
        .collect();
    assert!(
        seen.iter().any(|r| r.contains("TOOL-V1:tool:echo_tool")),
        "plugin tool never ran: {seen:?}"
    );

    env.post("/plugins/echoer/disable", json!({}));
    let before = env.provider.count();
    env.create_session_with("pt2", json!({"permissionMode":"bypass"}));
    env.say("pt2", "second session");
    env.wait_turns("pt2", 1);
    for r in &env.provider.requests()[before..] {
        assert!(
            !offered_tools(r).contains(&"echo_tool".to_string()),
            "disabled plugin tool still offered to the model"
        );
    }
}

#[test]
fn lifecycle_hooks_fire_in_order_and_pre_tool_hook_can_deny() {
    let log_dir = temp_dir("hooklog");
    let log = slash_path(&log_dir.path().join("hooks.log"));
    let script = slash_path(&log_dir.path().join("hook.js"));
    write(
        &log_dir.path().join("hook.js"),
        "const fs=require('fs');const e=process.env;\
         fs.appendFileSync(process.argv[2],(e.RIGHTKIT_HOOK_EVENT||'')+'|'+(e.RIGHTKIT_HOOK_TOOL||'')+'\\n');\
         if(e.RIGHTKIT_HOOK_EVENT==='PreToolUse'&&e.RIGHTKIT_HOOK_TOOL==='get_time'){console.log('DENIED-BY-E2E-HOOK');process.exit(2);}",
    );
    let cmd = format!("node {script} {log}");
    let provider = Provider::start(|body, _| {
        if tool_results(body).is_empty() {
            Reply::tool(
                "t1",
                "get_time",
                json!({"operation":"current_time","timezone":"UTC"}),
            )
        } else {
            Reply::text("after hook")
        }
    });
    let env = Env::start(
        provider,
        &[
            ("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT", &cmd),
            ("RIGHTKIT_HOOK_PRE_TOOL_USE", &cmd),
            ("RIGHTKIT_HOOK_POST_TOOL_USE", &cmd),
        ],
        |_, _| {},
    );

    // Operator surface: configured hooks are listed.
    let (_, listed) = env.get("/hooks/list");
    assert!(listed["configured"].as_i64().unwrap() >= 3, "{listed}");

    env.create_session_with("hk", json!({"permissionMode":"bypass"}));
    env.say("hk", "fire the hooks");
    assert_eq!(env.wait_turns("hk", 1), "completed");
    let lines = wait_for("hook log", 15, || {
        let t = std::fs::read_to_string(log_dir.path().join("hooks.log")).ok()?;
        (t.contains("PreToolUse")).then_some(t)
    });
    assert!(lines.contains("UserPromptSubmit"), "{lines}");
    assert!(lines.contains("PreToolUse|get_time"), "{lines}");
    // Exit code 2 from the pre-tool hook denies the tool: the model is handed the hook's stdout.
    let seen: Vec<String> = env
        .provider
        .requests()
        .iter()
        .flat_map(tool_results)
        .collect();
    assert!(
        seen.iter().any(|r| r.contains("DENIED-BY-E2E-HOOK")),
        "tool not denied by the hook: {seen:?}"
    );
    assert!(
        !lines.contains("PostToolUse|get_time"),
        "denied tool must not post-fire: {lines}"
    );
}
