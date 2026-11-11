//! The caller side of the engine CLI ([`crate::cli`]): one engine calling another installed engine.
//!
//! [`EngineProcess`] runs `<engine> <words...> [--input <json>|-]` at an explicit absolute path
//! (no `PATH` lookup). A payload up to [`MAX_INPUT_ARG_BYTES`] goes on the command line; a larger
//! one goes through `--input -` and stdin (Windows command lines top out near 32k characters and
//! JSON quoting can double the size). Every other call gets a null stdin, so the engine can never
//! block on an inherited pipe. Both output streams are captured up to [`MAX_CAPTURE_BYTES`] and the
//! call has a deadline; the engine's process tree is ended when it passes.
//!
//! [`parse_reply`] reads the result: one JSON value on stdout, or the `{"error":{code,message,
//! details?}}` envelope as [`CallError::Engine`]. [`handshake`] checks the callee's `doctor`
//! report (contract version and required capabilities) before any other call. [`json_lines`]
//! yields the NDJSON progress objects an engine writes to stderr.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rightkit_child_process::{NativeOutputCapture, OwnedCommand};
use serde_json::Value;

use crate::wire::EngineError;

/// Largest JSON payload passed on the command line; bigger ones go through `--input -` and stdin.
pub const MAX_INPUT_ARG_BYTES: usize = 8 * 1024;
/// Ceiling on one captured stream. Output past it is refused as incomplete.
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
/// Deadline of one call unless the caller sets another.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a call to another engine failed.
#[derive(Debug)]
pub enum CallError {
    /// The engine could not be started, or its pipes failed.
    Spawn {
        /// The engine path.
        path: PathBuf,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The call passed its deadline; the engine's process tree was ended.
    Timeout {
        /// The command words.
        command: String,
        /// The deadline, in seconds.
        seconds: u64,
    },
    /// The engine exited without a JSON result on stdout.
    EngineExit {
        /// The command words.
        command: String,
        /// The exit code, when there is one.
        code: Option<i32>,
        /// The last 400 bytes of stderr.
        stderr: String,
    },
    /// The output was not the JSON the contract promises, or was incomplete.
    BadOutput {
        /// The command words.
        command: String,
        /// What was wrong.
        detail: String,
    },
    /// The engine ran the command and reported a failure.
    Engine(EngineError),
    /// The engine speaks another contract version.
    ContractVersion {
        /// The engine's version.
        found: u64,
        /// The version the caller speaks.
        required: u64,
    },
    /// The engine lacks a capability, or has an older version of it.
    MissingCapability {
        /// Capability name.
        name: String,
        /// Version the caller needs.
        required: u32,
        /// Version the engine reports, if any.
        found: Option<u32>,
    },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { path, source } => {
                write!(f, "engine could not be started at {}: {source}", path.display())
            }
            Self::Timeout { command, seconds } => {
                write!(f, "engine `{command}` timed out after {seconds}s")
            }
            Self::EngineExit {
                command,
                code,
                stderr,
            } => write!(
                f,
                "engine `{command}` exited with {code:?} and no JSON result: {stderr}"
            ),
            Self::BadOutput { command, detail } => {
                write!(f, "engine `{command}` returned unreadable output: {detail}")
            }
            Self::Engine(e) => write!(f, "engine error {}: {}", e.code, e.message),
            Self::ContractVersion { found, required } => write!(
                f,
                "engine contract version {found} is not supported (need {required}); update the engine"
            ),
            Self::MissingCapability {
                name,
                required,
                found,
            } => write!(
                f,
                "engine lacks capability {name} v{required} (found {found:?}); update the engine"
            ),
        }
    }
}

impl std::error::Error for CallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::Engine(e) => Some(e),
            _ => None,
        }
    }
}

/// Raw result of one engine invocation.
#[derive(Debug, Clone, Default)]
pub struct RawOutput {
    /// Everything the engine wrote to stdout.
    pub stdout: String,
    /// Everything the engine wrote to stderr.
    pub stderr: String,
    /// The exit code, when there is one.
    pub code: Option<i32>,
}

/// Runs one engine command. [`EngineProcess`] is the real one; a caller may supply another (a
/// recorded transcript, an in-process engine).
pub trait Runner: Send + Sync {
    /// Run `words`, with `input` as the `--input` JSON when given.
    fn run(&self, words: &[&str], input: Option<&str>) -> Result<RawOutput, CallError>;
}

/// An installed engine binary at an explicit absolute path.
#[derive(Debug, Clone)]
pub struct EngineProcess {
    engine: PathBuf,
    timeout: Duration,
}

impl EngineProcess {
    /// The engine at `engine`, with [`DEFAULT_TIMEOUT`]. A relative path is refused when a call
    /// runs: the caller resolves the installed engine, never `PATH` or the working folder.
    pub fn new(engine: impl Into<PathBuf>) -> Self {
        Self {
            engine: engine.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Replace the deadline of each call.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The engine path.
    pub fn engine(&self) -> &Path {
        &self.engine
    }

    fn spawn_error(&self, source: std::io::Error) -> CallError {
        CallError::Spawn {
            path: self.engine.clone(),
            source,
        }
    }
}

impl Runner for EngineProcess {
    fn run(&self, words: &[&str], input: Option<&str>) -> Result<RawOutput, CallError> {
        if !self.engine.is_absolute() {
            return Err(self.spawn_error(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the engine path must be absolute",
            )));
        }
        let label = words.join(" ");
        let via_stdin = input.is_some_and(|i| i.len() > MAX_INPUT_ARG_BYTES);
        let mut cmd = OwnedCommand::new(&self.engine);
        cmd.command_mut().args(words);
        match input {
            Some(_) if via_stdin => {
                cmd.command_mut().args(["--input", "-"]);
            }
            Some(i) => {
                cmd.command_mut().args(["--input", i]);
            }
            None => {}
        }
        cmd.command_mut()
            .stdin(if via_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| self.spawn_error(e))?;
        // Drain both streams before anything blocks on the child.
        let (Some(out), Some(err)) = (child.take_stdout(), child.take_stderr()) else {
            let _ = child.terminate_tree();
            return Err(self.spawn_error(std::io::Error::other("engine output pipes missing")));
        };
        let mut stdout =
            NativeOutputCapture::stdout(out, MAX_CAPTURE_BYTES).map_err(|e| self.spawn_error(e))?;
        let mut stderr =
            NativeOutputCapture::stderr(err, MAX_CAPTURE_BYTES).map_err(|e| self.spawn_error(e))?;
        // Payload writer: a separate thread so the deadline below still applies if the child
        // never reads its stdin. Dropping the handle closes stdin (EOF).
        let writer = child.take_stdin().map(|mut s| {
            let payload = if via_stdin {
                input.unwrap_or("").to_string()
            } else {
                String::new()
            };
            std::thread::spawn(move || {
                let _ = s.write_all(payload.as_bytes());
            })
        });
        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.terminate_tree();
                return Err(CallError::Timeout {
                    command: label,
                    seconds: self.timeout.as_secs(),
                });
            }
            Err(source) => return Err(self.spawn_error(source)),
        };
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let stdout_bytes = stdout.finish_within(Duration::from_secs(2));
        let stderr_bytes = stderr.finish_within(Duration::from_secs(2));
        for capture in [&stdout, &stderr] {
            if let Some(source) = capture.take_error() {
                return Err(self.spawn_error(source));
            }
        }
        if !stdout.is_complete() || !stderr.is_complete() {
            return Err(CallError::BadOutput {
                command: label,
                detail: "output pipes did not reach EOF after the engine exited".into(),
            });
        }
        if stdout_bytes.truncated || stderr_bytes.truncated {
            return Err(CallError::BadOutput {
                command: label,
                detail: format!("output exceeded the {MAX_CAPTURE_BYTES}-byte capture limit"),
            });
        }
        Ok(RawOutput {
            stdout: String::from_utf8_lossy(&stdout_bytes.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes.bytes).into_owned(),
            code: status.code(),
        })
    }
}

/// The JSON objects among the engine's stderr lines (its NDJSON progress), in order.
pub fn json_lines(stderr: &str) -> impl Iterator<Item = Value> + '_ {
    stderr
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
}

/// The result of `command` from its raw output: the JSON value on stdout, or the error envelope
/// (an object whose only key is an `error` object) as [`CallError::Engine`]. A result that merely
/// carries an `error` field among others (a failed job's record) is a result.
pub fn parse_reply(command: &str, raw: &RawOutput) -> Result<Value, CallError> {
    let body = raw.stdout.trim();
    if body.is_empty() {
        return Err(CallError::EngineExit {
            command: command.to_string(),
            code: raw.code,
            stderr: tail(&raw.stderr),
        });
    }
    let value: Value = serde_json::from_str(body).map_err(|e| CallError::BadOutput {
        command: command.to_string(),
        detail: e.to_string(),
    })?;
    let envelope = value
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("error"))
        .filter(|e| e.is_object());
    if let Some(err) = envelope {
        let mut error = EngineError::new(
            err["code"].as_str().unwrap_or("unknown"),
            err["message"].as_str().unwrap_or(""),
        );
        if let Some(details) = err.get("details").filter(|d| !d.is_null()) {
            error = error.with_details(details.clone());
        }
        return Err(CallError::Engine(error));
    }
    Ok(value)
}

/// What a callee engine's `doctor` report says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineInfo {
    /// The engine's version.
    pub app_version: String,
    /// Its contract version.
    pub contract_version: u64,
    /// Its capabilities, `(name, version)`.
    pub capabilities: Vec<(String, u32)>,
    /// Its command names, when the report lists them.
    pub commands: Vec<String>,
}

impl EngineInfo {
    /// The version of capability `name`, if the engine has it.
    pub fn capability(&self, name: &str) -> Option<u32> {
        self.capabilities
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }
}

/// Check a callee's `doctor` report: exactly `contract_version` and at least the version of
/// each `required` capability.
pub fn handshake(
    report: &Value,
    contract_version: u64,
    required: &[(&str, u32)],
) -> Result<EngineInfo, CallError> {
    let v = rightkit_job_queue::parse_doctor(&report.to_string()).map_err(|e| {
        CallError::BadOutput {
            command: "doctor".to_string(),
            detail: e.to_string(),
        }
    })?;
    let contract = v["contract_version"].as_u64().unwrap_or(0);
    if contract != contract_version {
        return Err(CallError::ContractVersion {
            found: contract,
            required: contract_version,
        });
    }
    let capabilities: Vec<(String, u32)> = v["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some((
                        c["name"].as_str()?.to_string(),
                        u32::try_from(c["version"].as_u64()?).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let info = EngineInfo {
        app_version: v["app_version"].as_str().unwrap_or("").to_string(),
        contract_version: contract,
        commands: report["commands"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        capabilities,
    };
    for &(name, need) in required {
        let found = info.capability(name);
        if found.is_none_or(|f| f < need) {
            return Err(CallError::MissingCapability {
                name: name.to_string(),
                required: need,
                found,
            });
        }
    }
    Ok(info)
}

/// The last 400 bytes of `s`, on a character boundary.
fn tail(s: &str) -> String {
    let s = s.trim();
    let start = s.len().saturating_sub(400);
    let start = (start..=s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s[start..].to_string()
}
