use crate::atomic::atomic_write;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const ENVELOPE_VERSION: &str = "schemaVersion";
const ENVELOPE_NS: &str = "namespace";
const ENVELOPE_VALUES: &str = "values";
const ENVELOPE_REVISION: &str = "revision";
const LOCK_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("settings io: {0}")]
    Io(#[from] io::Error),
    #[error("settings serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings file is schema v{found}, this build supports up to v{supported}")]
    NewerSchema { found: u32, supported: u32 },
    #[error("invalid settings value: {0}")]
    Invalid(String),
    #[error("invalid schema: {0}")]
    BadSchema(&'static str),
    #[error("settings changed since revision {expected}; now at revision {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("timed out waiting for the settings lock at {0}")]
    LockTimeout(String),
}

/// One forward transform over the raw settings object, bringing it to `to`.
#[derive(Clone, Copy)]
pub struct Migration {
    pub to: u32,
    pub apply: fn(&mut Map<String, Value>),
}

/// Per-app settings identity: namespace, current version, ordered migrations.
#[derive(Clone)]
pub struct Schema {
    pub namespace: String,
    pub version: u32,
    pub migrations: Vec<Migration>,
}

impl Schema {
    pub fn new(namespace: impl Into<String>, version: u32) -> Self {
        Self {
            namespace: namespace.into(),
            version,
            migrations: Vec::new(),
        }
    }
    pub fn migration(mut self, to: u32, apply: fn(&mut Map<String, Value>)) -> Self {
        self.migrations.push(Migration { to, apply });
        self
    }
    fn validate(&self) -> Result<(), Error> {
        let ok_ns = !self.namespace.is_empty()
            && self
                .namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if !ok_ns {
            return Err(Error::BadSchema("namespace must be [A-Za-z0-9._-]+"));
        }
        let mut last = 0;
        for m in &self.migrations {
            if m.to <= last || m.to > self.version {
                return Err(Error::BadSchema(
                    "migrations must ascend and not exceed version",
                ));
            }
            last = m.to;
        }
        Ok(())
    }
}

/// Top-level keys whose value changed in one committed update.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub keys: Vec<String>,
}

type Listener = Arc<dyn Fn(&Change) + Send + Sync>;
type Listeners = Arc<Mutex<Vec<(u64, Listener)>>>;

/// Unsubscribes on drop.
pub struct Subscription {
    id: u64,
    listeners: Listeners,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.listeners).retain(|(id, _)| *id != self.id);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

struct Inner<T> {
    value: T,
    revision: u64,
}

/// Exclusive cross-process transaction guard (OS advisory lock on a sidecar
/// file; released when dropped or when the process dies).
struct FileGuard(File);

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn lock_file(settings: &Path) -> Result<FileGuard, Error> {
    let path = lock_path(settings);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileGuard(file)),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= LOCK_WAIT {
                    return Err(Error::LockTimeout(path.display().to_string()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

pub struct Store<T> {
    path: PathBuf,
    schema: Schema,
    inner: Mutex<Inner<T>>,
    listeners: Listeners,
    next_id: Mutex<u64>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone> Store<T> {
    /// Open `<root>/<namespace>/settings.json`, migrating, repairing, or
    /// recovering as needed. Persists once if migration or recovery changed it.
    pub fn open(root: &Path, schema: Schema) -> Result<Self, Error> {
        schema.validate()?;
        let path = root.join(&schema.namespace).join("settings.json");
        let _file = lock_file(&path)?;
        let loaded = load::<T>(&path, &schema)?;
        let mut revision = loaded.revision;
        if loaded.dirty {
            revision += 1;
            persist(&path, &schema, &loaded.value, revision)?;
        }
        Ok(Store {
            path,
            schema,
            inner: Mutex::new(Inner {
                value: loaded.value,
                revision,
            }),
            listeners: Arc::default(),
            next_id: Mutex::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current settings. Reads the file, so changes committed by another
    /// store or process on the same file are visible; falls back to the last
    /// known value if the file cannot be read right now.
    pub fn get(&self) -> T {
        let mut g = lock(&self.inner);
        if let Some(l) = self.peek() {
            g.value = l.value;
            g.revision = l.revision;
        }
        g.value.clone()
    }

    /// Monotonic revision of the on-disk settings (bumped by every committed
    /// write from any process). Use with [`Store::update_at`].
    pub fn revision(&self) -> u64 {
        let _ = self.get();
        lock(&self.inner).revision
    }

    /// Lock-free read of the committed file (writers replace it atomically).
    fn peek(&self) -> Option<Loaded<T>> {
        match read_doc(&self.path) {
            Read::Ok {
                version,
                values,
                revision,
            } if version == self.schema.version => {
                let (value, _) = repair::<T>(values).ok()?;
                Some(Loaded {
                    value,
                    dirty: false,
                    revision,
                })
            }
            _ => None,
        }
    }

    /// Mutate through a closure. This is a cross-process transaction: take the
    /// file lock, reload the committed settings, apply `f` to that fresh value,
    /// save atomically, release. Concurrent updates to different keys from
    /// other stores or processes are therefore never lost. On any failure the
    /// in-memory value is unchanged. Subscribers fire after the save, outside the lock.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> Result<R, Error>) -> Result<R, Error> {
        self.transact(None, f)
    }

    /// Compare-and-swap form of [`Store::update`]: fails with
    /// [`Error::Conflict`] unless the committed revision still equals `expected`
    /// (from [`Store::revision`]); nothing is written on conflict.
    pub fn update_at<R>(
        &self,
        expected: u64,
        f: impl FnOnce(&mut T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.transact(Some(expected), f)
    }

    fn transact<R>(
        &self,
        expected: Option<u64>,
        f: impl FnOnce(&mut T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let (out, change) = {
            let mut g = lock(&self.inner);
            let _file = lock_file(&self.path)?;
            let loaded = load::<T>(&self.path, &self.schema)?;
            if let Some(expected) = expected {
                if loaded.revision != expected {
                    return Err(Error::Conflict {
                        expected,
                        actual: loaded.revision,
                    });
                }
            }
            let mut next = loaded.value.clone();
            let out = f(&mut next)?;
            let before = to_object(&loaded.value)?;
            let after = to_object(&next)?;
            let change = diff(&before, &after);
            let mut revision = loaded.revision;
            if loaded.dirty || !change.keys.is_empty() {
                revision += 1;
                persist(&self.path, &self.schema, &next, revision)?;
            }
            // `change` is the delta this update committed on top of the fresh read.
            g.value = next;
            g.revision = revision;
            (out, change)
        };
        self.emit(change);
        Ok(out)
    }

    pub fn patch(&self, patch: &Value, allow: Option<&[&str]>) -> Result<T, Error> {
        let obj = patch
            .as_object()
            .ok_or_else(|| Error::Invalid("patch must be an object".into()))?;
        let defaults = to_object(&T::default())?;
        for k in obj.keys() {
            if !defaults.contains_key(k) {
                return Err(Error::Invalid(format!("unknown key `{k}`")));
            }
            if allow.is_some_and(|a| !a.contains(&k.as_str())) {
                return Err(Error::Invalid(format!("key `{k}` is not patchable")));
            }
        }
        self.update(|v| {
            let mut cur = to_object(v)?;
            for (k, nv) in obj {
                if nv.is_null() {
                    match defaults.get(k) {
                        Some(d) => cur.insert(k.clone(), d.clone()),
                        None => cur.remove(k),
                    };
                } else {
                    cur.insert(k.clone(), nv.clone());
                }
            }
            *v = serde_json::from_value(Value::Object(cur))
                .map_err(|e| Error::Invalid(e.to_string()))?;
            Ok(())
        })?;
        Ok(self.get())
    }

    pub fn subscribe(&self, f: impl Fn(&Change) + Send + Sync + 'static) -> Subscription {
        let mut n = lock(&self.next_id);
        *n += 1;
        lock(&self.listeners).push((*n, Arc::new(f)));
        Subscription {
            id: *n,
            listeners: self.listeners.clone(),
        }
    }

    fn emit(&self, change: Change) {
        if change.keys.is_empty() {
            return;
        }
        let ls: Vec<Listener> = lock(&self.listeners)
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in ls {
            l(&change);
        }
    }
}

fn persist<T: Serialize>(path: &Path, schema: &Schema, v: &T, revision: u64) -> Result<(), Error> {
    if path.exists() {
        // Keep the last good file; best-effort.
        let _ = fs::copy(path, bak_path(path));
    }
    let mut doc = envelope(schema, to_object(v)?);
    if let Value::Object(m) = &mut doc {
        m.insert(ENVELOPE_REVISION.into(), revision.into());
    }
    atomic_write(path, &serde_json::to_vec_pretty(&doc)?)?;
    Ok(())
}

struct Loaded<T> {
    value: T,
    dirty: bool,
    revision: u64,
}

fn bak_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

fn envelope(schema: &Schema, values: Map<String, Value>) -> Value {
    let mut m = Map::new();
    m.insert(ENVELOPE_VERSION.into(), schema.version.into());
    m.insert(ENVELOPE_NS.into(), schema.namespace.clone().into());
    m.insert(ENVELOPE_VALUES.into(), Value::Object(values));
    Value::Object(m)
}

fn to_object<T: Serialize>(v: &T) -> Result<Map<String, Value>, Error> {
    match serde_json::to_value(v)? {
        Value::Object(m) => Ok(m),
        _ => Err(Error::BadSchema(
            "settings type must serialize to an object",
        )),
    }
}

fn diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Change {
    let mut keys: Vec<String> = new
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    keys.extend(old.keys().filter(|k| !new.contains_key(*k)).cloned());
    keys.sort();
    Change { keys }
}

enum Read {
    Ok {
        version: u32,
        values: Map<String, Value>,
        revision: u64,
    },
    Missing,
    Corrupt,
}

fn read_doc(path: &Path) -> Read {
    let Ok(bytes) = fs::read(path) else {
        return Read::Missing;
    };
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&bytes) else {
        return Read::Corrupt;
    };
    let version = doc.get(ENVELOPE_VERSION).and_then(Value::as_u64);
    let revision = doc
        .get(ENVELOPE_REVISION)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    match (version, doc.remove(ENVELOPE_VALUES)) {
        (Some(v), Some(Value::Object(values))) => Read::Ok {
            version: v as u32,
            values,
            revision,
        },
        (Some(_), _) => Read::Corrupt,
        // Legacy flat file: the whole document is version-0 values.
        (None, _) => Read::Ok {
            version: 0,
            values: doc,
            revision: 0,
        },
    }
}

fn quarantine(path: &Path) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".corrupt-{ts}"));
    let _ = fs::rename(path, PathBuf::from(s));
}

/// Read, recover, migrate and repair the committed settings. Persists nothing;
/// callers holding the file lock write when `dirty`.
fn load<T: Serialize + DeserializeOwned + Default>(
    path: &Path,
    schema: &Schema,
) -> Result<Loaded<T>, Error> {
    let (version, values, mut dirty, revision) = match read_doc(path) {
        Read::Ok {
            version,
            values,
            revision,
        } => (version, values, false, revision),
        Read::Missing => match read_doc(&bak_path(path)) {
            Read::Ok {
                version,
                values,
                revision,
            } => (version, values, true, revision),
            _ => (schema.version, Map::new(), true, 0),
        },
        Read::Corrupt => {
            quarantine(path);
            match read_doc(&bak_path(path)) {
                Read::Ok {
                    version,
                    values,
                    revision,
                } => (version, values, true, revision),
                _ => (schema.version, Map::new(), true, 0),
            }
        }
    };
    if version > schema.version {
        return Err(Error::NewerSchema {
            found: version,
            supported: schema.version,
        });
    }
    let mut values = values;
    if version < schema.version {
        for m in schema.migrations.iter().filter(|m| m.to > version) {
            (m.apply)(&mut values);
        }
        dirty = true;
    }
    let (value, repaired) = repair::<T>(values)?;
    Ok(Loaded {
        value,
        dirty: dirty || repaired,
        revision,
    })
}

/// Deserialize, dropping individual invalid or unknown-typed fields back to
/// their defaults instead of resetting everything.
fn repair<T: Serialize + DeserializeOwned + Default>(
    values: Map<String, Value>,
) -> Result<(T, bool), Error> {
    if let Ok(v) = serde_json::from_value::<T>(Value::Object(values.clone())) {
        return Ok((v, false));
    }
    let mut good = to_object(&T::default())?;
    for (k, v) in values {
        let prev = good.insert(k.clone(), v);
        if serde_json::from_value::<T>(Value::Object(good.clone())).is_err() {
            match prev {
                Some(p) => good.insert(k, p),
                None => good.remove(&k),
            };
        }
    }
    Ok((
        serde_json::from_value(Value::Object(good)).unwrap_or_default(),
        true,
    ))
}
