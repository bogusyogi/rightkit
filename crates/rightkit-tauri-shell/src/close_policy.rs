//! What closing a window does. Moved from host `pill_overlays.rs`
//! (`should_hide_window_on_close_for_behavior`, `should_quit_app_on_close_for_behavior`) and the
//! branch order of `app_run.rs` `handle_window_event`. The host supplies the main window's label,
//! the stored close preference's values and whether the main window hides to its tray/pill on
//! close regardless of the preference (host: Windows and macOS).

/// What a close request on `label` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecision {
    /// Prevent the close and quit the app (the host runs its shutdown steps first).
    Quit,
    /// Prevent the close; the main window stays warm and hides itself (the host runs its
    /// hide-to-tray transition).
    MainToTray,
    /// Prevent the close and hide the window; the host suspends its rendering.
    Hide,
    /// Let the close proceed.
    Allow,
}

/// The values of the stored close preference.
#[derive(Debug, Clone, Copy)]
pub struct CloseValues<'a> {
    /// Preference value that quits when the main window closes.
    pub quit: &'a str,
    /// Preference value that hides the main window when it closes.
    pub minimize: &'a str,
}

/// Decide a close request. Only the main window quits or minimizes; every other window closes.
/// Order is the old one: quit first, then the unconditional main-window hide-to-tray, then the
/// preference-driven hide.
pub fn decide(
    label: &str,
    main_label: &str,
    close_behavior: &str,
    values: &CloseValues<'_>,
    main_hides_to_tray: bool,
) -> CloseDecision {
    if label != main_label {
        return CloseDecision::Allow;
    }
    if close_behavior == values.quit {
        return CloseDecision::Quit;
    }
    if main_hides_to_tray {
        return CloseDecision::MainToTray;
    }
    if close_behavior == values.minimize {
        return CloseDecision::Hide;
    }
    CloseDecision::Allow
}

/// The host's reactions to a close request, for [`handle_close_requested`]. Each runs after the
/// close has been prevented (every case but `Allow`).
pub struct CloseHandlers<'a, R: tauri::Runtime> {
    /// Quit: the host runs its shutdown steps and exits the app.
    pub quit: &'a dyn Fn(&tauri::Window<R>),
    /// The main window hides to its tray/pill (the host runs its transition).
    pub main_to_tray: &'a dyn Fn(&tauri::Window<R>),
    /// Hide the window and suspend its rendering. The window is hidden before this runs.
    pub hide: &'a dyn Fn(&tauri::Window<R>),
}

/// Decide a close request for `window` with [`decide`], prevent the close when it must not
/// proceed, and run the matching handler. Returns the decision. `Hide` hides the window itself
/// before calling `handlers.hide`; `Allow` runs nothing and lets the close proceed.
pub fn handle_close_requested<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    api: &tauri::CloseRequestApi,
    main_label: &str,
    close_behavior: &str,
    values: &CloseValues<'_>,
    main_hides_to_tray: bool,
    handlers: &CloseHandlers<'_, R>,
) -> CloseDecision {
    let decision = decide(
        window.label(),
        main_label,
        close_behavior,
        values,
        main_hides_to_tray,
    );
    match decision {
        CloseDecision::Quit => {
            api.prevent_close();
            (handlers.quit)(window);
        }
        CloseDecision::MainToTray => {
            api.prevent_close();
            (handlers.main_to_tray)(window);
        }
        CloseDecision::Hide => {
            api.prevent_close();
            let _ = window.hide();
            (handlers.hide)(window);
        }
        CloseDecision::Allow => {}
    }
    decision
}
