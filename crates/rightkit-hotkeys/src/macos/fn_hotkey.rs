//! fn-chord and push-to-talk tap (HeardRight `fn_hotkey` donor, instance-based).
//!
//! Chords: **fn+Space** toggles, **fn+Shift+Space** stops and sends, **fn+Escape** cancels; the
//! suppressing events are swallowed so no literal space or escape leaks into the focused app.
//! Push-to-talk: hold the selected Control/Option key alone (~180 ms arm delay) to record, release
//! to stop, press fn while held to send; any other key or modifier joining within one second turns
//! it into an ordinary chord and aborts.
//!
//! The donor kept all of this in process-wide statics with telemetry calls inline; here the state is
//! owned by the handle (so tests and multiple instances cannot interfere) and diagnostics go through
//! an optional callback.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ffi::*;
use super::tap::{EventTap, TapConfig, TapDecision, TapError, TapEvent, TapKind, TapLocation};
use crate::decision::should_accept_fn_chord;
use crate::ptt::{
    format_abort, should_abort_ptt_chord, should_arm_ptt, should_clear_ptt_hold_suppression,
    PttAbortCause, PttAbortKind, PttEnd, PttHandoff, PttModifiers, PttPhase,
};

pub type Action = Arc<dyn Fn() + Send + Sync + 'static>;
pub type FnAction = Arc<dyn Fn(bool) + Send + Sync + 'static>;
pub type Diag = Arc<dyn Fn(&str, &str) + Send + Sync + 'static>;

/// Delay between the selected key going down alone and recording starting; long enough that a quick
/// Ctrl+key chord invalidates the timer first.
pub const PTT_ARM_DELAY_MS: u64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttKey {
    Control = 0,
    LeftOption = 1,
    RightOption = 2,
}

impl PttKey {
    /// Parse through the crate-wide macOS alias table ([`crate::PttAlias`]): `ctrl` (also
    /// `control`, `lctrl`, `rctrl`), `lalt`, `ralt` (also `loption`, `roption`).
    pub fn parse(name: &str) -> Option<PttKey> {
        use crate::chord::{Platform, PttAlias};
        match PttAlias::parse(name, Platform::Mac).ok()? {
            PttAlias::Ctrl | PttAlias::RightCtrl => Some(PttKey::Control),
            PttAlias::LeftAlt => Some(PttKey::LeftOption),
            PttAlias::RightAlt => Some(PttKey::RightOption),
        }
    }

    fn from_u8(v: u8) -> PttKey {
        match v {
            1 => PttKey::LeftOption,
            2 => PttKey::RightOption,
            _ => PttKey::Control,
        }
    }

    fn name(self) -> &'static str {
        match self {
            PttKey::Control => "control",
            PttKey::LeftOption => "left_option",
            PttKey::RightOption => "right_option",
        }
    }

    fn modifier_flag(self) -> u8 {
        match self {
            PttKey::Control => PttModifiers::CTRL,
            _ => PttModifiers::ALT,
        }
    }
}

#[derive(Clone)]
pub struct FnHotkeyCallbacks {
    pub on_toggle: Action,
    pub on_stop_send: Action,
    /// fn + Escape: discard an active capture from anywhere.
    pub on_cancel: Action,
    /// fn (alone) pressed/released: lets onboarding light the chord UI (WebKit never sees fn).
    pub on_fn_change: FnAction,
    pub ptt_start: Action,
    pub ptt_stop: Action,
    pub ptt_stop_send: Action,
    pub ptt_cancel: Action,
    pub diag: Diag,
}

impl FnHotkeyCallbacks {
    pub fn noop() -> Self {
        let a: Action = Arc::new(|| {});
        Self {
            on_toggle: a.clone(),
            on_stop_send: a.clone(),
            on_cancel: a.clone(),
            on_fn_change: Arc::new(|_| {}),
            ptt_start: a.clone(),
            ptt_stop: a.clone(),
            ptt_stop_send: a.clone(),
            ptt_cancel: a,
            diag: Arc::new(|_, _| {}),
        }
    }
}

struct Shared {
    cb: FnHotkeyCallbacks,
    epoch: Instant,
    enabled: AtomicBool,
    ptt_enabled: AtomicBool,
    dictation_active: AtomicBool,
    ptt_key: AtomicU8,
    ptt_key_down: AtomicBool,
    left_option_down: AtomicBool,
    right_option_down: AtomicBool,
    hold_started_ms: AtomicU64,
    send_pending: AtomicBool,
    suppress_until_up: AtomicBool,
    armed: AtomicBool,
    generation: AtomicU64,
    handoff: PttHandoff,
    fn_down: AtomicBool,
    arm_delay: Duration,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64 + 1
    }

    fn hold_elapsed_ms(&self) -> u64 {
        match self.hold_started_ms.load(Ordering::SeqCst) {
            0 => 0,
            started => self.now_ms().saturating_sub(started),
        }
    }

    fn key(&self) -> PttKey {
        PttKey::from_u8(self.ptt_key.load(Ordering::SeqCst))
    }

    fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        std::thread::spawn(f);
    }

    fn dispatch_end(&self, (end, cause): (PttEnd, Option<PttAbortCause>)) {
        let target = match end {
            PttEnd::Stop => Some(self.cb.ptt_stop.clone()),
            PttEnd::Send => Some(self.cb.ptt_stop_send.clone()),
            PttEnd::Cancel => Some(self.cb.ptt_cancel.clone()),
            PttEnd::None => None,
        };
        let diag = self.cb.diag.clone();
        if let Some(cb) = target {
            self.spawn(move || {
                if let Some(cause) = cause {
                    diag("ptt_abort", &format_abort("macos", cause));
                }
                cb();
            });
        }
    }

    fn request_abort(&self, cause: PttAbortCause) {
        if let Some(end) = self.handoff.request_end(PttEnd::Cancel, Some(cause)) {
            self.dispatch_end(end);
        } else if cause.phase == PttPhase::Arming {
            let diag = self.cb.diag.clone();
            self.spawn(move || diag("ptt_abort", &format_abort("macos", cause)));
        }
    }

    fn reset_ptt(&self) {
        self.ptt_key_down.store(false, Ordering::SeqCst);
        self.hold_started_ms.store(0, Ordering::SeqCst);
        self.armed.store(false, Ordering::SeqCst);
        self.send_pending.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn other_modifiers(&self, keycode: u16, flags: u64) -> bool {
        match self.key() {
            PttKey::LeftOption => {
                flags & (FLAG_SHIFT | FLAG_CONTROL | FLAG_COMMAND) != 0
                    || keycode == KEYCODE_RIGHT_OPTION
                    || self.right_option_down.load(Ordering::SeqCst)
            }
            PttKey::RightOption => {
                flags & (FLAG_SHIFT | FLAG_CONTROL | FLAG_COMMAND) != 0
                    || keycode == KEYCODE_LEFT_OPTION
                    || self.left_option_down.load(Ordering::SeqCst)
            }
            PttKey::Control => flags & (FLAG_SHIFT | FLAG_ALTERNATE | FLAG_COMMAND) != 0,
        }
    }

    fn selected_edge(&self, keycode: u16, flags: u64) -> Option<bool> {
        match (self.key(), keycode) {
            (PttKey::LeftOption, KEYCODE_LEFT_OPTION) => {
                Some(!self.left_option_down.load(Ordering::SeqCst))
            }
            (PttKey::RightOption, KEYCODE_RIGHT_OPTION) => {
                Some(!self.right_option_down.load(Ordering::SeqCst))
            }
            (PttKey::Control, KEYCODE_LEFT_CONTROL | KEYCODE_RIGHT_CONTROL) => {
                Some(flags & FLAG_CONTROL != 0)
            }
            _ => None,
        }
    }
}

fn modifiers_from_flags(flags: u64) -> PttModifiers {
    let mut value = 0;
    for (mask, bit) in [
        (FLAG_CONTROL, PttModifiers::CTRL),
        (FLAG_SHIFT, PttModifiers::SHIFT),
        (FLAG_ALTERNATE, PttModifiers::ALT),
        (FLAG_COMMAND, PttModifiers::COMMAND_OR_WIN),
        (FLAG_SECONDARY_FN, PttModifiers::FN),
    ] {
        if flags & mask != 0 {
            value |= bit;
        }
    }
    PttModifiers(value)
}

fn on_flags_changed(s: &Arc<Shared>, ev: &TapEvent) {
    let flags = ev.flags;
    let fn_now = flags & FLAG_SECONDARY_FN != 0;
    let fn_was = s.fn_down.swap(fn_now, Ordering::SeqCst);
    let other_mods = s.other_modifiers(ev.keycode, flags);

    if fn_was != fn_now {
        if fn_now && s.ptt_enabled.load(Ordering::SeqCst) && s.handoff.is_active() {
            s.send_pending.store(true, Ordering::SeqCst);
        }
        let cb = s.cb.on_fn_change.clone();
        s.spawn(move || cb(fn_now));
    }

    let edge = s.selected_edge(ev.keycode, flags);
    if ev.keycode == KEYCODE_LEFT_OPTION {
        s.left_option_down.fetch_xor(true, Ordering::SeqCst);
    } else if ev.keycode == KEYCODE_RIGHT_OPTION {
        s.right_option_down.fetch_xor(true, Ordering::SeqCst);
    }
    if !s.ptt_enabled.load(Ordering::SeqCst) {
        return;
    }
    let ptt_now = edge.unwrap_or_else(|| s.ptt_key_down.load(Ordering::SeqCst));
    let was_ptt = s.ptt_key_down.swap(ptt_now, Ordering::SeqCst);
    if s.suppress_until_up.load(Ordering::SeqCst) {
        if should_clear_ptt_hold_suppression(ptt_now, other_mods) {
            s.suppress_until_up.store(false, Ordering::SeqCst);
            (s.cb.diag)("ptt_hold_suppression_cleared", "reason=clean_ptt_key_up");
        }
        return;
    }
    if ptt_now && !was_ptt {
        s.send_pending.store(false, Ordering::SeqCst);
        if should_arm_ptt(true, s.dictation_active.load(Ordering::SeqCst), other_mods) {
            s.hold_started_ms.store(s.now_ms(), Ordering::SeqCst);
            s.armed.store(true, Ordering::SeqCst);
            let generation = s.generation.fetch_add(1, Ordering::SeqCst) + 1;
            let s2 = s.clone();
            std::thread::spawn(move || {
                std::thread::sleep(s2.arm_delay);
                if s2.generation.load(Ordering::SeqCst) != generation
                    || !s2.ptt_key_down.load(Ordering::SeqCst)
                    || !s2.armed.load(Ordering::SeqCst)
                    || s2.dictation_active.load(Ordering::SeqCst)
                {
                    s2.armed.store(false, Ordering::SeqCst);
                    return;
                }
                if !s2.handoff.begin_start() {
                    return;
                }
                (s2.cb.ptt_start)();
                let still_valid = s2.generation.load(Ordering::SeqCst) == generation
                    && s2.ptt_key_down.load(Ordering::SeqCst)
                    && s2.armed.load(Ordering::SeqCst);
                if let Some(end) = s2.handoff.complete_start(still_valid) {
                    s2.dispatch_end(end);
                }
            });
        }
    } else if !ptt_now && was_ptt {
        s.hold_started_ms.store(0, Ordering::SeqCst);
        s.generation.fetch_add(1, Ordering::SeqCst);
        s.armed.store(false, Ordering::SeqCst);
        let end = if s.send_pending.swap(false, Ordering::SeqCst) {
            PttEnd::Send
        } else {
            PttEnd::Stop
        };
        if let Some(end) = s.handoff.request_end(end, None) {
            s.dispatch_end(end);
        }
    } else if ptt_now && was_ptt && other_mods && s.armed.load(Ordering::SeqCst) {
        let modifiers = modifiers_from_flags(flags);
        let cause = PttAbortCause {
            kind: PttAbortKind::ModifierJoined,
            key_code: None,
            modifiers,
            joined: PttModifiers(modifiers.0 & !s.key().modifier_flag()),
            phase: s.handoff.phase().unwrap_or(PttPhase::Arming),
            swallowed: false,
        };
        let elapsed = s.hold_elapsed_ms();
        if should_abort_ptt_chord(elapsed) {
            s.hold_started_ms.store(0, Ordering::SeqCst);
            s.armed.store(false, Ordering::SeqCst);
            s.generation.fetch_add(1, Ordering::SeqCst);
            s.request_abort(cause);
        } else {
            (s.cb.diag)(
                "ptt_chord_ignored_late",
                &format!("kind=modifier_joined elapsed_ms={elapsed}"),
            );
        }
    }
}

fn decide(s: &Arc<Shared>, ev: &TapEvent) -> TapDecision {
    match ev.kind {
        TapKind::FlagsChanged => {
            on_flags_changed(s, ev);
            return TapDecision::Pass;
        }
        TapKind::KeyDown | TapKind::KeyUp => {}
        TapKind::OtherMouseDown => return TapDecision::Pass,
    }
    let keycode = ev.keycode;
    let flags = ev.flags;

    if ev.kind == TapKind::KeyDown
        && s.ptt_enabled.load(Ordering::SeqCst)
        && s.armed.load(Ordering::SeqCst)
    {
        let modifiers = modifiers_from_flags(flags);
        let cause = PttAbortCause {
            kind: PttAbortKind::KeyDown,
            key_code: Some(keycode as u32),
            modifiers,
            joined: PttModifiers(modifiers.0 & !s.key().modifier_flag()),
            phase: s.handoff.phase().unwrap_or(PttPhase::Arming),
            swallowed: false,
        };
        let elapsed = s.hold_elapsed_ms();
        if should_abort_ptt_chord(elapsed) {
            s.hold_started_ms.store(0, Ordering::SeqCst);
            s.armed.store(false, Ordering::SeqCst);
            s.generation.fetch_add(1, Ordering::SeqCst);
            s.request_abort(cause);
        } else {
            (s.cb.diag)(
                "ptt_chord_ignored_late",
                &format!("kind=key_down key_code={keycode} elapsed_ms={elapsed}"),
            );
        }
    }

    if keycode != KEYCODE_SPACE && keycode != KEYCODE_ESCAPE {
        return TapDecision::Pass;
    }
    let event_has_fn = flags & FLAG_SECONDARY_FN != 0;
    if !event_has_fn {
        return TapDecision::Pass;
    }
    let enabled = s.enabled.load(Ordering::SeqCst);
    let cached = s.fn_down.load(Ordering::SeqCst);
    if !should_accept_fn_chord(event_has_fn, cached, enabled) {
        if ev.kind == TapKind::KeyDown {
            (s.cb.diag)(
                "fn_input_candidate",
                &format!("disposition=rejected flags={flags:#x} cached_fn_down={cached} enabled={enabled}"),
            );
        }
        return TapDecision::Pass;
    }
    if ev.kind == TapKind::KeyDown {
        let cb = if keycode == KEYCODE_ESCAPE {
            s.cb.on_cancel.clone()
        } else if flags & FLAG_SHIFT != 0 {
            s.cb.on_stop_send.clone()
        } else {
            s.cb.on_toggle.clone()
        };
        s.spawn(move || cb());
    }
    TapDecision::Swallow
}

pub struct FnHotkeyTap {
    shared: Arc<Shared>,
    _tap: EventTap,
}

impl FnHotkeyTap {
    pub fn spawn(location: TapLocation, callbacks: FnHotkeyCallbacks) -> Result<Self, TapError> {
        Self::spawn_with_arm_delay(location, callbacks, Duration::from_millis(PTT_ARM_DELAY_MS))
    }

    pub fn spawn_with_arm_delay(
        location: TapLocation,
        callbacks: FnHotkeyCallbacks,
        arm_delay: Duration,
    ) -> Result<Self, TapError> {
        let shared = Arc::new(Shared {
            cb: callbacks,
            epoch: Instant::now(),
            enabled: AtomicBool::new(true),
            ptt_enabled: AtomicBool::new(false),
            dictation_active: AtomicBool::new(false),
            ptt_key: AtomicU8::new(PttKey::Control as u8),
            ptt_key_down: AtomicBool::new(false),
            left_option_down: AtomicBool::new(false),
            right_option_down: AtomicBool::new(false),
            hold_started_ms: AtomicU64::new(0),
            send_pending: AtomicBool::new(false),
            suppress_until_up: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            handoff: PttHandoff::new(),
            fn_down: AtomicBool::new(false),
            arm_delay,
        });
        let handler_state = shared.clone();
        let tap = EventTap::spawn(
            TapConfig {
                name: "rightkit-fn-tap".into(),
                location,
                listen_only: false,
                kinds: vec![TapKind::KeyDown, TapKind::KeyUp, TapKind::FlagsChanged],
            },
            move |ev| decide(&handler_state, ev),
        )?;
        Ok(Self { shared, _tap: tap })
    }

    /// Gate for the swallowing fn chords (onboarding / shell state turns them off).
    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::SeqCst);
    }

    pub fn set_ptt_key(&self, key: PttKey) {
        let s = &self.shared;
        s.ptt_key.store(key as u8, Ordering::SeqCst);
        s.reset_ptt();
        s.handoff.reset();
        (s.cb.diag)("ptt_key", key.name());
    }

    pub fn set_ptt_enabled(&self, enabled: bool) {
        let s = &self.shared;
        s.ptt_enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            s.suppress_until_up.store(false, Ordering::SeqCst);
            s.reset_ptt();
            if let Some(end) = s.handoff.request_end(PttEnd::Cancel, None) {
                s.dispatch_end(end);
            }
        }
    }

    /// App-level recording state (including recordings started by fn+Space or the pill). PTT must
    /// not arm on top of an existing recording.
    pub fn set_dictation_active(&self, active: bool) {
        let s = &self.shared;
        let was = s.dictation_active.swap(active, Ordering::SeqCst);
        if was && !active && s.ptt_key_down.load(Ordering::SeqCst) {
            s.suppress_until_up.store(true, Ordering::SeqCst);
            s.armed.store(false, Ordering::SeqCst);
            s.hold_started_ms.store(0, Ordering::SeqCst);
            s.handoff.reset();
            s.send_pending.store(false, Ordering::SeqCst);
            s.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}
