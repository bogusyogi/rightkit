//! Managed PTY with lease-epoch input fencing.
//!
//! PTY transport follows CodeRight `managed_pty.rs` (portable-pty, slave closed after
//! spawn, session leader so the tree is one group). The lease follows PTY-002 from
//! `background_shell/process_model.rs`: every input write carries `(epoch, sequence)`;
//! a stale epoch or out-of-order sequence is rejected; a reattach fences the epoch
//! forward and refuses to resume silently while input is unacknowledged.
//! Divergence from the donor: `acknowledge_input(epoch, upto)` is exclusive-upper-bound
//! (`[acked, next)` is the unacknowledged range) and a successful write is NOT an
//! implicit acknowledgement, so the range is never permanently non-empty.

use crate::{process_tree, EnvPolicy, Reservation, SandboxError};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
            Self::StaleEpoch { presented, current } => write!(f, "stale process lease epoch (presented {presented}, current {current})"),
            Self::BadSequence { expected, got } => write!(f, "input sequence must be {expected}, got {got}"),
            Self::Detached => write!(f, "process is detached; attach before input"),
            Self::UnacknowledgedInput { from, to } => write!(f, "unacknowledged input in sequence range [{from}, {to}); reconcile explicitly"),
            Self::AckOutOfRange { upto, next, acked } => write!(f, "ack {upto} outside [{acked}, {next}]"),
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

/// A running PTY child. Dropping it kills the whole process tree and releases the reservation.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    pid: u32,
    lease: Mutex<Lease>,
    output: Arc<(Mutex<Output>, Condvar)>,
    _reservation: Reservation,
}

impl PtySession {
    pub(crate) fn spawn(
        program: OsString,
        args: Vec<OsString>,
        cwd: Option<PathBuf>,
        env: &EnvPolicy,
        extra_env: &[(String, String)],
        size: PtySize,
        reservation: Reservation,
    ) -> Result<Self, PtyError> {
        let pair = native_pty_system()
            .openpty(portable_pty::PtySize { rows: size.rows.max(1), cols: size.cols.max(1), pixel_width: 0, pixel_height: 0 })
            .map_err(|e| PtyError::Io(std::io::Error::other(e.to_string())))?;
        let mut cb = CommandBuilder::new(&program);
        cb.args(&args);
        if let Some(c) = cwd {
            cb.cwd(c);
        }
        if !env.inherit {
            cb.env_clear();
            for k in &env.keep {
                if let Some(v) = std::env::var_os(k) {
                    cb.env(k, v);
                }
            }
        }
        for (k, v) in env.set.iter() {
            cb.env(k, v);
        }
        for (k, v) in extra_env {
            cb.env(k, v);
        }
        let reader = pair.master.try_clone_reader().map_err(|e| PtyError::Io(std::io::Error::other(e.to_string())))?;
        let writer = pair.master.take_writer().map_err(|e| PtyError::Io(std::io::Error::other(e.to_string())))?;
        let child = pair.slave.spawn_command(cb).map_err(|e| PtyError::Io(std::io::Error::other(e.to_string())))?;
        // Holding the slave open would keep the master from ever seeing EOF.
        drop(pair.slave);
        let pid = child.process_id().unwrap_or(0);
        let output: Arc<(Mutex<Output>, Condvar)> = Arc::default();
        let out2 = output.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 4096];
            loop {
                let n = match reader.read(&mut chunk) {
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
            master: pair.master,
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            pid,
            lease: Mutex::new(Lease { epoch: 1, next: 0, acked: 0, attached: true }),
            output,
            _reservation: reservation,
        })
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
    pub fn reattach_after_reconciling(&self, decision: UnacknowledgedInputDecision) -> PtyLeaseSnapshot {
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
            return Err(PtyError::StaleEpoch { presented: epoch, current: l.epoch });
        }
        if upto < l.acked || upto > l.next {
            return Err(PtyError::AckOutOfRange { upto, next: l.next, acked: l.acked });
        }
        l.acked = upto;
        Ok(l.snap())
    }

    /// Write input fenced by `(epoch, sequence)`. Nothing reaches the child on rejection.
    pub fn write_input(&self, epoch: u64, sequence: u64, bytes: &[u8]) -> Result<PtyLeaseSnapshot, PtyError> {
        let mut l = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if !l.attached {
            return Err(PtyError::Detached);
        }
        if l.epoch != epoch {
            return Err(PtyError::StaleEpoch { presented: epoch, current: l.epoch });
        }
        if l.next != sequence {
            return Err(PtyError::BadSequence { expected: l.next, got: sequence });
        }
        {
            let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            w.write_all(bytes)?;
            w.flush()?;
        }
        l.next += 1;
        Ok(l.snap())
    }

    pub fn resize(&self, size: PtySize) -> std::io::Result<()> {
        self.master
            .resize(portable_pty::PtySize { rows: size.rows.max(1), cols: size.cols.max(1), pixel_width: 0, pixel_height: 0 })
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Retained output (bounded to 1 MiB, oldest bytes dropped).
    pub fn output(&self) -> Vec<u8> {
        self.output.0.lock().unwrap_or_else(|e| e.into_inner()).buf.clone()
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
            o = cv.wait_timeout(o, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Wait for the child to exit; `None` on timeout.
    pub fn wait_exit(&self, timeout: Duration) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(s) = self.child.lock().unwrap_or_else(|e| e.into_inner()).try_wait()? {
                return Ok(Some(s));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill the child and all descendants (session group plus ppid enumeration).
    pub fn kill_tree(&self) -> std::io::Result<()> {
        if self.pid != 0 {
            let _ = process_tree::kill_tree(self.pid, Duration::from_millis(200));
        }
        let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = c.kill();
        let _ = c.wait();
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.kill_tree();
    }
}
