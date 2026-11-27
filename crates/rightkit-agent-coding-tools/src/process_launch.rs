//! Explicit local process adapter used when host has not supplied backend.
//!
//! CG-B02: every daemon-launched process must resolve and enforce the
//! selected sandbox tier through the *same seam* the interactive shell tool
//! path uses (`crate::shell::{sandbox_tier_for_workspace,
//! local_sandbox_config_for_tier, macos_sandbox_wrap_for_tier,
//! spawn_windows_appcontainer}` / `SandboxConfig::confine`), rather
//! than duplicating a second policy. Enforcement is fail-closed: a launch
//! that asked for sandboxing (`ProcessLaunchContext::sandbox_required`)
//! never silently falls back to an unconfined process when the platform
//! backend fails to set up. The one legitimate exception is a caller that
//! made an explicit, typed decision to skip sandboxing
//! (`sandbox_required: false`, e.g. hooks CLI probes) — that decision is
//! recorded in `SandboxProcessProof`/logged diagnostics, never silently
//! defaulted.

use async_trait::async_trait;
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ExecutionChannel, ManagedChild, ProcessLaunchAdapter,
    ProcessLaunchContext, PtySize, SandboxProcessProof,
};
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{info, warn};

use crate::sandbox::SandboxTier;

pub struct LocalProcessLaunchAdapter;

/// Deliberately unconfined launch adapter for tests and diagnostics.
///
/// Production producers use [`LocalProcessLaunchAdapter`] or
/// `shell::NativeCommandSandboxAdapter`, both of which enforce the selected
/// sandbox tier (CG-B02). Some unit tests need to exercise the logic *around*
/// a launch — Git plumbing, plugin dispatch, corpus loading — rather than the
/// platform sandbox itself, and a confined child cannot reach the fixture
/// repository or plugin script those tests build. This adapter is the single
/// named seam for that: it never confines, and it says so in its process
/// proof (`enforced: false`, `backend: "none"`), so an accidental production
/// use is visible in diagnostics instead of silently unsandboxed.
pub struct UnconfinedProcessLaunchAdapter;

#[async_trait]
impl ProcessLaunchAdapter for UnconfinedProcessLaunchAdapter {
    async fn spawn(
        &self,
        spec: CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        let decision = unconfined_decision(&spec, "unconfined-test-double");
        spawn_with_decision(spec, decision).await
    }

    async fn spawn_channel(
        &self,
        spec: CommandLaunchSpec,
        channel: ExecutionChannel,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        match channel {
            ExecutionChannel::Pipe => self.spawn(spec).await,
            ExecutionChannel::Pty(size) => {
                let decision = unconfined_decision(&spec, "unconfined-test-double");
                spawn_pty_with_decision(spec, decision, size).await
            }
        }
    }
}

fn unconfined_decision(spec: &CommandLaunchSpec, fallback_profile: &str) -> SandboxDecision {
    SandboxDecision {
        tier: SandboxTier::Disabled,
        enforce: false,
        profile: spec
            .context
            .sandbox_profile
            .clone()
            .unwrap_or_else(|| fallback_profile.to_string()),
    }
}

/// The sandbox decision resolved for one launch, computed once so every
/// platform branch and the returned diagnostics agree with each other.
struct SandboxDecision {
    tier: SandboxTier,
    /// Whether the caller's typed contract asked this launch to be
    /// enforced (`ProcessLaunchContext::sandbox_required`). `false` is only
    /// ever reached because a consumer explicitly set it that way.
    enforce: bool,
    profile: String,
}

fn resolve_decision(context: &ProcessLaunchContext) -> SandboxDecision {
    let workspace_root = Path::new(&context.workspace_root);
    let mut tier = crate::shell::sandbox_tier_for_workspace(workspace_root);
    // `network_required` is itself a typed, per-call-site decision (not a
    // default): a launch that declared it needs the network is capped to
    // `WorkspaceWrite` so it keeps filesystem isolation without the network
    // deny that `Strict` implies. This mirrors how `effective_sandbox_tier`
    // caps the tier for `PermissionMode::Bypass` — explicit, typed, still
    // enforced, never a silent drop to `Disabled`.
    if context.network_required {
        tier = tier.min(SandboxTier::WorkspaceWrite);
    }
    let profile = context
        .sandbox_profile
        .clone()
        .unwrap_or_else(|| "process_launch".to_string());
    SandboxDecision {
        tier,
        enforce: context.sandbox_required,
        profile,
    }
}

#[async_trait]
impl ProcessLaunchAdapter for LocalProcessLaunchAdapter {
    async fn spawn(
        &self,
        spec: CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        let decision = resolve_decision(&spec.context);
        spawn_with_decision(spec, decision).await
    }

    async fn spawn_channel(
        &self,
        spec: CommandLaunchSpec,
        channel: ExecutionChannel,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        match channel {
            ExecutionChannel::Pipe => self.spawn(spec).await,
            ExecutionChannel::Pty(size) => {
                let decision = resolve_decision(&spec.context);
                spawn_pty_with_decision(spec, decision, size).await
            }
        }
    }
}

/// Whether `name` must never reach a child: a licence or device locator (licensing), or a
/// provider or host credential, a daemon token or any other name providers R4 lists (providers
/// R46, D39). The one launcher applies this to every child: tools, hooks, plugins, MCP stdio
/// servers, LSP servers, vendor CLI drivers and the browser.
fn is_scrubbed_env_name(name: &str) -> bool {
    rightkit_agent_config::licence_env::is_licence_env_name(name)
        || rightkit_llm::agent_providers::host_credentials::is_credential_env_name(name)
        || crate::shell::extra_scrubbed_env_name(name)
}

/// `spec` with every scrubbed variable dropped from its layered environment, so no spawn arm
/// (confined or not) can pass one on.
fn scrub_spec_env(mut spec: CommandLaunchSpec) -> CommandLaunchSpec {
    spec.env
        .retain(|(name, _)| !is_scrubbed_env_name(name) || is_driver_gateway_name(name));
    spec
}

/// The driver-gateway secrets the daemon layers onto a vendor driver's launch on purpose, so
/// the driver's session-bound MCP helper can reach the gateway (providers R46's one exception).
/// They are kept only when set on the launch spec; an inherited copy is still removed.
fn is_driver_gateway_name(name: &str) -> bool {
    name == rightkit_agent_config::env_name("GATEWAY_TOKEN")
        || name == rightkit_agent_config::env_name("DRIVER_INSTANCE_SECRET")
        || crate::shell::extra_kept_env_name(name)
}

/// Scrubbed variables the child would only get by inheriting this process's environment: every
/// scrubbed name present there that the launch does not set itself. The native sandbox adapter
/// removes these on Linux and macOS (Windows forwards an allowlist and never inherits), so a
/// vendor driver, background shell or LSP child cannot pick up a provider token, a daemon token
/// or a licence locator. Names the launch sets explicitly are the caller's deliberate choice and
/// stay. Empty unless the host turned on [`crate::shell::set_scrub_inherited_credentials`].
// Windows forwards an allowlist and never inherits, so nothing calls this there.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn inherited_scrubbed_env_names(
    explicit: &[(String, String)],
) -> Vec<std::ffi::OsString> {
    if !crate::shell::scrub_inherited_credentials() {
        return Vec::new();
    }
    inherited_scrubbed_from(std::env::vars_os().map(|(name, _)| name), explicit)
}

/// Variables a Claude Code session exports to the programs it starts. When the host itself was
/// started from inside a Claude Code session (a terminal tool call, an IDE terminal, a cloud
/// session), the Claude Code that it starts must not inherit them: it would report the parent's
/// entry point as its own, and could attach to the parent's session, messaging socket and token.
/// Removing them lets the program describe itself. `CLAUDECODE` and `CLAUDE_CODE_CHILD_SESSION` are
/// documented (code.claude.com/docs/en/env-vars); the others are the ones Claude Code 2.1.327 was
/// seen to export. Applied to a vendor program only, and only when the host turned on
/// [`crate::shell::set_strip_parent_claude_session_env`].
pub const CLAUDE_HOST_SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_DESKTOP_APP_VERSION",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
];

/// Whether a launch is a vendor program: a sandbox profile that starts with `vendor-` or `driver`.
fn is_vendor_launch(spec: &CommandLaunchSpec) -> bool {
    spec.context
        .sandbox_profile
        .as_deref()
        .is_some_and(|profile| profile.starts_with("vendor-") || profile.starts_with("driver"))
}

/// Every variable a launch must not inherit from this process: the scrubbed names
/// ([`inherited_scrubbed_env_names`]) and, with
/// [`crate::shell::set_strip_parent_claude_session_env`] and for a vendor program, a parent Claude
/// Code session's own variables. Names the launch sets itself stay.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn inherited_env_names_to_remove(spec: &CommandLaunchSpec) -> Vec<std::ffi::OsString> {
    let mut names = inherited_scrubbed_env_names(&spec.env);
    if crate::shell::strip_parent_claude_session_env() && is_vendor_launch(spec) {
        names.extend(
            CLAUDE_HOST_SESSION_ENV
                .iter()
                .filter(|name| std::env::var_os(name).is_some())
                .filter(|name| !spec.env.iter().any(|(set, _)| set == *name))
                .map(std::ffi::OsString::from),
        );
    }
    names
}

/// Drop from `command` every scrubbed variable it would inherit from this process. No-op unless the
/// host turned on [`crate::shell::set_scrub_inherited_credentials`].
pub(crate) fn remove_inherited_scrubbed_env(command: &mut std::process::Command) {
    if crate::shell::scrub_inherited_credentials() {
        remove_scrubbed_from(command, std::env::vars_os().map(|(name, _)| name));
    }
}

/// Linux child hardening: drop `LD_PRELOAD`/`LD_AUDIT` and set `RLIMIT_CORE` to 0 in the child
/// before exec. No-op unless the host turned on [`crate::shell::set_harden_children`].
///
/// Must be called before Landlock/seccomp `confine`: `pre_exec` hooks run in registration order
/// and the seccomp allowlist could block `prlimit64` if it were installed first. `PR_SET_DUMPABLE`
/// is not set here because `execve` resets it for an ordinary binary. `LD_LIBRARY_PATH` is kept on
/// purpose (toolchains need it).
#[cfg(target_os = "linux")]
pub(crate) fn harden_child(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    if !crate::shell::harden_children() {
        return;
    }
    command.env_remove("LD_PRELOAD");
    command.env_remove("LD_AUDIT");
    // SAFETY: the hook only calls `setrlimit` (a thin prlimit64 syscall wrapper) on a stack
    // value: no allocation, locks or formatting.
    unsafe {
        command.pre_exec(|| {
            let lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &lim) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// See the Linux variant. macOS drops `DYLD_INSERT_LIBRARIES`, the `LD_PRELOAD` counterpart;
/// Windows has nothing to do.
#[cfg(not(target_os = "linux"))]
pub(crate) fn harden_child(command: &mut std::process::Command) {
    #[cfg(target_os = "macos")]
    if crate::shell::harden_children() {
        command.env_remove("DYLD_INSERT_LIBRARIES");
    }
    let _ = command;
}

fn remove_scrubbed_from(
    command: &mut std::process::Command,
    inherited: impl Iterator<Item = std::ffi::OsString>,
) {
    let explicit: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(name, value)| {
            Some((name.to_str()?.to_string(), value?.to_str()?.to_string()))
        })
        .collect();
    for name in inherited_scrubbed_from(inherited, &explicit) {
        command.env_remove(name);
    }
}

fn inherited_scrubbed_from(
    inherited: impl Iterator<Item = std::ffi::OsString>,
    explicit: &[(String, String)],
) -> Vec<std::ffi::OsString> {
    inherited
        .filter(|name| name.to_str().is_some_and(is_scrubbed_env_name))
        .filter(|name| {
            !explicit
                .iter()
                .any(|(set, _)| std::ffi::OsStr::new(set) == name.as_os_str())
        })
        .collect()
}

/// Every scrubbed variable in the inherited environment or in `spec.env`.
fn licence_env_names(spec: &CommandLaunchSpec) -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .chain(
            spec.env
                .iter()
                .map(|(name, _)| std::ffi::OsString::from(name)),
        )
        .filter(|name| name.to_str().is_some_and(is_scrubbed_env_name))
        .filter(|name| {
            !(name.to_str().is_some_and(is_driver_gateway_name)
                && spec
                    .env
                    .iter()
                    .any(|(set, _)| std::ffi::OsStr::new(set) == name.as_os_str()))
        })
        .collect()
}

fn build_command(
    spec: &CommandLaunchSpec,
    program: &str,
    argv: &[String],
) -> tokio::process::Command {
    let mut command = rightkit_child_process::tokio_support::command(program);
    // Inherit the daemon environment and layer `spec.env` on top, matching the
    // shell tool path: `git` and the hook/script launchers need PATH and the
    // platform's own variables to resolve their programs at all. Isolation is
    // the sandbox tier's job, not an empty environment's.
    #[cfg(windows)]
    {
        let cmd_tail = Path::new(program).file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.eq_ignore_ascii_case("cmd") || name.eq_ignore_ascii_case("cmd.exe")
        }) && argv.len() >= 2
            && argv[argv.len() - 2].eq_ignore_ascii_case("/c");
        if cmd_tail {
            // /C consumes shell source. CRT escaping would turn its quotes
            // into literal backslashes, corrupting hook paths/redirection.
            command
                .args(["/D", "/S"])
                .args(&argv[..argv.len() - 1])
                .raw_arg(format!("\"{}\"", argv[argv.len() - 1]));
        } else {
            command.args(argv);
        }
    }
    #[cfg(not(windows))]
    command.args(argv);
    command.current_dir(&spec.cwd);
    if spec.starts_from_empty_env() {
        // Plugin children see exactly `spec.env` (contract R6).
        command.env_clear();
    }
    command
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // No licence, device or credential variable reaches a child process (providers R46), whether
    // inherited from the daemon or layered on by the caller.
    for name in licence_env_names(spec) {
        command.env_remove(name);
    }
    // With `set_strip_parent_claude_session_env`, a vendor program must not see a parent Claude
    // Code session's variables at all. The driver runtime can only pin a removal to an empty value
    // (the launch contract has no "unset"), and an empty `CLAUDE_CODE_ENTRYPOINT` is still a
    // variable the program can read. Unset them here, where the command is built, unless the
    // launch sets a real value itself.
    if crate::shell::strip_parent_claude_session_env() && is_vendor_launch(spec) {
        for name in CLAUDE_HOST_SESSION_ENV {
            let set_by_launch = spec
                .env
                .iter()
                .any(|(set, value)| set == name && !value.is_empty());
            if !set_by_launch {
                command.env_remove(name);
            }
        }
    }
    // Own the whole tree so cancellation/timeout takes descendants with it
    // rather than orphaning them (mirrors the shell tool path). The child leads
    // its own group, which `rightkit_child_process::tokio_support::terminate_tree`
    // signals as a unit; Windows relies on the Job Object.
    #[cfg(unix)]
    // Own session (setsid), not just a group: the child must not keep the
    // daemon's controlling terminal.
    rightkit_child_process::tokio_support::lead_session(&mut command);
    // With `set_anchor_children_to_parent`, MCP servers, vendor drivers and operator terminals
    // launched here die with the host. Linux registers the parent-death signal here (macOS ties
    // the group in `spawn_command` instead). Spawned through `spawn_command` (anchor thread).
    // Before `confine` in `spawn_enforced`: seccomp must not see prctl.
    #[cfg(target_os = "linux")]
    if crate::shell::anchor_children_to_parent() {
        crate::shell::parent_death::kill_on_parent_exit(&mut command);
    }
    // Registered here, before any `confine` in `spawn_enforced`, so the rlimit hook runs first.
    // A no-op unless the host turned on `set_harden_children`.
    harden_child(command.as_std_mut());
    command
}

/// Spawn a command from [`build_command`]. With `set_anchor_children_to_parent` it is forked on
/// Unix through the anchor thread, so the parent-death signal registered there fires when the host
/// dies, not when a retiring tokio worker thread exits, and the child's group is tied to the host.
async fn spawn_command(
    mut command: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(unix)]
    if crate::shell::anchor_children_to_parent() {
        return crate::shell::parent_death::spawn_anchored(command).await;
    }
    command.spawn()
}

async fn spawn_unconfined(
    spec: &CommandLaunchSpec,
    proof: SandboxProcessProof,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let command = build_command(spec, &spec.program, &spec.argv);
    spawn_command(command)
        .await
        .map(|child| {
            Box::new(LocalManagedChild::Tokio {
                child,
                proof: proof.clone(),
            }) as Box<dyn ManagedChild>
        })
        .map_err(|_| EffectExecutionError::SpawnFailed)
}

async fn spawn_with_decision(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let spec = scrub_spec_env(spec);
    if !decision.enforce {
        // Explicit, typed unsandboxed escape hatch: the caller's
        // `ProcessLaunchContext` set `sandbox_required: false` deliberately.
        // Never a silent default — recorded here and in the returned proof.
        warn!(
            profile = %decision.profile,
            program = %spec.program,
            workspace_root = %spec.context.workspace_root,
            "process_launch: caller opted out of sandbox enforcement (sandbox_required=false)"
        );
        let proof = SandboxProcessProof {
            backend: "none".to_string(),
            profile: decision.profile.clone(),
            enforced: false,
            network_enforced: false,
        };
        return spawn_unconfined(&spec, proof).await;
    }

    if decision.tier.is_disabled() {
        // Operator/workspace policy resolved the tier itself to `Disabled`.
        // This is a policy decision, not a caller escape hatch, so it is
        // logged distinctly from the opt-out path above.
        info!(
            profile = %decision.profile,
            program = %spec.program,
            "process_launch: sandbox tier resolved to Disabled by policy; running unconfined"
        );
        let proof = SandboxProcessProof {
            backend: "policy-disabled".to_string(),
            profile: decision.profile.clone(),
            enforced: false,
            network_enforced: false,
        };
        return spawn_unconfined(&spec, proof).await;
    }

    spawn_enforced(spec, decision).await
}

async fn spawn_pty_with_decision(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
    size: PtySize,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let spec = scrub_spec_env(spec);
    if spec.context.sandbox_required && decision.tier.is_disabled() {
        return Err(EffectExecutionError::SandboxMissing);
    }
    if !decision.enforce || decision.tier.is_disabled() {
        let backend = if decision.enforce {
            "policy-disabled"
        } else {
            "none"
        };
        let proof = SandboxProcessProof {
            backend: backend.to_string(),
            profile: decision.profile,
            enforced: false,
            network_enforced: false,
        };
        let program = spec.program.clone();
        let argv = spec.argv.clone();
        return crate::managed_pty::spawn(&spec, size, &program, &argv, proof, None);
    }

    #[cfg(target_os = "macos")]
    {
        let workspace_root = Path::new(&spec.context.workspace_root);
        let Some((program, argv)) = crate::shell::macos_sandbox_wrap_for_tier(
            &spec.program,
            &spec.argv,
            workspace_root,
            decision.tier,
        ) else {
            return Err(EffectExecutionError::SandboxMissing);
        };
        let proof = SandboxProcessProof {
            backend: "seatbelt".to_string(),
            profile: decision.profile,
            enforced: true,
            network_enforced: decision.tier.denies_network(),
        };
        crate::managed_pty::spawn(&spec, size, &program, &argv, proof, None)
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        let proof = SandboxProcessProof {
            backend: if cfg!(windows) {
                "appcontainer"
            } else {
                "landlock-seccomp"
            }
            .into(),
            profile: decision.profile,
            enforced: true,
            // Windows: the AppContainer token carries `internetClient` exactly
            // when the tier does not deny network (see `spawn_enforced`).
            network_enforced: decision.tier.denies_network(),
        };
        let program = spec.program.clone();
        let argv = spec.argv.clone();
        crate::managed_pty::spawn(&spec, size, &program, &argv, proof, Some(decision.tier))
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (spec, decision, size);
        Err(EffectExecutionError::SandboxMissing)
    }
}

#[cfg(target_os = "windows")]
async fn spawn_enforced(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let workspace_root = Path::new(&spec.context.workspace_root);
    // Network is a capability of the token, decided per launch by the tier
    // (a `network_required` launch is already capped to WorkspaceWrite by
    // `resolve_decision`): Strict withholds `internetClient`, anything looser
    // carries it. The proof below reports exactly what the token received.
    let network_allowed = !decision.tier.denies_network();
    let profile = match crate::sandbox::AppContainerProfile::for_workspace(workspace_root) {
        Ok(profile) => profile.with_network(network_allowed),
        Err(error) => {
            warn!(
                profile = %decision.profile,
                %error,
                "process_launch: AppContainer profile setup unavailable; refusing unsandboxed spawn"
            );
            return Err(EffectExecutionError::SandboxMissing);
        }
    };
    let confined = crate::sandbox::spawn_confined_piped_with_env(
        &profile,
        &spec.program,
        &spec.argv,
        Path::new(&spec.cwd),
        true,
        &spec.env,
    );
    let child = match confined {
        Ok(child) => child,
        Err(error) => {
            warn!(
                profile = %decision.profile,
                %error,
                "process_launch: AppContainer spawn unavailable; refusing unsandboxed spawn"
            );
            return Err(EffectExecutionError::SandboxMissing);
        }
    };
    let mut child = child;
    let pid = child.pid();
    // The confined spawn already created the child suspended and assigned it
    // to a kill-on-close Job Object before its first instruction. Reuse that
    // job rather than nesting a second one; only a child without one (never
    // expected) is assigned here, and a failure refuses the launch.
    let job = match child.take_job() {
        Some(job) => Some(job),
        None => match crate::shell::assign_windows_job_to_pid(pid) {
            Ok(job) => Some(job),
            Err(error) => {
                let _ = child.terminate();
                warn!(%error, "process_launch: Windows Job Object assignment failed; refusing unconfined spawn");
                return Err(EffectExecutionError::SandboxMissing);
            }
        },
    };
    let proof = SandboxProcessProof {
        backend: "appcontainer".to_string(),
        profile: decision.profile,
        enforced: true,
        network_enforced: !network_allowed,
    };
    Ok(Box::new(LocalManagedChild::Confined { child, job, proof }))
}

#[cfg(target_os = "macos")]
async fn spawn_enforced(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let workspace_root = Path::new(&spec.context.workspace_root);
    let wrapped = crate::shell::macos_sandbox_wrap_for_tier(
        &spec.program,
        &spec.argv,
        workspace_root,
        decision.tier,
    );
    let (program, argv) = match wrapped {
        Some(command) => command,
        None => {
            // Only a `Disabled` tier makes the wrapper decline, and that is
            // already handled by the caller before reaching this branch.
            warn!(
                profile = %decision.profile,
                "process_launch: Seatbelt wrap unavailable for a non-disabled tier; refusing unsandboxed spawn"
            );
            return Err(EffectExecutionError::SandboxMissing);
        }
    };
    let proof = SandboxProcessProof {
        backend: "seatbelt".to_string(),
        profile: decision.profile,
        enforced: true,
        network_enforced: decision.tier.denies_network(),
    };
    let command = build_command(&spec, &program, &argv);
    spawn_command(command)
        .await
        .map(|child| {
            Box::new(LocalManagedChild::Tokio {
                child,
                proof: proof.clone(),
            }) as Box<dyn ManagedChild>
        })
        .map_err(|_| EffectExecutionError::SandboxMissing)
}

#[cfg(target_os = "linux")]
async fn spawn_enforced(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let workspace_root = Path::new(&spec.context.workspace_root);
    let mut config = crate::shell::local_sandbox_config_for_tier(workspace_root, decision.tier);
    // The launched program itself must be readable and executable: a vendor CLI
    // usually lives outside the workspace and the system paths (`~/.local/bin`).
    for dir in program_dirs(&spec) {
        config = config.allow_read(dir);
    }
    // A vendor driver's MCP config launches the host app's own executable (the session gateway).
    if decision.profile.starts_with("driver") {
        if let Ok(own) = std::env::current_exe() {
            config = config.allow_read(own);
        }
    }
    let proof = SandboxProcessProof {
        backend: "landlock-seccomp".to_string(),
        profile: decision.profile,
        enforced: true,
        network_enforced: config.network_denied,
    };
    let mut command = build_command(&spec, &spec.program, &spec.argv);
    // RightKit prepares Landlock/seccomp in the parent and registers a syscall-only
    // `pre_exec` hook. A failure here (Landlock unavailable, invalid policy) means the
    // sandbox backend itself failed, so the launch is refused rather than run unconfined.
    config
        .confine(command.as_std_mut())
        .map_err(|_| EffectExecutionError::SandboxMissing)?;
    spawn_command(command)
        .await
        .map(|child| {
            Box::new(LocalManagedChild::Tokio {
                child,
                proof: proof.clone(),
            }) as Box<dyn ManagedChild>
        })
        // A pre_exec failure surfaces here as a generic spawn error; on the
        // enforced path that means the sandbox backend itself failed, so it
        // is reported as a sandbox failure, not a bare spawn failure.
        .map_err(|_| EffectExecutionError::SandboxMissing)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
async fn spawn_enforced(
    spec: CommandLaunchSpec,
    decision: SandboxDecision,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    // No sandbox backend exists for this platform. This mirrors the shell
    // tool path's own platform coverage (Windows/macOS/Linux only) rather
    // than inventing a fourth policy; logged distinctly from both the
    // caller opt-out and the policy-disabled paths above.
    warn!(
        profile = %decision.profile,
        "process_launch: no sandbox backend for this platform; running unconfined"
    );
    let proof = SandboxProcessProof {
        backend: "unsupported-platform".to_string(),
        profile: decision.profile,
        enforced: false,
        network_enforced: false,
    };
    spawn_unconfined(&spec, proof).await
}

enum LocalManagedChild {
    Tokio {
        child: tokio::process::Child,
        proof: SandboxProcessProof,
    },
    #[cfg(target_os = "windows")]
    Confined {
        // Declared before `child` so survivors are killed (job closed) before
        // the child's ACL leases are released.
        job: Option<crate::sandbox::WindowsJob>,
        child: crate::sandbox::ConfinedPipedChild,
        proof: SandboxProcessProof,
    },
}

#[async_trait]
impl ManagedChild for LocalManagedChild {
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Tokio { child, .. } => child.id(),
            #[cfg(target_os = "windows")]
            Self::Confined { child, .. } => Some(child.pid()),
        }
    }

    fn sandbox_proof(&self) -> Option<SandboxProcessProof> {
        match self {
            Self::Tokio { proof, .. } => Some(proof.clone()),
            #[cfg(target_os = "windows")]
            Self::Confined { proof, .. } => Some(proof.clone()),
        }
    }

    fn start_kill(&mut self) -> bool {
        match self {
            Self::Tokio { child, .. } => child.start_kill().is_ok(),
            #[cfg(target_os = "windows")]
            Self::Confined { child, job, .. } => {
                // The Job Object owns the whole tree; fall back to the root
                // process only when job termination is unavailable.
                job.as_ref().is_some_and(|job| job.terminate().is_ok()) || child.terminate().is_ok()
            }
        }
    }

    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        match self {
            Self::Tokio { child, .. } => child
                .stdin
                .take()
                .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncWrite + Send>>),
            #[cfg(target_os = "windows")]
            Self::Confined { child, .. } => child.take_stdin().map(|stream| {
                let pinned: Pin<Box<dyn AsyncWrite + Send + Unpin>> = Box::into_pin(stream);
                pinned as Pin<Box<dyn AsyncWrite + Send>>
            }),
        }
    }

    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        match self {
            Self::Tokio { child, .. } => child
                .stdout
                .take()
                .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncRead + Send>>),
            #[cfg(target_os = "windows")]
            Self::Confined { child, .. } => child.take_stdout().map(|stream| {
                let pinned: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::into_pin(stream);
                pinned as Pin<Box<dyn AsyncRead + Send>>
            }),
        }
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        match self {
            Self::Tokio { child, .. } => child
                .stderr
                .take()
                .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncRead + Send>>),
            #[cfg(target_os = "windows")]
            Self::Confined { child, .. } => child.take_stderr().map(|stream| {
                let pinned: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::into_pin(stream);
                pinned as Pin<Box<dyn AsyncRead + Send>>
            }),
        }
    }

    async fn wait(&mut self) -> Result<i32, EffectExecutionError> {
        match self {
            Self::Tokio { child, .. } => child
                .wait()
                .await
                .map(|status| status.code().unwrap_or(-1))
                .map_err(|_| EffectExecutionError::Internal),
            #[cfg(target_os = "windows")]
            Self::Confined { child, .. } => child
                .wait_code()
                .await
                .map(|code| code as i32)
                .map_err(|_| EffectExecutionError::Internal),
        }
    }

    async fn cancel(&mut self) -> Result<(), EffectExecutionError> {
        match self {
            Self::Tokio { child, .. } => rightkit_child_process::tokio_support::terminate_tree(
                child,
                rightkit_child_process::TerminationOptions::default()
                    .with_grace(std::time::Duration::from_secs(2)),
            )
            .await
            .map(|_| ())
            .map_err(|_| EffectExecutionError::KillFailed),
            #[cfg(target_os = "windows")]
            Self::Confined { child, job, .. } => {
                if job.as_ref().is_some_and(|job| job.terminate().is_ok()) {
                    return Ok(());
                }
                child
                    .terminate()
                    .map_err(|_| EffectExecutionError::KillFailed)
            }
        }
    }
}

/// The directories holding the program a launch names: where `PATH` (the launch's
/// own, else the host's) finds it, and where a symlink there resolves to.
#[cfg(target_os = "linux")]
pub(crate) fn program_dirs(spec: &CommandLaunchSpec) -> Vec<std::path::PathBuf> {
    let program = Path::new(&spec.program);
    let found = if program.components().count() > 1 {
        Some(program.to_path_buf())
    } else {
        let path_var = spec
            .env
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| std::ffi::OsString::from(value))
            .or_else(|| std::env::var_os("PATH"));
        path_var.and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
    };
    let Some(found) = found else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if let Some(parent) = found.parent() {
        dirs.push(parent.to_path_buf());
    }
    if let Some(parent) = found.canonicalize().ok().as_deref().and_then(Path::parent) {
        if !dirs.iter().any(|dir| dir == parent) {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs
}
