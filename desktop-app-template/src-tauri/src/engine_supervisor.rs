//! Owns the `rightapp-engine` sidecar: ownership-safe spawn (rightkit-process), bounded framed
//! stdio RPC (protocol crate). Restart policy is intentionally minimal here; apps needing crash
//! recovery adopt `rightkit_process::RestartTracker` (see ScrapeRight `engine_supervisor.rs`
//! for the mature version - an extraction candidate for `rightkit-jobs`/a supervisor crate).

use std::io::{BufReader, BufWriter};
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::time::Duration;

use rightapp_protocol::{framed, Request, Response, MAX_FRAME_BYTES};
use rightkit_process::{OwnedChild, OwnedCommand};
use serde_json::Value;
use tauri::{AppHandle, Manager};

pub struct EngineSupervisor {
    child: OwnedChild,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    buf: Vec<u8>,
    next_id: u64,
}

impl EngineSupervisor {
    pub fn spawn(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let path = engine_path(app)?;
        let mut command = OwnedCommand::new(path);
        command
            .command_mut()
            .arg("--stdio")
            .env("RIGHTAPP_APP_DATA", crate::app_data_root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command.windows_hide();
        let mut child = command.spawn()?;
        let stdin = BufWriter::new(child.take_stdin().ok_or("engine stdin unavailable")?);
        let stdout = BufReader::new(child.take_stdout().ok_or("engine stdout unavailable")?);
        Ok(Self { child, stdin, stdout, buf: Vec::new(), next_id: 1 })
    }

    pub fn call(&mut self, cmd: &str, input: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request { id, cmd: cmd.to_string(), input };
        framed::write_json_line(&mut self.stdin, &request, MAX_FRAME_BYTES).map_err(|e| e.to_string())?;
        loop {
            let response: Response = framed::read_json_line(&mut self.stdout, &mut self.buf, MAX_FRAME_BYTES)
                .map_err(|e| e.to_string())?
                .ok_or("engine closed stdout")?;
            if response.id != id {
                continue; // stale frame from a timed-out call
            }
            return match response.error {
                Some(e) => Err(format!("{}: {}", e.code, e.message)),
                None => Ok(response.result.unwrap_or(Value::Null)),
            };
        }
    }
}

impl Drop for EngineSupervisor {
    fn drop(&mut self) {
        // Closing stdin is the graceful shutdown signal; then bounded wait, then tree kill.
        let _ = self.child.wait_or_kill(Duration::from_secs(2));
    }
}

fn engine_path(app: &AppHandle) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
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
    let res = app.path().resource_dir()?.join(name);
    Ok(res)
}
