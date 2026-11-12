#[cfg(windows)]
enum BackgroundChild {
    Tokio(tokio::process::Child),
    Confined(crate::sandbox::ConfinedPipedChild),
}

#[cfg(windows)]
impl BackgroundChild {
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Tokio(child) => child.id(),
            Self::Confined(child) => Some(child.pid()),
        }
    }

    fn take_stdin(&mut self) -> Option<crate::sandbox::AsyncStdin> {
        match self {
            Self::Tokio(child) => child.stdin.take().map(|stdin| {
                let stdin: crate::sandbox::AsyncStdin = Box::new(stdin);
                stdin
            }),
            Self::Confined(child) => child.take_stdin(),
        }
    }

    fn take_stdout(&mut self) -> Option<crate::sandbox::AsyncOutput> {
        match self {
            Self::Tokio(child) => child.stdout.take().map(|stdout| {
                let stdout: crate::sandbox::AsyncOutput = Box::new(stdout);
                stdout
            }),
            Self::Confined(child) => child.take_stdout(),
        }
    }

    fn take_stderr(&mut self) -> Option<crate::sandbox::AsyncOutput> {
        match self {
            Self::Tokio(child) => child.stderr.take().map(|stderr| {
                let stderr: crate::sandbox::AsyncOutput = Box::new(stderr);
                stderr
            }),
            Self::Confined(child) => child.take_stderr(),
        }
    }

    async fn wait_code_mut(&mut self) -> std::io::Result<i32> {
        match self {
            Self::Tokio(child) => child.wait().await.map(|status| status.code().unwrap_or(-1)),
            Self::Confined(child) => child.wait_code().await.map(|code| code as i32),
        }
    }

    async fn terminate(&mut self, job: Option<&crate::sandbox::WindowsJob>) {
        if let Some(job) = job {
            let _ = job.terminate();
        }
        match self {
            Self::Tokio(child) => {
                let _ = rightkit_child_process::tokio_support::terminate_tree(
                    child,
                    rightkit_child_process::TerminationOptions::default()
                        .with_grace(std::time::Duration::from_secs(2)),
                )
                .await;
            }
            Self::Confined(child) => {
                if job.is_none() {
                    let _ = child.terminate();
                }
            }
        }
    }
}

#[cfg(windows)]
async fn run_windows_background(
    mut cmd: tokio::process::Command,
    program: String,
    args: Vec<String>,
    workspace_root: std::path::PathBuf,
    apply_local_sandbox: bool,
    shell: Arc<BgShell>,
    mut kill_rx: tokio::sync::oneshot::Receiver<()>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    _admission: crate::process_resource_authority::ProcessAdmission,
) {
    let mut child = if apply_local_sandbox
        && !crate::shell::sandbox_disabled_for_workspace(&workspace_root)
    {
        // The confined spawn is synchronous and can take seconds (profile,
        // ACL leases). Run it off the async worker so the runtime and the
        // kill path stay responsive, and let a kill that arrives meanwhile
        // finish the process row at once instead of after the spawn.
        let spawn_program = program.clone();
        let spawn_args = args.clone();
        let spawn_root = workspace_root.clone();
        let mut spawn = tokio::task::spawn_blocking(move || {
            crate::shell::spawn_windows_appcontainer(&spawn_program, &spawn_args, &spawn_root, true)
        });
        let spawned = tokio::select! {
            joined = &mut spawn => joined,
            _ = &mut kill_rx => {
                shell.set_status(BgStatus::Killed);
                // The spawn cannot be cancelled; reap whatever it yields
                // and keep the admission until it is gone.
                tokio::spawn(async move {
                    let _admission = _admission;
                    if let Ok(Ok(child)) = spawn.await {
                        BackgroundChild::Confined(child).terminate(None).await;
                    }
                });
                return;
            }
        };
        match spawned {
            Ok(Ok(child)) => BackgroundChild::Confined(child),
            Ok(Err(error)) => {
                shell.set_status(BgStatus::SpawnFailed(format!(
                    "AppContainer setup unavailable; refusing unsandboxed command: {error}"
                )));
                return;
            }
            Err(error) => {
                shell.set_status(BgStatus::SpawnFailed(format!(
                        "AppContainer setup unavailable; refusing unsandboxed command: spawn task failed: {error}"
                    )));
                return;
            }
        }
    } else {
        // Explicit compatibility path: callers that disable local sandboxing
        // retain native Windows process semantics. Sandboxed requests never
        // reach this branch; AppContainer failure above is fail-closed.
        match cmd.spawn() {
            Ok(child) => BackgroundChild::Tokio(child),
            Err(error) => {
                shell.set_status(BgStatus::SpawnFailed(error.to_string()));
                return;
            }
        }
    };
    let Some(pid) = child.pid() else {
        shell.set_status(BgStatus::SpawnFailed(
            "spawned child has no pid".to_string(),
        ));
        return;
    };
    let job = match crate::shell::assign_windows_job_to_pid(pid) {
        Ok(job) => Some(job),
        Err(error) => {
            child.terminate(None).await;
            shell.set_status(BgStatus::SpawnFailed(format!(
                "Windows Job Object assignment failed: {error}"
            )));
            return;
        }
    };
    shell.set_pid(Some(pid));
    *shell.stdin.lock().await = child.take_stdin();

    let mut readers = Vec::new();
    if let Some(out) = child.take_stdout() {
        let sh = shell.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = BufReader::new(out);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break,
                    Ok(_) => sh.append(&buf),
                    Err(_) => break,
                }
            }
        }));
    }
    if let Some(err) = child.take_stderr() {
        let sh = shell.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = BufReader::new(err);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break,
                    Ok(_) => sh.append(&buf),
                    Err(_) => break,
                }
            }
        }));
    }

    let final_status = if let Some(mut rx) = cancel_rx {
        let wait_for_cancel = async move {
            if *rx.borrow() {
                return;
            }
            loop {
                match rx.changed().await {
                    Ok(()) if *rx.borrow() => return,
                    Ok(()) => {}
                    Err(_) => std::future::pending::<()>().await,
                }
            }
        };
        tokio::select! {
            result = child.wait_code_mut() => BgStatus::Exited(result.unwrap_or(-1)),
            _ = kill_rx => {
                if let Some(job) = job.as_ref() {
                    let _ = job.terminate();
                }
                BgStatus::Killed
            }
            _ = wait_for_cancel => {
                if let Some(job) = job.as_ref() {
                    let _ = job.terminate();
                }
                BgStatus::Killed
            }
        }
    } else {
        tokio::select! {
            result = child.wait_code_mut() => BgStatus::Exited(result.unwrap_or(-1)),
            _ = kill_rx => {
                if let Some(job) = job.as_ref() {
                    let _ = job.terminate();
                }
                BgStatus::Killed
            }
        }
    };
    if matches!(final_status, BgStatus::Killed) {
        child.terminate(job.as_ref()).await;
        let _ = child.wait_code_mut().await;
    }
    drop(job);
    for mut reader in readers {
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    shell.set_status(final_status);
}

/// Spawn `command` in the background, returning its `shell_id`. Output is read
/// into the shell's buffer until the process exits or is killed.
pub fn spawn_background(
    command: String,
    workspace_root: &Path,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<String, crate::process_resource_authority::ProcessAdmissionError> {
    spawn_background_with_target(
        command,
        workspace_root,
        store,
        cancel_rx,
        BgExecutionTarget::default(),
    )
}

/// Spawn `command` with the selected execution target recorded on the live
/// process row. The local process runner is still used; non-local backends are
/// rejected before this point by the shell tool until their adapters are wired.
pub fn spawn_background_with_target(
    command: String,
    workspace_root: &Path,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
) -> Result<String, crate::process_resource_authority::ProcessAdmissionError> {
    let (program, flag) = crate::shell::platform_shell();
    spawn_background_program_with_target(
        program.to_string(),
        vec![flag.to_string(), command.clone()],
        command,
        workspace_root,
        store,
        cancel_rx,
        execution_target,
        true,
    )
}

pub fn spawn_background_program_with_target(
    program: String,
    args: Vec<String>,
    display_command: String,
    workspace_root: &Path,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
    apply_local_sandbox: bool,
) -> Result<String, crate::process_resource_authority::ProcessAdmissionError> {
    spawn_background_program_with_kind_and_target(
        program,
        args,
        display_command,
        workspace_root,
        store,
        cancel_rx,
        execution_target,
        apply_local_sandbox,
        "backgroundShell",
    )
}

pub fn spawn_background_program_with_kind_and_target(
    program: String,
    args: Vec<String>,
    display_command: String,
    workspace_root: &Path,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
    apply_local_sandbox: bool,
    process_kind: &str,
) -> Result<String, crate::process_resource_authority::ProcessAdmissionError> {
    spawn_background_program_with_kind_and_target_using(
        program,
        args,
        display_command,
        workspace_root,
        store,
        cancel_rx,
        execution_target,
        apply_local_sandbox,
        process_kind,
        || {
            crate::process_resource_authority::reserve_process(
                crate::process_resource_authority::ResourceKind::BackgroundProcess,
                rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
                    workspace_root.to_string_lossy().into_owned(),
                ),
                false,
            )
        },
    )
}

fn spawn_background_program_with_kind_and_target_using(
    program: String,
    args: Vec<String>,
    display_command: String,
    workspace_root: &Path,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
    apply_local_sandbox: bool,
    process_kind: &str,
    reserve: impl FnOnce() -> Result<
        crate::process_resource_authority::ProcessAdmission,
        crate::process_resource_authority::ProcessAdmissionError,
    >,
) -> Result<String, crate::process_resource_authority::ProcessAdmissionError> {
    let admission = reserve()?;
    let shell_id = format!("bg-{}", next_shell_sequence());
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
    let shell = Arc::new(BgShell {
        command: display_command,
        process_kind: process_kind.to_string(),
        execution_target,
        launch_proof: None,
        pid: Mutex::new(None),
        started_at_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        output: Mutex::new(rightkit_agent_engine_runtime_resources::BoundedOutput::default()),
        cursor: Mutex::new(0),
        status: Mutex::new(BgStatus::Running),
        kill_tx: Mutex::new(Some(kill_tx)),
        stdin: tokio::sync::Mutex::new(None),
        channel: Mutex::new(rightkit_sandbox::execution::ExecutionChannel::Pipe),
        control_tx: None,
        persistence: ProcessPersistence::Inline,
        lease: tokio::sync::Mutex::new(ProcessLease {
            epoch: 1,
            next_input_sequence: 0,
            last_acked_input_sequence: 0,
            attached: true,
        }),
        human: Default::default(),
    });
    store
        .lock()
        .unwrap()
        .insert(shell_id.clone(), shell.clone());
    notify_shell_event();

    #[cfg(windows)]
    let windows_program = program.clone();
    #[cfg(windows)]
    let windows_args = args.clone();

    #[cfg(windows)]
    let mut cmd = rightkit_child_process::tokio_support::command(program.clone());
    #[cfg(windows)]
    cmd.args(args)
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Own the whole tree: a background dev server or test watcher spawns
    // children that outlive a kill aimed at the shell alone. This spawn is
    // Windows-only, where the Job Object owns the tree and `rightkit_child_process::tokio_support::command`
    // has already set the no-window flag.

    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
    let _ = apply_local_sandbox;

    #[cfg(windows)]
    tokio::spawn(run_windows_background(
        cmd,
        windows_program,
        windows_args,
        workspace_root.to_path_buf(),
        apply_local_sandbox,
        shell,
        kill_rx,
        cancel_rx,
        admission,
    ));

    #[cfg(not(windows))]
    let workspace_root_owned = workspace_root.to_path_buf();

    #[cfg(not(windows))]
    tokio::spawn(async move {
        let _admission = admission;
        use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchContext};
        let adapter = crate::shell::NativeCommandSandboxAdapter;
        let spec = CommandLaunchSpec {
            program,
            argv: args,
            cwd: workspace_root_owned.to_string_lossy().into_owned(),
            context: ProcessLaunchContext {
                workspace_root: workspace_root_owned.to_string_lossy().into_owned(),
                sandbox_profile: Some("workspace".to_string()),
                sandbox_required: apply_local_sandbox
                    && !crate::shell::sandbox_disabled_for_workspace(&workspace_root_owned),
                network_required: false,
            },
            env: Vec::new(),
        };
        let mut child =
            match rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec).await {
                Ok(c) => c,
                Err(e) => {
                    shell.set_status(BgStatus::SpawnFailed(e.to_string()));
                    return;
                }
            };
        shell.set_pid(child.pid());
        #[cfg(windows)]
        let _windows_job = match assign_windows_job_to_child(&child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                shell.append(
                    format!("Failed to assign background shell to Windows Job Object: {e}\n")
                        .as_bytes(),
                );
                shell.set_status(BgStatus::SpawnFailed(format!(
                    "Windows Job Object assignment failed: {e}"
                )));
                return;
            }
        };
        *shell.stdin.lock().await = child.take_stdin();

        let mut readers = Vec::new();
        if let Some(out) = child.take_stdout() {
            let sh = shell.clone();
            readers.push(tokio::spawn(async move {
                let mut reader = BufReader::new(out);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {
                            sh.append(&buf);
                        }
                        Err(_) => break,
                    }
                }
            }));
        }
        if let Some(err) = child.take_stderr() {
            let sh = shell.clone();
            readers.push(tokio::spawn(async move {
                let mut reader = BufReader::new(err);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {
                            sh.append(&buf);
                        }
                        Err(_) => break,
                    }
                }
            }));
        }

        let final_status = if let Some(mut rx) = cancel_rx {
            let wait_for_cancel = async move {
                if *rx.borrow() {
                    return;
                }
                loop {
                    match rx.changed().await {
                        Ok(()) if *rx.borrow() => return,
                        Ok(()) => {}
                        Err(_) => std::future::pending::<()>().await,
                    }
                }
            };
            tokio::select! {
                res = child.wait() => {
                    BgStatus::Exited(res.unwrap_or(-1))
                }
                _ = kill_rx => {
                    #[cfg(windows)]
                    if let Some(job) = _windows_job.as_ref() {
                        let _ = job.terminate();
                    }
                    let _ = child.cancel().await;
                    let _ = child.wait().await;
                    BgStatus::Killed
                }
                _ = wait_for_cancel => {
                    #[cfg(windows)]
                    if let Some(job) = _windows_job.as_ref() {
                        let _ = job.terminate();
                    }
                    let _ = child.cancel().await;
                    let _ = child.wait().await;
                    BgStatus::Killed
                }
            }
        } else {
            tokio::select! {
                res = child.wait() => {
                    BgStatus::Exited(res.unwrap_or(-1))
                }
                _ = kill_rx => {
                    #[cfg(windows)]
                    if let Some(job) = _windows_job.as_ref() {
                        let _ = job.terminate();
                    }
                    let _ = child.cancel().await;
                    let _ = child.wait().await;
                    BgStatus::Killed
                }
            }
        };
        for mut r in readers {
            if tokio::time::timeout(std::time::Duration::from_secs(1), &mut r)
                .await
                .is_err()
            {
                // A descendant may retain inherited pipes after shell exit;
                // never let supervisor shutdown wait forever for its reader.
                r.abort();
            }
        }
        shell.set_status(final_status);
    });

    Ok(shell_id)
}
