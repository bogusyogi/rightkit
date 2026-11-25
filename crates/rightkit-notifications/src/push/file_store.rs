//! A file-backed device token store: one APNs token for each paired phone, in a small JSON file
//! (moved from Pulse's `engine/src/push.rs`).
//!
//! `{"devices": {"<device id>": {"token", "environment", "bundleId", "registeredMs"}}}`. Every call
//! reads the file and, when it changed something, writes it whole through a temporary file and a
//! rename. A token is never logged and never returned by a method that is meant for a reply:
//! `Registration`'s `Debug` hides it, and the [`DeviceTokenStore`] errors carry no token.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{DeviceToken, DeviceTokenStore, PushError};

/// Which APNs endpoint a token belongs to: a Debug build's is `development`, TestFlight's and the
/// App Store's `production`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Development,
    Production,
}

impl Environment {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "development" => Some(Self::Development),
            "production" => Some(Self::Production),
            _ => None,
        }
    }
}

/// The token as the phone reported it: hex of 32 to 100 bytes (either case), returned lowercase.
pub fn valid_token(hex: &str) -> Option<String> {
    let hex = hex.trim();
    let ok = hex.len().is_multiple_of(2)
        && (64..=200).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| hex.to_ascii_lowercase())
}

/// What is stored for one phone. `Debug` hides the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub token: String,
    pub environment: Environment,
    pub bundle_id: String,
    pub registered_ms: u64,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("token", &"<redacted>")
            .field("environment", &self.environment)
            .finish()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct TokenFile {
    #[serde(default)]
    devices: BTreeMap<String, Registration>,
}

/// One token for each paired phone, in a small JSON file.
pub struct FileTokenStore {
    path: PathBuf,
    io: Mutex<()>,
    on_unreadable: Option<Box<dyn Fn() + Send + Sync>>,
}

impl FileTokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            io: Mutex::new(()),
            on_unreadable: None,
        }
    }

    /// `hook` runs each time the file exists but cannot be parsed (it then holds nothing usable:
    /// phones register again at every launch). The hook must log no content.
    pub fn with_unreadable_hook(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_unreadable = Some(Box::new(hook));
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read(&self) -> TokenFile {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                if let Some(hook) = &self.on_unreadable {
                    hook();
                }
                TokenFile::default()
            }),
            Err(_) => TokenFile::default(),
        }
    }

    fn write(&self, file: &TokenFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(file).map_err(|e| e.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &self.path).map_err(|e| e.to_string())
    }

    /// Stores `registration` for `device_id`, replacing any earlier one. True when something
    /// changed (a repeat of the same token, environment and bundle id changes nothing and writes
    /// nothing).
    pub fn put(&self, device_id: &str, registration: Registration) -> Result<bool, String> {
        let _io = self.lock();
        let mut file = self.read();
        let same = file.devices.get(device_id).is_some_and(|old| {
            old.token == registration.token
                && old.environment == registration.environment
                && old.bundle_id == registration.bundle_id
        });
        if same {
            return Ok(false);
        }
        file.devices.insert(device_id.to_string(), registration);
        self.write(&file)?;
        Ok(true)
    }

    pub fn get(&self, device_id: &str) -> Option<Registration> {
        let _io = self.lock();
        self.read().devices.get(device_id).cloned()
    }

    /// Whether `device_id` holds a token here.
    pub fn has(&self, device_id: &str) -> bool {
        self.get(device_id).is_some()
    }

    /// Removes the entry, only while it still holds `only_token` when one is given (a phone that
    /// registered a fresh token between a send and its answer keeps the fresh one).
    pub fn remove(&self, device_id: &str, only_token: Option<&str>) -> bool {
        let _io = self.lock();
        let mut file = self.read();
        let held = file.devices.get(device_id).map(|r| r.token.clone());
        let Some(held) = held else { return false };
        if only_token.is_some_and(|token| token != held) {
            return false;
        }
        file.devices.remove(device_id);
        self.write(&file).is_ok()
    }

    /// Keeps only the entries of `keep`.
    pub fn retain(&self, keep: &[String]) {
        let _io = self.lock();
        let mut file = self.read();
        let before = file.devices.len();
        file.devices.retain(|id, _| keep.contains(id));
        if file.devices.len() != before {
            let _ = self.write(&file);
        }
    }

    /// The stored token of `device_id`, as lowercase hex, for a sender that builds its own request.
    pub fn token_hex(&self, device_id: &str) -> Option<String> {
        self.get(device_id).map(|r| r.token)
    }
}

impl DeviceTokenStore for FileTokenStore {
    fn get(&self, device_id: &str) -> Result<Option<DeviceToken>, PushError> {
        match self.token_hex(device_id) {
            Some(hex) => DeviceToken::from_hex(&hex).map(Some),
            None => Ok(None),
        }
    }

    fn put(&self, _device_id: &str, _token: DeviceToken) -> Result<(), PushError> {
        // A token carries its environment, which this interface cannot: phones register through
        // `FileTokenStore::put` with a whole `Registration`.
        Err(PushError::Store(
            "tokens are registered with a full registration".into(),
        ))
    }

    fn remove_if_current(&self, device_id: &str, token: &DeviceToken) -> Result<bool, PushError> {
        Ok(FileTokenStore::remove(
            self,
            device_id,
            Some(token.expose_hex()),
        ))
    }

    fn remove(&self, device_id: &str) -> Result<bool, PushError> {
        Ok(FileTokenStore::remove(self, device_id, None))
    }
}
