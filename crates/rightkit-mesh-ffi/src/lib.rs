//! C ABI over `rightkit-mesh` for a phone: the Joiner side of the desktop's SAS pairing
//! (`pairing.rs`) and one request per stream to a paired computer (`rkmesh_request`). Generic: it knows
//! no application protocol; the app builds its own requests on `rkmesh_request`.
//!
//! Threading: every function blocks, so the caller runs it off the main thread. The mesh lives on
//! its own two-worker runtime. The identity seed is read once from the caller, kept in memory that
//! is wiped on stop and never written to disk.
//!
//! Written, not compiled, not run (2026-10-18, #197).

mod circle;
mod pairing;
mod store;

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rightkit_mesh::{
    MemoryKeyStore, Mesh, MeshInit, MeshSettings, PathKind, path_is_nearby, path_kind,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use pairing::{
    Identity, InFlight, PairUri, PmError, Prompt, display_safe, now_ms, parse_pair_uri, unhex,
    within,
};
use rightkit_fs::digest::hex;
use store::{Host, Trust};

/// Largest request or response body of `rkmesh_request` (the desktop's `LINE_MAX`).
const MAX_BODY: usize = 256 * 1024;
const CONNECT_WAIT: Duration = Duration::from_secs(15);
const PROMPT_WAIT: Duration = Duration::from_secs(30);
/// Longest wait for the host owner's approval after the code (the pairing's lifetime plus slack).
const APPROVAL_WAIT: Duration = Duration::from_secs(130);

/// One running endpoint.
struct Ctx {
    rt: tokio::runtime::Runtime,
    mesh: Arc<Mesh>,
    identity: Arc<Identity>,
    trust: Arc<Trust>,
    /// Kept (wiped on drop) only so a relay change can restart the endpoint on the same key.
    seed: Zeroizing<[u8; 32]>,
    state_dir: PathBuf,
    name: String,
    relay: Option<String>,
    /// A relay was given or stored but failed `validate_relay`, so the mesh runs without one.
    relay_refused: bool,
    pair_alpn: &'static [u8],
    /// Last known state of each paired computer by device id: `connecting`, `direct`, `relayed`
    /// or `offline`. Written by requests and by the task of each open stream.
    states: Mutex<HashMap<String, &'static str>>,
    /// Device ids whose current direct path is on the same local network; cleared with the state.
    nearby: Mutex<HashSet<String>>,
    streams: Mutex<HashMap<u64, Arc<StreamEntry>>>,
    next_stream: AtomicU64,
}

/// A long-lived stream: a task owns the connection and reads frames into `rx`.
struct StreamEntry {
    rx: tokio::sync::Mutex<mpsc::Receiver<Item>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

enum Item {
    Frame(Vec<u8>),
    End,
    Failed(PmError),
}

impl Ctx {
    fn set_state(&self, device: &str, state: &'static str) {
        lock(&self.nearby).remove(device);
        lock(&self.states).insert(device.to_string(), state);
    }

    /// Record a live path: its word and, for a direct path only, whether it is nearby.
    fn set_path(&self, device: &str, state: &'static str, nearby: bool) {
        self.set_state(device, state);
        if nearby && state == "direct" {
            lock(&self.nearby).insert(device.to_string());
        }
    }
}

/// A path that is not provably a direct IP path counts as relayed (as the mesh does for its caps).
fn path_word(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Direct => "direct",
        _ => "relayed",
    }
}

/// The confirmed-but-unanswered pairing, shared with the task that may repeat it.
type InFlightCell = Arc<Mutex<Option<InFlight>>>;

const INFLIGHT_FILE: &str = "pairing-inflight.json";

fn save_inflight(dir: &std::path::Path, state: &InFlight) {
    if let Ok(raw) = serde_json::to_vec(state) {
        let _ = rightkit_fs::atomic_write(&dir.join(INFLIGHT_FILE), &raw);
    }
}

fn clear_inflight(dir: &std::path::Path, cell: &InFlightCell) {
    *lock(cell) = None;
    let _ = std::fs::remove_file(dir.join(INFLIGHT_FILE));
}

/// The persisted pairing still inside its resume window, else none (and the file is removed).
fn load_inflight(dir: &std::path::Path) -> Option<InFlight> {
    let path = dir.join(INFLIGHT_FILE);
    let state = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<InFlight>(&raw).ok())
        .filter(|s| now_ms() < s.resume_until_ms);
    if state.is_none() {
        let _ = std::fs::remove_file(&path);
    }
    state
}

struct Pairing {
    /// The pairing's confirmed-but-unanswered state while a repeat may still complete it.
    status: InFlightCell,
    code_tx: Option<oneshot::Sender<String>>,
    result_rx: oneshot::Receiver<Result<Host, PmError>>,
    handle: tokio::task::JoinHandle<()>,
    ctx: Arc<Ctx>,
}

static CTX: Mutex<Option<Arc<Ctx>>> = Mutex::new(None);
static PAIRINGS: Mutex<Option<HashMap<String, Pairing>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn current() -> Result<Arc<Ctx>, PmError> {
    lock(&CTX)
        .clone()
        .ok_or_else(|| PmError::new("not_started", "the mesh is not started"))
}

fn io_err(error: impl std::fmt::Display) -> PmError {
    PmError::retry("io", error.to_string())
}

fn mesh_err(error: impl std::fmt::Display) -> PmError {
    PmError::new("mesh", error.to_string())
}

impl Ctx {
    fn start(
        state_dir: PathBuf,
        relay: Option<String>,
        name: String,
        seed: Zeroizing<[u8; 32]>,
        pair_alpn: &'static [u8],
    ) -> Result<Arc<Ctx>, PmError> {
        std::fs::create_dir_all(&state_dir).map_err(|e| PmError::new("store", e.to_string()))?;
        let trust = Arc::new(Trust::load(state_dir.join("hosts.json"))?);
        // A relay from an older build's hosts.json is held to the same check as a scanned one; a
        // refused one is reported in the start answer (`relayRefused`) so the phone logs it.
        let candidate = relay.or_else(|| trust.latest_relay());
        let relay_refused = candidate
            .as_deref()
            .is_some_and(|r| pairing::validate_relay(r).is_err());
        let relay = candidate.filter(|_| !relay_refused);
        let identity = Arc::new(Identity::from_seed(&seed, Some(name.clone()))?);
        // Dial-out only: nothing is accepted. Local discovery on; the relay only when known.
        let settings = MeshSettings::for_device_link(relay.as_deref(), false, true);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mesh-link")
            .enable_all()
            .build()
            .map_err(mesh_err)?;
        let init = MeshInit {
            seed: *seed,
            settings,
            trust: trust.clone(),
            keys: Arc::new(MemoryKeyStore::new()),
            budget_path: state_dir.join("relay-budget.json"),
            records: None,
            bulk: None,
            services: Vec::new(),
            open_alpns: vec![pair_alpn],
            guest_alpns: Vec::new(),
        };
        let mesh = rt
            .block_on(Mesh::start(init, |router| router))
            .map_err(mesh_err)?;
        load_peer_addrs(&mesh, &state_dir);
        Ok(Arc::new(Ctx {
            rt,
            mesh: Arc::new(mesh),
            identity,
            trust,
            seed,
            state_dir,
            name,
            relay,
            relay_refused,
            pair_alpn,
            states: Mutex::new(HashMap::new()),
            nearby: Mutex::new(HashSet::new()),
            streams: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
        }))
    }
}

/// Stop `old` and start the same identity again with `relay` (the relay named by a scanned code
/// differs from the running one; the endpoint's relay is fixed when it binds).
fn restart(old: Arc<Ctx>, relay: Option<String>) -> Result<Arc<Ctx>, PmError> {
    for (_, entry) in lock(&old.streams).drain() {
        if let Some(task) = lock(&entry.task).take() {
            task.abort();
        }
    }
    old.rt.block_on(old.mesh.shutdown());
    let started = Ctx::start(
        old.state_dir.clone(),
        relay,
        old.name.clone(),
        Zeroizing::new(*old.seed),
        old.pair_alpn,
    );
    match started {
        Ok(new) => {
            *lock(&CTX) = Some(new.clone());
            Ok(new)
        }
        Err(e) => {
            *lock(&CTX) = None;
            Err(e)
        }
    }
}

// ---- operations ---------------------------------------------------------------------------------

fn start(
    state_dir: &str,
    relay: Option<&str>,
    name: &str,
    seed: Zeroizing<[u8; 32]>,
    pair_alpn: &str,
) -> Result<Value, PmError> {
    if pair_alpn.is_empty() {
        return Err(PmError::new("invalid_request", "pair_alpn is empty"));
    }
    stop()?;
    // Leaked once per start: the mesh wants a 'static protocol name.
    let alpn: &'static [u8] = Box::leak(pair_alpn.as_bytes().to_vec().into_boxed_slice());
    let relay = relay
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    let ctx = Ctx::start(
        PathBuf::from(state_dir),
        relay,
        name.to_string(),
        seed,
        alpn,
    )?;
    let result = json!({
        "endpointId": hex(&ctx.identity.public_key()),
        "deviceId": ctx.identity.device().device_id,
        "relayRefused": ctx.relay_refused,
    });
    *lock(&CTX) = Some(ctx.clone());
    resume_persisted(&ctx);
    Ok(result)
}

fn stop() -> Result<Value, PmError> {
    if let Some(map) = lock(&PAIRINGS).take() {
        for (_, pairing) in map {
            pairing.handle.abort();
        }
    }
    let ctx = lock(&CTX).take();
    if let Some(ctx) = ctx {
        for (_, entry) in lock(&ctx.streams).drain() {
            if let Some(task) = lock(&entry.task).take() {
                task.abort();
            }
        }
        ctx.rt.block_on(ctx.mesh.shutdown());
    }
    Ok(json!({}))
}

fn host_json(host: &Host) -> Value {
    json!({
        "deviceId": host.device_id,
        "endpointId": host.endpoint_id,
        "name": host.name,
        "platform": host.platform,
        "relay": host.relay,
        "pairedAtMs": host.paired_at_ms,
        "introducedBy": host.introduced_by,
    })
}

fn host_record(
    device: &pairing::DeviceRef,
    host_key: &[u8; 32],
    relay: Option<String>,
    pairing_id: String,
    admit_check: Option<circle::CheckKey>,
) -> Host {
    Host {
        endpoint_id: hex(host_key),
        device_id: device.device_id.clone(),
        name: Some(display_safe(device.display_name.as_deref())),
        platform: device.platform.tag().to_string(),
        relay,
        paired_at_ms: now_ms(),
        pairing_id,
        introduced_by: None,
        admit_check,
        endorsed_by: None,
        grant_pairing_id: None,
    }
}

/// Repeat the confirmation of `state` on new streams (1 s, doubling to 30 s between dials) until
/// it is answered, the host refuses (it closed the stream: `closed`, final), or the host's cache
/// window ends. Any end but success removes the persisted state.
async fn resume_loop(
    mesh: Arc<Mesh>,
    alpn: &'static [u8],
    state: InFlight,
    state_dir: &std::path::Path,
    status: &InFlightCell,
    identity: &Identity,
) -> Result<pairing::Joined, PmError> {
    let endpoint = unhex::<32>(&state.host_key)?;
    let mut delay = Duration::from_secs(1);
    let result = loop {
        if now_ms() >= state.resume_until_ms {
            break Err(PmError::new(
                "expired",
                "the pairing could not be completed in time",
            ));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
        let attempt: Result<pairing::Joined, PmError> = async {
            let connection = within(CONNECT_WAIT, async {
                mesh.connect(&endpoint, alpn)
                    .await
                    .map_err(|e| PmError::retry("connect", e.to_string()))
            })
            .await?;
            let (mut send, recv) = connection
                .open_bi()
                .await
                .map_err(|e| PmError::retry("stream", e.to_string()))?;
            send.write_all(&[0u8])
                .await
                .map_err(|e| PmError::retry("stream", e.to_string()))?;
            let mut reader = tokio::io::BufReader::new(recv);
            let joined =
                pairing::resume(&mut reader, &mut send, endpoint, &state, identity).await?;
            let _ = send.finish();
            connection.close(0u32.into(), b"done");
            Ok(joined)
        }
        .await;
        match attempt {
            Ok(joined) => break Ok(joined),
            Err(e) if e.retryable => continue,
            Err(e) => break Err(e),
        }
    };
    clear_inflight(state_dir, status);
    result
}

/// Dial the host named by the scanned code and run the pairing up to the typed code. When the
/// stream is lost after the confirmation was sent, repeat that same confirmation on new streams
/// (the host may already have recorded this phone) instead of starting a new pairing.
/// `peer-addrs.json` in the state folder: each paired computer's direct addresses from its QR,
/// reloaded into the mesh at start (`load_peer_addrs`) so a phone can redial without mDNS.
fn save_peer_addrs(state_dir: &std::path::Path, peer: &[u8; 32], addrs: &[std::net::SocketAddr]) {
    let path = state_dir.join("peer-addrs.json");
    let mut all: serde_json::Map<String, Value> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    all.insert(
        hex(peer),
        Value::from(addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>()),
    );
    if let Ok(bytes) = serde_json::to_vec(&all) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn load_peer_addrs(mesh: &Mesh, state_dir: &std::path::Path) {
    let Some(all) = std::fs::read(state_dir.join("peer-addrs.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Map<String, Value>>(&bytes).ok())
    else {
        return;
    };
    for (peer, list) in all {
        let Ok(peer) = pairing::unhex::<32>(&peer) else {
            continue;
        };
        let addrs: Vec<std::net::SocketAddr> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str()?.parse().ok())
            .collect();
        if !addrs.is_empty() {
            mesh.note_addrs(&peer, addrs);
        }
    }
}

async fn run_join(
    ctx_mesh: Arc<Mesh>,
    identity: Arc<Identity>,
    trust: Arc<Trust>,
    alpn: &'static [u8],
    uri: PairUri,
    relay: Option<String>,
    state_dir: PathBuf,
    status: InFlightCell,
    prompt_tx: &mut Option<oneshot::Sender<Result<Prompt, PmError>>>,
    code_rx: oneshot::Receiver<String>,
) -> Result<Host, PmError> {
    let host_relay = uri.relay.clone().or(relay);
    // The QR's direct addresses: dial them (no mDNS on iOS) and keep them for later connections.
    if !uri.addrs.is_empty() {
        ctx_mesh.note_addrs(&uri.endpoint, uri.addrs.clone());
        save_peer_addrs(&state_dir, &uri.endpoint, &uri.addrs);
    }
    let mut kept: Option<InFlight> = None;
    let first: Result<pairing::Joined, PmError> = async {
        let connection = within(CONNECT_WAIT, async {
            ctx_mesh
                .connect(&uri.endpoint, alpn)
                .await
                .map_err(|e| PmError::retry("connect", e.to_string()))
        })
        .await?;
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| PmError::retry("stream", e.to_string()))?;
        // QUIC shows a stream to the acceptor only once the opener sent data, so every opener
        // starts with one marker byte (`SyncMesh::open_stream`, `StreamHandler::accept`).
        send.write_all(&[0u8])
            .await
            .map_err(|e| PmError::retry("stream", e.to_string()))?;
        let mut reader = tokio::io::BufReader::new(recv);
        let joined = pairing::join(
            &mut reader,
            &mut send,
            &identity,
            uri.endpoint,
            uri.pairing_id,
            prompt_tx,
            code_rx,
            uri.secret,
            host_relay.clone(),
            &mut |state: InFlight| {
                save_inflight(&state_dir, &state);
                *lock(&status) = Some(state.clone());
                kept = Some(state);
            },
        )
        .await?;
        let _ = send.finish();
        connection.close(0u32.into(), b"done");
        Ok(joined)
    }
    .await;
    let (device, host_key, admit_check) = match first {
        Ok(joined) => joined,
        Err(e) => {
            // Never confirmed: nothing to repeat. A lost stream after the confirmation is
            // repeated; a refusal or a bad message is final.
            let Some(state) = kept.take() else {
                return Err(e);
            };
            if !(e.retryable || e.code == "closed") {
                clear_inflight(&state_dir, &status);
                return Err(e);
            }
            resume_loop(
                ctx_mesh.clone(),
                alpn,
                state,
                &state_dir,
                &status,
                &identity,
            )
            .await?
        }
    };
    clear_inflight(&state_dir, &status);
    let host = host_record(
        &device,
        &host_key,
        host_relay,
        hex(&uri.pairing_id),
        admit_check,
    );
    trust.add(host.clone())?;
    Ok(host)
}

/// After a start: finish a pairing the last run confirmed and never saw answered, if its window is
/// still open. Runs in the background; the host appears in `peers` when it succeeds.
fn resume_persisted(ctx: &Arc<Ctx>) {
    let Some(state) = load_inflight(&ctx.state_dir) else {
        return;
    };
    let (mesh, trust, alpn) = (ctx.mesh.clone(), ctx.trust.clone(), ctx.pair_alpn);
    let dir = ctx.state_dir.clone();
    let status: InFlightCell = Arc::new(Mutex::new(Some(state.clone())));
    let identity = ctx.identity.clone();
    ctx.rt.spawn(async move {
        let relay = state.relay.clone();
        let pairing_id = state.pairing_id.clone();
        if let Ok((device, host_key, admit_check)) =
            resume_loop(mesh, alpn, state, &dir, &status, &identity).await
        {
            let _ = trust.add(host_record(
                &device,
                &host_key,
                relay,
                pairing_id,
                admit_check,
            ));
        }
    });
}

fn pair_begin(uri: &str) -> Result<Value, PmError> {
    let parsed = parse_pair_uri(uri)?;
    if parsed.expires_at_ms <= now_ms() {
        return Err(PmError::new("expired", "that pairing code has expired"));
    }
    let mut ctx = current()?;
    if let Some(relay) = &parsed.relay
        && ctx.relay.as_deref() != Some(relay.as_str())
    {
        ctx = restart(ctx, Some(relay.clone()))?;
    }
    let auto_join = parsed.secret.is_some();
    let id = hex(&parsed.pairing_id);
    if let Some(old) = lock(&PAIRINGS).get_or_insert_with(HashMap::new).remove(&id) {
        old.handle.abort();
    }
    let (prompt_tx, prompt_rx) = oneshot::channel();
    let (code_tx, code_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();
    let status: InFlightCell = Arc::new(Mutex::new(None));
    let handle = {
        let state_dir = ctx.state_dir.clone();
        let status = status.clone();
        let mesh = ctx.mesh.clone();
        let identity = ctx.identity.clone();
        let trust = ctx.trust.clone();
        let alpn = ctx.pair_alpn;
        let relay = ctx.relay.clone();
        ctx.rt.spawn(async move {
            let mut prompt_tx = Some(prompt_tx);
            let result = run_join(
                mesh,
                identity,
                trust,
                alpn,
                parsed,
                relay,
                state_dir,
                status,
                &mut prompt_tx,
                code_rx,
            )
            .await;
            if let Err(e) = &result
                && let Some(tx) = prompt_tx.take()
            {
                let _ = tx.send(Err(e.clone()));
            }
            let _ = result_tx.send(result);
        })
    };
    let prompt = ctx.rt.block_on(async {
        match tokio::time::timeout(PROMPT_WAIT, prompt_rx).await {
            Ok(Ok(prompt)) => prompt,
            Ok(Err(_)) => Err(PmError::new("closed", "the pairing ended")),
            Err(_) => Err(PmError::retry("timeout", "the computer did not answer")),
        }
    });
    let prompt = match prompt {
        Ok(prompt) => prompt,
        Err(e) => {
            handle.abort();
            return Err(e);
        }
    };
    lock(&PAIRINGS).get_or_insert_with(HashMap::new).insert(
        id.clone(),
        Pairing {
            status,
            code_tx: Some(code_tx),
            result_rx,
            handle,
            ctx,
        },
    );
    Ok(json!({
        "pairingId": id,
        "hostName": display_safe(prompt.host.display_name.as_deref()),
        "hostDeviceId": prompt.host.device_id,
        "hostPlatform": prompt.host.platform.tag(),
        "fingerprint": prompt.host_fingerprint,
        "expiresAtMs": prompt.expires_at_ms,
        // The QR carried a single-use secret: the app sends no typed code (`pair_code` with any
        // text waits for the result).
        "autoJoin": auto_join,
    }))
}

fn pair_code(id: &str, code: &str) -> Result<Value, PmError> {
    let pairing = lock(&PAIRINGS)
        .get_or_insert_with(HashMap::new)
        .remove(id)
        .ok_or_else(|| PmError::new("not_found", "no such pairing is open"))?;
    let Pairing {
        status,
        mut code_tx,
        mut result_rx,
        handle,
        ctx,
    } = pairing;
    // A repeated call after `pending` carries no new code: the confirmation is already out.
    let waiting = lock(&status).is_some();
    let sent = match code_tx.take() {
        Some(tx) => tx.send(code.to_string()).is_ok(),
        None => waiting,
    };
    if !sent {
        return Err(PmError::new("closed", "the pairing ended"));
    }
    let waited = ctx.rt.block_on(async {
        match tokio::time::timeout(APPROVAL_WAIT, &mut result_rx).await {
            Ok(Ok(result)) => Some(result),
            Ok(Err(_)) => Some(Err(PmError::new("closed", "the pairing ended"))),
            Err(_) => None,
        }
    });
    match waited {
        Some(result) => Ok(json!({ "peer": host_json(&result?) })),
        None if lock(&status).is_some() => {
            // Confirmed, unanswered, and still repeating in the background: keep the pairing so
            // the app can ask again (or the host appears in `peers`).
            lock(&PAIRINGS).get_or_insert_with(HashMap::new).insert(
                id.to_string(),
                Pairing {
                    status,
                    code_tx: None,
                    result_rx,
                    handle,
                    ctx,
                },
            );
            Err(PmError::retry(
                "pending",
                "still waiting for the computer; the pairing continues",
            ))
        }
        None => Err(PmError::new(
            "expired",
            "the computer's owner did not approve in time",
        )),
    }
}

fn pair_cancel(id: &str) -> Result<Value, PmError> {
    if let Some(pairing) = lock(&PAIRINGS).get_or_insert_with(HashMap::new).remove(id) {
        pairing.handle.abort();
        clear_inflight(&pairing.ctx.state_dir, &pairing.status);
    }
    Ok(json!({}))
}

fn peers() -> Result<Value, PmError> {
    let ctx = current()?;
    let hosts: Vec<Value> = ctx.trust.all().iter().map(host_json).collect();
    Ok(json!({ "peers": hosts }))
}

fn unpair(peer: &str) -> Result<Value, PmError> {
    let ctx = current()?;
    ctx.trust.remove(peer)?;
    Ok(json!({}))
}

/// One request on one stream: marker byte, the body, finish; then the response to the end.
fn request(peer: &str, alpn: &str, body: &[u8], wait: Duration) -> Result<Vec<u8>, PmError> {
    if body.len() > MAX_BODY {
        return Err(PmError::new("too_large", "the request is too large"));
    }
    let ctx = current()?;
    let host = ctx
        .trust
        .by_device(peer)
        .ok_or_else(|| PmError::new("not_paired", "that computer is not paired with this phone"))?;
    let key = unhex::<32>(&host.endpoint_id)?;
    ctx.set_state(&host.device_id, "connecting");
    ctx.rt.block_on(async {
        let connection = within(CONNECT_WAIT, async {
            ctx.mesh
                .connect(&key, alpn.as_bytes())
                .await
                .map_err(|e| PmError::retry("connect", e.to_string()))
        })
        .await
        .inspect_err(|_| ctx.set_state(&host.device_id, "offline"))?;
        ctx.set_path(
            &host.device_id,
            path_word(path_kind(&connection)),
            path_is_nearby(&connection),
        );
        let answer = within(wait, async {
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|e| PmError::retry("stream", e.to_string()))?;
            send.write_all(&[0u8]).await.map_err(io_err)?;
            send.write_all(body).await.map_err(io_err)?;
            let _ = send.finish();
            let mut out = Vec::new();
            AsyncReadExt::read_to_end(
                &mut AsyncReadExt::take(&mut recv, MAX_BODY as u64 + 1),
                &mut out,
            )
            .await
            .map_err(io_err)?;
            Ok::<_, PmError>(out)
        })
        .await;
        connection.close(0u32.into(), b"done");
        if answer.is_err() {
            ctx.set_state(&host.device_id, "offline");
        }
        let out = answer?;
        if out.len() > MAX_BODY {
            return Err(PmError::new("too_large", "the answer is too large"));
        }
        if out.is_empty() {
            return Err(PmError::retry("no_answer", "the computer sent no answer"));
        }
        Ok(out)
    })
}

/// Frames on a stream are a 4-byte big-endian length and the payload, each way.
async fn pump<R: tokio::io::AsyncRead + Unpin>(mut recv: R, tx: mpsc::Sender<Item>) {
    loop {
        let mut head = [0u8; 4];
        match recv.read_exact(&mut head).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                let _ = tx.send(Item::End).await;
                return;
            }
            Err(e) => {
                let _ = tx
                    .send(Item::Failed(PmError::retry("io", e.to_string())))
                    .await;
                return;
            }
        }
        let len = u32::from_be_bytes(head) as usize;
        if len > MAX_BODY {
            let _ = tx
                .send(Item::Failed(PmError::new(
                    "too_large",
                    "a frame is too large",
                )))
                .await;
            return;
        }
        let mut frame = vec![0u8; len];
        if let Err(e) = recv.read_exact(&mut frame).await {
            let _ = tx
                .send(Item::Failed(PmError::retry("io", e.to_string())))
                .await;
            return;
        }
        if tx.send(Item::Frame(frame)).await.is_err() {
            return;
        }
    }
}

/// Open a long-lived stream: marker byte, then the opening body as the first frame. The computer
/// pushes frames until it or this phone closes the stream.
fn stream_open(peer: &str, alpn: &str, body: &[u8]) -> Result<Value, PmError> {
    if body.len() > MAX_BODY {
        return Err(PmError::new("too_large", "the opening frame is too large"));
    }
    let ctx = current()?;
    let host = ctx
        .trust
        .by_device(peer)
        .ok_or_else(|| PmError::new("not_paired", "that computer is not paired with this phone"))?;
    let key = unhex::<32>(&host.endpoint_id)?;
    let device = host.device_id.clone();
    ctx.set_state(&device, "connecting");
    let (tx, rx) = mpsc::channel(64);
    let opened = ctx.rt.block_on(async {
        let connection = within(CONNECT_WAIT, async {
            ctx.mesh
                .connect(&key, alpn.as_bytes())
                .await
                .map_err(|e| PmError::retry("connect", e.to_string()))
        })
        .await?;
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| PmError::retry("stream", e.to_string()))?;
        let mut first = vec![0u8];
        first.extend_from_slice(&(body.len() as u32).to_be_bytes());
        first.extend_from_slice(body);
        within(CONNECT_WAIT, async {
            send.write_all(&first).await.map_err(io_err)?;
            send.flush().await.map_err(io_err)
        })
        .await?;
        Ok::<_, PmError>((connection, send, recv))
    });
    let (connection, send, recv) = match opened {
        Ok(parts) => parts,
        Err(e) => {
            ctx.set_state(&device, "offline");
            return Err(e);
        }
    };
    ctx.set_path(
        &device,
        path_word(path_kind(&connection)),
        path_is_nearby(&connection),
    );
    let id = ctx.next_stream.fetch_add(1, Ordering::SeqCst);
    let task_ctx = ctx.clone();
    let task = ctx.rt.spawn(async move {
        let ticker = async {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                task_ctx.set_path(
                    &device,
                    path_word(path_kind(&connection)),
                    path_is_nearby(&connection),
                );
            }
        };
        tokio::select! {
            _ = ticker => {}
            _ = pump(recv, tx) => {}
        }
        task_ctx.set_state(&device, "offline");
        drop(send);
    });
    let entry = Arc::new(StreamEntry {
        rx: tokio::sync::Mutex::new(rx),
        task: Mutex::new(Some(task)),
    });
    lock(&ctx.streams).insert(id, entry);
    Ok(json!({ "streamId": id }))
}

/// The next pushed frame: `Ok(Some(bytes))`, `Ok(None)` on a quiet wait (`timeout`), and an
/// error value of `end` when the stream is over.
enum Next {
    Frame(Vec<u8>),
    Timeout,
    End,
}

fn stream_next(id: u64, wait: Duration) -> Result<Next, PmError> {
    let ctx = current()?;
    let entry = lock(&ctx.streams)
        .get(&id)
        .cloned()
        .ok_or_else(|| PmError::new("not_found", "no such stream"))?;
    let item = ctx.rt.block_on(async {
        let mut rx = entry.rx.lock().await;
        tokio::time::timeout(wait, rx.recv()).await
    });
    match item {
        Err(_) => Ok(Next::Timeout),
        Ok(Some(Item::Frame(frame))) => Ok(Next::Frame(frame)),
        Ok(Some(Item::End)) | Ok(None) => {
            stream_close(id);
            Ok(Next::End)
        }
        Ok(Some(Item::Failed(e))) => {
            stream_close(id);
            Err(e)
        }
    }
}

fn stream_close(id: u64) {
    if let Ok(ctx) = current()
        && let Some(entry) = lock(&ctx.streams).remove(&id)
        && let Some(task) = lock(&entry.task).take()
    {
        task.abort();
    }
}

fn peer_state(peer: &str) -> Result<Value, PmError> {
    let ctx = current()?;
    if ctx.trust.by_device(peer).is_none() {
        return Err(PmError::new(
            "not_paired",
            "that computer is not paired with this phone",
        ));
    }
    let state = lock(&ctx.states).get(peer).copied().unwrap_or("offline");
    let mut reply = json!({ "state": state });
    if state == "direct" {
        let nearby = lock(&ctx.nearby).contains(peer);
        reply["path"] = json!(if nearby { "nearby" } else { "direct" });
    }
    Ok(reply)
}

/// The ALPN of the desktop's link service (`engine/src/devicelink_host.rs`), which also answers the
/// circle's `device.circle.members` pull.
const LINK_ALPN: &str = "pulse/link/1";

/// Ask the paired computer `peer` for its signed member list and the signed removals it holds for
/// this phone, verify both with the key this phone paired with, remove what a valid removal names
/// and record the members the list proves (no grants; a list never removes one). The list adds a
/// member only when an owner-check endorsement chain this phone trusts carries it
/// (`circle::verify_list`); a removal applies under the circle's scoping (`circle::verify_revokes`).
/// Retry-safe: a reply already applied adds and removes nothing.
fn circle_sync(peer: &str) -> Result<Value, PmError> {
    let ctx = current()?;
    let host = ctx
        .trust
        .by_device(peer)
        .ok_or_else(|| PmError::new("not_paired", "that computer is not paired with this phone"))?;
    let key = unhex::<32>(&host.endpoint_id)?;
    let line = b"{\"method\":\"device.circle.members\",\"params\":{}}\n";
    let answer = request(peer, LINK_ALPN, line, Duration::from_secs(20))?;
    let reply: Value = serde_json::from_slice(answer.split(|b| *b == b'\n').next().unwrap_or(&[]))
        .map_err(|_| PmError::retry("no_answer", "the computer answered unreadably"))?;
    if reply["ok"].as_bool() != Some(true) {
        return Err(PmError::new(
            "refused",
            reply["code"].as_str().unwrap_or("refused").to_string(),
        ));
    }
    let me = ctx.identity.device().device_id.clone();
    let list: circle::MemberList = serde_json::from_value(reply["result"].clone())
        .map_err(|_| PmError::new("malformed", "bad member list"))?;
    // Removals first, so a host removed here is never added back by the list in the same reply.
    let revokes: Vec<circle::CircleRevoke> = reply["result"]["revokes"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let removed = ctx.trust.apply_revokes(&circle::verify_revokes(
        &revokes,
        &host,
        &key,
        &me,
        &ctx.trust.all(),
    ))?;
    let known = ctx.trust.all();
    let verified = circle::verify_list(&list, &host, &key, &me, &known, &ctx.trust.revoked())?;
    // Two joiners claimed one pairing id: remove the one held and remember both as removed, so
    // neither is added now or by a later list.
    let conflicted = ctx.trust.apply_revokes(&verified.remove)?;
    let added = ctx.trust.apply_circle(verified.add)?;
    Ok(json!({ "added": added, "removed": removed + conflicted }))
}

// ---- C ABI --------------------------------------------------------------------------------------

fn render(result: Result<Value, PmError>) -> *mut c_char {
    let value = match result {
        Ok(mut value) => {
            value["ok"] = json!(true);
            value
        }
        Err(e) => json!({
            "ok": false,
            "code": e.code,
            "message": e.message,
            "retryable": e.retryable,
        }),
    };
    CString::new(value.to_string())
        .unwrap_or_default()
        .into_raw()
}

fn guard(work: impl FnOnce() -> Result<Value, PmError>) -> *mut c_char {
    render(
        catch_unwind(AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err(PmError::new("panic", "internal error"))),
    )
}

unsafe fn text(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string valid for the call.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .ok()
        .map(str::to_string)
}

fn missing(what: &str) -> PmError {
    PmError::new("invalid_request", format!("{what} is missing or not UTF-8"))
}

/// # Safety
/// Pointers are NUL-terminated strings valid for the call; `seed_32` points at 32 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_start(
    state_dir: *const c_char,
    relay_url_or_null: *const c_char,
    device_name: *const c_char,
    seed_32: *const u8,
    pair_alpn: *const c_char,
) -> *mut c_char {
    // SAFETY: see the function's contract.
    let (dir, relay, name, alpn) = unsafe {
        (
            text(state_dir),
            text(relay_url_or_null),
            text(device_name),
            text(pair_alpn),
        )
    };
    let seed = if seed_32.is_null() {
        None
    } else {
        let mut seed = Zeroizing::new([0u8; 32]);
        // SAFETY: 32 readable bytes by contract.
        seed.copy_from_slice(unsafe { std::slice::from_raw_parts(seed_32, 32) });
        Some(seed)
    };
    guard(|| {
        start(
            &dir.ok_or_else(|| missing("state_dir"))?,
            relay.as_deref(),
            &name.ok_or_else(|| missing("device_name"))?,
            seed.ok_or_else(|| missing("seed_32"))?,
            &alpn.ok_or_else(|| missing("pair_alpn"))?,
        )
    })
}

/// # Safety
/// `uri` is a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_pair_begin(uri: *const c_char) -> *mut c_char {
    // SAFETY: see the function's contract.
    let uri = unsafe { text(uri) };
    guard(|| pair_begin(&uri.ok_or_else(|| missing("uri"))?))
}

/// # Safety
/// Both are NUL-terminated strings valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_pair_code(
    pairing_id: *const c_char,
    code: *const c_char,
) -> *mut c_char {
    // SAFETY: see the function's contract.
    let (id, code) = unsafe { (text(pairing_id), text(code)) };
    guard(|| {
        pair_code(
            &id.ok_or_else(|| missing("pairing_id"))?,
            &code.ok_or_else(|| missing("code"))?,
        )
    })
}

/// # Safety
/// `pairing_id` is a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_pair_cancel(pairing_id: *const c_char) -> *mut c_char {
    // SAFETY: see the function's contract.
    let id = unsafe { text(pairing_id) };
    guard(|| pair_cancel(&id.ok_or_else(|| missing("pairing_id"))?))
}

#[unsafe(no_mangle)]
pub extern "C" fn rkmesh_peers() -> *mut c_char {
    guard(peers)
}

/// # Safety
/// Strings are NUL-terminated and valid for the call; `body` points at `body_len` readable bytes
/// (or is NULL with length 0); `out_body` and `out_len` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_request(
    peer: *const c_char,
    alpn: *const c_char,
    body: *const u8,
    body_len: usize,
    timeout_secs: u32,
    out_body: *mut *mut u8,
    out_len: *mut usize,
) -> *mut c_char {
    // SAFETY: see the function's contract.
    unsafe {
        if !out_body.is_null() {
            *out_body = std::ptr::null_mut();
        }
        if !out_len.is_null() {
            *out_len = 0;
        }
    }
    // SAFETY: see the function's contract.
    let (peer, alpn) = unsafe { (text(peer), text(alpn)) };
    let body: Vec<u8> = if body.is_null() || body_len == 0 {
        Vec::new()
    } else {
        // SAFETY: `body_len` readable bytes by contract.
        unsafe { std::slice::from_raw_parts(body, body_len) }.to_vec()
    };
    guard(|| {
        let answer = request(
            &peer.ok_or_else(|| missing("peer"))?,
            &alpn.ok_or_else(|| missing("alpn"))?,
            &body,
            Duration::from_secs(u64::from(timeout_secs.max(1))),
        )?;
        let length = answer.len();
        let raw = Box::into_raw(answer.into_boxed_slice()) as *mut u8;
        // SAFETY: the out pointers are writable by contract.
        unsafe {
            if out_body.is_null() || out_len.is_null() {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    raw, length,
                )));
                return Err(PmError::new("invalid_request", "out pointers are missing"));
            }
            *out_body = raw;
            *out_len = length;
        }
        Ok(json!({ "length": length }))
    })
}

/// Pull the circle from the paired computer `peer`: the other computers the owner admitted through it
/// become paired computers of this phone, with no grants. Call after pairing and when the app opens.
///
/// # Safety
/// `peer` is a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_circle_sync(peer: *const c_char) -> *mut c_char {
    // SAFETY: see the function's contract.
    let peer = unsafe { text(peer) };
    guard(|| circle_sync(&peer.ok_or_else(|| missing("peer"))?))
}

/// # Safety
/// `peer` is a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_unpair(peer: *const c_char) -> *mut c_char {
    // SAFETY: see the function's contract.
    let peer = unsafe { text(peer) };
    guard(|| unpair(&peer.ok_or_else(|| missing("peer"))?))
}

#[unsafe(no_mangle)]
pub extern "C" fn rkmesh_stop() -> *mut c_char {
    guard(stop)
}

/// # Safety
/// `s` came from this library and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_free_string(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: allocated by `CString::into_raw` here.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// # Safety
/// `p` and `len` came from `rkmesh_request` and are freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_free_bytes(p: *mut u8, len: usize) {
    if !p.is_null() {
        // SAFETY: allocated by `Box::into_raw` of a boxed slice of `len` bytes here.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)) });
    }
}

/// # Safety
/// Strings are NUL-terminated and valid for the call; `body` points at `body_len` readable bytes
/// (or is NULL with length 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_stream_open(
    peer: *const c_char,
    alpn: *const c_char,
    body: *const u8,
    body_len: usize,
) -> *mut c_char {
    // SAFETY: see the function's contract.
    let (peer, alpn) = unsafe { (text(peer), text(alpn)) };
    let body: Vec<u8> = if body.is_null() || body_len == 0 {
        Vec::new()
    } else {
        // SAFETY: `body_len` readable bytes by contract.
        unsafe { std::slice::from_raw_parts(body, body_len) }.to_vec()
    };
    guard(|| {
        stream_open(
            &peer.ok_or_else(|| missing("peer"))?,
            &alpn.ok_or_else(|| missing("alpn"))?,
            &body,
        )
    })
}

/// # Safety
/// `out_body` and `out_len` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_stream_next(
    stream_id: u64,
    timeout_ms: u32,
    out_body: *mut *mut u8,
    out_len: *mut usize,
) -> *mut c_char {
    // SAFETY: see the function's contract.
    unsafe {
        if !out_body.is_null() {
            *out_body = std::ptr::null_mut();
        }
        if !out_len.is_null() {
            *out_len = 0;
        }
    }
    guard(|| {
        match stream_next(
            stream_id,
            Duration::from_millis(u64::from(timeout_ms.max(1))),
        )? {
            Next::Timeout => Ok(json!({ "event": "timeout" })),
            Next::End => Ok(json!({ "event": "end" })),
            Next::Frame(frame) => {
                let length = frame.len();
                if out_body.is_null() || out_len.is_null() {
                    return Err(PmError::new("invalid_request", "out pointers are missing"));
                }
                let raw = Box::into_raw(frame.into_boxed_slice()) as *mut u8;
                // SAFETY: the out pointers are writable by contract.
                unsafe {
                    *out_body = raw;
                    *out_len = length;
                }
                Ok(json!({ "event": "frame", "length": length }))
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rkmesh_stream_close(stream_id: u64) -> *mut c_char {
    guard(|| {
        stream_close(stream_id);
        Ok(json!({}))
    })
}

/// # Safety
/// `peer` is a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rkmesh_peer_state(peer: *const c_char) -> *mut c_char {
    // SAFETY: see the function's contract.
    let peer = unsafe { text(peer) };
    guard(|| peer_state(&peer.ok_or_else(|| missing("peer"))?))
}
