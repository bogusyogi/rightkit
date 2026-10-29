//! rightapp-engine: the engine sidecar AND the headless CLI, one command table served by rightkit-job-queue.
//!
//!   rightapp-engine [--stdio]                    serve rightkit-job-queue frames over stdio (the shell supervises it)
//!   rightapp-engine <words> [--input <json>]     one-shot CLI (rightkit-job-queue `run_cli`); JSON on stdout
//!   rightapp-engine contract [--json]            the app's CLI contract (M22): every tool with its effect and approval
//!
//! Failure: `{"error":{"code","message"}}` on stdout. Exit codes: 0 ok, 1 state not reached, 2 refused or bad
//! usage (an unknown command included), 3 no result known. Domain work is job kinds (`job.submit`); `echo` is
//! also a synchronous command.
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use rightapp_protocol::{
    app_data_root, JobError, JobResult, ENGINE_NAME, JOB_KIND_ECHO, METHOD_ECHO,
};
use rightkit_crash::{install_process_hook, CrashConfig, ProcessKind};
use rightkit_job_queue::{
    run_cli, serve, CommandSet, Dispatcher, Job, JobCommands, JobControl, JobHandler, JobLocks,
    JobStore, Runner, Sink, CONTRACT_VERSION,
};
use serde_json::{json, Value};

const USAGE: &str = "usage: rightapp-engine [--stdio] | contract [--json] | doctor | echo --input <json> | job submit --input <json> [--wait] | --version";

/// The tools this engine offers through its CLI, with the consequence class of each (SHAPE §4): name, summary, effect, approval.
const TOOLS: &[(&str, &str, &str, &str)] = &[
    (
        "echo",
        "Return the input unchanged (a synchronous command).",
        "read",
        "none",
    ),
    (
        "job submit",
        "Queue a durable echo job; `--wait` returns its result.",
        "write",
        "none",
    ),
];

/// The app's CLI contract (M22, schema rightkit-agent-tools/app-contract/1, contract version 2). Built here until
/// rightkit-agent-tools 0.4.0 ships `host::run`, which replaces this local builder.
fn contract() -> Value {
    json!({
        "contract": format!("{ENGINE_NAME}-cli"),
        "contract_version": 2,
        "schema": "rightkit-agent-tools/app-contract/1",
        "app": ENGINE_NAME,
        "app_version": env!("CARGO_PKG_VERSION"),
        "platforms": ["macos", "windows"],
        "exit_codes": { "0": "ok", "1": "state not reached", "2": "refused or bad usage", "3": "no result known" },
        "tools": TOOLS.iter().map(|(name, summary, effect, approval)| json!({
            "name": name, "summary": summary, "effect": effect, "approval": approval, "availability": "engine",
        })).collect::<Vec<_>>(),
    })
}

/// Runs one job of a known kind. Each kind is one domain command, expressed as durable work.
struct DomainJobs;

impl JobHandler for DomainJobs {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        match job.kind.as_str() {
            JOB_KIND_ECHO => Ok(job.request.clone()),
            other => Err(JobError::new(
                "unknown_kind",
                format!("unknown job kind: {other}"),
            )),
        }
    }
}

/// Synchronous domain commands: the same operations as the job kinds, answered at once.
struct DomainCommands;

impl CommandSet for DomainCommands {
    fn methods(&self) -> Vec<String> {
        vec![METHOD_ECHO.into()]
    }

    fn call(&self, method: &str, params: &Value, _dry_run: bool, _emit: &Sink) -> JobResult<Value> {
        match method {
            METHOD_ECHO => Ok(params.clone()),
            other => Err(JobError::new(
                "unknown_method",
                format!("unknown method: {other}"),
            )),
        }
    }
}

/// The one dispatcher behind both surfaces. The durable job store lives under the app data root.
fn dispatcher(data: &Path) -> Result<Dispatcher, String> {
    let dir = data.join("engine");
    let locks = dir.join("locks");
    std::fs::create_dir_all(&locks).map_err(|e| format!("create {}: {e}", locks.display()))?;
    let store = JobStore::open(dir.join("jobs.sqlite")).map_err(|e| e.to_string())?;
    let runner = Runner::new(Arc::new(store), JobLocks::new(&locks), Arc::new(DomainJobs));
    let root = data.display().to_string();
    Ok(Dispatcher::new(ENGINE_NAME, env!("CARGO_PKG_VERSION"))
        .with_doctor(Arc::new(move |dry_run: bool, methods: &[String]| {
            json!({
                "ok": true,
                "engine": ENGINE_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "contract_version": CONTRACT_VERSION,
                "app_data": root,
                "dry_run": dry_run,
                "methods": methods,
            })
        }))
        .with(Arc::new(DomainCommands))
        .with(Arc::new(JobCommands::new(Arc::new(runner)))))
}

fn exit_with(code: &str, message: &str, exit: i32) -> ! {
    println!(
        "{}",
        json!({ "error": { "code": code, "message": message } })
    );
    std::process::exit(exit)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("{}", json!({ "version": env!("CARGO_PKG_VERSION") }));
            return;
        }
        Some("--help") | Some("-h") | Some("help") => {
            eprintln!("{USAGE}");
            return;
        }
        // The contract needs no data root and opens no store: an agent may read it before the app ever ran.
        Some("contract") => {
            println!("{}", contract());
            return;
        }
        _ => {}
    }
    // `app_data_root` makes the process-wide release-or-test decision first (rightkit_daemon::mode), before any store opens.
    let data = app_data_root().unwrap_or_else(|e| exit_with("data_root", &e, 1));
    install_process_hook(
        ProcessKind::Engine,
        CrashConfig::new(ENGINE_NAME, env!("CARGO_PKG_VERSION"), data.join("crashes")),
    );
    let dispatcher = dispatcher(&data).unwrap_or_else(|e| exit_with("engine_start", &e, 1));

    if args.is_empty() || args[0] == "--stdio" {
        let served = serve(
            Arc::new(dispatcher),
            io::BufReader::new(io::stdin()),
            io::stdout(),
            false,
        );
        std::process::exit(if served.is_ok() { 0 } else { 1 });
    }

    let mut out: Vec<u8> = Vec::new();
    let code = run_cli(
        &dispatcher,
        args.into_iter(),
        &mut io::stdin(),
        &mut out,
        &mut io::stderr(),
    );
    // An unknown command is a refusal (exit 2, code `unknown_command`), as rightkit-job-queue reports it: exit 3 means no result known.
    let _ = io::stdout().write_all(&out);
    std::process::exit(code);
}
