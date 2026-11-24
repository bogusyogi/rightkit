//! Services that snippet expansion and launcher chat receive from the application.

pub(crate) use rightkit_launcher::host::{log_info, t};
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

/// Application services for Windows launcher behavior.
pub trait PanelHost: Send + Sync + 'static {
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

    /// Calls an engine method, waiting at most `limit`.
    fn engine_call_within(
        &self,
        _method: &str,
        _params: serde_json::Value,
        _limit: Duration,
    ) -> Result<serde_json::Value, EngineFailure> {
        Err(EngineFailure::Unreachable("no engine".to_string()))
    }
}

static HOST: OnceLock<Box<dyn PanelHost>> = OnceLock::new();

/// Installs the host. Returns false if one is already installed.
pub fn install(host: Box<dyn PanelHost>) -> bool {
    HOST.set(host).is_ok()
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
