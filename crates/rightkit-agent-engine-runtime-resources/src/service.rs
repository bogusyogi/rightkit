use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lifetime boundary for a shared service. A daemon-wide service must be
/// explicitly selected; session scope is the safe default for stateful work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServiceScope {
    Daemon,
    Workspace(String),
    Session(String),
}

/// Stable identity for a shareable resource. `fingerprint` must include every
/// behavior-affecting config input, never credentials in plain text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceKey {
    pub kind: String,
    pub scope: ServiceScope,
    pub fingerprint: String,
}

impl ServiceKey {
    pub fn new(
        kind: impl Into<String>,
        scope: ServiceScope,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            scope,
            fingerprint: fingerprint.into(),
        }
    }

    /// A deterministic non-secret diagnostic id for logs and snapshots.
    pub fn diagnostic_id(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSnapshot {
    pub key: ServiceKey,
    pub leases: usize,
    pub generation: u64,
    pub idle_for: Duration,
}

struct Entry<T> {
    value: Arc<T>,
    leases: AtomicUsize,
    generation: AtomicU64,
    last_used: Mutex<Instant>,
}

impl<T> Entry<T> {
    fn snapshot(&self, key: ServiceKey) -> ResourceSnapshot {
        let last_used = *self
            .last_used
            .lock()
            .expect("service last-used lock poisoned");
        ResourceSnapshot {
            key,
            leases: self.leases.load(Ordering::Acquire),
            generation: self.generation.load(Ordering::Acquire),
            idle_for: last_used.elapsed(),
        }
    }
}

/// Refcounted access to a service. Dropping every lease makes an entry eligible
/// for idle eviction; it never destroys a service with an active caller.
pub struct Lease<T> {
    entry: Arc<Entry<T>>,
}

impl<T> Lease<T> {
    pub fn value(&self) -> Arc<T> {
        Arc::clone(&self.entry.value)
    }

    pub fn generation(&self) -> u64 {
        self.entry.generation.load(Ordering::Acquire)
    }
}

impl<T> Clone for Lease<T> {
    fn clone(&self) -> Self {
        self.entry.leases.fetch_add(1, Ordering::AcqRel);
        Self {
            entry: Arc::clone(&self.entry),
        }
    }
}

impl<T> Drop for Lease<T> {
    fn drop(&mut self) {
        let prior = self.entry.leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prior > 0, "service lease underflow");
        *self
            .entry
            .last_used
            .lock()
            .expect("service last-used lock poisoned") = Instant::now();
    }
}

/// One registry per service type. Construction happens while the map is locked,
/// giving matching callers a single-flight result instead of duplicate process
/// launches or cache generations.
pub struct SharedServiceRegistry<T> {
    entries: Mutex<HashMap<ServiceKey, Arc<Entry<T>>>>,
    max_instances: usize,
}

impl<T> SharedServiceRegistry<T> {
    pub fn new(max_instances: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_instances,
        }
    }

    pub fn acquire(
        &self,
        key: ServiceKey,
        create: impl FnOnce() -> T,
    ) -> Result<Lease<T>, ResourceSnapshot> {
        let mut entries = self.entries.lock().expect("service registry lock poisoned");
        if let Some(entry) = entries.get(&key) {
            entry.leases.fetch_add(1, Ordering::AcqRel);
            *entry
                .last_used
                .lock()
                .expect("service last-used lock poisoned") = Instant::now();
            return Ok(Lease {
                entry: Arc::clone(entry),
            });
        }
        if entries.len() >= self.max_instances {
            let snapshot = entries
                .iter()
                .next()
                .map(|(entry_key, entry)| entry.snapshot(entry_key.clone()))
                .expect("nonzero max check requires a registry entry");
            return Err(snapshot);
        }
        let entry = Arc::new(Entry {
            value: Arc::new(create()),
            leases: AtomicUsize::new(1),
            generation: AtomicU64::new(1),
            last_used: Mutex::new(Instant::now()),
        });
        entries.insert(key, Arc::clone(&entry));
        Ok(Lease { entry })
    }

    pub fn snapshot(&self) -> Vec<ResourceSnapshot> {
        self.entries
            .lock()
            .expect("service registry lock poisoned")
            .iter()
            .map(|(key, entry)| entry.snapshot(key.clone()))
            .collect()
    }

    /// Current `generation` for `key`, or `None` if the entry is absent (evicted
    /// or never created). Used by the MCP shared single-flight pool to detect
    /// drift between the cached transport and the live lease.
    pub fn current_epoch(&self, key: &ServiceKey) -> Option<u64> {
        self.entries
            .lock()
            .expect("service registry lock poisoned")
            .get(key)
            .map(|entry| entry.generation.load(Ordering::Acquire))
    }

    /// Bump `key`'s lease epoch (generation) in place, WITHOUT touching the
    /// entry's `value` or lease count. San-pattern lease-epoch retention: a
    /// config reload must be observable to callers that revalidate against
    /// `current_epoch` (e.g. the MCP shared single-flight pool) while the
    /// already-dialed connection stays retained rather than being torn down
    /// and rebuilt. Returns the new generation, or `None` if `key` has no
    /// live entry (nothing to retain).
    pub fn bump_epoch(&self, key: &ServiceKey) -> Option<u64> {
        self.entries
            .lock()
            .expect("service registry lock poisoned")
            .get(key)
            .map(|entry| entry.generation.fetch_add(1, Ordering::AcqRel) + 1)
    }

    pub fn evict_idle(&self, minimum_idle: Duration) -> Vec<ServiceKey> {
        let mut entries = self.entries.lock().expect("service registry lock poisoned");
        let evict = entries
            .iter()
            .filter(|(_, entry)| {
                entry.leases.load(Ordering::Acquire) == 0
                    && entry
                        .last_used
                        .lock()
                        .expect("service last-used lock poisoned")
                        .elapsed()
                        >= minimum_idle
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &evict {
            entries.remove(key);
        }
        evict
    }
}
