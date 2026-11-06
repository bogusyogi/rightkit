//! The clipboard history's store (the Windows half of the clipboard spec): the data
//! model of section 2, the folder on disk, deduplication (3.4), retention (4) and the
//! pinboards (6). Reading the clipboard is the capture code; drawing the strip is
//! the strip code. Both reach the one store of this process through `with`.
//!
//! Kept in the `clipboard` folder under the host's data directory, in the clipboard spec's format,
//! so a folder copied from the Mac opens here and the other way round: `index.json` (item metadata only), `boards.json`,
//! `items/<id>.txt|png|html|rtf|files.json` and `thumbs/<id>.png`. Every file is written by
//! writing a temporary file and renaming it over the target; a payload is written before the
//! index row that names it, so a crash leaves at worst an orphan, which the next load deletes.
//! Only this process writes the folder: `.lock` is held open without sharing for as long as
//! the store lives.
//!
//! Nothing here is ever logged or published: item text, previews and paths stay in the store.
//! The notch's state file carries counts only (`counts`).

use crate::detect::{self as clip_detect, Detected};
use crate::json::{self, Value};
use crate::seal as clip_seal;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The index's largest size on disk, and the most items it may list (the cap setting's top).
const INDEX_BYTES: usize = 64 * 1024 * 1024;
const INDEX_ITEMS: usize = 20_000;
/// The preview's length in characters (spec 2.2).
pub const PREVIEW_CHARS: usize = 2000;
/// Largest plain text kept, and the largest HTML or RTF kept beside it (spec 3.2).
pub const TEXT_MAX: usize = 1024 * 1024;
pub const RICH_MAX: usize = 1024 * 1024;
/// Largest image kept, in bytes, and in pixels along a side.
pub const IMAGE_MAX: usize = 20 * 1024 * 1024;
pub const IMAGE_SIDE_MAX: u32 = 8192;
/// Most paths kept for one file item.
pub const PATHS_MAX: usize = 200;
/// Screenshots that follow each other within this many seconds share a stack.
const STACK_SECONDS: f64 = 60.0;
/// The screenshot lane's own size cap.
const SCREENSHOT_LANE_BYTES: u64 = 100 * 1024 * 1024;
/// An identical copy within this long of the last is one write that an app made twice.
const REPEAT_GUARD: Duration = Duration::from_millis(300);
/// The most boards, and the most items on one.
pub const BOARDS_MAX: usize = 20;
const BOARD_ITEMS_MAX: usize = 1000;
/// Deleted items whose files are small enough are kept this long for an undo (in memory).
pub const UNDO_SECONDS: f64 = 6.0;
const UNDO_BYTES: u64 = 5 * 1024 * 1024;

/// The palette of the pinboards (spec 6): name and `#RRGGBB`, the Mac's accent colours.
pub const PALETTE: [(&str, &str); 8] = [
    ("Blue", "#5E9BFF"),
    ("Green", "#34C759"),
    ("Orange", "#FF9F0A"),
    ("Red", "#FF453A"),
    ("Purple", "#BF5AF2"),
    ("Pink", "#FF6482"),
    ("Teal", "#40C8E0"),
    ("Grey", "#8E8E93"),
];

// ---- the model ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Link,
    Colour,
    Code,
    File,
    Image,
    Rich,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Link => "link",
            Kind::Colour => "colour",
            Kind::Code => "code",
            Kind::File => "file",
            Kind::Image => "image",
            Kind::Rich => "rich",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "text" => Some(Kind::Text),
            "link" => Some(Kind::Link),
            "colour" => Some(Kind::Colour),
            "code" => Some(Kind::Code),
            "file" => Some(Kind::File),
            "image" => Some(Kind::Image),
            "rich" => Some(Kind::Rich),
            _ => None,
        }
    }
}

/// The app a copy came from: the lower-case exe name and the name shown.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceApp {
    pub id: String,
    pub name: String,
}

/// One clipboard copy (spec 2.1). Keys this build does not know are kept in `extra` and
/// written back.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub preview: String,
    pub title: Option<String>,
    pub source: Option<SourceApp>,
    pub created: f64,
    pub used: f64,
    pub uses: u64,
    pub pinned: bool,
    pub boards: Vec<String>,
    pub size: u64,
    pub digest: String,
    /// Lane `screenshot` (otherwise `main`).
    pub screenshot: bool,
    pub meta: Vec<(String, Value)>,
    pub concealed: bool,
    extra: Vec<(String, Value)>,
}

impl Item {
    pub fn meta_value(&self, key: &str) -> Option<&Value> {
        self.meta.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.meta_value(key).and_then(Value::as_str)
    }

    pub fn meta_num(&self, key: &str) -> Option<f64> {
        self.meta_value(key).and_then(Value::as_f64)
    }

    /// A file item's first names (`meta.names`).
    pub fn meta_names(&self) -> Vec<String> {
        self.meta_value("names")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pinned items and items on a board are never evicted (spec 4).
    pub fn protected(&self) -> bool {
        self.pinned || !self.boards.is_empty()
    }

    fn set_meta(&mut self, key: &str, value: Value) {
        match self.meta.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.meta.push((key.to_string(), value)),
        }
    }

    /// The stack a screenshot belongs to (`meta.stackId`).
    pub fn stack_id(&self) -> Option<&str> {
        self.meta_str("stackId")
    }
}

/// A named, coloured, ordered list of items (spec 6).
#[derive(Clone, Debug)]
pub struct Board {
    pub id: String,
    pub name: String,
    pub colour: String,
    pub order: i64,
    pub item_order: Vec<String>,
    pub created: f64,
    extra: Vec<(String, Value)>,
}

/// What the store can be in. Anything but `Ready` keeps the index untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    /// The index is from a newer version: it is not written and nothing is captured.
    Newer,
    /// The index could not be read: it is left as it is.
    Unreadable,
    /// Another process holds the folder's lock.
    Locked,
}

/// A copy of what the views draw from, taken under the store's lock.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<Item>,
    pub boards: Vec<Board>,
    pub status: Status,
    /// The pinned and boarded items alone outgrew the size cap: their size in MB.
    pub pinned_mb: Option<u64>,
    /// The board the strip was last left on (`clipboardRememberBoard`).
    pub last_board: Option<String>,
}

/// The size and count caps in force (spec 4).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_items: usize,
    pub max_bytes: u64,
    /// Zero keeps main-lane items for ever.
    pub max_age_days: u32,
    /// Zero keeps screenshots until another cap.
    pub screenshot_hours: u32,
}

impl Limits {
    pub fn current() -> Limits {
        let set = crate::host::retention();
        Limits {
            max_items: set.max_items as usize,
            max_bytes: u64::from(set.max_mb) * 1024 * 1024,
            max_age_days: set.max_age_days,
            screenshot_hours: set.screenshot_hours,
        }
    }
}

/// A copy to be stored (what the capture code read and sorted by the spec's priority).
pub struct Capture {
    pub kind: Kind,
    /// The plain text; empty for an image or a file list.
    pub text: String,
    pub html: Option<Vec<u8>>,
    pub rtf: Option<Vec<u8>>,
    /// The PNG, its width and its height.
    pub png: Option<(Vec<u8>, u32, u32)>,
    /// The thumbnail PNG of an image.
    pub thumb: Option<Vec<u8>>,
    pub paths: Vec<String>,
    pub source: Option<SourceApp>,
    /// The copy is a screenshot by its source app or its size (spec 4).
    pub screenshot: bool,
}

/// What `add` did with a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    New,
    /// The same digest was there: it moved to the top and kept its id.
    Repeated,
    Skipped,
}

/// A deleted item, held for an undo: in memory, or for one over 5 MB as files moved into
/// `clipboard/.undo/` (spec 9).
pub struct Undo {
    item: Item,
    files: Vec<(String, Vec<u8>)>,
    /// The files kept on disk: the name they take back (as in `files`) and where they wait.
    disk: Vec<(String, PathBuf)>,
    slots: Vec<(String, usize)>,
    pub at: f64,
}

/// What a paste writes: the item's formats.
pub struct Payload {
    pub text: String,
    pub html: Option<Vec<u8>>,
    pub rtf: Option<Vec<u8>>,
    /// The PNG's file, for an image.
    pub png: Option<PathBuf>,
    pub paths: Vec<String>,
}

// ---- the store ---------------------------------------------------------------------------

/// What a clear removes (docs/clipboard-spec.md 9): everything, the items copied in the last hour,
/// or the items copied more than a day ago. The windows keep pinned and board items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearScope {
    All,
    LastHour,
    OlderThanDay,
}

impl ClearScope {
    /// The launcher row's title (the same words as the Mac's).
    pub fn title(self) -> &'static str {
        match self {
            ClearScope::All => crate::host::t("Clear clipboard history"),
            ClearScope::LastHour => crate::host::t("Clear clipboard from the last hour"),
            ClearScope::OlderThanDay => crate::host::t("Clear clipboard older than a day"),
        }
    }

    /// Every scope, in the launcher's order.
    pub const ALL: [ClearScope; 3] = [
        ClearScope::All,
        ClearScope::LastHour,
        ClearScope::OlderThanDay,
    ];
}

pub struct Store {
    dir: PathBuf,
    items: Vec<Item>,
    boards: Vec<Board>,
    top_extra: Vec<(String, Value)>,
    boards_extra: Vec<(String, Value)>,
    status: Status,
    /// The thumbnails' sizes, which count toward the size cap.
    thumbs: HashMap<String, u64>,
    last_capture: Option<(String, Instant)>,
    last_board: Option<String>,
    /// Held for the store's life: a second process cannot take it.
    _lock: Option<rightkit_fs::FileLock>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// Runs `f` on the store, opening it first when it is not open yet. Hold the lock only for a
/// moment: the strip's drawing and the capture's reading of the clipboard happen outside it.
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut guard = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Store::open))
}

/// A copy of the items and boards for drawing.
pub fn snapshot() -> Snapshot {
    with(|store| store.snapshot())
}

/// How many items the history holds and how many bytes they take, for the hub. The store is
/// not opened for this: with history off, or before the first use, it reads zero.
pub fn counts() -> (usize, u64) {
    let guard = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    guard.as_ref().map_or((0, 0), |store| {
        (
            store.items.len(),
            store.items.iter().map(|item| store.weight(item)).sum(),
        )
    })
}

/// The folder: `clipboard` under the host's data directory.
pub fn root() -> Option<PathBuf> {
    crate::host::data_dir().map(|dir| dir.join("clipboard"))
}

/// A new file name in the folder's `.tmp`, for the picture conversions (`clip_img.rs`). The
/// folder is emptied when the store opens.
pub fn tmp_file(extension: &str) -> Option<PathBuf> {
    let dir = root()?.join(".tmp");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{}.{extension}", uuid4())))
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Ids name files, so only lower-case hex and dashes are accepted.
fn id_ok(id: &str) -> bool {
    (8..=40).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// A random lower-case UUID version 4.
pub fn uuid4() -> String {
    let mut bytes = random_bytes::<16>();
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(windows)]
#[allow(non_snake_case)]
#[link(name = "advapi32")]
unsafe extern "system" {
    /// `RtlGenRandom`: fills the buffer with random bytes; non-zero on success.
    fn SystemFunction036(buffer: *mut u8, length: u32) -> u8;
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    #[cfg(windows)]
    // SAFETY: the buffer is `N` writable bytes that live through the call.
    let filled = unsafe { SystemFunction036(bytes.as_mut_ptr(), N as u32) } != 0;
    #[cfg(not(windows))]
    let filled = {
        use std::io::Read;
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .is_ok()
    };
    if !filled {
        // No system randomness: mix the clock and a counter through SHA-256 instead.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let seed = format!("{:?}-{n}", SystemTime::now());
        let hex = rightkit_fs::digest::sha256_bytes(seed.as_bytes());
        let digest = rightkit_fs::digest::unhex(&hex).unwrap_or_default();
        for (slot, byte) in bytes.iter_mut().zip(digest) {
            *slot = byte;
        }
    }
    bytes
}

/// The first `PREVIEW_CHARS` characters of `text`, with CR LF turned into LF.
fn preview_of(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .take(PREVIEW_CHARS)
        .collect()
}

/// Moves `from` over `to` (undo and restore): a rename, with no flush for what is only a copy
/// held for a few seconds.
fn move_file(from: &Path, to: &Path) -> bool {
    rightkit_fs::publish_file_with(
        from,
        to,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            ..rightkit_fs::AtomicWriteOptions::default()
        },
    )
    .is_ok()
}

impl Store {
    /// Opens the folder: takes the lock, reads the boards and the index, deletes orphans and
    /// runs a retention pass. An unreadable or newer index leaves the store read-only.
    fn open() -> Store {
        let mut store = Store {
            dir: root().unwrap_or_default(),
            items: Vec::new(),
            boards: Vec::new(),
            top_extra: Vec::new(),
            boards_extra: Vec::new(),
            status: Status::Ready,
            thumbs: HashMap::new(),
            last_capture: None,
            last_board: None,
            _lock: None,
        };
        if store.dir.as_os_str().is_empty()
            || fs::create_dir_all(store.dir.join("items")).is_err()
            || fs::create_dir_all(store.dir.join("thumbs")).is_err()
        {
            store.status = Status::Unreadable;
            return store;
        }
        let _ = fs::remove_dir_all(store.dir.join(".tmp"));
        // Undo copies of big items do not outlive the run (spec 9).
        let _ = fs::remove_dir_all(store.dir.join(".undo"));
        // Not shared: a second process that opens the same folder fails here and only reads.
        store._lock = rightkit_fs::FileLock::acquire(
            &store.dir.join(".lock"),
            &rightkit_fs::LockOptions::default(),
        )
        .ok();
        if store._lock.is_none() {
            store.status = Status::Locked;
        }
        // Sealed files are opened before anything is read (spec 9, #152). A process that does not
        // hold the lock changes nothing in the folder.
        let owner = store._lock.is_some();
        if clip_seal::prepare(&store.dir, owner) == clip_seal::Prepared::Unavailable && owner {
            store.status = Status::Unreadable;
        }
        store.read_boards();
        store.read_index();
        if store.status == Status::Ready {
            store.repair();
            store.sweep_orphans();
            store.retention(now());
            store.save_index();
            store.save_boards();
        }
        store
    }

    fn writable(&self) -> bool {
        self.status == Status::Ready && !self.dir.as_os_str().is_empty()
    }

    fn items_dir(&self) -> PathBuf {
        self.dir.join("items")
    }

    fn undo_dir(&self) -> PathBuf {
        self.dir.join(".undo")
    }

    fn thumbs_dir(&self) -> PathBuf {
        self.dir.join("thumbs")
    }

    pub fn status(&self) -> Status {
        self.status
    }

    fn weight(&self, item: &Item) -> u64 {
        item.size + self.thumbs.get(&item.id).copied().unwrap_or(0)
    }

    pub fn snapshot(&self) -> Snapshot {
        let total: u64 = self.items.iter().map(|item| self.weight(item)).sum();
        let cap = Limits::current().max_bytes;
        let unprotected = self.items.iter().any(|item| !item.protected());
        Snapshot {
            items: self.items.clone(),
            boards: self.boards.clone(),
            status: self.status,
            pinned_mb: (total > cap && !unprotected).then_some(total / (1024 * 1024)),
            last_board: self.last_board.clone(),
        }
    }

    // ---- reading ------------------------------------------------------------------------

    fn read_index(&mut self) {
        let path = self.dir.join("index.json");
        let bytes = match clip_seal::read(&path) {
            Some(bytes) => bytes,
            None if !path.exists() => return,
            None => {
                self.status = Status::Unreadable;
                return;
            }
        };
        let Some(Value::Object(members)) = json::parse_with_items(&bytes, INDEX_BYTES, INDEX_ITEMS)
        else {
            self.status = Status::Unreadable;
            return;
        };
        let member = |name: &str| members.iter().find(|(k, _)| k == name).map(|(_, v)| v);
        match member("version").and_then(Value::as_f64) {
            Some(version) if version > 1.0 => {
                self.status = Status::Newer;
                return;
            }
            Some(1.0) => {}
            _ => {
                self.status = Status::Unreadable;
                return;
            }
        }
        for (key, value) in &members {
            if !matches!(key.as_str(), "version" | "writer" | "items") {
                self.top_extra.push((key.clone(), value.clone()));
            }
        }
        let Some(rows) = member("items").and_then(Value::as_array) else {
            self.status = Status::Unreadable;
            return;
        };
        for row in rows {
            if let Some(item) = parse_item(row)
                && self.payload_present(&item)
                && !self.items.iter().any(|other| other.id == item.id)
            {
                self.items.push(item);
            }
        }
        self.items.sort_by(|a, b| b.used.total_cmp(&a.used));
    }

    /// The item's main file is on disk (a row without it is dropped, as the Mac does).
    fn payload_present(&self, item: &Item) -> bool {
        let name = match item.kind {
            Kind::Image => format!("{}.png", item.id),
            Kind::File => format!("{}.files.json", item.id),
            _ => format!("{}.txt", item.id),
        };
        self.items_dir().join(name).is_file()
    }

    fn read_boards(&mut self) {
        let path = self.dir.join("boards.json");
        let Some(bytes) = clip_seal::read(&path) else {
            return;
        };
        let Some(Value::Object(members)) = json::parse_with_items(&bytes, INDEX_BYTES, INDEX_ITEMS)
        else {
            return;
        };
        for (key, value) in &members {
            match key.as_str() {
                "version" => {}
                "boards" => {
                    for row in value.as_array().unwrap_or(&[]) {
                        if let Some(board) = parse_board(row) {
                            self.boards.push(board);
                        }
                    }
                }
                "lastBoard" => self.last_board = value.as_str().map(str::to_string),
                _ => self.boards_extra.push((key.clone(), value.clone())),
            }
        }
        self.boards.sort_by_key(|board| board.order);
    }

    /// Makes `boards` and each board's `itemOrder` agree (the index is the truth for who is
    /// on a board): an id that names nothing is dropped.
    fn repair(&mut self) {
        let known: Vec<String> = self.boards.iter().map(|b| b.id.clone()).collect();
        for item in &mut self.items {
            item.boards.retain(|id| known.contains(id));
        }
        let items = &self.items;
        for board in &mut self.boards {
            board.item_order.retain(|id| {
                items
                    .iter()
                    .any(|item| item.id == *id && item.boards.contains(&board.id))
            });
        }
        if self
            .last_board
            .as_ref()
            .is_some_and(|id| !known.contains(id))
        {
            self.last_board = None;
        }
        // Thumbnail sizes count toward the cap.
        for item in &self.items {
            if item.kind == Kind::Image
                && let Ok(meta) = fs::metadata(self.thumbs_dir().join(format!("{}.png", item.id)))
            {
                self.thumbs.insert(item.id.clone(), meta.len());
            }
        }
    }

    /// Deletes files no index row names (a payload written before a crash).
    fn sweep_orphans(&self) {
        for folder in [self.items_dir(), self.thumbs_dir()] {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.split('.').next().unwrap_or("");
                if !self.items.iter().any(|item| item.id == id) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    // ---- writing ------------------------------------------------------------------------

    fn write_file(&self, path: &Path, bytes: &[u8]) -> bool {
        self.writable() && clip_seal::write(path, bytes)
    }

    fn save_index(&self) {
        if !self.writable() {
            return;
        }
        let mut out = String::with_capacity(256 + self.items.len() * 400);
        out.push_str("{\"version\":1,\"writer\":");
        json_str(&mut out, &format!("windows {}", crate::host::app_version()));
        out.push_str(",\"items\":[");
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            write_item(&mut out, item);
        }
        out.push(']');
        for (key, value) in &self.top_extra {
            out.push(',');
            json_str(&mut out, key);
            out.push(':');
            write_value(&mut out, value);
        }
        out.push_str("}\n");
        self.write_file(&self.dir.join("index.json"), out.as_bytes());
    }

    fn save_boards(&self) {
        if !self.writable() {
            return;
        }
        let mut out = String::from("{\"version\":1,\"boards\":[");
        for (index, board) in self.boards.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"id\":");
            json_str(&mut out, &board.id);
            out.push_str(",\"name\":");
            json_str(&mut out, &board.name);
            out.push_str(",\"colour\":");
            json_str(&mut out, &board.colour);
            out.push_str(&format!(",\"order\":{},\"itemOrder\":[", board.order));
            for (n, id) in board.item_order.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                json_str(&mut out, id);
            }
            out.push_str("],\"createdAt\":");
            write_number(&mut out, board.created);
            for (key, value) in &board.extra {
                out.push(',');
                json_str(&mut out, key);
                out.push(':');
                write_value(&mut out, value);
            }
            out.push('}');
        }
        out.push(']');
        if let Some(id) = &self.last_board {
            out.push_str(",\"lastBoard\":");
            json_str(&mut out, id);
        }
        for (key, value) in &self.boards_extra {
            out.push(',');
            json_str(&mut out, key);
            out.push(':');
            write_value(&mut out, value);
        }
        out.push_str("}\n");
        self.write_file(&self.dir.join("boards.json"), out.as_bytes());
    }

    fn remove_files(&mut self, id: &str) {
        for name in ["txt", "png", "html", "rtf", "files.json"] {
            let _ = fs::remove_file(self.items_dir().join(format!("{id}.{name}")));
        }
        let _ = fs::remove_file(self.thumbs_dir().join(format!("{id}.png")));
        self.thumbs.remove(id);
    }

    // ---- adding -------------------------------------------------------------------------

    /// Stores a capture (spec 3.2 to 3.4): the limits, then the digest; a digest already in
    /// the history moves that item to the top instead of adding another.
    pub fn add(&mut self, capture: Capture, now: f64) -> Added {
        if !self.writable() {
            return Added::Skipped;
        }
        let Some(prepared) = Prepared::build(capture) else {
            return Added::Skipped;
        };
        if let Some((digest, at)) = &self.last_capture
            && *digest == prepared.digest
            && at.elapsed() < REPEAT_GUARD
        {
            return Added::Skipped;
        }
        self.last_capture = Some((prepared.digest.clone(), Instant::now()));
        if let Some(position) = self
            .items
            .iter()
            .position(|item| item.digest == prepared.digest && item.kind == prepared.kind)
        {
            // The same copy again: pins, boards, the id and the lane stay.
            let mut item = self.items.remove(position);
            item.used = now;
            if prepared.source.is_some() {
                item.source = prepared.source;
            }
            self.items.insert(0, item);
            self.save_index();
            return Added::Repeated;
        }
        let id = uuid4();
        if !self.write_payload(&id, &prepared) {
            return Added::Skipped;
        }
        let mut meta = prepared.meta;
        let mut screenshot = false;
        if prepared.kind == Kind::Image && prepared.screenshot {
            screenshot = true;
            // A run of screenshots less than a minute apart is one stack.
            let before = self
                .items
                .iter()
                .filter(|other| other.screenshot)
                .max_by(|a, b| a.created.total_cmp(&b.created));
            let stack = match before {
                Some(other) if now - other.created <= STACK_SECONDS => {
                    other.stack_id().map(str::to_string)
                }
                _ => None,
            };
            meta.push((
                "stackId".to_string(),
                Value::Text(stack.unwrap_or_else(uuid4)),
            ));
        }
        if prepared.kind == Kind::Image {
            meta.push(("screenshot".to_string(), Value::Bool(screenshot)));
        }
        if let Some(thumb) = &prepared.thumb {
            let path = self.thumbs_dir().join(format!("{id}.png"));
            if self.write_file(&path, thumb) {
                self.thumbs.insert(id.clone(), thumb.len() as u64);
            }
        }
        self.items.insert(
            0,
            Item {
                id,
                kind: prepared.kind,
                preview: prepared.preview,
                title: None,
                source: prepared.source,
                created: now,
                used: now,
                uses: 0,
                pinned: false,
                boards: Vec::new(),
                size: prepared.size,
                digest: prepared.digest,
                screenshot,
                meta,
                concealed: false,
                extra: Vec::new(),
            },
        );
        self.retention(now);
        self.save_index();
        Added::New
    }

    /// Writes the item's payload files; false when one could not be written.
    fn write_payload(&self, id: &str, prepared: &Prepared) -> bool {
        let dir = self.items_dir();
        let write = |extension: &str, bytes: &[u8]| {
            self.write_file(&dir.join(format!("{id}.{extension}")), bytes)
        };
        match prepared.kind {
            Kind::Image => prepared.png.as_deref().is_some_and(|png| write("png", png)),
            Kind::File => {
                let mut out = String::from("{\"paths\":[");
                for (index, path) in prepared.paths.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    json_str(&mut out, path);
                }
                out.push_str("]}");
                write("files.json", out.as_bytes())
            }
            _ => {
                let mut ok = write("txt", prepared.text.as_bytes());
                if let Some(html) = &prepared.html {
                    ok &= write("html", html);
                }
                if let Some(rtf) = &prepared.rtf {
                    ok &= write("rtf", rtf);
                }
                ok
            }
        }
    }

    // ---- retention ----------------------------------------------------------------------

    /// The caps of spec section 4. The strictest wins; the oldest `used` goes first among
    /// items that are not protected. Returns whether anything was removed.
    pub fn retention(&mut self, now: f64) -> bool {
        let limits = Limits::current();
        let before = self.items.len();
        let mut doomed: Vec<String> = Vec::new();
        let expired = |item: &Item| {
            if item.protected() {
                return false;
            }
            let age = now - item.used;
            if item.screenshot {
                limits.screenshot_hours > 0 && age > f64::from(limits.screenshot_hours) * 3600.0
            } else {
                limits.max_age_days > 0 && age > f64::from(limits.max_age_days) * 86_400.0
            }
        };
        doomed.extend(
            self.items
                .iter()
                .filter(|i| expired(i))
                .map(|i| i.id.clone()),
        );
        // Oldest first, so the evictions below take the right ones.
        let mut oldest: Vec<usize> = (0..self.items.len())
            .filter(|&i| !self.items[i].protected() && !doomed.contains(&self.items[i].id))
            .collect();
        oldest.sort_by(|&a, &b| self.items[a].used.total_cmp(&self.items[b].used));
        // Count: the unprotected items of the main lane only; pinned and boarded ones neither
        // count toward the cap nor are taken.
        let mut main_lane = self
            .items
            .iter()
            .filter(|item| !item.screenshot && !item.protected() && !doomed.contains(&item.id))
            .count();
        for &i in &oldest {
            if main_lane <= limits.max_items {
                break;
            }
            if !self.items[i].screenshot {
                doomed.push(self.items[i].id.clone());
                main_lane -= 1;
            }
        }
        // The screenshot lane's own size cap.
        let mut lane: u64 = self
            .items
            .iter()
            .filter(|item| item.screenshot && !doomed.contains(&item.id))
            .map(|item| self.weight(item))
            .sum();
        for &i in &oldest {
            if lane <= SCREENSHOT_LANE_BYTES {
                break;
            }
            let item = &self.items[i];
            if item.screenshot && !doomed.contains(&item.id) {
                lane = lane.saturating_sub(self.weight(item));
                doomed.push(item.id.clone());
            }
        }
        // The total size cap, everything counted.
        let mut total: u64 = self
            .items
            .iter()
            .filter(|item| !doomed.contains(&item.id))
            .map(|item| self.weight(item))
            .sum();
        for &i in &oldest {
            if total <= limits.max_bytes {
                break;
            }
            let item = &self.items[i];
            if !doomed.contains(&item.id) {
                total = total.saturating_sub(self.weight(item));
                doomed.push(item.id.clone());
            }
        }
        for id in &doomed {
            self.remove_files(id);
        }
        self.items.retain(|item| !doomed.contains(&item.id));
        self.items.len() != before
    }

    /// The hourly pass (and the one after a setting changed).
    pub fn retention_pass(&mut self) {
        if self.writable() && self.retention(now()) {
            self.save_index();
        }
    }

    // ---- reading an item ----------------------------------------------------------------

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    /// The id of the newest item (the one a capture just added or moved to the top).
    pub fn newest_id(&self) -> Option<String> {
        self.items.first().map(|item| item.id.clone())
    }

    /// The formats a paste writes for the item.
    pub fn payload(&self, id: &str) -> Option<Payload> {
        let item = self.get(id)?;
        let dir = self.items_dir();
        let read = |extension: &str| clip_seal::read(&dir.join(format!("{}.{extension}", item.id)));
        let mut payload = Payload {
            text: String::new(),
            html: None,
            rtf: None,
            png: None,
            paths: Vec::new(),
        };
        match item.kind {
            Kind::Image => {
                let path = dir.join(format!("{}.png", item.id));
                path.is_file().then_some(())?;
                payload.png = Some(path);
            }
            Kind::File => {
                let bytes = read("files.json")?;
                let Some(Value::Object(members)) = json::parse(&bytes, 4 * 1024 * 1024) else {
                    return None;
                };
                payload.paths = members
                    .iter()
                    .find(|(key, _)| key == "paths")
                    .and_then(|(_, value)| value.as_array())?
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_string))
                    .collect();
            }
            _ => {
                payload.text = String::from_utf8_lossy(&read("txt")?).into_owned();
                if item.kind == Kind::Rich {
                    payload.html = read("html");
                    payload.rtf = read("rtf");
                }
            }
        }
        Some(payload)
    }

    /// The item was pasted or copied from the strip or the launcher.
    pub fn touch(&mut self, id: &str, now: f64) {
        if let Some(position) = self.items.iter().position(|item| item.id == id) {
            let mut item = self.items.remove(position);
            item.used = now;
            item.uses += 1;
            self.items.insert(0, item);
            self.save_index();
        }
    }

    // ---- changing an item ---------------------------------------------------------------

    pub fn set_pinned(&mut self, id: &str, pinned: bool) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.pinned = pinned;
            self.save_index();
        }
    }

    /// "Keep as screenshot" and "Not a screenshot": the lane sticks (`laneLocked`).
    pub fn set_lane(&mut self, id: &str, screenshot: bool) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id)
            && item.kind == Kind::Image
        {
            item.screenshot = screenshot;
            item.set_meta("screenshot", Value::Bool(screenshot));
            item.set_meta("laneLocked", Value::Bool(true));
            self.save_index();
        }
    }

    /// The ids in the same stack as `id` (itself included), for a stack card.
    pub fn stack_of(&self, id: &str) -> Vec<String> {
        let Some(stack) = self.get(id).and_then(|item| item.stack_id()) else {
            return vec![id.to_string()];
        };
        self.items
            .iter()
            .filter(|item| item.screenshot && item.stack_id() == Some(stack))
            .map(|item| item.id.clone())
            .collect()
    }

    /// Removes the item at once, files and all. The returned copy restores it when it is
    /// asked for within `UNDO_SECONDS`; an item over 5 MB has its files moved into `.undo/`
    /// (removed after that time, or when the next launch finds them).
    pub fn delete(&mut self, id: &str, now: f64) -> Option<Undo> {
        let position = self.items.iter().position(|item| item.id == id)?;
        let item = self.items.remove(position);
        let mut files = Vec::new();
        let mut disk = Vec::new();
        if item.size > UNDO_BYTES {
            let undo_dir = self.undo_dir();
            if fs::create_dir_all(&undo_dir).is_ok() {
                let mut sources = Vec::new();
                for name in ["txt", "png", "html", "rtf", "files.json"] {
                    let file = format!("{}.{name}", item.id);
                    sources.push((self.items_dir().join(&file), file));
                }
                let thumb = format!("thumbs/{}.png", item.id);
                sources.push((self.thumbs_dir().join(format!("{}.png", item.id)), thumb));
                for (from, file) in sources {
                    let to = undo_dir.join(file.replace('/', "_"));
                    if from.exists() && move_file(&from, &to) {
                        disk.push((file, to));
                    }
                }
                let waiting: Vec<PathBuf> = disk.iter().map(|(_, path)| path.clone()).collect();
                let _ = std::thread::Builder::new()
                    .name("rightkit-clip-undo".to_string())
                    .spawn(move || {
                        std::thread::sleep(Duration::from_secs_f64(UNDO_SECONDS + 1.0));
                        for path in waiting {
                            let _ = fs::remove_file(path);
                        }
                    });
            }
        } else {
            for name in ["txt", "png", "html", "rtf", "files.json"] {
                let file = format!("{}.{name}", item.id);
                if let Some(bytes) = clip_seal::read(&self.items_dir().join(&file)) {
                    files.push((file, bytes));
                }
            }
            if let Some(bytes) =
                clip_seal::read(&self.thumbs_dir().join(format!("{}.png", item.id)))
            {
                files.push((format!("thumbs/{}.png", item.id), bytes));
            }
        }
        let mut slots = Vec::new();
        for board in &mut self.boards {
            if let Some(at) = board
                .item_order
                .iter()
                .position(|listed| *listed == item.id)
            {
                board.item_order.remove(at);
                slots.push((board.id.clone(), at));
            }
        }
        self.remove_files(id);
        self.save_index();
        self.save_boards();
        let kept = !files.is_empty() || !disk.is_empty();
        kept.then_some(Undo {
            item,
            files,
            disk,
            slots,
            at: now,
        })
    }

    /// Puts a deleted item back: its files, its row and its places on the boards.
    pub fn restore(&mut self, undo: Undo) {
        if !self.writable() {
            return;
        }
        for (file, waiting) in &undo.disk {
            let path = match file.strip_prefix("thumbs/") {
                Some(name) => self.thumbs_dir().join(name),
                None => self.items_dir().join(file),
            };
            if move_file(waiting, &path) && file.starts_with("thumbs/") {
                let size = fs::metadata(&path).map_or(0, |meta| meta.len());
                self.thumbs.insert(undo.item.id.clone(), size);
            }
        }
        for (file, bytes) in &undo.files {
            let path = match file.strip_prefix("thumbs/") {
                Some(name) => self.thumbs_dir().join(name),
                None => self.items_dir().join(file),
            };
            self.write_file(&path, bytes);
            if file.starts_with("thumbs/") {
                self.thumbs.insert(undo.item.id.clone(), bytes.len() as u64);
            }
        }
        for (board_id, at) in &undo.slots {
            if let Some(board) = self.boards.iter_mut().find(|board| board.id == *board_id) {
                board
                    .item_order
                    .insert((*at).min(board.item_order.len()), undo.item.id.clone());
            }
        }
        self.items.push(undo.item);
        self.items.sort_by(|a, b| b.used.total_cmp(&a.used));
        self.save_index();
        self.save_boards();
    }

    /// Deletes every item, pinned ones too (Boards stay). The caller has asked "Delete N
    /// items? This can't be undone."
    pub fn clear_all(&mut self) {
        if !self.writable() {
            return;
        }
        for folder in [self.items_dir(), self.thumbs_dir()] {
            if let Ok(entries) = fs::read_dir(&folder) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        self.items.clear();
        self.thumbs.clear();
        for board in &mut self.boards {
            board.item_order.clear();
        }
        self.save_index();
        self.save_boards();
    }

    /// The items a clear of `scope` removes now. A window keeps pinned and board items, as the
    /// retention pass does; `All` takes everything.
    fn scope_ids(&self, scope: ClearScope, now: f64) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| match scope {
                ClearScope::All => true,
                ClearScope::LastHour => !item.protected() && item.created >= now - 3600.0,
                ClearScope::OlderThanDay => !item.protected() && item.created < now - 86_400.0,
            })
            .map(|item| item.id.clone())
            .collect()
    }

    /// How many items a clear of `scope` would remove (the launcher's confirmation says so).
    pub fn scope_count(&self, scope: ClearScope, now: f64) -> usize {
        self.scope_ids(scope, now).len()
    }

    /// Clears `scope` (docs/clipboard-spec.md 9): `All` is `clear_all`; a window removes its
    /// items at once, files and board places with them, and saves the index. Returns the count.
    pub fn clear_scope(&mut self, scope: ClearScope, now: f64) -> usize {
        if scope == ClearScope::All {
            let count = self.items.len();
            self.clear_all();
            return count;
        }
        if !self.writable() {
            return 0;
        }
        let ids = self.scope_ids(scope, now);
        for id in &ids {
            self.remove_files(id);
            for board in &mut self.boards {
                board.item_order.retain(|placed| placed != id);
            }
        }
        self.items.retain(|item| !ids.contains(&item.id));
        if !ids.is_empty() {
            self.save_index();
            self.save_boards();
        }
        ids.len()
    }

    // ---- boards -------------------------------------------------------------------------

    /// A new board with the first palette colour no board uses.
    pub fn board_create(&mut self, name: &str, now: f64) -> Result<String, &'static str> {
        let name = name.trim();
        self.check_name(name, None)?;
        if self.boards.len() >= BOARDS_MAX {
            return Err("Up to 20 boards");
        }
        let colour = PALETTE
            .iter()
            .map(|(_, hex)| *hex)
            .find(|hex| !self.boards.iter().any(|board| board.colour == *hex))
            .unwrap_or(PALETTE[0].1);
        let hex: String = uuid4().chars().filter(|c| *c != '-').take(6).collect();
        let id = format!("b-{hex}");
        let order = self
            .boards
            .iter()
            .map(|b| b.order)
            .max()
            .map_or(0, |m| m + 1);
        self.boards.push(Board {
            id: id.clone(),
            name: name.to_string(),
            colour: colour.to_string(),
            order,
            item_order: Vec::new(),
            created: now,
            extra: Vec::new(),
        });
        self.save_boards();
        Ok(id)
    }

    fn check_name(&self, name: &str, own: Option<&str>) -> Result<(), &'static str> {
        let count = name.chars().count();
        if count == 0 {
            return Err("Name the board");
        }
        if count > 40 {
            return Err("Up to 40 characters");
        }
        let folded = crate::search::fold(name);
        let taken = self.boards.iter().any(|board| {
            Some(board.id.as_str()) != own && crate::search::fold(&board.name) == folded
        });
        if taken {
            Err("That name is taken")
        } else {
            Ok(())
        }
    }

    pub fn board_rename(&mut self, id: &str, name: &str) -> Result<(), &'static str> {
        let name = name.trim();
        self.check_name(name, Some(id))?;
        if let Some(board) = self.boards.iter_mut().find(|board| board.id == id) {
            board.name = name.to_string();
            self.save_boards();
        }
        Ok(())
    }

    pub fn board_colour(&mut self, id: &str, colour: &str) {
        if let Some(board) = self.boards.iter_mut().find(|board| board.id == id) {
            board.colour = colour.to_string();
            self.save_boards();
        }
    }

    /// Moves a board one place left (`-1`) or right (`1`) among the tabs.
    pub fn board_move(&mut self, id: &str, delta: i32) {
        let Some(at) = self.boards.iter().position(|board| board.id == id) else {
            return;
        };
        let to = (at as i32 + delta).clamp(0, self.boards.len() as i32 - 1) as usize;
        if to != at {
            let board = self.boards.remove(at);
            self.boards.insert(to, board);
            for (order, board) in self.boards.iter_mut().enumerate() {
                board.order = order as i64;
            }
            self.save_boards();
        }
    }

    /// Deletes a board; its items stay in History (and unprotected when it was all that
    /// kept them).
    pub fn board_delete(&mut self, id: &str) {
        self.boards.retain(|board| board.id != id);
        for item in &mut self.items {
            item.boards.retain(|listed| listed != id);
        }
        if self.last_board.as_deref() == Some(id) {
            self.last_board = None;
        }
        self.save_boards();
        self.save_index();
    }

    /// Puts the item on the board, or takes it off when it is on it. New items go first.
    /// Returns whether the item is on the board now.
    pub fn board_toggle(&mut self, item_id: &str, board_id: &str) -> bool {
        let Some(board) = self.boards.iter_mut().find(|board| board.id == board_id) else {
            return false;
        };
        let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) else {
            return false;
        };
        let on = if let Some(at) = item.boards.iter().position(|listed| listed == board_id) {
            item.boards.remove(at);
            board.item_order.retain(|listed| listed != item_id);
            false
        } else if board.item_order.len() >= BOARD_ITEMS_MAX {
            false
        } else {
            item.boards.push(board_id.to_string());
            board.item_order.insert(0, item_id.to_string());
            true
        };
        self.save_boards();
        self.save_index();
        on
    }

    /// Moves an item one place earlier (`-1`) or later (`1`) in a board's own order.
    pub fn board_item_move(&mut self, item_id: &str, board_id: &str, delta: i32) {
        let on_board: Vec<String> = {
            let mut listed: Vec<&Item> = self
                .items
                .iter()
                .filter(|item| item.boards.iter().any(|id| id == board_id))
                .collect();
            let Some(board) = self.boards.iter().find(|board| board.id == board_id) else {
                return;
            };
            // The order the strip shows: the board's list, then the rest by use.
            let place = |item: &&Item| {
                board
                    .item_order
                    .iter()
                    .position(|id| *id == item.id)
                    .unwrap_or(usize::MAX)
            };
            listed.sort_by_key(place);
            listed.iter().map(|item| item.id.clone()).collect()
        };
        let Some(at) = on_board.iter().position(|id| id == item_id) else {
            return;
        };
        let to = (at as i32 + delta).clamp(0, on_board.len() as i32 - 1) as usize;
        let mut order = on_board;
        let moved = order.remove(at);
        order.insert(to, moved);
        if let Some(board) = self.boards.iter_mut().find(|board| board.id == board_id) {
            board.item_order = order;
            self.save_boards();
        }
    }

    /// The board the strip is left on, kept in `boards.json` for `clipboardRememberBoard`.
    pub fn remember_board(&mut self, id: Option<&str>) {
        let id = id.map(str::to_string);
        if self.last_board != id {
            self.last_board = id;
            self.save_boards();
        }
    }
}

// ---- turning a capture into the stored form -----------------------------------------------

/// A capture with its limits applied, its kind decided and its digest made (spec 3.2, 3.3).
struct Prepared {
    kind: Kind,
    preview: String,
    text: String,
    html: Option<Vec<u8>>,
    rtf: Option<Vec<u8>>,
    png: Option<Vec<u8>>,
    thumb: Option<Vec<u8>>,
    paths: Vec<String>,
    meta: Vec<(String, Value)>,
    source: Option<SourceApp>,
    screenshot: bool,
    size: u64,
    digest: String,
}

fn number(value: usize) -> Value {
    Value::Number(value as f64)
}

impl Prepared {
    fn build(capture: Capture) -> Option<Prepared> {
        let Capture {
            kind,
            text,
            html,
            rtf,
            png,
            thumb,
            paths,
            source,
            screenshot,
        } = capture;
        let mut prepared = Prepared {
            kind,
            preview: String::new(),
            text: String::new(),
            html: None,
            rtf: None,
            png: None,
            thumb: None,
            paths: Vec::new(),
            meta: Vec::new(),
            source,
            screenshot,
            size: 0,
            digest: String::new(),
        };
        match kind {
            Kind::Image => {
                let (bytes, width, height) = png?;
                if bytes.is_empty()
                    || bytes.len() > IMAGE_MAX
                    || width > IMAGE_SIDE_MAX
                    || height > IMAGE_SIDE_MAX
                {
                    return None;
                }
                prepared.digest = format!("sha256:{}", rightkit_fs::digest::sha256_bytes(&bytes));
                prepared.size = bytes.len() as u64;
                prepared.meta = vec![
                    ("width".to_string(), Value::Number(f64::from(width))),
                    ("height".to_string(), Value::Number(f64::from(height))),
                    ("format".to_string(), Value::Text("png".to_string())),
                ];
                prepared.png = Some(bytes);
                prepared.thumb = thumb;
            }
            Kind::File => {
                let total = paths.len();
                let mut kept: Vec<String> = paths.into_iter().take(PATHS_MAX).collect();
                if kept.is_empty() {
                    return None;
                }
                let names: Vec<String> = kept
                    .iter()
                    .map(|path| {
                        Path::new(path)
                            .file_name()
                            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned())
                    })
                    .collect();
                prepared.preview = names.join(", ").chars().take(PREVIEW_CHARS).collect();
                prepared.meta = vec![
                    ("count".to_string(), number(total)),
                    (
                        "names".to_string(),
                        Value::Array(names.iter().take(8).cloned().map(Value::Text).collect()),
                    ),
                    ("anyMissing".to_string(), Value::Bool(false)),
                ];
                kept.sort();
                let mut bytes = Vec::new();
                for path in &kept {
                    bytes.extend_from_slice(path.as_bytes());
                    bytes.push(b'\n');
                }
                prepared.digest = format!("sha256:{}", rightkit_fs::digest::sha256_bytes(&bytes));
                prepared.size = kept.iter().map(|p| p.len() as u64).sum();
                prepared.paths = kept;
                // The order the copy had is not kept: the sorted list is what is stored.
            }
            _ => {
                if text.trim().is_empty()
                    || text.len() > TEXT_MAX
                    || (kind != Kind::Rich && text.trim().chars().count() < 2)
                {
                    return None;
                }
                let html = html.filter(|bytes| !bytes.is_empty() && bytes.len() <= RICH_MAX);
                let rtf = rtf.filter(|bytes| !bytes.is_empty() && bytes.len() <= RICH_MAX);
                // Rich text with nothing rich left is plain text.
                let rich = kind == Kind::Rich && (html.is_some() || rtf.is_some());
                prepared.preview = preview_of(&text);
                let chars = text.chars().count();
                let lines = text.lines().count().max(1);
                if rich {
                    prepared.kind = Kind::Rich;
                    prepared.meta = vec![
                        ("chars".to_string(), number(chars)),
                        ("hasHtml".to_string(), Value::Bool(html.is_some())),
                        ("hasRtf".to_string(), Value::Bool(rtf.is_some())),
                    ];
                    let mut bytes = text.as_bytes().to_vec();
                    bytes.push(0);
                    bytes.extend_from_slice(html.as_deref().unwrap_or(&[]));
                    prepared.digest =
                        format!("sha256:{}", rightkit_fs::digest::sha256_bytes(&bytes));
                    prepared.size = (text.len()
                        + html.as_ref().map_or(0, Vec::len)
                        + rtf.as_ref().map_or(0, Vec::len))
                        as u64;
                    prepared.html = html;
                    prepared.rtf = rtf;
                } else {
                    match clip_detect::detect(&text) {
                        Detected::Text => {
                            prepared.kind = Kind::Text;
                            prepared.meta = text_meta(chars, lines, None);
                        }
                        Detected::Code { language } => {
                            prepared.kind = Kind::Code;
                            prepared.meta = text_meta(chars, lines, language);
                        }
                        Detected::Link { host, scheme } => {
                            prepared.kind = Kind::Link;
                            prepared.preview = text.trim().to_string();
                            prepared.meta = vec![
                                ("host".to_string(), Value::Text(host)),
                                ("scheme".to_string(), Value::Text(scheme)),
                            ];
                        }
                        Detected::Colour { hex } => {
                            prepared.kind = Kind::Colour;
                            prepared.preview = hex.clone();
                            prepared.meta = vec![
                                ("hex".to_string(), Value::Text(hex)),
                                ("original".to_string(), Value::Text(text.clone())),
                            ];
                        }
                    }
                    prepared.digest = format!(
                        "sha256:{}",
                        rightkit_fs::digest::sha256_bytes(text.as_bytes())
                    );
                    prepared.size = text.len() as u64;
                }
                prepared.text = text;
            }
        }
        Some(prepared)
    }
}

fn text_meta(chars: usize, lines: usize, language: Option<&str>) -> Vec<(String, Value)> {
    let mut meta = vec![
        ("chars".to_string(), number(chars)),
        ("lines".to_string(), number(lines)),
    ];
    if let Some(language) = language {
        meta.push(("language".to_string(), Value::Text(language.to_string())));
    }
    meta
}

// ---- reading and writing rows -------------------------------------------------------------

const ITEM_KEYS: [&str; 15] = [
    "id",
    "kind",
    "preview",
    "title",
    "sourceApp",
    "createdAt",
    "lastUsedAt",
    "useCount",
    "pinned",
    "boards",
    "size",
    "digest",
    "lane",
    "meta",
    "concealed",
];

pub fn parse_item(row: &Value) -> Option<Item> {
    let Value::Object(members) = row else {
        return None;
    };
    let get = |name: &str| members.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let id = get("id")?.as_str()?;
    if !id_ok(id) {
        return None;
    }
    let kind = Kind::parse(get("kind")?.as_str()?)?;
    let source = match get("sourceApp") {
        Some(Value::Object(_)) => {
            let app = get("sourceApp")?;
            Some(SourceApp {
                id: app.get("id")?.as_str()?.to_string(),
                name: app
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        }
        _ => None,
    };
    let created = get("createdAt").and_then(Value::as_f64).unwrap_or_else(now);
    let boards = get("boards")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let meta = match get("meta") {
        Some(Value::Object(meta)) => meta.clone(),
        _ => Vec::new(),
    };
    Some(Item {
        id: id.to_string(),
        kind,
        preview: get("preview")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        title: get("title").and_then(Value::as_str).map(str::to_string),
        source,
        created,
        used: get("lastUsedAt").and_then(Value::as_f64).unwrap_or(created),
        uses: get("useCount")
            .and_then(Value::as_f64)
            .map_or(0, |n| n.max(0.0) as u64),
        pinned: get("pinned").and_then(Value::as_bool).unwrap_or(false),
        boards,
        size: get("size")
            .and_then(Value::as_f64)
            .map_or(0, |n| n.max(0.0) as u64),
        digest: get("digest")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        screenshot: get("lane").and_then(Value::as_str) == Some("screenshot"),
        meta,
        concealed: get("concealed").and_then(Value::as_bool).unwrap_or(false),
        extra: members
            .iter()
            .filter(|(key, _)| !ITEM_KEYS.contains(&key.as_str()))
            .cloned()
            .collect(),
    })
}

pub fn parse_board(row: &Value) -> Option<Board> {
    let id = row.get("id")?.as_str()?;
    let name = row.get("name")?.as_str()?;
    if !(id.starts_with("b-") && (3..=40).contains(&id.len())) || name.is_empty() {
        return None;
    }
    let Value::Object(members) = row else {
        return None;
    };
    Some(Board {
        id: id.to_string(),
        name: name.to_string(),
        colour: row
            .get("colour")
            .and_then(Value::as_str)
            .unwrap_or(PALETTE[0].1)
            .to_string(),
        order: row
            .get("order")
            .and_then(Value::as_f64)
            .map_or(0, |n| n as i64),
        item_order: row
            .get("itemOrder")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        created: row.get("createdAt").and_then(Value::as_f64).unwrap_or(0.0),
        extra: members
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "id" | "name" | "colour" | "order" | "itemOrder" | "createdAt"
                )
            })
            .cloned()
            .collect(),
    })
}

/// A JSON string, escaped.
fn json_str(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_number(out: &mut String, value: f64) {
    if !value.is_finite() {
        out.push('0');
    } else if value.fract() == 0.0 && value.abs() < 9e15 {
        out.push_str(&format!("{}", value as i64));
    } else {
        out.push_str(&format!("{value}"));
    }
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(n) => write_number(out, *n),
        Value::Text(text) => json_str(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (index, (key, item)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                json_str(out, key);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

/// One item as a compact JSON object, the keys in the spec's order, then any unknown ones.
fn write_item(out: &mut String, item: &Item) {
    out.push_str("{\"id\":");
    json_str(out, &item.id);
    out.push_str(",\"kind\":");
    json_str(out, item.kind.as_str());
    out.push_str(",\"preview\":");
    json_str(out, &item.preview);
    out.push_str(",\"title\":");
    match &item.title {
        Some(title) => json_str(out, title),
        None => out.push_str("null"),
    }
    out.push_str(",\"sourceApp\":");
    match &item.source {
        Some(app) => {
            out.push_str("{\"id\":");
            json_str(out, &app.id);
            out.push_str(",\"name\":");
            json_str(out, &app.name);
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"createdAt\":");
    write_number(out, item.created);
    out.push_str(",\"lastUsedAt\":");
    write_number(out, item.used);
    out.push_str(&format!(
        ",\"useCount\":{},\"pinned\":{},\"boards\":[",
        item.uses, item.pinned
    ));
    for (index, id) in item.boards.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        json_str(out, id);
    }
    out.push_str(&format!("],\"size\":{},\"digest\":", item.size));
    json_str(out, &item.digest);
    out.push_str(",\"lane\":");
    json_str(
        out,
        if item.screenshot {
            "screenshot"
        } else {
            "main"
        },
    );
    out.push_str(",\"meta\":");
    write_value(out, &Value::Object(item.meta.clone()));
    out.push_str(&format!(",\"concealed\":{}", item.concealed));
    for (key, value) in &item.extra {
        out.push(',');
        json_str(out, key);
        out.push(':');
        write_value(out, value);
    }
    out.push('}');
}

// ---- the strip's hot key ------------------------------------------------------------------

/// The keys with a name besides letters and digits, as the window shortcuts name them.
const NAMED_KEYS: [&str; 19] = [
    "left", "up", "right", "down", "return", "space", "delete", "tab", "-", "=", "[", "]", ",",
    ".", "/", ";", "'", "\\", "`",
];

/// A shortcut in the spec's grammar (7.12): `ctrl`, `alt` and `shift` joined by `+`, then
/// one key, lower-case ("alt+shift+v"). It needs a modifier, and `win` is not allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

pub fn parse_hotkey(text: &str) -> Option<Hotkey> {
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        key: String::new(),
    };
    for part in text.trim().to_ascii_lowercase().split('+') {
        match part {
            "ctrl" => hotkey.ctrl = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            key if hotkey.key.is_empty() => {
                let single = key.len() == 1
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
                if !(single || NAMED_KEYS.contains(&key)) {
                    return None;
                }
                hotkey.key = key.to_string();
            }
            _ => return None,
        }
    }
    let held = hotkey.ctrl || hotkey.alt || hotkey.shift;
    (!hotkey.key.is_empty() && held).then_some(hotkey)
}

pub fn hotkey_valid(text: &str) -> bool {
    parse_hotkey(text).is_some()
}

impl Hotkey {
    /// The chord as RightKit reads it ("Ctrl+Alt+Shift+v"); its Delete is Backspace here.
    pub fn kit_text(&self) -> String {
        let mut text = String::new();
        for (held, word) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if held {
                text.push_str(word);
            }
        }
        text.push_str(if self.key == "delete" {
            "backspace"
        } else {
            &self.key
        });
        text
    }
}
