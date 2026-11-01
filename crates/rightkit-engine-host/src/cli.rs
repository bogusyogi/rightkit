//! Headless CLI mode: `<engine> <words...> [--input <json>|-] [--dry-run] [--wait]
//! [--request-id <id>] [--wait-timeout <s>] [--data-dir <dir>]`.
//!
//! The result is JSON on stdout (exit 0); a failure is `{"error":{code,message,details?}}` on
//! stdout with exit 1 (command failure) or 2 (usage). stdin is read only for `--input -`, because
//! callers often leave an unclosed pipe attached and reading it unasked would block forever.

use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::registry::CommandRegistry;
use crate::wire::EngineError;
use crate::EngineSpec;

/// Exit code of a successful command.
pub const EXIT_OK: i32 = 0;
/// Exit code of a command that ran and failed.
pub const EXIT_FAILED: i32 = 1;
/// Exit code of a usage or input error (nothing ran).
pub const EXIT_USAGE: i32 = 2;

/// The parsed command line.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CliArgs {
    /// `--dry-run`.
    pub dry_run: bool,
    /// `--input` (`-` reads stdin).
    pub input: Option<String>,
    /// `--wait`.
    pub wait: bool,
    /// `--request-id`.
    pub request_id: Option<String>,
    /// `--wait-timeout` (seconds, unparsed).
    pub wait_timeout: Option<String>,
    /// `--data-dir`, made absolute.
    pub data_dir: Option<PathBuf>,
    /// The command words.
    pub words: Vec<String>,
}

/// Remove every `--data-dir <dir>` pair from `argv` and return the last folder, made absolute.
pub fn take_data_dir_flag(argv: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let mut found: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--data-dir" {
            let value = argv
                .get(i + 1)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or("--data-dir needs a folder")?;
            argv.drain(i..i + 2);
            found = Some(value);
        } else {
            i += 1;
        }
    }
    found
        .map(|dir| std::path::absolute(&dir).map_err(|e| format!("--data-dir {dir}: {e}")))
        .transpose()
}

/// Parse `argv` (without the program name).
pub fn parse_args(mut argv: Vec<String>) -> Result<CliArgs, String> {
    let data_dir = take_data_dir_flag(&mut argv)?;
    let common = rightkit_job_queue::parse_flags(argv.into_iter());
    Ok(CliArgs {
        dry_run: common.dry_run,
        input: common.input,
        wait: common.wait,
        request_id: common.request_id,
        wait_timeout: common.wait_timeout,
        data_dir,
        words: common.words,
    })
}

/// The JSON input of a command: `--input -` reads `stdin`, an absent or blank input is `{}`.
pub fn read_input(raw: Option<&str>, stdin: &mut dyn Read) -> Result<Value, EngineError> {
    let text = match raw {
        Some("-") => {
            let mut buf = String::new();
            stdin
                .read_to_string(&mut buf)
                .map_err(|e| EngineError::new("bad_input", format!("read stdin: {e}")))?;
            buf
        }
        Some(text) => text.to_string(),
        None => String::new(),
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text)
        .map_err(|e| EngineError::new("bad_input", format!("input is not JSON: {e}")))
}

/// Write `value` as pretty JSON and a newline.
pub fn print_json(out: &mut dyn Write, value: &Value) {
    let text = serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| r#"{"error":{"code":"encode","message":"json encode failed"}}"#.into());
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// Print an error envelope and return `exit`.
pub fn fail(out: &mut dyn Write, error: &EngineError, exit: i32) -> i32 {
    print_json(out, &error.envelope());
    exit
}

/// Run the CLI over `args` with explicit stdin and stdout; returns the exit code.
///
/// Built-in words: `--version`/`-V` (`{"version"}`), `--help`/`-h`/`help` (usage on `err`),
/// `--contract` (the registry contract) and `--contract-ts [<TypeName>]` (the TypeScript command
/// union, raw text). A `--data-dir` is exported as `<PREFIX>_APP_DATA` so every child inherits it.
/// `--wait`, `--request-id` and `--wait-timeout` are copied into the input as `wait`,
/// `request_id` and `wait_timeout_s`.
pub fn run_with(
    spec: &EngineSpec,
    registry: &CommandRegistry,
    args: Vec<String>,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => return fail(out, &EngineError::new("bad_usage", message), EXIT_USAGE),
    };
    if let Some(dir) = &args.data_dir {
        std::env::set_var(spec.env_name("APP_DATA"), dir);
    }
    match args.words.first().map(String::as_str) {
        Some("--version" | "-V") => {
            print_json(out, &json!({ "version": spec.version }));
            return EXIT_OK;
        }
        Some("--help" | "-h" | "help") | None => {
            let _ = writeln!(err, "{}", spec.usage);
            return if args.words.is_empty() {
                EXIT_USAGE
            } else {
                EXIT_OK
            };
        }
        Some("--contract") => {
            print_json(
                out,
                &registry.contract(&spec.name, &spec.version, spec.contract_version),
            );
            return EXIT_OK;
        }
        Some("--contract-ts") => {
            let type_name = args
                .words
                .get(1)
                .map(String::as_str)
                .unwrap_or("EngineCommand");
            let _ = write!(out, "{}", registry.typescript(type_name));
            return EXIT_OK;
        }
        _ => {}
    }
    let input = match read_input(args.input.as_deref(), stdin) {
        Ok(input) => input,
        Err(error) => return fail(out, &error, EXIT_USAGE),
    };
    let (name, mut input) = match registry.from_words(&args.words, input) {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = writeln!(err, "{}", spec.usage);
            return fail(out, &error, EXIT_USAGE);
        }
    };
    if args.wait {
        input["wait"] = json!(true);
    }
    if let Some(id) = args.request_id {
        input["request_id"] = json!(id);
    }
    if let Some(raw) = args.wait_timeout {
        match raw.trim().parse::<f64>() {
            Ok(secs) if secs.is_finite() && secs >= 0.0 => input["wait_timeout_s"] = json!(secs),
            _ => {
                return fail(
                    out,
                    &EngineError::new(
                        "bad_input",
                        "--wait-timeout takes a non-negative number of seconds",
                    ),
                    EXIT_USAGE,
                )
            }
        }
    }
    match registry.dispatch(&name, &input, args.dry_run) {
        Ok(value) => {
            print_json(out, &value);
            EXIT_OK
        }
        Err(error) => fail(out, &error, EXIT_FAILED),
    }
}

/// [`run_with`] over the process's stdin, stdout and stderr.
pub fn run(spec: &EngineSpec, registry: &CommandRegistry, args: Vec<String>) -> i32 {
    run_with(
        spec,
        registry,
        args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::CommandSpec;
    use std::sync::Arc;

    fn setup() -> (EngineSpec, CommandRegistry) {
        let spec = EngineSpec::new("demo-engine", "1.2.3").with_usage("usage: demo-engine");
        let registry = CommandRegistry::new(Arc::new(
            |name: &str, input: &Value, dry: bool| -> Result<Value, EngineError> {
                if name == "boom" {
                    return Err(EngineError::new("boom", "failed"));
                }
                Ok(json!({"name": name, "input": input, "dry": dry}))
            },
        ))
        .register(CommandSpec::new("job.status", "status").positional(["job_id"]))
        .register(CommandSpec::new("boom", "fails"));
        (spec, registry)
    }

    fn run_args(args: &[&str], stdin: &str) -> (i32, Value) {
        let (spec, registry) = setup();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_with(
            &spec,
            &registry,
            args.iter().map(|s| s.to_string()).collect(),
            &mut stdin.as_bytes(),
            &mut out,
            &mut err,
        );
        let value = serde_json::from_slice(&out).unwrap_or(Value::Null);
        (code, value)
    }

    #[test]
    fn a_command_prints_its_result() {
        let (code, v) = run_args(&["job", "status", "j1", "--dry-run", "--wait"], "");
        assert_eq!(code, EXIT_OK);
        assert_eq!(v["name"], "job.status");
        assert_eq!(v["input"]["job_id"], "j1");
        assert_eq!(v["input"]["wait"], true);
        assert_eq!(v["dry"], true);
    }

    #[test]
    fn input_from_stdin_only_when_asked() {
        let (code, v) = run_args(&["job", "status", "--input", "-"], "{\"a\":1}");
        assert_eq!(code, EXIT_OK);
        assert_eq!(v["input"]["a"], 1);
        let (code, v) = run_args(&["job", "status", "--input", "{bad"], "");
        assert_eq!(code, EXIT_USAGE);
        assert_eq!(v["error"]["code"], "bad_input");
    }

    #[test]
    fn failures_and_builtins() {
        assert_eq!(run_args(&["boom"], "").0, EXIT_FAILED);
        assert_eq!(run_args(&["nope"], "").0, EXIT_USAGE);
        let (code, v) = run_args(&["--version"], "");
        assert_eq!((code, v["version"].as_str()), (EXIT_OK, Some("1.2.3")));
        let (_, v) = run_args(&["--contract"], "");
        assert_eq!(v["engine"], "demo-engine");
        assert_eq!(
            run_args(&["job", "status", "--wait-timeout", "-1"], "").0,
            EXIT_USAGE
        );
    }

    #[test]
    fn data_dir_is_taken_out_and_made_absolute() {
        let mut argv: Vec<String> = ["--data-dir", "rel", "doctor"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dir = take_data_dir_flag(&mut argv).unwrap().unwrap();
        assert!(dir.is_absolute());
        assert_eq!(argv, ["doctor"]);
        let mut bad: Vec<String> = vec!["--data-dir".into()];
        assert!(take_data_dir_flag(&mut bad).is_err());
    }
}
