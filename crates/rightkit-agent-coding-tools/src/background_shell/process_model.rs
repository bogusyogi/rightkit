// Background shell execution (Claude-Code `run_in_background` + `BashOutput` +
// `KillShell`).
//
// `shell` with `run_in_background: true` spawns a long-running command (dev
// server, test watcher, build) and returns immediately with a `shell_id`. The
// process keeps running across tool calls; output is buffered. `get_shell_output`
// returns whatever is new since the last poll, plus the run status, and
// `kill_shell` terminates it. The registry lives on the engine and is shared
// into every tool's `ToolContext`, so the ids stay valid for the whole session.

use crate::context::ToolContext;
use crate::lock_recover::lock_recover;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_runtime_resources::BoundedOutput;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(not(windows))]
use tokio::io::AsyncWrite;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static SHELL_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_shell_sequence() -> u64 {
    SHELL_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Process-wide "a background shell appeared or changed state" signal. Waiters
/// subscribe and await `changed()` instead of waking on a timer, so an idle
/// session costs zero wakeups and a completion is observed immediately.
static SHELL_EVENTS: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> =
    std::sync::OnceLock::new();

fn shell_events() -> &'static tokio::sync::watch::Sender<u64> {
    SHELL_EVENTS.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Subscribe to background-shell insert/status-change notifications.
pub fn subscribe_shell_events() -> tokio::sync::watch::Receiver<u64> {
    shell_events().subscribe()
}

/// Announce that a shell was inserted into a store or changed status.
pub(crate) fn notify_shell_event() {
    shell_events().send_modify(|version| *version = version.wrapping_add(1));
}

/// Run status of a background shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BgStatus {
    Running,
    Exited(i32),
    Killed,
    SpawnFailed(String),
    /// Process identity is no longer authoritative after host restart.
    Unknown,
}

/// Typed terminal status for a background run status, or `None` while the run
/// is still `Running` / not authoritative (`Unknown`).
///
/// This is only a faithful "confirmed exit" because every writer of a terminal
/// `BgStatus` sets it AFTER the child was waited on (reaped); there is no path
/// that publishes `Exited`/`Killed` while the child may still be running.
/// Background shells have no wall-clock timeout, so a kill is always an
/// explicit stop or operator cancel. A missing exit code on unix means the
/// child died from a signal the adapter did not surface.
pub fn exit_status_for(
    status: &BgStatus,
) -> Option<rightkit_agent_engine_protocol::ShellExitStatus> {
    use rightkit_agent_engine_protocol::ShellExitStatus;
    match status {
        BgStatus::Running | BgStatus::Unknown => None,
        BgStatus::Exited(code) => Some(if cfg!(unix) && *code == -1 {
            ShellExitStatus::Signaled { signal: None }
        } else {
            ShellExitStatus::Exited { code: *code }
        }),
        BgStatus::Killed => Some(ShellExitStatus::KilledByCancel),
        BgStatus::SpawnFailed(reason) => Some(ShellExitStatus::SpawnFailed {
            reason: reason.clone(),
        }),
    }
}

/// Execution target selected when a background shell was launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPersistence {
    Inline,
    Service,
}

impl Default for ProcessPersistence {
    fn default() -> Self {
        Self::Inline
    }
}

impl ProcessPersistence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Service => "service",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessLeaseSnapshot {
    pub epoch: u64,
    pub next_input_sequence: u64,
    pub last_acked_input_sequence: u64,
    pub attached: bool,
    /// `Some((from, to))` when input sequence numbers in `[from, to)` were
    /// written under this (or a prior) lease epoch but never explicitly
    /// acknowledged. `to` is always `next_input_sequence`. A caller that
    /// observes this must not resume as if history were clean: it has to
    /// explicitly reconcile (treat the range as delivered or as lost) via
    /// `ProcessLease::reattach_after_reconciling_input`, never replay it
    /// ambiguously.
    pub unacknowledged_input: Option<(u64, u64)>,
}

/// Explicit decision a reconnecting client makes about an outstanding
/// unacknowledged input range surfaced on reattach. There is no implicit
/// third option: a lease with outstanding unacknowledged input never
/// reattaches silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnacknowledgedInputDecision {
    /// The client has independently confirmed (e.g. by observing process
    /// output) that the unacknowledged bytes were delivered; mark the range
    /// acknowledged and proceed.
    ReconcileAsDelivered,
    /// The client cannot confirm delivery and treats the range as lost; the
    /// send cursor is rolled back to the last acknowledged point so the
    /// bytes are resent under fresh, unambiguous sequence numbers instead of
    /// being replayed under their original ones.
    ReconcileAsLost,
}

/// `BgShell::attach` currently reports lease conflicts as strings. Keep the
/// foreground operation from treating unrelated failures as reconciliation
/// requests by recognizing only its exact unacknowledged-input error.
pub(crate) fn is_unacknowledged_input_error(error: &str) -> bool {
    error.starts_with("process lease has unacknowledged input in sequence range [")
        && error.ends_with(
            "); reattach must explicitly reconcile or reject it via reattach_after_reconciling_input rather than resume ambiguously",
        )
}

#[derive(Debug)]
pub(crate) struct ProcessLease {
    epoch: u64,
    next_input_sequence: u64,
    last_acked_input_sequence: u64,
    attached: bool,
}

impl ProcessLease {
    /// Range of input sequence numbers that were written but never
    /// acknowledged, if any. See `ProcessLeaseSnapshot::unacknowledged_input`.
    fn unacknowledged_input_range(&self) -> Option<(u64, u64)> {
        if self.next_input_sequence > self.last_acked_input_sequence {
            Some((self.last_acked_input_sequence, self.next_input_sequence))
        } else {
            None
        }
    }

    fn snapshot(&self) -> ProcessLeaseSnapshot {
        ProcessLeaseSnapshot {
            epoch: self.epoch,
            next_input_sequence: self.next_input_sequence,
            last_acked_input_sequence: self.last_acked_input_sequence,
            attached: self.attached,
            unacknowledged_input: self.unacknowledged_input_range(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BgExecutionTarget {
    pub backend_id: Option<String>,
    pub workspace_id: Option<String>,
    pub network_profile: Option<String>,
}

/// One bounded slice of a background shell's output.
pub(crate) struct OutputWindow {
    pub bytes: Vec<u8>,
    pub start: usize,
    pub next_offset: usize,
    pub total: usize,
    pub gap: bool,
    pub more: bool,
    pub beyond_end: bool,
}

/// A single background process and its accumulated output.
pub struct BgShell {
    pub command: String,
    process_kind: String,
    execution_target: BgExecutionTarget,
    /// What the launch adapter reported about the process boundary (rule 55). `None` when the
    /// launch path reports no proof.
    launch_proof: Option<rightkit_sandbox::execution::SandboxProcessProof>,
    pid: Mutex<Option<u32>>,
    started_at_ns: u128,
    output: Mutex<BoundedOutput>,
    /// Byte offset already returned by `get_shell_output`, for incremental reads.
    cursor: Mutex<usize>,
    status: Mutex<BgStatus>,
    kill_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    stdin: tokio::sync::Mutex<Option<BackgroundStdin>>,
    channel: Mutex<rightkit_sandbox::execution::ExecutionChannel>,
    control_tx: Option<tokio::sync::mpsc::Sender<ManagedProcessControl>>,
    persistence: ProcessPersistence,
    lease: tokio::sync::Mutex<ProcessLease>,
    /// #291: shared-input arbitration between an attached human and the agent.
    human: Mutex<HumanInput>,
}

/// #291: the human always wins the shared PTY input. After any human input the
/// agent may not type until the human has been idle this long.
pub const HUMAN_IDLE: std::time::Duration = std::time::Duration::from_millis(1500);
/// Agent bytes held back while the human is typing; beyond this they are refused.
pub const MAX_AGENT_QUEUE_BYTES: usize = 64 * 1024;

/// Agent input held back for a typing human. Its lease sequence was already
/// reserved when it was queued.
#[derive(Debug)]
struct QueuedAgentInput {
    bytes: Vec<u8>,
    epoch: u64,
    sequence: u64,
}

#[derive(Debug, Default)]
struct HumanInput {
    /// A human client is attached to this PTY.
    attached: bool,
    /// When the human last pressed a key or pasted.
    last_input: Option<std::time::Instant>,
    queue: Vec<QueuedAgentInput>,
    drain_running: bool,
    /// Held writes that could not be delivered (rule 29, D31), oldest first, until taken.
    failures: Vec<String>,
}

impl HumanInput {
    /// Time left until the human counts as idle; zero when no human is attached
    /// or they have been quiet for [`HUMAN_IDLE`].
    fn wait_until_idle(&self, now: std::time::Instant) -> std::time::Duration {
        if !self.attached {
            return std::time::Duration::ZERO;
        }
        match self.last_input {
            Some(at) => HUMAN_IDLE.saturating_sub(now.saturating_duration_since(at)),
            None => std::time::Duration::ZERO,
        }
    }

    fn queued_bytes(&self) -> usize {
        self.queue.iter().map(|q| q.bytes.len()).sum()
    }
}

/// What happened to agent input written to a PTY a human may be sharing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInputOutcome {
    /// Written to the process.
    Written(ProcessLeaseSnapshot),
    /// A human is typing: held, and sent in order once they are idle.
    Queued {
        lease: ProcessLeaseSnapshot,
        queued_bytes: usize,
    },
}

#[cfg(windows)]
type BackgroundStdin = crate::sandbox::AsyncStdin;

#[cfg(not(windows))]
type BackgroundStdin = std::pin::Pin<Box<dyn AsyncWrite + Send>>;

impl BgShell {
    /// True when the launch proof says the process runs outside the sandbox (`enforced: false`),
    /// like the operator terminal. A confined agent must not type into such a process (rule 55).
    pub fn launched_unconfined(&self) -> bool {
        self.launch_proof
            .as_ref()
            .is_some_and(|proof| !proof.enforced)
    }

    fn append(&self, chunk: &[u8]) {
        lock_recover(&self.output, "background_shell.output").push(chunk);
    }
    fn set_status(&self, st: BgStatus) {
        *lock_recover(&self.status, "background_shell.status") = st;
        notify_shell_event();
    }
    fn drain_new_payload(&self) -> (Vec<u8>, bool) {
        let buf = lock_recover(&self.output, "background_shell.output");
        let mut cur = lock_recover(&self.cursor, "background_shell.cursor");
        let total = buf.total_bytes() as usize;
        if *cur == total {
            return (Vec::new(), false);
        }
        let preview = buf.preview_bytes();
        let truncated = buf.truncated() || *cur > preview.len();
        let bytes = if truncated {
            preview.to_vec()
        } else {
            preview[*cur..].to_vec()
        };
        *cur = total;
        (bytes, truncated)
    }
    /// Bounded read of retained output. `since` is an absolute byte offset;
    /// `None` continues from the internal cursor (new output only). At most
    /// `max_bytes` are returned and the cursor advances only by what was
    /// returned, so remaining bytes are delivered on the next call.
    /// When the retained window no longer covers the request (output was
    /// truncated), a bounded snapshot is returned with `gap = true`.
    fn read_window(&self, since: Option<usize>, max_bytes: usize) -> OutputWindow {
        let buf = lock_recover(&self.output, "background_shell.output");
        let mut cur = lock_recover(&self.cursor, "background_shell.cursor");
        let total = buf.total_bytes() as usize;
        let start = since.unwrap_or(*cur);
        if start > total {
            return OutputWindow {
                bytes: Vec::new(),
                start: total,
                next_offset: total,
                total,
                gap: false,
                more: false,
                beyond_end: true,
            };
        }
        if start == total {
            if since.is_none() {
                *cur = total;
            }
            return OutputWindow {
                bytes: Vec::new(),
                start,
                next_offset: total,
                total,
                gap: false,
                more: false,
                beyond_end: false,
            };
        }
        let preview = buf.preview_bytes();
        let (bytes, begin, end, gap) = if buf.truncated() {
            // Offsets are not addressable inside a truncated window: return
            // the bounded snapshot and move to the end of the stream.
            let take = preview.len().min(max_bytes);
            (
                preview[preview.len() - take..].to_vec(),
                total.saturating_sub(take),
                total,
                true,
            )
        } else {
            let end = total.min(start.saturating_add(max_bytes));
            (preview[start..end].to_vec(), start, end, false)
        };
        if since.is_none() || end > *cur {
            *cur = end;
        }
        OutputWindow {
            bytes,
            start: begin,
            next_offset: end,
            total,
            gap,
            more: end < total,
            beyond_end: false,
        }
    }
    pub fn persistence(&self) -> ProcessPersistence {
        self.persistence
    }
    pub fn lease_snapshot_now(&self) -> ProcessLeaseSnapshot {
        let lease = self
            .lease
            .try_lock()
            .expect("process lease is not synchronously contended");
        lease.snapshot()
    }
    pub async fn lease_snapshot(&self) -> ProcessLeaseSnapshot {
        let lease = self.lease.lock().await;
        lease.snapshot()
    }
    pub async fn detach(&self) -> Result<ProcessLeaseSnapshot, String> {
        if self.persistence == ProcessPersistence::Inline {
            return Err("inline process cannot be detached; use Service persistence".into());
        }
        let mut lease = self.lease.lock().await;
        lease.attached = false;
        Ok(lease.snapshot())
    }
    /// Attach a reconnecting client to this process lease.
    ///
    /// If there is an unacknowledged input range outstanding (bytes written
    /// under a previous epoch that were never acknowledged), this rejects
    /// the reattach rather than silently resuming with an ambiguous
    /// history: the caller must explicitly resolve it through
    /// `reattach_after_reconciling_input`. When there is nothing
    /// outstanding, the epoch is fenced forward as before.
    pub async fn attach(&self) -> Result<ProcessLeaseSnapshot, String> {
        let mut lease = self.lease.lock().await;
        if let Some((from, to)) = lease.unacknowledged_input_range() {
            return Err(format!(
                "process lease has unacknowledged input in sequence range [{from}, {to}); reattach must explicitly reconcile or reject it via reattach_after_reconciling_input rather than resume ambiguously"
            ));
        }
        lease.epoch = lease.epoch.saturating_add(1);
        lease.attached = true;
        Ok(lease.snapshot())
    }
    /// Explicitly resolve an outstanding unacknowledged input range on
    /// reattach, then fence the epoch forward and attach. This is the only
    /// path that clears an unacknowledged range that blocked `attach()`;
    /// the resolution is always an explicit caller decision, never
    /// inferred.
    pub async fn reattach_after_reconciling_input(
        &self,
        decision: UnacknowledgedInputDecision,
    ) -> ProcessLeaseSnapshot {
        let mut lease = self.lease.lock().await;
        if lease.unacknowledged_input_range().is_some() {
            match decision {
                UnacknowledgedInputDecision::ReconcileAsDelivered => {
                    lease.last_acked_input_sequence = lease.next_input_sequence;
                }
                UnacknowledgedInputDecision::ReconcileAsLost => {
                    lease.next_input_sequence = lease.last_acked_input_sequence;
                }
            }
        }
        lease.epoch = lease.epoch.saturating_add(1);
        lease.attached = true;
        lease.snapshot()
    }
    pub async fn acknowledge_input(
        &self,
        epoch: u64,
        sequence: u64,
    ) -> Result<ProcessLeaseSnapshot, String> {
        let mut lease = self.lease.lock().await;
        if lease.epoch != epoch {
            return Err("stale process lease epoch".into());
        }
        if sequence >= lease.next_input_sequence {
            return Err("cannot acknowledge unsent input sequence".into());
        }
        if sequence < lease.last_acked_input_sequence {
            return Err("input acknowledgement must be monotonic".into());
        }
        lease.last_acked_input_sequence = sequence;
        Ok(lease.snapshot())
    }
    pub async fn write_stdin_bytes_with_lease(
        &self,
        input: &[u8],
        epoch: u64,
        sequence: u64,
    ) -> Result<ProcessLeaseSnapshot, String> {
        let mut lease = self.lease.lock().await;
        if !lease.attached {
            return Err("process is detached; attach before input".into());
        }
        if lease.epoch != epoch {
            return Err("stale process lease epoch".into());
        }
        if lease.next_input_sequence != sequence {
            return Err(format!(
                "input sequence must be {}, got {}",
                lease.next_input_sequence, sequence
            ));
        }
        self.write_stdin_bytes(input).await?;
        lease.next_input_sequence = lease.next_input_sequence.saturating_add(1);
        lease.last_acked_input_sequence = sequence;
        Ok(lease.snapshot())
    }
    /// #291: a human client attached to (or detached from) this PTY.
    pub fn set_human_attached(&self, attached: bool) {
        let mut human = lock_recover(&self.human, "background_shell.human");
        human.attached = attached;
        human.last_input = None;
    }
    /// #291: the human pressed a key or pasted; the agent's input is held back
    /// until they have been idle for [`HUMAN_IDLE`].
    pub fn note_human_input(&self) {
        lock_recover(&self.human, "background_shell.human").last_input =
            Some(std::time::Instant::now());
    }
    /// Bytes of agent input waiting for the human to go idle.
    pub fn queued_agent_bytes(&self) -> usize {
        lock_recover(&self.human, "background_shell.human").queued_bytes()
    }
    /// #291: lease-fenced agent input that yields to an attached, active human.
    /// With no human attached, or an idle one, this is exactly
    /// `write_stdin_bytes_with_lease`. Otherwise the input is held (bounded),
    /// its lease sequence is reserved, and it is delivered in order after the
    /// human has been idle; beyond the bound it is refused with an error.
    pub async fn write_agent_input_with_lease(
        self: &Arc<Self>,
        input: &[u8],
        epoch: u64,
        sequence: u64,
    ) -> Result<AgentInputOutcome, String> {
        let hold = {
            let human = lock_recover(&self.human, "background_shell.human");
            !human.wait_until_idle(std::time::Instant::now()).is_zero()
        };
        if !hold {
            self.flush_agent_queue().await?;
            return self
                .write_stdin_bytes_with_lease(input, epoch, sequence)
                .await
                .map(AgentInputOutcome::Written);
        }
        if input.len() > 65536 {
            return Err("process input exceeds 65536 bytes".into());
        }
        if self.queued_agent_bytes() + input.len() > MAX_AGENT_QUEUE_BYTES {
            return Err(format!(
                "input refused: a human is typing in this terminal and {} KB of your input is already waiting; nothing was sent. Retry after the human is idle.",
                MAX_AGENT_QUEUE_BYTES / 1024
            ));
        }
        let snapshot = {
            let mut lease = self.lease.lock().await;
            if !lease.attached {
                return Err("process is detached; attach before input".into());
            }
            if lease.epoch != epoch {
                return Err("stale process lease epoch".into());
            }
            if lease.next_input_sequence != sequence {
                return Err(format!(
                    "input sequence must be {}, got {}",
                    lease.next_input_sequence, sequence
                ));
            }
            // Reserve the sequence now so the caller's next write lines up.
            lease.next_input_sequence = lease.next_input_sequence.saturating_add(1);
            lease.snapshot()
        };
        let queued_bytes = {
            let mut human = lock_recover(&self.human, "background_shell.human");
            human.queue.push(QueuedAgentInput {
                bytes: input.to_vec(),
                epoch,
                sequence,
            });
            human.queued_bytes()
        };
        self.ensure_agent_drain();
        Ok(AgentInputOutcome::Queued {
            lease: snapshot,
            queued_bytes,
        })
    }
    /// Send every held agent write, in order. A held write that cannot be delivered (a stale
    /// lease epoch, or the process refusing the bytes) is reported to the agent in the process
    /// output and kept in [`Self::take_input_failures`], never dropped silently (rule 29, D31).
    async fn flush_agent_queue(&self) -> Result<(), String> {
        let pending =
            std::mem::take(&mut lock_recover(&self.human, "background_shell.human").queue);
        let mut remaining = pending.into_iter();
        while let Some(queued) = remaining.next() {
            let mut lease = self.lease.lock().await;
            if lease.epoch != queued.epoch {
                self.report_lost_input(format!(
                    "{} bytes of held input were dropped: the process lease changed (stale epoch)",
                    queued.bytes.len()
                ));
                continue;
            }
            if let Err(error) = self.write_stdin_bytes(&queued.bytes).await {
                let lost: usize =
                    queued.bytes.len() + remaining.map(|q| q.bytes.len()).sum::<usize>();
                self.report_lost_input(format!(
                    "{lost} bytes of held input could not be written: {error}"
                ));
                return Err(error);
            }
            if queued.sequence >= lease.last_acked_input_sequence {
                lease.last_acked_input_sequence = queued.sequence;
            }
        }
        Ok(())
    }
    /// Tell the agent a held write was lost: a line in the process output (read with the next
    /// output poll) and an entry for [`Self::take_input_failures`].
    fn report_lost_input(&self, reason: String) {
        self.append(format!("\n[rightkit: {reason}]\n").as_bytes());
        lock_recover(&self.human, "background_shell.human")
            .failures
            .push(reason);
    }
    /// Held-write failures recorded since the last call (rule 29).
    pub fn take_input_failures(&self) -> Vec<String> {
        std::mem::take(&mut lock_recover(&self.human, "background_shell.human").failures)
    }
    /// Background task that returns the lease to the agent once the human is
    /// idle (or detached) and sends what it queued.
    fn ensure_agent_drain(self: &Arc<Self>) {
        {
            let mut human = lock_recover(&self.human, "background_shell.human");
            if human.drain_running {
                return;
            }
            human.drain_running = true;
        }
        let shell = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let wait = {
                    let mut human = lock_recover(&shell.human, "background_shell.human");
                    if human.queue.is_empty() {
                        human.drain_running = false;
                        return;
                    }
                    human.wait_until_idle(std::time::Instant::now())
                };
                if wait.is_zero() {
                    // A failure is already reported to the agent by `flush_agent_queue`.
                    let _ = shell.flush_agent_queue().await;
                } else {
                    tokio::time::sleep(wait).await;
                }
            }
        });
    }
    pub async fn close_stdin_with_lease(
        &self,
        epoch: u64,
        sequence: u64,
    ) -> Result<ProcessLeaseSnapshot, String> {
        let mut lease = self.lease.lock().await;
        if !lease.attached {
            return Err("process is detached; attach before EOF".into());
        }
        if lease.epoch != epoch {
            return Err("stale process lease epoch".into());
        }
        if lease.next_input_sequence != sequence {
            return Err(format!(
                "input sequence must be {}, got {}",
                lease.next_input_sequence, sequence
            ));
        }
        self.close_stdin().await?;
        lease.next_input_sequence = lease.next_input_sequence.saturating_add(1);
        lease.last_acked_input_sequence = sequence;
        Ok(lease.snapshot())
    }
    pub fn execution_channel(&self) -> rightkit_sandbox::execution::ExecutionChannel {
        *lock_recover(&self.channel, "background_shell.channel")
    }
    pub async fn close_stdin(&self) -> Result<(), String> {
        let mut slot = self.stdin.lock().await;
        if let Some(input) = slot.as_mut() {
            input
                .shutdown()
                .await
                .map_err(|e| format!("process EOF: {e}"))?;
            slot.take();
        }
        Ok(())
    }
    pub async fn resize_pty(
        &self,
        size: rightkit_sandbox::execution::PtySize,
    ) -> Result<(), String> {
        let sender = self
            .control_tx
            .as_ref()
            .ok_or("process is not a managed PTY")?;
        let (reply, result) = tokio::sync::oneshot::channel();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            sender
                .send(ManagedProcessControl::Resize(size, reply))
                .await
                .map_err(|_| "process has exited".to_string())?;
            result
                .await
                .map_err(|_| "process control response lost".to_string())?
        })
        .await
        .map_err(|_| "process resize timed out".to_string())?
    }
    pub fn status(&self) -> BgStatus {
        lock_recover(&self.status, "background_shell.status").clone()
    }
    /// Typed, reaped exit status; `None` while the process is still running.
    pub fn exit_status(&self) -> Option<rightkit_agent_engine_protocol::ShellExitStatus> {
        exit_status_for(&self.status())
    }
    pub fn execution_target(&self) -> BgExecutionTarget {
        self.execution_target.clone()
    }
    pub fn process_kind(&self) -> &str {
        &self.process_kind
    }
    pub fn pid(&self) -> Option<u32> {
        *lock_recover(&self.pid, "background_shell.pid")
    }
    pub fn started_at_ns(&self) -> u128 {
        self.started_at_ns
    }
    fn set_pid(&self, pid: Option<u32>) {
        *lock_recover(&self.pid, "background_shell.pid") = pid;
    }
    pub fn output_snapshot(&self) -> String {
        String::from_utf8_lossy(
            &lock_recover(&self.output, "background_shell.output").preview_bytes(),
        )
        .into_owned()
    }
    pub fn kill(&self) -> bool {
        lock_recover(&self.kill_tx, "background_shell.kill_tx")
            .take()
            .is_some_and(|tx| tx.send(()).is_ok())
    }
    pub async fn write_stdin(&self, input: &str) -> Result<(), String> {
        self.write_stdin_bytes(input.as_bytes()).await
    }
    pub async fn write_stdin_bytes(&self, input: &[u8]) -> Result<(), String> {
        if input.len() > 65536 {
            return Err("process input exceeds 65536 bytes".into());
        }
        // A confined spawn (AppContainer profile, ancestor leases) can take
        // seconds; until the child has a pid its stdin is not wired yet, so
        // wait for the spawn and then allow the short grace below.
        let spawn_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while self.pid().is_none()
            && matches!(self.status(), BgStatus::Running)
            && std::time::Instant::now() < spawn_deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        for attempt in 0..20 {
            if !matches!(self.status(), BgStatus::Running) {
                return Err("execution process is not running".to_string());
            }
            {
                let mut stdin = self.stdin.lock().await;
                if let Some(stdin) = stdin.as_mut() {
                    stdin
                        .write_all(input)
                        .await
                        .map_err(|e| format!("write execution process stdin: {e}"))?;
                    return stdin
                        .flush()
                        .await
                        .map_err(|e| format!("flush execution process stdin: {e}"));
                }
            }
            if attempt == 19 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        Err("execution process stdin is not available".to_string())
    }
}

pub type BgStore = Arc<Mutex<HashMap<String, Arc<BgShell>>>>;

enum ManagedProcessControl {
    Resize(
        rightkit_sandbox::execution::PtySize,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ),
}
