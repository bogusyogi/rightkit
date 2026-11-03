//! Shared single-flight MCP initialization pool (FM-100).
//!
//! Absorbed from jcode's `SharedMcpPool`
//! (`crates/jcode-base/src/mcp/pool.rs`, ~lines 28-423). Concurrent
//! initializations of the same MCP server key share ONE in-flight build: the
//! leader runs the builder, waiters block on a `Notify` and then read the
//! completed transport. This prevents N concurrent loads from spawning N
//! duplicate server processes / handshakes.
//!
//! jcode stored `McpClient`/`McpHandle`; here the value is the host app's
//! `Arc<dyn McpTransport>` so the pool plugs into the existing
//! `McpTransportPool` leasing layer without changing the host app's MCP
//! architecture. `ConnectAttempt`, `begin_connect`, and `finish_connect` are
//! lifted verbatim from the donor.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_agent_engine_runtime_resources::ServiceKey;
use tokio::sync::{Mutex, Notify};

use crate::agent_mcp::pool::McpTransportPool;
use crate::agent_mcp::transport::McpTransport;

/// Outcome of beginning a connection attempt for a given key.
/// Lifted verbatim from jcode's `ConnectAttempt`.
enum ConnectAttempt {
    Connected,
    Leader(Arc<Notify>),
    Wait(Arc<Notify>),
}

/// A successfully built shared transport plus the bookkeeping needed to detect
/// that it has drifted out from under the lower leasing layer
/// (`McpTransportPool`).
///
/// - `epoch` is the lower lease `generation()` captured at build time (or at the
///   next `record_epoch` call). A value of `0` means "unknown" until the lower
///   pool stamps it.
/// - `last_validated` is the instant we last confirmed the entry against the
///   lower pool; entries older than `cache_lifetime` are force-rebuilt.
struct BuiltEntry {
    transport: Arc<dyn McpTransport>,
    service_key: Option<ServiceKey>,
    epoch: u64,
    last_validated: Instant,
}

/// A recorded connection failure, used to space out reconnect attempts.
struct ConnectionFailure {
    error: String,
    cooldown_until: Instant,
}

/// Single-flight pool: for a given key, only one in-flight `build` future runs
/// at a time; concurrent callers await the same result.
///
/// Mirrors jcode's `SharedMcpPool` (which stored `McpClient`/`McpHandle`) but
/// is value-generic over the host app's `Arc<dyn McpTransport>`.
///
/// Correctness (FM-100): the strong `Arc` held in `built` can otherwise outlive
/// the lower `McpTransportPool` lease that sourced it. To keep the two layers
/// consistent this pool (a) enforces a reconnect `cooldown`, (b) revalidates
/// against the lower pool's lease `epoch` on every `Connected` read, and (c)
/// expires entries after `cache_lifetime` so a dead transport is never served.
pub struct SharedMcpPool {
    built: Mutex<HashMap<String, BuiltEntry>>,
    connecting: Mutex<HashMap<String, Arc<Notify>>>,
    last_errors: Mutex<HashMap<String, ConnectionFailure>>,
    cooldown: Duration,
    cache_lifetime: Duration,
}

impl Default for SharedMcpPool {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedMcpPool {
    pub fn new() -> Self {
        Self {
            built: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashMap::new()),
            last_errors: Mutex::new(HashMap::new()),
            cooldown: Duration::from_secs(30),
            cache_lifetime: Duration::from_secs(300),
        }
    }

    /// Get (or initialize) the shared transport for `key`, without binding to
    /// the lower leasing layer. Mirrors jcode's `ensure_connected`: the first
    /// caller (leader) runs `build`; any concurrent caller for the same key
    /// waits on a `Notify` and then returns the same built transport (or the
    /// recorded error). Epoch/cache-lifetime revalidation is skipped because no
    /// lower pool is supplied, so this is only for standalone/test use.
    pub async fn get_or_init<F, Fut>(
        &self,
        key: String,
        build: F,
    ) -> Result<Arc<dyn McpTransport>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Arc<dyn McpTransport>, String>>,
    {
        self.ensure_connected(key, None, None, build).await
    }

    /// Pooled variant (FM-100 live path): binds `key` to its lower-layer
    /// `ServiceKey` (`service_key`) and the source `McpTransportPool` (`lower`).
    /// On a `Connected` hit the entry is revalidated against the lower pool's
    /// lease `epoch` and the `cache_lifetime`; a drift or expiry forces a fresh
    /// single-flight rebuild so a dead transport is never served.
    pub async fn get_or_init_pooled<F, Fut>(
        &self,
        key: String,
        service_key: ServiceKey,
        lower: Option<&McpTransportPool>,
        build: F,
    ) -> Result<Arc<dyn McpTransport>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Arc<dyn McpTransport>, String>>,
    {
        self.ensure_connected(key, Some(service_key), lower, build)
            .await
    }

    async fn ensure_connected<F, Fut>(
        &self,
        key: String,
        service_key: Option<ServiceKey>,
        lower: Option<&McpTransportPool>,
        build: F,
    ) -> Result<Arc<dyn McpTransport>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Arc<dyn McpTransport>, String>>,
    {
        // FM-100 cooldown: while a recent failure's cooldown window is active,
        // surface the cached error instead of rebuilding, preventing reconnect
        // storms. After the window lapses, callers fall through to a real retry.
        {
            let errors = self.last_errors.lock().await;
            if let Some(failure) = errors.get(&key) {
                if failure.cooldown_until > Instant::now() {
                    return Err(failure.error.clone());
                }
            }
        }
        loop {
            match self.begin_connect(&key).await {
                ConnectAttempt::Connected => {
                    // Revalidate against the lower leasing layer before serving
                    // a cached transport: the lower lease may have been evicted
                    // (epoch changed/nonexistent) or the cache lifetime elapsed.
                    let cached = {
                        let mut built = self.built.lock().await;
                        let Some(entry) = built.get_mut(&key) else {
                            continue;
                        };
                        let valid = if let (Some(lower), Some(sk)) = (lower, &service_key) {
                            let epoch = lower.current_epoch(sk);
                            let expired = Instant::now().duration_since(entry.last_validated)
                                > self.cache_lifetime;
                            epoch == Some(entry.epoch) && !expired
                        } else {
                            true
                        };
                        if valid {
                            entry.last_validated = Instant::now();
                            Some(Arc::clone(&entry.transport))
                        } else {
                            // Removal shares this lock with insertion, preventing
                            // a stale validator from deleting a newer epoch.
                            built.remove(&key);
                            None
                        }
                    };
                    if let Some(transport) = cached {
                        return Ok(transport);
                    }
                    continue;
                }
                ConnectAttempt::Wait(notify) => {
                    notify.notified().await;
                    if self.built.lock().await.contains_key(&key) {
                        // Re-enter through Connected so every caller, including
                        // waiters, validates lower-pool epoch before use.
                        continue;
                    }
                    let error = self
                        .last_errors
                        .lock()
                        .await
                        .get(&key)
                        .map(|failure| failure.error.clone())
                        .unwrap_or_else(|| "build attempt did not produce a transport".to_string());
                    return Err(error);
                }
                ConnectAttempt::Leader(notify) => {
                    let result = build().await;
                    self.finish_connect(&key, service_key, &notify, &result)
                        .await;
                    return result;
                }
            }
        }
    }

    async fn begin_connect(&self, name: &str) -> ConnectAttempt {
        let mut connecting = self.connecting.lock().await;
        if let Some(notify) = connecting.get(name) {
            return ConnectAttempt::Wait(Arc::clone(notify));
        }

        if self.built.lock().await.contains_key(name) {
            return ConnectAttempt::Connected;
        }

        let notify = Arc::new(Notify::new());
        connecting.insert(name.to_string(), Arc::clone(&notify));
        ConnectAttempt::Leader(notify)
    }

    async fn finish_connect(
        &self,
        name: &str,
        service_key: Option<ServiceKey>,
        notify: &Arc<Notify>,
        result: &Result<Arc<dyn McpTransport>, String>,
    ) {
        match result {
            Ok(transport) => {
                {
                    let mut built = self.built.lock().await;
                    built.insert(
                        name.to_string(),
                        BuiltEntry {
                            transport: Arc::clone(transport),
                            service_key,
                            epoch: 0,
                            last_validated: Instant::now(),
                        },
                    );
                }
                {
                    let mut errors = self.last_errors.lock().await;
                    errors.remove(name);
                }
            }
            Err(error) => {
                let mut errors = self.last_errors.lock().await;
                errors.insert(
                    name.to_string(),
                    ConnectionFailure {
                        error: error.clone(),
                        cooldown_until: Instant::now() + self.cooldown,
                    },
                );
            }
        }

        {
            let mut connecting = self.connecting.lock().await;
            if connecting
                .get(name)
                .map(|current| Arc::ptr_eq(current, notify))
                .unwrap_or(false)
            {
                connecting.remove(name);
            }
        }

        notify.notify_waiters();
    }

    /// Stamp the cached entry's lower-lease `epoch`. Called by the live path
    /// right after the lower `McpTransportPool::acquire` returns, so subsequent
    /// reads can detect that the lower lease was evicted or regenerated.
    /// Refreshes `last_validated` to the moment of this successful validation.
    pub async fn record_epoch(&self, key: &str, epoch: u64) {
        let mut built = self.built.lock().await;
        if let Some(entry) = built.get_mut(key) {
            entry.epoch = epoch;
            entry.last_validated = Instant::now();
        }
    }

    /// Drop the cached entry for `key`. Called by the reaper after the lower
    /// pool evicts the corresponding lease, so a dead transport is not served.
    pub async fn invalidate(&self, key: &str) {
        self.built.lock().await.remove(key);
    }

    /// Drop every cached transport tied to an evicted lower-pool service.
    /// This compares the complete stable [`ServiceKey`], never a diagnostic or
    /// debug string, so formatting changes cannot leave a stale transport live.
    pub async fn invalidate_service(&self, service_key: &ServiceKey) {
        self.built
            .lock()
            .await
            .retain(|_, entry| entry.service_key.as_ref() != Some(service_key));
    }
}

/// Global pool singleton.
static SHARED_POOL: tokio::sync::OnceCell<Arc<SharedMcpPool>> = tokio::sync::OnceCell::const_new();

/// Initialize the global shared MCP single-flight pool. Call once at daemon
/// startup. Mirrors jcode's `init_shared_pool`.
pub async fn init_shared_pool() -> Arc<SharedMcpPool> {
    SHARED_POOL
        .get_or_init(|| async { Arc::new(SharedMcpPool::new()) })
        .await
        .clone()
}

/// Get the global shared pool, if initialized via [`init_shared_pool`].
/// Mirrors jcode's `get_shared_pool`.
pub fn get_shared_pool() -> Option<Arc<SharedMcpPool>> {
    SHARED_POOL.get().cloned()
}

/// Get the global shared pool, initializing it lazily if a startup call to
/// [`init_shared_pool`] has not happened. Use this from call sites that may run
/// before daemon startup (e.g. tests) without forcing an explicit init.
pub async fn get_or_init_shared_pool() -> Arc<SharedMcpPool> {
    if let Some(pool) = get_shared_pool() {
        return pool;
    }
    init_shared_pool().await
}
