// Channel-aware continuation of the existing background process inventory.
// No process is launched here except through the injected governed adapter.

pub async fn spawn_managed_channel(
    adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    channel: rightkit_sandbox::execution::ExecutionChannel,
    display_command: String,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
    persistence: ProcessPersistence,
) -> Result<String, rightkit_sandbox::execution::EffectExecutionError> {
    spawn_managed_channel_with_kind(
        adapter,
        spec,
        channel,
        display_command,
        store,
        cancel_rx,
        execution_target,
        persistence,
        "backgroundShell".into(),
    )
    .await
}

/// Same as `spawn_managed_channel`, but the caller states the process kind
/// explicitly instead of it being hardcoded. The kind must be passed by the
/// caller — it is never inferred from `ExecutionChannel::Pty` or from
/// `persistence`. PTY does not imply ServiceHost persistence, operator-
/// terminal ownership or a second registry.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_managed_channel_with_kind(
    adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    channel: rightkit_sandbox::execution::ExecutionChannel,
    display_command: String,
    store: &BgStore,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    execution_target: BgExecutionTarget,
    persistence: ProcessPersistence,
    process_kind: String,
) -> Result<String, rightkit_sandbox::execution::EffectExecutionError> {
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        return Err(rightkit_sandbox::execution::EffectExecutionError::Cancelled);
    }
    // Admission completes before reporting a successful tool result/handle.
    let mut child = adapter.spawn_channel(spec, channel).await?;
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        let _ = child.cancel().await;
        let _ = child.wait().await;
        return Err(rightkit_sandbox::execution::EffectExecutionError::Cancelled);
    }
    let shell_id = format!("bg-{}", next_shell_sequence());
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::mpsc::channel(8);
    #[cfg(windows)]
    let stdin = child
        .take_stdin()
        .map(|input| Box::new(input) as BackgroundStdin);
    #[cfg(not(windows))]
    let stdin = child.take_stdin();
    let shell = Arc::new(BgShell {
        command: display_command,
        // It belongs to this host/session. PTY does not imply ServiceHost
        // persistence, operator-terminal ownership or a second registry.
        process_kind,
        execution_target,
        pid: Mutex::new(child.pid()),
        started_at_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        output: Mutex::new(BoundedOutput::default()),
        cursor: Mutex::new(0),
        status: Mutex::new(BgStatus::Running),
        kill_tx: Mutex::new(Some(kill_tx)),
        stdin: tokio::sync::Mutex::new(stdin),
        channel: Mutex::new(child.execution_channel()),
        control_tx: Some(control_tx),
        persistence,
        lease: tokio::sync::Mutex::new(ProcessLease {
            epoch: 1,
            next_input_sequence: 0,
            last_acked_input_sequence: 0,
            attached: true,
        }),
        human: Default::default(),
    });
    lock_recover(store, "background_shell.store").insert(shell_id.clone(), shell.clone());
    notify_shell_event();
    tokio::spawn(supervise_managed_channel(
        child, shell, kill_rx, control_rx, cancel_rx,
    ));
    Ok(shell_id)
}

async fn supervise_managed_channel(
    mut child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
    shell: Arc<BgShell>,
    mut kill_rx: tokio::sync::oneshot::Receiver<()>,
    mut controls: tokio::sync::mpsc::Receiver<ManagedProcessControl>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    use tokio::io::AsyncReadExt;
    let mut readers = Vec::new();
    for stream in [child.take_stdout(), child.take_stderr()]
        .into_iter()
        .flatten()
    {
        let output = shell.clone();
        readers.push(tokio::spawn(async move {
            let mut stream = stream;
            let mut buffer = [0_u8; 8192];
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => output.append(&buffer[..count]),
                    Err(_) => break,
                }
            }
        }));
    }
    let cancellation = wait_for_managed_cancel(cancel_rx);
    tokio::pin!(cancellation);
    let mut controls_open = true;
    let status = loop {
        // Control operations cancel only the wait future, not child ownership.
        // Native ManagedChild waits are cancellation-safe.
        tokio::select! {
            biased;
            _ = &mut kill_rx => { break stop_managed_child(&mut child).await; }
            _ = &mut cancellation => { break stop_managed_child(&mut child).await; }
            control = controls.recv(), if controls_open => {
                match control {
                    Some(ManagedProcessControl::Resize(size, reply)) => {
                        let result = child.resize_pty(size).map_err(|e| e.to_string());
                        if result.is_ok() {
                            *lock_recover(&shell.channel, "background_shell.channel") = child.execution_channel();
                        }
                        let _ = reply.send(result);
                    }
                    None => controls_open = false,
                }
            }
            result = child.wait() => {
                break match result {
                    Ok(code) => BgStatus::Exited(code),
                    Err(error) => {
                        let _ = child.cancel().await;
                        BgStatus::SpawnFailed(format!("managed process settlement failed: {error}"))
                    }
                };
            }
        }
    };
    // Release input even when it was taken out of ManagedChild by inventory.
    shell.stdin.lock().await.take();
    for mut reader in readers {
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    drop(child);
    // Existing engine background completion/async-inbox machinery consumes
    // this same status; no parallel durable event or settlement store.
    shell.set_status(status);
}

async fn stop_managed_child(
    child: &mut Box<dyn rightkit_sandbox::execution::ManagedChild>,
) -> BgStatus {
    match child.cancel().await {
        Ok(()) => match child.wait().await {
            Ok(_) => BgStatus::Killed,
            Err(error) => BgStatus::SpawnFailed(format!("process cleanup failed: {error}")),
        },
        Err(error) => BgStatus::SpawnFailed(format!("process cancellation failed: {error}")),
    }
}

async fn wait_for_managed_cancel(mut receiver: Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(receiver) = &mut receiver {
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
    std::future::pending::<()>().await;
}
