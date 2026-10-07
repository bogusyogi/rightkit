use crate::error::{Error, Result};
use crate::namespace::Namespace;
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

/// One OS credential store. Implementations must never return an error that embeds a value.
pub trait Backend: Send + Sync + 'static {
    fn name(&self) -> &'static str;
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
}

/// In-process store (guard mode, tests). Never touches the OS.
#[derive(Default)]
pub struct MemoryBackend {
    map: Mutex<HashMap<(String, String), Secret>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
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
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(&Self::key(ns, account))
            .cloned())
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .contains_key(&Self::key(ns, account)))
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
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
        let mut m = self.map.lock().unwrap();
        let k = Self::key(ns, account);
        if m.contains_key(&k) {
            return Ok(false);
        }
        m.insert(k, secret.clone());
        Ok(true)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .remove(&Self::key(ns, account))
            .is_some())
    }
}

/// Platforms without a wired OS store (Linux today): every call fails closed.
pub struct UnavailableBackend(pub &'static str);

impl Backend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "unavailable"
    }
    fn get(&self, _: &Namespace, _: &str, _: Interaction) -> Result<Option<Secret>> {
        Err(Error::Unavailable(self.0.into()))
    }
    fn exists(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
        Err(Error::Unavailable(self.0.into()))
    }
    fn set(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<()> {
        Err(Error::Unavailable(self.0.into()))
    }
    fn add_if_absent(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<bool> {
        Err(Error::Unavailable(self.0.into()))
    }
    fn delete(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
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
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Arc::new(UnavailableBackend(
            "no OS credential store is wired for this platform",
        ))
    }
}
