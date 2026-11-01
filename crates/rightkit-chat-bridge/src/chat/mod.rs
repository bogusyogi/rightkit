//! The host app chat: the cross-machine part of AI chats messaging each other.
//! Chats on one computer already talk natively (Claude to Claude over their
//! sockets, Claude to Codex with `codex queue`). the host app links computers over
//! ssh and delivers natively on the receiving side.
//!
//! Pieces
//! * `envelope`: the JSON message (`from`, `to`, `body`).
//! * `links`: linked computers (`links.json`) and the two remote commands run
//!   over ssh: list chats, post one envelope.
//! * `mesh`: chat over the device link (paired computers as links beside ssh): its four methods,
//!   who may use them, and the stamp that names the sender from the mesh.
//! * `store`: inboxes, links, registered chats, the hub's heartbeat.
//! * `receipts`: updates sent back to the sender after a send (seen, picked up, released,
//!   refused, expired), stored per sending chat, posted over the same link as replies.
//! * `roster`: which chats exist (`LocalSession`, `Peer`).
//! * `control`: how the CLI asks the running hub to deliver (files in the state folder).
//! * `hub`: what the hub runs: registration as a Claude peer, heartbeat,
//!   replies routed back over links, control requests, status.
//! * `deliver_claude`, `deliver_codex`: native delivery into a chat here.
//! * `deliver_hook`, `muse`: Muse (and any later hook harness) chats, found in Muse's own index and
//!   reached through the host app plugin's hook.
//! * `install`: puts Chat skill where Claude and Codex load it.
//! * `said`: what a chat's user typed in that chat, read from its transcript (`<cli> chat said`).
//!
//! The agent-facing surface is the CLI (`<cli> chat peers|send|link|status`),
//! built on `all_peers`, `identify` and `send_text`. `Receipt` carries a
//! `ReceiptState`: `Delivered` (the chat has it), `Queued` (the chat's own
//! queue has it), `Sent` (posted, not confirmed), `Held` (stored in the chat's
//! chat inbox, `<cli> chat inbox`; returned only after that write was
//! verified), `Refused`, `Unsupported` (this chat can't be pushed to) and
//! `Unknown` (the outcome could not be established).
//!
//! Chat switch: when Chat is off on a computer (`Store::chat_enabled`),
//! `send_text`, `receive`, `deliver_here` and the hub's control and reply paths
//! return `Refused` (`"Chat is off on <device>"`) and keep no inbox copy.

pub mod brand;
pub mod codex_hook;
pub mod confirm;
pub mod control;
pub mod cross_user;
pub mod deliver_claude;
pub mod deliver_codex;
pub mod deliver_hook;
pub mod envelope;
pub mod harness;
pub mod hub;
pub mod install;
pub mod links;
pub mod mesh;
pub mod muse;
pub mod read;
pub mod receipts;
pub mod roster;
pub mod said;
pub mod sendlog;
pub mod store;
mod tail;
#[cfg(feature = "harness-vectors")]
#[doc(hidden)]
pub mod vectors;
pub mod whoami;

pub use rightkit_device_link_types::MemberRole;

/// `n` random bytes as lowercase hex (session ids, tokens, message ids). The system RNG; if it
/// fails, the standard library's per-process hasher keys mixed with the clock.
pub(crate) fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom;
    let mut bytes = vec![0u8; n];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            let value = hasher.finish().to_le_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
    }
    rightkit_fs::digest::hex(&bytes)
}

/// Epoch seconds as RFC 3339; empty when out of range.
pub(crate) fn rfc3339(epoch: u64) -> String {
    i64::try_from(epoch)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|at| at.format_rfc3339())
        .unwrap_or_default()
}
pub use envelope::{Attachment, Envelope, EnvelopeError, Sender, Target};
pub use roster::{LocalSession, Peer, RosterEntry};
pub use store::Store;

use deliver_codex::DiscoveryError;
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// Not possible here (for example delivery into Claude on Windows).
    #[error("{0}")]
    Unsupported(String),
    #[error("no chat matches \"{0}\"; list the chats with the chat peers command")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Delivery(String),
    /// The chat closed or its files are gone.
    #[error("{0}")]
    SessionGone(String),
    /// The chat turned the connection away.
    #[error("{0}")]
    AuthRejected(String),
}

impl From<std::io::Error> for ChatError {
    fn from(error: std::io::Error) -> Self {
        ChatError::Io(error.to_string())
    }
}

impl From<EnvelopeError> for ChatError {
    fn from(error: EnvelopeError) -> Self {
        ChatError::Invalid(error.to_string())
    }
}

/// How a delivery ended, in the words Claude's cross-session messaging uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptState {
    /// The chat has the message.
    Delivered,
    /// The chat's own queue has it (it shows it at its next turn).
    Queued,
    /// Posted to the chat; receipt not confirmed.
    Sent,
    /// Stored in the chat's chat inbox, not shown yet.
    Held,
    /// The chat's side turned it away (for example too many messages).
    Refused,
    /// The chat can't be pushed to (other protocol, no Codex CLI, Windows).
    Unsupported,
    /// The outcome could not be established; it may or may not have arrived.
    Unknown,
}

impl ReceiptState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptState::Delivered => "delivered",
            ReceiptState::Queued => "queued",
            ReceiptState::Sent => "sent",
            ReceiptState::Held => "held",
            ReceiptState::Refused => "refused",
            ReceiptState::Unsupported => "unsupported",
            ReceiptState::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<ReceiptState> {
        match text {
            "delivered" => Some(ReceiptState::Delivered),
            "queued" => Some(ReceiptState::Queued),
            "sent" => Some(ReceiptState::Sent),
            "held" => Some(ReceiptState::Held),
            "refused" => Some(ReceiptState::Refused),
            "unsupported" => Some(ReceiptState::Unsupported),
            "unknown" => Some(ReceiptState::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub msg_id: String,
    /// The receiving chat.
    pub session: String,
    pub state: ReceiptState,
    pub detail: String,
}

impl Receipt {
    fn with(state: ReceiptState, detail: impl Into<String>) -> Receipt {
        Receipt {
            msg_id: String::new(),
            session: String::new(),
            state,
            detail: detail.into(),
        }
    }

    pub fn delivered(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Delivered, detail)
    }

    pub fn held(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Held, detail)
    }

    pub fn refused(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Refused, detail)
    }

    pub fn queued(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Queued, detail)
    }

    pub fn sent(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Sent, detail)
    }

    pub fn unknown(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Unknown, detail)
    }

    /// The chat can't be pushed to (other protocol, no Codex CLI, Windows). The
    /// delivery path then stores the message in Chat inbox and reports `Held`.
    pub fn unsupported(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Unsupported, detail)
    }
}

/// This computer as Chat knows it: the name chats are shown under.
#[derive(Debug, Clone)]
pub struct Identity {
    pub alias: String,
}

fn default_alias() -> String {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Computer".to_string());
    clean_alias(host.trim_end_matches(".local"))
}

/// A computer name safe to use in `<device>:<session>` ids: no `:`.
pub fn clean_alias(alias: &str) -> String {
    let cleaned: String = alias
        .chars()
        .map(|c| if c == ':' || c.is_control() { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "Computer".to_string()
    } else {
        cleaned
    }
}

/// This computer's name: what the running hub recorded, else the host name.
pub fn local_identity(store: &Store) -> Result<Identity, ChatError> {
    let alias = store
        .relay_status()
        .map(|s| s.alias)
        .filter(|a| !a.is_empty())
        .map(|a| clean_alias(&a))
        .unwrap_or_else(default_alias);
    Ok(Identity { alias })
}

/// A stable id for this install: `<chat>/device-id`, created on first use. It names this
/// computer to its peers (in chat listings and in every envelope it sends).
pub fn local_device_id(store: &Store) -> String {
    let path = store.root().join("device-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if id.len() == 36 {
            return id.to_string();
        }
    }
    let id = rightkit_logs::identity::random_uuid();
    let _ = rightkit_fs::private_cached::ensure_dir(store.root());
    // The id names this computer to its peers, so a torn or lost file would change it.
    let _ = rightkit_fs::atomic_write(&path, format!("{id}\n").as_bytes());
    id
}

/// Where this computer's chat signing key lives: `<chat>/chat-key.json`, owner-only.
const CHAT_KEY_FILE: &str = "chat-key.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ChatKeyFile {
    /// The Ed25519 seed, 32 bytes as hex.
    #[serde(default)]
    seed: String,
}

/// This computer's chat signing key (Ed25519), created on first use. It signs every envelope
/// posted to a link (`seal`); its public half is in this computer's chat listing (`chatKey`),
/// which linked computers pin. A key file that exists but cannot be read is never replaced
/// (a new key would make every link refuse this computer as another one): `None`, and
/// envelopes go unsigned, which their receivers show as unverified.
fn chat_key(store: &Store) -> Option<ring::signature::Ed25519KeyPair> {
    use ring::rand::SecureRandom;
    let existing: Option<ChatKeyFile> = store.read_state(CHAT_KEY_FILE);
    let seed_hex = match existing {
        Some(file) if !file.seed.is_empty() => file.seed,
        _ if store.root().join(CHAT_KEY_FILE).exists() => return None,
        _ => {
            let mut seed = [0u8; 32];
            ring::rand::SystemRandom::new().fill(&mut seed).ok()?;
            let fresh = rightkit_fs::digest::hex(&seed);
            let mut kept = String::new();
            // Under the file's lock: two first uses agree on one key.
            store
                .update_state(CHAT_KEY_FILE, |file: &mut ChatKeyFile| {
                    if file.seed.is_empty() {
                        file.seed = fresh;
                    }
                    kept = file.seed.clone();
                })
                .ok()?;
            kept
        }
    };
    let seed = rightkit_fs::digest::unhex(&seed_hex).filter(|s| s.len() == 32)?;
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).ok()
}

/// This computer's chat public key as hex, for its chat listing. `None` when there is no
/// usable key (see `chat_key`).
pub fn chat_public_key(store: &Store) -> Option<String> {
    use ring::signature::KeyPair;
    chat_key(store).map(|key| rightkit_fs::digest::hex(key.public_key().as_ref()))
}

/// `env` signed for posting over `link`: the signature covers the envelope and the receiving
/// computer's device id as `link` pinned it (`Envelope::signing_input`). Without a usable key
/// the envelope goes unsigned.
pub(crate) fn seal(store: &Store, link: &links::Link, mut env: Envelope) -> Envelope {
    env.sig.clear();
    if let Some(key) = chat_key(store) {
        let input = env.signing_input(link.device_id.as_deref().unwrap_or(""));
        env.sig = rightkit_fs::digest::hex(key.sign(&input).as_ref());
    }
    env
}

/// Whether `env` carries a signature by the chat key `link` pinned, made for this computer.
/// False for a link with no pinned key, an unsigned envelope or any signature that fails.
fn signed_by(store: &Store, link: &links::Link, env: &Envelope) -> bool {
    use ring::signature::{ED25519, UnparsedPublicKey};
    let Some(key) = link
        .chat_key
        .as_deref()
        .and_then(rightkit_fs::digest::unhex)
    else {
        return false;
    };
    let Some(sig) = rightkit_fs::digest::unhex(&env.sig) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&env.signing_input(&local_device_id(store)), &sig)
        .is_ok()
}

/// The registry device id of the device link that is the twin of the ssh link whose pinned chat
/// key signed `env`: the signature proves the ssh link, and that link's own pinned device id
/// (not the id the sender wrote in the envelope) names the twin. `None` when no saved ssh link's
/// key verifies, or its computer has no device-link twin.
fn signed_twin_device_id(store: &Store, env: &Envelope) -> Option<String> {
    store.links().into_iter().find_map(|ssh| {
        if mesh::mesh_id(&ssh).is_some() || !signed_by(store, &ssh, env) {
            return None;
        }
        let pinned = ssh.device_id.as_deref()?;
        let twin = links::find_by_device_id(store, pinned)?;
        mesh::mesh_id(&twin).map(str::to_string)
    })
}

/// Every chat that can be messaged: this computer's and the linked ones'
/// (asked over ssh, in parallel).
pub fn all_peers(store: &Store, me: &Identity) -> Vec<Peer> {
    all_peers_within(store, me, None)
}

/// [`all_peers`] for the `peers` listing: `Some(wait)` bounds every link's poll (C-D4). A send
/// resolves its target through `all_peers` and keeps the full wait.
pub fn all_peers_within(store: &Store, me: &Identity, wait: Option<Duration>) -> Vec<Peer> {
    roster::merge_with(
        &roster::local_sessions(store),
        &me.alias,
        &links::list_all_within(store, wait),
        &links::live_sessions_elsewhere(store),
    )
}

/// This computer's chats only (what a linked computer asks for). A Claude Desktop chat whose
/// process runs on another computer, and that a linked computer last listed live, is not listed
/// here: this computer's row for it is stale, and listing it twice would show the chat on two
/// computers.
pub fn local_peers(store: &Store, me: &Identity) -> Vec<Peer> {
    local_peers_from(store, me, &roster::local_sessions(store))
}

/// [`local_peers`] from a roster sweep the caller already made, so one request sweeps the chat
/// folders once (issue #560: a linked computer's `chat.peers` swept them twice, ~290 MB of reads
/// per request on the Dell).
pub fn local_peers_from(
    store: &Store,
    me: &Identity,
    sessions: &[roster::LocalSession],
) -> Vec<Peer> {
    roster::merge_with(
        sessions,
        &me.alias,
        &[],
        &links::live_sessions_elsewhere(store),
    )
}

/// The refusal every entry point gives while Chat is off here.
fn chat_off(store: &Store, msg_id: &str, session: &str) -> Option<Receipt> {
    if store.chat_enabled() {
        return None;
    }
    let device = local_identity(store)
        .map(|me| me.alias)
        .unwrap_or_else(|_| default_alias());
    Some(Receipt {
        msg_id: msg_id.to_string(),
        session: session.to_string(),
        ..Receipt::refused(format!("Chat is off on {device}"))
    })
}

/// `chat_off` for the hub (`hub::handle_control`, `hub::on_local_reply`).
pub(crate) fn chat_off_detail(store: &Store) -> Option<String> {
    chat_off(store, "", "").map(|r| r.detail)
}

/// Put `env` into a chat on this computer from this process: Claude and Codex
/// chats are pushed directly (`deliver_claude::deliver_via`,
/// `deliver_codex::deliver`); anything not delivered keeps the message in the
/// chat's inbox for `<cli> chat inbox`, and says `Held` only once that write
/// succeeded. `reply_socket` (the sending Claude chat's own messaging socket)
/// is where a Claude target replies; without one the hub's reply listener is
/// used, and the route back is saved for it. A message this chat already has (the same id) is
/// not delivered again: the receipt is the one it got the first time (`deliver_once`).
pub fn deliver_local_via(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    deliver_once(store, &session.id, env, || {
        deliver_local_push(store, session, env, reply_socket)
    })
}

/// The receipt for a message the chat's delivery record already has (`Store::begin_delivery`):
/// the one it got then, a delivery still running, or a reused id.
fn repeat_receipt(env: &Envelope, session: &str, seen: store::Delivery) -> Receipt {
    let receipt = |state: ReceiptState, detail: String| Receipt {
        msg_id: env.id.clone(),
        session: session.to_string(),
        state,
        detail,
    };
    let which = |_msg_id: &str| "This message".to_string();
    match seen {
        store::Delivery::Done {
            msg_id,
            state,
            detail,
        } => receipt(
            ReceiptState::parse(&state).unwrap_or(ReceiptState::Unknown),
            format!(
                "{} already reached this chat and was not delivered again. {detail}",
                which(&msg_id)
            )
            .trim()
            .to_string(),
        ),
        // Not yet in the chat or its inbox, so not `sent`: the caller checks `inbox`/`status`.
        store::Delivery::InProgress { msg_id } => receipt(
            ReceiptState::Unknown,
            format!(
                "{} is being delivered to this chat right now (or its delivery stopped \
                 midway); it is not delivered twice. Check inbox and status.",
                which(&msg_id)
            ),
        ),
        store::Delivery::Conflict | store::Delivery::New => receipt(
            ReceiptState::Refused,
            "message id reused with different content".to_string(),
        ),
    }
}

/// Deliver `env` to the chat `session` at most once. The chat's delivery record is checked and
/// the message marked in progress under one lock (`Store::begin_delivery`); a repeat gets the
/// first delivery's receipt (`repeat_receipt`) and `push` does not run. `push` returns its
/// receipt and whether the message was kept (reached the chat or its inbox): a kept one is
/// recorded with that receipt, anything else is forgotten so a resend tries again. When the
/// record cannot be taken (its lock timed out or it could not be written) nothing is pushed:
/// an untracked push is what let a retried id reach the chat again. The message goes to
/// the chat's inbox instead, so it is never dropped, and the sender gets `unknown`.
fn deliver_once(
    store: &Store,
    session: &str,
    env: &Envelope,
    push: impl FnOnce() -> (Receipt, bool),
) -> Receipt {
    match store.begin_delivery(session, env) {
        Ok(store::Delivery::New) => {}
        Ok(seen) => return repeat_receipt(env, session, seen),
        // Never dropped: the message is kept in the chat's inbox (unrecorded, so it is read
        // once from there), and the sender is told `unknown`.
        Err(_) => {
            return match store.append_inbox(session, env) {
                Ok(_) => {
                    // Recorded as held, so a resend of this id is answered from the record.
                    store.record_inbox_delivery(session, env);
                    Receipt::unknown(
                        "The app could not record the message for this chat; it is kept in the chat inbox. Check inbox before resending.",
                    )
                }
                Err(e) => Receipt::unknown(format!(
                    "The app could not record or store the message for this chat ({e}); send it again."
                )),
            };
        }
    }
    let (receipt, kept) = push();
    if kept {
        let state = receipt.state.as_str();
        let _ = store.finish_delivery(session, &env.id, state, &receipt.detail);
    } else {
        store.forget_delivery(session, &env.id);
    }
    receipt
}

/// A control request for a delivery that the hub never answered and that has expired (the hub
/// stopped while it waited): its message is kept in the chat's inbox, once, so a sender told
/// the hub had it does not lose it.
pub(crate) fn keep_unanswered(store: &Store, session: &str, env: &Envelope) {
    let _ = deliver_once(store, session, env, || {
        match store.append_inbox(session, env) {
            Ok(_) => (
                Receipt {
                    msg_id: env.id.clone(),
                    session: session.to_string(),
                    ..Receipt::held("The app stopped before delivering it; kept in the chat inbox.")
                },
                true,
            ),
            Err(e) => (
                Receipt::unknown(format!("could not store the message: {e}")),
                false,
            ),
        }
    });
}

/// `deliver_local_via` without the delivery record: the push itself, and whether the message
/// was kept (it reached the chat, or its inbox).
fn deliver_local_push(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> (Receipt, bool) {
    let receipt = |state: ReceiptState, detail: String| Receipt {
        msg_id: env.id.clone(),
        session: session.id.clone(),
        state,
        detail,
    };
    // Every push names `<cli> chat reply <id>` in its header (Claude and Codex alike, over
    // any route), so the route for that id is stored before the text can reach the chat. If
    // it cannot be stored, nothing is delivered and the receipt says why.
    if let Err(why) = save_reply_route_for(store, session, env) {
        return (receipt(ReceiptState::Refused, why), false);
    }
    let pushed = match session.kind.as_str() {
        "claude" => deliver_claude::deliver_via(session, env, reply_socket),
        "codex" => deliver_codex::deliver(store, session, env),
        // Muse reads its messages through the host app plugin's hook (core/src/chat/deliver_hook.rs).
        // Any harness that registers through the plugin's hook is reached the same way.
        kind if codex_hook::valid_hook_harness(kind) => deliver_hook::deliver(session, env),
        // ChatGPT has no socket to push into: it reads its inbox through the gateway's
        // `chat_inbox` (core/src/remote/gptchat.rs).
        "chatgpt" => Err(ChatError::Unsupported(
            "ChatGPT reads this with its chat_inbox tool, whenever it chooses to".to_string(),
        )),
        other => Err(ChatError::Unsupported(format!(
            "can't push into a {other} chat; it reads its chat inbox"
        ))),
    };
    let (state, detail) = match pushed {
        Ok(r) if reached_chat(r.state) => {
            return (
                Receipt {
                    msg_id: env.id.clone(),
                    session: session.id.clone(),
                    ..r
                },
                true,
            );
        }
        Ok(r) => (r.state, r.detail),
        // A message the chat can never take (too long to send) is refused, not kept.
        Err(ChatError::Invalid(why)) => return (receipt(ReceiptState::Refused, why), false),
        Err(e) => (ReceiptState::Held, e.to_string()),
    };
    match store.append_inbox(&session.id, env) {
        Ok(_) => {
            let state = match state {
                ReceiptState::Refused | ReceiptState::Unknown => state,
                _ => ReceiptState::Held,
            };
            (
                receipt(state, format!("{detail} (kept in the chat inbox)")),
                true,
            )
        }
        // Not recorded, even when the push's outcome is unknown: the sender is told `unknown`
        // and resends once, which must be able to deliver.
        Err(e) => (
            receipt(
                ReceiptState::Unknown,
                format!("could not store the message: {e} ({detail})"),
            ),
            false,
        ),
    }
}

/// Store the route a `<cli> chat reply <env.id>` from `session` follows, under the id the
/// header text prints. The id must survive the header's cleaning unchanged (letters, digits
/// and `-`, at most 64), and an id already routed to a different sender or chat is never
/// re-pointed: the route only lets the receiver of a message answer its sender.
fn save_reply_route_for(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
) -> Result<(), String> {
    let clean = !env.id.is_empty()
        && env.id.len() <= 64
        && env
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !clean {
        return Err(
            "the message id can't be used in a reply command, so no reply route was stored \
             and the message was not delivered"
                .to_string(),
        );
    }
    // The route names the sender's computer as this computer's link does, when the message's
    // link can be determined (the sender's own name for itself can differ, #428/#366).
    let link = sender_link(store, env);
    let from_device = link
        .as_ref()
        .map(|link| link.device.clone())
        .unwrap_or_else(|| env.from.device.clone());
    // The registry device id `chat::confirm` reads the sender's role by: the device the mesh
    // stamped, else the device link that is the twin of the ssh link whose pinned chat key signed
    // the envelope (never the sender's own claimed device id). A sender on this computer is the
    // owner's own. Anything else (an ssh link with no device-link twin) is `None`: unknown, never
    // "not required".
    let mesh_device_id = if !env.mesh.is_empty() {
        Some(env.mesh.clone())
    } else if let Some(id) = signed_twin_device_id(store, env) {
        Some(id)
    } else if link.is_none() && claims_this_computer(store, env) {
        Some(confirm::LOCAL_SENDER.to_string())
    } else {
        None
    };
    if let Some(old) = store.reply_route(&env.id)
        && (old.from_device != from_device
            || old.from_session != env.from.session
            || old.to_session != session.id)
    {
        return Err(
            "the message id is already routed to another chat, so no reply route was stored \
             and the message was not delivered"
                .to_string(),
        );
    }
    store
        .save_reply_route(&store::ReplyRoute {
            msg_id: env.id.clone(),
            from_device,
            from_session: env.from.session.clone(),
            to_session: session.id.clone(),
            to_name: session.name.clone(),
            created_ms: envelope::now_ms(),
            mesh_device_id,
            payload_hash: store::payload_hash(env),
            to_pid: session.pid,
            to_start: deliver_claude::text_of(&session.raw["procStart"]),
        })
        .map_err(|e| {
            format!("could not store the reply route ({e}), so the message was not delivered")
        })
}

/// The harness a chat on this computer runs in: "claude", "codex", "muse" or another hook harness, empty when it is not
/// one of this computer's chats (a plain terminal).
pub fn harness_of_chat(store: &Store, id: &str) -> String {
    roster::local_sessions(store)
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.kind)
        .unwrap_or_default()
}

/// The link a message came over, as this computer knows it. The sender's own words are not
/// trusted for the machine: the sender's stable device id (`from.device_id`, which the sender
/// also reports in its chat listing and the link pinned) names the link. `None` when no link
/// can be determined.
fn sender_link(store: &Store, env: &Envelope) -> Option<links::Link> {
    let id = env.from.device_id.trim();
    if id.is_empty() || id == local_device_id(store) {
        return None;
    }
    links::find_by_device_id(store, id)
}

/// Who sent `env`, as this computer states it. The machine is stamped here, with the name of
/// the link the message came over (`sender_link`), not the name the sender gave itself, and
/// only when the envelope is signed by the chat key that link pinned (`signed_by`); otherwise
/// the machine is marked unverified. The harness and chat are the sender's own labels and
/// are shown as such. A message from this computer has no machine (its device is left out;
/// `receive` refuses a post that claims this computer). The sender counts as verified only
/// when its the host app proved the sending chat from its process (`Envelope::verified`) and that
/// The host app is this computer's or verified; only then are the user's stamped words shown.
pub fn origin_of(env: &Envelope) -> envelope::Origin {
    let store = Store::open_default().ok();
    let claimed = env.from.device.trim().to_string();
    // Arrived over the device link: the engine stamped the sender's name and id from the mesh's
    // authenticated stream (`mesh::serve`), so no chat key is needed to believe the machine.
    let meshed = !env.mesh.is_empty() && env.mesh == env.from.device_id;
    // Only the owner's own computers vouch for the machine; a grant holder shows unverified.
    let mesh_trusted = meshed && env.mesh_owner;
    let linked = if meshed {
        None
    } else {
        store.as_ref().and_then(|store| {
            sender_link(store, env).map(|link| (signed_by(store, &link, env), link.device))
        })
    };
    let from_here = !meshed
        && store
            .as_ref()
            .is_some_and(|store| claims_this_computer(store, env));
    let (machine, unverified) = if meshed {
        (Some(claimed), !mesh_trusted)
    } else {
        match linked {
            Some((signed, name)) => (Some(name), !signed),
            None if from_here => (None, false),
            None => (Some(claimed), true),
        }
    };
    // A machine no link vouches for cannot vouch for its chat or its user's words either.
    let sender_verified = !unverified && env.verified;
    envelope::Origin {
        harness: envelope::harness_word(&env.from),
        chat: envelope::chat_title(&env.from),
        machine,
        unverified,
        sender_verified,
        role: if meshed && env.mesh_role != "computer" {
            envelope::plain_label(&env.mesh_role, 16)
        } else {
            String::new()
        },
        said: if sender_verified {
            env.said.clone()
        } else {
            Vec::new()
        },
        attachment: attachment_note(env),
    }
}

/// Whether `env` names this computer as its sender: this computer's device id, or no id and
/// this computer's name with no link answering to it. An envelope with another computer's id
/// and this computer's name does not: it is shown under that name, unverified.
fn claims_this_computer(store: &Store, env: &Envelope) -> bool {
    let id = env.from.device_id.trim();
    if !id.is_empty() {
        return id == local_device_id(store);
    }
    let me = local_identity(store).map(|me| me.alias).unwrap_or_default();
    env.from.device.trim().eq_ignore_ascii_case(&me) && sender_link(store, env).is_none()
}

/// Whether the chat `session` has not yet been handed a the host app Chat message, so the message
/// for it carries the full introduction. A store that cannot be read counts as not yet met.
pub fn first_contact_pending(session: &str) -> bool {
    !Store::open_default().is_ok_and(|store| store.chat_met(session))
}

fn reached_chat(state: ReceiptState) -> bool {
    matches!(
        state,
        ReceiptState::Delivered | ReceiptState::Queued | ReceiptState::Sent
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub msg_id: String,
    pub to: Peer,
    /// A `ReceiptState` word, from whichever computer the chat is on.
    pub status: String,
    pub detail: String,
}

/// The chat a chat command runs in: who a message is from and where a
/// native reply should land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub id: String,
    pub name: String,
    /// The calling Claude chat's own messaging socket.
    pub reply_socket: Option<String>,
    /// Proven from the calling process (`identify`): this command descends from the one
    /// process the chat's own registry names for it. Never from `--from`, a route or an
    /// environment variable alone. Only then is the envelope marked verified and the user's
    /// words stamped from its transcript, so a sender cannot attach another
    /// chat's words to its message.
    pub detected: bool,
}

/// The name a chat signs its messages with. A Codex chat says so (`"Codex chat <title>"`), so the
/// receiving chat's `from-name` reads `"Codex chat <title> on <device> via the host app"` (from another
/// computer; `"Codex chat <title> via the host app"` from this one, `deliver_claude::sender_name`) and its
/// reader can tell the sender is a Codex chat (docs/chat-delivery-research-2026-10-11.md).
pub fn sender_name_of(session: &LocalSession) -> String {
    if session.kind == "codex" && !session.name.to_lowercase().starts_with("codex") {
        format!("Codex chat {}", session.name)
    } else {
        session.name.clone()
    }
}

/// The chat id and name used outside any chat (a plain terminal).
pub const CLI_CALLER_ID: &str = "cli";

/// The caller outside any chat (a plain terminal).
fn cli_caller() -> Caller {
    Caller {
        id: CLI_CALLER_ID.to_string(),
        name: format!("{} CLI", brand::product()),
        reply_socket: None,
        detected: false,
    }
}

/// How many processes above this command are searched for the chat it runs in.
const ANCESTRY_DEPTH: usize = 16;

/// A process above this command: its id and its name in lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestor {
    pub pid: u32,
    pub name: String,
}

/// One process in this command's ancestry: what the process table says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestryStep {
    pub pid: u32,
    /// Lowercase image name.
    pub name: String,
    pub parent: Option<u32>,
    /// Start time in seconds since the epoch; 0 when the host would not say.
    pub start: u64,
}

/// This command and the processes above it as the process table shows them, and why the walk
/// stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestry {
    pub own: AncestryStep,
    /// Nearest first, at most `ANCESTRY_DEPTH`.
    pub ancestors: Vec<AncestryStep>,
    pub ended: String,
}

/// Walk up from this process. A parent that started after its child is not its parent (its id
/// was reused after the real parent ended) and ends the walk, as does a parent no longer in the
/// process table. `None` when this process is not in the process table.
pub fn read_ancestry() -> Option<Ancestry> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let step = |p: &sysinfo::Process| AncestryStep {
        pid: p.pid().as_u32(),
        name: p.name().to_string_lossy().to_ascii_lowercase(),
        parent: p.parent().map(|pid| pid.as_u32()),
        start: p.start_time(),
    };
    let mut child = system.process(Pid::from_u32(std::process::id()))?;
    let own = step(child);
    let mut ancestors = Vec::new();
    let ended = loop {
        if ancestors.len() >= ANCESTRY_DEPTH {
            break format!("depth limit {ANCESTRY_DEPTH}");
        }
        let Some(parent_pid) = child.parent() else {
            break "no parent process id".to_string();
        };
        let Some(parent) = system.process(parent_pid) else {
            break format!("parent {parent_pid} is not in the process table (it has ended)");
        };
        let (born, child_born) = (parent.start_time(), child.start_time());
        if parent.pid() == child.pid() {
            break "a process is its own parent".to_string();
        }
        if born > 0 && child_born > 0 && born > child_born {
            break format!(
                "parent {} started at {born}, after its child {} at {child_born} (pid reused)",
                parent.pid(),
                child.pid()
            );
        }
        ancestors.push(step(parent));
        child = parent;
    };
    Some(Ancestry {
        own,
        ancestors,
        ended,
    })
}

/// This command's ancestors, nearest first, at most `ANCESTRY_DEPTH` (`read_ancestry`). `None`
/// when this process is not in the process table, so nothing can be proven.
pub fn caller_ancestors() -> Option<Vec<Ancestor>> {
    read_ancestry().map(|found| {
        found
            .ancestors
            .into_iter()
            .map(|a| Ancestor {
                pid: a.pid,
                name: a.name,
            })
            .collect()
    })
}

/// What runs as one process above this command, as the chat registries say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatProcess {
    /// Exactly one chat: the Claude chat whose own registry file (`sessions/<pid>.json`) names
    /// this process. The caller is proven (`detected`).
    One(Caller),
    /// A process of the `kind` harness (Codex, Muse), named for it or registered by its hook.
    /// One such process can run several chats (the Codex app's server runs every thread), so it
    /// cannot say by itself which chat runs the command.
    Harness(String),
}

/// The environment variable a harness names its chat in, for the commands it runs.
fn harness_variable(kind: &str) -> String {
    if kind == "codex" {
        "CODEX_THREAD_ID".to_string()
    } else {
        format!("{}_SESSION_ID", kind.to_ascii_uppercase().replace('-', "_"))
    }
}

/// The Claude chat whose own registry file (`<dir>/<pid>.json`) names process `pid`, for the
/// first folder in `claude_dirs` where one verifies: its `pid` is `pid`, it is not the host app's own
/// registration, it names a session, and its recorded start is the process's (a file an ended
/// chat left names a pid that now belongs to another process). The notes say, folder by
/// folder, why each file was skipped (`<cli> chat whoami`).
fn registry_chat(
    sessions: &[LocalSession],
    claude_dirs: &[std::path::PathBuf],
    pid: u32,
) -> (Option<Caller>, Vec<String>) {
    let mut notes = Vec::new();
    for dir in claude_dirs {
        let path = dir.join(format!("{pid}.json"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                notes.push(format!(
                    "{}: no registry file ({})",
                    dir.display(),
                    error.kind()
                ));
                continue;
            }
        };
        // Claude writes plain JSON; a byte-order mark is tolerated the way the roster does.
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
            notes.push(format!(
                "{}: registry file is not valid JSON",
                dir.display()
            ));
            continue;
        };
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        if let Some(entry) = text("entrypoint")
            .filter(|e| e.starts_with(brand::brand().entrypoint_prefix.as_str()) || e == "cc-post")
        {
            notes.push(format!(
                "{}: registry file is the app's own (entrypoint {entry}), not a chat",
                dir.display()
            ));
            continue;
        }
        let recorded_pid = value.get("pid").and_then(serde_json::Value::as_u64);
        if recorded_pid != Some(u64::from(pid)) {
            notes.push(format!(
                "{}: registry file names pid {recorded_pid:?}, not {pid}",
                dir.display()
            ));
            continue;
        }
        let Some(id) = text("sessionId") else {
            notes.push(format!("{}: registry file has no sessionId", dir.display()));
            continue;
        };
        let recorded = text("procStart");
        let domain = text("pidDomain");
        match deliver_claude::start_check(pid, recorded.as_deref(), domain.as_deref()) {
            None => {}
            Some(Ok(gap)) if gap <= 2.0 => {}
            Some(Ok(gap)) => {
                notes.push(format!(
                    "{}: start identity false: the recorded start differs from the live process by {gap:.0} s (pid reused by another process)",
                    dir.display()
                ));
                continue;
            }
            Some(Err(why)) => {
                notes.push(format!("{}: start identity false: {why}", dir.display()));
                continue;
            }
        }
        let listed = sessions.iter().find(|s| s.id == id);
        return (
            Some(Caller {
                name: listed.map_or_else(|| "Claude chat".to_string(), sender_name_of),
                reply_socket: text("messagingSocketPath"),
                id,
                detected: true,
            }),
            notes,
        );
    }
    (None, notes)
}

/// What `ancestor` is, from the Claude registry folders `claude_dirs` (a `<pid>.json` that
/// names it, whose recorded start matches the process) and the chats harness hooks
/// registered (`registered`). the host app's own registration (the hub's) is not a chat.
pub fn chat_process(
    sessions: &[LocalSession],
    registered: &[store::RegisteredSession],
    claude_dirs: &[std::path::PathBuf],
    ancestor: &Ancestor,
) -> Option<ChatProcess> {
    let pid = ancestor.pid;
    if let (Some(caller), _) = registry_chat(sessions, claude_dirs, pid) {
        return Some(ChatProcess::One(caller));
    }
    let harness = |kind: &str| kind == "codex" || codex_hook::valid_hook_harness(kind);
    if let Some(found) = registered
        .iter()
        .find(|r| r.pid == pid && harness(r.kind.as_str()))
    {
        return Some(ChatProcess::Harness(found.kind.clone()));
    }
    std::iter::once(&"codex")
        .chain(codex_hook::HOOK_HARNESSES)
        .find(|kind| ancestor.name.contains(**kind))
        .map(|kind| ChatProcess::Harness(kind.to_string()))
}

/// The calling chat, proven from the calling process rather than taken from its environment
///. `ancestors` are this command's ancestor processes, nearest first (`None`: they could
/// not be read); `chat_process` says what each one is. The nearest one that is a chat decides:
/// * a Claude chat's own process: that chat, proven (`detected`). A
///   `CLAUDE_CODE_MESSAGING_SOCKET` naming another chat's socket is refused.
/// * a harness process (Codex, Muse): the chat its variable (`CODEX_THREAD_ID`,
///   `MUSE_SESSION_ID`) names, not proven, because one such process runs several chats: its
///   messages are marked unverified and carry no user words. A variable naming a chat of
///   another harness is refused; no variable is a plain terminal.
/// * no chat above it: a plain terminal (`the host app CLI`), and any chat variable in the
///   environment is refused, since nothing shows this command runs in that chat. So is a
///   chat variable when the processes could not be read.
///
/// Every variable is only checked against the process, never trusted alone.
pub fn prove_caller(
    sessions: &[LocalSession],
    env: &dyn Fn(&str) -> Option<String>,
    ancestors: Option<&[Ancestor]>,
    chat_process: &dyn Fn(&Ancestor) -> Option<ChatProcess>,
) -> Result<Caller, ChatError> {
    let var = |name: &str| {
        env(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let refuse = |name: &str, value: &str, why: &str| {
        ChatError::Invalid(format!(
            "{name} names the chat \"{value}\", but {why}. The app takes the sender from the \
             calling process, not from the environment, so nothing was sent as that chat"
        ))
    };
    let mut claims: Vec<String> = [
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
    ]
    .iter()
    .map(|name| name.to_string())
    .collect();
    claims.extend(
        std::iter::once(&"codex")
            .chain(codex_hook::HOOK_HARNESSES)
            .map(|kind| harness_variable(kind)),
    );
    // A harness registered through the plugin's hook names its chat the same way.
    for kind in sessions
        .iter()
        .map(|s| s.kind.as_str())
        .filter(|kind| codex_hook::valid_hook_harness(kind))
    {
        let name = harness_variable(kind);
        if !claims.contains(&name) {
            claims.push(name);
        }
    }
    let unproven = |why: &str| match claims
        .iter()
        .find_map(|name| var(name.as_str()).map(|value| (name.as_str(), value)))
    {
        Some((name, value)) => Err(refuse(name, value.as_str(), why)),
        None => Ok(cli_caller()),
    };
    let Some(ancestors) = ancestors else {
        return unproven("this computer's process list could not be read to check it");
    };
    for ancestor in ancestors {
        match chat_process(ancestor) {
            None => continue,
            Some(ChatProcess::One(caller)) => {
                if let Some(socket) = var("CLAUDE_CODE_MESSAGING_SOCKET")
                    && caller
                        .reply_socket
                        .as_deref()
                        .is_some_and(|own| own != socket)
                {
                    return Err(refuse(
                        "CLAUDE_CODE_MESSAGING_SOCKET",
                        socket.as_str(),
                        format!("this command runs in the Claude chat \"{}\"", caller.name)
                            .as_str(),
                    ));
                }
                return Ok(caller);
            }
            Some(ChatProcess::Harness(kind)) => {
                let variable = harness_variable(&kind);
                let Some(id) = var(variable.as_str()) else {
                    return Ok(cli_caller());
                };
                let listed = sessions.iter().find(|s| s.id == id);
                if let Some(other) = listed.filter(|s| s.kind != kind) {
                    return Err(refuse(
                        variable.as_str(),
                        id.as_str(),
                        format!(
                            "that is a {} chat and this command runs in {kind}",
                            other.kind
                        )
                        .as_str(),
                    ));
                }
                let word: String = kind
                    .chars()
                    .take(1)
                    .flat_map(char::to_uppercase)
                    .chain(kind.chars().skip(1))
                    .collect();
                return Ok(Caller {
                    name: listed.map_or_else(|| format!("{word} chat"), sender_name_of),
                    id,
                    reply_socket: None,
                    detected: false,
                });
            }
        }
    }
    unproven("this command does not run inside any chat")
}

/// Work out the calling chat. With `from` (`--from`, a chat id or title on this computer) it is
/// a lookup by name, for reading that chat's inbox or receipts: it proves nothing (`detected`
/// is false) and a send never takes its sender from it. Without `from`, the chat is proven
/// from this process's ancestors (`prove_caller`), reading Claude's registry folders and the
/// chats the host app registered.
pub fn identify(
    sessions: &[LocalSession],
    env: &dyn Fn(&str) -> Option<String>,
    from: Option<&str>,
) -> Result<Caller, ChatError> {
    let caller_of = |s: &LocalSession| Caller {
        id: s.id.clone(),
        name: sender_name_of(s),
        reply_socket: if s.kind == "claude" {
            s.messaging_socket.clone()
        } else {
            None
        },
        detected: false,
    };
    if let Some(wanted) = from {
        let wanted = wanted.trim().to_lowercase();
        let named: Vec<&LocalSession> = sessions
            .iter()
            .filter(|s| s.name.to_lowercase() == wanted)
            .collect();
        let hit = sessions
            .iter()
            .find(|s| s.id.to_lowercase() == wanted)
            .or(if named.len() == 1 {
                Some(named[0])
            } else {
                None
            });
        return hit.map(caller_of).ok_or(ChatError::NotFound(wanted));
    }
    let registered = Store::open_default()
        .map(|store| store.registered_sessions())
        .unwrap_or_default();
    let claude_dirs: Vec<std::path::PathBuf> = deliver_claude::sessions_dirs()
        .into_iter()
        .map(|(dir, _)| dir)
        .collect();
    let ancestry = read_ancestry();
    let ancestors: Option<Vec<Ancestor>> = ancestry.as_ref().map(|found| {
        found
            .ancestors
            .iter()
            .map(|a| Ancestor {
                pid: a.pid,
                name: a.name.clone(),
            })
            .collect()
    });
    prove_caller(sessions, env, ancestors.as_deref(), &|ancestor| {
        chat_process(sessions, &registered, &claude_dirs, ancestor)
    })
    .map_err(|error| match error {
        ChatError::Invalid(message) => ChatError::Invalid(format!(
            "{message}. Proof trace: {}",
            proof_trace(sessions, ancestry.as_ref(), &claude_dirs)
        )),
        other => other,
    })
}

/// Whether the proven calling chat is the Claude chat a reply route was saved for, after that
/// chat's session id changed while its process did not (compaction or a session switch inside the
/// same live Claude; #506). It holds only when the proof came from the registry (`detected`), the
/// chat is a Claude chat listed here, and it runs in the same process id AND the same recorded
/// process start the route saved. A route without a recorded start is never accepted, and a pid
/// reused by another process has another start, so a different chat is never taken for the one
/// that received the message.
pub fn route_successor(
    sessions: &[LocalSession],
    proven: &Caller,
    to_pid: Option<u32>,
    to_start: Option<&str>,
) -> bool {
    let (Some(pid), Some(start)) = (to_pid, to_start.filter(|s| !s.is_empty())) else {
        return false;
    };
    proven.detected
        && sessions.iter().any(|s| {
            s.id == proven.id
                && s.kind == "claude"
                && s.pid == Some(pid)
                && deliver_claude::text_of(&s.raw["procStart"]).as_deref() == Some(start)
        })
}

/// One line saying which step of the sender proof failed, for a refusal and `<cli> chat whoami`:
/// how far the walk went and why it ended, the chain of processes seen, what was expected, and
/// why the registry files of the most likely chat process (a `claude`-named ancestor, else the
/// last one seen) did not count. Names pids, image names and folders only, never a secret.
pub fn proof_trace(
    sessions: &[LocalSession],
    ancestry: Option<&Ancestry>,
    claude_dirs: &[std::path::PathBuf],
) -> String {
    let Some(found) = ancestry else {
        return "this process is not in the process table, so no ancestor could be checked"
            .to_string();
    };
    let chain: Vec<String> = found
        .ancestors
        .iter()
        .map(|a| format!("{}:{}", a.pid, a.name))
        .collect();
    let last = found
        .ancestors
        .last()
        .map_or_else(|| "none".to_string(), |a| format!("{} {}", a.pid, a.name));
    let suspect = found
        .ancestors
        .iter()
        .find(|a| a.name.contains("claude"))
        .or(found.ancestors.last());
    let why = suspect.map_or_else(String::new, |a| {
        let (_, notes) = registry_chat(sessions, claude_dirs, a.pid);
        format!(
            "; registry check of {} {}: {}",
            a.pid,
            a.name,
            notes.join(" | ")
        )
    });
    format!(
        "this command {} {}; walked {} of at most {ANCESTRY_DEPTH} ancestors [{}]; walk ended: {}; \
         last process seen: {last}; expected: an ancestor whose <claude config>/sessions/<pid>.json \
         names that pid with a matching start, in one of {} registry folders, or a Codex/hook \
         harness process{why}",
        found.own.pid,
        found.own.name,
        found.ancestors.len(),
        chain.join(" < "),
        found.ended,
        claude_dirs.len()
    )
}

/// Whether a direct post from this process reaches a Claude chat here. On
/// macOS a chat accepts a peer message only from a process descending from a
/// registered session: a chat's own shell is one; `<cli>` run by sshd is not.
fn direct_post_trusted() -> bool {
    !cfg!(unix) || std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").is_ok_and(|v| !v.trim().is_empty())
}

/// How long a delivery handed to the hub waits for the chat's verdict. Past it a request the hub
/// took is reported `queued` (it has the message and goes on delivering it, or keeps it in the
/// chat's inbox), so a send never
/// waits for a slow chat. A post from another computer runs this wait inside the ssh command
/// that computer gives up on at `links::REMOTE_TIMEOUT`, so it ends with room for ssh to start.
const HUB_VERDICT_WAIT: Duration = Duration::from_secs(15);
const _: () = assert!(HUB_VERDICT_WAIT.as_millis() + 5_000 <= links::REMOTE_TIMEOUT.as_millis());

/// Put `env` into a chat on this computer the way that works here: through
/// the running hub (a registered peer) when a direct post would be dropped,
/// else directly. `reply_socket` as in `deliver_local_via`. Refused while the
/// chat is off.
pub fn deliver_here(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    let receipt = deliver_here_by_route(store, session, env, reply_socket);
    if reached_chat(receipt.state) {
        store.note_chat_met(&session.id);
    }
    receipt
}

/// `deliver_here` without the first-contact record: the route that works here.
fn deliver_here_by_route(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    if let Some(off) = chat_off(store, &env.id, &session.id) {
        return off;
    }
    let needs_hub = session.kind == "claude" && !direct_post_trusted();
    // The hub owns the reply listener on both platforms: a delivery it posts advertises a
    // real reply address, one the CLI posts only a placeholder. So the hub is preferred
    // whenever it runs; the direct post stays for when it does not (and is trusted).
    let prefer_hub = session.kind == "claude" && control::hub_running(store);
    if (needs_hub || prefer_hub) && control::hub_running(store) {
        // A message the chat already has is answered from its delivery record without asking
        // the hub (the hub checks the record again under its lock before it pushes).
        match store.peek_delivery(&session.id, env) {
            store::Delivery::New => {}
            seen => return repeat_receipt(env, &session.id, seen),
        }
        let mut args = json!({"session": session.id, "envelope": links::encode_envelope(env)});
        if let Some(socket) = reply_socket {
            args["reply_socket"] = json!(socket);
        }
        let result = |state: ReceiptState, detail: String| Receipt {
            msg_id: env.id.clone(),
            session: session.id.clone(),
            state,
            detail,
        };
        return match control::call(store, "deliver", args, HUB_VERDICT_WAIT) {
            Ok(reply) if reply["ok"].as_bool() == Some(true) => result(
                reply["status"]
                    .as_str()
                    .and_then(ReceiptState::parse)
                    .unwrap_or(ReceiptState::Unknown),
                reply["detail"].as_str().unwrap_or("").to_string(),
            ),
            Ok(reply) => result(
                ReceiptState::Unknown,
                reply["error"]
                    .as_str()
                    .unwrap_or("The app refused")
                    .to_string(),
            ),
            // The hub took the request and is still pushing it (an idle Desktop chat being
            // woken, a slow chat): the hub delivers it once and, if that fails or it stops
            // first, keeps it in the chat's inbox (`control::settle_unkept`), so the message is
            // queued for the chat, not lost and not yet posted.
            Err(e) if control::is_accepted(&e) => result(ReceiptState::Queued, e),
            Err(e) => result(ReceiptState::Unknown, e),
        };
    }
    if needs_hub {
        return deliver_once(store, &session.id, env, || {
            match store.append_inbox(&session.id, env) {
                Ok(_) => (
                    Receipt {
                        msg_id: env.id.clone(),
                        session: session.id.clone(),
                        state: ReceiptState::Held,
                        detail: "The app isn't running here; Claude only takes messages posted by it. Kept in the chat inbox."
                            .to_string(),
                    },
                    true,
                ),
                Err(e) => (
                    Receipt {
                        msg_id: env.id.clone(),
                        session: session.id.clone(),
                        state: ReceiptState::Unknown,
                        detail: format!("could not store the message: {e}"),
                    },
                    false,
                ),
            }
        });
    }
    deliver_local_via(store, session, env, reply_socket)
}

/// Whether `text` names a chat exactly: `<kind>:<id>` or a bare 36-character uuid.
fn exact_local_id(text: &str) -> Option<&str> {
    let text = text.trim();
    if let Some((kind, id)) = text.split_once(':')
        && (matches!(kind, "claude" | "codex") || codex_hook::valid_hook_harness(kind))
        && !id.is_empty()
    {
        return Some(id);
    }
    let uuid_shaped = text.len() == 36
        && text.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    uuid_shaped.then_some(text)
}

/// The chat `to` names, with its local session when it is on this computer.
/// Exact ids are resolved without asking any link; `<device>:<chat>` and
/// `"<chat> on <device>"` ask only that device's link; everything else (fuzzy
/// titles) falls back to full discovery.
fn resolve_target(
    store: &Store,
    me: &Identity,
    to: &str,
) -> Result<(Peer, Option<LocalSession>), ChatError> {
    let local_peer = |s: &LocalSession| {
        roster::merge(std::slice::from_ref(s), &me.alias, &[])
            .into_iter()
            .next()
    };
    if let Some(id) = exact_local_id(to) {
        let mut found = roster::local_sessions(store)
            .into_iter()
            .find(|s| s.id.eq_ignore_ascii_case(id));
        if found.is_none() {
            found = match roster::resolve_exact_codex(id) {
                Ok(session) => session,
                Err(DiscoveryError::NoSource) => None,
                Err(e) => {
                    return Err(ChatError::Delivery(format!(
                        "couldn't look up the Codex chat \"{id}\": {e}"
                    )));
                }
            };
        }
        if found.is_none() {
            found = roster::resolve_exact_hook(store, id);
        }
        if let Some(session) = found {
            // A Claude Desktop chat whose process runs on another computer has a local row that
            // is not live; when a linked computer lists that session live, the message goes
            // there, never to the dead local row. Decided from the kept listings, which
            // ask no computer; the full listing then finds the live row.
            if session.kind == "claude"
                && session.liveness != "live"
                && links::live_sessions_elsewhere(store).contains(session.id.as_str())
                && let Some(peer) = all_peers(store, me)
                    .into_iter()
                    .find(|p| !p.local && p.session == session.id)
            {
                return Ok((peer, None));
            }
            if let Some(peer) = local_peer(&session) {
                return Ok((peer, Some(session)));
            }
        }
    }
    // (device as typed, text before it, text after it): the device may be typed as a link's name
    // or as the name that computer reports for itself, and is restated as the link's name.
    let mut devices: Vec<(String, String, String)> = to
        .match_indices(':')
        .map(|(i, _)| (to[..i].to_string(), String::new(), to[i..].to_string()))
        .collect();
    if let Some((head, device)) = to.rsplit_once(" on ") {
        devices.push((
            device.trim().to_string(),
            format!("{head} on "),
            String::new(),
        ));
    }
    // The link a name pointed at, and the target restated in that link's name. Once a name
    // pointed at a link, nothing on another computer may answer for it.
    let mut named: Option<(String, String)> = None;
    for (device, before, after) in devices {
        if device.eq_ignore_ascii_case(&me.alias) {
            continue;
        }
        let Some(link) = links::find(store, &device) else {
            continue;
        };
        let query = format!("{before}{}{after}", link.device);
        named = Some((link.device.clone(), query.clone()));
        let listing = links::list_chats_or_twin(store, &link);
        if let Err(e) = &listing
            && to.contains(':')
            && !to.contains(" on ")
        {
            return Err(ChatError::Delivery(e.clone()));
        }
        let remote = [links::RemoteChats {
            link,
            listing,
            cached: None,
        }];
        let peers = roster::merge(&[], &me.alias, &remote);
        match roster::resolve(&peers, &query) {
            Ok(peer) => return Ok((peer, None)),
            // Two chats answer to the name there: say so, never fall back to a wider search.
            Err(e @ ChatError::Invalid(_)) => return Err(e),
            Err(_) => {}
        }
    }
    let peers = all_peers(store, me);
    let query = named.as_ref().map_or(to, |(_, query)| query.as_str());
    let peer = roster::resolve(&peers, query)?;
    // The peer must be on the computer the name pointed at, under whichever of its names the
    // listing used (the device link's name, its ssh twin's, or the name it reports, #500).
    if let Some((link, _)) = &named
        && !peer.device.eq_ignore_ascii_case(link)
        && !links::find(store, &peer.device).is_some_and(|l| l.device.eq_ignore_ascii_case(link))
    {
        return Err(ChatError::NotFound(to.to_string()));
    }
    Ok((peer, None))
}

/// The chat `to` names (as `send` resolves it), without its local session.
pub fn resolve_peer(store: &Store, me: &Identity, to: &str) -> Result<Peer, ChatError> {
    resolve_target(store, me, to).map(|(peer, _)| peer)
}

/// The address a reply to a message goes to. A sender on this computer is addressed by
/// its chat id. A sender elsewhere named itself (its own hostname), which can differ from
/// the name this computer's link to it carries, so the link is found by that name (any
/// letter case) and otherwise by asking every link which one reports the sender's chat.
/// Exactly one link must report it; the sender's identity stays unverified either way.
pub fn reply_address(
    store: &Store,
    me: &Identity,
    from_device: &str,
    from_session: &str,
) -> Result<String, String> {
    if from_device == me.alias {
        return Ok(from_session.to_string());
    }
    if let Some(link) = links::find(store, from_device) {
        return Ok(format!("{}:{}", link.device, from_session));
    }
    let mut matches: Vec<String> = Vec::new();
    let mut unreachable: Vec<String> = Vec::new();
    for remote in links::list_all(store) {
        match &remote.listing {
            Ok(listing) if listing.chats.iter().any(|c| c.session == from_session) => {
                matches.push(remote.link.device.clone());
            }
            Ok(_) => {}
            Err(_) => unreachable.push(remote.link.device.clone()),
        }
    }
    match matches.as_slice() {
        [device] => Ok(format!("{device}:{from_session}")),
        [] => {
            let mut text = brand::branded(&format!(
                "no link is named \"{from_device}\" and no linked computer lists chat {from_session}; \
                 check `@cli@ chat peers` and `@cli@ chat link`"
            ));
            if !unreachable.is_empty() {
                text.push_str(&format!(" (not reachable: {})", unreachable.join(", ")));
            }
            Err(text)
        }
        many => Err(format!(
            "more than one link lists chat {from_session} (the sender calls itself \
             \"{from_device}\"): {}; send to \"<chat> on <link>\" instead",
            many.join(", ")
        )),
    }
}

/// The most chats one broadcast may reach.
pub const FANOUT_LIMIT: usize = 50;

/// Which chats a send reaches: one named chat, or a group selected at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// One chat, named the way `send` takes a target.
    Chat(String),
    /// Every chat not known to be stale, on every linked computer and here.
    All,
    /// Every chat not known to be stale on the computer with this name (a link's name, or this
    /// computer's).
    Device(String),
    /// Every chat not known to be stale of one kind: "claude" or "codex".
    Kind(String),
}

impl Pick {
    fn label(&self) -> String {
        match self {
            Pick::Chat(to) => to.clone(),
            Pick::All => "all chats".to_string(),
            Pick::Device(device) => format!("chats on {device}"),
            Pick::Kind(kind) => format!("{kind} chats"),
        }
    }
}

/// A peer that is only a name, for a receipt with no chat behind it.
fn placeholder(label: &str, alias: &str) -> Peer {
    Peer {
        id: label.to_string(),
        session: label.to_string(),
        name: label.to_string(),
        display: label.to_string(),
        device: alias.to_string(),
        device_alias: alias.to_string(),
        local: true,
        kind: String::new(),
        cwd: String::new(),
        status: String::new(),
        updated_ms: None,
        liveness: "unknown".to_string(),
        last_seen_ms: None,
        wake: String::new(),
        account: String::new(),
    }
}

/// The receipt for a send that never reached its chat (the target did not resolve, or the
/// delivery failed before a receipt came back).
pub fn failed(label: &str, alias: &str, error: &ChatError) -> SendOutcome {
    let status = match error {
        ChatError::NotFound(_) | ChatError::Invalid(_) => ReceiptState::Refused,
        _ => ReceiptState::Unknown,
    };
    SendOutcome {
        msg_id: String::new(),
        to: placeholder(label, alias),
        status: status.as_str().to_string(),
        detail: error.to_string(),
    }
}

/// Whether a chat is in a group selection (never a named chat, never a stale one).
fn in_group(pick: &Pick, peer: &Peer) -> bool {
    if peer.liveness == "stale" {
        return false;
    }
    match pick {
        Pick::All => true,
        Pick::Device(device) => {
            peer.device.eq_ignore_ascii_case(device)
                || peer.device_alias.eq_ignore_ascii_case(device)
        }
        Pick::Kind(kind) => peer.kind == *kind,
        Pick::Chat(_) => false,
    }
}

/// Send `text` from `from` to the chat `to` names. A chat on this computer
/// gets it at once; a chat on a linked computer gets it over ssh, and this
/// waits for that computer's receipt. Refused while Chat is off here.
pub fn send_text(
    store: &Store,
    me: &Identity,
    from: &Caller,
    to: &str,
    text: &str,
) -> Result<SendOutcome, ChatError> {
    if let Some(off) = chat_off(store, "", "") {
        return Ok(SendOutcome {
            msg_id: String::new(),
            to: placeholder(to, &me.alias),
            status: off.state.as_str().to_string(),
            detail: off.detail,
        });
    }
    let (peer, known) = resolve_target(store, me, to)?;
    send_to_peer(store, me, from, peer, known, text, None)
}

/// Answer the chat `session` on the computer `device`, named exactly by the id a message came
/// from: no title search, no listing. A chat on this computer is addressed as `send_text` does.
/// On a linked computer (found by the link's name or the name it reports, `links::find`) the
/// envelope is delivered by session id even when that computer's chat listing lacks the chat
/// (an idle Desktop chat, #428); the far side resolves it and refuses what it does not have.
pub fn send_reply(
    store: &Store,
    me: &Identity,
    from: &Caller,
    device: &str,
    session: &str,
    text: &str,
) -> Result<SendOutcome, ChatError> {
    let remote = if device.eq_ignore_ascii_case(&me.alias) {
        None
    } else {
        links::find(store, device)
    };
    let Some(link) = remote else {
        let to = if device.eq_ignore_ascii_case(&me.alias) {
            session.to_string()
        } else {
            format!("{device}:{session}")
        };
        return send_text(store, me, from, &to, text);
    };
    if let Some(off) = chat_off(store, "", "") {
        return Ok(SendOutcome {
            msg_id: String::new(),
            to: placeholder(session, &me.alias),
            status: off.state.as_str().to_string(),
            detail: off.detail,
        });
    }
    send_to_peer(
        store,
        me,
        from,
        reply_peer(&link, session),
        None,
        text,
        None,
    )
}

/// The chat `session` on the linked computer `link`, addressed by its id alone (a reply).
fn reply_peer(link: &links::Link, session: &str) -> Peer {
    Peer {
        id: format!("{}:{session}", link.device),
        session: session.to_string(),
        display: format!("{session} on {}", link.device),
        name: session.to_string(),
        device: link.device.clone(),
        device_alias: link.device.clone(),
        local: false,
        kind: String::new(),
        cwd: String::new(),
        status: String::new(),
        updated_ms: None,
        liveness: "unknown".to_string(),
        last_seen_ms: None,
        wake: String::new(),
        account: String::new(),
    }
}

/// Most files one `--file` send carries (the receiver refuses more: `share_mesh::AGENT_FILES_MAX`).
pub const ATTACH_FILES_MAX: usize = 20;
/// Largest `--file` send in bytes (2 GiB; the receiver refuses more).
pub const ATTACH_BYTES_MAX: u64 = 2 * 1024 * 1024 * 1024;

/// Which chat a `--file` send goes to: the chat a name or id resolves to (`send`), or the exact
/// chat a reply answers (`reply`, from its stored route).
pub enum FileTarget<'a> {
    Named(&'a str),
    Reply { device: &'a str, session: &'a str },
}

/// What the caller's Send hand-off is asked to do: send `paths` to the paired device
/// `device_id`, tagged for the chat `chat` there with `reference`, and return once it arrived.
pub struct FileSendAsk<'a> {
    pub device_id: &'a str,
    pub chat: &'a str,
    pub reference: &'a str,
    pub paths: &'a [String],
}

/// Why the files did not go: `uncertain` when they may have arrived (the receipt is `unknown`).
pub struct FileSendFailure {
    pub uncertain: bool,
    pub message: String,
}

/// Send files and then `text` to a chat on another of the owner's computers. The files go
/// first, through the host app Send over the device link (`transfer` hands them to this computer's
/// engine and waits for them to arrive), tagged with the receiving chat's id from the chat route
/// and a fresh reference; the message then carries that reference, and the receiving the host app puts
/// where it saved them in the message's header. A chat on this computer, a computer linked only
/// over ssh, or files that did not arrive are refused and no message is sent.
pub fn send_with_files(
    store: &Store,
    me: &Identity,
    from: &Caller,
    target: FileTarget<'_>,
    text: &str,
    files: &[std::path::PathBuf],
    transfer: &dyn Fn(&FileSendAsk<'_>) -> Result<(), FileSendFailure>,
) -> Result<SendOutcome, ChatError> {
    let label = match &target {
        FileTarget::Named(to) => to.to_string(),
        FileTarget::Reply { device, session } => format!("{device}:{session}"),
    };
    if let Some(off) = chat_off(store, "", "") {
        return Ok(SendOutcome {
            msg_id: String::new(),
            to: placeholder(&label, &me.alias),
            status: off.state.as_str().to_string(),
            detail: off.detail,
        });
    }
    let peer = match target {
        FileTarget::Named(to) => resolve_target(store, me, to)?.0,
        FileTarget::Reply { device, session } => {
            if device.eq_ignore_ascii_case(&me.alias) {
                resolve_target(store, me, session)?.0
            } else {
                let link = links::find(store, device)
                    .ok_or_else(|| ChatError::NotFound(format!("no link named {device}")))?;
                reply_peer(&link, session)
            }
        }
    };
    if peer.local {
        return Err(ChatError::Invalid(
            "--file goes to a chat on another computer; that chat is on this computer, so put \
            the file's path in the message"
                .to_string(),
        ));
    }
    let link = links::find(store, &peer.device)
        .ok_or_else(|| ChatError::NotFound(format!("no link named {}", peer.device)))?;
    let Some(device_id) = mesh::mesh_id(&link).map(str::to_string) else {
        return Err(ChatError::Invalid(format!(
            "--file needs the device link to {}; that computer is linked over ssh only (pair it \
            in the app's device settings)",
            peer.device
        )));
    };
    let (paths, bytes) = attachment_paths(files)?;
    let reference = rightkit_logs::identity::random_uuid()
        .replace('-', "")
        .to_ascii_lowercase();
    let attachment = Attachment {
        reference,
        count: u32::try_from(paths.len()).unwrap_or(u32::MAX),
        bytes,
    };
    if !attachment.valid() {
        return Err(ChatError::Invalid(
            "could not make a reference for the files".to_string(),
        ));
    }
    let ask = FileSendAsk {
        device_id: &device_id,
        chat: &peer.session,
        reference: &attachment.reference,
        paths: &paths,
    };
    if let Err(failure) = transfer(&ask) {
        // One line, codes and counts only: never a name or a path.
        sendlog::event(
            rightkit_logs::Level::Warn,
            "chat.attach.failed",
            json!({ "files": paths.len(), "bytes": bytes, "uncertain": failure.uncertain }),
        );
        let (status, detail) = if failure.uncertain {
            (
                ReceiptState::Unknown,
                format!(
                    "the files may not have arrived ({}), so the message was not sent; check with \
                    the chat there before sending again",
                    failure.message
                ),
            )
        } else {
            (
                ReceiptState::Refused,
                format!(
                    "the files did not arrive ({}), so the message was not sent",
                    failure.message
                ),
            )
        };
        return Ok(SendOutcome {
            msg_id: String::new(),
            to: peer,
            status: status.as_str().to_string(),
            detail,
        });
    }
    sendlog::event(
        rightkit_logs::Level::Info,
        "chat.attach.sent",
        json!({ "files": paths.len(), "bytes": bytes }),
    );
    send_to_peer(store, me, from, peer, None, text, Some(attachment))
}

/// The files of a `--file` send, checked before anything is sent: each an existing regular file
/// (not a folder), given as an absolute path, no two with the same name (the receiver saves
/// them side by side), at most `ATTACH_FILES_MAX` files and `ATTACH_BYTES_MAX` bytes in all.
fn attachment_paths(files: &[std::path::PathBuf]) -> Result<(Vec<String>, u64), ChatError> {
    if files.is_empty() {
        return Err(ChatError::Invalid("--file needs a file".to_string()));
    }
    if files.len() > ATTACH_FILES_MAX {
        return Err(ChatError::Invalid(format!(
            "at most {ATTACH_FILES_MAX} files per message"
        )));
    }
    let mut paths = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut bytes: u64 = 0;
    for file in files {
        let path = std::fs::canonicalize(file)
            .map_err(|e| ChatError::Invalid(format!("can't read {}: {e}", file.display())))?;
        let meta = std::fs::metadata(&path)
            .map_err(|e| ChatError::Invalid(format!("can't read {}: {e}", file.display())))?;
        if !meta.is_file() {
            return Err(ChatError::Invalid(format!(
                "{} is not a file (folders are not sent; zip it first)",
                file.display()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !names.insert(name) {
            return Err(ChatError::Invalid(format!(
                "two files are named {}; rename one",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )));
        }
        bytes = bytes.saturating_add(meta.len());
        paths.push(path.to_string_lossy().into_owned());
    }
    if bytes > ATTACH_BYTES_MAX {
        return Err(ChatError::Invalid(brand::branded(
            "the files are over 2 GiB in all; send them with `@cli@ send` instead",
        )));
    }
    Ok((paths, bytes))
}

/// Header lines that say where this computer saved the files sent ahead of `env`, from
/// this computer's own record of that send (`<state>/agent-files/<reference>.json`, written by
/// the engine when the batch completed), never from the envelope. The record must name the
/// device the device link proved sent the message (`env.mesh`) and the chat it is for; a message
/// that did not come over the device link is not matched. Paths are JSON strings, so a file name
/// cannot end the header. Empty when the message names no files.
pub fn attachment_note(env: &Envelope) -> String {
    let Some(attachment) = env.attachment.as_ref().filter(|a| a.valid()) else {
        return String::new();
    };
    let failed = |why: &str| {
        format!(
            "\n[{}: the sender sent {} file(s), {} bytes, ahead of this message, but {why}]",
            brand::product(),
            attachment.count,
            attachment.bytes
        )
    };
    if env.mesh.is_empty() || env.mesh != env.from.device_id {
        return failed(
            "the message did not come over the device link, so the app could not match it to a \
            received send; ask the sender",
        );
    }
    let Some(root) = rightkit_app_host::state_root() else {
        return failed("The app could not read its state folder");
    };
    let record: Option<serde_json::Value> = std::fs::read(
        root.join("agent-files")
            .join(format!("{}.json", attachment.reference)),
    )
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let Some(record) = record else {
        return failed(
            "no completed send with that reference is on this computer (it was refused or did not finish)",
        );
    };
    if record["deviceId"].as_str() != Some(env.mesh.as_str())
        || record["chat"].as_str() != Some(env.to.session.as_str())
    {
        return failed(
            "the send with that reference came from another computer or for another chat",
        );
    }
    let files: Vec<(String, u64)> = record["files"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|f| {
                    Some((
                        f["path"].as_str()?.to_string(),
                        f["size"].as_u64().unwrap_or(0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = brand::branded(
        "\n[@product@: files the sender sent ahead of this message, saved on this computer by @product@:",
    );
    for (path, size) in &files {
        let there = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        out.push_str(&format!(
            "\n{} ({size} bytes){}",
            serde_json::to_string(path).unwrap_or_default(),
            if there { "" } else { " (no longer there)" }
        ));
    }
    if files.len() != attachment.count as usize {
        out.push_str(&format!(
            "\n({} of the {} files the sender named arrived)",
            files.len(),
            attachment.count
        ));
    }
    out.push(']');
    out
}

/// Send `text` from `from` to chats the picks name: one envelope, one message id and one
/// receipt per chat, in order. A named chat that does not resolve gets a `refused` receipt
/// and the rest still go. Group picks reach every chat not known to be stale, never the
/// sending chat itself, and each chat is reached once. More than `FANOUT_LIMIT` chats is
/// refused before anything is sent. Refused while Chat is off here.
pub fn broadcast(
    store: &Store,
    me: &Identity,
    from: &Caller,
    picks: &[Pick],
    text: &str,
) -> Result<Vec<SendOutcome>, ChatError> {
    if let Some(off) = chat_off(store, "", "") {
        return Ok(picks
            .iter()
            .map(|pick| SendOutcome {
                msg_id: String::new(),
                to: placeholder(&pick.label(), &me.alias),
                status: off.state.as_str().to_string(),
                detail: off.detail.clone(),
            })
            .collect());
    }
    let mut outcomes: Vec<SendOutcome> = Vec::new();
    let mut targets: Vec<(Peer, Option<LocalSession>)> = Vec::new();
    let sessions = roster::local_sessions(store);
    let mut members: Option<Vec<Peer>> = None;
    for pick in picks {
        match pick {
            Pick::Chat(to) => match resolve_target(store, me, to) {
                Ok((peer, known)) => targets.push((peer, known)),
                Err(e) => outcomes.push(failed(to, &me.alias, &e)),
            },
            group => {
                let all = members.get_or_insert_with(|| all_peers(store, me));
                for peer in all.iter() {
                    if peer.local && peer.session == from.id {
                        continue;
                    }
                    if in_group(group, peer) {
                        let known = if peer.local {
                            sessions.iter().find(|s| s.id == peer.session).cloned()
                        } else {
                            None
                        };
                        targets.push((peer.clone(), known));
                    }
                }
            }
        }
    }
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut plan: Vec<(Peer, Option<LocalSession>)> = Vec::new();
    for (peer, known) in targets {
        let key = (peer.device.to_lowercase(), peer.session.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        plan.push((peer, known));
    }
    let total = plan.len() + outcomes.len();
    if total == 0 {
        return Err(ChatError::NotFound(brand::branded(
            "no chat matches those targets; see `@cli@ chat peers`",
        )));
    }
    if total > FANOUT_LIMIT {
        return Err(ChatError::Invalid(format!(
            "those targets are {total} chats; one send reaches at most {FANOUT_LIMIT}, so narrow them"
        )));
    }
    for (peer, known) in plan {
        let label = peer.display.clone();
        match send_to_peer(store, me, from, peer, known, text, None) {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => outcomes.push(failed(&label, &me.alias, &e)),
        }
    }
    Ok(outcomes)
}

/// Send `text` to a chat already resolved to `peer` (with its local session when it is on this
/// computer): one envelope, one receipt.
fn send_to_peer(
    store: &Store,
    me: &Identity,
    from: &Caller,
    peer: Peer,
    known: Option<LocalSession>,
    text: &str,
    attachment: Option<Attachment>,
) -> Result<SendOutcome, ChatError> {
    if peer.local && peer.session == from.id {
        return Err(ChatError::Invalid(
            "that is this chat; pick another one".to_string(),
        ));
    }
    receipts::note_sender(store, &from.id);
    let env = Envelope::new(
        Sender {
            device: me.alias.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, me.alias),
            harness: harness_of_chat(store, &from.id),
            device_id: local_device_id(store),
        },
        Target {
            device: peer.device.clone(),
            session: peer.session.clone(),
        },
        text,
    )?;
    let mut env = env;
    env.attachment = attachment;
    // The user's latest turns in the sending chat, read here from its transcript, only
    // for a chat proven from the calling process.
    let mut env = if from.detected {
        env.stamped(said::stamp_for(
            store,
            me,
            &harness_of_chat(store, &from.id),
            &from.id,
        ))
    } else {
        env
    };
    env.verified = from.detected;
    if peer.local {
        let session = match known {
            Some(session) => session,
            None => roster::local_sessions(store)
                .into_iter()
                .find(|s| s.id == peer.session)
                .ok_or_else(|| ChatError::NotFound(peer.display.clone()))?,
        };
        let receipt = deliver_here(store, &session, &env, from.reply_socket.as_deref());
        if receipt.state != ReceiptState::Refused {
            store.note_sent(receipt.state.as_str());
        }
        return Ok(SendOutcome {
            msg_id: env.id,
            to: peer,
            status: receipt.state.as_str().to_string(),
            detail: receipt.detail,
        });
    }
    let link = links::find(store, &peer.device)
        .ok_or_else(|| ChatError::NotFound(format!("no link named {}", peer.device)))?;
    let env = seal(store, &link, env);
    let started = std::time::Instant::now();
    let (posted, trace) = links::post_traced(&link, &env);
    // One `chat.send` line in the hub log for every send to another computer, whatever came back
    //. No message text, title or address.
    let state = match &posted {
        Ok(receipt) => ReceiptState::parse(receipt["status"].as_str().unwrap_or(""))
            .unwrap_or(ReceiptState::Unknown)
            .as_str()
            .to_string(),
        Err(_) => "error".to_string(),
    };
    sendlog::record(&sendlog::SendRecord {
        device: link.device.clone(),
        transport: trace.transport.to_string(),
        fallback: trace.fallback,
        state,
        error_code: trace.mesh_error.clone().unwrap_or_default(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
    let receipt = posted.map_err(ChatError::Delivery)?;
    let status = ReceiptState::parse(receipt["status"].as_str().unwrap_or(""));
    if status != Some(ReceiptState::Refused) {
        store.note_sent(status.unwrap_or(ReceiptState::Unknown).as_str());
    }
    Ok(SendOutcome {
        msg_id: env.id,
        to: peer,
        status: status.unwrap_or(ReceiptState::Unknown).as_str().to_string(),
        detail: receipt["detail"].as_str().unwrap_or("").to_string(),
    })
}

/// Envelopes older than this are refused (a replayed or very late post).
const ENVELOPE_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// A linked computer posted an envelope here (`<cli> chat post`): put it
/// into the chat it names. Refused while Chat is off, when the envelope
/// is older than 24 hours, when it names this computer as its sender (a post only comes from
/// another computer, so that is a local process posting in this computer's name), and when
/// its id was already received (the same content again is a duplicate, other content under
/// the same id a conflict). Anything else is delivered; its sender is shown as verified only
/// when its link's key signed it (`origin_of`). A duplicate the chat's delivery record has (a
/// post retried after its receipt came too late) is answered with the receipt it got the first
/// time, so the sender gets a definite answer and the chat gets it once.
pub fn receive(store: &Store, env: &Envelope) -> Receipt {
    let started = std::time::Instant::now();
    let mut repeat = false;
    let receipt = receive_inner(store, env, &mut repeat);
    let took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // The sender stops waiting 15 s after its first attempt and, after a redial, 45 s in all, and
    // is then told `unknown`. A delivery that ran that long also sends the sender a
    // `delivered` update, so it sees the message arrived and does not send it again. A repeat
    // took the first attempt's receipt unless it ran as long as the redial wait itself.
    let late = !repeat || took_ms >= u64::try_from(REPEAT_WAIT.as_millis()).unwrap_or(u64::MAX);
    if late {
        receipts::late_delivered(store, &env.to.session, env, receipt.state, took_ms);
    }
    // One line per inbound chat post on the receiving computer: the outcome and
    // how long it took, so a message that arrived late can be matched to the sender's `unknown`.
    // Fixed words and numbers only: no text, title, session id or sender name.
    sendlog::event(
        if matches!(receipt.state, ReceiptState::Unknown) {
            rightkit_logs::Level::Warn
        } else {
            rightkit_logs::Level::Info
        },
        "chat.receive",
        json!({
            "state": receipt.state.as_str(),
            "repeat": repeat,
            "durationMs": took_ms,
            "lateReceipt": late
                && took_ms >= receipts::LATE_AFTER_MS
                && matches!(
                    receipt.state,
                    ReceiptState::Delivered | ReceiptState::Queued | ReceiptState::Held
                ),
            "pid": std::process::id(),
        }),
    );
    receipt
}

/// How long a repeat of an id waits in all, over both of its waits (`settled_delivery`), for the
/// first delivery of that id, still running, to finish. Inside the 30 s the sender's redial waits
/// for an answer (C-D3).
const REPEAT_WAIT: Duration = Duration::from_secs(25);
const REPEAT_POLL: Duration = Duration::from_millis(250);

/// The delivery record's word on `env` for chat `session`, after waiting (until `until`, which
/// `receive_inner` derives once from `REPEAT_WAIT` for both waits) for a delivery of the same id
/// that is still running. The sender's redial of a slow first attempt (C-D3) lands here: it gets
/// the first attempt's real receipt, not "being delivered".
fn settled_delivery(
    store: &Store,
    session: &str,
    env: &Envelope,
    until: std::time::Instant,
) -> store::Delivery {
    loop {
        let seen = store.peek_delivery(session, env);
        if !matches!(seen, store::Delivery::InProgress { .. }) || std::time::Instant::now() >= until
        {
            return seen;
        }
        std::thread::sleep(REPEAT_POLL);
    }
}

fn receive_inner(store: &Store, env: &Envelope, repeat: &mut bool) -> Receipt {
    let refuse = |detail: &str| Receipt {
        msg_id: env.id.clone(),
        session: env.to.session.clone(),
        ..Receipt::refused(detail)
    };
    if let Some(off) = chat_off(store, &env.id, &env.to.session) {
        return off;
    }
    if envelope::now_ms().saturating_sub(env.ts) > ENVELOPE_MAX_AGE_MS {
        return refuse("expired");
    }
    if claims_this_computer(store, env) {
        return refuse(&brand::branded(
            "a post names this computer as its sender; posts come only from linked computers \
             (use `@cli@ chat send` here)",
        ));
    }
    let sessions = roster::local_sessions(store);
    let Some(session) = sessions.iter().find(|s| s.id == env.to.session) else {
        return refuse("That chat isn't open on this computer any more.");
    };
    let repeat_until = std::time::Instant::now() + REPEAT_WAIT;
    match store.journal_seen(store::origin_key(env), &env.id, &store::payload_hash(env)) {
        store::Seen::New => {}
        store::Seen::Duplicate => {
            *repeat = true;
            match settled_delivery(store, &session.id, env, repeat_until) {
                // The id is in the journal but the chat has no record of it: the first attempt
                // was never kept (it was refused or failed before the chat or its inbox had it),
                // or is queued in the hub and has not begun. Refusing it as a "duplicate" told the
                // sender nothing was wrong while nothing was delivered. `deliver_once` is the
                // at-most-once gate (it marks the id in progress under one lock), so delivering
                // here cannot reach the chat twice.
                store::Delivery::New => {}
                seen => return repeat_receipt(env, &session.id, seen),
            }
        }
        store::Seen::Conflict => return refuse("message id reused with different content"),
        // The message could not be recorded, so it was not accepted: sending the same id again is safe.
        // It is `unknown` (exit 3, one resend allowed), not `refused` (exit 2, "do not retry").
        store::Seen::Unavailable => {
            return Receipt::unknown(
                "The app could not record the message on this computer, so it was not delivered; send it again.",
            );
        }
    }
    let mut receipt = deliver_here(store, session, env, None);
    // A repeat that raced the first delivery (it had not marked the id in progress when this one
    // looked) is told "being delivered right now"; wait for the first to finish and give its receipt.
    // It waits only when the first delivery is still running (an `Unknown` for any other reason
    // is final: waiting would only spend the sender's redial wait), and only for what is left of
    // the one `REPEAT_WAIT` budget the first wait started.
    let mut taken_from_first = false;
    if *repeat && receipt.state == ReceiptState::Unknown {
        let mut seen = store.peek_delivery(&session.id, env);
        if matches!(seen, store::Delivery::InProgress { .. }) {
            seen = settled_delivery(store, &session.id, env, repeat_until);
        }
        if matches!(seen, store::Delivery::Done { .. }) {
            receipt = repeat_receipt(env, &session.id, seen);
            taken_from_first = true;
        }
    }
    // The first delivery counts itself; a repeat counts only a delivery it made itself (the first
    // attempt was never kept), never an answer it took from the first.
    let counted = !taken_from_first && !(*repeat && receipt.state == ReceiptState::Unknown);
    if receipt.state != ReceiptState::Refused && counted {
        store.note_received(receipt.state.as_str());
    }
    receipt
}
