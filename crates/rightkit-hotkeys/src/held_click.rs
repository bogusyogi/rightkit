//! Pure decision logic for a mouse click the low-level hook holds back while it decides what is
//! under the pointer. Platform-neutral; the Windows hook in
//! `windows::mouse_hook` supplies the events and implements [`ClickEnv`] (the effects).
//!
//! Modelled here: every held click that reaches [`settle`] ends in exactly ONE of three outcomes,
//! and a click is never delivered to anything but the window under the press point when the worker
//! identified it (what happens when the worker itself panics or blocks is in
//! `windows::mouse_hook`):
//!
//! 1. DELIVERED: replayed to the application at the original press point. Position and click are
//!    ONE `SendInput` batch (an absolute move to the press point, the button events, an absolute
//!    move back), so the pointer cannot drift between the position and the click. The window under
//!    the point is verified immediately before that batch.
//! 2. CONSUMED: the host toggle was handed to its own dispatch lane (it ran or will run once).
//! 3. DROPPED, REPORTED: neither of the above, with a distinct [`DropReason`] (the host gets a
//!    `middle_mouse_win_dropped` event). A drop happens only when delivering would be wrong or
//!    impossible: the target window changed, cannot be identified or was identified too late to be
//!    attributed to the press, the press point is not on the virtual desktop, the host toggle is
//!    busy with an earlier press, or the OS refused (or half-inserted) the input.
//!
//! Not modelled here, by design: a press that the hook thread cannot hold (queue full, worker gone,
//! lock busy, a held click still in flight, the identity facility saturated) is not held at all and
//! passes, so nothing is eaten.
//!
//! | path                                                   | outcome                          |
//! | ------------------------------------------------------ | -------------------------------- |
//! | target window changed / cannot be verified (any path)  | dropped (`target_*`)             |
//! | hyperlink confirmed                                    | replay (delivered)               |
//! | confirmed not a link, toggle present, lane idle        | toggle (consumed)                |
//! | confirmed not a link, toggle present, lane busy        | dropped (`toggle_busy`)          |
//! | confirmed not a link, toggle present, lane stalled     | replay (host toggle unavailable) |
//! | confirmed not a link, no toggle callback               | replay (delivered)               |
//! | lookup unknown (error, timeout, UIA unavailable, died) | replay (delivered)               |
//! | press point not on the virtual desktop                 | dropped (`position_unmappable`)  |
//! | OS rejects the injected click (UIPI, secure desktop)   | dropped (`send_rejected`)        |
//! | OS inserts only the down; the up cannot follow         | dropped (`button_stuck`)         |
//! | OS inserts the down; the up follows in a second call   | delivered                        |
//! | button-up before resolution                            | up swallowed with its down       |
//! | pointer moved before resolution                        | replay at the ORIGINAL point, pointer then put back |
//! | hook uninstall / worker shutdown / worker panic        | replay without resolving (still target-verified) |
//! | identity not attributable to the press (picked up late)| dropped (`pickup_too_late`)      |

use crate::hook_detail::MouseKind;

/// What the hook callback does with one mouse event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateAction {
    /// Let the event through untouched.
    Pass,
    /// Swallow a middle-button press and hand it to the resolver worker.
    Hold,
    /// Swallow the button-up that belongs to a held press (the replay supplies its own up).
    SwallowUp,
}

/// One step of the hook-thread gate.
///
/// * `ours`: the event is the hook's own tagged replay (injected flag AND the host's tag).
/// * `enabled`: the host enabled the middle-click feature.
/// * `up_owed`: a held press has not yet had its button-up swallowed.
/// * `try_hold`: queue the press for the worker; returns `false` when it cannot be queued
///   (queue full, worker gone, lock busy, target window not identifiable). It is called only
///   when a hold is wanted.
///
/// Returns the action and the next `up_owed`. A press that cannot be queued passes and leaves
/// `up_owed` as it was, so its real up passes too.
pub fn gate_step(
    kind: MouseKind,
    ours: bool,
    enabled: bool,
    up_owed: bool,
    try_hold: impl FnOnce() -> bool,
) -> (GateAction, bool) {
    if ours {
        return (GateAction::Pass, up_owed);
    }
    match kind {
        MouseKind::MiddleDown if enabled => {
            if try_hold() {
                (GateAction::Hold, true)
            } else {
                (GateAction::Pass, up_owed)
            }
        }
        // A press delivered while disabled: its up must be delivered too, and any stale debt
        // from a lost up is cleared.
        MouseKind::MiddleDown => (GateAction::Pass, false),
        MouseKind::MiddleUp if up_owed => (GateAction::SwallowUp, false),
        _ => (GateAction::Pass, up_owed),
    }
}

// ---------------------------------------------------------------------------------------------
// Lookup: what is under the pointer (tri-state)
// ---------------------------------------------------------------------------------------------

/// Raw answer of the UI Automation probe. `Unknown` is any failure to answer (hit test failed,
/// UI Automation unavailable); it is never folded into `NotLink`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Link,
    NotLink,
    Unknown,
}

/// Why a lookup could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownCause {
    /// UI Automation could not hit-test the point.
    LookupFailed,
    /// No answer within the deadline.
    TimedOut,
    /// The resolver could not run (thread refused, slots exhausted).
    ResolverUnavailable,
    /// The resolver died (panic, channel closed).
    ResolverDied,
    /// No lookup was attempted (uninstall, worker shutdown or panic drain).
    Skipped,
    /// A hand cursor was seen but the owning process is unknown, so the fallback cannot judge.
    TargetUnknown,
}

// ---------------------------------------------------------------------------------------------
// Press point on the virtual desktop
// ---------------------------------------------------------------------------------------------

/// The virtual desktop in physical pixels (`SM_*VIRTUALSCREEN`); the origin can be negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// The longest axis (in pixels) [`absolute_coord`] maps; a longer one is `None` (the press is then
/// dropped as `position_unmappable` instead of landing a pixel off). See that function's proof.
pub const MAX_ABSOLUTE_EXTENT: i32 = 32_768;

/// The `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` coordinate (0..=65535) of pixel `p` on an
/// axis that starts at `origin` and is `extent` pixels long; `None` when the pixel is not on the
/// axis or the axis is longer than [`MAX_ABSOLUTE_EXTENT`].
///
/// What the documentation fixes. Microsoft's `MOUSEINPUT` / `mouse_event` documentation says that
/// with `MOUSEEVENTF_ABSOLUTE` the coordinates are normalised to 0 to 65,535, that (0,0) is the
/// upper-left corner and (65535,65535) the lower-right corner of the display surface, and that
/// `MOUSEEVENTF_VIRTUALDESK` maps them to the entire desktop. It states NO conversion formula and no
/// rounding rule, so which pixel a value `v` selects is not documented. This function therefore does
/// not rely on one formula; it picks `v` so that every conversion in common use selects `p`:
///
/// ```text
/// v = ceil(p * 65535 / (extent - 1))          (extent >= 2; v = 0 for extent = 1)
/// A:  p == floor(v * extent / 65536)           truncating, scale 65536
/// B:  p == floor(v * (extent-1) / 65535 + 1/2) rounding, scale (extent-1)/65535
/// C:  p == floor(v * (extent-1) / 65535)       truncating, scale (extent-1)/65535
/// ```
///
/// Arithmetic. Let `N = extent`, `S = N - 1`, `K = 65535 / S`. Then `p*K <= v < p*K + 1`.
/// C: `v*S/65535 = v/K` is in `[p, p + 1/K)` and `1/K = S/65535 < 1`, so the floor is `p`.
/// B: `v/K + 1/2` is in `[p + 1/2, p + 1/K + 1/2)`, whose floor is `p` while `1/K <= 1/2`, that is
/// `S <= 32767`, that is `N <= 32768`.
/// A: needs `65536p/N <= v < 65536(p+1)/N`. Lower: `v >= 65535p/S >= 65536p/N` because
/// `65535 N >= 65536 S` for `N <= 65536`. Upper: `v < 65535p/S + 1`, and `g(p) = 65536(p+1)/N -
/// 65535p/S - 1` is linear in `p` with `g(0) = 65536/N - 1 >= 0` and `g(S) = 65536 - 65535 - 1 = 0`,
/// so `g >= 0` on `0..=S`.
/// The last pixel gets `v = 65535`, the documented lower-right corner; pixel 0 gets 0, the upper-left.
/// Also checked exhaustively: for every `N` in `1..=32768` and every `p` in `0..N`, A, B and C all
/// return `p` (an arithmetic check of the three formulas, not a test of the operating system;
/// HANDOFF.md, "Absolute coordinates", says how it was run). The previous value,
/// `((4d+1) * 65536) / (4N)`, satisfied only A: for `N = 1920` it misses 463 pixels under B and
/// 1,452 under C (same check).
///
/// Not proven: which conversion Windows actually uses (none of A, B, C is documented), nor that the
/// process's DPI awareness makes the caller's pixels and `GetSystemMetrics` agree (HANDOFF.md,
/// "Known residuals"). Installed journey J6 compares `GetCursorPos` after a replay with the press
/// point.
pub fn absolute_coord(p: i32, origin: i32, extent: i32) -> Option<i32> {
    if extent <= 0 || extent > MAX_ABSOLUTE_EXTENT {
        return None;
    }
    let d = i64::from(p) - i64::from(origin);
    if d < 0 || d >= i64::from(extent) {
        return None;
    }
    if extent == 1 {
        return Some(0);
    }
    let span = i64::from(extent) - 1;
    Some(((d * 65_535 + span - 1) / span) as i32)
}

/// What the resolver learned about the held click, after the hand-cursor fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Link,
    /// Confirmed: UI Automation answered and found no hyperlink, and the fallback did not
    /// contradict it. The only resolution that may run the host toggle.
    NotLink,
    Unknown(UnknownCause),
}

/// Combine the UI Automation answer with the hand-cursor fallback.
///
/// The fallback's role: UI Automation can under-report in another process (Chromium keeps its
/// tree dormant until an assistive client attaches and then answers "document", which is a
/// confident-looking `NotLink`). A hand cursor over ANOTHER process is therefore positive
/// evidence of a link and overrides `NotLink` and `Unknown`. In the host's own process
/// (`own_process == Some(true)`) pointer-styled controls are not links, so the fallback is
/// ignored there. If a hand cursor was seen but the owner is unknown (`None`), a `NotLink` cannot
/// be confirmed and becomes `Unknown`. The fallback never turns anything into `NotLink`.
pub fn classify(
    lookup: Result<Lookup, UnknownCause>,
    hand_cursor: bool,
    own_process: Option<bool>,
) -> Resolution {
    let hand_link = hand_cursor && own_process == Some(false);
    match lookup {
        _ if hand_link => Resolution::Link,
        Ok(Lookup::Link) => Resolution::Link,
        Ok(Lookup::NotLink) if hand_cursor && own_process.is_none() => {
            Resolution::Unknown(UnknownCause::TargetUnknown)
        }
        Ok(Lookup::NotLink) => Resolution::NotLink,
        Ok(Lookup::Unknown) => Resolution::Unknown(UnknownCause::LookupFailed),
        Err(cause) => Resolution::Unknown(cause),
    }
}

// ---------------------------------------------------------------------------------------------
// Target identity
// ---------------------------------------------------------------------------------------------

/// Which window a press landed on: the window under the point, its root, and the owning process
/// (pid plus creation time, so a recycled pid is not mistaken for the same process).
/// `started` is `None` until the process creation time has been read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetIdentity {
    pub hwnd: isize,
    pub root: isize,
    pub pid: u32,
    pub started: Option<u64>,
}

/// Result of comparing the window under the press point now with the one captured at press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetCheck {
    Same,
    /// A different window, root or process is under the point (or the pid was recycled).
    Changed,
    /// Either identity is missing or incomplete (no window at the point, process not openable).
    /// Treated like `Changed`: nothing is injected.
    Unverifiable,
}

pub fn compare_target(pressed: Option<TargetIdentity>, now: Option<TargetIdentity>) -> TargetCheck {
    let (Some(a), Some(b)) = (pressed, now) else {
        return TargetCheck::Unverifiable;
    };
    if a.hwnd != b.hwnd || a.root != b.root || a.pid != b.pid {
        return TargetCheck::Changed;
    }
    match (a.started, b.started) {
        (Some(x), Some(y)) if x == y => TargetCheck::Same,
        (Some(_), Some(_)) => TargetCheck::Changed,
        _ => TargetCheck::Unverifiable,
    }
}

// ---------------------------------------------------------------------------------------------
// Host toggle lane
// ---------------------------------------------------------------------------------------------

/// State of the dedicated lane the host toggle callback runs on (never the queue worker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// No callback is running: a toggle may be handed off.
    Idle,
    /// A callback is running (for less than the stall limit): a new toggle is dropped, reported.
    Busy,
    /// A callback has run past the stall limit, or the lane is gone: the host toggle is treated
    /// as unavailable, like a missing callback, so non-link clicks are replayed again.
    Stalled,
}

pub fn lane_state(running_for_ms: Option<u64>, stall_after_ms: u64) -> LaneState {
    match running_for_ms {
        None => LaneState::Idle,
        Some(ms) if ms >= stall_after_ms => LaneState::Stalled,
        Some(_) => LaneState::Busy,
    }
}

// ---------------------------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------------------------

/// Why a held click was neither delivered nor consumed by the toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    TargetChanged,
    TargetUnverifiable,
    /// The window under the point was identified too long after the press to be attributed to it.
    PickupTooLate,
    /// The press point is not on the virtual desktop (or its metrics are unavailable).
    PositionUnmappable,
    ToggleBusy,
    SendRejected,
    /// The OS inserted the button-down and the matching up could not be inserted.
    ButtonStuck,
    /// The code that settles the click panicked; nothing is known to have been sent.
    SettleFailed,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::TargetChanged => "target_changed",
            DropReason::TargetUnverifiable => "target_unverifiable",
            DropReason::PickupTooLate => "pickup_too_late",
            DropReason::PositionUnmappable => "position_unmappable",
            DropReason::ToggleBusy => "toggle_busy",
            DropReason::SendRejected => "send_rejected",
            DropReason::ButtonStuck => "button_stuck",
            DropReason::SettleFailed => "settle_failed",
        }
    }
}

/// What happens to the held click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Replay the click to the application (still subject to the position and send gates).
    Replay,
    /// Hand the host toggle to its lane (deliberately consumed).
    Toggle,
    /// Neither: dropped and reported.
    Drop(DropReason),
}

/// Final fate of one held click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickOutcome {
    /// The application received exactly one click at the press point.
    Delivered {
        /// The pointer was put back where the user had it (plus their motion meanwhile).
        pointer_restored: bool,
        /// The click was replayed because the host toggle lane is stalled or unavailable.
        host_toggle_unavailable: bool,
    },
    /// The host toggle was handed off.
    Toggled,
    /// Neither delivered nor consumed; reported.
    Dropped(DropReason),
}

/// Decide the fate of one held click before any input is sent.
///
/// Order matters: (1) the target gate comes first and applies to every path, including shutdown
/// and the toggle (a `NotLink` proven for a window that is no longer under the press point is not
/// a confirmed `NotLink` for the press); (2) shutdown replays; (3) only a confirmed `NotLink`
/// with a toggle callback and an idle lane toggles; `Unknown` replays, because the user's click
/// is then delivered as pressed instead of being eaten on a guess.
pub fn delivery_for(
    resolution: Resolution,
    shutting_down: bool,
    has_toggle: bool,
    target: TargetCheck,
    lane: LaneState,
) -> Delivery {
    match target {
        TargetCheck::Changed => return Delivery::Drop(DropReason::TargetChanged),
        TargetCheck::Unverifiable => return Delivery::Drop(DropReason::TargetUnverifiable),
        TargetCheck::Same => {}
    }
    if shutting_down {
        return Delivery::Replay;
    }
    match resolution {
        Resolution::Link | Resolution::Unknown(_) => Delivery::Replay,
        Resolution::NotLink if !has_toggle => Delivery::Replay,
        Resolution::NotLink => match lane {
            LaneState::Idle => Delivery::Toggle,
            LaneState::Busy => Delivery::Drop(DropReason::ToggleBusy),
            LaneState::Stalled => Delivery::Replay,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Partial insertion
// ---------------------------------------------------------------------------------------------

/// What must still be done after `SendInput` inserted only the first `inserted` events of the click
/// batch. The batch is `[absolute move to the press point, button down, button up, (absolute move
/// back)]`; the OS inserts a prefix and reports its length (UIPI, a secure desktop, a concurrent
/// block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The down and the up were both inserted: the application received the click.
    pub click_complete: bool,
    /// The down was inserted and the up was not: a matching up must follow or the button stays
    /// logically down.
    pub release_needed: bool,
    /// The pointer was moved to the press point and the move back was not inserted.
    pub restore_needed: bool,
}

/// Pure map from the inserted prefix to the recovery. `has_restore` is whether the batch carried
/// the move back at all.
pub fn recovery_for(inserted: usize, has_restore: bool) -> Recovery {
    match inserted {
        // Nothing reached the input stream.
        0 => Recovery {
            click_complete: false,
            release_needed: false,
            restore_needed: false,
        },
        // Only the move: the pointer sits at the press point, no button was pressed.
        1 => Recovery {
            click_complete: false,
            release_needed: false,
            restore_needed: has_restore,
        },
        // The move and the down: the button is down and the up must follow.
        2 => Recovery {
            click_complete: false,
            release_needed: true,
            restore_needed: has_restore,
        },
        // The click is complete; at most the move back is missing.
        n => Recovery {
            click_complete: true,
            release_needed: false,
            restore_needed: has_restore && n < 4,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Effects, abstracted so the effect sequence is one function (`settle`) over a small trait
// ---------------------------------------------------------------------------------------------

/// What the one click batch did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickSend {
    /// The application received exactly one click at the press point. `pointer_restored` is
    /// whether the pointer was put back where the user had it.
    Sent { pointer_restored: bool },
    /// The press point cannot be expressed on the virtual desktop; nothing was sent.
    Unmappable,
    /// The OS inserted nothing usable (or only the move, which was undone).
    Rejected,
    /// The down was inserted and the matching up could not be.
    ButtonStuck,
}

/// The effects a settle needs. The Windows hook implements it with real calls (`mouse_hook::WinEnv`).
pub trait ClickEnv {
    /// Read everything the replay needs that must NOT be read between the target check and the
    /// send (the virtual-desktop metrics and the press point's absolute coordinates), and say
    /// whether the press point can be expressed at all. Called once, before [`ClickEnv::target`].
    fn prepare(&mut self, origin: (i32, i32)) -> bool;
    /// Re-resolve the window under the ORIGINAL press point and compare it with the identity
    /// captured at press pickup.
    fn target(&mut self) -> TargetCheck;
    /// State of the host toggle lane.
    fn lane(&mut self) -> LaneState;
    /// Hand the toggle to its lane without waiting; `false` when it was not accepted.
    fn submit_toggle(&mut self) -> bool;
    /// Deliver the middle click at `origin` as ONE input batch: an absolute move to `origin`, the
    /// down, the up, and a move back to where the pointer was read just before the send. Position
    /// and click are therefore atomic to the input queue; nothing the user does can land between
    /// them.
    fn click_at(&mut self, origin: (i32, i32)) -> ClickSend;
}

/// Replay the click at `origin`. The caller has just verified the target (`settle` does, with no
/// effect in between); there is no pointer move to verify any more, because the move is part of the
/// click batch. `mappable` is what [`ClickEnv::prepare`] answered: a press point that cannot be
/// expressed sends nothing.
pub fn replay_at<E: ClickEnv>(
    env: &mut E,
    origin: (i32, i32),
    host_toggle_unavailable: bool,
    mappable: bool,
) -> ClickOutcome {
    if !mappable {
        return ClickOutcome::Dropped(DropReason::PositionUnmappable);
    }
    match env.click_at(origin) {
        ClickSend::Sent { pointer_restored } => ClickOutcome::Delivered {
            pointer_restored,
            host_toggle_unavailable,
        },
        ClickSend::Unmappable => ClickOutcome::Dropped(DropReason::PositionUnmappable),
        ClickSend::Rejected => ClickOutcome::Dropped(DropReason::SendRejected),
        ClickSend::ButtonStuck => ClickOutcome::Dropped(DropReason::ButtonStuck),
    }
}

/// Settle one held click: decide, then perform exactly one outcome.
pub fn settle<E: ClickEnv>(
    env: &mut E,
    origin: (i32, i32),
    resolution: Resolution,
    shutting_down: bool,
    has_toggle: bool,
) -> ClickOutcome {
    // Everything the replay needs is read first, so that between the target check and the send
    // only the send's own preparation remains.
    let mappable = env.prepare(origin);
    // The lane state is read before the target check too (the hand-off itself re-checks it
    // atomically in `submit_toggle`), so the target check is the last read before the effect.
    let lane = env.lane();
    let target = env.target();
    let toggle_wanted = !shutting_down && has_toggle && resolution == Resolution::NotLink;
    match delivery_for(resolution, shutting_down, has_toggle, target, lane) {
        Delivery::Drop(reason) => ClickOutcome::Dropped(reason),
        Delivery::Replay => replay_at(
            env,
            origin,
            toggle_wanted && lane == LaneState::Stalled,
            mappable,
        ),
        Delivery::Toggle => {
            if env.submit_toggle() {
                ClickOutcome::Toggled
            } else {
                // The lane refused (gone): the host toggle is unavailable, so replay.
                replay_at(env, origin, true, mappable)
            }
        }
    }
}
