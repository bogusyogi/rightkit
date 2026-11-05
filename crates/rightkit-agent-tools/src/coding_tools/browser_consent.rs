//! User consent for the agent browser to reuse the user's real browser sign-ins
//! (docs/contracts/agent-browser.md R3, R22, R23, R25).
//!
//! The model: RightKit's `UserConsentReceipt` is a persisted, revocable, per-domain
//! grant. Granting it is a user action on a daemon route; the model can only name an
//! existing, active receipt when it opens a browser (`browser_open consent=<id>`),
//! never create one. A consented session runs on a private copy of the consented
//! profile (RightKit's `ProfileSpec::ImportedFrom`), never on the live default-profile
//! browser, and every action is confined to the consented domains. Revoking a receipt
//! stops every live session that uses it and purges the imported copy.
//!
//! Layout (R23): one fixed folder, `<state_root>/browser-consent/`, owner-only:
//! `receipts/<id>.json` (a `UserConsentReceipt`) and `profiles/<name>/` (the imported
//! copies). Nothing in this feature writes anywhere else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use rightkit_browser::ConsentRevocation;
pub use rightkit_browser::{BrowserKind, UserConsentReceipt};

use crate::coding_tools::browser::BrowserSession;

/// Folder name under the product state root (R23).
pub const CONSENT_DIR: &str = "browser-consent";
const RECEIPTS_DIR: &str = "receipts";
const PROFILES_DIR: &str = "profiles";

/// How a consented session runs. Cookie import first, attach second (decisions.md:105).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsentedMode {
    /// A headless session on a private copy of the consented profile's sign-ins.
    Import,
    /// A visible session on the same private copy (R22). Never the live profile.
    Attach,
}

impl ConsentedMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "import" => Some(Self::Import),
            "attach" => Some(Self::Attach),
            _ => None,
        }
    }
}

/// Everything a consented launch needs; built only by [`ConsentStore::launch_for`].
#[derive(Clone)]
pub struct ConsentedLaunch {
    pub receipt: UserConsentReceipt,
    pub revocation: ConsentRevocation,
    pub mode: ConsentedMode,
    pub profiles_root: PathBuf,
}

struct LiveConsent {
    revocation: ConsentRevocation,
    sessions: Vec<Weak<BrowserSession>>,
}

fn live() -> &'static Mutex<HashMap<String, LiveConsent>> {
    static LIVE: OnceLock<Mutex<HashMap<String, LiveConsent>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

fn live_lock() -> std::sync::MutexGuard<'static, HashMap<String, LiveConsent>> {
    live()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Directory name of the private profile copy for `receipt` (same rule RightKit's
/// `attach_user_browser` uses, so attach and import share one copy per browser profile).
pub fn imported_profile_name(receipt: &UserConsentReceipt) -> String {
    let slug: String = receipt
        .profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("user-{}-{}", receipt.browser.name(), slug)
}

/// The receipt store rooted at one folder.
#[derive(Clone, Debug)]
pub struct ConsentStore {
    root: PathBuf,
}

impl ConsentStore {
    /// A store at an explicit root (tests, relocated state).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The product store: `<state_root>/browser-consent`.
    pub fn product() -> Result<Self, String> {
        let root = rightkit_app_settings::agent_config::state_root()
            .ok_or_else(|| "the state directory is unavailable".to_string())?;
        Ok(Self::at(root.join(CONSENT_DIR)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn receipts_dir(&self) -> PathBuf {
        self.root.join(RECEIPTS_DIR)
    }

    fn profiles_dir(&self) -> PathBuf {
        self.root.join(PROFILES_DIR)
    }

    fn receipt_path(&self, id: &str) -> Result<PathBuf, String> {
        validate_receipt_id(id)?;
        Ok(self.receipts_dir().join(format!("{id}.json")))
    }

    fn write_receipt(&self, receipt: &UserConsentReceipt) -> Result<(), String> {
        let path = self.receipt_path(&receipt.id)?;
        rightkit_fs::ensure_private_dir(&self.root).map_err(|e| e.to_string())?;
        rightkit_fs::ensure_private_dir(&self.receipts_dir()).map_err(|e| e.to_string())?;
        let json = receipt.to_json().map_err(|e| e.to_string())?;
        rightkit_fs::write_private_file(&path, json.as_bytes()).map_err(|e| e.to_string())
    }

    /// Record the user's consent. `domains` must be non-empty: consent is per-domain (R25),
    /// so "every domain" cannot be granted. This is called by the daemon's consent route
    /// after the user confirms the consent screen; it is never reachable from a model tool.
    pub fn grant(
        &self,
        browser: BrowserKind,
        profile: &str,
        domains: Vec<String>,
        expires_at_ms: Option<u64>,
    ) -> Result<UserConsentReceipt, String> {
        if domains.is_empty() {
            return Err(
                "consent is per-domain: name at least one domain the agent may use".to_string(),
            );
        }
        if !browser.is_chromium_family() {
            return Err(format!(
                "{} sign-ins cannot be reused (Chromium-family browsers only)",
                browser.name()
            ));
        }
        let id = new_receipt_id(browser, profile);
        let mut receipt = UserConsentReceipt::new(id, browser, profile, Some(domains), now_ms())
            .map_err(|e| e.to_string())?;
        if let Some(expires) = expires_at_ms {
            receipt = receipt.with_expiry(expires);
        }
        self.write_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn get(&self, id: &str) -> Result<UserConsentReceipt, String> {
        let path = self.receipt_path(id)?;
        let text =
            std::fs::read_to_string(&path).map_err(|_| format!("no browser consent '{id}'"))?;
        UserConsentReceipt::from_json(&text)
            .map_err(|e| format!("browser consent '{id}' is unreadable: {e}"))
    }

    /// Every stored receipt (active, revoked or expired), newest grant first. A receipt
    /// file that does not parse is reported in `broken` with its path; it never hides the
    /// others (contract R10 spirit).
    pub fn list(&self) -> Result<(Vec<UserConsentReceipt>, Vec<(PathBuf, String)>), String> {
        let dir = self.receipts_dir();
        let mut receipts = Vec::new();
        let mut broken = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((receipts, broken));
            }
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| UserConsentReceipt::from_json(&text).map_err(|e| e.to_string()))
            {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => broken.push((path, error)),
            }
        }
        receipts.sort_by_key(|receipt| std::cmp::Reverse(receipt.granted_at_ms));
        Ok((receipts, broken))
    }

    /// Resolve the receipt a model named into a launch plan. Refuses a receipt that is
    /// revoked, expired or unknown. The live revocation switch is shared by every session
    /// opened under the same receipt so a revoke reaches them without a re-read.
    pub fn launch_for(&self, id: &str, mode: ConsentedMode) -> Result<ConsentedLaunch, String> {
        let receipt = self.get(id)?;
        receipt.check_active().map_err(|e| e.to_string())?;
        let revocation = live_lock()
            .entry(receipt.id.clone())
            .or_insert_with(|| LiveConsent {
                revocation: ConsentRevocation::new(),
                sessions: Vec::new(),
            })
            .revocation
            .clone();
        Ok(ConsentedLaunch {
            receipt,
            revocation,
            mode,
            profiles_root: self.profiles_dir(),
        })
    }

    /// Revoke consent: persist the revocation, stop every live session that runs under
    /// it, and purge the imported copy. The model never sees the copy's contents.
    pub async fn revoke(&self, id: &str) -> Result<UserConsentReceipt, String> {
        let mut receipt = self.get(id)?;
        if receipt.revoked_at_ms.is_none() {
            receipt.revoke(now_ms());
            self.write_receipt(&receipt)?;
        }
        let sessions = {
            let mut table = live_lock();
            match table.remove(&receipt.id) {
                Some(entry) => {
                    entry.revocation.revoke();
                    entry.sessions
                }
                None => Vec::new(),
            }
        };
        for session in sessions.into_iter().filter_map(|weak| weak.upgrade()) {
            session.shutdown().await;
            drop(session);
        }
        self.purge_copy(&receipt).await?;
        Ok(receipt)
    }

    /// Delete the private profile copy unless another still-active receipt uses it.
    async fn purge_copy(&self, receipt: &UserConsentReceipt) -> Result<(), String> {
        let name = imported_profile_name(receipt);
        let (others, _) = self.list()?;
        let shared = others.iter().any(|other| {
            other.id != receipt.id
                && other.check_active().is_ok()
                && imported_profile_name(other) == name
        });
        if shared {
            return Ok(());
        }
        let dir = self.profiles_dir().join(&name);
        if !dir.exists() {
            return Ok(());
        }
        // A session shutdown releases the profile lock asynchronously; give it a moment.
        let mut last = None;
        for _ in 0..20 {
            match rightkit_browser::purge_imported_profile(&self.profiles_dir(), &name) {
                Ok(()) => return Ok(()),
                Err(rightkit_browser::BrowserError::ProfileInUse(_)) => {
                    last = Some("the profile is still in use".to_string());
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Err(last.unwrap_or_else(|| "could not purge the imported profile".to_string()))
    }
}

/// Register a live session under its receipt so a revoke stops it.
pub(crate) fn register_session(receipt_id: &str, session: &Arc<BrowserSession>) {
    if let Some(entry) = live_lock().get_mut(receipt_id) {
        entry.sessions.retain(|weak| weak.strong_count() > 0);
        entry.sessions.push(Arc::downgrade(session));
    }
}

/// The default browser and its profiles, for the consent screen. Reads only profile names.
pub fn consent_sources() -> (BrowserKind, Vec<(String, String)>) {
    let default = rightkit_browser::default_browser();
    let profiles = if default.is_chromium_family() {
        rightkit_browser::list_profiles(default)
            .map(|list| {
                list.into_iter()
                    .map(|p| (p.dir_name, p.display_name))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (default, profiles)
}

/// The installed executable for a Chromium-family browser, when there is one.
pub fn browser_executable(kind: BrowserKind) -> Option<PathBuf> {
    rightkit_browser::browser_executable(kind)
}

/// A unique receipt id: a digest over the grant time, the process, a counter and what was granted.
fn new_receipt_id(browser: BrowserKind, profile: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{nanos}:{}:{}:{}:{profile}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        browser.name()
    );
    let digest = rightkit_crypto::sha256(seed.as_bytes());
    format!("consent-{}", rightkit_crypto::hex_encode(&digest[..12]))
}

fn validate_receipt_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err("a browser consent id is letters, digits, '-' and '_' only".to_string())
    }
}
