//! Chat over the device link: a paired computer is a chat link beside the ssh links.
//!
//! The device link (`pulse/phone/1` on the mesh, `engine/src/devicelink_host.rs`) carries four
//! methods between two paired computers, the same three commands an ssh link runs plus the
//! receipt update:
//!
//! * `chat.peers`: that computer's own chats (what `<cli> chat peers --local --json` prints);
//! * `chat.deliver {envelope}`: deliver one envelope there, answered with the receipt
//!   `{msg_id, status, detail}` `<cli> chat post` prints;
//! * `chat.said {target, limit}`: what the user typed in one chat there (`<cli> chat said --local`);
//! * `chat.receipt {update}`: a receipt update for one of that computer's sending chats.
//!
//! **Who may.** The receiving engine decides per call from the caller's registry record
//! (`rightkit_mesh::devicelink::policy::authorize`): the owner's own computer (a record the owner check admitted, endorsed or
//! joined by token) or a computer holding an unexpired `reversible` grant (a chat message is a
//! reversible request; the grant is the existing effect class, `docs/device-link.md` "Grants",
//! there is no separate `chat` class). A phone is never a chat link.
//!
//! **Who is speaking.** The sender is the device the mesh authenticated, not anything the
//! envelope says: `serve` overwrites `from.device` and `from.device_id` with the caller's registry
//! name and id, clears any signature and sets `Envelope::mesh`, which `origin_of` reads as "this
//! machine is verified" (the mesh stream replaces the ssh link's chat-key signature). Delivery
//! stays at most once: `receive` keeps its journal on `(from.device, id)`, so a resend of one id
//! is a duplicate, not a second message.
//!
//! **Where a link comes from.** Nothing is stored. The process that holds the mesh (the engine)
//! installs a provider (`set_provider`); the CLI installs one that asks the engine
//! (`device.chat.links`, `device.chat.call`). A mesh link is a `Link` whose `ssh` field is
//! `mesh:<device id>` (`mesh_id`), merged with the saved ssh links by `links::all`, `find` and
//! `list_all`; the mesh link wins and the ssh link of the same computer is its fallback.

use super::envelope::Envelope;
use super::links::{self, Link};
use super::roster::{self, Peer};
use super::store::Store;
use super::{Identity, receipts, said};
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};

/// The start of a mesh link's `ssh` field; the rest is the paired device's id.
pub const PREFIX: &str = "mesh:";

pub const PEERS: &str = "chat.peers";
pub const DELIVER: &str = "chat.deliver";
pub const SAID: &str = "chat.said";
pub const RECEIPT: &str = "chat.receipt";

/// A paired computer that can carry chat: its device-link id and the name it is shown under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeer {
    pub device_id: String,
    pub name: String,
}

/// A call over the device link that did not produce an answer. `may_have_arrived` is true only
/// when the request was written and nothing readable came back, so the other computer may have
/// acted: the caller reports `unknown`, and only for a non-answer (`SAME_ID_RETRY_CODES` in
/// links.rs) asks once more over the ssh twin with the same id (C-D19). Every other failure
/// (nothing dialled, a refusal) means nothing was delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshFailure {
    pub code: String,
    pub message: String,
    pub may_have_arrived: bool,
}

impl MeshFailure {
    pub fn new(code: &str, message: impl Into<String>, may_have_arrived: bool) -> MeshFailure {
        MeshFailure {
            code: code.to_string(),
            message: message.into(),
            may_have_arrived,
        }
    }
}

/// Failure codes that mean the device link could not be reached at all (nothing dialled, no
/// engine, a dial that ran out of time before a byte was written). Only these may let a caller
/// try another transport; every other code is a refusal or a policy answer and is terminal
/// (R05: a revoked, unauthorized, expired or unsupported peer must not reach an ssh twin).
const UNREACHABLE_CODES: &[&str] = &[
    "not_mesh",
    "mesh_off",
    "engine_unreachable",
    "host_unreachable",
    "dial_timeout",
    "call_failed",
];

impl MeshFailure {
    /// True only for a transport-unreachable failure that certainly delivered nothing. A request
    /// that may have arrived is handled in links.rs `post_traced`: a non-answer (`request_failed`,
    /// `answer_timeout`, `engine_unknown`, `engine_unresponsive`) is retried once over ssh with
    /// the same id (C-D19); anything else stays `unknown`; a refusal is terminal (R05).
    pub fn may_fall_back(&self) -> bool {
        !self.may_have_arrived && UNREACHABLE_CODES.contains(&self.code.as_str())
    }
}

impl std::fmt::Display for MeshFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

/// How the provider's last lookup of the paired computers went (#500: a failed lookup used to
/// read as "no paired computers", and names and targets changed between runs with no error).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeersState {
    /// Why the lookup failed; `None` when it answered.
    pub error: Option<String>,
    /// When the lookup failed and a saved list stands in: how old that list is (ms).
    pub list_age_ms: Option<u64>,
}

impl PeersState {
    /// The sentence `<cli> chat status` and `peers` print; `None` when the lookup answered.
    pub fn note(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        Some(match self.list_age_ms {
            Some(age) => format!(
                "device link: could not ask the app for the paired computers ({error}); using the list saved {} ago",
                age_words(age)
            ),
            None => format!(
                "device link: could not ask the app for the paired computers ({error}); no saved list, so only ssh links are used"
            ),
        })
    }
}

fn age_words(age_ms: u64) -> String {
    let secs = age_ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}min", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// What reaches paired computers: the engine's mesh, or the CLI's connection to the engine.
pub trait MeshChat: Send + Sync {
    /// The paired computers (never phones) chat may be tried with. A provider whose lookup can
    /// fail answers with its last good list then, and says so in `state`.
    fn peers(&self) -> Vec<MeshPeer>;
    /// How the last `peers` went.
    fn state(&self) -> PeersState {
        PeersState::default()
    }
    /// One call to a paired computer's `method` with `params`: its answer, or why none came.
    fn call(&self, device_id: &str, method: &str, params: Value) -> Result<Value, MeshFailure>;
    /// `call` for a read-only question that must not hold the caller long: a provider that
    /// can bound its wait answers within `wait` or fails; the default is `call`.
    fn call_within(
        &self,
        device_id: &str,
        method: &str,
        params: Value,
        wait: std::time::Duration,
    ) -> Result<Value, MeshFailure> {
        let _ = wait;
        self.call(device_id, method, params)
    }
}

/// How long `status` and `peers` wait for one linked computer's `chat.peers` over the device link
/// (#582: they waited 30 to 43 s on a slow peer). Past it the link shows its last good listing as
/// unreachable (`links::poll`), the same as a computer that is down. The engine goes on with the
/// call and warms its answer cache; nothing is sent twice, a listing is a read.
pub const PEERS_WAIT: std::time::Duration = std::time::Duration::from_secs(9);

static PROVIDER: RwLock<Option<Arc<dyn MeshChat>>> = RwLock::new(None);

/// Install the provider this process reaches the device link through (replaces any earlier one).
pub fn set_provider(provider: Arc<dyn MeshChat>) {
    if let Ok(mut slot) = PROVIDER.write() {
        *slot = Some(provider);
    }
}

fn provider() -> Option<Arc<dyn MeshChat>> {
    PROVIDER.read().ok().and_then(|slot| slot.clone())
}

/// How the last lookup of the paired computers went (`PeersState`); the default when no provider
/// is installed.
pub fn peers_state() -> PeersState {
    provider().map(|p| p.state()).unwrap_or_default()
}

/// The paired device id of a mesh link; `None` for an ssh link.
pub fn mesh_id(link: &Link) -> Option<&str> {
    link.ssh.strip_prefix(PREFIX)
}

/// The mesh links of this moment: one per paired computer the provider names. Empty when no
/// provider is installed or the device link is not running.
pub fn links() -> Vec<Link> {
    provider()
        .map(|p| p.peers())
        .unwrap_or_default()
        .into_iter()
        .filter(|peer| !peer.device_id.is_empty() && !peer.name.trim().is_empty())
        .map(|peer| Link {
            device: peer.name.trim().to_string(),
            ssh: format!("{PREFIX}{}", peer.device_id),
            device_id: Some(peer.device_id),
            ..Link::default()
        })
        .collect()
}

/// One call over `link`, which must be a mesh link.
pub fn call(link: &Link, method: &str, params: Value) -> Result<Value, MeshFailure> {
    let id =
        mesh_id(link).ok_or_else(|| MeshFailure::new("not_mesh", "not a device link", false))?;
    let provider = provider().ok_or_else(|| {
        MeshFailure::new(
            "mesh_off",
            "the device link is not reachable from here (is the app running?)",
            false,
        )
    })?;
    provider.call(id, method, params)
}

/// `call` for a read-only question with a short bound (`MeshChat::call_within`, #582).
pub fn call_within(
    link: &Link,
    method: &str,
    params: Value,
    wait: std::time::Duration,
) -> Result<Value, MeshFailure> {
    let id =
        mesh_id(link).ok_or_else(|| MeshFailure::new("not_mesh", "not a device link", false))?;
    let provider = provider().ok_or_else(|| {
        MeshFailure::new(
            "mesh_off",
            "the device link is not reachable from here (is the app running?)",
            false,
        )
    })?;
    provider.call_within(id, method, params, wait)
}

/// The device that called, as the mesh proved it (never read from the request).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Caller {
    pub device_id: String,
    pub name: String,
    /// the device-link policy said "owner": the owner's own computer. A grant holder is not, and gets no
    /// verified machine, no stamped user words and no `chat.said`.
    pub owner: bool,
    /// The caller's role from this computer's registry (`PairedDevice::member_role`).
    pub role: rightkit_device_link_types::MemberRole,
}

/// A refusal of a served call, with a stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

/// Serve one chat method for `caller`, already authorized by the engine (`rightkit_mesh::devicelink::policy::authorize`).
pub fn serve(
    store: &Store,
    caller: &Caller,
    method: &str,
    params: &Value,
) -> Result<Value, Refusal> {
    let me = super::local_identity(store).map_err(|e| refuse("unavailable", e.to_string()))?;
    match method {
        PEERS => Ok(peers_listing(store, &me)),
        DELIVER => {
            let text = params
                .get("envelope")
                .and_then(Value::as_str)
                .ok_or_else(|| refuse("invalid_params", "missing param `envelope`"))?;
            let mut env: Envelope =
                links::decode_envelope(text).map_err(|e| refuse("invalid_params", e))?;
            stamp(&mut env, caller);
            let receipt = super::receive(store, &env);
            Ok(
                json!({"msg_id": receipt.msg_id, "status": receipt.state.as_str(),
                      "detail": receipt.detail}),
            )
        }
        SAID => {
            if !caller.owner {
                return Err(refuse(
                    "not_granted",
                    "the user's typed words are shared with the owner's own computers only",
                ));
            }
            let target = params
                .get("target")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| refuse("invalid_params", "missing param `target`"))?;
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(said::DEFAULT_LIMIT, |n| n as usize)
                .clamp(1, said::MAX_LIMIT);
            let (kind, session) = match target.split_once(':') {
                Some((kind, id))
                    if matches!(kind, "claude" | "codex")
                        || super::codex_hook::valid_hook_harness(kind) =>
                {
                    (Some(kind), id)
                }
                _ => (None, target),
            };
            Ok(match said::read_local(store, &me, kind, session, limit) {
                Ok(found) => {
                    let mut value = found.to_json();
                    value["ok"] = json!(true);
                    value
                }
                Err(said::SaidError::Refused(why)) => {
                    json!({"ok": false, "refused": true, "error": why})
                }
                Err(said::SaidError::Failed(why)) => {
                    json!({"ok": false, "refused": false, "error": why})
                }
            })
        }
        RECEIPT => {
            let text = params
                .get("update")
                .and_then(Value::as_str)
                .ok_or_else(|| refuse("invalid_params", "missing param `update`"))?;
            let bytes = links::unb64(text.trim())
                .ok_or_else(|| refuse("invalid_params", "`update` is not base64"))?;
            let update: receipts::Update = serde_json::from_slice(&bytes)
                .map_err(|e| refuse("invalid_params", e.to_string()))?;
            Ok(
                match receipts::apply_remote_from(store, update, Some(caller)) {
                    Ok(stored) => json!({"ok": true, "stored": stored}),
                    Err(refusal) => {
                        json!({"ok": false, "code": refusal.code, "error": refusal.error})
                    }
                },
            )
        }
        _ => Err(refuse("unknown_method", "unknown chat method")),
    }
}

/// Put the mesh-proven sender on `env`: the caller's registry name and id replace whatever the
/// envelope said, any signature (made for an ssh link) is dropped, and `mesh` records that the
/// device link vouched for the machine. Names are cut to the envelope's own field limit.
fn stamp(env: &mut Envelope, caller: &Caller) {
    env.from.device = caller.name.chars().take(100).collect();
    env.from.device_id = caller.device_id.clone();
    env.mesh = caller.device_id.clone();
    env.mesh_owner = caller.owner;
    // The role is the registry's word, never the sender's.
    env.mesh_role = caller.role.as_str().to_string();
    env.sig.clear();
    if !caller.owner {
        // A grant holder's claim to have proven the sending chat, and the user words it stamped,
        // are its own say-so: neither is believed.
        env.verified = false;
        env.said.clear();
    }
}

/// What `<cli> chat peers --local --json` prints: this computer's chats as a linked computer
/// reads them (`links::RemoteListing` plus its device id and chat key).
/// How long a `chat.peers` answer is served again to linked computers (issue #560). Each answer
/// sweeps every local chat folder (on the Dell, ~290 MB of reads and a 27-42% CPU spike); the Mac
/// and the box each ask about once a minute. The answer may come from the hub's own scan, itself up
/// to this old, so a chat started moments ago can be missing for up to twice this long (about 20 s
/// with the hub's 5 s scan). Delivery resolves its target from a fresh roster, so only the listing lags.
const PEERS_CACHE: std::time::Duration = std::time::Duration::from_secs(15);

/// The `chat.peers` answer: one roster sweep, then served from memory for [`PEERS_CACHE`].
fn peers_listing(store: &Store, me: &Identity) -> Value {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, String, Value)>> = Mutex::new(None);
    /// One sweep at a time (single-flight): callers that miss together wait here, then find the
    /// first one's answer in the cache instead of sweeping again.
    static FLIGHT: Mutex<()> = Mutex::new(());
    // The answer depends on who asks about which store (N10): the key names both.
    let key = format!("{}|{}", store.root().display(), me.alias);
    // A poisoned lock (a panic while the cache was written) is recovered, once logged, and the
    // cache starts empty: it never silently turns the cache off.
    let lock_cache = || {
        CACHE.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "{}: the chat.peers cache lock was poisoned; recovered and cleared",
                super::brand::brand().chat_marker()
            );
            let mut cache = poisoned.into_inner();
            *cache = None;
            CACHE.clear_poison();
            cache
        })
    };
    let cached = || -> Option<Value> {
        let cache = lock_cache();
        let (at, cached_key, value) = cache.as_ref()?;
        (*cached_key == key && at.elapsed() < PEERS_CACHE).then(|| value.clone())
    };
    if let Some(value) = cached() {
        return value;
    }
    let _flight = FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(value) = cached() {
        return value;
    }
    let swept = Instant::now();
    // The hub's own scan (every few seconds while Chat is on) serves this when it is no older
    // than the cache window; otherwise it sweeps. Before this, each answer swept the folders a
    // second time beside the hub's scan.
    let sessions = super::hub::local_chats(store, PEERS_CACHE);
    let list = super::local_peers_from(store, me, &sessions);
    let value = listing_json_from(store, me, &list, &sessions);
    *lock_cache() = Some((Instant::now(), key.clone(), value.clone()));
    // One line per answer built (answers served from this cache are not logged), whether it swept or
    // took the hub's scan: how long it took and how many chats it found, so a slow `chat.peers` is
    // visible in the hub log. Counts only; not comparable with sweep counts before #449.
    super::sendlog::event(
        rightkit_logs::Level::Info,
        "chat.peers.sweep",
        json!({
            "durationMs": u64::try_from(swept.elapsed().as_millis()).unwrap_or(u64::MAX),
            "sessions": sessions.len(),
            "pid": std::process::id(),
        }),
    );
    value
}

pub fn listing_json(store: &Store, me: &Identity, list: &[Peer]) -> Value {
    listing_json_from(store, me, list, &roster::local_sessions(store))
}

/// [`listing_json`] from a roster sweep the caller already made.
pub fn listing_json_from(
    store: &Store,
    me: &Identity,
    list: &[Peer],
    sessions: &[roster::LocalSession],
) -> Value {
    let liveness = |id: &str| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .map_or("unknown", |s| s.liveness.as_str())
            .to_string()
    };
    let rows: Vec<Value> = list
        .iter()
        .map(|p| {
            json!({"chat": p.display, "name": p.name, "device": p.device_alias,
                   "kind": p.kind, "status": p.status, "local": p.local, "id": p.id,
                   "session": p.session, "cwd": p.cwd, "updatedMs": p.updated_ms,
                   "liveness": if p.local { liveness(&p.session) } else { p.liveness.clone() },
                   "lastSeenMs": p.last_seen_ms, "wake": p.wake, "account": p.account})
        })
        .collect();
    // `chats` is what a linked computer reads (links::RemoteListing).
    json!({"device": me.alias, "deviceId": super::local_device_id(store),
           "chatKey": super::chat_public_key(store), "peers": rows,
           "claudeAccountNames": rightkit_ai_accounts::names::to_json(&rightkit_ai_accounts::names::load()),
           "chats": list.iter().filter(|p| p.local).map(|p| json!({
               "session": p.session, "name": p.name, "kind": p.kind,
               "cwd": p.cwd, "status": p.status, "updated_ms": p.updated_ms,
               "liveness": liveness(&p.session), "wake": p.wake,
               "account": p.account})).collect::<Vec<_>>(),
           "hubRunning": store.relay_alive()})
}
