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
        // Own session, so cancel can stop the whole tree (see `cancel`).
        rightkit_child_process::tokio_support::lead_session(&mut command);
        let mut enforced = false;
        let mut network_enforced = false;
        if spec.context.sandbox_required {
            if spec.context.sandbox_profile.is_none() {
                return Err(rightkit_sandbox::execution::EffectExecutionError::SandboxMissing);
            }
            let root = std::path::PathBuf::from(&spec.context.workspace_root);
            // Network-required producers (for example cloudflared) retain
            // filesystem confinement but must not inherit strict seccomp's
            // network denial. Default launches remain strict/fail-closed.
            let config = if spec.context.network_required {
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
            // RightKit prepares the confinement in the parent; a failure is a
            // refused launch, never an unconfined one.
            config
                .confine(command.as_std_mut())
                .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::SandboxMissing)?;
            enforced = true;
        }
        command
            .spawn()
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
        if !spec.context.sandbox_required {
            // Typed opt-out (e.g. user lifecycle hooks outside safe mode): run
            // through the local adapter, which records an unenforced proof.
            return <crate::process_launch::LocalProcessLaunchAdapter as rightkit_sandbox::execution::ProcessLaunchAdapter>::spawn(
                &crate::process_launch::LocalProcessLaunchAdapter,
                spec,
            )
            .await;
        }
        let root = std::path::PathBuf::from(&spec.context.workspace_root);
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
        let network_required = spec.context.network_required;
        let program = spec.program.clone();
        let argv = spec.argv.clone();
        let env = spec.env.clone();
        let confined = tokio::task::spawn_blocking(move || {
            crate::sandbox::AppContainerProfile::for_workspace(&root).and_then(|profile| {
                let profile = profile.with_network(network_required);
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
                network_enforced: !spec.context.network_required,
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
        let (program, argv) = if spec.context.sandbox_required {
            let tier = if spec.context.network_required {
                crate::sandbox::SandboxTier::WorkspaceWrite
            } else {
                crate::shell::effective_sandbox_tier(&root, None)
            };
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
        // Own session, so cancel can stop the whole tree (see `cancel`).
        rightkit_child_process::tokio_support::lead_session(&mut command);
        command
            .spawn()
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
/// Live deltas are coalesced: flushed at this size or age, whichever is first.
const DELTA_FLUSH_BYTES: usize = 8 * 1024;
const DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(40);

/// Background reader that forwards coalesced chunks to the context's live
/// output notifier while accumulating a bounded copy for the final tool result.
/// Aborted on drop so an early return (timeout/cancel) never leaks the task.
struct StreamDrain(tokio::task::JoinHandle<Vec<u8>>);

impl StreamDrain {
    fn spawn<R>(
        stream: Option<R>,
        kind: rightkit_agent_engine_protocol::ToolOutputStream,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
    ) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::spawn_capped(stream, kind, notifier, STREAM_INGRESS_CAP_BYTES)
    }

    fn spawn_capped<R>(
        stream: Option<R>,
        kind: rightkit_agent_engine_protocol::ToolOutputStream,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
        cap: usize,
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
            let live = notifier.is_set();
            let mut carry: Vec<u8> = Vec::new();
            let mut dropped: u64 = 0;
            let mut last_flush = tokio::time::Instant::now();
            let mut buf = [0u8; 4096];
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
                        let room = cap.saturating_sub(all.len());
                        let keep = n.min(room);
                        all.extend_from_slice(&buf[..keep]);
                        dropped += (n - keep) as u64;
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
                    notify_utf8(&notifier, kind, &mut carry);
                    last_flush = tokio::time::Instant::now();
                }
            }
            if live && !carry.is_empty() {
                notifier.notify(kind, &String::from_utf8_lossy(&carry));
            }
            if dropped > 0 {
                all.extend_from_slice(
                    format!(
                        "\n[output truncated: {dropped} bytes beyond the {cap}-byte capture limit were discarded]\n"
                    )
                    .as_bytes(),
                );
            }
            all
        }))
    }

    async fn finish(mut self) -> Vec<u8> {
        (&mut self.0).await.unwrap_or_default()
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

async fn collect_child_output(
    mut child: tokio::process::Child,
    timeout_ms: u64,
    ctx: &ToolContext,
    cmd: &str,
) -> ToolResult {
    let mut cancel_rx_cloned = ctx.cancel_rx.clone();
    let pid_hint = child.id();
    let process_id = pid_hint.map(|p| p.to_string());
    #[cfg(windows)]
    let _windows_job = match assign_windows_job_to_child(&child) {
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

    let stdout_drain = StreamDrain::spawn(
        child.stdout.take(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
        ctx.output_notifier.clone(),
    );
    let stderr_drain = StreamDrain::spawn(
        child.stderr.take(),
        rightkit_agent_engine_protocol::ToolOutputStream::Stderr,
        ctx.output_notifier.clone(),
    );
    let mut wait_status = None;
    if let Some(ref mut cancel_rx) = cancel_rx_cloned {
        let cancel_result = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                // Kill the whole process group, then reap to release the PID and
                // update the process-id column. Without the reap the OS can
                // leave a zombie until the next wait/kill on the same child.
                terminate_owned_tree(&mut child).await;
                let message = format!(
                    "Command timed out after {}ms: {} (pid {:?})",
                    timeout_ms, cmd, pid_hint
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
                Some(ToolResult::error(message).with_process_id(process_id.clone()))
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
                Some(ToolResult::error(message).with_process_id(process_id.clone()))
            }
            status = child.wait() => {
                wait_status = status.ok();
                None
            },
        };
        if let Some(result) = cancel_result {
            return result;
        }
    } else {
        let cancel_result = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                terminate_owned_tree(&mut child).await;
                let message = format!(
                    "Command timed out after {}ms: {} (pid {:?})",
                    timeout_ms, cmd, pid_hint
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
                Some(ToolResult::error(message).with_process_id(process_id.clone()))
            }
            status = child.wait() => {
                wait_status = status.ok();
                None
            },
        };
        if let Some(result) = cancel_result {
            return result;
        }
    }

    // Process exited normally — collect output.
    let stdout_buf = stdout_drain.finish().await;
    let stderr_buf = stderr_drain.finish().await;
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
pub(crate) fn spawn_windows_appcontainer(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    with_stdin: bool,
) -> std::io::Result<crate::sandbox::ConfinedPipedChild> {
    spawn_windows_appcontainer_with_grant(program, args, workspace_root, with_stdin, false, None)
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn local_sandbox_config(workspace_root: &Path) -> SandboxConfig {
    local_sandbox_config_for_tier(workspace_root, sandbox_tier_for_workspace(workspace_root))
}

pub(crate) fn local_sandbox_config_for_tier(
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
) -> SandboxConfig {
    local_sandbox_config_with_home(
        workspace_root,
        tier,
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
    )
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

pub(crate) fn local_sandbox_config_with_home(
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
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
    match tier {
        crate::sandbox::SandboxTier::Strict => config.with_seccomp().deny_network(),
        _ => config,
    }
}

/// macOS Seatbelt wrapper: rewrite `(program, args)` to run under
/// `sandbox-exec -p <profile>` unless the tier is disabled. Returns `None` when
/// no wrapping should happen (tier `none`).
#[cfg(target_os = "macos")]
pub(crate) fn macos_sandbox_wrap(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
) -> Option<(String, Vec<String>)> {
    let tier = effective_sandbox_tier(workspace_root, permission_mode);
    macos_sandbox_wrap_for_tier(program, args, workspace_root, tier)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_sandbox_wrap_for_tier(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    tier: crate::sandbox::SandboxTier,
) -> Option<(String, Vec<String>)> {
    if tier.is_disabled() {
        return None;
    }
    let config = local_sandbox_config_for_tier(workspace_root, tier);
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
