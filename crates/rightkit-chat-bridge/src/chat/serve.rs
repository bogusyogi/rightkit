//! Chat as a hub or settings UI shows it: the same operations as the `chat` command line
//! ([`super::cli`]), with the same checks and limits, returning serializable data or a string
//! error. Nothing here panics. Anything that can wait on ssh to a linked computer blocks, so the
//! caller runs it on a blocking thread.
//!
//! The host app supplies the two things it owns through [`ServeHost`]: whether Chat is still
//! starting, and what to do with the folders chats work in.

use super::links::{self, Link};
use super::roster;
use super::store::Store;
use super::{self as chat, Peer, brand::brand, identify};
use serde::Serialize;

/// What the host app decides for the serving functions.
pub trait ServeHost {
    /// `Err` with the host's retryable "starting" error while its chat registration is running;
    /// every function here returns it unchanged before doing anything.
    fn ready(&self) -> Result<(), String>;
    /// The working folders of the chats listed here, for a host that enrolls them somewhere.
    /// Default: nothing.
    fn note_folders(&self, _folders: Vec<String>) {}
}

/// The chat store and this computer's name.
fn open() -> Result<(Store, chat::Identity), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let me = chat::local_identity(&store).map_err(|e| e.to_string())?;
    Ok((store, me))
}

/// One linked computer and how its last poll went.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLink {
    device: String,
    ssh: String,
    online: bool,
    last_ok_ms: Option<u64>,
    last_error_ms: Option<u64>,
    last_error: Option<String>,
}

/// `chat status`: the switch, whether the hub runs, chats here and the links.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStatus {
    enabled: bool,
    hub_running: bool,
    device: String,
    chats_here: usize,
    links: Vec<ChatLink>,
}

pub fn chat_status(host: &dyn ServeHost) -> Result<ChatStatus, String> {
    host.ready()?;
    let (store, me) = open()?;
    let states = links::link_status(&store);
    let links = links::all(&store)
        .into_iter()
        .map(|link| {
            let state = states
                .iter()
                .find(|s| s.device.eq_ignore_ascii_case(&link.device));
            // A computer on the device link has no ssh destination to show.
            let ssh = if chat::mesh::mesh_id(&link).is_some() {
                "device link".to_string()
            } else {
                link.ssh.clone()
            };
            ChatLink {
                online: state.is_some_and(|s| s.online()),
                last_ok_ms: state.and_then(|s| s.last_ok_ms),
                last_error_ms: state.and_then(|s| s.last_error_ms),
                last_error: state.and_then(|s| s.last_error.clone()),
                device: link.device,
                ssh,
            }
        })
        .collect();
    let sessions = roster::local_sessions(&store);
    host.note_folders(sessions.iter().map(|s| s.cwd.clone()).collect());
    Ok(ChatStatus {
        enabled: store.chat_enabled(),
        hub_running: chat::control::hub_running(&store),
        device: me.alias,
        chats_here: sessions.len(),
        links,
    })
}

/// A chat that can be messaged, here or on a linked computer (`chat peers`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPeer {
    /// What `send` takes: the chat id here, `<device>:<session>` elsewhere.
    id: String,
    name: String,
    /// `"<chat title> on <device>"`.
    display: String,
    device: String,
    local: bool,
    /// "claude", "codex" or a hook harness such as "muse".
    kind: String,
    /// Claude: busy or idle. Codex: active or idle.
    state: String,
    /// "live", "stale" or "unknown".
    liveness: String,
    updated_ms: Option<u64>,
    /// The last part of the chat's working directory.
    folder: String,
}

impl From<Peer> for ChatPeer {
    fn from(peer: Peer) -> ChatPeer {
        let folder = std::path::Path::new(&peer.cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        ChatPeer {
            id: peer.id,
            name: peer.name,
            display: peer.display,
            device: peer.device,
            local: peer.local,
            kind: peer.kind,
            state: peer.status,
            liveness: peer.liveness,
            updated_ms: peer.updated_ms,
            folder,
        }
    }
}

/// Every chat that can be messaged, here and on linked computers (asked over ssh in parallel).
pub fn chat_peers(host: &dyn ServeHost) -> Result<Vec<ChatPeer>, String> {
    host.ready()?;
    let (store, me) = open()?;
    let peers = chat::all_peers(&store, &me);
    host.note_folders(
        peers
            .iter()
            .filter(|p| p.local)
            .map(|p| p.cwd.clone())
            .collect(),
    );
    Ok(peers.into_iter().map(ChatPeer::from).collect())
}

/// The receipt of one send: the message id, the chat it went to, the state and its detail.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReceipt {
    msg_id: String,
    chat: String,
    state: String,
    detail: String,
}

/// `chat send <chat> <text>`: the same checks, then the same delivery, from this process.
pub fn chat_send(
    host: &dyn ServeHost,
    target: String,
    text: String,
) -> Result<ChatReceipt, String> {
    host.ready()?;
    let target = target.trim().to_string();
    if target.is_empty() {
        return Err("choose a chat to message".to_string());
    }
    if text.trim().is_empty() {
        return Err("the message text is empty".to_string());
    }
    let (store, me) = open()?;
    let env = |name: &str| std::env::var(name).ok();
    let sessions = roster::local_sessions(&store);
    let from = identify(&sessions, &env, None).map_err(|e| e.to_string())?;
    let outcome = chat::send_text(&store, &me, &from, &target, &text).map_err(|e| e.to_string())?;
    Ok(ChatReceipt {
        msg_id: outcome.msg_id,
        chat: outcome.to.display,
        state: outcome.status,
        detail: outcome.detail,
    })
}

/// A message a chat here has not read yet (`chat inbox`: unread only, not marked read).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// The message id, which `reply` takes.
    id: String,
    /// The chat whose inbox holds it.
    chat: String,
    from: String,
    body: String,
    received_ms: u64,
}

/// The unread messages of every chat here, newest first, and how many the size cap dropped.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatInbox {
    messages: Vec<ChatMessage>,
    evicted: u64,
}

pub fn chat_inbox(host: &dyn ServeHost) -> Result<ChatInbox, String> {
    host.ready()?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let mut inbox = ChatInbox::default();
    for session in roster::local_sessions(&store) {
        let refused = store.refused_inbox(&session.id);
        let entries = store
            .take_unread_ack(&session.id, None, false)
            .map_err(|e| e.to_string())?;
        inbox.evicted += store.inbox_evicted(&session.id);
        inbox.messages.extend(
            entries
                .into_iter()
                .filter(|entry| !refused.contains(&entry.env.id))
                .map(|entry| ChatMessage {
                    id: entry.env.id,
                    chat: session.name.clone(),
                    from: entry.env.from.name,
                    body: entry.env.body,
                    received_ms: entry.received,
                }),
        );
    }
    inbox
        .messages
        .sort_by_key(|m| std::cmp::Reverse(m.received_ms));
    Ok(inbox)
}

/// What a new link reported: its name, its ssh host, the chats it lists and the name it gave.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLinked {
    device: String,
    ssh: String,
    chats: usize,
    their_name: String,
}

/// `chat link <device> <ssh-host> [--<cli> PATH]`. `links::add` checks the link over ssh before
/// saving it, so this waits on the network. `cli` defaults to the brand's command (on PATH).
pub fn chat_link(
    host: &dyn ServeHost,
    device: String,
    ssh_host: String,
    cli: Option<String>,
) -> Result<ChatLinked, String> {
    host.ready()?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let cli = cli
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(&brand().cli)
        .to_string();
    let link = Link {
        device: device.trim().to_string(),
        ssh: ssh_host.trim().to_string(),
        cli,
        ..Link::default()
    };
    let listing = links::add(&store, link.clone())?;
    Ok(ChatLinked {
        device: link.device,
        ssh: link.ssh,
        chats: listing.chats.len(),
        their_name: listing.device,
    })
}

/// `chat unlink <device>`: removes the saved link; nothing is contacted.
pub fn chat_unlink(host: &dyn ServeHost, device: String) -> Result<(), String> {
    host.ready()?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    if links::remove(&store, &device)? {
        Ok(())
    } else {
        Err(format!("no link named {device}"))
    }
}
