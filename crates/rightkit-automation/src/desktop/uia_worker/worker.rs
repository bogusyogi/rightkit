//! Worker loop, request dispatch & native UI Automation operations.
//!
//! Three invariants are enforced centrally in this file (HANDOFF sections 3.5 and
//! 3.6):
//!
//! * Class A, every unit of work has a deadline the supervisor knows about.
//!   A "unit" is a request OR a release window (`run_release_unit`): anything
//!   that can call into a provider or `Release` a COM pointer. `begin` records
//!   it; `Supervisor::reap_overrun` abandons the worker when it hangs. Elements
//!   that leave a `PinStore` are released inside such a window, including on a
//!   clean close (`begin_release` admits a closed handle's running worker). They
//!   are leaked only when the generation was abandoned (counted at abandonment
//!   from the supervisor's own pin count, forgotten without a second count if the
//!   thread returns) or the window could not be opened; a `PinStore` dropped with
//!   pins in it (unwinding) leaks and counts them too.
//! * Class B, no provider call between the claim and the side effect. Every
//!   externally visible effect is `effects::run_effect`: all provider READS
//!   first, then `Ctx::claim_effect` (liveness, deadline and caller handshake
//!   under the supervisor lock), then ONE crate call that performs only the
//!   effect (`uia::set_focus_raw`). The last provider
//!   read of an effect is `window_membership`: the element must currently belong
//!   to the captured top-level window, or nothing is issued. The `claim` closure
//!   is the supervisor claim FOLLOWED BY `exact_window_holds`, a re-check of the
//!   exact top-level window, the proven member HWND and the foreground window with
//!   cheap OS calls only; its success is the last statement before the effect
//!   call. A denied supervisor claim and a refused re-check are both "not
//!   issued". The effect call itself can hang and complete late: that is "issued,
//!   outcome unknown", never "not issued".
//! * Pins (`pins::PinStore`): a token is usable only on the handle and generation
//!   that minted it AND while the pin is in the map.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::desktop::windows::process_creation_time;
use crate::uia_text::{TerminalLimits, TextRead};
use crate::win::uia::{self, El};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, GA_ROOT,
};

use super::api::Inner;
use super::effects::{run_effect, EffectRun};
use super::pins::{PinStore, Pinned, GLOBAL_LEDGER};
use super::protocol::{
    CapturedStop, Config, FocusedElementIdentity, NativeTarget, Request, RestoreOutcome, StopRef,
    TextProof,
};
use super::supervisor::{deadline_after, remaining, retry_fits, walk_end, Begin, EffectGate};

// The VS Code terminal static-label rule lives with RightKit's UIA text rules.
pub(super) use crate::uia_text::is_static_terminal_label;

/// What a unit of work knows about the worker it runs on.
struct Ctx<'a> {
    config: &'a Config,
    inner: &'a Inner,
    generation: u64,
}

impl Ctx<'_> {
    fn emit(&self, event: &str, detail: &str) {
        if let Some(sink) = self.config.event_sink {
            sink(event, detail);
        }
    }

    /// The single place a worker answers. Results computed after abandonment
    /// are dropped (the caller sees a disconnect, the same fail-closed value as
    /// a timeout).
    fn reply<T>(&self, reply: &mpsc::SyncSender<T>, value: T) {
        if self.inner.is_live(self.generation) {
            let _ = reply.send(value);
        }
    }

    /// THE gate for every externally visible side effect. It is only ever called
    /// from the `claim` argument of `run_effect`, as its FIRST statement; the
    /// OS-only `exact_window_holds` re-check follows it inside the same closure,
    /// so nothing but that re-check and the single effect call can follow it.
    /// True means the supervisor handshake succeeded (the gate is marked issued).
    /// False means it was skipped and was NOT issued; the reason is reported as
    /// `uia_effect_not_issued`. If the re-check that follows then refuses
    /// (`uia_effect_target_changed`), the effect is still not issued although the
    /// gate was marked: the worker answers `NotIssued`; if that answer is lost
    /// (the generation was abandoned meanwhile) the caller reports
    /// `IssuedUnconfirmed`, an over-report in the safe direction.
    fn claim_effect(&self, kind: &str, deadline: Instant, gate: &EffectGate) -> bool {
        let verdict = self.inner.permit_effect(self.generation, deadline, gate);
        match verdict {
            Ok(()) => true,
            Err(reason) => {
                self.emit(
                    "uia_effect_not_issued",
                    &format!("kind={kind} reason={}", reason.as_str()),
                );
                false
            }
        }
    }

    /// The effect was issued but no verified result followed. The caller must
    /// treat this as a possible change to the target / the user's focus.
    fn issued_unconfirmed(&self, kind: &str, reason: &str) {
        self.emit(
            "uia_effect_issued_unconfirmed",
            &format!("kind={kind} reason={reason}"),
        );
    }

    /// End of life of a COM temporary the unit owns: released normally while
    /// the generation may release them, leaked and counted once it is not, so a thread nothing
    /// supervises never calls `Release` into a provider that just proved
    /// unresponsive. (A temporary is not part of the supervisor's pin count, so
    /// this leak is counted here.)
    fn discard<T>(&self, value: T) {
        if self.inner.may_release(self.generation) {
            drop(value);
        } else {
            GLOBAL_LEDGER.leak_one(value);
        }
    }

    /// A capture produced a pin: take ownership of it in the supervisor's count.
    /// False when this worker is no longer live; the caller then `discard`s the
    /// element.
    fn pin_added(&self) -> bool {
        self.inner.pin_added(self.generation)
    }
}

enum Wait {
    Request(Request),
    /// The idle timer fired: prune and look again.
    Tick,
    /// The channel closed.
    Closed,
}

/// Block for the next request. `next_expiry` is the earliest pin expiry; with
/// none held the worker sleeps until a request arrives, otherwise it wakes for
/// the TTL prune even when no request ever comes.
fn wait_for_request(rx: &mpsc::Receiver<Request>, next_expiry: Option<Instant>) -> Wait {
    match next_expiry {
        None => rx.recv().map_or(Wait::Closed, Wait::Request),
        Some(at) => match rx.recv_timeout(remaining(at, Instant::now())) {
            Ok(request) => Wait::Request(request),
            Err(mpsc::RecvTimeoutError::Timeout) => Wait::Tick,
            Err(mpsc::RecvTimeoutError::Disconnected) => Wait::Closed,
        },
    }
}

/// Everything that is due for release now: capacity victims left by the last
/// capture, TTL victims, and releases that could not be queued. Each goes
/// through `run_release_unit`. False means the worker must leave.
fn release_due(inner: &Inner, generation: u64, pins: &mut PinStore<El>) -> bool {
    let mut victims = pins.take_evicted();
    victims.extend(pins.take_expired(Instant::now(), inner.config.retained_stop_target_ttl));
    for token in inner.take_pending_releases(generation) {
        if let Some(pin) = pins.remove(token) {
            victims.push(pin);
        }
    }
    run_release_unit(inner, generation, victims)
}

/// The persistent read-only loop of one worker generation. It owns the
/// retained stop elements of that generation (`PinStore`). Every `Release` of
/// one of them happens inside a supervised release window (`run_release_unit`),
/// also when the handle was closed cleanly; on an abandoned generation they are
/// forgotten instead (they were counted as leaked when it was abandoned).
pub(super) fn worker_loop(
    rx: mpsc::Receiver<Request>,
    inner: Arc<Inner>,
    generation: u64,
    abandoned: Arc<AtomicBool>,
) {
    // Armed before ANY initialisation so the exit (clean, after abandonment, or
    // on unwind from a failed init) is always reported to the supervisor.
    let _exit = ExitGuard {
        inner: Arc::clone(&inner),
        generation,
    };
    // Initialisation is bounded by the supervisor, not by this thread: the
    // worker is `starting` from spawn, and a caller (or the next `acquire`)
    // abandons it once `Config::worker_init_timeout` + `abandon_grace` pass.
    //
    // This worker uses its own UIA instance, so a replacement never waits on a
    // `CoCreateInstance` another generation is hung inside. Keep the instance
    // in this scope so COM release finishes before the exit guard frees the
    // supervisor's slot. Failed initialization backs off the next spawn.
    let Some(_automation) = uia::PrivateAutomation::enter() else {
        inner.note_init_failure(generation);
        return;
    };
    if abandoned.load(Ordering::Acquire) {
        return;
    }
    inner.apply_provider_timeouts();
    if abandoned.load(Ordering::Acquire) || !inner.worker_ready(generation) {
        return;
    }
    let mut pins: PinStore<El> = PinStore::new(inner.handle_id(), generation, &GLOBAL_LEDGER);
    let ctx = Ctx {
        config: &inner.config,
        inner: &inner,
        generation,
    };
    loop {
        // TTL victims, capacity victims and queued-late releases, in their own
        // windows, BEFORE waiting and before a request starts: an expired pin is
        // never served, and a hung Release is attributed to this worker.
        if !release_due(&inner, generation, &mut pins) {
            break;
        }
        if abandoned.load(Ordering::Acquire) {
            break;
        }
        let request =
            match wait_for_request(&rx, pins.next_expiry(inner.config.retained_stop_target_ttl)) {
                Wait::Request(request) => request,
                Wait::Tick => continue,
                Wait::Closed => break,
            };
        if abandoned.load(Ordering::Acquire) {
            break;
        }
        // The wait may have been long: expire again before serving.
        if !release_due(&inner, generation, &mut pins) {
            break;
        }
        // A release is always honoured (never skipped for being queued past its
        // informational deadline) and always runs in its own window.
        if let Request::ReleaseStopTarget { token, .. } = request {
            if let Some(pin) = pins.remove(token) {
                if !run_release_unit(&inner, generation, vec![pin]) {
                    break;
                }
            }
            continue;
        }
        match inner.begin(generation, request.deadline()) {
            Begin::Run => {}
            // Queued past its deadline: the caller has given up. Not started,
            // not answered (an effect request's gate is cancelled by the caller).
            Begin::Expired => continue,
            Begin::Abandoned => break,
        }
        process_request(request, &mut pins, &ctx);
        inner.finish(generation);
        if abandoned.load(Ordering::Acquire) {
            break;
        }
    }
    // Exit: whatever is still pinned is released inside a window (a clean close
    // or any other exit of a running generation), or forgotten without a second
    // count when the generation was abandoned (counted at abandonment).
    let rest = pins.drain_all();
    run_release_unit(&inner, generation, rest);
}

/// The only place the loop drops retained elements. Returns false when the
/// worker must leave (its generation was abandoned or replaced).
///
/// * Running generation, closed or not: the drop (COM `Release`) runs between
///   `begin_release` and `finish` with a `Config::release_timeout` deadline, so
///   a Release hung in the provider is abandoned by the supervisor like any hung
///   request (at the next `acquire`, caller timeout or `shutdown` budget end),
///   and the pins are then counted as leaked. After a normal release the
///   supervisor's pin count is reduced (`pins_released`).
/// * Abandoned or replaced generation: the elements were counted as leaked at
///   abandonment (`Supervisor::abandon`), so they are forgotten here WITHOUT a
///   second count.
/// * A window that cannot be opened (an unrepresentable or already-passed
///   deadline; not reachable with a validated `release_timeout` of at least
///   50 ms): the elements are leaked and counted here, and removed from the
///   supervisor's pin count so an abandonment does not count them again.
///
/// The ledger is bounded process-wide: past `Config::max_forgotten_pins` new pins
/// are refused.
fn run_release_unit(inner: &Inner, generation: u64, victims: Vec<Pinned<El>>) -> bool {
    if victims.is_empty() {
        return true;
    }
    let count = victims.len();
    let Some(deadline) = deadline_after(Instant::now(), inner.config.release_budget()) else {
        GLOBAL_LEDGER.leak(victims);
        inner.pins_released(generation, count);
        return true;
    };
    match inner.begin_release(generation, deadline) {
        Begin::Run => {
            drop(victims);
            inner.finish(generation);
            inner.pins_released(generation, count);
            true
        }
        Begin::Expired => {
            GLOBAL_LEDGER.leak(victims);
            inner.pins_released(generation, count);
            true
        }
        Begin::Abandoned => {
            std::mem::forget(victims);
            false
        }
    }
}

struct ExitGuard {
    inner: Arc<Inner>,
    generation: u64,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.inner.worker_exited(self.generation);
    }
}

fn process_request(request: Request, pins: &mut PinStore<El>, ctx: &Ctx<'_>) {
    let config = ctx.config;
    match request {
        Request::Editable { deadline, reply } => {
            let value = if expired(deadline) {
                None
            } else {
                editable(deadline, config)
            };
            ctx.reply(&reply, value);
        }
        Request::Password { deadline, reply } => {
            let value = if expired(deadline) {
                None
            } else {
                password(deadline, config)
            };
            ctx.reply(&reply, value);
        }
        Request::Identity {
            deadline,
            process_id,
            control_handle,
            window_handle,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                identity(process_id, control_handle, window_handle)
            };
            ctx.reply(&reply, value);
        }
        Request::ReadText {
            deadline,
            target,
            expected,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                read_text(&target, &expected, deadline, config)
            };
            ctx.reply(&reply, value);
        }
        Request::ReadStopText {
            deadline,
            stop,
            reply,
        } => {
            // `usable` is the second line of defence behind the API-side check:
            // a token of another handle or generation, or a pin that is no
            // longer in the map, fails closed here.
            let value = if expired(deadline) || !pins.usable(stop.element_token) {
                None
            } else {
                read_stop_text(pins, &stop, deadline, config)
            };
            ctx.reply(&reply, value);
        }
        Request::CaptureStopTarget {
            deadline,
            target,
            reply,
        } => {
            let value = if expired(deadline) {
                None
            } else {
                capture_stop_target_inner(deadline, target, pins, ctx)
            };
            ctx.reply(&reply, value);
        }
        Request::ValidateStopTarget {
            deadline,
            stop,
            reply,
        } => {
            let value =
                !expired(deadline) && validate_stop_target_inner(deadline, &stop, pins, config);
            ctx.reply(&reply, value);
        }
        Request::StopTargetIsGone {
            deadline,
            stop,
            reply,
        } => {
            let value = if expired(deadline) || !pins.usable(stop.element_token) {
                None
            } else {
                stop_target_is_gone(pins, &stop)
            };
            ctx.reply(&reply, value);
        }
        Request::RestoreStopTarget {
            deadline,
            stop,
            gate,
            reply,
        } => {
            let outcome = if expired(deadline) || !pins.usable(stop.element_token) {
                RestoreOutcome::NotIssued
            } else {
                restore_stop_target_inner(deadline, &stop, pins, ctx, &gate)
            };
            ctx.reply(&reply, outcome);
        }
        Request::WebContainerNamed {
            deadline,
            stop,
            hotkey_control,
            window_handle,
            reply,
        } => {
            let value = !expired(deadline)
                && pins.usable(stop.element_token)
                && web_container_named(
                    deadline,
                    pins,
                    &stop,
                    hotkey_control,
                    window_handle,
                    config,
                );
            ctx.reply(&reply, value);
        }
        // Handled by the loop before `begin` (it needs a window of its own, not
        // a request window); never dispatched here.
        Request::ReleaseStopTarget { .. } => {}
    }
}

fn expired(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

fn foreground_window() -> isize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() as isize }
}

/// Why a recheck refused. The tag is the `reason` of the
/// `uia_effect_target_changed` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetChange {
    /// No window handle was recorded for the target.
    Unbound,
    /// The recorded HWND no longer names a window (`IsWindow`).
    WindowGone,
    /// The HWND exists but its owning process id is not the recorded one (the
    /// handle was recycled by another process, or the window was re-parented to
    /// another owner).
    OwnerChanged,
    /// The process id is the recorded one but its creation time is not (the
    /// process id was reused by a new process).
    ProcessReplaced,
    /// No member HWND was proven for the element (the recheck was called without
    /// a membership proof).
    MemberUnproven,
    /// The HWND-backed node the membership proof found no longer has the
    /// captured window as its root (destroyed, re-parented).
    MemberRootChanged,
    /// The recorded window is not the foreground window.
    NotForeground,
}

impl TargetChange {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::WindowGone => "window_gone",
            Self::OwnerChanged => "owner_changed",
            Self::ProcessReplaced => "process_replaced",
            Self::MemberUnproven => "member_unproven",
            Self::MemberRootChanged => "member_root_changed",
            Self::NotForeground => "not_foreground",
        }
    }
}

/// Class B re-check of the EXACT top-level window an effect was bound to, made
/// as the last step before the effect call (inside the `claim` closure, after the
/// supervisor claim). Cheap OS calls only (`IsWindow`, `GetWindowThreadProcessId`,
/// `OpenProcess` + `GetProcessTimes` through `process_creation_time`,
/// `GetAncestor`, `GetForegroundWindow`); no UIA or other provider call, so it
/// cannot block on the target. `member_hwnd` is the HWND-backed node that
/// `window_membership` proved belongs to the window; its root is read again here
/// so a node destroyed or re-parented after the proof is caught. The foreground
/// comparison is last: it is the condition that changes fastest. `creation_time`
/// is `None` only for a request that never recorded one (the dormant SetValue
/// path); then the creation-time comparison is skipped and the caller says so.
fn exact_window_holds(
    window_handle: isize,
    process_id: u32,
    creation_time: Option<u64>,
    member_hwnd: isize,
) -> Result<(), TargetChange> {
    if window_handle == 0 || process_id == 0 {
        return Err(TargetChange::Unbound);
    }
    if member_hwnd == 0 {
        return Err(TargetChange::MemberUnproven);
    }
    let hwnd = window_handle as HWND;
    // SAFETY: IsWindow and GetWindowThreadProcessId accept any handle value and
    // fail (0) for one that is not a window; the out pointer is a local.
    let owner = unsafe {
        if IsWindow(hwnd) == 0 {
            return Err(TargetChange::WindowGone);
        }
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        owner
    };
    if owner != process_id {
        return Err(TargetChange::OwnerChanged);
    }
    if let Some(expected) = creation_time {
        if process_creation_time(process_id) != Some(expected) {
            return Err(TargetChange::ProcessReplaced);
        }
    }
    if root_window_of(member_hwnd) != Some(window_handle) {
        return Err(TargetChange::MemberRootChanged);
    }
    if foreground_window() != window_handle {
        return Err(TargetChange::NotForeground);
    }
    Ok(())
}

/// The top-level window (`GetAncestor(GA_ROOT)`) of a window handle; `None` when
/// the OS returns no window (invalid or destroyed handle). `GA_ROOT` walks the
/// PARENT chain only: a child window gives its top-level window, an owned popup
/// or dialog (top-level, owned but not a child) gives itself, so it is not the
/// window of its owner. A handle that is itself top-level gives itself.
fn root_window_of(hwnd: isize) -> Option<isize> {
    // SAFETY: GetAncestor accepts any handle value and returns NULL for one that
    // is not a window.
    let root = unsafe { GetAncestor(hwnd as HWND, GA_ROOT) };
    (!root.is_null()).then_some(root as isize)
}

/// Why `window_membership` could not prove membership. Never a "yes".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unproven {
    /// No window handle was recorded for the target.
    Unbound,
    /// The walk's wall-clock budget or the request deadline passed.
    Deadline,
    /// No HWND-backed node within `Config::membership_max_hops` ancestors (a deep
    /// virtual tree).
    DepthCap,
    /// The parent read FAILED (`El::parent_checked` returned `Err`: the walker or
    /// the call failed, the element is gone) before an HWND-backed node was
    /// reached.
    ParentUnreadable,
    /// The walk reached the root of the tree (`El::parent_checked` returned
    /// `Ok(None)`) without finding an HWND-backed node: there is no window
    /// ancestor to prove membership through.
    NoWindowAncestor,
    /// `El::native_hwnd_checked` FAILED on a node (`Err`). A failed read is never
    /// taken as "this node has no window handle": the walk stops here instead of
    /// climbing to a parent, which could belong to a different window (an owned
    /// dialog's own window node climbing to its owner).
    HandleUnreadable,
    /// An HWND-backed node was found but the OS gave no root for it (destroyed).
    RootUnreadable,
}

/// The tri-state answer to "does this element currently belong to the captured
/// top-level window?". It is matched, never converted to a `bool` before the
/// decision; only `Member` allows an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Membership {
    /// Proven: `hwnd` is the element's own HWND or that of its nearest
    /// HWND-backed ancestor, and its `GA_ROOT` is the captured window.
    Member { hwnd: isize },
    /// Proven the other way: that HWND's root is a different window (another
    /// top-level window of the same process, an owned popup or dialog, a window
    /// of another process).
    Outside,
    /// Could not be established.
    Unproven(Unproven),
}

impl Membership {
    /// Event reason tag for a refusal (`Member` is never refused).
    fn refusal_tag(self) -> &'static str {
        match self {
            Self::Member { .. } => "member",
            Self::Outside => "element_outside_window",
            Self::Unproven(Unproven::Unbound) => "membership_unbound",
            Self::Unproven(Unproven::Deadline) => "membership_deadline",
            Self::Unproven(Unproven::DepthCap) => "membership_depth_cap",
            Self::Unproven(Unproven::ParentUnreadable) => "membership_parent_unreadable",
            Self::Unproven(Unproven::NoWindowAncestor) => "membership_no_window_ancestor",
            Self::Unproven(Unproven::HandleUnreadable) => "membership_handle_unreadable",
            Self::Unproven(Unproven::RootUnreadable) => "membership_root_unreadable",
        }
    }
}

/// Is `element` currently inside the top-level window `window_handle`? Provider
/// reads, so it belongs BEFORE the final `exact_window_holds`.
///
/// Calls: `El::native_hwnd_checked()` (`CurrentNativeWindowHandle`) on the
/// element, then on each `El::parent_checked()` (the ControlView walker's
/// `GetParentElement`), at most `Config::membership_max_hops` parent steps
/// (clamped 1..256, default 64) inside `Config::web_walk_budget` (default 400 ms,
/// at most the request `deadline`). Both accessors tell a FAILED call from a
/// successful "none" answer: a failed handle read is `Unproven(HandleUnreadable)`
/// and ends the walk (it is not "virtual"), a failed parent read is
/// `Unproven(ParentUnreadable)`, and reaching the root of the tree with no
/// HWND-backed node is `Unproven(NoWindowAncestor)`. Only a successful read that
/// says "no handle" (`Ok(None)`) climbs to the parent.
/// The first node that has a native window handle ends the walk: its
/// `GetAncestor(GA_ROOT)` must equal `window_handle`. A virtual element (a
/// browser page element, no HWND of its own) is therefore resolved through its
/// nearest HWND-backed ancestor; none within the cap is `Unproven`. A node whose
/// handle names a different root is `Outside`, with no further climbing: this
/// refuses an owned popup or dialog (its root is itself) and a window of another
/// top-level, even one of the same process. A child window whose root IS the
/// captured window is accepted.
///
/// Each provider call can block; the walk checks its budget before every hop and
/// the supervisor's request deadline and abandonment bound a call that hangs (no
/// per-call cancel exists). It reads nothing but handles and parents.
fn window_membership(
    element: &El,
    window_handle: isize,
    deadline: Instant,
    config: &Config,
) -> Membership {
    if window_handle == 0 {
        return Membership::Unproven(Unproven::Unbound);
    }
    let stop_at = walk_end(Instant::now(), config.web_walk_budget, deadline);
    let mut current = element.clone();
    let mut hops = 0usize;
    loop {
        if Instant::now() >= stop_at {
            return Membership::Unproven(Unproven::Deadline);
        }
        match current.native_hwnd_checked() {
            Err(()) => return Membership::Unproven(Unproven::HandleUnreadable),
            Ok(Some(handle)) => {
                return match root_window_of(handle) {
                    Some(root) if root == window_handle => Membership::Member { hwnd: handle },
                    Some(_) => Membership::Outside,
                    None => Membership::Unproven(Unproven::RootUnreadable),
                };
            }
            Ok(None) => {}
        }
        if hops >= config.membership_max_hops {
            return Membership::Unproven(Unproven::DepthCap);
        }
        hops += 1;
        match current.parent_checked() {
            Ok(Some(parent)) => current = parent,
            Ok(None) => return Membership::Unproven(Unproven::NoWindowAncestor),
            Err(()) => return Membership::Unproven(Unproven::ParentUnreadable),
        }
    }
}

fn focused() -> Option<El> {
    uia::focused_element()
}

/// Native window handle of an element for the capture-time identity; 0 when it
/// has none OR the read failed (`native_hwnd` does not tell them apart). Used only
/// to RECORD an identity; every decision that rests on a handle (validation, the
/// native write's `matching_element`, `window_membership`) uses
/// `native_hwnd_checked` and refuses a failed read.
fn native_handle(element: &El) -> isize {
    element.native_hwnd().unwrap_or(0)
}

fn editable_once() -> Option<bool> {
    focused()?.is_editable()
}

fn editable(deadline: Instant, config: &Config) -> Option<bool> {
    let first = editable_once();
    if first != Some(false) || expired(deadline) {
        return first;
    }
    if !retry_fits(Instant::now(), config.transient_retry_delay, deadline) {
        return first;
    }
    std::thread::sleep(config.transient_retry_delay);
    if expired(deadline) {
        first
    } else {
        editable_once()
    }
}

fn password(deadline: Instant, config: &Config) -> Option<bool> {
    let probe = || focused()?.password_state();
    let first = probe();
    if first.is_some() || expired(deadline) {
        return first;
    }
    if !retry_fits(Instant::now(), config.transient_retry_delay, deadline) {
        return first;
    }
    std::thread::sleep(config.transient_retry_delay);
    if expired(deadline) {
        None
    } else {
        probe()
    }
}

fn identity(
    expected_process_id: i32,
    expected_control_handle: isize,
    expected_window_handle: isize,
) -> Option<FocusedElementIdentity> {
    let element = focused()?;
    let process_id = element.pid()?;
    let native_window_handle = native_handle(&element);
    if process_id != expected_process_id
        || (native_window_handle != 0
            && expected_control_handle != 0
            && native_window_handle != expected_control_handle
            && native_window_handle != expected_window_handle)
        || !element.has_focus()
        || foreground_window() != expected_window_handle
    {
        return None;
    }
    Some(FocusedElementIdentity {
        process_id,
        native_window_handle,
        runtime_id: element.runtime_id()?,
    })
}

fn capture_stop_target_inner(
    deadline: Instant,
    target: NativeTarget,
    pins: &mut PinStore<El>,
    ctx: &Ctx<'_>,
) -> Option<CapturedStop> {
    // Too many COM references are already held leaked process-wide: refuse new
    // pins (the caller falls back exactly as for any failed capture) and say so
    // once.
    if GLOBAL_LEDGER.suspended(ctx.config.max_forgotten_pins) {
        if GLOBAL_LEDGER.report_suspension_once() {
            ctx.emit(
                "uia_pins_suspended",
                &format!(
                    "leaked={} cap={}",
                    GLOBAL_LEDGER.leaked(),
                    ctx.config.max_forgotten_pins
                ),
            );
        }
        return None;
    }
    // Under the cap (a new handle may use a larger one): a later suspension is
    // reported again.
    GLOBAL_LEDGER.rearm_suspension_report();
    let element = focused()?;
    let process_id = target.process_id? as i32;
    let window_handle = target.window_handle?;
    if expired(deadline)
        || element.pid()? != process_id
        || !element.has_focus()
        || foreground_window() != window_handle
    {
        return None;
    }
    let captured_process_creation_time = process_creation_time(process_id as u32)?;
    let native_window_handle = native_handle(&element);
    let identity = FocusedElementIdentity {
        process_id,
        native_window_handle,
        runtime_id: element.runtime_id()?,
    };
    let focused_text_input = element.is_editable();
    let is_password = element.password_state();
    // UIA reads above may yield to another app. Reject a capture that did not
    // finish against the same foreground window and focused runtime element.
    let final_element = focused()?;
    if expired(deadline)
        || foreground_window() != window_handle
        || process_creation_time(process_id as u32) != Some(captured_process_creation_time)
        || final_element.pid()? != process_id
        || !final_element.has_focus()
        || final_element.runtime_id()? != identity.runtime_id
    {
        return None;
    }
    // The token carries this handle and worker generation: it is valid on this
    // worker only. A spent per-generation sequence refuses the capture instead
    // of reusing a token.
    let token = pins.mint()?;
    // The pin is owned by this generation only while it is live. The check and
    // the supervisor's pin count are one step under the abandonment lock; a
    // worker abandoned (or closed) while it was inside the reads above does not
    // take the element into its store, and `discard` releases it (closed,
    // provider responsive) or leaks and counts it (abandoned).
    if !ctx.pin_added() {
        ctx.discard(final_element);
        return None;
    }
    // Capacity eviction happens here and nowhere else: the oldest pins move to
    // the eviction list (released later in their own window), never the new pin
    // and never a pin another request is using.
    pins.insert_new(
        token,
        final_element,
        Instant::now(),
        ctx.config.max_retained_stop_targets,
    );
    Some(CapturedStop {
        window_handle,
        process_creation_time: captured_process_creation_time,
        identity,
        focused_text_input,
        is_password,
        element_token: token,
    })
}

/// Continuity from the live focus. The pin must still exist: a token whose pin
/// was released, TTL-expired or evicted never validates, even when the live
/// focus matches the identity it once named. The live focused
/// element must also currently belong to the captured top-level window
/// (`window_membership`; an element that is `Outside` or `Unproven` does not
/// validate), and `exact_window_holds` is the last statement, so a pid, runtime
/// id and foreground match alone no longer validates a second window of the same
/// process. This is the host's gate before it pastes and the check behind
/// `RestoreOutcome::Restored`.
fn validate_stop_target_inner(
    deadline: Instant,
    stop: &StopRef,
    pins: &PinStore<El>,
    config: &Config,
) -> bool {
    if !pins.usable(stop.element_token) {
        return false;
    }
    let Some(element) = focused() else {
        return false;
    };
    if expired(deadline)
        || Some(stop.window_handle) != Some(foreground_window())
        || element.pid() != Some(stop.identity.process_id)
        || !element.has_focus()
    {
        return false;
    }
    if process_creation_time(stop.identity.process_id as u32) != Some(stop.process_creation_time) {
        return false;
    }
    // A FAILED native-handle read refuses; only a successful "no handle" (a
    // virtual element) leaves the decision to the membership proof below.
    let Ok(native_read) = element.native_hwnd_checked() else {
        return false;
    };
    let native = native_read.unwrap_or(0);
    if !((stop.identity.native_window_handle == 0
        || native == 0
        || native == stop.identity.native_window_handle)
        && element
            .runtime_id()
            .is_some_and(|id| id == stop.identity.runtime_id))
    {
        return false;
    }
    let Membership::Member { hwnd } =
        window_membership(&element, stop.window_handle, deadline, config)
    else {
        return false;
    };
    exact_window_holds(
        stop.window_handle,
        stop.identity.process_id as u32,
        Some(stop.process_creation_time),
        hwnd,
    )
    .is_ok()
}

/// Chromium reports a Document/web-area container as focused while the caret
/// is in an inner editor whose own HWND is 0. Accept the hotkey proof as
/// naming the retained element only when: the element is still the captured
/// one, positively non-password, keyboard-focusable and either has a writable
/// Value pattern, and a bounded parent walk reaches a same-process
/// Document whose native window is the proof's control, never reaching the
/// top-level window or a Window control first.
///
/// RightKit exposes no TextEdit-pattern probe on its opaque element, so the
/// TextEdit-only contenteditable case is not recognised here and fails closed
/// (the writable-Value case still binds).
fn web_container_named(
    deadline: Instant,
    pins: &PinStore<El>,
    stop: &StopRef,
    hotkey_control: isize,
    window_handle: isize,
    config: &Config,
) -> bool {
    if hotkey_control == 0 || hotkey_control == window_handle || window_handle != stop.window_handle
    {
        return false;
    }
    let Some(entry) = pins.get(stop.element_token) else {
        return false;
    };
    let element = &entry.element;
    if element.pid() != Some(stop.identity.process_id)
        || element.runtime_id().as_ref() != Some(&stop.identity.runtime_id)
        || element.password_state() != Some(false)
    {
        return false;
    }
    let writable_value = writable_value_pattern(element).unwrap_or(false);
    // A plain page-level Document (no writable Value / TextEdit) never binds.
    if !(config.rules.web_area_editor_is_bindable)(
        Some(false),
        element.is_keyboard_focusable().unwrap_or(false),
        writable_value,
        false,
    ) {
        return false;
    }
    let stop_at = walk_end(Instant::now(), config.web_walk_budget, deadline);
    let mut current = element.clone();
    for _ in 0..config.web_walk_max_hops {
        if Instant::now() >= stop_at {
            return false;
        }
        let Some(parent) = current.parent() else {
            return false;
        };
        if parent.pid() != Some(stop.identity.process_id) {
            return false;
        }
        let handle = parent.native_hwnd();
        let role = uia::read_node(&parent).map(|node| node.native_role);
        let is_window = role.as_deref() == Some("UIA_Window");
        let is_document = role.as_deref() == Some("UIA_Document");
        if handle == Some(window_handle) || is_window {
            return false;
        }
        if is_document && handle == Some(hotkey_control) {
            // The hotkey control must itself sit in the captured top-level
            // window (OS call), not merely in the same process; `window_handle`
            // equals the captured `stop.window_handle` (checked on entry).
            return root_window_of(hotkey_control) == Some(stop.window_handle);
        }
        current = parent;
    }
    false
}
/// `Some(true)` when the retained element is provably gone or replaced.
/// RightKit reports an unavailable element and a failed read the same way
/// (`pid()` is `None`), so an unreadable element stays unknown (`None`)
/// rather than "gone".
fn stop_target_is_gone(pins: &PinStore<El>, stop: &StopRef) -> Option<bool> {
    let element = &pins.get(stop.element_token)?.element;
    let process_id = element.pid()?;
    if process_id != stop.identity.process_id {
        return Some(true);
    }
    element
        .runtime_id()
        .map(|id| id != stop.identity.runtime_id)
}

/// Focus restore, the one effect on the user's focus. Sequenced by
/// `run_effect`, in this order:
///
/// 1. `pre`, all provider READS: foreground window and process creation time
///    (OS), the element's pid and runtime id (provider; a refusal here is
///    reported as `deadline|not_foreground|process_replaced|element_pid_changed|element_replaced`),
///    `uia::focus_precheck` (`focus_precheck_refused`)
///    (element pid: provider; foreground pid: OS), and LAST
///    `window_membership`: the retained element must currently belong to the
///    captured top-level window (`stop.window_handle`), proven through the
///    element's own or nearest HWND-backed ancestor's `GetAncestor(GA_ROOT)`. A
///    pid and runtime id match is not enough: two top-level windows of one
///    process share a pid. `Outside` (another top-level window, an owned popup or
///    dialog, a window of another process) and `Unproven` (deadline, depth cap,
///    unreadable handle, unreadable parent, root reached with no HWND-backed
///    ancestor, destroyed handle) both refuse:
///    `uia_effect_target_changed kind=focus reason=<tag>`, `NotIssued`, and
///    SetFocus is never called, so the user's focus is untouched.
/// 2. `claim`: `Ctx::claim_effect` (supervisor mutex: liveness, deadline, caller
///    handshake), then, as the closure's last statement, `exact_window_holds`
///    (OS calls only): window exists, owner pid, process creation time, the
///    proven member HWND still rooted in the captured window, and the captured
///    window still the foreground window.
/// 3. `effect`: `uia::set_focus_raw`, one provider call that only sets focus.
///
/// Nothing sits between the successful `exact_window_holds` and `SetFocus`
/// except the closure return and `run_effect`'s own branch. A precheck refusal, a
/// membership refusal, a changed target or a denied claim is `NotIssued`:
/// SetFocus was never called. Once claimed and rechecked the call counts as
/// issued and any non-verified end is `IssuedUnconfirmed`. Residual: the SetFocus
/// call itself may block and complete late; the caller then reports
/// `IssuedUnconfirmed`.
fn restore_stop_target_inner(
    deadline: Instant,
    stop: &StopRef,
    pins: &PinStore<El>,
    ctx: &Ctx<'_>,
    gate: &EffectGate,
) -> RestoreOutcome {
    let Some(pinned) = pins.get(stop.element_token) else {
        return RestoreOutcome::NotIssued;
    };
    let element = &pinned.element;
    // The member HWND the proof found, handed from `pre` to the final recheck.
    let member = Cell::new(0isize);
    let run = run_effect(
        || {
            // Each refusal is reported (`uia_effect_target_changed`, a stable
            // tag, no content) before it returns, so none is silent.
            let refuse = |reason: &str| {
                ctx.emit(
                    "uia_effect_target_changed",
                    &format!("kind=focus reason={reason}"),
                );
                Err(())
            };
            if expired(deadline) {
                return refuse("deadline");
            }
            if Some(stop.window_handle) != Some(foreground_window()) {
                return refuse("not_foreground");
            }
            if process_creation_time(stop.identity.process_id as u32)
                != Some(stop.process_creation_time)
            {
                return refuse("process_replaced");
            }
            if element.pid() != Some(stop.identity.process_id) {
                return refuse("element_pid_changed");
            }
            if element.runtime_id().as_ref() != Some(&stop.identity.runtime_id) {
                return refuse("element_replaced");
            }
            // Focus executes on the persistent MTA, preserving UIA apartment
            // affinity for virtual browser/chat editors with no stable child
            // HWND. RightKit's Focus refuses unless the element is in the
            // already-foreground app; that check is `focus_precheck`.
            let ready = match uia::focus_precheck(element) {
                Ok(ready) => ready,
                Err(_) => return refuse("focus_precheck_refused"),
            };
            // Last provider read: is the element in THIS top-level window?
            match window_membership(element, stop.window_handle, deadline, ctx.config) {
                Membership::Member { hwnd } => member.set(hwnd),
                refused => {
                    ctx.emit(
                        "uia_effect_target_changed",
                        &format!("kind=focus reason={}", refused.refusal_tag()),
                    );
                    return Err(());
                }
            }
            Ok(ready)
        },
        // The supervisor claim first, the OS-only recheck LAST: a wait for the
        // supervisor lock can no longer sit between the recheck and SetFocus.
        || {
            if !ctx.claim_effect("focus", deadline, gate) {
                return false;
            }
            match exact_window_holds(
                stop.window_handle,
                stop.identity.process_id as u32,
                Some(stop.process_creation_time),
                member.get(),
            ) {
                Ok(()) => true,
                Err(change) => {
                    ctx.emit(
                        "uia_effect_target_changed",
                        &format!("kind=focus reason={}", change.as_str()),
                    );
                    false
                }
            }
        },
        uia::set_focus_raw,
    );
    match run {
        EffectRun::Refused(()) | EffectRun::NotClaimed(_) => RestoreOutcome::NotIssued,
        // A failed call is reported as unconfirmed too: RightKit reports a
        // refusal and a failed SetFocus the same way, so "no change" cannot be
        // proven.
        EffectRun::Issued(Err(_)) => {
            ctx.issued_unconfirmed("focus", "focus_call_failed");
            RestoreOutcome::IssuedUnconfirmed
        }
        EffectRun::Issued(Ok(_)) => {
            std::thread::sleep(Duration::from_millis(5));
            if expired(deadline) {
                ctx.issued_unconfirmed("focus", "deadline_after_issue");
                return RestoreOutcome::IssuedUnconfirmed;
            }
            if validate_stop_target_inner(deadline, stop, pins, ctx.config) {
                RestoreOutcome::Restored
            } else {
                ctx.issued_unconfirmed("focus", "validation_failed");
                RestoreOutcome::IssuedUnconfirmed
            }
        }
    }
}

fn matching_element(target: &NativeTarget, expected: &FocusedElementIdentity) -> Option<El> {
    let element = focused()?;
    // A failed native-handle read refuses (`?`); only a successful "no handle"
    // is 0.
    let native = element.native_hwnd_checked().ok()?.unwrap_or(0);
    if element.pid()? != expected.process_id
        || (expected.native_window_handle != 0
            && native != 0
            && native != expected.native_window_handle)
        || element.runtime_id()? != expected.runtime_id
        || !element.has_focus()
        || element.password_state()? // unknown password state fails closed
        || target.window_handle != Some(foreground_window())
    {
        return None;
    }
    Some(element)
}

fn read_stop_text(
    pins: &PinStore<El>,
    stop: &StopRef,
    deadline: Instant,
    config: &Config,
) -> Option<TextProof> {
    let retained = pins.get(stop.element_token)?;
    if retained.element.pid()? != stop.identity.process_id
        || process_creation_time(stop.identity.process_id as u32)
            != Some(stop.process_creation_time)
        || retained.element.runtime_id()? != stop.identity.runtime_id
    {
        return None;
    }
    // Recheck sensitive role on retained element immediately before any
    // context/fingerprint read; unknown password state fails closed.
    if retained.element.password_state()? {
        return None;
    }
    read_text_element(
        &retained.element,
        stop.identity.process_id,
        stop.window_handle,
        deadline,
        config,
    )
}

fn read_text(
    target: &NativeTarget,
    expected: &FocusedElementIdentity,
    deadline: Instant,
    config: &Config,
) -> Option<TextProof> {
    let element = matching_element(target, expected)?;
    read_text_element(
        &element,
        expected.process_id,
        target.window_handle?,
        deadline,
        config,
    )
}

/// One bounded UIA text representation. `failed` is true when the pattern
/// answered but the text was over the proof budget (a missing pattern is not
/// a failure).
fn bounded_text(read: TextRead, config: &Config) -> (Option<String>, bool) {
    match read {
        TextRead::Absent => (None, false),
        TextRead::TooLarge => (None, true),
        TextRead::Text(text) if text.len() <= config.max_proof_utf8_bytes => (Some(text), false),
        TextRead::Text(_) => (None, true),
    }
}

fn read_text_element(
    element: &El,
    expected_process_id: i32,
    expected_window_handle: isize,
    deadline: Instant,
    config: &Config,
) -> Option<TextProof> {
    // Keep both independent representations. ValuePattern can expose an empty
    // or placeholder value while TextPattern has the document, and selecting
    // whichever API answered first loses useful proof evidence. Both reads
    // are bounded by UIA units and fail closed on an unknown password state.
    let (value_text, value_read_failed) = bounded_text(
        element.read_value_checked(config.max_proof_utf16_units),
        config,
    );
    let (document_text, document_read_failed) = bounded_text(
        element.read_document_text_checked(config.max_proof_utf16_units),
        config,
    );
    let total = value_text
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(document_text.as_ref().map_or(0, String::len));
    let (mut value_text, mut document_text) = if total <= config.max_proof_utf8_bytes {
        (value_text, document_text)
    } else {
        // Preserve one complete representation when retaining both would
        // exceed the transaction budget. Prefer document text over an empty
        // ValuePattern placeholder, matching the macOS proof policy.
        match (value_text, document_text) {
            (Some(value), Some(document)) if value.is_empty() && !document.is_empty() => {
                (None, Some(document))
            }
            (Some(value), _) => (Some(value), None),
            (None, document) => (None, document),
        }
    };
    // VS Code's terminal edit exposes a static label through TextPattern and
    // an empty ValuePattern. The actual draft is exposed as ListItem rows
    // below the terminal container, so retain that representation separately.
    if value_text.as_deref().is_some_and(is_static_terminal_label) {
        value_text = None;
    }
    if document_text
        .as_deref()
        .is_some_and(is_static_terminal_label)
    {
        document_text = None;
    }
    let terminal = element
        .terminal_rows(
            expected_process_id,
            expected_window_handle,
            &TerminalLimits {
                max_rows: config.terminal_max_rows,
                max_row_utf16: config.max_proof_utf16_units,
                max_total_bytes: config.max_proof_utf8_bytes,
            },
            deadline,
        )
        .map(|rows| {
            (
                rows.scope_runtime_id,
                rows.scope_native_window_handle,
                rows.text,
            )
        });
    let (terminal_rows, terminal_scope_runtime_id, terminal_scope_native_window_handle) = terminal
        .map_or((None, None, None), |(id, hwnd, text)| {
            (Some(text), Some(id), Some(hwnd))
        });
    if (value_read_failed
        && document_text.as_deref().is_none_or(|text| text.is_empty())
        && terminal_rows.as_deref().is_none_or(|text| text.is_empty()))
        || (document_read_failed
            && value_text.as_deref().is_none_or(|text| text.is_empty())
            && terminal_rows.as_deref().is_none_or(|text| text.is_empty()))
    {
        return None;
    }
    if value_text.is_none()
        && document_text.is_none()
        && terminal_rows.as_deref().is_none_or(|text| text.is_empty())
    {
        return None;
    }
    let all_text_bytes = value_text
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(document_text.as_ref().map_or(0, String::len))
        .saturating_add(terminal_rows.as_ref().map_or(0, String::len));
    if all_text_bytes > config.max_proof_utf8_bytes && terminal_rows.is_some() {
        // Terminal rows are the only authoritative representation for this
        // VS Code target; discard incidental editor label/value text when
        // retaining both would exceed the proof transaction budget.
        value_text = None;
        document_text = None;
    }
    let proof = TextProof {
        value: value_text,
        document: document_text,
        terminal_rows,
        terminal_scope_runtime_id,
        terminal_scope_native_window_handle,
    };
    // A readable empty field is a proven state, not a missing capture. The
    // caller distinguishes this from UIA/read failures by the outer Option.
    Some(proof)
}
