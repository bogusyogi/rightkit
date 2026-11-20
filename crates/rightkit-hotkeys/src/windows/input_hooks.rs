//! One install for the process's low-level input hooks: the keyboard hook (cancel guard and
//! push-to-talk, [`ptt_hook`](super::ptt_hook)) and the mouse hook (middle-click toggle,
//! [`mouse_hook`](super::mouse_hook)) on one [`LowLevelHooks`] thread, plus the off-thread
//! dispatch and telemetry workers.
//!
//! Windows lets this design one `WH_KEYBOARD_LL` and one `WH_MOUSE_LL` consumer per process (the
//! slots of [`LowLevelHooks`]); both belong here together. It cannot coexist with
//! [`hub`](super::hub) in one process: the hub claims the same slots, and it never hands modifier
//! keys to its handlers, which push-to-talk on Alt or Ctrl needs.
//!
//! Invariants:
//! - The hook thread only pumps messages and runs the handlers (plus, between key events, the
//!   arm-timer message handler). Host work (`on_cancel`, `on_ptt`) runs on dispatch threads, never
//!   on the hook thread.
//! - Telemetry from the handlers goes through a lane to its own drain thread, so a slow sink can
//!   never delay the next hook callback; the text is built on that thread.
//! - `on_cancel` and `on_ptt` run on SEPARATE threads: a push-to-talk callback that stalls cannot
//!   hold a Ctrl+Escape cancel back. Push-to-talk events stay in order on their own thread.
//!
//! # Lifecycle (HANDOFF.md, "Lifecycle")
//!
//! Everything one [`InputHooks::install`] starts belongs to one generation. Ending it, by dropping
//! the [`InputHooks`] or by [`InputHooks::end`], runs in this order:
//!
//! 0. `KeyboardState::quiesce` and `MiddleClickState::quiesce`: the install's authority ends. From
//!    here the handlers act on nothing new: a key or click passes untouched, nothing is recorded or
//!    posted, with one exception (owed key-ups and button-ups, below). The arm timer is asked to
//!    stop and `post_gate` is taken once, so a timer callback between its generation check and its
//!    one `PostThreadMessageW` has finished and none can pass the check afterwards;
//! 1. the hooks are removed on a helper thread (`rk-input-hook-remove`) and waited for at most
//!    [`HOOK_REMOVE_WAIT`]. The crate's `LowLevelHooks::drop` joins the hook thread without a bound,
//!    so the wait is bounded here, not there. If it runs out, the drop returns anyway and reports
//!    `input_hooks_error`; the hook stays installed but inert (step 0) until the helper finishes,
//!    and a new install is refused with `AlreadyInstalled` until then (no new generation can start
//!    while an old hook thread can still run handlers on the same state objects);
//! 2. the keyboard session ends: the command, cancel and telemetry lanes close, so the dispatch,
//!    cancel and telemetry threads drain what was queued and exit and the per-gesture state is
//!    reset. A start the host was given and no end for is ended by one `PttEvent::Cancel` from the
//!    dispatch thread; queued starts are discarded;
//! 3. the mouse worker is stopped: held clicks are settled (replayed if the target still verifies),
//!    a resolver blocked in a UI Automation call is abandoned (detached; its late answer is
//!    dropped), and the toggle lane ends once a running host callback returns.
//!
//! OWED KEY-UPS. A swallowed key-down (push-to-talk key, companion modifier) has its repeats and its
//! key-up swallowed while a hook that can see them is installed, including after step 0. The debt is
//! dropped only when the hook thread is gone (`KeyboardState::on_hook_removed`,
//! `MiddleClickState::on_hook_removed`). If the hook is removed while such a key or the middle button
//! is held, its release reaches the application without its press; nothing can prevent that without a
//! hook. It is reported once (`hook_key_up_owed`, `middle_mouse_win_error`).
//!
//! COST OF `Drop`. `Drop` blocks the dropping thread for the quiesce step (one wait for a
//! `PostThreadMessageW` and a preempted timer thread; no host code runs under it) plus the removal
//! wait (at most [`HOOK_REMOVE_WAIT`]); everything else in it is a lock, a channel send or an atomic
//! store. Only if no helper thread can be created does the removal run on the dropping thread, where
//! the crate's join is unbounded. [`InputHooks::end`] returns at once and runs the same teardown on a
//! helper thread (`rk-input-hooks-end`).
//!
//! DROP ORDER AND HOST CALLS. Every state change of a teardown is made by a drop guard, so it also
//! happens when something panics: the `HOOKS_NOT_REMOVED` decrement (`NotRemoved`), the keyboard
//! session end and the mouse worker stop (`EndSessions`). The host's sink is called LAST and never on
//! the dropping thread: the lines a teardown has (`input_hooks_error`, `hook_key_up_owed`,
//! `middle_mouse_win_error`) are handed to `lifecycle::report_detached` (a detached thread, at most
//! 4 in the process, at most 4 lines each) after the guards have run, inside `catch_unwind`; no
//! host call is made on a path that holds bookkeeping, and a sink that never returns pins that one
//! thread and nothing else. No host callback runs on the dropping thread either.
//!
//! A failed or refused install ends everything it had started (the same drop path). A second
//! install while one is live, or while a previous install's hook thread has not exited, is refused
//! with `HookError::AlreadyInstalled`; the refusal touches nothing.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::mouse_hook::{MiddleClickState, StartError};
use super::ptt_hook::{
    no_key_cause, EventSink, HookCommand, KbSession, KeyboardState, PttEvent, QueuedHookCommand, MSG_PTT_ARM_ELAPSED,
};
use super::{HookConfig, HookError, LowLevelHooks};
use crate::lifecycle::{report_detached, run_dispatch, Cmd};
use crate::release_edge::NativeReleaseEdge;

/// Physical PTT release callback, run off the hook thread in command order.
/// None is native clock failure, never permission to sample dispatch time.
pub type PttReleaseCallback = Box<dyn Fn(PttEvent, Option<NativeReleaseEdge>) + Send + 'static>;

/// The longest `InputHooks::drop` waits for the hook thread to finish and the hooks to be removed
/// (after `KeyboardState::quiesce`).
pub const HOOK_REMOVE_WAIT: Duration = Duration::from_millis(1_500);

/// Hook threads this module installed whose removal has not finished (counted from a successful
/// `LowLevelHooks::install` until `Removal::drop` has joined the thread and told the keyboard and
/// mouse states it is gone). `InputHooks::install` is refused while it is nonzero.
static HOOKS_NOT_REMOVED: AtomicUsize = AtomicUsize::new(0);

/// `n` hook threads counted in `HOOKS_NOT_REMOVED`; the count goes down when this is dropped, also
/// when something panics (a drop guard: the one place the counter is decremented).
struct NotRemoved(usize);

impl Drop for NotRemoved {
    fn drop(&mut self) {
        if self.0 != 0 {
            HOOKS_NOT_REMOVED.fetch_sub(self.0, Ordering::SeqCst);
        }
    }
}

/// Host sink lines collected during a teardown and handed over when this is dropped: after every
/// guard declared after it has run (locals and fields drop in reverse order, so it is declared
/// first). The hand-off never waits for the host (`lifecycle::report_detached`) and is inside
/// `catch_unwind`.
struct ReportOnDrop {
    sink: EventSink,
    lines: Vec<(&'static str, String)>,
}

impl ReportOnDrop {
    fn new(sink: EventSink) -> Self {
        Self {
            sink,
            lines: Vec::new(),
        }
    }

    fn push(&mut self, event: &'static str, message: String) {
        self.lines.push((event, message));
    }
}

impl Drop for ReportOnDrop {
    fn drop(&mut self) {
        let lines = std::mem::take(&mut self.lines);
        let sink = self.sink;
        let _ = catch_unwind(AssertUnwindSafe(|| report_detached(sink, lines)));
    }
}

/// Ends the keyboard session and the mouse worker of a teardown when dropped (a drop guard, so it
/// also runs when the removal or a quiesce panics).
struct EndSessions {
    keyboard: Option<&'static KeyboardState>,
    mouse: Option<&'static MiddleClickState>,
}

impl Drop for EndSessions {
    fn drop(&mut self) {
        if let Some(state) = self.keyboard.take() {
            state.end_session();
        }
        if let Some(state) = self.mouse.take() {
            state.stop_worker();
        }
    }
}

/// Host callbacks for the keyboard hook. Both run on the dispatch thread.
pub struct KeyboardHookConfig {
    pub state: &'static KeyboardState,
    /// Ctrl+Escape pressed while recording (or while the onboarding chord test is active).
    pub on_cancel: Box<dyn Fn() + Send + 'static>,
    /// A push-to-talk event: start, stop, send, cancel (with its cause) or onboarding preview.
    pub on_ptt: Box<dyn Fn(PttEvent) + Send + 'static>,
}

pub struct MouseHookConfig {
    pub state: &'static MiddleClickState,
}

pub struct InputHooksConfig {
    pub keyboard: Option<KeyboardHookConfig>,
    pub mouse: Option<MouseHookConfig>,
    /// Host telemetry sink; called only off the hook thread.
    pub event_sink: EventSink,
}

/// The installed hooks and everything started for them. Dropping it (or [`InputHooks::end`])
/// uninstalls: it ends the install's authority, removes the hooks, ends the keyboard session and
/// stops the mouse worker (module notes, "Lifecycle"). `Drop` blocks the dropping thread for one quiesce step plus at most
/// [`HOOK_REMOVE_WAIT`]; [`InputHooks::end`] does not. Threads the install
/// started that are inside a host callback finish it, and none acts on a later install.
pub struct InputHooks {
    hooks: Vec<LowLevelHooks>,
    /// Counts `hooks` in `HOOKS_NOT_REMOVED`; declared after `hooks` so a plain drop joins the
    /// threads first and then decrements.
    pending: NotRemoved,
    /// The host's sink, for the lines a teardown can have (`ReportOnDrop`).
    sink: EventSink,
    /// Keyboard state whose session this install began; ended on drop, after the hooks are
    /// removed.
    keyboard: Option<&'static KeyboardState>,
    /// Mouse state whose resolver worker this install started; stopped on drop, after the hooks
    /// are removed, so every click still held is settled (`mouse_hook`, "Held click outcomes").
    mouse: Option<&'static MiddleClickState>,
}

impl InputHooks {
    fn end_keyboard(&mut self) {
        if let Some(state) = self.keyboard.take() {
            state.end_session();
        }
    }

    fn end_mouse(&mut self) {
        if let Some(state) = self.mouse.take() {
            state.stop_worker();
        }
    }

    /// Record a hook thread this install owns (it counts against `HOOKS_NOT_REMOVED`).
    fn track(&mut self, hooks: LowLevelHooks) {
        HOOKS_NOT_REMOVED.fetch_add(1, Ordering::SeqCst);
        self.pending.0 += 1;
        self.hooks.push(hooks);
    }

    /// Uninstall without blocking the calling thread: the teardown `Drop` performs (quiesce, hook
    /// removal waited for at most [`HOOK_REMOVE_WAIT`], session and worker end) runs on a helper
    /// thread (`rk-input-hooks-end`) and this returns at once. `true` when it was handed to a
    /// helper. `false` when no thread could be created: the teardown then ran on THIS thread, with
    /// the cost documented for `Drop`.
    ///
    /// A new install is refused with `HookError::AlreadyInstalled` until the helper has finished.
    pub fn end(self) -> bool {
        thread::Builder::new()
            .name("rk-input-hooks-end".to_string())
            .spawn(move || drop(self))
            .is_ok()
    }
}

impl Drop for InputHooks {
    fn drop(&mut self) {
        // Declared first, so dropped last: the host's sink lines go out after every state change.
        let mut reports = ReportOnDrop::new(self.sink);
        let keyboard = self.keyboard.take();
        let mouse = self.mouse.take();
        // Ends the session and the worker whatever happens below, after the removal.
        let _ends = EndSessions { keyboard, mouse };
        // The install's authority ends first: no handler acts on anything new from here, and no
        // arm-timer callback can post to a hook thread that is about to exit.
        if let Some(state) = keyboard {
            state.quiesce();
        }
        if let Some(state) = mouse {
            state.quiesce();
        }
        // Remove the hooks next. Then (guard) end the sessions. Then (guard) report.
        let removal = Removal {
            hooks: std::mem::take(&mut self.hooks),
            _count: std::mem::replace(&mut self.pending, NotRemoved(0)),
            report: ReportOnDrop::new(self.sink),
            keyboard,
            mouse,
        };
        if !remove_hooks(removal) {
            reports.push("input_hooks_error", "removal_wait_exceeded".to_string());
        }
    }
}

/// The hook threads of one install and the states that must be told once they are gone. Dropping it
/// joins each hook thread (the crate's `LowLevelHooks::drop`, unbounded), THEN tells the states (so
/// they drop their owed key-up and button-up debts: nothing can see those releases any more), THEN
/// lets a new install start. Done in `Drop` so that it also happens when a helper thread could not
/// be created and the closure holding this was dropped on the calling thread.
///
/// Field order is drop order: `hooks`, then `_count` (the new install may start), then `report` (the
/// host's sink lines). `Drop::drop` itself only joins and clears the debts; it calls no host code.
struct Removal {
    hooks: Vec<LowLevelHooks>,
    _count: NotRemoved,
    report: ReportOnDrop,
    keyboard: Option<&'static KeyboardState>,
    mouse: Option<&'static MiddleClickState>,
}

impl Drop for Removal {
    fn drop(&mut self) {
        self.hooks.clear();
        if let Some(state) = self.keyboard {
            let owed = state.on_hook_removed();
            if owed > 0 {
                self.report.push("hook_key_up_owed", format!("owed={owed}"));
            }
        }
        if let Some(state) = self.mouse {
            if state.on_hook_removed() {
                self.report.push(
                    "middle_mouse_win_error",
                    "hook_removed_while_middle_down".to_string(),
                );
            }
        }
    }
}

/// Remove the hooks on a helper thread and wait for it at most [`HOOK_REMOVE_WAIT`]. `false` when
/// the wait ran out: the helper finishes the removal later (and then tells the states).
fn remove_hooks(removal: Removal) -> bool {
    if removal.hooks.is_empty() {
        // No hook thread ever ran: nothing to join, the states are told inline.
        drop(removal);
        return true;
    }
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let spawned = thread::Builder::new()
        .name("rk-input-hook-remove".to_string())
        .spawn(move || {
            drop(removal);
            let _ = done_tx.send(());
        });
    match spawned {
        Ok(_) => done_rx.recv_timeout(HOOK_REMOVE_WAIT).is_ok(),
        // The closure, and with it `removal`, was dropped on this thread: removed inline.
        Err(_) => true,
    }
}

impl InputHooks {
    /// Install the requested hooks. When the combined install fails (one of the two hooks is
    /// refused), each hook is tried on its own so a refused mouse hook does not cost push-to-talk
    /// (and the reverse); the error is reported through the sink. Fails only when none installs;
    /// then everything this call started has been ended. `HookError::AlreadyInstalled` when a
    /// session of the same state object is still live.
    pub fn install(config: InputHooksConfig) -> Result<Self, HookError> {
        Self::install_inner(config, None)
    }

    /// Install with a callback replacing `on_ptt` only for physical Stop/Send.
    /// Forced ends, cancel, start & preview continue through `on_ptt`.
    pub fn install_with_ptt_release_edges(
        config: InputHooksConfig,
        on_release: PttReleaseCallback,
    ) -> Result<Self, HookError> {
        Self::install_inner(config, Some(on_release))
    }

    fn install_inner(
        config: InputHooksConfig,
        on_release: Option<PttReleaseCallback>,
    ) -> Result<Self, HookError> {
        // A previous install's hook thread that has not exited can still run its handlers on the
        // same state objects: no new generation starts until it is gone.
        if HOOKS_NOT_REMOVED.load(Ordering::SeqCst) != 0 {
            return Err(HookError::AlreadyInstalled);
        }
        let InputHooksConfig {
            keyboard,
            mouse,
            event_sink,
        } = config;
        let kb_state = keyboard.as_ref().map(|k| k.state);
        let mouse_state = mouse.map(|m| m.state);
        let mut kb_session: Option<Arc<KbSession>> = None;

        // From here on `this` owns everything that was started; any early return drops it, which
        // ends it.
        let mut this = InputHooks {
            hooks: Vec::new(),
            pending: NotRemoved(0),
            sink: event_sink,
            keyboard: None,
            mouse: None,
        };

        if let Some(keyboard) = keyboard {
            match start_keyboard(keyboard, on_release) {
                Ok(session) => {
                    kb_session = Some(session);
                    this.keyboard = kb_state;
                }
                Err(HookError::AlreadyInstalled) => return Err(HookError::AlreadyInstalled),
                Err(HookError::Install(err)) => {
                    // The error text goes to the caller in the `Err`; the sink gets a tag.
                    event_sink(
                        "recording_cancel_hook_error",
                        "keyboard_session_start_failed",
                    );
                    return Err(HookError::Install(err));
                }
            }
        }

        // The mouse hook holds clicks and needs its resolver worker: without one no click may
        // be held, so the mouse hook is not installed.
        if let Some(state) = mouse_state {
            match state.start_worker() {
                Ok(()) => {
                    // Dropped clicks are reported through the host's sink (off the hook thread).
                    state.attach_event_sink(event_sink);
                    this.mouse = Some(state);
                }
                Err(StartError::AlreadyRunning) => return Err(HookError::AlreadyInstalled),
                Err(StartError::Spawn(err)) => {
                    event_sink("middle_mouse_win_error", "mouse_worker_start_failed");
                    if kb_state.is_none() {
                        return Err(HookError::Install(err));
                    }
                }
            }
        }

        let build = |with_keyboard: bool, with_mouse: bool| {
            let mut cfg = HookConfig::new();
            if let (true, Some(state), Some(session)) =
                (with_keyboard, kb_state, kb_session.as_ref())
            {
                // Both handlers hold this install's own session (so the hook thread reads no shared
                // slot and takes no lock) and act only while its generation is the live one.
                let key_session = Arc::clone(session);
                let message_session = Arc::clone(session);
                cfg = cfg
                    .keyboard(move |event| state.handle_key_in(&key_session, event))
                    // The arm timer posts here; the recording is only ever started on this thread.
                    .thread_message(move |message, wparam, _| {
                        if message == MSG_PTT_ARM_ELAPSED {
                            state.on_arm_elapsed(&message_session, wparam);
                        }
                    });
            }
            if let (true, Some(state)) = (with_mouse, mouse_state) {
                cfg = cfg.mouse(move |event| state.handle_mouse(event));
            }
            cfg
        };
        let installed = |kb: bool, ms: bool| {
            if kb {
                event_sink("recording_cancel_hook", "installed");
            }
            if ms {
                event_sink("middle_mouse_win", "installed");
            }
        };

        let (want_kb, want_ms) = (this.keyboard.is_some(), this.mouse.is_some());
        match LowLevelHooks::install(build(want_kb, want_ms)) {
            Ok(hooks) => {
                installed(want_kb, want_ms);
                this.track(hooks);
                Ok(this)
            }
            Err(first) if want_kb && want_ms => {
                // The error is matched, never formatted: the sink gets a tag (the message of an
                // `Install` error is an operating-system text and stays with the caller).
                event_sink(
                    "input_hooks_error",
                    hook_error_tag(&first, "combined_install_failed"),
                );
                for (kb, ms) in [(true, false), (false, true)] {
                    match LowLevelHooks::install(build(kb, ms)) {
                        Ok(one) => {
                            installed(kb, ms);
                            this.track(one);
                        }
                        Err(err) => {
                            event_sink(
                                if kb {
                                    "recording_cancel_hook_error"
                                } else {
                                    "middle_mouse_win_error"
                                },
                                hook_error_tag(&err, "hook_install_failed"),
                            );
                            // A refused hook ends the half of the install that served it.
                            if kb {
                                this.end_keyboard();
                            } else {
                                this.end_mouse();
                            }
                        }
                    }
                }
                if this.hooks.is_empty() {
                    // `this` drops here; nothing it started survives.
                    Err(first)
                } else {
                    Ok(this)
                }
            }
            Err(err) => {
                event_sink(
                    if want_kb {
                        "recording_cancel_hook_error"
                    } else {
                        "middle_mouse_win_error"
                    },
                    hook_error_tag(&err, "hook_install_failed"),
                );
                // `this` drops here and ends the session and the worker it started.
                Err(err)
            }
        }
    }
}

/// A stable tag for a failed install: the variant, never its payload (`Install` carries an
/// operating-system message, which is not formatted into any sink text).
fn hook_error_tag(error: &HookError, failed: &'static str) -> &'static str {
    match error {
        HookError::AlreadyInstalled => "already_installed",
        HookError::Install(_) => failed,
    }
}

/// What the dispatch thread delivers, as the pure lifecycle code sees it.
fn command_kind(command: &QueuedHookCommand) -> Cmd {
    match &command.command {
        HookCommand::Cancel => Cmd::HookCancel,
        HookCommand::Ptt(PttEvent::Start) => Cmd::PttStart,
        HookCommand::Ptt(PttEvent::Stop) => Cmd::PttStop,
        HookCommand::Ptt(PttEvent::Send) => Cmd::PttSend,
        HookCommand::Ptt(PttEvent::Cancel(_)) => Cmd::PttCancel,
        HookCommand::Ptt(PttEvent::Preview(down)) => Cmd::Preview(*down),
    }
}

/// Report commands the lanes dropped because the host's callbacks fell far behind (or because a
/// `Start` did not fit: nothing else of that gesture is sent either).
fn report_overflow(session: &KbSession, state: &KeyboardState) {
    let dropped = session.take_command_overflow();
    if dropped > 0 {
        state.sink("hook_command_dropped", &format!("dropped={dropped}"));
    }
}

/// Begin the keyboard session and start its three worker threads: the telemetry drain, the
/// push-to-talk dispatch and the cancel dispatch. The lanes are lock-free and bounded; the hook
/// thread itself only pumps messages: an LL hook is serviced only while its thread pumps, so
/// running host work (which can take locks and wait on other processes) there stalls keystrokes
/// system-wide and risks Windows silently removing the hook. All three threads end when the
/// session's lanes close (uninstall), after draining what was queued; none is ever joined.
fn start_keyboard(
    keyboard: KeyboardHookConfig,
    on_release: Option<PttReleaseCallback>,
) -> Result<Arc<KbSession>, HookError> {
    let KeyboardHookConfig {
        state,
        on_cancel,
        on_ptt,
    } = keyboard;
    let (session, command_rx, telemetry_rx) = state.begin_session()?;
    let Some(cancel_rx) = session.take_cancel_rx() else {
        state.end_session();
        return Err(HookError::Install("cancel lane unavailable".to_string()));
    };

    // Dedicated off-thread drain for telemetry queued from the hook proc: fully decoupled from
    // the hook thread's own message pump so a slow or contended disk write here can never delay
    // the next WH_KEYBOARD_LL dispatch. The text is built here, not on the hook thread.
    let telemetry_session = Arc::clone(&session);
    let telemetry = thread::Builder::new()
        .name("rk-input-hook-telemetry".to_string())
        .spawn(move || {
            while let Ok(event) = telemetry_rx.recv() {
                // A panicking sink must not end the drain.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    state.sink(event.event, &event.message());
                    let dropped = telemetry_session.take_telemetry_overflow();
                    if dropped > 0 {
                        state.sink("hook_telemetry_dropped", &format!("dropped={dropped}"));
                    }
                    // A dropped `Start` queues a telemetry event, so a stalled dispatch thread
                    // (which cannot report the drop itself) does not keep it from being reported.
                    report_overflow(&telemetry_session, state);
                }));
            }
        });
    if telemetry.is_err() {
        // No drain: stop queueing instead of growing an unread lane.
        session.close_telemetry();
    }

    // Ctrl+Escape cancel: its own thread, so a stalled push-to-talk callback cannot hold it back.
    let cancel_session = Arc::clone(&session);
    let cancel = thread::Builder::new()
        .name("rk-input-hook-cancel".to_string())
        .spawn(move || {
            run_dispatch(
                cancel_rx,
                || cancel_session.is_open(),
                command_kind,
                |command| {
                    // A panicking host callback must not end the thread.
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        if let HookCommand::Cancel = command.command {
                            on_cancel();
                        }
                    }));
                },
                |_closer| {},
            );
        });
    if cancel.is_err() {
        state.end_session();
        return Err(HookError::Install(
            "cancel dispatch thread spawn failed".to_string(),
        ));
    }

    // Push-to-talk events, in the order they happened.
    let dispatch_session = Arc::clone(&session);
    let dispatch = thread::Builder::new()
        .name("rk-input-hook-dispatch".to_string())
        .spawn(move || {
            run_dispatch(
                command_rx,
                || dispatch_session.is_open(),
                command_kind,
                |command| {
                    // A panicking host callback must not end the dispatch thread.
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        if let HookCommand::Ptt(event) = command.command {
                            match (command.release, on_release.as_ref()) {
                                (Some(edge), Some(callback)) => callback(event, edge),
                                _ => on_ptt(event),
                            }
                        }
                    }));
                    report_overflow(&dispatch_session, state);
                },
                |closer| {
                    let _ = catch_unwind(AssertUnwindSafe(|| match closer {
                        Cmd::PttCancel => on_ptt(PttEvent::Cancel(no_key_cause())),
                        Cmd::Preview(down) => on_ptt(PttEvent::Preview(down)),
                        _ => {}
                    }));
                },
            );
        });
    match dispatch {
        Ok(_) => Ok(session),
        Err(_) => {
            state.end_session();
            Err(HookError::Install(
                "dispatch thread spawn failed".to_string(),
            ))
        }
    }
}
