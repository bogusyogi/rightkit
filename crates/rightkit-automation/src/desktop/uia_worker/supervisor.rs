//! Worker supervision: pure bookkeeping for the persistent UIA worker and the
//! workers that were abandoned because a provider call never returned.
//!
//! A thread blocked inside another application's UI Automation provider cannot
//! be cancelled or killed from Rust. The only bound that always holds is to
//! stop waiting for that thread: mark it abandoned, never send it work again,
//! start a fresh worker for later calls, and cap how many abandoned threads may
//! be alive at once. This module holds that state machine and the deadline
//! arithmetic with no COM, no threads and no clock of its own (every method
//! takes `now`), so the decisions are unit-testable without a Windows session.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest single request deadline accepted. A larger `timeout` is clamped so
/// `Instant + Duration` can never overflow and a caller cannot ask for an
/// effectively unbounded wait.
pub(crate) const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Deadline for a request starting at `now`; `None` for a zero timeout (the
/// caller fails closed without queueing) or an unrepresentable instant.
pub(crate) fn deadline_after(now: Instant, timeout: Duration) -> Option<Instant> {
    if timeout.is_zero() {
        return None;
    }
    now.checked_add(timeout.min(MAX_REQUEST_TIMEOUT))
}

/// Time left until `deadline`, zero once it has passed.
pub(crate) fn remaining(deadline: Instant, now: Instant) -> Duration {
    deadline.saturating_duration_since(now)
}

/// True once `now` is at least `grace` past `deadline`. An overflowing sum is
/// treated as not overdue (never abandons on arithmetic failure).
pub(crate) fn overdue(deadline: Instant, grace: Duration, now: Instant) -> bool {
    deadline
        .checked_add(grace)
        .is_some_and(|limit| now >= limit)
}

/// `now + d` with `d` clamped to `MAX_REQUEST_TIMEOUT`; `None` only when even
/// the clamped sum is unrepresentable. The one place a host `Duration` meets an
/// `Instant` for a wait or a deadline (zero is allowed here, unlike
/// `deadline_after`).
pub(crate) fn add_clamped(now: Instant, d: Duration) -> Option<Instant> {
    now.checked_add(d.min(MAX_REQUEST_TIMEOUT))
}

/// May a probe that sleeps `delay` and then runs again still finish before
/// `deadline`? False on overflow (never retries on arithmetic failure).
pub(crate) fn retry_fits(now: Instant, delay: Duration, deadline: Instant) -> bool {
    add_clamped(now, delay).is_some_and(|at| at < deadline)
}

/// End of a bounded walk: the earlier of the request deadline and `now +
/// budget`. An unrepresentable `now + budget` leaves the request deadline.
pub(crate) fn walk_end(now: Instant, budget: Duration, deadline: Instant) -> Instant {
    add_clamped(now, budget).map_or(deadline, |end| end.min(deadline))
}

/// Pin tokens are `handle (16 bits) | generation (24 bits) | seq (24 bits)`.
/// `handle` is the per-`Uia`-handle id, so a token minted by another handle is
/// refused; `generation` is the worker generation that captured the pin, so a
/// token is valid on that generation only; `seq` is a per-generation counter
/// that never wraps (the capture is refused when it is spent) and is never
/// zero. Every field is non-zero, so a token is never zero. The checks run
/// here, before any request is sent, so a stale or foreign token never reaches
/// a worker.
pub(crate) const FIELD_BITS: u32 = 24;
pub(crate) const MAX_SEQ: u64 = (1 << FIELD_BITS) - 1;

/// Highest generation a token can carry; `admit` refuses to spawn past it.
pub(crate) const MAX_GENERATION: u64 = (1 << FIELD_BITS) - 1;

/// Highest handle id a token can carry.
pub(crate) const MAX_HANDLE: u64 = 0xFFFF;

/// Build a pin token. `None` when any field is zero or out of range (never a
/// clamped or wrapped token).
pub(crate) fn make_token(handle: u64, generation: u64, seq: u64) -> Option<u64> {
    if handle == 0
        || handle > MAX_HANDLE
        || generation == 0
        || generation > MAX_GENERATION
        || seq == 0
        || seq > MAX_SEQ
    {
        return None;
    }
    Some((handle << (2 * FIELD_BITS)) | (generation << FIELD_BITS) | seq)
}

/// Handle id a token was minted by; 0 for token 0 and any malformed token
/// (handle 0 never exists).
pub(crate) fn token_handle(token: u64) -> u64 {
    token >> (2 * FIELD_BITS)
}

/// Generation a token was minted on; 0 for token 0 and any malformed token
/// (generation 0 never exists, so it is never current).
pub(crate) fn token_generation(token: u64) -> u64 {
    (token >> FIELD_BITS) & MAX_GENERATION
}

/// Allocate a per-handle id from `counter`; `None` once the 16-bit space is
/// spent (the handle is then created closed, every call fails closed). The
/// counter stops at `MAX_HANDLE`: it is never incremented past it, so it cannot
/// wrap and hand an id out twice.
pub(crate) fn mint_handle_id(counter: &std::sync::atomic::AtomicU32) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
            (u64::from(used) < MAX_HANDLE).then(|| used + 1)
        })
        .ok()
        .map(|previous| u64::from(previous) + 1)
}

/// First wait after a worker failed to initialise (UIA instance creation); it
/// doubles per consecutive failure up to `INIT_BACKOFF_MAX`. Calls inside the
/// wait fail fast and spawn no thread.
const INIT_BACKOFF_BASE: Duration = Duration::from_millis(500);
const INIT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Wait before the next spawn after `failures` consecutive init failures.
fn init_backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(6);
    INIT_BACKOFF_BASE
        .saturating_mul(1u32 << doublings)
        .min(INIT_BACKOFF_MAX)
}

/// One worker abandoned; reported through the event sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Abandonment {
    pub(crate) generation: u64,
    /// True when the worker never finished starting (COM / UIA initialisation).
    pub(crate) during_init: bool,
    /// Abandoned workers still alive after this one was added.
    pub(crate) live: usize,
    /// Abandoned workers over the life of the handle.
    pub(crate) total: u64,
    /// How far past its deadline the in-flight request was (zero when forced).
    pub(crate) overdue_by: Duration,
    /// Pins the abandoned generation owned and had not yet released (in its
    /// store, or taken out for a release window that never finished). The
    /// caller adds them to the process-wide leak count at this moment: they
    /// are forgotten for good, whether or not the thread ever returns.
    pub(crate) pins: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Too many abandoned workers are still blocked; fail fast, spawn nothing.
    /// `first` is true only for the first refusal since the last recovery, so
    /// the event sink is told once, not on every call.
    Exhausted { first: bool },
    /// `shutdown` ran (or the generation space is spent); no more work.
    Closed,
    /// A pin token from another worker generation. Never sent anywhere; the
    /// caller returns its fail-closed value.
    StalePin,
    /// A pin token minted by a different `Uia` handle. Same handling.
    ForeignHandle,
    /// The last worker failed to initialise (its UIA instance could not be
    /// created) and the backoff before the next attempt has not elapsed.
    InitBackoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Send to the running worker of this generation.
    Use(u64),
    /// No worker is running: the caller must spawn one for this generation.
    Spawn(u64),
    Refuse(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    /// The current worker left on its own (channel closed by shutdown).
    Clean,
    /// An abandoned worker finally returned and left; `live` remain.
    Recovered { live: usize },
    /// Nothing to account for.
    Stale,
}

/// Answer of `Supervisor::begin`: may this worker start a unit of work?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Begin {
    /// Start it; the supervisor now knows its deadline.
    Run,
    /// Its deadline already passed while it was queued: do not start it. The
    /// caller has given up, so nothing is answered and nothing is recorded.
    Expired,
    /// The worker was abandoned, replaced, closed or is still starting: leave.
    Abandoned,
}

/// Why a side effect was not issued. Reported through the event sink as
/// `uia_effect_not_issued`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The worker generation was abandoned, replaced or closed.
    Abandoned,
    /// The request deadline had passed.
    Deadline,
    /// The caller already stopped waiting and cancelled the effect.
    CallerGone,
}

impl SkipReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SkipReason::Abandoned => "abandoned",
            SkipReason::Deadline => "deadline",
            SkipReason::CallerGone => "caller_gone",
        }
    }
}

const GATE_PENDING: u8 = 0;
const GATE_ISSUED: u8 = 1;
const GATE_CANCELLED: u8 = 2;

/// One-shot handshake between a caller and the worker for ONE externally
/// visible side effect. Exactly one of two things happens, atomically: the
/// worker `claim`s it (and then issues the call: "issued") or the caller
/// `cancel`s it (the call will never be made: "not issued"). A caller that
/// stops waiting uses the result to tell "not issued" from "issued, outcome
/// unknown", which a plain timeout cannot.
#[derive(Clone, Debug, Default)]
pub(crate) struct EffectGate(Arc<AtomicU8>);

impl EffectGate {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicU8::new(GATE_PENDING)))
    }

    /// Worker side, immediately before the effect call. True means the caller
    /// has not cancelled and the worker may (and from now on is deemed to) issue.
    pub(crate) fn claim(&self) -> bool {
        self.0
            .compare_exchange(
                GATE_PENDING,
                GATE_ISSUED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Caller side, once it will wait no longer. True when no effect was and
    /// none ever will be issued; false when the worker already claimed it.
    pub(crate) fn cancel(&self) -> bool {
        match self.0.compare_exchange(
            GATE_PENDING,
            GATE_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(current) => current == GATE_CANCELLED,
        }
    }

    #[allow(dead_code)] // asserted by the unit tests
    pub(crate) fn issued(&self) -> bool {
        self.0.load(Ordering::Acquire) == GATE_ISSUED
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InFlight {
    deadline: Instant,
}

pub(crate) struct Supervisor {
    /// This handle's id, carried by every token it mints.
    handle: u64,
    max_abandoned: usize,
    grace: Duration,
    init_budget: Duration,
    generation: u64,
    running: bool,
    /// The running worker has not yet reported `worker_ready_for` (COM
    /// apartment, UIA instance, provider timeouts). It serves nothing while
    /// starting.
    starting: bool,
    init_deadline: Option<Instant>,
    in_flight: Option<InFlight>,
    abandoned_live: usize,
    abandoned_total: u64,
    closed: bool,
    exhausted_reported: bool,
    /// Pins the running generation owns and has not released: incremented by
    /// `pin_added` when a capture succeeds, decremented by `pins_released` after
    /// a release window finished. Read at abandonment (`Abandonment::pins`).
    pins_owned: usize,
    /// Consecutive workers that failed to initialise; reset by a ready worker.
    init_failures: u32,
    /// No worker is spawned before this instant (set by `note_init_failure`).
    retry_at: Option<Instant>,
}

impl Supervisor {
    /// `max_abandoned` is floored at 1: zero would refuse every call.
    pub(crate) fn new(
        handle: u64,
        max_abandoned: usize,
        grace: Duration,
        init_budget: Duration,
    ) -> Self {
        Self {
            handle,
            max_abandoned: max_abandoned.max(1),
            grace,
            init_budget: init_budget.min(MAX_REQUEST_TIMEOUT),
            generation: 0,
            running: false,
            starting: false,
            init_deadline: None,
            in_flight: None,
            abandoned_live: 0,
            abandoned_total: 0,
            closed: false,
            exhausted_reported: false,
            pins_owned: 0,
            init_failures: 0,
            retry_at: None,
        }
    }

    pub(crate) fn worker_running(&self) -> bool {
        self.running
    }

    /// A worker exists and has finished initialising.
    pub(crate) fn worker_ready(&self) -> bool {
        self.running && !self.starting
    }

    /// A worker exists but is still initialising (not a healthy worker yet).
    #[allow(dead_code)] // asserted by the unit tests; `Health` reports `worker_ready`
    pub(crate) fn worker_starting(&self) -> bool {
        self.running && self.starting
    }

    /// True when `token` was minted by the worker that is running now.
    pub(crate) fn pin_is_current(&self, token: u64) -> bool {
        self.running
            && !self.starting
            && token_handle(token) == self.handle
            && token_generation(token) == self.generation
    }

    /// `generation` is the running, ready, not-closed worker. False for an
    /// abandoned, replaced or shut-down generation.
    pub(crate) fn is_live(&self, generation: u64) -> bool {
        self.running && !self.starting && !self.closed && self.generation == generation
    }

    /// THE gate for every externally visible side effect (focus, value set).
    /// Called by the worker immediately before the call, under the same lock
    /// that every abandonment takes, so "abandoned" and "claimed" cannot both
    /// happen: either the abandonment came first and the effect is skipped, or
    /// the claim came first and the effect counts as issued. Order: liveness,
    /// deadline, then the caller handshake (last, so nothing is marked issued
    /// when the effect is skipped).
    pub(crate) fn permit_effect(
        &self,
        generation: u64,
        deadline: Instant,
        now: Instant,
        gate: &EffectGate,
    ) -> Result<(), SkipReason> {
        if !self.is_live(generation) {
            Err(SkipReason::Abandoned)
        } else if now >= deadline {
            Err(SkipReason::Deadline)
        } else if !gate.claim() {
            Err(SkipReason::CallerGone)
        } else {
            Ok(())
        }
    }

    pub(crate) fn abandoned_live(&self) -> usize {
        self.abandoned_live
    }

    pub(crate) fn abandoned_total(&self) -> u64 {
        self.abandoned_total
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn is_exhausted(&self) -> bool {
        !self.running && self.abandoned_live >= self.max_abandoned
    }

    /// The generation space is spent: no worker is running and `admit` refuses
    /// every call (as `Refusal::Closed`) because the next generation would not fit
    /// in a token.
    pub(crate) fn generations_spent(&self) -> bool {
        !self.running && self.generation >= MAX_GENERATION
    }

    /// The last worker failed to initialise and the backoff has not elapsed.
    pub(crate) fn in_init_backoff(&self, now: Instant) -> bool {
        !self.running && self.retry_at.is_some_and(|at| now < at)
    }

    /// Worker side: the worker of `generation` could not create its UIA instance
    /// and is about to leave. Records the failure and starts the backoff; returns
    /// the consecutive failure count and the wait. `None` when that generation
    /// is not the starting worker (abandoned meanwhile: nothing to record).
    pub(crate) fn note_init_failure(
        &mut self,
        generation: u64,
        now: Instant,
    ) -> Option<(u32, Duration)> {
        if !(self.running && self.starting && self.generation == generation) {
            return None;
        }
        self.init_failures = self.init_failures.saturating_add(1);
        let delay = init_backoff(self.init_failures);
        self.retry_at = Some(now.checked_add(delay).unwrap_or(now));
        Some((self.init_failures, delay))
    }

    /// Worker side: a capture produced a pin. False when `generation` is not the
    /// live generation (abandoned, replaced, closed): the pin is then NOT owned
    /// here and the caller disposes of the element itself. The check and the
    /// increment are one step under the lock every abandonment takes, so a pin is
    /// counted either by this increment (and then by the abandonment snapshot) or
    /// by its caller, never both and never neither.
    pub(crate) fn pin_added(&mut self, generation: u64) -> bool {
        if !self.is_live(generation) {
            return false;
        }
        self.pins_owned += 1;
        true
    }

    /// Worker side: `count` pins left the worker's ownership (a release window
    /// finished, or they were leaked and counted by the caller). Ignored for a
    /// generation that is no longer the running one: its pins were already taken
    /// into account at abandonment.
    pub(crate) fn pins_released(&mut self, generation: u64, count: usize) {
        if self.running && self.generation == generation {
            self.pins_owned = self.pins_owned.saturating_sub(count);
        }
    }

    /// May the worker of `generation` call `Release` on a COM reference? True
    /// while it is the running generation, including after `close` (a clean
    /// close releases inside a window); false once it was abandoned or replaced.
    pub(crate) fn may_release(&self, generation: u64) -> bool {
        self.running && self.generation == generation
    }

    /// Abandon the current worker when its in-flight request is past its
    /// deadline plus the grace period. Called before admitting a request and
    /// when a caller times out, so a hung worker is replaced by the next call
    /// instead of every later call queueing behind it.
    pub(crate) fn reap_overrun(&mut self, now: Instant) -> Option<Abandonment> {
        if !self.running {
            return None;
        }
        // A worker stuck before its first request is bounded the same way as
        // one stuck inside a request: past its init deadline plus grace.
        if self.starting {
            let deadline = self.init_deadline?;
            if !overdue(deadline, self.grace, now) {
                return None;
            }
            let overdue_by = now.saturating_duration_since(deadline);
            return Some(self.abandon(overdue_by));
        }
        let flight = self.in_flight?;
        if !overdue(flight.deadline, self.grace, now) {
            return None;
        }
        let overdue_by = now.saturating_duration_since(flight.deadline);
        Some(self.abandon(overdue_by))
    }

    /// Abandon the current worker unconditionally (shutdown budget spent).
    pub(crate) fn abandon_current(&mut self) -> Option<Abandonment> {
        if !self.running {
            return None;
        }
        Some(self.abandon(Duration::ZERO))
    }

    fn abandon(&mut self, overdue_by: Duration) -> Abandonment {
        let during_init = self.starting;
        let pins = std::mem::take(&mut self.pins_owned);
        self.running = false;
        self.starting = false;
        self.init_deadline = None;
        self.in_flight = None;
        self.abandoned_live += 1;
        self.abandoned_total += 1;
        Abandonment {
            generation: self.generation,
            during_init,
            live: self.abandoned_live,
            total: self.abandoned_total,
            overdue_by,
            pins,
        }
    }

    /// `pin` is the token of a retained-pin request, if any. A pin from another
    /// generation is refused before anything is spawned or sent: a replacement
    /// worker is never handed a token it did not mint.
    pub(crate) fn admit(&mut self, now: Instant, pin: Option<u64>) -> Admit {
        if self.closed {
            return Admit::Refuse(Refusal::Closed);
        }
        if let Some(token) = pin {
            return if token_handle(token) != self.handle {
                Admit::Refuse(Refusal::ForeignHandle)
            } else if self.pin_is_current(token) {
                Admit::Use(self.generation)
            } else {
                Admit::Refuse(Refusal::StalePin)
            };
        }
        if self.running {
            return Admit::Use(self.generation);
        }
        if self.abandoned_live >= self.max_abandoned {
            let first = !self.exhausted_reported;
            self.exhausted_reported = true;
            return Admit::Refuse(Refusal::Exhausted { first });
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return Admit::Refuse(Refusal::InitBackoff);
        }
        if self.generation >= MAX_GENERATION {
            return Admit::Refuse(Refusal::Closed);
        }
        self.generation += 1;
        self.running = true;
        self.starting = true;
        self.init_deadline = Some(now.checked_add(self.init_budget).unwrap_or(now));
        self.in_flight = None;
        self.pins_owned = 0;
        Admit::Spawn(self.generation)
    }

    /// The thread for `generation` could not be started.
    pub(crate) fn spawn_failed(&mut self, generation: u64) {
        if self.running && self.generation == generation {
            self.running = false;
            self.starting = false;
            self.init_deadline = None;
            self.in_flight = None;
        }
    }

    /// Worker side: initialisation finished. False when this worker was
    /// abandoned (or replaced) while it was starting; it must exit without
    /// serving anything.
    pub(crate) fn worker_ready_for(&mut self, generation: u64) -> bool {
        if !self.running || self.generation != generation {
            return false;
        }
        self.starting = false;
        self.init_deadline = None;
        self.init_failures = 0;
        self.retry_at = None;
        true
    }

    /// Worker side: about to run a unit of work (a request, or a release /
    /// prune window: anything that can call into a provider or release a COM
    /// pointer). EVERY such unit carries a deadline and is recorded here, so
    /// `reap_overrun` can abandon a worker hung in any of them. Abandoned or
    /// replaced workers must exit without touching UIA again, which also stops
    /// an abandoned worker from running a queued mutation (Focus). A unit whose
    /// deadline already passed in the queue is `Expired`: not started, so it
    /// can neither run late nor be mistaken for an overrun.
    pub(crate) fn begin(&mut self, generation: u64, deadline: Instant, now: Instant) -> Begin {
        if !self.running || self.starting || self.closed || self.generation != generation {
            return Begin::Abandoned;
        }
        if now >= deadline {
            return Begin::Expired;
        }
        self.in_flight = Some(InFlight { deadline });
        Begin::Run
    }

    /// Worker side: about to release COM references (a release / prune window or
    /// the exit drain). Same as `begin` except that a CLOSED handle still admits
    /// it while the worker is the running generation: a clean close (`shutdown`,
    /// `Drop`) must release inside a supervised, deadline-bound window instead of
    /// leaking. `Abandoned` means the generation was abandoned or replaced: its
    /// pins were counted as leaked at that moment and must not be released.
    pub(crate) fn begin_release(
        &mut self,
        generation: u64,
        deadline: Instant,
        now: Instant,
    ) -> Begin {
        if !self.running || self.starting || self.generation != generation {
            return Begin::Abandoned;
        }
        if now >= deadline {
            return Begin::Expired;
        }
        self.in_flight = Some(InFlight { deadline });
        Begin::Run
    }

    pub(crate) fn finish(&mut self, generation: u64) {
        if self.running && self.generation == generation {
            self.in_flight = None;
        }
    }

    pub(crate) fn worker_exited(&mut self, generation: u64) -> Exit {
        if self.running && self.generation == generation {
            self.running = false;
            self.starting = false;
            self.init_deadline = None;
            self.in_flight = None;
            // The thread released or leaked (and counted) its own pins on the way
            // out; nothing remains owned.
            self.pins_owned = 0;
            return Exit::Clean;
        }
        if generation <= self.generation && self.abandoned_live > 0 {
            self.abandoned_live -= 1;
            if self.abandoned_live < self.max_abandoned {
                self.exhausted_reported = false;
            }
            return Exit::Recovered {
                live: self.abandoned_live,
            };
        }
        Exit::Stale
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
    }
}
