//! PTY-002 durable lease state: execution identity, `Running | Exited |
//! Unknown` execution state, and a store that lets an [`InputLease`] survive a
//! host restart or crash without its epoch going backwards or its
//! unacknowledged input being silently forgotten.
//!
//! Contract (CodeRight canon PTY-002): "Each execution has one durable
//! CodeRight identity and typed `Running | Exited | Unknown` state ... Host
//! crash/restart reports the real resulting state (`Exited`/`Unknown`/
//! recovered-by-explicit-policy) rather than fabricating continuity."
//!
//! [`InputLease::open`] recovers from a [`LeaseStore`]:
//! - the epoch is restored and then bumped once (a recovery fence), so every
//!   pre-restart holder is stale and no epoch is ever granted twice;
//! - the lease comes back detached (the holder's connection did not survive);
//! - a `Running` execution comes back `Unknown` (the host cannot prove the
//!   target survived); `Exited` stays `Exited`;
//! - input that was unacknowledged at the crash comes back as `Unknown`
//!   input and blocks new input until it is resolved explicitly
//!   ([`InputLease::acknowledge_input`], [`InputLease::reconcile`] or
//!   [`InputLease::acquire_reconciling`]).
//!
//! A store that cannot be read, or holds a corrupt record, fails closed: the
//! lease does not open, and nothing resets to epoch 0.
//!
//! [`InputLease`]: crate::control::lease::InputLease
//! [`InputLease::open`]: crate::control::lease::InputLease::open
//! [`InputLease::acknowledge_input`]: crate::control::lease::InputLease::acknowledge_input
//! [`InputLease::reconcile`]: crate::control::lease::InputLease::reconcile
//! [`InputLease::acquire_reconciling`]: crate::control::lease::InputLease::acquire_reconciling
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::control::admission::lock;

/// Typed execution state (CodeRight `Running | Exited | Unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionState {
    /// The target is live and accepts input.
    Running,
    /// The target ended. Terminal: input is refused for this identity.
    Exited,
    /// The host restarted and cannot prove the target survived. Input is
    /// refused until the host resolves it with
    /// [`crate::control::lease::InputLease::set_execution_state`].
    Unknown,
}

impl ExecutionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// Durable identity of one controlled execution. Generated once and kept
/// across every restart of the host that recovers it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutionIdentity(String);

impl ExecutionIdentity {
    /// A fresh 128-bit random identity (32 lowercase hex characters).
    pub fn generate() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let word = |salt: u64| {
            let mut h = RandomState::new().build_hasher();
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            h.write_u64(n);
            h.write_u64(salt);
            h.finish()
        };
        Self(format!("{:016x}{:016x}", word(1), word(2)))
    }
    /// A caller-chosen identity: 1–128 characters of `[A-Za-z0-9._:-]`.
    pub fn new(id: impl Into<String>) -> Result<Self, StoreError> {
        let id = id.into();
        let ok = !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
        if ok {
            Ok(Self(id))
        } else {
            Err(StoreError::Invalid(format!(
                "execution identity {id:?} must be 1-128 characters of [A-Za-z0-9._:-]"
            )))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExecutionIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The durable projection of one lease. Content-free: no input payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    pub name: String,
    pub execution_id: ExecutionIdentity,
    pub execution_state: ExecutionState,
    /// Current epoch; never decreases across saves or restarts.
    pub epoch: u64,
    /// Epoch of the attached holder, `None` when detached.
    pub holder_epoch: Option<u64>,
    pub next_input_sequence: u64,
    /// Exclusive: every sequence below this is acknowledged.
    pub acked_input_sequence: u64,
    /// `Some((from, to))` when `[from, to)` was unacknowledged at a host
    /// crash/restart and has not been resolved since. Always equals
    /// `(acked_input_sequence, next_input_sequence)` when present.
    pub unknown_input: Option<(u64, u64)>,
}

impl LeaseRecord {
    fn validate(&self) -> Result<(), StoreError> {
        let bad = |m: &str| Err(StoreError::Corrupt(m.into()));
        if self.name.contains(['\n', '\r']) {
            return bad("lease name contains a line break");
        }
        if self.acked_input_sequence > self.next_input_sequence {
            return bad("acknowledged sequence is ahead of the next sequence");
        }
        if let Some(h) = self.holder_epoch {
            if h != self.epoch || h == 0 {
                return bad("holder epoch does not match the current epoch");
            }
        }
        if let Some(r) = self.unknown_input {
            if r != (self.acked_input_sequence, self.next_input_sequence) || r.0 >= r.1 {
                return bad("unknown input range does not match the unacknowledged range");
            }
        }
        Ok(())
    }
}

/// Why a store could not load or save a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Io(String),
    /// The committed record is unreadable, torn or inconsistent.
    Corrupt(String),
    /// The record belongs to another lease, or a value is not storable.
    Invalid(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "lease store i/o: {m}"),
            Self::Corrupt(m) => write!(f, "lease store record is corrupt: {m}"),
            Self::Invalid(m) => write!(f, "lease store: {m}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Durable backing for one [`crate::control::lease::InputLease`]. `save` must be
/// atomic: after a crash, `load` returns either the previous or the new
/// record, never a mix. The lease serializes calls to one store.
pub trait LeaseStore: Send + Sync {
    /// `Ok(None)` when nothing was ever saved.
    fn load(&self) -> Result<Option<LeaseRecord>, StoreError>;
    fn save(&self, record: &LeaseRecord) -> Result<(), StoreError>;
}

/// Process-local store, for hosts that persist elsewhere and for tests.
#[derive(Debug, Default)]
pub struct MemoryLeaseStore(Mutex<Option<LeaseRecord>>);

impl MemoryLeaseStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record(&self) -> Option<LeaseRecord> {
        lock(&self.0).clone()
    }
}

impl LeaseStore for MemoryLeaseStore {
    fn load(&self) -> Result<Option<LeaseRecord>, StoreError> {
        Ok(lock(&self.0).clone())
    }
    fn save(&self, record: &LeaseRecord) -> Result<(), StoreError> {
        record.validate()?;
        *lock(&self.0) = Some(record.clone());
        Ok(())
    }
}

const HEADER: &str = "rightkit-control-lease 1";

/// One lease record per file, replaced atomically: the record is written to
/// `<path>.tmp`, fsynced, renamed over `<path>`, and the directory is fsynced
/// (Unix). `load` reads only `<path>`, so a torn temp file left by a crash is
/// ignored and overwritten by the next save. The file carries a checksum; a
/// committed file that fails it is reported as [`StoreError::Corrupt`].
///
/// One process should own a given file; the store does not lock across
/// processes.
#[derive(Debug, Clone)]
pub struct FileLeaseStore {
    path: PathBuf,
}

impl FileLeaseStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Where in-progress writes go before the atomic rename.
    pub fn temp_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(".tmp");
        PathBuf::from(p)
    }
}

impl LeaseStore for FileLeaseStore {
    fn load(&self) -> Result<Option<LeaseRecord>, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => decode(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                Err(StoreError::Corrupt("record is not UTF-8".into()))
            }
            Err(e) => Err(StoreError::Io(format!("{}: {e}", self.path.display()))),
        }
    }

    fn save(&self, record: &LeaseRecord) -> Result<(), StoreError> {
        record.validate().map_err(|e| match e {
            StoreError::Corrupt(m) => StoreError::Invalid(m),
            other => other,
        })?;
        let body = encode(record);
        let tmp = self.temp_path();
        let io = |what: &str, e: std::io::Error| StoreError::Io(format!("{what}: {e}"));
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| io("create lease directory", e))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| io("open temp record", e))?;
        f.write_all(body.as_bytes())
            .map_err(|e| io("write temp record", e))?;
        f.sync_all().map_err(|e| io("fsync temp record", e))?;
        drop(f);
        std::fs::rename(&tmp, &self.path).map_err(|e| io("rename temp record", e))?;
        #[cfg(unix)]
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(|e| io("fsync lease directory", e))?;
        }
        Ok(())
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn opt(v: Option<u64>) -> String {
    v.map_or_else(|| "-".into(), |n| n.to_string())
}

fn encode(r: &LeaseRecord) -> String {
    let unknown = r
        .unknown_input
        .map_or_else(|| "-".into(), |(a, b)| format!("{a}..{b}"));
    let body = format!(
        "{HEADER}\nname={}\nexecution_id={}\nexecution_state={}\nepoch={}\nholder_epoch={}\nnext_input_sequence={}\nacked_input_sequence={}\nunknown_input={unknown}\n",
        r.name,
        r.execution_id,
        r.execution_state.label(),
        r.epoch,
        opt(r.holder_epoch),
        r.next_input_sequence,
        r.acked_input_sequence,
    );
    let sum = fnv1a(&body);
    format!("{body}checksum={sum:016x}\n")
}

fn decode(text: &str) -> Result<LeaseRecord, StoreError> {
    let corrupt = |m: &str| StoreError::Corrupt(m.to_string());
    let split = text
        .rfind("checksum=")
        .ok_or_else(|| corrupt("missing checksum"))?;
    let (body, tail) = text.split_at(split);
    let sum = tail
        .strip_prefix("checksum=")
        .and_then(|t| t.strip_suffix('\n'))
        .and_then(|t| u64::from_str_radix(t, 16).ok())
        .ok_or_else(|| corrupt("malformed checksum"))?;
    if sum != fnv1a(body) {
        return Err(corrupt("checksum mismatch"));
    }
    let mut lines = body.lines();
    if lines.next() != Some(HEADER) {
        return Err(corrupt("unknown header or version"));
    }
    let mut field = |key: &str| -> Result<String, StoreError> {
        lines
            .next()
            .and_then(|l| l.strip_prefix(key))
            .and_then(|l| l.strip_prefix('='))
            .map(str::to_string)
            .ok_or_else(|| StoreError::Corrupt(format!("missing field {key}")))
    };
    let num = |v: String, key: &str| {
        v.parse::<u64>()
            .map_err(|_| StoreError::Corrupt(format!("field {key} is not a number")))
    };
    let opt_num = |v: String, key: &str| {
        if v == "-" {
            Ok(None)
        } else {
            num(v, key).map(Some)
        }
    };
    let name = field("name")?;
    let execution_id = ExecutionIdentity::new(field("execution_id")?)
        .map_err(|_| corrupt("invalid execution identity"))?;
    let execution_state = ExecutionState::parse(&field("execution_state")?)
        .ok_or_else(|| corrupt("invalid execution state"))?;
    let epoch = num(field("epoch")?, "epoch")?;
    let holder_epoch = opt_num(field("holder_epoch")?, "holder_epoch")?;
    let next_input_sequence = num(field("next_input_sequence")?, "next_input_sequence")?;
    let acked_input_sequence = num(field("acked_input_sequence")?, "acked_input_sequence")?;
    let unknown = field("unknown_input")?;
    let unknown_input = if unknown == "-" {
        None
    } else {
        let (a, b) = unknown
            .split_once("..")
            .ok_or_else(|| corrupt("malformed unknown input range"))?;
        Some((
            num(a.into(), "unknown_input")?,
            num(b.into(), "unknown_input")?,
        ))
    };
    if lines.next().is_some() {
        return Err(corrupt("unexpected trailing fields"));
    }
    let record = LeaseRecord {
        name,
        execution_id,
        execution_state,
        epoch,
        holder_epoch,
        next_input_sequence,
        acked_input_sequence,
        unknown_input,
    };
    record.validate()?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LeaseRecord {
        LeaseRecord {
            name: "pty:1".into(),
            execution_id: ExecutionIdentity::generate(),
            execution_state: ExecutionState::Running,
            epoch: 3,
            holder_epoch: Some(3),
            next_input_sequence: 7,
            acked_input_sequence: 5,
            unknown_input: Some((5, 7)),
        }
    }

    #[test]
    fn round_trip_and_checksum() {
        let r = sample();
        let text = encode(&r);
        assert_eq!(decode(&text).unwrap(), r);
        let tampered = text.replace("epoch=3\n", "epoch=1\n");
        assert!(matches!(decode(&tampered), Err(StoreError::Corrupt(_))));
        let truncated = &text[..text.len() / 2];
        assert!(matches!(decode(truncated), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn identities_are_unique_and_validated() {
        assert_ne!(ExecutionIdentity::generate(), ExecutionIdentity::generate());
        assert_eq!(ExecutionIdentity::generate().as_str().len(), 32);
        assert!(ExecutionIdentity::new("job:42-a").is_ok());
        assert!(ExecutionIdentity::new("bad id\n").is_err());
        assert!(ExecutionIdentity::new("").is_err());
    }
}
