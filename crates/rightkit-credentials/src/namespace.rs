use crate::error::{Error, Result};

/// `CRED_MAX_GENERIC_TARGET_NAME_LENGTH`: the longest generic-credential target name, in UTF-16
/// code units.
const MAX_WINDOWS_TARGET_UNITS: usize = 32_767;

/// How a namespace + account maps to a Windows Credential Manager target name.
/// macOS always uses (service = namespace service, account = account).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsTarget {
    /// `"{prefix}/{account}"` (prefix defaults to the service). `<prefix>/<account>` shape:
    /// prefix `MyApp`, account `SQLCipher/DBKey`.
    ///
    /// A prefix must be non-empty, at most 250 bytes and free of control characters (Windows
    /// ends a target name at the first NUL, so `"victim\0x"` would address `victim` for every
    /// account). Use [`Namespace::try_windows_target`] to have it checked when the namespace is
    /// built; every [`crate::Backend`] checks it again before any OS call.
    Slash { prefix: Option<String> },
    /// `"{account}.{service}"`: what the `keyring` crate writes on Windows (keyring-based apps).
    /// Reads and migrates their existing items in place.
    ///
    /// The blob encoding is selected by this variant and nothing else: UTF-16LE text with no
    /// terminator, for reads and writes alike, exactly as `keyring` stores it. A secret that is
    /// not valid UTF-8 text cannot be stored (`Error::NotUtf8`), and a stored blob that is not
    /// valid UTF-16LE is `Error::NotUtf8`; neither is guessed at. Items that rightkit-credentials
    /// 0.1.1 and 0.1.2 wrote as raw UTF-8 are read through
    /// [`WindowsTarget::KeyringCompatLegacyUtf8`].
    KeyringCompat,
    /// The same target name as [`WindowsTarget::KeyringCompat`], but the blob is the raw bytes
    /// that 0.1.1 and 0.1.2 (published as rightkit-secrets) stored (UTF-8 text, or binary). Explicit migration
    /// path only: it is read and delete only (`set` and `add_if_absent` are refused), and a
    /// caller migrates an item it knows is legacy by reading it through a store that uses this
    /// variant and writing it through a store that uses [`WindowsTarget::KeyringCompat`]. There
    /// is no automatic detection, because an even-length UTF-8 blob and a UTF-16LE blob can be
    /// byte-for-byte identical.
    KeyringCompatLegacyUtf8,
}

/// How a Windows credential blob maps to secret bytes. Chosen by the [`WindowsTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum BlobCodec {
    /// The blob is the secret.
    Raw,
    /// UTF-16LE text (the `keyring` crate's layout).
    KeyringUtf16,
    /// Raw legacy bytes, read and delete only.
    LegacyUtf8,
}

/// One app's credential namespace (the Keychain `service`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    service: String,
    windows: WindowsTarget,
}

fn check(what: &'static str, v: &str) -> Result<()> {
    if v.is_empty() {
        return Err(Error::Invalid {
            what,
            reason: "empty".into(),
        });
    }
    if v.len() > 250 {
        return Err(Error::Invalid {
            what,
            reason: "longer than 250 bytes".into(),
        });
    }
    if v.chars().any(|c| c.is_control()) {
        return Err(Error::Invalid {
            what,
            reason: "contains a control character".into(),
        });
    }
    Ok(())
}

pub(crate) fn check_account(account: &str) -> Result<()> {
    check("account", account)
}

/// Validation every [`crate::Backend`] applies to its inputs before any OS call, so a caller that
/// bypasses [`crate::Store`] still cannot pass an empty account (which the macOS Keychain treats
/// as "any account"), an otherwise invalid service or account, or a Windows target prefix that
/// would truncate or exceed the native target name.
pub(crate) fn check_target(ns: &Namespace, account: &str) -> Result<()> {
    check("namespace", ns.service())?;
    ns.windows_target_checked(account).map(|_| ())
}

/// The name two credential targets are serialised under. Windows treats target names
/// case-insensitively, so names that differ only in case must share one lock; treating a few more
/// names as equal only costs a little parallelism.
pub(crate) fn normalize_target_name(target: &str) -> String {
    target
        .chars()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Separator inside a credential identity. A control character, so it cannot occur in a service
/// or an account (both validated to contain none) and the join is injective.
const IDENTITY_SEP: char = '\u{1f}';

/// The name of one credential on macOS: the Keychain `service` and `account`, folded so that
/// names differing only in case share one exclusion key (treating a few more names as equal only
/// costs a little parallelism). The ONLY place the macOS identity is derived; the Keychain
/// backend's mutation exclusion uses exactly this. Windows target prefixes play no part in it.
///
/// Format: `macos:<folded service>\u{1f}<folded account>`.
pub fn macos_credential_identity(ns: &Namespace, account: &str) -> Result<String> {
    check_target(ns, account)?;
    let (service, account) = ns.keychain_item(account);
    Ok(format!(
        "macos:{}{IDENTITY_SEP}{}",
        normalize_target_name(service),
        normalize_target_name(account)
    ))
}

/// The name of one credential on Windows: the validated Credential Manager target, normalised
/// (Windows compares target names case-insensitively). The ONLY place the Windows identity is
/// derived; the Windows backend's lock name and the store's mutation exclusion both use it.
///
/// Format: `windows:<normalised target>`.
pub fn windows_credential_identity(ns: &Namespace, account: &str) -> Result<String> {
    Ok(format!("windows:{}", ns.windows_lock_target(account)?))
}

impl Namespace {
    /// The Keychain `(service, account)` a macOS operation addresses. Every native macOS call
    /// (find, add, modify, delete) takes its names from here and from nowhere else.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn keychain_item<'a>(&'a self, account: &'a str) -> (&'a str, &'a str) {
        (self.service.as_str(), account)
    }

    /// The normalised, validated Windows target the per-target lock and the exclusion identity
    /// are keyed on.
    pub(crate) fn windows_lock_target(&self, account: &str) -> Result<String> {
        Ok(normalize_target_name(
            &self.windows_target_checked(account)?,
        ))
    }

    /// `service` is frozen per app (changing it orphans every stored item): use the bundle
    /// identifier, e.g. `app.example.myapp`.
    pub fn new(service: impl Into<String>) -> Result<Self> {
        let service = service.into();
        check("namespace", &service)?;
        Ok(Self {
            service,
            windows: WindowsTarget::Slash { prefix: None },
        })
    }

    /// Select the Windows target shape. This does not validate (it cannot fail): an invalid
    /// prefix is rejected with [`Error::Invalid`] by every backend call. Prefer
    /// [`Namespace::try_windows_target`].
    pub fn windows_target(mut self, style: WindowsTarget) -> Self {
        self.windows = style;
        self
    }

    /// Select the Windows target shape and validate it now: a prefix that is empty, longer than
    /// 250 bytes or contains a control character (NUL included) is `Error::Invalid`.
    pub fn try_windows_target(self, style: WindowsTarget) -> Result<Self> {
        let candidate = Self {
            windows: style,
            ..self
        };
        candidate.check_windows_style()?;
        Ok(candidate)
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn blob_codec(&self) -> BlobCodec {
        match self.windows {
            WindowsTarget::Slash { .. } => BlobCodec::Raw,
            WindowsTarget::KeyringCompat => BlobCodec::KeyringUtf16,
            WindowsTarget::KeyringCompatLegacyUtf8 => BlobCodec::LegacyUtf8,
        }
    }

    fn check_windows_style(&self) -> Result<()> {
        if let WindowsTarget::Slash {
            prefix: Some(prefix),
        } = &self.windows
        {
            check("windows target prefix", prefix)?;
        }
        Ok(())
    }

    /// The validated Windows Credential Manager target for an account: service, account and
    /// prefix checked, no control character (so no NUL to truncate it), and within the native
    /// length limit. Backends and the per-target locks use exactly this name.
    pub(crate) fn windows_target_checked(&self, account: &str) -> Result<String> {
        check_account(account)?;
        self.check_windows_style()?;
        let name = self.windows_target_name(account);
        if name.chars().any(char::is_control) {
            return Err(Error::Invalid {
                what: "windows target name",
                reason: "contains a control character".into(),
            });
        }
        if name.encode_utf16().count() > MAX_WINDOWS_TARGET_UNITS {
            return Err(Error::Invalid {
                what: "windows target name",
                reason: "longer than the Credential Manager limit".into(),
            });
        }
        Ok(name)
    }

    /// Windows Credential Manager target for an account.
    pub fn windows_target_name(&self, account: &str) -> String {
        match &self.windows {
            WindowsTarget::Slash { prefix } => {
                format!("{}/{}", prefix.as_deref().unwrap_or(&self.service), account)
            }
            WindowsTarget::KeyringCompat | WindowsTarget::KeyringCompatLegacyUtf8 => {
                format!("{}.{}", account, self.service)
            }
        }
    }
}
