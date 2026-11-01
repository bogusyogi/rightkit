//! What the launcher needs from the application that embeds it: translation, HTTPS reads, event
//! logging, the file-index scope and a notification when currency rates arrive. The application
//! installs one [`Host`] at start-up; until it does, the launcher uses English text, no network and
//! no logging.

use std::sync::OnceLock;
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

/// The text for `key`, translated by the host when one is installed.
pub fn t(key: &str) -> String {
    HOST.get()
        .map_or_else(|| key.to_string(), |host| host.translate(key))
}

/// The translated template for `key` with each `{}` filled from `args` in order. A placeholder
/// with no argument is left as written.
pub fn tf(key: &str, args: &[&str]) -> String {
    fill(&t(key), args)
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
