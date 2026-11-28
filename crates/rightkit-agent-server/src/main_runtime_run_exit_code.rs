/// The headless exit classes (headless-cli.md section 2.5, R9). The process
/// exit is one of the suite codes 0 to 3; the cause is the stable
/// `error.code` string, which is fixed per contract version.
///
/// | Exit | Meaning                                  | `error.code`                                                                                       |
/// |------|------------------------------------------|----------------------------------------------------------------------------------------------------|
/// | 0    | Success, dry run, or accepted            | none                                                                                               |
/// | 1    | The call ran and the state was not reached | `provider_auth`, `tool_denied`, `interaction_required`, `budget_exceeded`, `sandbox`, `timeout`, `schema_violation`, `interrupted`, `generic` |
/// | 2    | Refused or bad usage; nothing ran        | `usage`, `unknown_command`, `untrusted_workspace`, `licence_required`, `interactive_only`, `refused_by_policy` |
/// | 3    | No result is known                       | `unreachable` (nothing ran, safe to retry), `unknown` (the call may have run)                      |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunExitCode {
    Success,
    Generic,
    Usage,
    ProviderAuthFailure,
    ToolDenied,
    /// An unattended run needed an answer or a plan decision.
    InteractionRequired,
    BudgetExceeded,
    Sandbox,
    TimeoutStall,
    /// Interrupted and settled.
    UserAbort,
    SchemaViolation,
    /// The workspace is not trusted for what the call asked.
    UntrustedWorkspace,
    /// The policy gate refused the call before anything ran.
    RefusedByPolicy,
    /// The call needs a person at the app: an agent caller cannot make it.
    InteractiveOnly,
    /// Nothing ran and nothing was written; safe to retry.
    Unreachable,
    /// The call may have run; check state before retrying.
    Unknown,
}

impl RunExitCode {
    /// Process exit code: one of the suite codes 0 to 3.
    pub fn as_i32(self) -> i32 {
        match self {
            RunExitCode::Success => 0,
            RunExitCode::Generic
            | RunExitCode::ProviderAuthFailure
            | RunExitCode::ToolDenied
            | RunExitCode::InteractionRequired
            | RunExitCode::BudgetExceeded
            | RunExitCode::Sandbox
            | RunExitCode::TimeoutStall
            | RunExitCode::UserAbort
            | RunExitCode::SchemaViolation => 1,
            RunExitCode::Usage
            | RunExitCode::UntrustedWorkspace
            | RunExitCode::RefusedByPolicy
            | RunExitCode::InteractiveOnly => 2,
            RunExitCode::Unreachable | RunExitCode::Unknown => 3,
        }
    }

    /// The stable `error.code` for this class (fixed per contract version).
    pub fn class(self) -> &'static str {
        match self {
            RunExitCode::Success => "success",
            RunExitCode::Generic => "generic",
            RunExitCode::Usage => "usage",
            RunExitCode::ProviderAuthFailure => "provider_auth",
            RunExitCode::ToolDenied => "tool_denied",
            RunExitCode::InteractionRequired => "interaction_required",
            RunExitCode::BudgetExceeded => "budget_exceeded",
            RunExitCode::Sandbox => "sandbox",
            RunExitCode::TimeoutStall => "timeout",
            RunExitCode::UserAbort => "interrupted",
            RunExitCode::SchemaViolation => "schema_violation",
            RunExitCode::UntrustedWorkspace => "untrusted_workspace",
            RunExitCode::RefusedByPolicy => "refused_by_policy",
            RunExitCode::InteractiveOnly => "interactive_only",
            RunExitCode::Unreachable => "unreachable",
            RunExitCode::Unknown => "unknown",
        }
    }

    /// Whether a retry can help, fixed per contract version: `true` only for
    /// `interrupted`, `timeout` and `unreachable`.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            RunExitCode::UserAbort | RunExitCode::TimeoutStall | RunExitCode::Unreachable
        )
    }
}

/// Convert the legacy `CliExitCode` (CLI parsing / protocol-event classifier)
/// into the catalogued `RunExitCode`. The first six variants preserve the
/// legacy numeric exit codes that existed before this feature landed.
pub(crate) fn run_exit_code_from_cli(code: CliExitCode) -> RunExitCode {
    match code {
        CliExitCode::Generic => RunExitCode::Generic,
        CliExitCode::Usage => RunExitCode::Usage,
        CliExitCode::Auth => RunExitCode::ProviderAuthFailure,
        CliExitCode::Permission => RunExitCode::ToolDenied,
        CliExitCode::Budget => RunExitCode::BudgetExceeded,
        CliExitCode::Sandbox => RunExitCode::Sandbox,
        CliExitCode::TimeoutStall => RunExitCode::TimeoutStall,
    }
}

pub(crate) fn run_exit_code_for_event(
    event: &rightkit_agent_engine_protocol::ProtocolEvent,
) -> Option<RunExitCode> {
    match event {
        rightkit_agent_engine_protocol::ProtocolEvent::RunInterrupted { .. } => {
            Some(RunExitCode::UserAbort)
        }
        rightkit_agent_engine_protocol::ProtocolEvent::BudgetExceeded { .. } => {
            Some(RunExitCode::BudgetExceeded)
        }
        rightkit_agent_engine_protocol::ProtocolEvent::ToolApprovalResolved {
            approved: false,
            ..
        } => Some(RunExitCode::ToolDenied),
        rightkit_agent_engine_protocol::ProtocolEvent::RunFailed { kind, error, .. } => {
            Some(run_exit_code_from_failure_kind(kind, error))
        }
        _ => None,
    }
}

fn run_exit_code_from_failure_kind(kind: &str, error: &str) -> RunExitCode {
    match kind {
        "auth" => RunExitCode::ProviderAuthFailure,
        "permission" => RunExitCode::ToolDenied,
        "budget" => RunExitCode::BudgetExceeded,
        // Network and rate-limit failures are transient: exit 1 `timeout`, retryable.
        "max_iterations" | "stall" | "timeout" | "network" | "rate_limited" => {
            RunExitCode::TimeoutStall
        }
        // The engine's output-limit failure: it has its own runFailed.kind but no
        // error.code of its own (headless-cli.md fixes that set per contract version).
        "incomplete_output" => RunExitCode::Generic,
        // Unmapped kinds (provider_error, other) funnel through the legacy
        // error-text classifier so api-key error text keeps mapping to Auth.
        _ => run_exit_code_from_cli(cli_exit_code_from_error(error)),
    }
}

/// Pure decision function consumed by `run_session` after the engine loop
/// returns. Factored out so the `--exit-on-error` short-circuit path is
/// testable without driving a live LLM.
///
/// Semantics:
/// - `exit_on_error=true`: return the FIRST typed-error code seen
///   (`first_typed`), else fall back to `terminal_event`, else `Success`.
/// - `exit_on_error=false`: keep legacy behavior — on Ok return Success
///   (current code is 0); on Err return the inferred terminal code or
///   `fallback_for_err` (currently `RunExitCode::Generic`).
pub(crate) fn final_run_exit_code(
    result_is_ok: bool,
    exit_on_error: bool,
    first_typed: Option<RunExitCode>,
    terminal_event: Option<RunExitCode>,
    fallback_for_err: RunExitCode,
) -> RunExitCode {
    if exit_on_error {
        return first_typed
            .or(terminal_event)
            .unwrap_or(RunExitCode::Success);
    }
    if result_is_ok {
        return terminal_event.unwrap_or(RunExitCode::Success);
    }
    terminal_event.unwrap_or(fallback_for_err)
}
