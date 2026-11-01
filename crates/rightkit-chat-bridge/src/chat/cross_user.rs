//! Linux-only, root-configured chat boundary for a website user's sessions and a separate the host app
//! engine uid. RightKit's resident transport deliberately accepts only its own uid. This adapter
//! extends that boundary for four chat methods, without exposing the resident socket or keys.
//! Session discovery, hooks and transcripts stay under their original uid and HOME.

use super::mesh;
use serde_json::Value;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    fn config_dir() -> &'static str {
        &crate::chat::brand::brand().bridge_config_dir
    }
    fn config_path() -> String {
        format!("{}/chat-bridge.json", config_dir())
    }
    fn socket_path() -> &'static str {
        &crate::chat::brand::brand().bridge_socket
    }
    fn cli_path() -> &'static str {
        &crate::chat::brand::brand().bridge_cli
    }
    const CAP: usize = 2 * 1024 * 1024;

    #[derive(Deserialize)]
    pub struct Config {
        pub engine_uid: u32,
        pub chat_uid: u32,
    }

    pub fn config() -> Result<Option<Config>, String> {
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(config_path())
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let parent = std::fs::symlink_metadata(config_dir()).map_err(|e| e.to_string())?;
        if !meta.is_file()
            || meta.uid() != 0
            || meta.mode() & 0o022 != 0
            || meta.len() > 4096
            || !parent.is_dir()
            || parent.uid() != 0
            || parent.mode() & 0o022 != 0
        {
            return Err(
                "chat bridge config must be root-owned and not group/world writable".into(),
            );
        }
        let cfg: Config = serde_json::from_reader(file).map_err(|e| e.to_string())?;
        if cfg.engine_uid == 0 || cfg.chat_uid == 0 || cfg.engine_uid == cfg.chat_uid {
            return Err("chat bridge needs two distinct non-root uids".into());
        }
        Ok(Some(cfg))
    }

    fn uid() -> u32 {
        unsafe { libc::geteuid() }
    }
    fn peer_uid(stream: &UnixStream) -> Result<u32, String> {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        if rc != 0 || size as usize != std::mem::size_of::<libc::ucred>() {
            return Err("cannot authenticate chat bridge peer".into());
        }
        Ok(cred.uid)
    }

    fn allowed(method: &str) -> bool {
        matches!(
            method,
            mesh::PEERS | mesh::DELIVER | mesh::SAID | mesh::RECEIPT
        )
    }
    pub fn allowed_engine(method: &str, params: &Value) -> bool {
        method == "device.chat.links"
            || (method == "device.chat.call"
                && params
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(allowed))
    }

    fn write_frame(stream: &mut UnixStream, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > CAP {
            return Err("chat bridge response too large".into());
        }
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .map_err(|e| e.to_string())?;
        stream.write_all(&bytes).map_err(|e| e.to_string())
    }
    fn read_frame(stream: &mut UnixStream) -> Result<Value, String> {
        let mut length = [0; 4];
        stream.read_exact(&mut length).map_err(|e| e.to_string())?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > CAP {
            return Err("chat bridge frame too large".into());
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
    fn deadlines(stream: &UnixStream) -> Result<(), String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(20)))
            .map_err(|e| e.to_string())
    }

    /// Engine-only: the authenticated mesh principal crosses the local uid boundary intact.
    pub fn forward(
        caller: &mesh::Caller,
        method: &str,
        params: &Value,
    ) -> Result<Option<Value>, String> {
        let Some(cfg) = config()? else {
            return Ok(None);
        };
        if uid() != cfg.engine_uid {
            return Ok(None);
        }
        if !allowed(method) {
            return Err("chat method refused".into());
        }
        let mut stream = UnixStream::connect(socket_path())
            .map_err(|e| format!("chat bridge unavailable: {e}"))?;
        if peer_uid(&stream)? != cfg.chat_uid {
            return Err("wrong chat bridge uid".into());
        }
        deadlines(&stream)?;
        write_frame(
            &mut stream,
            &json!({ "caller": caller, "method": method, "params": params }),
        )?;
        let response = read_frame(&mut stream)?;
        if response["ok"] != true {
            return Err(response["error"]
                .as_str()
                .unwrap_or("chat bridge refused")
                .to_string());
        }
        Ok(Some(response["result"].clone()))
    }

    /// Website-user CLI: only an immutable sudo target may cross into the engine uid.
    pub fn engine_call(method: &str, params: &Value) -> Result<Option<Value>, String> {
        let Some(cfg) = config()? else {
            return Ok(None);
        };
        if uid() != cfg.chat_uid {
            return Ok(None);
        }
        if !allowed_engine(method, params) {
            return Err("cross-user broker permits chat transport only".into());
        }
        let bytes = serde_json::to_vec(&json!({ "method": method, "params": params }))
            .map_err(|e| e.to_string())?;
        if bytes.len() > CAP {
            return Err("chat request too large".into());
        }
        let who = format!("#{}", cfg.engine_uid);
        let result = rightkit_child_process::run_with_stdin(
            "/usr/bin/sudo",
            [
                "-n",
                "-H",
                "-u",
                who.as_str(),
                "--",
                cli_path(),
                "chat",
                "bridge-engine",
            ],
            &bytes,
            &rightkit_child_process::RunOptions::new(Duration::from_secs(80)).stdout_cap(CAP),
        )
        .map_err(|e| format!("chat broker outcome unknown: {e}"))?;
        if result.timed_out || !result.status.success() {
            return Err("chat broker outcome unknown; check delivery before retrying".into());
        }
        let response: Value = serde_json::from_slice(&result.stdout.bytes)
            .map_err(|_| "chat broker outcome unknown")?;
        if response["ok"] != true {
            return Err(response["error"]
                .as_str()
                .unwrap_or("chat broker refused")
                .to_string());
        }
        Ok(Some(response["result"].clone()))
    }

    /// The sudo target validates again after changing uid. It accepts no CLI arguments or file paths.
    pub fn broker_request() -> Result<(String, Value), String> {
        let cfg = config()?.ok_or("chat bridge not configured")?;
        if uid() != cfg.engine_uid {
            return Err("wrong broker uid".into());
        }
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((CAP + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > CAP {
            return Err("chat request too large".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let method = value["method"].as_str().ok_or("missing method")?;
        if !allowed_engine(method, &value["params"]) {
            return Err("chat method refused".into());
        }
        Ok((method.to_string(), value["params"].clone()))
    }

    /// Runs under the original session uid; never opens the engine's state or credentials.
    pub fn serve() -> Result<(), String> {
        let cfg = config()?.ok_or("chat bridge not configured")?;
        if uid() != cfg.chat_uid {
            return Err("wrong session uid".into());
        }
        // systemd owns the lifetime and creates /run/pulse-chat; never unlink an arbitrary path.
        match std::fs::symlink_metadata(socket_path()) {
            Ok(meta) => {
                use std::os::unix::fs::FileTypeExt;
                if !meta.file_type().is_socket() || meta.uid() != uid() {
                    return Err("unsafe bridge socket".into());
                }
                if UnixStream::connect(socket_path()).is_ok() {
                    return Err("chat bridge already running".into());
                }
                std::fs::remove_file(socket_path()).map_err(|e| e.to_string())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let listener = UnixListener::bind(socket_path()).map_err(|e| e.to_string())?;
        std::fs::set_permissions(socket_path(), std::fs::Permissions::from_mode(0o660))
            .map_err(|e| e.to_string())?;
        std::thread::spawn(|| {
            loop {
                if let Ok(store) = super::super::Store::open_default() {
                    super::super::receipts::background(&store);
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        let inflight = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            if peer_uid(&stream).ok() != Some(cfg.engine_uid) {
                continue;
            }
            if inflight.fetch_add(1, Ordering::AcqRel) >= 8 {
                inflight.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let count = inflight.clone();
            std::thread::spawn(move || {
                struct Slot(Arc<AtomicUsize>);
                impl Drop for Slot {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _slot = Slot(count);
                let result = (|| -> Result<Value, String> {
                    deadlines(&stream)?;
                    let request = read_frame(&mut stream)?;
                    let method = request["method"].as_str().ok_or("missing method")?;
                    if !allowed(method) {
                        return Err("chat method refused".into());
                    }
                    let caller: mesh::Caller = serde_json::from_value(request["caller"].clone())
                        .map_err(|e| e.to_string())?;
                    let store = super::super::Store::open_default().map_err(|e| e.to_string())?;
                    mesh::serve(&store, &caller, method, &request["params"]).map_err(|e| e.message)
                })();
                let response = match result {
                    Ok(value) => json!({ "ok": true, "result": value }),
                    Err(error) => json!({ "ok": false, "error": error }),
                };
                let _ = write_frame(&mut stream, &response);
            });
        }
        Err("chat bridge listener stopped".into())
    }
}

#[cfg(target_os = "linux")]
pub use linux::{broker_request, engine_call, forward, serve};
#[cfg(not(target_os = "linux"))]
pub fn forward(_: &mesh::Caller, _: &str, _: &Value) -> Result<Option<Value>, String> {
    Ok(None)
}
#[cfg(not(target_os = "linux"))]
pub fn engine_call(_: &str, _: &Value) -> Result<Option<Value>, String> {
    Ok(None)
}
