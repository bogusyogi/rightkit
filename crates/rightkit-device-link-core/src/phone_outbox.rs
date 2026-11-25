//! The phone outbox: items a computer's owner sent to a paired phone, waiting for the phone to
//! pick them up (#404: the phone shows received items per computer, in each computer's tab).
//!
//! A phone only dials out, so the computer cannot push. An explicit owner Send to a paired phone
//! puts the item here; the phone, when it opens this computer's tab (and on pull-to-refresh and
//! a short tick), asks `receive.list`, fetches each item with `receive.take` (files in bounded
//! chunks) and says `receive.ack` once it has stored the item; the computer then deletes it. The
//! phone keeps no history (received is a scratchpad), and an item nobody fetched is deleted after
//! its time to live.
//!
//! * Items are created only by [`enqueue`], which the host calls for an explicit owner Send.
//!   No chat message, agent or phone request ever creates one.
//! * Every item is bound to the phone's `device_id` AND its `pairing_id` (the pairing epoch). A
//!   phone that was revoked and paired again has a new `pairing_id` and never sees an old item;
//!   [`drop_device`] deletes a revoked phone's items at once.
//! * The store is in memory; file bodies wait in owner-only files under the root given to
//!   [`set_root`], which is emptied at start (a restart forgets the outbox, like the TTL would).
//! * What a phone may do here is decided by [`dispatch`], called from the host's phone method
//!   table after it has proved the paired device and checked the receive switch.
//!
//! No item text or file name is logged here.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use rightkit_owner_approvals::PhoneError;

use crate::phone_policy::Caller;

/// Longest text one item carries, in bytes (the same bound as a phone's `send.text`).
pub const TEXT_MAX: usize = 48 * 1024;
/// Largest file or image one item carries: the phone's existing 10 MB cap. Larger is refused.
pub const FILE_MAX: u64 = 10 * 1024 * 1024;
/// Raw bytes in one `receive.take` answer (base64 makes it a third larger, under the line cap).
pub const CHUNK_BYTES: usize = 128 * 1024;
/// Items waiting for one phone at most.
pub const PER_DEVICE_MAX: usize = 20;
/// Bytes waiting for one phone at most.
pub const PER_DEVICE_BYTES: u64 = 50 * 1024 * 1024;
/// Default life of an item nobody fetched.
pub const TTL_DEFAULT_MS: u64 = 60 * 60 * 1000;
/// Longest life the owner can give an item.
pub const TTL_MAX_MS: u64 = 24 * 60 * 60 * 1000;

/// What an item is, as the phone shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    File,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Image => "image",
            Kind::File => "file",
        }
    }
}

/// What the owner is sending. `Text` carries the text; `Bytes` a file or image body.
pub enum Body {
    Text(String),
    Bytes(Vec<u8>),
}

/// The metadata of a new item.
pub struct NewItem {
    /// The item's id (8 to 64 letters or digits), so the sender can follow it before it is
    /// queued; `None` makes one.
    pub id: Option<String>,
    pub device_id: String,
    /// The pairing the phone has now (the registry's `pairing_id`).
    pub pairing_id: String,
    /// This computer's name, shown on the phone (empty: the phone shows the computer's tab name).
    pub sender: String,
    /// A file name (a text item may leave it empty).
    pub name: String,
    pub mime: Option<String>,
    /// How long it waits before it is deleted; clamped to `1 minute ..= TTL_MAX_MS`.
    pub ttl_ms: u64,
}

/// How an item left the outbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The phone stored it and acknowledged.
    Acked,
    /// Nobody fetched it in time.
    Expired,
    /// The phone was revoked or re-paired.
    Dropped,
}

/// Told when an item leaves the outbox, with its id. Runs on whatever thread settled it, with no
/// outbox lock held.
pub type Observer = Arc<dyn Fn(&str, Settled) + Send + Sync>;

enum Payload {
    Text(String),
    File(PathBuf),
}

struct Item {
    id: String,
    device_id: String,
    pairing_id: String,
    kind: Kind,
    name: String,
    mime: String,
    size: u64,
    sender: String,
    created_ms: u64,
    expires_ms: u64,
    payload: Payload,
}

impl Item {
    fn remove_body(&self) {
        if let Payload::File(path) = &self.payload {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct State {
    root: Option<PathBuf>,
    items: Vec<Item>,
}

static STATE: Mutex<State> = Mutex::new(State {
    root: None,
    items: Vec::new(),
});
static OBSERVER: Mutex<Option<Observer>> = Mutex::new(None);
static APP_NAME: Mutex<&'static str> = Mutex::new("the app");

/// The app's name as the owner reads it in the "open it on the phone first" refusal.
pub fn set_app_name(name: &'static str) {
    *lock(&APP_NAME) = name;
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Where file bodies wait. Empties the folder: an item the previous run left is forgotten.
pub fn set_root(root: PathBuf) {
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let mut state = lock(&STATE);
    state.root = Some(root);
    state.items.clear();
}

/// The host hears every item that leaves the outbox (its Send receipt follows it).
pub fn set_observer(observer: Option<Observer>) {
    *lock(&OBSERVER) = observer;
}

fn settle(items: Vec<Item>, how: Settled) {
    if items.is_empty() {
        return;
    }
    let observer = lock(&OBSERVER).clone();
    for item in items {
        item.remove_body();
        if let Some(observer) = &observer {
            observer(&item.id, how);
        }
    }
}

/// Take out every item for which `gone` is true; returns them without the lock held.
fn extract(gone: impl Fn(&Item) -> bool) -> Vec<Item> {
    let mut state = lock(&STATE);
    let mut out = Vec::new();
    let mut i = 0;
    while i < state.items.len() {
        if gone(&state.items[i]) {
            out.push(state.items.remove(i));
        } else {
            i += 1;
        }
    }
    out
}

/// Delete the items past their time. The host calls it as it publishes Send's state, and
/// every phone request calls it.
pub fn sweep() {
    let now = now_ms();
    settle(extract(|item| item.expires_ms <= now), Settled::Expired);
}

/// Delete every item for `device_id` (the phone was revoked). Runs at once, not at the next request.
pub fn drop_device(device_id: &str) {
    settle(
        extract(|item| item.device_id == device_id),
        Settled::Dropped,
    );
}

/// Delete these items (a send that failed part way). Each is reported to the observer as dropped.
pub fn drop_ids(ids: &[String]) {
    settle(extract(|item| ids.contains(&item.id)), Settled::Dropped);
}

/// Items waiting for `device_id` right now.
pub fn pending_for(device_id: &str) -> usize {
    lock(&STATE)
        .items
        .iter()
        .filter(|item| item.device_id == device_id)
        .count()
}

/// A file name that cannot climb out of any folder: the last path part, no control characters,
/// no leading or trailing dots or spaces, at most 120 characters.
fn safe_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let cut: String = cleaned.chars().take(120).collect();
    if cut.is_empty() {
        "item".to_string()
    } else {
        cut
    }
}

fn new_id() -> Result<String, String> {
    crate::wire::random::<12>()
        .map(|bytes| rightkit_fs::digest::hex(&bytes))
        .map_err(|_| "Couldn't make an item id.".to_string())
}

/// Put one item for a paired phone in the outbox; returns its id. The refusals are sentences to
/// show the owner. The caller must have checked that the owner asked for this send.
pub fn enqueue(new: NewItem, body: Body) -> Result<String, String> {
    sweep();
    let now = now_ms();
    let ttl = new.ttl_ms.clamp(60_000, TTL_MAX_MS);
    let (kind, size, name, mime) = match &body {
        Body::Text(text) => {
            if text.is_empty() {
                return Err("There is nothing to send.".to_string());
            }
            if text.len() > TEXT_MAX {
                return Err(format!(
                    "That text is too long for a phone (at most {} KB).",
                    TEXT_MAX / 1024
                ));
            }
            (
                Kind::Text,
                text.len() as u64,
                String::new(),
                "text/plain".to_string(),
            )
        }
        Body::Bytes(bytes) => {
            let size = bytes.len() as u64;
            if size > FILE_MAX {
                return Err("That file is over 10 MB, too large to send to a phone.".to_string());
            }
            let mime = new
                .mime
                .clone()
                .filter(|m| !m.is_empty() && m.len() <= 100)
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let kind = if mime.starts_with("image/") {
                Kind::Image
            } else {
                Kind::File
            };
            (kind, size, safe_name(&new.name), mime)
        }
    };
    let id = match new.id.clone() {
        Some(id)
            if (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            id
        }
        Some(_) => return Err("Couldn't make an item id.".to_string()),
        None => new_id()?,
    };
    let payload = match body {
        Body::Text(text) => Payload::Text(text),
        Body::Bytes(bytes) => {
            let root = lock(&STATE)
                .root
                .clone()
                .ok_or_else(|| "The phone outbox isn't running.".to_string())?;
            let _ = std::fs::create_dir_all(&root);
            let path = root.join(format!("{id}.bin"));
            rightkit_fs::atomic_write_with_options(
                &path,
                rightkit_fs::AtomicWriteOptions {
                    privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                    ..Default::default()
                },
                |file| std::io::Write::write_all(file, &bytes),
            )
            .map_err(|e| format!("Couldn't hold the item for the phone: {e}"))?;
            Payload::File(path)
        }
    };
    let item = Item {
        id: id.clone(),
        device_id: new.device_id.clone(),
        pairing_id: new.pairing_id,
        kind,
        name,
        mime,
        size,
        sender: new.sender,
        created_ms: now,
        expires_ms: now.saturating_add(ttl),
        payload,
    };
    let mut state = lock(&STATE);
    let (count, bytes) = state
        .items
        .iter()
        .filter(|i| i.device_id == new.device_id)
        .fold((0usize, 0u64), |(c, b), i| (c + 1, b + i.size));
    if count >= PER_DEVICE_MAX || bytes.saturating_add(size) > PER_DEVICE_BYTES {
        drop(state);
        item.remove_body();
        return Err(format!(
            "The phone already has a lot waiting. Open {} on it first.",
            *lock(&APP_NAME)
        ));
    }
    state.items.push(item);
    Ok(id)
}

// ---- the phone's methods ---------------------------------------------------------------

/// `receive.list`, `receive.take` and `receive.ack`; `None` for any other method. Items bound to
/// another pairing of this phone are deleted on sight.
pub fn dispatch(
    caller: &Caller,
    method: &str,
    params: &Value,
) -> Option<Result<Value, PhoneError>> {
    match method {
        "receive.list" | "receive.take" | "receive.ack" => {}
        _ => return None,
    }
    sweep();
    // A re-paired phone has a new epoch: whatever was addressed to the old one is gone.
    settle(
        extract(|item| item.device_id == caller.device_id && item.pairing_id != caller.pairing_id),
        Settled::Dropped,
    );
    Some(match method {
        "receive.list" => Ok(list(caller)),
        "receive.take" => take(caller, params),
        _ => ack(caller, params),
    })
}

fn id_param(params: &Value) -> Result<&str, PhoneError> {
    params
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .ok_or_else(|| PhoneError::new("invalid_request", "missing param `id`"))
}

fn list(caller: &Caller) -> Value {
    let state = lock(&STATE);
    let items: Vec<Value> = state
        .items
        .iter()
        .filter(|i| i.device_id == caller.device_id && i.pairing_id == caller.pairing_id)
        .map(|i| {
            json!({
                "id": i.id, "kind": i.kind.word(), "name": i.name, "mime": i.mime,
                "size": i.size, "sender": i.sender,
                "createdMs": i.created_ms, "expiresMs": i.expires_ms,
            })
        })
        .collect();
    json!({ "items": items, "chunkBytes": CHUNK_BYTES, "maxBytes": FILE_MAX })
}

fn take(caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
    let id = id_param(params)?;
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
    // What to read is copied out; the file is read with no lock held.
    let (kind, name, mime, size, sender, payload) = {
        let state = lock(&STATE);
        let item = state
            .items
            .iter()
            .find(|i| {
                i.id == id && i.device_id == caller.device_id && i.pairing_id == caller.pairing_id
            })
            .ok_or_else(|| PhoneError::new("not_found", "that item is no longer waiting"))?;
        let payload = match &item.payload {
            Payload::Text(text) => Payload::Text(text.clone()),
            Payload::File(path) => Payload::File(path.clone()),
        };
        (
            item.kind,
            item.name.clone(),
            item.mime.clone(),
            item.size,
            item.sender.clone(),
            payload,
        )
    };
    let mut answer = json!({
        "id": id, "kind": kind.word(), "name": name, "mime": mime, "size": size,
        "sender": sender, "offset": offset,
    });
    match payload {
        Payload::Text(text) => {
            answer["text"] = json!(text);
            answer["done"] = json!(true);
        }
        Payload::File(path) => {
            if offset > size {
                return Err(PhoneError::new(
                    "invalid_request",
                    "`offset` is past the end",
                ));
            }
            let want = usize::try_from((size - offset).min(CHUNK_BYTES as u64)).unwrap_or(0);
            let mut buffer = vec![0u8; want];
            let mut file = std::fs::File::open(&path)
                .map_err(|_| PhoneError::new("not_found", "that item is no longer waiting"))?;
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(&mut buffer))
                .map_err(|e| PhoneError::retry("read_failed", e.to_string()))?;
            answer["data"] = json!(rightkit_crypto::base64_encode(&buffer));
            answer["done"] = json!(offset + want as u64 >= size);
        }
    }
    Ok(answer)
}

fn ack(caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
    let id = id_param(params)?;
    let gone = extract(|i| {
        i.id == id && i.device_id == caller.device_id && i.pairing_id == caller.pairing_id
    });
    let acked = !gone.is_empty();
    settle(gone, Settled::Acked);
    // A repeat of an ack whose answer was lost finds nothing and is still a success.
    Ok(json!({ "acked": acked }))
}
