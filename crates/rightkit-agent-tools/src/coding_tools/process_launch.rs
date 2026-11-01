//! Explicit local process adapter used when host has not supplied backend.
//!
//! CG-B02: every daemon-launched process must resolve and enforce the
//! selected sandbox tier through the *same seam* the interactive shell tool
//! path uses (`crate::coding_tools::shell::{sandbox_tier_for_workspace,
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

use crate::coding_tools::sandbox::SandboxTier;

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
    let mut tier = crate::coding_tools::shell::sandbox_tier_for_workspace(workspace_root);
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

/// Every licence- or device-named variable in the inherited environment or in `spec.env`.
fn licence_env_names(spec: &CommandLaunchSpec) -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .chain(
            spec.env
                .iter()
                .map(|(name, _)| std::ffi::OsString::from(name)),
        )
        .filter(|name| {
            name.to_str()
                .is_some_and(rightkit_app_settings::agent_config::licence_env::is_licence_env_name)
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
    command
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Defence in depth: no licence or device variable reaches a tool, hook or script process,
    // whether inherited from the daemon or layered on by the caller. The daemon holds none (it
    // opens the licence record itself), so this only closes a variable that appears later.
    for name in licence_env_names(spec) {
        command.env_remove(name);
    }
    // Own the whole tree so cancellation/timeout takes descendants with it
    // rather than orphaning them (mirrors the shell tool path). The child leads
    // its own group, which `rightkit_child_process::tokio_support::terminate_tree`
    // signals as a unit; Windows relies on the Job Object.
    #[cfg(unix)]
    // Own session (setsid), not just a group: the child must not keep the
    // daemon's controlling terminal.
    rightkit_child_process::tokio_support::lead_session(&mut command);
    command
}

async fn spawn_unconfined(
    spec: &CommandLaunchSpec,
    proof: SandboxProcessProof,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let mut command = build_command(spec, &spec.program, &spec.argv);
    command
        .spawn()
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
        return crate::coding_tools::managed_pty::spawn(&spec, size, &program, &argv, proof, None);
    }

    #[cfg(target_os = "macos")]
    {
        let workspace_root = Path::new(&spec.context.workspace_root);
        let Some((program, argv)) = crate::coding_tools::shell::macos_sandbox_wrap_for_tier(
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
        return crate::coding_tools::managed_pty::spawn(&spec, size, &program, &argv, proof, None);
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
        crate::coding_tools::managed_pty::spawn(
            &spec,
            size,
            &program,
            &argv,
            proof,
            Some(decision.tier),
        )
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
    let profile = match crate::coding_tools::sandbox::AppContainerProfile::for_workspace(
        workspace_root,
    ) {
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
    let confined = crate::coding_tools::sandbox::spawn_confined_piped_with_env(
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
        None => match crate::coding_tools::shell::assign_windows_job_to_pid(pid) {
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
    let wrapped = crate::coding_tools::shell::macos_sandbox_wrap_for_tier(
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
    let mut command = build_command(&spec, &program, &argv);
    command
        .spawn()
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
    let config =
        crate::coding_tools::shell::local_sandbox_config_for_tier(workspace_root, decision.tier);
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
    command
        .spawn()
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
        job: Option<crate::coding_tools::sandbox::WindowsJob>,
        child: crate::coding_tools::sandbox::ConfinedPipedChild,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(
        program: &str,
        argv: Vec<&str>,
        workspace_root: &str,
        sandbox_required: bool,
    ) -> CommandLaunchSpec {
        CommandLaunchSpec {
            program: program.to_string(),
            argv: argv.into_iter().map(str::to_string).collect(),
            cwd: workspace_root.to_string(),
            context: ProcessLaunchContext {
                workspace_root: workspace_root.to_string(),
                sandbox_profile: Some("test".to_string()),
                sandbox_required,
                network_required: false,
            },
            env: Vec::new(),
        }
    }

    #[test]
    fn resolve_decision_honors_explicit_typed_opt_out() {
        let context = ProcessLaunchContext {
            workspace_root: std::env::temp_dir().to_string_lossy().into_owned(),
            sandbox_profile: Some("hooks".to_string()),
            sandbox_required: false,
            network_required: false,
        };
        let decision = resolve_decision(&context);
        assert!(
            !decision.enforce,
            "sandbox_required=false must stay opt-out, never default to enforce"
        );
        assert_eq!(decision.profile, "hooks");
    }

    #[test]
    fn resolve_decision_defaults_profile_when_unset() {
        let context = ProcessLaunchContext {
            workspace_root: std::env::temp_dir().to_string_lossy().into_owned(),
            sandbox_profile: None,
            sandbox_required: true,
            network_required: false,
        };
        let decision = resolve_decision(&context);
        assert!(decision.enforce);
        assert_eq!(decision.profile, "process_launch");
    }

    #[test]
    fn a_licence_named_variable_is_removed_from_every_launched_command() {
        let mut spec = spec_with("tool", Vec::<&str>::new(), ".", false);
        spec.env = vec![
            (
                "RIGHTKIT_SIGNED_LICENSE_TOKEN".to_string(),
                "secret".to_string(),
            ),
            (
                "RIGHTKIT_LICENSE_DEVICE_ID".to_string(),
                "device".to_string(),
            ),
            ("KEEP_ME".to_string(), "1".to_string()),
        ];
        let command = build_command(&spec, "tool", &[]);
        let envs: Vec<(String, Option<String>)> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for name in [
            "RIGHTKIT_SIGNED_LICENSE_TOKEN",
            "RIGHTKIT_LICENSE_DEVICE_ID",
        ] {
            assert!(
                envs.contains(&(name.to_string(), None)),
                "{name} must be removed, not set"
            );
        }
        assert!(envs.contains(&("KEEP_ME".to_string(), Some("1".to_string()))));
    }

    #[tokio::test]
    async fn unsandboxed_opt_out_records_diagnostic_proof() {
        let workspace = std::env::temp_dir();
        let spec = spec_with(
            shell_program(),
            shell_args("exit 0"),
            &workspace.to_string_lossy(),
            false,
        );
        let adapter = LocalProcessLaunchAdapter;
        let mut child = adapter.spawn(spec).await.expect("spawn should succeed");
        let proof = child
            .sandbox_proof()
            .expect("opt-out path must still record a diagnostic proof");
        assert!(
            !proof.enforced,
            "explicit opt-out must never report enforced=true"
        );
        assert_eq!(proof.backend, "none");
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn cancel_kills_a_running_child() {
        let workspace = std::env::temp_dir();
        let spec = spec_with(
            shell_program(),
            long_sleep_args(),
            &workspace.to_string_lossy(),
            false,
        );
        let adapter = LocalProcessLaunchAdapter;
        let mut child = adapter.spawn(spec).await.expect("spawn should succeed");
        let pid = child.pid();
        child.cancel().await.expect("cancel should succeed");
        if let Some(pid) = pid {
            // Give the OS a moment to reap; the process must not still be
            // reported alive by a fresh wait.
            let alive = process_still_running(pid);
            assert!(!alive, "cancel() must actually terminate the child process");
        }
    }

    #[cfg(unix)]
    fn shell_program() -> &'static str {
        "/bin/sh"
    }
    #[cfg(windows)]
    fn shell_program() -> &'static str {
        "cmd"
    }

    #[cfg(unix)]
    fn shell_args(cmd: &str) -> Vec<&str> {
        vec!["-c", cmd]
    }
    #[cfg(windows)]
    fn shell_args(cmd: &str) -> Vec<&str> {
        vec!["/C", cmd]
    }

    #[cfg(unix)]
    fn long_sleep_args() -> Vec<&'static str> {
        vec!["-c", "sleep 30"]
    }
    #[cfg(windows)]
    fn long_sleep_args() -> Vec<&'static str> {
        vec!["/C", "ping -n 30 127.0.0.1 >NUL"]
    }

    #[cfg(unix)]
    fn process_still_running(pid: u32) -> bool {
        // SAFETY: signal 0 only probes existence/permission; it does not
        // deliver a signal.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    fn process_still_running(_pid: u32) -> bool {
        // Best-effort on Windows: absence of a crash from wait() above plus
        // job/handle termination is the primary signal here.
        false
    }
}
