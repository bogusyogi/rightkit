//! The host seams of the digest and Reddit reader modules: the channel store, the calendar
//! writes a digest apply makes, and the endpoint settings. A host registers one [`ResearchHost`]
//! at start-up with [`set_host`].
use std::path::Path;
use std::sync::OnceLock;

use rightkit_db::rusqlite::Connection;
use rightkit_social_publish::http::EndpointSettings;
use serde_json::Value;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);

/// What the digest and Reddit reader modules need from the host app.
pub trait ResearchHost: Send + Sync {
    /// Opens the channel store (schema migrated) under the data root.
    fn open_store(&self, root: &Path) -> Result<Connection, Fail>;
    /// Writes a calendar brief (`brief.put`); returns the stored brief with its `revision`.
    fn brief_put(&self, root: &Path, input: Value) -> Result<Value, Fail>;
    /// Reads a calendar entry; `not_found` when there is none.
    fn calendar_get(&self, root: &Path, input: Value) -> Result<Value, Fail>;
    /// Writes a draft calendar entry.
    fn calendar_put(&self, root: &Path, input: Value) -> Result<Value, Fail>;
    /// The caption prefix that marks a placeholder caption not yet approved.
    fn placeholder_caption_prefix(&self) -> &str {
        "PLACEHOLDER, replace before approval."
    }
    /// The endpoint settings used to validate the Reddit reader base URL.
    fn endpoint_settings(&self) -> EndpointSettings;
}

static HOST: OnceLock<Box<dyn ResearchHost>> = OnceLock::new();

/// Register the host once. Returns `false` when one is already registered.
pub fn set_host(host: Box<dyn ResearchHost>) -> bool {
    HOST.set(host).is_ok()
}

pub(crate) fn host() -> Result<&'static dyn ResearchHost, Fail> {
    HOST.get().map(|h| h.as_ref()).ok_or((
        "internal",
        "no research host is registered (host::set_host)".to_string(),
    ))
}
