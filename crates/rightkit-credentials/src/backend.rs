use crate::error::{Error, Result};
use crate::namespace::{check_target, windows_credential_identity, Namespace};
use crate::secret::Secret;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Whether the OS may show an authorization prompt for this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interaction {
    /// Background/runtime default: anything that would prompt fails with
    /// [`Error::InteractionRequired`].
    #[default]
    NonInteractive,
    /// Only for an explicit user action (a "Save key" click); may raise the OS prompt.
    Interactive,
}

/// Outcome of [`Backend::delete_if_equals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalDelete {
    /// The item still held the expected value and was removed.
    Deleted,
    /// There was no item (already removed).
    Absent,
    /// The item exists but no longer holds the expected value (another writer replaced it);
    /// nothing was removed.
    Changed,
    /// This backend cannot make "compare, then delete" atomic against its writers, so it removed
    /// nothing. Callers must treat the item as still present.
    Unsupported,
}

/// One OS credential store. Implementations must never return an error that embeds a value.
pub trait Backend: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// A stable identity for the credential store this instance writes to, shared by EVERY
    /// instance of the backend that addresses the same store (for the OS backends: the same
    /// user-default keychain or Credential Manager). `Store` serialises mutations of one
    /// credential per identity, so two `Store`s reopened over separate instances of the same OS
    /// store still exclude each other, including past a timeout. The default, `None`, means "this
    /// instance only" (identified by its allocation), which is right for in-memory and test
    /// backends and wrong for any backend whose instances share external state.
    ///
    /// For a backend whose target can change between calls (see [`Backend::pinned`]) this is the
    /// identity of the store a PINNED backend is bound to; it is never resolved afresh.
    fn exclusion_identity(&self) -> Option<String> {
        None
    }
    /// Decide ONCE which store the next mutation addresses, for a backend whose target can change
    /// between calls (the macOS user-default keychain is whichever keychain the user has set as
    /// default at that moment). `Store` calls this before it mutates: it takes the mutation's
    /// exclusion identity from the returned backend and makes the mutation on that same backend,
    /// so the store that is locked and the store that is written cannot differ, however the
    /// default changes in between.
    ///
    /// Return `Ok(Some(backend))` with a backend fixed to the store resolved just now; it MUST
    /// report [`Backend::exclusion_identity`] as `Some(..)` (`Store` refuses the mutation
    /// otherwise). Return `Err` when the target cannot be decided: the mutation is not attempted.
    /// The default, `Ok(None)`, is right for every backend that addresses one store for its whole
    /// life: `Store` then uses the backend itself.
    fn pinned(&self) -> Result<Option<Arc<dyn Backend>>> {
        Ok(None)
    }
    /// The identity of the ONE native credential `(ns, account)` addresses in this backend, used
    /// (together with [`Backend::exclusion_identity`]) to serialise mutations. Two calls that can
    /// reach the same native item MUST return the same string, whatever differs in `ns`; this is
    /// not the Windows target for a backend that does not address by Windows target.
    ///
    /// The default is [`windows_credential_identity`] (right for any backend keyed on the
    /// Windows target shape). A backend that addresses by anything else MUST override it: the
    /// macOS Keychain addresses by service and account only, so it uses
    /// [`crate::macos_credential_identity`]; a backend keyed on service and account without case
    /// folding may return its own exact key.
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        windows_credential_identity(ns, account)
    }
    /// Whether a value this backend accepts survives the process. Defaults to `false`
    /// (fail closed): migration deletes its source only after the copy is in a backend that
    /// says `true`, because copying into process memory and then deleting the persistent copy
    /// would lose the secret on exit. The OS backends return `true`; [`MemoryBackend`] returns
    /// `false` (see [`MemoryBackend::simulating_durable`] for tests).
    fn durable(&self) -> bool {
        false
    }
    /// `Ok(None)` ONLY when the store authoritatively reports the item absent.
    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>>;
    /// Passive presence: must not request secret bytes and must not prompt.
    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool>;
    /// Create or replace.
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()>;
    /// Create only if absent; `Ok(false)` when an item already exists. Atomic where the OS allows.
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool>;
    /// `Ok(true)` if an item was removed, `Ok(false)` if there was none.
    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool>;
    /// Remove the item only if it still holds `expected`, as one step that no writer of this
    /// store can interleave with. The default is [`ConditionalDelete::Unsupported`]: it removes
    /// nothing, which is the safe answer for a store that cannot compare and delete atomically
    /// (a plain read followed by a delete would remove a value written in between). Migration
    /// uses this to delete a source it has copied, and keeps the source when it is unsupported.
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        mode: Interaction,
    ) -> Result<ConditionalDelete> {
        let _ = (expected, mode);
        check_target(ns, account)?;
        Ok(ConditionalDelete::Unsupported)
    }
}

/// In-process store (guard mode, tests). Never touches the OS.
#[derive(Default)]
pub struct MemoryBackend {
    map: Mutex<HashMap<(String, String), Secret>>,
    simulate_durable: bool,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
    /// A memory backend that REPORTS itself durable ([`Backend::durable`]), so tests can
    /// exercise destructive migration against a fake. It still loses everything on exit; never
    /// use it where the value matters.
    pub fn simulating_durable() -> Self {
        Self {
            simulate_durable: true,
            ..Self::default()
        }
    }
    /// Shared by every `Store` in this process that is guarded into memory.
    pub fn process_wide() -> Arc<MemoryBackend> {
        static SHARED: OnceLock<Arc<MemoryBackend>> = OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(MemoryBackend::new()))
            .clone()
    }
    fn key(ns: &Namespace, account: &str) -> (String, String) {
        (ns.service().to_string(), account.to_string())
    }
}

impl Backend for MemoryBackend {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        check_target(ns, account)?;
        // Exactly the key the map uses.
        let (service, account) = Self::key(ns, account);
        Ok(format!("memory:{service}\u{1f}{account}"))
    }
    fn durable(&self) -> bool {
        self.simulate_durable
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(&Self::key(ns, account))
            .cloned())
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Ok(self
            .map
            .lock()
            .unwrap()
            .contains_key(&Self::key(ns, account)))
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        check_target(ns, account)?;
        self.map
            .lock()
            .unwrap()
            .insert(Self::key(ns, account), secret.clone());
        Ok(())
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        let mut m = self.map.lock().unwrap();
        let k = Self::key(ns, account);
        if m.contains_key(&k) {
            return Ok(false);
        }
        m.insert(k, secret.clone());
        Ok(true)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Ok(self
            .map
            .lock()
            .unwrap()
            .remove(&Self::key(ns, account))
            .is_some())
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        // Compare and remove under the one map lock every writer of this store takes.
        let mut m = self.map.lock().unwrap();
        let k = Self::key(ns, account);
        let outcome = match m.get(&k) {
            None => ConditionalDelete::Absent,
            Some(current) if current.ct_eq(expected) => ConditionalDelete::Deleted,
            Some(_) => ConditionalDelete::Changed,
        };
        if outcome == ConditionalDelete::Deleted {
            m.remove(&k);
        }
        Ok(outcome)
    }
}

/// Platforms without a wired OS store: every call fails closed.
pub struct UnavailableBackend(pub &'static str);

impl Backend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "unavailable"
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
    fn set(&self, ns: &Namespace, account: &str, _: &Secret, _: Interaction) -> Result<()> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        _: &Secret,
        _: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        _: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        Err(Error::Unavailable(self.0.into()))
    }
}

/// The OS store for this platform.
pub fn system_backend() -> Arc<dyn Backend> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(crate::macos::KeychainBackend::system())
    }
    #[cfg(windows)]
    {
        Arc::new(crate::windows::CredentialManagerBackend)
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(crate::linux::SecretServiceBackend)
    }
    #[cfg(target_os = "android")]
    {
        // Resolves the bridge and files directory that `android::install` set, on every call;
        // until then each call fails with `Error::Unavailable`.
        Arc::new(crate::android::AndroidKeystoreBackend::system())
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux", target_os = "android")))]
    {
        Arc::new(UnavailableBackend(
            "no OS credential store is wired for this platform",
        ))
    }
}
