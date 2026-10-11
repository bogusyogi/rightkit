//! rightkit-shell: the shared Tauri desktop shell for Right Suite apps.
//!
//! What every desktop app needs around its own UI: a hardened webview, window state, a native
//! menu, a tray, single instance, deep links, autostart, a rich clipboard, a hang watchdog and
//! the window-to-engine seam. Layout is NOT part of the shell (the sidebar/titlebar layout lives
//! in `@rightkit/app-shell`, optional and replaceable); this crate is the behaviour underneath.
//!
//! ```ignore
//! tauri::Builder::default()
//!     .plugin(rightkit_shell::Shell::builder("myapp").app_name("MyApp").tray(spec).build().plugin())
//!     .invoke_handler(rightkit_shell::handler![my_app_command])
//!     .run(tauri::generate_context!())
//! ```
//!
//! See `API.md` for per-app migration.

pub mod autostart;
pub mod b64;
pub mod broker;
pub mod clipboard;
pub mod commands;
pub mod deep_link;
pub mod devtools;
#[cfg(feature = "engine")]
pub mod engine;
pub mod error;
pub mod frameless;
pub mod harden;
pub mod idle;
pub mod instance;
mod legacy_hold;
mod legacy_instance;
pub mod lifecycle;
pub mod menu;
pub mod modules;
pub mod nav;
pub mod notify;
pub mod open_url;
pub mod opener;
pub mod presence;
pub mod quit;
pub mod recovery;
pub mod single_instance;
pub mod site;
pub mod startup_cache;
pub mod stream;
pub mod tray;
pub mod watchdog;
pub mod window_state;
/// The `windows` / `webview2-com` generation selected by `wv2-0_38` / `wv2-0_39`.
#[cfg(windows)]
pub(crate) mod winapi {
    #[cfg(not(any(feature = "wv2-0_38", feature = "wv2-0_39")))]
    compile_error!("rightkit-shell on Windows needs `wv2-0_38` (Tauri 2.11, in default features) or `wv2-0_39` (Tauri 2.12); with default-features = false add one, e.g. features = [\"wv2-0_38\"]");
    #[cfg(all(feature = "wv2-0_38", not(feature = "wv2-0_39")))]
    pub use ::{webview2_com_038 as webview2_com, windows_061 as windows};
    #[cfg(feature = "wv2-0_39")]
    pub use ::{webview2_com_039 as webview2_com, windows_062 as windows};
}
pub mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::menu::MenuItem;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Manager, Wry};

pub use autostart::{AutostartMigration, AutostartOptions, LegacyAutostartNames};
pub use broker::PathBroker;
pub use clipboard::{ClipboardItem, ClipboardOptions, ClipboardPolicy, ImagePreference};
pub use devtools::{ContentWebview, DevToolsError, DevToolsGate, DevToolsPolicy, WebviewDecision};
pub use error::ShellError;
pub use harden::{Hardening, HardeningScript};
pub use idle::{
    IdleErrorCallback, IdleFailure, IdlePhase, IdleTransition, IdleTransitionCallback,
    IdleTransitionKind,
};
pub use menu::{MenuEntry, MenuGroup, MenuSpec};
pub use modules::{Module, ModuleSet, ShellPlugin, ShellPluginBuilder};
pub use nav::{NavDecision, NavPolicy};
pub use opener::{ExternalOpener, SystemOpener};
pub use presence::{AutoPrefs, PresenceState};
pub use recovery::{MigrationOutcome, RecoverySpec};
pub use single_instance::{
    ClaimFailure, ClaimOptions, ClaimOutcome, InstanceMessage, LegacySingleInstanceNames,
    SingleInstance, SingleInstanceGuard,
};
pub use site::SitePolicy;
pub use stream::{StreamEvent, StreamEventKind, StreamObserver, StreamPolicy};
pub use tray::{LeftClick, TraySpec};
pub use watchdog::{ShellRuntimeStatus, StallReport, WatchdogConfig, WatchdogStatus};

pub const EVENT_MENU: &str = "rightkit-shell://menu";
pub const EVENT_TRAY: &str = "rightkit-shell://tray";
pub const EVENT_DEEP_LINK: &str = "rightkit-shell://deep-link";
pub const EVENT_SECOND_INSTANCE: &str = "rightkit-shell://second-instance";
pub const EVENT_STALL: &str = "rightkit-shell://stall";
pub const EVENT_ENGINE: &str = "rightkit-shell://engine";
pub const EVENT_WINDOW: &str = "rightkit-shell://window";
pub const EVENT_IDLE: &str = "rightkit-shell://idle";
pub const EVENT_PRESENCE: &str = "rightkit-shell://presence";
/// Emitted instead of exiting when the host opted into [`ShellBuilder::quit_drain`].
pub const EVENT_QUIT_REQUESTED: &str = "rightkit-shell://quit-requested";

type MenuCallback = Arc<dyn Fn(&AppHandle<Wry>, &str) + Send + Sync>;
type PresenceCallback = Arc<dyn Fn(&AppHandle<Wry>, PresenceState) + Send + Sync>;
type VisibilityCallback = Arc<dyn Fn(&AppHandle<Wry>, lifecycle::Visibility) + Send + Sync>;
type RevealDecider = lifecycle::RevealDecider<AppHandle<Wry>>;

/// What closing the main window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloseBehavior {
    /// Closing the last window quits (the default).
    #[default]
    Quit,
    /// Closing hides the main window; the app keeps running (what `close_to_tray(true)` does).
    HideToTray,
    /// Like `HideToTray`, and on macOS the app also drops to an Accessory app (no Dock icon, no menu
    /// bar) while hidden and returns to Regular when revealed. On other platforms it is `HideToTray`.
    HideToBackground,
}

/// Everything the shell shares at runtime (managed Tauri state).
pub struct ShellState {
    // Read only by the macOS native-dispatch path.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) main_thread_id: std::thread::ThreadId,
    pub app_id: String,
    pub app_name: String,
    pub main_label: String,
    pub nav: Arc<NavPolicy>,
    pub opener: Arc<dyn ExternalOpener>,
    pub frameless: bool,
    pub close_to_tray: bool,
    /// `CloseBehavior::HideToBackground`: hide and reveal also change the macOS activation policy.
    pub hide_to_background: bool,
    pub on_visibility: Option<VisibilityCallback>,
    pub(crate) activation: lifecycle::ActivationTracker,
    /// The host's window-visibility policy, obeyed by [`lifecycle::reveal_mode`].
    pub show_policy: lifecycle::ShowPolicy,
    /// The opt-in quit drain (see [`quit`]).
    pub quit: Arc<quit::QuitGate>,
    pub windows: Arc<window_state::WindowStateStore>,
    pub deep: deep_link::DeepLinkQueue,
    pub schemes: Vec<String>,
    pub frontend_ready: AtomicBool,
    pub exiting: AtomicBool,
    pub traffic: frameless::TrafficLights,
    pub autostart: autostart::AutostartSpec,
    pub menu_items: Mutex<HashMap<String, MenuItem<Wry>>>,
    pub menu_rows: Mutex<Vec<(String, String)>>,
    pub tray: Option<tray::TrayRuntime>,
    pub on_menu: Option<MenuCallback>,
    #[cfg(feature = "engine")]
    pub engine: Option<Arc<engine::EngineHost>>,
    pub hosted: Mutex<Option<rightkit_service::Hosted>>,
    pub single_instance: Mutex<String>,
    /// Grants for files the person handed to the app (see [`broker`]).
    pub broker: Arc<PathBroker>,
    pub notifications: notify::NotificationState,
    pub presence: Arc<presence::PresenceDnd>,
    pub idle: Option<idle::IdleHost>,
    pub recovery: Option<recovery::RecoveryStore>,
    /// Result of the startup cache-directory repair (`None` when disabled).
    pub cache_repair: Mutex<Option<(PathBuf, recovery::CacheDirOutcome)>>,
    /// First scheme registered with [`ShellBuilder::stream_protocol`]; the default for `rk_shell_stream_grant`.
    pub stream_scheme: Option<String>,
    /// Every scheme registered with [`ShellBuilder::stream_protocol`].
    pub stream_schemes: Vec<String>,
    /// Scheme registered with [`ShellBuilder::site_protocol`].
    pub site_scheme: Option<String>,
    pub site_policy: site::SitePolicy,
    /// Modules this app installed (see [`Module`]).
    pub modules: ModuleSet,
    pub clipboard: clipboard::ClipboardPolicy,
    /// Read-side clipboard options (Windows image preference, opt-in unknown formats).
    pub clipboard_options: clipboard::ClipboardOptions,
    /// Per-app temp directory for clipboard images.
    pub clipboard_dir: PathBuf,
}

/// Called on the first instance when a later launch forwards its argv and working directory.
pub type SecondInstanceCallback = Arc<dyn Fn(&AppHandle<Wry>, &[String], &str) + Send + Sync>;

pub struct ShellBuilder {
    app_id: String,
    app_name: String,
    main_label: String,
    hardening: Hardening,
    hardening_script: HardeningScript,
    devtools_policy: DevToolsPolicy,
    content_webviews: Vec<ContentWebview>,
    nav: NavPolicy,
    extra_internal_schemes: Vec<String>,
    opener: Arc<dyn ExternalOpener>,
    single_instance: bool,
    close_to_tray: bool,
    hide_to_background: bool,
    on_visibility: Option<VisibilityCallback>,
    never_show: bool,
    never_show_env: Vec<String>,
    never_show_env_values: Vec<(String, String)>,
    reveal_on_reopen: bool,
    lifecycle_only: bool,
    qa_instance_exempt: bool,
    on_second_instance_reveal: Option<RevealDecider>,
    quit_drain: Option<std::time::Duration>,
    frameless: bool,
    show_on_ready: Option<bool>,
    state_dir: Option<PathBuf>,
    schemes: Vec<String>,
    menu: Option<MenuSpec>,
    tray: Option<TraySpec>,
    watchdog: Option<WatchdogConfig>,
    on_menu: Option<MenuCallback>,
    on_second_instance: Option<SecondInstanceCallback>,
    streams: Vec<(String, StreamPolicy)>,
    site: Option<(String, site::SitePolicy)>,
    modules: ModuleSet,
    clipboard: clipboard::ClipboardPolicy,
    clipboard_options: clipboard::ClipboardOptions,
    idle_on_error: Option<idle::IdleErrorCallback>,
    idle_on_transition: Option<idle::IdleTransitionCallback>,
    autostart: autostart::AutostartOptions,
    recovery: Option<RecoverySpec>,
    idle_after: Option<std::time::Duration>,
    idle_options: Option<idle::IdleOptions>,
    cache_dir: Option<PathBuf>,
    repair_cache: Option<bool>,
    on_presence: Option<PresenceCallback>,
    #[cfg(feature = "engine")]
    engine: Option<engine::EngineSpec>,
}

pub struct Shell {
    cfg: ShellBuilder,
}

impl Shell {
    /// `app_id`: lowercase letters, digits and `-` (it names the single-instance service and the
    /// LaunchAgent / registry entry), at most 40 characters.
    pub fn builder(app_id: &str) -> ShellBuilder {
        ShellBuilder {
            app_id: app_id.to_owned(),
            app_name: app_id.to_owned(),
            main_label: "main".into(),
            hardening: Hardening::default(),
            hardening_script: HardeningScript::default(),
            devtools_policy: DevToolsPolicy::default(),
            content_webviews: Vec::new(),
            nav: NavPolicy::default(),
            extra_internal_schemes: Vec::new(),
            opener: Arc::new(SystemOpener),
            single_instance: true,
            close_to_tray: false,
            hide_to_background: false,
            on_visibility: None,
            never_show: false,
            never_show_env: Vec::new(),
            never_show_env_values: Vec::new(),
            reveal_on_reopen: true,
            lifecycle_only: false,
            qa_instance_exempt: false,
            on_second_instance_reveal: None,
            quit_drain: None,
            frameless: false,
            show_on_ready: None,
            state_dir: None,
            schemes: Vec::new(),
            menu: None,
            tray: None,
            watchdog: Some(WatchdogConfig::default()),
            on_menu: None,
            on_second_instance: None,
            streams: Vec::new(),
            site: None,
            modules: ModuleSet::all(),
            clipboard: clipboard::ClipboardPolicy::default(),
            clipboard_options: clipboard::ClipboardOptions::default(),
            idle_on_error: None,
            idle_on_transition: None,
            autostart: autostart::AutostartOptions::default(),
            recovery: None,
            idle_after: None,
            idle_options: None,
            cache_dir: None,
            repair_cache: None,
            on_presence: None,
            #[cfg(feature = "engine")]
            engine: None,
        }
    }
}

impl ShellBuilder {
    pub fn app_name(mut self, name: &str) -> Self {
        self.app_name = name.to_owned();
        self
    }
    pub fn main_label(mut self, label: &str) -> Self {
        self.main_label = label.to_owned();
        self
    }
    pub fn hardening(mut self, h: Hardening) -> Self {
        self.hardening = h;
        self
    }
    /// Select DOM script injection scope for the full plugin (default: all frames).
    /// Native hardening & navigation policy still apply when the script is off.
    pub fn hardening_script(mut self, scope: HardeningScript) -> Self {
        self.hardening_script = scope;
        self
    }
    /// Who may use DevTools. The default, [`DevToolsPolicy::Denied`], makes no webview of the app
    /// inspectable. [`DevToolsPolicy::ContentOnly`] is a named allowance for an app that renders
    /// opened content (ViewRight) and must also register that content with
    /// [`ShellBuilder::content_webview`]. The policy applies whichever modules are installed.
    pub fn devtools_policy(mut self, p: DevToolsPolicy) -> Self {
        self.devtools_policy = p;
        self
    }
    /// Register a webview that renders opened content as inspectable under
    /// [`DevToolsPolicy::ContentOnly`]. The main webview can never be registered, and setup fails
    /// with a typed [`DevToolsError`] when the content webview could reach the app's IPC.
    pub fn content_webview(mut self, c: ContentWebview) -> Self {
        self.content_webviews.push(c);
        self
    }
    pub fn nav_policy(mut self, p: NavPolicy) -> Self {
        self.nav = p;
        self
    }
    /// Treat `scheme` as the app's own content: the webview loads it in place. Adds to
    /// `nav_policy` whatever order they are called in. See [`NavPolicy::with_internal_scheme`].
    ///
    /// # Panics
    /// Panics on a scheme that is empty, malformed, or a web or script scheme (`http`, `https`,
    /// `file`, `javascript`, ...): a programmer error at startup, like an invalid app id.
    pub fn internal_scheme(mut self, scheme: &str) -> Self {
        nav::validate_internal_scheme(scheme).expect("rightkit-shell: internal_scheme");
        self.extra_internal_schemes.push(scheme.to_owned());
        self
    }
    /// Replace the system link handler (tests, or an app that routes links itself).
    pub fn opener(mut self, o: impl ExternalOpener) -> Self {
        self.opener = Arc::new(o);
        self
    }
    pub fn single_instance(mut self, on: bool) -> Self {
        self.single_instance = on;
        self
    }
    /// Closing the main window hides it; the app keeps running in the tray until Quit.
    pub fn close_to_tray(mut self, on: bool) -> Self {
        self.close_to_tray = on;
        if !on {
            self.hide_to_background = false;
        }
        self
    }
    /// What closing the main window does. The last of `close_to_tray` and `close_behavior` wins.
    /// [`CloseBehavior::HideToBackground`] is for an app with no tray: it leaves the Dock while
    /// hidden on macOS and is reachable from wherever the host reveals it (a menu-bar item, a
    /// notch, a hotkey, a second launch).
    pub fn close_behavior(mut self, behavior: CloseBehavior) -> Self {
        self.close_to_tray = behavior != CloseBehavior::Quit;
        self.hide_to_background = behavior == CloseBehavior::HideToBackground;
        self
    }
    /// Called after the main window is hidden (close, tray toggle) or revealed (tray, second launch,
    /// Dock reopen, opened URL), on the thread that raised the event. Runs after the shell's own
    /// activation-policy change, so a host can drive its own policy with `close_to_tray(true)`
    /// instead of `HideToBackground`.
    pub fn on_visibility(
        mut self,
        f: impl Fn(&AppHandle<Wry>, lifecycle::Visibility) + Send + Sync + 'static,
    ) -> Self {
        self.on_visibility = Some(Arc::new(f));
        self
    }
    /// Host-declared "never show": the shell shows, unminimises and focuses no window under any
    /// event (reopen, opened URL, second launch, show-on-ready, tray, the window command,
    /// `windows::open`/`reveal`, window-state restore), and on macOS the activation policy is set
    /// to Prohibited at plugin setup, before any window exists. Absolute: it beats `show_on_ready`,
    /// `second_instance_reveal` and every other visibility option. Default off. See also
    /// [`ShellBuilder::never_show_env`]. The host's own direct window calls are outside the shell.
    pub fn never_show(mut self, on: bool) -> Self {
        self.never_show = on;
        self
    }
    /// Register an environment variable that means never-show for this process when it is set to
    /// exactly `1`, `true`, `yes` or `on` (ASCII case-insensitive; anything else, including empty,
    /// `0` and unrecognised text, is off; read at each reveal, so a harness can set it before
    /// launch). Call it once per name (ScrapeRight: `RIGHTKIT_QA_NATIVE_REAL`).
    ///
    /// # Panics
    /// Panics on an empty name or one containing `=` or NUL: a programmer error at startup.
    pub fn never_show_env(mut self, name: &str) -> Self {
        assert!(
            !name.is_empty() && !name.contains(['=', '\0']),
            "rightkit-shell: never_show_env needs a plain environment variable name"
        );
        if !self.never_show_env.iter().any(|n| n == name) {
            self.never_show_env.push(name.to_owned());
        }
        self
    }
    /// Register an environment variable that means never-show only when it is set to exactly
    /// `expected` (case-sensitive, no trimming); any other value, or unset, is off. For a harness
    /// that uses its own token.
    ///
    /// # Panics
    /// Panics on an empty name (or one containing `=` or NUL) or an empty `expected`.
    pub fn never_show_env_value(mut self, name: &str, expected: &str) -> Self {
        assert!(
            !name.is_empty() && !name.contains(['=', '\0']) && !expected.is_empty(),
            "rightkit-shell: never_show_env_value needs a plain variable name and a non-empty expected value"
        );
        let pair = (name.to_owned(), expected.to_owned());
        if !self.never_show_env_values.contains(&pair) {
            self.never_show_env_values.push(pair);
        }
        self
    }
    /// Whether a macOS Dock reopen with no visible window, and an OS-delivered URL or file
    /// (`RunEvent::Opened`), reveal the main window (default on). Off: both still deliver their
    /// links and nothing is shown, unminimised or focused. Also off when `Module::RevealOnReopen`
    /// is not installed.
    pub fn reveal_on_reopen(mut self, on: bool) -> Self {
        self.reveal_on_reopen = on;
        self
    }
    /// A launch with `RIGHTKIT_QA_HIDDEN=1` neither claims the primary nor forwards to the running
    /// instance, so a hidden QA copy never reveals the user's app and is never revealed by it.
    /// Default off (a hidden QA launch arbitrates like any other).
    pub fn qa_instance_exempt(mut self, on: bool) -> Self {
        self.qa_instance_exempt = on;
        self
    }
    /// Keep the default second-launch behaviour (the `second-instance` event and deep links) but let
    /// the host decide whether it reveals and focuses the main window: `f` returns true to reveal.
    /// Ignored when [`ShellBuilder::on_second_instance`] replaces the default.
    pub fn second_instance_reveal(
        mut self,
        f: impl Fn(&AppHandle<Wry>, &[String], &str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.on_second_instance_reveal = Some(Arc::new(f));
        self
    }
    /// Let the renderer drain before the app exits. A quit request (tray Quit, Cmd+Q, the last
    /// window closing with a webview still alive) then emits `rightkit-shell://quit-requested`
    /// instead of exiting, and the app exits when the renderer calls `rk_shell_finish_quit` or after
    /// [`quit::DEFAULT_QUIT_DRAIN_BOUND`] (15 s), whichever comes first. Off by default: quit is
    /// immediate. `rk_shell_quit` (and `shell.quit({force: true})`) stays immediate either way.
    pub fn quit_drain(mut self, on: bool) -> Self {
        self.quit_drain = on.then_some(quit::DEFAULT_QUIT_DRAIN_BOUND);
        self
    }
    /// [`ShellBuilder::quit_drain`] with the host-side bound set (clamped to
    /// [`quit::MAX_QUIT_DRAIN_BOUND`], 120 s). Give the renderer's own `quitTimeoutMs` (10 s by
    /// default) less than this so its timeout reports before the host gives up.
    pub fn quit_drain_bound(mut self, bound: std::time::Duration) -> Self {
        self.quit_drain = Some(bound);
        self
    }
    /// Frameless chrome for windows created through `windows::builder`/`open` (see `frameless`).
    pub fn frameless(mut self, on: bool) -> Self {
        self.frameless = on;
        self
    }
    /// Show the main window when its webview is ready (default on). Calling this is an explicit
    /// visibility decision: `RIGHTKIT_QA_HIDDEN` no longer overrides it. `--background` still does.
    pub fn show_on_ready(mut self, on: bool) -> Self {
        self.show_on_ready = Some(on);
        self
    }
    /// Directory for window-state.json (default: `<app data dir>/shell`).
    pub fn state_dir(mut self, dir: PathBuf) -> Self {
        self.state_dir = Some(dir);
        self
    }
    /// URL schemes this app accepts as deep links (`myapp` for `myapp://...`).
    pub fn deep_link_scheme(mut self, scheme: &str) -> Self {
        self.schemes.push(scheme.to_ascii_lowercase());
        self
    }
    pub fn menu(mut self, m: MenuSpec) -> Self {
        self.menu = Some(m);
        self
    }
    pub fn tray(mut self, t: TraySpec) -> Self {
        self.tray = Some(t);
        self
    }
    /// Login-item options: the launch arguments, macOS `LowPriorityIO`, and removing the entry
    /// `tauri-plugin-autostart` wrote (see [`autostart`]).
    pub fn autostart_options(mut self, options: autostart::AutostartOptions) -> Self {
        self.autostart = options;
        self
    }
    /// Called once per stall episode with the stall duration, on the watchdog thread (a host can
    /// write a hang dump here). Applies to the watchdog config in place: call it after
    /// [`ShellBuilder::watchdog`], or it has no effect. No effect when the watchdog is disabled.
    pub fn on_stall(mut self, f: impl Fn(&watchdog::StallReport) + Send + Sync + 'static) -> Self {
        if let Some(config) = self.watchdog.as_mut() {
            config.on_stall = Some(Arc::new(f));
        }
        self
    }
    pub fn watchdog(mut self, w: Option<WatchdogConfig>) -> Self {
        self.watchdog = w;
        self
    }
    /// Rust-side handler for custom menu item clicks (the event is always emitted too).
    /// Replace the default second-launch behaviour (event + reveal the main window) with `f`,
    /// which receives the first instance's handle, the second launch's argv and its cwd, and fully
    /// owns focus. Arbitration is unchanged. URLs on that argv still reach [`open_url`] handlers
    /// and [`open_url::current_urls`] before `f` runs.
    pub fn on_second_instance(
        mut self,
        f: impl Fn(&AppHandle<Wry>, &[String], &str) + Send + Sync + 'static,
    ) -> Self {
        self.on_second_instance = Some(Arc::new(f));
        self
    }
    pub fn on_menu(mut self, f: impl Fn(&AppHandle<Wry>, &str) + Send + Sync + 'static) -> Self {
        self.on_menu = Some(Arc::new(f));
        self
    }
    /// Serve broker-granted files over a ranged custom protocol: `<scheme>://localhost/<id>` with
    /// the id from `rk_shell_stream_grant` (see [`stream`]). Off unless this is called.
    pub fn stream_protocol(mut self, scheme: &str, policy: StreamPolicy) -> Self {
        let scheme = scheme.to_ascii_lowercase();
        self.streams.retain(|(s, _)| *s != scheme);
        self.streams.push((scheme, policy));
        self
    }
    /// Serve allowlisted subresources of granted site roots over `<scheme>://localhost/<token>/<path>`
    /// (see [`site`]). Granted with the `rk_shell_site_grant` command. Off unless this is called.
    pub fn site_protocol(mut self, scheme: &str, policy: site::SitePolicy) -> Self {
        self.site = Some((scheme.to_ascii_lowercase(), policy));
        self
    }
    /// Age after which clipboard temp images are purged (default one hour; see [`clipboard`]).
    pub fn clipboard_purge_after(mut self, age: std::time::Duration) -> Self {
        self.clipboard.purge_after = age;
        self
    }
    /// Read-side clipboard options for `rk_shell_clipboard_read` and `rk_shell_clipboard_item`: the
    /// Windows bitmap preference and the opt-in capture of unknown pasteboard types (see
    /// [`clipboard::ClipboardOptions`]). The default captures no unknown types and prefers PNG.
    pub fn clipboard_options(mut self, options: clipboard::ClipboardOptions) -> Self {
        self.clipboard_options = options;
        self
    }
    /// Enable crash-safe setup recovery commands (see [`recovery`]).
    pub fn recovery(mut self, spec: RecoverySpec) -> Self {
        self.recovery = Some(spec);
        self
    }
    /// Suspend the main webview after it has been hidden this long (see [`idle`]). Off by default.
    pub fn webview_idle(mut self, suspend_after: std::time::Duration) -> Self {
        self.idle_after = Some(suspend_after);
        self
    }
    /// [`ShellBuilder::webview_idle`] with the full option set: the per-host timeout
    /// (`IdleOptions::suspend_after`, default [`idle::DEFAULT_SUSPEND_AFTER`]), starting the timer
    /// when the main window is minimised, hiding the WebView2 controller when the timer fires
    /// instead of at once, and whether idle events are emitted where nothing is suspended. A later
    /// `webview_idle(d)` replaces only the timeout.
    pub fn webview_idle_options(mut self, options: idle::IdleOptions) -> Self {
        self.idle_after = Some(options.suspend_after);
        self.idle_options = Some(options);
        self
    }
    /// Receive suspend and resume failures of the main webview (see [`idle::IdleFailure`]). Without
    /// this, platform failures are silent.
    pub fn webview_idle_on_error(
        mut self,
        f: impl Fn(&idle::IdleFailure) + Send + Sync + 'static,
    ) -> Self {
        self.idle_on_error = Some(Arc::new(f));
        self
    }
    /// Receive one [`idle::IdleTransition`] each time the main webview is really suspended or
    /// resumed (window label and elapsed time), for the host's log. Without this nothing changes.
    pub fn webview_idle_on_transition(
        mut self,
        f: impl Fn(&idle::IdleTransition) + Send + Sync + 'static,
    ) -> Self {
        self.idle_on_transition = Some(Arc::new(f));
        self
    }
    /// App cache directory to keep usable (default: Tauri's app cache dir).
    pub fn cache_dir(mut self, dir: PathBuf) -> Self {
        self.cache_dir = Some(dir);
        self
    }
    /// Repair a dangling symlink / stray file at the cache directory on startup (default on; default
    /// off under `lifecycle_only`, where this call turns it on).
    pub fn repair_cache(mut self, on: bool) -> Self {
        self.repair_cache = Some(on);
        self
    }
    /// Rust-side hook for automatic presence changes (meeting started, fullscreen app, ...).
    pub fn on_presence(
        mut self,
        f: impl Fn(&AppHandle<Wry>, PresenceState) + Send + Sync + 'static,
    ) -> Self {
        self.on_presence = Some(Arc::new(f));
        self
    }
    #[cfg(feature = "engine")]
    pub fn engine(mut self, spec: engine::EngineSpec) -> Self {
        self.engine = Some(spec);
        self
    }
    pub fn build(self) -> Shell {
        assert!(
            !self.app_id.is_empty()
                && self.app_id.len() <= 40
                && self
                    .app_id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "rightkit-shell: app_id must be lowercase letters, digits and '-' (<= 40 chars)"
        );
        Shell { cfg: self }
    }
}

impl Shell {
    /// Install single-instance arbitration only. Add this plugin FIRST.
    /// Only the builder's app_id is used; arbitration is always enabled.
    /// The callback receives forwarded argv & cwd and owns all second-launch behavior.
    /// No script, navigation policy, window state, presence loop or ShellState is installed.
    /// Keep using the app's own invoke handler; `handler!` requires the full shell.
    pub fn single_instance_plugin(
        self,
        on_second_instance: impl Fn(&AppHandle<Wry>, &[String], &str) + Send + Sync + 'static,
    ) -> TauriPlugin<Wry> {
        let app_id = self.cfg.app_id;
        PluginBuilder::<Wry>::new("rightkit-single-instance")
            .setup(move |app, _api| {
                let handle = app.clone();
                let version = app.package_info().version.to_string();
                let identifier = app.config().identifier.clone();
                let arbitration = instance::arbitrate_with_legacy(
                    &app_id,
                    &version,
                    Some(identifier.as_str()),
                    Arc::new(move |second| {
                        on_second_instance(&handle, &second.argv, &second.cwd);
                    }),
                );
                match arbitration {
                    instance::Arbitration::Primary(hosted) => {
                        app.manage(PluginClaim(single_instance::hold(hosted)));
                    }
                    instance::Arbitration::Secondary => std::process::exit(0),
                    // Fail closed: an undecidable claim must not run as a second primary.
                    instance::Arbitration::Unavailable(why) => {
                        return Err(unavailable_error(&why));
                    }
                    instance::Arbitration::InvalidAppId(e) => return Err(invalid_app_id_error(&e)),
                }
                Ok(())
            })
            .on_event(|app, event| {
                if matches!(event, tauri::RunEvent::Exit) {
                    if let Some(claim) = app.try_state::<PluginClaim>() {
                        single_instance::release(claim.0);
                    }
                }
            })
            .build()
    }

    /// The shell's lifecycle without its webview policy: single instance, close behaviour, tray,
    /// menu, second-launch routing, deep links, quit and the commands, but no hardening init script
    /// or native hardening, no navigation policy, no presence thread, no watchdog and no
    /// window-state restore or save. The same as
    /// `ShellPlugin::builder().shell(self).lifecycle_only()`. [`Shell::plugin`] is unchanged.
    pub fn lifecycle_plugin(self) -> TauriPlugin<Wry> {
        ShellPlugin::builder().shell(self).lifecycle_only()
    }

    /// The single Tauri plugin that installs the whole shell, with every module enabled. Add it FIRST
    /// so single-instance arbitration runs before any other plugin does work. To opt modules out, use
    /// [`ShellPlugin::builder`].
    pub fn plugin(self) -> TauriPlugin<Wry> {
        ShellPlugin::builder().shell(self).build()
    }
}

/// Build the plugin for `cfg`, honouring `cfg.modules`. Shared by [`Shell::plugin`] and
/// [`ShellPluginBuilder::build`].
pub(crate) fn install(cfg: ShellBuilder) -> TauriPlugin<Wry> {
    let modules = cfg.modules;
    let mut nav_policy = cfg.nav.clone();
    for scheme in &cfg.extra_internal_schemes {
        nav_policy = nav_policy
            .with_internal_scheme(scheme)
            .expect("rightkit-shell: internal_scheme");
    }
    let nav = Arc::new(nav_policy);
    let opener = Arc::clone(&cfg.opener);
    let hardening = cfg.hardening.clone();
    let script_scope = cfg.hardening_script;
    let (nav_hook, opener_hook) = (Arc::clone(&nav), Arc::clone(&opener));
    let hardening_for_views = hardening.clone();
    // DevTools: the process guard and the gate. A failure is carried to plugin setup, which
    // refuses to start; until then every webview is treated as denied.
    let guard_result = devtools::guard_process_start();
    let gate_result = devtools::build_gate_with(
        cfg.devtools_policy,
        &cfg.main_label,
        &cfg.content_webviews,
        cfg!(feature = "content-devtools"),
        devtools::DEV_INSPECT_COMPILED,
    );
    let gate_for_views = gate_result
        .clone()
        .unwrap_or_else(|_| DevToolsGate::denied(&cfg.main_label));
    let content_labels = gate_for_views.content_labels();
    let harden_webviews = modules.contains(Module::JsHardening);
    let show_on_ready_explicit = cfg.show_on_ready;
    // `lifecycle_only` turns ShowOnReady off; only the host's explicit `show_on_ready(true)` brings
    // it back. Every other module set is decided by `modules` alone.
    let show_on_ready_module = lifecycle::show_on_ready_installed(
        modules.contains(Module::ShowOnReady),
        cfg.lifecycle_only,
        show_on_ready_explicit,
    );
    let ready_policy = show_policy_of(&cfg);
    let restore_windows = modules.contains(Module::WindowState);
    let main_label = cfg.main_label.clone();
    let (streams, site) = if modules.contains(Module::Stream) {
        (cfg.streams.clone(), cfg.site.clone())
    } else {
        (Vec::new(), None)
    };

    let mut plugin = PluginBuilder::<Wry>::new("rightkit-shell");
    if modules.contains(Module::NavigationPolicy) {
        plugin = plugin.on_navigation(move |_webview, url| match nav_hook.decide(url) {
            NavDecision::Allow => true,
            NavDecision::External { url } => {
                let _ = opener::open_checked(opener_hook.as_ref(), &url);
                false
            }
            NavDecision::Block => false,
        });
    }
    plugin = plugin
        .on_webview_ready(move |webview| {
            let decision = gate_for_views.decide(webview.label());
            // The DevTools policy is not a module: it applies to every webview, always.
            // A denied webview whose DevTools could not be disabled is refused: it has been
            // closed and reported, and nothing else is set up for it.
            if devtools::apply_native(&webview, decision).is_err() {
                return;
            }
            if harden_webviews {
                harden::apply_native(&webview, &hardening_for_views, decision);
            }
            if let Err(e) = devtools::verify_not_remote_debuggable(webview.app_handle()) {
                eprintln!("{e}");
                webview.app_handle().exit(1);
                return;
            }
            let app = webview.app_handle().clone();
            let label = webview.label().to_owned();
            lifecycle::enforce_never_show(&app, &ready_policy);
            windows::with_placement_announced(&app, &label, || {
                if restore_windows {
                    if let Some(state) = app.try_state::<ShellState>() {
                        let _ = state.windows.restore(&app, &label);
                    }
                }
            });
            if label == main_label
                && lifecycle::show_on_ready_decision(
                    show_on_ready_explicit,
                    show_on_ready_module,
                    lifecycle::launched_in_background(),
                    lifecycle::qa_hidden(),
                )
            {
                if let Some(w) = app.get_webview_window(&label) {
                    // Through the guard: a never-show host gets nothing here.
                    let _ = crate::windows::show_window(
                        &app,
                        &w,
                        lifecycle::RevealSource::ShowOnReady,
                        windows::RevealStyle::ShowOnly,
                    );
                }
            }
        })
        .on_event(|app, event| {
            if app.try_state::<ShellState>().is_some() {
                lifecycle::run_event(app, event);
            }
        })
        .setup(move |app, _api| {
            let app = app.clone();
            if let Err(e) = guard_result {
                eprintln!("{e}");
                return Err(Box::new(e));
            }
            match gate_result {
                Ok(gate) => {
                    app.manage(gate);
                }
                Err(e) => {
                    eprintln!("{e}");
                    return Err(Box::new(e));
                }
            }
            setup(&app, cfg, nav, opener)
        });
    if harden_webviews {
        plugin = match script_scope {
            HardeningScript::AllFrames => {
                plugin.js_init_script_on_all_frames(hardening.script_for(&content_labels))
            }
            // WebView2 injects even main-frame-only scripts into child frames. Guard the body
            // as well as requesting native main-frame scope (WKWebView honors that scope).
            HardeningScript::TopFrameOnly => plugin.js_init_script(format!(
                "if (window.top !== window.self) {{ return; }}\n{}",
                hardening.script_for(&content_labels)
            )),
            HardeningScript::Off => plugin,
        };
    }
    for (scheme, policy) in streams {
        plugin = plugin.register_asynchronous_uri_scheme_protocol(
            scheme,
            move |ctx, request, responder| {
                let state = ctx.app_handle().state::<ShellState>();
                stream::serve(&state.broker, &policy, &request, move |response| {
                    responder.respond(response)
                });
            },
        );
    }
    if let Some((scheme, policy)) = site {
        plugin = plugin.register_asynchronous_uri_scheme_protocol(
            scheme,
            move |ctx, request, responder| {
                let state = ctx.app_handle().state::<ShellState>();
                site::serve_site_request(&state.broker, &policy, &request, move |response| {
                    responder.respond(response)
                });
            },
        );
    }
    plugin.build()
}

/// The claim token of the primary the single-instance plugin holds for the process lifetime.
struct PluginClaim(u64);

/// The startup error for an undecidable single-instance claim.
fn unavailable_error(why: &str) -> Box<dyn std::error::Error> {
    format!("rightkit-shell: single-instance could not be decided, refusing to start as a second primary: {why}").into()
}

/// An app id that cannot name the single-instance service: a programming error, so startup fails
/// with the id and the rule instead of running with single-instance protection silently off.
fn invalid_app_id_error(e: &instance::InvalidAppId) -> Box<dyn std::error::Error> {
    format!("rightkit-shell: single-instance cannot start, app id is not usable: {e}").into()
}

/// The window-visibility policy a configuration declares.
fn show_policy_of(cfg: &ShellBuilder) -> lifecycle::ShowPolicy {
    lifecycle::ShowPolicy {
        never_show: cfg.never_show,
        never_show_env: cfg.never_show_env.clone(),
        never_show_env_values: cfg.never_show_env_values.clone(),
        reveal_on_reopen: cfg.reveal_on_reopen && cfg.modules.contains(Module::RevealOnReopen),
    }
}

fn setup(
    app: &AppHandle<Wry>,
    cfg: ShellBuilder,
    nav: Arc<NavPolicy>,
    opener: Arc<dyn ExternalOpener>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 0. Never-show: Prohibited before arbitration, menus, tray or any window exists.
    let show_policy = show_policy_of(&cfg);
    lifecycle::enforce_never_show(app, &show_policy);
    // 1. Single-instance arbitration, before anything heavy.
    let mut single = String::from("disabled");
    let mode = lifecycle::instance_mode(
        cfg.single_instance,
        cfg.qa_instance_exempt,
        lifecycle::qa_hidden(),
    );
    if mode == lifecycle::InstanceMode::QaExempt {
        single = "qa-exempt".into();
    }
    if mode == lifecycle::InstanceMode::Arbitrate {
        let for_cb = app.clone();
        let custom = cfg.on_second_instance.clone();
        let decider = cfg.on_second_instance_reveal.clone();
        let version = app.package_info().version.to_string();
        let identifier = app.config().identifier.clone();
        match instance::arbitrate_with_legacy(
            &cfg.app_id,
            &version,
            Some(identifier.as_str()),
            Arc::new(move |s| {
                lifecycle::route_second_instance(
                    &for_cb,
                    custom.as_ref(),
                    &s.argv,
                    &s.cwd,
                    |app, argv, cwd| {
                        lifecycle::on_second_instance(app, argv, cwd, decider.as_ref())
                    },
                )
            }),
        ) {
            instance::Arbitration::Primary(h) => {
                // Held for the process lifetime (or until `SingleInstance::destroy`).
                let _claim = single_instance::hold(h);
                single = "primary".into();
            }
            instance::Arbitration::Secondary => std::process::exit(0),
            // Fail closed: setup errors abort startup instead of running as a second primary.
            instance::Arbitration::Unavailable(why) => return Err(unavailable_error(&why)),
            instance::Arbitration::InvalidAppId(e) => return Err(invalid_app_id_error(&e)),
        }
    }

    // 2. Shared state.
    let state_dir = match cfg.state_dir.clone() {
        Some(d) => d,
        None => app
            .path()
            .app_data_dir()
            .map(|d| d.join("shell"))
            .unwrap_or_else(|_| std::env::temp_dir().join(&cfg.app_id)),
    };
    let exe = std::env::current_exe().unwrap_or_default();
    let tray_runtime = cfg.tray.as_ref().map(|t| tray::TrayRuntime {
        icon: Mutex::new(None),
        ids: Mutex::new(Default::default()),
        show_label: t.show_label.clone(),
        quit_label: t.quit_label.clone(),
    });
    #[cfg(feature = "engine")]
    let engine = cfg
        .engine
        .clone()
        .map(|s| Arc::new(engine::EngineHost::new(s)));
    // A lifecycle-only host owns its cache: repair is off unless it asked for it.
    let cache_repair = if lifecycle::repair_cache_enabled(cfg.repair_cache, cfg.lifecycle_only) {
        let dir = cfg
            .cache_dir
            .clone()
            .or_else(|| app.path().app_cache_dir().ok());
        dir.and_then(|d| recovery::ensure_cache_dir(&d).ok().map(|o| (d, o)))
    } else {
        None
    };
    let presence = Arc::new(presence::PresenceDnd::new());
    if cfg.modules.contains(Module::Presence) {
        let (handle, on_presence) = (app.clone(), cfg.on_presence.clone());
        presence::start_loop(Arc::clone(&presence), cfg.app_name.clone(), move |s| {
            use tauri::Emitter;
            let _ = handle.emit(EVENT_PRESENCE, s);
            if let Some(cb) = &on_presence {
                cb(&handle, s);
            }
        });
    }
    let recovery = if cfg.modules.contains(Module::Recovery) {
        cfg.recovery
            .clone()
            .map(|spec| recovery::RecoveryStore::new(spec, &state_dir))
    } else {
        None
    };
    let stream_schemes: Vec<String> = if cfg.modules.contains(Module::Stream) {
        cfg.streams.iter().map(|(s, _)| s.clone()).collect()
    } else {
        Vec::new()
    };
    let site_scheme = if cfg.modules.contains(Module::Stream) {
        cfg.site.as_ref().map(|(s, _)| s.clone())
    } else {
        None
    };
    let autostart_spec = autostart::AutostartSpec {
        app_id: cfg.app_id.clone(),
        app_name: cfg.app_name.clone(),
        executable: exe,
        options: cfg.autostart.clone(),
        legacy_plugin_name: cfg
            .autostart
            .migrate_from_plugin_entry
            .then(|| app.package_info().name.clone()),
    };
    if cfg.autostart.migrate_from_plugin_entry {
        let legacy = app.package_info().name.clone();
        match autostart::migrate_plugin_entry(&autostart_spec, &legacy) {
            Ok(_) => {}
            // A failed migration must not stop startup: the old entry stays until the next launch.
            Err(e) => eprintln!(
                "rightkit-shell: could not migrate the tauri-plugin-autostart login entry: {e}"
            ),
        }
    }
    app.manage(ShellState {
        main_thread_id: std::thread::current().id(),
        app_id: cfg.app_id.clone(),
        app_name: cfg.app_name.clone(),
        main_label: cfg.main_label.clone(),
        nav,
        opener,
        frameless: cfg.frameless && cfg.modules.contains(Module::Frameless),
        close_to_tray: cfg.close_to_tray,
        hide_to_background: cfg.hide_to_background,
        on_visibility: cfg.on_visibility.clone(),
        activation: Default::default(),
        show_policy,
        quit: Arc::new(quit::QuitGate::new(cfg.quit_drain)),
        windows: Arc::new(window_state::WindowStateStore::new(state_dir)),
        deep: Default::default(),
        schemes: cfg.schemes.clone(),
        frontend_ready: AtomicBool::new(false),
        exiting: AtomicBool::new(false),
        traffic: Default::default(),
        autostart: autostart_spec,
        menu_items: Mutex::new(HashMap::new()),
        menu_rows: Mutex::new(Vec::new()),
        tray: tray_runtime,
        on_menu: cfg.on_menu.clone(),
        #[cfg(feature = "engine")]
        engine: engine.clone(),
        hosted: Mutex::new(None),
        single_instance: Mutex::new(single),
        broker: Arc::new(PathBroker::new()),
        notifications: notify::NotificationState::new(),
        presence,
        idle: cfg.idle_after.map(|d| {
            // `webview_idle(d)` replaces only the timeout; a provider set on the options is kept.
            let mut options = cfg.idle_options.unwrap_or_default();
            options.suspend_after = d;
            idle::IdleHost::new(d)
                .with_options(options)
                .with_on_error(cfg.idle_on_error.clone())
                .with_on_transition(cfg.idle_on_transition.clone())
        }),
        recovery,
        cache_repair: Mutex::new(cache_repair),
        stream_scheme: stream_schemes.first().cloned(),
        stream_schemes,
        site_scheme,
        site_policy: cfg
            .site
            .as_ref()
            .map(|(_, p)| p.clone())
            .unwrap_or_default(),
        modules: cfg.modules,
        clipboard: cfg.clipboard,
        clipboard_options: cfg.clipboard_options,
        clipboard_dir: clipboard::clipboard_temp_dir(&cfg.app_id),
    });

    // 3. Native menu.
    if let Some(spec) = &cfg.menu {
        let built = menu::build(
            app,
            &MenuSpec {
                app_name: cfg.app_name.clone(),
                ..spec.clone()
            },
        )?;
        let state = app.state::<ShellState>();
        *state.menu_rows.lock().unwrap_or_else(|e| e.into_inner()) = menu::describe(&built.menu);
        *state.menu_items.lock().unwrap_or_else(|e| e.into_inner()) = built.items;
        app.set_menu(built.menu)?;
        app.on_menu_event(|app, event| lifecycle::on_menu_event(app, event.id().as_ref()));
    }

    // 4. Tray.
    if let Some(spec) = &cfg.tray {
        let icon = tray::install(app, spec, &cfg.main_label)?;
        let state = app.state::<ShellState>();
        if let Some(rt) = &state.tray {
            *rt.icon.lock().unwrap_or_else(|e| e.into_inner()) = Some(icon);
        }
    }

    // 5. Engine, off the UI thread.
    #[cfg(feature = "engine")]
    if let Some(engine) = engine {
        let handle = app.clone();
        std::thread::Builder::new()
            .name("rk-shell-engine".into())
            .spawn(move || {
                use tauri::Emitter;
                let payload = match engine.start() {
                    Ok(hello) => serde_json::json!({"state": "ready", "hello": hello}),
                    Err(e) => {
                        serde_json::json!({"state": "error", "code": e.code, "message": e.message})
                    }
                };
                engine.set_lifecycle(payload.clone());
                let _ = handle.emit(
                    EVENT_ENGINE,
                    serde_json::json!({"call": "$lifecycle", "event": payload}),
                );
            })?;
    }

    // 6. Watchdog.
    if cfg.modules.contains(Module::Watchdog) {
        if let Some(w) = cfg.watchdog.clone() {
            watchdog::install(app.clone(), w);
        }
    }

    // 8 (queue only). Links on the launch argv wait for the frontend.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let state = app.state::<ShellState>();
    state
        .deep
        .push_all(argv.iter().map(String::as_str), &state.schemes);
    notify::set_default_app_id(&app.config().identifier);
    open_url::deliver(&argv);
    Ok(())
}

/// Build the `invoke_handler` with the shell commands plus the app's own.
/// `rightkit_shell::handler![my_command, other]`.
#[macro_export]
macro_rules! handler {
    ($($app_cmd:path),* $(,)?) => {
        ::tauri::generate_handler![
            $crate::commands::rk_shell_open_external,
            $crate::commands::rk_shell_ready,
            $crate::commands::rk_shell_status,
            $crate::commands::rk_shell_window,
            $crate::commands::rk_shell_open_window,
            $crate::commands::rk_shell_set_tray,
            $crate::commands::rk_shell_set_menu_item_enabled,
            $crate::commands::rk_shell_autostart,
            $crate::commands::rk_shell_clipboard_read,
            $crate::commands::rk_shell_clipboard_write,
            $crate::commands::rk_shell_engine_call,
            $crate::commands::rk_shell_traffic_lights,
            $crate::commands::rk_shell_quit,
            $crate::commands::rk_shell_finish_quit,
            $crate::commands::rk_shell_capabilities,
            $crate::commands::rk_shell_set_theme,
            $crate::commands::rk_shell_notify,
            $crate::commands::rk_shell_notify_status,
            $crate::commands::rk_shell_notify_clear,
            $crate::commands::rk_shell_badge,
            $crate::commands::rk_shell_set_dnd,
            $crate::commands::rk_shell_presence,
            $crate::commands::rk_shell_presence_prefs,
            $crate::commands::rk_shell_presence_probe,
            $crate::commands::rk_shell_presence_evaluate,
            $crate::commands::rk_shell_idle_status,
            $crate::commands::rk_shell_idle_note,
            $crate::commands::rk_shell_path_require,
            $crate::commands::rk_shell_path_revoke,
            $crate::commands::rk_shell_path_grant_recent,
            $crate::commands::rk_shell_path_regrant_saved,
            $crate::commands::rk_shell_stream_grant,
            $crate::commands::rk_shell_recovery_save,
            $crate::commands::rk_shell_recovery_load,
            $crate::commands::rk_shell_recovery_clear,
            $crate::commands::rk_shell_clipboard_item,
            $crate::commands::rk_shell_site_grant,
            $crate::commands::rk_shell_site_revoke
            $(, $app_cmd)*
        ]
    };
}
