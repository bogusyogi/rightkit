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
    /// valid UTF-16LE is `Error::NotUtf8`; neither is guessed at. Items that rightkit-secrets
    /// 0.1.1 and 0.1.2 wrote as raw UTF-8 are read through
    /// [`WindowsTarget::KeyringCompatLegacyUtf8`].
    KeyringCompat,
    /// The same target name as [`WindowsTarget::KeyringCompat`], but the blob is the raw bytes
    /// that rightkit-secrets 0.1.1 and 0.1.2 stored (UTF-8 text, or binary). Explicit migration
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

impl Namespace {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn slash(prefix: &str) -> WindowsTarget {
        WindowsTarget::Slash {
            prefix: Some(prefix.to_string()),
        }
    }

    #[test]
    fn a_prefix_with_nul_or_a_control_character_is_rejected_at_construction() {
        let ns = Namespace::new("svc").unwrap();
        for bad in [
            "victim\0ignored",
            "\0",
            "new\nline",
            "tab\there",
            "del\u{7f}",
            "c1\u{85}",
            "",
        ] {
            assert!(
                matches!(
                    ns.clone().try_windows_target(slash(bad)),
                    Err(Error::Invalid {
                        what: "windows target prefix",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        assert!(ns
            .clone()
            .try_windows_target(slash(&"p".repeat(251)))
            .is_err());
        assert!(ns
            .clone()
            .try_windows_target(slash(&"p".repeat(250)))
            .is_ok());
        assert!(ns.try_windows_target(slash("Valid Prefix")).is_ok());
    }

    #[test]
    fn an_unvalidated_bad_prefix_is_still_rejected_at_the_backend_boundary() {
        let ns = Namespace::new("svc")
            .unwrap()
            .windows_target(slash("victim\0ignored"));
        assert!(matches!(
            check_target(&ns, "acct"),
            Err(Error::Invalid {
                what: "windows target prefix",
                ..
            })
        ));
        assert!(ns.windows_target_checked("acct").is_err());
        // A good account on a good namespace passes.
        let good = Namespace::new("svc").unwrap();
        assert_eq!(good.windows_target_checked("acct").unwrap(), "svc/acct");
    }

    #[test]
    fn the_checked_target_is_the_name_the_account_isolation_depends_on() {
        let ns = Namespace::new("svc")
            .unwrap()
            .windows_target(slash("shared"));
        let a = ns.windows_target_checked("alice").unwrap();
        let b = ns.windows_target_checked("bob").unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("shared/alice", "shared/bob"));
        assert!(!a.contains('\0') && !b.contains('\0'));
    }

    #[test]
    fn services_sharing_a_prefix_share_one_normalised_target() {
        let a = Namespace::new("svc-a")
            .unwrap()
            .windows_target(slash("Shared"));
        let b = Namespace::new("svc-b")
            .unwrap()
            .windows_target(slash("SHARED"));
        let (ta, tb) = (
            a.windows_target_checked("acct").unwrap(),
            b.windows_target_checked("acct").unwrap(),
        );
        assert_ne!(ta, tb, "the raw names differ only in case");
        assert_eq!(normalize_target_name(&ta), normalize_target_name(&tb));
        // Different accounts stay different.
        assert_ne!(
            normalize_target_name(&ta),
            normalize_target_name(&a.windows_target_checked("other").unwrap())
        );
    }

    #[test]
    fn keyring_compat_variants_share_a_target_and_differ_in_codec() {
        let compat = Namespace::new("svc")
            .unwrap()
            .windows_target(WindowsTarget::KeyringCompat);
        let legacy = Namespace::new("svc")
            .unwrap()
            .windows_target(WindowsTarget::KeyringCompatLegacyUtf8);
        assert_eq!(
            compat.windows_target_name("acct"),
            legacy.windows_target_name("acct")
        );
        assert_eq!(compat.windows_target_name("acct"), "acct.svc");
        assert_eq!(compat.blob_codec(), BlobCodec::KeyringUtf16);
        assert_eq!(legacy.blob_codec(), BlobCodec::LegacyUtf8);
        assert_eq!(Namespace::new("svc").unwrap().blob_codec(), BlobCodec::Raw);
    }
}
