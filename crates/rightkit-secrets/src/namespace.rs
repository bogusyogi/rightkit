use crate::error::{Error, Result};

/// How a namespace + account maps to a Windows Credential Manager target name.
/// macOS always uses (service = namespace service, account = account).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsTarget {
    /// `"{prefix}/{account}"` (prefix defaults to the service). HeardRight/MailRight shape:
    /// prefix `HeardRight`, account `SQLCipher/DBKey`.
    Slash { prefix: Option<String> },
    /// `"{account}.{service}"`: what the `keyring` crate writes on Windows (GenRight,
    /// CodeRight, ScrapeRight, CutRight). Reads and migrates their existing items in place.
    KeyringCompat,
}

/// One app's credential namespace (the Keychain `service`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    service: String,
    windows: WindowsTarget,
}

fn check(what: &'static str, v: &str) -> Result<()> {
    if v.is_empty() {
        return Err(Error::Invalid { what, reason: "empty".into() });
    }
    if v.len() > 250 {
        return Err(Error::Invalid { what, reason: "longer than 250 bytes".into() });
    }
    if v.chars().any(|c| c.is_control()) {
        return Err(Error::Invalid { what, reason: "contains a control character".into() });
    }
    Ok(())
}

pub(crate) fn check_account(account: &str) -> Result<()> {
    check("account", account)
}

impl Namespace {
    /// `service` is frozen per app (changing it orphans every stored item): use the bundle
    /// identifier, e.g. `app.orthiclabs.genright`.
    pub fn new(service: impl Into<String>) -> Result<Self> {
        let service = service.into();
        check("namespace", &service)?;
        Ok(Self { service, windows: WindowsTarget::Slash { prefix: None } })
    }
    pub fn windows_target(mut self, style: WindowsTarget) -> Self {
        self.windows = style;
        self
    }
    pub fn service(&self) -> &str {
        &self.service
    }
    /// Windows Credential Manager target for an account.
    pub fn windows_target_name(&self, account: &str) -> String {
        match &self.windows {
            WindowsTarget::Slash { prefix } => {
                format!("{}/{}", prefix.as_deref().unwrap_or(&self.service), account)
            }
            WindowsTarget::KeyringCompat => format!("{}.{}", account, self.service),
        }
    }
}
