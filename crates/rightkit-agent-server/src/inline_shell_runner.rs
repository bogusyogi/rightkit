//! Permission-gated runner for inline `!{cmd}` expansion in custom commands
//! (A208; <https://code.claude.com/docs/en/slash-commands>, "Bash command execution").
//!
//! Every command is evaluated by `PermissionEngine::evaluate_shell_command`
//! (the same policy the shell tool uses). Only an `Allow` verdict runs; `Ask`
//! (approval needed, no approval path exists while a prompt is being expanded)
//! and `Deny` leave the command unrun and visible in the expanded text. This
//! runner never approves anything itself.

use rightkit_agent_policy::{
    PermissionDecision, PermissionEngine, PermissionMode, ShellDialect, WorkspaceTrust,
};
use rightkit_agent_skills::inline_shell::{ShellOutcome, ShellRunner};

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
        execute_shell(&self.workspace, command)
    }
}

/// The policy's verdict on one inline command and its reason, without running anything.
pub(crate) fn verdict(
    mode: PermissionMode,
    trust: WorkspaceTrust,
    command: &str,
) -> (PermissionDecision, String) {
    let dialect = if cfg!(windows) {
        ShellDialect::PowerShell
    } else {
        ShellDialect::Posix
    };
    let verdict = PermissionEngine::new().evaluate_shell_command(mode, trust, command, dialect);
    (verdict.decision, verdict.reason.to_string())
}

/// Run one command that the policy (or the user, on a card) has already allowed.
pub(crate) fn execute_shell(workspace: &std::path::Path, command: &str) -> ShellOutcome {
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
    // never true. The child also dies with this process (`kill_on_parent_exit`: group-wide
    // watchdog on Linux and macOS; the Job Object covers Windows). Only the first MAX_OUTPUT_BYTES of stdout are kept; the rest is drained.
    let mut owned = rightkit_child_process::OwnedCommand::new(program);
    owned.command_mut().args(&args).current_dir(workspace);
    owned.kill_on_parent_exit(true);
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

/// Runner for a typed prompt in a run nobody attends: an `Allow` verdict runs, anything that
/// would need approval is refused with the reason `unattended` (the same word the approval path
/// uses), and a `Deny` keeps the policy's own reason.
struct UnattendedShellRunner {
    mode: PermissionMode,
    trust: WorkspaceTrust,
    workspace: std::path::PathBuf,
}

impl ShellRunner for UnattendedShellRunner {
    fn run(&mut self, command: &str) -> ShellOutcome {
        match verdict(self.mode, self.trust, command) {
            (PermissionDecision::Allow, _) => execute_shell(&self.workspace, command),
            (PermissionDecision::Ask, _) => ShellOutcome::Denied("unattended".to_string()),
            (PermissionDecision::Deny, reason) => ShellOutcome::Denied(reason),
        }
    }
}

/// Expand `!{cmd}` in a typed prompt of an unattended run (`-p`).
pub(crate) fn expand_typed_prompt_unattended(
    prompt: &str,
    mode: PermissionMode,
    trust: WorkspaceTrust,
    workspace: &std::path::Path,
) -> String {
    let mut runner = UnattendedShellRunner {
        mode,
        trust,
        workspace: workspace.to_path_buf(),
    };
    rightkit_agent_skills::inline_shell::expand(prompt, &mut runner)
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
    rightkit_agent_skills::inline_shell::expand(body, &mut runner)
}
