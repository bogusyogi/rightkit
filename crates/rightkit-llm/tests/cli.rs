//! `provider::cli` against a fake CLI: a `/bin/sh` script written by the test. No
//! test calls a real model or the network.
#![cfg(all(feature = "cli", unix))]

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_llm::failover::{FailoverChain, FailoverPolicy, Route};
use rightkit_llm::provider::cli::{
    resolve_program, CancelHandle, CliError, CliLimits, CliProvider, CliSpec, JsonOutput,
    OutputFormat, PromptDelivery, StreamKind, UsageReport,
};
use rightkit_llm::{
    ChatRequest, ChatResponse, ContentBlock, LlmError, Message, Metering, PriceTable, Provider,
    StopReason, StreamEvent, ToolSpec, Usage,
};

const SCRIPT: &str = r#"
mode="$1"; shift
case "$mode" in
  text) cat ;;
  json)
    cat >/dev/null
    printf '%s' '{"result":"hello json","is_error":false,"usage":{"input_tokens":11,"output_tokens":7,"cache_read_input_tokens":3,"cache_creation_input_tokens":2},"total_cost_usd":0.0123}' ;;
  jsonnousage)
    cat >/dev/null
    printf '%s' '{"result":"no usage here","is_error":false}' ;;
  jsonerror)
    cat >/dev/null
    printf '%s' '{"result":"Not logged in - Please run /login","is_error":true}'; exit 1 ;;
  jsonmissing)
    cat >/dev/null
    printf '%s' '{"other":1,"is_error":false}' ;;
  argv) printf '%s\n' "$@" ;;
  hang)
    echo $$ > pid
    sleep 300 &
    echo $! > gpid
    wait ;;
  idle)
    printf x
    echo $$ > pid
    sleep 300 ;;
  bigout)
    cat >/dev/null
    head -c 5000000 /dev/zero | tr '\0' a ;;
  bigerr)
    echo $$ > pid
    head -c 100000 /dev/zero | tr '\0' e >&2
    sleep 300 ;;
  fail)
    cat >/dev/null
    echo "boom sk-ant-api03-ABCDEFGHIJKLMNOP123456 token=hunter2" >&2
    exit 3 ;;
  signedout)
    cat >/dev/null
    echo "Not logged in. Please run /login" >&2
    exit 1 ;;
  badjson)
    cat >/dev/null
    printf '%s' '{not json' ;;
  env) env ;;
  pwd) pwd -P ;;
  empty) cat >/dev/null ;;
esac
"#;

struct Fixture {
    base: PathBuf,
    script: PathBuf,
    scratch: PathBuf,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "rk-llm-cli-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let base = {
            std::fs::create_dir_all(&base).unwrap();
            base.canonicalize().unwrap()
        };
        let script = base.join("fake.sh");
        std::fs::write(&script, SCRIPT).unwrap();
        let scratch = base.join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        Self {
            base,
            script,
            scratch,
        }
    }

    fn spec(&self, mode: &str, output: OutputFormat) -> CliSpec {
        CliSpec {
            id: "fake".into(),
            program: PathBuf::from("/bin/sh"),
            args: vec![self.script.display().to_string(), mode.into()],
            model_args: vec!["--model".into(), "{model}".into()],
            prompt: PromptDelivery::Stdin,
            output,
            env_allow: vec!["HOME".into(), "PATH".into()],
            env_set: Vec::new(),
            not_signed_in_patterns: vec!["not logged in".into()],
            not_signed_in_exit_codes: Vec::new(),
            limits: CliLimits {
                wall_timeout: Duration::from_secs(20),
                idle_timeout: Duration::from_secs(20),
                max_stdout_bytes: 64 << 10,
                max_stderr_bytes: 4 << 10,
                max_prompt_bytes: 1 << 20,
            },
        }
    }

    fn provider(&self, spec: CliSpec) -> CliProvider {
        CliProvider::new(spec, &self.scratch).unwrap()
    }

    fn text(&self, mode: &str) -> CliProvider {
        self.provider(self.spec(mode, OutputFormat::Text))
    }

    fn json(&self, mode: &str) -> CliProvider {
        self.provider(self.spec(
            mode,
            OutputFormat::Json(JsonOutput {
                text: "/result".into(),
                error_flag: Some("/is_error".into()),
                model: None,
                input_tokens: Some("/usage/input_tokens".into()),
                output_tokens: Some("/usage/output_tokens".into()),
                cache_read_tokens: Some("/usage/cache_read_input_tokens".into()),
                cache_write_tokens: Some("/usage/cache_creation_input_tokens".into()),
                cost_usd: Some("/total_cost_usd".into()),
            }),
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn req(prompt: &str) -> ChatRequest {
    ChatRequest::new("", vec![Message::user(prompt)])
}

fn cli_err(e: LlmError) -> CliError {
    match e {
        LlmError::Cli(c) => c,
        other => panic!("expected a CLI error, got {other}"),
    }
}

fn pid_file(dir: &Path, name: &str) -> u32 {
    std::fs::read_to_string(dir.join(name))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Poll until `name` holds a parseable pid (the fake writes it, so it may be half-written).
fn wait_for_pid_file(dir: &Path, name: &str) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "{name} never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success();
        if !alive {
            return;
        }
        assert!(Instant::now() < deadline, "process {pid} is still alive");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn text_success_reports_unknown_usage_and_subscription_metering() {
    let fx = Fixture::new();
    let p = fx.text("text");
    let r = p.complete(&req("say hi")).unwrap();
    assert_eq!(r.text(), "say hi");
    assert_eq!(r.stop_reason, StopReason::EndTurn);
    assert!(!r.usage_known, "usage must be unknown, not zero");
    assert_eq!(r.metering, Metering::Subscription);
    assert_eq!(PriceTable::bundled().cost_of(&r), None);
    let d = p.complete_detailed(&req("x")).unwrap();
    assert_eq!(d.usage, UsageReport::Unknown);
}

#[test]
fn json_success_extracts_usage_and_cost() {
    let fx = Fixture::new();
    let d = fx.json("json").complete_detailed(&req("hello")).unwrap();
    assert_eq!(d.response.text(), "hello json");
    assert_eq!(
        d.usage,
        UsageReport::Known(Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_tokens: 3,
            cache_write_tokens: 2
        })
    );
    assert!(d.response.usage_known);
    assert_eq!(d.reported_cost_usd, Some(0.0123));
    assert_eq!(d.metering, Metering::Subscription);
    // Known usage on a subscription route is still never priced.
    assert_eq!(PriceTable::bundled().cost_of(&d.response), None);
}

#[test]
fn json_without_usage_is_unknown_not_zero() {
    let fx = Fixture::new();
    let d = fx
        .json("jsonnousage")
        .complete_detailed(&req("hello"))
        .unwrap();
    assert_eq!(d.usage, UsageReport::Unknown);
    assert!(!d.response.usage_known);
}

#[test]
fn prompt_with_dashes_and_shell_metacharacters_is_delivered_verbatim_on_stdin() {
    let fx = Fixture::new();
    let prompt = "--dangerously-skip-everything $(touch pwned) `touch pwned2` ; rm -rf / 'q' \"d\" \\ | & > x";
    let r = fx.text("text").complete(&req(prompt)).unwrap();
    assert_eq!(r.text(), prompt);
    assert!(!fx.scratch.join("pwned").exists());
    assert!(!fx.scratch.join("pwned2").exists());
    assert!(!fx.scratch.join("x").exists());
}

#[test]
fn prompt_after_terminator_is_the_last_argument_behind_dashdash() {
    let fx = Fixture::new();
    let mut spec = fx.spec("argv", OutputFormat::Text);
    spec.prompt = PromptDelivery::ArgAfterTerminator;
    let r = fx
        .provider(spec)
        .complete(&req("--help;touch pwned"))
        .unwrap();
    let lines: Vec<String> = r.text().lines().map(String::from).collect();
    assert_eq!(&lines[lines.len() - 2..], ["--", "--help;touch pwned"]);
    assert!(!fx.scratch.join("pwned").exists());
}

#[test]
fn model_is_an_argument_only_when_it_is_a_plain_token() {
    let fx = Fixture::new();
    let mut spec = fx.spec("argv", OutputFormat::Text);
    spec.prompt = PromptDelivery::ArgAfterTerminator;
    let p = fx.provider(spec);
    let mut r = req("hi");
    r.model = "claude-sonnet-5".into();
    let out = p.complete(&r).unwrap().text();
    assert!(out.contains("--model\nclaude-sonnet-5\n--\nhi"), "{out}");
    r.model = "--dangerous".into();
    assert!(matches!(
        cli_err(p.complete(&r).unwrap_err()),
        CliError::InvalidRequest(_)
    ));
}

#[test]
fn wall_timeout_kills_child_and_descendants() {
    let fx = Fixture::new();
    let mut spec = fx.spec("hang", OutputFormat::Text);
    spec.limits.wall_timeout = Duration::from_millis(600);
    let started = Instant::now();
    let e = cli_err(fx.provider(spec).complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::Timeout { .. }), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(pid_file(&fx.scratch, "pid"));
    assert_gone(pid_file(&fx.scratch, "gpid"));
}

#[test]
fn cancel_mid_run_kills_the_process_tree_and_returns_cancelled() {
    let fx = Fixture::new();
    let p = fx.text("hang");
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();
    let scratch = fx.scratch.clone();
    let canceller = std::thread::spawn(move || {
        // Cancel only once the fake CLI and its background child are both running.
        wait_for_pid_file(&scratch, "gpid");
        trigger.cancel();
    });
    let started = Instant::now();
    let e = p.complete_detailed_cancellable(&req("x"), &cancel).unwrap_err();
    canceller.join().unwrap();
    assert_eq!(e, CliError::Cancelled);
    assert!(!e.is_failover_trigger());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(pid_file(&fx.scratch, "pid"));
    assert_gone(pid_file(&fx.scratch, "gpid"));
}

#[test]
fn cancel_before_start_returns_cancelled_without_spawning() {
    let fx = Fixture::new();
    let p = fx.text("hang");
    let cancel = CancelHandle::new();
    cancel.cancel();
    assert!(cancel.is_cancelled());
    let e = p.complete_detailed_cancellable(&req("x"), &cancel).unwrap_err();
    assert_eq!(e, CliError::Cancelled);
    assert!(!fx.scratch.join("pid").exists(), "the CLI must not start");
}

#[test]
fn claude_code_answer_only_preset_argv_is_exact() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    // A stand-in `claude` that records the argv it was started with (in its cwd, the
    // scratch directory) and answers in the preset's JSON shape.
    let claude = fx.base.join("claude");
    std::fs::write(
        &claude,
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' "$@" > argv.txt
printf '%s' '{"result":"ok","is_error":false}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    let provider = fx.provider(CliSpec::claude_code(claude.clone()));
    let out = provider
        .complete_detailed(&ChatRequest::new("claude-sonnet-5", vec![Message::user("hi")]))
        .unwrap();
    assert_eq!(out.response.text(), "ok");

    let argv: Vec<String> = std::fs::read_to_string(fx.scratch.join("argv.txt"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    // Answer-only argv, verified against Claude Code 2.1.300 (see CliSpec::claude_code).
    // The prompt goes on stdin, never in argv. `--disallowedTools "*"` removes every tool
    // (built-in and MCP); `--tools ""` disables the built-ins; `--strict-mcp-config` with
    // no `--mcp-config` loads no MCP servers.
    let expected: Vec<&str> = vec![
        "-p",
        "--output-format",
        "json",
        "--no-session-persistence",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--setting-sources",
        "local",
        "--disallowedTools",
        "*",
        "--tools",
        "",
        "--model",
        "claude-sonnet-5",
    ];
    assert_eq!(argv, expected);
}

#[test]
fn idle_timeout_kills_a_silent_child() {
    let fx = Fixture::new();
    let mut spec = fx.spec("idle", OutputFormat::Text);
    spec.limits.idle_timeout = Duration::from_millis(400);
    let e = cli_err(fx.provider(spec).complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::IdleTimeout { .. }), "{e}");
    assert_gone(pid_file(&fx.scratch, "pid"));
}

#[test]
fn stdout_over_cap_is_rejected() {
    let fx = Fixture::new();
    let mut spec = fx.spec("bigout", OutputFormat::Text);
    spec.limits.max_stdout_bytes = 1000;
    let e = cli_err(fx.provider(spec).complete(&req("x")).unwrap_err());
    assert_eq!(
        e,
        CliError::OutputTooLarge {
            stream: StreamKind::Stdout,
            limit: 1000
        }
    );
}

#[test]
fn stderr_over_cap_kills_the_child() {
    let fx = Fixture::new();
    let mut spec = fx.spec("bigerr", OutputFormat::Text);
    spec.limits.max_stderr_bytes = 1000;
    let e = cli_err(fx.provider(spec).complete(&req("x")).unwrap_err());
    assert_eq!(
        e,
        CliError::OutputTooLarge {
            stream: StreamKind::Stderr,
            limit: 1000
        }
    );
    assert_gone(pid_file(&fx.scratch, "pid"));
}

#[test]
fn non_zero_exit_carries_redacted_truncated_stderr() {
    let fx = Fixture::new();
    let e = cli_err(fx.text("fail").complete(&req("x")).unwrap_err());
    match e {
        CliError::NonZeroExit { code, stderr } => {
            assert_eq!(code, Some(3));
            assert!(stderr.contains("boom"));
            assert!(!stderr.contains("ABCDEFGHIJ"), "{stderr}");
            assert!(!stderr.contains("hunter2"), "{stderr}");
        }
        other => panic!("{other}"),
    }
}

#[test]
fn not_signed_in_is_detected_from_stderr_and_from_json() {
    let fx = Fixture::new();
    let e = cli_err(fx.text("signedout").complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::NotSignedIn { .. }), "{e}");
    let e = cli_err(fx.json("jsonerror").complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::NotSignedIn { .. }), "{e}");
}

#[test]
fn malformed_json_and_missing_field_are_typed_errors() {
    let fx = Fixture::new();
    let e = cli_err(fx.json("badjson").complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::BadOutput(_)), "{e}");
    let e = cli_err(fx.json("jsonmissing").complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::BadOutput(_)), "{e}");
    let e = cli_err(fx.text("empty").complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::BadOutput(_)), "{e}");
}

#[test]
fn missing_program_is_not_installed() {
    let fx = Fixture::new();
    let mut spec = fx.spec("text", OutputFormat::Text);
    spec.program = PathBuf::from("/nonexistent/rightkit-fake-cli");
    let e = cli_err(fx.provider(spec).complete(&req("x")).unwrap_err());
    assert!(matches!(e, CliError::NotInstalled { .. }), "{e}");
}

#[test]
fn environment_is_allow_listed_and_secrets_never_reach_the_child() {
    let fx = Fixture::new();
    std::env::set_var("RK_CLI_TEST_PLANTED", "planted-value");
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-planted-key-0123456789");
    std::env::set_var("OPENAI_API_KEY", "sk-planted-openai-0123456789");
    let mut spec = fx.spec("env", OutputFormat::Text);
    spec.env_set.push(("RK_FIXED".into(), "fixed".into()));
    let out = fx.provider(spec).complete(&req("x")).unwrap().text();
    assert!(!out.contains("planted"), "{out}");
    assert!(!out.contains("API_KEY"), "{out}");
    assert!(out.contains("RK_FIXED=fixed"));
    for line in out.lines() {
        let name = line.split('=').next().unwrap();
        assert!(
            ["HOME", "PATH", "PWD", "SHLVL", "_", "OLDPWD", "RK_FIXED", "LC_CTYPE"].contains(&name),
            "unexpected variable in child env: {name}"
        );
    }
}

#[test]
fn working_directory_is_the_supplied_scratch_dir() {
    let fx = Fixture::new();
    let out = fx.text("pwd").complete(&req("x")).unwrap().text();
    assert_eq!(PathBuf::from(out), fx.scratch.canonicalize().unwrap());
}

#[test]
fn tool_use_is_rejected_with_a_typed_error() {
    let fx = Fixture::new();
    let p = fx.text("text");
    let mut r = req("x");
    r.tools.push(ToolSpec {
        name: "t".into(),
        description: "d".into(),
        input_schema: serde_json::json!({}),
        strict: false,
    });
    assert!(matches!(
        cli_err(p.complete(&r).unwrap_err()),
        CliError::Unsupported(_)
    ));
    let mut r = req("x");
    r.force_tool = Some("t".into());
    assert!(matches!(
        cli_err(p.complete(&r).unwrap_err()),
        CliError::Unsupported(_)
    ));
    let r = ChatRequest::new("", vec![Message::tool_result("1", "out", false)]);
    assert!(matches!(
        cli_err(p.complete(&r).unwrap_err()),
        CliError::Unsupported(_)
    ));
    let mut assistant_tool = Message::assistant("");
    assistant_tool.content = vec![ContentBlock::ToolUse {
        id: "1".into(),
        name: "t".into(),
        input: serde_json::json!({}),
    }];
    let r = ChatRequest::new("", vec![assistant_tool]);
    assert!(matches!(
        cli_err(p.complete(&r).unwrap_err()),
        CliError::Unsupported(_)
    ));
}

#[test]
fn spec_validation_rejects_unsafe_specs() {
    let fx = Fixture::new();
    let mut spec = fx.spec("text", OutputFormat::Text);
    spec.program = PathBuf::from("claude");
    assert!(matches!(
        CliProvider::new(spec, &fx.scratch).unwrap_err(),
        CliError::InvalidSpec(_)
    ));
    let mut spec = fx.spec("text", OutputFormat::Text);
    spec.env_allow.push("ANTHROPIC_API_KEY".into());
    assert!(matches!(
        CliProvider::new(spec, &fx.scratch).unwrap_err(),
        CliError::InvalidSpec(_)
    ));
    // A non-empty scratch directory is refused.
    std::fs::write(fx.scratch.join("app-data"), "x").unwrap();
    assert!(matches!(
        CliProvider::new(fx.spec("text", OutputFormat::Text), &fx.scratch).unwrap_err(),
        CliError::InvalidSpec(_)
    ));
}

#[test]
fn resolve_program_is_an_explicit_host_step() {
    let found = resolve_program("sh", std::ffi::OsStr::new("/bin:/usr/bin")).unwrap();
    assert!(found.is_absolute() && found.ends_with("sh"));
    assert_eq!(resolve_program("../sh", std::ffi::OsStr::new("/bin")), None);
    assert_eq!(
        resolve_program("nope-not-here", std::ffi::OsStr::new("/bin:/usr/bin")),
        None
    );
}

#[test]
fn presets_are_valid_specs() {
    let fx = Fixture::new();
    for spec in [
        CliSpec::claude_code("/bin/sh"),
        CliSpec::codex_exec("/bin/sh"),
    ] {
        CliProvider::new(spec, &fx.scratch).unwrap();
    }
    let claude = CliSpec::claude_code("/bin/sh");
    assert!(claude.args.windows(2).any(|w| w == ["--tools", ""]));
    assert!(CliSpec::codex_exec("/bin/sh")
        .args
        .windows(2)
        .any(|w| w == ["--sandbox", "read-only"]));
}

#[test]
fn attempt_record_matches_failover_attempts() {
    let fx = Fixture::new();
    let ok = fx.text("text").attempt(&req("hi"));
    assert_eq!(ok.attempt.route, "fake");
    assert_eq!(ok.attempt.reason, "ok");
    assert!(ok.outcome.is_ok());
    let bad = fx.text("fail").attempt(&req("hi"));
    assert_eq!(bad.attempt.route, "fake");
    assert!(
        bad.attempt.reason.contains("exited with status 3"),
        "{}",
        bad.attempt.reason
    );
    assert!(bad.outcome.is_err());
}

#[test]
fn stream_is_buffered_into_one_text_event() {
    let fx = Fixture::new();
    let p = fx.text("text");
    let mut events = Vec::new();
    let r = p
        .stream(&req("streamed"), &mut |e| {
            events.push(e);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(events, vec![StreamEvent::Text("streamed".into())]);
    assert_eq!(r.text(), "streamed");
    let cancelled = p.stream(&req("x"), &mut |_| ControlFlow::Break(()));
    assert!(matches!(cancelled, Err(LlmError::Cancelled)));
}

struct Stub;
impl Provider for Stub {
    fn id(&self) -> &str {
        "stub"
    }
    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        Ok(ChatResponse {
            id: "s".into(),
            model: req.model.clone(),
            content: vec![ContentBlock::text("from stub")],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            usage_known: true,
            metering: Metering::PerToken,
        })
    }
    fn stream(
        &self,
        req: &ChatRequest,
        _: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        self.complete(req)
    }
}

#[test]
fn failover_moves_past_a_failed_cli_route() {
    let fx = Fixture::new();
    let mut spec = fx.spec("text", OutputFormat::Text);
    spec.program = PathBuf::from("/nonexistent/rightkit-fake-cli");
    let chain = FailoverChain::new(
        vec![
            Route::new("cli", Arc::new(fx.provider(spec)), ""),
            Route::new("stub", Arc::new(Stub), "m"),
        ],
        FailoverPolicy::default(),
    );
    assert_eq!(chain.complete(&req("hi")).unwrap().text(), "from stub");
}

/// Helper, not a test of its own: the parent process of `child_dies_when_the_parent_is_sigkilled`.
/// It does nothing unless that test launched it with the fixture directory in the environment;
/// then it runs one call against the hanging fake CLI and blocks until it is killed.
#[test]
fn sigkill_parent_helper() {
    let Ok(base) = std::env::var("RK_LLM_PDEATH_BASE") else {
        return;
    };
    let base = PathBuf::from(base);
    let fx = Fixture {
        script: base.join("fake.sh"),
        scratch: base.join("scratch"),
        base,
    };
    let p = fx.text("hang");
    // The test kills this process mid-call; no destructor may run, so the fixture is leaked.
    std::mem::forget(fx);
    let _ = p.complete_detailed(&req("x"));
}

#[test]
fn child_dies_when_the_parent_is_sigkilled() {
    let fx = Fixture::new();
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sigkill_parent_helper", "--nocapture", "--test-threads=1"])
        .env("RK_LLM_PDEATH_BASE", &fx.base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Both the fake CLI and its background child are running before the parent dies.
    let pid = wait_for_pid_file(&fx.scratch, "pid");
    let gpid = wait_for_pid_file(&fx.scratch, "gpid");
    // `Child::kill` is SIGKILL: nothing in the parent gets to clean up.
    parent.kill().unwrap();
    parent.wait().unwrap();
    // The macOS supervisor polls once a second; allow for that.
    let deadline = Instant::now() + Duration::from_secs(10);
    for pid in [pid, gpid] {
        loop {
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success();
            if !alive {
                break;
            }
            if Instant::now() >= deadline {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
                panic!("process {pid} survived SIGKILL of its parent");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
