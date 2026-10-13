//! `right-ctl`: drive any Right Suite app (and desktop / browser) from a shell.
//!
//!   right-ctl --list                               live services under the suite root
//!   right-ctl <app> --tools                        tools right-ctl may call on <app> (JSON)
//!   right-ctl <app> <method> [json | k=v ...]      a control method (health click type key dom ...)
//!   right-ctl <app> command <name> [args...]       one of the app's own typed commands
//!   right-ctl <app> <name> [args...]               shorthand for `command <name>`
//!   right-ctl heardright transcribe <file> [language=xx]   file transcription via the speech service
//!   right-ctl run                                  JSON array [{"tool","args"}] on stdin, one session
//!                                                  (desktop.*, browser.*, <app>.*, speech.*)
//!
//! Options: --suite-root <abs path> (else RIGHTKIT_SUITE_ROOT / per-user default, fail-closed),
//!          --journal <file> (content-free JSONL of started/denied/settled).
//! Exit: 0 ok, 1 tool error, 2 usage, 3 refused (exposure, allowlist, admission).
//! Result JSON goes to stdout; errors are `{"error":{code,message,retryable}}` on stderr.

use rightkit_agent_tools::*;
use rightkit_daemon::Suite;
use serde_json::{json, Value};
use std::io::Read;
use std::process::ExitCode;
use std::sync::Arc;

fn usage() -> ExitCode {
    eprintln!("usage: right-ctl [--suite-root DIR] [--journal FILE] (--list | <app> --tools | <app> <command> [args] | run)");
    ExitCode::from(2)
}

fn fail(e: &ToolError) -> ExitCode {
    eprintln!("{}", json!({"error": e.to_value()}));
    match e.code.as_str() {
        ERR_NOT_EXPOSED | ERR_NOT_ALLOWED | ERR_DENIED => ExitCode::from(3),
        ERR_INVALID_ARGS | ERR_UNKNOWN_TOOL => ExitCode::from(2),
        _ => ExitCode::from(1),
    }
}

/// `k=v` pairs (value parsed as JSON when it is, else a string) or one JSON object.
fn parse_args(rest: &[String]) -> Result<Value, ToolError> {
    if rest.len() == 1 && rest[0].trim_start().starts_with('{') {
        return serde_json::from_str(&rest[0])
            .map_err(|e| ToolError::new(ERR_INVALID_ARGS, format!("bad JSON: {e}")));
    }
    let mut m = serde_json::Map::new();
    for r in rest {
        let (k, v) = r.split_once('=').ok_or_else(|| {
            ToolError::new(
                ERR_INVALID_ARGS,
                format!("expected key=value or a JSON object, got `{r}`"),
            )
        })?;
        m.insert(
            k.to_string(),
            serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string())),
        );
    }
    Ok(Value::Object(m))
}

fn print(v: &Value) {
    println!("{v}");
}

/// Toolbox for `run`: control tools for `apps`, plus desktop and browser.
fn full_toolbox(suite: &Suite, apps: &[String]) -> Result<Toolbox, ToolError> {
    let names: Vec<&str> = apps.iter().map(String::as_str).collect();
    #[allow(unused_mut)]
    let mut tb = control_toolbox(suite, &names)?;
    #[cfg(all(feature = "automation", any(target_os = "macos", windows)))]
    {
        let policy = rightkit_automation::Policy::new(
            std::env::var("RIGHT_CTL_DESKTOP_ALLOW")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        let auto = Arc::new(rightkit_automation::native(policy));
        tb.register_all(backends::desktop::desktop_tools(auto))?;
        tb.allow(CTL_CALLER, ["desktop.*"]);
    }
    #[cfg(feature = "browser")]
    {
        let b = backends::web::BrowserTools::new(rightkit_browser::LaunchOptions::default())?;
        tb.register_all(b.tools())?;
        tb.allow(CTL_CALLER, ["browser.*"]);
    }
    Ok(tb)
}

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut suite_root: Option<String> = None;
    let mut journal: Option<String> = None;
    while let Some(i) = argv
        .iter()
        .position(|a| a == "--suite-root" || a == "--journal")
    {
        if i + 1 >= argv.len() {
            return usage();
        }
        let v = argv.remove(i + 1);
        match argv.remove(i).as_str() {
            "--suite-root" => suite_root = Some(v),
            _ => journal = Some(v),
        }
    }
    if argv.is_empty() {
        return usage();
    }
    let suite = match suite_root.map(Suite::at).unwrap_or_else(Suite::resolve) {
        Ok(s) => s,
        Err(e) => return fail(&ToolError::new("no_suite_root", e.to_string())),
    };

    if argv[0] == "--list" {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(suite.root().join("runtime")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(r) = rightkit_daemon::read_live_record(&suite, &name) {
                    out.push(json!({"service": name, "role": r.role, "pid": r.pid, "version": r.version}));
                }
            }
        }
        print(&json!(out));
        return ExitCode::SUCCESS;
    }

    let caller = Caller::new(CTL_CALLER);
    let attach = |tb: &mut Toolbox| {
        if let Some(j) = &journal {
            tb.add_journal(Arc::new(JsonlJournal::new(j)));
        }
    };

    if argv[0] == "run" {
        let mut input = String::new();
        if std::io::stdin().read_to_string(&mut input).is_err() {
            return usage();
        }
        let calls: Vec<Value> = match serde_json::from_str(&input) {
            Ok(c) => c,
            Err(e) => {
                return fail(&ToolError::new(
                    ERR_INVALID_ARGS,
                    format!("stdin must be a JSON array of {{tool,args}}: {e}"),
                ))
            }
        };
        let apps: Vec<String> = calls
            .iter()
            .filter_map(|c| c["tool"].as_str())
            .filter_map(|t| t.split('.').next())
            .filter(|f| !matches!(*f, "desktop" | "browser" | "speech"))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tb = match full_toolbox(&suite, &apps) {
            Ok(t) => t,
            Err(e) => return fail(&e),
        };
        attach(&mut tb);
        let mut results = vec![];
        let mut code = ExitCode::SUCCESS;
        for c in &calls {
            match tb.call(
                &caller,
                c["tool"].as_str().unwrap_or_default(),
                c["args"].clone(),
            ) {
                Ok(v) => results.push(json!({"ok": true, "result": v})),
                Err(e) => {
                    results.push(json!({"ok": false, "error": e.to_value()}));
                    code = fail(&e);
                    break;
                }
            }
        }
        print(&json!(results));
        return code;
    }

    // right-ctl <app> ...
    let app = argv[0].clone();
    if argv.len() < 2 {
        return usage();
    }
    let mut tb = match control_toolbox(&suite, &[&app]) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    attach(&mut tb);
    if argv[1] == "--tools" {
        print(&json!(tb.list_for(CTL_CALLER)));
        return ExitCode::SUCCESS;
    }
    let verb = argv[1].clone();
    let rest = &argv[2..];

    let (tool, args) = if app == "heardright" && verb == "transcribe" {
        let Some(file) = rest.first() else {
            return usage();
        };
        let mut a = match parse_args(&rest[1..]) {
            Ok(a) => a,
            Err(e) => return fail(&e),
        };
        a["path"] = json!(std::fs::canonicalize(file)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file.clone()));
        ("speech.transcribe".to_string(), a)
    } else if backends::control::CONTROL_METHODS.contains(&verb.as_str()) && verb != "command" {
        match parse_args(rest) {
            Ok(a) => (format!("{app}.{verb}"), a),
            Err(e) => return fail(&e),
        }
    } else {
        // `command <name> [args...]` or `<name> [args...]`
        let (name, args) = if verb == "command" {
            match rest.split_first() {
                Some((n, a)) => (n.clone(), a.to_vec()),
                None => return usage(),
            }
        } else {
            (verb, rest.to_vec())
        };
        (
            format!("{app}.command"),
            json!({"name": name, "args": args.join(" ")}),
        )
    };

    match tb.call(&caller, &tool, args) {
        Ok(v) => {
            print(&v);
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}
