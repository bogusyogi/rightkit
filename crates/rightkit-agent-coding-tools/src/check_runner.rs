//! Deterministic pre-gate checks (F3).
//!
//! A `CheckSpec` is a shell command (build/test/fmt) run via the same
//! `execute_shell` path as the agent's own shell tool — so the catastrophic
//! guard + permission engine still apply — and judged purely on exit code.
//! Cheap to run, they gate the expensive jury: a red `cargo test` should block
//! "done" before three model panels ever spin up.

use crate::context::ToolContext;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OnFailure {
    FailFast,
    Retry,
    Continue,
}

impl Default for OnFailure {
    fn default() -> Self {
        Self::FailFast
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckSpec {
    pub id: String,
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub backoff_ms: u64,
    #[serde(default)]
    pub on_failure: OnFailure,
}

fn default_timeout() -> u64 {
    120_000
}
fn default_attempts() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub id: String,
    pub passed: bool,
    pub exit_code: i32,
    pub attempts: u32,
    pub duration_ms: u64,
    pub output_tail: String,
}

/// `shell.rs` prepends `[exit N]` on a non-zero exit; its absence means exit 0.
pub fn parse_exit_code(tool_output: &str) -> i32 {
    if let Some(pos) = tool_output.find("[exit ") {
        tool_output[pos + 6..]
            .split(']')
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1)
    } else {
        0
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

pub async fn run_check(spec: &CheckSpec, ctx: &ToolContext) -> CheckOutcome {
    use serde_json::json;
    let start = std::time::Instant::now();
    let mut attempt = 0;
    loop {
        attempt += 1;
        // The catastrophic-command + permission guards live inside execute_shell,
        // so a destructive CheckSpec is refused here exactly as it would be from
        // the agent's shell tool (proven by `destructive_check_command_is_blocked`).
        let res = crate::shell::execute_shell(
            json!({ "command": spec.command, "timeout_ms": spec.timeout_ms }),
            ctx,
            crate::shell::ShellFlavor::Platform,
        )
        .await;
        let code = parse_exit_code(&res.content);
        let passed = !res.is_error && code == 0;
        if passed || spec.on_failure != OnFailure::Retry || attempt >= spec.max_attempts {
            return CheckOutcome {
                id: spec.id.clone(),
                passed,
                exit_code: code,
                attempts: attempt,
                duration_ms: start.elapsed().as_millis() as u64,
                output_tail: tail(&res.content, 500),
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(spec.backoff_ms)).await;
    }
}

/// Verdict of the pre-gate over a batch of check outcomes.
#[derive(Debug, PartialEq, Eq)]
pub enum PreGateDecision {
    AllowJury,
    SkipJuryFailed(Vec<String>),
}

pub fn decide(outcomes: &[CheckOutcome]) -> PreGateDecision {
    let failed: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.passed)
        .map(|o| format!("{} (exit {})", o.id, o.exit_code))
        .collect();
    if failed.is_empty() {
        PreGateDecision::AllowJury
    } else {
        PreGateDecision::SkipJuryFailed(failed)
    }
}
