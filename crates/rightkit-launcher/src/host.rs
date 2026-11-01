//! What the launcher needs from the application that embeds it: translation, HTTPS reads, event
//! logging, the file-index scope and a notification when currency rates arrive. The application
//! installs one [`Host`] at start-up; until it does, the launcher uses English text, no network and
//! no logging.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// A completed HTTPS read.
pub struct HttpReply {
    /// The HTTP status code.
    pub status: u16,
    /// The response body.
    pub body: Vec<u8>,
}

/// Which folders and drives the file search may walk: whole disk or not, the volumes and the
/// excluded patterns (`None` means the built-in defaults).
pub type IndexScope = (bool, Vec<String>, Option<Vec<String>>);

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

/// Services the embedding application provides.
pub trait Host: Send + Sync + 'static {
    /// The text for `key` in the user's language; the key itself when there is no translation.
    fn translate(&self, key: &str) -> String {
        key.to_string()
    }

    /// A blocking HTTPS GET of `https://host` + `path`; `None` when the request could not be made.
    fn https_get(
        &self,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Option<HttpReply>;

    /// Records an informational event with `fields`.
    fn log_info(&self, _event: &str, _fields: &[(&str, &str)]) {}

    /// The file-search scope the user chose.
    fn file_index_scope(&self) -> IndexScope {
        (false, Vec::new(), None)
    }

    /// Whether the named user switch is on.
    fn switch_on(&self, _key: &str) -> bool {
        false
    }

    /// The user's launcher items as the JSON text [`crate::config::parse`] reads.
    fn launcher_config_json(&self) -> String {
        String::new()
    }

    /// Starts or stops the keyboard hook that feeds snippet expansion.
    fn snippets_need_hook(&self, _on: bool) {}

    /// Sends `events` as one tagged batch; `ctx` names the sender in the log.
    #[cfg(windows)]
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

    /// Starts holding keys typed before the panel owns the keyboard.
    fn capture_begin(&self) {}

    /// The held keys, as text.
    fn capture_take(&self) -> String {
        String::new()
    }

    /// Stops holding keys.
    fn capture_release(&self) {}

    /// Whether the launcher chord needs the low-level keyboard hook.
    fn launcher_needs_hook(&self, _on: bool) {}

    /// The answer to "who gets Alt+Space" kept from an earlier run: `"pulse"`-style tokens are the
    /// host's own.
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
    #[cfg(windows)]
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

    /// One account's usage as a line of text; `provider` 0 is the first, 1 the second.
    fn usage_line(&self, _provider: usize) -> String {
        String::new()
    }

    /// File name of the per-folder ignore list the file search honours.
    fn ignore_file_name(&self) -> String {
        ".rightkitignore".to_string()
    }

    /// New currency rates were saved; an open panel may refresh.
    fn rates_ready(&self) {}
}

static HOST: OnceLock<Box<dyn Host>> = OnceLock::new();

/// Installs the host. Returns `false` when one was already installed (the first one stays).
pub fn install(host: Box<dyn Host>) -> bool {
    HOST.set(host).is_ok()
}

/// The text for `key`, translated by the host when one is installed. Each distinct translation is
/// kept for the life of the process (the catalog is finite), so the result is `'static`.
pub fn t(key: &str) -> &'static str {
    static KEPT: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let text = HOST
        .get()
        .map_or_else(|| key.to_string(), |host| host.translate(key));
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(found) = kept.get(text.as_str()) {
        return found;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    kept.insert(leaked);
    leaked
}

/// The translated template for `key` with each `{}` filled from `args` in order. A placeholder
/// with no argument is left as written.
pub fn tf(key: &str, args: &[&str]) -> String {
    fill(t(key), args)
}

fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut rest = template;
    while let Some(open) = rest.find("{}") {
        out.push_str(&rest[..open]);
        match args.get(next) {
            Some(arg) => out.push_str(arg),
            None => out.push_str("{}"),
        }
        next += 1;
        rest = &rest[open + 2..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn https_get(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Option<HttpReply> {
    HOST.get()?.https_get(host, path, headers, timeout)
}

pub(crate) fn log_info(event: &str, fields: &[(&str, &str)]) {
    if let Some(host) = HOST.get() {
        host.log_info(event, fields);
    }
}

pub(crate) fn file_index_scope() -> IndexScope {
    HOST.get()
        .map_or((false, Vec::new(), None), |host| host.file_index_scope())
}

pub(crate) fn ignore_file_name() -> String {
    HOST.get().map_or_else(
        || ".rightkitignore".to_string(),
        |host| host.ignore_file_name(),
    )
}

pub(crate) fn rates_ready() {
    if let Some(host) = HOST.get() {
        host.rates_ready();
    }
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(windows)]
pub(crate) fn switch_on(key: &str) -> bool {
    HOST.get().is_some_and(|host| host.switch_on(key))
}

#[cfg(windows)]
pub(crate) fn launcher_config_json() -> String {
    HOST.get()
        .map(|host| host.launcher_config_json())
        .unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn snippets_need_hook(on: bool) {
    if let Some(host) = HOST.get() {
        host.snippets_need_hook(on);
    }
}

#[cfg(windows)]
pub(crate) fn send_input(events: &[rightkit_automation::win::input::RawInput], ctx: &str) {
    if let Some(host) = HOST.get() {
        host.send_input(events, ctx);
    }
}

#[cfg(windows)]
pub(crate) fn paste() {
    if let Some(host) = HOST.get() {
        host.paste();
    }
}

#[cfg(windows)]
pub(crate) fn snippet_text(body: &str) -> String {
    HOST.get()
        .map_or_else(|| body.to_string(), |host| host.snippet_text(body))
}

#[cfg(windows)]
pub(crate) fn log_error(op: &str, error: &windows::core::Error, ctx: &str) {
    log_info(ctx, &[("operation", op), ("error", &error.message())]);
}

#[cfg(windows)]
pub(crate) fn engine_running() -> bool {
    HOST.get().is_some_and(|host| host.engine_running())
}

#[cfg(windows)]
pub(crate) fn engine_poll(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    HOST.get().map_or_else(
        || Err("no host".to_string()),
        |host| host.engine_poll(method, params),
    )
}

#[cfg(windows)]
pub(crate) fn engine_call(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    HOST.get().map_or_else(
        || Err("no host".to_string()),
        |host| host.engine_call(method, params),
    )
}

#[cfg(windows)]
pub(crate) fn window_title() -> String {
    HOST.get()
        .map_or_else(|| "RightKit".to_string(), |host| host.window_title())
}

/// The part of `text` that fills the `{}` of the translated template for `key`, when it matches.
pub fn arg<'a>(key: &str, text: &'a str) -> Option<&'a str> {
    let template = t(key);
    let open = template.find('{')?;
    let close = open + template[open..].find('}')?;
    text.strip_prefix(&template[..open])?
        .strip_suffix(&template[close + 1..])
}

#[cfg(windows)]
pub(crate) fn data_dir() -> Option<std::path::PathBuf> {
    HOST.get()?.data_dir()
}

#[cfg(windows)]
pub(crate) fn wake_ui() {
    if let Some(host) = HOST.get() {
        host.wake_ui();
    }
}

#[cfg(windows)]
pub(crate) fn capture_begin() {
    if let Some(host) = HOST.get() {
        host.capture_begin();
    }
}

#[cfg(windows)]
pub(crate) fn capture_take() -> String {
    HOST.get()
        .map(|host| host.capture_take())
        .unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn capture_release() {
    if let Some(host) = HOST.get() {
        host.capture_release();
    }
}

#[cfg(windows)]
pub(crate) fn launcher_needs_hook(on: bool) {
    if let Some(host) = HOST.get() {
        host.launcher_needs_hook(on);
    }
}

#[cfg(windows)]
pub(crate) fn launcher_hotkey_conflict() -> Option<String> {
    HOST.get()?.launcher_hotkey_conflict()
}

#[cfg(windows)]
pub(crate) fn set_live_hotkey_conflict(answer: Option<&str>) {
    if let Some(host) = HOST.get() {
        host.set_live_hotkey_conflict(answer);
    }
}

#[cfg(windows)]
pub(crate) fn launcher_ask_target() -> &'static str {
    HOST.get()
        .map_or("inLauncher", |host| host.launcher_ask_target())
}

#[cfg(windows)]
pub(crate) fn launcher_web_engine() -> String {
    HOST.get()
        .map(|host| host.launcher_web_engine())
        .unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn launcher_hotkey() -> LauncherHotkey {
    HOST.get()
        .map_or(LauncherHotkey::AltSpace, |host| host.launcher_hotkey())
}

#[cfg(windows)]
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

#[cfg(windows)]
pub(crate) fn open_strip_at(id: &str) {
    if let Some(host) = HOST.get() {
        host.open_strip_at(id);
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
pub(crate) fn drives() -> Vec<Drive> {
    HOST.get().map(|host| host.drives()).unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn open_hub(page: &str) -> bool {
    HOST.get().is_some_and(|host| host.open_hub(page))
}

#[cfg(windows)]
pub(crate) fn copy_text(text: &str) -> bool {
    HOST.get().is_some_and(|host| host.copy_text(text))
}

#[cfg(windows)]
pub(crate) fn usage_line(provider: usize) -> String {
    HOST.get()
        .map(|host| host.usage_line(provider))
        .unwrap_or_default()
}
