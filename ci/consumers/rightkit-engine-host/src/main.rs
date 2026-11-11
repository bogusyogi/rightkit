// rightkit-engine-host 0.2.0 consumed by exact registry pin (default features), outside the workspace.
use std::sync::Arc;

use rightkit_engine_host::{
    cli, CommandRegistry, CommandSpec, EngineError, EngineRequest, EngineSpec,
};
use serde_json::{json, Value};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let spec = EngineSpec::new("consumer-engine", "0.0.1");
    let registry = CommandRegistry::new(Arc::new(
        |name: &str, input: &Value, _dry: bool| -> Result<Value, EngineError> {
            Ok(json!({ "name": name, "input": input }))
        },
    ))
    .register(
        CommandSpec::new("job.status", "status")
            .quick()
            .positional(["job_id"]),
    );

    let (name, input) = registry
        .from_words(
            &["job".to_string(), "status".to_string(), "j1".to_string()],
            json!({}),
        )
        .expect("words map");
    check(
        name == "job.status" && input["job_id"] == "j1",
        "words map to a command",
    );
    check(
        registry.dispatch("missing", &json!({}), false).is_err(),
        "unknown command refused",
    );
    check(
        registry.contract("consumer-engine", "0.0.1", 1)["commands"][0]["name"] == "job.status",
        "contract export",
    );
    check(
        registry.typescript("Cmd").contains("\"job.status\""),
        "typescript export",
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::run_with(
        &spec,
        &registry,
        vec!["job".into(), "status".into(), "j2".into()],
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    let v: Value = serde_json::from_slice(&out).unwrap_or(Value::Null);
    check(
        code == cli::EXIT_OK && v["input"]["job_id"] == "j2",
        "cli runs a command",
    );
    let mut out = Vec::new();
    let code = cli::run_with(
        &spec,
        &registry,
        vec!["nope".into()],
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    check(code == cli::EXIT_USAGE, "cli usage failure exit code");

    let req = EngineRequest::Command {
        name: "job.status".into(),
        input: json!({}),
    };
    let (m, p) = req.to_wire();
    check(EngineRequest::from_wire(&m, p) == req, "wire round trip");

    let caller = rightkit_engine_host::caller::parse_caller("Consumer App").expect("caller");
    check(
        caller.slug == "consumer-app" && rightkit_engine_host::caller::parse_caller("a/b").is_err(),
        "caller names become slugs and unsafe names are refused",
    );

    let raw = rightkit_engine_host::call::RawOutput {
        stdout: r#"{"error":{"code":"bad_input","message":"no"}}"#.into(),
        stderr: "{\"event\":\"p\"}\nplain\n".into(),
        code: Some(1),
    };
    check(
        matches!(
            rightkit_engine_host::call::parse_reply("x", &raw),
            Err(rightkit_engine_host::call::CallError::Engine(ref e)) if e.code == "bad_input"
        ),
        "call: the error envelope becomes an engine error",
    );
    check(
        rightkit_engine_host::call::json_lines(&raw.stderr).count() == 1,
        "call: only JSON stderr lines are progress",
    );
    let report = json!({
        "app_version": "1.0.0", "contract_version": 2,
        "capabilities": [{"name": "generate", "version": 1}], "commands": ["generate"]
    });
    check(
        rightkit_engine_host::call::handshake(&report, 2, &[("generate", 1)]).is_ok()
            && rightkit_engine_host::call::handshake(&report, 3, &[]).is_err()
            && rightkit_engine_host::call::handshake(&report, 2, &[("job.ack", 1)]).is_err(),
        "call: contract and capability handshake",
    );
    use rightkit_engine_host::call::Runner;
    check(
        rightkit_engine_host::call::EngineProcess::new("relative-engine")
            .run(&["doctor"], None)
            .is_err(),
        "call: a relative engine path is refused",
    );
}
