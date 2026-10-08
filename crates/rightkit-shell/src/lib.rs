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
#[cfg(feature = "engine")]
pub mod engine;
pub mod error;
pub mod frameless;
pub mod harden;
pub mod idle;
pub mod instance;
pub mod lifecycle;
pub mod menu;
pub mod nav;
pub mod notify;
pub mod open_url;
pub mod opener;
pub mod presence;
pub mod recovery;
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

pub use broker::PathBroker;
pub use error::ShellError;
pub use harden::{Devtools, Hardening};
pub use menu::{MenuEntry, MenuGroup, MenuSpec};
pub use nav::{NavDecision, NavPolicy};
pub use opener::{ExternalOpener, SystemOpener};
pub use presence::{AutoPrefs, PresenceState};
pub use recovery::RecoverySpec;
pub use stream::StreamPolicy;
pub use tray::{LeftClick, TraySpec};
pub use watchdog::{StallReport, WatchdogConfig};

pub const EVENT_MENU: &str = "rightkit-shell://menu";
pub const EVENT_TRAY: &str = "rightkit-shell://tray";
pub const EVENT_DEEP_LINK: &str = "rightkit-shell://deep-link";
pub const EVENT_SECOND_INSTANCE: &str = "rightkit-shell://second-instance";
pub const EVENT_STALL: &str = "rightkit-shell://stall";
pub const EVENT_ENGINE: &str = "rightkit-shell://engine";
pub const EVENT_WINDOW: &str = "rightkit-shell://window";
pub const EVENT_IDLE: &str = "rightkit-shell://idle";
pub const EVENT_PRESENCE: &str = "rightkit-shell://presence";

type MenuCallback = Arc<dyn Fn(&AppHandle<Wry>, &str) + Send + Sync>;
type PresenceCallback = Arc<dyn Fn(&AppHandle<Wry>, PresenceState) + Send + Sync>;

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
    /// Scheme registered with [`ShellBuilder::stream_protocol`].
    pub stream_scheme: Option<String>,
}

/// Called on the first instance when a later launch forwards its argv and working directory.
pub type SecondInstanceCallback = Arc<dyn Fn(&AppHandle<Wry>, &[String], &str) + Send + Sync>;

pub struct ShellBuilder {
    app_id: String,
    app_name: String,
    main_label: String,
    hardening: Hardening,
    nav: NavPolicy,
    opener: Arc<dyn ExternalOpener>,
    single_instance: bool,
    close_to_tray: bool,
    frameless: bool,
    show_on_ready: bool,
    state_dir: Option<PathBuf>,
    schemes: Vec<String>,
    menu: Option<MenuSpec>,
    tray: Option<TraySpec>,
    watchdog: Option<WatchdogConfig>,
    on_menu: Option<MenuCallback>,
    on_second_instance: Option<SecondInstanceCallback>,
    stream: Option<(String, StreamPolicy)>,
    recovery: Option<RecoverySpec>,
    idle_after: Option<std::time::Duration>,
    cache_dir: Option<PathBuf>,
    repair_cache: bool,
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
            nav: NavPolicy::default(),
            opener: Arc::new(SystemOpener),
            single_instance: true,
            close_to_tray: false,
            frameless: false,
            show_on_ready: true,
            state_dir: None,
            schemes: Vec::new(),
            menu: None,
            tray: None,
            watchdog: Some(WatchdogConfig::default()),
            on_menu: None,
            on_second_instance: None,
            stream: None,
            recovery: None,
            idle_after: None,
            cache_dir: None,
            repair_cache: true,
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
    pub fn nav_policy(mut self, p: NavPolicy) -> Self {
        self.nav = p;
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
        self
    }
    /// Frameless chrome for windows created through `windows::builder`/`open` (see `frameless`).
    pub fn frameless(mut self, on: bool) -> Self {
        self.frameless = on;
        self
    }
    pub fn show_on_ready(mut self, on: bool) -> Self {
        self.show_on_ready = on;
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
        self.stream = Some((scheme.to_ascii_lowercase(), policy));
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
    /// App cache directory to keep usable (default: Tauri's app cache dir).
    pub fn cache_dir(mut self, dir: PathBuf) -> Self {
        self.cache_dir = Some(dir);
        self
    }
    /// Repair a dangling symlink / stray file at the cache directory on startup (default on).
    pub fn repair_cache(mut self, on: bool) -> Self {
        self.repair_cache = on;
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
    /// The single Tauri plugin that installs the whole shell. Add it FIRST so single-instance
    /// arbitration runs before any other plugin does work.
    pub fn plugin(self) -> TauriPlugin<Wry> {
        let cfg = self.cfg;
        let nav = Arc::new(cfg.nav.clone());
        let opener = Arc::clone(&cfg.opener);
        let hardening = cfg.hardening.clone();
        let script = cfg.hardening.script();
        let (nav_hook, opener_hook) = (Arc::clone(&nav), Arc::clone(&opener));
        let hardening_for_views = hardening.clone();
        let show_on_ready = cfg.show_on_ready;
        let main_label = cfg.main_label.clone();
        let stream = cfg.stream.clone();

        let mut plugin = PluginBuilder::<Wry>::new("rightkit-shell")
            .js_init_script_on_all_frames(script)
            .on_navigation(move |_webview, url| match nav_hook.decide(url) {
                NavDecision::Allow => true,
                NavDecision::External { url } => {
                    let _ = opener::open_checked(opener_hook.as_ref(), &url);
                    false
                }
                NavDecision::Block => false,
            })
            .on_webview_ready(move |webview| {
                harden::apply_native(&webview, &hardening_for_views);
                let app = webview.app_handle().clone();
                let label = webview.label().to_owned();
                if let Some(state) = app.try_state::<ShellState>() {
                    let _ = state.windows.restore(&app, &label);
                }
                if show_on_ready
                    && label == main_label
                    && !lifecycle::launched_in_background()
                    && !lifecycle::qa_hidden()
                {
                    if let Some(w) = app.get_webview_window(&label) {
                        let _ = crate::windows::show_background_aware(&w);
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
                setup(&app, cfg, nav, opener)
            });
        if let Some((scheme, policy)) = stream {
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
        plugin.build()
    }
}

fn setup(
    app: &AppHandle<Wry>,
    cfg: ShellBuilder,
    nav: Arc<NavPolicy>,
    opener: Arc<dyn ExternalOpener>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Single-instance arbitration, before anything heavy.
    let mut hosted = None;
    let mut single = String::from("disabled");
    if cfg.single_instance {
        let for_cb = app.clone();
        let custom = cfg.on_second_instance.clone();
        let version = app.package_info().version.to_string();
        match instance::arbitrate(
            &cfg.app_id,
            &version,
            Arc::new(move |s| {
                lifecycle::route_second_instance(
                    &for_cb,
                    custom.as_ref(),
                    &s.argv,
                    &s.cwd,
                    lifecycle::on_second_instance,
                )
            }),
        ) {
            instance::Arbitration::Primary(h) => {
                hosted = Some(h);
                single = "primary".into();
            }
            instance::Arbitration::Secondary => std::process::exit(0),
            instance::Arbitration::Unavailable(why) => single = format!("unavailable: {why}"),
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
    let cache_repair = if cfg.repair_cache {
        let dir = cfg
            .cache_dir
            .clone()
            .or_else(|| app.path().app_cache_dir().ok());
        dir.and_then(|d| recovery::ensure_cache_dir(&d).ok().map(|o| (d, o)))
    } else {
        None
    };
    let presence = Arc::new(presence::PresenceDnd::new());
    {
        let (handle, on_presence) = (app.clone(), cfg.on_presence.clone());
        presence::start_loop(Arc::clone(&presence), cfg.app_name.clone(), move |s| {
            use tauri::Emitter;
            let _ = handle.emit(EVENT_PRESENCE, s);
            if let Some(cb) = &on_presence {
                cb(&handle, s);
            }
        });
    }
    let recovery = cfg
        .recovery
        .clone()
        .map(|spec| recovery::RecoveryStore::new(spec, &state_dir));
    app.manage(ShellState {
        main_thread_id: std::thread::current().id(),
        app_id: cfg.app_id.clone(),
        app_name: cfg.app_name.clone(),
        main_label: cfg.main_label.clone(),
        nav,
        opener,
        frameless: cfg.frameless,
        close_to_tray: cfg.close_to_tray,
        windows: Arc::new(window_state::WindowStateStore::new(state_dir)),
        deep: Default::default(),
        schemes: cfg.schemes.clone(),
        frontend_ready: AtomicBool::new(false),
        exiting: AtomicBool::new(false),
        traffic: Default::default(),
        autostart: autostart::AutostartSpec {
            app_id: cfg.app_id.clone(),
            app_name: cfg.app_name.clone(),
            executable: exe,
        },
        menu_items: Mutex::new(HashMap::new()),
        menu_rows: Mutex::new(Vec::new()),
        tray: tray_runtime,
        on_menu: cfg.on_menu.clone(),
        #[cfg(feature = "engine")]
        engine: engine.clone(),
        hosted: Mutex::new(hosted),
        single_instance: Mutex::new(single),
        broker: Arc::new(PathBroker::new()),
        notifications: notify::NotificationState::new(),
        presence,
        idle: cfg.idle_after.map(idle::IdleHost::new),
        recovery,
        cache_repair: Mutex::new(cache_repair),
        stream_scheme: cfg.stream.as_ref().map(|(s, _)| s.clone()),
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
    if let Some(w) = cfg.watchdog.clone() {
        watchdog::install(app.clone(), w);
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
            $crate::commands::rk_shell_recovery_clear
            $(, $app_cmd)*
        ]
    };
}
