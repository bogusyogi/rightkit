//! rightapp-engine: the engine sidecar AND the headless CLI, one command table served by rightkit-jobs.
//!
//!   rightapp-engine [--stdio]                    serve rightkit-jobs frames over stdio (the shell supervises it)
//!   rightapp-engine <words> [--input <json>]     one-shot CLI (rightkit-jobs `run_cli`); JSON on stdout
//!
//! Failure: `{"error":{"code","message"}}` on stdout. Exit codes: 1 engine error, 2 bad input or usage,
//! 3 unknown command. Domain work is job kinds (`job.submit`); `echo` is also a synchronous command.
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use rightapp_protocol::{
    app_data_root, JobError, JobResult, ENGINE_NAME, JOB_KIND_ECHO, METHOD_ECHO,
};
use rightkit_jobs::{
    run_cli, serve, CommandSet, Dispatcher, Job, JobCommands, JobControl, JobHandler, JobLocks,
    JobStore, Runner, Sink, CONTRACT_VERSION,
};
use serde_json::{json, Value};

const USAGE: &str = "usage: rightapp-engine [--stdio] | doctor | echo --input <json> | job submit --input <json> [--wait] | --version";

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
        _ => {}
    }
    let data = app_data_root().unwrap_or_else(|e| exit_with("data_root", &e, 1));
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
    // rightkit-jobs exits 2 for any usage failure; the engine contract has always used 3 for an unknown command.
    let code = if code == 2 && String::from_utf8_lossy(&out).contains("\"unknown_command\"") {
        3
    } else {
        code
    };
    let _ = io::stdout().write_all(&out);
    std::process::exit(code);
}
