//! Send and receive rules over a transport that is not LocalSend's HTTPS: a device mesh, a relay,
//! any channel that can carry small items and file streams between peers it has already
//! authenticated (rightkit#407).
//!
//! The LocalSend route (`Service`) owns its sockets. This module owns what is the same on every
//! route and was re-implemented per host: whom a request is admitted from (the per-device trust
//! store, keyed by [`TrustSubject`]), which file names are saved at all, how a batch of files is
//! announced and approved as a whole, and how a send is cut into small items and file streams.
//! A host implements [`Transport`] (about a screenful) and gets [`Sender`] and [`Receiver`].
//!
//! Wire: every small item is one JSON object tagged by `t` (`text`, `manifest`, `batch`; see
//! [`Small`]). A text of at most [`TEXT_MAX`] bytes is one small item. Files go as the manifest
//! (every file's name and size, chunked under the small-item cap), then the announce (`batch`),
//! then one stream per file named `b<16 hex>/<relative path>`. The receiver approves the whole
//! manifest once (an Allow device from a proven transport is not asked) and then accepts a stream
//! only for a name in it, each name once, at no more than its declared size. A name is refused,
//! never rewritten: nothing here sanitizes unless the host opts into announce-only batches
//! (`Receiver::announce_only`) for a peer that sends no manifest.
//!
//! Identity is the transport's business. A paired mesh device is proven by its pairing, so the
//! host passes `proven = true`; a peer the transport cannot vouch for is passed as `false` and is
//! asked about even when set to Allow, and held to the unproven limits of [`Policy`].

use super::error::Error;
use super::lock;
use super::policy::Policy;
use super::proto::{random_hex, sanitize_relative};
use super::send::{self, Outcome, Phase, Progress, SendItem};
use super::trust::{Admission, TrustState, TrustStore, TrustSubject};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Largest text sent as one small item; a mesh's small path is 64 KB including its envelope.
pub const TEXT_MAX: usize = 48 * 1024;
/// Largest small item a receiver reads.
pub const SMALL_MAX: usize = 64 * 1024;
/// Most files one announce may carry.
pub const FILES_MAX: usize = 20_000;
/// Longest relative name, in bytes, and longest part of one.
pub const NAME_MAX: usize = 1024;
pub const NAME_PART_MAX: usize = 255;
/// Deepest relative path.
pub const NAME_DEPTH_MAX: usize = 32;
/// One manifest item carries at most this many bytes of names and this many entries.
pub const MANIFEST_PART_BYTES: usize = 40 * 1024;
pub const MANIFEST_PART_ENTRIES: usize = 1000;
/// A manifest waits this long for its announce, and a peer may have this many waiting.
const MANIFEST_LIFE: Duration = Duration::from_secs(10 * 60);
const MANIFESTS_PER_PEER: usize = 4;
/// An accepted announce whose first file has not begun in this long is dropped.
const START_WAIT: Duration = Duration::from_secs(90);
/// An accepted announce that began but heard nothing for this long is dropped.
const STALL_WAIT: Duration = Duration::from_secs(120);
/// Text ids remembered so a resent text is acknowledged, not shown twice.
const SEEN_TEXTS: usize = 64;
/// Names shown on a card.
const SHOWN: usize = 8;

// ---- what a host provides ----------------------------------------------------------

/// A peer the transport can reach now.
#[derive(Clone, Debug)]
pub struct TransportPeer {
    /// The proven identity of the peer: the key its trust record lives under.
    pub subject: TrustSubject,
    /// The name to show.
    pub name: String,
    /// `mac`, `windows`, `linux`, `ios`, `android`, or whatever the transport reports.
    pub platform: String,
}

/// One file stream: `name` is `b<16 hex>/<relative path>`.
#[derive(Clone, Debug)]
pub struct FileOffer {
    pub name: String,
    pub size: u64,
    pub mime: Option<String>,
}

/// How a small item travelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The peer was online and has the item.
    Live,
    /// The peer is unreachable; the sealed item waits in a mailbox. Never produced for an item
    /// sent live-only, which is every item [`Sender`] sends.
    Mailbox,
}

/// What the receiver said about a file it was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDelivery {
    /// Stored.
    Saved,
    /// This exact attempt was already committed there; nothing was stored again.
    AlreadyCommitted,
    /// Stored, but the receiver could not write its receipt.
    SavedNoReceipt,
}

/// Why one small item did not go, typed. The words are the host's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SmallFailure {
    /// No connection could be opened to the peer.
    Unreachable { detail: String },
    /// The peer's owner or trust setting refused it, or it found the item malformed.
    Declined,
    /// Nobody answered the peer's card in time, or receiving is switched off there.
    Held,
    /// The peer cannot read this kind of item.
    Unsupported,
    /// The connection opened but no verdict came back.
    NoAnswer,
    /// Refused before any connection (too large, not paired, link not running).
    Local { detail: String },
}

impl SmallFailure {
    /// A fixed word for a log.
    pub fn code(&self) -> &'static str {
        match self {
            SmallFailure::Unreachable { .. } => "unreachable",
            SmallFailure::Declined => "declined",
            SmallFailure::Held => "held",
            SmallFailure::Unsupported => "unsupported",
            SmallFailure::NoAnswer => "no_answer",
            SmallFailure::Local { .. } => "local",
        }
    }
}

/// Why one file did not go, typed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileFailure {
    /// A definite no: the receiver refused or could not save it, or it could not be read here.
    Refused { detail: String },
    /// The connection dropped without the receiver's verdict: what happened to the file is not known.
    Lost,
    /// The caller's cancel flag ended it.
    Cancelled,
}

impl FileFailure {
    pub fn code(&self) -> &'static str {
        match self {
            FileFailure::Refused { .. } => "refused",
            FileFailure::Lost => "lost",
            FileFailure::Cancelled => "cancelled",
        }
    }
}

/// Which small item a [`SmallFailure`] belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmallStage {
    Text,
    Manifest,
    Announce,
}

impl SmallStage {
    pub fn as_str(self) -> &'static str {
        match self {
            SmallStage::Text => "text",
            SmallStage::Manifest => "manifest",
            SmallStage::Announce => "announce",
        }
    }
}

/// A channel between peers it has authenticated. Blocking; called from threads the host owns.
pub trait Transport: Send + Sync + 'static {
    /// Peers reachable or paired now.
    fn peers(&self) -> Vec<TransportPeer>;

    /// One small item (at most [`SMALL_MAX`] bytes). `live_only`: fail rather than leave it in a
    /// mailbox.
    fn send_small(
        &self,
        peer: &TrustSubject,
        bytes: &[u8],
        live_only: bool,
    ) -> Result<Delivery, SmallFailure>;

    /// One file: read `data` to its end (exactly `offer.size` bytes), call `progress(bytes_done)`
    /// as it goes, stop when `cancel` is set. Blocks until the receiver confirmed it stored the
    /// bytes.
    fn send_file(
        &self,
        peer: &TrustSubject,
        offer: &FileOffer,
        data: &mut (dyn Read + Send),
        progress: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<FileDelivery, FileFailure>;

    /// How the live connection runs (`nearby`, `direct`, `relay`), when known.
    fn path_of(&self, _peer: &TrustSubject) -> Option<&'static str> {
        None
    }
}

// ---- wire ---------------------------------------------------------------------------

/// One file of an announced batch, as the sender names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub name: String,
    pub size: u64,
}

/// A small item.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Small {
    Text {
        id: String,
        text: String,
        #[serde(default)]
        clipboard: bool,
    },
    Batch {
        id: String,
        count: usize,
        bytes: u64,
        names: Vec<String>,
        #[serde(default)]
        clipboard: bool,
        /// How many `Manifest` items came first (0: none).
        #[serde(default)]
        parts: u32,
        /// A host's own tag on the batch, carried untouched.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<serde_json::Value>,
    },
    /// One chunk of a batch's manifest, sent before its announce.
    Manifest {
        id: String,
        part: u32,
        files: Vec<ManifestEntry>,
    },
}

impl Small {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|e| Error::Internal {
            detail: e.to_string(),
        })
    }

    pub fn decode(bytes: &[u8]) -> Option<Small> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Splits `b<16 hex>/<rest>`.
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    let (batch, rest) = name.split_once('/')?;
    (batch.len() == 17
        && batch.starts_with('b')
        && batch[1..].bytes().all(|b| b.is_ascii_hexdigit()))
    .then_some((batch, rest))
}

// ---- names ---------------------------------------------------------------------------

/// Why a relative name is not saved as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameProblem {
    EmptyOrTooLong,
    Absolute,
    Climbs,
    PartTooLong,
    ControlOrDirection,
    UnsavableCharacter,
    TrailingDotOrSpace,
    ReservedDeviceName,
    TooDeep,
}

impl NameProblem {
    pub fn as_str(self) -> &'static str {
        match self {
            NameProblem::EmptyOrTooLong => "empty_or_too_long",
            NameProblem::Absolute => "absolute",
            NameProblem::Climbs => "climbs",
            NameProblem::PartTooLong => "part_too_long",
            NameProblem::ControlOrDirection => "control_or_direction",
            NameProblem::UnsavableCharacter => "unsavable_character",
            NameProblem::TrailingDotOrSpace => "trailing_dot_or_space",
            NameProblem::ReservedDeviceName => "reserved_device_name",
            NameProblem::TooDeep => "too_deep",
        }
    }
}

fn is_bidi_or_separator(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
    )
}

/// A relative name this computer will save as it is, as its parts, or why not. Refuses what
/// `proto::sanitize_relative` would rewrite: a `..` or `.` part, an absolute or UNC path, a
/// backslash, a control or bidi character, and on Windows a reserved device name, a trailing dot
/// or space, or one of `<>:"|?*` (which also covers alternate data streams).
pub fn strict_relative(name: &str) -> Result<Vec<String>, NameProblem> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(NameProblem::EmptyOrTooLong);
    }
    if name.starts_with('/') || name.contains('\\') {
        return Err(NameProblem::Absolute);
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(NameProblem::Climbs);
        }
        if part.len() > NAME_PART_MAX {
            return Err(NameProblem::PartTooLong);
        }
        if part
            .chars()
            .any(|c| c.is_control() || is_bidi_or_separator(c))
        {
            return Err(NameProblem::ControlOrDirection);
        }
        if cfg!(windows) {
            if part
                .chars()
                .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
            {
                return Err(NameProblem::UnsavableCharacter);
            }
            if part.ends_with('.') || part.ends_with(' ') {
                return Err(NameProblem::TrailingDotOrSpace);
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            let reserved = matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0');
            if reserved {
                return Err(NameProblem::ReservedDeviceName);
            }
        }
        parts.push(part.to_string());
        if parts.len() > NAME_DEPTH_MAX {
            return Err(NameProblem::TooDeep);
        }
    }
    Ok(parts)
}

// ---- sending -------------------------------------------------------------------------

/// Sends files and text to a peer of a [`Transport`] under the per-device trust rules.
pub struct Sender {
    transport: Arc<dyn Transport>,
    trust: Arc<Mutex<TrustStore>>,
}

impl Sender {
    pub fn new(transport: Arc<dyn Transport>, trust: Arc<Mutex<TrustStore>>) -> Sender {
        Sender { transport, trust }
    }

    /// Send `items` to `peer`. Blocks until the receiver has everything, said no, or `cancel` was
    /// set; `on` hears progress (it may be called from the transport's thread). `Outcome::Declined`
    /// is the receiver's owner or trust setting saying no. A peer that is not on the transport is
    /// `Error::DeviceNotNearby`; one set to Deny is `Error::DeviceBlocked`, before anything is sent.
    pub fn send(
        &self,
        peer: &TrustSubject,
        items: &[SendItem],
        clipboard: bool,
        cancel: &AtomicBool,
        on: &(dyn Fn(Progress) + Sync),
    ) -> Result<Outcome, Error> {
        let target = self
            .transport
            .peers()
            .into_iter()
            .find(|p| &p.subject == peer)
            .ok_or(Error::DeviceNotNearby)?;
        if lock(&self.trust).state_of(peer) == TrustState::Deny {
            return Err(Error::DeviceBlocked);
        }
        let entries = send::expand(items)?;
        if entries.len() > FILES_MAX {
            return Err(Error::TooManyFiles { max: FILES_MAX });
        }
        for entry in &entries {
            if entry.text().is_none() {
                strict_relative(&entry.name).map_err(|problem| Error::NameRefused {
                    file: entry.name.clone(),
                    problem,
                })?;
            }
        }
        let total: u64 = entries.iter().map(|e| e.size).sum();
        let mut progress = Progress {
            phase: Phase::Waiting,
            done: 0,
            total,
            files_done: 0,
            files_total: entries.len(),
            current: None,
            accepted: false,
        };
        on(progress.clone());
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Cancelled);
        }

        // One text is one small item.
        if let [only] = entries.as_slice()
            && let Some(text) = only.text()
        {
            if text.len() > TEXT_MAX {
                return Err(Error::TextTooLarge { max: TEXT_MAX });
            }
            let bytes = Small::Text {
                id: random_hex(8),
                text: text.to_string(),
                clipboard,
            }
            .encode()?;
            return match self.transport.send_small(peer, &bytes, true) {
                Ok(_) => {
                    progress.accepted = true;
                    progress.phase = Phase::Delivered;
                    progress.done = total;
                    progress.files_done = 1;
                    on(progress);
                    Ok(Outcome::Done)
                }
                Err(SmallFailure::Declined) => Ok(Outcome::Declined),
                Err(failure) => Err(Error::ItemFailed {
                    peer: target.name,
                    stage: SmallStage::Text,
                    failure,
                }),
            };
        }

        let batch = format!("b{}", random_hex(8));
        let parts = manifest_parts(&entries);
        for (number, files) in parts.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Outcome::Cancelled);
            }
            let bytes = Small::Manifest {
                id: batch.clone(),
                part: number as u32,
                files: files.clone(),
            }
            .encode()?;
            if let Err(failure) = self.transport.send_small(peer, &bytes, true) {
                return Err(Error::ItemFailed {
                    peer: target.name,
                    stage: SmallStage::Manifest,
                    failure,
                });
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Cancelled);
        }
        let announce = Small::Batch {
            id: batch.clone(),
            count: entries.len(),
            bytes: total,
            names: entries.iter().take(SHOWN).map(|e| e.name.clone()).collect(),
            clipboard,
            parts: parts.len() as u32,
            agent: None,
        }
        .encode()?;
        match self.transport.send_small(peer, &announce, true) {
            Ok(_) => {}
            Err(SmallFailure::Declined) => return Ok(Outcome::Declined),
            Err(failure) => {
                return Err(Error::ItemFailed {
                    peer: target.name,
                    stage: SmallStage::Announce,
                    failure,
                });
            }
        }
        // The receiver's owner has said yes.
        progress.accepted = true;
        progress.phase = Phase::Sending;
        on(progress.clone());

        for entry in &entries {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Outcome::Cancelled);
            }
            progress.current = Some(entry.name.clone());
            on(progress.clone());
            let offer = FileOffer {
                name: format!("{batch}/{}", entry.name),
                size: entry.size,
                mime: Some(entry.mime.clone()),
            };
            let mut reader = entry.open_reader().map_err(|e| Error::FileFailed {
                peer: target.name.clone(),
                file: entry.name.clone(),
                failure: FileFailure::Refused {
                    detail: e.to_string(),
                },
            })?;
            let base = progress.done;
            let template = progress.clone();
            let report = move |moved: u64| {
                let mut update = template.clone();
                update.done = base + moved;
                on(update);
            };
            let result = self
                .transport
                .send_file(peer, &offer, &mut *reader, &report, cancel);
            match result {
                Ok(_) => {
                    progress.files_done += 1;
                    progress.done = base + entry.size;
                    on(progress.clone());
                }
                Err(_) if cancel.load(Ordering::Relaxed) => return Ok(Outcome::Cancelled),
                Err(FileFailure::Cancelled) => return Ok(Outcome::Cancelled),
                Err(failure) => {
                    return Err(Error::FileFailed {
                        peer: target.name,
                        file: entry.name.clone(),
                        failure,
                    });
                }
            }
        }
        Ok(Outcome::Done)
    }
}

/// The manifest cut into items under the small-item cap.
fn manifest_parts(entries: &[send::Entry]) -> Vec<Vec<ManifestEntry>> {
    let mut parts: Vec<Vec<ManifestEntry>> = Vec::new();
    let mut current: Vec<ManifestEntry> = Vec::new();
    let mut bytes = 0usize;
    for entry in entries {
        // Name plus the JSON around it (`{"name":"","size":}` and a size of up to 20 digits).
        let weight = entry.name.len() + 48;
        if !current.is_empty()
            && (current.len() >= MANIFEST_PART_ENTRIES || bytes + weight > MANIFEST_PART_BYTES)
        {
            parts.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += weight;
        current.push(ManifestEntry {
            name: entry.name.clone(),
            size: entry.size,
        });
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

// ---- receiving -----------------------------------------------------------------------

/// What the owner is asked about.
#[derive(Clone, Debug)]
pub struct Request {
    pub from: TrustSubject,
    pub count: usize,
    pub bytes: u64,
    /// The first few names and sizes (sizes are 0 for an announce-only batch).
    pub shown: Vec<ManifestEntry>,
    pub is_message: bool,
}

/// What [`Receiver::receive_small`] decided. The host acknowledges `Text`, `Batch`, `Stored` and
/// `Repeat`; drops and acknowledges `Dropped`; leaves `Held` queued; answers `Unsupported` as such.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A text message was accepted. `clipboard` is true only for an Allow device that asked for it.
    Text { text: String, clipboard: bool },
    /// A batch was accepted: file streams for `id` may follow ([`Receiver::accept_file`]).
    Batch {
        id: String,
        count: usize,
        bytes: u64,
        clipboard: bool,
    },
    /// One manifest chunk was kept.
    Stored,
    /// A text already delivered once; acknowledge it, show nothing.
    Repeat,
    /// Refused or malformed: acknowledge and drop. The code is fixed, for a log.
    Dropped(&'static str),
    /// Not decided (the card went unanswered): leave it queued.
    Held,
    /// Not a small item this build reads.
    Unsupported,
}

/// A file stream the receiver may take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedFile {
    pub batch: String,
    /// Where to save it, relative to the save folder, one part per folder.
    pub relative: Vec<String>,
    pub size: u64,
    /// The last file of its batch: the batch is now closed.
    pub last: bool,
}

/// Why a file stream is not taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRefusal {
    /// It names no accepted announce of this peer (or the announce expired).
    NotAnnounced,
    /// The name is not in the approved manifest, or was already used.
    NotInManifest,
    /// The stream is larger than the manifest declared.
    OverDeclaredSize,
    Name(NameProblem),
}

struct PendingManifest {
    from: TrustSubject,
    batch: String,
    parts: BTreeMap<u32, Vec<ManifestEntry>>,
    entries: usize,
    born: Instant,
}

struct Session {
    from: TrustSubject,
    remaining: usize,
    bytes_left: u64,
    born: Instant,
    started: bool,
    /// The approved manifest, name to declared size, each name usable once. `None` only for an
    /// announce-only batch.
    manifest: Option<HashMap<String, u64>>,
}

#[derive(Default)]
struct State {
    manifests: Vec<PendingManifest>,
    sessions: HashMap<String, Session>,
    seen: VecDeque<String>,
}

/// Applies the receive rules to what a transport delivers. Thread-safe; the host calls it from
/// the threads its transport receives on. `receive_small` may block while `decide` waits for the
/// owner, so never call it from an async executor thread.
pub struct Receiver {
    trust: Arc<Mutex<TrustStore>>,
    policy: Policy,
    announce_only: bool,
    state: Mutex<State>,
}

impl Receiver {
    pub fn new(trust: Arc<Mutex<TrustStore>>, policy: Policy) -> Receiver {
        Receiver {
            trust,
            policy,
            announce_only: false,
            state: Mutex::new(State::default()),
        }
    }

    /// Accept a batch that sends no manifest (a phone's announce) and save it under sanitized
    /// names. Off by default: the manifest is the boundary for names.
    pub fn announce_only(mut self, yes: bool) -> Receiver {
        self.announce_only = yes;
        self
    }

    /// Read one small item from `from`. `proven`: the transport proved the peer's key (a paired
    /// device). `decide` shows the owner a card for a request that needs one and returns their
    /// answer, or `None` when nobody answered in time. It is called only for Ask, never while a
    /// lock is held.
    pub fn receive_small(
        &self,
        from: &TrustSubject,
        proven: bool,
        plaintext: &[u8],
        decide: &dyn Fn(&Request) -> Option<bool>,
    ) -> Verdict {
        if plaintext.len() > SMALL_MAX {
            return Verdict::Dropped("small_too_large");
        }
        let Some(small) = Small::decode(plaintext) else {
            return Verdict::Unsupported;
        };
        self.prune();
        match small {
            Small::Text {
                id,
                text,
                clipboard,
            } => {
                if id.len() > 64 || text.chars().count() > self.policy.max_message_chars {
                    return Verdict::Dropped("text_malformed");
                }
                if lock(&self.state).seen.iter().any(|s| *s == id) {
                    return Verdict::Repeat;
                }
                let request = Request {
                    from: from.clone(),
                    count: 1,
                    bytes: text.len() as u64,
                    shown: Vec::new(),
                    is_message: true,
                };
                match self.admit(from, proven, &request, decide) {
                    None => Verdict::Held,
                    Some(false) => Verdict::Dropped("owner_or_trust_declined"),
                    Some(true) => {
                        {
                            let mut state = lock(&self.state);
                            state.seen.push_back(id);
                            while state.seen.len() > SEEN_TEXTS {
                                state.seen.pop_front();
                            }
                        }
                        Verdict::Text {
                            text,
                            clipboard: clipboard && self.allowed(from),
                        }
                    }
                }
            }
            Small::Manifest { id, part, files } => self.keep_manifest(from, &id, part, files),
            Small::Batch {
                id,
                count,
                bytes,
                names,
                clipboard,
                parts,
                agent: _,
            } => {
                if count == 0 || count > FILES_MAX || split_name(&format!("{id}/x")).is_none() {
                    return Verdict::Dropped("announce_malformed");
                }
                let manifest: Option<Vec<ManifestEntry>> = if parts > 0 {
                    let Some(all) = self.manifest_for(from, &id, parts, count, bytes) else {
                        return Verdict::Dropped("manifest_missing_or_mismatched");
                    };
                    let mut unique = HashSet::new();
                    let fine = all.iter().all(|f| {
                        strict_relative(&f.name).is_ok()
                            && f.size <= self.policy.max_file_bytes
                            && unique.insert(f.name.as_str())
                    });
                    if !fine {
                        self.forget_manifest(from, &id);
                        return Verdict::Dropped("manifest_names_invalid");
                    }
                    Some(all)
                } else if self.announce_only {
                    None
                } else {
                    return Verdict::Dropped("announce_without_manifest");
                };
                if !proven
                    && (count > self.policy.unproven_max_files
                        || bytes > self.policy.unproven_max_bytes)
                {
                    self.forget_manifest(from, &id);
                    return Verdict::Dropped("unproven_over_limit");
                }
                let shown: Vec<ManifestEntry> = match &manifest {
                    Some(all) => all.iter().take(SHOWN).cloned().collect(),
                    None => names
                        .iter()
                        .take(SHOWN)
                        .map(|name| ManifestEntry {
                            name: name.clone(),
                            size: 0,
                        })
                        .collect(),
                };
                let request = Request {
                    from: from.clone(),
                    count,
                    bytes,
                    shown,
                    is_message: false,
                };
                let verdict = self.admit(from, proven, &request, decide);
                if verdict.is_some() {
                    self.forget_manifest(from, &id);
                }
                match verdict {
                    None => Verdict::Held,
                    Some(false) => Verdict::Dropped("owner_or_trust_declined"),
                    Some(true) => {
                        let session = Session {
                            from: from.clone(),
                            remaining: count,
                            bytes_left: bytes,
                            born: Instant::now(),
                            started: false,
                            manifest: manifest
                                .map(|all| all.into_iter().map(|f| (f.name, f.size)).collect()),
                        };
                        lock(&self.state).sessions.insert(id.clone(), session);
                        Verdict::Batch {
                            id,
                            count,
                            bytes,
                            clipboard: clipboard && self.allowed(from),
                        }
                    }
                }
            }
        }
    }

    /// Decide whether the stream `name` of `size` bytes from `from` is taken. The name must be
    /// `b<16 hex>/<relative path>` of an accepted announce of this same peer. On success the name
    /// is used up, and the host saves the file under [`AcceptedFile::relative`].
    pub fn accept_file(
        &self,
        from: &TrustSubject,
        name: &str,
        size: u64,
    ) -> Result<AcceptedFile, FileRefusal> {
        self.prune();
        let (batch, rest) = split_name(name).ok_or(FileRefusal::NotAnnounced)?;
        let mut state = lock(&self.state);
        let session = state
            .sessions
            .get_mut(batch)
            .filter(|s| &s.from == from)
            .ok_or(FileRefusal::NotAnnounced)?;
        let relative = match &mut session.manifest {
            Some(manifest) => {
                let declared = *manifest.get(rest).ok_or(FileRefusal::NotInManifest)?;
                if size > declared {
                    return Err(FileRefusal::OverDeclaredSize);
                }
                let relative = strict_relative(rest).map_err(FileRefusal::Name)?;
                manifest.remove(rest);
                relative
            }
            None => {
                if size > session.bytes_left {
                    return Err(FileRefusal::OverDeclaredSize);
                }
                let relative = sanitize_relative(rest);
                if relative.is_empty() {
                    return Err(FileRefusal::Name(NameProblem::EmptyOrTooLong));
                }
                session.bytes_left -= size;
                relative
            }
        };
        session.started = true;
        session.born = Instant::now();
        session.remaining = session.remaining.saturating_sub(1);
        let last = session.remaining == 0;
        if last {
            state.sessions.remove(batch);
        }
        Ok(AcceptedFile {
            batch: batch.to_string(),
            relative,
            size,
            last,
        })
    }

    /// A sender stopped before its batch finished: drop the accepted announce. Returns whether
    /// the batch was this peer's and open.
    pub fn abandon(&self, from: &TrustSubject, batch: &str) -> bool {
        let mut state = lock(&self.state);
        let owned = state
            .sessions
            .get(batch)
            .is_some_and(|session| &session.from == from);
        owned && state.sessions.remove(batch).is_some()
    }

    // ---- internals

    /// Allow, Ask or Deny for a request, calling `decide` for an Ask with no lock held. A Deny
    /// that arrives while the card waits still wins.
    fn admit(
        &self,
        from: &TrustSubject,
        proven: bool,
        request: &Request,
        decide: &dyn Fn(&Request) -> Option<bool>,
    ) -> Option<bool> {
        let admission = lock(&self.trust).admission(from, proven);
        match admission {
            Admission::Deny => {
                lock(&self.trust).refuse(from);
                Some(false)
            }
            Admission::Accept => Some(true),
            Admission::Ask => {
                let answer = decide(request);
                if answer == Some(true) && lock(&self.trust).state_of(from) == TrustState::Deny {
                    return Some(false);
                }
                answer
            }
        }
    }

    fn allowed(&self, from: &TrustSubject) -> bool {
        lock(&self.trust).state_of(from) == TrustState::Allow
    }

    /// Drop manifests and accepted announces that waited too long.
    fn prune(&self) {
        let mut state = lock(&self.state);
        state.manifests.retain(|m| m.born.elapsed() < MANIFEST_LIFE);
        state
            .sessions
            .retain(|_, s| s.born.elapsed() < if s.started { STALL_WAIT } else { START_WAIT });
    }

    /// Keep one chunk of a manifest. Bounded: entries per chunk, per batch and batches per peer.
    fn keep_manifest(
        &self,
        from: &TrustSubject,
        id: &str,
        part: u32,
        files: Vec<ManifestEntry>,
    ) -> Verdict {
        if split_name(&format!("{id}/x")).is_none()
            || files.is_empty()
            || files.len() > MANIFEST_PART_ENTRIES
            || part as usize >= FILES_MAX
            || files.iter().any(|f| f.name.len() > NAME_MAX)
        {
            return Verdict::Dropped("manifest_malformed");
        }
        let mut state = lock(&self.state);
        let at = match state
            .manifests
            .iter()
            .position(|m| &m.from == from && m.batch == id)
        {
            Some(at) => at,
            None => {
                while state.manifests.iter().filter(|m| &m.from == from).count()
                    >= MANIFESTS_PER_PEER
                {
                    match state.manifests.iter().position(|m| &m.from == from) {
                        Some(oldest) => {
                            state.manifests.remove(oldest);
                        }
                        None => break,
                    }
                }
                state.manifests.push(PendingManifest {
                    from: from.clone(),
                    batch: id.to_string(),
                    parts: BTreeMap::new(),
                    entries: 0,
                    born: Instant::now(),
                });
                state.manifests.len() - 1
            }
        };
        if state.manifests[at].parts.contains_key(&part) {
            return Verdict::Stored;
        }
        if state.manifests[at].entries + files.len() > FILES_MAX {
            state.manifests.remove(at);
            return Verdict::Dropped("manifest_too_many_files");
        }
        let held = &mut state.manifests[at];
        held.entries += files.len();
        held.parts.insert(part, files);
        Verdict::Stored
    }

    /// The complete manifest of an announce, or `None` when a chunk is missing or the totals
    /// differ from what the announce says. It stays held until [`Receiver::forget_manifest`].
    fn manifest_for(
        &self,
        from: &TrustSubject,
        id: &str,
        parts: u32,
        count: usize,
        bytes: u64,
    ) -> Option<Vec<ManifestEntry>> {
        let state = lock(&self.state);
        let held = state
            .manifests
            .iter()
            .find(|m| &m.from == from && m.batch == id)?;
        if held.parts.len() != parts as usize || (0..parts).any(|n| !held.parts.contains_key(&n)) {
            return None;
        }
        let all: Vec<ManifestEntry> = held.parts.values().flatten().cloned().collect();
        let total = all
            .iter()
            .try_fold(0u64, |sum, f| sum.checked_add(f.size))?;
        (all.len() == count && total == bytes).then_some(all)
    }

    fn forget_manifest(&self, from: &TrustSubject, id: &str) {
        lock(&self.state)
            .manifests
            .retain(|m| !(&m.from == from && m.batch == id));
    }
}
