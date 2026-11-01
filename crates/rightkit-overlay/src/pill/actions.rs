//! What the native pill asks of the application: dictation actions, hub navigation, settings and
//! diagnostics. Installed once with [`install_actions`]; every method runs off the overlay owner
//! thread's critical path (the driver spawns a thread per click).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The application behind the native pill.
pub trait PillActions: Send + Sync + 'static {
    /// A diagnostic line.
    fn log(&self, _event: &str, _message: &str) {}
    /// A file inside the application data directory.
    fn data_file(&self, name: &str) -> PathBuf;
    /// Generation of the current terminal error, read when the pill arms its expiry timer.
    fn terminal_error_generation(&self) -> Option<u64> {
        None
    }
    /// Show the pill after launch (the driver asks when the first frame is ready).
    fn reveal_pill(&self) {}
    /// Reset a terminal error whose expiry timer fired.
    fn recover_terminal_error(&self, generation: Option<u64>) -> Result<(), String>;
    /// Release the app-side holds taken while the clipboard card is open.
    fn reset_clipboard_state(&self) {}
    /// Open the hub at `route`. The application runs this on its main thread.
    fn open_hub(&self, route: &'static str) -> Result<(), String>;
    /// Start dictation from the pill.
    fn start_dictation(&self) -> Result<(), String>;
    /// Stop dictation from the pill; `send_enter` also presses Enter after the paste.
    fn stop_dictation(&self, send_enter: bool) -> Result<(), String>;
    /// Cancel dictation from the pill's cancel button.
    fn cancel_dictation(&self) -> Result<(), String>;
    /// Toggle the recent-items clipboard card.
    fn toggle_clipboard(&self) -> Result<(), String>;
    /// Copy text to the system clipboard.
    fn copy_text(&self, text: String) -> Result<(), String>;
    /// Apply a settings patch.
    fn apply_settings_patch(&self, patch: serde_json::Value) -> Result<(), String>;
    /// Edit a history item's transcript from the clipboard card (macOS bridge).
    fn update_history_item(&self, _delivery_id: String, _transcript: String) -> Result<(), String> {
        Err("history edit is not supported".to_string())
    }
    /// Close the native clipboard card without touching the renderer (macOS bridge).
    fn dismiss_clipboard_native(&self) {}
    /// Close the native hint card (macOS bridge).
    fn dismiss_hint_native(&self) {}
    /// Stop dictation and send Enter after the paste.
    fn stop_and_send(&self) -> Result<(), String> {
        self.stop_dictation(true)
    }
    /// Refresh the items shown by the native clipboard card (macOS bridge).
    fn refresh_clipboard_items(&self) -> Result<(), String> {
        Ok(())
    }
    /// True once the engine is warm and the pill is live.
    fn launch_finished(&self) -> bool {
        true
    }
    /// Reveal the pill, keeping whatever it currently shows (QA launches without a splash).
    fn reveal_pill_preserving_active(&self) {}
    /// Whether the launch splash has waited long enough to offer recovery actions.
    fn splash_is_stalled(&self, _waited_secs: u64) -> bool {
        false
    }
    /// Retry the launch after a stalled splash.
    fn splash_retry(&self) {}
    /// Quit from a stalled splash.
    fn splash_quit(&self) {}
    /// Copy the diagnostics text to the clipboard.
    fn splash_copy_diagnostics(&self) -> Result<(), String> {
        Ok(())
    }
    /// Reveal the exported diagnostics.
    fn splash_reveal_diagnostics(&self) -> Result<(), String> {
        Ok(())
    }
    /// The pill capsule's hitbox and physical window frame changed (physical pixels).
    fn publish_pill_geometry(&self, _hitbox: (f64, f64, f64, f64), _frame: (f64, f64, f64)) {}
}

static ACTIONS: OnceLock<Arc<dyn PillActions>> = OnceLock::new();

/// Install the application once at startup. Returns false when one is already installed.
pub fn install_actions(actions: Arc<dyn PillActions>) -> bool {
    ACTIONS.set(actions).is_ok()
}

/// The installed actions, if any.
pub fn actions() -> Option<&'static dyn PillActions> {
    ACTIONS.get().map(|actions| actions.as_ref())
}

/// A diagnostic line through the installed actions; dropped when none are installed.
pub fn log_message(event: &str, message: &str) {
    if let Some(actions) = actions() {
        actions.log(event, message);
    }
}

/// Write `bytes` to `path` through a sibling temp file and rename.
pub fn replace_file_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
