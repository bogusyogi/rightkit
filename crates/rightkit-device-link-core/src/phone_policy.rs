//! What a paired phone may do over the device link, and the phone-to-computer upload state.
//!
//! The host decides WHICH method names exist and what they say; this module holds the policy and
//! the bookkeeping that do not depend on any one app:
//!
//! * [`PhoneError`], the refusal with a stable code;
//! * [`Mode`], [`Settings`], [`Access`], [`Change`] and the per-device [`PolicyStore`] (`read`
//!   allow or deny, `send` allow, ask or deny, and the separate `admin` switch), kept in one JSON
//!   file bound per device to the pairing it was written for;
//! * [`Asks`]: sends held for the owner, answered by an owner-gated call or timed out;
//! * [`Inbox`], [`Sink`] and [`UploadTable`]: multi-file uploads from a phone, one file at a time,
//!   in order, with retry-safe chunks, idle clean-up and slot limits;
//! * [`SendDedup`]: the same client message id from the same device returns the first answer.
//!
//! No chat text, name or message id is logged here.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Longest message text a phone may send, in bytes.
pub const SEND_TEXT_MAX: usize = 16 * 1024;
/// How long a held send waits for the owner.
pub const ASK_WAIT: Duration = Duration::from_secs(60);
const ASKS_MAX: usize = 8;
const SENT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SENT_CAP: usize = 500;

/// Longest `send.text` text, in bytes (the Send small path is 64 KB with its envelope).
pub const SEND_ITEM_TEXT_MAX: usize = 48 * 1024;
/// Raw bytes in one `send.files.chunk` (base64 makes it 4/3 larger, under the 256 KB line).
/// 160 KiB raw is 218,456 base64 characters, so a chunk request stays under MeshLink's 256 KiB request cap with room for the
/// JSON envelope (the phone sends slashes unescaped).
pub const FILES_CHUNK_BYTES: usize = 160 * 1024;
/// Saved paths returned by `send.files.end`; the rest are counted, not listed.
const SAVED_SAMPLE: usize = 20;
/// Most bytes one `send.files.begin` may announce. The computer cannot tell a relayed path from
/// a direct one on this service, so the 25 MB relay cap is the phone's to keep (it sees the path
/// state); this is the absolute ceiling for any path.
pub const FILES_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Most files in one `send.files.begin`.
pub const FILES_MAX_COUNT: u64 = 20_000;
/// The relayed per-transfer cap (`rightkit-mesh` caps.rs, D7), advertised to the phone.
pub const FILES_RELAY_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// Open uploads per phone, and across all phones.
const UPLOADS_PER_DEVICE: usize = 2;
const UPLOADS_MAX: usize = 8;
/// An upload with no request for this long is abandoned: its partial file is deleted.
const UPLOAD_IDLE: Duration = Duration::from_secs(5 * 60);
const REL_PATH_MAX: usize = 1024;
const REL_COMPONENTS_MAX: usize = 8;
const REL_COMPONENT_MAX: usize = 255;
const PREVIEW_NAMES_MAX: usize = 8;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// ---- errors ------------------------------------------------------------------------------

/// A refusal with a stable code; one type shared with rightkit-owner-approvals.
pub use rightkit_owner_approvals::PhoneError;

/// A required string parameter: trimmed, non-empty and at most 256 bytes.
pub fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, PhoneError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| PhoneError::invalid(format!("missing param `{key}`")))
}

/// `clientMsgId`: 8 to 64 of `A-Za-z0-9_-`.
pub fn client_id(params: &Value) -> Result<&str, PhoneError> {
    let client = str_param(params, "clientMsgId")?;
    if !(8..=64).contains(&client.len())
        || !client
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(PhoneError::invalid(
            "`clientMsgId` is 8 to 64 letters, digits, - or _",
        ));
    }
    Ok(client)
}

// ---- policy ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Allow,
    Ask,
    Deny,
}

impl Mode {
    pub fn parse(word: &str) -> Option<Mode> {
        match word {
            "allow" => Some(Mode::Allow),
            "ask" => Some(Mode::Ask),
            "deny" => Some(Mode::Deny),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Allow => "allow",
            Mode::Ask => "ask",
            Mode::Deny => "deny",
        }
    }
}

/// The settings the host reads for each request.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// The master switch. Default on (inert until a phone is paired).
    pub enabled: bool,
    /// The default read mode for a new phone. Allow or Deny; anything else is Deny.
    pub read: Mode,
    /// The default send mode for a new phone. Default Ask.
    pub send: Mode,
}

impl Settings {
    /// From the published setting values; a missing or unknown word is the documented default.
    pub fn from_words(enabled: Option<bool>, read: Option<&str>, send: Option<&str>) -> Self {
        let read = match read.and_then(Mode::parse) {
            None => Mode::Allow,
            Some(Mode::Allow) => Mode::Allow,
            // "ask" is not a reading mode; fail closed.
            Some(_) => Mode::Deny,
        };
        Self {
            enabled: enabled.unwrap_or(true),
            read,
            send: send.and_then(Mode::parse).unwrap_or(Mode::Ask),
        }
    }
}

/// What one phone may do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub read: Mode,
    pub send: Mode,
}

/// A change to one of a device's two modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Keep,
    /// Remove the override: the settings' default applies.
    Default,
    Set(Mode),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Override {
    /// The pairing the entry was written for; a different pairing ignores it.
    pairing_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    send: Option<Mode>,
    /// `admin.*`: only `allow` lets this phone approve privileged commands. No entry, no field, or
    /// an unreadable file is deny; there is no settings default for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admin: Option<Mode>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PolicyFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    devices: BTreeMap<String, Override>,
}

/// The per-device policy file (`phone-access.json` beside the registry in the Pulse host).
pub struct PolicyStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PolicyStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<PolicyFile, PhoneError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                PhoneError::retry(
                    "policy_unreadable",
                    "the phone access file could not be read",
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PolicyFile::default()),
            Err(_) => Err(PhoneError::retry(
                "policy_unreadable",
                "the phone access file could not be read",
            )),
        }
    }

    /// What this pairing may do: its entry when the entry belongs to this pairing, else the
    /// settings' defaults. An unreadable policy file is an error, never "allow".
    pub fn access_for(
        &self,
        device_id: &str,
        pairing_id: &str,
        settings: &Settings,
    ) -> Result<Access, PhoneError> {
        let file = self.load()?;
        let entry = file
            .devices
            .get(device_id)
            .filter(|o| o.pairing_id == pairing_id);
        Ok(Access {
            read: entry.and_then(|o| o.read).unwrap_or(settings.read),
            send: entry.and_then(|o| o.send).unwrap_or(settings.send),
        })
    }

    /// Owner call: change a paired phone's modes. Reading takes allow or deny only.
    pub fn set_access(
        &self,
        device_id: &str,
        pairing_id: &str,
        read: Change,
        send: Change,
    ) -> Result<(), PhoneError> {
        if read == Change::Set(Mode::Ask) {
            return Err(PhoneError::invalid("reading is allow or deny"));
        }
        let _guard = lock(&self.lock);
        let mut file = self.load()?;
        file.version = 1;
        let mut entry = match file.devices.remove(device_id) {
            Some(o) if o.pairing_id == pairing_id => o,
            _ => Override {
                pairing_id: pairing_id.to_string(),
                ..Override::default()
            },
        };
        let apply = |slot: &mut Option<Mode>, change: Change| match change {
            Change::Keep => {}
            Change::Default => *slot = None,
            Change::Set(mode) => *slot = Some(mode),
        };
        apply(&mut entry.read, read);
        apply(&mut entry.send, send);
        if entry.read.is_some() || entry.send.is_some() || entry.admin.is_some() {
            file.devices.insert(device_id.to_string(), entry);
        }
        self.write(&file)
    }

    /// Owner call: let one paired phone approve privileged commands (`allow`) or stop it
    /// (`false`). Never a default and never inherited: each phone is switched on by itself.
    /// Writes the policy only; the host does what else a switch-off means (revoking the phone's
    /// approval key, cancelling its approved commands).
    pub fn set_admin(
        &self,
        device_id: &str,
        pairing_id: &str,
        allow: bool,
    ) -> Result<(), PhoneError> {
        let _guard = lock(&self.lock);
        let mut file = self.load()?;
        file.version = 1;
        let mut entry = match file.devices.remove(device_id) {
            Some(o) if o.pairing_id == pairing_id => o,
            _ => Override {
                pairing_id: pairing_id.to_string(),
                ..Override::default()
            },
        };
        entry.admin = allow.then_some(Mode::Allow);
        if entry.read.is_some() || entry.send.is_some() || entry.admin.is_some() {
            file.devices.insert(device_id.to_string(), entry);
        }
        self.write(&file)
    }

    /// True only when the owner switched `admin` on for this pairing. A policy file that cannot
    /// be read is false.
    pub fn admin_allowed(&self, device_id: &str, pairing_id: &str) -> bool {
        self.load().is_ok_and(|file| {
            file.devices
                .get(device_id)
                .filter(|o| o.pairing_id == pairing_id)
                .is_some_and(|o| o.admin == Some(Mode::Allow))
        })
    }

    fn write(&self, file: &PolicyFile) -> Result<(), PhoneError> {
        let bytes = serde_json::to_vec_pretty(file)
            .map_err(|e| PhoneError::new("policy_unwritable", e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rightkit_fs::atomic_write_with_options(
            &self.path,
            rightkit_fs::AtomicWriteOptions {
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| std::io::Write::write_all(file, &bytes),
        )
        .map_err(|e| PhoneError::retry("policy_unwritable", e.to_string()))
    }
}

// ---- asks (held sends) -------------------------------------------------------------------

/// A send held for the owner, as the host describes it. The preview is already redacted.
#[derive(Clone, Debug)]
pub struct AskCard {
    pub device_id: String,
    pub device_name: String,
    pub chat: String,
    pub chat_name: String,
    pub preview: String,
    pub created_ms: u64,
}

struct Card {
    id: String,
    card: AskCard,
    decision: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    Declined,
    TimedOut,
    /// Too many sends are already waiting for the owner.
    Busy,
}

/// Sends held for the owner. The owner UI reads [`Asks::pending`] and answers with
/// [`Asks::answer`] (owner-gated calls).
#[derive(Default)]
pub struct Asks {
    cards: Mutex<Vec<Card>>,
    wake: Condvar,
    counter: AtomicU64,
}

impl Asks {
    /// Hold `card` until the owner answers or `wait` runs out.
    pub fn ask(&self, card: AskCard, wait: Duration) -> Answer {
        let id = format!(
            "ask-{}-{}",
            now_ms(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        {
            let mut cards = lock(&self.cards);
            if cards.len() >= ASKS_MAX {
                return Answer::Busy;
            }
            cards.push(Card {
                id: id.clone(),
                card,
                decision: None,
            });
        }
        let deadline = Instant::now() + wait;
        let mut cards = lock(&self.cards);
        let outcome = loop {
            match cards.iter().find(|c| c.id == id) {
                Some(card) => {
                    if let Some(allow) = card.decision {
                        break if allow {
                            Answer::Allowed
                        } else {
                            Answer::Declined
                        };
                    }
                }
                None => break Answer::Declined,
            }
            let now = Instant::now();
            if now >= deadline {
                break Answer::TimedOut;
            }
            cards = self
                .wake
                .wait_timeout(cards, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        cards.retain(|c| c.id != id);
        outcome
    }

    /// The sends waiting for the owner. Carries a text preview: serve it to the owner UI only.
    pub fn pending(&self) -> Vec<Value> {
        lock(&self.cards)
            .iter()
            .filter(|c| c.decision.is_none())
            .map(|c| {
                json!({
                    "askId": c.id,
                    "deviceId": c.card.device_id,
                    "deviceName": c.card.device_name,
                    "chat": c.card.chat,
                    "chatName": c.card.chat_name,
                    "preview": c.card.preview,
                    "createdAtMs": c.card.created_ms,
                })
            })
            .collect()
    }

    /// The owner's answer. False when no undecided ask has that id (answered, or timed out).
    pub fn answer(&self, ask_id: &str, allow: bool) -> bool {
        let found = {
            let mut cards = lock(&self.cards);
            match cards
                .iter_mut()
                .find(|c| c.id == ask_id && c.decision.is_none())
            {
                Some(card) => {
                    card.decision = Some(allow);
                    true
                }
                None => false,
            }
        };
        if found {
            self.wake.notify_all();
        }
        found
    }
}

// ---- the caller --------------------------------------------------------------------------

/// The paired device that sent a request, as the registry knows it.
#[derive(Clone, Debug)]
pub struct Caller {
    pub device_id: String,
    pub pairing_id: String,
    /// The label the phone chose for itself (display only; the host passes it through).
    pub name: String,
    /// The registry's platform word (`ios`).
    pub platform: String,
}

// ---- send de-duplication -----------------------------------------------------------------

enum SentState {
    Pending,
    Done(Value),
}

struct Sent {
    at: Instant,
    state: SentState,
}

/// Remembers sends by (device id, client id) for 24 hours, in memory (a restart forgets them): a
/// repeat returns the first answer instead of sending again.
#[derive(Default)]
pub struct SendDedup {
    sent: Mutex<HashMap<(String, String), Sent>>,
}

impl SendDedup {
    /// `Some(answer)` for an id already sent (returned again, not sent again); `None` after
    /// marking a new one in flight.
    pub fn begin(&self, key: &(String, String)) -> Result<Option<Value>, PhoneError> {
        let mut sent = lock(&self.sent);
        sent.retain(|_, entry| entry.at.elapsed() < SENT_TTL);
        if let Some(entry) = sent.get(key) {
            return match &entry.state {
                SentState::Done(value) => {
                    let mut again = value.clone();
                    if let Some(object) = again.as_object_mut() {
                        object.insert("deduplicated".to_string(), json!(true));
                    }
                    Ok(Some(again))
                }
                SentState::Pending => Err(PhoneError::retry(
                    "in_progress",
                    "this message is still being sent",
                )),
            };
        }
        if sent.len() >= SENT_CAP {
            let oldest = sent
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                sent.remove(&oldest);
            }
        }
        sent.insert(
            key.clone(),
            Sent {
                at: Instant::now(),
                state: SentState::Pending,
            },
        );
        Ok(None)
    }

    /// Record the answer of a send that finished.
    pub fn finish(&self, key: &(String, String), value: Value) {
        lock(&self.sent).insert(
            key.clone(),
            Sent {
                at: Instant::now(),
                state: SentState::Done(value),
            },
        );
    }

    /// Forget a send that failed, so a retry sends again.
    pub fn forget(&self, key: &(String, String)) {
        lock(&self.sent).remove(key);
    }
}

// ---- phone to computer: uploads ----------------------------------------------------------

/// Where a phone's items land. Implemented by the host over its own receive handling; this module
/// owns the upload bookkeeping.
pub trait Inbox: Send + Sync + 'static {
    /// Deliver one text. `id` is stable for a retry. May wait for the owner (an accept card).
    fn text(&self, from: &Caller, id: &str, text: &str) -> Result<(), PhoneError>;
    /// Announce a batch (may wait for the owner). Returns the batch id, which names the upload.
    fn begin(
        &self,
        from: &Caller,
        count: usize,
        total: u64,
        preview: &[String],
    ) -> Result<String, PhoneError>;
    /// Start one file of an accepted batch. `rel` is already validated; the inbox still
    /// sanitizes it, saves below its own folder and never overwrites.
    fn open(
        &self,
        from: &Caller,
        batch: &str,
        rel: &str,
        size: u64,
    ) -> Result<Box<dyn Sink>, PhoneError>;
    /// The phone stopped (cancel, error or idle) before the batch finished: close its transfer.
    fn abandon(&self, device_id: &str, batch: &str);
}

/// One incoming file. `finish` publishes it and returns where it was saved; `abort` deletes the
/// partial file.
pub trait Sink: Send {
    fn write(&mut self, chunk: &[u8]) -> Result<(), PhoneError>;
    fn finish(self: Box<Self>) -> Result<String, PhoneError>;
    fn abort(self: Box<Self>);
}

struct Current {
    index: u64,
    size: u64,
    written: u64,
    sink: Box<dyn Sink>,
}

struct Upload {
    device_id: String,
    batch: String,
    count: u64,
    total: u64,
    next_index: u64,
    current: Option<Current>,
    saved: Vec<String>,
    ended: bool,
    touched: Instant,
}

impl Upload {
    /// Delete the partial file and close the computer's transfer card.
    fn abort(&mut self, inbox: Option<&Arc<dyn Inbox>>) {
        if let Some(current) = self.current.take() {
            current.sink.abort();
        }
        if !self.ended
            && let Some(inbox) = inbox
        {
            inbox.abandon(&self.device_id, &self.batch);
        }
    }
}

/// Open `send.files` uploads, keyed by (device id, batch id).
type Uploads = HashMap<(String, String), Arc<Mutex<Upload>>>;

/// The upload state of every phone: the [`Inbox`], the open uploads and the begin slots.
#[derive(Default)]
pub struct UploadTable {
    inbox: Mutex<Option<Arc<dyn Inbox>>>,
    uploads: Mutex<Uploads>,
    /// Devices whose `begin` is waiting on the owner (an accept card). Each one holds an upload
    /// slot from the busy check until its batch is in `uploads` or the begin failed.
    opening: Mutex<Vec<String>>,
}

impl UploadTable {
    /// Where uploads deliver.
    pub fn set_inbox(&self, inbox: Arc<dyn Inbox>) {
        *lock(&self.inbox) = Some(inbox);
    }

    /// The inbox, when one was set.
    pub fn inbox(&self) -> Option<Arc<dyn Inbox>> {
        lock(&self.inbox).clone()
    }

    fn require_inbox(&self) -> Result<Arc<dyn Inbox>, PhoneError> {
        self.inbox()
            .ok_or_else(|| PhoneError::new("not_built", "this build cannot receive from phones"))
    }

    /// `send.files.begin`.
    pub fn begin(&self, caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
        let inbox = self.require_inbox()?;
        let count = params
            .get("count")
            .and_then(Value::as_u64)
            .filter(|n| (1..=FILES_MAX_COUNT).contains(n))
            .ok_or_else(|| {
                PhoneError::invalid(format!("`count` is 1 to {FILES_MAX_COUNT} files"))
            })?;
        let total = params
            .get("totalBytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| PhoneError::invalid("missing param `totalBytes`"))?;
        if total > FILES_MAX_BYTES {
            return Err(PhoneError::new(
                "too_large",
                format!("a transfer is at most {FILES_MAX_BYTES} bytes"),
            ));
        }
        let mut preview = Vec::new();
        if let Some(names) = params.get("names") {
            let names = names
                .as_array()
                .ok_or_else(|| PhoneError::invalid("`names` is a list"))?;
            for name in names.iter().take(PREVIEW_NAMES_MAX) {
                let name = name
                    .as_str()
                    .ok_or_else(|| PhoneError::invalid("`names` holds strings"))?;
                preview.push(valid_rel_path(name)?);
            }
        }
        // The busy check and the slot are one step: `inbox.begin` may wait for the owner (an Ask
        // card), so a second begin meanwhile must count this one, or each would open its own card.
        {
            let uploads = lock(&self.uploads);
            let mut opening = lock(&self.opening);
            let mine = uploads
                .keys()
                .filter(|(d, _)| *d == caller.device_id)
                .count()
                + opening.iter().filter(|d| **d == caller.device_id).count();
            if mine >= UPLOADS_PER_DEVICE || uploads.len() + opening.len() >= UPLOADS_MAX {
                return Err(PhoneError::retry(
                    "busy",
                    "this computer is already receiving from this phone",
                ));
            }
            opening.push(caller.device_id.clone());
        }
        // The slot is released on every way out of `inbox.begin`, a panic included, and only once
        // the uploads lock is held, so the upload (if any) replaces it in one step.
        struct Reserved<'a> {
            opening: &'a Mutex<Vec<String>>,
            device: &'a str,
        }
        impl Drop for Reserved<'_> {
            fn drop(&mut self) {
                let mut opening = lock(self.opening);
                if let Some(at) = opening.iter().position(|d| d == self.device) {
                    opening.remove(at);
                }
            }
        }
        let (begun, mut uploads) = {
            let _reserved = Reserved {
                opening: &self.opening,
                device: &caller.device_id,
            };
            let begun = inbox.begin(caller, count as usize, total, &preview);
            (begun, lock(&self.uploads))
        };
        let batch = begun?;
        let upload = Upload {
            device_id: caller.device_id.clone(),
            batch: batch.clone(),
            count,
            total,
            next_index: 0,
            current: None,
            saved: Vec::new(),
            ended: false,
            touched: Instant::now(),
        };
        uploads.insert(
            (caller.device_id.clone(), batch.clone()),
            Arc::new(Mutex::new(upload)),
        );
        drop(uploads);
        Ok(json!({
            "transferId": batch,
            "chunkBytes": FILES_CHUNK_BYTES,
            "maxBytes": FILES_MAX_BYTES,
        }))
    }

    fn find(
        &self,
        caller: &Caller,
        params: &Value,
    ) -> Result<(String, Arc<Mutex<Upload>>), PhoneError> {
        let id = str_param(params, "transferId")?.to_string();
        let found = lock(&self.uploads)
            .get(&(caller.device_id.clone(), id.clone()))
            .cloned()
            .ok_or_else(|| PhoneError::new("not_found", "no such transfer, or it expired"))?;
        Ok((id, found))
    }

    /// Drop an upload and delete its partial file.
    fn drop_upload(&self, caller: &Caller, id: &str, upload: &Arc<Mutex<Upload>>) {
        lock(upload).abort(lock(&self.inbox).as_ref());
        lock(&self.uploads).remove(&(caller.device_id.clone(), id.to_string()));
    }

    /// `send.files.chunk`.
    pub fn chunk(&self, caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
        let (id, upload) = self.find(caller, params)?;
        let inbox = self.require_inbox()?;
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| PhoneError::invalid("missing param `index`"))?;
        let offset = params
            .get("offset")
            .and_then(Value::as_u64)
            .ok_or_else(|| PhoneError::invalid("missing param `offset`"))?;
        let data = params
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| PhoneError::invalid("missing param `data`"))?;
        if data.len() > FILES_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(PhoneError::new("too_large", "a chunk is too large"));
        }
        let bytes = rightkit_crypto::base64_decode(data)
            .map_err(|_| PhoneError::invalid("`data` is not valid base64"))?;
        if bytes.len() > FILES_CHUNK_BYTES {
            return Err(PhoneError::new("too_large", "a chunk is too large"));
        }

        let mut up = lock(&upload);
        up.touched = Instant::now();
        if up.ended {
            return Err(PhoneError::invalid("the transfer has ended"));
        }
        // A retry of the chunk that finished a file whose answer was lost.
        if index < up.next_index {
            return Ok(
                json!({ "written": offset.saturating_add(bytes.len() as u64), "fileDone": true, "duplicate": true }),
            );
        }
        if up.current.is_none() {
            if index != up.next_index || index >= up.count {
                return Err(PhoneError::invalid("files go in order, one at a time"));
            }
            let rel = valid_rel_path(
                params
                    .get("relPath")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PhoneError::invalid("a file's first chunk names `relPath`"))?,
            )?;
            let size = params
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| PhoneError::invalid("a file's first chunk gives `size`"))?;
            if offset != 0 {
                return Err(PhoneError::invalid("a file starts at offset 0"));
            }
            if size > up.total {
                drop(up);
                self.drop_upload(caller, &id, &upload);
                return Err(PhoneError::invalid(
                    "a file is larger than the announced total",
                ));
            }
            let batch = up.batch.clone();
            match inbox.open(caller, &batch, &rel, size) {
                Ok(sink) => {
                    up.current = Some(Current {
                        index,
                        size,
                        written: 0,
                        sink,
                    });
                }
                Err(e) => {
                    drop(up);
                    self.drop_upload(caller, &id, &upload);
                    return Err(e);
                }
            }
        }
        let outcome = {
            let current = up.current.as_mut().expect("a file is open");
            if current.index != index {
                return Err(PhoneError::invalid("finish the current file first"));
            }
            let len = bytes.len() as u64;
            if offset < current.written {
                // A retry of a chunk already stored (its answer was lost).
                if offset + len <= current.written {
                    return Ok(
                        json!({ "written": current.written, "fileDone": false, "duplicate": true }),
                    );
                }
                return Err(PhoneError::new(
                    "bad_offset",
                    format!("expected offset {}", current.written),
                ));
            }
            if offset != current.written {
                return Err(PhoneError::new(
                    "bad_offset",
                    format!("expected offset {}", current.written),
                ));
            }
            if current.written + len > current.size {
                Err(PhoneError::invalid("more bytes than the file's `size`"))
            } else {
                current.sink.write(&bytes).map(|()| {
                    current.written += len;
                    current.written
                })
            }
        };
        match outcome {
            Err(e) => {
                drop(up);
                self.drop_upload(caller, &id, &upload);
                Err(e)
            }
            Ok(written) => {
                let done = up.current.as_ref().is_some_and(|c| written == c.size);
                if done {
                    let current = up.current.take().expect("a file is open");
                    match current.sink.finish() {
                        Ok(path) => {
                            up.saved.push(path);
                            up.next_index += 1;
                        }
                        Err(e) => {
                            drop(up);
                            self.drop_upload(caller, &id, &upload);
                            return Err(e);
                        }
                    }
                }
                Ok(json!({ "written": written, "fileDone": done }))
            }
        }
    }

    /// `send.files.end`.
    pub fn end(&self, caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
        let (id, upload) = self.find(caller, params)?;
        let mut up = lock(&upload);
        up.touched = Instant::now();
        if !up.ended {
            if up.current.is_some() || up.next_index != up.count {
                drop(up);
                self.drop_upload(caller, &id, &upload);
                return Err(PhoneError::new(
                    "incomplete",
                    "the transfer ended before every file arrived; files already saved stay, the unfinished one was deleted",
                ));
            }
            up.ended = true;
        }
        // The answer must fit MeshLink's 256 KiB response cap whatever the file count: the count, the shared folder and at
        // most SAVED_SAMPLE paths (the phone shows one result line, not a list).
        let folder = common_parent(&up.saved);
        let sample: Vec<&String> = up.saved.iter().take(SAVED_SAMPLE).collect();
        Ok(json!({ "saved": sample, "count": up.saved.len(), "folder": folder }))
    }

    /// `send.files.cancel`.
    pub fn cancel(&self, caller: &Caller, params: &Value) -> Result<Value, PhoneError> {
        if let Ok((id, upload)) = self.find(caller, params) {
            self.drop_upload(caller, &id, &upload);
        }
        Ok(json!({ "cancelled": true }))
    }

    /// Abandon uploads nobody has touched for `UPLOAD_IDLE`: partial files are deleted and the
    /// transfer cards closed. Runs on each phone request and from the host's device-link loop,
    /// so an upload whose phone vanished is cleared without another request.
    pub fn sweep(&self) {
        let stale: Vec<Arc<Mutex<Upload>>> = {
            let mut uploads = lock(&self.uploads);
            let keys: Vec<_> = uploads
                .iter()
                .filter(|(_, u)| {
                    // A busy upload (its lock held by a request) is not idle.
                    u.try_lock()
                        .is_ok_and(|u| u.touched.elapsed() > UPLOAD_IDLE)
                })
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| uploads.remove(k)).collect()
        };
        let inbox = lock(&self.inbox).clone();
        for upload in stale {
            lock(&upload).abort(inbox.as_ref());
        }
    }
}

/// A relative path from a phone, refused (not repaired) when it could leave the save folder or
/// is not a plain `a/b/c.ext`: no absolute path, drive letter, `..`, `.`, empty part, NUL or other
/// control character, backslash, over-long part or too many parts. Returns it `/`-joined. The
/// inbox then sanitizes each part again for the host OS (reserved Windows names and characters).
pub fn valid_rel_path(raw: &str) -> Result<String, PhoneError> {
    let bad = |why: &str| PhoneError::invalid(format!("a file path {why}"));
    if raw.is_empty() || raw.len() > REL_PATH_MAX {
        return Err(bad("is empty or too long"));
    }
    if raw.chars().any(|c| c.is_control() || c == '\\') {
        return Err(bad("holds a control character or a backslash"));
    }
    if raw.starts_with('/') {
        return Err(bad("must be relative"));
    }
    let parts: Vec<&str> = raw.split('/').collect();
    if parts.len() > REL_COMPONENTS_MAX {
        return Err(bad("is nested too deeply"));
    }
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(bad("holds an empty, `.` or `..` part"));
        }
        if part.len() > REL_COMPONENT_MAX {
            return Err(bad("has a part that is too long"));
        }
        if part.trim().is_empty() || part.ends_with(' ') || part.ends_with('.') {
            return Err(bad("has a part that is blank or ends with a space or dot"));
        }
    }
    let first = parts[0].as_bytes();
    if first.len() >= 2 && first[0].is_ascii_alphabetic() && first[1] == b':' {
        return Err(bad("must not start with a drive letter"));
    }
    Ok(raw.to_string())
}

/// The deepest folder every saved path shares (by path components), or None for no paths.
fn common_parent(paths: &[String]) -> Option<String> {
    let mut parts: Option<Vec<std::path::Component<'_>>> = None;
    for path in paths {
        let parent: Vec<_> = std::path::Path::new(path).parent()?.components().collect();
        parts = Some(match parts {
            None => parent,
            Some(prev) => prev
                .iter()
                .zip(parent.iter())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| *a)
                .collect(),
        });
    }
    let parts = parts?;
    (!parts.is_empty()).then(|| {
        parts
            .iter()
            .collect::<std::path::PathBuf>()
            .display()
            .to_string()
    })
}
