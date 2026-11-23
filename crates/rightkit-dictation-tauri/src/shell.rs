//! The per-app seam: the few operations of a dictation shell that need the application handle
//! itself (the renderer, the tray and menu bar mirrors, the wake lab, hints, the launch reveal).
//!
//! Process-wide values (settings, gates, pill looks, platform hooks) are on
//! [`crate::host::DictationHost`]. Everything here is held by one object per application handle,
//! which the embedding shell creates and passes to the entry points of [`crate::lifecycle`],
//! [`crate::engine_events`], [`crate::dictation_control`] and [`crate::hotkey_wiring`]. The package
//! has no Tauri dependency: the shell's object owns the handle.

use std::sync::Arc;

use rightkit_dictation_core::state::AppState;

/// A look the runtime asks the pill to show. The shell maps each to its own pill event and owns
/// every label; the runtime never names a string the user reads.
#[derive(Debug, Clone, PartialEq)]
pub enum PillCue {
    /// Recording look, no spinner.
    Recording,
    /// Processing look; never cancellable from this cue.
    Processing,
    /// Live input level, 0.0 to 1.0.
    Voice { level: f32 },
    /// Text was inserted.
    Pasted,
    /// A voice command ran. `action` and `detail` are the engine's own words, not labels.
    CommandSucceeded {
        action: String,
        detail: Option<String>,
    },
    /// A recognised voice command failed to dispatch.
    CommandFailed {
        /// The failure names Accessibility access.
        accessibility_denied: bool,
    },
    /// A start or stop failed; `message` is the error text the shell maps to a label.
    Error { message: String },
}

/// An event for the renderer. The shell owns the event names and routing; each variant carries the
/// payload the runtime built.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    /// The wake word fired.
    WakeFired(serde_json::Value),
    /// A recording input level for the onboarding meter.
    RecordingLevel(serde_json::Value),
    /// A delivery was recorded in history.
    DeliveryRecorded(serde_json::Value),
    /// A shortcut was pressed during the onboarding shortcut test (`{"kind": ...}`).
    OnboardingShortcut(serde_json::Value),
    /// The fn key changed during the onboarding chord test (`{"down": bool}`).
    OnboardingFn(serde_json::Value),
    /// The middle mouse button was clicked during the onboarding control test.
    OnboardingMiddleClick,
    /// Push-to-talk was pressed or released during the onboarding control test. Raised from an
    /// input-hook thread: the shell delivers it where the renderer must be addressed (it may hop
    /// threads), maps it to its own event name and payload, and
    /// calls `complete` exactly once with the actual emit result. `emit_event` itself returns only
    /// whether the delivery was scheduled; a delivery is reported by `complete`, never by that
    /// `Ok`.
    OnboardingPtt {
        down: bool,
        complete: OnboardingPttCompletion,
    },
}

/// Called by the shell with the push-to-talk direction and the actual result of delivering the
/// onboarding preview to the renderer. Compares equal to any other completion (a function pointer
/// has no stable identity).
#[derive(Clone, Copy)]
pub struct OnboardingPttCompletion(pub fn(down: bool, result: Result<(), String>));

impl std::fmt::Debug for OnboardingPttCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnboardingPttCompletion")
    }
}

impl PartialEq for OnboardingPttCompletion {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl OnboardingPttCompletion {
    /// Report the delivery result.
    pub fn call(self, down: bool, result: Result<(), String>) {
        (self.0)(down, result)
    }
}

/// Operations that need the application handle.
pub trait DictationShell: Send + Sync + 'static {
    /// Mirror a committed state to every surface the shell owns besides the pill: the renderer's
    /// state event, the tray icon, the menu bar helper, the recording hooks.
    fn emit_state_mirrors(&self, state: &AppState);
    /// Deliver an event to the renderer.
    fn emit_event(&self, event: ShellEvent) -> Result<(), String>;
    /// Offer the clipboard-recovery popup for a transcript that is on the clipboard without a
    /// confirmed insertion.
    fn show_clipboard_recovery(&self, transcript: &str) -> Result<(), String>;
    /// A transcript was inserted; the shell may show a one-time hint.
    fn note_completed_transcript(&self);
    /// A cancel just finalised to history; the shell may show the one-time cancel hint.
    fn maybe_show_cancel_saved_hint(&self);
    /// A screenshot voice command ran; `detail` is the engine's detail for it.
    fn show_screenshot_notice(&self, detail: Option<&str>);
    /// The engine is warm: reveal the pill and finish the launch sequence. May be called again
    /// after an engine restart.
    fn finish_launch(&self);
    /// A start failed; the shell may record it (Windows microphone-off state).
    #[cfg(target_os = "windows")]
    fn note_start_error(&self, error: &str);
    /// Relaunch via the app's platform-specific helper.
    fn relaunch_helper(&self, reason: &str) -> Result<(), String>;
    /// Restart through the application's host runtime.
    fn restart_app(&self);
    /// Terminate the application with the given exit code.
    fn exit_app(&self, code: i32);
}

/// A shared handle to the shell's object, cloned into callbacks and worker threads.
pub type Shell = Arc<dyn DictationShell>;
