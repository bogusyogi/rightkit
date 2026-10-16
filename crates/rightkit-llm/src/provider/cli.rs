//! Provider for an already-installed, already-signed-in command-line assistant
//! (Claude Code `claude -p`, OpenAI `codex exec`), run as a bounded child process
//! through `rightkit-child-process`. No API key is stored, read or passed by this crate:
//! the tool authenticates itself from its own credential store.
//!
//! Text completion only. Tool use and forced-tool structured output are rejected
//! with [`CliError::Unsupported`]. Streaming is buffered: the complete answer is
//! delivered as one [`StreamEvent::Text`].
//!
//! The tool is described by data ([`CliSpec`]): an absolute program path resolved
//! once by the host, a fixed argument template, stdin prompt delivery (or a `--`
//! terminated final argument), an output format, and optional JSON pointers for
//! usage. There is no shell: the child is spawned with an argument vector, a
//! minimal allow-listed environment and an empty scratch working directory.
//!
//! Every run is bounded by a wall-clock timeout, an idle (no output) timeout and
//! stdout/stderr byte caps. Any bound kills the child and its descendants through
//! `rightkit-child-process` and returns a typed [`CliError`].

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{OwnedCommand, ReapOutcome, TerminationOptions};
use serde_json::Value;

use crate::error::{Attempt, LlmError};
use crate::provider::Provider;
use crate::types::*;

/// Maximum bytes of tool stderr or output text echoed into an error message.
const ERROR_TEXT_MAX: usize = 512;
/// After the child exits, how long to wait for its pipes to reach EOF before
/// concluding that a descendant kept them open and tearing the tree down.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Bound on killing and reaping the tree after a limit is hit.
const KILL_BOUND: Duration = Duration::from_secs(5);
/// TERM-to-KILL grace. Nonzero so an exited leader is reaped before the
/// follow-up KILL (macOS answers EPERM for a signal to an all-zombie group).
const KILL_GRACE: Duration = Duration::from_millis(300);
const TICK: Duration = Duration::from_millis(25);

/// How the prompt reaches the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptDelivery {
    /// Written to the child's stdin, then stdin is closed. Preferred.
    #[default]
    Stdin,
    /// Appended as the final argument after a `--` terminator, for tools with no
    /// stdin support. The prompt can never be parsed as a flag.
    ArgAfterTerminator,
}

/// JSON pointers (RFC 6901) into the tool's JSON output. Everything is data:
/// nothing from the output is executed or used as a path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JsonOutput {
    /// Pointer to the answer text (a JSON string). Required.
    pub text: String,
    /// Pointer to a boolean that is true when the tool reports an error.
    pub error_flag: Option<String>,
    /// Pointer to the model name the tool actually used.
    pub model: Option<String>,
    /// Usage pointers. Usage is "known" only when both `input_tokens` and
    /// `output_tokens` resolve to unsigned integers.
    pub input_tokens: Option<String>,
    pub output_tokens: Option<String>,
    pub cache_read_tokens: Option<String>,
    pub cache_write_tokens: Option<String>,
    /// Pointer to a reported cost in USD (informational; not a price lookup).
    pub cost_usd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Stdout is the answer (UTF-8 text).
    #[default]
    Text,
    /// Stdout is one JSON document.
    Json(JsonOutput),
}

/// Bounds for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliLimits {
    pub wall_timeout: Duration,
    /// Maximum time with no stdout or stderr output.
    pub idle_timeout: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    /// Maximum rendered prompt size.
    pub max_prompt_bytes: usize,
}

impl Default for CliLimits {
    fn default() -> Self {
        Self {
            wall_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(90),
            max_stdout_bytes: 1 << 20,
            max_stderr_bytes: 64 << 10,
            max_prompt_bytes: 1 << 20,
        }
    }
}

/// A command-line assistant described as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSpec {
    /// Provider and route id (for example `claude-code`).
    pub id: String,
    /// Absolute path to the executable, resolved once by the host (see
    /// [`resolve_program`]). A bare name is rejected.
    pub program: PathBuf,
    /// Fixed leading arguments. Never contains user text.
    pub args: Vec<String>,
    /// Arguments added when the request names a model; `{model}` is replaced by
    /// the validated model id. Skipped when the request model is empty.
    pub model_args: Vec<String>,
    pub prompt: PromptDelivery,
    pub output: OutputFormat,
    /// Parent environment variables copied to the child. Everything else is
    /// dropped. Names that look like credentials are rejected.
    pub env_allow: Vec<String>,
    /// Fixed variables set in the child (name, value).
    pub env_set: Vec<(String, String)>,
    /// Case-insensitive substrings that, found in stderr or stdout of a failed
    /// run, mean the tool is not signed in.
    pub not_signed_in_patterns: Vec<String>,
    /// Exit codes that mean the tool is not signed in.
    pub not_signed_in_exit_codes: Vec<i32>,
    pub limits: CliLimits,
}

/// Variables every tool needs to find its home and helpers.
fn base_env_allow() -> Vec<String> {
    #[allow(unused_mut)]
    let mut v: Vec<String> = vec!["HOME".into(), "PATH".into()];
    #[cfg(windows)]
    v.extend(
        [
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "SystemRoot",
            "TEMP",
            "TMP",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    v
}

impl CliSpec {
    /// Claude Code in print mode as an answer-only assistant: every tool disabled
    /// (built-in, MCP and any other), no MCP servers, no session persistence, no
    /// skills, user and project settings (and so hooks and plugins) skipped. The
    /// caller runs it in an empty scratch directory (see [`CliProvider::new`]).
    ///
    /// How each part disables tools, per the Claude Code CLI reference
    /// (<https://code.claude.com/docs/en/cli-reference>):
    /// - `--tools ""` disables all built-in tools. The docs say the flag "doesn't
    ///   affect MCP tools", so it is not enough alone.
    /// - `--disallowedTools "*"` "removes every tool" from Claude's context, MCP tools
    ///   included (`"mcp__*"` would cover only MCP). This is the belt that covers
    ///   tools `--tools` does not reach, such as managed or plugin MCP servers.
    /// - `--strict-mcp-config` with no `--mcp-config` uses no MCP servers from any
    ///   other configuration.
    /// - `--disable-slash-commands` disables skills; `--setting-sources local` skips
    ///   user settings (so user hooks); `--no-session-persistence` keeps no transcript.
    ///
    /// Verified on Claude Code 2.1.300 (`claude --version`), against `claude --help`
    /// (read-only) and the CLI reference above, and pinned by the argv contract test
    /// `claude_code_answer_only_preset_argv_is_exact`. NOT verified by a live run,
    /// because that needs a model call: the runtime effect (no tool call happens
    /// under this argv), the JSON field names (`result`, `is_error`, `usage.*`,
    /// `total_cost_usd`), that `--setting-sources local` leaves the sign-in intact,
    /// and the not-signed-in patterns.
    pub fn claude_code(program: impl Into<PathBuf>) -> Self {
        let mut env_allow = base_env_allow();
        env_allow.push("CLAUDE_CONFIG_DIR".into());
        Self {
            id: "claude-code".into(),
            program: program.into(),
            args: [
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
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            model_args: vec!["--model".into(), "{model}".into()],
            prompt: PromptDelivery::Stdin,
            output: OutputFormat::Json(JsonOutput {
                text: "/result".into(),
                error_flag: Some("/is_error".into()),
                model: None,
                input_tokens: Some("/usage/input_tokens".into()),
                output_tokens: Some("/usage/output_tokens".into()),
                cache_read_tokens: Some("/usage/cache_read_input_tokens".into()),
                cache_write_tokens: Some("/usage/cache_creation_input_tokens".into()),
                cost_usd: Some("/total_cost_usd".into()),
            }),
            env_allow,
            env_set: Vec::new(),
            not_signed_in_patterns: vec!["not logged in".into(), "/login".into()],
            not_signed_in_exit_codes: Vec::new(),
            limits: CliLimits::default(),
        }
    }

    /// OpenAI Codex non-interactive mode: read-only sandbox, no session files, no
    /// user config or exec-policy rules, prompt on stdin via `-`.
    ///
    /// Flag status against `codex exec --help` (codex-cli 0.161.0) on the
    /// author's machine: `--sandbox read-only`, `--skip-git-repo-check`,
    /// `--ephemeral`, `--ignore-user-config`, `--ignore-rules`, `--color never`,
    /// `--model` and reading the prompt from stdin with `-` are present in the
    /// help text (verified). NOT verified, because no model call was made: that
    /// the final answer is the only thing on stdout in this mode, that tokens are
    /// not reported (usage is treated as unknown), and the not-signed-in
    /// patterns. Codex has no tools-off switch; the read-only sandbox is the
    /// strongest available restriction and still permits reads.
    pub fn codex_exec(program: impl Into<PathBuf>) -> Self {
        let mut env_allow = base_env_allow();
        env_allow.push("CODEX_HOME".into());
        Self {
            id: "codex-exec".into(),
            program: program.into(),
            args: [
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--color",
                "never",
                "-",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            model_args: vec!["--model".into(), "{model}".into()],
            prompt: PromptDelivery::Stdin,
            output: OutputFormat::Text,
            env_allow,
            env_set: Vec::new(),
            not_signed_in_patterns: vec!["not logged in".into(), "codex login".into()],
            not_signed_in_exit_codes: Vec::new(),
            limits: CliLimits::default(),
        }
    }

    fn validate(&self) -> Result<(), CliError> {
        let bad = |m: &str| Err(CliError::InvalidSpec(m.to_string()));
        if self.id.is_empty() {
            return bad("id is empty");
        }
        if !self.program.is_absolute() {
            return bad("program must be an absolute path (resolve it once with resolve_program)");
        }
        if self.args.iter().any(|a| a.contains('\0'))
            || self.model_args.iter().any(|a| a.contains('\0'))
        {
            return bad("arguments must not contain NUL");
        }
        for name in self
            .env_allow
            .iter()
            .chain(self.env_set.iter().map(|(n, _)| n))
        {
            if name.is_empty() || name.contains(['=', '\0']) {
                return bad("environment variable name is empty or malformed");
            }
            if looks_like_credential(name) {
                return Err(CliError::InvalidSpec(format!(
                    "environment variable '{name}' looks like a credential and cannot be passed"
                )));
            }
        }
        if self.env_set.iter().any(|(_, v)| v.contains('\0')) {
            return bad("environment values must not contain NUL");
        }
        if let OutputFormat::Json(j) = &self.output {
            if !pointer_ok(&j.text) {
                return bad("json text pointer must start with '/'");
            }
            for p in [
                &j.error_flag,
                &j.model,
                &j.input_tokens,
                &j.output_tokens,
                &j.cache_read_tokens,
                &j.cache_write_tokens,
                &j.cost_usd,
            ]
            .into_iter()
            .flatten()
            {
                if !pointer_ok(p) {
                    return bad("json pointers must start with '/'");
                }
            }
        }
        let l = &self.limits;
        if l.wall_timeout.is_zero()
            || l.idle_timeout.is_zero()
            || l.max_stdout_bytes == 0
            || l.max_stderr_bytes == 0
            || l.max_prompt_bytes == 0
        {
            return bad("limits must be non-zero");
        }
        Ok(())
    }
}

fn pointer_ok(p: &str) -> bool {
    p.is_empty() || p.starts_with('/')
}

fn looks_like_credential(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
        "COOKIE",
    ]
    .iter()
    .any(|w| u.contains(w))
}

/// Resolve a bare program name against `search_path` (a PATH-style list) once, at
/// host start-up, to an absolute path of an existing file. This is the host's
/// explicit opt-in to PATH lookup; the provider never searches PATH at call time.
pub fn resolve_program(name: &str, search_path: &std::ffi::OsStr) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\', '\0']) {
        return None;
    }
    std::env::split_paths(search_path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| is_executable_file(p))
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

/// Typed failures of a CLI route. Messages carry stderr only truncated and
/// redacted.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CliError {
    /// The spec is unusable (relative program, credential-like env name, ...).
    InvalidSpec(String),
    /// The request cannot be served by a text-only CLI route.
    InvalidRequest(String),
    /// Tool use, forced-tool output or another unsupported capability.
    Unsupported(String),
    /// The program does not exist or is not executable.
    NotInstalled { program: PathBuf },
    /// The tool reported that it is not signed in.
    NotSignedIn { detail: String },
    /// The caller cancelled the run through a [`CancelHandle`]; the process tree was
    /// killed. Not a failover trigger: the caller asked for the stop.
    Cancelled,
    /// Wall-clock timeout; the tree was killed.
    Timeout { after: Duration },
    /// No output for the idle timeout; the tree was killed.
    IdleTimeout { after: Duration },
    /// A stream exceeded its byte cap; the tree was killed.
    OutputTooLarge { stream: StreamKind, limit: usize },
    /// Output was not valid UTF-8 / JSON or lacked the expected field.
    BadOutput(String),
    /// The tool exited non-zero.
    NonZeroExit { code: Option<i32>, stderr: String },
    /// The tool's JSON said the run failed although it exited zero.
    ToolReportedError { message: String },
    /// The OS refused to start the child.
    Spawn(String),
    /// A bound was hit and the process tree could not be confirmed dead.
    Kill(String),
}

impl CliError {
    /// Whether another route could plausibly succeed.
    pub fn is_failover_trigger(&self) -> bool {
        !matches!(
            self,
            CliError::InvalidSpec(_)
                | CliError::InvalidRequest(_)
                | CliError::Unsupported(_)
                | CliError::Cancelled
        )
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::InvalidSpec(m) => write!(f, "invalid CLI spec: {m}"),
            CliError::InvalidRequest(m) => write!(f, "invalid CLI request: {m}"),
            CliError::Unsupported(m) => write!(f, "unsupported by CLI route: {m}"),
            CliError::NotInstalled { program } => {
                write!(f, "CLI not installed: {}", program.display())
            }
            CliError::NotSignedIn { detail } => write!(f, "CLI not signed in: {detail}"),
            CliError::Cancelled => write!(f, "CLI call was cancelled"),
            CliError::Timeout { after } => write!(f, "CLI timed out after {after:?}"),
            CliError::IdleTimeout { after } => write!(f, "CLI produced no output for {after:?}"),
            CliError::OutputTooLarge { stream, limit } => {
                write!(f, "CLI {stream:?} exceeded {limit} bytes")
            }
            CliError::BadOutput(m) => write!(f, "CLI output unusable: {m}"),
            CliError::NonZeroExit { code, stderr } => match code {
                Some(c) => write!(f, "CLI exited with status {c}: {stderr}"),
                None => write!(f, "CLI terminated by signal: {stderr}"),
            },
            CliError::ToolReportedError { message } => {
                write!(f, "CLI reported an error: {message}")
            }
            CliError::Spawn(m) => write!(f, "CLI could not start: {m}"),
            CliError::Kill(m) => write!(f, "CLI process tree not confirmed dead: {m}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<CliError> for LlmError {
    fn from(e: CliError) -> Self {
        LlmError::Cli(e)
    }
}

/// Token usage as the tool reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageReport {
    Known(Usage),
    /// The tool did not report token counts. Not zero.
    Unknown,
}

/// A successful run with the facts the plain [`ChatResponse`] cannot carry.
#[derive(Debug, Clone, PartialEq)]
pub struct CliCompletion {
    pub response: ChatResponse,
    pub usage: UsageReport,
    /// Cost the tool reported, if any. Informational: the route is
    /// subscription-metered, so price lookups do not apply.
    pub reported_cost_usd: Option<f64>,
    pub metering: Metering,
}

/// One attempt, successful or not, shaped like failover's [`Attempt`] records.
#[derive(Debug)]
pub struct CliAttempt {
    /// `route` is the provider id; `reason` is `ok` or the error text.
    pub attempt: Attempt,
    pub elapsed: Duration,
    pub outcome: Result<CliCompletion, CliError>,
}

/// Cancels one in-flight CLI call. Clones share the same flag, so the handle can be
/// kept by a UI or a daemon while the call runs on another thread.
#[derive(Debug, Clone, Default)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. A running call kills its whole process tree (through
    /// `rightkit-child-process`) within one poll tick and returns
    /// [`CliError::Cancelled`]. A call that has not started yet returns it without
    /// spawning anything. Idempotent.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Provider over a [`CliSpec`] and an explicit scratch working directory.
#[derive(Debug, Clone)]
pub struct CliProvider {
    spec: CliSpec,
    scratch: PathBuf,
}

impl CliProvider {
    /// `scratch` must be an existing, empty, absolute directory supplied by the
    /// host. The tool runs inside it and must not see the app's data.
    pub fn new(spec: CliSpec, scratch: impl Into<PathBuf>) -> Result<Self, CliError> {
        spec.validate()?;
        let scratch = scratch.into();
        if !scratch.is_absolute() {
            return Err(CliError::InvalidSpec(
                "scratch directory must be absolute".into(),
            ));
        }
        let mut entries = std::fs::read_dir(&scratch)
            .map_err(|e| CliError::InvalidSpec(format!("scratch directory unreadable: {e}")))?;
        if entries.next().is_some() {
            return Err(CliError::InvalidSpec(
                "scratch directory must be empty".into(),
            ));
        }
        Ok(Self { spec, scratch })
    }

    pub fn spec(&self) -> &CliSpec {
        &self.spec
    }

    /// Run one attempt and return its record whether or not it succeeded.
    pub fn attempt(&self, req: &ChatRequest) -> CliAttempt {
        let started = Instant::now();
        let outcome = self.run(req, None);
        let reason = match &outcome {
            Ok(_) => "ok".to_string(),
            Err(e) => e.to_string(),
        };
        CliAttempt {
            attempt: Attempt {
                route: self.spec.id.clone(),
                reason,
            },
            elapsed: started.elapsed(),
            outcome,
        }
    }

    /// Like `complete`, with usage reported as [`UsageReport`] and the cost.
    pub fn complete_detailed(&self, req: &ChatRequest) -> Result<CliCompletion, CliError> {
        self.run(req, None)
    }

    /// Like `complete_detailed`, but the call can be cancelled mid-run through `cancel`,
    /// which kills the process tree and returns [`CliError::Cancelled`]. The `Provider`
    /// trait has no cancel parameter, so cancellable calls go through this method.
    pub fn complete_detailed_cancellable(
        &self,
        req: &ChatRequest,
        cancel: &CancelHandle,
    ) -> Result<CliCompletion, CliError> {
        self.run(req, Some(cancel))
    }

    fn run(
        &self,
        req: &ChatRequest,
        cancel: Option<&CancelHandle>,
    ) -> Result<CliCompletion, CliError> {
        let prompt = render_prompt(req)?;
        if prompt.len() > self.spec.limits.max_prompt_bytes {
            return Err(CliError::InvalidRequest(
                "prompt exceeds max_prompt_bytes".into(),
            ));
        }
        if self.spec.prompt == PromptDelivery::ArgAfterTerminator && prompt.contains('\0') {
            return Err(CliError::InvalidRequest("prompt contains NUL".into()));
        }
        let model = validated_model(&req.model)?;
        if !is_executable_file(&self.spec.program) {
            return Err(CliError::NotInstalled {
                program: self.spec.program.clone(),
            });
        }
        if !self.scratch.is_dir() {
            return Err(CliError::InvalidSpec("scratch directory is missing".into()));
        }
        if cancel.is_some_and(CancelHandle::is_cancelled) {
            return Err(CliError::Cancelled);
        }

        let mut args: Vec<String> = self.spec.args.clone();
        if let Some(m) = model {
            args.extend(self.spec.model_args.iter().map(|a| a.replace("{model}", m)));
        }
        if self.spec.prompt == PromptDelivery::ArgAfterTerminator {
            args.push("--".into());
            args.push(prompt.clone());
        }

        let mut cmd = OwnedCommand::new(supervised_program(&self.spec.program));
        cmd.env_allowlist(&self.spec.env_allow);
        // Linux: the kernel signals the child when this process dies, SIGKILL included. The
        // call blocks on the child from the spawning thread, which satisfies the thread
        // keying. Windows is covered by the kill-on-close job object; other Unix hosts (macOS)
        // by the supervisor shell below.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        cmd.kill_on_parent_exit(true);
        {
            let c = cmd.command_mut();
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            c.args(["-c", PARENT_DEATH_SUPERVISOR, "rk-supervisor"])
                .arg(&self.spec.program);
            c.args(&args)
                .current_dir(&self.scratch)
                .stdin(match self.spec.prompt {
                    PromptDelivery::Stdin => Stdio::piped(),
                    PromptDelivery::ArgAfterTerminator => Stdio::null(),
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in &self.spec.env_set {
                c.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CliError::NotInstalled {
                program: self.spec.program.clone(),
            },
            _ => CliError::Spawn(e.to_string()),
        })?;

        let (tx, rx) = mpsc::channel::<Msg>();
        let out_buf = Arc::new(Mutex::new(Vec::new()));
        let err_buf = Arc::new(Mutex::new(Vec::new()));
        let limits = &self.spec.limits;
        if let Some(out) = child.take_stdout() {
            spawn_reader(
                out,
                StreamKind::Stdout,
                limits.max_stdout_bytes,
                out_buf.clone(),
                tx.clone(),
            );
        } else {
            let _ = tx.send(Msg::Eof(StreamKind::Stdout));
        }
        if let Some(err) = child.take_stderr() {
            spawn_reader(
                err,
                StreamKind::Stderr,
                limits.max_stderr_bytes,
                err_buf.clone(),
                tx.clone(),
            );
        } else {
            let _ = tx.send(Msg::Eof(StreamKind::Stderr));
        }
        drop(tx);
        if self.spec.prompt == PromptDelivery::Stdin {
            if let Some(mut stdin) = child.take_stdin() {
                let bytes = prompt.into_bytes();
                thread::spawn(move || {
                    // A tool that stops reading early is judged by its exit status.
                    let _ = stdin.write_all(&bytes);
                    let _ = stdin.flush();
                });
            }
        }

        let started = Instant::now();
        let mut last_output = started;
        let (mut out_eof, mut err_eof) = (false, false);
        let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
        let mut disconnected = false;

        enum Stop {
            Done(std::process::ExitStatus),
            Bound(CliError),
        }
        let stop = loop {
            let now = Instant::now();
            if cancel.is_some_and(CancelHandle::is_cancelled) {
                break Stop::Bound(CliError::Cancelled);
            }
            if now.duration_since(started) >= limits.wall_timeout {
                break Stop::Bound(CliError::Timeout {
                    after: limits.wall_timeout,
                });
            }
            if exited.is_none() && now.duration_since(last_output) >= limits.idle_timeout {
                break Stop::Bound(CliError::IdleTimeout {
                    after: limits.idle_timeout,
                });
            }
            if let Some((status, at)) = exited {
                if (out_eof && err_eof) || disconnected {
                    break Stop::Done(status);
                }
                if now.duration_since(at) >= PIPE_DRAIN_GRACE {
                    // A descendant holds the pipes open; the tree is torn down below
                    // and whatever was captured so far is judged.
                    break Stop::Done(status);
                }
            }
            match rx.recv_timeout(TICK) {
                Ok(Msg::Activity) => last_output = Instant::now(),
                Ok(Msg::Eof(StreamKind::Stdout)) => out_eof = true,
                Ok(Msg::Eof(StreamKind::Stderr)) => err_eof = true,
                Ok(Msg::Over(stream)) => {
                    let limit = match stream {
                        StreamKind::Stdout => limits.max_stdout_bytes,
                        StreamKind::Stderr => limits.max_stderr_bytes,
                    };
                    break Stop::Bound(CliError::OutputTooLarge { stream, limit });
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => disconnected = true,
            }
            if exited.is_none() {
                match child.try_wait() {
                    Ok(Some(status)) => exited = Some((status, Instant::now())),
                    Ok(None) => {}
                    Err(e) => break Stop::Bound(CliError::Spawn(format!("wait failed: {e}"))),
                }
            }
        };

        // Always take the whole tree down (descendants included) and reap it.
        let reaped = child.terminate_tree_bounded_with_options(
            KILL_BOUND,
            TerminationOptions::default().with_grace(KILL_GRACE),
        );
        let status = match stop {
            Stop::Bound(e) => {
                return match reaped {
                    Ok(ReapOutcome::Exited(_)) => Err(e),
                    Ok(ReapOutcome::TimedOut) => Err(CliError::Kill(format!(
                        "{e}; tree still alive after {KILL_BOUND:?}"
                    ))),
                    Err(io) => Err(CliError::Kill(format!("{e}; kill failed: {io}"))),
                };
            }
            Stop::Done(status) => status,
        };

        let stdout = out_buf.lock().map(|b| b.clone()).unwrap_or_default();
        let stderr = err_buf.lock().map(|b| b.clone()).unwrap_or_default();
        let stderr_text = String::from_utf8_lossy(&stderr).into_owned();

        if !status.success() {
            let stdout_text = String::from_utf8_lossy(&stdout);
            if self.signed_out(status.code(), &stderr_text, &stdout_text) {
                return Err(CliError::NotSignedIn {
                    detail: redact_and_truncate(&stderr_text, ERROR_TEXT_MAX),
                });
            }
            return Err(CliError::NonZeroExit {
                code: status.code(),
                stderr: redact_and_truncate(&stderr_text, ERROR_TEXT_MAX),
            });
        }
        self.parse(req, &stdout, &stderr_text)
    }

    fn signed_out(&self, code: Option<i32>, stderr: &str, stdout: &str) -> bool {
        if code.is_some_and(|c| self.spec.not_signed_in_exit_codes.contains(&c)) {
            return true;
        }
        if self.spec.not_signed_in_patterns.is_empty() {
            return false;
        }
        let hay = format!("{stderr}\n{stdout}").to_lowercase();
        self.spec
            .not_signed_in_patterns
            .iter()
            .any(|p| !p.is_empty() && hay.contains(&p.to_lowercase()))
    }

    fn parse(
        &self,
        req: &ChatRequest,
        stdout: &[u8],
        stderr: &str,
    ) -> Result<CliCompletion, CliError> {
        let text_of = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| CliError::BadOutput("stdout is not valid UTF-8".into()))
        };
        let (answer, model, usage, cost) = match &self.spec.output {
            OutputFormat::Text => (text_of(stdout)?, None, UsageReport::Unknown, None),
            OutputFormat::Json(j) => {
                let doc: Value = serde_json::from_slice(stdout)
                    .map_err(|e| CliError::BadOutput(format!("malformed JSON: {e}")))?;
                if let Some(p) = &j.error_flag {
                    if doc.pointer(p).and_then(Value::as_bool) == Some(true) {
                        let msg = doc
                            .pointer(&j.text)
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if self.signed_out(None, stderr, &msg) {
                            return Err(CliError::NotSignedIn {
                                detail: redact_and_truncate(&msg, ERROR_TEXT_MAX),
                            });
                        }
                        return Err(CliError::ToolReportedError {
                            message: redact_and_truncate(&msg, ERROR_TEXT_MAX),
                        });
                    }
                }
                let text = doc
                    .pointer(&j.text)
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CliError::BadOutput("answer field missing or not a string".into())
                    })?
                    .to_string();
                let model = j
                    .model
                    .as_ref()
                    .and_then(|p| doc.pointer(p))
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(128).collect::<String>());
                let num = |p: &Option<String>| -> Option<u64> {
                    p.as_ref()
                        .and_then(|p| doc.pointer(p))
                        .and_then(Value::as_u64)
                };
                let usage = match (num(&j.input_tokens), num(&j.output_tokens)) {
                    (Some(i), Some(o)) => UsageReport::Known(Usage {
                        input_tokens: i,
                        output_tokens: o,
                        cache_read_tokens: num(&j.cache_read_tokens).unwrap_or(0),
                        cache_write_tokens: num(&j.cache_write_tokens).unwrap_or(0),
                    }),
                    _ => UsageReport::Unknown,
                };
                let cost = j
                    .cost_usd
                    .as_ref()
                    .and_then(|p| doc.pointer(p))
                    .and_then(Value::as_f64)
                    .filter(|c| c.is_finite() && *c >= 0.0);
                (text, model, usage, cost)
            }
        };
        let answer = answer.trim_end().to_string();
        if answer.is_empty() {
            return Err(CliError::BadOutput("empty answer".into()));
        }
        let (u, known) = match usage {
            UsageReport::Known(u) => (u, true),
            UsageReport::Unknown => (Usage::default(), false),
        };
        Ok(CliCompletion {
            response: ChatResponse {
                id: String::new(),
                model: model.unwrap_or_else(|| req.model.clone()),
                content: vec![ContentBlock::text(answer)],
                stop_reason: StopReason::EndTurn,
                usage: u,
                usage_known: known,
                metering: Metering::Subscription,
            },
            usage,
            reported_cost_usd: cost,
            metering: Metering::Subscription,
        })
    }
}

impl Provider for CliProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.run(req, None)
            .map(|c| c.response)
            .map_err(LlmError::Cli)
    }

    /// Buffered: the tool runs to completion, then the answer is delivered as
    /// one text event.
    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        let response = self.complete(req)?;
        if on_event(StreamEvent::Text(response.text())).is_break() {
            return Err(LlmError::Cancelled);
        }
        Ok(response)
    }
}

enum Msg {
    Activity,
    Eof(StreamKind),
    Over(StreamKind),
}

/// macOS and other non-Linux Unix: `rightkit-child-process` has no parent-death primitive
/// there, so the tool runs under a small `/bin/sh` supervisor. The supervisor starts the tool
/// (stdin, stdout and stderr pass straight through), and a watcher polls once a second. When
/// the supervisor's parent id changes (the host died and the child was re-parented, which
/// happens on SIGKILL) or the supervisor itself is gone, the watcher kills the tool's process
/// group, or the tool and its direct children when the supervisor is not a group leader.
/// Absolute tool paths are used because the child environment is allow-listed.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const PARENT_DEATH_SUPERVISOR: &str = r#"p=$PPID
"$@" <&0 &
c=$!
(
  while kill -0 "$c" 2>/dev/null; do
    q=$(/bin/ps -o ppid= -p "$$" 2>/dev/null)
    q=${q##* }
    if { [ -n "$q" ] && [ "$q" != "$p" ]; } || ! kill -0 "$$" 2>/dev/null; then
      g=$(/bin/ps -o pgid= -p "$c" 2>/dev/null)
      g=${g##* }
      if [ -n "$g" ] && [ "$g" = "$$" ]; then
        kill -s KILL -- "-$g" 2>/dev/null
      fi
      /usr/bin/pkill -KILL -P "$c" 2>/dev/null
      kill -s KILL "$c" 2>/dev/null
      exit 0
    fi
    /bin/sleep 1
  done
) >/dev/null 2>&1 </dev/null &
w=$!
wait "$c"
s=$?
kill "$w" 2>/dev/null
exit "$s"
"#;

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn supervised_program(_program: &Path) -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(not(all(unix, not(any(target_os = "linux", target_os = "android")))))]
fn supervised_program(program: &Path) -> PathBuf {
    program.to_path_buf()
}

fn spawn_reader(
    mut r: impl Read + Send + 'static,
    kind: StreamKind,
    cap: usize,
    buf: Arc<Mutex<Vec<u8>>>,
    tx: Sender<Msg>,
) {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) => {
                    let _ = tx.send(Msg::Eof(kind));
                    return;
                }
                Ok(n) => {
                    let over = {
                        let Ok(mut b) = buf.lock() else { return };
                        let room = cap - b.len();
                        if n > room {
                            b.extend_from_slice(&chunk[..room]);
                            true
                        } else {
                            b.extend_from_slice(&chunk[..n]);
                            false
                        }
                    };
                    if over {
                        let _ = tx.send(Msg::Over(kind));
                        return;
                    }
                    if tx.send(Msg::Activity).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = tx.send(Msg::Eof(kind));
                    return;
                }
            }
        }
    });
}

/// The model id becomes an argument, so it must be a plain token that cannot be
/// read as a flag. Empty means "tool default".
fn validated_model(model: &str) -> Result<Option<&str>, CliError> {
    if model.is_empty() {
        return Ok(None);
    }
    let ok = model.len() <= 128
        && !model.starts_with('-')
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-' | '@' | '[' | ']')
        });
    if ok {
        Ok(Some(model))
    } else {
        Err(CliError::InvalidRequest(
            "model id is not a plain token".into(),
        ))
    }
}

/// Render the request as one text prompt. Text-only: tools and tool blocks are
/// rejected. A lone user message with no system prompt is sent verbatim.
fn render_prompt(req: &ChatRequest) -> Result<String, CliError> {
    if !req.tools.is_empty() || req.force_tool.is_some() {
        return Err(CliError::Unsupported(
            "tool use and forced-tool output (text completion only)".into(),
        ));
    }
    let mut turns: Vec<(Role, String)> = Vec::new();
    for m in &req.messages {
        let mut parts: Vec<&str> = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => parts.push(text),
                _ => {
                    return Err(CliError::Unsupported(
                        "tool-use or tool-result content (text completion only)".into(),
                    ))
                }
            }
        }
        turns.push((m.role, parts.join("\n")));
    }
    if turns.iter().all(|(_, t)| t.trim().is_empty()) {
        return Err(CliError::InvalidRequest("no prompt text".into()));
    }
    let system = req.system.as_deref().filter(|s| !s.is_empty());
    if system.is_none() && turns.len() == 1 && turns[0].0 == Role::User {
        return Ok(turns.remove(0).1);
    }
    let mut out = String::new();
    if let Some(s) = system {
        out.push_str("System instructions:\n");
        out.push_str(s);
        out.push_str("\n\n");
    }
    for (i, (role, text)) in turns.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(match role {
            Role::User => "User:\n",
            Role::Assistant => "Assistant:\n",
        });
        out.push_str(text);
    }
    Ok(out)
}

/// Truncate to `max` bytes on a character boundary, strip control characters, and
/// redact credential-shaped tokens before text is put in an error.
pub(crate) fn redact_and_truncate(text: &str, max: usize) -> String {
    let secret_names: BTreeSet<&str> = [
        "key", "token", "secret", "password", "passwd", "auth", "cookie",
    ]
    .into_iter()
    .collect();
    let mut out = String::new();
    let mut redact_next = false;
    for raw in text.split_whitespace() {
        let mut tok = raw.to_string();
        if redact_next {
            tok = "[redacted]".into();
            redact_next = false;
        } else if raw.eq_ignore_ascii_case("bearer") || raw.eq_ignore_ascii_case("basic") {
            redact_next = true;
        } else if let Some((name, value)) = raw.split_once(['=', ':']) {
            let n = name.to_ascii_lowercase();
            if (!value.is_empty() && secret_names.iter().any(|s| n.contains(s)))
                || looks_like_secret_value(value)
            {
                tok = format!("{name}=[redacted]");
            }
        } else if looks_like_secret_value(raw) {
            tok = "[redacted]".into();
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&tok);
    }
    let cleaned: String = out.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() <= max {
        return cleaned;
    }
    let mut end = max;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &cleaned[..end])
}

fn looks_like_secret_value(s: &str) -> bool {
    let s = s.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    s.len() >= 20
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '='))
        && s.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_and_truncates() {
        let t = redact_and_truncate(
            "bad key sk-ant-api03-ABCDEFGHIJKLMNOP123456 and Bearer abc.def api_key=hunter2 ok",
            512,
        );
        assert!(!t.contains("ABCDEFGHIJ"), "{t}");
        assert!(!t.contains("hunter2"), "{t}");
        assert!(!t.contains("abc.def"), "{t}");
        assert!(t.contains("ok"));
        let long = redact_and_truncate(&"é ".repeat(2000), 100);
        assert!(long.ends_with("...[truncated]"));
    }

    #[test]
    fn model_must_be_plain_token() {
        assert!(validated_model("--help").is_err());
        assert!(validated_model("a b").is_err());
        assert_eq!(validated_model("").unwrap(), None);
        assert_eq!(
            validated_model("claude-sonnet-5").unwrap(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn credential_like_names() {
        assert!(looks_like_credential("ANTHROPIC_API_KEY"));
        assert!(looks_like_credential("GITHUB_TOKEN"));
        assert!(!looks_like_credential("CODEX_HOME"));
    }
}
