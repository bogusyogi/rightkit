//! rightapp-engine: the engine sidecar AND the headless CLI, one command table served by rightkit-job-queue.
//!
//!   rightapp-engine [--stdio]                    serve rightkit-job-queue frames over stdio (the shell supervises it)
//!   rightapp-engine <words> [--input <json>]     one-shot CLI (rightkit-job-queue `run_cli`); JSON on stdout
//!
//! Failure: `{"error":{"code","message"}}` on stdout. Exit codes: 1 engine error, 2 bad input or usage,
//! 3 unknown command. Domain work is job kinds (`job.submit`); `echo` is also a synchronous command.
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use rightapp_protocol::{
    app_data_root, build_label, crash_dir, JobError, JobResult, APP_NAME, ENGINE_NAME,
    JOB_KIND_ECHO, METHOD_ECHO, MODE_APP,
};
use rightkit_crash::{install_process_hook, CrashConfig, ProcessKind};
use rightkit_job_queue::{
    run_cli, serve, CommandSet, Dispatcher, Job, JobCommands, JobControl, JobHandler, JobLocks,
    JobStore, Runner, Sink, CONTRACT_VERSION,
};
use serde_json::{json, Value};

const USAGE: &str = "usage: rightapp-engine [--stdio] | doctor | echo --input <json> | job submit --input <json> [--wait] | --version";

/// One job kind to its result. Each kind is one domain command, expressed as durable work.
fn run_kind(kind: &str, request: &Value) -> JobResult<Value> {
    match kind {
        JOB_KIND_ECHO => Ok(request.clone()),
        other => Err(JobError::new(
            "unknown_kind",
            format!("unknown job kind: {other}"),
        )),
    }
}

/// Runs one job of a known kind.
struct DomainJobs;

impl JobHandler for DomainJobs {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        run_kind(&job.kind, &job.request)
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

/// The `doctor` answer: the engine, its data root and every served method.
fn doctor_payload(root: &str, dry_run: bool, methods: &[String]) -> Value {
    json!({
        "ok": true,
        "engine": ENGINE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "contract_version": CONTRACT_VERSION,
        "app_data": root,
        "dry_run": dry_run,
        "methods": methods,
    })
}

/// The product's own commands and `doctor`, without the durable `job.*` surface.
fn base_dispatcher(root: String) -> Dispatcher {
    Dispatcher::new(ENGINE_NAME, env!("CARGO_PKG_VERSION"))
        .with_doctor(Arc::new(move |dry_run: bool, methods: &[String]| {
            doctor_payload(&root, dry_run, methods)
        }))
        .with(Arc::new(DomainCommands))
}

/// The one dispatcher behind both surfaces. The durable job store lives under the app data root.
fn dispatcher(data: &Path) -> Result<Dispatcher, String> {
    let dir = data.join("engine");
    let locks = dir.join("locks");
    std::fs::create_dir_all(&locks).map_err(|e| format!("create {}: {e}", locks.display()))?;
    let store = JobStore::open(dir.join("jobs.sqlite")).map_err(|e| e.to_string())?;
    let runner = Runner::new(Arc::new(store), JobLocks::new(locks), Arc::new(DomainJobs));
    Ok(base_dispatcher(data.display().to_string())
        .with(Arc::new(JobCommands::new(Arc::new(runner)))))
}

/// Crash reports of this process land in the shared `crash` folder under the data root.
fn crash_config(data: &Path) -> CrashConfig {
    let mut config = CrashConfig::new(APP_NAME, env!("CARGO_PKG_VERSION"), crash_dir(data));
    config.build = Some(build_label().to_owned());
    config
}

fn exit_with(code: &str, message: &str, exit: i32) -> ! {
    println!(
        "{}",
        json!({ "error": { "code": code, "message": message } })
    );
    std::process::exit(exit)
}

fn main() {
    // The mode decision comes before any store, log, credential or service opens.
    rightkit_daemon::mode::init_or_exit(&MODE_APP);
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
    install_process_hook(ProcessKind::Engine, crash_config(&data));
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
    // rightkit-job-queue exits 2 for any usage failure; the engine contract has always used 3 for an unknown command.
    let code = if code == 2 && String::from_utf8_lossy(&out).contains("\"unknown_command\"") {
        3
    } else {
        code
    };
    let _ = io::stdout().write_all(&out);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent() -> Sink {
        Arc::new(|_event: Value| {})
    }

    // rk-test: pure
    #[test]
    fn the_echo_command_returns_its_params() {
        let params = json!({ "n": 1, "s": "kept" });
        let out = DomainCommands
            .call(METHOD_ECHO, &params, false, &silent())
            .unwrap();
        assert_eq!(out, params);
    }

    // rk-test: pure
    #[test]
    fn an_unknown_method_is_the_typed_unknown_method_error() {
        let err = DomainCommands
            .call("frobnicate", &json!({}), false, &silent())
            .unwrap_err();
        assert_eq!(err.code, "unknown_method");
        assert!(err.message.contains("frobnicate"));
    }

    // rk-test: pure
    #[test]
    fn the_echo_job_kind_returns_its_request() {
        let request = json!({ "a": [1, 2, 3] });
        assert_eq!(run_kind(JOB_KIND_ECHO, &request).unwrap(), request);
    }

    // rk-test: pure
    #[test]
    fn an_unknown_job_kind_is_the_typed_unknown_kind_error() {
        let err = run_kind("frobnicate", &json!(null)).unwrap_err();
        assert_eq!(err.code, "unknown_kind");
        assert!(err.message.contains("frobnicate"));
    }

    // rk-test: pure
    #[test]
    fn the_dispatcher_serves_echo_and_a_doctor_that_names_the_data_root() {
        let dispatcher = base_dispatcher("the-data-root".into());
        let sink = silent();
        let echoed = dispatcher
            .call(METHOD_ECHO, &json!({ "n": 1 }), false, &sink)
            .unwrap();
        assert_eq!(echoed, json!({ "n": 1 }));

        let doctor = dispatcher.call("doctor", &json!({}), true, &sink).unwrap();
        assert_eq!(doctor["ok"], json!(true));
        assert_eq!(doctor["engine"], json!(ENGINE_NAME));
        assert_eq!(doctor["version"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(doctor["contract_version"], json!(CONTRACT_VERSION));
        assert_eq!(doctor["app_data"], json!("the-data-root"));
        assert_eq!(doctor["dry_run"], json!(true));
        let methods = doctor["methods"].as_array().unwrap();
        assert!(methods.contains(&json!(METHOD_ECHO)));

        let err = dispatcher
            .call("frobnicate", &json!({}), false, &sink)
            .unwrap_err();
        assert_eq!(err.code, "unknown_method");
    }
}
