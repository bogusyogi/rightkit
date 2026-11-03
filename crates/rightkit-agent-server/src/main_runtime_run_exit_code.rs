/// Stable process exit codes for `coderight run`/`exec`. Each variant maps to
/// a stable `u8` value so downstream CI scripts and integrations can branch on
/// the numeric exit code without parsing the textual class label.
///
/// | Code | Variant                | When it is emitted                                                                 |
/// |------|------------------------|------------------------------------------------------------------------------------|
/// | 0    | `Success`              | Run completed without a typed terminal error.                                      |
/// | 1    | `Generic`              | Unclassified failure or unmapped engine error.                                     |
/// | 2    | `Usage`                | CLI usage error (unrecognized args, missing prompt, invalid MCP config, etc.).    |
/// | 3    | `ProviderAuthFailure`  | Provider/API key rejected (`kind="auth"`, missing key, 401 Unauthorized).           |
/// | 4    | `ToolDenied`           | Tool approval denied or policy `Deny`.                                             |
/// | 5    | `BudgetExceeded`       | Budget ceiling hit (`kind="budget"` or `BudgetExceeded` event).                    |
/// | 6    | `Sandbox`              | Sandbox / landlock / seccomp rejection. Preserves the legacy CliExitCode number.   |
/// | 7    | `TimeoutStall`         | Provider timed out, wall-clock stall, or `max_iterations` ceiling.                 |
/// | 8    | `UserAbort`            | Run was interrupted on user request (Ctrl-C, stdin shutdown, daemon kill).          |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunExitCode {
    Success = 0,
    Generic = 1,
    Usage = 2,
    ProviderAuthFailure = 3,
    ToolDenied = 4,
    BudgetExceeded = 5,
    Sandbox = 6,
    TimeoutStall = 7,
    UserAbort = 8,
    SchemaViolation = 9,
}

impl RunExitCode {
    /// Numeric exit code (stable across releases).
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Stable class label, used in CLI error envelopes and `--stream-json`
    /// events so downstream classifiers can map without code numbers.
    pub fn class(self) -> &'static str {
        match self {
            RunExitCode::Success => "success",
            RunExitCode::Generic => "generic",
            RunExitCode::Usage => "usage",
            RunExitCode::ProviderAuthFailure => "provider_auth_failure",
            RunExitCode::ToolDenied => "tool_denied",
            RunExitCode::BudgetExceeded => "budget_exceeded",
            RunExitCode::Sandbox => "sandbox",
            RunExitCode::TimeoutStall => "timeout_stall",
            RunExitCode::UserAbort => "user_abort",
            RunExitCode::SchemaViolation => "schema_violation",
        }
    }
}

/// Convert the legacy `CliExitCode` (CLI parsing / protocol-event classifier)
/// into the catalogued `RunExitCode`. The first six variants preserve the
/// legacy numeric exit codes that existed before this feature landed.
pub(crate) fn run_exit_code_from_cli(code: CliExitCode) -> RunExitCode {
    match code {
        CliExitCode::Success => RunExitCode::Success,
        CliExitCode::Generic => RunExitCode::Generic,
        CliExitCode::Usage => RunExitCode::Usage,
        CliExitCode::Auth => RunExitCode::ProviderAuthFailure,
        CliExitCode::Permission => RunExitCode::ToolDenied,
        CliExitCode::Budget => RunExitCode::BudgetExceeded,
        CliExitCode::Sandbox => RunExitCode::Sandbox,
        CliExitCode::TimeoutStall => RunExitCode::TimeoutStall,
        CliExitCode::UserAbort => RunExitCode::UserAbort,
        CliExitCode::SchemaViolation => RunExitCode::SchemaViolation,
    }
}

/// First matching event-to-RunExitCode mapping. Walks terminal protocol events
/// in emission order so the FIRST typed class wins (used for `--exit-on-error`
/// short-circuit). Captures `TimeoutStall` and `UserAbort` which the legacy
/// `CliExitCode` enum does not surface.
pub(crate) fn run_exit_code_for_events(
    events: &[rightkit_agent_engine_protocol::ProtocolEvent],
) -> Option<RunExitCode> {
    for ev in events {
        if let Some(code) = run_exit_code_for_event(ev) {
            return Some(code);
        }
    }
    None
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
        "max_iterations" | "stall" | "timeout" => RunExitCode::TimeoutStall,
        // Unmapped kinds (network, rate_limited, provider_error, other) funnel
        // through the legacy error-text classifier so a `RunFailed { kind:
        // "network", error: "401 Unauthorized" }` keeps mapping to Auth (3) and
        // rate-limited messages keep mapping to Auth. This is the SPEC-vs-
        // legacy bridge: any future refactor here MUST keep this fallback or
        // rate-limited / api-key error text regresses to Generic.
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

#[cfg(test)]
mod run_exit_code_unit_tests {
    use super::*;
    use rightkit_agent_engine_protocol::ProtocolEvent;
    include!("main_runtime_run_exit_code_tests.rs");
}
