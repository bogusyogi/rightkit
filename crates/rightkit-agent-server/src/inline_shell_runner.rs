//! Permission-gated runner for inline `!{cmd}` expansion in custom commands
//! (A208; https://code.claude.com/docs/en/slash-commands, "Bash command execution").
//!
//! Every command is evaluated by `PermissionEngine::evaluate_shell_command`
//! (the same policy the shell tool uses). Only an `Allow` verdict runs; `Ask`
//! (approval needed, no approval path exists while a prompt is being expanded)
//! and `Deny` leave the command unrun and visible in the expanded text. This
//! runner never approves anything itself.

use coderight_skills::inline_shell::{ShellOutcome, ShellRunner};
use rightkit_agent_policy::{
    PermissionDecision, PermissionEngine, PermissionMode, ShellDialect, WorkspaceTrust,
};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub(crate) struct PolicyShellRunner {
    pub mode: PermissionMode,
    pub trust: WorkspaceTrust,
    pub workspace: std::path::PathBuf,
}

/// Policy decision only (no execution); `Some(outcome)` means do not run.
pub(crate) fn gate(
    mode: PermissionMode,
    trust: WorkspaceTrust,
    command: &str,
) -> Option<ShellOutcome> {
    let dialect = if cfg!(windows) {
        ShellDialect::PowerShell
    } else {
        ShellDialect::Posix
    };
    let verdict = PermissionEngine::new().evaluate_shell_command(mode, trust, command, dialect);
    match verdict.decision {
        PermissionDecision::Allow => None,
        PermissionDecision::Ask => Some(ShellOutcome::Denied(format!(
            "approval required ({})",
            verdict.reason
        ))),
        PermissionDecision::Deny => Some(ShellOutcome::Denied(verdict.reason.to_string())),
    }
}

impl ShellRunner for PolicyShellRunner {
    fn run(&mut self, command: &str) -> ShellOutcome {
        if let Some(blocked) = gate(self.mode, self.trust, command) {
            return blocked;
        }
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            (
                "powershell",
                vec!["-NoProfile", "-NonInteractive", "-Command", command],
            )
        } else {
            ("sh", vec!["-c", command])
        };
        // Shared RightKit runner with no deadline (a user command may run as long as it needs):
        // owned process tree (terminated if the call is cancelled or this thread unwinds), hidden
        // console on Windows, null stdin. This call chain has no cancel signal, so the predicate is
        // never true. Only the first MAX_OUTPUT_BYTES of stdout are kept; the rest is drained.
        let mut owned = rightkit_child_process::OwnedCommand::new(program);
        owned.command_mut().args(&args).current_dir(&self.workspace);
        let options = rightkit_child_process::StreamOptions::new()
            .stdout_tail_cap(0)
            .stderr_tail_cap(0);
        let mut stdout = Vec::new();
        let output = rightkit_child_process::run_streaming(
            owned,
            &options,
            &|| false,
            &mut |chunk: &[u8]| {
                let room = MAX_OUTPUT_BYTES.saturating_sub(stdout.len());
                stdout.extend_from_slice(&chunk[..room.min(chunk.len())]);
            },
            &mut |_line: &str| {},
        );
        match output {
            Ok(out) if out.status.success() => {
                ShellOutcome::Output(String::from_utf8_lossy(&stdout).into_owned())
            }
            Ok(out) => ShellOutcome::Failed(format!("exit {:?}", out.status.code())),
            Err(error) => ShellOutcome::Failed(error.to_string()),
        }
    }
}

/// Expand a custom command body under the live permission mode and trust.
pub(crate) fn expand_custom_command(
    body: &str,
    mode: PermissionMode,
    trust: WorkspaceTrust,
    workspace: &std::path::Path,
) -> String {
    let mut runner = PolicyShellRunner {
        mode,
        trust,
        workspace: workspace.to_path_buf(),
    };
    coderight_skills::inline_shell::expand(body, &mut runner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_mode_never_runs_a_shell_command() {
        // Ask mode requires approval for shell; the expander must not run it.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran.txt");
        let body = format!("x !{{echo hi > \"{}\"}} y", marker.display());
        let out = expand_custom_command(
            &body,
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            dir.path(),
        );
        assert!(out.contains("command not run"), "{out}");
        assert!(!marker.exists());
    }
}
