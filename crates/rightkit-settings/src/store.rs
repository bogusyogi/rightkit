use crate::atomic::atomic_write;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const ENVELOPE_VERSION: &str = "schemaVersion";
const ENVELOPE_NS: &str = "namespace";
const ENVELOPE_VALUES: &str = "values";

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
        Self { namespace: namespace.into(), version, migrations: Vec::new() }
    }
    pub fn migration(mut self, to: u32, apply: fn(&mut Map<String, Value>)) -> Self {
        self.migrations.push(Migration { to, apply });
        self
    }
    fn validate(&self) -> Result<(), Error> {
        let ok_ns = !self.namespace.is_empty()
            && self.namespace.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if !ok_ns {
            return Err(Error::BadSchema("namespace must be [A-Za-z0-9._-]+"));
        }
        let mut last = 0;
        for m in &self.migrations {
            if m.to <= last || m.to > self.version {
                return Err(Error::BadSchema("migrations must ascend and not exceed version"));
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
        let (value, dirty) = load::<T>(&path, &schema)?;
        let store = Store {
            path,
            schema,
            inner: Mutex::new(Inner { value }),
            listeners: Arc::default(),
            next_id: Mutex::new(0),
        };
        if dirty {
            let v = lock(&store.inner).value.clone();
            store.persist(&v)?;
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self) -> T {
        lock(&self.inner).value.clone()
    }

    /// Mutate through a closure. Saved atomically; on any failure the in-memory
    /// value is unchanged. Subscribers fire after the save, outside the lock.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> Result<R, Error>) -> Result<R, Error> {
        let (out, change) = {
            let mut g = lock(&self.inner);
            let mut next = g.value.clone();
            let out = f(&mut next)?;
            let change = diff(&to_object(&g.value)?, &to_object(&next)?);
            self.persist(&next)?;
            g.value = next;
            (out, change)
        };
        self.emit(change);
        Ok(out)
    }

    /// Merge-patch from untrusted input. Only keys that exist in the typed
    /// shape and deserialize to it are accepted; otherwise nothing changes.
    /// `allow` (if given) restricts patchable keys. `null` resets a key to its default.
    pub fn patch(&self, patch: &Value, allow: Option<&[&str]>) -> Result<T, Error> {
        let obj = patch.as_object().ok_or_else(|| Error::Invalid("patch must be an object".into()))?;
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
            *v = serde_json::from_value(Value::Object(cur)).map_err(|e| Error::Invalid(e.to_string()))?;
            Ok(())
        })?;
        Ok(self.get())
    }

    pub fn subscribe(&self, f: impl Fn(&Change) + Send + Sync + 'static) -> Subscription {
        let mut n = lock(&self.next_id);
        *n += 1;
        lock(&self.listeners).push((*n, Arc::new(f)));
        Subscription { id: *n, listeners: self.listeners.clone() }
    }

    fn emit(&self, change: Change) {
        if change.keys.is_empty() {
            return;
        }
        let ls: Vec<Listener> = lock(&self.listeners).iter().map(|(_, l)| l.clone()).collect();
        for l in ls {
            l(&change);
        }
    }

    fn persist(&self, v: &T) -> Result<(), Error> {
        if self.path.exists() {
            // Keep the last good file; best-effort.
            let _ = fs::copy(&self.path, bak_path(&self.path));
        }
        let doc = envelope(&self.schema, to_object(v)?);
        atomic_write(&self.path, &serde_json::to_vec_pretty(&doc)?)?;
        Ok(())
    }
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
        _ => Err(Error::BadSchema("settings type must serialize to an object")),
    }
}

fn diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Change {
    let mut keys: Vec<String> = new.iter().filter(|(k, v)| old.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect();
    keys.extend(old.keys().filter(|k| !new.contains_key(*k)).cloned());
    keys.sort();
    Change { keys }
}

enum Read {
    Ok { version: u32, values: Map<String, Value> },
    Missing,
    Corrupt,
}

fn read_doc(path: &Path) -> Read {
    let Ok(bytes) = fs::read(path) else { return Read::Missing };
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&bytes) else { return Read::Corrupt };
    let version = doc.get(ENVELOPE_VERSION).and_then(Value::as_u64);
    match (version, doc.remove(ENVELOPE_VALUES)) {
        (Some(v), Some(Value::Object(values))) => Read::Ok { version: v as u32, values },
        (Some(_), _) => Read::Corrupt,
        // Legacy flat file: the whole document is version-0 values.
        (None, _) => Read::Ok { version: 0, values: doc },
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

/// Returns (value, needs_persist).
fn load<T: Serialize + DeserializeOwned + Default>(path: &Path, schema: &Schema) -> Result<(T, bool), Error> {
    let (version, values, mut dirty) = match read_doc(path) {
        Read::Ok { version, values } => (version, values, false),
        Read::Missing => match read_doc(&bak_path(path)) {
            Read::Ok { version, values } => (version, values, true),
            _ => (schema.version, Map::new(), true),
        },
        Read::Corrupt => {
            quarantine(path);
            match read_doc(&bak_path(path)) {
                Read::Ok { version, values } => (version, values, true),
                _ => (schema.version, Map::new(), true),
            }
        }
    };
    if version > schema.version {
        return Err(Error::NewerSchema { found: version, supported: schema.version });
    }
    let mut values = values;
    if version < schema.version {
        for m in schema.migrations.iter().filter(|m| m.to > version) {
            (m.apply)(&mut values);
        }
        dirty = true;
    }
    let (value, repaired) = repair::<T>(values)?;
    Ok((value, dirty || repaired))
}

/// Deserialize, dropping individual invalid or unknown-typed fields back to
/// their defaults instead of resetting everything.
fn repair<T: Serialize + DeserializeOwned + Default>(values: Map<String, Value>) -> Result<(T, bool), Error> {
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
    Ok((serde_json::from_value(Value::Object(good)).unwrap_or_default(), true))
}
