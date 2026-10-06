use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_framed_sidecar::{read_json_line, write_json_line};
use rightkit_process::{OwnedChild, OwnedCommand};
use serde_json::{json, Value};

use crate::contract::DEFAULT_FRAME_LIMIT;
use crate::error::{JobError, JobResult};

type OnEvent = Box<dyn Fn(&Value) + Send>;
struct Pending {
    tx: Sender<JobResult<Value>>,
    on_event: Option<OnEvent>,
}
type PendingMap = Arc<Mutex<HashMap<u64, Pending>>>;

/// Shell-side client for an engine speaking the stdio contract. Multiplexes
/// concurrent calls; events of a call stream to its callback before it returns.
/// Restart/health policy stays with `rightkit-process`; this only talks frames.
pub struct EngineClient {
    writer: Mutex<Box<dyn Write + Send>>,
    pending: PendingMap,
    next_id: AtomicU64,
    hello: Value,
    child: Mutex<Option<OwnedChild>>,
}

impl EngineClient {
    /// Connect over an existing duplex; reads the engine's hello frame first.
    pub fn connect(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> JobResult<Self> {
        let mut reader = reader;
        let mut buf = Vec::new();
        let hello: Value = read_json_line(&mut reader, &mut buf, DEFAULT_FRAME_LIMIT)
            .map_err(|e| JobError::new("engine_protocol", e.to_string()))?
            .ok_or_else(|| JobError::new("engine_exited", "engine closed before hello"))?;
        if hello["type"] != "hello" {
            return Err(JobError::new("engine_protocol", "first frame was not hello"));
        }
        let pending: PendingMap = Arc::default();
        let map = Arc::clone(&pending);
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            loop {
                match read_json_line::<_, Value>(&mut reader, &mut buf, DEFAULT_FRAME_LIMIT) {
                    Ok(Some(v)) => dispatch(&map, &v),
                    Ok(None) | Err(rightkit_framed_sidecar::FrameError::Io(_)) => break,
                    Err(_) => continue, // resynchronised on the next frame
                }
            }
            for (_, p) in map.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                let _ = p.tx.send(Err(JobError::new("engine_exited", "engine closed the connection")));
            }
        });
        Ok(Self {
            writer: Mutex::new(Box::new(writer)),
            pending,
            next_id: AtomicU64::new(1),
            hello,
            child: Mutex::new(None),
        })
    }

    /// Spawn an engine child (stdin/stdout are piped here) and connect.
    pub fn spawn(mut cmd: OwnedCommand) -> JobResult<Self> {
        use std::process::Stdio;
        cmd.command_mut().stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdin = child.take_stdin().ok_or_else(|| JobError::new("io", "no child stdin"))?;
        let stdout = child.take_stdout().ok_or_else(|| JobError::new("io", "no child stdout"))?;
        let client = Self::connect(BufReader::new(stdout), stdin)?;
        *client.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        Ok(client)
    }

    pub fn hello(&self) -> &Value {
        &self.hello
    }

    pub fn call(&self, method: &str, params: Value) -> JobResult<Value> {
        self.call_with_events(method, params, None)
    }

    pub fn call_with_events(&self, method: &str, params: Value, on_event: Option<OnEvent>) -> JobResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, Pending { tx, on_event });
        let frame = json!({"type": "request", "id": id, "method": method, "params": params});
        let sent = {
            let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            write_json_line(&mut *w, &frame, DEFAULT_FRAME_LIMIT)
                .map_err(|e| JobError::new("engine_write", e.to_string()))
                .and_then(|_| w.flush().map_err(JobError::from))
        };
        if let Err(e) = sent {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(e);
        }
        rx.recv().unwrap_or_else(|_| Err(JobError::new("engine_exited", "engine closed the connection")))
    }

    /// Ask the engine to stop, then terminate an owned child that outlives `grace`.
    pub fn shutdown(&self, grace: Duration) {
        let _ = self.call("shutdown", json!({}));
        if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.wait_or_kill(grace);
        }
    }
}

fn dispatch(map: &PendingMap, v: &Value) {
    let Some(id) = v["id"].as_u64() else { return };
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    match v["type"].as_str() {
        Some("event") => {
            if let Some(cb) = m.get(&id).and_then(|p| p.on_event.as_ref()) {
                cb(&v["event"]);
            }
        }
        Some("response") => {
            if let Some(p) = m.remove(&id) {
                let r = if v["ok"] == true {
                    Ok(v["result"].clone())
                } else {
                    Err(JobError::new(
                        v["error"]["code"].as_str().unwrap_or("error"),
                        v["error"]["message"].as_str().unwrap_or(""),
                    ))
                };
                let _ = p.tx.send(r);
            }
        }
        _ => {}
    }
}
