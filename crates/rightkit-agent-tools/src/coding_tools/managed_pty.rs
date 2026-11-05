//! Raw managed PTY transport for the host app-owned child execution.
//!
//! This module intentionally stops at the kernel PTY boundary. It owns byte
//! input/output, resize, EOF, child wait/cancellation, and descendant cleanup.
//! It does not parse terminal escape sequences or own a terminal grid,
//! scrollback, font shaping, rendering, tabs, or panes.

#[cfg(all(unix, not(target_os = "linux")))]
use portable_pty::{native_pty_system, CommandBuilder};
#[cfg(unix)]
use portable_pty::{Child, ChildKiller, MasterPty};
#[cfg(unix)]
use rightkit_child_process::AdoptedSignal;
#[cfg(unix)]
use rightkit_sandbox::execution::ExecutionChannel;
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ManagedChild, PtySize, SandboxProcessProof,
};
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

#[cfg(windows)]
#[path = "managed_pty_windows.rs"]
mod windows_backend;

#[cfg(target_os = "linux")]
#[path = "managed_pty_linux.rs"]
mod linux_backend;

pub(crate) fn spawn(
    spec: &CommandLaunchSpec,
    size: PtySize,
    program: &str,
    argv: &[String],
    proof: SandboxProcessProof,
    _sandbox_tier: Option<crate::coding_tools::sandbox::SandboxTier>,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    if program.is_empty()
        || program.contains('\0')
        || !std::path::Path::new(&spec.cwd).is_absolute()
        || !std::path::Path::new(&spec.cwd).is_dir()
        || spec.cwd.is_empty()
        || spec.cwd.contains('\0')
        || argv.iter().any(|arg| arg.contains('\0'))
        || spec.env.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
        })
        || size.rows == 0
        || size.cols == 0
        || size.rows > 32767
        || size.cols > 32767
    {
        return Err(EffectExecutionError::InvalidRequest {
            field: "execution_channel".into(),
            reason: "invalid PTY command, environment, working directory or dimensions".into(),
        });
    }
    #[cfg(windows)]
    {
        return windows_backend::spawn(spec, size, program, argv, proof, _sandbox_tier);
    }
    #[cfg(target_os = "linux")]
    {
        return linux_backend::spawn(spec, size, program, argv, proof, _sandbox_tier);
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        return spawn_portable(spec, size, program, argv, proof);
    }
    #[allow(unreachable_code)]
    Err(EffectExecutionError::InvalidRequest {
        field: "execution_channel".to_string(),
        reason: "managed PTY execution is unsupported on this platform".to_string(),
    })
}

/// Spawn an already policy-resolved program under a native PTY.
///
/// Callers own validation, policy/approval, resource admission, and sandbox
/// resolution before entering this function. `program`/`argv` may therefore
/// be a sandbox wrapper around the original `spec` program on platforms where
/// the native sandbox composes with a PTY.
///
/// Only the non-Linux Unix branch of [`spawn`] calls this (Linux and Windows
/// use their own backends), so it is gated to that branch.
#[cfg(all(unix, not(target_os = "linux")))]
fn spawn_portable(
    spec: &CommandLaunchSpec,
    size: PtySize,
    program: &str,
    argv: &[String],
    proof: SandboxProcessProof,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let portable_size = portable_pty::PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    };
    let pair = native_pty_system()
        .openpty(portable_size)
        .map_err(|_| EffectExecutionError::SpawnFailed)?;

    let mut command = CommandBuilder::new(program);
    command.args(argv);
    command.cwd(&spec.cwd);
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    // portable-pty uses this to create the controlling terminal/session on
    // Unix. That gives cancellation a process-group boundary rather than a
    // single child PID; on Windows the same descendant ownership is reinforced
    // by the host app's existing Job Object below.
    command.set_controlling_tty(true);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    // Holding the slave side open prevents the master from observing EOF after
    // the child exits. Both pinned PTY donors close it immediately after spawn.
    drop(pair.slave);

    finish_unix_spawn(pair.master, child, reader, writer, size, proof)
}

#[cfg(unix)]
fn finish_unix_spawn(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    size: PtySize,
    proof: SandboxProcessProof,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let pid = child.process_id();
    let killer = child.clone_killer();
    // Establish drop ownership before any fallible bridge allocation.
    let mut managed = ManagedPtyChild {
        master,
        child: Some(child),
        killer,
        pid,
        proof,
        size,
        input: None,
        input_shutdown: Arc::new(AtomicBool::new(false)),
        output: None,
        output_drain: Arc::new(AtomicBool::new(false)),
        exit_code: None,
    };
    let (output, drain) =
        spawn_reader_bridge(reader).map_err(|_| EffectExecutionError::SpawnFailed)?;
    managed.output = Some(output);
    managed.output_drain = drain;
    let (input, stop) =
        spawn_writer_bridge(writer).map_err(|_| EffectExecutionError::SpawnFailed)?;
    managed.input = Some(input);
    managed.input_shutdown = stop;
    Ok(Box::new(managed))
}

#[cfg(unix)]
struct ManagedPtyChild {
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
    proof: SandboxProcessProof,
    size: PtySize,
    input: Option<PtyAsyncWriter>,
    input_shutdown: Arc<AtomicBool>,
    output: Option<PtyAsyncReader>,
    output_drain: Arc<AtomicBool>,
    exit_code: Option<i32>,
}

#[cfg(unix)]
#[async_trait::async_trait]
impl ManagedChild for ManagedPtyChild {
    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn sandbox_proof(&self) -> Option<SandboxProcessProof> {
        Some(self.proof.clone())
    }

    fn execution_channel(&self) -> ExecutionChannel {
        ExecutionChannel::Pty(self.size)
    }

    fn start_kill(&mut self) -> bool {
        self.signal_owned_groups(AdoptedSignal::Force) || self.killer.kill().is_ok()
    }

    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        self.input
            .take()
            .map(|writer| Box::pin(writer) as Pin<Box<dyn AsyncWrite + Send>>)
    }

    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        self.output
            .take()
            .map(|reader| Box::pin(reader) as Pin<Box<dyn AsyncRead + Send>>)
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        // A PTY has one terminal byte stream; stderr is intentionally merged.
        None
    }

    fn resize_pty(&mut self, size: PtySize) -> Result<(), EffectExecutionError> {
        if size.rows == 0 || size.cols == 0 || size.rows > 32767 || size.cols > 32767 {
            return Err(EffectExecutionError::InvalidRequest {
                field: "execution_channel".into(),
                reason: "PTY dimensions must be 1..32767".into(),
            });
        }
        let portable = portable_pty::PtySize {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        };
        self.master
            .resize(portable)
            .map_err(|_| EffectExecutionError::Internal)?;
        self.size = PtySize {
            rows: portable.rows,
            cols: portable.cols,
            pixel_width: portable.pixel_width,
            pixel_height: portable.pixel_height,
        };
        Ok(())
    }

    async fn wait(&mut self) -> Result<i32, EffectExecutionError> {
        if let Some(code) = self.exit_code {
            return Ok(code);
        }
        // Retain child ownership if the caller cancels the wait future.
        let child = self.child.as_mut().ok_or(EffectExecutionError::Internal)?;
        let status = loop {
            match child
                .try_wait()
                .map_err(|_| EffectExecutionError::Internal)?
            {
                Some(status) => break status,
                None => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };
        let code = status.exit_code() as i32;
        self.exit_code = Some(code);
        self.signal_owned_groups(AdoptedSignal::Force);
        self.pid.take();
        self.input_shutdown.store(true, Ordering::Release);
        Ok(code)
    }

    async fn cancel(&mut self) -> Result<(), EffectExecutionError> {
        self.output_drain.store(true, Ordering::Release);
        self.input_shutdown.store(true, Ordering::Release);
        self.input.take();
        self.signal_owned_groups(AdoptedSignal::Graceful);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if self.signal_owned_groups(AdoptedSignal::Force) {
            return Ok(());
        }
        self.killer
            .kill()
            .map_err(|_| EffectExecutionError::KillFailed)
    }
}

#[cfg(unix)]
impl ManagedPtyChild {
    fn signal_owned_groups(&self, signal: AdoptedSignal) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        // Every job-control group of the owned session (RightKit's process-table scan;
        // this process's own group is never signalled).
        match std::num::NonZeroU32::new(pid) {
            Some(session) => {
                if let Err(error) = rightkit_child_process::signal_session(session, signal) {
                    tracing::warn!(%error, pid, "PTY session group enumeration failed; applying owned-group cleanup");
                }
            }
            None => tracing::warn!(
                pid,
                "PTY session id is invalid; applying owned-group cleanup"
            ),
        }
        // Interactive shells put the foreground job in a separate process
        // group. It is still attached to this owned controlling terminal.
        if let Some(foreground) = self
            .master
            .process_group_leader()
            .filter(|group| *group > 0)
        {
            if foreground as u32 != pid {
                let _ = signal_process_group(foreground as u32, signal);
            }
        }
        signal_process_group(pid, signal)
    }
}

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: AdoptedSignal) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let number = match signal {
        AdoptedSignal::Graceful => libc::SIGTERM,
        AdoptedSignal::Force => libc::SIGKILL,
    };
    // SAFETY: killpg takes integer ids only. A stale process group returns an
    // OS error and is reported as false; it cannot dereference process memory.
    unsafe { libc::killpg(pgid, number) == 0 }
}

fn spawn_reader_bridge(
    mut reader: Box<dyn Read + Send>,
) -> std::io::Result<(PtyAsyncReader, Arc<AtomicBool>)> {
    let (tx, rx) = mpsc::channel(32);
    let drain = Arc::new(AtomicBool::new(false));
    let worker_drain = drain.clone();
    std::thread::Builder::new()
        .name("rightkit-agent-pty-reader".to_string())
        .spawn(move || {
            let mut buffer = vec![0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut message = Ok(buffer[..read].to_vec());
                        while !worker_drain.load(Ordering::Acquire) {
                            match tx.try_send(message) {
                                Ok(()) => break,
                                Err(mpsc::error::TrySendError::Full(returned)) => {
                                    message = returned;
                                    std::thread::sleep(std::time::Duration::from_millis(10));
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => return,
                            }
                        }
                        // During cancellation/drop, drain without blocking on
                        // an abandoned consumer so ConPTY close cannot deadlock.
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    #[cfg(unix)]
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => break,
                    Err(error) => {
                        let _ = tx.try_send(Err(error.to_string()));
                        break;
                    }
                }
            }
        })
        .map_err(std::io::Error::other)?;
    Ok((
        PtyAsyncReader {
            rx,
            current: Vec::new(),
            offset: 0,
        },
        drain,
    ))
}

fn spawn_writer_bridge(
    mut writer: Box<dyn Write + Send>,
) -> std::io::Result<(PtyAsyncWriter, Arc<AtomicBool>)> {
    // Only one acknowledged operation may be outstanding, capped at 8 KiB.
    // Backpressure is enforced before reporting a write as accepted.
    let (tx, rx) = std::sync::mpsc::sync_channel::<PtyWriteOperation>(1);
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = stopped.clone();
    std::thread::Builder::new()
        .name("rightkit-agent-pty-writer".to_string())
        .spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match rx.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok(PtyWriteOperation { bytes, ack }) => {
                        let count = bytes.len();
                        let result = writer.write_all(&bytes).and_then(|_| writer.flush());
                        let failed = result.is_err();
                        let _ = ack.send(result.map(|_| count));
                        if failed {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        // Closing the input pipe hangs up ConPTY. Keep it alive
                        // after canonical EOF until the managed child settles.
                        while !worker_stop.load(Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        break;
                    }
                }
            }
        })
        .map_err(std::io::Error::other)?;
    Ok((
        PtyAsyncWriter {
            tx: Some(tx),
            pending: None,
            eof_sent: false,
        },
        stopped,
    ))
}

struct PtyWriteOperation {
    bytes: Vec<u8>,
    ack: tokio::sync::oneshot::Sender<std::io::Result<usize>>,
}

struct PtyAsyncReader {
    rx: mpsc::Receiver<Result<Vec<u8>, String>>,
    current: Vec<u8>,
    offset: usize,
}

impl AsyncRead for PtyAsyncReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        target: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if target.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if self.offset < self.current.len() {
                let available = &self.current[self.offset..];
                let count = available.len().min(target.remaining());
                target.put_slice(&available[..count]);
                self.offset += count;
                return Poll::Ready(Ok(()));
            }
            self.current.clear();
            self.offset = 0;
            match Pin::new(&mut self.rx).poll_recv(cx) {
                Poll::Ready(Some(Ok(bytes))) => self.current = bytes,
                Poll::Ready(Some(Err(message))) => {
                    return Poll::Ready(Err(std::io::Error::other(message)));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

struct PtyAsyncWriter {
    tx: Option<std::sync::mpsc::SyncSender<PtyWriteOperation>>,
    pending: Option<tokio::sync::oneshot::Receiver<std::io::Result<usize>>>,
    eof_sent: bool,
}

impl PtyAsyncWriter {
    fn enqueue(&mut self, bytes: Vec<u8>) -> std::io::Result<()> {
        let tx = self.tx.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "PTY input is closed")
        })?;
        let (ack, pending) = tokio::sync::oneshot::channel();
        tx.try_send(PtyWriteOperation { bytes, ack }).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "PTY writer exited")
        })?;
        self.pending = Some(pending);
        Ok(())
    }

    fn poll_ack(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<usize>> {
        use std::future::Future;
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        match Pin::new(pending).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                Poll::Ready(result.unwrap_or_else(|_| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "PTY writer acknowledgement lost",
                    ))
                }))
            }
        }
    }
}

impl AsyncWrite for PtyAsyncWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.eof_sent {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY input is closed",
            )));
        }
        // A Pending write must not consume the caller buffer. Drain the
        // previously accepted bounded operation before accepting a new one.
        match self.poll_ack(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => {}
        }
        let count = bytes.len().min(8192);
        if count == 0 {
            return Poll::Ready(Ok(0));
        }
        match self.enqueue(bytes[..count].to_vec()) {
            Ok(()) => Poll::Ready(Ok(count)),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_ack(cx).map(|result| result.map(|_| ()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.poll_ack(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => {}
        }
        if !self.eof_sent {
            // Canonical terminal EOF is not a pipe half-close. Raw-mode
            // programs interpret their own protocol; cancellation is separate.
            let eof = if cfg!(windows) {
                vec![0x1a, b'\r']
            } else {
                vec![0x04]
            };
            if let Err(error) = self.enqueue(eof) {
                return Poll::Ready(Err(error));
            }
            self.eof_sent = true;
            return self.poll_shutdown(cx);
        }
        self.tx.take();
        Poll::Ready(Ok(()))
    }
}

#[cfg(unix)]
impl Drop for ManagedPtyChild {
    fn drop(&mut self) {
        self.output_drain.store(true, Ordering::Release);
        self.input_shutdown.store(true, Ordering::Release);
        self.input.take();
        self.signal_owned_groups(AdoptedSignal::Force);
        if let Some(mut child) = self.child.take() {
            // Reap on drop as well as explicit wait; no detached process owner.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
