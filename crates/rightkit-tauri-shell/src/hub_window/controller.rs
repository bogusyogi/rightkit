//! The orchestration that drives the hub window's lifecycle through this module's mechanics: show
//! the hub, build it on demand, route it to a tab, present or hide the launch splash, and keep the
//! legal-gate hand-over in order.
//!
//! Donor: the hub-window orchestration of a desktop app that keeps one long-lived hidden-and-shown
//! main window (show, build on demand, route, launch splash, legal-gate hand-over).
//!
//! Everything product-specific is a [`HubHost`] field: labels, title, the route format, surface
//! sizes, colours, the splash look, the log wording that is not a tag, and the callbacks that reach
//! the host's pill, settings, activation policy and native panels. The crate holds the order of
//! the steps, the macOS create-on-demand path, the splash one-shot latch and the Windows rule below.
//!
//! # Windows never builds a webview at runtime
//!
//! `WebviewWindowBuilder::build` needs the event loop to pump WebView2 initialisation, so building
//! from a command handler or a `run_on_main_thread` callback wedges the loop and leaves a stuck
//! half-built window. On Windows the hub is therefore a static window the host declares in its
//! Tauri configuration, and [`HubController::create`] and [`HubController::show_route`] refuse to
//! build one (event `{label}_static_missing`). [`HubController::splash_build`] is the one build
//! left and is for `setup` only. macOS builds the hub on demand (`WKWebView` has no such wedge).
//!
//! # Events
//!
//! Every event goes to [`HubHost::sink`] as `(event, detail)`. Hub tags use the host's `label` as
//! their prefix (`{label}_show_invoked`, `{label}_recreated`, `{label}_recreated_result`,
//! `{label}_recreated_hidden_qa`, `{label}_show_from_pill`, `{label}_show_from_pill_error`,
//! `{label}_show_result`, `{label}_create_from_pill_error`, `{label}_static_missing`,
//! `{label}_hide_after_legal_error`, `{label}_hidden_after_legal`, `{label}_surface_*`). The splash
//! tags are fixed: `launch_splash_shown`, `launch_splash_suppressed`, `launch_splash_missing`,
//! `splash_skipped_hidden_qa`, `splash_build_error`.

use tauri::{AppHandle, Manager, Runtime, WebviewWindow, Wry};

use super::macos::TitlebarPolicy;
use super::{
    apply_surface_size, build_window, fit_work_area, reveal_mode, route_token, show_and_focus,
    show_only, unminimize, window_snapshot, EventSink, PillOrigin, RenderingPolicy, RevealMode,
    RevealSource, SurfaceSize, WindowBuild,
};
use crate::startup::LegalGateLatch;
use crate::windows::WorkAreaFit;

/// Refusal text for a hub that is missing on Windows (see the module doc).
pub const STATIC_HUB_MISSING: &str = "static hub window missing from tauri.windows.conf.json";

/// One surface the hub can present: its name in log lines, size, minimum and resize policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceSpec {
    pub name: &'static str,
    pub size: (f64, f64),
    pub min_size: (f64, f64),
    pub resizable: bool,
}

/// The launch splash window the host wants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplashSpec {
    pub label: &'static str,
    pub route: &'static str,
    pub size: (f64, f64),
    pub background: [u8; 4],
}

/// macOS native splash panel hooks (the AppKit view is the host's).
#[derive(Clone, Copy)]
pub struct NativeSplash {
    /// Shows the native splash; `true` when the view accepted the show.
    pub show: fn() -> bool,
    /// A hidden QA launch skipped the splash (the host's handoff gate counts it as complete).
    pub note_skipped_hidden_qa: Option<fn()>,
}

/// Host wording for the legal-gate hand-over log lines.
#[derive(Debug, Clone, Copy)]
pub struct LegalGateWording {
    /// The `reason=` value, the host's name for the acceptance step.
    pub reason: &'static str,
    /// Detail of `{label}_hidden_after_legal`.
    pub hub_hidden_detail: &'static str,
}

type HubAnimation = fn(&WebviewWindow<Wry>) -> Result<(), String>;

/// Everything the controller needs from the host. Plain data and function pointers, so a
/// `HubController` can be a `static`. The crate drives `Wry` windows only (`fit_work_area` and the
/// idle machine are `Wry`).
#[derive(Clone, Copy)]
pub struct HubHost {
    /// The hub window's label, and the prefix of every hub event.
    pub label: &'static str,
    pub title: &'static str,
    /// The reveal source every hub and splash reveal asks the shell's guard with.
    pub reveal: RevealSource,
    pub sink: EventSink,
    /// A hidden QA run owns the windows: they keep rendering and stay hidden. Host input: the
    /// shell's own `lifecycle::qa_hidden` reads a different switch (see the crate patch note).
    pub qa_hidden: fn() -> bool,
    /// The surface the hub is about to present.
    pub surface: fn() -> SurfaceSpec,
    /// The tab token that opens the hub's default view; it is dropped before `route`.
    pub default_tab: &'static str,
    /// The hub's route for a validated tab token (`None` for the default view).
    pub route: fn(Option<&str>) -> String,
    /// Tells the renderer to navigate to `tab` (the host's event name and payload).
    pub navigate: fn(&AppHandle<Wry>, &str) -> Result<(), String>,
    pub background: fn() -> [u8; 4],
    pub titlebar: TitlebarPolicy,
    pub work_area_fit: WorkAreaFit,
    pub splash: SplashSpec,
    pub native_splash: Option<NativeSplash>,
    pub legal: LegalGateWording,
    /// `onboarding_complete=` of `{label}_show_invoked`.
    pub onboarding_complete: fn() -> bool,
    /// Runs on every hide of the hub, before the renderer is suspended (stop a native capture).
    pub on_hidden: fn(),
    /// Before the hub is shown. Called only on a `Foreground` reveal, never in a hidden QA run and
    /// never on a `Background` or `Forbidden` one, so the host may reconcile visibility (a pill
    /// invariant, an activation policy) that can reveal or reassert native windows.
    pub before_show: fn(&AppHandle<Wry>, RevealMode),
    /// Before the hub is built; `present` is the EFFECTIVE presentation: the caller asked to show,
    /// the host's QA-hidden predicate is false and the guard allows a foreground reveal.
    pub before_create: fn(&AppHandle<Wry>, bool),
    /// macOS only: the hub was revealed in the foreground (Dock tile). Never called elsewhere.
    pub after_reveal: fn(&AppHandle<Wry>),
    /// The hand-over from the pill to the hub: the AppKit panel on macOS, the native shell on
    /// Windows. `None` elsewhere. Called only when the guard allows a foreground reveal. An `Err`
    /// falls through to an ordinary show.
    pub animate_from_pill: Option<HubAnimation>,
}

/// Drives one hub window. `const`-constructible for a `static`.
pub struct HubController {
    host: HubHost,
    pill_origin: PillOrigin,
}

impl HubController {
    pub const fn new(host: HubHost) -> Self {
        Self {
            host,
            pill_origin: PillOrigin::new(),
        }
    }

    pub fn host(&self) -> &HubHost {
        &self.host
    }

    fn log(&self, event: &str, detail: &str) {
        (self.host.sink)(event, detail);
    }

    fn tag(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.host.label)
    }

    /// Whether the shell's guard allows a foreground reveal for the hub's source.
    pub fn foreground_reveal_allowed(&self, app: &AppHandle<Wry>) -> bool {
        reveal_mode(app, self.host.reveal) == RevealMode::Foreground
    }

    /// The launch flight is over: from now on a hub open can be armed to originate from the pill.
    pub fn mark_launch_done(&self) {
        self.pill_origin.mark_launch_done();
    }

    /// The next hub show originates from the pill when the hub is hidden (on macOS also absent).
    pub fn arm_open_from_pill(&self, app: &AppHandle<Wry>) {
        self.pill_origin
            .arm_for_window(app, self.host.label, cfg!(target_os = "macos"));
    }

    /// Battery: the hidden hub's rendering is suspended through the shell's idle machine. Every
    /// hide path funnels through here, so [`HubHost::on_hidden`] runs on all of them.
    pub fn suspend_rendering(&self, win: &WebviewWindow<Wry>, label: &str) {
        super::suspend_rendering(
            win,
            label,
            &RenderingPolicy {
                label: self.host.label,
                on_hidden: &self.host.on_hidden,
                keep_rendering: &self.host.qa_hidden,
            },
        );
    }

    /// The hub is about to show: resume its webview (a no-op unless the idle machine suspended it).
    pub fn resume_rendering(&self, win: &WebviewWindow<Wry>, label: &str) {
        super::resume_rendering(win, label, self.host.label, self.host.reveal);
    }

    /// Resize and re-centre the hub for `surface` before it is shown (idempotent).
    pub fn size_for_surface<R: Runtime>(&self, win: &WebviewWindow<R>, surface: SurfaceSpec) {
        apply_surface_size(
            win,
            SurfaceSize {
                name: surface.name,
                size: surface.size,
                resizable: surface.resizable,
                // Resizing must not bring back the zoom/maximise affordance (macOS green button).
                maximizable: false,
                recenter: true,
            },
            &self.tag("surface"),
            self.host.sink,
        );
    }

    fn after_foreground_reveal(&self, app: &AppHandle<Wry>) {
        if cfg!(target_os = "macos") {
            (self.host.after_reveal)(app);
        }
    }

    /// Show the hub: entry point of the tray, page-load re-show, second-instance and deferred
    /// post-splash resume. The hub never waits on splash or pill state.
    pub fn show(&self, app: &AppHandle<Wry>) {
        let mut built_for_pill = false;
        // The macOS create-on-demand path shows again after it builds: a loop, not a recursion.
        while self.show_once(app, &mut built_for_pill) {}
    }

    /// One pass of [`HubController::show`]; `true` asks for another pass.
    fn show_once(&self, app: &AppHandle<Wry>, built_for_pill: &mut bool) -> bool {
        let h = &self.host;
        // Every entry point is logged so a spurious open-then-close race can be cross-referenced
        // with the hide event that follows.
        self.log(
            &self.tag("show_invoked"),
            &format!(
                "onboarding_complete={} splash_handoff_done={}",
                (h.onboarding_complete)(),
                self.pill_origin.launch_done()
            ),
        );
        if (h.qa_hidden)() {
            if app.get_webview_window(h.label).is_none() {
                let _ = self.create(app, None, true);
            }
            if let Some(win) = app.get_webview_window(h.label) {
                let _ = win.hide();
            }
            return false;
        }

        // The guard's answer comes first: nothing below that can present a window runs on a
        // `Forbidden` reveal, and the morph only on a `Foreground` one.
        let mode = reveal_mode(app, h.reveal);
        if mode == RevealMode::Foreground {
            (h.before_show)(app, mode);
        }
        if let Some(win) = app.get_webview_window(h.label) {
            self.resume_rendering(&win, h.label);
            super::macos::configure_titlebar(app, &win, h.titlebar, h.reveal, h.label, h.sink);
            unminimize(app, &win, h.reveal, h.label, h.sink);
            // Size for the surface BEFORE showing: the declared size is the app size, so a pending
            // legal gate would otherwise appear large and visibly shrink once the renderer mounts.
            self.size_for_surface(&win, (h.surface)());
            fit_work_area(app, h.label, h.label, h.work_area_fit, h.sink);
            if cfg!(any(target_os = "macos", target_os = "windows")) && self.pill_origin.take() {
                let transition = match h.animate_from_pill {
                    Some(animate) if mode == RevealMode::Foreground => animate(&win),
                    Some(_) => Err(format!("pill hand-over suppressed: reveal mode {mode:?}")),
                    None => Err("no native pill hand-over".to_string()),
                };
                match transition {
                    Ok(()) => {
                        self.after_foreground_reveal(app);
                        self.log(&self.tag("show_from_pill"), &window_snapshot(&win));
                        return false;
                    }
                    Err(err) => self.log(&self.tag("show_from_pill_error"), &err),
                }
            }
            let shown = show_and_focus(app, &win, h.reveal, h.label, h.label, h.sink);
            if shown == RevealMode::Foreground {
                self.after_foreground_reveal(app);
            }
            self.log(&self.tag("show_result"), &window_snapshot(&win));
            return false;
        }
        if cfg!(target_os = "macos") && !*built_for_pill && self.pill_origin.is_armed() {
            match self.create(app, None, false) {
                Ok(_) => {
                    *built_for_pill = true;
                    return true;
                }
                Err(err) => {
                    self.pill_origin.clear();
                    self.log(&self.tag("create_from_pill_error"), &err);
                }
            }
        }
        let _ = self.create(app, None, true);
        false
    }

    /// Build the hub on demand (macOS), hidden for a pill hand-over or shown at once. Refuses on
    /// Windows (see the module doc). `initial_tab` may come from another process: only a short
    /// token reaches the route, anything else opens the default view.
    pub fn create(
        &self,
        app: &AppHandle<Wry>,
        initial_tab: Option<&str>,
        present: bool,
    ) -> Result<WebviewWindow<Wry>, String> {
        let h = &self.host;
        if cfg!(target_os = "windows") {
            self.log(&self.tag("static_missing"), STATIC_HUB_MISSING);
            return Err(STATIC_HUB_MISSING.to_string());
        }
        // Effective presentation first: a hidden QA run never presents, and a non-foreground guard
        // answer never lets a host callback activate the app.
        let present_now =
            present && !(h.qa_hidden)() && reveal_mode(app, h.reveal) == RevealMode::Foreground;
        (h.before_create)(app, present_now);
        let surface = (h.surface)();
        let tab = initial_tab
            .and_then(route_token)
            .filter(|tab| *tab != h.default_tab);
        let route = (h.route)(tab);
        let win = build_window(
            app,
            &WindowBuild {
                label: h.label,
                title: h.title,
                route: &route,
                inner_size: surface.size,
                min_size: Some(surface.min_size),
                visible: present && !(h.qa_hidden)(),
                reveal_source: h.reveal,
                resizable: surface.resizable,
                maximizable: false,
                minimizable: None,
                transparent: Some(false),
                background_color: Some((h.background)()),
                shadow: true,
                center: true,
                skip_taskbar: Some(false),
                always_on_top: None,
            },
            h.sink,
        )?;
        super::macos::configure_titlebar(app, &win, h.titlebar, h.reveal, h.label, h.sink);

        self.log(
            &self.tag("recreated"),
            &format!(
                "show_hub created a new hub window tab={}",
                tab.unwrap_or(h.default_tab)
            ),
        );
        if !present {
            let _ = win.hide();
            return Ok(win);
        }
        if (h.qa_hidden)() {
            self.log(&self.tag("recreated_hidden_qa"), "skipped show and focus");
            return Ok(win);
        }
        let mode = show_and_focus(app, &win, h.reveal, &self.tag("recreated"), h.label, h.sink);
        if mode == RevealMode::Foreground {
            self.after_foreground_reveal(app);
        }
        self.log(&self.tag("recreated_result"), &window_snapshot(&win));
        Ok(win)
    }

    /// Show the hub on `tab` and tell the renderer to navigate there.
    pub fn show_route(&self, app: &AppHandle<Wry>, tab: &str) -> Result<(), String> {
        let h = &self.host;
        if app.get_webview_window(h.label).is_none() {
            if cfg!(target_os = "windows") {
                self.log(&self.tag("static_missing"), STATIC_HUB_MISSING);
                return Err(STATIC_HUB_MISSING.to_string());
            }
            if cfg!(target_os = "macos") && self.pill_origin.is_armed() {
                if let Err(err) = self.create(app, Some(tab), false) {
                    self.pill_origin.clear();
                    return Err(err);
                }
                self.show(app);
                return (h.navigate)(app, tab);
            }
            self.create(app, Some(tab), true)?;
            return Ok(());
        }
        self.show(app);
        (h.navigate)(app, tab)
    }

    /// [`HubController::show_route`] for a hub open that originates from the pill.
    pub fn show_route_from_pill(&self, app: &AppHandle<Wry>, tab: &str) -> Result<(), String> {
        self.arm_open_from_pill(app);
        self.show_route(app, tab)
    }

    /// The one guarded way to raise the native macOS splash. `true` when the view accepted it.
    /// Refuses in a hidden QA run, when the shell's guard does not allow a foreground reveal, and
    /// when the host has no native splash.
    pub fn show_native_splash_guarded(&self, app: &AppHandle<Wry>) -> bool {
        if (self.host.qa_hidden)() || !self.foreground_reveal_allowed(app) {
            return false;
        }
        match self.host.native_splash {
            Some(native) => (native.show)(),
            None => false,
        }
    }

    /// Present the launch splash at `setup`. SETUP ONLY, never at runtime (see the module doc);
    /// `visible` is false when an onboarded user is held at the legal gate: the window is built
    /// hidden and acceptance only shows it. macOS never builds a window here, only the native panel.
    pub fn splash_build(&self, app: &AppHandle<Wry>, visible: bool) {
        let h = &self.host;
        // A hidden QA launch must never paint on the operator's desktop. The host's handoff gate
        // treats a splash that was never shown as complete.
        if (h.qa_hidden)() {
            self.log(
                "splash_skipped_hidden_qa",
                "hidden QA launch shows no splash",
            );
            if let Some(note) = h
                .native_splash
                .and_then(|native| native.note_skipped_hidden_qa)
            {
                note();
            }
            return;
        }
        if cfg!(target_os = "macos") {
            if visible && self.foreground_reveal_allowed(app) {
                let shown = h.native_splash.is_some_and(|native| (native.show)());
                if !shown {
                    self.log("launch_splash_suppressed", "reason=native_refused");
                }
            }
            return;
        }
        if app.get_webview_window(h.splash.label).is_some() {
            return;
        }
        let built = build_window(
            app,
            &WindowBuild {
                label: h.splash.label,
                title: h.title,
                route: h.splash.route,
                inner_size: h.splash.size,
                min_size: None,
                visible,
                reveal_source: h.reveal,
                resizable: false,
                maximizable: false,
                minimizable: Some(false),
                transparent: None,
                background_color: Some(h.splash.background),
                shadow: true,
                center: true,
                skip_taskbar: Some(false),
                always_on_top: Some(true),
            },
            h.sink,
        );
        if let Err(err) = built {
            self.log("splash_build_error", &err);
        }
    }

    /// Raise the launch splash for an engine boot that setup's splash never covered: the
    /// returning-user-cleared-the-legal-gate path. One-shot through `latch`, and a no-op once
    /// `launch_finished()` says the splash is already on the pill. SHOW ONLY: the splash was built
    /// hidden at setup; building here would wedge the event loop.
    pub fn splash_for_engine_boot(
        &self,
        app: &AppHandle<Wry>,
        latch: &LegalGateLatch,
        launch_finished: fn() -> bool,
    ) {
        let h = &self.host;
        if !latch.take() {
            return;
        }
        if launch_finished() {
            return;
        }
        let reason = h.legal.reason;
        if cfg!(target_os = "macos") {
            if self.show_native_splash_guarded(app) {
                self.log(
                    "launch_splash_shown",
                    &format!("reason={reason} phase=engine_boot path=native"),
                );
            } else {
                self.log(
                    "launch_splash_suppressed",
                    &format!("reason={reason} phase=engine_boot path=native"),
                );
            }
            if let Some(hub) = app.get_webview_window(h.label) {
                if let Err(err) = hub.hide() {
                    self.log(&self.tag("hide_after_legal_error"), &err.to_string());
                } else {
                    self.suspend_rendering(&hub, h.label);
                }
            }
            return;
        }
        // A hidden QA run never reveals the splash, on any OS: the host's predicate, not the
        // shell's, decides. The hub hide below still runs, so the returning-user hand-over holds.
        let qa = (h.qa_hidden)();
        // A missing splash is reported, and the hub hand-over below still runs: hiding the hub
        // does not depend on the splash.
        if let Some(splash) = app.get_webview_window(h.splash.label) {
            // The splash is topmost and never takes focus; shown only for a foreground reveal.
            let shown = !qa
                && self.foreground_reveal_allowed(app)
                && show_only(app, &splash, h.reveal, "launch_splash", h.sink)
                    == RevealMode::Foreground;
            let _ = splash.center();
            self.log(
                "launch_splash_shown",
                &format!("reason={reason} phase=engine_boot show_ok={shown}"),
            );
        } else {
            self.log(
                "launch_splash_missing",
                &format!("reason={reason} phase=engine_boot"),
            );
        }

        // The hub was shown only to present the legal gate; hide it so the splash-to-pill hand-off
        // owns the screen, as in a normal onboarded launch. This path runs only for a returning
        // user held at the gate, so a first-run user's gate-to-onboarding hand-off keeps the hub.
        if let Some(hub) = app.get_webview_window(h.label) {
            if let Err(err) = hub.hide() {
                self.log(&self.tag("hide_after_legal_error"), &err.to_string());
            } else {
                self.suspend_rendering(&hub, h.label);
                self.log(&self.tag("hidden_after_legal"), h.legal.hub_hidden_detail);
            }
        }
    }
}
