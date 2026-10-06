//! Owns the `rightapp-engine` sidecar: ownership-safe spawn (`rightkit-process`), framed stdio RPC
//! (protocol crate) with a hard per-call deadline, and bounded on-demand restart
//! (`rightkit_process::RestartTracker`).
//!
//! A call that outlives its deadline kills the engine and fails; it is never replayed (a command may
//! already have taken effect). The next call restarts the engine within the restart budget, and
//! callers see `engine restart budget exhausted` once the budget is spent instead of looping.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rightapp_protocol::{framed, Request, Response, MAX_FRAME_BYTES};
use rightkit_process::{Generation, OwnedChild, OwnedCommand, RestartMode, RestartOutcome, RestartPolicy, RestartTracker};
use serde_json::Value;
use tauri::{AppHandle, Manager};

/// Default deadline for one engine call. Long jobs belong behind a job store, not a blocked call.
pub const DEFAULT_CALL_DEADLINE: Duration = Duration::from_secs(30);

struct Live {
    child: OwnedChild,
    stdin: BufWriter<ChildStdin>,
    frames: Receiver<Result<Response, String>>,
    generation: Generation,
}

pub struct EngineSupervisor {
    path: PathBuf,
    data_root: PathBuf,
    live: Option<Live>,
    tracker: RestartTracker,
    epoch: Instant,
    next_id: u64,
    ever_started: bool,
}

impl EngineSupervisor {
    /// Resolve the engine for this app and start it.
    pub fn spawn(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let mut s = Self::with_path(engine_path(app)?, crate::app_data_root())?;
        s.ensure_live()?;
        Ok(s)
    }

    /// Supervise an explicit engine executable. The engine starts lazily on the first call.
    pub fn with_path(path: PathBuf, data_root: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let policy = RestartPolicy::new(RestartMode::OnDemand, 5, Duration::from_secs(60), Duration::from_millis(100), Duration::from_secs(2))?;
        Ok(Self { path, data_root, live: None, tracker: RestartTracker::new(policy), epoch: Instant::now(), next_id: 1, ever_started: false })
    }

    fn ensure_live(&mut self) -> Result<(), String> {
        if self.live.is_some() {
            return Ok(());
        }
        if self.ever_started {
            match self.tracker.request_restart(self.epoch.elapsed()) {
                RestartOutcome::Restart(permit) => {
                    std::thread::sleep(permit.delay());
                    if !self.tracker.permit_valid(&permit, self.epoch.elapsed()) {
                        return Err("engine restart permit expired".into());
                    }
                }
                RestartOutcome::Exhausted => return Err("engine restart budget exhausted".into()),
                RestartOutcome::Stopped => return Err("engine supervisor stopped".into()),
                RestartOutcome::AwaitingDemand | RestartOutcome::IgnoredStale => {}
            }
        }
        self.live = Some(self.start().map_err(|e| e.to_string())?);
        self.ever_started = true;
        Ok(())
    }

    fn start(&mut self) -> Result<Live, Box<dyn std::error::Error>> {
        let mut command = OwnedCommand::new(&self.path);
        command
            .command_mut()
            .arg("--stdio")
            .env("RIGHTAPP_APP_DATA", &self.data_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command.windows_hide();
        let mut child = command.spawn()?;
        let stdin = BufWriter::new(child.take_stdin().ok_or("engine stdin unavailable")?);
        let mut stdout = BufReader::new(child.take_stdout().ok_or("engine stdout unavailable")?);
        let (tx, frames) = channel();
        // One reader per engine generation: the channel lets `call` wait with a deadline, and the
        // thread ends when the engine's stdout closes (exit or kill).
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            loop {
                match framed::read_json_line::<_, Response>(&mut stdout, &mut buf, MAX_FRAME_BYTES) {
                    Ok(Some(r)) => {
                        if tx.send(Ok(r)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Err("engine closed stdout".into()));
                        return;
                    }
                    Err(framed::FrameError::Io(e)) => {
                        let _ = tx.send(Err(format!("engine stdout failed: {e}")));
                        return;
                    }
                    Err(e) => {
                        if tx.send(Err(format!("bad engine frame: {e}"))).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let generation = self.tracker.started(self.epoch.elapsed());
        Ok(Live { child, stdin, frames, generation })
    }

    /// Call with [`DEFAULT_CALL_DEADLINE`].
    pub fn call(&mut self, cmd: &str, input: Value) -> Result<Value, String> {
        self.call_with_deadline(cmd, input, DEFAULT_CALL_DEADLINE)
    }

    pub fn call_with_deadline(&mut self, cmd: &str, input: Value, deadline: Duration) -> Result<Value, String> {
        self.ensure_live()?;
        let id = self.next_id;
        self.next_id += 1;
        let started = Instant::now();
        let request = Request { id, cmd: cmd.to_string(), input };
        let live = self.live.as_mut().expect("ensure_live set it");
        if let Err(e) = framed::write_json_line(&mut live.stdin, &request, MAX_FRAME_BYTES) {
            self.retire();
            return Err(format!("engine write failed: {e}"));
        }
        loop {
            let left = deadline.saturating_sub(started.elapsed());
            let live = self.live.as_mut().expect("live during call");
            match live.frames.recv_timeout(left) {
                Ok(Ok(response)) if response.id == id => {
                    return match response.error {
                        Some(e) => Err(format!("{}: {}", e.code, e.message)),
                        None => Ok(response.result.unwrap_or(Value::Null)),
                    };
                }
                Ok(Ok(_)) => continue, // frame for an earlier, abandoned call
                Ok(Err(msg)) if msg.starts_with("bad engine frame") => continue,
                Ok(Err(msg)) => {
                    self.retire();
                    return Err(msg);
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.retire();
                    return Err(format!("engine call `{cmd}` exceeded its {}ms deadline; engine stopped (not replayed)", deadline.as_millis()));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.retire();
                    return Err("engine reader ended".into());
                }
            }
        }
    }

    /// Stop the current engine (bounded) and record the exit with the restart tracker.
    fn retire(&mut self) {
        if let Some(mut live) = self.live.take() {
            drop(live.stdin); // closing stdin is the graceful shutdown signal
            let _ = live.child.wait_or_kill(Duration::from_millis(500));
            let _ = self.tracker.exited(live.generation, self.epoch.elapsed());
        }
    }
}

impl Drop for EngineSupervisor {
    fn drop(&mut self) {
        self.tracker.shutdown();
        if let Some(mut live) = self.live.take() {
            drop(live.stdin);
            let _ = live.child.wait_or_kill(Duration::from_secs(2));
        }
    }
}

fn engine_path(app: &AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = std::env::var_os("RIGHTAPP_ENGINE_BIN") {
        return Ok(p.into());
    }
    let name = if cfg!(windows) { "rightapp-engine.exe" } else { "rightapp-engine" };
    // Packaged: Tauri places externalBin next to the executable, triple suffix stripped.
    let exe_dir = std::env::current_exe()?.parent().map(|p| p.to_path_buf()).ok_or("no exe dir")?;
    let packaged = exe_dir.join(name);
    if packaged.is_file() {
        return Ok(packaged);
    }
    Ok(app.path().resource_dir()?.join(name))
}
