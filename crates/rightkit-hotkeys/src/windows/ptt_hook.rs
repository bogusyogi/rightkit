//! Keyboard half of the process-wide low-level input hooks: the Ctrl+Escape cancel guard and the
//! single-key push-to-talk gesture (arm timer, release, chord abort, send companion), as one state
//! block plus a handler for [`LowLevelHooks`](super::LowLevelHooks).
//!
//! Moved out of an app's recording-cancel hook module. Product-neutral: the host owns the state
//! block (`static STATE: KeyboardState = KeyboardState::new(..)`), wires the handler through
//! [`InputHooks`](super::input_hooks::InputHooks) and maps the events it receives to its own commands.
//!
//! Invariants (do not relax):
//! - The keyboard handler (`KeyboardState::handle_key_in`, registered by `InputHooks`) runs inside the
//!   `WH_KEYBOARD_LL` procedure. It never runs host work, never does disk I/O, formats no text and
//!   calls into no other process. It is handed its install's own `KbSession` and so reads no shared
//!   slot and takes no lock (the arm timer slot is a `try_lock` only the teardown ever holds).
//!   Everything the host must react to is queued on a lock-free, bounded lane and run on a dispatch
//!   thread (see `input_hooks`). There is no other entry: the handler is crate-internal.
//! - Telemetry from the handler is queued as plain data and formatted on the drain thread, never
//!   inline. It carries a key CLASS (`modifier`, `ptt`, `other`), never a key code or name.
//! - The 1,000 ms chord-abort window is [`crate::ptt::should_abort_ptt_chord`]; it is not repeated here.
//! - Everything an install starts (command lanes, telemetry lane, arm timer) belongs to one
//!   session with one generation number (`KbSession::generation`), begun by `InputHooks::install`
//!   and ended by its drop (`crate::lifecycle`). `KeyboardState::live_gen` holds the generation
//!   whose handlers may act (0 once it ended). The hook handler, the arm-timer callback and the
//!   arm-message handler each compare their own generation with it FIRST; after it ended they do
//!   nothing except the owed-key-up rule below.
//! - Gesture changes are made on the hook thread, in key order, with one exception: the host-thread
//!   setters (`set_ptt_enabled(false)`, `set_ptt_key`, `set_recording_active(false)`,
//!   `set_onboarding_test_active`) reset the gesture from the host's thread. The arm timer only POSTS
//!   a message to the hook thread (`WM_APP + 0x41`); the thread then runs
//!   `KeyboardState::on_arm_elapsed` between key events. The timer thread never starts a recording.
//! - Begin and end are paired for the host: a `PttEvent::Start` that was queued is followed by exactly
//!   one of `Stop`, `Send` or `Cancel` (`KeyboardState::ptt_open`, taken with one `swap`, so a hook
//!   thread and a host thread cannot both end it), and the end uses the lane's reserve so it is
//!   queued even when ordinary items are being dropped. The same holds for `Preview(true)` and
//!   `Preview(false)` (`preview_open`). Exceptions: the host reported its own recording ended
//!   (`set_recording_active(false)`: no end is sent, the host already ended it), and an uninstall (the
//!   dispatch thread then delivers one `Cancel` for a start it delivered).
//! - OWED KEY-UPS. While a hook that can see the key-up is installed, a swallowed key-down's repeats
//!   and its key-up are swallowed (`ledger`, `mod_ledger`), including after the install ended
//!   (`handle_ended`). The debt is cleared only by `on_hook_removed` (after the hook thread is gone)
//!   or by `begin_session`. If the hook is removed while a key is held, the key-up that follows
//!   reaches the application without its key-down: nothing can prevent that without a hook, and it
//!   is reported once (`hook_key_up_owed`). Escape (the Ctrl+Escape cancel) is outside the ledger's
//!   key range and has its own record (`KeyboardState::escape`, `HeldKey`) with the same rules: a
//!   swallowed down has its repeats and up swallowed, a delivered down has them delivered, across a
//!   cancel, a session end and an uninstall. HANDOFF.md has the key-pairing table.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use super::{HookDecision, HookError};
use crate::hook_detail::KeyEventDetail;
use crate::lifecycle::{lock, next_generation, HeldKey, KeyHold, Lane, LaneRx, PressLedger};
pub use crate::lifecycle::{press_route, PressRoute};
use crate::ptt::{PttAbortCause, PttAbortKind, PttModifiers, PttPhase};
use crate::ptt_timer::ArmTimer;
use crate::release_edge::{native_release_now, NativeReleaseEdge};

/// Host event sink: `(event, message)`. Called off the hook thread only.
pub type EventSink = fn(&str, &str);

/// Work the hook procedure hands to the dispatch threads. Push-to-talk events share one lane so
/// they are handled in the order they happened; the Ctrl+Escape cancel has a lane of its own, so a
/// host push-to-talk callback that stalls cannot hold a cancel back.
pub enum HookCommand {
    Cancel,
    Ptt(PttEvent),
}

pub(crate) struct QueuedHookCommand {
    pub(crate) command: HookCommand,
    // Outer Some means a physical release; inner None means native clock failure.
    pub(crate) release: Option<Option<NativeReleaseEdge>>,
}

impl From<HookCommand> for QueuedHookCommand {
    fn from(command: HookCommand) -> Self {
        Self {
            command,
            release: None,
        }
    }
}

/// A telemetry event queued from the hook procedure: plain data, formatted into text by
/// `HookTelemetryEvent::message` on the drain thread.
pub struct HookTelemetryEvent {
    pub(crate) event: &'static str,
    pub(crate) body: TelemetryBody,
}

/// What a telemetry event says. Everything the hook procedure queues is `Copy` data or a static
/// string; the only allocation is `Owned`, which only host code outside the hook procedure uses.
pub(crate) enum TelemetryBody {
    Text(&'static str),
    Owned(String),
    Gesture {
        reason: &'static str,
        class: KeyClass,
        is_keydown: bool,
        action: PttAction,
        swallowed: bool,
        snapshot: GestureSnapshot,
    },
    CancelKey {
        class: KeyClass,
        is_keydown: bool,
        armed: bool,
        recording: bool,
    },
    ChordIgnoredLate {
        class: KeyClass,
        elapsed_ms: u64,
    },
    Abort(PttAbortCause, KeyClass),
}

/// What kind of key an event was, for telemetry: never which key. `Ptt` is the configured
/// push-to-talk key, `Modifier` any other Shift, Ctrl, Alt or Win key, `Other` everything else.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyClass {
    Modifier,
    Ptt,
    Other,
}

impl KeyClass {
    fn as_str(self) -> &'static str {
        match self {
            KeyClass::Modifier => "modifier",
            KeyClass::Ptt => "ptt",
            KeyClass::Other => "other",
        }
    }
}

fn is_modifier_vk(vk: u32) -> bool {
    [
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
    ]
    .iter()
    .any(|&modifier| u32::from(modifier) == vk)
}

/// The gesture flags and the physical modifier state at one instant (bools only).
#[derive(Clone, Copy, Debug)]
pub(crate) struct GestureSnapshot {
    ptt_key_down: bool,
    armed: bool,
    recording: bool,
    suppress_until_key_up: bool,
    ctrl_down: bool,
    lctrl: bool,
    rctrl: bool,
    lalt: bool,
    ralt: bool,
}

fn gesture_text(
    reason: &str,
    class: KeyClass,
    is_keydown: bool,
    action: PttAction,
    swallowed: bool,
    s: &GestureSnapshot,
) -> String {
    format!(
        "reason={reason} key_class={} is_keydown={is_keydown} \
         action={action:?} swallowed={swallowed} ptt_key_down={} armed={} recording={} \
         suppress_until_key_up={} ctrl_down={} physical=lctrl:{},rctrl:{},lalt:{},ralt:{}",
        class.as_str(),
        s.ptt_key_down,
        s.armed,
        s.recording,
        s.suppress_until_key_up,
        s.ctrl_down,
        s.lctrl,
        s.rctrl,
        s.lalt,
        s.ralt,
    )
}

impl HookTelemetryEvent {
    /// The message the host's sink receives. Same text as before the lanes carried plain data.
    pub(crate) fn message(&self) -> String {
        match &self.body {
            TelemetryBody::Text(text) => (*text).to_string(),
            TelemetryBody::Owned(text) => text.clone(),
            TelemetryBody::Gesture {
                reason,
                class,
                is_keydown,
                action,
                swallowed,
                snapshot,
            } => gesture_text(reason, *class, *is_keydown, *action, *swallowed, snapshot),
            TelemetryBody::CancelKey {
                class,
                is_keydown,
                armed,
                recording,
            } => format!(
                "key_class={} is_keydown={is_keydown} armed={armed} recording={recording}",
                class.as_str()
            ),
            TelemetryBody::ChordIgnoredLate { class, elapsed_ms } => {
                format!(
                    "platform=windows key_class={} elapsed_ms={elapsed_ms}",
                    class.as_str()
                )
            }
            // The cause carries no key code (`windows_abort_cause`); diagnostic text omits key identities.
            TelemetryBody::Abort(cause, class) => format!(
                "{} key_class={}",
                crate::ptt::format_abort("windows", *cause),
                class.as_str()
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttEvent {
    Start,
    Stop,
    Send,
    Cancel(PttAbortCause),
    Preview(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttAction {
    None,
    Arm,
    Stop,
    Cancel,
    Disarm,
}

/// Depth of the push-to-talk command lane: far more than a host that keeps up ever queues; a host
/// callback stalled past it loses the newest commands (counted, reported). A `Start` or
/// `Preview(true)` that does not fit is dropped and so is nothing else of that gesture: no end is
/// sent for it.
const COMMAND_LANE_DEPTH: usize = 128;
/// Extra depth only ends may use (`Lane::send_end`): at most one open `Start` and one open
/// `Preview(true)` exist at a time, so 2 suffice (`Lane::send_end` explains the argument); 4 leaves
/// slack.
const COMMAND_LANE_END_RESERVE: usize = 4;
/// Depth of the cancel lane (Ctrl+Escape is debounced to one per 250 ms).
const CANCEL_LANE_DEPTH: usize = 16;
/// Depth of the telemetry lane; the newest events are dropped (counted, reported) past it.
const TELEMETRY_LANE_DEPTH: usize = 256;

/// The message the arm timer posts to the hook thread when its delay elapsed; `wParam` is the
/// generation that was armed. Inside the application range the hook thread accepts (`WM_APP..`).
pub(crate) const MSG_PTT_ARM_ELAPSED: u32 = WM_APP + 0x41;

type SessionLanes = (
    Arc<KbSession>,
    LaneRx<QueuedHookCommand>,
    LaneRx<HookTelemetryEvent>,
);

/// The lanes of one install. Shared (as `Arc`) with every thread of that install, so closing them
/// stops all of those threads from sending, whatever clones are still alive.
pub(crate) struct KbSession {
    /// The install generation (never 0); `KeyboardState::live_gen` equals it while the install is
    /// live.
    generation: u64,
    commands: Arc<Lane<QueuedHookCommand>>,
    cancels: Arc<Lane<QueuedHookCommand>>,
    telemetry: Lane<HookTelemetryEvent>,
    /// Receiver of the cancel lane; taken once by the cancel dispatch thread.
    cancel_rx: Mutex<Option<LaneRx<QueuedHookCommand>>>,
    /// The session's one arm-timer thread (`rk-ptt-arm`, [`ArmTimer`]): arming is a channel send,
    /// so the hook thread never creates a thread. `None` once the session closed; dropping the
    /// timer stops its thread without firing.
    arm: Mutex<Option<ArmTimer>>,
}

impl KbSession {
    pub(crate) fn is_open(&self) -> bool {
        self.commands.is_open()
    }

    /// The receiver of the cancel lane, for the cancel dispatch thread (once).
    pub(crate) fn take_cancel_rx(&self) -> Option<LaneRx<QueuedHookCommand>> {
        lock(&self.cancel_rx).take()
    }

    /// Commands dropped (lane full) since the last call.
    pub(crate) fn take_command_overflow(&self) -> u64 {
        self.commands.take_dropped() + self.cancels.take_dropped()
    }

    /// Telemetry events dropped (lane full) since the last call.
    pub(crate) fn take_telemetry_overflow(&self) -> u64 {
        self.telemetry.take_dropped()
    }

    pub(crate) fn close(&self) {
        self.commands.close();
        self.cancels.close();
        self.telemetry.close();
        // Asks the timer thread to stop. Not on the hook thread. A callback already running is not
        // interrupted: it is fenced by the generation check in `KeyboardState::arm_elapsed`.
        self.stop_timer();
    }

    /// Ask the arm timer's thread to stop, without closing the lanes (`KeyboardState::quiesce`
    /// does this before the hooks are removed). Dropping the timer only SENDS a shutdown message: a
    /// deadline that is already due can still call the callback once, and a callback already running
    /// finishes. Neither can act, because the callback checks the generation first.
    pub(crate) fn stop_timer(&self) {
        drop(lock(&self.arm).take());
    }

    /// Arm the timer for `generation`. Never blocks: when the session is closing (the lock is
    /// held by `close`) or already closed, nothing is armed and `false` is returned.
    fn arm_timer(&self, generation: u64, delay: Duration) -> bool {
        match self.arm.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(timer) => {
                    timer.arm(generation, delay);
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Stop queueing telemetry (no drain thread could be started).
    pub(crate) fn close_telemetry(&self) {
        self.telemetry.close();
    }

    /// Queue one telemetry event of this session as plain data: no lock, no wait; a full or closed
    /// lane drops it (counted when full, reported by the drain thread).
    fn push(&self, event: &'static str, body: TelemetryBody) {
        self.telemetry.send(HookTelemetryEvent { event, body });
    }
}

/// The cause attached to a `Cancel` that no key caused: the gesture was abandoned by a host setting
/// (`set_ptt_enabled(false)`, `set_ptt_key`) or the install ended while it was open.
pub(crate) fn no_key_cause() -> PttAbortCause {
    PttAbortCause {
        kind: PttAbortKind::KeyDown,
        key_code: None,
        modifiers: PttModifiers(0),
        joined: PttModifiers(0),
        phase: PttPhase::Recording,
        swallowed: false,
    }
}

/// Longest `arm_delay_ms` accepted: the same bound `ArmTimer::arm` applies
/// (`crate::ptt_timer::MAX_ARM_DELAY`, 1,000 ms).
pub const MAX_ARM_DELAY_MS: u64 = crate::ptt_timer::MAX_ARM_DELAY.as_millis() as u64;
/// Longest `cancel_debounce_ms` accepted.
pub const MAX_CANCEL_DEBOUNCE_MS: u64 = 5_000;

/// Timing the host chooses. Both values are clamped to their ranges when the state is built
/// ([`KeyboardState::new`]): `arm_delay_ms` to `0..=MAX_ARM_DELAY_MS`, `cancel_debounce_ms` to
/// `0..=MAX_CANCEL_DEBOUNCE_MS`.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardTiming {
    /// Hold time before an armed push-to-talk gesture starts recording.
    pub arm_delay_ms: u64,
    /// Minimum gap between two Ctrl+Escape cancels.
    pub cancel_debounce_ms: u64,
}

impl KeyboardTiming {
    /// The same timing with both values inside their ranges.
    pub const fn clamped(self) -> Self {
        Self {
            arm_delay_ms: if self.arm_delay_ms > MAX_ARM_DELAY_MS {
                MAX_ARM_DELAY_MS
            } else {
                self.arm_delay_ms
            },
            cancel_debounce_ms: if self.cancel_debounce_ms > MAX_CANCEL_DEBOUNCE_MS {
                MAX_CANCEL_DEBOUNCE_MS
            } else {
                self.cancel_debounce_ms
            },
        }
    }
}

/// All shared state of the keyboard hook. `const`-constructible so a host can keep it in a `static`
/// and call the setters before or after the hooks are installed.
pub struct KeyboardState {
    recording_active: AtomicBool,
    onboarding_test_active: AtomicBool,
    onboarding_ptt_preview_active: AtomicBool,
    ctrl_down: AtomicBool,
    last_trigger_ms: AtomicU64,
    ptt_enabled: AtomicBool,
    ptt_key_vk: AtomicU32,
    ptt_key_down: AtomicBool,
    /// Virtual key of a push-to-talk press whose key-down this hook swallowed and whose key-up is
    /// still owed (0 = none). Keeps swallow/deliver balanced when the gesture is switched on,
    /// off or re-keyed while the key is held: a delivered down never gets its up swallowed (the
    /// application would see a stuck key) and a swallowed down never gets a stray up delivered.
    /// Cleared only when no hook that can see the key-up is installed (`on_hook_removed`, or
    /// `begin_session` when none ran): see the module notes, "OWED KEY-UPS".
    ledger: PressLedger,
    /// Companion modifiers whose key-down this hook swallowed (the AltGr Ctrl, the send Ctrl or
    /// Alt) and whose key-up is still owed. A modifier whose down was DELIVERED (pressed before the
    /// gesture armed) has its up delivered: a swallowed up for a delivered down would leave the key
    /// stuck down in the application.
    mod_ledger: PressLedger,
    /// How the hook routed the last down of Escape (outside the ledgers' key range): a swallowed
    /// down owes its up, a delivered down has its up delivered. Same clearing rule as the ledgers.
    escape: KeyHold,
    /// Id of the hook thread, recorded by the hook thread when it arms the gesture (0 = unknown):
    /// where the arm timer posts its message. Reset at both ends of a session.
    hook_thread_id: AtomicU32,
    /// The last logged state of the companion Ctrl (autorepeat logs once, not per repeat).
    companion_logged: AtomicBool,
    ptt_armed: AtomicBool,
    ptt_recording: AtomicBool,
    ptt_suppress_until_key_up: AtomicBool,
    ptt_send_pending: AtomicBool,
    ptt_send_key_down: AtomicBool,
    ptt_ralt_send_companion_down: AtomicBool,
    ptt_hold_started_ms: AtomicU64,
    /// The per-arm sequence: bumped by every gesture change. An arm-timer message carries the value
    /// it was armed with and does nothing when it no longer matches.
    ptt_gen: AtomicU64,
    /// Generation of the install whose handlers may act (`KbSession::generation`); 0 when no install
    /// is live or it has ended. Read FIRST by the hook handler, the arm-timer callback and the arm
    /// message handler.
    live_gen: AtomicU64,
    /// Held by the arm-timer callback across its generation check and its one `PostThreadMessageW`
    /// (nothing else, no host code), and taken once by `quiesce` after it zeroed `live_gen`: when
    /// `quiesce` returns, no callback is between its check and its post and none can pass the
    /// check again.
    post_gate: Mutex<()>,
    /// A `PttEvent::Start` was queued for the host and its end (`Stop`, `Send` or `Cancel`) has not
    /// been queued. Whoever `swap`s it to false queues the end.
    ptt_open: AtomicBool,
    /// `PttEvent::Preview(true)` was queued and `Preview(false)` has not been.
    preview_open: AtomicBool,
    /// The live install's lanes; `None` while no install is live. Replaced by each install.
    session: Mutex<Option<Arc<KbSession>>>,
    event_sink: EventSink,
    timing: KeyboardTiming,
}

impl KeyboardState {
    pub const fn new(event_sink: EventSink, timing: KeyboardTiming) -> Self {
        Self {
            recording_active: AtomicBool::new(false),
            onboarding_test_active: AtomicBool::new(false),
            onboarding_ptt_preview_active: AtomicBool::new(false),
            ctrl_down: AtomicBool::new(false),
            last_trigger_ms: AtomicU64::new(0),
            ptt_enabled: AtomicBool::new(false),
            ptt_key_vk: AtomicU32::new(VK_RMENU as u32),
            ptt_key_down: AtomicBool::new(false),
            ledger: PressLedger::new(),
            mod_ledger: PressLedger::new(),
            escape: KeyHold::new(),
            hook_thread_id: AtomicU32::new(0),
            companion_logged: AtomicBool::new(false),
            ptt_armed: AtomicBool::new(false),
            ptt_recording: AtomicBool::new(false),
            ptt_suppress_until_key_up: AtomicBool::new(false),
            ptt_send_pending: AtomicBool::new(false),
            ptt_send_key_down: AtomicBool::new(false),
            ptt_ralt_send_companion_down: AtomicBool::new(false),
            ptt_hold_started_ms: AtomicU64::new(0),
            ptt_gen: AtomicU64::new(0),
            live_gen: AtomicU64::new(0),
            post_gate: Mutex::new(()),
            ptt_open: AtomicBool::new(false),
            preview_open: AtomicBool::new(false),
            session: Mutex::new(None),
            event_sink,
            timing: timing.clamped(),
        }
    }

    /// Begin one install's session: fresh lanes, a fresh arm timer, fresh gesture state. Refuses
    /// while a session is live (one install per state object). The receivers belong to the
    /// dispatch and telemetry threads the caller starts (the cancel lane's receiver is taken with
    /// [`KbSession::take_cancel_rx`]). The session is built outside the lock and published once.
    /// The hook thread never reads the slot: its closure holds its own session. Only host-thread
    /// calls and the timer thread read it, and a slot read on the hook thread is a `try_lock`.
    pub(crate) fn begin_session(&'static self) -> Result<SessionLanes, HookError> {
        if lock(&self.session).as_ref().is_some_and(|s| s.is_open()) {
            return Err(HookError::AlreadyInstalled);
        }
        let generation = next_generation();
        let (commands, command_rx) =
            Lane::with_depth_and_reserve(COMMAND_LANE_DEPTH, COMMAND_LANE_END_RESERVE);
        let commands = Arc::new(commands);
        let (cancels, cancel_rx) = Lane::with_depth(CANCEL_LANE_DEPTH);
        let (telemetry, telemetry_rx) = Lane::with_depth(TELEMETRY_LANE_DEPTH);
        let state: &'static KeyboardState = self;
        // The callback carries the install generation: after the install ended it does nothing.
        let timer = ArmTimer::spawn("rk-ptt-arm", move |arm_seq| {
            state.arm_elapsed(generation, arm_seq)
        })
        .map_err(|_| HookError::Install("push-to-talk arm timer spawn failed".to_string()))?;
        let session = Arc::new(KbSession {
            generation,
            commands,
            cancels: Arc::new(cancels),
            telemetry,
            cancel_rx: Mutex::new(Some(cancel_rx)),
            arm: Mutex::new(Some(timer)),
        });
        {
            let mut slot = lock(&self.session);
            if slot.as_ref().is_some_and(|s| s.is_open()) {
                // Lost a race with another install: end ours (stops its timer), touch nothing.
                drop(slot);
                session.close();
                return Err(HookError::AlreadyInstalled);
            }
            *slot = Some(Arc::clone(&session));
        }
        // No hook of this state can be running yet (`InputHooks::install` refuses while a previous
        // install's hooks are not yet removed), so the debts of an earlier install are gone too.
        self.reset_session_state();
        self.clear_debts();
        // Published last: from here the hook handler for `generation` may act.
        self.live_gen.store(generation, Ordering::SeqCst);
        Ok((session, command_rx, telemetry_rx))
    }

    /// The arm delay elapsed (runs on the session's timer thread). `generation` is the install the
    /// timer was made for, `arm_seq` the per-arm sequence (`ptt_gen`) it was armed with.
    ///
    /// The gesture must not be changed from this thread: the key-up the hook thread is deciding at
    /// this very moment would be judged against a recording that has not started yet. So the timer
    /// only POSTS a message to the hook thread, which runs `KeyboardState::on_arm_elapsed` between
    /// key events. It never starts the recording itself, and without a known hook thread it posts
    /// nothing.
    ///
    /// Fencing: the check that the install is still the live one and that no gesture change has
    /// superseded the arm, and the post that follows it, are one step under `post_gate`, which
    /// `quiesce` takes after it zeroed `live_gen`. `ArmTimer` cannot cancel a callback that is
    /// already running or whose deadline is already due, so this is what makes a late callback
    /// harmless: after `quiesce` returned, no callback is between its check and its post, and every
    /// later one fails the check. Telemetry is queued after the gate is released.
    fn arm_elapsed(&self, generation: u64, arm_seq: u64) {
        let outcome = {
            let _gate = lock(&self.post_gate);
            if self.live_gen.load(Ordering::SeqCst) != generation
                || self.ptt_gen.load(Ordering::SeqCst) != arm_seq
            {
                return;
            }
            let hook_thread = self.hook_thread_id.load(Ordering::SeqCst);
            if hook_thread == 0 {
                "arm_no_hook_thread"
            } else {
                let posted = unsafe {
                    PostThreadMessageW(hook_thread, MSG_PTT_ARM_ELAPSED, arm_seq as usize, 0)
                } != 0;
                if posted {
                    return;
                }
                "arm_post_failed"
            }
        };
        // Nothing starts: the gesture stays armed until the key is released, which disarms it.
        self.push_telemetry("ptt_state", TelemetryBody::Text(outcome));
    }

    /// Hook-thread handler of [`MSG_PTT_ARM_ELAPSED`] (`wparam` is the per-arm sequence that was
    /// armed; `generation` the install the handler was registered for).
    pub(crate) fn on_arm_elapsed(&self, session: &KbSession, wparam: usize) {
        if self.live_gen.load(Ordering::SeqCst) != session.generation {
            return;
        }
        self.fire_arm(session, wparam as u64);
    }

    /// Start recording only if the session is still the live one, the gesture is still the one
    /// that was armed, and the key is still held. Runs on the hook thread only. If the lane cannot
    /// take the `Start`, nothing started and no end will be sent for it (the dispatch thread
    /// reports the drop as `hook_command_dropped`).
    ///
    /// The host-thread setters can reset the gesture at any moment. The `Start` is queued BEFORE
    /// `ptt_open` is set, so no end can ever be queued ahead of it; and the generation is checked
    /// again after `ptt_open` is set: a setter that ran in between saw `ptt_open` still false and
    /// queued nothing, so this function ends the start itself (`ptt_end` takes `ptt_open` with one
    /// `swap`, so exactly one of the two ends it).
    fn fire_arm(&self, session: &KbSession, arm_seq: u64) {
        let commands = &session.commands;
        if !commands.is_open()
            || self.ptt_gen.load(Ordering::SeqCst) != arm_seq
            || !self.ptt_armed.load(Ordering::SeqCst)
            || !self.ptt_key_down.load(Ordering::SeqCst)
        {
            return;
        }
        self.ptt_recording.store(true, Ordering::SeqCst);
        if !commands.is_open() || self.ptt_gen.load(Ordering::SeqCst) != arm_seq {
            self.ptt_recording.store(false, Ordering::SeqCst);
            return;
        }
        if !commands.send(HookCommand::Ptt(PttEvent::Start).into()) {
            // The lane is full (or closing). Nothing was queued, so there is nothing to end.
            if self.ptt_gen.load(Ordering::SeqCst) == arm_seq {
                self.ptt_recording.store(false, Ordering::SeqCst);
            }
            self.push_telemetry("ptt_state", TelemetryBody::Text("start_dropped"));
            return;
        }
        self.ptt_open.store(true, Ordering::SeqCst);
        if self.ptt_gen.load(Ordering::SeqCst) != arm_seq {
            self.ptt_end(PttEvent::Cancel(no_key_cause()), Some(session));
        }
    }

    /// Queue the end of the open `Start`, if one is open: whoever takes `ptt_open` queues the end,
    /// so a hook thread (key-up) and a host thread (setter) never both end the same start. The end
    /// uses the lane's reserve. `hook` is the session a hook-thread caller was handed: it is used
    /// as it is, with no slot read, so the end can never be lost to a lock a host thread happens to
    /// hold. A host-thread caller passes `None` and reads the session slot (it may wait for the lock;
    /// when no session is left the dispatch thread's teardown closes the start instead).
    fn ptt_end(&self, event: PttEvent, hook: Option<&KbSession>) {
        self.ptt_end_with_release(event, hook, None);
    }

    fn ptt_end_with_release(
        &self,
        event: PttEvent,
        hook: Option<&KbSession>,
        release: Option<Option<NativeReleaseEdge>>,
    ) {
        if !self.ptt_open.swap(false, Ordering::SeqCst) {
            return;
        }
        match hook {
            Some(session) => {
                session.commands.send_end(QueuedHookCommand {
                    command: HookCommand::Ptt(event),
                    release,
                });
            }
            None => {
                let session = lock(&self.session).clone();
                if let Some(session) = session {
                    session.commands.send_end(QueuedHookCommand {
                        command: HookCommand::Ptt(event),
                        release,
                    });
                }
            }
        }
    }

    /// The same pairing for `Preview(false)` after `Preview(true)`.
    fn preview_end(&self, hook: Option<&KbSession>) {
        if !self.preview_open.swap(false, Ordering::SeqCst) {
            return;
        }
        let end = HookCommand::Ptt(PttEvent::Preview(false));
        match hook {
            Some(session) => {
                session.commands.send_end(end.into());
            }
            None => {
                let session = lock(&self.session).clone();
                if let Some(session) = session {
                    session.commands.send_end(end.into());
                }
            }
        }
    }

    /// End the install's authority (uninstall step 0, before the hooks are removed): the handlers
    /// stop acting at once (`live_gen` = 0: the hook handler passes everything except an owed
    /// key-up, the timer callback and the arm message do nothing), the arm timer is asked to stop,
    /// and `post_gate` is taken once, so a timer callback that was between its generation check and
    /// its post has finished and none can pass the check afterwards: nothing is posted after this
    /// returns. The wait is for one `PostThreadMessageW` (documented as not waiting for the target
    /// thread) and a preempted timer thread; no host code runs under the gate.
    ///
    /// An event already inside `handle_key_live` when this runs finishes under the old rules.
    pub(crate) fn quiesce(&self) {
        self.live_gen.store(0, Ordering::SeqCst);
        self.hook_thread_id.store(0, Ordering::SeqCst);
        let session = lock(&self.session).clone();
        if let Some(session) = session {
            session.stop_timer();
        }
        drop(lock(&self.post_gate));
    }

    /// End the live session: both lanes close (so the dispatch and telemetry threads drain and
    /// exit and no timer of this session can send again) and every per-gesture field is reset, so
    /// a later install starts clean and an old arm timer fails its generation check. The owed
    /// key-up debts are NOT cleared here: the hook may still be installed (a removal that timed
    /// out); `on_hook_removed` clears them. Idempotent.
    pub(crate) fn end_session(&self) {
        self.live_gen.store(0, Ordering::SeqCst);
        let session = lock(&self.session).take();
        if let Some(session) = session {
            session.close();
        }
        self.reset_session_state();
    }

    /// No hook of this state is installed any more (its thread has exited). The key-up of a key
    /// whose key-down was swallowed can no longer be seen, so the debts are dropped and their count
    /// returned: the application then receives that key-up without its key-down (for Alt, the menu
    /// bar can open). Changes state only and calls no host code: the caller reports the count
    /// (`input_hooks::Removal::drop`, off the drop path). Called by `InputHooks` after the hook
    /// thread ended, from a non-hook thread.
    pub(crate) fn on_hook_removed(&self) -> u32 {
        self.clear_debts()
    }

    /// Drop the owed-key-up debts (the two ledgers and Escape); how many keys were owed.
    fn clear_debts(&self) -> u32 {
        let owed = self.ledger.count() + self.mod_ledger.count() + self.escape.owed();
        self.ledger.clear();
        self.mod_ledger.clear();
        self.escape.clear();
        owed
    }

    /// Per-gesture state, reset at both ends of a session. Settings (enabled, key, recording and
    /// onboarding flags) are the host's and survive; the owed-key-up debts are not touched.
    fn reset_session_state(&self) {
        self.ptt_open.store(false, Ordering::SeqCst);
        self.preview_open.store(false, Ordering::SeqCst);
        self.hook_thread_id.store(0, Ordering::SeqCst);
        self.companion_logged.store(false, Ordering::SeqCst);
        self.ptt_key_down.store(false, Ordering::SeqCst);
        self.reset_ptt_gesture_to_idle();
        self.ptt_send_key_down.store(false, Ordering::SeqCst);
        self.ptt_ralt_send_companion_down
            .store(false, Ordering::SeqCst);
        self.ptt_suppress_until_key_up
            .store(false, Ordering::SeqCst);
        self.ctrl_down.store(false, Ordering::SeqCst);
        // Any arm timer of the previous session now fails its generation check.
        self.ptt_gen.fetch_add(1, Ordering::SeqCst);
    }

    /// The live session, for host-thread and timer-thread callers: `try_lock`, never a wait. The lock
    /// is held only to swap or clone an `Arc`, so a miss means a session is being ended (nothing to
    /// send to). The hook thread does not use it.
    fn current_session(&self) -> Option<Arc<KbSession>> {
        self.session.try_lock().ok().and_then(|slot| slot.clone())
    }

    pub(crate) fn sink(&self, event: &str, message: &str) {
        (self.event_sink)(event, message);
    }

    /// Queue a telemetry event as plain data. Never blocks, never formats, never allocates for the
    /// kinds the hook procedure uses; silently drops the event when no session is live (or the
    /// lane is full: counted and reported by the drain thread).
    fn push_telemetry(&self, event: &'static str, body: TelemetryBody) {
        if let Some(session) = self.current_session() {
            session.telemetry.send(HookTelemetryEvent { event, body });
        }
    }

    /// Queue a telemetry event whose text the CALLER built. Kept for hosts; the text was built on
    /// the calling thread, so do not call it from a hook procedure. The hook's own events use
    /// `Self::push_telemetry` and are formatted on the drain thread.
    pub fn enqueue_hook_telemetry(&self, event: &'static str, message: String) {
        self.push_telemetry(event, TelemetryBody::Owned(message));
    }

    /// Queue `command` on its lane; `true` when it was queued (`false`: no live session, the lane
    /// is closed or full; a full lane counts the drop and the dispatch thread reports it).
    fn command_send(&self, command: HookCommand) -> bool {
        match self.current_session() {
            Some(session) => {
                let lane = match command {
                    HookCommand::Cancel => &session.cancels,
                    HookCommand::Ptt(_) => &session.commands,
                };
                lane.send(command.into())
            }
            None => false,
        }
    }

    /// Queue one push-to-talk event as it is (no begin/end pairing; the hook's own `Start` and its
    /// ends go through `fire_arm` and `ptt_end`).
    pub fn ptt_send(&self, event: PttEvent) {
        self.command_send(HookCommand::Ptt(event));
    }

    pub fn is_ptt_vk(&self, vk: u32) -> bool {
        vk == self.ptt_key_vk.load(Ordering::Relaxed)
    }

    pub fn is_ralt_companion_ctrl(&self, vk: u32) -> bool {
        self.ptt_key_vk.load(Ordering::Relaxed) == VK_RMENU as u32
            && self.ptt_key_down.load(Ordering::Relaxed)
            && (vk == VK_CONTROL as u32 || vk == VK_LCONTROL as u32)
    }

    pub fn is_ptt_send_vk(&self, vk: u32) -> bool {
        let ptt_vk = self.ptt_key_vk.load(Ordering::Relaxed);
        ((ptt_vk == VK_LMENU as u32 || ptt_vk == VK_RMENU as u32) && vk == VK_RCONTROL as u32)
            || (ptt_vk == VK_RCONTROL as u32 && vk == VK_RMENU as u32)
    }

    pub fn is_ralt_send_companion_ctrl(&self, vk: u32) -> bool {
        (vk == VK_CONTROL as u32 || vk == VK_LCONTROL as u32)
            && (self.ptt_ralt_send_companion_down.load(Ordering::Relaxed)
                || (self.ptt_key_vk.load(Ordering::Relaxed) == VK_RCONTROL as u32
                    && self.ptt_key_down.load(Ordering::Relaxed)))
    }

    pub fn ptt_hold_elapsed_ms(&self) -> u64 {
        let started = self.ptt_hold_started_ms.load(Ordering::Relaxed);
        if started == 0 {
            return 0;
        }
        current_time_ms().saturating_sub(started)
    }

    pub fn debounce_ok(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let last = self.last_trigger_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) <= self.timing.cancel_debounce_ms {
            return false;
        }
        self.last_trigger_ms.store(now, Ordering::Relaxed);
        true
    }

    /// Pure PTT transition. `ptt_key_down` is the state AFTER this event is applied
    /// (the LL hook fires before the OS updates async key state, and PTT key events
    /// are suppressed, so the hook owns this tracked state).
    /// `disqualified` means any other modifier is currently held (Shift, the
    /// Alt/LCtrl side that isn't the PTT key: keeps PTT from firing during
    /// in-app shortcuts that happen to brush the chosen key). The caller owns the
    /// arm timer, generation counter, channel.
    #[allow(clippy::too_many_arguments)]
    pub fn ptt_transition(
        &self,
        vk: u32,
        is_keydown: bool,
        ptt_key_down: bool,
        disqualified: bool,
        armed: bool,
        recording: bool,
        abort_allowed: bool,
    ) -> PttAction {
        if is_keydown {
            if self.is_ptt_vk(vk) {
                // PTT-key-down alone, nothing disqualifying, idle -> arm.
                if ptt_key_down && !disqualified && !armed && !recording {
                    return PttAction::Arm;
                }
                return PttAction::None;
            }
            // Any other keydown mid-gesture is a chord -> abort.
            if recording {
                return if abort_allowed {
                    PttAction::Cancel
                } else {
                    PttAction::None
                };
            }
            if armed {
                return if abort_allowed {
                    PttAction::Disarm
                } else {
                    PttAction::None
                };
            }
            return PttAction::None;
        }
        // PTT-key-up ends the gesture.
        if self.is_ptt_vk(vk) {
            if recording {
                return PttAction::Stop;
            }
            if armed {
                return PttAction::Disarm;
            }
        }
        PttAction::None
    }

    pub fn ptt_disqualified(&self) -> bool {
        let ptt_vk = self.ptt_key_vk.load(Ordering::Relaxed) as i32;
        unsafe {
            // Shift always disqualifies (PTT key + Shift is never intentional).
            if (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0 {
                return true;
            }
            // Win keys never disqualify: orthogonal on Windows, no need to block
            // PTT-key+Win presses (those have no native Windows meaning either).
            // Alt/LCtrl disqualify UNLESS that side IS the chosen PTT key. The
            // atomic read of the PTT key resolves which side to skip.
            let lalt = (GetAsyncKeyState(VK_LMENU as i32) as u16 & 0x8000) != 0;
            let ralt = (GetAsyncKeyState(VK_RMENU as i32) as u16 & 0x8000) != 0;
            let lctrl = (GetAsyncKeyState(VK_LCONTROL as i32) as u16 & 0x8000) != 0;
            // RAlt often arrives as AltGr (implicit LCtrl+RAlt) on Windows layouts.
            // Do not let that synthetic LCtrl disqualify the actual RAlt PTT key.
            // For RCtrl, LCtrl remains disqualifying.
            if lctrl && ptt_vk != VK_RMENU as i32 {
                return true;
            }
            if ptt_vk == VK_RMENU as i32 {
                // RAlt is the PTT key. LAlt + RAlt is AltGr on EU layouts, but
                // holding RAlt alone is the goal, so LAlt held means the user is
                // mid-chord. Disqualify.
                lalt
            } else if ptt_vk == VK_LMENU as i32 {
                // LAlt is the PTT key; RAlt remains a chord modifier. RCtrl is
                // handled separately as the send companion.
                ralt
            } else {
                // RCtrl is the PTT key. Either Alt held disqualifies.
                lalt || ralt
            }
        }
    }

    /// Common subset reset by every "clear the PTT gesture" call site below; each still sets its
    /// own extra field(s) and bumps `ptt_gen` itself, since those differ (or are conditional).
    fn reset_ptt_gesture_to_idle(&self) {
        self.ptt_armed.store(false, Ordering::SeqCst);
        self.ptt_recording.store(false, Ordering::SeqCst);
        self.ptt_send_pending.store(false, Ordering::SeqCst);
        self.ptt_hold_started_ms.store(0, Ordering::SeqCst);
    }

    /// Turning push-to-talk off while the host was told to start a recording abandons that gesture
    /// from this (the host's) thread: the host gets one `PttEvent::Cancel` with no key as its cause
    /// (`no_key_cause`), queued after the gesture is reset. The swallowed key's own key-up is still
    /// swallowed (`ledger`).
    pub fn set_ptt_enabled(&self, enabled: bool) {
        self.ptt_enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.reset_ptt_gesture_to_idle();
            self.ptt_send_key_down.store(false, Ordering::SeqCst);
            self.ptt_ralt_send_companion_down
                .store(false, Ordering::SeqCst);
            self.ptt_suppress_until_key_up
                .store(false, Ordering::SeqCst);
            self.ptt_gen.fetch_add(1, Ordering::SeqCst);
            self.ptt_end(PttEvent::Cancel(no_key_cause()), None);
        }
    }

    /// Resolve the user's chosen PTT key name to a virtual-key code and store it in the atomic
    /// the hook reads. `"lalt"`, `"ralt"`, and `"rctrl"` are accepted; the caller is expected to
    /// have validated the name already.
    ///
    /// Second-line boundary check at this call site: `parse_ptt_key_name` guards any caller that
    /// bypasses the host's settings validator, so a free-form string cannot reach the Windows VK
    /// lookup. On `Err`, fall back to the documented default (`ralt`).
    pub fn set_ptt_key(&self, name: &str) {
        let vk = match crate::parse_ptt_key_name(name) {
            Ok(parsed) => match parsed.modifier {
                crate::Modifier::Alt if parsed.side == crate::Side::Left => VK_LMENU as u32,
                crate::Modifier::Alt => VK_RMENU as u32,
                crate::Modifier::Ctrl => VK_RCONTROL as u32,
                // parse_ptt_key_name already restricts to Alt | Ctrl on the supported sides, so
                // this branch is unreachable; treat it as the documented default to keep the
                // hook in a sane state if the validator ever grows new variants.
                _ => VK_RMENU as u32,
            },
            Err(_) => {
                // Defensive: a caller bypassed the settings allowlist. Fall back to the
                // documented default rather than entering a no-PTT state.
                // The rejected text is host input and may be a key name: not echoed.
                self.sink("push_to_talk_key_rejected", "unknown_name_fallback_ralt");
                VK_RMENU as u32
            }
        };
        self.ptt_key_vk.store(vk, Ordering::SeqCst);
        // Force a fresh state read on the next event: switching keys mid-gesture could
        // otherwise leave the static-key "down" stuck true.
        self.ptt_key_down.store(false, Ordering::Relaxed);
        self.reset_ptt_gesture_to_idle();
        self.ptt_send_key_down.store(false, Ordering::SeqCst);
        self.ptt_ralt_send_companion_down
            .store(false, Ordering::SeqCst);
        self.ptt_suppress_until_key_up
            .store(false, Ordering::SeqCst);
        self.ptt_gen.fetch_add(1, Ordering::SeqCst);
        // The host's recording is still running and this hook can no longer end it from the key.
        self.ptt_end(PttEvent::Cancel(no_key_cause()), None);
    }

    /// The host's statement of its own state. `false` means the host's recording is over (manual
    /// stop, cancel, error, or any state change that is not "recording"): the gesture is cleared so
    /// the later release does not try to stop a finished capture, and NO end event is sent, because
    /// the host ended the recording itself and a `Cancel` would be a second, unwanted end.
    ///
    /// Not covered, and by this design: if the host reports "not recording" while the `Start` it was
    /// given is still being processed (it has not reached its own recording state yet), the release
    /// does not stop that recording. The report is counted once as `ptt_state` /
    /// `host_reported_inactive_while_open`.
    pub fn set_recording_active(&self, active: bool) {
        self.recording_active.store(active, Ordering::SeqCst);
        if !active {
            self.last_trigger_ms.store(0, Ordering::SeqCst);
            self.ctrl_down.store(false, Ordering::SeqCst);
            // Keep PTT in sync: a recording can end by any path (manual stop, Ctrl+Escape
            // cancel, error). Clear the gesture so a later release doesn't try
            // to stop an already-finished capture.
            if self.ptt_key_down.load(Ordering::SeqCst) {
                self.ptt_suppress_until_key_up.store(true, Ordering::SeqCst);
            }
            self.reset_ptt_gesture_to_idle();
            self.ptt_gen.fetch_add(1, Ordering::SeqCst);
            if self.ptt_open.swap(false, Ordering::SeqCst) {
                self.push_telemetry(
                    "ptt_state",
                    TelemetryBody::Text("host_reported_inactive_while_open"),
                );
            }
        }
    }

    /// Entering or leaving the onboarding chord test. A `Preview(true)` the host was given is
    /// ended with `Preview(false)` when the preview stops being active (queued from this thread). The
    /// push-to-talk gesture is cleared as for `set_recording_active(false)` (no end event: the host
    /// started the test itself).
    pub fn set_onboarding_test_active(&self, active: bool, ptt_preview: bool) {
        self.onboarding_test_active.store(active, Ordering::SeqCst);
        self.onboarding_ptt_preview_active
            .store(active && ptt_preview, Ordering::SeqCst);
        if !(active && ptt_preview) {
            self.preview_end(None);
        }
        if !active || ptt_preview {
            self.ptt_key_down.store(false, Ordering::SeqCst);
            self.reset_ptt_gesture_to_idle();
            self.ptt_gen.fetch_add(1, Ordering::SeqCst);
            if self.ptt_open.swap(false, Ordering::SeqCst) {
                self.push_telemetry(
                    "ptt_state",
                    TelemetryBody::Text("host_reported_inactive_while_open"),
                );
            }
        }
        if !active && !self.recording_active.load(Ordering::SeqCst) {
            self.last_trigger_ms.store(0, Ordering::SeqCst);
            self.ctrl_down.store(false, Ordering::SeqCst);
        }
    }

    /// The gesture flags and the physical Alt and Ctrl state at this instant, as plain data.
    fn gesture_snapshot(&self) -> GestureSnapshot {
        let down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
        GestureSnapshot {
            ptt_key_down: self.ptt_key_down.load(Ordering::Relaxed),
            armed: self.ptt_armed.load(Ordering::Relaxed),
            recording: self.ptt_recording.load(Ordering::Relaxed),
            suppress_until_key_up: self.ptt_suppress_until_key_up.load(Ordering::Relaxed),
            ctrl_down: self.ctrl_down.load(Ordering::Relaxed),
            lctrl: down(VK_LCONTROL),
            rctrl: down(VK_RCONTROL),
            lalt: down(VK_LMENU),
            ralt: down(VK_RMENU),
        }
    }

    /// The hook's own `ptt_hook_event` line: queued as plain data and formatted on the drain
    /// thread (nothing is formatted or allocated on the hook thread).
    fn log_gesture(
        &self,
        session: &KbSession,
        reason: &'static str,
        vk: u32,
        is_keydown: bool,
        action: PttAction,
        swallowed: bool,
    ) {
        session.push(
            "ptt_hook_event",
            TelemetryBody::Gesture {
                reason,
                class: self.key_class(vk),
                is_keydown,
                action,
                swallowed,
                snapshot: self.gesture_snapshot(),
            },
        );
    }

    /// The same line, built on the CALLING thread. Kept for hosts; do not call it from a hook
    /// procedure (the hook uses `log_gesture`).
    pub fn log_ptt_hook_event(
        &self,
        reason: &str,
        vk: u32,
        is_keydown: bool,
        action: PttAction,
        swallowed: bool,
    ) {
        self.enqueue_hook_telemetry(
            "ptt_hook_event",
            gesture_text(
                reason,
                self.key_class(vk),
                is_keydown,
                action,
                swallowed,
                &self.gesture_snapshot(),
            ),
        );
    }

    /// The class of `vk` for telemetry (never the key itself).
    fn key_class(&self, vk: u32) -> KeyClass {
        if self.is_ptt_vk(vk) {
            KeyClass::Ptt
        } else if is_modifier_vk(vk) {
            KeyClass::Modifier
        } else {
            KeyClass::Other
        }
    }

    /// Route one Escape event by its own record (`escape`); `true` = swallow. `wants_swallow` is the
    /// cancel guard's decision for a NEW down (Ctrl held while recording or testing). A down while
    /// a down is recorded repeats it, and is routed like it, unless the key does not read down (the
    /// hook fires before the system state changes, so a repeat reads down): then the record is
    /// stale (an up that was never seen) and the down is a new one. An up settles the record: it is
    /// swallowed only for a swallowed down; an up with no record belongs to a down this hook never
    /// saw and is delivered.
    fn route_escape(&self, is_keydown: bool, wants_swallow: bool) -> bool {
        if !is_keydown {
            return self.escape.settle() == HeldKey::Swallowed;
        }
        let recorded = self.escape.get();
        if recorded != HeldKey::None
            && unsafe { GetAsyncKeyState(VK_ESCAPE as i32) as u16 & 0x8000 } != 0
        {
            return recorded == HeldKey::Swallowed;
        }
        self.escape.set(if wants_swallow {
            HeldKey::Swallowed
        } else {
            HeldKey::Delivered
        });
        wants_swallow
    }

    /// Swallow a companion modifier's key-down (and remember the debt) or route its key-up with
    /// its down: swallowed when its down was, delivered when its down was.
    fn route_modifier(&self, vk: u32, is_keydown: bool) -> HookDecision {
        if is_keydown {
            self.mod_ledger.set(vk);
            HookDecision::Swallow
        } else if self.mod_ledger.settle(vk) {
            HookDecision::Swallow
        } else {
            HookDecision::Pass
        }
    }

    /// A key-down of `vk` that repeats a down this hook DELIVERED (the key was already held when
    /// the gesture armed, push-to-talk was enabled or the hook was installed): it must be delivered
    /// too, or its up could not be. The hook fires before the system key state changes, so a key
    /// that reads down on a key-down event is a repeat; one of its own swallowed downs (owed in
    /// either ledger) is not.
    fn is_delivered_repeat(&self, vk: u32) -> bool {
        !self.mod_ledger.owes(vk)
            && !self.ledger.owes(vk)
            && unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
    }

    /// The handler `InputHooks` registers (the only entry). `session` is the install's own session,
    /// held by the hook closure for the life of the hook thread, so the hook thread reads no shared
    /// slot and no lock:
    /// every lane, the arm timer and the telemetry lane are reached through it. It acts only while
    /// that session's generation is the live one (`live_gen`), checked FIRST. After the install ended
    /// (`quiesce`, `end_session`) the hook can still be installed for a while (a removal that timed
    /// out); every event then passes untouched, nothing is recorded and nothing is posted, except
    /// the owed key-ups ([`Self::handle_ended`]).
    pub(crate) fn handle_key_in(
        &'static self,
        session: &KbSession,
        event: &KeyEventDetail,
    ) -> HookDecision {
        if self.live_gen.load(Ordering::SeqCst) != session.generation {
            return self.handle_ended(event);
        }
        self.handle_key_live(session, event)
    }

    /// An event reaching the handler of an install that has ended. Passes it, with one exception: a
    /// key whose key-down this hook swallowed while the install was live still owes its key-up, and
    /// its repeats and its key-up are swallowed until the up is seen (or the hook is removed:
    /// `on_hook_removed`). Only the two ledgers and the Escape record are read and settled; no gesture state is touched,
    /// no lane is written, nothing is posted. Injected events pass.
    fn handle_ended(&self, event: &KeyEventDetail) -> HookDecision {
        if event.injected {
            return HookDecision::Pass;
        }
        let vk = event.vk;
        if vk == VK_ESCAPE as u32 {
            // Only the record is read or settled: a swallowed down keeps its repeats and its up
            // swallowed, a delivered one has them delivered, a new down is delivered.
            return if event.down {
                if self.escape.get() == HeldKey::Swallowed {
                    HookDecision::Swallow
                } else {
                    HookDecision::Pass
                }
            } else if self.escape.settle() == HeldKey::Swallowed {
                HookDecision::Swallow
            } else {
                HookDecision::Pass
            };
        }
        if event.down {
            if self.ledger.owes(vk) || self.mod_ledger.owes(vk) {
                return HookDecision::Swallow;
            }
            return HookDecision::Pass;
        }
        // Both ledgers are settled (no short-circuit): the key may be owed in either.
        let from_keys = self.ledger.settle(vk);
        let from_modifiers = self.mod_ledger.settle(vk);
        if from_keys || from_modifiers {
            HookDecision::Swallow
        } else {
            HookDecision::Pass
        }
    }

    /// The pairing rule for every owed-up record, applied once to the outcome of EVERY key-down on
    /// every path (so no path can forget it): the debt of a key is decided by its most recent down.
    /// A DELIVERED down clears the owed-up record of that key in both ledgers before the decision is
    /// returned (a stale record left by an up this hook never saw, or by a swallowed down whose repeat
    /// is now delivered, must not swallow the up of the delivered down: the application would see the
    /// key stuck down). A SWALLOWED down leaves the record set (every swallow site sets it; if one
    /// ever did not, it is set here), so its up is swallowed. Escape has its own record
    /// (`route_escape` sets it per down, `Delivered` overwriting `Swallowed`) and is not handled here.
    /// Injected events carry no debt.
    fn handle_key_live(&'static self, session: &KbSession, event: &KeyEventDetail) -> HookDecision {
        // An up of a key that owes a modifier up is swallowed whatever path decides it, and the
        // debt is closed: one swallowed up ends the key's records and, when the key has meanwhile
        // become the push-to-talk key and a repeat started a gesture, that gesture too (the
        // decision path falls through to the gesture's own release handling in that case).
        let modifier_up_owed = !event.down && !event.injected && self.mod_ledger.owes(event.vk);
        let decision = self.decide_key_live(session, event);
        if modifier_up_owed {
            self.mod_ledger.settle(event.vk);
            self.ledger.settle(event.vk);
            return HookDecision::Swallow;
        }
        if event.down && !event.injected && event.vk != VK_ESCAPE as u32 {
            match decision {
                HookDecision::Pass => {
                    self.ledger.settle(event.vk);
                    self.mod_ledger.settle(event.vk);
                }
                _ => {
                    if !self.ledger.owes(event.vk) && !self.mod_ledger.owes(event.vk) {
                        self.ledger.set(event.vk);
                    }
                }
            }
        }
        decision
    }

    fn decide_key_live(&'static self, session: &KbSession, event: &KeyEventDetail) -> HookDecision {
        // Skip injected events (SendInput from the host's command dispatch). They are
        // post-decision wire events, not user input: letting them pass keeps modifier-state
        // recovery (and any future auto-typing) from being second-guessed by this state machine.
        if event.injected {
            return HookDecision::Pass;
        }
        let vk = event.vk;
        let is_keydown = event.down;
        let recording_active = self.recording_active.load(Ordering::Relaxed);
        let chord_test_active = self.onboarding_test_active.load(Ordering::Relaxed);
        let hook_active = recording_active || chord_test_active;
        let (next_ctrl_down, transition_should_cancel, transition_should_swallow) =
            ctrl_cancel_transition(
                self.ctrl_down.load(Ordering::Relaxed),
                vk,
                is_keydown,
                hook_active,
            );
        let ctrl_down = resolve_ctrl_down(vk, next_ctrl_down, ctrl_physically_down());
        self.ctrl_down.store(ctrl_down, Ordering::Relaxed);
        let escape_event = vk == VK_ESCAPE as u32;
        let should_cancel =
            transition_should_cancel || (hook_active && escape_event && is_keydown && ctrl_down);
        let wants_swallow = transition_should_swallow || (hook_active && escape_event && ctrl_down);
        // Escape is paired by its own record, not by the state of the hook now: a swallowed down has
        // its repeats and its up swallowed, a delivered down has them delivered, whatever the
        // recording or Ctrl state is when they arrive (cancel, session end, Ctrl released first).
        let should_swallow = if escape_event {
            self.route_escape(is_keydown, wants_swallow)
        } else {
            wants_swallow
        };
        if should_cancel && should_swallow && self.debounce_ok() {
            session.cancels.send(HookCommand::Cancel.into());
            session.push(
                "recording_cancel_hook",
                TelemetryBody::Text("ctrl_escape_swallowed"),
            );
            return HookDecision::Swallow;
        }
        if should_swallow {
            return HookDecision::Swallow;
        }

        // ---- Push-to-talk: hold the user-selected key (LAlt, RAlt, or RCtrl) ----
        // Suppress the configured PTT key itself. It is an app gesture, not user input for
        // Windows. This keeps RAlt from leaking into the shell, so commands like alt+tab can
        // commit while the user is still physically holding the PTT key. Other keys still pass
        // through and cancel/disarm the gesture.
        let ptt_enabled = self.ptt_enabled.load(Ordering::Relaxed);
        let ptt_preview_active = self.onboarding_ptt_preview_active.load(Ordering::Relaxed);
        let input_active = ptt_input_active(ptt_enabled, ptt_preview_active);
        // Balance swallowed and delivered presses of the push-to-talk key (see `press_route`). The
        // ledger holds every key whose down was swallowed, so a second key (the user switched the
        // push-to-talk key mid-hold) never hides the first one's debt.
        let swallowed_vk = if self.ledger.owes(vk) { vk } else { 0 };
        match press_route(
            vk,
            is_keydown,
            swallowed_vk,
            self.ptt_key_vk.load(Ordering::Relaxed),
            input_active,
        ) {
            PressRoute::SwallowOwedUp => {
                self.ledger.settle(vk);
                // One up settles every record of the key (it may also be owed as a modifier).
                self.mod_ledger.settle(vk);
                // The tracked "down" state belongs to the CURRENT push-to-talk key only.
                if self.is_ptt_vk(vk) {
                    self.ptt_key_down.store(false, Ordering::Relaxed);
                }
                return HookDecision::Swallow;
            }
            PressRoute::PassDeliveredUp => {
                self.ptt_key_down.store(false, Ordering::Relaxed);
                // `press_route` reads the push-to-talk ledger only. The key may still owe an up
                // as a MODIFIER: its down was swallowed as a send or companion modifier and the
                // push-to-talk key was then rebound to it while it was held. That up is swallowed
                // and the debt settled here, with the same release bookkeeping as below, instead
                // of reaching the application as an orphan up.
                if !is_keydown && self.mod_ledger.settle(vk) {
                    if vk == VK_CONTROL as u32 || vk == VK_LCONTROL as u32 {
                        self.companion_logged.store(false, Ordering::Relaxed);
                        self.ptt_ralt_send_companion_down
                            .store(false, Ordering::SeqCst);
                    } else if vk == VK_RCONTROL as u32 || vk == VK_RMENU as u32 {
                        self.ptt_send_key_down.store(false, Ordering::SeqCst);
                    }
                    return HookDecision::Swallow;
                }
                return HookDecision::Pass;
            }
            PressRoute::Normal => {
                // Up of the owed press settles the debt. A fresh down of that key that this hook
                // will now deliver (gesture off or key no longer the push-to-talk key) clears a
                // stale debt left by a lost up, so the new press's up is not swallowed.
                if swallowed_vk != 0
                    && swallowed_vk == vk
                    && (!is_keydown || !(input_active && self.is_ptt_vk(vk)))
                {
                    self.ledger.settle(vk);
                }
            }
        }
        // The up of a companion modifier whose down this hook swallowed is swallowed too, whatever
        // the gesture is doing now (it may have ended, or been switched off, while the key was
        // held). Its tracked state goes with it.
        if !is_keydown && self.mod_ledger.settle(vk) {
            if vk == VK_CONTROL as u32 || vk == VK_LCONTROL as u32 {
                self.companion_logged.store(false, Ordering::Relaxed);
                self.ptt_ralt_send_companion_down
                    .store(false, Ordering::SeqCst);
            } else if vk == VK_RCONTROL as u32 || vk == VK_RMENU as u32 {
                self.ptt_send_key_down.store(false, Ordering::SeqCst);
            }
            // Rebind while held, then a repeat before the release: the key is now the
            // push-to-talk key and its repeat may have armed or started a gesture. Its up must
            // also run the gesture's release (Stop) below; `handle_key_live` swallows this up
            // whatever that path returns. In every other case the up is done here.
            if !(input_active && self.is_ptt_vk(vk)) {
                return HookDecision::Swallow;
            }
        }
        if input_active {
            let ptt_key_event = self.is_ptt_vk(vk);
            if ptt_key_event && is_keydown {
                // A down of the key that is a repeat of one this hook DELIVERED (the key was already
                // held when push-to-talk was enabled, the key was switched to it, or the hook was
                // installed) is delivered too, so its up can be delivered: it is no gesture. Tested
                // before the ledger is touched.
                if self.is_delivered_repeat(vk) {
                    return HookDecision::Pass;
                }
                // Every path below swallows the push-to-talk key-down.
                self.ledger.set(vk);
            }
            if ptt_key_event && self.ptt_suppress_until_key_up.load(Ordering::Relaxed) {
                self.ptt_key_down.store(is_keydown, Ordering::Relaxed);
                if !is_keydown {
                    self.ptt_suppress_until_key_up
                        .store(false, Ordering::SeqCst);
                    self.ptt_key_down.store(false, Ordering::Relaxed);
                }
                if !is_keydown {
                    self.log_gesture(
                        session,
                        "suppress_until_key_up",
                        vk,
                        is_keydown,
                        PttAction::None,
                        true,
                    );
                }
                return HookDecision::Swallow;
            }
            if ptt_key_event && ptt_preview_active {
                let was_down = self.ptt_key_down.swap(is_keydown, Ordering::SeqCst);
                match ptt_preview_event(true, true, is_keydown, was_down) {
                    Some(true) => {
                        // Paired: `Preview(false)` is queued only for a `Preview(true)` that was.
                        if session
                            .commands
                            .send(HookCommand::Ptt(PttEvent::Preview(true)).into())
                        {
                            self.preview_open.store(true, Ordering::SeqCst);
                            // A host thread that ended the preview in between saw it not open.
                            if !self.onboarding_ptt_preview_active.load(Ordering::SeqCst) {
                                self.preview_end(Some(session));
                            }
                        } else {
                            session.push("ptt_state", TelemetryBody::Text("preview_dropped"));
                        }
                    }
                    Some(false) => self.preview_end(Some(session)),
                    None => {}
                }
                return HookDecision::Swallow;
            }
            if !ptt_enabled {
                return HookDecision::Pass;
            }
            let ptt_key_down = if self.is_ptt_vk(vk) {
                is_keydown
            } else {
                self.ptt_key_down.load(Ordering::Relaxed)
            };
            self.ptt_key_down.store(ptt_key_down, Ordering::Relaxed);
            if self.is_ralt_companion_ctrl(vk) {
                // Windows often represents Right Alt as AltGr: an implicit
                // LCtrl plus RAlt pair. That companion Ctrl can arrive after
                // the PTT arm timer has already started recording; treat it as
                // part of the PTT key, not as a second-key chord cancel.
                // Swallowed only when it arrives after the PTT key (a Ctrl pressed before it was
                // delivered, and its up must be delivered too: owed ups were settled above).
                if !is_keydown {
                    self.companion_logged.store(false, Ordering::Relaxed);
                    return HookDecision::Pass;
                }
                if self.is_delivered_repeat(vk) {
                    return HookDecision::Pass;
                }
                // Autorepeat logs once per state change, not once per repeat.
                if self.companion_logged.swap(is_keydown, Ordering::Relaxed) != is_keydown {
                    self.log_gesture(
                        session,
                        "ralt_companion_ctrl",
                        vk,
                        is_keydown,
                        PttAction::None,
                        true,
                    );
                }
                return self.route_modifier(vk, true);
            }
            let armed = self.ptt_armed.load(Ordering::Relaxed);
            let recording = self.ptt_recording.load(Ordering::Relaxed);
            if self.is_ralt_send_companion_ctrl(vk) {
                if !is_keydown {
                    // The companion's down was delivered (the owed ones were settled above).
                    self.ptt_ralt_send_companion_down
                        .store(false, Ordering::SeqCst);
                    return HookDecision::Pass;
                }
                if self.is_delivered_repeat(vk) {
                    return HookDecision::Pass;
                }
                self.ptt_ralt_send_companion_down
                    .store(true, Ordering::SeqCst);
                self.log_gesture(
                    session,
                    "ralt_send_companion_ctrl",
                    vk,
                    is_keydown,
                    PttAction::None,
                    true,
                );
                return self.route_modifier(vk, true);
            }
            if (armed
                || recording
                || self.ptt_send_pending.load(Ordering::Relaxed)
                || self.ptt_send_key_down.load(Ordering::Relaxed))
                && self.is_ptt_send_vk(vk)
            {
                if !is_keydown {
                    // The send key was already down when the gesture armed (its down was
                    // delivered): its up is delivered, and it is not a send.
                    self.ptt_send_key_down.store(false, Ordering::SeqCst);
                    return HookDecision::Pass;
                }
                if self.is_delivered_repeat(vk) {
                    return HookDecision::Pass;
                }
                self.ptt_send_key_down.store(true, Ordering::SeqCst);
                self.ptt_send_pending.store(true, Ordering::SeqCst);
                self.log_gesture(
                    session,
                    "send_modifier",
                    vk,
                    is_keydown,
                    PttAction::None,
                    true,
                );
                return self.route_modifier(vk, true);
            }
            let elapsed_ms = self.ptt_hold_elapsed_ms();
            let abort_allowed = crate::ptt::should_abort_ptt_chord(elapsed_ms);
            let action = self.ptt_transition(
                vk,
                is_keydown,
                ptt_key_down,
                self.ptt_disqualified(),
                armed,
                recording,
                abort_allowed,
            );
            if action == PttAction::Cancel {
                session.push(
                    "ptt_cancel_key",
                    TelemetryBody::CancelKey {
                        class: self.key_class(vk),
                        is_keydown,
                        armed,
                        recording,
                    },
                );
            }
            if action == PttAction::None
                && is_keydown
                && (armed || recording)
                && !ptt_key_event
                && !abort_allowed
            {
                session.push(
                    "ptt_chord_ignored_late",
                    TelemetryBody::ChordIgnoredLate {
                        class: self.key_class(vk),
                        elapsed_ms,
                    },
                );
            }
            if !matches!(action, PttAction::None) {
                self.log_gesture(
                    session,
                    "ptt_transition",
                    vk,
                    is_keydown,
                    action,
                    ptt_key_event,
                );
            }
            match action {
                PttAction::Arm => {
                    session.push("ptt_state", TelemetryBody::Text("armed"));
                    self.ptt_hold_started_ms
                        .store(current_time_ms(), Ordering::SeqCst);
                    self.ptt_send_pending.store(false, Ordering::SeqCst);
                    self.ptt_send_key_down.store(false, Ordering::SeqCst);
                    self.ptt_ralt_send_companion_down
                        .store(false, Ordering::SeqCst);
                    self.ptt_armed.store(true, Ordering::SeqCst);
                    let generation = self.ptt_gen.fetch_add(1, Ordering::SeqCst) + 1;
                    // Where the arm timer posts its message (this thread).
                    self.hook_thread_id
                        .store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
                    // The session's timer thread posts it back; nothing is created on the hook
                    // thread. A closing session arms nothing.
                    if !session
                        .arm_timer(generation, Duration::from_millis(self.timing.arm_delay_ms))
                    {
                        session.push("ptt_state", TelemetryBody::Text("arm_timer_unavailable"));
                    }
                }
                PttAction::Stop => {
                    // This branch follows a non-injected physical key-up. Sample before
                    // queueing or publishing any state that can end the gesture elsewhere.
                    let release = Some(native_release_now());
                    self.ptt_gen.fetch_add(1, Ordering::SeqCst);
                    self.ptt_armed.store(false, Ordering::SeqCst);
                    self.ptt_recording.store(false, Ordering::SeqCst);
                    self.ptt_hold_started_ms.store(0, Ordering::SeqCst);
                    // The end of the open start; nothing when a host-thread setter already ended it.
                    if self.ptt_send_pending.swap(false, Ordering::SeqCst) {
                        self.ptt_end_with_release(PttEvent::Send, Some(session), release);
                    } else {
                        self.ptt_end_with_release(PttEvent::Stop, Some(session), release);
                    }
                }
                PttAction::Cancel => {
                    let cause = windows_abort_cause(vk, armed, recording, ptt_key_event);
                    self.ptt_gen.fetch_add(1, Ordering::SeqCst);
                    self.ptt_armed.store(false, Ordering::SeqCst);
                    self.ptt_recording.store(false, Ordering::SeqCst);
                    self.ptt_send_pending.store(false, Ordering::SeqCst);
                    self.ptt_hold_started_ms.store(0, Ordering::SeqCst);
                    self.ptt_end(PttEvent::Cancel(cause), Some(session));
                }
                PttAction::Disarm => {
                    if !ptt_key_event {
                        let cause = windows_abort_cause(vk, armed, recording, false);
                        session.push("ptt_abort", TelemetryBody::Abort(cause, self.key_class(vk)));
                    }
                    self.ptt_gen.fetch_add(1, Ordering::SeqCst);
                    self.ptt_armed.store(false, Ordering::SeqCst);
                    self.ptt_send_pending.store(false, Ordering::SeqCst);
                    self.ptt_hold_started_ms.store(0, Ordering::SeqCst);
                }
                PttAction::None => {}
            }
            if ptt_key_event {
                return HookDecision::Swallow;
            }
        }
        HookDecision::Pass
    }
}

pub fn windows_ptt_modifiers() -> PttModifiers {
    let down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
    let mut value = 0;
    if down(VK_CONTROL) || down(VK_LCONTROL) || down(VK_RCONTROL) {
        value |= PttModifiers::CTRL;
    }
    if down(VK_SHIFT) || down(VK_LSHIFT) || down(VK_RSHIFT) {
        value |= PttModifiers::SHIFT;
    }
    if down(VK_LMENU) || down(VK_RMENU) {
        value |= PttModifiers::ALT;
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        value |= PttModifiers::COMMAND_OR_WIN;
    }
    PttModifiers(value)
}

pub fn windows_abort_cause(
    vk: u32,
    armed: bool,
    recording: bool,
    swallowed: bool,
) -> PttAbortCause {
    let modifiers = windows_ptt_modifiers();
    let joined_flag = match vk as u16 {
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => PttModifiers::CTRL,
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => PttModifiers::SHIFT,
        VK_LMENU | VK_RMENU => PttModifiers::ALT,
        VK_LWIN | VK_RWIN => PttModifiers::COMMAND_OR_WIN,
        _ => 0,
    };
    PttAbortCause {
        kind: if joined_flag == 0 {
            PttAbortKind::KeyDown
        } else {
            PttAbortKind::ModifierJoined
        },
        // No key code: the cause reaches host callbacks and sink text, and a key is not telemetry.
        // The class of the key is `kind` (a modifier joined or not) and `joined`.
        key_code: None,
        modifiers,
        joined: PttModifiers(joined_flag),
        phase: if recording {
            PttPhase::Recording
        } else if armed {
            PttPhase::Arming
        } else {
            PttPhase::Starting
        },
        swallowed,
    }
}

pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub fn hook_modifier_snapshot() -> String {
    unsafe {
        let lctrl = (GetAsyncKeyState(VK_LCONTROL as i32) as u16 & 0x8000) != 0;
        let rctrl = (GetAsyncKeyState(VK_RCONTROL as i32) as u16 & 0x8000) != 0;
        let lalt = (GetAsyncKeyState(VK_LMENU as i32) as u16 & 0x8000) != 0;
        let ralt = (GetAsyncKeyState(VK_RMENU as i32) as u16 & 0x8000) != 0;
        format!("lctrl:{lctrl},rctrl:{rctrl},lalt:{lalt},ralt:{ralt}")
    }
}

pub fn ctrl_physically_down() -> bool {
    unsafe {
        (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0
            || (GetAsyncKeyState(VK_LCONTROL as i32) as u16 & 0x8000) != 0
            || (GetAsyncKeyState(VK_RCONTROL as i32) as u16 & 0x8000) != 0
    }
}

/// Resolve whether Ctrl is held, for the cancel guard.
///
/// PLAIN ESCAPE MUST NEVER STOP OR CANCEL A CAPTURE. It is far too common a key (every
/// dismiss/close in every app). Cancel is Ctrl+Escape: the modifier is the whole point.
///
/// The bug this exists to prevent: the LL hook fires BEFORE the OS updates async key state. So on
/// the Ctrl key's OWN key-UP, `GetAsyncKeyState` still reports Ctrl held. Re-asserting that stale
/// `true` on the very event that should clear it left `ctrl_down` stuck, and from then on a BARE
/// Escape satisfied the cancel guard and killed the recording.
///
/// For the Ctrl key's own event the hook is authoritative, so trust the transition. Poll async
/// state only for OTHER keys: that is what still catches Ctrl already being held before the hook
/// attached.
pub fn resolve_ctrl_down(vk_code: u32, next_ctrl_down: bool, ctrl_async_down: bool) -> bool {
    let is_ctrl_key_event = matches!(
        vk_code,
        x if x == VK_CONTROL as u32 || x == VK_LCONTROL as u32 || x == VK_RCONTROL as u32
    );
    if is_ctrl_key_event {
        next_ctrl_down
    } else {
        next_ctrl_down || ctrl_async_down
    }
}

pub fn ptt_preview_event(
    preview_active: bool,
    ptt_key_event: bool,
    is_keydown: bool,
    was_down: bool,
) -> Option<bool> {
    (preview_active && ptt_key_event && is_keydown != was_down).then_some(is_keydown)
}

pub fn ptt_input_active(ptt_enabled: bool, preview_active: bool) -> bool {
    ptt_enabled || preview_active
}

/// Returns `(next_ctrl_down, should_cancel, should_swallow)`. A low-level keyboard hook delivers
/// only key-down and key-up (plain and SYS), so `is_keydown` carries all four messages.
pub fn ctrl_cancel_transition(
    ctrl_down: bool,
    vk_code: u32,
    is_keydown: bool,
    recording_active: bool,
) -> (bool, bool, bool) {
    let next_ctrl_down = if matches!(
        vk_code,
        x if x == VK_CONTROL as u32 || x == VK_LCONTROL as u32 || x == VK_RCONTROL as u32
    ) {
        is_keydown
    } else {
        ctrl_down
    };
    let should_cancel =
        recording_active && is_keydown && vk_code == VK_ESCAPE as u32 && next_ctrl_down;
    let should_swallow = recording_active && vk_code == VK_ESCAPE as u32 && next_ctrl_down;
    (next_ctrl_down, should_cancel, should_swallow)
}
