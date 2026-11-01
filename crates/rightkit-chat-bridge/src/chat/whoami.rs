//! `<cli> chat whoami`: read-only proof trace of who this command runs as. It repeats what
//! `identify` does (this process's ancestry, each ancestor checked against the Claude registry
//! folders and the harness registrations, then `prove_caller`) and says at each step what was
//! found or why it did not count. It sends nothing, writes nothing and changes no decision.
//!
//! It prints no secret: the messaging socket is shown as present or absent plus its last six
//! characters, and session ids are cut to their first eight.

use super::store::Store;
use super::{
    Ancestor, ChatProcess, LocalSession, chat_process, deliver_claude, proof_trace, prove_caller,
    read_ancestry, registry_chat,
};
use serde_json::{Value, json};

/// The first eight characters of an id.
fn head(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The last six characters of a value.
fn tail(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    chars[chars.len().saturating_sub(6)..].iter().collect()
}

/// The trace as JSON (see docs/chat.md, `whoami`).
pub fn report(sessions: &[LocalSession], env: &dyn Fn(&str) -> Option<String>) -> Value {
    let var = |name: &str| {
        env(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let registered = Store::open_default()
        .map(|store| store.registered_sessions())
        .unwrap_or_default();
    let claude_dirs: Vec<std::path::PathBuf> = deliver_claude::sessions_dirs()
        .into_iter()
        .map(|(dir, _)| dir)
        .collect();
    let ancestry = read_ancestry();
    let ancestors: Option<Vec<Ancestor>> = ancestry.as_ref().map(|found| {
        found
            .ancestors
            .iter()
            .map(|a| Ancestor {
                pid: a.pid,
                name: a.name.clone(),
            })
            .collect()
    });
    let steps: Vec<Value> = ancestry
        .iter()
        .flat_map(|found| found.ancestors.iter())
        .map(|step| {
            let ancestor = Ancestor {
                pid: step.pid,
                name: step.name.clone(),
            };
            let (_, notes) = registry_chat(sessions, &claude_dirs, step.pid);
            let matched = match chat_process(sessions, &registered, &claude_dirs, &ancestor) {
                Some(ChatProcess::One(caller)) => {
                    json!({"kind": "claude chat", "id": head(&caller.id), "name": caller.name,
                           "proven": caller.detected})
                }
                Some(ChatProcess::Harness(kind)) => json!({"kind": format!("{kind} harness")}),
                None => Value::Null,
            };
            json!({"pid": step.pid, "name": step.name, "parent": step.parent,
                   "startSecs": step.start, "matched": matched,
                   "whyNot": if matched.is_null() { json!(notes) } else { json!([]) }})
        })
        .collect();
    let socket = var("CLAUDE_CODE_MESSAGING_SOCKET");
    let result = prove_caller(sessions, env, ancestors.as_deref(), &|ancestor| {
        chat_process(sessions, &registered, &claude_dirs, ancestor)
    });
    let result = match result {
        Ok(caller) => json!({"ok": true, "id": head(&caller.id), "name": caller.name,
                             "proven": caller.detected,
                             "replySocketTail": caller.reply_socket.as_deref().map(tail)}),
        Err(error) => {
            let mut message = error.to_string();
            if let Some(socket) = &socket {
                message = message.replace(socket.as_str(), &format!("...{}", tail(socket)));
            }
            // The refusal names the claimed chat by the variable's whole value: show only its start,
            // for every variable `prove_caller` checks, so a session id never prints in full.
            let claims = [
                "CLAUDE_SESSION_ID",
                "CLAUDE_CODE_SESSION_ID",
                "CODEX_THREAD_ID",
            ]
            .into_iter()
            .map(str::to_string)
            .chain(
                super::codex_hook::HOOK_HARNESSES
                    .iter()
                    .map(|kind| super::harness_variable(kind)),
            );
            for name in claims {
                if let Some(value) = var(&name).filter(|v| v.chars().count() > 8) {
                    message = message.replace(value.as_str(), &format!("{}...", head(&value)));
                }
            }
            json!({"ok": false, "error": message})
        }
    };
    json!({
        "processTable": ancestry.is_some(),
        "self": ancestry.as_ref().map(|found| json!({
            "pid": found.own.pid, "name": found.own.name, "parent": found.own.parent,
            "startSecs": found.own.start})),
        "walkEnded": ancestry.as_ref().map(|found| found.ended.clone()),
        "ancestors": steps,
        "claudeDirs": claude_dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
        "environment": {
            "CLAUDE_CODE_MESSAGING_SOCKET": {
                "present": socket.is_some(),
                "last6": socket.as_deref().map(tail)},
            "CLAUDE_SESSION_ID": var("CLAUDE_SESSION_ID").is_some(),
            "CLAUDE_CODE_SESSION_ID": var("CLAUDE_CODE_SESSION_ID").is_some(),
            "CODEX_THREAD_ID": var("CODEX_THREAD_ID").is_some(),
            "CLAUDE_CONFIG_DIR": var("CLAUDE_CONFIG_DIR"),
        },
        "proofTrace": proof_trace(sessions, ancestry.as_ref(), &claude_dirs),
        "identify": result,
    })
}

/// The trace as lines of text.
pub fn render(report: &Value) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    if report["processTable"] != json!(true) {
        line("process table: this process was not found, so nothing can be proven".into());
    }
    if let Some(own) = report["self"].as_object() {
        line(format!(
            "this command: pid {} {} (parent {}, started {})",
            own["pid"],
            own["name"].as_str().unwrap_or("?"),
            own["parent"],
            own["startSecs"]
        ));
    }
    for step in report["ancestors"].as_array().into_iter().flatten() {
        line(format!(
            "ancestor: pid {} {} (parent {}, started {})",
            step["pid"],
            step["name"].as_str().unwrap_or("?"),
            step["parent"],
            step["startSecs"]
        ));
        match step["matched"].as_object() {
            Some(found) => line(format!(
                "  matched: {} {}{}",
                found["kind"].as_str().unwrap_or("?"),
                found.get("id").and_then(Value::as_str).unwrap_or(""),
                if found.get("proven") == Some(&json!(true)) {
                    " (proven)"
                } else {
                    ""
                }
            )),
            None => {
                line("  not a chat".into());
                for why in step["whyNot"].as_array().into_iter().flatten() {
                    line(format!("    {}", why.as_str().unwrap_or("?")));
                }
            }
        }
    }
    if let Some(ended) = report["walkEnded"].as_str() {
        line(format!("walk ended: {ended}"));
    }
    line("Claude registry folders searched:".into());
    for dir in report["claudeDirs"].as_array().into_iter().flatten() {
        line(format!("  {}", dir.as_str().unwrap_or("?")));
    }
    let env = &report["environment"];
    let socket = &env["CLAUDE_CODE_MESSAGING_SOCKET"];
    line(match socket["last6"].as_str() {
        Some(last) => format!("CLAUDE_CODE_MESSAGING_SOCKET: present, ends ...{last}"),
        None => "CLAUDE_CODE_MESSAGING_SOCKET: absent".to_string(),
    });
    for name in [
        "CLAUDE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
    ] {
        line(format!(
            "{name}: {}",
            if env[name] == json!(true) {
                "present"
            } else {
                "absent"
            }
        ));
    }
    if let Some(dir) = env["CLAUDE_CONFIG_DIR"].as_str() {
        line(format!("CLAUDE_CONFIG_DIR: {dir}"));
    }
    if let Some(trace) = report["proofTrace"].as_str() {
        line(format!("proof trace: {trace}"));
    }
    let result = &report["identify"];
    line(if result["ok"] == json!(true) {
        format!(
            "identify: {} \"{}\" ({})",
            result["id"].as_str().unwrap_or("?"),
            result["name"].as_str().unwrap_or("?"),
            if result["proven"] == json!(true) {
                "proven from this process"
            } else {
                "not proven: sends are marked unverified"
            }
        )
    } else {
        format!(
            "identify: refused: {}",
            result["error"].as_str().unwrap_or("?")
        )
    });
    out
}
