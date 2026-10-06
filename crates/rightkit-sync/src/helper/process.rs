//! Child-process line-JSON client. One long-lived helper, respawned after any
//! failure (timeout, crash, protocol error); the helper keeps no state that
//! matters across restarts because the cursor lives in the ledger.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};

use super::wire::{Response, PROTOCOL_VERSION};
use crate::error::{SyncError, SyncResult};

/// Largest accepted response line. A line over the cap kills the helper.
const MAX_LINE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HelperConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    /// Per-request deadline (donor: 45 s; CloudKit batches can be slow).
    pub request_timeout: Duration,
}

impl HelperConfig {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            request_timeout: Duration::from_secs(120),
        }
    }
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.envs.push((k.into(), v.into()));
        self
    }
    pub fn timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }
    /// Arguments the real `rightkit-cloudkit-helper` needs.
    pub fn cloudkit(self, container: &str, zone: &str, subscription: &str) -> Self {
        self.arg("--container")
            .arg(container)
            .arg("--zone")
            .arg(zone)
            .arg("--subscription")
            .arg(subscription)
    }
}

struct Live {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Result<String, String>>,
}

pub struct HelperProcess {
    cfg: HelperConfig,
    live: Mutex<Option<Live>>,
    next_id: AtomicU64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    spawns: AtomicU64,
}

impl HelperProcess {
    pub fn new(cfg: HelperConfig) -> Self {
        Self {
            cfg,
            live: Mutex::new(None),
            next_id: AtomicU64::new(1),
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            spawns: AtomicU64::new(0),
        }
    }

    /// How many helper processes were started (observability and tests).
    pub fn spawn_count(&self) -> u64 {
        self.spawns.load(Ordering::Relaxed)
    }

    fn spawn(&self) -> SyncResult<Live> {
        let mut cmd = Command::new(&self.cfg.program);
        cmd.args(&self.cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.cfg.envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            SyncError::Unavailable(format!(
                "sync helper {} could not start: {e}",
                self.cfg.program.display()
            ))
        })?;
        self.spawns.fetch_add(1, Ordering::Relaxed);
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader
                    .by_ref()
                    .take(MAX_LINE_BYTES as u64 + 1)
                    .read_line(&mut line)
                {
                    Ok(0) => {
                        let _ = tx.send(Err("helper closed stdout".to_string()));
                        return;
                    }
                    Ok(_) if line.len() > MAX_LINE_BYTES => {
                        let _ = tx.send(Err("helper response line too large".to_string()));
                        return;
                    }
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
        });
        let tail = Arc::clone(&self.stderr_tail);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = tail.lock().unwrap();
                if t.len() >= 20 {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });
        Ok(Live {
            child,
            stdin,
            lines: rx,
        })
    }

    fn stderr_summary(&self) -> String {
        let t = self.stderr_tail.lock().unwrap();
        t.back().cloned().unwrap_or_default()
    }

    fn kill(live: Option<Live>) {
        if let Some(mut l) = live {
            let _ = l.child.kill();
            let _ = l.child.wait();
        }
    }

    /// Stop the helper (also happens on drop).
    pub fn shutdown(&self) {
        let live = self.live.lock().unwrap().take();
        Self::kill(live);
    }

    /// One request, one response. Transport-level failures (spawn, write,
    /// timeout, EOF, malformed line) kill the helper and surface as retryable
    /// errors; helper-reported errors come back as `Ok(Response{ok:false})`
    /// mapped by the caller.
    pub fn call(&self, cmd: &str, params: Map<String, Value>) -> SyncResult<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut req = params;
        req.insert("v".into(), PROTOCOL_VERSION.into());
        req.insert("id".into(), id.into());
        req.insert("cmd".into(), cmd.into());
        let line = Value::Object(req).to_string();

        let mut guard = self.live.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.spawn()?);
        }
        let outcome = (|| -> Result<Response, String> {
            let live = guard.as_mut().expect("spawned");
            live.stdin
                .write_all(line.as_bytes())
                .and_then(|()| live.stdin.write_all(b"\n"))
                .and_then(|()| live.stdin.flush())
                .map_err(|e| format!("write to helper failed: {e}"))?;
            let deadline = std::time::Instant::now() + self.cfg.request_timeout;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match live.lines.recv_timeout(left) {
                    Ok(Ok(text)) => {
                        let resp: Response = serde_json::from_str(text.trim())
                            .map_err(|e| format!("malformed helper response: {e}"))?;
                        if resp.id == id {
                            return Ok(resp);
                        }
                        // Stale id from a request we already abandoned.
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(format!(
                            "helper timed out after {:?}",
                            self.cfg.request_timeout
                        ))
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err("helper reader stopped".to_string())
                    }
                }
            }
        })();
        match outcome {
            Ok(resp) => Ok(resp),
            Err(why) => {
                Self::kill(guard.take());
                let tail = self.stderr_summary();
                let detail = if tail.is_empty() {
                    why
                } else {
                    format!("{why} ({tail})")
                };
                Err(SyncError::RetryAfter { secs: 15.0, detail })
            }
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        let live = self.live.get_mut().ok().and_then(Option::take);
        Self::kill(live);
    }
}
