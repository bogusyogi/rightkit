//! Durable per-provider quota scheduling.
//!
//! A provider that answers 429/503 with a long `Retry-After` (minutes or hours) should not
//! be asked again before that time, even after the app restarts. [`QuotaSchedule`] keeps one
//! "not before" wall-clock time per provider key and, when opened on a file, writes every
//! change to it atomically (temp file, flush, rename), so the schedule survives a restart.
//!
//! It never sleeps and never calls the network: the app asks [`QuotaSchedule::remaining`]
//! before dispatching a job and re-queues the job for later when it is `Some`. Pair it with
//! `RetryPolicy::defer_over_cap` so a `Retry-After` longer than the in-call cap comes back
//! to the caller whole instead of being slept for.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::HttpError;
use crate::retry::parse_retry_after_at;

/// Longest wait that is recorded; a larger `Retry-After` is clamped to this (one year).
pub const MAX_DEFER: Duration = Duration::from_secs(365 * 24 * 3600);
/// Most providers kept at once (expired entries are dropped first).
pub const MAX_PROVIDERS: usize = 4096;

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    version: u32,
    /// Provider key -> not-before time, milliseconds since the Unix epoch.
    not_before_ms: BTreeMap<String, u64>,
}

/// Per-provider "do not call before" times, optionally persisted to a file.
#[derive(Debug, Default)]
pub struct QuotaSchedule {
    not_before_ms: BTreeMap<String, u64>,
    path: Option<PathBuf>,
}

fn now_ms() -> u64 {
    system_ms(SystemTime::now())
}

/// Milliseconds since the Unix epoch (0 before it).
fn system_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Storage behind a quota schedule, for an app that keeps the value in its own database. The
/// app implements it; [`QuotaSchedule`] implements it too. Keys are arbitrary non-empty strings
/// (a provider, an account, or a provider and account pair).
pub trait QuotaStore {
    /// The stored not-before time for `provider`, in milliseconds since the Unix epoch.
    fn get_not_before_ms(&self, provider: &str) -> io::Result<Option<u64>>;
    /// Store `not_before_ms` for `provider`, keeping the larger of the stored value and this
    /// one (max-merge: a shorter wait never shortens a longer one).
    fn merge_not_before_ms(&mut self, provider: &str, not_before_ms: u64) -> io::Result<()>;
}

impl QuotaStore for QuotaSchedule {
    fn get_not_before_ms(&self, provider: &str) -> io::Result<Option<u64>> {
        Ok(self.not_before_ms.get(provider).copied())
    }

    fn merge_not_before_ms(&mut self, provider: &str, not_before_ms: u64) -> io::Result<()> {
        if provider.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty provider key",
            ));
        }
        if !self.not_before_ms.contains_key(provider) && self.not_before_ms.len() >= MAX_PROVIDERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many providers in the quota schedule",
            ));
        }
        let slot = self.not_before_ms.entry(provider.to_string()).or_insert(0);
        if not_before_ms > *slot {
            *slot = not_before_ms;
        }
        self.save()
    }
}

/// Record a wait for `key` in `store`, measured from the injected `now`, and return the time it
/// ends. The wait is max-merged with any stored one and clamped to [`MAX_DEFER`].
pub fn defer_in<S: QuotaStore + ?Sized>(
    store: &mut S,
    key: &str,
    wait: Duration,
    now: SystemTime,
) -> io::Result<SystemTime> {
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty provider key",
        ));
    }
    let until = system_ms(now).saturating_add(wait.min(MAX_DEFER).as_millis() as u64);
    let existing = store.get_not_before_ms(key)?.unwrap_or(0);
    let merged = existing.max(until);
    store.merge_not_before_ms(key, merged)?;
    Ok(UNIX_EPOCH + Duration::from_millis(merged))
}

/// Time left before `key` may be called again as seen from the injected `now`, read from `store`.
/// `None` when it is free.
pub fn remaining_in<S: QuotaStore + ?Sized>(
    store: &S,
    key: &str,
    now: SystemTime,
) -> io::Result<Option<Duration>> {
    Ok(store.get_not_before_ms(key)?.and_then(|until| {
        until
            .checked_sub(system_ms(now))
            .filter(|d| *d > 0)
            .map(Duration::from_millis)
    }))
}

impl QuotaSchedule {
    /// An in-memory schedule (lost on exit).
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or start) a schedule stored at `path`. A missing file is an empty schedule;
    /// a file that is not a schedule is `InvalidData`. Expired entries are dropped.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let not_before_ms = match std::fs::read(&path) {
            Ok(bytes) => {
                let stored: Stored = serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                stored.not_before_ms
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let mut s = Self {
            not_before_ms,
            path: Some(path),
        };
        s.prune(now_ms());
        Ok(s)
    }

    /// The file this schedule persists to, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn prune(&mut self, now: u64) {
        self.not_before_ms.retain(|_, until| *until > now);
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let stored = Stored {
            version: 1,
            not_before_ms: self.not_before_ms.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(io::Error::other)?;
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Do not call `provider` for `wait` from now. A shorter wait never shortens an
    /// existing one. Persists when the schedule has a file.
    pub fn defer(&mut self, provider: &str, wait: Duration) -> io::Result<()> {
        self.defer_from(provider, wait, now_ms())
    }

    fn defer_from(&mut self, provider: &str, wait: Duration, now: u64) -> io::Result<()> {
        if provider.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty provider key",
            ));
        }
        self.prune(now);
        if !self.not_before_ms.contains_key(provider) && self.not_before_ms.len() >= MAX_PROVIDERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many providers in the quota schedule",
            ));
        }
        let until = now.saturating_add(wait.min(MAX_DEFER).as_millis() as u64);
        let slot = self.not_before_ms.entry(provider.to_string()).or_insert(0);
        if until > *slot {
            *slot = until;
        }
        self.save()
    }

    /// Like [`QuotaSchedule::defer`], measured from the injected `now`.
    pub fn defer_at(&mut self, provider: &str, wait: Duration, now: SystemTime) -> io::Result<()> {
        self.defer_from(provider, wait, system_ms(now))
    }

    /// Like [`QuotaSchedule::remaining`], measured from the injected `now`.
    pub fn remaining_at(&self, provider: &str, now: SystemTime) -> Option<Duration> {
        self.remaining_ms(provider, system_ms(now))
    }

    /// Like [`QuotaSchedule::defer_for_header`], with the injected `now` as the clock for an
    /// HTTP-date `Retry-After`.
    pub fn defer_for_header_at(
        &mut self,
        provider: &str,
        value: &str,
        now: SystemTime,
    ) -> io::Result<Option<Duration>> {
        match parse_retry_after_at(value, now) {
            Some(wait) => {
                self.defer_at(provider, wait, now)?;
                Ok(Some(wait.min(MAX_DEFER)))
            }
            None => Ok(None),
        }
    }

    /// Record a raw `Retry-After` header value (seconds or HTTP-date). Returns the wait that
    /// was recorded, or `None` when the value does not parse.
    pub fn defer_for_header(
        &mut self,
        provider: &str,
        value: &str,
    ) -> io::Result<Option<Duration>> {
        match parse_retry_after_at(value, SystemTime::now()) {
            Some(wait) => {
                self.defer(provider, wait)?;
                Ok(Some(wait.min(MAX_DEFER)))
            }
            None => Ok(None),
        }
    }

    /// Record the `Retry-After` carried by a rate-limit or status error. Returns the wait
    /// recorded, or `None` for any other error (nothing changes).
    pub fn defer_for_error(
        &mut self,
        provider: &str,
        err: &HttpError,
    ) -> io::Result<Option<Duration>> {
        let wait = match err {
            HttpError::Status {
                retry_after: Some(w),
                ..
            } => *w,
            _ => return Ok(None),
        };
        self.defer(provider, wait)?;
        Ok(Some(wait.min(MAX_DEFER)))
    }

    /// Time left before `provider` may be called again, `None` when it is free now.
    pub fn remaining(&self, provider: &str) -> Option<Duration> {
        self.remaining_ms(provider, now_ms())
    }

    fn remaining_ms(&self, provider: &str, now: u64) -> Option<Duration> {
        let until = *self.not_before_ms.get(provider)?;
        until
            .checked_sub(now)
            .filter(|d| *d > 0)
            .map(Duration::from_millis)
    }

    /// The wall-clock time `provider` becomes callable again, `None` when it is free now.
    pub fn not_before(&self, provider: &str) -> Option<SystemTime> {
        self.remaining(provider).map(|d| SystemTime::now() + d)
    }

    /// Forget the wait for `provider` (for example after a successful call). Persists.
    pub fn clear(&mut self, provider: &str) -> io::Result<()> {
        if self.not_before_ms.remove(provider).is_some() {
            self.prune(now_ms());
            self.save()?;
        }
        Ok(())
    }

    /// Providers with a wait still running, with the time left, soonest first.
    pub fn pending(&self) -> Vec<(String, Duration)> {
        let now = now_ms();
        let mut v: Vec<(String, Duration)> = self
            .not_before_ms
            .iter()
            .filter_map(|(k, until)| {
                until
                    .checked_sub(now)
                    .filter(|d| *d > 0)
                    .map(|d| (k.clone(), Duration::from_millis(d)))
            })
            .collect();
        v.sort_by_key(|(_, d)| *d);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longer_waits_win_and_shorter_never_shorten() {
        let mut q = QuotaSchedule::new();
        q.defer_from("p", Duration::from_secs(3600), 1_000).unwrap();
        q.defer_from("p", Duration::from_secs(10), 1_000).unwrap();
        assert_eq!(q.remaining_ms("p", 1_000), Some(Duration::from_secs(3600)));
        assert_eq!(q.remaining_ms("p", 1_000 + 3_600_000), None);
        assert_eq!(q.remaining_ms("other", 1_000), None);
    }

    #[test]
    fn schedule_survives_a_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-http-quota-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("quota.json");
        {
            let mut q = QuotaSchedule::open(&file).unwrap();
            q.defer("openai", Duration::from_secs(7200)).unwrap();
        }
        let q = QuotaSchedule::open(&file).unwrap();
        let left = q.remaining("openai").unwrap();
        assert!(left > Duration::from_secs(7000) && left <= Duration::from_secs(7200));
        std::fs::write(&file, b"not json").unwrap();
        assert!(QuotaSchedule::open(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn header_and_error_forms_are_recorded() {
        let mut q = QuotaSchedule::new();
        assert_eq!(
            q.defer_for_header("a", "120").unwrap(),
            Some(Duration::from_secs(120))
        );
        assert_eq!(q.defer_for_header("a", "garbage").unwrap(), None);
        let err = HttpError::Status {
            status: 429,
            body: String::new(),
            retry_after: Some(Duration::from_secs(600)),
            attempts: 1,
        };
        assert_eq!(
            q.defer_for_error("b", &err).unwrap(),
            Some(Duration::from_secs(600))
        );
        assert_eq!(q.defer_for_error("c", &HttpError::Cancelled).unwrap(), None);
        assert!(q.remaining("c").is_none());
        q.clear("a").unwrap();
        assert!(q.remaining("a").is_none());
        assert!(q.defer("", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn injected_clock_and_store_are_max_merged() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut q = QuotaSchedule::new();
        q.defer_at("acct-1", Duration::from_secs(60), now).unwrap();
        q.defer_at("acct-1", Duration::from_secs(10), now).unwrap();
        assert_eq!(q.remaining_at("acct-1", now), Some(Duration::from_secs(60)));
        assert_eq!(
            q.remaining_at("acct-1", now + Duration::from_secs(60)),
            None
        );
        let until = defer_in(&mut q, "acct-2", Duration::from_secs(120), now).unwrap();
        assert_eq!(until, now + Duration::from_secs(120));
        assert_eq!(
            remaining_in(&q, "acct-2", now).unwrap(),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            q.defer_for_header_at("acct-3", "Thu, 01 Jan 1970 00:20:00 GMT", now)
                .unwrap(),
            Some(Duration::from_secs(200))
        );
        assert!(defer_in(&mut q, "", Duration::from_secs(1), now).is_err());
    }
}
