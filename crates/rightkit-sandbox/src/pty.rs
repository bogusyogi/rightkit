//! Managed PTY with lease-epoch input fencing.
//!
//! PTY transport is `rightkit-pty` (rightkit#557): one implementation of the pseudo-terminal,
//! the owned process tree (session leader on Unix, Job Object on Windows) and the staged kill.
//! This module adds the synchronous surface on top: a private one-worker tokio runtime per
//! session drives the terminal's async streams, so callers need no runtime of their own. The lease follows PTY-002 from
//! `background_shell/process_model.rs`: every input write carries `(epoch, sequence)`;
//! a stale epoch or out-of-order sequence is rejected; a reattach fences the epoch
//! forward and refuses to resume silently while input is unacknowledged.
//! Divergence from the donor: `acknowledge_input(epoch, upto)` is exclusive-upper-bound
//! (`[acked, next)` is the unacknowledged range) and a successful write is NOT an
//! implicit acknowledgement, so the range is never permanently non-empty.

use crate::{EnvPolicy, Reservation, SandboxError};
use rightkit_child_process::TerminationOptions;
use rightkit_pty::{PtyChild, PtyCommand, PtyWriter};
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

/// Terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[derive(Debug)]
pub enum PtyError {
    StaleEpoch { presented: u64, current: u64 },
    BadSequence { expected: u64, got: u64 },
    Detached,
    UnacknowledgedInput { from: u64, to: u64 },
    AckOutOfRange { upto: u64, next: u64, acked: u64 },
    Sandbox(SandboxError),
    Io(std::io::Error),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleEpoch { presented, current } => write!(
                f,
                "stale process lease epoch (presented {presented}, current {current})"
            ),
            Self::BadSequence { expected, got } => {
                write!(f, "input sequence must be {expected}, got {got}")
            }
            Self::Detached => write!(f, "process is detached; attach before input"),
            Self::UnacknowledgedInput { from, to } => write!(
                f,
                "unacknowledged input in sequence range [{from}, {to}); reconcile explicitly"
            ),
            Self::AckOutOfRange { upto, next, acked } => {
                write!(f, "ack {upto} outside [{acked}, {next}]")
            }
            Self::Sandbox(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "pty io: {e}"),
        }
    }
}
impl std::error::Error for PtyError {}
impl From<std::io::Error> for PtyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyLeaseSnapshot {
    pub epoch: u64,
    pub next_input_sequence: u64,
    pub acked_input_sequence: u64,
    pub attached: bool,
    /// `[from, to)` written but never acknowledged.
    pub unacknowledged_input: Option<(u64, u64)>,
}

/// Explicit decision when reattaching over unacknowledged input; there is no implicit option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnacknowledgedInputDecision {
    /// Independently confirmed delivered: mark acknowledged.
    ReconcileAsDelivered,
    /// Treated as lost: roll the send cursor back so bytes are resent under fresh sequences.
    ReconcileAsLost,
}

#[derive(Debug)]
struct Lease {
    epoch: u64,
    next: u64,
    acked: u64,
    attached: bool,
}

impl Lease {
    fn unacked(&self) -> Option<(u64, u64)> {
        (self.next > self.acked).then_some((self.acked, self.next))
    }
    fn snap(&self) -> PtyLeaseSnapshot {
        PtyLeaseSnapshot {
            epoch: self.epoch,
            next_input_sequence: self.next,
            acked_input_sequence: self.acked,
            attached: self.attached,
            unacknowledged_input: self.unacked(),
        }
    }
}

const OUTPUT_CAP: usize = 1 << 20;

#[derive(Default)]
struct Output {
    buf: Vec<u8>,
    total: u64,
    eof: bool,
}

/// How the child is confined once the terminal exists.
pub(crate) enum Confinement {
    /// Already wrapped into the argv (macOS Seatbelt) or not confined.
    None,
    /// Landlock and seccomp applied in the forked child as the last step before `exec`.
    #[cfg(target_os = "linux")]
    Linux(crate::linux::Prepared),
}

/// A running PTY child. Dropping it kills the whole process tree and releases the reservation.
pub struct PtySession {
    rt: Option<tokio::runtime::Runtime>,
    child: Mutex<Option<PtyChild>>,
    writer: Mutex<Option<PtyWriter>>,
    pid: u32,
    lease: Mutex<Lease>,
    output: Arc<(Mutex<Output>, Condvar)>,
    _reservation: Reservation,
}

fn io_other(message: &str) -> std::io::Error {
    std::io::Error::other(message.to_string())
}

impl PtySession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        program: OsString,
        args: Vec<OsString>,
        cwd: Option<PathBuf>,
        env: &EnvPolicy,
        extra_env: &[(String, String)],
        size: PtySize,
        confinement: Confinement,
        reservation: Reservation,
    ) -> Result<Self, PtyError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("rightkit-sandbox-pty")
            .enable_all()
            .build()?;
        let mut command = PtyCommand::from_argv(std::iter::once(program).chain(args))?;
        command.size(to_pty_size(size));
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        if !env.inherit {
            command.owned_mut().command_mut().env_clear();
            for key in &env.keep {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
        }
        for (key, value) in env.set.iter() {
            command.env(key, value);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        match confinement {
            Confinement::None => {}
            #[cfg(target_os = "linux")]
            Confinement::Linux(prepared) => {
                // SAFETY: `Prepared::apply` performs only syscalls (the same contract the
                // non-PTY spawn path relies on), and it was prepared in the parent.
                unsafe {
                    command.pre_exec_last(move || prepared.apply());
                }
            }
        }
        // The terminal registers with the reactor of the runtime that is current at spawn.
        let mut child = {
            let _enter = rt.enter();
            command.spawn()?
        };
        let pid = child.id();
        let reader = child
            .take_reader()?
            .ok_or_else(|| io_other("the pty reader was already taken"))?;
        let writer = child
            .take_writer()?
            .ok_or_else(|| io_other("the pty writer was already taken"))?;
        let output: Arc<(Mutex<Output>, Condvar)> = Arc::default();
        let out2 = output.clone();
        rt.spawn(async move {
            let mut reader = reader;
            let mut chunk = [0u8; 4096];
            loop {
                let n = match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let (m, cv) = &*out2;
                let mut o = m.lock().unwrap_or_else(|e| e.into_inner());
                o.buf.extend_from_slice(&chunk[..n]);
                o.total += n as u64;
                if o.buf.len() > OUTPUT_CAP {
                    let drop_n = o.buf.len() - OUTPUT_CAP;
                    o.buf.drain(..drop_n);
                }
                cv.notify_all();
            }
            let (m, cv) = &*out2;
            m.lock().unwrap_or_else(|e| e.into_inner()).eof = true;
            cv.notify_all();
        });
        Ok(Self {
            rt: Some(rt),
            child: Mutex::new(Some(child)),
            writer: Mutex::new(Some(writer)),
            pid,
            lease: Mutex::new(Lease {
                epoch: 1,
                next: 0,
                acked: 0,
                attached: true,
            }),
            output,
            _reservation: reservation,
        })
    }

    /// Runs `future` on the session's runtime and blocks for its result. Safe to call from any
    /// thread, including one that belongs to another tokio runtime.
    fn run<T, F>(&self, future: F) -> std::io::Result<T>
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let rt = self
            .rt
            .as_ref()
            .ok_or_else(|| io_other("the pty runtime has stopped"))?;
        let (tx, rx) = mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(future.await);
        });
        rx.recv()
            .map_err(|_| io_other("the pty runtime stopped before the operation finished"))
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn lease(&self) -> PtyLeaseSnapshot {
        self.lease.lock().unwrap_or_else(|e| e.into_inner()).snap()
    }

    /// Detach the current client; input is rejected until [`attach`](Self::attach).
    pub fn detach(&self) -> PtyLeaseSnapshot {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        l.attached = false;
        l.snap()
    }

    /// Reattach, fencing the epoch forward. Refused while input is unacknowledged.
    pub fn attach(&self) -> Result<PtyLeaseSnapshot, PtyError> {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((from, to)) = l.unacked() {
            return Err(PtyError::UnacknowledgedInput { from, to });
        }
        l.epoch += 1;
        l.attached = true;
        Ok(l.snap())
    }

    /// Resolve an unacknowledged range explicitly, then fence the epoch forward and attach.
    pub fn reattach_after_reconciling(
        &self,
        decision: UnacknowledgedInputDecision,
    ) -> PtyLeaseSnapshot {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if l.unacked().is_some() {
            match decision {
                UnacknowledgedInputDecision::ReconcileAsDelivered => l.acked = l.next,
                UnacknowledgedInputDecision::ReconcileAsLost => l.next = l.acked,
            }
        }
        l.epoch += 1;
        l.attached = true;
        l.snap()
    }

    /// Acknowledge input sequences `< upto`. Monotonic, `acked <= upto <= next`.
    pub fn acknowledge_input(&self, epoch: u64, upto: u64) -> Result<PtyLeaseSnapshot, PtyError> {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if l.epoch != epoch {
            return Err(PtyError::StaleEpoch {
                presented: epoch,
                current: l.epoch,
            });
        }
        if upto < l.acked || upto > l.next {
            return Err(PtyError::AckOutOfRange {
                upto,
                next: l.next,
                acked: l.acked,
            });
        }
        l.acked = upto;
        Ok(l.snap())
    }

    /// Write input fenced by `(epoch, sequence)`. Nothing reaches the child on rejection.
    /// Blocks until the terminal has accepted every byte (a program that does not read its
    /// input holds the call).
    pub fn write_input(
        &self,
        epoch: u64,
        sequence: u64,
        bytes: &[u8],
    ) -> Result<PtyLeaseSnapshot, PtyError> {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if !l.attached {
            return Err(PtyError::Detached);
        }
        if l.epoch != epoch {
            return Err(PtyError::StaleEpoch {
                presented: epoch,
                current: l.epoch,
            });
        }
        if l.next != sequence {
            return Err(PtyError::BadSequence {
                expected: l.next,
                got: sequence,
            });
        }
        // The lease lock is held for the whole write, so writes stay in sequence order and the
        // writer is always present here.
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| io_other("the pty writer is busy"))?;
        let data = bytes.to_vec();
        let outcome = self.run(async move {
            let result = writer.write_input(&data).await;
            (writer, result)
        });
        let (writer, result) = outcome?;
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(writer);
        result?;
        l.next += 1;
        Ok(l.snap())
    }

    pub fn resize(&self, size: PtySize) -> std::io::Result<()> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let child = guard
            .as_mut()
            .ok_or_else(|| io_other("the pty child is being stopped"))?;
        child.resize(to_pty_size(size))
    }

    /// Retained output (bounded to 1 MiB, oldest bytes dropped).
    pub fn output(&self) -> Vec<u8> {
        self.output
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buf
            .clone()
    }

    /// Block until retained output contains `needle` or `timeout` elapses.
    pub fn wait_for_output(&self, needle: &[u8], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (m, cv) = &*self.output;
        let mut o = m.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !needle.is_empty() && o.buf.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || o.eof {
                return false;
            }
            o = cv
                .wait_timeout(o, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Wait for the child to exit; `None` on timeout.
    pub fn wait_exit(&self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(child) = guard.as_mut() {
                    if let Some(status) = child.try_wait()? {
                        return Ok(Some(status));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill the child and all descendants: the staged kill of the owned tree (SIGTERM to the
    /// group, 200 ms grace, SIGKILL, then every other process group of the session; on Windows
    /// the Job Object is terminated).
    pub fn kill_tree(&self) -> std::io::Result<()> {
        let taken = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(mut child) = taken else {
            return Ok(());
        };
        let options = TerminationOptions::default().with_grace(Duration::from_millis(200));
        let outcome = self.run(async move {
            let result = child.cancel(options).await;
            (child, result)
        });
        match outcome {
            Ok((child, result)) => {
                *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                result.map(|_| ())
            }
            Err(error) => Err(error),
        }
    }
}

fn to_pty_size(size: PtySize) -> rightkit_pty::PtySize {
    rightkit_pty::PtySize::new(size.rows.max(1), size.cols.max(1))
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.kill_tree();
        self.writer
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.child
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
