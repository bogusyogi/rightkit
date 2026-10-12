//! Windows-only trusted CDP transport.
//!
//! The sandbox worker creates both named-pipe servers with an owned security
//! descriptor, `FILE_FLAG_OVERLAPPED`, & non-inheritable handles. This bridge
//! only adopts those handles; it never names or creates a pipe endpoint.

use crate::error::{BrowserError, Result};
use async_tungstenite::tokio::accept_hdr_async_with_config;
use async_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    http::{Response as HttpResponse, StatusCode},
    protocol::{Message, WebSocketConfig},
};
use futures::StreamExt;
use std::ffi::c_void;
use std::io;
use std::net::{IpAddr, Ipv4Addr, TcpListener as StdTcpListener};
use std::os::windows::io::{AsRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{windows::named_pipe::NamedPipeServer, TcpListener};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const MAX_JSON_BYTES: usize = 24 * 1024 * 1024;
const WS_BUFFER_BYTES: usize = 64 * 1024;
const ERROR_NOT_FOUND: i32 = 1168;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;

#[derive(Clone, Copy, Eq, PartialEq)]
enum ShutdownPhase {
    Running,
    Quiescing,
    Finalizing,
}

#[link(name = "kernel32")]
extern "system" {
    fn CancelIoEx(handle: *mut c_void, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(handle: *mut c_void) -> i32;
}

type TokioWebSocket = async_tungstenite::WebSocketStream<
    async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>,
>;
type TokioWebSocketSender = async_tungstenite::WebSocketSender<
    async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>,
>;
type TokioWebSocketReceiver = async_tungstenite::WebSocketReceiver<
    async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>,
>;

/// The private loopback endpoint which connects chromiumoxide to Chrome's
/// inherited DevTools pipe pair.
pub(crate) struct PipeBridge {
    websocket_url: String,
    failure: Arc<Mutex<Option<String>>>,
    shutdown: watch::Sender<ShutdownPhase>,
    task: Option<JoinHandle<()>>,
}

impl PipeBridge {
    /// Adopt the sandbox-created pipe servers & start one trusted loopback
    /// WebSocket relay. `to_child` carries client-to-Chrome JSON; `from_child`
    /// carries Chrome-to-client JSON.
    pub(crate) fn start(
        to_child: OwnedHandle,
        from_child: OwnedHandle,
        deadline: Instant,
    ) -> Result<Self> {
        if Instant::now() >= deadline {
            return Err(BrowserError::Timeout(
                "Windows CDP pipe bridge startup".into(),
            ));
        }

        let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let listener = TcpListener::from_std(listener)?;

        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|error| BrowserError::Launch(format!("CDP bridge random source: {error}")))?;
        let secret = hex(&secret);
        let path = format!("/{secret}");
        let websocket_url = format!("ws://127.0.0.1:{port}{path}");

        // SAFETY: sandbox worker guarantees these are owned, overlapped named-pipe
        // server handles. `into_raw_handle` transfers each close obligation exactly
        // once to Tokio; this bridge has no authority to create alternate endpoints.
        let to_child = unsafe { NamedPipeServer::from_raw_handle(to_child.into_raw_handle()) }?;
        // SAFETY: same worker guarantee as `to_child` above.
        let from_child = unsafe { NamedPipeServer::from_raw_handle(from_child.into_raw_handle()) }?;

        let failure = Arc::new(Mutex::new(None));
        let (shutdown, shutdown_rx) = watch::channel(ShutdownPhase::Running);
        let task_failure = failure.clone();
        let task = tokio::spawn(async move {
            if let Err(reason) =
                run_bridge(listener, to_child, from_child, path, deadline, shutdown_rx).await
            {
                remember_failure(&task_failure, reason);
            }
        });

        Ok(Self {
            websocket_url,
            failure,
            shutdown,
            task: Some(task),
        })
    }

    /// This value stays crate-private so an arbitrary caller cannot advertise
    /// the secret-bearing endpoint.
    pub(crate) fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    /// First terminal transport failure, retained for session diagnostics.
    pub(crate) fn failure_reason(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Stop admitting bridge I/O. Integration must prove its Job empty before
    /// calling `shutdown`, which cancels endpoint I/O while task ownership holds.
    pub(crate) fn begin_shutdown(&self) {
        let _ = self.shutdown.send(ShutdownPhase::Quiescing);
    }

    /// Cancel endpoint I/O, then join its owner before `deadline`. Success
    /// proves transport inertness; Tokio may finish inert native callbacks later.
    pub(crate) async fn shutdown(&mut self, deadline: Instant) -> Result<()> {
        self.begin_shutdown();
        let _ = self.shutdown.send(ShutdownPhase::Finalizing);
        let Some(mut task) = self.task.take() else {
            return self
                .failure_reason()
                .map_or(Ok(()), |reason| Err(bridge_error(&reason)));
        };

        let result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut task).await;
        match result {
            Ok(Ok(())) => self
                .failure_reason()
                .map_or(Ok(()), |reason| Err(bridge_error(&reason))),
            Ok(Err(error)) => {
                let reason = format!("relay task failed: {error}");
                remember_failure(&self.failure, reason.clone());
                Err(bridge_error(&reason))
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                let reason = "shutdown deadline expired".to_string();
                remember_failure(&self.failure, reason.clone());
                Err(bridge_error(&reason))
            }
        }
    }
}

impl Drop for PipeBridge {
    fn drop(&mut self) {
        let _ = self.shutdown.send(ShutdownPhase::Finalizing);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run_bridge(
    mut listener: TcpListener,
    mut to_child: NamedPipeServer,
    mut from_child: NamedPipeServer,
    expected_path: String,
    deadline: Instant,
    mut shutdown: watch::Receiver<ShutdownPhase>,
) -> std::result::Result<(), String> {
    let websocket = loop {
        if is_stopping(&shutdown) {
            break None;
        }
        let accepted = tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                break None;
            }
            accepted = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                listener.accept(),
            ) => match accepted {
                Ok(accepted) => accepted,
                Err(_) if is_stopping(&shutdown) => break None,
                Err(_) => return Err("deadline expired while waiting for CDP client".into()),
            },
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(_) if is_stopping(&shutdown) => break None,
            Err(error) => return Err(format!("CDP listener failed: {error}")),
        };
        if is_stopping(&shutdown) {
            break None;
        }
        if peer.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            continue;
        }

        let path = expected_path.clone();
        let config = websocket_config();
        let handshake = accept_hdr_async_with_config(
            stream,
            move |request: &Request, response: Response| {
                let expected_upgrade =
                    request.method() == async_tungstenite::tungstenite::http::Method::GET
                        && request.headers().get("upgrade").is_some_and(|value| {
                            value.as_bytes().eq_ignore_ascii_case(b"websocket")
                        });
                let right_path = request
                    .uri()
                    .path_and_query()
                    .is_some_and(|value| value.as_str() == path);
                // chromiumoxide's string URL client sends no Origin. A supplied
                // Origin therefore cannot belong to this fixed caller.
                let no_origin = request.headers().get("origin").is_none();
                if expected_upgrade && right_path && no_origin {
                    Ok(response)
                } else {
                    Err(HttpResponse::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(Some("forbidden".to_string()))
                        .expect("fixed HTTP response is valid"))
                }
            },
            Some(config),
        );
        tokio::pin!(handshake);
        let websocket = tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                break None;
            }
            result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut handshake) => {
                match result {
                    Ok(Ok(websocket)) => websocket,
                    // A rejected or malformed unauthenticated attempt has no
                    // access to either pipe. Keep listener available until an
                    // authenticated client arrives or its deadline expires.
                    Ok(Err(_)) => continue,
                    Err(_) if is_stopping(&shutdown) => break None,
                    Err(_) => return Err("deadline expired during CDP handshake".into()),
                }
            }
        };
        break Some(websocket);
    };

    // One authenticated client owns this bridge. Dropping listener permanently
    // closes its endpoint before any pipe traffic starts.
    drop(listener);
    if let Some(websocket) = websocket {
        relay(
            websocket,
            &mut to_child,
            &mut from_child,
            deadline,
            shutdown.clone(),
        )
        .await?;
    }

    await_finalization(&mut shutdown).await?;
    cancel_and_disconnect(&to_child, &from_child)
}

async fn relay(
    websocket: TokioWebSocket,
    to_child: &mut NamedPipeServer,
    from_child: &mut NamedPipeServer,
    deadline: Instant,
    shutdown: watch::Receiver<ShutdownPhase>,
) -> std::result::Result<(), String> {
    let (websocket_out, websocket_in) = websocket.split();
    let websocket_to_pipe =
        relay_websocket_to_pipe(websocket_in, to_child, deadline, shutdown.clone());
    let pipe_to_websocket =
        relay_pipe_to_websocket(from_child, websocket_out, deadline, shutdown.clone());
    tokio::pin!(websocket_to_pipe);
    tokio::pin!(pipe_to_websocket);

    let result = tokio::select! {
        result = &mut websocket_to_pipe => result,
        result = &mut pipe_to_websocket => result,
    };
    if result.is_err() && is_stopping(&shutdown) {
        Ok(())
    } else {
        result
    }
}

async fn relay_websocket_to_pipe(
    mut websocket_in: TokioWebSocketReceiver,
    to_child: &mut NamedPipeServer,
    deadline: Instant,
    mut shutdown: watch::Receiver<ShutdownPhase>,
) -> std::result::Result<(), String> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                return Ok(());
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                return Err("deadline expired during CDP relay".into());
            }
            incoming = websocket_in.next() => {
                if is_stopping(&shutdown) {
                    return Ok(());
                }
                let incoming = incoming
                    .ok_or_else(|| "CDP WebSocket disconnected".to_string())
                    .and_then(|message| message.map_err(|error| format!("CDP WebSocket error: {error}")))?;
                let Message::Text(json) = incoming else {
                    return Err("CDP WebSocket sent a non-text message".into());
                };
                validate_json(json.as_str())?;
                let write = write_pipe_message(to_child, json.as_bytes(), deadline);
                tokio::pin!(write);
                tokio::select! {
                    changed = shutdown.changed() => {
                        let _ = changed;
                        return Ok(());
                    }
                    result = &mut write => result?,
                }
            }
        }
    }
}

async fn relay_pipe_to_websocket(
    from_child: &mut NamedPipeServer,
    mut websocket_out: TokioWebSocketSender,
    deadline: Instant,
    mut shutdown: watch::Receiver<ShutdownPhase>,
) -> std::result::Result<(), String> {
    let mut pipe_buffer = Vec::with_capacity(WS_BUFFER_BYTES);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                return Ok(());
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                return Err("deadline expired during CDP relay".into());
            }
            outgoing = read_pipe_message(from_child, &mut pipe_buffer) => {
                if is_stopping(&shutdown) {
                    return Ok(());
                }
                let json = outgoing?;
                validate_json(&json)?;
                let send = websocket_out.send(Message::text(json));
                tokio::pin!(send);
                tokio::select! {
                    changed = shutdown.changed() => {
                        let _ = changed;
                        return Ok(());
                    }
                    result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut send) => {
                        result
                            .map_err(|_| "deadline expired writing CDP WebSocket".to_string())?
                            .map_err(|error| format!("CDP WebSocket write failed: {error}"))?;
                    }
                }
            }
        }
    }
}

fn is_stopping(shutdown: &watch::Receiver<ShutdownPhase>) -> bool {
    *shutdown.borrow() != ShutdownPhase::Running
}

async fn await_finalization(
    shutdown: &mut watch::Receiver<ShutdownPhase>,
) -> std::result::Result<(), String> {
    loop {
        if *shutdown.borrow_and_update() == ShutdownPhase::Finalizing {
            return Ok(());
        }
        shutdown
            .changed()
            .await
            .map_err(|_| "CDP bridge shutdown coordinator dropped".to_string())?;
    }
}

fn cancel_and_disconnect(
    to_child: &NamedPipeServer,
    from_child: &NamedPipeServer,
) -> std::result::Result<(), String> {
    let mut failure = None;
    for handle in [to_child.as_raw_handle(), from_child.as_raw_handle()] {
        if let Err(error) = cancel_pipe_io(handle) {
            failure.get_or_insert(error);
        }
    }
    for handle in [to_child.as_raw_handle(), from_child.as_raw_handle()] {
        if let Err(error) = disconnect_pipe(handle) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

fn cancel_pipe_io(handle: RawHandle) -> std::result::Result<(), String> {
    // SAFETY: `run_bridge` owns each NamedPipeServer until this function
    // returns, so each raw handle remains valid for CancelIoEx.
    let canceled = unsafe { CancelIoEx(handle.cast(), std::ptr::null_mut()) };
    if canceled != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NOT_FOUND) {
        Ok(())
    } else {
        Err(format!("CDP pipe I/O cancellation failed: {error}"))
    }
}

fn disconnect_pipe(handle: RawHandle) -> std::result::Result<(), String> {
    // SAFETY: `run_bridge` owns each NamedPipeServer until this function
    // returns, so each raw handle remains valid for DisconnectNamedPipe.
    let disconnected = unsafe { DisconnectNamedPipe(handle.cast()) };
    if disconnected != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED) {
        Ok(())
    } else {
        Err(format!("CDP pipe disconnect failed: {error}"))
    }
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(WS_BUFFER_BYTES)
        .write_buffer_size(WS_BUFFER_BYTES)
        .max_write_buffer_size(MAX_JSON_BYTES + WS_BUFFER_BYTES)
        .max_message_size(Some(MAX_JSON_BYTES))
        .max_frame_size(Some(MAX_JSON_BYTES))
}

async fn write_pipe_message(
    pipe: &mut NamedPipeServer,
    json: &[u8],
    deadline: Instant,
) -> std::result::Result<(), String> {
    if json.len() > MAX_JSON_BYTES {
        return Err("CDP JSON message exceeds 24 MiB".into());
    }
    let write = async {
        pipe.write_all(json).await?;
        pipe.write_all(&[0]).await
    };
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), write)
        .await
        .map_err(|_| "deadline expired writing CDP pipe".to_string())?
        .map_err(|error| format!("CDP pipe write failed: {error}"))
}

async fn read_pipe_message(
    pipe: &mut NamedPipeServer,
    buffered: &mut Vec<u8>,
) -> std::result::Result<String, String> {
    loop {
        if let Some(end) = buffered.iter().position(|byte| *byte == 0) {
            if end > MAX_JSON_BYTES {
                return Err("CDP JSON message exceeds 24 MiB".into());
            }
            let message = buffered.drain(..end).collect::<Vec<_>>();
            buffered.drain(..1);
            return String::from_utf8(message)
                .map_err(|_| "CDP pipe JSON is not UTF-8".to_string());
        }
        if buffered.len() > MAX_JSON_BYTES {
            return Err("CDP JSON message exceeds 24 MiB".into());
        }
        let mut chunk = [0u8; WS_BUFFER_BYTES];
        let read = pipe.read(&mut chunk).await.map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                "CDP pipe reached EOF".to_string()
            } else {
                format!("CDP pipe read failed: {error}")
            }
        })?;
        if read == 0 {
            return Err("CDP pipe reached EOF".into());
        }
        buffered.extend_from_slice(&chunk[..read]);
    }
}

fn validate_json(value: &str) -> std::result::Result<(), String> {
    if value.len() > MAX_JSON_BYTES {
        return Err("CDP JSON message exceeds 24 MiB".into());
    }
    if value.as_bytes().contains(&0) {
        return Err("CDP JSON contains an embedded NUL".into());
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|_| "CDP relay received invalid JSON".into())
}

fn remember_failure(failure: &Arc<Mutex<Option<String>>>, reason: String) {
    let mut stored = failure
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if stored.is_none() {
        *stored = Some(reason);
    }
}

fn bridge_error(reason: &str) -> BrowserError {
    BrowserError::Cdp(format!("Windows CDP pipe bridge: {reason}"))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
