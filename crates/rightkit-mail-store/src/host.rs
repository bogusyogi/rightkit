//! The host seams of the mail store: identity, secret key storage, the data folder and the build
//! channel. A host registers one [`MailStoreHost`] at start-up with [`set_host`]; the store reads
//! nothing from the environment itself.
use std::path::PathBuf;
use std::sync::OnceLock;

/// How the data folder was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirKind {
    /// The real per-user install folder.
    Default,
    /// An explicitly chosen folder (a QA test root); never the real one.
    Override,
}

/// The build channel the migration gate keys its backup rules on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A development build.
    Dev,
    /// A release build.
    Release,
}

impl Channel {
    /// The channel name recorded in the migration log.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Dev => "dev",
            Channel::Release => "release",
        }
    }
}

/// One key rotation: a staged key that is either committed or aborted.
pub trait KeyRotation {
    /// Make the staged key primary and drop the marker.
    fn commit(&self) -> Result<(), String>;
    /// Discard the staged key; the primary is untouched.
    fn abort(&self);
}

/// What the store needs from the host app.
pub trait MailStoreHost: Send + Sync {
    /// Short lowercase app identity: database file stem, lock label and settings schema name.
    fn app_slug(&self) -> &str;
    /// Product name shown in user-facing refusals.
    fn product_name(&self) -> &str;
    /// The SQLCipher database key, created on first use.
    fn get_or_create_db_key(&self) -> Result<String, String>;
    /// The staged key of an interrupted rotation, if any.
    fn read_pending_db_key(&self) -> Option<String>;
    /// Recovery: `used` (the pending key that opened the database) becomes the primary key.
    fn promote_pending_db_key(&self, used: &str) -> Result<(), String>;
    /// A fresh random database key.
    fn new_db_key(&self) -> Result<String, String>;
    /// Stage `new_key` as the pending key before a re-encryption.
    fn begin_key_rotation(&self, new_key: &str) -> Result<Box<dyn KeyRotation>, String>;
    /// The data folder and how it was chosen, or the refusal for a half-set test selection.
    fn data_dir_checked(&self) -> Result<(PathBuf, DirKind), String>;
    /// This build's channel and short commit id.
    fn build(&self) -> (Channel, String);
    /// Append a line to a log file, rotating it at `max` bytes.
    fn append_rotating_log(
        &self,
        path: &std::path::Path,
        max: u64,
        line: &str,
    ) -> std::io::Result<()>;
    /// A test-run path or value selector; `None` outside a test run.
    fn test_selector(&self, _name: &str) -> Option<String> {
        None
    }
    /// A performance diagnostics line (tests only use it).
    fn perf_diag(&self, _line: &str) {}
}

static HOST: OnceLock<Box<dyn MailStoreHost>> = OnceLock::new();

/// Register the host once. Returns `false` when one is already registered.
pub fn set_host(host: Box<dyn MailStoreHost>) -> bool {
    HOST.set(host).is_ok()
}

/// The registered host. Panics when none is: a store used without a host is a start-up bug.
pub(crate) fn host() -> &'static dyn MailStoreHost {
    HOST.get()
        .map(|h| h.as_ref())
        .expect("no mail store host is registered (host::set_host)")
}
