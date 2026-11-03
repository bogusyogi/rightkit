//! Change requests to the owning process: a directory queue for an app whose settings have one
//! writer (the owner) and other processes (a CLI, a hub, a helper) that may ask for a change.
//!
//! A requester calls [`ChangeRequestQueue::submit`], which writes one request file atomically,
//! and may wait for the typed answer with [`ChangeRequestQueue::wait_outcome`]. The owner calls
//! [`ChangeRequestQueue::drain`] on its own wake (a file watcher, a timer, a command): each
//! request is checked against a [`RequestPolicy`] (protected keys refused, undeclared keys
//! refused, expired requests dropped), applied through [`Store::patch`] and answered with a
//! [`RequestOutcome`] file, then removed. The owner stays the only writer of the settings file;
//! the queue only carries intent. Re-applying one request twice sets the same value, so a drain
//! interrupted after the patch and before the removal is safe to repeat.
//!
//! Files in the queue directory: `<id>.request.json` and `<id>.outcome.json`; ids sort by time,
//! so the oldest request is applied first. Swift apps relay requests with
//! `AppGroupCommandQueue` in RightKitSwift; both carry the same meaning.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::manifest::Manifest;
use crate::{atomic_write, Error, Store};

const REQUEST_SUFFIX: &str = ".request.json";
const OUTCOME_SUFFIX: &str = ".outcome.json";
const POLL_EVERY: Duration = Duration::from_millis(50);

/// One request to change one setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangeRequest {
    pub id: String,
    pub key: String,
    /// The new value; `null` resets the key like [`Store::patch`] does.
    pub value: Value,
    /// Unix milliseconds when the request was made.
    pub issued_at_ms: u64,
}

/// The owner's typed answer to one request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequestOutcome {
    /// The value was stored as asked; `revision` is the store's revision after the write.
    Applied { revision: u64 },
    /// A value was stored, but not the one asked for (the store clamped or repaired it).
    Adjusted { value: Value },
    /// Nothing was changed; `reason` says why (protected, undeclared or invalid).
    Refused { reason: String },
    /// The request was older than the policy's lifetime and was dropped unapplied.
    Expired,
}

/// What the owner accepts from a queue.
#[derive(Clone, Debug)]
pub struct RequestPolicy {
    protected: Vec<String>,
    declared: Option<Vec<String>>,
    ttl: Duration,
}

impl Default for RequestPolicy {
    fn default() -> Self {
        Self {
            protected: Vec::new(),
            declared: None,
            ttl: Duration::from_secs(60),
        }
    }
}

impl RequestPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// The policy a [`Manifest`] states: only its keys, and its protected keys refused.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            protected: manifest
                .protected_keys()
                .iter()
                .map(|k| (*k).to_owned())
                .collect(),
            declared: Some(manifest.settings().iter().map(|s| s.key.clone()).collect()),
            ttl: Duration::from_secs(60),
        }
    }

    /// Refuse these keys (they change only through the owner path).
    pub fn protect<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.protected.extend(keys.into_iter().map(Into::into));
        self
    }

    /// Accept only these keys; any other is refused.
    pub fn only<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.declared = Some(keys.into_iter().map(Into::into).collect());
        self
    }

    /// How long a request stays valid after it was made (default 60 s).
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
}

/// A directory of change requests and their outcomes.
#[derive(Clone, Debug)]
pub struct ChangeRequestQueue {
    dir: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn invalid(text: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, text.into())
}

fn single(key: &str, value: &Value) -> Value {
    let mut patch = Map::new();
    patch.insert(key.to_owned(), value.clone());
    Value::Object(patch)
}

fn same_value(asked: &Value, stored: &Value) -> bool {
    match (asked.as_f64(), stored.as_f64()) {
        (Some(a), Some(b)) if asked.is_number() && stored.is_number() => a == b,
        _ => asked == stored,
    }
}

impl ChangeRequestQueue {
    /// The queue in `dir` (created on the first `submit`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn request_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}{REQUEST_SUFFIX}"))
    }

    fn outcome_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}{OUTCOME_SUFFIX}"))
    }

    /// Queue a request and return its id. The file is published atomically, so the owner never
    /// reads half a request.
    pub fn submit(&self, key: &str, value: Value) -> io::Result<String> {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(invalid(
                "a setting key is 1 to 128 characters with no control characters",
            ));
        }
        let issued_at_ms = now_ms();
        let id = format!(
            "{issued_at_ms:016x}-{:08x}-{:04x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed) & 0xffff
        );
        let request = ChangeRequest {
            id: id.clone(),
            key: key.to_owned(),
            value,
            issued_at_ms,
        };
        let bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
        atomic_write(&self.request_path(&id), &bytes)?;
        Ok(id)
    }

    fn ids(&self, suffix: &str) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut ids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter_map(|name| name.strip_suffix(suffix).map(str::to_owned))
            .collect();
        ids.sort();
        Ok(ids)
    }

    /// The readable requests waiting, oldest first. A file that does not parse is skipped here
    /// and answered `Refused` by [`ChangeRequestQueue::drain`].
    pub fn pending(&self) -> io::Result<Vec<ChangeRequest>> {
        let mut out = Vec::new();
        for id in self.ids(REQUEST_SUFFIX)? {
            if let Ok(bytes) = fs::read(self.request_path(&id)) {
                if let Ok(request) = serde_json::from_slice::<ChangeRequest>(&bytes) {
                    out.push(request);
                }
            }
        }
        Ok(out)
    }

    /// The answer to request `id`, once the owner wrote it. The outcome file stays until
    /// [`ChangeRequestQueue::wait_outcome`] takes it or the owner's next drain reaps it.
    pub fn outcome(&self, id: &str) -> io::Result<Option<RequestOutcome>> {
        match fs::read(self.outcome_path(id)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Wait up to `timeout` for the answer to `id` and remove its outcome file. `None` when the
    /// owner has not answered in time (the request stays queued).
    pub fn wait_outcome(&self, id: &str, timeout: Duration) -> io::Result<Option<RequestOutcome>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(outcome) = self.outcome(id)? {
                let _ = fs::remove_file(self.outcome_path(id));
                return Ok(Some(outcome));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL_EVERY);
        }
    }

    fn answer(&self, id: &str, outcome: &RequestOutcome) -> io::Result<()> {
        let bytes = serde_json::to_vec(outcome).map_err(io::Error::other)?;
        atomic_write(&self.outcome_path(id), &bytes)?;
        let _ = fs::remove_file(self.request_path(id));
        Ok(())
    }

    /// The owner's side: apply every waiting request to `store` under `policy`, write each
    /// answer and remove the request. Returns the answers in request order. A store failure
    /// that is not a refusal of the value (a lock timeout, an I/O error) stops the drain and
    /// is returned; that request and the later ones stay queued for the next drain.
    pub fn drain<T>(
        &self,
        store: &Store<T>,
        policy: &RequestPolicy,
    ) -> io::Result<Vec<(String, RequestOutcome)>>
    where
        T: Serialize + DeserializeOwned + Default + Clone,
    {
        let now = now_ms();
        let ttl_ms = u64::try_from(policy.ttl.as_millis()).unwrap_or(u64::MAX);
        let mut answers = Vec::new();
        for id in self.ids(REQUEST_SUFFIX)? {
            let parsed = fs::read(self.request_path(&id))
                .map_err(|e| e.to_string())
                .and_then(|b| {
                    serde_json::from_slice::<ChangeRequest>(&b).map_err(|e| e.to_string())
                });
            let outcome = match parsed {
                Err(why) => RequestOutcome::Refused {
                    reason: format!("unreadable request: {why}"),
                },
                Ok(r) if now.saturating_sub(r.issued_at_ms) > ttl_ms => RequestOutcome::Expired,
                Ok(r) if policy.protected.contains(&r.key) => RequestOutcome::Refused {
                    reason: format!(
                        "`{}` is protected; it changes only through the owner path",
                        r.key
                    ),
                },
                Ok(r)
                    if policy
                        .declared
                        .as_ref()
                        .is_some_and(|d| !d.contains(&r.key)) =>
                {
                    RequestOutcome::Refused {
                        reason: format!("`{}` is not a declared setting", r.key),
                    }
                }
                Ok(r) => match store.patch(&single(&r.key, &r.value), None) {
                    Ok(current) => {
                        let stored = serde_json::to_value(&current)
                            .ok()
                            .and_then(|v| v.get(&r.key).cloned());
                        match stored {
                            Some(v) if !r.value.is_null() && !same_value(&r.value, &v) => {
                                RequestOutcome::Adjusted { value: v }
                            }
                            _ => RequestOutcome::Applied {
                                revision: store.revision(),
                            },
                        }
                    }
                    Err(Error::Invalid(reason)) => RequestOutcome::Refused { reason },
                    Err(e) => return Err(io::Error::other(e)),
                },
            };
            self.answer(&id, &outcome)?;
            answers.push((id, outcome));
        }
        self.reap(ttl_ms.saturating_mul(10));
        Ok(answers)
    }

    /// Remove outcome files nobody collected within `max_age_ms`.
    fn reap(&self, max_age_ms: u64) {
        let Ok(ids) = self.ids(OUTCOME_SUFFIX) else {
            return;
        };
        let max_age = Duration::from_millis(max_age_ms);
        for id in ids {
            let path = self.outcome_path(&id);
            let old = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if old {
                let _ = fs::remove_file(path);
            }
        }
    }
}
