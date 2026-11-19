//! Transcript sync on top of the engine: the record mapper, stable ids, the local
//! edit-version book, the one-time legacy ledger migration, the CloudKit environment pin and the
//! retry classifiers. Everything here takes explicit paths so integration tests can drive it
//! without an app runtime. Every product-specific value (record type, id and scope hash domains,
//! default source label, helper bundle names, on-disk file names) comes in through
//! [`TranscriptSyncProfile`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_fs::digest::Sha256Hasher;
use serde::{Deserialize, Serialize};

use crate::{field, FieldValue, ItemHeader, Ledger, RawRecord, RecordMapper};

/// A production-schema rejection cannot be recovered by retrying from this
/// Mac; keep a long, finite window so a Console schema deployment can land.
pub const PERMANENT_SCHEMA_FAILURE_BACKOFF: Duration = Duration::from_secs(60 * 60);

// ---------------------------------------------------------------------------
// Host inputs
// ---------------------------------------------------------------------------

/// File names of the sync state, relative to the host's data root. These are on-disk formats:
/// an existing install keeps working only if the host passes the names it already wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFileNames {
    pub ledger: String,
    pub versions: String,
    pub outbox: String,
    pub legacy_tombstones: String,
    pub legacy_pending: String,
    pub legacy_aliases: String,
    pub legacy_acks: String,
}

impl Default for StateFileNames {
    fn default() -> Self {
        Self {
            ledger: "cloud-sync-ledger.json".to_string(),
            versions: "cloud-sync-local-versions.json".to_string(),
            outbox: "cloud-sync-deletions-outbox.json".to_string(),
            legacy_tombstones: "cloud-sync-tombstones.json".to_string(),
            legacy_pending: "cloud-sync-tombstones.pending.json".to_string(),
            legacy_aliases: "cloud-sync-aliases.json".to_string(),
            legacy_acks: "cloud-sync-acks.json".to_string(),
        }
    }
}

/// Everything product-specific the transcript sync needs.
///
/// `id_domain` and `scope_domain` are hashed into logical ids and legacy scoped file names. They
/// are part of the on-disk and on-wire identity: changing either for an existing store changes
/// every logical id and orphans the synced history, so a host that already syncs must pass the
/// exact strings it has always used. The defaults are for a new product only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSyncProfile {
    /// CloudKit record type of one transcript.
    pub record_type: String,
    /// Domain string hashed (followed by a NUL byte) before a non-UUID raw id.
    pub id_domain: String,
    /// Domain string hashed (followed by a NUL byte) into a legacy per-scope file name.
    pub scope_domain: String,
    /// `source` used when a downloaded record carries none.
    pub default_source: String,
    /// Name of the Production-signed CloudKit helper bundle.
    pub helper_bundle_production: String,
    /// Name of the Development-signed CloudKit helper bundle.
    pub helper_bundle_development: String,
    pub files: StateFileNames,
}

impl Default for TranscriptSyncProfile {
    fn default() -> Self {
        Self {
            record_type: "Transcript".to_string(),
            id_domain: "rightkit-cloud-id".to_string(),
            scope_domain: "rightkit-cloud-scope".to_string(),
            default_source: "Unknown".to_string(),
            helper_bundle_production: "CloudKitHelper".to_string(),
            helper_bundle_development: "CloudKitHelperDev".to_string(),
            files: StateFileNames::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Transcript item + mapper
// ---------------------------------------------------------------------------

/// One transcript as the engine sees it. Field set mirrors the CloudKit
/// `Transcript` record shared with iOS: `id`, `text`, `rawText`, `createdAt`,
/// `updatedAt`, `source`, `tags`, `folder` (`deletedAt` is engine-owned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudTranscript {
    /// Logical id (`logical_cloud_id`), also the record name.
    pub id: String,
    pub text: String,
    pub raw_text: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub source: String,
    pub tags: Vec<String>,
    pub folder: Option<String>,
}

fn to_i64(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

fn to_u64(v: i64) -> u64 {
    v.max(0) as u64
}

/// Whole-file atomic replacement through `rightkit_fs::atomic`. Parent directories are created,
/// the temp file is fsynced, the new file is renamed over the old one (`ReplaceMode::Rename`),
/// unix fsyncs the directory, and nothing forces owner-only permissions. One attempt; the error is
/// reported. A unix directory-fsync failure after the rename is returned as an error.
fn replace_file_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let options = rightkit_fs::atomic::AtomicWriteOptions {
        create_parents: true,
        ..Default::default()
    };
    rightkit_fs::atomic::atomic_write_with_options(path, options, |file| file.write_all(bytes))
        .map_err(|error| match error {
            rightkit_fs::FsError::Io { source, .. }
            | rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. } => source,
            other => io::Error::other(other.to_string()),
        })
}

/// Largest legacy file, version book (read by the migration) or version-1 outbox this module
/// reads. A file over it is an error, never a truncated success.
const MAX_STATE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Read a whole state file through one no-follow handle (`O_NOFOLLOW` on Unix, the reparse point
/// itself on Windows; only a regular file opens). `max_bytes` is checked on the open handle before
/// any byte is read and again while reading, and a file over it is an `InvalidData` error. A
/// missing file is `NotFound`. The error text carries no path.
fn read_state_file(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let too_large = || io::Error::new(io::ErrorKind::InvalidData, "state file larger than its cap");
    let file = rightkit_fs::open_read_nofollow(path, &rightkit_fs::ReadOptions::new()).map_err(
        |error| match error {
            rightkit_fs::FsError::Io { source, .. } => source,
            _ => io::Error::new(
                io::ErrorKind::InvalidData,
                "state file is a link or not a regular file",
            ),
        },
    )?;
    if file.metadata()?.len() > max_bytes {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    Ok(bytes)
}

pub struct TranscriptMapper {
    profile: TranscriptSyncProfile,
}

impl TranscriptMapper {
    pub fn new(profile: TranscriptSyncProfile) -> Self {
        Self { profile }
    }
}

impl RecordMapper for TranscriptMapper {
    type Item = CloudTranscript;

    fn record_type(&self) -> &str {
        &self.profile.record_type
    }

    fn header(&self, item: &CloudTranscript) -> ItemHeader {
        ItemHeader {
            id: item.id.clone(),
            created_at_ms: to_i64(item.created_at_ms),
            updated_at_ms: to_i64(item.updated_at_ms),
            origin_device: None,
        }
    }

    fn encode(&self, item: &CloudTranscript, record: &mut RawRecord) {
        record.set("text", FieldValue::Text(item.text.clone()));
        record.set("rawText", FieldValue::Text(item.raw_text.clone()));
        record.set("source", FieldValue::Text(item.source.clone()));
        // Empty list is omitted: CloudKit rejects creating a list field from [].
        record.set_opt(
            "tags",
            (!item.tags.is_empty()).then(|| FieldValue::Strings(item.tags.clone())),
        );
        record.set_opt("folder", item.folder.clone().map(FieldValue::Text));
    }

    fn decode(&self, record: &RawRecord) -> Result<CloudTranscript, String> {
        let raw_id = record
            .text(field::ID)
            .unwrap_or_else(|| record.record_name.clone());
        let text = record
            .get("text")
            .and_then(FieldValue::as_text_or_bytes)
            .ok_or_else(|| "missing text".to_string())?;
        let raw_text = record
            .get("rawText")
            .and_then(FieldValue::as_text_or_bytes)
            .ok_or_else(|| "missing rawText".to_string())?;
        let fallback = record.modified_ms.or(record.created_ms);
        let created = record
            .date_ms(field::CREATED_AT)
            .or(fallback)
            .ok_or_else(|| "missing createdAt".to_string())?;
        let updated = record
            .date_ms(field::UPDATED_AT)
            .or(fallback)
            .ok_or_else(|| "missing updatedAt".to_string())?;
        Ok(CloudTranscript {
            id: self.profile.logical_cloud_id(&raw_id),
            text,
            raw_text,
            created_at_ms: to_u64(created),
            updated_at_ms: to_u64(updated),
            source: record
                .text("source")
                .unwrap_or_else(|| self.profile.default_source.clone()),
            tags: record
                .get("tags")
                .and_then(FieldValue::as_strings)
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            folder: record.text("folder"),
        })
    }
}

impl TranscriptSyncProfile {
    /// Normalize legacy `delivery-*` IDs into the same stable UUID-shaped IDs
    /// used by iOS. The engine keeps every record name seen as an alias.
    pub fn logical_cloud_id(&self, raw: &str) -> String {
        if let Some(uuid) = raw.strip_prefix("cloudkit-") {
            if let Some(id) = canonical_uuid(uuid) {
                return id;
            }
        }
        if let Some(uuid) = canonical_uuid(raw) {
            return uuid;
        }
        let mut digest = Sha256Hasher::new();
        digest.update(self.id_domain.as_bytes());
        digest.update(b"\0");
        digest.update(raw.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.finalize_bytes()[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],bytes[1],bytes[2],bytes[3],bytes[4],bytes[5],bytes[6],bytes[7],
            bytes[8],bytes[9],bytes[10],bytes[11],bytes[12],bytes[13],bytes[14],bytes[15])
    }

    /// Old per-scope file naming; used only to locate legacy files to migrate.
    pub fn legacy_scoped_file_name(
        &self,
        authorized: &str,
        legacy: Option<&str>,
        base: &str,
    ) -> String {
        if legacy.is_none() || legacy == Some(authorized) {
            return base.to_string();
        }
        let mut digest = Sha256Hasher::new();
        digest.update(self.scope_domain.as_bytes());
        digest.update(b"\0");
        digest.update(authorized.as_bytes());
        let hash = digest.finalize();
        format!("cloud-sync-{hash}-{base}")
    }

    pub fn helper_bundle_name(&self, env: CloudKitEnvironment) -> &str {
        match env {
            CloudKitEnvironment::Production => &self.helper_bundle_production,
            CloudKitEnvironment::Development => &self.helper_bundle_development,
        }
    }
}

fn canonical_uuid(raw: &str) -> Option<String> {
    // Hyphens exactly at 8/13/18/23, hex everywhere else.
    let shaped = raw.len() == 36
        && raw.as_bytes().iter().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    shaped.then(|| raw.to_ascii_lowercase())
}

pub fn content_hash(text: &str, raw_text: &str) -> String {
    let mut h = Sha256Hasher::new();
    for part in [text, raw_text] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize()
}

pub fn cloud_source_is_mac(source: &str) -> bool {
    matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "mac" | "macos" | "desktop"
    )
}

// ---------------------------------------------------------------------------
// Local edit versions
// ---------------------------------------------------------------------------

/// History rows carry no `updatedAt`. This book gives each logical id a
/// durable version clock: it advances when the content hash changes, and is
/// set to the remote version when a remote edit lands. LWW needs this.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct VersionEntry {
    #[serde(default)]
    hash: Option<String>,
    updated_at_ms: u64,
}

#[derive(Debug, Default)]
pub struct LocalVersions {
    path: PathBuf,
    entries: BTreeMap<String, VersionEntry>,
    dirty: bool,
}

impl LocalVersions {
    /// Missing or unreadable file starts empty (rows are first-sight: version
    /// = created time, so remote edits win and nothing is lost locally).
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    /// Like [`LocalVersions::load`], but bounded and strict about what it cannot read. A missing
    /// file starts empty and a file that does not decode starts empty (the book is rebuilt from
    /// history). A file that cannot be read, is a link or not a regular file, is over `max_bytes`,
    /// or holds more than `max_entries` entries is an `Err`, so a caller that stops there never
    /// rewrites it. The byte cap is enforced before the file is read into memory; the entry count
    /// is checked after decoding, so decoding is bounded by `max_bytes`.
    pub fn load_capped(
        path: impl Into<PathBuf>,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<Self, String> {
        let path = path.into();
        let entries = match read_state_file(&path, max_bytes) {
            Ok(bytes) => {
                serde_json::from_slice::<BTreeMap<String, VersionEntry>>(&bytes).unwrap_or_default()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(format!("read cloud sync versions: {error}")),
        };
        if entries.len() > max_entries {
            return Err("cloud sync versions hold more entries than the cap".to_string());
        }
        Ok(Self {
            path,
            entries,
            dirty: false,
        })
    }

    pub fn observe(&mut self, id: &str, hash: &str, created_ms: u64, now_ms: u64) -> u64 {
        match self.entries.get_mut(id) {
            None => {
                self.entries.insert(
                    id.to_string(),
                    VersionEntry {
                        hash: Some(hash.to_string()),
                        updated_at_ms: created_ms,
                    },
                );
                self.dirty = true;
                created_ms
            }
            Some(e) if e.hash.is_none() => {
                e.hash = Some(hash.to_string());
                self.dirty = true;
                e.updated_at_ms
            }
            Some(e) if e.hash.as_deref() == Some(hash) => e.updated_at_ms,
            Some(e) => {
                e.updated_at_ms = now_ms.max(e.updated_at_ms.saturating_add(1));
                e.hash = Some(hash.to_string());
                self.dirty = true;
                e.updated_at_ms
            }
        }
    }

    pub fn applied(&mut self, id: &str, hash: &str, updated_at_ms: u64) {
        self.entries.insert(
            id.to_string(),
            VersionEntry {
                hash: Some(hash.to_string()),
                updated_at_ms,
            },
        );
        self.dirty = true;
    }

    pub fn seed(&mut self, id: &str, updated_at_ms: u64) {
        self.entries.entry(id.to_string()).or_insert(VersionEntry {
            hash: None,
            updated_at_ms,
        });
        self.dirty = true;
    }

    pub fn retain_ids(&mut self, live: &BTreeSet<String>) {
        let before = self.entries.len();
        self.entries.retain(|id, _| live.contains(id));
        if self.entries.len() != before {
            self.dirty = true;
        }
    }

    pub fn save_if_dirty(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&self.entries).map_err(|e| e.to_string())?;
        replace_file_atomically(&self.path, &bytes)
            .map_err(|e| format!("write cloud sync versions: {e}"))?;
        self.dirty = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Deletion outbox (durable, engine-lock-free)
// ---------------------------------------------------------------------------

/// The version-1 deletion outbox: a bare `{logical id: enqueued ms}` object with no account
/// scope. The transcript host (`transcript_host`) writes the scoped version-2 file instead (see
/// its `outbox` module). These two functions remain only for a caller that still holds the old
/// shape; they refuse a version-2 file (`{"v":2,"partitions":[...]}`) instead of misreading it as
/// empty or replacing it, because that would drop deletions queued for an account.
pub fn read_outbox(path: &Path) -> Result<BTreeMap<String, u64>, String> {
    match read_state_file(path, MAX_STATE_FILE_BYTES) {
        Ok(b) => {
            if outbox_is_versioned(&b) {
                return Err("deletion outbox is in the scoped format; not readable as v1".into());
            }
            serde_json::from_slice(&b).map_err(|e| format!("parse deletion outbox: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("read deletion outbox: {e}")),
    }
}

/// Refuses to overwrite a version-2 file; see [`read_outbox`].
pub fn write_outbox(path: &Path, entries: &BTreeMap<String, u64>) -> Result<(), String> {
    match read_state_file(path, MAX_STATE_FILE_BYTES) {
        Ok(b) if outbox_is_versioned(&b) => {
            return Err("deletion outbox is in the scoped format; not overwritten as v1".into());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("read deletion outbox: {e}")),
    }
    let bytes = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
    replace_file_atomically(path, &bytes).map_err(|e| format!("write deletion outbox: {e}"))
}

/// A legacy outbox has only logical-id keys (UUID-shaped), never `v`.
fn outbox_is_versioned(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|value| value.get("v").is_some())
}

// ---------------------------------------------------------------------------
// Consent scope (host-side gate in front of the engine)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsentScopeBinding {
    #[serde(default)]
    pub authorized_scope: Option<String>,
    #[serde(default)]
    pub legacy_scope: Option<String>,
}

/// `account_changed_message` is the error returned when the live account differs from the
/// authorized one; the host supplies the wording it shows to the user.
pub fn authorize_scope(
    authorized: &mut Option<String>,
    legacy: &mut Option<String>,
    acknowledged: Option<&str>,
    actual: &str,
    explicit: bool,
    account_changed_message: &str,
) -> Result<(), String> {
    if actual.is_empty() {
        return Err("cloud sync scope lookup returned an empty scope".to_string());
    }
    if legacy.is_none() {
        *legacy = authorized
            .clone()
            .or_else(|| acknowledged.map(str::to_string))
            .or_else(|| Some(actual.to_string()));
    }
    if explicit {
        *authorized = Some(actual.to_string());
    } else if authorized.is_none() {
        *authorized = Some(acknowledged.unwrap_or(actual).to_string());
    }
    if authorized.as_deref() != Some(actual) {
        return Err(account_changed_message.to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy migration (old tombstone / pending / alias / ack files -> Ledger)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LegacyFiles {
    pub tombstones: PathBuf,
    pub pending: PathBuf,
    pub aliases: PathBuf,
    pub acks: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub tombstones: usize,
    pub pending_tombstones: usize,
    pub alias_ids: usize,
    pub seeded_versions: usize,
    pub dropped_conflicts: usize,
}

#[derive(Deserialize)]
struct LegacyTombstone {
    id: String,
    #[serde(default, alias = "deletedAt")]
    deleted_at_ms: u64,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Deserialize)]
struct LegacyAlias {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Deserialize, Default)]
struct LegacyAckEntry {
    #[serde(default)]
    updated_at_ms: u64,
}

#[derive(Deserialize, Default)]
struct LegacyAcks {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    entries: BTreeMap<String, LegacyAckEntry>,
    #[serde(default)]
    conflicts: BTreeMap<String, serde_json::Value>,
}

fn read_legacy<T: serde::de::DeserializeOwned>(
    path: &Path,
    what: &str,
) -> Result<Option<T>, String> {
    match read_state_file(path, MAX_STATE_FILE_BYTES) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("parse legacy {what}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read legacy {what}: {e}")),
    }
}

/// A migrated legacy ledger, its local-version seeds, and what the migration did.
pub type MigratedLegacy = (Ledger, BTreeMap<String, u64>, MigrationReport);

impl TranscriptSyncProfile {
    /// Convert the legacy files into an engine `Ledger` bound to `scope` (the
    /// consent-authorized account that owned them) plus local-version seeds.
    /// Reads only; the old files are never modified or deleted (they are the
    /// backup). Ack seeds require matching nonempty stored and caller scopes.
    /// Unscoped tombstone/pending/alias files also require that matching ack
    /// scope; scoped files must have this profile's hash-qualified name for the
    /// caller scope. Unproven files are excluded intact, allowing normal sync.
    /// `Ok(None)` means no proven source was available. A corrupt proven
    /// tombstone/pending/alias file remains an error, preserving known deletions.
    /// The cursor and remote versions are deliberately not migrated: the first
    /// pass replays the zone and rebuilds them.
    pub fn migrate_legacy(
        &self,
        files: &LegacyFiles,
        scope: Option<&str>,
    ) -> Result<Option<MigratedLegacy>, String> {
        let scope = scope.filter(|value| !value.is_empty());
        // An unreadable/malformed ack cannot prove ownership of any unscoped source.
        let acks = read_legacy::<LegacyAcks>(&files.acks, "acks")
            .ok()
            .flatten()
            .filter(|acks| scope.is_some() && acks.scope.as_deref() == scope);
        let proven = |path: &Path, base: &str| {
            scope.is_some_and(|scope| {
                let scoped = self.legacy_scoped_file_name(scope, Some(""), base);
                path.file_name().is_some_and(|name| {
                    name == std::ffi::OsStr::new(&scoped)
                        || (acks.is_some() && name == std::ffi::OsStr::new(base))
                })
            })
        };
        let tombs: Option<Vec<LegacyTombstone>> =
            if proven(&files.tombstones, &self.files.legacy_tombstones) {
                read_legacy(&files.tombstones, "tombstones")?
            } else {
                None
            };
        let pending: Option<Vec<LegacyTombstone>> =
            if proven(&files.pending, &self.files.legacy_pending) {
                read_legacy(&files.pending, "pending tombstones")?
            } else {
                None
            };
        let aliases: Option<Vec<LegacyAlias>> =
            if proven(&files.aliases, &self.files.legacy_aliases) {
                read_legacy(&files.aliases, "aliases")?
            } else {
                None
            };
        if tombs.is_none() && pending.is_none() && aliases.is_none() && acks.is_none() {
            return Ok(None);
        }
        let mut ledger = Ledger::default();
        let mut report = MigrationReport::default();
        ledger.scope = scope.map(str::to_string);

        let add_aliases = |ledger: &mut Ledger, id: &str, names: &[String]| {
            let set = ledger.aliases.entry(id.to_string()).or_default();
            for n in names {
                set.insert(n.clone());
            }
        };
        for t in tombs.iter().flatten() {
            let id = self.logical_cloud_id(&t.id);
            let at = to_i64(t.deleted_at_ms);
            let e = ledger.tombstones.entry(id.clone()).or_insert(at);
            *e = (*e).max(at);
            add_aliases(&mut ledger, &id, &t.aliases);
        }
        for t in pending.iter().flatten() {
            let id = self.logical_cloud_id(&t.id);
            let at = to_i64(t.deleted_at_ms);
            let e = ledger.tombstones.entry(id.clone()).or_insert(at);
            *e = (*e).max(at);
            ledger.pending_tombstones.insert(id.clone());
            add_aliases(&mut ledger, &id, &t.aliases);
        }
        for a in aliases.iter().flatten() {
            add_aliases(&mut ledger, &self.logical_cloud_id(&a.id), &a.aliases);
        }
        let mut seeds = BTreeMap::new();
        if let Some(a) = &acks {
            report.dropped_conflicts = a.conflicts.len();
            for (id, entry) in &a.entries {
                if entry.updated_at_ms > 0 && !ledger.tombstones.contains_key(id) {
                    seeds.insert(id.clone(), entry.updated_at_ms);
                }
            }
        }
        report.tombstones = ledger.tombstones.len();
        report.pending_tombstones = ledger.pending_tombstones.len();
        report.alias_ids = ledger.aliases.len();
        report.seeded_versions = seeds.len();
        Ok(Some((ledger, seeds, report)))
    }

    /// Run the migration once: only when `ledger_path` does not exist yet. The
    /// ledger is written LAST, so its existence is the done marker. Existing
    /// version entries are never overwritten. With proven seeds, unreadable or
    /// malformed existing versions refuse migration before destination writes,
    /// preserving the retry path; only NotFound starts empty.
    pub fn migrate_legacy_once(
        &self,
        files: &LegacyFiles,
        scope: Option<&str>,
        ledger_path: &Path,
        versions_path: &Path,
    ) -> Result<Option<MigrationReport>, String> {
        let _ledger_lock = crate::storage::ledger_file_lock(ledger_path)?;
        if ledger_path.exists() {
            return Ok(None);
        }
        let Some((ledger, seeds, mut report)) = self.migrate_legacy(files, scope)? else {
            return Ok(None);
        };
        report.seeded_versions = 0;
        if !seeds.is_empty() {
            let entries = match read_state_file(versions_path, MAX_STATE_FILE_BYTES) {
                Ok(bytes) => serde_json::from_slice::<BTreeMap<String, VersionEntry>>(&bytes)
                    .map_err(|error| format!("parse existing cloud sync versions: {error}"))?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
                Err(error) => return Err(format!("read existing cloud sync versions: {error}")),
            };
            let mut versions = LocalVersions {
                path: versions_path.to_path_buf(),
                entries,
                dirty: false,
            };
            for (id, at) in &seeds {
                if !versions.entries.contains_key(id) {
                    versions.seed(id, *at);
                    report.seeded_versions += 1;
                }
            }
            versions.save_if_dirty()?;
        }
        let bytes = serde_json::to_vec(&ledger).map_err(|e| e.to_string())?;
        replace_file_atomically(ledger_path, &bytes)
            .map_err(|e| format!("write migrated cloud sync ledger: {e}"))?;
        Ok(Some(report))
    }
}

// ---------------------------------------------------------------------------
// CloudKit environment pin
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudKitEnvironment {
    Production,
    Development,
}

const ENTITLEMENT_KEY: &str = "com.apple.developer.icloud-container-environment";

/// Every build syncs against the Production CloudKit environment.
pub fn requested_environment() -> Result<CloudKitEnvironment, String> {
    Ok(CloudKitEnvironment::Production)
}

/// Read the signed environment out of `codesign -d --entitlements :-` XML.
pub fn parse_entitlement_environment(xml: &str) -> Option<CloudKitEnvironment> {
    let after = xml.split(ENTITLEMENT_KEY).nth(1)?;
    let value = after.split("<string>").nth(1)?.split("</string>").next()?;
    match value.trim() {
        "Development" => Some(CloudKitEnvironment::Development),
        "Production" => Some(CloudKitEnvironment::Production),
        _ => None,
    }
}

/// A Development expectation needs a Development-signed helper; Production refuses only a
/// helper explicitly signed Development.
pub fn check_helper_environment(
    expected: CloudKitEnvironment,
    signed: Option<CloudKitEnvironment>,
) -> Result<(), String> {
    match (expected, signed) {
        (CloudKitEnvironment::Development, Some(CloudKitEnvironment::Development)) => Ok(()),
        (CloudKitEnvironment::Development, other) => Err(format!(
            "a Development CloudKit helper is required; this one is signed {other:?}"
        )),
        (CloudKitEnvironment::Production, Some(CloudKitEnvironment::Development)) => {
            Err("release build refuses a CloudKit helper signed for Development".to_string())
        }
        (CloudKitEnvironment::Production, _) => Ok(()),
    }
}

/// State files are per environment so a Development run can never feed the
/// production ledger, cursor or deletion queue.
pub fn env_file_name(base: &str, env: CloudKitEnvironment) -> String {
    match env {
        CloudKitEnvironment::Production => base.to_string(),
        CloudKitEnvironment::Development => match base.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}-dev.{ext}"),
            None => format!("{base}-dev"),
        },
    }
}

/// The main-process Swift observer is signed for Production; it must never
/// run in a Development session.
pub fn native_observer_allowed(env: CloudKitEnvironment) -> bool {
    env == CloudKitEnvironment::Production
}

// ---------------------------------------------------------------------------
// Retry classification
// ---------------------------------------------------------------------------

/// CloudKit's `invalidArguments` is broad, so only the exact server
/// production-schema message is permanent.
pub fn is_production_schema_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("cannot create new type") && error.contains("production schema")
}

pub fn error_is_terminal(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    is_production_schema_error(error)
        || [
            "not authenticated",
            "not_authenticated",
            "missing entitlement",
            "missing_entitlement",
            "permission failure",
            "permission_failure",
            "account unavailable",
            "account changed",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}
