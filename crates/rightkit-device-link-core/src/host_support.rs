//! Helpers a device-link host needs around the protocol core, none of which depends on one app:
//!
//! * wiping secrets out of request lines and decoded requests ([`WipingReader`], [`read_request`],
//!   `wipe_*`);
//! * the stable refusal codes of a [`LinkError`] and of a pairing channel's end;
//! * the circle outbox entry that survives a restart;
//! * the relay a join token named, kept in a file beside the registry (`relay-learned.json`), and
//!   the relay recorded with the host;
//! * the relay admission state a computer keeps when it decides relay admission, and the group
//!   readable grant file;
//! * a byte pump between a local socket and a mesh tunnel;
//! * path-change summarising for the host's log.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use rightkit_device_link_types::PairedDevice;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::LinkError;
use super::transport::ChannelError;

// ---- request lines and secrets -----------------------------------------------------------

/// Read one bounded JSON line (`rightkit-sidecar`): at most `limit` payload bytes, an oversized
/// line is discarded as it streams (never buffered, discard itself capped at `limit`) and
/// answers `TooLarge`; a line that is not JSON is `Json`; `Ok(None)` is a clean close.
pub fn read_request<S: Read>(
    stream: &mut S,
    limit: u64,
) -> Result<Option<Value>, rightkit_sidecar::FrameError> {
    let limit = limit as usize;
    let mut buffer = Vec::new();
    let answer = rightkit_sidecar::read_json_line_with(
        &mut WipingReader::new(Read::by_ref(stream)),
        &mut buffer,
        limit,
        rightkit_sidecar::ReadLimits {
            max_discard: limit,
            deadline: None,
        },
    );
    // The line may hold a typed password: overwrite the raw line, capacity included, before it
    // is freed. The reader wipes its own buffer when dropped.
    wipe_capacity(&mut buffer);
    answer
}

/// Overwrite bytes with zeros in a way the compiler cannot drop as a dead store.
pub fn wipe_bytes(bytes: &mut [u8]) {
    rightkit_crypto::wipe(bytes);
}

/// Overwrite a string's bytes with zeros in place (it stays the same length, and zero bytes are
/// valid UTF-8, so it stays a valid string).
pub fn wipe_string(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    wipe_bytes(&mut bytes);
    // Zero bytes are valid UTF-8; the allocation is the one the secret was in.
    *text = String::from_utf8(bytes).unwrap_or_default();
}

/// Overwrite a vector's whole allocation, including bytes past its length, then empty it.
pub fn wipe_capacity(buffer: &mut Vec<u8>) {
    let capacity = buffer.capacity();
    // Within the capacity: no reallocation, so the whole allocation is the slice that is wiped.
    buffer.resize(capacity, 0);
    wipe_bytes(buffer.as_mut_slice());
    buffer.clear();
}

/// Overwrite the password a phone sent (`params.password`) in the decoded request, once the
/// request has been handled.
pub fn wipe_password(request: &mut Value) {
    if let Some(Value::String(password)) = request
        .get_mut("params")
        .and_then(|params| params.get_mut("password"))
    {
        wipe_string(password);
    }
}

/// Overwrite the refresh token a request carried (`params.refreshToken`), in the decoded request,
/// once it has been handled (best effort: the line buffer it was read from is not reachable here).
pub fn wipe_google_grant(request: &mut Value) {
    if let Some(params) = request.get_mut("params") {
        wipe_refresh_token(params);
    }
}

/// The same for a `params` object on its own.
pub fn wipe_refresh_token(params: &mut Value) {
    if let Some(Value::String(token)) = params.get_mut("refreshToken") {
        wipe_string(token);
    }
}

/// Overwrite the text a live view typed (`params.events[].text`) in the decoded request, once it
/// has been handled: typed text is applied and forgotten.
pub fn wipe_browser_text(request: &mut Value) {
    if let Some(params) = request.get_mut("params") {
        wipe_events(params);
    }
}

/// The same for a `params` object on its own (a copy taken out of the request).
pub fn wipe_events(params: &mut Value) {
    let Some(Value::Array(events)) = params.get_mut("events") else {
        return;
    };
    for event in events {
        if let Some(Value::String(text)) = event.get_mut("text") {
            wipe_string(text);
        }
    }
}

/// `BufReader` with a buffer that is overwritten when the reader is dropped: std's buffer would
/// be freed with the request line still in it.
pub struct WipingReader<R: Read> {
    inner: R,
    buffer: Vec<u8>,
    at: usize,
    filled: usize,
}

impl<R: Read> WipingReader<R> {
    pub fn new(inner: R) -> WipingReader<R> {
        WipingReader {
            inner,
            buffer: vec![0; 8192],
            at: 0,
            filled: 0,
        }
    }
}

impl<R: Read> Read for WipingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let taken = {
            let available = io::BufRead::fill_buf(self)?;
            let taken = available.len().min(out.len());
            out[..taken].copy_from_slice(&available[..taken]);
            taken
        };
        io::BufRead::consume(self, taken);
        Ok(taken)
    }
}

impl<R: Read> io::BufRead for WipingReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.at >= self.filled {
            self.filled = self.inner.read(&mut self.buffer)?;
            self.at = 0;
        }
        Ok(&self.buffer[self.at..self.filled])
    }

    fn consume(&mut self, amount: usize) {
        self.at = (self.at + amount).min(self.filled);
    }
}

impl<R: Read> Drop for WipingReader<R> {
    fn drop(&mut self) {
        wipe_bytes(&mut self.buffer);
    }
}

// ---- refusal codes -----------------------------------------------------------------------

/// The stable code of a protocol refusal, and whether trying again can help.
pub fn link_code(error: &LinkError) -> (&'static str, bool) {
    match error {
        LinkError::Random | LinkError::Vault(_) => ("vault_unavailable", true),
        LinkError::Store(_) => ("registry_unavailable", true),
        LinkError::Malformed(_) => ("malformed", false),
        LinkError::SchemaVersion(_) => ("schema_version", false),
        LinkError::WrongPhase => ("wrong_phase", false),
        LinkError::Expired => ("expired", false),
        LinkError::CommitmentMismatch => ("commitment_mismatch", false),
        LinkError::CodeMismatch => ("code_mismatch", false),
        LinkError::ApprovalMismatch => ("approval_mismatch", false),
        LinkError::SignatureInvalid => ("signature_invalid", false),
        LinkError::PairingRateLimited => ("pairing_rate_limited", true),
        LinkError::UnknownDevice => ("unknown_device", false),
        LinkError::Revoked => ("revoked", false),
        LinkError::PairingMismatch => ("pairing_mismatch", false),
        LinkError::RevisionConflict { .. } => ("revision_conflict", false),
        LinkError::NotGranted => ("not_granted", false),
        LinkError::GrantExpired => ("grant_expired", false),
        LinkError::Replay => ("replay", false),
        LinkError::ReplayStore(_) => ("journal_unavailable", true),
        LinkError::TlsPinMismatch => ("tls_pin_mismatch", false),
        LinkError::Busy => ("busy", true),
        LinkError::NotAuthorized => ("not_authorized", false),
        LinkError::ClockRolledBack => ("clock_rolled_back", false),
    }
}

/// The stable reason of a pairing channel's end. Never the error text: it can come from a peer.
pub fn channel_reason(error: &ChannelError) -> &'static str {
    match error {
        ChannelError::Link(e) => link_code(e).0,
        ChannelError::Io(_) => "connect_failed",
        ChannelError::Protocol(_) => "protocol",
        ChannelError::Timeout => "timeout",
        ChannelError::Expired => "expired",
        ChannelError::Declined => "declined",
        ChannelError::NoCode => "no_code",
        ChannelError::Hook(_) => "enrol_refused",
    }
}

/// The message that goes with [`channel_reason`]. A hook error is the host's own text (no
/// secret); the rest are fixed phrases.
pub fn channel_message(error: &ChannelError) -> String {
    match error {
        ChannelError::Hook(text) => text.clone(),
        other => other.to_string(),
    }
}

// ---- the circle outbox -------------------------------------------------------------------

/// The circle outbox (`circle-outbox.json` in the devicelink private directory): every circle
/// message waits there until its recipient took it, is retried whenever that recipient is next
/// reachable (a link-up) and at least every [`CIRCLE_OUTBOX_BACKSTOP`], and is dropped only after
/// delivery (or the recipient's `replay` answer), a refusal, a recipient or subject that is gone,
/// or its expiry. A removal is signed once with a thirty-day window so a member that is offline
/// for weeks still learns of it. Introductions and member lists keep a ten-minute window; they are
/// stored as intents and signed afresh from the current member list at each attempt, so one that
/// waited is re-issued on reconnect.
pub const CIRCLE_OUTBOX_BACKSTOP: Duration = Duration::from_secs(300);
/// A fresh enqueue that could not be delivered is retried this soon, until it is ten minutes old.
pub const CIRCLE_OUTBOX_QUICK: Duration = Duration::from_secs(30);
/// An intent (introduction, member list) is abandoned after this long undelivered.
pub const CIRCLE_INTENT_MAX_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// One circle message waiting for its recipient.
#[derive(Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: String,
    pub recipient: String,
    pub method: String,
    /// The admitted device (introduction) or the removed device (removal).
    pub subject: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    /// A removal's message, signed once when it was queued.
    pub signed: Option<Value>,
}

// ---- the relay a join token named --------------------------------------------------------

/// The file beside the registry that keeps the relay a join token named (`relay-learned.json`).
pub const LEARNED_RELAY_FILE: &str = "relay-learned.json";

/// An `https` URL of at most 256 characters: the same bound a relay setting and the join token
/// apply.
pub fn relay_url_ok(url: &str) -> bool {
    !url.is_empty()
        && url.len() <= 256
        && url
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

/// The relay kept in `relay-learned.json` under `root`, when the file holds a usable one. Whether
/// the host's own relay setting wins over it is the host's call.
pub fn read_learned_relay(root: &Path) -> Option<String> {
    let bytes = std::fs::read(root.join(LEARNED_RELAY_FILE)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let url = value.get("relayUrl")?.as_str()?.trim().to_string();
    relay_url_ok(&url).then_some(url)
}

/// Write `relay` to `relay-learned.json` under `root`, owner-only and atomically.
pub fn write_learned_relay(root: &Path, relay: &str) -> Result<(), String> {
    serde_json::to_vec(&json!({ "relayUrl": relay }))
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            rightkit_fs::ensure_private_dir(root).map_err(|e| e.to_string())?;
            rightkit_fs::atomic_write_private(&root.join(LEARNED_RELAY_FILE), &bytes)
                .map_err(|e| e.to_string())
        })
}

/// The relay recorded with the host this computer joined by token (`PairedDevice::relay` on the
/// host's record, written in the same registry write as the record itself), the fallback when
/// `relay-learned.json` is missing after a restart. Only a host this computer paired with
/// directly (not introduced), that is active and not a guest, with an `https` URL the same bound
/// as the token's; the newest such pairing wins. It is only where to dial: every peer is still
/// authenticated by its key.
pub fn host_record_relay(devices: &[PairedDevice]) -> Option<String> {
    devices
        .iter()
        .filter(|d| d.is_active() && !d.is_guest() && d.introduced_by.is_none())
        .filter_map(|d| {
            let relay = d.relay.as_deref()?.trim();
            relay_url_ok(relay).then(|| (d.paired_at_ms, relay.to_string()))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, relay)| relay)
}

// ---- relay admission state ---------------------------------------------------------------

/// The admitting computer's record of one guest link (`relay-admit-state.json`): who minted it,
/// its bootstrap key and until when the relay admits it, and the guest key that proved itself for
/// it and until when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdmitPairing {
    pub minted_by: String,
    pub pairing_id: String,
    pub bootstrap_key: String,
    pub bootstrap_until_ms: u64,
    pub lease_expires_at_ms: u64,
    #[serde(default)]
    pub guest_key: Option<String>,
    #[serde(default)]
    pub guest_until_ms: u64,
}

impl AdmitPairing {
    pub fn live(&self, now: u64) -> bool {
        now < self.bootstrap_until_ms || (self.guest_key.is_some() && now < self.guest_until_ms)
    }
}

/// Why the admission state could not be saved: a stable code and whether a retry can help.
#[derive(Clone, Debug)]
pub struct StateError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

/// The state file under `root` (`file_name` is the host's name for it); a missing or unreadable
/// file is empty.
pub fn admit_state_load(root: &Path, file_name: &str) -> Vec<AdmitPairing> {
    std::fs::read(root.join(file_name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Save the state file under `root`, owner-only and atomically.
pub fn admit_state_save(
    root: &Path,
    file_name: &str,
    state: &[AdmitPairing],
) -> Result<(), StateError> {
    let bytes = serde_json::to_vec(state).map_err(|e| StateError {
        code: "malformed",
        message: e.to_string(),
        retryable: false,
    })?;
    let unavailable = |e: rightkit_fs::FsError| StateError {
        code: "state_unavailable",
        message: e.to_string(),
        retryable: true,
    };
    rightkit_fs::ensure_private_dir(root).map_err(unavailable)?;
    rightkit_fs::atomic_write_private(&root.join(file_name), &bytes).map_err(unavailable)
}

/// Writes `text` to `path` through a temporary file and a rename, readable by the file's group
/// (mode 0640: the admission service's user is in the directory's group, never the world).
pub fn write_group_readable(path: &Path, text: &str) -> io::Result<()> {
    // The temp is created owner-only on Unix, so it is never more permissive than 0640 at any
    // instant. The mode is then set explicitly before the bytes go in: the create mode is masked by
    // the umask (0077 in a sandboxed unit gives 0600, which the admission service cannot read, and
    // every guest grant would silently admit nobody). Windows keeps the parent's inherited access.
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
            }
            file.write_all(text.as_bytes())
        },
    )
    .map_err(io::Error::from)
}

// ---- a byte pump between a socket and a tunnel -------------------------------------------

/// The reason code of the error that ended a tunnel session: the relay cap and budget say so, any
/// other failure is its io kind. A clean end is `None`.
pub fn tunnel_end_reason(error: Option<io::Error>) -> Option<String> {
    let error = error?;
    let text = error.to_string();
    Some(if error.kind() == io::ErrorKind::PermissionDenied {
        text.split(':').next().unwrap_or("denied").to_string()
    } else {
        format!("io_{:?}", error.kind())
    })
}

/// Copies bytes both ways between a local socket and the mesh stream until both directions end:
/// socket to mesh on a thread (the end of the socket's input finishes the stream's sending side),
/// mesh to socket here (the end of the stream shuts the socket's sending side). The first error
/// of either direction closes the socket, which ends the other. Returns the first error, if any.
pub fn pump_socket<R, W>(
    tcp: TcpStream,
    mut reader: R,
    mut writer: W,
    buffer_bytes: usize,
    thread_name: &str,
) -> Option<io::Error>
where
    R: Read,
    W: Write + Send + 'static,
{
    let mut socket_out = match tcp.try_clone() {
        Ok(socket) => socket,
        Err(error) => return Some(error),
    };
    let mut socket_in = tcp;
    let upstream = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || -> Option<io::Error> {
            let mut buf = vec![0u8; buffer_bytes];
            loop {
                match socket_in.read(&mut buf) {
                    Ok(0) => return None,
                    Ok(n) => {
                        if let Err(error) = writer.write_all(&buf[..n]) {
                            let _ = socket_in.shutdown(Shutdown::Both);
                            return Some(error);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Some(error),
                }
            }
        });
    let upstream = match upstream {
        Ok(handle) => handle,
        Err(error) => return Some(error),
    };
    let mut buf = vec![0u8; buffer_bytes];
    let mut down: Option<io::Error> = None;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = socket_out.shutdown(Shutdown::Write);
                break;
            }
            Ok(n) => {
                if let Err(error) = socket_out.write_all(&buf[..n]) {
                    down = Some(error);
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                down = Some(error);
                break;
            }
        }
    }
    if down.is_some() {
        let _ = socket_out.shutdown(Shutdown::Both);
    }
    let up = upstream.join().ok().flatten();
    down.or(up)
}

// ---- path summaries ----------------------------------------------------------------------

/// Window over which routine path churn is summarised.
const PATH_SUMMARY_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct PeerPathState {
    /// Kind and address (port stripped) of the last path change written as a path line.
    shown: Option<(&'static str, String)>,
    window_start: Option<Instant>,
    opened: u64,
    closed: u64,
    migrated: u64,
    dial: u64,
    accept: u64,
    kinds: std::collections::BTreeMap<&'static str, u64>,
    rtt_max_ms: u64,
}

/// One observation of a connection's selected path, as [`PathAggregate`] reads it. The transport
/// (`rightkit-mesh`'s `PathEvent`) implements it, so this crate names no transport type.
pub trait PathObservation {
    /// `nearby`, `direct`, `relay` or `unknown`.
    fn label(&self) -> &'static str;
    /// The selected path's remote address (`ip:port`), `relay` or empty while unknown.
    fn remote(&self) -> &str;
    fn rtt_ms(&self) -> u64;
    /// `dial` (we opened the connection) or `accept` (the peer opened it).
    fn side(&self) -> &'static str;
    /// `opened`, `migrated` or `closed`.
    fn change(&self) -> &'static str;
}

/// Decides which path observations are written and which are only counted.
#[derive(Default)]
pub struct PathAggregate {
    peers: std::collections::HashMap<String, PeerPathState>,
}

impl PathAggregate {
    /// Returns whether to write this event as its own line, and the summary lines now due. A due
    /// summary is flushed by any later observation, so a peer that goes quiet is reported when the
    /// next path event of any peer arrives.
    pub fn observe<E: PathObservation>(
        &mut self,
        device: &str,
        event: &E,
        now: Instant,
    ) -> (bool, Vec<serde_json::Value>) {
        let kind = event.label();
        let host = event
            .remote()
            .rsplit_once(':')
            .map_or(event.remote(), |(h, _)| h)
            .to_string();
        let state = self.peers.entry(device.to_string()).or_default();
        state.window_start.get_or_insert(now);
        // Only an open or a move can change the peer's best path; a close is an idle connection
        // timing out and is counted. A new kind or address is written at once.
        let changed =
            event.change() != "closed" && state.shown.as_ref() != Some(&(kind, host.clone()));
        if changed {
            state.shown = Some((kind, host));
        } else {
            match event.change() {
                "opened" => state.opened += 1,
                "migrated" => state.migrated += 1,
                _ => state.closed += 1,
            }
            if event.side() == "dial" {
                state.dial += 1;
            } else {
                state.accept += 1;
            }
            *state.kinds.entry(kind).or_default() += 1;
            state.rtt_max_ms = state.rtt_max_ms.max(event.rtt_ms());
        }
        let mut lines = Vec::new();
        for (peer, st) in &mut self.peers {
            let Some(start) = st.window_start else {
                continue;
            };
            let elapsed = now.duration_since(start);
            if elapsed < PATH_SUMMARY_WINDOW {
                continue;
            }
            if st.opened + st.closed + st.migrated > 0 {
                lines.push(json!({
                    "peer": peer,
                    "windowMs": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                    "opened": st.opened,
                    "closed": st.closed,
                    "migrated": st.migrated,
                    "dial": st.dial,
                    "accept": st.accept,
                    "kinds": st.kinds,
                    "rttMaxMs": st.rtt_max_ms,
                }));
            }
            let shown = st.shown.take();
            *st = PeerPathState {
                shown,
                window_start: Some(now),
                ..PeerPathState::default()
            };
        }
        (changed, lines)
    }
}
