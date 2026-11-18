//! Process-wide holders for the macOS fn-chord tap and the middle-mouse tap: they remember the
//! callbacks and settings applied before the tap exists, spawn the tap once everything it needs is
//! registered, and forward later changes to it.
//!
//! Moved out of an app's two thin macOS facades. Product-neutral: the host owns each holder in a
//! `static` and passes its callbacks and its telemetry sink.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::fn_hotkey::{Action, FnAction};
use super::{ax_link_at, FnHotkeyCallbacks, FnHotkeyTap, MiddleMouseTap, PttKey, TapLocation};

/// Host event sink: `(event, message)`.
pub type EventSink = fn(&str, &str);

type ChordSet = (Action, Action, Action, FnAction);
type PttSet = (Action, Action, Action, Action);

fn key_from_u8(v: u8) -> PttKey {
    [PttKey::Control, PttKey::LeftOption, PttKey::RightOption]
        .into_iter()
        .find(|k| *k as u8 == v)
        .unwrap_or(PttKey::Control)
}

pub struct FnHotkeyHolder {
    tap: OnceLock<FnHotkeyTap>,
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
            tap: OnceLock::new(),
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
        *self.chord.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((toggle, stop_send, cancel, fn_change));
    }

    pub fn set_ptt_callbacks(&self, start: Action, stop: Action, stop_send: Action, cancel: Action) {
        *self.ptt.lock().unwrap_or_else(|e| e.into_inner()) = Some((start, stop, stop_send, cancel));
    }

    /// Spawns the tap once both callback sets are registered. Idempotent.
    pub fn start(&self) {
        if self.tap.get().is_some() {
            return;
        }
        let (Some(chord), Some(ptt)) = (
            self.chord.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            self.ptt.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        ) else {
            return;
        };
        let sink = self.event_sink;
        let callbacks = FnHotkeyCallbacks {
            on_toggle: chord.0,
            on_stop_send: chord.1,
            on_cancel: chord.2,
            on_fn_change: chord.3,
            ptt_start: ptt.0,
            ptt_stop: ptt.1,
            ptt_stop_send: ptt.2,
            ptt_cancel: ptt.3,
            diag: Arc::new(move |event: &str, message: &str| sink(event, message)),
        };
        match FnHotkeyTap::spawn(TapLocation::Hid, callbacks) {
            Ok(tap) => {
                tap.set_enabled(self.enabled.load(Ordering::SeqCst));
                tap.set_ptt_key(key_from_u8(self.ptt_key.load(Ordering::SeqCst)));
                tap.set_ptt_enabled(self.ptt_enabled.load(Ordering::SeqCst));
                let _ = self.tap.set(tap);
                (self.event_sink)("fn_hotkey", "tap active");
            }
            Err(err) => (self.event_sink)("fn_hotkey_error", &err.to_string()),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if let Some(t) = self.tap.get() {
            t.set_enabled(enabled);
        }
    }

    pub fn set_ptt_enabled(&self, enabled: bool) {
        self.ptt_enabled.store(enabled, Ordering::SeqCst);
        if let Some(t) = self.tap.get() {
            t.set_ptt_enabled(enabled);
        }
    }

    pub fn set_ptt_key(&self, name: &str) {
        let key = PttKey::parse(name).unwrap_or_else(|| {
            (self.event_sink)(
                "mac_push_to_talk_key_rejected",
                &format!("unknown key '{name}', falling back to ctrl"),
            );
            PttKey::Control
        });
        self.ptt_key.store(key as u8, Ordering::SeqCst);
        if let Some(t) = self.tap.get() {
            t.set_ptt_key(key);
        }
    }

    pub fn set_dictation_active(&self, active: bool) {
        if let Some(t) = self.tap.get() {
            t.set_dictation_active(active);
        }
    }
}

pub struct MiddleMouseHolder {
    tap: OnceLock<MiddleMouseTap>,
    callback: Mutex<Option<Action>>,
    enabled: AtomicBool,
    event_sink: EventSink,
}

impl MiddleMouseHolder {
    pub const fn new(event_sink: EventSink) -> Self {
        Self {
            tap: OnceLock::new(),
            callback: Mutex::new(None),
            enabled: AtomicBool::new(false),
            event_sink,
        }
    }

    pub fn set_toggle_callback(&self, cb: Action) {
        *self.callback.lock().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if let Some(t) = self.tap.get() {
            t.set_enabled(enabled);
        }
    }

    /// Spawns the tap once the toggle callback is registered. Idempotent.
    pub fn spawn(&self) {
        if self.tap.get().is_some() {
            return;
        }
        let Some(cb) = self.callback.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        match MiddleMouseTap::spawn(TapLocation::Hid, move || cb(), ax_link_at) {
            Ok(tap) => {
                tap.set_enabled(self.enabled.load(Ordering::SeqCst));
                let _ = self.tap.set(tap);
                (self.event_sink)("middle_mouse", "tap active");
            }
            Err(err) => (self.event_sink)("middle_mouse_error", &err.to_string()),
        }
    }
}
