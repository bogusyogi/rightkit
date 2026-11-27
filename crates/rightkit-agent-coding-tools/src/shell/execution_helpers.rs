/// Per-session scratch directory for shell commands, exported as `TMPDIR` (and `TEMP`/`TMP`) when
/// the host turned on `set_session_scratch_tmpdir`. The sandbox only writes inside the workspace,
/// so `/tmp` is denied and `mktemp -d` fails. The directory is
/// `<workspace>/<state dir>/tmp/<session>` (the state folder is `rightkit_agent_config::state_dir_name`,
/// `.rightkit` by default); a `.gitignore` holding `*` is written beside it so nothing under
/// `<state dir>/tmp` shows in `git status`, in a plain checkout or a worktree alike.
/// Nothing is written inside `.git`. Created on first use; `None` when the option is off or the
/// directory cannot be created.
pub(crate) fn session_scratch_dir(ctx: &ToolContext) -> Option<std::path::PathBuf> {
    if !session_scratch_tmpdir() {
        return None;
    }
    let session: String = ctx
        .session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let session = if session.is_empty() {
        "session".to_string()
    } else {
        session
    };
    scratch_dir_in(&ctx.workspace_root, &session)
}

/// `<workspace>/<state dir>/tmp/<name>`, created on demand, with the ignore file beside it, when the
/// host turned on `set_session_scratch_tmpdir`; `None` otherwise. Background shells have no
/// session in scope and share the name `background`.
pub(crate) fn scratch_dir_in(
    workspace_root: &std::path::Path,
    name: &str,
) -> Option<std::path::PathBuf> {
    if !session_scratch_tmpdir() {
        return None;
    }
    let base = workspace_root
        .join(rightkit_agent_config::state_dir_name())
        .join("tmp");
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).ok()?;
    let ignore = base.join(".gitignore");
    if !ignore.exists() {
        // Best effort: a failure only means the scratch files show as untracked.
        let _ = std::fs::write(&ignore, "*\n");
    }
    Some(dir)
}

/// The `TMPDIR`, `TEMP` and `TMP` entries for `dir`, or none.
pub(crate) fn scratch_env(dir: Option<std::path::PathBuf>) -> Vec<(String, String)> {
    dir.map(|dir| {
        let dir = dir.display().to_string();
        ["TMPDIR", "TEMP", "TMP"]
            .iter()
            .map(|name| (name.to_string(), dir.clone()))
            .collect()
    })
    .unwrap_or_default()
}

/// Native policy adapter used by daemon process producers. It prepares the
/// executable/argv at the platform seam; child configuration (pre-exec,
/// Seatbelt, or AppContainer) remains owned by the concrete caller.
#[cfg(feature = "host")]
pub struct NativeCommandSandboxAdapter;

#[cfg(all(feature = "host", target_os = "linux"))]
struct LinuxManagedChild {
    child: tokio::process::Child,
    /// Recorded at spawn: whether the launch actually installed the sandbox
    /// and whether its config denies network. The proof reports the real
    /// launch outcome, never a hard-coded positive.
    enforced: bool,
    network_enforced: bool,
}

#[cfg(all(feature = "host", target_os = "linux"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ManagedChild for LinuxManagedChild {
    fn pid(&self) -> Option<u32> {
        self.child.id()
    }
    fn sandbox_proof(&self) -> Option<rightkit_sandbox::execution::SandboxProcessProof> {
        Some(rightkit_sandbox::execution::SandboxProcessProof {
            backend: if self.enforced {
                "linux-landlock-seccomp".into()
            } else {
                "none".into()
            },
            profile: "workspace".into(),
            enforced: self.enforced,
            network_enforced: self.network_enforced,
        })
    }
    fn start_kill(&mut self) -> bool {
        self.child.start_kill().is_ok()
    }
    fn take_stdin(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>> {
        self.child.stdin.take().map(|s| Box::pin(s) as _)
    }
    fn take_stdout(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.stdout.take().map(|s| Box::pin(s) as _)
    }
    fn take_stderr(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.stderr.take().map(|s| Box::pin(s) as _)
    }
    async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
        self.child
            .wait()
            .await
            .map(|s| s.code().unwrap_or(-1))
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::Internal)
    }
    async fn cancel(&mut self) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        // The child leads its own session: stop the whole tree, not only the
        // root, so a `sh -c '...; sleep'` leaves no orphan holding the pipes.
        rightkit_child_process::tokio_support::terminate_tree(
            &mut self.child,
            rightkit_child_process::TerminationOptions::default()
                .with_grace(std::time::Duration::from_secs(2)),
        )
        .await
        .map(|_| ())
        .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::KillFailed)
    }
}

#[cfg(all(feature = "host", target_os = "linux"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ProcessLaunchAdapter for NativeCommandSandboxAdapter {
    async fn spawn(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        let mut command = rightkit_child_process::tokio_support::command(&spec.program);
        command
            .args(&spec.argv)
            // The launch's own working directory; the sandbox still confines
            // to the workspace root.
            .current_dir(if spec.cwd.trim().is_empty() {
                &spec.context.workspace_root
            } else {
                &spec.cwd
            })
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if spec.starts_from_empty_env() {
            command.env_clear();
        }
        command.envs(spec.env.clone());
        // No scrubbed variable reaches the child by inheritance (empty unless the host turned on
        // `set_scrub_inherited_credentials`), and with `set_strip_parent_claude_session_env` a vendor
        // program does not inherit a parent Claude Code session's variables.
        for name in crate::process_launch::inherited_env_names_to_remove(&spec) {
            command.env_remove(name);
        }
        // Own session, so cancel can stop the whole tree (see `cancel`).
        rightkit_child_process::tokio_support::lead_session(&mut command);
        // Die with the host when it asked for that. Before `confine`: seccomp
        // must not see prctl.
        let anchored = crate::shell::anchor_children_to_parent();
        if anchored {
            parent_death::kill_on_parent_exit(&mut command);
        }
        // Harden the child (a no-op unless the host turned on `set_harden_children`) before
        // `confine` installs the sandbox hooks.
        crate::process_launch::harden_child(command.as_std_mut());
        let mut enforced = false;
        let mut network_enforced = false;
        if spec.context.sandbox_required {
            if spec.context.sandbox_profile.is_none() {
                return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
            }
            let root = std::path::PathBuf::from(&spec.context.workspace_root);
            // With `set_honour_disabled_sandbox_tier` the tier comes from `native_launch_tier`
            // and an operator-disabled tier runs unconfined on every OS.
            let honoured_tier = crate::shell::honour_disabled_sandbox_tier()
                .then(|| crate::shell::native_launch_tier(&root, spec.context.network_required));
            if !honoured_tier.is_some_and(|tier| tier.is_disabled()) {
                // Network-required producers (for example cloudflared) retain
                // filesystem confinement but must not inherit strict seccomp's
                // network denial. Default launches remain strict/fail-closed.
                let config = if let Some(tier) = honoured_tier {
                    crate::shell::local_sandbox_config_for_tier(&root, tier)
                } else if spec.context.network_required {
                    crate::shell::local_sandbox_config_for_tier(
                        &root,
                        crate::sandbox::SandboxTier::WorkspaceWrite,
                    )
                } else {
                    crate::shell::local_sandbox_config(&root)
                };
                network_enforced = config.network_denied;
                // The launched program must be readable and executable (see process_launch).
                let config = crate::process_launch::program_dirs(&spec)
                    .into_iter()
                    .fold(config, |config, dir| config.allow_read(dir));
                // With `set_native_vendor_own_executable_read`, a vendor program is given exactly
                // one MCP server, and that server is the host's own executable (see
                // process_launch, which grants the same for its driver profiles). Without this
                // the program cannot start it wherever the host is installed outside the system
                // paths, and has no tools.
                let config = if crate::shell::native_vendor_own_executable_read()
                    && spec
                        .context
                        .sandbox_profile
                        .as_deref()
                        .is_some_and(|profile| {
                            profile.starts_with("vendor-") || profile.starts_with("driver")
                        }) {
                    match std::env::current_exe() {
                        Ok(own) => config.allow_read(own),
                        Err(_) => config,
                    }
                } else {
                    config
                };
                // RightKit prepares the confinement in the parent; a failure is a
                // refused launch, never an unconfined one.
                config.confine(command.as_std_mut()).map_err(|_| {
                    rightkit_sandbox::execution::EffectExecutionError::SandboxMissing
                })?;
                enforced = true;
            }
        }
        let spawned = if anchored {
            parent_death::spawn_anchored(command).await
        } else {
            command.spawn()
        };
        spawned
            .map(|child| {
                Box::new(LinuxManagedChild {
                    child,
                    enforced,
                    network_enforced,
                }) as _
            })
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::SpawnFailed)
    }
    async fn spawn_channel(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
        channel: rightkit_sandbox::execution::ExecutionChannel,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        match channel {
            rightkit_sandbox::execution::ExecutionChannel::Pipe => self.spawn(spec).await,
            rightkit_sandbox::execution::ExecutionChannel::Pty(_) => {
                <crate::process_launch::LocalProcessLaunchAdapter as rightkit_sandbox::execution::ProcessLaunchAdapter>::spawn_channel(
                    &crate::process_launch::LocalProcessLaunchAdapter,
                    spec,
                    channel,
                )
                .await
            }
        }
    }
}

#[cfg(all(feature = "host", target_os = "windows"))]
struct WindowsManagedChild {
    child: crate::sandbox::ConfinedPipedChild,
    network_enforced: bool,
}

#[cfg(all(feature = "host", target_os = "windows"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ManagedChild for WindowsManagedChild {
    fn pid(&self) -> Option<u32> {
        Some(self.child.pid())
    }
    fn sandbox_proof(&self) -> Option<rightkit_sandbox::execution::SandboxProcessProof> {
        Some(rightkit_sandbox::execution::SandboxProcessProof {
            backend: "windows-appcontainer".into(),
            profile: "workspace".into(),
            enforced: true,
            network_enforced: self.network_enforced,
        })
    }
    fn start_kill(&mut self) -> bool {
        self.child.terminate().is_ok()
    }
    fn take_stdin(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>> {
        self.child.take_stdin().map(|s| Box::pin(s) as _)
    }
    fn take_stdout(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.take_stdout().map(|s| Box::pin(s) as _)
    }
    fn take_stderr(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.take_stderr().map(|s| Box::pin(s) as _)
    }
    async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
        self.child
            .wait_code()
            .await
            .map(|c| c as i32)
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::Internal)
    }
    async fn cancel(&mut self) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        self.child
            .terminate()
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::KillFailed)
    }
}

#[cfg(all(feature = "host", target_os = "windows"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ProcessLaunchAdapter for NativeCommandSandboxAdapter {
    async fn spawn(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        // A launch that requires a sandbox must name its profile, as on Linux and macOS.
        if spec.context.sandbox_required && spec.context.sandbox_profile.is_none() {
            return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
        }
        let root = std::path::PathBuf::from(&spec.context.workspace_root);
        // With `set_honour_disabled_sandbox_tier` the tier comes from `native_launch_tier`: an
        // operator-disabled tier runs unconfined and network access follows the tier.
        let honoured_tier = crate::shell::honour_disabled_sandbox_tier()
            .then(|| crate::shell::native_launch_tier(&root, spec.context.network_required));
        if !spec.context.sandbox_required || honoured_tier.is_some_and(|tier| tier.is_disabled()) {
            // Typed opt-out (e.g. user lifecycle hooks outside safe mode): run
            // through the local adapter, which records an unenforced proof.
            return <crate::process_launch::LocalProcessLaunchAdapter as rightkit_sandbox::execution::ProcessLaunchAdapter>::spawn(
                &crate::process_launch::LocalProcessLaunchAdapter,
                spec,
            )
            .await;
        }
        // Run in the requested cwd when it lies inside the sandbox root (e.g.
        // git inside a pooled worktree); never silently substitute the root,
        // which would retarget commands at the enclosing repository.
        let cwd = if spec.cwd.trim().is_empty() {
            root.clone()
        } else {
            std::path::PathBuf::from(&spec.cwd)
        };
        let cwd_inside_root = cwd
            .canonicalize()
            .ok()
            .zip(root.canonicalize().ok())
            .is_some_and(|(cwd, root)| cwd.starts_with(root));
        if !cwd_inside_root {
            tracing::warn!(cwd = %cwd.display(), root = %root.display(), "AppContainer launch refused: cwd outside workspace root");
            return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
        }
        // Profile preparation grants ACLs (recursive on the workspace) and
        // creates the process synchronously; run it on the blocking pool so it
        // never stalls an async runtime worker serving other requests.
        let network_allowed = match honoured_tier {
            Some(tier) => !tier.denies_network(),
            None => spec.context.network_required,
        };
        let program = spec.program.clone();
        let argv = spec.argv.clone();
        let env = spec.env.clone();
        let confined = tokio::task::spawn_blocking(move || {
            crate::sandbox::AppContainerProfile::for_workspace(&root).and_then(|profile| {
                let profile = profile.with_network(network_allowed);
                crate::sandbox::spawn_confined_piped_with_env(
                    &profile, &program, &argv, &cwd, true, &env,
                )
            })
        })
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
        match confined {
            Ok(child) => Ok(Box::new(WindowsManagedChild {
                child,
                network_enforced: !network_allowed,
            })),
            Err(error) => {
                tracing::warn!(%error, program = %spec.program, "AppContainer launch refused");
                Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing)
            }
        }
    }
    async fn spawn_channel(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
        channel: rightkit_sandbox::execution::ExecutionChannel,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        match channel {
            rightkit_sandbox::execution::ExecutionChannel::Pipe => self.spawn(spec).await,
            rightkit_sandbox::execution::ExecutionChannel::Pty(_) => {
                <crate::process_launch::LocalProcessLaunchAdapter as rightkit_sandbox::execution::ProcessLaunchAdapter>::spawn_channel(
                    &crate::process_launch::LocalProcessLaunchAdapter,
                    spec,
                    channel,
                )
                .await
            }
        }
    }
}

#[cfg(all(feature = "host", target_os = "macos"))]
struct MacManagedChild {
    child: tokio::process::Child,
    /// Recorded at spawn: whether the Seatbelt wrap was actually applied.
    enforced: bool,
    network_enforced: bool,
}

#[cfg(all(feature = "host", target_os = "macos"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ManagedChild for MacManagedChild {
    fn pid(&self) -> Option<u32> {
        self.child.id()
    }
    fn sandbox_proof(&self) -> Option<rightkit_sandbox::execution::SandboxProcessProof> {
        Some(rightkit_sandbox::execution::SandboxProcessProof {
            backend: if self.enforced {
                "macos-seatbelt".into()
            } else {
                "none".into()
            },
            profile: "workspace".into(),
            enforced: self.enforced,
            network_enforced: self.network_enforced,
        })
    }
    fn start_kill(&mut self) -> bool {
        self.child.start_kill().is_ok()
    }
    fn take_stdin(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>> {
        self.child.stdin.take().map(|s| Box::pin(s) as _)
    }
    fn take_stdout(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.stdout.take().map(|s| Box::pin(s) as _)
    }
    fn take_stderr(&mut self) -> Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.child.stderr.take().map(|s| Box::pin(s) as _)
    }
    async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
        self.child
            .wait()
            .await
            .map(|s| s.code().unwrap_or(-1))
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::Internal)
    }
    async fn cancel(&mut self) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        // The child leads its own session: stop the whole tree, not only the
        // root, so a `sh -c '...; sleep'` leaves no orphan holding the pipes.
        rightkit_child_process::tokio_support::terminate_tree(
            &mut self.child,
            rightkit_child_process::TerminationOptions::default()
                .with_grace(std::time::Duration::from_secs(2)),
        )
        .await
        .map(|_| ())
        .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::KillFailed)
    }
}

#[cfg(all(feature = "host", target_os = "macos"))]
#[async_trait::async_trait]
impl rightkit_sandbox::execution::ProcessLaunchAdapter for NativeCommandSandboxAdapter {
    async fn spawn(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        let root = std::path::PathBuf::from(&spec.context.workspace_root);
        if spec.context.sandbox_required && spec.context.sandbox_profile.is_none() {
            return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
        }
        let mut enforced = false;
        let mut network_enforced = false;
        let starts_from_empty_env = spec.starts_from_empty_env();
        let scrubbed_inherited = crate::process_launch::inherited_env_names_to_remove(&spec);
        // With `set_honour_disabled_sandbox_tier` an operator-disabled tier runs unconfined, as on
        // Linux and Windows; without it the launch is refused here.
        let honoured_tier = crate::shell::honour_disabled_sandbox_tier()
            .then(|| crate::shell::native_launch_tier(&root, spec.context.network_required));
        let (program, argv) = if spec.context.sandbox_required
            && !honoured_tier.is_some_and(|tier| tier.is_disabled())
        {
            let tier = honoured_tier.unwrap_or_else(|| {
                if spec.context.network_required {
                    crate::sandbox::SandboxTier::WorkspaceWrite
                } else {
                    crate::shell::effective_sandbox_tier(&root, None)
                }
            });
            let wrapped =
                crate::shell::macos_sandbox_wrap_for_tier(&spec.program, &spec.argv, &root, tier)
                    .ok_or(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing)?;
            enforced = true;
            network_enforced = tier.denies_network();
            wrapped
        } else {
            (spec.program, spec.argv)
        };
        let mut command = rightkit_child_process::tokio_support::command(program);
        command
            .args(argv)
            .current_dir(if spec.cwd.trim().is_empty() {
                root
            } else {
                std::path::PathBuf::from(&spec.cwd)
            })
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if starts_from_empty_env {
            command.env_clear();
        }
        command.envs(spec.env.clone());
        // No scrubbed variable reaches the child by inheritance (empty unless the host turned on
        // `set_scrub_inherited_credentials`).
        for name in scrubbed_inherited {
            command.env_remove(name);
        }
        // Own session, so cancel can stop the whole tree (see `cancel`).
        rightkit_child_process::tokio_support::lead_session(&mut command);
        // macOS: drops DYLD_INSERT_LIBRARIES only, and only when the host turned on
        // `set_harden_children` (see `harden_child`).
        crate::process_launch::harden_child(command.as_std_mut());
        // With `set_anchor_children_to_parent` the child's group is tied to the host (macOS has no
        // parent-death signal; the watchdog is armed by `spawn_anchored`).
        let anchored = crate::shell::anchor_children_to_parent();
        let spawned = if anchored {
            parent_death::spawn_anchored(command).await
        } else {
            command.spawn()
        };
        spawned
            .map(|child| {
                Box::new(MacManagedChild {
                    child,
                    enforced,
                    network_enforced,
                }) as _
            })
            .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::SpawnFailed)
    }
    async fn spawn_channel(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
        channel: rightkit_sandbox::execution::ExecutionChannel,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        match channel {
            rightkit_sandbox::execution::ExecutionChannel::Pipe => self.spawn(spec).await,
            rightkit_sandbox::execution::ExecutionChannel::Pty(_) => {
                <crate::process_launch::LocalProcessLaunchAdapter as rightkit_sandbox::execution::ProcessLaunchAdapter>::spawn_channel(
                    &crate::process_launch::LocalProcessLaunchAdapter,
                    spec,
                    channel,
                )
                .await
            }
        }
    }
}

#[cfg(feature = "host")]
impl rightkit_sandbox::execution::CommandSandboxAdapter for NativeCommandSandboxAdapter {
    fn prepare(
        &self,
        program: &str,
        argv: &[String],
        context: &rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Result<
        rightkit_sandbox::execution::PreparedProcessLaunch,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        if program.is_empty() || context.workspace_root.is_empty() {
            return Err(
                rightkit_sandbox::execution::EffectExecutionError::InvalidRequest {
                    field: "launch".into(),
                    reason: "program and workspace are required".into(),
                },
            );
        }
        if context.sandbox_required && context.sandbox_profile.is_none() {
            return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
        }
        Ok(rightkit_sandbox::execution::PreparedProcessLaunch {
            program: program.to_string(),
            argv: argv.to_vec(),
            backend: if context.sandbox_required {
                "rightkit-agent-native-policy".into()
            } else {
                "trusted-internal".into()
            },
        })
    }
}

fn selected_execution_backend(ctx: &ToolContext) -> ShellExecutionBackend<'_> {
    let Some(backend) = ctx
        .execution_backend
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return ShellExecutionBackend::Local;
    };

    // Check the registry first if available.
    if let Some(registry) = &ctx.execution_backend_registry {
        if let Some(config) = registry.get(backend) {
            if !config.enabled {
                return ShellExecutionBackend::Unsupported(backend);
            }
            match config.kind {
                rightkit_sandbox::execution::BackendKind::Local => {
                    return ShellExecutionBackend::Local
                }
            }
        }
    }

    // Fallback: hardcoded name matching.
    if backend.eq_ignore_ascii_case("local") {
        return ShellExecutionBackend::Local;
    }
    ShellExecutionBackend::Unsupported(backend)
}

/// Hard ceiling on bytes retained per stream for the final tool result. The
/// pipe is always drained to EOF (a full pipe would stall the child), but bytes
/// past the cap are counted and discarded, never buffered.
const STREAM_INGRESS_CAP_BYTES: usize = 64 * 1024 * 1024;
/// With `set_shell_head_tail_capture`: the ceiling is the first half and the last half of this
/// many bytes. It sits just under the 8 MiB recovery copy, so the copy keeps the whole capture,
/// end included.
const STREAM_HEAD_TAIL_CAP_BYTES: usize = 8 * 1024 * 1024 - 8 * 1024;
/// Live deltas are coalesced: flushed at this size or age, whichever is first.
const DELTA_FLUSH_BYTES: usize = 8 * 1024;
const DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(40);

/// Background reader that forwards coalesced chunks to the context's live
/// output notifier while accumulating a bounded copy for the final tool result.
/// Aborted on drop so an early return (timeout/cancel) never leaks the task.
struct StreamDrain(tokio::task::JoinHandle<Vec<u8>>);

/// What a foreground command's output readers report to the password-prompt handler: that the
/// output now ends in a prompt, and the secrets that must never appear in what they forward.
#[derive(Clone)]
struct DrainWatch {
    prompt_tx: tokio::sync::mpsc::UnboundedSender<()>,
    secrets: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// Bytes of recent output a prompt is looked for in.
const PROMPT_WINDOW_BYTES: usize = 256;

/// Whether `tail` (the last bytes a command printed) ends in a password prompt: a line that names a
/// password, passphrase or passcode and ends with a colon, with nothing after it but blanks. A
/// prompt that is followed by a newline is output, not a question.
fn ends_in_password_prompt(tail: &[u8]) -> bool {
    static PROMPT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PROMPT.get_or_init(|| {
        regex::Regex::new(r"(?i)(password|passphrase|passcode)[^\n]{0,80}:[ \t]*$")
            .expect("password prompt pattern is valid")
    });
    pattern.is_match(&String::from_utf8_lossy(tail))
}

/// `bytes` with every supplied secret replaced by `<redacted>`.
fn redact_secret_bytes(bytes: &mut Vec<u8>, secrets: &std::sync::Mutex<Vec<String>>) {
    let secrets = secrets
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        let needle = secret.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index..].starts_with(needle) {
                out.extend_from_slice(b"<redacted>");
                index += needle.len();
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        *bytes = out;
    }
}

/// Redacted text for a message built from command output.
fn redact_secret_text(text: String, secrets: &std::sync::Mutex<Vec<String>>) -> String {
    let mut bytes = text.into_bytes();
    redact_secret_bytes(&mut bytes, secrets);
    String::from_utf8_lossy(&bytes).into_owned()
}

impl StreamDrain {
    fn spawn<R>(
        stream: Option<R>,
        kind: rightkit_agent_engine_protocol::ToolOutputStream,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
    ) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::spawn_capped(stream, kind, notifier, default_stream_cap(), None)
    }

    /// [`spawn`](Self::spawn) with a password-prompt watch and secret redaction.
    fn spawn_watched<R>(
        stream: Option<R>,
        kind: rightkit_agent_engine_protocol::ToolOutputStream,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
        watch: DrainWatch,
    ) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::spawn_capped(stream, kind, notifier, default_stream_cap(), Some(watch))
    }

    fn spawn_capped<R>(
        stream: Option<R>,
        kind: rightkit_agent_engine_protocol::ToolOutputStream,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
        cap: usize,
        watch: Option<DrainWatch>,
    ) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        Self(tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut all = Vec::new();
            let Some(mut stream) = stream else {
                return all;
            };
            // With head/tail capture `all` keeps the head and `tail` rolls over the most recent bytes
            // (a failing build's error is at the end); otherwise `all` keeps the head up to `cap`.
            let head_tail = shell_head_tail_capture();
            let half = if head_tail { cap / 2 } else { cap };
            let mut tail: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
            let live = notifier.is_set();
            let mut carry: Vec<u8> = Vec::new();
            let mut dropped: u64 = 0;
            let mut last_flush = tokio::time::Instant::now();
            let mut buf = [0u8; 4096];
            // The last bytes read, for the password-prompt watch, and how many bytes were read when
            // the last prompt was reported (one report per prompt).
            let mut window: Vec<u8> = Vec::new();
            let mut total_read: u64 = 0;
            let mut reported_at: u64 = 0;
            loop {
                // With pending live bytes, wait only until the flush deadline
                // so a stalled producer never strands the tail of a delta.
                let read = if live && !carry.is_empty() {
                    let deadline = last_flush + DELTA_FLUSH_INTERVAL;
                    tokio::time::timeout_at(deadline, stream.read(&mut buf))
                        .await
                        .ok()
                } else {
                    Some(stream.read(&mut buf).await)
                };
                match read {
                    Some(Ok(0)) | Some(Err(_)) => break,
                    Some(Ok(n)) => {
                        if let Some(watch) = &watch {
                            total_read += n as u64;
                            window.extend_from_slice(&buf[..n]);
                            if window.len() > PROMPT_WINDOW_BYTES {
                                let excess = window.len() - PROMPT_WINDOW_BYTES;
                                window.drain(..excess);
                            }
                            if total_read != reported_at && ends_in_password_prompt(&window) {
                                reported_at = total_read;
                                let _ = watch.prompt_tx.send(());
                            }
                        }
                        let room = half.saturating_sub(all.len());
                        let keep = n.min(room);
                        all.extend_from_slice(&buf[..keep]);
                        if head_tail {
                            tail.extend(&buf[keep..n]);
                            if tail.len() > cap - half {
                                let excess = tail.len() - (cap - half);
                                tail.drain(..excess);
                                dropped += excess as u64;
                            }
                        } else {
                            dropped += (n - keep) as u64;
                        }
                        if live {
                            carry.extend_from_slice(&buf[..n]);
                        }
                    }
                    None => {}
                }
                if live
                    && !carry.is_empty()
                    && (carry.len() >= DELTA_FLUSH_BYTES
                        || last_flush.elapsed() >= DELTA_FLUSH_INTERVAL)
                {
                    if let Some(watch) = &watch {
                        redact_secret_bytes(&mut carry, &watch.secrets);
                    }
                    notify_utf8(&notifier, kind, &mut carry);
                    last_flush = tokio::time::Instant::now();
                }
            }
            if live && !carry.is_empty() {
                if let Some(watch) = &watch {
                    redact_secret_bytes(&mut carry, &watch.secrets);
                }
                notifier.notify(kind, &String::from_utf8_lossy(&carry));
            }
            if dropped > 0 {
                let note = if head_tail {
                    format!(
                        "\n[output truncated: {dropped} bytes between the first {half} and the last {} bytes were discarded]\n",
                        cap - half
                    )
                } else {
                    format!(
                        "\n[output truncated: {dropped} bytes beyond the {cap}-byte capture limit were discarded]\n"
                    )
                };
                all.extend_from_slice(note.as_bytes());
            }
            all.extend(tail);
            all
        }))
    }

    async fn finish(mut self) -> Vec<u8> {
        (&mut self.0).await.unwrap_or_default()
    }
}

/// The per-stream capture ceiling in force.
fn default_stream_cap() -> usize {
    if shell_head_tail_capture() {
        STREAM_HEAD_TAIL_CAP_BYTES
    } else {
        STREAM_INGRESS_CAP_BYTES
    }
}

impl Drop for StreamDrain {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Emit the decodable prefix of `carry`, keeping an incomplete trailing
/// UTF-8 sequence for the next chunk.
fn notify_utf8(
    notifier: &rightkit_agent_engine_protocol::ToolOutputNotifier,
    kind: rightkit_agent_engine_protocol::ToolOutputStream,
    carry: &mut Vec<u8>,
) {
    loop {
        match std::str::from_utf8(carry) {
            Ok(text) => {
                notifier.notify(kind, text);
                carry.clear();
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    notifier.notify(kind, &String::from_utf8_lossy(&carry[..valid]));
                }
                match e.error_len() {
                    None => {
                        carry.drain(..valid);
                        return;
                    }
                    Some(bad) => {
                        notifier.notify(kind, "\u{FFFD}");
                        carry.drain(..valid + bad);
                        if carry.is_empty() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Append the typed, reaped exit status to a foreground result message. The
/// call sites run it only after the child was waited on, so the status is
/// confirmed rather than assumed.
fn with_exit_status(
    message: String,
    status: rightkit_agent_engine_protocol::ShellExitStatus,
) -> String {
    format!("{message}\n[status: {}]", status.label())
}

/// How long a command's stdin stays open for a password prompt that has not come yet, and again
/// after each answered prompt. Past it the pipe is closed, so a command that reads stdin to its
/// end sees EOF after this long, not at once.
const PASSWORD_STDIN_IDLE_GRACE: Duration = Duration::from_secs(3);
/// A prompt is acted on only after output has been quiet this long and the command is still
/// running, so a line that merely ends in `password:` does not open a card.
const PASSWORD_PROMPT_SETTLE: Duration = Duration::from_millis(400);

/// The time a command spent waiting for its operator to answer a password prompt, so the
/// command's timeout does not run while a person is typing.
#[derive(Default)]
struct PromptClock {
    completed_ms: std::sync::atomic::AtomicU64,
    taken_ms: std::sync::atomic::AtomicU64,
    since: std::sync::Mutex<Option<std::time::Instant>>,
}

impl PromptClock {
    fn begin_wait(&self) {
        *self.since.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::time::Instant::now());
    }

    fn end_wait(&self) {
        if let Some(start) = self.since.lock().unwrap_or_else(|p| p.into_inner()).take() {
            self.completed_ms.fetch_add(
                start.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }

    /// Waiting time not yet handed back, including a wait still going on.
    fn take_waited(&self) -> Duration {
        use std::sync::atomic::Ordering::SeqCst;
        let current = self
            .since
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map_or(0, |start| start.elapsed().as_millis() as u64);
        let total = self.completed_ms.load(SeqCst) + current;
        let before = self.taken_ms.swap(total, SeqCst);
        Duration::from_millis(total.saturating_sub(before))
    }
}

/// Resolves when a command has run for `timeout_ms` not counting the time its operator spent
/// answering password prompts. Without a clock it is a plain sleep.
async fn command_deadline(timeout_ms: u64, clock: Option<std::sync::Arc<PromptClock>>) {
    let mut allowed = Duration::from_millis(timeout_ms);
    loop {
        tokio::time::sleep(allowed).await;
        match clock.as_ref().map(|clock| clock.take_waited()) {
            Some(extra) if !extra.is_zero() => allowed = extra,
            _ => return,
        }
    }
}

/// The password-prompt handler of one foreground command: a task that, whenever the output ends
/// in a prompt, asks the operator through the masked credential card, writes the answer and a
/// newline to the command's stdin and remembers the secret so it is redacted from everything the
/// command prints afterwards. Aborted when dropped.
struct PasswordPrompts {
    task: tokio::task::JoinHandle<()>,
    watch: DrainWatch,
    clock: std::sync::Arc<PromptClock>,
    exited: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PasswordPrompts {
    fn start(
        requester: std::sync::Arc<dyn crate::context::CredentialRequester>,
        tool_call_id: String,
        mut stdin: tokio::process::ChildStdin,
    ) -> Self {
        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let secrets = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let clock = std::sync::Arc::new(PromptClock::default());
        let exited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = {
            let (secrets, clock, exited) = (secrets.clone(), clock.clone(), exited.clone());
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                loop {
                    tokio::select! {
                        prompt = prompt_rx.recv() => {
                            if prompt.is_none() {
                                break;
                            }
                            tokio::time::sleep(PASSWORD_PROMPT_SETTLE).await;
                            if exited.load(std::sync::atomic::Ordering::SeqCst) {
                                break;
                            }
                            while prompt_rx.try_recv().is_ok() {}
                            clock.begin_wait();
                            let answer = requester
                                .request(
                                    tool_call_id.clone(),
                                    crate::context::CredentialPrompt {
                                        provider: "shell password".to_string(),
                                        reason: "A command is asking for a password.".to_string(),
                                    },
                                )
                                .await;
                            clock.end_wait();
                            match answer {
                                Ok(Some(secret)) => {
                                    let value = secret.expose().to_string();
                                    secrets
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .push(value.clone());
                                    let written = async {
                                        stdin.write_all(value.as_bytes()).await?;
                                        stdin.write_all(b"\n").await?;
                                        stdin.flush().await
                                    }
                                    .await;
                                    if written.is_err() {
                                        break;
                                    }
                                }
                                // Declined, cancelled or nobody to ask: the command sees EOF.
                                Ok(None) | Err(_) => break,
                            }
                        }
                        _ = tokio::time::sleep(PASSWORD_STDIN_IDLE_GRACE) => break,
                    }
                }
                drop(stdin);
            })
        };
        Self {
            task,
            watch: DrainWatch { prompt_tx, secrets },
            clock,
            exited,
        }
    }

    fn mark_exited(&self) {
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for PasswordPrompts {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn spawn_foreground_drain<R>(
    stream: Option<R>,
    kind: rightkit_agent_engine_protocol::ToolOutputStream,
    ctx: &ToolContext,
    watch: &Option<DrainWatch>,
) -> StreamDrain
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    match watch {
        Some(watch) => {
            StreamDrain::spawn_watched(stream, kind, ctx.output_notifier.clone(), watch.clone())
        }
        None => StreamDrain::spawn(stream, kind, ctx.output_notifier.clone()),
    }
}

/// How much of each stream a killed command reports back to the model.
const KILLED_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// With `set_shell_report_output_on_kill`: a command killed by its timeout or a cancel still
/// produced output the model needs to see (the failing test, the prompt it hung on). The tree is
/// already terminated, so the drains get [`POST_EXIT_DRAIN_GRACE`] to finish; the tail of each
/// stream is appended to the kill message.
async fn append_partial_output(
    mut message: String,
    stdout_drain: StreamDrain,
    stderr_drain: StreamDrain,
) -> String {
    let drains = async move {
        let stdout = stdout_drain.finish().await;
        let stderr = stderr_drain.finish().await;
        (stdout, stderr)
    };
    let Ok((stdout, stderr)) = tokio::time::timeout(POST_EXIT_DRAIN_GRACE, drains).await else {
        message.push_str("\n[output before the kill could not be collected]");
        return message;
    };
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        if bytes.is_empty() {
            continue;
        }
        let start = bytes.len().saturating_sub(KILLED_OUTPUT_TAIL_BYTES);
        let tail = String::from_utf8_lossy(&bytes[start..]);
        if start > 0 {
            message.push_str(&format!(
                "\n--- {label} before the kill (last {KILLED_OUTPUT_TAIL_BYTES} of {} bytes) ---\n",
                bytes.len()
            ));
        } else {
            message.push_str(&format!("\n--- {label} before the kill ---\n"));
        }
        message.push_str(tail.trim_end());
    }
    message
}

/// Appended to a timeout message with `set_shell_report_output_on_kill`: tells the model how to get
/// past a timeout instead of just reporting it.
const TIMEOUT_HINT: &str =
    "Raise timeout_ms (max 300000) or set run_in_background=true for long commands.";

/// What follows `(pid N)` in a timeout message.
fn timeout_message_suffix() -> String {
    if shell_report_output_on_kill() {
        format!(". {TIMEOUT_HINT}")
    } else {
        String::new()
    }
}

/// Appended to stderr when processes the command left running were stopped because they kept its
/// output open past the timeout or a cancel (`set_shell_stop_leftover_processes`).
fn leftover_processes_note() -> String {
    format!(
        "\n[{}: the command exited but processes it left running kept its output open; they were stopped at the timeout or cancel. Use run_in_background for long-running processes.]\n",
        shell_notice_label()
    )
}

/// How long the output pipes get to close after leftover processes were stopped.
const POST_EXIT_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Collect both output streams after the shell itself exited. A process it left running
/// (`server &`) can hold the pipes open indefinitely, so the wait ends at the command's remaining
/// timeout or an operator cancel; then `kill_leftovers` stops the tree and the drains get
/// [`POST_EXIT_DRAIN_GRACE`]. Returns `(stdout, stderr, killed)`.
async fn finish_drains_after_exit<K, KF>(
    stdout_drain: StreamDrain,
    stderr_drain: StreamDrain,
    remaining: Duration,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    kill_leftovers: K,
) -> (Vec<u8>, Vec<u8>, bool)
where
    K: FnOnce() -> KF,
    KF: std::future::Future<Output = ()>,
{
    let drains = async move {
        let stdout = stdout_drain.finish().await;
        let stderr = stderr_drain.finish().await;
        (stdout, stderr)
    };
    tokio::pin!(drains);
    tokio::select! {
        (stdout, stderr) = &mut drains => return (stdout, stderr, false),
        _ = tokio::time::sleep(remaining) => {}
        _ = cancel_requested(cancel_rx) => {}
    }
    kill_leftovers().await;
    match tokio::time::timeout(POST_EXIT_DRAIN_GRACE, &mut drains).await {
        Ok((stdout, stderr)) => (stdout, stderr, true),
        // A descendant that left the tree still holds the pipe; give up on its output rather
        // than hang (dropping the drains aborts them).
        Err(_) => (Vec::new(), Vec::new(), true),
    }
}

/// Resolves once the operator cancels; never resolves without a cancel channel or after its
/// sender is gone.
async fn cancel_requested(cancel_rx: Option<tokio::sync::watch::Receiver<bool>>) {
    let Some(mut rx) = cancel_rx else {
        return std::future::pending().await;
    };
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

async fn collect_child_output(
    mut child: tokio::process::Child,
    timeout_ms: u64,
    ctx: &ToolContext,
    cmd: &str,
) -> ToolResult {
    let mut cancel_rx_cloned = ctx.cancel_rx.clone();
    let started = tokio::time::Instant::now();
    let pid_hint = child.id();
    // A command whose stdin is a pipe (an attended session, `set_shell_password_prompts`) may ask
    // for a password.
    let prompts = if shell_password_prompts() {
        match (ctx.credential_requester.clone(), child.stdin.take()) {
            (Some(requester), Some(stdin)) => Some(PasswordPrompts::start(
                requester,
                ctx.tool_call_id.clone(),
                stdin,
            )),
            _ => None,
        }
    } else {
        None
    };
    let command_clock = prompts.as_ref().map(|prompts| prompts.clock.clone());
    let prompt_watch = prompts.as_ref().map(|prompts| prompts.watch.clone());

    let process_id = pid_hint.map(|p| p.to_string());
    #[cfg(windows)]
    let windows_job = match assign_windows_job_to_child(&child) {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let message = format!(
                "Failed to assign command to Windows Job Object (pid {:?}): {}",
                pid_hint, e
            );
            record_foreground_process(
                ctx,
                cmd,
                &message,
                crate::background_shell::BgStatus::Killed,
                process_id.clone(),
            );
            return ToolResult::error(message).with_process_id(process_id);
        }
    };

    let stdout_drain = spawn_foreground_drain(
        child.stdout.take(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
        ctx,
        &prompt_watch,
    );
    let stderr_drain = spawn_foreground_drain(
        child.stderr.take(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stderr,
        ctx,
        &prompt_watch,
    );
    let mut wait_status = None;
    if let Some(ref mut cancel_rx) = cancel_rx_cloned {
        let cancel_result = tokio::select! {
            _ = command_deadline(timeout_ms, command_clock.clone()) => {
                // Kill the whole process group, then reap to release the PID and
                // update the process-id column. Without the reap the OS can
                // leave a zombie until the next wait/kill on the same child.
                terminate_owned_tree(&mut child).await;
                let message = format!(
                    "Command timed out after {}ms: {} (pid {:?}){}",
                    timeout_ms,
                    cmd,
                    pid_hint,
                    timeout_message_suffix()
                );
                let message = with_exit_status(
                    message,
                    rightkit_agent_engine_protocol::ShellExitStatus::KilledByTimeout { timeout_ms },
                );
                record_foreground_process(
                    ctx,
                    &cmd,
                    &message,
                    crate::background_shell::BgStatus::Killed,
                    process_id.clone(),
                );
                Some(message)
            }
            _ = async {
                // A cancel that landed before this select (watch clones mark
                // the current value seen) must still fire.
                if *cancel_rx.borrow() {
                    return;
                }
                let _ = cancel_rx.changed().await;
            } => {
                terminate_owned_tree(&mut child).await;
                let message = format!(
                    "Command cancelled by operator (pid {:?}).",
                    pid_hint
                );
                let message = with_exit_status(
                    message,
                    rightkit_agent_engine_protocol::ShellExitStatus::KilledByCancel,
                );
                record_foreground_process(
                    ctx,
                    &cmd,
                    &message,
                    crate::background_shell::BgStatus::Killed,
                    process_id.clone(),
                );
                Some(message)
            }
            status = child.wait() => {
                wait_status = status.ok();
                None
            },
        };
        if let Some(message) = cancel_result {
            let message = if shell_report_output_on_kill() {
                append_partial_output(message, stdout_drain, stderr_drain).await
            } else {
                message
            };
            let message = match &prompt_watch {
                Some(watch) => redact_secret_text(message, &watch.secrets),
                None => message,
            };
            return ToolResult::error(message).with_process_id(process_id);
        }
    } else {
        let cancel_result = tokio::select! {
            _ = command_deadline(timeout_ms, command_clock.clone()) => {
                terminate_owned_tree(&mut child).await;
                let message = format!(
                    "Command timed out after {}ms: {} (pid {:?}){}",
                    timeout_ms,
                    cmd,
                    pid_hint,
                    timeout_message_suffix()
                );
                let message = with_exit_status(
                    message,
                    rightkit_agent_engine_protocol::ShellExitStatus::KilledByTimeout { timeout_ms },
                );
                record_foreground_process(
                    ctx,
                    &cmd,
                    &message,
                    crate::background_shell::BgStatus::Killed,
                    process_id.clone(),
                );
                Some(message)
            }
            status = child.wait() => {
                wait_status = status.ok();
                None
            },
        };
        if let Some(message) = cancel_result {
            let message = if shell_report_output_on_kill() {
                append_partial_output(message, stdout_drain, stderr_drain).await
            } else {
                message
            };
            let message = match &prompt_watch {
                Some(watch) => redact_secret_text(message, &watch.secrets),
                None => message,
            };
            return ToolResult::error(message).with_process_id(process_id);
        }
    }

    if let Some(prompts) = &prompts {
        prompts.mark_exited();
    }
    // Process exited normally — collect output. With `set_shell_stop_leftover_processes`, a
    // process the command left running can still hold the pipes, so the wait stays bounded by the
    // command's timeout and the operator's cancel.
    #[cfg(unix)]
    let kill_leftovers = move || async move {
        // The shell led its own session (`lead_session`), so its pid names the group; the id
        // stays valid while a leftover member lives.
        if let Some(pgid) = pid_hint.and_then(std::num::NonZeroU32::new) {
            let _ = rightkit_child_process::tokio_support::terminate_process_group(
                pgid,
                rightkit_child_process::TerminationOptions::default()
                    .with_grace(Duration::from_secs(2)),
            )
            .await;
        }
    };
    #[cfg(windows)]
    let kill_leftovers = move || async move {
        if let Some(job) = windows_job {
            let _ = job.terminate();
        }
    };
    #[cfg(not(any(unix, windows)))]
    let kill_leftovers = || async {};
    let (stdout_buf, mut stderr_buf) = if shell_stop_leftover_processes() {
        // A shell that exits right at its deadline still gets a moment for its own pipes to
        // reach EOF before anything is stopped.
        let remaining = Duration::from_millis(timeout_ms)
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(500));
        let (stdout_buf, mut stderr_buf, killed_leftovers) = finish_drains_after_exit(
            stdout_drain,
            stderr_drain,
            remaining,
            ctx.cancel_rx.clone(),
            kill_leftovers,
        )
        .await;
        if killed_leftovers {
            stderr_buf.extend_from_slice(leftover_processes_note().as_bytes());
        }
        (stdout_buf, stderr_buf)
    } else {
        (stdout_drain.finish().await, stderr_drain.finish().await)
    };
    let mut stdout_buf = stdout_buf;
    if let Some(watch) = &prompt_watch {
        redact_secret_bytes(&mut stdout_buf, &watch.secrets);
        redact_secret_bytes(&mut stderr_buf, &watch.secrets);
    }
    let output = std::process::Output {
        status: wait_status.unwrap_or_default(),
        stdout: stdout_buf,
        stderr: stderr_buf,
    };
    output_to_result(output, process_id, Some(ctx), &cmd)
}

#[cfg(windows)]
async fn collect_confined_child_output(
    mut child: crate::sandbox::ConfinedPipedChild,
    timeout_ms: u64,
    ctx: &ToolContext,
    cmd: &str,
) -> ToolResult {
    use std::os::windows::process::ExitStatusExt;

    let pid = child.pid();
    let process_id = Some(pid.to_string());
    // The confined spawn created the child suspended and already placed it in
    // a kill-on-close Job Object before its first instruction; take that job
    // instead of nesting a second one. Only a child without one (never
    // expected) is assigned here, and a failure refuses the command.
    let job = match child
        .take_job()
        .map(Ok)
        .unwrap_or_else(|| assign_windows_job_to_pid(pid))
    {
        Ok(job) => job,
        Err(error) => {
            let _ = child.terminate();
            let _ = child.wait_code().await;
            eprintln!(
                "[sandbox] confined command refused: pid {pid} has no Job Object and assignment failed: {error}"
            );
            let message = format!(
                "Failed to assign confined command to Windows Job Object (pid {pid}): {error}"
            );
            record_foreground_process(
                ctx,
                cmd,
                &message,
                crate::background_shell::BgStatus::Killed,
                process_id.clone(),
            );
            return ToolResult::error(message).with_process_id(process_id);
        }
    };

    let stdout_drain = StreamDrain::spawn(
        child.take_stdout(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
        ctx.output_notifier.clone(),
    );
    let stderr_drain = StreamDrain::spawn(
        child.take_stderr(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stderr,
        ctx.output_notifier.clone(),
    );
    let mut wait_code = None;
    let mut cancel_rx = ctx.cancel_rx.clone();
    let cancelled = tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            let _ = job.terminate();
            let message = format!(
                "Command timed out after {}ms: {} (pid {})",
                timeout_ms, cmd, pid
            );
                let message = with_exit_status(
                    message,
                    rightkit_agent_engine_protocol::ShellExitStatus::KilledByTimeout { timeout_ms },
                );
            record_foreground_process(
                ctx,
                cmd,
                &message,
                crate::background_shell::BgStatus::Killed,
                process_id.clone(),
            );
            Some(ToolResult::error(message).with_process_id(process_id.clone()))
        }
        _ = async {
            if let Some(cancel_rx) = cancel_rx.as_mut() {
                if *cancel_rx.borrow() {
                    return;
                }
                let _ = cancel_rx.changed().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            let _ = job.terminate();
            let message = format!("Command cancelled by operator (pid {pid}).");
            let message = with_exit_status(message, rightkit_agent_engine_protocol::ShellExitStatus::KilledByCancel);
            record_foreground_process(
                ctx,
                cmd,
                &message,
                crate::background_shell::BgStatus::Killed,
                process_id.clone(),
            );
            Some(ToolResult::error(message).with_process_id(process_id.clone()))
        }
        code = child.wait_code() => {
            wait_code = Some(code.unwrap_or(1));
            None
        }
    };
    drop(job);
    if let Some(result) = cancelled {
        let _ = child.wait_code().await;
        return result;
    }

    let stdout_buf = stdout_drain.finish().await;
    let stderr_buf = stderr_drain.finish().await;
    let output = std::process::Output {
        status: std::process::ExitStatus::from_raw(wait_code.unwrap_or(1)),
        stdout: stdout_buf,
        stderr: stderr_buf,
    };
    output_to_result(output, process_id, Some(ctx), cmd)
}

/// Staged kill of a shell child's whole tree (RightKit owns the mechanism: SIGTERM to
/// the child's group, a 2 s grace, SIGKILL; Windows sweeps by process identity), then
/// reap. A failure to reap is not actionable at the call sites, which report the
/// timeout/cancel regardless.
async fn terminate_owned_tree(child: &mut tokio::process::Child) {
    let _ = rightkit_child_process::tokio_support::terminate_tree(
        child,
        rightkit_child_process::TerminationOptions::default()
            .with_grace(std::time::Duration::from_secs(2)),
    )
    .await;
}

#[cfg(windows)]
fn assign_windows_job_to_child(
    child: &tokio::process::Child,
) -> std::io::Result<Option<crate::sandbox::WindowsJob>> {
    let Some(pid) = child.id() else {
        return Ok(None);
    };
    Ok(Some(assign_windows_job_to_pid(pid)?))
}

#[cfg(windows)]
pub(crate) fn assign_windows_job_to_pid(pid: u32) -> std::io::Result<crate::sandbox::WindowsJob> {
    let job = crate::sandbox::shell_job()?;
    job.assign_process_id(pid)?;
    Ok(job)
}

#[cfg(windows)]
pub(crate) fn spawn_windows_appcontainer_with_grant(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    with_stdin: bool,
    network_allowed: bool,
    grant: Option<&crate::sandbox::SandboxGrant>,
) -> std::io::Result<crate::sandbox::ConfinedPipedChild> {
    let profile = crate::sandbox::AppContainerProfile::for_workspace(workspace_root)?
        .with_network(network_allowed || grant.is_some_and(|grant| grant.network));
    if let Some(grant) = grant {
        for path in &grant.read_paths {
            profile.lease_extra_access(path, false)?;
        }
        for path in &grant.write_paths {
            profile.lease_extra_access(path, true)?;
        }
    }
    crate::sandbox::spawn_confined_piped(&profile, program, args, workspace_root, with_stdin)
}

fn record_foreground_process(
    ctx: &ToolContext,
    command: &str,
    output: &str,
    status: crate::background_shell::BgStatus,
    process_id: Option<String>,
) {
    if let Some(store) = &ctx.background_shells {
        crate::background_shell::record_foreground_process(
            process_id,
            command.to_string(),
            output.to_string(),
            status,
            store,
            execution_target_from_context(ctx),
        );
    }
}

fn execution_target_from_context(ctx: &ToolContext) -> crate::background_shell::BgExecutionTarget {
    crate::background_shell::BgExecutionTarget {
        backend_id: ctx.execution_backend.clone(),
        workspace_id: ctx.execution_workspace_id.clone(),
        network_profile: ctx.execution_network_profile.clone(),
    }
}

pub(crate) fn sandbox_tier_for_workspace(workspace_root: &Path) -> crate::sandbox::SandboxTier {
    sandbox_policy_for_workspace(workspace_root).effective
}

/// The tier to actually run under, given the session's permission mode.
///
/// The workspace/operator policy sets the ceiling. `Bypass` — the mode the
/// composer labels **Full access** — lowers it to filesystem isolation only.
///
/// Without this the two disagreed in the worst possible direction: the UI said
/// "Full access" while every command still ran under the Strict profile's
/// `(deny network-outbound)`, because the tier was resolved from the workspace
/// file and operator env alone and the mode was never consulted. `ssh`, `git
/// push`, `curl` and any LAN bridge failed with `Operation not permitted` — an
/// OS-level EPERM that reads like a broken network rather than a policy we
/// applied. Claiming an access level we then silently refuse is worse than
/// either honouring it or not offering it.
///
/// `Bypass` is documented as "everything allowed — only for an explicit sandbox
/// or disposable worktree", so honouring it here is the mode working as
/// specified, per session, chosen deliberately. Every other mode keeps the
/// resolved tier untouched, so the default stays Strict.
pub fn effective_sandbox_tier(
    workspace_root: &Path,
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
) -> crate::sandbox::SandboxTier {
    let resolved = sandbox_tier_for_workspace(workspace_root);
    if permission_mode == Some(rightkit_agent_policy::PermissionMode::Bypass) {
        return if resolved.is_disabled() {
            resolved
        } else {
            crate::sandbox::SandboxTier::WorkspaceWrite
        };
    }
    resolved
}

/// The tier a [`NativeCommandSandboxAdapter`] launch runs under when the host turned on
/// `set_honour_disabled_sandbox_tier`, one rule on every OS:
/// - an operator-disabled tier stays disabled: the launch runs unconfined, as the foreground and
///   background shells already do;
/// - a network-requiring launch keeps filesystem confinement but never the Strict network denial
///   (WorkspaceWrite);
/// - otherwise the workspace's resolved tier applies.
///
/// Callers derive network access from the returned tier (`!tier.denies_network()`), never from
/// `network_required` alone, so a WorkspaceWrite workspace has network on Windows as it does
/// elsewhere.
#[cfg_attr(not(feature = "host"), allow(dead_code))]
pub(crate) fn native_launch_tier(
    workspace_root: &Path,
    network_required: bool,
) -> crate::sandbox::SandboxTier {
    native_launch_tier_from(sandbox_tier_for_workspace(workspace_root), network_required)
}

#[cfg_attr(not(feature = "host"), allow(dead_code))]
pub(crate) fn native_launch_tier_from(
    resolved: crate::sandbox::SandboxTier,
    network_required: bool,
) -> crate::sandbox::SandboxTier {
    if resolved.is_disabled() {
        resolved
    } else if network_required {
        crate::sandbox::SandboxTier::WorkspaceWrite
    } else {
        resolved
    }
}

pub(crate) fn sandbox_policy_for_workspace(
    workspace_root: &Path,
) -> rightkit_agent_policy::SandboxPolicyResolution {
    let operator = sandbox_operator_tier_value();
    let workspace = sandbox_tier_value_from_workspace_file(workspace_root);
    rightkit_agent_policy::resolve_sandbox_policy(operator.as_deref(), workspace.as_deref())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn sandbox_disabled_for_workspace(workspace_root: &Path) -> bool {
    sandbox_tier_for_workspace(workspace_root).is_disabled()
}

/// Whether every shell command in `workspace_root` runs under an enforcing OS sandbox: the
/// tier is not disabled on a platform with a sandbox backend. The shell refuses to run when
/// the sandbox cannot start, so `true` means "confined or not run at all".
pub fn os_sandbox_enforced(workspace_root: &Path) -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )) && !sandbox_disabled_for_workspace(workspace_root)
}

/// The sandbox configuration for `workspace_root` at its current tier. Linux only: the other
/// launch paths pick their tier themselves.
#[cfg(target_os = "linux")]
pub(crate) fn local_sandbox_config(workspace_root: &Path) -> SandboxConfig {
    local_sandbox_config_for_tier(workspace_root, sandbox_tier_for_workspace(workspace_root))
}

/// The sandbox configuration for `workspace_root` at `tier`, with no rerun grant. Compiled only
/// where a caller exists (Linux launches and the macOS streamed PTY); Windows uses
/// `local_sandbox_config_for_tier_with_grant` through the shell tool.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn local_sandbox_config_for_tier(
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
) -> SandboxConfig {
    local_sandbox_config_for_tier_with_grant(workspace_root, tier, None)
}

/// `local_sandbox_config_for_tier` widened by the one digest-bound rerun grant the engine
/// issued after the operator chose "Approve rerun" (`ToolContext::sandbox_retry_grant`, bound to
/// the approved payload digest by the engine's tool dispatch). The shell tool passes it only with
/// `set_sandbox_rerun_grant_unix`. This is the Linux counterpart of the Windows grant applied in
/// the foreground shell (`spawn_windows_appcontainer_with_grant`).
pub(crate) fn local_sandbox_config_for_tier_with_grant(
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
    grant: Option<&crate::sandbox::SandboxGrant>,
) -> SandboxConfig {
    local_sandbox_config_with_grant_and_home(
        workspace_root,
        tier,
        grant,
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
    )
}

/// Whether a backgrounded command may use the network: the same decision the foreground shell
/// makes from the session's permission mode, so Bypass ("Full access") applies whether or not
/// `run_in_background` is set. Used with `set_shell_background_follows_session`.
pub(crate) fn background_network_allowed(ctx: &ToolContext) -> bool {
    background_network_allowed_for(&ctx.workspace_root, ctx.sandbox_permission_mode())
}

pub(crate) fn background_network_allowed_for(
    workspace_root: &Path,
    permission_mode: rightkit_agent_policy::PermissionMode,
) -> bool {
    !effective_sandbox_tier(workspace_root, Some(permission_mode)).denies_network()
}

/// Resolve one rerun-grant path to the existing, canonical path the OS sandbox can grant.
///
/// Landlock (`add_rule` opens the path with `O_PATH`) and Seatbelt can only grant an existing
/// inode, so a grant on a not-yet-created file would silently grant nothing. RightKit extracts
/// the denied path from the shell's error, which is exactly such a file, so a missing path falls
/// back to its parent directory (one level only). That widens a file grant to the whole
/// directory for the one approved rerun. Refusals mirror the Windows `lease_extra_access_in`:
/// a filesystem root, a protected secret, a path containing secrets, or (here) the home
/// directory or one of its ancestors.
///
/// With `create_missing` (write grants only), a missing parent folder is created after approval
/// instead: see `create_grant_folder`. The new folder then passes the same refusals as above.
fn grantable_path(
    path: &Path,
    home: Option<&Path>,
    create_missing: bool,
) -> Option<std::path::PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let chosen: std::path::PathBuf = if std::fs::symlink_metadata(path).is_ok() {
        path.to_path_buf()
    } else if create_missing {
        create_grant_folder(path)?
    } else {
        path.parent()?.to_path_buf()
    };
    let canonical = match std::fs::canonicalize(&chosen) {
        Ok(canonical) => canonical,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "rerun grant path skipped: does not resolve");
            return None;
        }
    };
    let refusal = if canonical.parent().is_none() {
        Some("filesystem root")
    // The secret checks run on the path as written and as resolved, so a symlinked or
    // non-canonical home (macOS `/var` vs `/private/var`) cannot slip a secret past either.
    } else if crate::sandbox::path_is_protected_secret(&canonical)
        || crate::sandbox::path_is_protected_secret(&chosen)
    {
        Some("protected secret location")
    } else if !crate::sandbox::secret_paths_within(&canonical).is_empty()
        || !crate::sandbox::secret_paths_within(&chosen).is_empty()
    {
        Some("contains protected secrets")
    } else if home.is_some_and(|home| {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        home.starts_with(&canonical)
    }) {
        Some("home directory or an ancestor of it")
    } else {
        None
    };
    if let Some(reason) = refusal {
        tracing::warn!(path = %path.display(), granted = %canonical.display(), reason, "rerun grant path refused");
        return None;
    }
    Some(canonical)
}

/// Most missing folder levels a rerun grant may create.
const MAX_CREATED_GRANT_DEPTH: usize = 8;

/// Create the missing parent folder of a not-yet-existing grant path and return it. The last
/// component of `path` is the file itself and is never created. The nearest existing ancestor
/// must be reached without crossing a symlink below the top level: a link the agent planted in
/// the workspace (`ws/link -> ~/.config`) would otherwise make the folder appear somewhere the
/// approval card never showed. Top-level links such as macOS `/tmp -> /private/tmp` belong to
/// root and are allowed. The ancestor must not be a root, a protected secret or contain
/// secrets, and at most `MAX_CREATED_GRANT_DEPTH` plain components may be created.
fn create_grant_folder(path: &Path) -> Option<std::path::PathBuf> {
    let refuse = |reason: &str| {
        tracing::warn!(path = %path.display(), reason, "rerun grant path refused");
    };
    let mut ancestor = path.parent()?;
    let mut remaining = Vec::new();
    while std::fs::symlink_metadata(ancestor).is_err() {
        remaining.push(ancestor.components().next_back()?);
        if remaining.len() > MAX_CREATED_GRANT_DEPTH {
            refuse("too many missing folder levels");
            return None;
        }
        ancestor = ancestor.parent()?;
    }
    let crosses_symlink = ancestor
        .ancestors()
        .filter(|prefix| prefix.components().count() > 2)
        .any(|prefix| {
            std::fs::symlink_metadata(prefix).is_ok_and(|meta| meta.file_type().is_symlink())
        });
    if crosses_symlink {
        refuse("existing part of the path goes through a symbolic link");
        return None;
    }
    if !remaining
        .iter()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        refuse("missing folder path has a non-plain component");
        return None;
    }
    remaining.reverse();
    let canonical_ancestor = match std::fs::canonicalize(ancestor) {
        Ok(canonical) => canonical,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "rerun grant path skipped: does not resolve");
            return None;
        }
    };
    if canonical_ancestor.parent().is_none() {
        refuse("filesystem root");
        return None;
    }
    if crate::sandbox::path_is_protected_secret(&canonical_ancestor) {
        refuse("protected secret location");
        return None;
    }
    if !crate::sandbox::secret_paths_within(&canonical_ancestor).is_empty() {
        refuse("contains protected secrets");
        return None;
    }
    let target = remaining.iter().fold(canonical_ancestor, |dir, component| {
        dir.join(component.as_os_str())
    });
    // Refuse before creating anything, so a refused grant leaves no empty folders behind. The
    // target does not exist yet, so it cannot be home or an ancestor of it.
    if crate::sandbox::path_is_protected_secret(&target) {
        refuse("protected secret location");
        return None;
    }
    if let Err(error) = std::fs::create_dir_all(&target) {
        tracing::warn!(path = %path.display(), %error, "rerun grant folder could not be created");
        return None;
    }
    Some(target)
}

/// Character devices a confined child may open for writing. Each is a device node, so a write grant
/// reaches no file. `/dev/tty` is deliberately absent: confined shells run in their own session
/// with no controlling terminal, and a terminal grant would let a child write escape sequences
/// to the operator's terminal.
#[cfg(target_os = "linux")]
const LINUX_WRITE_DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/full"];
#[cfg(target_os = "linux")]
const LINUX_READ_DEVICES: &[&str] = &["/dev/urandom", "/dev/random"];
/// System files a confined child may read. Specific files only: `/etc`, `/proc` and `/sys` stay closed.
#[cfg(target_os = "linux")]
const LINUX_SYSTEM_READ_FILES: &[&str] = &[
    "/etc/gitconfig",
    "/etc/alternatives",
    "/etc/ld.so.cache",
    "/etc/passwd",
    "/etc/group",
    "/etc/nsswitch.conf",
    "/etc/hosts",
    "/etc/resolv.conf",
    "/etc/localtime",
];
/// Per-user git settings, read file by file. Never the `~/.config/git` directory: git keeps its
/// `credentials` store there, and `~/.git-credentials` is never granted.
#[cfg(target_os = "linux")]
const LINUX_HOME_READ_FILES: &[&str] = &[
    ".gitconfig",
    ".config/git/config",
    ".config/git/ignore",
    ".config/git/attributes",
];

/// Linux only: whether the Strict shell withholds the workspace secret files, from the host option
/// (`shell_withhold_secrets`) or from the
/// environment variable `<prefix>_SHELL_WITHHOLD_SECRETS` set to exactly `1`.
#[cfg(target_os = "linux")]
fn withhold_secrets_enabled() -> bool {
    shell_withhold_secrets()
        || std::env::var_os(rightkit_agent_config::env_name("SHELL_WITHHOLD_SECRETS"))
            .is_some_and(|value| value == "1")
}

/// Linux only: workspace-internal paths the file tools withhold from reads (`.env`, `*.pem`,
/// the host's runtime folder), found by a bounded walk that does not follow symlinks. A matching
/// entry is recorded and not descended into. Bounds, by policy: `.git`, `node_modules` and
/// `target` are skipped, nothing deeper than 8 folders is withheld, and the walk stops after
/// 20 000 entries with a warning (failing closed there would refuse every command in a large
/// monorepo). A secret beyond these bounds stays readable to the shell; the file tools still
/// deny it. macOS and Windows are unaffected.
#[cfg(target_os = "linux")]
fn linux_withheld_reads(workspace_root: &Path) -> Vec<std::path::PathBuf> {
    const MAX_DEPTH: usize = 8;
    const MAX_ENTRIES: usize = 20_000;
    let root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut withheld = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                tracing::warn!(
                    root = %root.display(),
                    limit = MAX_ENTRIES,
                    "workspace secret-glob walk stopped at the entry cap"
                );
                return withheld;
            }
            // `file_type` does not follow symlinks, so a symlinked dir is never descended.
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let name = entry.file_name();
            if is_dir && matches!(name.to_str(), Some(".git" | "node_modules" | "target")) {
                continue;
            }
            let absolute = entry.path();
            let Ok(relative) = absolute.strip_prefix(&root) else {
                continue;
            };
            if rightkit_agent_policy::deny_globs::shell_withholds_read(relative, is_dir) {
                withheld.push(absolute);
            } else if is_dir && depth < MAX_DEPTH {
                stack.push((absolute, depth + 1));
            }
        }
    }
    withheld
}

pub(crate) fn local_sandbox_config_with_grant_and_home(
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
    grant: Option<&crate::sandbox::SandboxGrant>,
    home: Option<&Path>,
) -> SandboxConfig {
    let config = SandboxConfig::new()
        .allow_read(workspace_root)
        .allow_write(workspace_root)
        .allow_read("/usr")
        .allow_read("/lib")
        .allow_read("/lib64")
        .allow_read("/bin")
        .allow_read("/etc/ssl");
    // The rerun grant adds only paths `grantable_path` accepts. A write path also needs read.
    let config = match grant {
        Some(grant) => {
            let config = grant
                .read_paths
                .iter()
                .filter_map(|path| grantable_path(path, home, false))
                .fold(config, |config, path| config.allow_read(path));
            grant
                .write_paths
                .iter()
                .filter_map(|path| grantable_path(path, home, true))
                .fold(config, |config, path| {
                    config.allow_read(&path).allow_write(path)
                })
        }
        None => config,
    };
    // Linux Landlock denies everything it does not list, and git, ssh and most shells open
    // `/dev/null` read-write. Git treats an unreadable config file as fatal (EACCES, not
    // "missing"), and tools resolve users, hosts and alternatives through the `/etc` files.
    #[cfg(target_os = "linux")]
    let config = {
        let config = LINUX_WRITE_DEVICES
            .iter()
            .fold(config, |config, device| config.allow_write(*device));
        let config = LINUX_READ_DEVICES
            .iter()
            .fold(config, |config, device| config.allow_read(*device));
        let config = LINUX_SYSTEM_READ_FILES
            .iter()
            .fold(config, |config, file| config.allow_read(*file));
        home.into_iter()
            .flat_map(|home| {
                LINUX_HOME_READ_FILES
                    .iter()
                    .map(move |file| home.join(file))
            })
            .fold(config, |config, path| config.allow_read(path))
    };
    #[cfg(not(target_os = "linux"))]
    let _ = home;
    // Opt-in (`shell_withhold_secrets`): Strict also withholds the workspace-internal secret files
    // the file tools withhold. Off by default because Landlock cannot deny beneath an allowed
    // folder: every folder on the path to a withheld file loses read and execute, so files a
    // command creates there cannot be read back by that command (a first cargo build in a fresh
    // clone, `echo x > f && cat f`).
    #[cfg(target_os = "linux")]
    let config =
        if matches!(tier, crate::sandbox::SandboxTier::Strict) && withhold_secrets_enabled() {
            linux_withheld_reads(workspace_root)
                .into_iter()
                .fold(config, |config, path| config.withhold_read(path))
        } else {
            config
        };
    match tier {
        // RightKit's seccomp allowlist is network-denying, so a network rerun grant runs
        // Landlock-only, the same as Bypass and WorkspaceWrite.
        crate::sandbox::SandboxTier::Strict if !grant.is_some_and(|grant| grant.network) => {
            config.with_seccomp().deny_network()
        }
        _ => config,
    }
}

/// macOS Seatbelt wrapper: rewrite `(program, args)` to run under
/// `sandbox-exec -p <profile>` unless the tier is disabled. Returns `None` when
/// no wrapping should happen (tier `none`). `grant` is the one digest-bound rerun grant.
#[cfg(target_os = "macos")]
pub(crate) fn macos_sandbox_wrap(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
    grant: Option<&crate::sandbox::SandboxGrant>,
) -> Option<(String, Vec<String>)> {
    let tier = effective_sandbox_tier(workspace_root, permission_mode);
    macos_sandbox_wrap_for_tier_with_grant(program, args, workspace_root, tier, grant)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_sandbox_wrap_for_tier(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
) -> Option<(String, Vec<String>)> {
    macos_sandbox_wrap_for_tier_with_grant(program, args, workspace_root, tier, None)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_sandbox_wrap_for_tier_with_grant(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
    grant: Option<&crate::sandbox::SandboxGrant>,
) -> Option<(String, Vec<String>)> {
    if tier.is_disabled() {
        return None;
    }
    let config = local_sandbox_config_for_tier_with_grant(workspace_root, tier, grant);
    // Write-confinement-only Seatbelt (RightKit `ConfinementMode::WriteOnly`): reads and
    // execution stay open, writes are limited to the workspace plus temp/dev paths, and
    // the strict tier denies outbound network. RightKit realpath-resolves the write roots.
    // `wrap_argv` fails closed (a profile that cannot be built denies everything).
    let mut policy = rightkit_sandbox::SandboxPolicy::new()
        .mode(rightkit_sandbox::ConfinementMode::WriteOnly)
        .network(if config.network_denied {
            rightkit_sandbox::NetworkPolicy::Deny
        } else {
            rightkit_sandbox::NetworkPolicy::Allow
        });
    for path in &config.allowed_write_paths {
        policy = policy.allow_write(path);
    }
    let program = std::ffi::OsString::from(program);
    let args: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    let (wrapper, wrapped_args) = rightkit_sandbox::seatbelt::wrap_argv(&policy, &program, &args);
    Some((
        wrapper.to_string_lossy().into_owned(),
        wrapped_args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    ))
}
