//! The host seams: how the channel store is opened, who approves proofs, and the engine name
//! recorded in acquisition run detail. Hosts register these once at start-up.
use std::path::Path;
use std::sync::OnceLock;

use rightkit_db::rusqlite::Connection;

use crate::Fail;

/// Opens the host channel store (schema migrated, `channels` table present) under the data root.
pub type ChannelOpener = fn(&Path) -> Result<Connection, Fail>;

static OPENER: OnceLock<ChannelOpener> = OnceLock::new();
static APPROVER: OnceLock<String> = OnceLock::new();
static ENGINE_NAME: OnceLock<String> = OnceLock::new();

/// Register how the host opens its channel store. Returns `false` when one is already set.
pub fn set_channel_opener(opener: ChannelOpener) -> bool {
    OPENER.set(opener).is_ok()
}

/// Register the author name allowed to approve a proof (default `owner`). Returns `false`
/// when one is already set.
pub fn set_approver(name: &str) -> bool {
    APPROVER.set(name.to_string()).is_ok()
}

/// Register the engine name recorded in acquisition run detail (default `rightkit`). Returns
/// `false` when one is already set.
pub fn set_engine_name(name: &str) -> bool {
    ENGINE_NAME.set(name.to_string()).is_ok()
}

pub(crate) fn open_channels(root: &Path) -> Result<Connection, Fail> {
    match OPENER.get() {
        Some(open) => open(root),
        None => Err((
            "internal",
            "no channel store opener is registered (host::set_channel_opener)".to_string(),
        )),
    }
}

pub(crate) fn approver() -> &'static str {
    APPROVER.get().map(String::as_str).unwrap_or("owner")
}

pub(crate) fn engine_name() -> &'static str {
    ENGINE_NAME.get().map(String::as_str).unwrap_or("rightkit")
}

/// `sha256:` plus the hex digest of the text: the hash recorded for reviewed copy.
pub(crate) fn text_hash(text: &str) -> String {
    format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(text.as_bytes())
    )
}
