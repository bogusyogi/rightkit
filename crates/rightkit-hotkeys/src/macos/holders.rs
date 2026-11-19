//! Process-wide holders for the macOS fn-chord tap and the middle-mouse tap: they remember the
//! callbacks and settings applied before the tap exists, spawn the tap once everything it needs is
//! registered, and forward later changes to it.
//!
//! Moved out of an app's two thin macOS facades. Product-neutral: the host owns each holder in a
//! `static` and passes its callbacks and its telemetry sink.
//!
//! Lifecycle (HANDOFF.md, "Lifecycle"): each `start`/`spawn` begins one generation
//! (`lifecycle::Liveness`) and `stop` ends it:
//! - the crate's own enabled flags are switched off first (`set_enabled(false)`, and for the fn tap
//!   `set_ptt_enabled(false)`), so a tap whose drop is still pending passes events instead of
//!   swallowing them; then the tap is taken out of the holder and dropped OUTSIDE every lock, on a
//!   helper thread that is waited for at most [`TAP_STOP_WAIT`]. The crate's `EventTap::drop` loops
//!   until the run-loop thread has exited, with no bound of its own (an owner-side defect,
//!   HANDOFF.md), so the holder bounds the WAIT: `stop` returns, the helper finishes the drop later,
//!   and the holder reports it (`tap stop pending`). The fn tap's `set_ptt_enabled(false)` (and its
//!   drop) cancels the arm timer and ends a push-to-talk the tap started through the host's
//!   `ptt_cancel` callback (spawned, not awaited), once, so the host is never left recording;
//! - callbacks that START something (`on_toggle`, `ptt_start`) are gated by the generation AND by
//!   the holder's own enabled flags (they are consulted when the callback runs, so a tap that the
//!   crate creates enabled, or a setting changed while a lookup was in flight, cannot run a start the
//!   holder disabled); callbacks that END something (stop, send, cancel) are never gated;
//! - the fn tap's `diag` callback runs INSIDE the crate's tap callback ("must return quickly"); the
//!   holder gives it a bounded lane of fixed-size lines (copied, truncated to `DIAG_EVENT_BYTES` and
//!   `DIAG_MESSAGE_BYTES`) and a `rk-fn-tap-diag` thread that calls the host's sink, so a slow sink
//!   cannot stall the tap and the holder's callback allocates no `String` (lines past the lane's
//!   depth are dropped; what the channel itself may allocate, and the crate's own formatting before
//!   it calls `diag`, are described at `make_diag_lane`);
//! - the middle-mouse lookup (`ax_link_at`, a synchronous accessibility call) runs on the crate's
//!   single lookup worker, which cannot be interrupted. It is wrapped: at most
//!   [`MAX_AX_LOOKUPS_IN_FLIGHT`] calls are in flight in the process at once (further clicks pass
//!   through as if on a link, with no call), a disabled holder answers "link" (the click is
//!   delivered, never toggled), and an answer that arrives after `stop` is treated as "link", never
//!   as "not a link", so no toggle runs after `stop`;
//! - a toggle the crate decided on but that the holder then refuses (the tap was stopped or the
//!   feature disabled in between, or a toggle callback is still running) is REPORTED
//!   (`middle_mouse_dropped`): the crate has already swallowed that click and replays by
//!   coordinates only, so the holder cannot deliver it. This differs from Windows: a click held
//!   before the feature is disabled is dropped and reported here, and is DELIVERED there
//!   (`mouse_hook::MiddleClickState::settle_job`). It is the only macOS loss that is reported; a
//!   lookup error that answers "not a link" toggles and swallows a possible link click without a
//!   report (owner defect 1);
//! - at most one host toggle callback runs at a time per tap;
//! - a later `start`/`spawn` after `stop` builds a new tap with fresh state; settings the host set
//!   (enabled, push-to-talk key) are reapplied, callbacks are kept.
//!
//! NOT FIXED here (the published crate's own files; HANDOFF.md "Defects in rightkit-hotkeys 0.1.4
//! (owner's)"): a lookup error toggles, the replay is by coordinates with no target check, the tap
//! starts enabled, one thread is spawned per toggle, and `EventTap::drop` is unbounded. The macOS
//! middle-click path is NOT claimed fixed by this hand-off.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::fn_hotkey::{Action, Diag, FnAction};
use super::{ax_link_at, FnHotkeyCallbacks, FnHotkeyTap, MiddleMouseTap, PttKey, TapLocation};
use crate::lifecycle::{lock, report_detached, Lane, Liveness, ResolverCap};

/// Host event sink: `(event, message)`.
pub type EventSink = fn(&str, &str);

/// Accessibility lookups allowed in flight in the process (a blocked call keeps the crate's
/// single lookup worker, so repeated stop/spawn cycles must not stack them).
pub const MAX_AX_LOOKUPS_IN_FLIGHT: usize = 2;

static AX_LOOKUPS: ResolverCap = ResolverCap::new(MAX_AX_LOOKUPS_IN_FLIGHT);

/// The longest `stop` waits for a tap's run-loop thread to exit.
pub const TAP_STOP_WAIT: Duration = Duration::from_millis(1_500);

/// Drop `tap` on a helper thread and wait for it at most [`TAP_STOP_WAIT`]. `false` when the wait
/// ran out (the helper finishes the drop later).
fn drop_bounded<T: Send + 'static>(tap: T) -> bool {
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let spawned = thread::Builder::new()
        .name("rk-tap-stop".to_string())
        .spawn(move || {
            drop(tap);
            let _ = done_tx.send(());
        });
    match spawned {
        Ok(_) => done_rx.recv_timeout(TAP_STOP_WAIT).is_ok(),
        // The closure, and with it the tap, was dropped on this thread: stopped inline.
        Err(_) => true,
    }
}

type ChordSet = (Action, Action, Action, FnAction);
type PttSet = (Action, Action, Action, Action);

fn key_from_u8(v: u8) -> PttKey {
    [PttKey::Control, PttKey::LeftOption, PttKey::RightOption]
        .into_iter()
        .find(|k| *k as u8 == v)
        .unwrap_or(PttKey::Control)
}

/// A callback that starts something: does nothing once its generation has ended or while the
/// holder's flag for it is off (read when the callback runs).
fn gated(live: &Arc<Liveness>, flag: &Arc<AtomicBool>, action: Action) -> Action {
    let live = Arc::clone(live);
    let flag = Arc::clone(flag);
    Arc::new(move || {
        if live.is_live() && flag.load(Ordering::SeqCst) {
            action()
        }
    })
}

struct FnRunning {
    tap: FnHotkeyTap,
    live: Arc<Liveness>,
    /// `enabled` and `ptt_enabled` as the start callbacks see them.
    enabled: Arc<AtomicBool>,
    ptt_enabled: Arc<AtomicBool>,
    /// Lane behind the tap's `diag` callback; closed at `stop`.
    diag: Arc<Lane<DiagLine>>,
}

/// Depth of the fn tap's diag lane.
const DIAG_LANE_DEPTH: usize = 64;
/// Longest event name and message a diag line carries, in bytes; longer text is cut at a character
/// boundary (the host's sink then gets the beginning).
const DIAG_EVENT_BYTES: usize = 48;
const DIAG_MESSAGE_BYTES: usize = 192;

/// One diag line copied into fixed buffers, so the tap callback allocates nothing for it.
struct DiagLine {
    event: [u8; DIAG_EVENT_BYTES],
    event_len: u8,
    message: [u8; DIAG_MESSAGE_BYTES],
    message_len: u8,
}

/// Copy as much of `src` as fits `dst`, ending on a character boundary; the length copied.
fn copy_truncated(dst: &mut [u8], src: &str) -> usize {
    let mut n = src.len().min(dst.len());
    while !src.is_char_boundary(n) {
        n -= 1;
    }
    dst[..n].copy_from_slice(&src.as_bytes()[..n]);
    n
}

impl DiagLine {
    fn new(event: &str, message: &str) -> Self {
        let mut line = DiagLine {
            event: [0; DIAG_EVENT_BYTES],
            event_len: 0,
            message: [0; DIAG_MESSAGE_BYTES],
            message_len: 0,
        };
        line.event_len = copy_truncated(&mut line.event, event) as u8;
        line.message_len = copy_truncated(&mut line.message, message) as u8;
        line
    }

    fn event(&self) -> &str {
        std::str::from_utf8(&self.event[..usize::from(self.event_len)]).unwrap_or("")
    }

    fn message(&self) -> &str {
        std::str::from_utf8(&self.message[..usize::from(self.message_len)]).unwrap_or("")
    }
}

/// A `diag` callback that only copies the line into a fixed buffer and queues it (no lock, no
/// `String`, no host call on the tap thread) and the thread that delivers to the host's sink.
/// Without the thread the lines are dropped, never delivered inline.
///
/// What the tap callback still does that this holder cannot remove: the lane is a standard unbounded
/// channel with a depth counter, whose `send` can take a new list block from the allocator now and
/// then (one block holds several lines; not measured how often); and the crate formats some lines
/// itself (`format_abort`) before it calls `diag` (HANDOFF.md, "Defects in rightkit-hotkeys 0.1.4",
/// row 8).
fn make_diag_lane(sink: EventSink) -> (Arc<Lane<DiagLine>>, Diag) {
    let (lane, rx) = Lane::<DiagLine>::with_depth(DIAG_LANE_DEPTH);
    let lane = Arc::new(lane);
    let spawned = thread::Builder::new()
        .name("rk-fn-tap-diag".to_string())
        .spawn(move || {
            while let Ok(line) = rx.recv() {
                // A panicking sink must not end the thread.
                let _ = catch_unwind(AssertUnwindSafe(|| sink(line.event(), line.message())));
            }
        });
    if spawned.is_err() {
        lane.close();
    }
    let queue = Arc::clone(&lane);
    let diag: Diag = Arc::new(move |event: &str, message: &str| {
        let _ = queue.send(DiagLine::new(event, message));
    });
    (lane, diag)
}

pub struct FnHotkeyHolder {
    tap: Mutex<Option<FnRunning>>,
    chord: Mutex<Option<ChordSet>>,
    ptt: Mutex<Option<PttSet>>,
    enabled: AtomicBool,
    ptt_enabled: AtomicBool,
    ptt_key: AtomicU8,
    event_sink: EventSink,
}

impl FnHotkeyHolder {
    pub const fn new(event_sink: EventSink) -> Self {
        Self {
            tap: Mutex::new(None),
            chord: Mutex::new(None),
            ptt: Mutex::new(None),
            enabled: AtomicBool::new(true),
            ptt_enabled: AtomicBool::new(false),
            ptt_key: AtomicU8::new(PttKey::Control as u8),
            event_sink,
        }
    }

    /// `toggle`, `stop_send`, `cancel` and the fn-key change callback.
    pub fn set_chord_callbacks(
        &self,
        toggle: Action,
        stop_send: Action,
        cancel: Action,
        fn_change: FnAction,
    ) {
        *lock(&self.chord) = Some((toggle, stop_send, cancel, fn_change));
    }

    pub fn set_ptt_callbacks(
        &self,
        start: Action,
        stop: Action,
        stop_send: Action,
        cancel: Action,
    ) {
        *lock(&self.ptt) = Some((start, stop, stop_send, cancel));
    }

    /// Spawns the tap once both callback sets are registered. Idempotent while a tap is live.
    pub fn start(&self) {
        let mut slot = lock(&self.tap);
        if slot.is_some() {
            return;
        }
        let (Some(chord), Some(ptt)) = (lock(&self.chord).clone(), lock(&self.ptt).clone()) else {
            return;
        };
        let sink = self.event_sink;
        let live = Liveness::begin();
        let enabled = Arc::new(AtomicBool::new(self.enabled.load(Ordering::SeqCst)));
        let ptt_enabled = Arc::new(AtomicBool::new(self.ptt_enabled.load(Ordering::SeqCst)));
        let (diag_lane, diag) = make_diag_lane(sink);
        let callbacks = FnHotkeyCallbacks {
            on_toggle: gated(&live, &enabled, chord.0),
            on_stop_send: chord.1,
            on_cancel: chord.2,
            on_fn_change: chord.3,
            ptt_start: gated(&live, &ptt_enabled, ptt.0),
            ptt_stop: ptt.1,
            ptt_stop_send: ptt.2,
            ptt_cancel: ptt.3,
            diag,
        };
        let outcome = match FnHotkeyTap::spawn(TapLocation::Hid, callbacks) {
            Ok(tap) => {
                tap.set_enabled(self.enabled.load(Ordering::SeqCst));
                tap.set_ptt_key(key_from_u8(self.ptt_key.load(Ordering::SeqCst)));
                tap.set_ptt_enabled(self.ptt_enabled.load(Ordering::SeqCst));
                *slot = Some(FnRunning {
                    tap,
                    live,
                    enabled,
                    ptt_enabled,
                    diag: Arc::clone(&diag_lane),
                });
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        };
        drop(slot);
        match outcome {
            Ok(()) => (self.event_sink)("fn_hotkey", "tap active"),
            // The error text is the operating system's: the sink gets a tag.
            Err(_) => (self.event_sink)("fn_hotkey_error", "tap_spawn_failed"),
        }
    }

    /// End the tap: starts still queued become inert, a push-to-talk the tap started is cancelled
    /// through the host's cancel callback, the arm timer and the run-loop thread stop. Returns
    /// without waiting for host callbacks. Idempotent; `start` may be called again afterwards.
    pub fn stop(&self) {
        let running = lock(&self.tap).take();
        if let Some(running) = running {
            running.live.end();
            running.enabled.store(false, Ordering::SeqCst);
            running.ptt_enabled.store(false, Ordering::SeqCst);
            // The crate's own flags go off before the drop is even scheduled, so the tap passes
            // events whether or not the helper thread gets to run (`set_ptt_enabled(false)` also
            // ends a push-to-talk the tap started, once, through the host's `ptt_cancel`; the drop
            // then finds none open).
            running.tap.set_enabled(false);
            running.tap.set_ptt_enabled(false);
            // Dropped on a helper thread, waited for at most `TAP_STOP_WAIT` (see the module notes).
            let stopped = drop_bounded(running.tap);
            running.diag.close();
            // Every state change is done: the line goes out on a detached thread, so a sink that
            // never returns cannot hold `stop` (`lifecycle::report_detached`).
            let _ = report_detached(
                self.event_sink,
                vec![(
                    "fn_hotkey",
                    if stopped {
                        "tap stopped".to_string()
                    } else {
                        "tap stop pending".to_string()
                    },
                )],
            );
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if let Some(r) = lock(&self.tap).as_ref() {
            r.enabled.store(enabled, Ordering::SeqCst);
            r.tap.set_enabled(enabled);
        }
    }

    pub fn set_ptt_enabled(&self, enabled: bool) {
        self.ptt_enabled.store(enabled, Ordering::SeqCst);
        if let Some(r) = lock(&self.tap).as_ref() {
            r.ptt_enabled.store(enabled, Ordering::SeqCst);
            r.tap.set_ptt_enabled(enabled);
        }
    }

    pub fn set_ptt_key(&self, name: &str) {
        let key = PttKey::parse(name).unwrap_or_else(|| {
            // The rejected text is host input and may be a key name: not echoed.
            (self.event_sink)(
                "mac_push_to_talk_key_rejected",
                "unknown_name_fallback_ctrl",
            );
            PttKey::Control
        });
        self.ptt_key.store(key as u8, Ordering::SeqCst);
        if let Some(r) = lock(&self.tap).as_ref() {
            r.tap.set_ptt_key(key);
        }
    }

    pub fn set_dictation_active(&self, active: bool) {
        if let Some(r) = lock(&self.tap).as_ref() {
            r.tap.set_dictation_active(active);
        }
    }
}

struct MouseRunning {
    tap: MiddleMouseTap,
    live: Arc<Liveness>,
    /// `enabled` as the toggle and the lookup see it (the crate's tap starts enabled; this starts
    /// as the holder's value and is what the callbacks consult).
    gate: Arc<AtomicBool>,
}

pub struct MiddleMouseHolder {
    tap: Mutex<Option<MouseRunning>>,
    callback: Mutex<Option<Action>>,
    enabled: AtomicBool,
    event_sink: EventSink,
}

impl MiddleMouseHolder {
    pub const fn new(event_sink: EventSink) -> Self {
        Self {
            tap: Mutex::new(None),
            callback: Mutex::new(None),
            enabled: AtomicBool::new(false),
            event_sink,
        }
    }

    pub fn set_toggle_callback(&self, cb: Action) {
        *lock(&self.callback) = Some(cb);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if let Some(r) = lock(&self.tap).as_ref() {
            r.gate.store(enabled, Ordering::SeqCst);
            r.tap.set_enabled(enabled);
        }
    }

    /// Spawns the tap once the toggle callback is registered. Idempotent while a tap is live.
    pub fn spawn(&self) {
        let mut slot = lock(&self.tap);
        if slot.is_some() {
            return;
        }
        let Some(cb) = lock(&self.callback).clone() else {
            return;
        };
        let live = Liveness::begin();
        let gate = Arc::new(AtomicBool::new(self.enabled.load(Ordering::SeqCst)));
        let toggle_live = Arc::clone(&live);
        let lookup_live = Arc::clone(&live);
        let toggle_gate = Arc::clone(&gate);
        let lookup_gate = Arc::clone(&gate);
        let running_toggle = Arc::new(AtomicBool::new(false));
        let sink = self.event_sink;
        let spawned = MiddleMouseTap::spawn(
            TapLocation::Hid,
            // The toggle starts something: inert once the generation has ended or the feature is
            // off. The crate has ALREADY swallowed the click that led here and replays only by
            // coordinates, so a refused toggle cannot deliver it: it is reported instead.
            move || {
                if !toggle_live.is_live() || !toggle_gate.load(Ordering::SeqCst) {
                    sink("middle_mouse_dropped", "reason=stopped_or_disabled");
                    return;
                }
                // One host callback at a time per tap.
                if running_toggle.swap(true, Ordering::SeqCst) {
                    sink("middle_mouse_dropped", "reason=toggle_busy");
                    return;
                }
                // A panicking host callback must not leave the tap "busy" for ever.
                let _ = catch_unwind(AssertUnwindSafe(|| cb()));
                running_toggle.store(false, Ordering::SeqCst);
            },
            // `true` = "a link": the click is delivered and no toggle runs. Used while disabled, at
            // the cap and for an answer that arrives after `stop`; only a live, completed lookup
            // of an enabled holder can say "not a link". (A lookup that FAILED still says "not a
            // link": `ax_link_at` returns `false` on every AX error. Owner-side defect.)
            move |x, y| {
                if !lookup_gate.load(Ordering::SeqCst) {
                    return true;
                }
                let Some(_slot) = AX_LOOKUPS.try_acquire() else {
                    return true;
                };
                let is_link = ax_link_at(x, y);
                !lookup_live.is_live() || !lookup_gate.load(Ordering::SeqCst) || is_link
            },
        );
        let outcome = match spawned {
            Ok(tap) => {
                tap.set_enabled(self.enabled.load(Ordering::SeqCst));
                *slot = Some(MouseRunning { tap, live, gate });
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        };
        drop(slot);
        match outcome {
            Ok(()) => (self.event_sink)("middle_mouse", "tap active"),
            Err(_) => (self.event_sink)("middle_mouse_error", "tap_spawn_failed"),
        }
    }

    /// End the tap: the run-loop thread stops (waited for at most `TAP_STOP_WAIT`), no toggle runs
    /// afterwards, and a lookup still blocked in an accessibility call is left to finish on its own
    /// (capped, its answer ignored). Returns without waiting for it. Idempotent; `spawn` may be
    /// called again afterwards.
    pub fn stop(&self) {
        let running = lock(&self.tap).take();
        if let Some(running) = running {
            running.live.end();
            running.gate.store(false, Ordering::SeqCst);
            // The crate's flag goes off before the drop is scheduled: a tap whose run loop cannot be
            // stopped then passes every press instead of swallowing it (a release of an earlier
            // swallowed press is still accounted for by the crate's gate).
            running.tap.set_enabled(false);
            let stopped = drop_bounded(running.tap);
            // As `FnHotkeyHolder::stop`: the sink is not waited for.
            let _ = report_detached(
                self.event_sink,
                vec![(
                    "middle_mouse",
                    if stopped {
                        "tap stopped".to_string()
                    } else {
                        "tap stop pending".to_string()
                    },
                )],
            );
        }
    }
}
