//! Every privileged IPC command asks its caller check FIRST, and acts on the answer.
//!
//! A command that compiles, passes its happy-path test and forgets the caller check is the failure
//! this guards against, and a webview-bound call cannot be made without a running webview. So this
//! reads the command sources and fails when
//!
//! * a command attribute is in a form the scan cannot read (`cfg_attr` around a command),
//! * a command name belongs to no family in [`FAMILIES`], or is empty (macro-generated),
//! * a command's FIRST statement (after `use` items) is not that family's caller check, called
//!   with the invoking `webview`, with the refusal propagated: the call must end in `?`, or in
//!   `.map_err(..)?`; a bare `check(..);`, `.ok();`, `let _ =` or `if let` form does not pass,
//! * a `rk_shell_*` check names a command other than the function it opens, or
//! * any guarded family's scanned command set differs from what `handler!` registers.
//!
//! The decisions themselves are tested in `shell_guard.rs` and `host_license.rs`; no webview is
//! needed. What this cannot prove is the behaviour of a live webview (an app e2e covers that).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// A command family and the call that must open every command in it.
struct Family {
    prefix: &'static str,
    /// The text the first statement's call starts with (after any `let pattern =`).
    call: &'static str,
}

const FAMILIES: [Family; 12] = [
    Family {
        prefix: "rk_shell_",
        call: "guard_command(&webview, \"",
    },
    Family {
        prefix: "rightkit_license_",
        call: "license_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_settings_",
        call: "settings_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_secrets_",
        call: "secrets_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_legal_",
        call: "legal_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_preview_",
        call: "preview_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_update_",
        call: "update_host(&webview, \"",
    },
    Family {
        prefix: "rightkit_logs_",
        call: "authorize(&webview, \"",
    },
    Family {
        prefix: "rightkit_pdf_",
        call: "caller(&webview, \"",
    },
    Family {
        prefix: "rightkit_app_lock_",
        call: "native(&webview, \"",
    },
    Family {
        prefix: "rightkit_models_",
        call: "models_service(&webview, \"",
    },
    Family {
        prefix: "rightkit_crash_",
        call: "crash_service(&webview, \"",
    },
];

const LICENSE_COMMANDS: [&str; 5] = [
    "rightkit_license_status",
    "rightkit_license_activate",
    "rightkit_license_refresh",
    "rightkit_license_deactivate",
    "rightkit_license_request_trial",
];
const APP_LOCK_COMMANDS: [&str; 5] = [
    "rightkit_app_lock_status",
    "rightkit_app_lock_lock",
    "rightkit_app_lock_begin_unlock",
    "rightkit_app_lock_complete_unlock",
    "rightkit_app_lock_cancel_unlock",
];

struct Command {
    name: String,
    file: String,
    /// The text between the outer braces of the body.
    body: String,
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The index just past a string literal, char literal or line comment that starts at `i`, or
/// `None` when `bytes[i]` starts none of them.
fn skip_literal(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes[i] {
        b'"' => {
            let mut j = i + 1;
            while bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            Some(j + 1)
        }
        b'\'' => {
            // A char literal ('x' or '\n'), not a lifetime.
            if bytes[i + 1] == b'\\' {
                // Skip the escaped character, so the literal '\'' is one literal.
                let mut j = i + 3;
                while bytes[j] != b'\'' {
                    j += 1;
                }
                Some(j + 1)
            } else if bytes.get(i + 2) == Some(&b'\'') {
                Some(i + 3)
            } else {
                None
            }
        }
        b'/' if bytes.get(i + 1) == Some(&b'/') => {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        _ => None,
    }
}

/// The index just past the delimiter that closes the one at `open`.
fn matching(src: &str, open: usize) -> usize {
    let bytes = src.as_bytes();
    let (o, c) = match bytes[open] {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        other => panic!("not a delimiter: {}", other as char),
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if let Some(next) = skip_literal(bytes, i) {
            i = next;
            continue;
        }
        if bytes[i] == o {
            depth += 1;
        } else if bytes[i] == c {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    panic!("unbalanced delimiter");
}

/// Where an attribute that makes the next fn a command starts: `#[tauri::command]`,
/// `#[tauri::command(..)]`, `#[command]` and `#[command(..)]`. A `cfg_attr` that carries a
/// command attribute is refused outright: the scan cannot tell when it applies.
fn command_attrs(file: &str, src: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for marker in ["#[tauri::command", "#[::tauri::command", "#[command"] {
        let mut from = 0;
        while let Some(at) = src[from..].find(marker) {
            let start = from + at;
            from = start + marker.len();
            let next = src.as_bytes().get(from).copied();
            if matches!(next, Some(b']') | Some(b'(')) {
                found.push(start);
            }
        }
    }
    let mut from = 0;
    while let Some(at) = src[from..].find("cfg_attr(") {
        let open = from + at + "cfg_attr".len();
        let close = matching(src, open);
        let text = &src[open..close];
        assert!(
            !text.contains("command"),
            "{file}: a command attribute inside cfg_attr cannot be scanned; write it plainly"
        );
        from = close;
    }
    found.sort_unstable();
    found.dedup();
    found
}

fn commands_in(file: &str, src: &str) -> Vec<Command> {
    let mut found = Vec::new();
    for start in command_attrs(file, src) {
        let attr_open = start + 1;
        let attr_end = matching(src, attr_open);
        let rest = &src[attr_end..];
        let fn_at = rest.find("fn ").expect("a command attribute without a fn");
        let after_fn = &rest[fn_at + 3..];
        let name: String = after_fn
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let params_open = attr_end + fn_at + 3 + after_fn.find('(').unwrap();
        let params_close = matching(src, params_open);
        let body_open = params_close + src[params_close..].find('{').unwrap();
        let body_close = matching(src, body_open);
        found.push(Command {
            name,
            file: file.to_owned(),
            body: src[body_open + 1..body_close - 1].to_owned(),
        });
    }
    found
}

fn all_commands() -> Vec<Command> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&root, &mut files);
    let mut commands = Vec::new();
    for file in files {
        let src = fs::read_to_string(&file).unwrap();
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        commands.extend(commands_in(&rel, &src));
    }
    commands
}

/// The first statement of `body` that is not a `use` item or a comment, without its `;`.
fn first_statement(body: &str) -> String {
    let mut rest = body;
    loop {
        rest = rest.trim_start();
        if let Some(line_comment) = rest.strip_prefix("//") {
            rest = line_comment.split_once('\n').map_or("", |(_, tail)| tail);
            continue;
        }
        let bytes = rest.as_bytes();
        let mut depth = 0i32;
        let mut i = 0;
        let mut end = rest.len();
        while i < bytes.len() {
            if let Some(next) = skip_literal(bytes, i) {
                i = next;
                continue;
            }
            match bytes[i] {
                b'(' | b'{' | b'[' => depth += 1,
                b')' | b'}' | b']' => depth -= 1,
                b';' if depth == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let statement = rest[..end].trim();
        if statement.starts_with("use ") {
            rest = rest.get(end + 1..).unwrap_or("");
            continue;
        }
        return statement.to_owned();
    }
}

/// Why `statement` is not `[let pattern =] <call>(..)` with the refusal propagated, or `None`.
fn first_statement_problem(statement: &str, call: &str) -> Option<String> {
    let mut expr = statement;
    if let Some(binding) = expr.strip_prefix("let ") {
        expr = binding
            .split_once(" = ")
            .map_or(expr, |(_, value)| value)
            .trim_start();
    }
    let expr = expr.strip_prefix("crate::commands::").unwrap_or(expr);
    if !expr.starts_with(call) {
        return Some(format!("it does not start with `{call}`"));
    }
    let open = expr.find('(').unwrap();
    let after = expr[matching(expr, open)..].trim();
    let propagated = after == "?" || (after.starts_with(".map_err(") && after.ends_with('?'));
    if propagated {
        None
    } else {
        Some(format!(
            "the refusal is not propagated: after the call it has `{after}`"
        ))
    }
}

/// The names `handler!` lists that start with `prefix`.
fn registered(prefix: &str) -> BTreeSet<String> {
    let lib = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap();
    lib.lines()
        .filter_map(|line| line.trim().strip_suffix(',').or(Some(line.trim())))
        .filter_map(|line| line.rsplit("::").next())
        .filter(|name| name.starts_with(prefix))
        .map(|name| name.trim_end_matches(',').to_owned())
        .collect()
}

#[test]
fn every_command_belongs_to_a_family_and_opens_with_its_propagated_caller_check() {
    let commands = all_commands();
    assert!(
        commands.len() >= 80,
        "the scan found only {}",
        commands.len()
    );
    for command in &commands {
        let family = FAMILIES
            .iter()
            .find(|f| !command.name.is_empty() && command.name.starts_with(f.prefix))
            .unwrap_or_else(|| {
                panic!(
                    "`{}` ({}) is a command in no guarded family: guard it and add the family here",
                    command.name, command.file
                )
            });
        let statement = first_statement(&command.body);
        if let Some(problem) = first_statement_problem(&statement, family.call) {
            panic!(
                "{} ({}) must open with `{}..)` and propagate it; {problem}. First statement: {statement}",
                command.name, command.file, family.call
            );
        }
        if family.prefix == "rk_shell_" {
            let own = format!("guard_command(&webview, \"{}\")", command.name);
            assert!(
                statement.contains(&own),
                "{} ({}) must pass its own name to the guard: {statement}",
                command.name,
                command.file
            );
        }
    }
}

#[test]
fn a_licence_command_names_its_own_action() {
    for command in all_commands()
        .iter()
        .filter(|c| c.name.starts_with("rightkit_license_"))
    {
        let action = command.name.trim_start_matches("rightkit_license_");
        let statement = first_statement(&command.body);
        assert!(
            statement.contains(&format!("license_service(&webview, \"{action}\")")),
            "{}: {statement}",
            command.name
        );
    }
}

#[test]
fn the_scan_and_the_handler_list_agree_on_every_protected_family() {
    let commands = all_commands();
    for family in &FAMILIES {
        let prefix = family.prefix;
        let scanned: BTreeSet<String> = commands
            .iter()
            .filter(|c| c.name.starts_with(prefix))
            .map(|c| c.name.clone())
            .collect();
        let listed = registered(prefix);
        assert_eq!(
            scanned,
            listed,
            "{prefix}: scanned but not in handler!: {:?}; in handler! but not scanned: {:?}",
            scanned.difference(&listed).collect::<Vec<_>>(),
            listed.difference(&scanned).collect::<Vec<_>>()
        );
        let expected = match prefix {
            "rightkit_license_" => Some(LICENSE_COMMANDS.as_slice()),
            "rightkit_app_lock_" => Some(APP_LOCK_COMMANDS.as_slice()),
            _ => None,
        };
        if let Some(expected) = expected {
            let expected: BTreeSet<String> = expected.iter().map(|n| (*n).to_owned()).collect();
            assert_eq!(listed, expected, "{prefix}: fixed protected command set");
        } else if prefix == "rk_shell_" {
            assert!(listed.len() >= 49, "only {} shell commands", listed.len());
        } else {
            assert!(
                !listed.is_empty(),
                "{prefix}: no protected commands registered"
            );
        }
    }
}

#[test]
fn every_app_lock_command_names_its_own_closed_action() {
    for command in all_commands()
        .iter()
        .filter(|c| c.name.starts_with("rightkit_app_lock_"))
    {
        let action = command.name.trim_start_matches("rightkit_app_lock_");
        let statement = first_statement(&command.body);
        assert!(
            statement.contains(&format!("native(&webview, \"{action}\")")),
            "{}: {statement}",
            command.name
        );
    }
    let call = "native(&webview, \"";
    assert!(first_statement_problem("let native = native(&webview, \"status\")?", call).is_none());
    for bad in [
        "let _ = native(&webview, \"status\")",
        "native(&webview, \"status\").ok()",
        "native(&webview, \"status\")?.service.status().map_err(error)",
    ] {
        assert!(first_statement_problem(bad, call).is_some(), "{bad}");
    }
}

#[test]
fn the_scan_itself_refuses_what_it_is_meant_to_refuse() {
    let family = &FAMILIES[0];
    // Propagated, with and without a mapped error and the crate path.
    for good in [
        "guard_command(&webview, \"rk_shell_x\")?",
        "crate::commands::guard_command(&webview, \"rk_shell_x\")?",
        "guard_command(&webview, \"rk_shell_x\").map_err(|e| e.to_string())?",
        "guard_command(&webview, \"rk_shell_x\").map_err(|e| {\n json!({\"a\": 1})\n })?",
    ] {
        assert_eq!(first_statement_problem(good, family.call), None, "{good}");
    }
    // Present but not acted on, or not the first call at all.
    for bad in [
        "guard_command(&webview, \"rk_shell_x\")",
        "let _ = guard_command(&webview, \"rk_shell_x\")",
        "guard_command(&webview, \"rk_shell_x\").ok()",
        "if guard_command(&webview, \"rk_shell_x\").is_err() { return }",
        "state.touch()",
        "require_app_caller(&webview)?",
    ] {
        assert!(first_statement_problem(bad, family.call).is_some(), "{bad}");
    }
    // `use` items and comments before the check are skipped; any other statement is the first.
    assert_eq!(
        first_statement(
            "use tauri::Emitter;\n // note\n let h = update_host(&webview, \"a\")?;\n x();"
        ),
        "let h = update_host(&webview, \"a\")?"
    );
    assert_eq!(
        first_statement("let a = 1;\n guard_command(&webview, \"rk_shell_x\")?;"),
        "let a = 1"
    );
    // Every spelling of a command attribute is found; a cfg_attr one is refused.
    let src = "#[tauri::command]\nfn a() {}\n#[tauri::command(rename_all = \"snake_case\")]\nfn b() {}\n#[command]\nfn c() {}\n#[command(x)]\nfn d() {}\n#[command_other]\nfn e() {}\n";
    let names: Vec<String> = commands_in("t.rs", src)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["a", "b", "c", "d"]);
    let refused = std::panic::catch_unwind(|| {
        command_attrs(
            "t.rs",
            "#[cfg_attr(feature = \"x\", tauri::command)]\nfn f() {}",
        )
    });
    assert!(refused.is_err());
}
