//! Consented real-session mode: detect the user's default browser, record what the user agreed
//! to in a persistable, revocable receipt, copy a Chromium-family profile's sign-ins into a
//! named RightKit profile, and scope every action of the resulting session to the consented
//! domains.
//!
//! What this module does NOT do: it never reads, decrypts or transmits cookie values. Sign-ins
//! travel as the browser's own files (cookie database, local storage, the `Local State` key
//! material) copied into a private RightKit profile on the same machine. The browser's password
//! store (`Login Data`), form data (`Web Data`) and history are never copied, and typing into a
//! password field is refused, so passwords never pass through the agent: they stay in the
//! browser's own password manager.

use crate::error::{BrowserError, Result};
use crate::policy::{ActionKind, AdmissionHook, AdmissionRequest, Decision};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema version of [`UserConsentReceipt`].
pub const CONSENT_SCHEMA: u32 = 1;

/// Marker file written into an imported profile.
pub(crate) const IMPORT_MARKER: &str = ".rightkit-import.json";

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Browser kinds and default-browser detection
// ---------------------------------------------------------------------------

/// A concrete browser product.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Chrome,
    Edge,
    Chromium,
    Brave,
    Safari,
    Firefox,
    Unknown,
}

impl BrowserKind {
    /// Stable lower-case name (also used in profile names).
    pub fn name(self) -> &'static str {
        match self {
            BrowserKind::Chrome => "chrome",
            BrowserKind::Edge => "edge",
            BrowserKind::Chromium => "chromium",
            BrowserKind::Brave => "brave",
            BrowserKind::Safari => "safari",
            BrowserKind::Firefox => "firefox",
            BrowserKind::Unknown => "unknown",
        }
    }

    /// True for browsers that run on Chromium and keep a Chromium user-data directory.
    pub fn is_chromium_family(self) -> bool {
        matches!(
            self,
            BrowserKind::Chrome | BrowserKind::Edge | BrowserKind::Chromium | BrowserKind::Brave
        )
    }
}

/// Which browser an import reads from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSource {
    DefaultBrowser,
    Chrome,
    Edge,
    Safari,
    Firefox,
}

impl BrowserSource {
    /// The concrete browser (`DefaultBrowser` is detected now).
    pub fn resolve(self) -> BrowserKind {
        match self {
            BrowserSource::DefaultBrowser => default_browser(),
            BrowserSource::Chrome => BrowserKind::Chrome,
            BrowserSource::Edge => BrowserKind::Edge,
            BrowserSource::Safari => BrowserKind::Safari,
            BrowserSource::Firefox => BrowserKind::Firefox,
        }
    }
}

/// The user's default web browser on Windows, macOS and Linux, or [`BrowserKind::Unknown`]
/// when it cannot be determined.
pub fn default_browser() -> BrowserKind {
    detect_default_browser().unwrap_or(BrowserKind::Unknown)
}

#[cfg(windows)]
fn detect_default_browser() -> Option<BrowserKind> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\https\\UserChoice",
            "/v",
            "ProgId",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_windows_prog_id(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn detect_default_browser() -> Option<BrowserKind> {
    let output = std::process::Command::new("defaults")
        .args([
            "read",
            "com.apple.LaunchServices/com.apple.launchservices.secure",
            "LSHandlers",
        ])
        .output()
        .ok()?;
    // No https handler recorded means the system default, Safari.
    Some(
        parse_macos_handlers(&String::from_utf8_lossy(&output.stdout))
            .unwrap_or(BrowserKind::Safari),
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
fn detect_default_browser() -> Option<BrowserKind> {
    let output = std::process::Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .ok()?;
    parse_desktop_entry(&String::from_utf8_lossy(&output.stdout))
}

/// `reg query ... /v ProgId` output, for example `ProgId    REG_SZ    ChromeHTML`.
#[allow(dead_code)]
pub(crate) fn parse_windows_prog_id(output: &str) -> Option<BrowserKind> {
    let line = output.lines().find(|l| l.contains("ProgId"))?;
    let prog_id = line.split_whitespace().last()?.to_ascii_lowercase();
    let kind = if prog_id.contains("chromehtml") {
        BrowserKind::Chrome
    } else if prog_id.contains("msedge") {
        BrowserKind::Edge
    } else if prog_id.contains("firefox") {
        BrowserKind::Firefox
    } else if prog_id.contains("bravehtml") {
        BrowserKind::Brave
    } else if prog_id.contains("safari") {
        BrowserKind::Safari
    } else {
        BrowserKind::Unknown
    };
    Some(kind)
}

/// `defaults read ... LSHandlers` output: find the block that handles the `https` scheme.
#[allow(dead_code)]
pub(crate) fn parse_macos_handlers(output: &str) -> Option<BrowserKind> {
    for block in output.split('}') {
        let lower = block.to_ascii_lowercase();
        let handles_https = lower
            .lines()
            .any(|l| l.contains("lshandlerurlscheme") && l.contains("https"));
        if !handles_https {
            continue;
        }
        for line in lower.lines() {
            if line.contains("lshandlerroleall") {
                let bundle = line
                    .split('=')
                    .nth(1)?
                    .trim()
                    .trim_end_matches(';')
                    .trim_matches('"');
                return Some(match bundle {
                    "com.google.chrome" => BrowserKind::Chrome,
                    "com.microsoft.edgemac" => BrowserKind::Edge,
                    "org.chromium.chromium" => BrowserKind::Chromium,
                    "com.brave.browser" => BrowserKind::Brave,
                    "org.mozilla.firefox" => BrowserKind::Firefox,
                    "com.apple.safari" => BrowserKind::Safari,
                    _ => BrowserKind::Unknown,
                });
            }
        }
    }
    None
}

/// `xdg-settings get default-web-browser` output, for example `google-chrome.desktop`.
#[allow(dead_code)]
pub(crate) fn parse_desktop_entry(output: &str) -> Option<BrowserKind> {
    let entry = output.trim().to_ascii_lowercase();
    if entry.is_empty() {
        return None;
    }
    let kind = if entry.contains("google-chrome") {
        BrowserKind::Chrome
    } else if entry.contains("microsoft-edge") {
        BrowserKind::Edge
    } else if entry.contains("brave") {
        BrowserKind::Brave
    } else if entry.contains("chromium") {
        BrowserKind::Chromium
    } else if entry.contains("firefox") {
        BrowserKind::Firefox
    } else {
        BrowserKind::Unknown
    };
    Some(kind)
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

/// Lower-case a domain and reject anything that is not a bare host name. A leading `*.` or `.`
/// is accepted and dropped: a domain always covers its subdomains.
pub(crate) fn normalize_domain(raw: &str) -> Result<String> {
    let d = raw
        .trim()
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let ok = !d.is_empty()
        && d.len() <= 253
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if ok {
        Ok(d)
    } else {
        Err(BrowserError::Invalid(format!(
            "'{raw}' is not a bare domain name"
        )))
    }
}

fn normalize_domains(domains: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    match domains {
        None => Ok(None),
        Some(list) => {
            if list.is_empty() {
                return Err(BrowserError::Invalid(
                    "an empty domain list grants nothing; use None for all domains".into(),
                ));
            }
            let mut out = Vec::with_capacity(list.len());
            for d in &list {
                let d = normalize_domain(d)?;
                if !out.contains(&d) {
                    out.push(d);
                }
            }
            Ok(Some(out))
        }
    }
}

/// A plain profile directory name such as `Default` or `Profile 1`.
pub(crate) fn validate_source_profile(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(BrowserError::Invalid(format!(
            "invalid source profile name '{name}'"
        )))
    }
}

fn host_matches(domain: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn domain_allowed(domains: &Option<Vec<String>>, host: &str) -> bool {
    match domains {
        None => true,
        Some(list) => list.iter().any(|d| host_matches(d, host)),
    }
}

/// What the user agreed to: which browser, which profile, which domains. The app persists it
/// (it is `Serialize`/`Deserialize`), shows it, and revokes it. A revoked or expired receipt
/// refuses every later import, attach and action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserConsentReceipt {
    pub schema: u32,
    /// Caller-chosen unique id (for example a UUID).
    pub id: String,
    pub browser: BrowserKind,
    /// Source profile directory name, for example `Default` or `Profile 1`.
    pub profile: String,
    /// Domains (each covers its subdomains) the agent may act on; `None` means every domain.
    pub domains: Option<Vec<String>>,
    pub granted_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub revoked_at_ms: Option<u64>,
}

impl UserConsentReceipt {
    pub fn new(
        id: impl Into<String>,
        browser: BrowserKind,
        profile: impl Into<String>,
        domains: Option<Vec<String>>,
        granted_at_ms: u64,
    ) -> Result<Self> {
        let receipt = Self {
            schema: CONSENT_SCHEMA,
            id: id.into(),
            browser,
            profile: profile.into(),
            domains,
            granted_at_ms,
            expires_at_ms: None,
            revoked_at_ms: None,
        };
        receipt.validated()
    }

    /// Receipt that lapses at `expires_at_ms` (Unix milliseconds).
    pub fn with_expiry(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }

    fn validated(mut self) -> Result<Self> {
        if self.schema != CONSENT_SCHEMA {
            return Err(BrowserError::Consent(format!(
                "unsupported receipt schema {}",
                self.schema
            )));
        }
        if self.id.is_empty() || self.id.len() > 128 {
            return Err(BrowserError::Invalid(
                "consent id must be 1-128 characters".into(),
            ));
        }
        if self.browser == BrowserKind::Unknown {
            return Err(BrowserError::Invalid(
                "consent names an unknown browser".into(),
            ));
        }
        validate_source_profile(&self.profile)?;
        self.domains = normalize_domains(self.domains.take())?;
        Ok(self)
    }

    /// Mark the receipt revoked at `now_ms`. The app persists the result.
    pub fn revoke(&mut self, now_ms: u64) {
        if self.revoked_at_ms.is_none() {
            self.revoked_at_ms = Some(now_ms);
        }
    }

    pub fn is_active_at(&self, now_ms: u64) -> bool {
        self.revoked_at_ms.is_none() && self.expires_at_ms.map_or(true, |e| now_ms < e)
    }

    pub fn check_active(&self) -> Result<()> {
        if self.is_active_at(now_ms()) {
            Ok(())
        } else {
            Err(BrowserError::Consent(format!(
                "consent {} is revoked or has expired",
                self.id
            )))
        }
    }

    /// Whether `host` is inside the consented domains (a domain covers its subdomains).
    pub fn permits_host(&self, host: &str) -> bool {
        domain_allowed(&self.domains, host)
    }

    /// Whether an http(s) `url` is inside the consented domains.
    pub fn permits_url(&self, url: &str) -> bool {
        match url::Url::parse(url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => {
                u.host_str().map_or(false, |h| self.permits_host(h))
            }
            _ => false,
        }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| BrowserError::Consent(e.to_string()))
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let receipt: Self =
            serde_json::from_str(json).map_err(|e| BrowserError::Consent(e.to_string()))?;
        receipt.validated()
    }
}

/// Live revocation switch shared with a running session. Calling [`Self::revoke`] makes the
/// session's next action fail, without waiting for the app to persist the receipt.
#[derive(Clone, Debug, Default)]
pub struct ConsentRevocation(Arc<AtomicBool>);

impl ConsentRevocation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn revoke(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_revoked(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl PartialEq for ConsentRevocation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ConsentRevocation {}

// ---------------------------------------------------------------------------
// Import specification
// ---------------------------------------------------------------------------

/// Consent-gated copy of a browser profile's sign-ins into a named RightKit profile (see
/// [`crate::ProfileSpec::ImportedFrom`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportSpec {
    pub browser: BrowserSource,
    /// Source profile directory name; `None` means the consent's profile.
    pub source_profile: Option<String>,
    /// Narrow the consent further; `None` means the consent's domains. Every entry must be
    /// inside the consent. Scopes the session's actions (the cookie database is copied whole).
    pub domains: Option<Vec<String>>,
    pub consent: UserConsentReceipt,
    pub revocation: ConsentRevocation,
    /// Copy again even when this consent already imported into the profile.
    pub reimport: bool,
}

impl ImportSpec {
    pub fn new(browser: BrowserSource, consent: UserConsentReceipt) -> Self {
        Self {
            browser,
            source_profile: None,
            domains: None,
            consent,
            revocation: ConsentRevocation::new(),
            reimport: false,
        }
    }

    pub fn source_profile(mut self, name: impl Into<String>) -> Self {
        self.source_profile = Some(name.into());
        self
    }

    pub fn domains(mut self, domains: Vec<String>) -> Self {
        self.domains = Some(domains);
        self
    }

    pub fn revocation(mut self, revocation: ConsentRevocation) -> Self {
        self.revocation = revocation;
        self
    }

    pub fn reimport(mut self) -> Self {
        self.reimport = true;
        self
    }

    pub(crate) fn effective_profile(&self) -> &str {
        self.source_profile
            .as_deref()
            .unwrap_or(self.consent.profile.as_str())
    }

    /// Domains the session may act on: the narrowed list, else the consent's.
    pub(crate) fn effective_domains(&self) -> Result<Option<Vec<String>>> {
        match normalize_domains(self.domains.clone())? {
            Some(list) => {
                for d in &list {
                    if !self.consent.permits_host(d) {
                        return Err(BrowserError::Consent(format!(
                            "domain {d} is outside the consent"
                        )));
                    }
                }
                Ok(Some(list))
            }
            None => Ok(self.consent.domains.clone()),
        }
    }

    /// Everything that must hold before any browser data is touched.
    pub(crate) fn validate(&self) -> Result<BrowserKind> {
        self.consent.check_active()?;
        if self.revocation.is_revoked() {
            return Err(BrowserError::Consent("consent was revoked".into()));
        }
        let kind = self.browser.resolve();
        if kind != self.consent.browser {
            return Err(BrowserError::Consent(format!(
                "consent was granted for {}, not {}",
                self.consent.browser.name(),
                kind.name()
            )));
        }
        validate_source_profile(self.effective_profile())?;
        if self.effective_profile() != self.consent.profile {
            return Err(BrowserError::Consent(format!(
                "consent was granted for profile '{}', not '{}'",
                self.consent.profile,
                self.effective_profile()
            )));
        }
        self.effective_domains()?;
        if !kind.is_chromium_family() {
            return Err(BrowserError::Unsupported(format!(
                "{} sessions cannot be imported into a CDP profile; drive {} through rightkit-automation",
                kind.name(),
                kind.name()
            )));
        }
        Ok(kind)
    }
}

// ---------------------------------------------------------------------------
// Scoped admission
// ---------------------------------------------------------------------------

fn scope_verdict(
    consent: &UserConsentReceipt,
    domains: &Option<Vec<String>>,
    revocation: &ConsentRevocation,
    req: &AdmissionRequest,
) -> Option<String> {
    if revocation.is_revoked() || !consent.is_active_at(now_ms()) {
        return Some("user consent was revoked or has expired".into());
    }
    if matches!(req.action, ActionKind::Fill | ActionKind::TypeText) {
        let target = req.target.as_deref().unwrap_or("").to_ascii_lowercase();
        if target.contains("password") || target.contains("passwd") {
            return Some(
                "passwords come from the browser's own password manager, never from the agent"
                    .into(),
            );
        }
    }
    match req.url.as_deref() {
        None => {
            if matches!(req.action, ActionKind::Navigate) {
                Some("navigation without a URL".into())
            } else {
                None
            }
        }
        Some("about:blank") => None,
        Some(raw) => match url::Url::parse(raw) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => match u.host_str() {
                Some(host) if domain_allowed(domains, host) => None,
                _ => Some(format!(
                    "{raw} is outside the domains the user consented to"
                )),
            },
            _ => Some(format!("{raw} is not an http(s) page inside the consent")),
        },
    }
}

/// Admission hook that refuses every action outside the consent, then defers to `inner`.
pub(crate) fn scoped_admission(
    spec: &ImportSpec,
    inner: Option<AdmissionHook>,
) -> Result<AdmissionHook> {
    let consent = spec.consent.clone();
    let domains = spec.effective_domains()?;
    let revocation = spec.revocation.clone();
    Ok(Arc::new(move |req: AdmissionRequest| {
        let verdict = scope_verdict(&consent, &domains, &revocation, &req);
        let inner = inner.clone();
        async move {
            if let Some(reason) = verdict {
                return Decision::Deny(reason);
            }
            match inner {
                Some(hook) => hook(req).await,
                None => Decision::Allow,
            }
        }
        .boxed()
    }))
}

// ---------------------------------------------------------------------------
// Chromium-family profile discovery and import
// ---------------------------------------------------------------------------

/// The browser's own user-data directory (read only by this module), when it exists.
pub fn user_data_dir(kind: BrowserKind) -> Option<PathBuf> {
    let rel: &[&str] = match kind {
        BrowserKind::Chrome => &["Google", "Chrome"],
        BrowserKind::Edge => &["Microsoft", "Edge"],
        BrowserKind::Chromium => &["Chromium"],
        BrowserKind::Brave => &["BraveSoftware", "Brave-Browser"],
        _ => return None,
    };
    let base: PathBuf = if cfg!(windows) {
        let mut p = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        for part in rel {
            p.push(part);
        }
        p.push("User Data");
        return p.is_dir().then_some(p);
    } else if cfg!(target_os = "macos") {
        let mut p = PathBuf::from(std::env::var_os("HOME")?);
        p.push("Library");
        p.push("Application Support");
        p
    } else {
        let mut p = PathBuf::from(std::env::var_os("HOME")?);
        p.push(".config");
        p
    };
    let mut path = base;
    if cfg!(target_os = "macos") {
        for part in rel {
            path.push(part);
        }
    } else {
        // Linux directory names are lower-case and hyphenated.
        let name = match kind {
            BrowserKind::Chrome => "google-chrome",
            BrowserKind::Edge => "microsoft-edge",
            BrowserKind::Chromium => "chromium",
            _ => "BraveSoftware/Brave-Browser",
        };
        path.push(name);
    }
    path.is_dir().then_some(path)
}

/// The installed browser executable for a Chromium-family kind.
pub fn browser_executable(kind: BrowserKind) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        match kind {
            BrowserKind::Chrome => &[
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
                "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
            ],
            BrowserKind::Edge => &[
                "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
                "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
            ],
            BrowserKind::Brave => {
                &["C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"]
            }
            _ => &[],
        }
    } else if cfg!(target_os = "macos") {
        match kind {
            BrowserKind::Chrome => {
                &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
            }
            BrowserKind::Edge => {
                &["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"]
            }
            BrowserKind::Chromium => &["/Applications/Chromium.app/Contents/MacOS/Chromium"],
            BrowserKind::Brave => &["/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"],
            _ => &[],
        }
    } else {
        match kind {
            BrowserKind::Chrome => &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable"],
            BrowserKind::Edge => &["/usr/bin/microsoft-edge", "/usr/bin/microsoft-edge-stable"],
            BrowserKind::Chromium => &["/usr/bin/chromium", "/usr/bin/chromium-browser"],
            BrowserKind::Brave => &["/usr/bin/brave-browser"],
            _ => &[],
        }
    };
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

/// One profile of a Chromium-family browser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceProfile {
    /// Directory name inside the user-data directory (`Default`, `Profile 1`).
    pub dir_name: String,
    /// Name shown in the browser's profile picker.
    pub display_name: String,
}

pub(crate) fn parse_profiles(local_state: &str) -> Result<Vec<SourceProfile>> {
    let value: serde_json::Value = serde_json::from_str(local_state)
        .map_err(|e| BrowserError::Profile(format!("unreadable Local State: {e}")))?;
    let mut out = Vec::new();
    if let Some(cache) = value
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(|c| c.as_object())
    {
        for (dir, info) in cache {
            let display = info
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(dir.as_str())
                .to_string();
            out.push(SourceProfile {
                dir_name: dir.clone(),
                display_name: display,
            });
        }
    }
    out.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    Ok(out)
}

/// Profiles of an installed Chromium-family browser, for the consent screen.
pub fn list_profiles(kind: BrowserKind) -> Result<Vec<SourceProfile>> {
    if !kind.is_chromium_family() {
        return Err(BrowserError::Unsupported(format!(
            "profile listing covers Chromium-family browsers, not {}",
            kind.name()
        )));
    }
    let dir = user_data_dir(kind).ok_or_else(|| {
        BrowserError::Profile(format!("{} has no user-data directory", kind.name()))
    })?;
    let text = std::fs::read_to_string(dir.join("Local State"))?;
    parse_profiles(&text)
}

/// Result of one import. Per-domain cookie counts need the cookie database opened, which this
/// module does not do, so `domain_counts` is `None`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImportReport {
    pub consent_id: String,
    pub browser: BrowserKind,
    pub source_profile: String,
    /// Paths copied, relative to the source user-data directory.
    pub files_copied: Vec<String>,
    pub bytes_copied: u64,
    pub domain_counts: Option<BTreeMap<String, u64>>,
    pub imported_at_ms: u64,
}

fn copy_tree(src: &Path, dst: &Path, bytes: &mut u64) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let target = dst.join(entry.file_name());
        if meta.is_dir() {
            copy_tree(&entry.path(), &target, bytes)?;
        } else if entry.file_name() != "LOCK" {
            *bytes += copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<u64> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst).map_err(|e| {
        BrowserError::Profile(format!(
            "could not copy {} ({e}); close the browser or wait for it to release the file, then import again",
            src.display()
        ))
    })
}

/// Copy the sign-in files of `source_profile` from `user_data` into `dest_root` (the RightKit
/// profile directory, which Chrome later opens as its user-data directory). Copies the cookie
/// database, local storage and `Local State`; never `Login Data`, `Web Data` or history.
pub(crate) fn import_chromium_profile(
    user_data: &Path,
    source_profile: &str,
    dest_root: &Path,
    consent_id: &str,
    browser: BrowserKind,
) -> Result<ImportReport> {
    validate_source_profile(source_profile)?;
    let src = user_data.join(source_profile);
    if !src.is_dir() {
        return Err(BrowserError::Profile(format!(
            "source profile not found: {}",
            src.display()
        )));
    }
    let dst = dest_root.join("Default");
    let mut files: Vec<String> = Vec::new();
    let mut bytes: u64 = 0;

    for rel in [
        "Network/Cookies",
        "Network/Cookies-journal",
        "Cookies",
        "Cookies-journal",
    ] {
        let from = src.join(rel);
        if from.is_file() {
            bytes += copy_file(&from, &dst.join(rel))?;
            files.push(format!("{source_profile}/{rel}"));
        }
    }
    if !files.iter().any(|f| f.ends_with("Cookies")) {
        return Err(BrowserError::Profile(format!(
            "no cookie database in {}",
            src.display()
        )));
    }
    let local_storage = src.join("Local Storage");
    if local_storage.is_dir() {
        let target = dst.join("Local Storage");
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        copy_tree(&local_storage, &target, &mut bytes)?;
        files.push(format!("{source_profile}/Local Storage"));
    }
    let local_state = user_data.join("Local State");
    if local_state.is_file() {
        bytes += copy_file(&local_state, &dest_root.join("Local State"))?;
        files.push("Local State".into());
    }
    Ok(ImportReport {
        consent_id: consent_id.to_string(),
        browser,
        source_profile: source_profile.to_string(),
        files_copied: files,
        bytes_copied: bytes,
        domain_counts: None,
        imported_at_ms: now_ms(),
    })
}

fn marker_consent_id(dest_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dest_root.join(IMPORT_MARKER)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("consent_id")?.as_str().map(|s| s.to_string())
}

/// Validate the spec and import into `dest_root` when this consent has not imported there yet
/// (or `reimport` is set). Returns the report when a copy happened.
pub(crate) fn import_if_needed(
    spec: &ImportSpec,
    dest_root: &Path,
) -> Result<Option<ImportReport>> {
    let kind = spec.validate()?;
    if !spec.reimport && marker_consent_id(dest_root).as_deref() == Some(spec.consent.id.as_str()) {
        return Ok(None);
    }
    let user_data = user_data_dir(kind).ok_or_else(|| {
        BrowserError::Profile(format!(
            "{} has no user-data directory on this machine",
            kind.name()
        ))
    })?;
    let report = import_chromium_profile(
        &user_data,
        spec.effective_profile(),
        dest_root,
        &spec.consent.id,
        kind,
    )?;
    let json =
        serde_json::to_string_pretty(&report).map_err(|e| BrowserError::Profile(e.to_string()))?;
    std::fs::write(dest_root.join(IMPORT_MARKER), json)?;
    Ok(Some(report))
}

/// Delete an imported RightKit profile (revoking consent should call this). Refuses a directory
/// RightKit did not import into, and a profile a live session holds.
pub fn purge_imported_profile(root: &Path, name: &str) -> Result<()> {
    crate::config::validate_profile_name(name)?;
    crate::config::reject_real_browser_profile(root)?;
    let dir = root.join(name);
    if !dir.join(IMPORT_MARKER).is_file() {
        return Err(BrowserError::Profile(format!(
            "'{name}' is not an imported profile; nothing deleted"
        )));
    }
    {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join(".rightkit-browser.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(BrowserError::ProfileInUse(name.to_string()));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Profile name for [`crate::BrowserSession::attach_user_browser`].
pub(crate) fn attach_profile_name(consent: &UserConsentReceipt) -> String {
    let slug: String = consent
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
    format!("user-{}-{}", consent.browser.name(), slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(domains: Option<Vec<&str>>) -> UserConsentReceipt {
        UserConsentReceipt::new(
            "c-1",
            BrowserKind::Chrome,
            "Default",
            domains.map(|d| d.into_iter().map(String::from).collect()),
            1_000,
        )
        .unwrap()
    }

    fn request(action: ActionKind, url: Option<&str>, target: Option<&str>) -> AdmissionRequest {
        AdmissionRequest {
            session_id: "bs-1".into(),
            page_id: "p-1".into(),
            action,
            url: url.map(String::from),
            target: target.map(String::from),
            detail: None,
        }
    }

    #[test]
    fn detects_windows_prog_ids() {
        let out =
            "\r\nHKEY_CURRENT_USER\\...\\UserChoice\r\n    ProgId    REG_SZ    ChromeHTML\r\n";
        assert_eq!(parse_windows_prog_id(out), Some(BrowserKind::Chrome));
        let out = "    ProgId    REG_SZ    MSEdgeHTM";
        assert_eq!(parse_windows_prog_id(out), Some(BrowserKind::Edge));
        let out = "    ProgId    REG_SZ    FirefoxURL-308046B0AF4A39CB";
        assert_eq!(parse_windows_prog_id(out), Some(BrowserKind::Firefox));
        assert_eq!(parse_windows_prog_id("ERROR: not found"), None);
    }

    #[test]
    fn detects_macos_https_handler() {
        let out = "(\n    {\n        LSHandlerPreferredVersions = {\n            LSHandlerRoleAll = \"-\";\n        };\n        LSHandlerRoleAll = \"com.google.chrome\";\n        LSHandlerURLScheme = https;\n    },\n    {\n        LSHandlerRoleAll = \"com.apple.mail\";\n        LSHandlerURLScheme = mailto;\n    }\n)";
        assert_eq!(parse_macos_handlers(out), Some(BrowserKind::Chrome));
        assert_eq!(parse_macos_handlers("(\n)"), None);
    }

    #[test]
    fn detects_linux_desktop_entries() {
        assert_eq!(
            parse_desktop_entry("google-chrome.desktop\n"),
            Some(BrowserKind::Chrome)
        );
        assert_eq!(
            parse_desktop_entry("firefox.desktop"),
            Some(BrowserKind::Firefox)
        );
        assert_eq!(
            parse_desktop_entry("chromium-browser.desktop"),
            Some(BrowserKind::Chromium)
        );
        assert_eq!(parse_desktop_entry(""), None);
    }

    #[test]
    fn domains_normalize_and_cover_subdomains() {
        let r = receipt(Some(vec!["*.Example.com", "example.com", "bank.test"]));
        assert_eq!(
            r.domains,
            Some(vec!["example.com".to_string(), "bank.test".to_string()])
        );
        assert!(r.permits_url("https://app.example.com/x"));
        assert!(r.permits_url("https://example.com"));
        assert!(!r.permits_url("https://notexample.com"));
        assert!(!r.permits_url("https://example.com.evil.test"));
        assert!(!r.permits_url("file:///etc/passwd"));
        assert!(
            UserConsentReceipt::new("c", BrowserKind::Chrome, "Default", Some(vec![]), 1).is_err()
        );
        assert!(UserConsentReceipt::new(
            "c",
            BrowserKind::Chrome,
            "Default",
            Some(vec!["a/b".into()]),
            1
        )
        .is_err());
        assert!(UserConsentReceipt::new("c", BrowserKind::Chrome, "..", None, 1).is_err());
        assert!(UserConsentReceipt::new("c", BrowserKind::Unknown, "Default", None, 1).is_err());
    }

    #[test]
    fn receipt_round_trips_and_revokes() {
        let mut r = receipt(Some(vec!["example.com"])).with_expiry(5_000);
        assert!(r.is_active_at(4_999));
        assert!(!r.is_active_at(5_000));
        let back = UserConsentReceipt::from_json(&r.to_json().unwrap()).unwrap();
        assert_eq!(back, r);
        r.revoke(2_000);
        assert!(!r.is_active_at(2_001));
        assert_eq!(r.revoked_at_ms, Some(2_000));
        assert!(r.check_active().is_err());
    }

    #[test]
    fn import_spec_rejects_scope_widening_and_mismatches() {
        let consent = receipt(Some(vec!["example.com"]));
        let ok = ImportSpec::new(BrowserSource::Chrome, consent.clone());
        assert!(ok.validate().is_ok());
        let wider = ImportSpec::new(BrowserSource::Chrome, consent.clone())
            .domains(vec!["other.test".into()]);
        assert!(matches!(wider.validate(), Err(BrowserError::Consent(_))));
        let narrower = ImportSpec::new(BrowserSource::Chrome, consent.clone())
            .domains(vec!["shop.example.com".into()]);
        assert_eq!(
            narrower.effective_domains().unwrap(),
            Some(vec!["shop.example.com".to_string()])
        );
        let wrong_browser = ImportSpec::new(BrowserSource::Edge, consent.clone());
        assert!(matches!(
            wrong_browser.validate(),
            Err(BrowserError::Consent(_))
        ));
        let wrong_profile =
            ImportSpec::new(BrowserSource::Chrome, consent.clone()).source_profile("Profile 1");
        assert!(matches!(
            wrong_profile.validate(),
            Err(BrowserError::Consent(_))
        ));
        let revocation = ConsentRevocation::new();
        let live = ImportSpec::new(BrowserSource::Chrome, consent).revocation(revocation.clone());
        revocation.revoke();
        assert!(matches!(live.validate(), Err(BrowserError::Consent(_))));
        let safari = UserConsentReceipt::new("s", BrowserKind::Safari, "Default", None, 1).unwrap();
        assert!(matches!(
            ImportSpec::new(BrowserSource::Safari, safari).validate(),
            Err(BrowserError::Unsupported(_))
        ));
    }

    #[test]
    fn scope_refuses_other_domains_passwords_and_revocation() {
        let consent = receipt(Some(vec!["example.com"]));
        let spec = ImportSpec::new(BrowserSource::Chrome, consent);
        let hook = scoped_admission(&spec, None).unwrap();
        let run = |req| futures::executor::block_on(hook(req));
        assert_eq!(
            run(request(
                ActionKind::Navigate,
                Some("https://app.example.com/"),
                None
            )),
            Decision::Allow
        );
        assert_eq!(
            run(request(ActionKind::Click, Some("about:blank"), None)),
            Decision::Allow
        );
        assert!(matches!(
            run(request(
                ActionKind::Navigate,
                Some("https://evil.test/"),
                None
            )),
            Decision::Deny(_)
        ));
        assert!(matches!(
            run(request(
                ActionKind::Click,
                Some("https://evil.test/"),
                Some("#go")
            )),
            Decision::Deny(_)
        ));
        assert!(matches!(
            run(request(
                ActionKind::Fill,
                Some("https://example.com/login"),
                Some("input[type=password]")
            )),
            Decision::Deny(_)
        ));
        assert_eq!(
            run(request(
                ActionKind::Fill,
                Some("https://example.com/login"),
                Some("#user")
            )),
            Decision::Allow
        );
        spec.revocation.revoke();
        assert!(matches!(
            run(request(
                ActionKind::Click,
                Some("https://example.com/"),
                None
            )),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn scope_defers_to_the_inner_hook() {
        let consent = receipt(None);
        let spec = ImportSpec::new(BrowserSource::Chrome, consent);
        let inner: AdmissionHook = Arc::new(|_req: AdmissionRequest| {
            async { Decision::Deny("inner says no".into()) }.boxed()
        });
        let hook = scoped_admission(&spec, Some(inner)).unwrap();
        let verdict = futures::executor::block_on(hook(request(
            ActionKind::Click,
            Some("https://example.com/"),
            None,
        )));
        assert_eq!(verdict, Decision::Deny("inner says no".into()));
    }

    #[test]
    fn parses_profile_picker_names() {
        let json = "{\"profile\":{\"info_cache\":{\"Profile 1\":{\"name\":\"Work\"},\"Default\":{\"name\":\"Adrian\"}}}}";
        let got = parse_profiles(json).unwrap();
        assert_eq!(
            got,
            vec![
                SourceProfile {
                    dir_name: "Default".into(),
                    display_name: "Adrian".into()
                },
                SourceProfile {
                    dir_name: "Profile 1".into(),
                    display_name: "Work".into()
                },
            ]
        );
    }

    #[test]
    fn import_copies_sign_in_files_and_never_the_password_store() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().join("user data");
        let profile = user.join("Default");
        std::fs::create_dir_all(profile.join("Network")).unwrap();
        std::fs::create_dir_all(profile.join("Local Storage").join("leveldb")).unwrap();
        std::fs::write(profile.join("Network").join("Cookies"), "cookies").unwrap();
        std::fs::write(
            profile
                .join("Local Storage")
                .join("leveldb")
                .join("000003.log"),
            "ls",
        )
        .unwrap();
        std::fs::write(
            profile.join("Local Storage").join("leveldb").join("LOCK"),
            "",
        )
        .unwrap();
        std::fs::write(profile.join("Login Data"), "passwords").unwrap();
        std::fs::write(profile.join("Web Data"), "forms").unwrap();
        std::fs::write(profile.join("History"), "history").unwrap();
        std::fs::write(user.join("Local State"), "{}").unwrap();
        let dest = temp.path().join("dest");
        let report =
            import_chromium_profile(&user, "Default", &dest, "c-1", BrowserKind::Chrome).unwrap();
        assert!(dest
            .join("Default")
            .join("Network")
            .join("Cookies")
            .is_file());
        assert!(dest
            .join("Default")
            .join("Local Storage")
            .join("leveldb")
            .join("000003.log")
            .is_file());
        assert!(!dest
            .join("Default")
            .join("Local Storage")
            .join("leveldb")
            .join("LOCK")
            .exists());
        assert!(dest.join("Local State").is_file());
        for never in ["Login Data", "Web Data", "History"] {
            assert!(
                !dest.join("Default").join(never).exists(),
                "{never} was copied"
            );
        }
        assert_eq!(report.domain_counts, None);
        assert!(report
            .files_copied
            .iter()
            .any(|f| f == "Default/Network/Cookies"));
        let missing =
            import_chromium_profile(&user, "Profile 9", &dest, "c-1", BrowserKind::Chrome);
        assert!(matches!(missing, Err(BrowserError::Profile(_))));
        assert!(import_chromium_profile(&user, "../x", &dest, "c-1", BrowserKind::Chrome).is_err());
    }

    #[test]
    fn purge_only_touches_imported_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(purge_imported_profile(temp.path(), "plain").is_err());
        assert!(plain.is_dir());
        let imported = temp.path().join("imp");
        std::fs::create_dir_all(&imported).unwrap();
        std::fs::write(imported.join(IMPORT_MARKER), "{}").unwrap();
        purge_imported_profile(temp.path(), "imp").unwrap();
        assert!(!imported.exists());
    }

    #[test]
    fn attach_profile_name_is_a_valid_profile_name() {
        let r = UserConsentReceipt::new("c", BrowserKind::Edge, "Profile 1", None, 1).unwrap();
        let name = attach_profile_name(&r);
        assert_eq!(name, "user-edge-profile-1");
        assert!(crate::config::validate_profile_name(&name).is_ok());
    }
}
