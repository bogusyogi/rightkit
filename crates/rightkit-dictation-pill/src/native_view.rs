//! The Rust side of a native pill view's C ABI (a framework panel linked into the same process,
//! for example the Swift pill): the callback table the view calls back through, every callback body
//! (bounded reads, the panic guard, validation, the background thread each action runs on, the
//! fixed-tag failure logs), the wire encoding of a pill event, the calls Rust makes into the view
//! and the splash hand-off latch's use.
//!
//! Moved from HeardRight `pill_bridge_macos.rs`. Platform-free: there is no `extern` block here.
//! The host declares the view's exported symbols and passes them as plain function pointers
//! ([`Entry`]), so the symbol names, the layout of the host's C header and the wire numbers
//! ([`WireKinds`]) are the host's, while everything that can go subtly wrong is here.
//!
//! What the host supplies, always (no `Default`): [`Entry`] (the view's exported functions),
//! [`Actions`] (what each callback does in the app, over plain values: a callback never receives
//! an app handle or a view pointer), [`Config`] (event-name prefix and names, wire numbers, byte
//! bounds, route and destination tables, the splash hand-off backstop) and the diagnostic sink.
//!
//! # The C table
//!
//! [`CallbackTable`] is `#[repr(C)]` and must match the host's C header field for field, in order.
//! Adding or reordering a field is an ABI change of the view and the host together.
//!
//! # Who may cause an action
//!
//! The native view is linked into this process and can only call the function pointers
//! [`NativeView::initialize`] hands it; no action is selected by a notification name, URL or pipe
//! another process could also use. The trust boundary is the process. Every string and integer the
//! view passes is still treated as untrusted: strings are read with a byte bound before anything is
//! copied ([`bridge::string_from_c`]), codes map through fixed tables (an unknown code never
//! becomes a route or a destination), ids are only looked up in the app's own store, and a panic
//! never crosses the C boundary ([`bridge::guarded`]).
//!
//! # Logging
//!
//! Nothing from an error string reaches a log (they can carry paths and titles). Every failure is a
//! fixed `outcome=` tag under `<prefix>_<suffix>`; the view's own lines go through
//! [`bridge::log_text`].
//!
//! A callback is dropped (it answers its documented default) until [`NativeView::initialize`] ran.

use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::bridge::{self, c_string, prefix_from_c, string_from_c};
use crate::config::ActionOutcome;
use crate::splash_handoff::{HandoffAction, SplashHandoff};
use crate::PillEvent;

/// The host's diagnostic sink: `(event, detail)`.
pub type Sink = fn(&str, &str);

/// The C callback table the view is given. Field order and types are the host's C header's.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CallbackTable {
    /// Returns 0 copied, 2 failed. Called on a background queue; the clipboard write has
    /// finished when it returns, so the pill shows "copied" only after a 0.
    pub copy_text: Option<unsafe extern "C" fn(*const c_char) -> c_int>,
    pub update_history_item: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    pub open_hub: Option<unsafe extern "C" fn(c_int)>,
    /// The view dismissed its clip card. The argument is the generation of the show that made that
    /// card, captured by the view when it dismissed (before it scheduled this callback), so a later
    /// show cannot be mistaken for the card that was dismissed.
    pub dismiss_clipboard: Option<unsafe extern "C" fn(u64)>,
    pub dismiss_hint: Option<unsafe extern "C" fn()>,
    pub set_screenshot_destination: Option<unsafe extern "C" fn(c_int)>,
    pub log: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    pub stop_recording: Option<unsafe extern "C" fn()>,
    pub cancel_processing: Option<unsafe extern "C" fn()>,
    pub start_dictation: Option<unsafe extern "C" fn()>,
    pub stop_and_send: Option<unsafe extern "C" fn()>,
    pub request_clipboard_items: Option<unsafe extern "C" fn()>,
    pub recover_terminal_error: Option<unsafe extern "C" fn(i64)>,
    pub terminal_error_generation: Option<unsafe extern "C" fn() -> i64>,
    pub splash_stalled: Option<unsafe extern "C" fn(i32) -> i32>,
    pub splash_action: Option<unsafe extern "C" fn(c_int)>,
    /// Returns 0 copied, 1 history empty, 2 failed.
    pub copy_last: Option<unsafe extern "C" fn() -> c_int>,
    /// The splash window has actually closed (a finished or forced hand-off, or already gone when
    /// the fly into the pill was requested).
    pub splash_closed: Option<unsafe extern "C" fn(u64)>,
}

/// The view's exported entry points, as the host's `extern` block declares them.
#[derive(Clone, Copy)]
pub struct Entry {
    pub init: unsafe extern "C" fn(),
    pub shutdown: unsafe extern "C" fn(),
    pub push_event: unsafe extern "C" fn(c_int, f64, bool, *const c_char),
    pub reveal: unsafe extern "C" fn(),
    pub set_visible: unsafe extern "C" fn(bool),
    pub is_visible: unsafe extern "C" fn() -> bool,
    pub refresh_action_sound_settings: unsafe extern "C" fn(),
    pub splash_handoff_active: unsafe extern "C" fn() -> bool,
    pub splash_handoff_force_complete: unsafe extern "C" fn(u64),
    /// `(items_json, mode, loaded, generation)`: the view applies the show only when `generation`
    /// is not older than the last show or close it applied.
    pub set_clipboard_items: unsafe extern "C" fn(*const c_char, c_int, bool, u64),
    /// `(generation)`: dropped by the view when older than the last show or close it applied.
    pub close_clipboard: unsafe extern "C" fn(u64),
    /// Whether the view's panel is actually presenting a clip card (the view's own state).
    pub clipboard_visible: unsafe extern "C" fn() -> bool,
    pub set_hint: unsafe extern "C" fn(c_int),
    /// Synchronously returns whether the native runtime accepted this exact show.
    pub splash_show: unsafe extern "C" fn(u64) -> bool,
    /// Synchronously returns whether this exact active or pending show was dismissed.
    pub splash_dismiss: unsafe extern "C" fn(u64) -> bool,
    pub splash_fly_into_pill: unsafe extern "C" fn(),
    pub hub_animate_from_pill: unsafe extern "C" fn(*mut c_void),
    pub hub_animate_to_pill: unsafe extern "C" fn(*mut c_void),
    pub register_callbacks: unsafe extern "C" fn(CallbackTable),
}

/// Completion of a hub route request, called at most once on the host's main thread.
pub type RouteCompletion = Box<dyn FnOnce(bool) + Send + 'static>;

/// What each callback does in the app. A function returning `bool` answers `true` for success; a
/// `false` is logged here as a fixed `outcome=failed` tag (the host never returns an error text).
/// A callback that needs the app (a window, a runtime) reaches it inside its own function.
#[derive(Clone, Copy)]
pub struct Actions {
    /// Write the text to the clipboard and return only when the write has finished (it runs on a
    /// background queue, so it may block).
    pub copy_text: fn(String) -> bool,
    /// Store the edited transcript for a record id that passed [`bridge::valid_token`]. Runs on a
    /// thread started here.
    pub update_history_item: fn(String, String) -> bool,
    /// Open the hub route (one of [`Config::routes`]) on the host's main thread and call `done`
    /// with whether it opened, from that thread, when it is done. `done` must be called at most
    /// once; if the host cannot schedule the work it drops `done` and nothing is logged.
    pub open_route: fn(&'static str, RouteCompletion),
    /// The view dismissed the card made by the show with this generation (compare it to the
    /// committed override with `clip_override::clear_if_current`; never allocate a generation here).
    pub dismiss_clipboard: fn(u64),
    pub dismiss_hint: fn(),
    /// Store the screenshot destination (one of [`Config::screenshot_destinations`]).
    pub set_screenshot_destination: fn(&'static str) -> bool,
    pub stop_recording: fn() -> bool,
    pub start_dictation: fn() -> bool,
    pub stop_and_send: fn() -> bool,
    pub cancel_processing: fn() -> bool,
    /// Refresh the clip card's items from the app's history.
    pub refresh_clipboard_items: fn() -> bool,
    /// Reset a terminal error. `generation` is the one the view saw, `None` when it passed a
    /// negative number.
    pub recover_terminal_error: fn(Option<u64>) -> bool,
    /// The current terminal-error generation, `None` when there is none.
    pub terminal_error_generation: fn() -> Option<u64>,
    /// Whether a splash that has been up for this many seconds counts as stalled.
    pub splash_stalled: fn(u64) -> bool,
    pub splash_retry: fn(),
    pub splash_quit: fn(),
    pub copy_diagnostics: fn() -> bool,
    pub reveal_diagnostics: fn() -> bool,
    /// Copy the newest transcript: `Done` copied, `Empty` nothing to copy, `Failed`.
    pub copy_last: fn() -> ActionOutcome,
    /// Whether the engine is warm and can serve a dictation request (gates the splash hand-off
    /// backstop).
    pub engine_ready: fn() -> bool,
    /// The native splash window has actually closed. Runs on a thread started here, so it may
    /// schedule work on the main thread. The host retires its launch hand-off here.
    /// Compare this token against the app's launch latch before retiring that latch. The app
    /// must retain the token across any main-thread hop, where it revalidates ownership.
    pub splash_closed: fn(u64),
}

/// The numbers the view's `push_event` decodes. The host's numbering; a state the view does not
/// draw is given the number of the state it is drawn as (`starting` is typically `recording`).
#[derive(Clone, Copy, Debug)]
pub struct WireKinds {
    pub hide: i32,
    pub idle: i32,
    pub starting: i32,
    pub recording: i32,
    pub voice: i32,
    pub processing: i32,
    pub success: i32,
    pub status: i32,
    pub error: i32,
}

/// Names of the diagnostics that do not carry the prefix.
#[derive(Clone, Copy, Debug)]
pub struct Names {
    /// Event and detail logged when the view's cancel button was pressed, before the work starts.
    pub cancel_requested: (&'static str, &'static str),
    /// Event of the splash hand-off lifecycle (`phase=handoff_forced reason=rust_deadline`).
    pub splash_lifecycle: &'static str,
    /// Event logged (`outcome=failed`) when a splash stall action failed.
    pub splash_action_error: &'static str,
}

/// Every host number, name and table.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Diagnostic prefix; an event is `<prefix>_<suffix>` (suffixes: `bridge`, `ffi_panic`,
    /// `copy_error`, `history_update_error`, `action`, `screenshot_destination_error`,
    /// `log_rejected`, `stop_error`, `start_error`, `stop_send_error`, `cancel_error`,
    /// `clipboard_refresh_error`, `terminal_reset_rejected`, `clipboard_dropped`).
    pub prefix: &'static str,
    pub names: Names,
    pub kinds: WireKinds,
    /// Longest label sent to the view (bytes).
    pub max_label_bytes: usize,
    /// Longest text the view may ask to copy or store (bytes).
    pub max_text_bytes: usize,
    /// Longest record id the view may pass (bytes).
    pub max_id_bytes: usize,
    /// Longest event name the view may log under (bytes).
    pub max_event_name_bytes: usize,
    /// Longest log message from the view (bytes).
    pub max_log_message_bytes: usize,
    /// Route names by the view's integer code, and the route for an unknown code.
    pub routes: &'static [&'static str],
    pub default_route: &'static str,
    /// Screenshot destinations by the view's integer code, and the one for an unknown code.
    pub screenshot_destinations: &'static [&'static str],
    pub default_screenshot_destination: &'static str,
    /// If the view reports the splash hand-off active for longer than this, Rust terminates it,
    /// but only once [`Actions::engine_ready`] says so (see [`SplashHandoff`]).
    pub splash_handoff_max_active: Duration,
}

/// One event in the shape the C ABI carries it.
struct WireEvent {
    kind: i32,
    /// `0.0..=1.0`; non-zero only for `Voice`.
    level: f64,
    cancellable: bool,
    /// Present for `Status` and `Error`; at most `max_label_bytes` bytes, free of NUL.
    label: Option<String>,
}

/// The process's one native view binding. Declare it as a `static` (`NativeView::new` is `const`)
/// and call [`NativeView::initialize`] once.
pub struct NativeView {
    entry: Entry,
    actions: Actions,
    config: Config,
    sink: Sink,
    handoff: Mutex<SplashHandoff>,
    /// Counts every [`NativeView::splash_show`] (a hand-off reset). Written under `handoff`; read
    /// before a native call made with `handoff` released, to revalidate after it.
    handoff_epoch: AtomicU64,
    /// Set before enqueueing a native show, cleared only by its generation-bearing close.
    /// A refused duplicate must not replace the identity of the still visible first splash.
    splash_in_flight: AtomicBool,
    /// Accessed under `handoff`. Native callbacks can reenter before show returns its ACK.
    pending_splash: AtomicU64,
    pending_splash_closed: AtomicBool,
    closed_epoch: AtomicU64,
    initialized: AtomicBool,
    stopped: AtomicBool,
    splash_skipped: AtomicBool,
    // No Rust lock is ever held across a call into the view: those calls can be synchronous hops
    // to the AppKit main thread. Clip ordering is the view's (it drops a show or close older than
    // the last one it applied, by generation) plus the `is_superseded` checks before and after.
}

/// The view the callbacks answer for, set by [`NativeView::initialize`].
static ACTIVE: OnceLock<&'static NativeView> = OnceLock::new();

impl NativeView {
    pub const fn new(entry: Entry, actions: Actions, config: Config, sink: Sink) -> NativeView {
        NativeView {
            entry,
            actions,
            config,
            sink,
            handoff: Mutex::new(SplashHandoff::new(config.splash_handoff_max_active)),
            handoff_epoch: AtomicU64::new(0),
            splash_in_flight: AtomicBool::new(false),
            pending_splash: AtomicU64::new(0),
            pending_splash_closed: AtomicBool::new(false),
            closed_epoch: AtomicU64::new(0),
            initialized: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            splash_skipped: AtomicBool::new(false),
        }
    }

    fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.config.prefix)
    }

    fn log(&self, suffix: &str, detail: &str) {
        (self.sink)(&self.name(suffix), detail);
    }

    /// Encode an event for the view. The host's [`WireKinds::starting`] decides how a `Starting`
    /// event is shown.
    fn wire_event(&self, event: &PillEvent) -> WireEvent {
        let kinds = &self.config.kinds;
        let max = self.config.max_label_bytes;
        let (kind, level, cancellable, label) = match event {
            PillEvent::Hide => (kinds.hide, 0.0, false, None),
            PillEvent::Idle => (kinds.idle, 0.0, false, None),
            PillEvent::Starting => (kinds.starting, 0.0, false, None),
            PillEvent::Recording => (kinds.recording, 0.0, false, None),
            PillEvent::Voice { level } => (kinds.voice, f64::from(*level), false, None),
            PillEvent::Processing { cancellable } => (kinds.processing, 0.0, *cancellable, None),
            PillEvent::Success => (kinds.success, 0.0, false, None),
            PillEvent::Status { label } => (
                kinds.status,
                0.0,
                false,
                Some(bridge::bounded_label(label, max)),
            ),
            PillEvent::Error { label } => (
                kinds.error,
                0.0,
                false,
                Some(bridge::bounded_label(label, max)),
            ),
        };
        let level = if level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            0.0
        };
        WireEvent {
            kind,
            level,
            cancellable,
            label,
        }
    }

    /// Register the callbacks and start the view. Call once; the view answers callbacks only
    /// after this.
    pub fn initialize(&'static self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if ACTIVE.set(self).is_err()
            && !ACTIVE
                .get()
                .is_some_and(|active| std::ptr::eq(*active, self))
        {
            self.log("bridge", "outcome=refused reason=binding_exists");
            return;
        }
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            (self.entry.register_callbacks)(CallbackTable {
                copy_text: Some(cb_copy_text),
                update_history_item: Some(cb_update_history_item),
                open_hub: Some(cb_open_hub),
                dismiss_clipboard: Some(cb_dismiss_clipboard),
                dismiss_hint: Some(cb_dismiss_hint),
                set_screenshot_destination: Some(cb_set_screenshot_destination),
                log: Some(cb_log),
                stop_recording: Some(cb_stop_recording),
                cancel_processing: Some(cb_cancel_processing),
                start_dictation: Some(cb_start_dictation),
                stop_and_send: Some(cb_stop_and_send),
                request_clipboard_items: Some(cb_request_clipboard_items),
                recover_terminal_error: Some(cb_recover_terminal_error),
                terminal_error_generation: Some(cb_terminal_error_generation),
                splash_stalled: Some(cb_splash_stalled),
                splash_action: Some(cb_splash_action),
                copy_last: Some(cb_copy_last),
                splash_closed: Some(cb_splash_closed),
            });
            (self.entry.init)();
        }
        self.log("bridge", "initialized");
    }

    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let _handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            self.pending_splash.store(0, Ordering::SeqCst);
            self.pending_splash_closed.store(false, Ordering::SeqCst);
            self.splash_in_flight.store(false, Ordering::SeqCst);
        }
        if self.initialized.load(Ordering::SeqCst) {
            unsafe { (self.entry.shutdown)() };
        }
    }

    pub fn push_event(&self, event: &PillEvent) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let wire = self.wire_event(event);
        let label = wire.label.as_deref().map(c_string);
        unsafe {
            (self.entry.push_event)(
                wire.kind,
                wire.level,
                wire.cancellable,
                label
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
            );
        }
    }

    pub fn reveal(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        unsafe { (self.entry.reveal)() };
    }

    pub fn set_visible(&self, visible: bool) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        unsafe { (self.entry.set_visible)(visible) };
    }

    pub fn is_visible(&self) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        unsafe { (self.entry.is_visible)() }
    }

    pub fn refresh_action_sound_settings(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        unsafe { (self.entry.refresh_action_sound_settings)() };
    }

    /// Whether the splash hand-off defers pill mutations. Redundant, Rust-side backstop behind the
    /// view's own deadline: a hand-off active for longer than [`Config::splash_handoff_max_active`]
    /// is terminated, but ONLY once the engine is warm. Forcing completion before the engine is
    /// ready tears the splash down and shows the pill while the engine cannot yet serve a
    /// dictation request, silently discarding the user's first speech. A forced hand-off is
    /// terminal for this launch ([`SplashHandoff`] keeps the latch).
    pub fn splash_handoff_active(&self) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        // The native read can be a synchronous main-thread hop, so `handoff` is released across it:
        // read the epoch under the lock, drop the lock, ask the view, re-take the lock and
        // revalidate (a `splash_show` reset or a forced hand-off in between makes the answer stale:
        // ask again; after two stale rounds report active, which only defers a pill mutation).
        let mut state = None;
        for _ in 0..2 {
            let epoch = {
                let handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
                if self.pending_splash.load(Ordering::SeqCst) != 0 {
                    return true;
                }
                if handoff.is_forced() {
                    return false;
                }
                self.handoff_epoch.load(Ordering::SeqCst)
            };
            let native_active = unsafe { (self.entry.splash_handoff_active)() };
            // App callbacks can also synchronously hop to AppKit. Sample readiness with no
            // Rust lock held, then revalidate the show that owns both answers.
            let engine_ready = native_active && (self.actions.engine_ready)();
            let mut handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            if self.pending_splash.load(Ordering::SeqCst) != 0 {
                return true;
            }
            if handoff.is_forced() {
                return false;
            }
            if self.handoff_epoch.load(Ordering::SeqCst) != epoch {
                continue;
            }
            // Engine not warm yet: the deadline elapsing is expected (a model compile can take
            // ~30s) and must NOT terminate the handoff early. `observe` keeps reporting active
            // until it is.
            state = Some((
                handoff.observe(native_active, || engine_ready, Instant::now()),
                epoch,
            ));
            break;
        }
        let Some((state, epoch)) = state else {
            return true;
        };
        if state.action == HandoffAction::ForceComplete {
            // A newer show may be queued during this hop. The native runtime accepts the
            // force only for this exact generation, so it cannot retire that newer launch.
            unsafe { (self.entry.splash_handoff_force_complete)(epoch) };
            (self.sink)(
                self.config.names.splash_lifecycle,
                "phase=handoff_forced reason=rust_deadline",
            );
        }
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        state.active
            || self.handoff_epoch.load(Ordering::SeqCst) != epoch
            || self.pending_splash.load(Ordering::SeqCst) != 0
    }

    /// Show the clip card. `generation` is the one the request got from `clip_override`
    /// (`commit`, `snapshot`): a request older than the committed override is dropped here, before
    /// the view's own queue can reorder it behind a newer one, and logged as
    /// `<prefix>_clipboard_dropped`. Returns whether the card was handed to the view.
    pub fn set_clipboard_items(
        &self,
        items_json: &str,
        mode: i32,
        loaded: bool,
        generation: u64,
    ) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        // No lock is held across the call: the slot's lock is taken and released inside
        // `is_superseded`. Two racing requests reach the view in either order; the view drops the
        // older generation.
        if crate::clip_override::is_superseded(generation) {
            self.log(
                "clipboard_dropped",
                &format!("request=show generation={generation}"),
            );
            return false;
        }
        let items = c_string(items_json);
        unsafe { (self.entry.set_clipboard_items)(items.as_ptr(), mode, loaded, generation) };
        // Revalidate: a newer write during the hop only needs a note; its own request follows.
        if crate::clip_override::is_superseded(generation) {
            self.log(
                "clipboard_dropped",
                &format!("request=show_applied_stale generation={generation}"),
            );
        }
        true
    }

    /// Close the clip card, with the generation of the write that asked for it (`clear_commit`,
    /// or the generation of an empty show). Dropped, like a show, when the committed override is
    /// newer: the close then belongs to a card that has since been replaced. Returns whether the
    /// close was handed to the view.
    pub fn close_clipboard(&self, generation: u64) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        if crate::clip_override::is_superseded(generation) {
            self.log(
                "clipboard_dropped",
                &format!("request=close generation={generation}"),
            );
            return false;
        }
        unsafe { (self.entry.close_clipboard)(generation) };
        if crate::clip_override::is_superseded(generation) {
            self.log(
                "clipboard_dropped",
                &format!("request=close_applied_stale generation={generation}"),
            );
        }
        true
    }

    /// Whether the view is actually presenting a clip card, read from the view itself
    /// (`Entry::clipboard_visible`), not from what Rust last asked. The toggle that opens or
    /// closes the card reads this instead of a window lookup.
    pub fn clipboard_visible(&self) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        unsafe { (self.entry.clipboard_visible)() }
    }

    /// [`Self::clipboard_visible`] for a caller that decides from `generation` (a
    /// `clip_override::snapshot`): no lock is held across the synchronous view call, and the
    /// generation is revalidated after it. `None` when a newer write superseded `generation`
    /// meanwhile (the answer may describe another card; read again).
    pub fn clipboard_visible_for(&self, generation: u64) -> Option<bool> {
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        let visible = unsafe { (self.entry.clipboard_visible)() };
        if crate::clip_override::is_superseded(generation) {
            return None;
        }
        Some(visible)
    }

    pub fn set_hint(&self, view: i32) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        unsafe { (self.entry.set_hint)(view) };
    }

    /// A new splash starts: forget a forced hand-off, then show it.
    /// The host allocates and commits this nonzero, strictly increasing token to its launch
    /// latch before calling. Returns false for an obsolete token, an active/pending show, or a
    /// shut-down binding. The host must not replace its accepted app latch on a false result.
    pub fn splash_show(&self, generation: u64) -> bool {
        {
            let _handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            if self.splash_in_flight.load(Ordering::SeqCst) {
                return false;
            }
            if generation == 0
                || generation <= self.handoff_epoch.load(Ordering::SeqCst)
                || generation <= self.closed_epoch.load(Ordering::SeqCst)
            {
                return false;
            }
            self.splash_in_flight.store(true, Ordering::SeqCst);
            self.pending_splash.store(generation, Ordering::SeqCst);
            self.pending_splash_closed.store(false, Ordering::SeqCst);
        }
        let accepted = unsafe { (self.entry.splash_show)(generation) };
        let closed = {
            let mut handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst)
                || self.pending_splash.load(Ordering::SeqCst) != generation
            {
                return false;
            }
            self.pending_splash.store(0, Ordering::SeqCst);
            let closed = self.pending_splash_closed.swap(false, Ordering::SeqCst);
            if !accepted {
                self.splash_in_flight.store(false, Ordering::SeqCst);
                return false;
            }
            handoff.reset();
            self.handoff_epoch.store(generation, Ordering::SeqCst);
            self.splash_skipped.store(false, Ordering::SeqCst);
            if closed {
                self.splash_in_flight.store(false, Ordering::SeqCst);
                self.closed_epoch.fetch_max(generation, Ordering::SeqCst);
            }
            closed
        };
        if closed {
            // The host binding is static, but `self` is not necessarily borrowed as static here.
            // Use the installed binding only when it is exactly this one.
            if let Some(view) = ACTIVE
                .get()
                .copied()
                .filter(|view| std::ptr::eq(*view, self))
            {
                notify_splash_closed(view, generation);
            }
        }
        true
    }

    /// Dismiss the request's captured native owner, never a newer launch. A false ACK leaves
    /// Rust ownership unchanged. Keep this token across app queues, rather than reading it later.
    pub fn splash_dismiss(&self, generation: u64) -> bool {
        {
            let _handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst) || !self.splash_in_flight.load(Ordering::SeqCst)
            {
                return false;
            }
            let pending = self.pending_splash.load(Ordering::SeqCst);
            let current = if pending != 0 {
                pending
            } else {
                self.handoff_epoch.load(Ordering::SeqCst)
            };
            if generation == 0 || generation != current {
                return false;
            }
        }
        if !unsafe { (self.entry.splash_dismiss)(generation) } {
            return false;
        }
        {
            let _handoff = self.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if self.pending_splash.load(Ordering::SeqCst) == generation {
                self.pending_splash.store(0, Ordering::SeqCst);
                self.pending_splash_closed.store(false, Ordering::SeqCst);
                self.splash_in_flight.store(false, Ordering::SeqCst);
                self.closed_epoch.fetch_max(generation, Ordering::SeqCst);
            } else if self.pending_splash.load(Ordering::SeqCst) == 0
                && self.handoff_epoch.load(Ordering::SeqCst) == generation
            {
                self.splash_in_flight.store(false, Ordering::SeqCst);
                self.closed_epoch.fetch_max(generation, Ordering::SeqCst);
            }
        }
        true
    }

    /// A hidden QA launch shows no splash, so the native hand-off never starts and has no flight
    /// to finish: the view's fly-into-pill bails on its missing panel and the pill would never
    /// reveal. After this the first [`NativeView::splash_fly_into_pill`] reveals directly instead.
    pub fn note_splash_skipped(&self) {
        self.splash_skipped.store(true, Ordering::SeqCst);
    }

    /// Fly the splash into the pill, or call `reveal_directly` when [`Self::note_splash_skipped`]
    /// ran.
    pub fn splash_fly_into_pill(&self, reveal_directly: impl FnOnce()) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if self.splash_skipped.load(Ordering::SeqCst) {
            reveal_directly();
            return;
        }
        unsafe { (self.entry.splash_fly_into_pill)() };
    }

    /// Animate the host's native hub window from the pill.
    ///
    /// # Safety
    ///
    /// `window` must point to a live native window of the type expected by the configured
    /// [`Entry`]. The caller must keep it alive until this call returns and satisfy the native
    /// view's threading requirements. An entry point that uses the window asynchronously must
    /// retain it before returning and keep it alive until that work finishes.
    pub unsafe fn animate_hub_from_pill(&self, window: *mut c_void) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: The caller guarantees the window lifetime, type and native thread contract.
        unsafe { (self.entry.hub_animate_from_pill)(window) };
    }

    /// Animate the host's native hub window to the pill.
    ///
    /// # Safety
    ///
    /// `window` must point to a live native window of the type expected by the configured
    /// [`Entry`]. The caller must keep it alive until this call returns and satisfy the native
    /// view's threading requirements. An entry point that uses the window asynchronously must
    /// retain it before returning and keep it alive until that work finishes.
    pub unsafe fn animate_hub_to_pill(&self, window: *mut c_void) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: The caller guarantees the window lifetime, type and native thread contract.
        unsafe { (self.entry.hub_animate_to_pill)(window) };
    }

    /// Run `run` on a new thread; a `false` answer is logged as `<prefix>_<suffix> outcome=failed`.
    fn spawn_action(&'static self, suffix: &'static str, run: fn() -> bool) {
        std::thread::spawn(move || {
            if !run() {
                self.log(suffix, "outcome=failed");
            }
        });
    }
}

/// Run a callback body and return `default` if it panics (or if no view is installed yet): a panic
/// must not unwind across the C boundary into the view. The panic is reported by a fixed tag only
/// (never its text).
fn guard<R>(default: R, body: impl FnOnce(&'static NativeView) -> R) -> R {
    let Some(view) = ACTIVE.get().copied() else {
        return default;
    };
    if view.stopped.load(Ordering::SeqCst) {
        return default;
    }
    bridge::guarded(
        default,
        || view.log("ffi_panic", "outcome=caught"),
        || body(view),
    )
}

/// Copy `text` to the clipboard and report the result: 0 copied, 2 failed. The view calls this from
/// a background queue (never the AppKit thread), so the blocking clipboard write runs here and the
/// answer is real: the pill shows "copied" only after a 0. A refusal (text over
/// [`Config::max_text_bytes`], checked before the text is copied), a clipboard error and a panic
/// all answer 2. Nothing about the text or the error string is logged, only the outcome.
unsafe extern "C" fn cb_copy_text(text: *const c_char) -> c_int {
    guard(2, |view| {
        let Some(text) = (unsafe { string_from_c(text, view.config.max_text_bytes) }) else {
            view.log("copy_error", "outcome=refused reason=too_large");
            return 2;
        };
        if (view.actions.copy_text)(text) {
            0
        } else {
            view.log("copy_error", "outcome=failed");
            2
        }
    })
}

unsafe extern "C" fn cb_update_history_item(delivery_id: *const c_char, transcript: *const c_char) {
    guard((), |view| {
        let delivery_id = unsafe { string_from_c(delivery_id, view.config.max_id_bytes) };
        let transcript = unsafe { string_from_c(transcript, view.config.max_text_bytes) };
        let (Some(delivery_id), Some(transcript)) = (delivery_id, transcript) else {
            view.log("history_update_error", "outcome=refused reason=too_large");
            return;
        };
        if bridge::valid_token(&delivery_id, view.config.max_id_bytes).is_none() {
            view.log(
                "history_update_error",
                "outcome=refused reason=malformed_id",
            );
            return;
        }
        std::thread::spawn(move || {
            if !(view.actions.update_history_item)(delivery_id, transcript) {
                view.log("history_update_error", "outcome=failed");
            }
        });
    })
}

unsafe extern "C" fn cb_open_hub(tab: c_int) {
    guard((), |view| {
        // An unknown code opens the default route.
        let route = bridge::choice_or(tab, view.config.routes, view.config.default_route);
        view.log(
            "action",
            &format!("action=open_hub route={route} phase=callback_received"),
        );
        std::thread::spawn(move || {
            let started = Instant::now();
            (view.actions.open_route)(
                route,
                Box::new(move |opened| {
                    view.log(
                        "action",
                        &format!(
                            "action=open_hub route={route} phase=result outcome={} elapsed_ms={}",
                            if opened { "ok" } else { "error" },
                            started.elapsed().as_millis()
                        ),
                    );
                }),
            );
        });
    })
}

unsafe extern "C" fn cb_dismiss_clipboard(generation: u64) {
    guard((), |view| {
        let dismiss = view.actions.dismiss_clipboard;
        std::thread::spawn(move || dismiss(generation));
    })
}

unsafe extern "C" fn cb_dismiss_hint() {
    guard((), |view| {
        std::thread::spawn(view.actions.dismiss_hint);
    })
}

unsafe extern "C" fn cb_set_screenshot_destination(destination: c_int) {
    guard((), |view| {
        std::thread::spawn(move || {
            // An unknown code means the default destination.
            let destination = bridge::choice_or(
                destination,
                view.config.screenshot_destinations,
                view.config.default_screenshot_destination,
            );
            if !(view.actions.set_screenshot_destination)(destination) {
                view.log("screenshot_destination_error", "outcome=failed");
            }
        });
    })
}

unsafe extern "C" fn cb_log(surface: *const c_char, message: *const c_char) {
    guard((), |view| {
        // One byte more than the longest valid name, so an over-long name is refused, not cut
        // into a valid one.
        let surface = unsafe { prefix_from_c(surface, view.config.max_event_name_bytes + 1) };
        let message = unsafe { prefix_from_c(message, view.config.max_log_message_bytes) };
        // The native view names its own telemetry events; the name is a small safe alphabet and
        // the message is bounded, so it cannot forge another surface's events or flood a log line.
        let Some(surface) = bridge::valid_event_name(&surface, view.config.max_event_name_bytes)
        else {
            view.log("log_rejected", "reason=invalid_event_name");
            return;
        };
        // `log_text` keeps `key=value` codes and turns everything else (control characters, line
        // breaks, path separators, non-ASCII text) into `?`.
        (view.sink)(
            surface,
            &bridge::log_text(&message, view.config.max_log_message_bytes),
        );
    })
}

unsafe extern "C" fn cb_stop_recording() {
    guard((), |view| {
        view.spawn_action("stop_error", view.actions.stop_recording)
    })
}

unsafe extern "C" fn cb_start_dictation() {
    guard((), |view| {
        view.spawn_action("start_error", view.actions.start_dictation)
    })
}

unsafe extern "C" fn cb_stop_and_send() {
    guard((), |view| {
        view.spawn_action("stop_send_error", view.actions.stop_and_send)
    })
}

unsafe extern "C" fn cb_cancel_processing() {
    guard((), |view| {
        let (event, detail) = view.config.names.cancel_requested;
        (view.sink)(event, detail);
        view.spawn_action("cancel_error", view.actions.cancel_processing);
    })
}

unsafe extern "C" fn cb_request_clipboard_items() {
    guard((), |view| {
        view.log("action", "action=open_clipboard phase=callback_received");
        std::thread::spawn(move || {
            let started = Instant::now();
            if !(view.actions.refresh_clipboard_items)() {
                view.log("clipboard_refresh_error", "outcome=failed");
            } else {
                view.log(
                    "action",
                    &format!(
                        "action=open_clipboard phase=result outcome=ok elapsed_ms={}",
                        started.elapsed().as_millis()
                    ),
                );
            }
        });
    })
}

/// Called by the view on a background queue, so the blocking call is safe.
/// 0 copied, 1 history empty, 2 failed (also when the call panics).
unsafe extern "C" fn cb_copy_last() -> c_int {
    guard(2, |view| match (view.actions.copy_last)() {
        ActionOutcome::Done => 0,
        ActionOutcome::Empty => 1,
        ActionOutcome::Failed => 2,
    })
}

/// The terminal-error generation, or -1 when there is none (also when the call panics).
unsafe extern "C" fn cb_terminal_error_generation() -> i64 {
    guard(-1, |view| {
        (view.actions.terminal_error_generation)().map_or(-1, |generation| generation as i64)
    })
}

/// 1 when the splash counts as stalled, else 0 (also when the call panics).
unsafe extern "C" fn cb_splash_stalled(waited_secs: i32) -> i32 {
    guard(0, |view| {
        i32::from((view.actions.splash_stalled)(waited_secs.max(0) as u64))
    })
}

/// 0 retry, 1 quit, 2 copy diagnostics, 3 reveal diagnostics; any other code does nothing.
unsafe extern "C" fn cb_splash_action(action: c_int) {
    guard((), |view| {
        std::thread::spawn(move || {
            let succeeded = match action {
                0 => {
                    (view.actions.splash_retry)();
                    true
                }
                1 => {
                    (view.actions.splash_quit)();
                    true
                }
                2 => (view.actions.copy_diagnostics)(),
                3 => (view.actions.reveal_diagnostics)(),
                _ => true,
            };
            if !succeeded {
                (view.sink)(view.config.names.splash_action_error, "outcome=failed");
            }
        });
    })
}

unsafe extern "C" fn cb_recover_terminal_error(generation: i64) {
    guard((), |view| {
        let generation = u64::try_from(generation).ok();
        std::thread::spawn(move || {
            if !(view.actions.recover_terminal_error)(generation) {
                view.log("terminal_reset_rejected", "result=rejected");
            }
        });
    })
}

unsafe extern "C" fn cb_splash_closed(generation: u64) {
    guard((), |view| {
        {
            let _handoff = view.handoff.lock().unwrap_or_else(|e| e.into_inner());
            if generation != 0 && view.pending_splash.load(Ordering::SeqCst) == generation {
                view.pending_splash_closed.store(true, Ordering::SeqCst);
                return;
            }
            if generation == 0 || view.handoff_epoch.load(Ordering::SeqCst) != generation {
                return;
            }
            // Native animation completion and forced completion may both report retirement.
            // Keep only the first retirement per generation; never lower this watermark.
            if view.closed_epoch.fetch_max(generation, Ordering::SeqCst) >= generation {
                return;
            }
            view.splash_in_flight.store(false, Ordering::SeqCst);
        }
        notify_splash_closed(view, generation);
    })
}

fn notify_splash_closed(view: &'static NativeView, generation: u64) {
    std::thread::spawn(move || {
        if !view.stopped.load(Ordering::SeqCst)
            && view.handoff_epoch.load(Ordering::SeqCst) == generation
        {
            (view.actions.splash_closed)(generation);
        }
    });
}
