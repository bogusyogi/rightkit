//! Application services the disk-backed history stores call, installed once with
//! [`install_host`].

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The database key's storage. Rotation writes the new key to staging first, rekeys the file,
/// then promotes it to primary, so a crash in between never loses the key.
pub trait KeyVault {
    /// The key the database is currently encrypted with.
    fn read_primary(&self) -> Result<Option<String>, String>;
    /// Replace the primary key.
    fn write_primary(&self, value: &str) -> Result<(), String>;
    /// A key staged by a rotation that has not been promoted yet.
    fn read_staging(&self) -> Result<Option<String>, String>;
    /// Stage a key.
    fn write_staging(&self, value: &str) -> Result<(), String>;
    /// Drop the staged key.
    fn delete_staging(&self) -> Result<(), String>;
}

/// What the history stores need from the application.
pub trait HistoryHost: Send + Sync + 'static {
    /// The application data directory.
    fn data_root(&self) -> PathBuf;
    /// A file inside the data directory.
    fn data_file(&self, name: &str) -> PathBuf {
        self.data_root().join(name)
    }
    /// File name of the encrypted database inside the data directory. Existing installs keep
    /// the name they were created with.
    fn sqlite_file_name(&self) -> String {
        "history.sqlite".to_string()
    }
    /// A diagnostic log line.
    fn log(&self, _event: &str, _message: &str) {}

    /// The stored database key; `Ok(None)` only when the credential store authoritatively holds none.
    fn db_key_state(&self) -> Result<Option<String>, String>;
    /// The stored key, minting and persisting one when absent.
    fn get_or_create_db_key(&self) -> Result<String, String>;
    /// Persist the primary database key.
    fn persist_db_key(&self, secret: &str) -> Result<(), String>;
    /// A fresh random key (standard base64 of 32 random bytes).
    fn new_db_key(&self) -> Result<String, String>;
    /// True in a dev or QA run that must never write the OS credential store.
    fn qa_credentials_isolated(&self) -> bool {
        false
    }
    /// The vault used for key rotation and staged-key recovery.
    fn key_vault(&self) -> &dyn KeyVault;

    /// The sync scope the user consented to, if any.
    fn authorized_sync_scope(&self) -> Option<String> {
        None
    }
    /// The live sync consent epoch.
    fn consent_generation(&self) -> u64 {
        0
    }
    /// Ask the sync scheduler for a pull.
    fn notify_remote_change(&self) {}
}

static HOST: OnceLock<Arc<dyn HistoryHost>> = OnceLock::new();

/// Install the host once at startup. Returns false when one is already installed.
pub fn install_host(host: Arc<dyn HistoryHost>) -> bool {
    HOST.set(host).is_ok()
}

/// The installed host.
///
/// # Panics
///
/// Panics when [`install_host`] was not called first.
pub fn host() -> &'static dyn HistoryHost {
    HOST.get()
        .expect("rightkit-transcript-library: install_host must be called before opening a store")
        .as_ref()
}
