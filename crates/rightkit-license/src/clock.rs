//! Anti-rollback license clock.
//!
//! A device clock is user-controlled, so a purely wall-clock expiry check can be
//! defeated by moving the clock back. This module keeps a persisted **high-water mark**
//! of the trusted time the device has observed, and refuses to extend validity when the
//! wall clock falls behind it.
//!
//! Three inputs feed the mark:
//! 1. the local wall clock (while it keeps advancing),
//! 2. a per-process monotonic offset (`wall at process start + Instant elapsed`), which
//!    keeps time moving inside one run even if the wall clock is wound back, and
//! 3. **server anchors**: the `iat` of a freshly issued, signature-verified token, which
//!    re-anchors the mark on server time (see [`AntiRollbackClock::reanchor`]).
//!
//! The decision logic ([`reconcile_clock`]) is a pure function. [`AntiRollbackClock`]
//! wraps it with a [`ClockStore`] so each app persists the mark where it already keeps
//! secrets (keychain, credential manager). The persisted JSON layout
//! ([`ClockState::encode`]) is byte-compatible with the records ViewRight and HeardRight
//! already store, including the legacy scalar form, so adopting this module does not
//! reset existing installs.

use std::collections::hash_map::RandomState;
use std::convert::Infallible;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A wall clock may sit this many seconds behind the high-water mark before it counts
/// as a rollback (ordinary NTP corrections and small manual adjustments).
pub const CLOCK_ROLLBACK_TOLERANCE_SECS: i64 = 300;

/// [`AntiRollbackClock::commit`] writes at most this often unless the process identity or
/// server anchor changed. Keychain writes are slow and can prompt.
pub const CLOCK_WRITE_INTERVAL_SECS: i64 = 60;

/// How many times a ledger write is re-planned against a concurrently changed ledger
/// before it fails with [`ClockError::Contended`].
pub const CLOCK_WRITE_ATTEMPTS: u32 = 16;

/// Serialises every load/decide/write on a ledger by every [`AntiRollbackClock`] in this
/// process. One lock for all clocks (not one per store) because two clocks may sit on the
/// same keychain item through different [`ClockStore`] values. Critical sections are one
/// store read plus at most one store write.
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

fn ledger_lock() -> MutexGuard<'static, ()> {
    LEDGER_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The persisted anti-rollback ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockState {
    /// Highest trusted unix time observed so far.
    pub highest_seen: i64,
    /// Identity of the process that last advanced the mark. A different process cannot
    /// prove monotonic progress, so it must see wall time strictly past the mark.
    pub trusted_process: String,
    /// `iat` of the last server anchor. A re-anchor must be strictly newer (replay guard).
    pub last_server_iat: i64,
}

#[derive(Serialize, Deserialize)]
struct StoredClock {
    v: u8,
    highest_seen: i64,
    trusted_process: String,
    #[serde(default)]
    last_server_iat: i64,
}

/// Why a persisted clock record could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockDecodeError {
    /// A JSON record with a `v` this crate does not understand.
    UnsupportedVersion(u8),
    /// Neither the JSON record nor the legacy scalar form.
    Malformed,
}

impl fmt::Display for ClockDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => {
                write!(f, "stored license clock has an unsupported version ({v})")
            }
            Self::Malformed => write!(f, "stored license clock is malformed"),
        }
    }
}
impl std::error::Error for ClockDecodeError {}

impl ClockState {
    /// Version written by [`ClockState::encode`].
    pub const STORED_VERSION: u8 = 1;

    /// Serialise as the JSON record shared with the apps' existing clock entries:
    /// `{"v":1,"highest_seen":..,"trusted_process":"..","last_server_iat":..}`.
    pub fn encode(&self) -> String {
        serde_json::to_string(&StoredClock {
            v: Self::STORED_VERSION,
            highest_seen: self.highest_seen,
            trusted_process: self.trusted_process.clone(),
            last_server_iat: self.last_server_iat,
        })
        .expect("serialising a plain clock record cannot fail")
    }

    /// Decode a stored record. The original scalar form (a bare integer) is accepted as
    /// `trusted_process = "legacy"`: it cannot prove same-process monotonic progress, so
    /// a backward clock fails closed until a fresh signed server response re-anchors it.
    pub fn decode(raw: &str) -> Result<Self, ClockDecodeError> {
        if let Ok(stored) = serde_json::from_str::<StoredClock>(raw) {
            if stored.v != Self::STORED_VERSION {
                return Err(ClockDecodeError::UnsupportedVersion(stored.v));
            }
            return Ok(Self {
                highest_seen: stored.highest_seen,
                trusted_process: stored.trusted_process,
                last_server_iat: stored.last_server_iat,
            });
        }
        raw.trim()
            .parse::<i64>()
            .map(|highest_seen| Self {
                highest_seen,
                trusted_process: "legacy".to_string(),
                last_server_iat: 0,
            })
            .map_err(|_| ClockDecodeError::Malformed)
    }
}

/// Result of one pure reconcile step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockDecision {
    /// The clock is believable. `effective_now` is the time to evaluate expiry at;
    /// `next` is the ledger to persist.
    Trusted {
        effective_now: i64,
        next: ClockState,
    },
    /// The wall clock is behind the high-water mark (or stood still across a restart).
    Rollback { highest_seen: i64 },
}

/// Pure anti-rollback decision.
///
/// - `wall_now`: the device wall clock.
/// - `monotonic_now`: `wall at process start + monotonic elapsed` (see [`ProcessAnchor`]).
/// - `process_id`: this process's identity.
/// - `prior`: the persisted ledger, if any.
///
/// Rules:
/// - No prior ledger: trust `max(wall, monotonic)` and start the ledger there.
/// - Wall more than [`CLOCK_ROLLBACK_TOLERANCE_SECS`] behind the mark: rollback.
/// - A different process than the one that wrote the mark must see wall time strictly
///   past it; otherwise repeatedly restarting at the same wall second would freeze expiry.
/// - Same process with a wall clock that is behind the mark or lagging the monotonic
///   offset: ignore the wall clock and keep advancing on the monotonic offset.
/// - Otherwise trust `max(wall, monotonic, mark)`; the mark never moves backwards here.
pub fn reconcile_clock(
    wall_now: i64,
    monotonic_now: i64,
    process_id: &str,
    prior: Option<&ClockState>,
) -> ClockDecision {
    let Some(prior) = prior else {
        let effective_now = wall_now.max(monotonic_now);
        return ClockDecision::Trusted {
            effective_now,
            next: ClockState {
                highest_seen: effective_now,
                trusted_process: process_id.to_string(),
                last_server_iat: 0,
            },
        };
    };

    if wall_now.saturating_add(CLOCK_ROLLBACK_TOLERANCE_SECS) < prior.highest_seen {
        return ClockDecision::Rollback {
            highest_seen: prior.highest_seen,
        };
    }

    // A restarted process has no surviving monotonic anchor. Reusing an equal
    // wall-clock second across restarts would otherwise freeze local expiry.
    if prior.trusted_process != process_id && wall_now <= prior.highest_seen {
        return ClockDecision::Rollback {
            highest_seen: prior.highest_seen,
        };
    }

    let clock_is_not_advancing =
        wall_now < prior.highest_seen || monotonic_now.saturating_sub(wall_now) > 2;
    if clock_is_not_advancing {
        if prior.trusted_process != process_id {
            return ClockDecision::Rollback {
                highest_seen: prior.highest_seen,
            };
        }
        let effective_now = prior.highest_seen.max(monotonic_now);
        return ClockDecision::Trusted {
            effective_now,
            next: ClockState {
                highest_seen: effective_now,
                trusted_process: process_id.to_string(),
                last_server_iat: prior.last_server_iat,
            },
        };
    }

    let effective_now = wall_now.max(monotonic_now).max(prior.highest_seen);
    ClockDecision::Trusted {
        effective_now,
        next: ClockState {
            highest_seen: effective_now,
            trusted_process: process_id.to_string(),
            last_server_iat: prior.last_server_iat,
        },
    }
}

/// The current process's identity and monotonic reference.
///
/// Create exactly one per process (hold it in a `static`/`LazyLock`). `monotonic_now`
/// is `wall_at_start + Instant elapsed`; `Instant` does not follow wall-clock changes.
/// On some platforms `Instant` pauses during system sleep, so the offset can lag the
/// real wall clock after sleep; that only ever makes the offset smaller than wall time,
/// which [`reconcile_clock`] tolerates.
#[derive(Debug, Clone)]
pub struct ProcessAnchor {
    id: String,
    started: Instant,
    wall_at_start: i64,
}

impl ProcessAnchor {
    /// Anchor this process at `wall_now`. The id combines the OS pid, the start time and
    /// a random nonce, so two runs never share an id.
    pub fn new(wall_now: i64) -> Self {
        let nonce = RandomState::new().build_hasher().finish();
        Self::with_id(
            format!("{}-{wall_now}-{nonce:016x}", std::process::id()),
            wall_now,
        )
    }

    /// Anchor with an explicit id (tests, or hosts that already have a process identity).
    pub fn with_id(id: impl Into<String>, wall_now: i64) -> Self {
        Self {
            id: id.into(),
            started: Instant::now(),
            wall_at_start: wall_now,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `wall at start + monotonic seconds elapsed since start`.
    pub fn monotonic_now(&self) -> i64 {
        let elapsed = i64::try_from(self.started.elapsed().as_secs()).unwrap_or(i64::MAX);
        self.wall_at_start.saturating_add(elapsed)
    }
}

/// Seconds since the unix epoch from the system wall clock.
pub fn system_now_unix() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "device clock is before the Unix epoch".to_string())?
        .as_secs();
    i64::try_from(seconds).map_err(|error| error.to_string())
}

/// Where the ledger is persisted. Implement this over the keychain/credential store the
/// app already uses for the license record.
pub trait ClockStore {
    type Error;
    /// `Ok(None)` means no ledger exists; any read failure must be `Err` (never `None`),
    /// because a missing ledger on an initialised install is treated as tampering.
    fn load(&self) -> Result<Option<ClockState>, Self::Error>;
    fn save(&self, state: &ClockState) -> Result<(), Self::Error>;

    /// Replace the ledger with `new` only if it still equals `expected` (`None` meaning
    /// "no ledger"); return `Ok(false)` without writing when it changed. This is the write
    /// primitive [`AntiRollbackClock`] uses, so a writer that read the ledger earlier can
    /// never overwrite a newer one.
    ///
    /// The default is a plain load, compare, save: correct against every other user of
    /// the store inside this process (the clock serialises them), but **not atomic against
    /// another process**. A store shared between processes (for example one app's helper
    /// and main process using the same keychain item) should override this with a real
    /// atomic compare-and-swap or an OS-level lock, or accept the small window between
    /// the load and the save.
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        if self.load()?.as_ref() != expected {
            return Ok(false);
        }
        self.save(new)?;
        Ok(true)
    }
}

impl<T: ClockStore + ?Sized> ClockStore for &T {
    type Error = T::Error;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        (**self).load()
    }
    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        (**self).save(state)
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        (**self).compare_and_save(expected, new)
    }
}

/// In-memory [`ClockStore`]. For tests and ephemeral QA builds only: it does not survive
/// a restart, so it provides no rollback protection.
#[derive(Debug, Default)]
pub struct MemoryClockStore(Mutex<Option<ClockState>>);

impl MemoryClockStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Remove the ledger (simulates the secure-store entry being deleted).
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl ClockStore for MemoryClockStore {
    type Error = Infallible;
    fn load(&self) -> Result<Option<ClockState>, Infallible> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
    fn save(&self, state: &ClockState) -> Result<(), Infallible> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(state.clone());
        Ok(())
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Infallible> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.as_ref() != expected {
            return Ok(false);
        }
        *guard = Some(new.clone());
        Ok(true)
    }
}

/// Failure of a clock operation.
#[derive(Debug, PartialEq, Eq)]
pub enum ClockError<E> {
    /// The install was initialised (`was_initialized`) but its ledger is gone. Treated as
    /// tampering: the user must correct the clock and re-activate online.
    Missing,
    /// A server anchor that is not strictly newer than the last one (replay guard).
    ReplayedAnchor { last_server_iat: i64, got: i64 },
    /// The [`ClockStore`] failed.
    Storage(E),
    /// Another writer kept changing the ledger between this call's load and its write, so
    /// the write was abandoned after [`CLOCK_WRITE_ATTEMPTS`] tries. Nothing was changed
    /// by this call; retry.
    Contended,
}

impl<E: fmt::Display> fmt::Display for ClockError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "secure license clock is missing; correct the clock and reactivate online"
            ),
            Self::ReplayedAnchor {
                last_server_iat,
                got,
            } => write!(
                f,
                "The server returned an old or replayed license timestamp ({got} <= {last_server_iat})."
            ),
            Self::Storage(error) => write!(f, "license clock storage failed: {error}"),
            Self::Contended => write!(
                f,
                "license clock ledger is being changed by another writer; retry"
            ),
        }
    }
}
impl<E: fmt::Debug + fmt::Display> std::error::Error for ClockError<E> {}

/// One reconcile outcome, carrying what [`AntiRollbackClock::commit`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockReading {
    Trusted {
        /// Evaluate expiry at this time, never at the raw wall clock.
        effective_now: i64,
        next: ClockState,
        prior: Option<ClockState>,
    },
    Rollback {
        highest_seen: i64,
    },
}

/// Result of [`AntiRollbackClock::reanchor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reanchored {
    pub state: ClockState,
    /// The anchor was earlier than the previous high-water mark, so the mark moved
    /// backwards. This is the clock-repair path (see [`AntiRollbackClock::reanchor`]).
    pub lowered: bool,
}

/// The persisted anti-rollback clock.
///
/// ## Concurrency
/// Every load, decision and write on a ledger is serialised across all
/// `AntiRollbackClock`s in the process, and every write is a
/// [`ClockStore::compare_and_save`]. A writer therefore plans against the ledger as it is
/// at write time, never against a value it read earlier:
///
/// * a [`commit`](Self::commit) only ever moves the mark forward, and is dropped if a
///   newer server anchor has landed since its reading was taken;
/// * a [`reanchor`](Self::reanchor) is checked for replay against the current
///   `last_server_iat`, so it cannot overwrite a newer one.
///
/// Across processes the same holds as far as the store's `compare_and_save` is atomic.
///
/// ## Trusted time held in memory
/// Every trusted observation is remembered for the life of this clock, whether or not it
/// was written (writes are throttled). Reconciliation takes the larger of the stored mark
/// and the remembered one, so a throttled observation cannot be undone by a lower wall
/// reading. The memory is dropped when the stored ledger carries a *newer server anchor*
/// than the one the memory was built on (a [`reanchor`](Self::reanchor), possibly from
/// another process, supersedes it).
///
/// Create one clock per process. Several clocks over one ledger stay serialised and
/// forward-only, but each remembers only its own observations.
#[derive(Debug)]
pub struct AntiRollbackClock<S> {
    store: S,
    anchor: ProcessAnchor,
    /// Highest trusted state this clock has observed, persisted or not.
    memory: Mutex<Option<ClockState>>,
    /// Process time reference installed by a successful reanchor: `(trusted time, when)`.
    /// Replaces the [`ProcessAnchor`]'s reference for monotonic purposes.
    reference: Mutex<Option<(i64, Instant)>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Fold a throttled-away observation (`memory`) into what the store holds.
fn effective_prior(stored: Option<ClockState>, memory: Option<ClockState>) -> Option<ClockState> {
    match (stored, memory) {
        (stored, None) => stored,
        (None, memory) => memory,
        (Some(stored), Some(memory)) => {
            // A newer server anchor than the one the memory was built on supersedes it.
            if memory.last_server_iat < stored.last_server_iat
                || memory.highest_seen <= stored.highest_seen
            {
                Some(stored)
            } else {
                Some(ClockState {
                    highest_seen: memory.highest_seen,
                    trusted_process: memory.trusted_process,
                    last_server_iat: stored.last_server_iat,
                })
            }
        }
    }
}

/// What a commit of `next` should do to a ledger that currently holds `stored`: the state
/// to write, or `None` when there is nothing to move forward.
fn forward_only(stored: &ClockState, next: &ClockState) -> Option<ClockState> {
    // `next` was computed before a newer server anchor landed; applying it would roll the
    // replay guard back (and may carry a mark inflated by the state that anchor repaired).
    if next.last_server_iat < stored.last_server_iat {
        return None;
    }
    if next.highest_seen <= stored.highest_seen && next.last_server_iat == stored.last_server_iat {
        return None;
    }
    let (highest_seen, trusted_process) = if next.highest_seen > stored.highest_seen {
        (next.highest_seen, next.trusted_process.clone())
    } else {
        (stored.highest_seen, stored.trusted_process.clone())
    };
    Some(ClockState {
        highest_seen,
        trusted_process,
        last_server_iat: next.last_server_iat,
    })
}

impl<S: ClockStore> AntiRollbackClock<S> {
    pub fn new(store: S, anchor: ProcessAnchor) -> Self {
        Self {
            store,
            anchor,
            memory: Mutex::new(None),
            reference: Mutex::new(None),
        }
    }

    pub fn anchor(&self) -> &ProcessAnchor {
        &self.anchor
    }

    /// This process's monotonic time: the [`ProcessAnchor`]'s, unless a successful
    /// [`reanchor`](Self::reanchor) has replaced the reference with server time.
    pub fn monotonic_now(&self) -> i64 {
        match *locked(&self.reference) {
            Some((trusted, since)) => {
                let elapsed = i64::try_from(since.elapsed().as_secs()).unwrap_or(i64::MAX);
                trusted.saturating_add(elapsed)
            }
            None => self.anchor.monotonic_now(),
        }
    }

    /// Read the persisted ledger.
    pub fn load(&self) -> Result<Option<ClockState>, ClockError<S::Error>> {
        self.store.load().map_err(ClockError::Storage)
    }

    /// Decide what time to believe. Does **not** write; call [`Self::commit`] once the
    /// caller has decided the reading is worth persisting. It does record a trusted
    /// observation in this clock's memory (see the type docs).
    ///
    /// `was_initialized` is the license record's "clock initialised" flag. If it is set
    /// and the ledger has vanished, this returns [`ClockError::Missing`] rather than
    /// silently starting a fresh ledger (deleting the ledger must not reset rollback
    /// protection).
    pub fn reconcile(
        &self,
        wall_now: i64,
        was_initialized: bool,
    ) -> Result<ClockReading, ClockError<S::Error>> {
        let _guard = ledger_lock();
        let prior = self.load()?;
        if was_initialized && prior.is_none() {
            return Err(ClockError::Missing);
        }
        let known = effective_prior(prior.clone(), locked(&self.memory).clone());
        Ok(
            match reconcile_clock(
                wall_now,
                self.monotonic_now(),
                self.anchor.id(),
                known.as_ref(),
            ) {
                ClockDecision::Trusted {
                    effective_now,
                    next,
                } => {
                    *locked(&self.memory) = Some(next.clone());
                    ClockReading::Trusted {
                        effective_now,
                        next,
                        prior,
                    }
                }
                ClockDecision::Rollback { highest_seen } => ClockReading::Rollback { highest_seen },
            },
        )
    }

    /// Persist a trusted reading. A rollback reading is never persisted.
    ///
    /// The write is planned against the ledger **as it is now**, not as the reading saw
    /// it: it moves the mark forward only, keeps the stored `last_server_iat` when that is
    /// newer, and does nothing when a server anchor landed after the reading was taken.
    /// Throttled: skipped when the process identity and server anchor are unchanged and
    /// the mark would move less than [`CLOCK_WRITE_INTERVAL_SECS`] (the observation stays
    /// in memory regardless).
    pub fn commit(&self, reading: &ClockReading) -> Result<(), ClockError<S::Error>> {
        let ClockReading::Trusted { next, .. } = reading else {
            return Ok(());
        };
        let _guard = ledger_lock();
        for _ in 0..CLOCK_WRITE_ATTEMPTS {
            let stored = self.load()?;
            let to_write = match &stored {
                None => next.clone(),
                Some(stored) => {
                    let Some(to_write) = forward_only(stored, next) else {
                        return Ok(());
                    };
                    let identity_changed = to_write.trusted_process != stored.trusted_process;
                    let server_iat_changed = to_write.last_server_iat != stored.last_server_iat;
                    let interval_elapsed = to_write.highest_seen
                        >= stored
                            .highest_seen
                            .saturating_add(CLOCK_WRITE_INTERVAL_SECS);
                    if !identity_changed && !server_iat_changed && !interval_elapsed {
                        return Ok(());
                    }
                    to_write
                }
            };
            if self
                .store
                .compare_and_save(stored.as_ref(), &to_write)
                .map_err(ClockError::Storage)?
            {
                return Ok(());
            }
        }
        Err(ClockError::Contended)
    }

    /// Re-anchor on server time: `server_time` is the `iat` of a token the caller has
    /// **already signature-verified** (and judged for app, device and lifecycle;
    /// [`crate::refresh_and_reanchor`] does all of that before calling here). Always
    /// persists (no throttle).
    ///
    /// ## The rule
    /// Time may be set *back* only to a server-attested time that is strictly newer than
    /// every earlier server attestation, and a repair affects the whole clock:
    ///
    /// 1. **Replay guard.** Against the ledger as it is at write time, `server_time` must
    ///    be `> last_server_iat`, otherwise [`ClockError::ReplayedAnchor`] and nothing
    ///    changes. A concurrent newer anchor is therefore never overwritten, and trusted
    ///    time never goes back past a value that was itself server-attested (the mark is
    ///    always `>= last_server_iat`).
    /// 2. **Mark.** With an existing ledger the high-water mark becomes `server_time`
    ///    **even when that is lower than the previous mark**. This is the repair path for
    ///    a ledger inflated by a clock that was once set far ahead
    ///    ([`Reanchored::lowered`]).
    /// 3. **Process reference.** On success this clock's in-memory observations are
    ///    replaced by the new state and its monotonic reference is re-based on the new
    ///    mark, so the old, possibly too-far-ahead process anchor can no longer pull
    ///    reconciliation back up. A failed reanchor (replay, missing ledger, storage
    ///    error) changes neither the ledger nor the reference.
    ///
    /// 4. **No ledger.** With `was_initialized`: [`ClockError::Missing`] (fail closed).
    ///    Otherwise the ledger starts at `max(wall_now, server_time)`.
    ///
    /// Other clocks in the same process lose their memory on their next reconcile (a newer
    /// server anchor supersedes it) but keep their own process reference; a process that
    /// did not perform the reanchor should be restarted to drop an inflated reference.
    pub fn reanchor(
        &self,
        server_time: i64,
        wall_now: i64,
        was_initialized: bool,
    ) -> Result<Reanchored, ClockError<S::Error>> {
        let _guard = ledger_lock();
        for _ in 0..CLOCK_WRITE_ATTEMPTS {
            let prior = self.load()?;
            let (state, lowered) = match &prior {
                Some(prior) => {
                    if server_time <= prior.last_server_iat {
                        return Err(ClockError::ReplayedAnchor {
                            last_server_iat: prior.last_server_iat,
                            got: server_time,
                        });
                    }
                    (
                        ClockState {
                            highest_seen: server_time,
                            trusted_process: self.anchor.id().to_string(),
                            last_server_iat: server_time,
                        },
                        server_time < prior.highest_seen,
                    )
                }
                None if was_initialized => return Err(ClockError::Missing),
                None => (
                    ClockState {
                        highest_seen: wall_now.max(server_time),
                        trusted_process: self.anchor.id().to_string(),
                        last_server_iat: server_time,
                    },
                    false,
                ),
            };
            if self
                .store
                .compare_and_save(prior.as_ref(), &state)
                .map_err(ClockError::Storage)?
            {
                *locked(&self.memory) = Some(state.clone());
                *locked(&self.reference) = Some((state.highest_seen, Instant::now()));
                return Ok(Reanchored { state, lowered });
            }
        }
        Err(ClockError::Contended)
    }
}

/// Why a [`TrustedClock`] declined to give a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockRefusal {
    /// The wall clock is behind the persisted high-water mark.
    Rollback { highest_seen: i64 },
    /// The ledger of an initialised install is missing.
    Missing,
    /// The ledger store failed.
    Storage(String),
    /// The underlying clock could not be read.
    Unavailable(String),
}

impl fmt::Display for ClockRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rollback { highest_seen } => write!(
                f,
                "device clock moved backwards or stopped advancing after restart (last trusted time {highest_seen})"
            ),
            Self::Missing => write!(
                f,
                "secure license clock is missing; correct the clock and reactivate online"
            ),
            Self::Storage(error) => write!(f, "license clock storage failed: {error}"),
            Self::Unavailable(error) => write!(f, "license clock unavailable: {error}"),
        }
    }
}
impl std::error::Error for ClockRefusal {}

/// The hook an authorization layer consults for "what time is it" instead of reading the
/// wall clock itself.
///
/// - [`SystemClock`] returns the wall clock (today's behaviour).
/// - [`AntiRollbackClock`] returns the reconciled time and refuses on rollback.
///
/// Object safe, so a host can hold a `&dyn TrustedClock`.
pub trait TrustedClock {
    /// `wall_now` is the current wall clock; `clock_was_initialized` is the license
    /// record's "clock initialised" flag (see [`AntiRollbackClock::reconcile`]).
    fn trusted_now(&self, wall_now: i64, clock_was_initialized: bool) -> Result<i64, ClockRefusal>;
}

/// Wall-clock [`TrustedClock`]: no rollback protection.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl TrustedClock for SystemClock {
    fn trusted_now(&self, wall_now: i64, _: bool) -> Result<i64, ClockRefusal> {
        Ok(wall_now)
    }
}

impl<S> TrustedClock for AntiRollbackClock<S>
where
    S: ClockStore,
    S::Error: fmt::Display,
{
    fn trusted_now(&self, wall_now: i64, clock_was_initialized: bool) -> Result<i64, ClockRefusal> {
        let reading = self
            .reconcile(wall_now, clock_was_initialized)
            .map_err(clock_refusal)?;
        match reading {
            ClockReading::Trusted { effective_now, .. } => {
                self.commit(&reading).map_err(clock_refusal)?;
                Ok(effective_now)
            }
            ClockReading::Rollback { highest_seen } => Err(ClockRefusal::Rollback { highest_seen }),
        }
    }
}

fn clock_refusal<E: fmt::Display>(error: ClockError<E>) -> ClockRefusal {
    match error {
        ClockError::Missing => ClockRefusal::Missing,
        ClockError::Storage(error) => ClockRefusal::Storage(error.to_string()),
        other @ (ClockError::ReplayedAnchor { .. } | ClockError::Contended) => {
            ClockRefusal::Unavailable(other.to_string())
        }
    }
}
