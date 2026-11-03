//! Generic leased pool: any `Arc<V>` keyed by any `K`, with refcounted leases, per-key epochs,
//! idle eviction, a diagnostic snapshot, single-flight connect and a failure cooldown.
//!
//! [`ConnectionPool`](crate::ConnectionPool) stays the trust-gated pool of [`McpClient`]s; this
//! pool carries no trust gate and no MCP knowledge. A host builds its key from whatever
//! identifies a connection (scope, kind, endpoint, args/env fingerprint, config hash) and
//! passes a `create` closure that connects.
//!
//! [`McpClient`]: crate::McpClient

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::{McpError, Result};

/// Tuning for a [`LeasedPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeasedPoolConfig {
    /// After a failed connect, later callers get a cooldown error for this long instead of
    /// connecting again. Callers already waiting on the failed connect share its error whatever
    /// this is. Zero (the default) means no cooldown.
    pub reconnect_cooldown: Duration,
    /// A cached value older than this is dropped from the pool and reconnected on next use
    /// (leases already out keep their value). `None` (the default) never expires.
    pub cache_lifetime: Option<Duration>,
}

/// Lease bookkeeping shared by a pooled entry and its leases.
struct Usage {
    count: AtomicUsize,
    last_released: Mutex<Instant>,
}

impl Usage {
    fn new(initial: usize) -> Arc<Self> {
        Arc::new(Usage {
            count: AtomicUsize::new(initial),
            last_released: Mutex::new(Instant::now()),
        })
    }

    fn idle(&self) -> Duration {
        lock(&self.last_released).elapsed()
    }
}

/// A counted hold on a pooled value. The value is released (and may be evicted by
/// [`LeasedPool::evict_idle`]) when the last lease drops. Dereferences to the value.
pub struct Lease<V: ?Sized> {
    value: Arc<V>,
    epoch: u64,
    usage: Arc<Usage>,
}

impl<V: ?Sized> Lease<V> {
    /// The pooled value, cloned out of the lease (it outlives the lease).
    pub fn value(&self) -> Arc<V> {
        self.value.clone()
    }

    /// The key's epoch when this value was connected. Compare with
    /// [`LeasedPool::current_epoch`] (or call [`LeasedPool::is_current`]) to learn that a
    /// reload happened while the lease was held.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl<V: ?Sized> Deref for Lease<V> {
    type Target = V;

    fn deref(&self) -> &V {
        &self.value
    }
}

impl<V: ?Sized> Drop for Lease<V> {
    fn drop(&mut self) {
        *lock(&self.usage.last_released) = Instant::now();
        self.usage.count.fetch_sub(1, Ordering::SeqCst);
    }
}

impl<V: ?Sized> std::fmt::Debug for Lease<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("epoch", &self.epoch)
            .field("leases", &self.usage.count.load(Ordering::SeqCst))
            .finish()
    }
}

/// What a pooled key is doing, as reported by [`LeasedPool::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// A connected value is cached.
    Ready,
    /// One caller is connecting; others wait for it.
    Connecting,
    /// The last connect failed and is inside the cooldown.
    CoolingDown,
}

/// One key's diagnostic row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySnapshot<K> {
    pub key: K,
    pub state: EntryState,
    /// The key's current epoch (for `Ready`, the epoch it was connected at).
    pub epoch: u64,
    /// Leases currently out (`Ready` only).
    pub leases: usize,
    /// Time since the value was connected, or since the failure (zero while connecting).
    pub age: Duration,
    /// Time since the last lease was released or the value was connected (`Ready` only).
    pub idle: Duration,
}

#[derive(Default)]
struct Flight {
    outcome: Mutex<Option<Result<()>>>,
    done: Condvar,
}

impl Flight {
    fn finish(&self, outcome: Result<()>) {
        *lock(&self.outcome) = Some(outcome);
        self.done.notify_all();
    }

    fn wait(&self) -> Result<()> {
        let mut outcome = lock(&self.outcome);
        loop {
            if let Some(outcome) = outcome.as_ref() {
                return outcome.clone();
            }
            outcome = self.done.wait(outcome).unwrap_or_else(|e| e.into_inner());
        }
    }
}

struct Ready<V: ?Sized> {
    value: Arc<V>,
    epoch: u64,
    created: Instant,
    usage: Arc<Usage>,
}

enum State<V: ?Sized> {
    Ready(Ready<V>),
    Connecting(Arc<Flight>),
    Failed { at: Instant, error: McpError },
}

struct Table<K, V: ?Sized> {
    states: HashMap<K, State<V>>,
    key_epochs: HashMap<K, u64>,
    global_epoch: u64,
}

impl<K: Eq + Hash, V: ?Sized> Table<K, V> {
    fn epoch(&self, key: &K) -> u64 {
        self.global_epoch + self.key_epochs.get(key).copied().unwrap_or(0)
    }
}

/// What `acquire` decided to do after looking at one key, with the table lock released.
enum Step<V: ?Sized> {
    Lease(Lease<V>),
    Wait(Arc<Flight>),
    Cooldown(McpError, Duration),
    Lead(Arc<Flight>, u64),
}

/// A bounded-lifetime cache of shared connections with refcounted leases.
///
/// `acquire` is single-flight per key: concurrent first use shares one in-flight `create`
/// (the first caller runs it, the rest wait and share its value or its error).
pub struct LeasedPool<K, V: ?Sized> {
    cfg: LeasedPoolConfig,
    table: Mutex<Table<K, V>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears a `Connecting` entry and releases the waiters if `create` unwinds.
struct LeaderGuard<'a, K: Eq + Hash, V: ?Sized> {
    pool: &'a LeasedPool<K, V>,
    key: &'a K,
    flight: &'a Arc<Flight>,
    armed: bool,
}

impl<K: Eq + Hash, V: ?Sized> Drop for LeaderGuard<'_, K, V> {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.pool.table).states.remove(self.key);
            self.flight
                .finish(Err(McpError::Transport("connect panicked".into())));
        }
    }
}

impl<K: Eq + Hash + Clone, V: ?Sized> LeasedPool<K, V> {
    pub fn new(cfg: LeasedPoolConfig) -> Self {
        Self {
            cfg,
            table: Mutex::new(Table {
                states: HashMap::new(),
                key_epochs: HashMap::new(),
                global_epoch: 0,
            }),
        }
    }

    /// A lease on the value for `key`, connecting with `create` when none is cached.
    ///
    /// A cached value is reused unless its epoch is stale (see [`bump_epoch`](Self::bump_epoch))
    /// or it outlived `cache_lifetime`; then it leaves the pool and `create` runs again. While
    /// one caller connects, others wait and share the value or the error. After a failure, calls
    /// within `reconnect_cooldown` return a `McpError::Transport` cooldown error without calling
    /// `create`. A value connected across an epoch bump is returned to its caller but not cached.
    pub fn acquire(&self, key: K, create: impl FnOnce() -> Result<Arc<V>>) -> Result<Lease<V>> {
        let mut create = Some(create);
        loop {
            match self.step(&key) {
                Step::Lease(lease) => return Ok(lease),
                Step::Cooldown(error, remaining) => {
                    return Err(McpError::Transport(format!(
                        "reconnect cooling down for {}ms after: {error}",
                        remaining.as_millis()
                    )))
                }
                Step::Wait(flight) => {
                    flight.wait()?;
                }
                Step::Lead(flight, epoch) => {
                    let Some(create) = create.take() else {
                        return Err(McpError::Closed);
                    };
                    return self.lead(&key, &flight, epoch, create);
                }
            }
        }
    }

    fn step(&self, key: &K) -> Step<V> {
        let mut table = lock(&self.table);
        let epoch = table.epoch(key);
        let lifetime = self.cfg.cache_lifetime;
        let cooldown = self.cfg.reconnect_cooldown;
        let stale_or_expired = match table.states.get(key) {
            Some(State::Ready(ready)) => {
                ready.epoch != epoch || lifetime.is_some_and(|l| ready.created.elapsed() >= l)
            }
            _ => false,
        };
        if stale_or_expired {
            table.states.remove(key);
        }
        match table.states.get(key) {
            Some(State::Ready(ready)) => {
                ready.usage.count.fetch_add(1, Ordering::SeqCst);
                Step::Lease(Lease {
                    value: ready.value.clone(),
                    epoch: ready.epoch,
                    usage: ready.usage.clone(),
                })
            }
            Some(State::Connecting(flight)) => Step::Wait(flight.clone()),
            Some(State::Failed { at, error }) if at.elapsed() < cooldown => {
                Step::Cooldown(error.clone(), cooldown.saturating_sub(at.elapsed()))
            }
            _ => {
                let flight = Arc::new(Flight::default());
                table
                    .states
                    .insert(key.clone(), State::Connecting(flight.clone()));
                Step::Lead(flight, epoch)
            }
        }
    }

    fn lead(
        &self,
        key: &K,
        flight: &Arc<Flight>,
        epoch: u64,
        create: impl FnOnce() -> Result<Arc<V>>,
    ) -> Result<Lease<V>> {
        let mut guard = LeaderGuard {
            pool: self,
            key,
            flight,
            armed: true,
        };
        let created = create();
        guard.armed = false;
        let mut table = lock(&self.table);
        let result = match created {
            Ok(value) => {
                let usage = Usage::new(1);
                if table.epoch(key) == epoch {
                    table.states.insert(
                        key.clone(),
                        State::Ready(Ready {
                            value: value.clone(),
                            epoch,
                            created: Instant::now(),
                            usage: usage.clone(),
                        }),
                    );
                } else {
                    table.states.remove(key);
                }
                Ok(Lease {
                    value,
                    epoch,
                    usage,
                })
            }
            Err(error) => {
                if self.cfg.reconnect_cooldown.is_zero() {
                    table.states.remove(key);
                } else {
                    table.states.insert(
                        key.clone(),
                        State::Failed {
                            at: Instant::now(),
                            error: error.clone(),
                        },
                    );
                }
                Err(error)
            }
        };
        drop(table);
        flight.finish(result.as_ref().map(|_| ()).map_err(Clone::clone));
        result
    }

    /// The key's current epoch (0 until bumped). A lease whose [`Lease::epoch`] is lower was
    /// connected before the last reload.
    pub fn current_epoch(&self, key: &K) -> u64 {
        lock(&self.table).epoch(key)
    }

    /// True when `lease` was connected at the key's current epoch.
    pub fn is_current(&self, key: &K, lease: &Lease<V>) -> bool {
        self.current_epoch(key) == lease.epoch
    }

    /// Invalidates the cached value for `key`: the next `acquire` reconnects. Leases already
    /// out keep working and report the old epoch. Returns the new epoch.
    pub fn bump_epoch(&self, key: &K) -> u64 {
        let mut table = lock(&self.table);
        *table.key_epochs.entry(key.clone()).or_insert(0) += 1;
        table.epoch(key)
    }

    /// Invalidates every key (a configuration reload). Returns the new global epoch.
    pub fn bump_epoch_on_reload(&self) -> u64 {
        let mut table = lock(&self.table);
        table.global_epoch += 1;
        table.global_epoch
    }

    /// Drops every cached value with no lease out that has been idle for at least `min_idle`
    /// (`Duration::ZERO` drops all unleased values). Returns how many were dropped.
    pub fn evict_idle(&self, min_idle: Duration) -> usize {
        let mut table = lock(&self.table);
        let before = table.states.len();
        table.states.retain(|_, state| match state {
            State::Ready(ready) => {
                ready.usage.count.load(Ordering::SeqCst) > 0 || ready.usage.idle() < min_idle
            }
            _ => true,
        });
        before - table.states.len()
    }

    /// Removes the cached value (or failure record) for `key`. Leases out keep their value.
    /// An in-flight connect is not interrupted. Returns whether something was removed.
    pub fn remove(&self, key: &K) -> bool {
        let mut table = lock(&self.table);
        if matches!(table.states.get(key), Some(State::Connecting(_))) {
            return false;
        }
        table.states.remove(key).is_some()
    }

    /// Number of cached connected values.
    pub fn len(&self) -> usize {
        lock(&self.table)
            .states
            .values()
            .filter(|s| matches!(s, State::Ready(_)))
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One row per key (unordered), for diagnostics.
    pub fn snapshot(&self) -> Vec<EntrySnapshot<K>> {
        let table = lock(&self.table);
        table
            .states
            .iter()
            .map(|(key, state)| match state {
                State::Ready(ready) => EntrySnapshot {
                    key: key.clone(),
                    state: EntryState::Ready,
                    epoch: ready.epoch,
                    leases: ready.usage.count.load(Ordering::SeqCst),
                    age: ready.created.elapsed(),
                    idle: ready.usage.idle(),
                },
                State::Connecting(_) => EntrySnapshot {
                    key: key.clone(),
                    state: EntryState::Connecting,
                    epoch: table.epoch(key),
                    leases: 0,
                    age: Duration::ZERO,
                    idle: Duration::ZERO,
                },
                State::Failed { at, .. } => EntrySnapshot {
                    key: key.clone(),
                    state: EntryState::CoolingDown,
                    epoch: table.epoch(key),
                    leases: 0,
                    age: at.elapsed(),
                    idle: Duration::ZERO,
                },
            })
            .collect()
    }
}
