//! Test double for Pulse's `membrane` binary, emitting the documented `--json` schemas
//! (`membrane.<command>.v1`, pulse `membrane/README.md` "CLI contract"). Behaviour is chosen by
//! `FAKE_MEMBRANE_MODE`: `ok` (default), `hang`, `fail` (exit 1), `usage` (exit 2),
//! `unavailable` (exit 3), `v2` (every document claims schema v2). When
//! `FAKE_MEMBRANE_ARGS_LOG` names a file, each invocation's argv is appended to it as one line
//! (arguments joined with U+001F). Sentinels: memory id `missing` (exit 3, `deleted: false`),
//! transcript session `gone` (410 conflict; tip exits 2), transcript cursor other than 5
//! (409 conflict with `current_cursor` 5).
//!
//! Optional e2e hooks (unset by default, so the unit-level behaviour above is unchanged):
//! `FAKE_MEMBRANE_STORE` names a file; `memory push` appends the text to it (one JSON string per
//! line) and `memory search` then returns the stored memories (scope `global`) instead of the
//! canned hit, so a save is observable on a later recall. `FAKE_MEMBRANE_STDIN_LOG` names a file;
//! `adapt observe` and `transcript append` append `<tag><TAB><compact stdin JSON>` to it.
use serde_json::{json, Value};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(path) = std::env::var("FAKE_MEMBRANE_ARGS_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            // One line per call: escape newlines inside multi-line args (context prompts).
            let line = args.join("\u{1f}").replace('\r', "").replace('\n', "\\n");
            let _ = writeln!(f, "{line}");
        }
    }
    let mode = std::env::var("FAKE_MEMBRANE_MODE").unwrap_or_default();
    match mode.as_str() {
        "hang" => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        "fail" => exit_with(1, "membrane: boom"),
        "usage" => exit_with(2, "membrane: bad flag"),
        "unavailable" => exit_with(3, "membrane: repository is not enrolled"),
        _ => {}
    }
    let ver = if mode == "v2" { "v2" } else { "v1" };
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let out: String = match a.as_slice() {
        ["context", "pull", ..] => doc(
            ver,
            "context.pull",
            json!({"context": "fake context line\nsecond line", "reason": "retrieved", "detail": {"hits": 2}}),
        ),
        ["memory", "push", t] => {
            if let Ok(path) = std::env::var("FAKE_MEMBRANE_STORE") {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{}", Value::String((*t).to_owned()));
                }
            }
            json!({"stored": true, "echo": t}).to_string()
        }
        ["memory", "list", ..] => doc(
            ver,
            "memory.list",
            json!({"scope": null, "limit": 5, "items": [
                {"id": "mem-1", "tier": "durable", "chars": 5, "access": 0, "inject": true},
                {"id": "mem-2", "tier": "session", "chars": 6, "access": 1, "inject": false}],
                "completeness": {"state": "complete"}}),
        ),
        ["memory", "search", q, ..] => {
            let items = match std::env::var("FAKE_MEMBRANE_STORE") {
                Ok(path) => {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    text.lines()
                        .filter_map(|l| serde_json::from_str::<String>(l).ok())
                        .enumerate()
                        .map(|(i, content)| {
                            json!({"id": format!("stored-{}", i + 1), "tier": "semantic",
                                "scope": "global", "score": 0.9,
                                "chars": content.chars().count(), "content": content})
                        })
                        .collect::<Vec<_>>()
                }
                Err(_) => vec![json!({"id": "mem-1", "tier": "semantic", "scope": "global",
                    "score": 0.75, "chars": 5, "content": "first"})],
            };
            doc(
                ver,
                "memory.search",
                json!({"query": q, "tiers": ["semantic"], "scope": null, "limit": 5, "pool": 9,
                    "more": true, "items": items}),
            )
        }
        ["memory", "delete", "missing", ..] => {
            println!(
                "{}",
                doc(
                    ver,
                    "memory.delete",
                    json!({"id": "missing", "deleted": false})
                )
            );
            exit_with(3, "membrane: no memory has that id")
        }
        ["memory", "delete", id, ..] => {
            doc(ver, "memory.delete", json!({"id": id, "deleted": true}))
        }
        ["blueprint", "search", ..] => {
            doc(ver, "blueprint.search", json!({"results": [{"id": "n1"}]}))
        }
        ["blueprint", ..] => doc(ver, "blueprint", json!({"results": [{"id": "n1"}]})),
        ["adapt", "observe", ..] => {
            let input = read_stdin();
            log_stdin("adapt-observe", &input);
            let req: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
            doc(ver, "adapt.observe", json!({"result": {"echo": req}}))
        }
        ["adapt", "recall", q, ..] => doc(
            ver,
            "adapt.recall",
            json!({"result": {"query": q, "records": []}}),
        ),
        ["adapt", "apply", ..] => doc(
            ver,
            "adapt.apply",
            json!({"result": {"valid": true, "errors": [], "accepted_record_ids": ["r1"]}}),
        ),
        ["adapt", "taste-review", ..] => doc(
            ver,
            "adapt.taste_review",
            json!({"result": {"manifest": {"pending": true}}}),
        ),
        ["transcript", "tip", "--session", "gone", ..] => {
            exit_with(2, "membrane: transcript session was forgotten")
        }
        ["transcript", "tip", "--session", sess, ..] => doc(
            ver,
            "transcript.tip",
            json!({"session": sess, "current_cursor": 5, "prefix_digest": "d1"}),
        ),
        ["transcript", "append", ..] => {
            let val = |flag: &str| a.windows(2).find(|w| w[0] == flag).map(|w| w[1].to_owned());
            let sess = val("--session").unwrap_or_default();
            let cursor: u64 = val("--cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
            let raw = read_stdin();
            log_stdin("transcript-append", &raw);
            let events: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            if sess == "gone" || cursor != 5 {
                let (status, code, cur) = if sess == "gone" {
                    (410, "transcript_session_gone", Value::Null)
                } else {
                    (409, "transcript_cursor_mismatch", json!(5))
                };
                println!(
                    "{}",
                    doc(
                        ver,
                        "transcript.append",
                        json!({"session": sess, "cursor": cursor, "conflict": true,
                            "replayed": false, "status": status, "code": code,
                            "current_cursor": cur, "detail": {"reason": "fake"}})
                    )
                );
                exit_with(2, "membrane: transcript append refused")
            }
            let n = events.as_array().map_or(0, Vec::len);
            doc(
                ver,
                "transcript.append",
                json!({"session": sess, "cursor": cursor, "conflict": false, "replayed": false,
                    "receipt": {"streamId": "s1", "inserted": n, "alreadyPresent": 0,
                        "lastSeq": cursor + n as u64}}),
            )
        }
        ["status", "--json"] => doc(
            ver,
            "status",
            json!({"product_version": "9.9", "mode": "in_process", "stateRoot": "/state",
                "db": {"path": "/state/mem.db", "present": true}, "model": "off",
                "resident": {"state": "absent"}}),
        ),
        ["where", "--json"] => {
            let me = std::env::current_exe().unwrap();
            doc(
                ver,
                "where",
                json!({"binary": me.to_string_lossy(), "product_version": "9.9",
                    "membrane_source_commit": "abc123", "target": "fake", "stateRoot": "/state"}),
            )
        }
        _ => exit_with(2, &format!("membrane: unexpected args {a:?}")),
    };
    println!("{out}");
    let _ = std::io::stdout().flush();
}

/// Append `<tag><TAB><compact JSON>` to `FAKE_MEMBRANE_STDIN_LOG` when it is set.
fn log_stdin(tag: &str, input: &str) {
    if let Ok(path) = std::env::var("FAKE_MEMBRANE_STDIN_LOG") {
        let compact = serde_json::from_str::<Value>(input)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| input.replace('\n', " "));
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{tag}	{compact}");
        }
    }
}

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
    s
}

/// One-line document with `schema` first, as Pulse prints it.
fn doc(ver: &str, command: &str, fields: Value) -> String {
    let mut m = serde_json::Map::new();
    m.insert(
        "schema".into(),
        Value::String(format!("membrane.{command}.{ver}")),
    );
    if let Value::Object(f) = fields {
        m.extend(f);
    }
    Value::Object(m).to_string()
}

fn exit_with(code: i32, message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(code)
}
