//! Linux Secret Service backend (GNOME Keyring, KWallet, KeePassXC and any other provider of
//! `org.freedesktop.secrets` on the user's session bus), over the `secret-service` crate 5.2.0
//! (blocking API, pure-Rust crypto, async-io runtime).
//!
//! ## Item layout (byte-compatible with the `keyring` ecosystem)
//! An item is addressed by two attributes, `service` = the [`Namespace`] service and `username` =
//! the account, in the user's default collection (the collection with alias `default`), with
//! content type `application/octet-stream` and the label `keyring:{account}@{service}`. That is
//! exactly the layout `keyring-core` with `zbus-secret-service-keyring-store` writes, so items
//! those crates wrote stay readable and items written here stay readable by them. The secret
//! bytes are stored raw, as `keyring-core` stores them. A [`crate::WindowsTarget`] plays no part
//! on Linux. The `target` modifier of that store (a collection other than `default`) is not
//! supported: an item found by `service` and `username` is used wherever it lives, but a new item
//! always goes into the default collection.
//!
//! ## Fails closed
//! * No session bus, no provider, or no `default` collection: [`Error::Unavailable`]. There is no
//!   fallback to a file, to memory or to another collection.
//! * More than one item matches `service` and `username`: an error, never a guess. Nothing is
//!   read, replaced or deleted.
//! * A locked item or collection with [`Interaction::NonInteractive`]:
//!   [`Error::InteractionRequired`]. The unlock prompt is raised only for
//!   [`Interaction::Interactive`]; a dismissed prompt is [`Error::Denied`]. `exists` never
//!   requests the secret and never unlocks: a search reports locked items too.
//! * Errors carry an operation name and a small code only, never a provider message.
//!
//! ## Exclusion
//! The Secret Service has no compare-and-set. Every mutation (`set`, `add_if_absent`, `delete`,
//! `delete_if_equals`) holds an exclusive `flock` on `$XDG_RUNTIME_DIR/rightkit-credentials.lock`
//! for its whole find-then-write, so writers that use this crate exclude each other across
//! threads and processes of one user (the lock is per user, not per credential). The directory
//! must be absolute, owned by the current user and closed to group and others, and the lock file
//! must be a regular file of the same owner with no group or other access; otherwise the mutation
//! is refused. The lock waits without a limit; `Store` bounds the call. The lock cannot order the
//! `keyring` crate or other tools. Reads take no lock.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::locked::{self, CredentialOps};
use crate::namespace::{check_target, Namespace};
use crate::secret::Secret;
use secret_service::blocking::{Item, SecretService};
use secret_service::{EncryptionType, Error as SsError};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use zeroize::Zeroizing;

const CONTENT_TYPE: &str = "application/octet-stream";
const LOCK_FILE: &str = "rightkit-credentials.lock";

/// The Secret Service as the Linux credential store.
pub struct SecretServiceBackend;

fn unavailable(why: &str) -> Error {
    Error::Unavailable(why.into())
}

/// Maps a `secret-service` error without carrying its text (a provider message could name an
/// item). `Locked` is what a non-interactive call hit; a dismissed prompt is a denial.
fn map(op: &'static str, e: SsError) -> Error {
    match e {
        SsError::Locked => Error::InteractionRequired,
        SsError::Prompt => Error::Denied,
        SsError::Unavailable => unavailable("no Secret Service provider or session bus"),
        SsError::NoResult => unavailable("the Secret Service has no default collection"),
        SsError::PromptDisconnected => Error::Backend { op, code: -2 },
        SsError::Crypto(_) => Error::Backend { op, code: -3 },
        _ => Error::Backend { op, code: -1 },
    }
}

/// A new session with the provider. Any failure to establish one means there is no usable store.
fn connect() -> Result<SecretService<'static>> {
    SecretService::connect(EncryptionType::Dh)
        .map_err(|_| unavailable("cannot open a Secret Service session"))
}

fn attributes<'a>(ns: &'a Namespace, account: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("service", ns.service()), ("username", account)])
}

/// The one item an `(ns, account)` pair addresses.
struct Found<'a> {
    item: Item<'a>,
    locked: bool,
}

fn find<'a>(ss: &'a SecretService<'a>, ns: &Namespace, account: &str) -> Result<Option<Found<'a>>> {
    let r = ss
        .search_items(attributes(ns, account))
        .map_err(|e| map("search", e))?;
    let mut all: Vec<Found<'a>> = r
        .unlocked
        .into_iter()
        .map(|item| Found {
            item,
            locked: false,
        })
        .chain(
            r.locked
                .into_iter()
                .map(|item| Found { item, locked: true }),
        )
        .collect();
    match all.len() {
        0 => Ok(None),
        1 => Ok(all.pop()),
        _ => Err(Error::Backend {
            op: "ambiguous match",
            code: all.len() as i64,
        }),
    }
}

/// The unlocked Secret Service primitives; callers hold the user lock.
struct Ops {
    mode: Interaction,
}

impl Ops {
    fn unlock_item(&self, ss: &SecretService<'_>, f: &Found<'_>) -> Result<()> {
        if !f.locked {
            return Ok(());
        }
        match self.mode {
            Interaction::NonInteractive => Err(Error::InteractionRequired),
            Interaction::Interactive => {
                ss.unlock_all(&[&f.item]).map_err(|e| map("unlock", e))?;
                // A dismissed prompt can return without unlocking; never write or read on hope.
                if f.item.is_locked().map_err(|e| map("unlock", e))? {
                    return Err(Error::Denied);
                }
                Ok(())
            }
        }
    }
}

fn lock_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| unavailable("XDG_RUNTIME_DIR is not set to an absolute path"))?;
    // The current user's uid, read from a path the kernel always owns as that user.
    let me = std::fs::metadata("/proc/self")
        .map_err(|_| unavailable("cannot identify the current user"))?
        .uid();
    let meta = std::fs::metadata(&dir).map_err(|_| unavailable("XDG_RUNTIME_DIR is unusable"))?;
    if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(unavailable("XDG_RUNTIME_DIR is not private to this user"));
    }
    Ok(dir.join(LOCK_FILE))
}

fn open_lock() -> Result<File> {
    let path = lock_path()?;
    let me = std::fs::metadata("/proc/self")
        .map_err(|_| unavailable("cannot identify the current user"))?
        .uid();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|_| unavailable("cannot open the credential lock file"))?;
    let meta = file
        .metadata()
        .map_err(|_| unavailable("cannot inspect the credential lock file"))?;
    if !meta.is_file() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(unavailable(
            "the credential lock file is not private to this user",
        ));
    }
    Ok(file)
}

impl CredentialOps for Ops {
    /// The open lock file; the `flock` is released when it drops.
    type Guard = File;

    fn lock(&self, _normalized_target: &str, _ns: &Namespace, _account: &str) -> Result<File> {
        let file = open_lock()?;
        file.lock().map_err(|_| Error::Backend {
            op: "lock",
            code: -1,
        })?;
        Ok(file)
    }

    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
        let ss = connect()?;
        let Some(found) = find(&ss, ns, account)? else {
            return Ok(None);
        };
        if !want_blob {
            // Presence only: no unlock, no secret bytes.
            return Ok(Some(Secret::from_bytes(Vec::new())));
        }
        self.unlock_item(&ss, &found)?;
        let raw = Zeroizing::new(found.item.get_secret().map_err(|e| map("read", e))?);
        Ok(Some(Secret::from_bytes(raw.to_vec())))
    }

    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()> {
        let ss = connect()?;
        if let Some(found) = find(&ss, ns, account)? {
            self.unlock_item(&ss, &found)?;
            return found
                .item
                .set_secret(secret.expose_bytes(), CONTENT_TYPE)
                .map_err(|e| map("write", e));
        }
        let collection = ss
            .get_default_collection()
            .map_err(|e| map("default collection", e))?;
        if collection.is_locked().map_err(|e| map("write", e))? {
            match self.mode {
                Interaction::NonInteractive => return Err(Error::InteractionRequired),
                Interaction::Interactive => {
                    collection.unlock().map_err(|e| map("unlock", e))?;
                    if collection.is_locked().map_err(|e| map("unlock", e))? {
                        return Err(Error::Denied);
                    }
                }
            }
        }
        let label = format!("keyring:{account}@{}", ns.service());
        collection
            .create_item(
                &label,
                attributes(ns, account),
                secret.expose_bytes(),
                false,
                CONTENT_TYPE,
            )
            .map_err(|e| map("write", e))?;
        Ok(())
    }

    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool> {
        let ss = connect()?;
        let Some(found) = find(&ss, ns, account)? else {
            return Ok(false);
        };
        self.unlock_item(&ss, &found)?;
        found.item.delete().map_err(|e| map("delete", e))?;
        Ok(true)
    }
}

impl Backend for SecretServiceBackend {
    fn name(&self) -> &'static str {
        "linux-secret-service"
    }
    fn exclusion_identity(&self) -> Option<String> {
        // Every instance addresses the one per-user Secret Service.
        Some("linux-secret-service".to_string())
    }
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        check_target(ns, account)?;
        // Secret Service attributes match exactly (case-sensitive): no folding.
        Ok(format!("linux:{}\u{1f}{account}", ns.service()))
    }
    fn durable(&self) -> bool {
        true
    }
    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Ops { mode }.read(ns, account, true)
    }
    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Ok(Ops { mode }.read(ns, account, false)?.is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()> {
        check_target(ns, account)?;
        locked::set(&Ops { mode }, ns, account, secret)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        locked::add_if_absent(&Ops { mode }, ns, account, secret)
    }
    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        locked::delete(&Ops { mode }, ns, account)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        mode: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        locked::delete_if_equals(&Ops { mode }, ns, account, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_without_provider_text() {
        assert_eq!(map("x", SsError::Locked), Error::InteractionRequired);
        assert_eq!(map("x", SsError::Prompt), Error::Denied);
        assert!(matches!(
            map("x", SsError::Unavailable),
            Error::Unavailable(_)
        ));
        assert!(matches!(map("x", SsError::NoResult), Error::Unavailable(_)));
        assert_eq!(
            map("x", SsError::Crypto("secret detail")),
            Error::Backend { op: "x", code: -3 }
        );
    }

    #[test]
    fn identity_is_exact_and_validates_the_target() {
        let b = SecretServiceBackend;
        let ns = Namespace::new("dev.rightkit.test").unwrap();
        assert_eq!(
            b.credential_identity(&ns, "Acct").unwrap(),
            "linux:dev.rightkit.test\u{1f}Acct"
        );
        assert_ne!(
            b.credential_identity(&ns, "acct").unwrap(),
            b.credential_identity(&ns, "Acct").unwrap()
        );
        assert!(b.credential_identity(&ns, "").is_err());
    }

    #[test]
    fn attributes_are_the_keyring_core_pair() {
        let ns = Namespace::new("svc").unwrap();
        let a = attributes(&ns, "user");
        assert_eq!(a.len(), 2);
        assert_eq!(a["service"], "svc");
        assert_eq!(a["username"], "user");
    }

    /// With no session bus every call fails closed (no fallback, no panic).
    #[test]
    fn no_session_bus_is_unavailable() {
        // zbus also falls back to `$XDG_RUNTIME_DIR/bus`; skip when any bus could be reached.
        let fallback_bus = std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|d| std::path::Path::new(&d).join("bus").exists());
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() || fallback_bus {
            return;
        }
        let ns = Namespace::new("dev.rightkit.test.nobus").unwrap();
        let b = SecretServiceBackend;
        let ni = Interaction::NonInteractive;
        assert!(matches!(b.get(&ns, "a", ni), Err(Error::Unavailable(_))));
        assert!(matches!(b.exists(&ns, "a", ni), Err(Error::Unavailable(_))));
    }
}
