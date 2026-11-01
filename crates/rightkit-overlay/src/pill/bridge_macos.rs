//! C ABI boundary for the macOS-native pill and splash surfaces.
//!
//! Swift owns all AppKit/SwiftUI state.  The exported Swift entry points enqueue
//! their work onto the main queue and return; every callback below starts Rust
//! work on a background thread so a panel action can never block AppKit.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::actions::{actions, log_message};
use super::PillEvent;

/// Redundant, Rust-side backstop behind the Swift-side deadline and watchdog
/// in `SplashPanelController`. If the FFI answer ever reports the splash
/// handoff as active for longer than this, Rust terminates the native handoff
/// before treating it as over — but ONLY once the engine is actually warm
/// (`launch_finished()` / `FINISH_LAUNCH_DONE`). Forcing completion before the
/// engine is ready is exactly the 0.1.152 regression: the splash was torn
/// down and the pill shown ~21s before the engine could serve a dictation
/// request, silently discarding the user's first speech. Deferred pill
/// reveal/resize/visibility calls are therefore never alternated around a
/// stale native phase once this backstop is allowed to fire.
const SPLASH_HANDOFF_MAX_ACTIVE: Duration = Duration::from_millis(3000);
static SPLASH_HANDOFF_FIRST_SEEN_ACTIVE: Mutex<Option<Instant>> = Mutex::new(None);
// A forced handoff is terminal for this launch. Keep this latch separate from
// the elapsed-time marker so a stale native phase cannot alternate between
// deferred and allowed on each later mutation.
static SPLASH_HANDOFF_FORCED: Mutex<bool> = Mutex::new(false);

#[repr(C)]
#[derive(Clone, Copy)]
struct HrPillCallbacks {
    copy_text: Option<unsafe extern "C" fn(*const c_char)>,
    update_history_item: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    open_hub: Option<unsafe extern "C" fn(c_int)>,
    dismiss_clipboard: Option<unsafe extern "C" fn()>,
    dismiss_hint: Option<unsafe extern "C" fn()>,
    set_screenshot_destination: Option<unsafe extern "C" fn(c_int)>,
    log: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    stop_recording: Option<unsafe extern "C" fn()>,
    cancel_processing: Option<unsafe extern "C" fn()>,
    start_dictation: Option<unsafe extern "C" fn()>,
    stop_and_send: Option<unsafe extern "C" fn()>,
    request_clipboard_items: Option<unsafe extern "C" fn()>,
    recover_terminal_error: Option<unsafe extern "C" fn(i64)>,
    terminal_error_generation: Option<unsafe extern "C" fn() -> i64>,
    splash_stalled: Option<unsafe extern "C" fn(i32) -> i32>,
    splash_action: Option<unsafe extern "C" fn(c_int)>,
}

unsafe extern "C" {
    fn hr_pill_init();
    fn hr_pill_shutdown();
    fn hr_pill_push_event(kind: c_int, level: f64, cancellable: bool, label: *const c_char);
    fn hr_pill_reveal();
    fn hr_pill_set_visible(visible: bool);
    fn hr_pill_is_visible() -> bool;
    fn hr_pill_refresh_action_sound_settings();
    fn hr_pill_splash_handoff_active() -> bool;
    fn hr_pill_splash_handoff_force_complete();
    fn hr_pill_set_clipboard_items(items_json: *const c_char, mode: c_int, loaded: bool);
    fn hr_pill_close_clipboard();
    fn hr_pill_set_hint(view: c_int);
    fn hr_splash_show();
    fn hr_splash_dismiss();
    fn hr_splash_fly_into_pill();
    fn hr_hub_animate_from_pill(window: *mut c_void);
    fn hr_hub_animate_to_pill(window: *mut c_void);
    fn hr_pill_register_callbacks(callbacks: HrPillCallbacks);
}

fn c_string(value: &str) -> CString {
    // The UI text originates in Rust strings, but C strings cannot represent a
    // NUL byte. Replacing it preserves the surrounding user-visible text.
    CString::new(value).unwrap_or_else(|_| CString::new(value.replace('\0', "�")).unwrap())
}

unsafe fn string_from_c(value: *const c_char) -> String {
    if value.is_null() {
        return String::new();
    }
    // Swift gives Rust a pointer valid for the duration of the callback; copy
    // before moving any work to a background thread.
    unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned()
}

pub fn initialize() {
    unsafe {
        hr_pill_register_callbacks(HrPillCallbacks {
            copy_text: Some(hr_pill_copy_text),
            update_history_item: Some(hr_pill_update_history_item),
            open_hub: Some(hr_pill_open_hub),
            dismiss_clipboard: Some(hr_pill_dismiss_clipboard),
            dismiss_hint: Some(hr_pill_dismiss_hint),
            set_screenshot_destination: Some(hr_pill_set_screenshot_destination),
            log: Some(hr_pill_log),
            stop_recording: Some(hr_pill_stop_recording),
            cancel_processing: Some(hr_pill_cancel_processing),
            start_dictation: Some(hr_pill_start_dictation),
            stop_and_send: Some(hr_pill_stop_and_send),
            request_clipboard_items: Some(hr_pill_request_clipboard_items),
            recover_terminal_error: Some(hr_pill_recover_terminal_error),
            terminal_error_generation: Some(hr_pill_terminal_error_generation),
            splash_stalled: Some(hr_pill_splash_stalled),
            splash_action: Some(hr_pill_splash_action),
        });
        hr_pill_init();
    }
    log_message("pill_native_bridge", "initialized");
}

pub fn shutdown() {
    unsafe { hr_pill_shutdown() };
}

pub fn push_event(event: &PillEvent) {
    let (kind, level, cancellable, label) = match event {
        PillEvent::Hide => (0, 0.0, false, None),
        PillEvent::Idle => (1, 0.0, false, None),
        // The React state machine renders `starting` as the recording mode.
        PillEvent::Starting | PillEvent::Recording => (2, 0.0, false, None),
        PillEvent::Voice { level } => (3, *level, false, None),
        PillEvent::Processing { cancellable } => (4, 0.0, *cancellable, None),
        PillEvent::Success => (5, 0.0, false, None),
        PillEvent::Status { label } => (6, 0.0, false, Some(label.as_str())),
        PillEvent::Error { label } => (7, 0.0, false, Some(label.as_str())),
    };
    let label = label.map(c_string);
    unsafe {
        hr_pill_push_event(
            kind,
            f64::from(level.clamp(0.0, 1.0)),
            cancellable,
            label
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
        );
    }
}

pub fn reveal() {
    unsafe { hr_pill_reveal() };
}

pub fn set_visible(visible: bool) {
    unsafe { hr_pill_set_visible(visible) };
}

pub fn is_visible() -> bool {
    unsafe { hr_pill_is_visible() }
}

pub fn refresh_action_sound_settings() {
    unsafe { hr_pill_refresh_action_sound_settings() };
}

pub fn splash_handoff_active() -> bool {
    if SPLASH_HANDOFF_FORCED
        .lock()
        .map(|forced| *forced)
        .unwrap_or(false)
    {
        return false;
    }
    let native_active = unsafe { hr_pill_splash_handoff_active() };
    let Ok(mut first_seen) = SPLASH_HANDOFF_FIRST_SEEN_ACTIVE.lock() else {
        return native_active;
    };
    if !native_active {
        *first_seen = None;
        return false;
    }
    let started_at = *first_seen.get_or_insert_with(Instant::now);
    if started_at.elapsed() > SPLASH_HANDOFF_MAX_ACTIVE {
        if !actions().is_none_or(|a| a.launch_finished()) {
            // Engine is not warm yet: the deadline elapsing is expected (ANE
            // compile can take ~30s) and must NOT terminate the handoff early.
            // Keep reporting active so the splash stays up and dictation
            // hotkeys stay gated until the engine really is ready.
            return true;
        }
        *first_seen = None;
        if let Ok(mut forced) = SPLASH_HANDOFF_FORCED.lock() {
            *forced = true;
        }
        drop(first_seen);
        unsafe { hr_pill_splash_handoff_force_complete() };
        log_message(
            "splash_lifecycle",
            "phase=handoff_forced reason=rust_deadline",
        );
        return false;
    }
    true
}

pub fn set_clipboard_items(items_json: &str, mode: i32, loaded: bool) {
    let items = c_string(items_json);
    unsafe { hr_pill_set_clipboard_items(items.as_ptr(), mode, loaded) };
}

pub fn close_clipboard() {
    unsafe { hr_pill_close_clipboard() };
}

pub fn set_hint(view: i32) {
    unsafe { hr_pill_set_hint(view) };
}

pub fn splash_show() {
    if let Ok(mut forced) = SPLASH_HANDOFF_FORCED.lock() {
        *forced = false;
    }
    if let Ok(mut first_seen) = SPLASH_HANDOFF_FIRST_SEEN_ACTIVE.lock() {
        *first_seen = None;
    }
    unsafe { hr_splash_show() };
}

pub fn splash_dismiss() {
    unsafe { hr_splash_dismiss() };
}

/// A hidden QA launch shows no splash (`hub_window::show_splash`), so the native handoff never
/// starts and has no flight to finish: `flyIntoPill` bails on its missing panel and the pill would
/// never reveal. The first reveal goes direct instead.
#[cfg(feature = "qa-native")]
static SPLASH_SKIPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "qa-native")]
pub fn note_splash_skipped() {
    SPLASH_SKIPPED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn splash_fly_into_pill() {
    #[cfg(feature = "qa-native")]
    if SPLASH_SKIPPED.load(std::sync::atomic::Ordering::SeqCst) {
        if let Some(actions) = actions() {
            actions.reveal_pill_preserving_active();
        }
        return;
    }
    unsafe { hr_splash_fly_into_pill() };
}

pub fn animate_hub_from_pill(window: *mut c_void) {
    unsafe { hr_hub_animate_from_pill(window) };
}

pub fn animate_hub_to_pill(window: *mut c_void) {
    unsafe { hr_hub_animate_to_pill(window) };
}

fn spawn_action(f: impl FnOnce(&'static dyn super::actions::PillActions) + Send + 'static) {
    std::thread::spawn(move || {
        if let Some(actions) = actions() {
            f(actions);
        }
    });
}

unsafe extern "C" fn hr_pill_copy_text(text: *const c_char) {
    let text = unsafe { string_from_c(text) };
    spawn_action(move |actions| {
        if let Err(error) = actions.copy_text(text) {
            log_message("pill_native_copy_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_update_history_item(
    delivery_id: *const c_char,
    transcript: *const c_char,
) {
    let delivery_id = unsafe { string_from_c(delivery_id) };
    let transcript = unsafe { string_from_c(transcript) };
    spawn_action(move |actions| {
        if let Err(error) = actions.update_history_item(delivery_id, transcript) {
            log_message("pill_native_history_update_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_open_hub(tab: c_int) {
    let route = if tab == 1 { "history" } else { "home" };
    log_message(
        "pill_native_action",
        &format!("action=open_hub route={route} phase=callback_received"),
    );
    spawn_action(move |actions| {
        let started = std::time::Instant::now();
        let outcome = actions.open_hub(route);
        log_message(
            "pill_native_action",
            &format!(
                "action=open_hub route={route} phase=result outcome={} elapsed_ms={}",
                if outcome.is_ok() { "ok" } else { "error" },
                started.elapsed().as_millis()
            ),
        );
    });
}

unsafe extern "C" fn hr_pill_dismiss_clipboard() {
    spawn_action(|actions| actions.dismiss_clipboard_native());
}

unsafe extern "C" fn hr_pill_dismiss_hint() {
    spawn_action(|actions| actions.dismiss_hint_native());
}

unsafe extern "C" fn hr_pill_set_screenshot_destination(destination: c_int) {
    spawn_action(move |actions| {
        let destination = if destination == 1 {
            "disk"
        } else {
            "clipboard"
        };
        let patch = serde_json::json!({
            "screenshot_destination": destination,
            "screenshot_prompt_selected": true,
        });
        if let Err(error) = actions.apply_settings_patch(patch) {
            log_message("pill_native_screenshot_destination_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_log(surface: *const c_char, message: *const c_char) {
    let surface = unsafe { string_from_c(surface) };
    let message = unsafe { string_from_c(message) };
    log_message(&surface, &message);
}

unsafe extern "C" fn hr_pill_stop_recording() {
    spawn_action(|actions| {
        if let Err(error) = actions.stop_dictation(false) {
            log_message("pill_native_stop_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_start_dictation() {
    spawn_action(|actions| {
        if let Err(error) = actions.start_dictation() {
            log_message("pill_native_start_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_stop_and_send() {
    spawn_action(|actions| {
        if let Err(error) = actions.stop_and_send() {
            log_message("pill_native_stop_send_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_cancel_processing() {
    log_message(
        "cancel_input_candidate",
        "source=pill_cancel_button phase=callback_received",
    );
    spawn_action(|actions| {
        if let Err(error) = actions.cancel_dictation() {
            log_message("pill_native_cancel_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_request_clipboard_items() {
    log_message(
        "pill_native_action",
        "action=open_clipboard phase=callback_received",
    );
    spawn_action(|actions| {
        let started = std::time::Instant::now();
        if let Err(error) = actions.refresh_clipboard_items() {
            log_message("pill_native_clipboard_refresh_error", &error);
        } else {
            log_message(
                "pill_native_action",
                &format!(
                    "action=open_clipboard phase=result outcome=ok elapsed_ms={}",
                    started.elapsed().as_millis()
                ),
            );
        }
    });
}

unsafe extern "C" fn hr_pill_terminal_error_generation() -> i64 {
    actions()
        .and_then(|a| a.terminal_error_generation())
        .map_or(-1, |g| g as i64)
}

unsafe extern "C" fn hr_pill_splash_stalled(waited_secs: i32) -> i32 {
    i32::from(actions().is_some_and(|a| a.splash_is_stalled(waited_secs.max(0) as u64)))
}

/// 0 retry, 1 quit, 2 copy diagnostics, 3 reveal diagnostics.
unsafe extern "C" fn hr_pill_splash_action(action: c_int) {
    spawn_action(move |actions| {
        let result = match action {
            0 => {
                actions.splash_retry();
                Ok(())
            }
            1 => {
                actions.splash_quit();
                Ok(())
            }
            2 => actions.splash_copy_diagnostics(),
            3 => actions.splash_reveal_diagnostics(),
            _ => Ok(()),
        };
        if let Err(error) = result {
            log_message("splash_stall_action_error", &error);
        }
    });
}

unsafe extern "C" fn hr_pill_recover_terminal_error(generation: i64) {
    let generation = u64::try_from(generation).ok();
    spawn_action(move |actions| {
        if let Err(error) = actions.recover_terminal_error(generation) {
            log_message("pill_native_terminal_reset_rejected", &error);
        }
    });
}
