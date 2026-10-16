//! Startup / shutdown order and RunEvent routing.
//!
//! Startup (plugin `setup`, then Tauri creates windows):
//!
//! 1. single-instance arbitration (a second launch forwards argv and exits before anything heavy)
//! 2. shared state managed, 3. native menu, 4. tray
//! 5. engine started off the UI thread (never blocks first paint), 6. watchdog
//! 7. each webview becomes ready: native hardening, saved geometry restored, window shown
//!    (unless launched with `--background`)
//! 8. deep links queued from argv are delivered when the frontend calls `rk_shell_ready`
//!
//! Shutdown (`quit` -> `ExitRequested` -> `Exit`): mark exiting (new engine calls refused), save
//! window geometry, stop the engine (bounded), remove the tray, release the single-instance
//! service.
//!
//! Donors: HeardRight `tray.rs` Quit (engine shutdown before exit), `hub_window` (close hides,
//! Dock reopen restores), ViewRight `RunEvent::Reopen`, CodeRight `window_state` save hooks.

use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, WindowEvent, Wry};

use crate::quit::QuitStep;
use crate::{ShellState, EVENT_DEEP_LINK, EVENT_MENU, EVENT_QUIT_REQUESTED, EVENT_SECOND_INSTANCE};

/// True when this launch asked to stay in the tray (`--background`).
pub fn launched_in_background() -> bool {
    std::env::args().any(|a| a == crate::autostart::BACKGROUND_ARG)
}

/// QA-only launch policy. The harness sets this before the frontend exists so every
/// shell-owned show/reopen path can preserve the user's foreground app.
pub fn qa_hidden() -> bool {
    std::env::var_os("RIGHTKIT_QA_HIDDEN").as_deref() == Some(std::ffi::OsStr::new("1"))
}

pub fn qa_background() -> bool {
    std::env::var_os("RIGHTKIT_QA_BACKGROUND").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// Why the shell is bringing a window up. Every source asks the same guard ([`reveal_mode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevealSource {
    /// macOS Dock reopen (`RunEvent::Reopen`).
    Reopen,
    /// The OS handed the app a URL or file (`RunEvent::Opened`).
    Opened,
    /// A second launch forwarded to the primary.
    SecondInstance,
    /// The main webview became ready.
    ShowOnReady,
    /// Tray menu "Show" or a tray click.
    Tray,
    /// The `rk_shell_window` `show` command from the renderer.
    WindowCommand,
    /// `windows::open` (create or re-reveal a secondary window).
    OpenWindow,
    /// `WindowStateStore::restore_with` visibility options.
    Restore,
    /// A host call to `windows::reveal`.
    Api,
}

/// What the guard allows for one reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealMode {
    /// Nothing: no show, no unminimise, no focus, no activation.
    Forbidden,
    /// QA background: native order-back only; never unminimise or focus.
    Background,
    /// Ordinary production reveal.
    Foreground,
}

/// The documented truthy values for a never-show environment variable, compared exactly
/// (ASCII case-insensitive, no trimming).
pub const NEVER_SHOW_TRUTHY: [&str; 4] = ["1", "true", "yes", "on"];

/// True when an environment value means "never show": exactly one of [`NEVER_SHOW_TRUTHY`]
/// (`1`, `true`, `yes`, `on`), ASCII case-insensitive. Anything else (unset, empty, `0`, padded or
/// unrecognised text) is off.
pub fn env_means_never_show(value: Option<&OsStr>) -> bool {
    let Some(value) = value.and_then(|v| v.to_str()) else {
        return false;
    };
    NEVER_SHOW_TRUTHY
        .iter()
        .any(|truthy| value.eq_ignore_ascii_case(truthy))
}

/// True when the value is exactly `expected` (case-sensitive, no trimming).
pub fn env_equals(value: Option<&OsStr>, expected: &str) -> bool {
    value.is_some_and(|v| v == OsStr::new(expected))
}

/// The host's say over window visibility, evaluated by [`ShowPolicy::mode`].
#[derive(Debug, Clone)]
pub struct ShowPolicy {
    /// Host-declared: the shell never shows, unminimises or focuses a window, and on macOS the app
    /// is a Prohibited-activation app.
    pub never_show: bool,
    /// Extra environment variables that mean never-show when set to a truthy value (see [`env_means_never_show`]).
    pub never_show_env: Vec<String>,
    /// Environment variables that mean never-show only when set to exactly the paired value
    /// (see [`env_equals`]), for a harness whose variable uses its own token.
    pub never_show_env_values: Vec<(String, String)>,
    /// Whether `Reopen` and `Opened` may reveal at all (also needs the module).
    pub reveal_on_reopen: bool,
}

impl Default for ShowPolicy {
    fn default() -> Self {
        Self {
            never_show: false,
            never_show_env: Vec::new(),
            never_show_env_values: Vec::new(),
            reveal_on_reopen: true,
        }
    }
}

impl ShowPolicy {
    /// True when never-show is declared by the host or by any registered environment variable.
    pub fn is_never_show(&self, env: &dyn Fn(&str) -> Option<OsString>) -> bool {
        self.never_show
            || self
                .never_show_env
                .iter()
                .any(|name| env_means_never_show(env(name).as_deref()))
            || self
                .never_show_env_values
                .iter()
                .any(|(name, expected)| env_equals(env(name).as_deref(), expected))
    }

    /// [`ShowPolicy::is_never_show`] against the process environment.
    pub fn never_show_now(&self) -> bool {
        self.is_never_show(&|name: &str| std::env::var_os(name))
    }

    /// The one decision. Never-show beats everything; the reopen opt-out beats QA mode.
    pub fn mode(
        &self,
        source: RevealSource,
        qa_background: bool,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> RevealMode {
        if self.is_never_show(env) {
            return RevealMode::Forbidden;
        }
        if matches!(source, RevealSource::Reopen | RevealSource::Opened) && !self.reveal_on_reopen {
            return RevealMode::Forbidden;
        }
        if qa_background {
            RevealMode::Background
        } else {
            RevealMode::Foreground
        }
    }

    /// [`ShowPolicy::mode`] against the process environment and `RIGHTKIT_QA_BACKGROUND`.
    pub fn mode_now(&self, source: RevealSource) -> RevealMode {
        self.mode(source, qa_background(), &|name: &str| {
            std::env::var_os(name)
        })
    }
}

/// THE guard. Every path that shows, unminimises or focuses a window (reopen, opened, second
/// instance, show-on-ready, tray, the window command, `windows::open`, `windows::reveal`, restore)
/// asks this function and does exactly what the answer allows. Without `ShellState` (a standalone
/// helper outside the plugin) the default policy applies.
pub fn reveal_mode<R: Runtime>(app: &AppHandle<R>, source: RevealSource) -> RevealMode {
    match app.try_state::<ShellState>() {
        Some(state) => state.show_policy.mode_now(source),
        None => ShowPolicy::default().mode_now(source),
    }
}

/// True when the host forbade showing windows (builder flag or registered environment variable).
pub fn never_show<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<ShellState>()
        .is_some_and(|state| state.show_policy.never_show_now())
}

/// A Dock reopen with no visible window is the only reopen that asks to reveal.
pub fn reopen_wants_reveal(has_visible_windows: bool) -> bool {
    !has_visible_windows
}

/// macOS: make the app Prohibited (no Dock icon, no menu bar, never frontmost) when never-show
/// applies. Idempotent; called at plugin setup and again when each webview is ready.
#[cfg(target_os = "macos")]
pub(crate) fn enforce_never_show(app: &AppHandle<Wry>, policy: &ShowPolicy) {
    if policy.never_show_now() {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn enforce_never_show(_app: &AppHandle<Wry>, _policy: &ShowPolicy) {}

/// How a launch takes part in single-instance arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceMode {
    /// The host turned single instance off: no claim, no forward.
    Disabled,
    /// Claim the primary, or forward to it and exit (the default).
    Arbitrate,
    /// A hidden QA launch (`RIGHTKIT_QA_HIDDEN=1`) of an app that opted in with
    /// `qa_instance_exempt`: it neither claims the primary nor forwards to the running instance, so
    /// it can never be revealed by, or reveal, the user's own copy.
    QaExempt,
}

/// Pure decision for [`InstanceMode`].
pub fn instance_mode(single_instance: bool, qa_exempt: bool, qa_hidden_env: bool) -> InstanceMode {
    match (single_instance, qa_exempt && qa_hidden_env) {
        (false, _) => InstanceMode::Disabled,
        (true, true) => InstanceMode::QaExempt,
        (true, false) => InstanceMode::Arbitrate,
    }
}

/// Whether the main window is shown when its webview is ready.
///
/// `explicit` is what the host passed to `show_on_ready` (`None`: never called). An explicit choice
/// is the host's visibility decision, so `RIGHTKIT_QA_HIDDEN` only decides when the host said
/// nothing. A login launch (`--background`) and a disabled module always keep the window hidden.
pub fn show_on_ready_decision(
    explicit: Option<bool>,
    module_enabled: bool,
    background_arg: bool,
    qa_hidden_env: bool,
) -> bool {
    if !module_enabled || background_arg {
        return false;
    }
    explicit.unwrap_or(!qa_hidden_env)
}

/// Whether the `ShowOnReady` behaviour is installed. `lifecycle_only` turns the module off, and
/// only the host's explicit `show_on_ready(true)` brings it back; every other module set is
/// decided by the module alone.
pub fn show_on_ready_installed(
    module_in_set: bool,
    lifecycle_only: bool,
    explicit: Option<bool>,
) -> bool {
    module_in_set || (lifecycle_only && explicit == Some(true))
}

/// Whether the startup cache repair runs: the host's explicit choice, else on, except under
/// `lifecycle_only` where the host owns its cache.
pub fn repair_cache_enabled(explicit: Option<bool>, lifecycle_only: bool) -> bool {
    explicit.unwrap_or(!lifecycle_only)
}

/// What happened to the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Hidden,
    Revealed,
}

/// macOS activation policy, mirrored so the decision is testable on every host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// Dock icon and menu bar (a normal app).
    Regular,
    /// No Dock icon, no menu bar: a background app.
    Accessory,
}

/// The activation policy a hide or reveal asks for, when the host chose "hide to background".
/// `other_visible`: some other window is still on screen, so the app must stay in the Dock.
pub fn activation_for(
    hide_to_background: bool,
    visibility: Visibility,
    other_visible: bool,
) -> Option<Activation> {
    if !hide_to_background {
        return None;
    }
    match visibility {
        Visibility::Hidden if other_visible => None,
        Visibility::Hidden => Some(Activation::Accessory),
        Visibility::Revealed => Some(Activation::Regular),
    }
}

/// Remembers the policy last applied so only real changes reach the OS.
#[derive(Debug, Default)]
pub struct ActivationTracker {
    accessory: AtomicBool,
}

impl ActivationTracker {
    /// The change to apply for `want`, or `None` when it is no change.
    pub fn transition(&self, want: Option<Activation>) -> Option<Activation> {
        match want? {
            Activation::Accessory => {
                (!self.accessory.swap(true, Ordering::SeqCst)).then_some(Activation::Accessory)
            }
            Activation::Regular => self
                .accessory
                .swap(false, Ordering::SeqCst)
                .then_some(Activation::Regular),
        }
    }
}

/// Called after the main window is hidden or revealed: apply the macOS activation policy for a
/// "hide to background" host, then run the host's visibility callback.
pub(crate) fn note_visibility(app: &AppHandle<Wry>, visibility: Visibility) {
    let state = app.state::<ShellState>();
    let other_visible = app
        .webview_windows()
        .iter()
        .any(|(label, w)| *label != state.main_label && w.is_visible().unwrap_or(false));
    // A never-show host stays Prohibited: hide/reveal never changes the activation policy.
    let want = if state.show_policy.never_show_now() {
        None
    } else {
        activation_for(state.hide_to_background, visibility, other_visible)
    };
    if let Some(change) = state.activation.transition(want) {
        apply_activation(app, change);
    }
    if let Some(cb) = &state.on_visibility {
        cb(app, visibility);
    }
}

#[cfg(target_os = "macos")]
fn apply_activation(app: &AppHandle<Wry>, change: Activation) {
    let policy = match change {
        Activation::Regular => tauri::ActivationPolicy::Regular,
        Activation::Accessory => tauri::ActivationPolicy::Accessory,
    };
    let _ = app.set_activation_policy(policy);
}

#[cfg(not(target_os = "macos"))]
fn apply_activation(_app: &AppHandle<Wry>, _change: Activation) {}

/// Begin a quit from a host-side request (tray Quit): drain when the host opted in, else exit now.
pub(crate) fn request_quit(app: &AppHandle<Wry>, source: &'static str) {
    let state = app.state::<ShellState>();
    let alive = !app.webview_windows().is_empty();
    match state.quit.request(&state.exiting, alive) {
        QuitStep::ExitNow => quit(app),
        QuitStep::AlreadyDraining => {}
        QuitStep::Drain => {
            if !start_drain(app, source) {
                state.quit.finish();
                quit(app);
            }
        }
    }
}

/// Emit `quit-requested` and arm the bound. False when the renderer cannot be reached.
fn start_drain(app: &AppHandle<Wry>, source: &'static str) -> bool {
    let state = app.state::<ShellState>();
    let Some(bound) = state.quit.bound() else {
        return false;
    };
    let payload = serde_json::json!({"source": source, "boundMs": bound.as_millis() as u64});
    if app.emit(EVENT_QUIT_REQUESTED, payload).is_err() {
        return false;
    }
    let handle = app.clone();
    state.quit.arm_bound(move || quit(&handle));
    true
}

/// The renderer finished draining (or never will): exit.
pub(crate) fn finish_quit(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    // `exiting` first: a quit request racing this one must see ExitNow, not start a new drain.
    state.exiting.store(true, Ordering::SeqCst);
    state.quit.finish();
    quit(app);
}

pub fn quit(app: &AppHandle<Wry>) {
    app.state::<ShellState>()
        .exiting
        .store(true, Ordering::SeqCst);
    app.exit(0);
}

pub(crate) fn deliver_links<'a>(app: &AppHandle<Wry>, raw: impl IntoIterator<Item = &'a str>) {
    let state = app.state::<ShellState>();
    let raw: Vec<&str> = raw.into_iter().collect();
    crate::open_url::deliver(&raw);
    state.deep.push_all(raw, &state.schemes);
    flush_links(app);
}

pub(crate) fn flush_links(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    if !state.frontend_ready.load(Ordering::SeqCst) {
        return;
    }
    for link in state.deep.drain() {
        let _ = app.emit(EVENT_DEEP_LINK, link);
    }
}

/// An app's second-instance callback: (app, argv, cwd).
pub(crate) type SecondInstanceFn<A> = std::sync::Arc<dyn Fn(&A, &[String], &str) + Send + Sync>;

/// Second-launch routing: URLs reach the open-URL hub first, then the app's callback owns the
/// rest; without one the `default` (event, deep links, reveal) runs.
pub(crate) fn route_second_instance<A>(
    app: &A,
    custom: Option<&SecondInstanceFn<A>>,
    argv: &[String],
    cwd: &str,
    default: impl Fn(&A, &[String], &str),
) {
    crate::open_url::deliver_second_instance(argv);
    match custom {
        Some(f) => f(app, argv, cwd),
        None => default(app, argv, cwd),
    }
}

/// A host's say over whether a second launch reveals the main window: (app, argv, cwd) -> reveal.
pub(crate) type RevealDecider<A> =
    std::sync::Arc<dyn Fn(&A, &[String], &str) -> bool + Send + Sync>;

/// True when the default second-launch behaviour should reveal and focus the main window.
pub(crate) fn second_instance_reveals<A>(
    decider: Option<&RevealDecider<A>>,
    app: &A,
    argv: &[String],
    cwd: &str,
) -> bool {
    decider.is_none_or(|f| f(app, argv, cwd))
}

pub(crate) fn on_second_instance(
    app: &AppHandle<Wry>,
    argv: &[String],
    cwd: &str,
    decider: Option<&RevealDecider<AppHandle<Wry>>>,
) {
    let _ = app.emit(
        EVENT_SECOND_INSTANCE,
        serde_json::json!({"argv": argv, "cwd": cwd}),
    );
    deliver_links(app, argv.iter().skip(1).map(String::as_str));
    if second_instance_reveals(decider, app, argv, cwd) {
        crate::windows::reveal_with(app, None, RevealSource::SecondInstance);
    }
}

pub(crate) fn on_menu_event(app: &AppHandle<Wry>, id: &str) {
    let state = app.state::<ShellState>();
    let _ = app.emit(EVENT_MENU, serde_json::json!({"id": id}));
    if let Some(cb) = &state.on_menu {
        cb(app, id);
    }
}

/// Feed the idle machine a minimise or restore of the main window (only for a host that asked with
/// `IdleOptions::start_on_minimize`).
fn note_minimized_for_idle(app: &AppHandle<Wry>, label: &str) {
    let state = app.state::<ShellState>();
    if label != state.main_label
        || !state
            .idle
            .as_ref()
            .is_some_and(|h| h.options.start_on_minimize)
    {
        return;
    }
    if let Some(w) = app.get_webview_window(label) {
        crate::idle::note_minimized(app, w.is_minimized().unwrap_or(false));
    }
}

pub(crate) fn run_event(app: &AppHandle<Wry>, event: &RunEvent) {
    let state = app.state::<ShellState>();
    match event {
        RunEvent::WindowEvent { label, event, .. } => match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                if state.modules.contains(crate::Module::WindowState) {
                    state.windows.schedule_save(app.clone(), label.clone());
                }
                if matches!(event, WindowEvent::Resized(_)) {
                    if let Some(w) = app.get_webview_window(label) {
                        if state.modules.contains(crate::Module::Frameless) {
                            state.traffic.reapply(&w);
                        }
                    }
                    note_minimized_for_idle(app, label);
                }
            }
            WindowEvent::Focused(false) => note_minimized_for_idle(app, label),
            WindowEvent::Focused(true) => {
                if let Some(w) = app.get_webview_window(label) {
                    if state.modules.contains(crate::Module::Frameless) {
                        state.traffic.reapply(&w);
                    }
                }
                if *label == state.main_label {
                    crate::idle::note_visible(app);
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                if state.modules.contains(crate::Module::WindowState) {
                    let _ = state.windows.save_now(app, label);
                }
                if state.close_to_tray
                    && *label == state.main_label
                    && !state.exiting.load(Ordering::SeqCst)
                {
                    api.prevent_close();
                    if let Some(w) = app.get_webview_window(label) {
                        let _ = w.hide();
                    }
                    crate::idle::note_hidden(app, true);
                    note_visibility(app, Visibility::Hidden);
                }
            }
            _ => {}
        },
        RunEvent::ExitRequested { api, code, .. } => {
            // `code == None`: the last window closed. A tray-resident app keeps running.
            if code.is_none() && state.close_to_tray && !state.exiting.load(Ordering::SeqCst) {
                api.prevent_exit();
                return;
            }
            // Opt-in drain: ask the renderer to flush first. `exiting` set by finish/force/bound
            // makes this return ExitNow, so the exit that follows never prompts twice.
            match state
                .quit
                .request(&state.exiting, !app.webview_windows().is_empty())
            {
                QuitStep::AlreadyDraining => {
                    api.prevent_exit();
                    return;
                }
                QuitStep::Drain => {
                    if start_drain(app, "exit-requested") {
                        api.prevent_exit();
                        return;
                    }
                    state.quit.finish();
                }
                QuitStep::ExitNow => {}
            }
            state.exiting.store(true, Ordering::SeqCst);
            for w in app.webview_windows().keys() {
                if state.modules.contains(crate::Module::WindowState) {
                    let _ = state.windows.save_now(app, w);
                }
            }
        }
        RunEvent::Exit => shutdown(app),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if reopen_wants_reveal(*has_visible_windows) {
                crate::windows::reveal_with(app, None, RevealSource::Reopen);
            }
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        RunEvent::Opened { urls } => {
            let raw: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
            deliver_links(app, raw.iter().map(String::as_str));
            crate::windows::reveal_with(app, None, RevealSource::Opened);
        }
        _ => {}
    }
}

fn shutdown(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    state.exiting.store(true, Ordering::SeqCst);
    #[cfg(feature = "engine")]
    if let Some(engine) = &state.engine {
        engine.shutdown();
    }
    if state.tray.is_some() {
        let _ = app.remove_tray_by_id(crate::tray::TRAY_ID);
    }
    // Releasing the claims frees the single-instance record.
    crate::single_instance::release_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(Vec<String>, String)>>>;

    #[test]
    fn default_runs_without_a_callback() {
        let ran = Cell::new(0);
        let argv = vec!["app".to_owned()];
        route_second_instance(&(), None, &argv, "/w", |_, a, c| {
            assert_eq!((a, c), (&argv[..], "/w"));
            ran.set(ran.get() + 1);
        });
        assert_eq!(ran.get(), 1);
    }

    #[test]
    fn custom_callback_gets_argv_and_cwd_and_suppresses_default() {
        let calls: Calls = Arc::default();
        let c = calls.clone();
        let cb: SecondInstanceFn<()> =
            Arc::new(move |_, a, cwd| c.lock().unwrap().push((a.to_vec(), cwd.to_owned())));
        let argv = vec!["app".to_owned(), "--x".to_owned()];
        route_second_instance(&(), Some(&cb), &argv, "/work", |_, _, _| {
            panic!("default focus behaviour must not run")
        });
        assert_eq!(*calls.lock().unwrap(), vec![(argv, "/work".to_owned())]);
    }

    #[test]
    fn the_default_second_launch_reveals_unless_the_host_declines() {
        let argv = vec!["app".to_owned()];
        assert!(second_instance_reveals::<()>(None, &(), &argv, "/w"));
        let yes: RevealDecider<()> = Arc::new(|_, _, _| true);
        let no: RevealDecider<()> = Arc::new(|_, a, cwd| a.len() > 5 && cwd == "/w");
        assert!(second_instance_reveals(Some(&yes), &(), &argv, "/w"));
        assert!(!second_instance_reveals(Some(&no), &(), &argv, "/w"));
    }
}
