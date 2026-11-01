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
