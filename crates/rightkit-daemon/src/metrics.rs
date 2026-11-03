//! Request, latency, in-flight and connection-table counters of a running host (rightkit#1280).
//!
//! Read them as numbers with [`Hosted::metrics`](crate::Hosted::metrics): requests per method (and
//! per method per minute over the last hour), a latency histogram per method, error codes, the
//! most requests that were in flight at once, and the connection table's occupancy, its peak and
//! its cap. Counts and durations only, never request content. Recording takes one short lock per
//! request; nothing polls.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Upper bounds of the latency histogram buckets, in milliseconds. A request slower than the last
/// bound is counted in the final, open bucket (one more count than bounds).
pub const LATENCY_BOUNDS_MS: [u64; 10] = [1, 5, 10, 25, 50, 100, 250, 500, 1_000, 5_000];
/// Distinct method names counted apart; further names are counted under [`OTHER_METHOD`].
pub const MAX_METHODS: usize = 256;
/// The bucket for methods past [`MAX_METHODS`].
pub const OTHER_METHOD: &str = "(other)";
/// Minutes of per-minute history kept per method.
const MINUTES_KEPT: usize = 60;

/// What the counters hold for one method.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MethodMetrics {
    /// Requests that reached the handler.
    pub requests: u64,
    /// Requests that returned an error, by error code.
    pub errors: BTreeMap<String, u64>,
    /// Requests per latency bucket: index `i` counts latencies up to
    /// [`LATENCY_BOUNDS_MS`]`[i]`, the last index everything slower.
    pub latency_buckets: [u64; LATENCY_BOUNDS_MS.len() + 1],
    /// Sum of the latencies, microseconds.
    pub total_micros: u64,
    /// The slowest request, microseconds.
    pub max_micros: u64,
    /// `(minute since the epoch, requests in it)`, oldest first, at most the last hour.
    pub per_minute: Vec<(u64, u64)>,
}

impl MethodMetrics {
    /// Requests that returned an error, in all.
    pub fn error_count(&self) -> u64 {
        self.errors.values().sum()
    }

    /// Requests in the minute that contains `now` (wall clock), for a rate reading.
    pub fn requests_in_minute_of(&self, now: SystemTime) -> u64 {
        let minute = minute_of(now);
        self.per_minute
            .iter()
            .find(|(m, _)| *m == minute)
            .map_or(0, |(_, n)| *n)
    }
}

/// A reading of the counters, from [`Hosted::metrics`](crate::Hosted::metrics).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostMetrics {
    /// Counters per method.
    pub methods: BTreeMap<String, MethodMetrics>,
    /// Requests being run now.
    pub inflight: usize,
    /// The most requests that were in flight at once.
    pub inflight_max: usize,
    /// Connections held now (admitted, handshake or not).
    pub connections: usize,
    /// The most connections held at once.
    pub connections_peak: usize,
    /// The cap of the connection table ([`HostOptions::max_connections`](crate::HostOptions)); on
    /// Windows the pipe-instance cap bounds the table instead, and this is that cap.
    pub connection_limit: usize,
    /// Connections refused because the table was full.
    pub connections_refused: u64,
}

impl HostMetrics {
    /// Every request counted, across methods.
    pub fn total_requests(&self) -> u64 {
        self.methods.values().map(|m| m.requests).sum()
    }
}

#[derive(Default)]
struct PerMethod {
    stats: MethodMetrics,
    minutes: VecDeque<(u64, u64)>,
}

/// The live counters. One per host.
pub(crate) struct Metrics {
    methods: Mutex<BTreeMap<String, PerMethod>>,
    inflight_max: AtomicUsize,
    connections_peak: AtomicUsize,
    refused: std::sync::atomic::AtomicU64,
    connection_limit: usize,
}

fn minute_of(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 60)
        .unwrap_or(0)
}

impl Metrics {
    pub(crate) fn new(connection_limit: usize) -> Self {
        Self {
            methods: Mutex::new(BTreeMap::new()),
            inflight_max: AtomicUsize::new(0),
            connections_peak: AtomicUsize::new(0),
            refused: std::sync::atomic::AtomicU64::new(0),
            connection_limit,
        }
    }

    /// A request started; `now_inflight` is the count including it.
    pub(crate) fn note_inflight(&self, now_inflight: usize) {
        self.inflight_max.fetch_max(now_inflight, Ordering::SeqCst);
    }

    /// The table now holds `live` connections.
    pub(crate) fn note_connections(&self, live: usize) {
        self.connections_peak.fetch_max(live, Ordering::SeqCst);
    }

    pub(crate) fn note_refused(&self) {
        self.refused.fetch_add(1, Ordering::SeqCst);
    }

    /// One finished request.
    pub(crate) fn record(&self, method: &str, took: Duration, error_code: Option<&str>) {
        let mut methods = self.methods.lock().unwrap_or_else(|e| e.into_inner());
        let key = if methods.contains_key(method) || methods.len() < MAX_METHODS {
            method
        } else {
            OTHER_METHOD
        };
        let entry = methods.entry(key.to_string()).or_default();
        entry.stats.requests += 1;
        if let Some(code) = error_code {
            *entry.stats.errors.entry(code.to_string()).or_insert(0) += 1;
        }
        let ms = took.as_millis() as u64;
        let bucket = LATENCY_BOUNDS_MS
            .iter()
            .position(|bound| ms <= *bound)
            .unwrap_or(LATENCY_BOUNDS_MS.len());
        entry.stats.latency_buckets[bucket] += 1;
        let micros = took.as_micros().min(u128::from(u64::MAX)) as u64;
        entry.stats.total_micros = entry.stats.total_micros.saturating_add(micros);
        entry.stats.max_micros = entry.stats.max_micros.max(micros);
        let minute = minute_of(SystemTime::now());
        match entry.minutes.back_mut() {
            Some((m, n)) if *m == minute => *n += 1,
            _ => {
                entry.minutes.push_back((minute, 1));
                while entry.minutes.len() > MINUTES_KEPT {
                    entry.minutes.pop_front();
                }
            }
        }
    }

    pub(crate) fn snapshot(&self, inflight: usize, connections: usize) -> HostMetrics {
        let methods = self
            .methods
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, m)| {
                let mut stats = m.stats.clone();
                stats.per_minute = m.minutes.iter().copied().collect();
                (name.clone(), stats)
            })
            .collect();
        HostMetrics {
            methods,
            inflight,
            inflight_max: self.inflight_max.load(Ordering::SeqCst),
            connections,
            connections_peak: self.connections_peak.load(Ordering::SeqCst),
            connection_limit: self.connection_limit,
            connections_refused: self.refused.load(Ordering::SeqCst),
        }
    }
}
