//! Rolling CPU, memory, swap & network readings with a visibility lease.

use crate::monitor::NetworkRateSampler;
use crate::{LiveSampler, SamplerError};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const FAST: Duration = Duration::from_secs(2);
const IDLE: Duration = Duration::from_secs(10);
const WARMUP: Duration = Duration::from_millis(500);
const MAX_SAMPLES: usize = 3600;
const MAX_LEASES: usize = 128;
const MAX_CLIENT_BYTES: usize = 256;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// Unix time in milliseconds.
    pub ts: u64,
    /// Overall CPU use, 0 to 100 across all cores.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// Null where the platform does not report memory pressure.
    pub pressure: Option<String>,
    /// Bytes per second; null until two counter reads exist or when unreadable.
    pub net_down: Option<f64>,
    pub net_up: Option<f64>,
}

#[derive(Default)]
struct State {
    stopped: bool,
    paused: bool,
    running: bool,
    leases: HashMap<String, Instant>,
    wake: bool,
    samples: VecDeque<(Instant, Sample)>,
    // Keep the cursor even if all samples have expired.
    last_ts: Option<u64>,
}

impl State {
    fn watching(&mut self, now: Instant) -> bool {
        self.leases.retain(|_, expiry| *expiry > now);
        !self.leases.is_empty()
    }

    fn prune(&mut self, now: Instant, retention: Duration) {
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > retention)
        {
            self.samples.pop_front();
        }
    }
}

/// One history shared by the worker, lease holders & readers of a host service.
pub struct SystemHistory {
    state: Mutex<State>,
    changed: Condvar,
    retention: Duration,
}

// Owns admission, never a mutex guard: host callbacks may reenter this instance.
// Local sampler variables drop before this guard releases runner admission.
struct Runner<'a>(&'a SystemHistory);

impl Drop for Runner<'_> {
    fn drop(&mut self) {
        self.0.state().running = false;
    }
}

impl Default for SystemHistory {
    fn default() -> Self {
        Self::with_retention(Duration::from_secs(30 * 60))
    }
}

impl SystemHistory {
    /// Samples also have a count limit of 3,600 regardless of retention.
    pub fn with_retention(retention: Duration) -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            retention,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Terminally stop this instance, discard visibility leases & wake its runner.
    /// Retained samples remain readable. Later `run` calls return without sampling;
    /// later leases are rejected. This request does not join or create a thread.
    /// The host must retain its thread handle, call `stop`, then join it before
    /// releasing host callback resources. In-flight callbacks/OS reads must return
    /// before `run` can finish; no history mutex is held while they execute.
    pub fn stop(&self) {
        let mut state = self.state();
        state.stopped = true;
        state.leases.clear();
        state.wake = false;
        self.changed.notify_all();
    }

    /// Pause admission, discard visibility leases & wake normal/warmup waits.
    /// Retained samples & cursor survive. Publication serialized after this request
    /// is rejected, including a reading whose OS read or clock callback is in flight.
    /// Reads/callbacks already in flight may finish; this method does not join them.
    /// The host must serialize preference changes, pause, then join its old or
    /// pending runner handle before `resume` & spawning its replacement.
    pub fn pause(&self) {
        let mut state = self.state();
        state.paused = true;
        state.leases.clear();
        state.wake = false;
        self.changed.notify_all();
    }

    /// Re-arm admission after the host has joined its old or pending runner.
    /// Returns false after terminal stop or while any runner owns admission.
    /// Returns true when admission is enabled; this method creates no worker.
    /// A handle awaiting startup is not yet admitted: joining it before resume
    /// prevents that stale invocation from racing with a replacement runner.
    /// A new run initializes fresh CPU & network baselines on this same history.
    pub fn resume(&self) -> bool {
        let mut state = self.state();
        if state.stopped || state.running {
            return false;
        }
        state.paused = false;
        true
    }

    fn is_inactive(&self) -> bool {
        let state = self.state();
        state.stopped || state.paused
    }

    /// Renew a visibility lease. The first live lease wakes an idle sampler.
    /// Accepts nonempty UTF-8 IDs of at most 256 bytes, up to 128 live IDs.
    /// Existing IDs may renew at capacity; expired IDs are reclaimed first.
    /// Returns false for an invalid ID, a new ID at capacity, while paused, or after `stop`.
    /// TTL remains clamped to 1..=60,000 ms (default 6,000 ms).
    pub fn lease(&self, client: &str, ttl_ms: Option<u64>) -> bool {
        if client.is_empty() || client.len() > MAX_CLIENT_BYTES {
            return false;
        }
        let ttl = Duration::from_millis(ttl_ms.unwrap_or(6000).clamp(1, 60_000));
        let mut state = self.state();
        if state.stopped || state.paused {
            return false;
        }
        let now = Instant::now();
        let idle = !state.watching(now);
        if state.leases.len() >= MAX_LEASES && !state.leases.contains_key(client) {
            return false;
        }
        state.leases.insert(client.to_owned(), now + ttl);
        if idle {
            state.wake = true;
            self.changed.notify_one();
        }
        true
    }

    /// Return retained samples in order, excluding the supplied cursor.
    pub fn samples(&self, since_ms: Option<u64>) -> Vec<Sample> {
        let mut state = self.state();
        state.prune(Instant::now(), self.retention);
        state
            .samples
            .iter()
            .filter(|(_, s)| since_ms.is_none_or(|since| s.ts > since))
            .map(|(_, s)| s.clone())
            .collect()
    }

    fn record(&self, mut sample: Sample) {
        let mut state = self.state();
        if state.stopped || state.paused {
            return;
        }
        let now = Instant::now();
        state.prune(now, self.retention);
        if let Some(last) = state.last_ts {
            // The host supplies a signed millisecond clock. Each reading remains addressable
            // by an exclusive cursor, including after an empty window or a backward jump.
            let Some(next) = last.checked_add(1) else {
                return;
            };
            sample.ts = sample.ts.max(next);
        }
        state.last_ts = Some(sample.ts);
        if state.samples.len() >= MAX_SAMPLES {
            state.samples.pop_front();
        }
        state.samples.push_back((now, sample));
    }

    // Warmup ignores lease wakes so CPU deltas still span the full warmup.
    // Both waits observe pause/stop under the condvar predicate mutex.
    fn wait_until(&self, deadline: Instant, wake_on_lease: bool) -> bool {
        let mut state = self.state();
        loop {
            if state.stopped || state.paused {
                return false;
            }
            if wake_on_lease && std::mem::take(&mut state.wake) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Run on a thread created & joined by the host; this package spawns none.
    /// Admission starts enabled. Concurrent calls, paused calls & calls after
    /// terminal `stop` return immediately. Pause/stop wake normal & warmup waits;
    /// return drops CPU & network samplers before releasing runner admission.
    /// Callbacks execute without history locks & must return for joining to finish.
    /// The host must serialize preference transitions & retain its runner handle:
    /// pause, join the old or pending invocation, resume, then spawn a replacement.
    /// Join pending handles too: `resume` cannot detect a thread before `run` starts.
    /// For an initially disabled preference, pause before spawning any runner.
    pub fn run(&self, wall_ms: impl Fn() -> i64) -> Result<(), SamplerError> {
        let _worker = {
            let mut state = self.state();
            if state.running || state.stopped || state.paused {
                return Ok(());
            }
            state.running = true;
            Runner(self)
        };
        let mut samplers = None;
        let mut deadline = Instant::now();
        loop {
            if !self.wait_until(deadline, true) {
                return Ok(());
            }
            let tick = Instant::now();
            if samplers.is_none() {
                samplers = Some((LiveSampler::new()?, NetworkRateSampler::new(), tick));
                if !self.wait_until(Instant::now() + WARMUP, false) {
                    return Ok(());
                }
            }
            if let Some((cpu, network, origin)) = &mut samplers {
                let reading = cpu.sample();
                let rates = network
                    .sample_current(origin.elapsed().as_millis() as u64)
                    .value;
                if self.is_inactive() {
                    return Ok(());
                }
                self.record(Sample {
                    ts: u64::try_from(wall_ms()).unwrap_or(0),
                    cpu: reading.cpu_percent,
                    mem_used: reading.memory_used_bytes,
                    mem_total: reading.memory_total_bytes,
                    swap_used: reading.swap_used_bytes,
                    swap_total: reading.swap_total_bytes,
                    pressure: reading.memory_pressure,
                    net_down: rates.map(|r| r.received_bytes_per_second),
                    net_up: rates.map(|r| r.transmitted_bytes_per_second),
                });
            }
            let now = Instant::now();
            let interval = if self.state().watching(now) {
                FAST
            } else {
                IDLE
            };
            deadline = tick + interval;
            if deadline < now {
                deadline = now + interval;
            }
        }
    }
}
