// `shell` — execute a shell command in the workspace.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::sandbox::SandboxConfig;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{PermissionDecision, PermissionEngine, ShellDialect};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub struct ShellTool;
pub struct PowerShellTool;

#[derive(Clone, Copy)]
pub(crate) enum ShellFlavor {
    Platform,
    PowerShell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellExecutionBackend<'a> {
    Local,
    Unsupported(&'a str),
}

/// The platform shell + its "run this command string" flag. PowerShell on
/// Windows (with `-Command`), `sh -c` on Unix.
pub(crate) fn platform_shell() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("powershell", "-Command")
    } else {
        ("sh", "-c")
    }
}

fn selected_shell(flavor: ShellFlavor) -> (&'static str, &'static str) {
    match flavor {
        ShellFlavor::Platform => platform_shell(),
        ShellFlavor::PowerShell if cfg!(windows) => ("powershell", "-Command"),
        ShellFlavor::PowerShell => ("pwsh", "-Command"),
    }
}

/// Whether this machine can actually run the `powershell` tool.
///
/// Windows always can. Elsewhere the tool shells out to `pwsh`, which is not
/// installed by default on macOS or Linux, so the registry uses this to decide
/// whether to offer the tool at all rather than advertising one that fails on
/// first use.
pub fn powershell_available() -> bool {
    if cfg!(windows) {
        return true;
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join("pwsh");
                std::fs::metadata(&candidate).is_ok_and(|m| m.is_file())
            })
        })
        .unwrap_or(false)
}

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 300_000;

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Shell)
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess)
    }

    fn durable_write_kind(&self, _args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        Some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
    }

    fn description(&self) -> &str {
        "Execute shell command; returns stdout+stderr combined (30s timeout, max 50KB). use run_in_background=true for dev servers/watchers (returns shell_id to poll). Fail: command blocked by destructive guard (rm -rf, format disk) or permission denied."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (max 300000)" },
                "run_in_background": { "type": "boolean", "description": "Run detached and return a shell_id instead of blocking (default false)" },
                "execution_channel": { "type": "string", "enum": ["pipe", "pty"], "default": "pipe", "description": "PTY returns a session-owned process handle for interactive input/output; it does not select ServiceHost." },
                "pty_size": { "type": "object", "additionalProperties": false, "properties": { "rows": { "type": "integer", "minimum": 1, "maximum": 32767 }, "cols": { "type": "integer", "minimum": 1, "maximum": 32767 } }, "required": ["rows", "cols"] }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        execute_shell(args, ctx, ShellFlavor::Platform).await
    }
}

#[async_trait::async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }

    fn description(&self) -> &str {
        "Execute a PowerShell command in the workspace using powershell on Windows or pwsh elsewhere. Uses the same destructive guard, permission checks, timeout, output limit, and background-shell support as shell."
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Shell)
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess)
    }

    fn durable_write_kind(&self, _args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        Some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "PowerShell command to execute" },
                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (max 300000)" },
                "run_in_background": { "type": "boolean", "description": "Run detached and return a shell_id instead of blocking (default false)" },
                "execution_channel": { "type": "string", "enum": ["pipe", "pty"], "default": "pipe", "description": "PTY returns a session-owned process handle for interactive input/output; it does not select ServiceHost." },
                "pty_size": { "type": "object", "additionalProperties": false, "properties": { "rows": { "type": "integer", "minimum": 1, "maximum": 32767 }, "cols": { "type": "integer", "minimum": 1, "maximum": 32767 } }, "required": ["rows", "cols"] }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        execute_shell(args, ctx, ShellFlavor::PowerShell).await
    }
}

pub(crate) async fn execute_shell(
    args: Value,
    ctx: &ToolContext,
    flavor: ShellFlavor,
) -> ToolResult {
    // Fail closed on an empty workspace root: `current_dir("")` would inherit the
    // daemon's process cwd, letting a command run against whatever directory the
    // daemon was launched from (e.g. the the host app repo) instead of the session's
    // workspace.
    if ctx.workspace_root.as_os_str().is_empty() {
        return ToolResult::error(
            "This session has no workspace root; refusing to run a shell command \
             against the daemon's working directory. Open a folder for the session first.",
        );
    }
    let cmd = match args["command"].as_str() {
        Some(c) => c.to_string(),
        None => return ToolResult::error("Missing required argument: command"),
    };

    let channel = match parse_execution_channel(&args) {
        Ok(channel) => channel,
        Err(error) => return error,
    };
    if cmd.is_empty() || cmd.contains('\0') {
        return ToolResult::error("invalid_request: shell command is empty or contains NUL");
    }

    // Shells are universal mutation escape hatches. When web/MCP content has
    // influenced the session, require the same explicit per-call approval as
    // typed durable sinks before launching any interpreter. This is deliberately
    // conservative because parsing every language and redirection form cannot
    // prove a command read-only.
    if !rightkit_agent_policy::durable_write_is_authorized(
        &ctx.content_provenance,
        ctx.durable_write_approved,
    ) {
        return ToolResult::error(format!(
            "Blocked shell command influenced by {} content from '{}': explicit durable-write approval is required.",
            ctx.content_provenance.origin.label(),
            ctx.content_provenance.source
        ));
    }

    // Non-disableable destructive guard: catastrophic, irreversible
    // whole-system commands are blocked regardless of permission mode.
    if let Some(reason) = rightkit_agent_policy::catastrophic_reason(&cmd) {
        return ToolResult::error(format!(
            "Blocked by destructive guard ({reason}): this command is refused in any mode."
        ));
    }
    let backend = selected_execution_backend(ctx);
    match backend {
        ShellExecutionBackend::Local => {
            if let Some(message) = ctx.unenforced_network_profile_error("shell") {
                return ToolResult::error(message);
            }
        }
        ShellExecutionBackend::Unsupported(id) => {
            return ToolResult::error(format!(
                "shell executionBackend '{id}' is not available for command execution; supported backends: local"
            ));
        }
    }

    let timeout_ms = args["timeout_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let engine = PermissionEngine::new();
    let dialect = match flavor {
        ShellFlavor::PowerShell => ShellDialect::PowerShell,
        ShellFlavor::Platform if cfg!(windows) => ShellDialect::PowerShell,
        ShellFlavor::Platform => ShellDialect::Posix,
    };
    let verdict =
        engine.evaluate_shell_command(ctx.permission_mode, ctx.workspace_trust, &cmd, dialect);

    match verdict.decision {
        PermissionDecision::Deny => {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ))
        }
        PermissionDecision::Ask => {
            return ToolResult::error(format!(
                "Tool requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ))
        }
        PermissionDecision::Allow => {}
    }

    // PTY selection follows the SAME shell validation, destructive guard,
    // provenance gate and permission/approval decision as ordinary pipes.
    if matches!(
        channel,
        rightkit_sandbox::execution::ExecutionChannel::Pty(_)
    ) {
        let Some(store) = &ctx.background_shells else {
            return ToolResult::error("managed process inventory is not available");
        };
        let Some(adapter) = ctx.process_launch_adapter.clone() else {
            return ToolResult::error(
                "resource_limits_missing: process authority was not injected",
            );
        };
        let (program, flag) = selected_shell(flavor);
        let tier = effective_sandbox_tier(&ctx.workspace_root, Some(ctx.sandbox_permission_mode()));
        let mut context = ctx.process_launch_context.clone();
        context.workspace_root = ctx.workspace_root.display().to_string();
        context.sandbox_profile = Some("shell".into());
        context.sandbox_required = !tier.is_disabled();
        context.network_required = !tier.denies_network();
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: program.into(),
            argv: vec![flag.into(), cmd.clone()],
            cwd: ctx.workspace_root.display().to_string(),
            context,
            env: Vec::new(),
        };
        return match crate::coding_tools::background_shell::spawn_managed_channel(
            adapter,
            spec,
            channel,
            cmd.clone(),
            store,
            ctx.cancel_rx.clone(),
            execution_target_from_context(ctx),
            crate::coding_tools::background_shell::ProcessPersistence::Inline,
        )
        .await
        {
            Ok(id) => ToolResult::ok(
                json!({
                    "shell_id": id, "execution_channel": "pty",
                    "control": "Use process input/output/resize/eof/stop with this process_id",
                })
                .to_string(),
            )
            .with_process_id(Some(id)),
            Err(error) => ToolResult::error(error.to_string()),
        };
    }

    // Background mode: spawn detached, return a shell_id to poll/kill.
    if args["run_in_background"].as_bool().unwrap_or(false) {
        match &ctx.background_shells {
            Some(store) => {
                let id = match crate::coding_tools::background_shell::spawn_background_with_target(
                    cmd.clone(),
                    &ctx.workspace_root,
                    store,
                    ctx.cancel_rx.clone(),
                    execution_target_from_context(ctx),
                ) {
                    Ok(id) => id,
                    Err(error) => return ToolResult::error(error.to_string()),
                };
                return ToolResult::ok(format!(
                    "Started background shell '{id}'. Poll get_shell_output(shell_id='{id}') \
                         for output; stop it with kill_shell(shell_id='{id}')."
                ));
            }
            None => {
                return ToolResult::error(
                    "Background shells are not available in this context; run without \
                         run_in_background.",
                )
            }
        }
    }

    // Refactor-1 KRN-008/009: reserve process capacity after policy/approval
    // but before either the Windows AppContainer path or the generic OS spawn.
    // Cancel registered before start: a cancel already signalled must not spawn.
    if ctx.cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        return ToolResult::error("Command cancelled by operator before start.");
    }

    // The local variable remains live across the output collector await, so
    // Practical capacity is held for the full foreground child lifetime.
    let _resource_admission = match crate::coding_tools::process_resource_authority::reserve_process(
        crate::coding_tools::process_resource_authority::ResourceKind::ChildProcess,
        rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
            ctx.workspace_root.to_string_lossy().into_owned(),
        ),
        true,
    ) {
        Ok(admission) => admission,
        Err(error) => {
            return ToolResult::error(format!(
                "resource admission denied before shell spawn: {error:?}"
            ))
        }
    };

    // Gated by PermissionEngine; denial verdict aborts before spawn.
    // Platform-aware shell: PowerShell on Windows, sh elsewhere. Hardcoding
    // `sh -c` silently fails on Windows (a desktop target).
    //
    // Sandbox architecture:
    // Build a sandbox config permitting read/write within workspace and read-only
    // access to common system paths needed for shell execution (standard library,
    // package managers, TLS certificates). On Linux it is enforced through
    // `rightkit_sandbox::confine_command` (Landlock + seccomp, applied in the child
    // after fork but before exec); a host without Landlock refuses the launch.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    let sandbox = local_sandbox_config_for_tier(
        &ctx.workspace_root,
        effective_sandbox_tier(&ctx.workspace_root, Some(ctx.sandbox_permission_mode())),
    );

    let (shell_program, flag) = selected_shell(flavor);
    let base_args = vec![flag.to_string(), cmd.clone()];
    // macOS: wrap with `sandbox-exec -p <profile>` (Seatbelt). Other platforms
    // run the shell directly (Linux adds Landlock via pre_exec below).
    #[cfg(target_os = "macos")]
    let (program, exec_args): (String, Vec<String>) = match macos_sandbox_wrap(
        shell_program,
        &base_args,
        &ctx.workspace_root,
        Some(ctx.sandbox_permission_mode()),
    ) {
        Some(command) => command,
        None if sandbox_disabled_for_workspace(&ctx.workspace_root) => {
            (shell_program.to_string(), base_args.clone())
        }
        None => {
            return ToolResult::error("Sandbox setup unavailable; refusing unsandboxed command")
        }
    };
    #[cfg(not(target_os = "macos"))]
    let (program, exec_args): (String, Vec<String>) = (shell_program.to_string(), base_args);

    #[cfg(windows)]
    if !sandbox_disabled_for_workspace(&ctx.workspace_root) {
        match crate::coding_tools::shell::spawn_windows_appcontainer_with_grant(
            &program,
            &exec_args,
            &ctx.workspace_root,
            false,
            !effective_sandbox_tier(&ctx.workspace_root, Some(ctx.sandbox_permission_mode()))
                .denies_network(),
            ctx.sandbox_retry_grant.as_ref(),
        ) {
            Ok(child) => {
                return collect_confined_child_output(child, timeout_ms, ctx, &cmd).await;
            }
            Err(error) => {
                return ToolResult::error(format!(
                    "AppContainer setup unavailable; refusing unsandboxed command: {error}"
                ));
            }
        }
    }

    let mut command = rightkit_child_process::tokio_support::command(&program);
    command.args(&exec_args).current_dir(&ctx.workspace_root);

    // Own the whole tree so a timeout or cancel takes the command's children
    // with it rather than orphaning them.
    // The child leads its own group (killed as a unit by
    // `rightkit_child_process::tokio_support::terminate_tree`); Windows uses the Job
    // Object and `rightkit_child_process::tokio_support::command` has already set the no-window flag.
    #[cfg(unix)]
    // Own session (setsid), not just a group: the child must not keep the
    // daemon's controlling terminal.
    rightkit_child_process::tokio_support::lead_session(&mut command);

    // Apply sandbox restrictions on Linux: RightKit prepares Landlock/seccomp in the
    // parent and registers a syscall-only `pre_exec` hook. A failure refuses the launch.
    #[cfg(target_os = "linux")]
    {
        if !sandbox_disabled_for_workspace(&ctx.workspace_root) {
            if let Err(error) = sandbox.confine(command.as_std_mut()) {
                return ToolResult::error(format!(
                    "sandbox unavailable; refusing unsandboxed command: {error}"
                ));
            }
        }
    }

    let child = match command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to spawn command: {}", e)),
    };

    collect_child_output(child, timeout_ms, ctx, &cmd).await
}

fn parse_execution_channel(
    args: &Value,
) -> Result<rightkit_sandbox::execution::ExecutionChannel, ToolResult> {
    use rightkit_sandbox::execution::{ExecutionChannel, PtySize};
    match args.get("execution_channel") {
        None => Ok(ExecutionChannel::Pipe),
        Some(Value::String(value)) if value == "pipe" => Ok(ExecutionChannel::Pipe),
        Some(Value::String(value)) if value == "pty" => {
            let size = match args.get("pty_size") {
                None => PtySize::default(),
                Some(value) => serde_json::from_value::<PtySize>(value.clone())
                    .map_err(|e| ToolResult::error(format!("invalid_request: pty_size: {e}")))?,
            };
            if size.rows == 0 || size.cols == 0 || size.rows > 32767 || size.cols > 32767 {
                return Err(ToolResult::error(
                    "invalid_request: PTY dimensions must be 1..32767",
                ));
            }
            Ok(ExecutionChannel::Pty(size))
        }
        _ => Err(ToolResult::error(
            "invalid_request: execution_channel must be pipe or pty",
        )),
    }
}
