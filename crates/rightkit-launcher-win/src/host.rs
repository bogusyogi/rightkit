//! What the Win32 launcher panel needs from the application that embeds it: settings, the
//! engine, the hub, the clipboard strip, drives and the application's own pages and
//! readings. The application installs one [`PanelHost`] at start-up (and the core's
//! [`rightkit_launcher::host::Host`] for translation, HTTPS and logging); until it does, every
//! switch is off and there is no engine, hub or saved state.
//!
//! Nothing here names a product: the product name, the application's pages and its usage rows
//! are the host's.

pub use rightkit_launcher::host::{arg, log_info, t, tf};
use std::sync::OnceLock;
use std::time::Duration;

/// What a failed engine call was.
#[derive(Debug)]
pub enum EngineFailure {
    /// Nothing ran: no engine answered. Safe to retry later.
    Unreachable(String),
    /// The request was sent but no answer arrived.
    Unknown(String),
    /// The engine refused, with its stable code and message.
    Refused {
        /// The engine's stable code.
        code: String,
        /// The engine's message.
        message: String,
    },
}

/// The chord that opens the launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherHotkey {
    /// Alt+Space.
    AltSpace,
    /// Ctrl+Space.
    CtrlSpace,
}

impl LauncherHotkey {
    /// The setting value.
    pub fn as_str(self) -> &'static str {
        match self {
            LauncherHotkey::AltSpace => "altSpace",
            LauncherHotkey::CtrlSpace => "ctrlSpace",
        }
    }

    /// Reads a setting value.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "altSpace" => Some(LauncherHotkey::AltSpace),
            "ctrlSpace" => Some(LauncherHotkey::CtrlSpace),
            _ => None,
        }
    }
}

/// An Alt+key press offered to the launcher by the keyboard hook.
#[derive(Clone, Copy, Debug)]
pub struct AltChord {
    /// The virtual-key code.
    pub vk: u32,
    /// Shift is held.
    pub shift: bool,
    /// Auto-repeat of a key already reported.
    pub repeat: bool,
}

/// A drive the storage rows describe.
#[derive(Clone, Debug)]
pub struct Drive {
    /// Root path such as `C:\`.
    pub root: String,
    /// Capacity in bytes.
    pub total: u64,
    /// Free bytes.
    pub free: u64,
    /// The system drive.
    pub system: bool,
    /// A removable or USB drive.
    pub external: bool,
}

impl Drive {
    /// The used share of the capacity, 0.0 to 1.0; 0.0 for a drive that reports no capacity.
    #[must_use]
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let used = self.total.saturating_sub(self.free);
        (used as f64 / self.total as f64) as f32
    }
}

/// One of the application's own pages the panel offers when its name is typed ("Open Storage").
#[derive(Clone, Debug)]
pub struct AppPage {
    /// The row's title, already translated.
    pub title: String,
    /// The page [`PanelHost::open_hub`] opens.
    pub page: &'static str,
    /// The quiet line under the title, already translated.
    pub note: String,
    /// The row's icon.
    pub glyph: PageGlyph,
}

/// The icon of an application row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageGlyph {
    /// A drive.
    #[default]
    Drive,
    /// Sparkles (cleanup).
    Sparkles,
    /// A monitor.
    Monitor,
    /// A grid (apps).
    Grid,
    /// Sliders (settings).
    Sliders,
    /// A gauge (a usage reading).
    Gauge,
}

/// A reading the application offers when its title, or the start of one of `words`, is typed
/// (such as an account's usage).
#[derive(Clone, Debug)]
pub struct UsageRow {
    /// The row's title, already translated.
    pub title: String,
    /// Words whose start finds the row.
    pub words: Vec<String>,
    /// The reading, as a line of text.
    pub line: String,
    /// The page [`PanelHost::open_hub`] opens when the row is chosen.
    pub page: &'static str,
}

/// The application's last saved cleanup scan, for the cleanup row.
#[derive(Clone, Copy, Debug)]
pub struct CleanupSummary {
    /// Items found.
    pub items: usize,
    /// Bytes that could be freed safely.
    pub safe_bytes: f64,
    /// When the scan ran, seconds since the Unix epoch.
    pub scanned_at: f64,
}

/// Services the embedding application provides to the Win32 panel.
pub trait PanelHost: Send + Sync + 'static {
    /// The product name in messages (`"Use Alt+Space for <name>"`).
    fn product_name(&self) -> String {
        "this app".to_string()
    }

    /// The heading of the section that holds the application's own rows.
    fn app_section_title(&self) -> String {
        "APP".to_string()
    }

    /// The application's pages the panel offers by name (at most four are shown).
    fn app_pages(&self) -> Vec<AppPage> {
        Vec::new()
    }

    /// The application's readings the panel offers by name.
    fn usage_rows(&self) -> Vec<UsageRow> {
        Vec::new()
    }

    /// The application's last saved cleanup scan; `None` when it has none.
    fn cleanup_summary(&self) -> Option<CleanupSummary> {
        None
    }

    /// The page the storage rows open, and the page the cleanup row opens; `None` shows no
    /// storage or cleanup rows.
    fn storage_pages(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Whether the named user switch is on.
    fn switch_on(&self, _key: &str) -> bool {
        false
    }

    /// The user's launcher items as the JSON text [`rightkit_launcher::config::parse`] reads.
    fn launcher_config_json(&self) -> String {
        String::new()
    }

    /// Starts or stops the keyboard hook that feeds snippet expansion.
    fn snippets_need_hook(&self, _on: bool) {}

    /// Sends `events` as one tagged batch; `ctx` names the sender in the log.
    fn send_input(&self, events: &[rightkit_automation::win::input::RawInput], ctx: &str);

    /// Sends Ctrl+V with no key held.
    fn paste(&self) {}

    /// A snippet body with `{clipboard}` and `{date}` filled and `{argument}` empty.
    fn snippet_text(&self, body: &str) -> String {
        body.to_string()
    }

    /// Whether the always-on agent's engine is running.
    fn engine_running(&self) -> bool {
        false
    }

    /// Asks the engine for a read-only status; `Err` carries the reason when it did not answer.
    fn engine_poll(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("no engine".to_string())
    }

    /// Calls an engine method; `Err` carries the reason.
    fn engine_call(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("no engine".to_string())
    }

    /// Title given to the application's overlay windows.
    fn window_title(&self) -> String {
        "RightKit".to_string()
    }

    /// The application's data folder for the launcher; `None` disables saved state.
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Asks the panel's window thread to wake (the hub's counts follow).
    fn wake_ui(&self) {}

    /// Whether the launcher chord needs the low-level keyboard hook.
    fn launcher_needs_hook(&self, _on: bool) {}

    /// The answer to "who gets Alt+Space" kept from an earlier run: `"app"` (this application
    /// takes it), `"other"` (the other program keeps it) or nothing.
    fn launcher_hotkey_conflict(&self) -> Option<String> {
        None
    }

    /// Keeps the answer to the hotkey question for this run.
    fn set_live_hotkey_conflict(&self, _answer: Option<&str>) {}

    /// Where an Ask row goes: `inLauncher` or another target the host names.
    fn launcher_ask_target(&self) -> &'static str {
        "inLauncher"
    }

    /// The web search engine the user chose.
    fn launcher_web_engine(&self) -> String {
        String::new()
    }

    /// The chord that opens the launcher.
    fn launcher_hotkey(&self) -> LauncherHotkey {
        LauncherHotkey::AltSpace
    }

    /// Calls an engine method, waiting at most `limit`.
    fn engine_call_within(
        &self,
        _method: &str,
        _params: serde_json::Value,
        _limit: Duration,
    ) -> Result<serde_json::Value, EngineFailure> {
        Err(EngineFailure::Unreachable("no engine".to_string()))
    }

    /// Opens the clipboard strip on item `id`.
    fn open_strip_at(&self, _id: &str) {}

    /// Draws the outline mark for a clipboard item kind about `centre`, `size` pixels square.
    fn kind_mark(
        &self,
        _canvas: &mut rightkit_win_ui::canvas::Canvas,
        _kind: rightkit_clipboard_history::model::Kind,
        _centre: (f32, f32),
        _size: f32,
        _ink: (u32, f32),
    ) {
    }

    /// The fixed and removable drives, for the storage rows.
    fn drives(&self) -> Vec<Drive> {
        Vec::new()
    }

    /// Opens a page of the application's hub.
    fn open_hub(&self, _page: &str) -> bool {
        false
    }

    /// Puts `text` on the clipboard.
    fn copy_text(&self, _text: &str) -> bool {
        false
    }
}

static HOST: OnceLock<Box<dyn PanelHost>> = OnceLock::new();

/// Installs the panel host. Returns `false` when one was already installed (the first one stays).
pub fn install(host: Box<dyn PanelHost>) -> bool {
    HOST.set(host).is_ok()
}

pub(crate) fn product_name() -> String {
    HOST.get()
        .map_or_else(|| "this app".to_string(), |host| host.product_name())
}

pub(crate) fn app_section_title() -> &'static str {
    t(&HOST
        .get()
        .map_or_else(|| "APP".to_string(), |host| host.app_section_title()))
}

pub(crate) fn app_pages() -> Vec<AppPage> {
    HOST.get().map(|host| host.app_pages()).unwrap_or_default()
}

pub(crate) fn usage_rows() -> Vec<UsageRow> {
    HOST.get().map(|host| host.usage_rows()).unwrap_or_default()
}

pub(crate) fn cleanup_summary() -> Option<CleanupSummary> {
    HOST.get()?.cleanup_summary()
}

pub(crate) fn storage_pages() -> Option<(&'static str, &'static str)> {
    HOST.get()?.storage_pages()
}

pub(crate) fn switch_on(key: &str) -> bool {
    HOST.get().is_some_and(|host| host.switch_on(key))
}

pub(crate) fn launcher_config_json() -> String {
    HOST.get()
        .map(|host| host.launcher_config_json())
        .unwrap_or_default()
}

pub(crate) fn snippets_need_hook(on: bool) {
    if let Some(host) = HOST.get() {
        host.snippets_need_hook(on);
    }
}

pub(crate) fn send_input(events: &[rightkit_automation::win::input::RawInput], ctx: &str) {
    if let Some(host) = HOST.get() {
        host.send_input(events, ctx);
    }
}

pub(crate) fn paste() {
    if let Some(host) = HOST.get() {
        host.paste();
    }
}

pub(crate) fn snippet_text(body: &str) -> String {
    HOST.get()
        .map_or_else(|| body.to_string(), |host| host.snippet_text(body))
}

pub(crate) fn log_error(op: &str, error: &windows::core::Error, ctx: &str) {
    log_info(ctx, &[("operation", op), ("error", &error.message())]);
}

pub(crate) fn engine_running() -> bool {
    HOST.get().is_some_and(|host| host.engine_running())
}

pub(crate) fn engine_poll(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    HOST.get().map_or_else(
        || Err("no host".to_string()),
        |host| host.engine_poll(method, params),
    )
}

pub(crate) fn engine_call(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    HOST.get().map_or_else(
        || Err("no host".to_string()),
        |host| host.engine_call(method, params),
    )
}

pub(crate) fn window_title() -> String {
    HOST.get()
        .map_or_else(|| "RightKit".to_string(), |host| host.window_title())
}

pub(crate) fn data_dir() -> Option<std::path::PathBuf> {
    HOST.get()?.data_dir()
}

pub(crate) fn wake_ui() {
    if let Some(host) = HOST.get() {
        host.wake_ui();
    }
}

pub(crate) fn launcher_needs_hook(on: bool) {
    if let Some(host) = HOST.get() {
        host.launcher_needs_hook(on);
    }
}

pub(crate) fn launcher_hotkey_conflict() -> Option<String> {
    HOST.get()?.launcher_hotkey_conflict()
}

pub(crate) fn set_live_hotkey_conflict(answer: Option<&str>) {
    if let Some(host) = HOST.get() {
        host.set_live_hotkey_conflict(answer);
    }
}

pub(crate) fn launcher_ask_target() -> &'static str {
    HOST.get()
        .map_or("inLauncher", |host| host.launcher_ask_target())
}

pub(crate) fn launcher_web_engine() -> String {
    HOST.get()
        .map(|host| host.launcher_web_engine())
        .unwrap_or_default()
}

pub(crate) fn launcher_hotkey() -> LauncherHotkey {
    HOST.get()
        .map_or(LauncherHotkey::AltSpace, |host| host.launcher_hotkey())
}

pub(crate) fn engine_call_within(
    method: &str,
    params: serde_json::Value,
    limit: Duration,
) -> Result<serde_json::Value, EngineFailure> {
    HOST.get().map_or_else(
        || Err(EngineFailure::Unreachable("no host".to_string())),
        |host| host.engine_call_within(method, params, limit),
    )
}

pub(crate) fn open_strip_at(id: &str) {
    if let Some(host) = HOST.get() {
        host.open_strip_at(id);
    }
}

pub(crate) fn kind_mark(
    canvas: &mut rightkit_win_ui::canvas::Canvas,
    kind: rightkit_clipboard_history::model::Kind,
    centre: (f32, f32),
    size: f32,
    ink: (u32, f32),
) {
    if let Some(host) = HOST.get() {
        host.kind_mark(canvas, kind, centre, size, ink);
    }
}

pub(crate) fn drives() -> Vec<Drive> {
    HOST.get().map(|host| host.drives()).unwrap_or_default()
}

pub(crate) fn open_hub(page: &str) -> bool {
    HOST.get().is_some_and(|host| host.open_hub(page))
}

pub(crate) fn copy_text(text: &str) -> bool {
    HOST.get().is_some_and(|host| host.copy_text(text))
}
