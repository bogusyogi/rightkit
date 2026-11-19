//! Pure lifecycle decisions for the input hooks: generations, closable lanes, the resolver cap,
//! interruptible waits, the toggle guard, the dispatch tracker and the per-press ledgers.
//!
//! Platform-neutral on purpose: every rule that decides "may this thread still act?" lives here;
//! the Windows hook modules and the macOS holders only call it. Crate-internal (`pub(crate)` module,
//! not a stable API).
//!
//! The one idea: everything an install starts belongs to ONE generation. Uninstall ends the
//! generation. A thread that outlives its generation (a worker draining, a resolver blocked in a
//! provider call, a timer asleep) holds only handles that are closed or dead for that generation,
//! so it can finish but cannot act on the state a later install created. Nothing here joins a
//! thread: a thread that may be blocked in a provider or host callback is detached, never awaited.
//!
//! What the hook threads need from here: a lock-free, bounded [`Lane`] (a hook procedure may send on
//! it without waiting; [`Lane::send_end`] has a reserve so an end can follow a queued begin),
//! [`bounded_call`] (a call that can wait on another process runs on a detached thread behind a
//! deadline, counted against a [`HungCallCap`] only once it is past that deadline) and a
//! [`PressLedger`] that tracks every swallowed key, not one.

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvError, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::held_click::{lane_state, LaneState, Lookup, UnknownCause};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A process-unique, never-zero generation number. Zero means "no generation".
pub fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::SeqCst)
}

/// Lock, ignoring poison (a panicking host callback must not wedge the lifecycle).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Generation token
// ---------------------------------------------------------------------------------------------

/// "Is this generation still the live one?" Shared by every thread of one install. Once ended it
/// never becomes live again; a later install makes a new one.
#[derive(Debug)]
pub struct Liveness {
    id: u64,
    live: AtomicBool,
}

impl Liveness {
    pub fn begin() -> Arc<Self> {
        Arc::new(Self {
            id: next_generation(),
            live: AtomicBool::new(true),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    /// End the generation. `true` for the call that ended it.
    pub fn end(&self) -> bool {
        self.live.swap(false, Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------
// Closable lane: a channel sender that can be shut for every holder at once
// ---------------------------------------------------------------------------------------------

enum Slot<T> {
    Item(T),
    /// Sent once by [`Lane::close`]: the receiver ends here, whoever still holds the lane.
    Close,
}

/// A channel that can be shut for every holder of the lane at once, and whose `send` is safe to
/// call from a low-level hook procedure: no lock, no wait, at most one allocation.
///
/// A thread that kept an `Arc<Lane>` from an ended generation (an arm timer, a late resolver) can
/// no longer send: `send` returns `false` after `close`. Closing queues one end marker, so the
/// receiver sees everything queued before it and then the end, however many `Arc<Lane>` clones are
/// still alive (the lane itself owns the only sender; it is never dropped while a clone lives, so
/// the end must be a marker, not a disconnect).
///
/// Bounded: at most `cap` items wait for the receiver. When the receiver is that far behind (a
/// host callback stalled for dozens of events) `send` drops the NEWEST item, returns `false` and
/// counts it ([`Lane::take_dropped`]) so the owner can report it. Items sent by a thread that raced
/// `close` and were queued behind the end marker are not delivered; callers that must not lose
/// such an item stop sending before they close (the hooks are removed first).
pub struct Lane<T> {
    open: AtomicBool,
    depth: Arc<AtomicUsize>,
    cap: usize,
    /// Extra depth only [`Lane::send_end`] may use (0 for a lane without one).
    reserve: usize,
    dropped: AtomicU64,
    tx: Sender<Slot<T>>,
}

/// The receiving end of a [`Lane`]. Mirrors the `std::sync::mpsc::Receiver` methods the code uses.
pub struct LaneRx<T> {
    rx: Receiver<Slot<T>>,
    depth: Arc<AtomicUsize>,
    ended: Cell<bool>,
}

impl<T> Lane<T> {
    pub fn with_depth(cap: usize) -> (Self, LaneRx<T>) {
        Self::with_depth_and_reserve(cap, 0)
    }

    /// A lane whose [`Lane::send`] holds at most `cap` items and whose [`Lane::send_end`] may go
    /// `reserve` items beyond that, so an item that ENDS something a queued item began can still be
    /// queued when ordinary items are being dropped (see [`Lane::send_end`]).
    pub fn with_depth_and_reserve(cap: usize, reserve: usize) -> (Self, LaneRx<T>) {
        let (tx, rx) = mpsc::channel();
        let depth = Arc::new(AtomicUsize::new(0));
        (
            Self {
                open: AtomicBool::new(true),
                depth: Arc::clone(&depth),
                cap: cap.max(1),
                reserve,
                dropped: AtomicU64::new(0),
                tx,
            },
            LaneRx {
                rx,
                depth,
                ended: Cell::new(false),
            },
        )
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// `true` when the item was queued; `false` when the lane is closed, full (counted) or the
    /// receiver is gone. Never blocks and takes no lock.
    pub fn send(&self, value: T) -> bool {
        self.push(value, self.cap)
    }

    /// [`Lane::send`] for an item that ENDS something an earlier queued item began (a stop or cancel
    /// for a queued start). It may use the lane's reserve, so it is refused only when the lane is
    /// closed or full beyond `cap + reserve`.
    ///
    /// The caller must queue at most one end per queued begin. Then the reserve suffices: ordinary
    /// items are accepted only while the depth is below `cap`, so the only items that can sit above
    /// `cap` are ends of begins that were accepted at a depth of at most `cap`, and a host that
    /// pairs them (one open begin per kind) needs a reserve of at most one per open kind.
    pub fn send_end(&self, value: T) -> bool {
        self.push(value, self.cap + self.reserve)
    }

    fn push(&self, value: T, limit: usize) -> bool {
        if !self.open.load(Ordering::SeqCst) {
            return false;
        }
        if self.depth.fetch_add(1, Ordering::SeqCst) >= limit {
            self.depth.fetch_sub(1, Ordering::SeqCst);
            self.dropped.fetch_add(1, Ordering::SeqCst);
            return false;
        }
        if self.tx.send(Slot::Item(value)).is_err() {
            self.depth.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Items dropped because the lane was full since the last call.
    pub fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::SeqCst)
    }

    /// Close the lane. `true` for the call that closed it.
    pub fn close(&self) -> bool {
        let was_open = self.open.swap(false, Ordering::SeqCst);
        if was_open {
            let _ = self.tx.send(Slot::Close);
        }
        was_open
    }
}

impl<T> LaneRx<T> {
    fn unwrap_slot(&self, slot: Slot<T>) -> Option<T> {
        match slot {
            Slot::Item(value) => {
                self.depth.fetch_sub(1, Ordering::SeqCst);
                Some(value)
            }
            Slot::Close => {
                self.ended.set(true);
                None
            }
        }
    }

    /// The next item; `Err` once the lane ended (closed and drained, or its owner dropped).
    pub fn recv(&self) -> Result<T, RecvError> {
        if self.ended.get() {
            return Err(RecvError);
        }
        match self.rx.recv() {
            Ok(slot) => self.unwrap_slot(slot).ok_or(RecvError),
            Err(_) => {
                self.ended.set(true);
                Err(RecvError)
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        if self.ended.get() {
            return Err(RecvTimeoutError::Disconnected);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(slot) => self.unwrap_slot(slot).ok_or(RecvTimeoutError::Disconnected),
            Err(error) => {
                if error == RecvTimeoutError::Disconnected {
                    self.ended.set(true);
                }
                Err(error)
            }
        }
    }
}

/// Where [`run_dispatch`] takes its items from: a [`LaneRx`].
pub trait ItemSource<T> {
    /// The next item, or `None` when the source has ended.
    fn next_item(&self) -> Option<T>;
}

impl<T> ItemSource<T> for LaneRx<T> {
    fn next_item(&self) -> Option<T> {
        self.recv().ok()
    }
}

// ---------------------------------------------------------------------------------------------
// Resolver cap and detached resolvers
// ---------------------------------------------------------------------------------------------

/// Caps the resolver threads alive in the PROCESS (not per install): a resolver blocked in a
/// synchronous provider call cannot be killed, so repeated install/uninstall cycles must not
/// accumulate them. A slot is held by the thread itself until its call returns.
pub struct ResolverCap {
    in_flight: AtomicUsize,
    max: usize,
}

impl ResolverCap {
    pub const fn new(max: usize) -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            max,
        }
    }

    /// A slot is free right now (one atomic read: safe on a hook thread). A later `try_acquire` can
    /// still lose the race.
    pub fn has_free_slot(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) < self.max
    }

    pub fn try_acquire(&'static self) -> Option<ResolverPermit> {
        let mut current = self.in_flight.load(Ordering::SeqCst);
        loop {
            if current >= self.max {
                return None;
            }
            match self.in_flight.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Some(ResolverPermit(self)),
                Err(seen) => current = seen,
            }
        }
    }
}

/// One resolver slot; released when dropped (also when the thread panics or fails to spawn).
pub struct ResolverPermit(&'static ResolverCap);

impl Drop for ResolverPermit {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run `work` on its own detached thread under a cap slot. The answer is sent only while `live` is
/// still the live generation: a resolver that returns after uninstall drops its answer and does
/// nothing else. `None` when the cap is full or the thread could not be created. The caller never
/// joins the thread.
pub fn start_resolver<T: Send + 'static>(
    cap: &'static ResolverCap,
    live: Arc<Liveness>,
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<Receiver<T>> {
    start_detached(cap, Some(live), name, work)
}

/// [`start_resolver`] with the generation gate optional: with `None` the answer is delivered
/// whenever the caller still waits (the caller's own deadline is the bound). Used for calls whose
/// answer is wanted even while an install is being torn down.
pub fn start_detached<T: Send + 'static>(
    cap: &'static ResolverCap,
    live: Option<Arc<Liveness>>,
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<Receiver<T>> {
    let permit = cap.try_acquire()?;
    let (tx, rx) = mpsc::channel::<T>();
    let spawned = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _permit = permit;
            let answer = work();
            if live.as_ref().map_or(true, |live| live.is_live()) {
                let _ = tx.send(answer);
            }
        });
    // On a failed spawn the closure, and with it the permit, is dropped.
    spawned.ok().map(|_| rx)
}

/// Counts the calls that missed their deadline and have not returned ("hung" calls), process-wide.
/// Unlike [`ResolverCap`] it does NOT count a call that is still inside its deadline: a healthy call
/// that is merely in flight never uses a slot, so two live calls cannot make a facility look
/// saturated. A new call is refused only while `max` calls are hung.
pub struct HungCallCap {
    hung: AtomicUsize,
    max: usize,
}

impl HungCallCap {
    pub const fn new(max: usize) -> Self {
        Self {
            hung: AtomicUsize::new(0),
            max,
        }
    }

    /// A call would be accepted right now (one atomic read: safe on a hook thread).
    pub fn has_free_slot(&self) -> bool {
        self.hung.load(Ordering::SeqCst) < self.max
    }
}

const CALL_RUNNING: u8 = 0;
/// The waiter gave up and counted the call as hung; the call's thread uncounts it when it ends.
const CALL_ABANDONED: u8 = 1;
const CALL_DONE: u8 = 2;

/// Ends a [`bounded_call`]'s thread: marks the call done and, if the waiter had already counted it
/// as hung, uncounts it. A guard so that a panicking call uncounts too.
struct CallEnd {
    state: Arc<AtomicU8>,
    cap: &'static HungCallCap,
}

impl Drop for CallEnd {
    fn drop(&mut self) {
        if self.state.swap(CALL_DONE, Ordering::SeqCst) == CALL_ABANDONED {
            self.cap.hung.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Run `work` on its own detached thread and wait at most `deadline` for its answer. `None` when
/// `cap` already has `max` hung calls (nothing is started), the thread could not be created, the
/// deadline passed or the call panicked.
///
/// A call that misses the deadline cannot be interrupted: it keeps its thread, counts against `cap`
/// as hung until it returns, and its answer is dropped. The threads alive for one `cap` are
/// therefore at most `max` hung ones plus one per caller currently waiting inside its deadline. This
/// is how a call that can wait on another process (a window hit test that is answered by the target's
/// own thread) is bounded without ever being made on a thread that must not wait.
pub fn bounded_call<T: Send + 'static>(
    cap: &'static HungCallCap,
    name: &str,
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if !cap.has_free_slot() {
        return None;
    }
    let state = Arc::new(AtomicU8::new(CALL_RUNNING));
    let (tx, rx) = mpsc::channel::<T>();
    let end = CallEnd {
        state: Arc::clone(&state),
        cap,
    };
    // On a failed spawn the closure, and with it `end`, is dropped: the state flips to done and
    // nothing was counted.
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _end = end;
            let _ = tx.send(work());
        })
        .ok()?;
    match rx.recv_timeout(deadline) {
        Ok(answer) => Some(answer),
        Err(RecvTimeoutError::Disconnected) => None,
        Err(RecvTimeoutError::Timeout) => {
            // Count first, then claim: a thread that sees the claim has already seen the count, so
            // the counter never goes below zero.
            cap.hung.fetch_add(1, Ordering::SeqCst);
            if state
                .compare_exchange(
                    CALL_RUNNING,
                    CALL_ABANDONED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                // The call finished between the timeout and the claim: not hung after all, and its
                // answer is already in the channel.
                cap.hung.fetch_sub(1, Ordering::SeqCst);
                return rx.try_recv().ok();
            }
            None
        }
    }
}

/// How long one wait slice is.
pub const WAIT_SLICE: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStep {
    Wait(Duration),
    Expired,
}

/// The next wait slice for a wait that has run `elapsed` of `deadline`.
pub fn next_wait(elapsed: Duration, deadline: Duration, slice: Duration) -> WaitStep {
    if elapsed >= deadline {
        WaitStep::Expired
    } else {
        WaitStep::Wait(slice.min(deadline - elapsed))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Waited<T> {
    Got(T),
    TimedOut,
    /// The sender is gone without an answer (the resolver panicked or dropped its answer).
    Disconnected,
    /// The generation ended while waiting; any answer, even one already there, is ignored.
    Abandoned,
}

/// Wait for `rx` up to `deadline`, in slices, so an uninstall is noticed within one slice
/// instead of after the whole deadline.
pub fn wait_interruptible<T>(
    rx: &Receiver<T>,
    deadline: Duration,
    slice: Duration,
    abandoned: impl Fn() -> bool,
) -> Waited<T> {
    let start = Instant::now();
    loop {
        if abandoned() {
            return Waited::Abandoned;
        }
        match next_wait(start.elapsed(), deadline, slice) {
            WaitStep::Expired => return Waited::TimedOut,
            WaitStep::Wait(step) => match rx.recv_timeout(step) {
                Ok(answer) => {
                    return if abandoned() {
                        Waited::Abandoned
                    } else {
                        Waited::Got(answer)
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return if abandoned() {
                        Waited::Abandoned
                    } else {
                        Waited::Disconnected
                    }
                }
            },
        }
    }
}

/// What the click decision learns from a wait on the resolver. Only an answer that arrived while
/// the install was live is a lookup; everything else is an `Unknown` with its cause (never
/// `NotLink`), and an ended install is `Skipped`.
pub fn lookup_from_wait(waited: Waited<Lookup>) -> Result<Lookup, UnknownCause> {
    match waited {
        Waited::Got(answer) => Ok(answer),
        Waited::TimedOut => Err(UnknownCause::TimedOut),
        Waited::Disconnected => Err(UnknownCause::ResolverDied),
        Waited::Abandoned => Err(UnknownCause::Skipped),
    }
}

// ---------------------------------------------------------------------------------------------
// Toggle guard: at most one host toggle callback at a time, across installs
// ---------------------------------------------------------------------------------------------

/// Marks the host toggle callback as running. It lives in the host's state object (not in an
/// install), so a callback still running when its install ended keeps blocking a second callback
/// from a later install: "at most one callback runs" holds across generations.
pub struct ToggleGuard {
    since: Mutex<Option<Instant>>,
}

impl ToggleGuard {
    pub const fn new() -> Self {
        Self {
            since: Mutex::new(None),
        }
    }

    pub fn state_at(&self, now: Instant, stall_after: Duration) -> LaneState {
        let since = *lock(&self.since);
        lane_state(
            since.map(|t| now.saturating_duration_since(t).as_millis() as u64),
            stall_after.as_millis() as u64,
        )
    }

    /// Mark the callback as started. `false` when one is already marked (running).
    pub fn begin_if_idle(&self, now: Instant) -> bool {
        let mut since = lock(&self.since);
        if since.is_some() {
            return false;
        }
        *since = Some(now);
        true
    }

    pub fn end(&self) {
        *lock(&self.since) = None;
    }
}

/// The toggle lane thread body: run `call` once per queued toggle and mark the guard idle after
/// each. It ends when the sender is dropped, after the queued toggles (each was accepted before
/// the end: "consumed" means it runs exactly once). A callback that is running cannot be
/// interrupted; the thread ends when it returns.
pub fn run_toggle_lane(rx: Receiver<()>, guard: &ToggleGuard, mut call: impl FnMut()) {
    while rx.recv().is_ok() {
        call();
        guard.end();
    }
}

// ---------------------------------------------------------------------------------------------
// Dispatch tracker: what the host dispatch thread may still do once the install has ended
// ---------------------------------------------------------------------------------------------

/// The kinds of command the keyboard dispatch thread delivers to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    HookCancel,
    PttStart,
    PttStop,
    PttSend,
    PttCancel,
    Preview(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Run,
    Discard,
}

/// After uninstall a queued command that STARTS something (a push-to-talk start, a preview-down,
/// a Ctrl+Escape cancel) is discarded, but one that ENDS something the host was already told
/// about is still delivered, and a still-open push-to-talk or preview is closed on exit: the host
/// never keeps a recording or preview whose release this hook can no longer see.
#[derive(Debug, Default)]
pub struct DispatchTracker {
    ptt_open: bool,
    preview_down: bool,
}

impl DispatchTracker {
    pub const fn new() -> Self {
        Self {
            ptt_open: false,
            preview_down: false,
        }
    }

    pub fn admit(&mut self, live: bool, cmd: Cmd) -> Admit {
        match cmd {
            Cmd::HookCancel => {
                if live {
                    Admit::Run
                } else {
                    Admit::Discard
                }
            }
            Cmd::PttStart => {
                if live {
                    self.ptt_open = true;
                    Admit::Run
                } else {
                    Admit::Discard
                }
            }
            Cmd::PttStop | Cmd::PttSend | Cmd::PttCancel => {
                if live || self.ptt_open {
                    self.ptt_open = false;
                    Admit::Run
                } else {
                    Admit::Discard
                }
            }
            Cmd::Preview(true) => {
                if live {
                    self.preview_down = true;
                    Admit::Run
                } else {
                    Admit::Discard
                }
            }
            Cmd::Preview(false) => {
                if live || self.preview_down {
                    self.preview_down = false;
                    Admit::Run
                } else {
                    Admit::Discard
                }
            }
        }
    }

    /// What must still be delivered when the dispatch thread exits: a cancel for an open
    /// push-to-talk (the same teardown the macOS tap performs) and a preview-up for an open
    /// preview. Empty the second time.
    pub fn closers(&mut self) -> Vec<Cmd> {
        let mut out = Vec::new();
        if self.ptt_open {
            self.ptt_open = false;
            out.push(Cmd::PttCancel);
        }
        if self.preview_down {
            self.preview_down = false;
            out.push(Cmd::Preview(false));
        }
        out
    }
}

/// The dispatch thread body. `is_open` is read when an item is taken (not when it was sent), so an
/// item queued before uninstall but taken after is judged by [`DispatchTracker::admit`] as
/// belonging to an ended install. Ends when the lane is closed and drained; then delivers the
/// closers.
pub fn run_dispatch<T, S: ItemSource<T>>(
    rx: S,
    is_open: impl Fn() -> bool,
    kind: impl Fn(&T) -> Cmd,
    mut run: impl FnMut(T),
    mut run_closer: impl FnMut(Cmd),
) {
    let mut tracker = DispatchTracker::new();
    while let Some(item) = rx.next_item() {
        if tracker.admit(is_open(), kind(&item)) == Admit::Run {
            run(item);
        }
    }
    for closer in tracker.closers() {
        run_closer(closer);
    }
}

// ---------------------------------------------------------------------------------------------
// Host sink lines from a path that must never wait for the host (drop, teardown, hotkey thread)
// ---------------------------------------------------------------------------------------------

/// Most `rk-hook-report` threads alive in the PROCESS. A host sink that never returns pins one
/// thread; once this many are pinned further hand-offs are dropped (never waited for).
pub const MAX_REPORT_THREADS: usize = 4;
/// Most lines one [`report_detached`] hand-off carries; the rest are cut.
pub const MAX_REPORT_LINES: usize = 4;
static REPORT_THREADS: ResolverCap = ResolverCap::new(MAX_REPORT_THREADS);

/// Deliver `lines` to the host's `sink` on a detached thread and return at once, so a teardown or a
/// drop never waits for host code: `false` when nothing was handed over (the cap is full or no
/// thread could be created: the lines are dropped), `true` otherwise. A panicking sink is caught.
/// The caller makes every state change BEFORE calling this and holds no bookkeeping lock.
pub fn report_detached(sink: fn(&str, &str), mut lines: Vec<(&'static str, String)>) -> bool {
    lines.truncate(MAX_REPORT_LINES);
    if lines.is_empty() {
        return true;
    }
    let Some(permit) = REPORT_THREADS.try_acquire() else {
        return false;
    };
    // A failed spawn drops the closure and with it the permit.
    thread::Builder::new()
        .name("rk-hook-report".to_string())
        .spawn(move || {
            let _permit = permit;
            for (event, message) in lines {
                let _ = catch_unwind(AssertUnwindSafe(|| sink(event, &message)));
            }
        })
        .is_ok()
}

// ---------------------------------------------------------------------------------------------
// Per-press ledgers (balanced swallow/deliver across an install boundary)
// ---------------------------------------------------------------------------------------------

/// The keys whose key-down this hook swallowed and whose key-up is still owed. The owner clears it
/// only when no hook that could still see the key-up is installed (`KeyboardState::on_hook_removed`,
/// or `begin_session` when no hook ran): clearing it while the hook is live would let the key-up
/// reach the application without its key-down.
///
/// A SET, not one key: the user can switch the push-to-talk key while a key is held and then press
/// the new one, so two debts can be open at once, and the modifiers the gesture swallows as
/// companions (the AltGr Ctrl, the send Ctrl or Alt) are debts of their own. Keys are tracked in
/// the range `0xA0..0xC0` (left/right Shift, Control and Alt, the keys the gesture can swallow);
/// the generic `VK_CONTROL` is folded into `VK_LCONTROL`. Any other key is not tracked.
pub struct PressLedger {
    bits: AtomicU32,
}

const LEDGER_BASE: u32 = 0xA0;
const LEDGER_SPAN: u32 = 32;

fn ledger_bit(vk: u32) -> Option<u32> {
    let vk = if vk == 0x11 { 0xA2 } else { vk };
    (LEDGER_BASE..LEDGER_BASE + LEDGER_SPAN)
        .contains(&vk)
        .then(|| 1u32 << (vk - LEDGER_BASE))
}

impl PressLedger {
    pub const fn new() -> Self {
        Self {
            bits: AtomicU32::new(0),
        }
    }
    /// How many keys' ups are owed.
    pub fn count(&self) -> u32 {
        self.bits.load(Ordering::SeqCst).count_ones()
    }
    /// Whether the up of `vk` is owed.
    pub fn owes(&self, vk: u32) -> bool {
        ledger_bit(vk).is_some_and(|bit| self.bits.load(Ordering::Relaxed) & bit != 0)
    }
    /// Record that the down of `vk` was swallowed.
    pub fn set(&self, vk: u32) {
        if let Some(bit) = ledger_bit(vk) {
            self.bits.fetch_or(bit, Ordering::SeqCst);
        }
    }
    /// Settle the debt of `vk`; `true` when one was owed.
    pub fn settle(&self, vk: u32) -> bool {
        match ledger_bit(vk) {
            Some(bit) => self.bits.fetch_and(!bit, Ordering::SeqCst) & bit != 0,
            None => false,
        }
    }
    pub fn clear(&self) {
        self.bits.store(0, Ordering::SeqCst);
    }
}

/// How the hook routed the down of ONE key it can swallow outside the ledger's range (Escape, for
/// the Ctrl+Escape cancel), so its up is routed the same way whatever the hook's state is by then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldKey {
    /// No down seen (or its up was settled): an up of the key is delivered.
    None,
    /// The down was swallowed: its repeats and its up are swallowed (the up is owed).
    Swallowed,
    /// The down was delivered: its repeats and its up are delivered.
    Delivered,
}

/// The pairing record of one key (`HeldKey`), lock-free. Cleared only where the ledgers are:
/// `KeyboardState::on_hook_removed` or `begin_session`.
pub struct KeyHold {
    state: AtomicU8,
}

impl KeyHold {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    pub fn get(&self) -> HeldKey {
        match self.state.load(Ordering::SeqCst) {
            1 => HeldKey::Swallowed,
            2 => HeldKey::Delivered,
            _ => HeldKey::None,
        }
    }

    pub fn set(&self, held: HeldKey) {
        let value = match held {
            HeldKey::None => 0,
            HeldKey::Swallowed => 1,
            HeldKey::Delivered => 2,
        };
        self.state.store(value, Ordering::SeqCst);
    }

    /// The up arrived: what was recorded, and the record is cleared.
    pub fn settle(&self) -> HeldKey {
        match self.state.swap(0, Ordering::SeqCst) {
            1 => HeldKey::Swallowed,
            2 => HeldKey::Delivered,
            _ => HeldKey::None,
        }
    }

    /// `1` while an up is owed (swallowed down), else 0.
    pub fn owed(&self) -> u32 {
        u32::from(self.get() == HeldKey::Swallowed)
    }

    pub fn clear(&self) {
        self.state.store(0, Ordering::SeqCst);
    }
}

/// How a push-to-talk key event is routed so swallowed and delivered presses stay balanced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressRoute {
    /// Run the normal push-to-talk logic (which swallows the push-to-talk key).
    Normal,
    /// A key-up whose key-down this hook swallowed, but the gesture no longer applies
    /// (disabled or re-keyed while held): swallow it so no stray up reaches the application.
    SwallowOwedUp,
    /// A key-up of the push-to-talk key whose key-down was delivered (the gesture was switched
    /// on while the key was held): pass it so the application does not keep the key down.
    PassDeliveredUp,
}

/// Pure routing of the push-to-talk key's key-up against the press it belongs to.
/// `swallowed_vk` is the key whose down was swallowed (0 = none), `ptt_vk` the configured key.
pub fn press_route(
    vk: u32,
    is_keydown: bool,
    swallowed_vk: u32,
    ptt_vk: u32,
    input_active: bool,
) -> PressRoute {
    if is_keydown {
        return PressRoute::Normal;
    }
    if swallowed_vk != 0 && swallowed_vk == vk {
        return if input_active && vk == ptt_vk {
            PressRoute::Normal
        } else {
            PressRoute::SwallowOwedUp
        };
    }
    if input_active && vk == ptt_vk {
        return PressRoute::PassDeliveredUp;
    }
    PressRoute::Normal
}

/// A held middle press whose button-up is still owed, tagged with the install (generation) that
/// swallowed the down. A different install never swallows an up it does not owe.
pub struct UpOwed {
    session: AtomicU64,
}

impl UpOwed {
    pub const fn new() -> Self {
        Self {
            session: AtomicU64::new(0),
        }
    }

    /// `session` 0 (no install) never owes anything.
    pub fn is_owed(&self, session: u64) -> bool {
        session != 0 && self.session.load(Ordering::Relaxed) == session
    }

    /// Record the gate's decision: `owed` for `session`, or cleared.
    pub fn apply(&self, session: u64, owed: bool) {
        self.session
            .store(if owed { session } else { 0 }, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.session.store(0, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------------------------
// Scheduling handshake (macOS: a job handed to the main thread that the caller may give up on)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandshakeState {
    Pending,
    Running,
    Done,
    Abandoned,
}

/// A job scheduled on another thread that the scheduler may stop waiting for. The job calls
/// [`begin_run`](Self::begin_run) first and does nothing when it returns `false`; the scheduler
/// calls [`abandon`](Self::abandon) on timeout and, when that returns `false` (the job already
/// started or finished), schedules a cleanup behind it.
pub struct Handshake {
    state: Mutex<HandshakeState>,
}

impl Handshake {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(HandshakeState::Pending),
        }
    }

    pub fn begin_run(&self) -> bool {
        let mut state = lock(&self.state);
        if *state == HandshakeState::Pending {
            *state = HandshakeState::Running;
            true
        } else {
            false
        }
    }

    pub fn finish(&self) {
        let mut state = lock(&self.state);
        if *state == HandshakeState::Running {
            *state = HandshakeState::Done;
        }
    }

    /// `true`: the job has not started and now never will. `false`: it started or finished.
    pub fn abandon(&self) -> bool {
        let mut state = lock(&self.state);
        if *state == HandshakeState::Pending {
            *state = HandshakeState::Abandoned;
            true
        } else {
            false
        }
    }
}
