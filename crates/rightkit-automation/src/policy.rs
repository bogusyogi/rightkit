//! Safety: allowlist, kill switch, built-in deny list, risk classes, redaction,
//! indicator hook, and the evidence record of which tier ran.

use crate::json::{self, Obj};
use crate::model::*;
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

static KILLED: AtomicBool = AtomicBool::new(false);

/// Process-wide kill switch. Flip from a hotkey, tray button, or signal
/// handler: every act entry point and every step of a batch fails closed.
pub fn kill() {
    KILLED.store(true, Ordering::SeqCst);
}
pub fn reset_kill() {
    KILLED.store(false, Ordering::SeqCst);
}
pub fn killed() -> bool {
    KILLED.load(Ordering::SeqCst)
}

/// App ids that are never drivable, whatever the allowlist says: system
/// privacy panes and other computer-use hosts (an agent must not drive the
/// agent that governs it).
const ALWAYS_DENIED: &[&str] = &[
    "com.apple.systempreferences",
    "com.apple.systemsettings",
    "com.apple.keychainaccess",
    "com.apple.securityagent",
    "com.apple.accessibility.universalaccessauthwarn",
    "systemsettings.exe",
    "securityhealthsystray.exe",
    "consent.exe",
    "credentialuibroker.exe",
    "com.anthropic.claudefordesktop",
];

#[derive(Default)]
pub struct Policy {
    allowed: Mutex<HashSet<String>>,
    denied: Mutex<HashSet<String>>,
    self_pid: Mutex<Option<i32>>,
}

impl Policy {
    /// `allow` holds bundle ids (macOS) or lowercase exe names (Windows). Empty denies all.
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(allow: I) -> Self {
        Policy {
            allowed: Mutex::new(
                allow
                    .into_iter()
                    .map(|s| s.into().to_ascii_lowercase())
                    .collect(),
            ),
            denied: Mutex::new(HashSet::new()),
            self_pid: Mutex::new(None),
        }
    }
    /// Refuse this app id even when it is on the allow list. Evaluated before the
    /// allow list in [`Policy::check`], so a deny always wins.
    pub fn deny(&self, app_id: &str) {
        self.denied
            .lock()
            .unwrap()
            .insert(app_id.to_ascii_lowercase());
    }
    pub fn allow(&self, app_id: &str) {
        self.allowed
            .lock()
            .unwrap()
            .insert(app_id.to_ascii_lowercase());
    }
    pub fn revoke(&self, app_id: &str) {
        self.allowed
            .lock()
            .unwrap()
            .remove(&app_id.to_ascii_lowercase());
    }
    pub fn allowed(&self) -> Vec<String> {
        let mut v: Vec<String> = self.allowed.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }
    /// Refuse to drive this process itself.
    pub fn protect_pid(&self, pid: i32) {
        *self.self_pid.lock().unwrap() = Some(pid);
    }
    pub fn check(&self, app: &AppInfo) -> Result<()> {
        if killed() {
            return Err(Error::Blocked("kill switch engaged".into()));
        }
        let id = app.app_id.to_ascii_lowercase();
        if ALWAYS_DENIED.contains(&id.as_str()) {
            return Err(Error::Blocked(format!("{} is never drivable", app.app_id)));
        }
        if *self.self_pid.lock().unwrap() == Some(app.pid) {
            return Err(Error::Blocked("refusing to drive the host process".into()));
        }
        if self.denied.lock().unwrap().contains(&id) {
            return Err(Error::Blocked(format!(
                "{} is denied by policy",
                app.app_id
            )));
        }
        if self.allowed.lock().unwrap().contains(&id) {
            Ok(())
        } else {
            Err(Error::Blocked(format!(
                "{} is not on the allowlist",
                app.app_id
            )))
        }
    }
}

/// Label-word risk classes. Matching is deliberately conservative: a hit needs
/// `ActOptions::confirm` to contain the class.
pub fn classify_risk(label: &str) -> Option<RiskClass> {
    let l = label.to_ascii_lowercase();
    let words: Vec<&str> = l
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(w));
    if has(&["delete", "remove", "erase", "discard", "trash", "destroy"]) {
        Some(RiskClass::Delete)
    } else if has(&[
        "send", "submit", "post", "publish", "reply", "share", "tweet", "signup", "register",
        "apply", "confirm",
    ]) {
        Some(RiskClass::Send)
    } else if has(&[
        "buy",
        "purchase",
        "pay",
        "payment",
        "checkout",
        "order",
        "subscribe",
        "book",
        "reserve",
        "donate",
        "transfer",
    ]) {
        Some(RiskClass::Purchase)
    } else if has(&["close", "quit", "exit"]) {
        Some(RiskClass::Close)
    } else {
        None
    }
}

/// Prefixes of well-known credential formats, assembled at runtime so source
/// scanners do not mistake the detector for a leaked secret.
fn secret_prefixes() -> Vec<String> {
    ["s", "g", "x", "a", "b", "e"]
        .iter()
        .zip(["k-", "hp_", "oxb-", "kia", "earer ", "yj"])
        .map(|(a, b)| format!("{a}{b}"))
        .collect()
}

/// True when text looks like a credential and must never reach a log.
pub fn looks_secret(v: &str) -> bool {
    let t = v.trim();
    let lower = t.to_ascii_lowercase();
    if secret_prefixes()
        .iter()
        .any(|p| lower.starts_with(p.as_str()))
    {
        return true;
    }
    t.len() >= 24
        && !t.contains(' ')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

/// Words that mark a field as holding a credential. Typing into such a field,
/// or showing it in a capture, is refused even when the OS does not flag it secure.
pub fn credential_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    let words: Vec<&str> = l
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let pair = |a: &str, b: &str| words.windows(2).any(|p| p[0] == a && p[1] == b);
    [
        "password",
        "passcode",
        "passphrase",
        "secret",
        "token",
        "otp",
        "cvv",
        "cvc",
        "cvn",
        "ssn",
        "apikey",
        "pwd",
        "pin",
        "iban",
        "swift",
        "bic",
        "routing",
        "aadhaar",
        "aadhar",
        "passport",
        "nino",
        "cardholder",
    ]
    .iter()
    .any(|w| has(w))
        || pair("api", "key")
        || pair("private", "key")
        || pair("one", "time")
        || pair("card", "number")
        || pair("card", "no")
        || pair("credit", "card")
        || pair("debit", "card")
        || pair("account", "number")
        || pair("account", "no")
        || pair("bank", "account")
        || pair("sort", "code")
        || pair("security", "code")
        || pair("social", "security")
        || pair("national", "id")
        || pair("national", "insurance")
        || pair("tax", "id")
        || pair("identity", "number")
        || pair("id", "number")
        || pair("driver", "license")
        || pair("driver", "licence")
        || pair("drivers", "license")
        || pair("recovery", "code")
        || pair("expiry", "date")
        || pair("expiration", "date")
}

/// Receipt-safe form of a payload: length only unless `log_values`, and never
/// the text when the target is secure or the text looks like a credential.
pub fn redact(value: &str, secure: bool, log_values: bool) -> String {
    if secure || looks_secret(value) {
        return format!("<redacted {} chars>", value.chars().count());
    }
    if !log_values {
        return format!("<{} chars>", value.chars().count());
    }
    let head: String = value.chars().take(40).collect();
    if value.chars().count() > 40 {
        format!("{head}…")
    } else {
        head
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub tier: Tier,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Done,
    Changed,
    Blocked,
    Failed,
}

impl ResultKind {
    pub fn label(self) -> &'static str {
        match self {
            ResultKind::Done => "done",
            ResultKind::Changed => "changed",
            ResultKind::Blocked => "blocked",
            ResultKind::Failed => "failed",
        }
    }
}

/// Evidence of one act: which tiers were tried and which ran.
#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    pub seq: u64,
    pub session: String,
    pub ts_ms: u64,
    pub pid: i32,
    pub app_id: String,
    pub window_id: Option<u64>,
    pub snapshot: Option<String>,
    pub element: Option<String>,
    pub action: String,
    pub value: Option<String>,
    pub attempts: Vec<Attempt>,
    pub tier_used: Option<Tier>,
    pub frontmost_before: Option<i32>,
    pub frontmost_after: Option<i32>,
    /// Frontmost app changed although the tier used was not Foreground.
    pub focus_moved: bool,
    pub result: ResultKind,
    pub error: Option<String>,
    /// `(epoch, seq)` of the lease fence this input carried, when the app was leased.
    pub lease: Option<(u64, u64)>,
    /// Which gate refused, when `result` is `Blocked`.
    pub denied_by: Option<crate::events::DeniedBy>,
}

impl Receipt {
    pub fn to_json(&self) -> String {
        let attempts = json::arr(self.attempts.iter().map(|a| {
            Obj::new()
                .str("tier", a.tier.label())
                .bool("ok", a.ok)
                .str("detail", &a.detail)
                .build()
        }));
        let opt_num = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
        Obj::new()
            .num("seq", self.seq)
            .str("session", &self.session)
            .num("ts_ms", self.ts_ms)
            .num("pid", self.pid)
            .str("app_id", &self.app_id)
            .raw("window_id", opt_num(self.window_id.map(|w| w as i64)))
            .opt_str("snapshot", &self.snapshot)
            .opt_str("element", &self.element)
            .str("action", &self.action)
            .opt_str("value", &self.value)
            .raw("attempts", attempts)
            .raw(
                "tier_used",
                self.tier_used
                    .map(|t| json::s(t.label()))
                    .unwrap_or_else(|| "null".into()),
            )
            .raw(
                "frontmost_before",
                opt_num(self.frontmost_before.map(i64::from)),
            )
            .raw(
                "frontmost_after",
                opt_num(self.frontmost_after.map(i64::from)),
            )
            .bool("focus_moved", self.focus_moved)
            .str("result", self.result.label())
            .opt_str("error", &self.error)
            .raw(
                "lease_epoch",
                self.lease
                    .map(|l| l.0.to_string())
                    .unwrap_or_else(|| "null".into()),
            )
            .raw(
                "lease_seq",
                self.lease
                    .map(|l| l.1.to_string())
                    .unwrap_or_else(|| "null".into()),
            )
            .raw(
                "denied_by",
                self.denied_by
                    .map(|d| json::s(d.label()))
                    .unwrap_or_else(|| "null".into()),
            )
            .build()
    }
}

/// Visible-indicator hook. The host app shows a border/badge on the target
/// window while a session is active; the crate only emits events.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Started {
        session: String,
        pid: i32,
        app_id: String,
        label: String,
    },
    Acted {
        session: String,
        action: String,
        tier: Option<Tier>,
        result: ResultKind,
    },
    Ended {
        session: String,
    },
}

pub trait Indicator: Send + Sync {
    fn event(&self, e: &SessionEvent);
}

pub trait EvidenceSink: Send + Sync {
    fn record(&self, r: &Receipt);
}

/// In-memory receipts (tests, short runs).
#[derive(Default)]
pub struct MemoryEvidence(pub Mutex<Vec<Receipt>>);

impl EvidenceSink for MemoryEvidence {
    fn record(&self, r: &Receipt) {
        self.0.lock().unwrap().push(r.clone());
    }
}
impl MemoryEvidence {
    pub fn all(&self) -> Vec<Receipt> {
        self.0.lock().unwrap().clone()
    }
}

/// Collects indicator events (tests, short runs).
#[derive(Default)]
pub struct MemoryIndicator(pub Mutex<Vec<SessionEvent>>);

impl Indicator for MemoryIndicator {
    fn event(&self, e: &SessionEvent) {
        self.0.lock().unwrap().push(e.clone());
    }
}
impl MemoryIndicator {
    pub fn all(&self) -> Vec<SessionEvent> {
        self.0.lock().unwrap().clone()
    }
}

/// Append-only JSON-lines file.
pub struct JsonlEvidence(Mutex<std::fs::File>);

impl JsonlEvidence {
    /// `path` must be absolute: evidence never lands relative to the working directory.
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        if !path.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence path must be absolute (refusing a cwd-relative location)",
            ));
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        Ok(JsonlEvidence(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        )))
    }
}
impl EvidenceSink for JsonlEvidence {
    fn record(&self, r: &Receipt) {
        let _ = writeln!(self.0.lock().unwrap(), "{}", r.to_json());
    }
}

pub type SharedPolicy = Arc<Policy>;

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, pid: i32) -> AppInfo {
        AppInfo {
            pid,
            app_id: id.into(),
            name: id.into(),
            path: None,
            started: None,
        }
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let p = Policy::default();
        assert!(matches!(
            p.check(&app("com.apple.TextEdit", 1)),
            Err(Error::Blocked(_))
        ));
    }

    #[test]
    fn allowlist_deny_list_and_self_protection() {
        let p = Policy::new(["com.apple.TextEdit", "com.apple.systempreferences"]);
        assert!(p.check(&app("com.apple.textedit", 1)).is_ok());
        assert!(
            p.check(&app("com.apple.systempreferences", 2)).is_err(),
            "deny list beats allowlist"
        );
        p.protect_pid(1);
        assert!(p.check(&app("com.apple.TextEdit", 1)).is_err());
        assert!(p.check(&app("com.apple.TextEdit", 9)).is_ok());
        p.revoke("com.apple.TextEdit");
        assert!(p.check(&app("com.apple.TextEdit", 9)).is_err());
    }

    #[test]
    fn denied_id_is_refused_even_when_allowed() {
        let p = Policy::new(["com.apple.TextEdit", "com.test.Other"]);
        p.deny("com.apple.textedit");
        assert!(
            matches!(
                p.check(&app("com.apple.TextEdit", 1)),
                Err(Error::Blocked(_))
            ),
            "deny beats the allow list, case-insensitively"
        );
        assert!(
            p.check(&app("com.test.Other", 2)).is_ok(),
            "other allowed ids stay allowed"
        );
    }

    #[test]
    fn risk_and_redaction() {
        assert_eq!(classify_risk("Delete account"), Some(RiskClass::Delete));
        assert_eq!(classify_risk("Send"), Some(RiskClass::Send));
        assert_eq!(classify_risk("Buy now"), Some(RiskClass::Purchase));
        assert_eq!(classify_risk("Close window"), Some(RiskClass::Close));
        assert_eq!(classify_risk("Share"), Some(RiskClass::Send));
        assert_eq!(classify_risk("Book table"), Some(RiskClass::Purchase));
        assert_eq!(classify_risk("Resend"), None);
        assert_eq!(classify_risk("Equals"), None);
        assert!(looks_secret(&format!("{}{}", "sk", "-example-value")));
        assert!(looks_secret("A1b2C3d4E5f6G7h8I9j0K1l2M3"));
        assert!(!looks_secret("hello world"));
        assert_eq!(redact("hello", false, false), "<5 chars>");
        assert_eq!(redact("hello", false, true), "hello");
        assert_eq!(redact("hello", true, true), "<redacted 5 chars>");
    }

    #[test]
    fn credential_labels() {
        for l in [
            "Password",
            "Confirm password",
            "API key",
            "Card number",
            "One-time code",
            "Security Code",
            "Client secret",
            "Access token",
            "PIN",
            "IBAN",
            "Passport number",
            "Bank account",
            "Driver licence",
        ] {
            assert!(credential_label(l), "{l}");
        }
        for l in ["Search", "Body", "Keyboard", "Name", "Account settings"] {
            assert!(!credential_label(l), "{l}");
        }
    }

    #[test]
    fn receipt_json_shape() {
        let r = Receipt {
            seq: 1,
            session: "s1".into(),
            ts_ms: 5,
            pid: 3,
            app_id: "a".into(),
            window_id: Some(9),
            snapshot: None,
            element: Some("e1".into()),
            action: "press".into(),
            value: None,
            attempts: vec![Attempt {
                tier: Tier::Semantic,
                ok: true,
                detail: "AXPress".into(),
            }],
            tier_used: Some(Tier::Semantic),
            frontmost_before: Some(1),
            frontmost_after: Some(1),
            focus_moved: false,
            result: ResultKind::Done,
            error: None,
            lease: None,
            denied_by: None,
        };
        let j = r.to_json();
        assert!(j.contains("\"tier_used\":\"semantic\""));
        assert!(j.contains("\"window_id\":9"));
        assert!(j.contains("\"snapshot\":null"));
    }
}
