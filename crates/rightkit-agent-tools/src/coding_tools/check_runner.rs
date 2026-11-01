//! Deterministic pre-gate checks (F3).
//!
//! A `CheckSpec` is a shell command (build/test/fmt) run via the same
//! `execute_shell` path as the agent's own shell tool — so the catastrophic
//! guard + permission engine still apply — and judged purely on exit code.
//! Cheap to run, they gate the expensive jury: a red `cargo test` should block
//! "done" before three model panels ever spin up.

use crate::coding_tools::context::ToolContext;
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
        let res = crate::coding_tools::shell::execute_shell(
            json!({ "command": spec.command, "timeout_ms": spec.timeout_ms }),
            ctx,
            crate::coding_tools::shell::ShellFlavor::Platform,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn oc(id: &str, passed: bool) -> CheckOutcome {
        CheckOutcome {
            id: id.into(),
            passed,
            exit_code: if passed { 0 } else { 1 },
            attempts: 1,
            duration_ms: 1,
            output_tail: String::new(),
        }
    }

    #[test]
    fn parse_exit_code_reads_marker() {
        assert_eq!(parse_exit_code("boom\n[exit 2]"), 2);
        assert_eq!(parse_exit_code("ok"), 0);
        assert_eq!(parse_exit_code("[exit 137]\nkilled"), 137);
    }

    #[test]
    fn checkspec_defaults_apply() {
        let s: CheckSpec =
            serde_json::from_str(r#"{"id":"fmt","command":"cargo fmt --check"}"#).unwrap();
        assert_eq!(s.max_attempts, 1);
        assert_eq!(s.on_failure, OnFailure::FailFast);
        assert_eq!(s.timeout_ms, 120_000);
        assert_eq!(s.backoff_ms, 0);
    }

    #[test]
    fn decide_all_pass_allows_jury() {
        assert_eq!(
            decide(&[oc("a", true), oc("b", true)]),
            PreGateDecision::AllowJury
        );
    }

    #[test]
    fn decide_any_fail_skips_jury() {
        match decide(&[oc("a", true), oc("b", false)]) {
            PreGateDecision::SkipJuryFailed(f) => assert_eq!(f, vec!["b (exit 1)".to_string()]),
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn destructive_check_command_is_blocked_by_policy() {
        let root = std::env::temp_dir();
        let ctx = ToolContext::new("s1", &root, PermissionMode::Bypass, WorkspaceTrust::Trusted);
        let spec = CheckSpec {
            id: "x".into(),
            command: "rm -rf /".into(),
            timeout_ms: 5000,
            max_attempts: 1,
            backoff_ms: 0,
            on_failure: OnFailure::FailFast,
        };
        let out = run_check(&spec, &ctx).await;
        // If this fails, the guard was bypassed by the pub(crate) exposure — STOP.
        assert!(
            !out.passed,
            "destructive command must not pass the pre-gate"
        );
        assert!(
            out.output_tail.to_lowercase().contains("blocked") || out.exit_code != 0,
            "expected a blocked/non-zero outcome, got: {out:?}"
        );
    }
}
