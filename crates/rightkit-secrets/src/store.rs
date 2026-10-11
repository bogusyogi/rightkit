use crate::backend::{system_backend, Backend, ConditionalDelete, Interaction, MemoryBackend};
use crate::error::{Error, Result};
use crate::namespace::{check_target, normalize_target_name, Namespace};
use crate::secret::Secret;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Standard padded base64 (RFC 4648) of `bytes`. The only encoder this crate needs, for minted keys.
fn base64_standard(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Universal guard variable honoured by every app (in addition to the app's own variables).
pub const UNIVERSAL_GUARD_ENV: &str = "RIGHTKIT_NO_KEYCHAIN";

/// What a tripped guard does. Neither touches the OS credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// Swap in a process-wide in-memory store (e.g. an app guard such as `MYAPP_NO_KEYCHAIN`).
    Memory,
    /// Every operation fails with [`Error::Guarded`] (e.g. an app guard such as `MYAPP_DISABLE_OS_KEYCHAIN`).
    Deny,
}

#[derive(Clone)]
pub struct Config {
    namespace: Namespace,
    interaction: Interaction,
    guard_envs: Vec<String>,
    guard_mode: GuardMode,
    forced_guard: bool,
    timeout: Option<Duration>,
    backend: Option<Arc<dyn Backend>>,
}

impl Config {
    pub fn new(namespace: Namespace) -> Self {
        Self {
            namespace,
            interaction: Interaction::NonInteractive,
            guard_envs: Vec::new(),
            guard_mode: GuardMode::Memory,
            forced_guard: false,
            timeout: None,
            backend: None,
        }
    }
    /// App-specific guard variable names, e.g. `MYAPP_NO_KEYCHAIN`. A guard is tripped by a
    /// variable that is set to anything except empty/`0`/`false`/`off`/`no`.
    pub fn guard_env(mut self, name: impl Into<String>) -> Self {
        self.guard_envs.push(name.into());
        self
    }
    pub fn guard_mode(mut self, mode: GuardMode) -> Self {
        self.guard_mode = mode;
        self
    }
    /// Force the guard from code, e.g. `.guard_when(cfg!(test), GuardMode::Deny)`; a consuming
    /// crate's own test binaries are not visible to this library.
    pub fn guard_when(mut self, condition: bool, mode: GuardMode) -> Self {
        if condition {
            self.forced_guard = true;
            self.guard_mode = mode;
        }
        self
    }
    pub fn interaction(mut self, mode: Interaction) -> Self {
        self.interaction = mode;
        self
    }
    /// Per-operation cap (default 5s non-interactive, 90s interactive).
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
    /// Replace the OS store (tests, isolated keychains). Guards still take precedence.
    pub fn backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = Some(backend);
        self
    }
}

fn env_tripped(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off" | "no"
        ),
        Err(std::env::VarError::NotUnicode(_)) => true,
        Err(_) => false,
    }
}

/// Non-secret description of how a store is wired (safe for diagnostics and `/health`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Diagnostics {
    pub namespace: String,
    pub backend: &'static str,
    pub interaction: &'static str,
    pub guard: Option<String>,
}

/// A handle on one namespace of one credential store.
///
/// ## Timeouts and retries
/// Every call runs on a worker thread so it can time out. A timed-out OS call cannot be
/// cancelled and keeps running. Mutations (`set`, `delete`, `add_if_absent`, `delete_if_equals`,
/// and so `get_or_create_key`) of one native credential are therefore serialised, and the
/// target stays claimed until the OS call actually returns: a call that is still within its
/// caller's deadline is waited for, and a retry that arrives while a timed-out call is still
/// running gets [`Error::InProgress`] instead of racing it (the stalled call could otherwise
/// finish later and overwrite the retry). Retry after a short delay. Reads are never refused for
/// this reason.
#[derive(Clone)]
pub struct Store {
    ns: Namespace,
    backend: Arc<dyn Backend>,
    mode: Interaction,
    timeout: Duration,
    guard: Option<(String, GuardMode)>,
}

struct Denied(String);

impl Backend for Denied {
    fn name(&self) -> &'static str {
        "guard-deny"
    }
    fn get(&self, _: &Namespace, _: &str, _: Interaction) -> Result<Option<Secret>> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
    fn exists(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
    fn set(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<()> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
    fn add_if_absent(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<bool> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
    fn delete(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
    fn delete_if_equals(
        &self,
        _: &Namespace,
        _: &str,
        _: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        Err(Error::Guarded {
            guard: self.0.clone(),
        })
    }
}

/// Serializes read→mint→persist inside this process; `add_if_absent` covers other processes.
static MINT_LOCK: Mutex<()> = Mutex::new(());

/// Where a mutation of one credential target stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// The OS call is running and its caller is still waiting for it.
    Running,
    /// The OS call is still running but its caller gave up (timed out).
    Abandoned,
}

/// Identity of a credential target: the backend's store identity and the normalised native target
/// name. Stores that share a backend store and a Windows prefix share a native target, so they
/// share a key, even when each was opened over its own backend instance.
type TargetKey = (String, String);

/// `Backend::exclusion_identity`, else the instance's allocation.
fn backend_identity(backend: &Arc<dyn Backend>) -> String {
    backend
        .exclusion_identity()
        .unwrap_or_else(|| format!("instance:{:p}", Arc::as_ptr(backend) as *const ()))
}

/// Mutations in flight, one per target. A timed-out OS call cannot be cancelled and keeps
/// running on its worker thread; its entry stays here until that call returns, so a retry can
/// neither start beside it nor be overwritten by it.
static IN_FLIGHT: Mutex<BTreeMap<TargetKey, (u64, Slot)>> = Mutex::new(BTreeMap::new());
static IN_FLIGHT_CHANGED: Condvar = Condvar::new();
static NEXT_PERMIT: AtomicU64 = AtomicU64::new(1);

fn in_flight() -> MutexGuard<'static, BTreeMap<TargetKey, (u64, Slot)>> {
    IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The exclusive right to mutate one credential target. It lives on the worker thread and is
/// released when the OS call returns, not when the caller stops waiting.
struct TargetPermit {
    key: TargetKey,
    id: u64,
}

impl TargetPermit {
    /// Take the target. Another mutation of it that is still within its caller's deadline is
    /// waited for (until `deadline`); one whose caller already timed out is not waited for.
    /// Either way, a target that is still busy is `Error::InProgress`, never a second writer.
    fn acquire(key: TargetKey, op: &'static str, deadline: Instant) -> Result<Self> {
        let mut map = in_flight();
        loop {
            match map.get(&key).map(|(_, slot)| *slot) {
                None => {
                    let id = NEXT_PERMIT.fetch_add(1, Ordering::Relaxed);
                    map.insert(key.clone(), (id, Slot::Running));
                    return Ok(Self { key, id });
                }
                Some(Slot::Abandoned) => return Err(Error::InProgress { op }),
                Some(Slot::Running) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(Error::InProgress { op });
                    }
                    map = IN_FLIGHT_CHANGED
                        .wait_timeout(map, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    /// Handle the waiting caller keeps so it can mark this call abandoned when it times out.
    fn ticket(&self) -> (TargetKey, u64) {
        (self.key.clone(), self.id)
    }
}

impl Drop for TargetPermit {
    fn drop(&mut self) {
        let mut map = in_flight();
        if map.get(&self.key).is_some_and(|(id, _)| *id == self.id) {
            map.remove(&self.key);
        }
        drop(map);
        IN_FLIGHT_CHANGED.notify_all();
    }
}

/// The caller stopped waiting for the call that holds `ticket`; it is still running.
fn abandon(ticket: &(TargetKey, u64)) {
    let mut map = in_flight();
    if let Some(entry) = map.get_mut(&ticket.0) {
        if entry.0 == ticket.1 {
            entry.1 = Slot::Abandoned;
        }
    }
    drop(map);
    IN_FLIGHT_CHANGED.notify_all();
}

impl Store {
    /// Resolve guards and the backend. Infallible: a tripped guard yields a memory or denying
    /// store, never the OS store.
    pub fn open(cfg: Config) -> Store {
        let tripped = cfg
            .guard_envs
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(UNIVERSAL_GUARD_ENV))
            .find(|n| env_tripped(n))
            .map(str::to_string)
            .or_else(|| cfg.forced_guard.then(|| "forced-by-caller".to_string()));
        let timeout = cfg.timeout.unwrap_or(match cfg.interaction {
            Interaction::NonInteractive => Duration::from_secs(5),
            Interaction::Interactive => Duration::from_secs(90),
        });
        let (backend, guard): (Arc<dyn Backend>, _) = match tripped {
            Some(name) => match cfg.guard_mode {
                GuardMode::Memory => (
                    MemoryBackend::process_wide(),
                    Some((name, GuardMode::Memory)),
                ),
                GuardMode::Deny => (
                    Arc::new(Denied(name.clone())),
                    Some((name, GuardMode::Deny)),
                ),
            },
            None => (cfg.backend.unwrap_or_else(system_backend), None),
        };
        Store {
            ns: cfg.namespace,
            backend,
            mode: cfg.interaction,
            timeout,
            guard,
        }
    }

    pub fn namespace(&self) -> &Namespace {
        &self.ns
    }
    pub fn guard_active(&self) -> bool {
        self.guard.is_some()
    }
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            namespace: self.ns.service().to_string(),
            backend: self.backend.name(),
            interaction: match self.mode {
                Interaction::NonInteractive => "non-interactive",
                Interaction::Interactive => "interactive",
            },
            guard: self.guard.as_ref().map(|(n, _)| n.clone()),
        }
    }

    /// Same backend and policy, another namespace (legacy services, `SecretRef.service`).
    pub fn with_namespace(&self, ns: Namespace) -> Store {
        Store { ns, ..self.clone() }
    }
    /// Same store, interactive. Use only for an explicit user action such as "Save key".
    pub fn interactive(&self) -> Store {
        let timeout = if self.mode == Interaction::Interactive {
            self.timeout
        } else {
            Duration::from_secs(90)
        };
        Store {
            mode: Interaction::Interactive,
            timeout,
            ..self.clone()
        }
    }

    fn run<T: Send + 'static>(
        &self,
        op: &'static str,
        account: &str,
        mutation: bool,
        f: impl FnOnce(Arc<dyn Backend>, Namespace, String, Interaction) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        check_target(&self.ns, account)?;
        let started = Instant::now();
        // A timeout too large to add to the clock is treated as "a year".
        let deadline = started
            .checked_add(self.timeout)
            .unwrap_or_else(|| started + Duration::from_secs(365 * 24 * 60 * 60));
        // Mutations of one native target run one at a time, and a call that timed out keeps its
        // place until the OS returns: a retry is refused rather than raced.
        let permit = if mutation {
            let key = (
                backend_identity(&self.backend),
                normalize_target_name(&self.ns.windows_target_name(account)),
            );
            Some(TargetPermit::acquire(key, op, deadline)?)
        } else {
            None
        };
        let ticket = permit.as_ref().map(TargetPermit::ticket);
        let (backend, ns, acct, mode) = (
            self.backend.clone(),
            self.ns.clone(),
            account.to_string(),
            self.mode,
        );
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = f(backend, ns, acct, mode);
            // Released before the result is sent, so a caller that has its answer can retry at once.
            drop(permit);
            let _ = tx.send(result);
        });
        // An abandoned thread stays blocked in the OS call until a prompt is dismissed, then exits.
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(_) => {
                if let Some(ticket) = &ticket {
                    abandon(ticket);
                }
                Err(Error::Timeout {
                    op,
                    secs: self.timeout.as_secs(),
                })
            }
        }
    }

    /// `Ok(None)` only when the store authoritatively reports the item absent.
    pub fn get(&self, account: &str) -> Result<Option<Secret>> {
        self.run("get", account, false, |b, ns, a, m| b.get(&ns, &a, m))
    }
    /// Passive presence. Never requests secret bytes and never prompts.
    pub fn exists(&self, account: &str) -> Result<bool> {
        self.run("presence", account, false, |b, ns, a, m| {
            b.exists(&ns, &a, m)
        })
    }
    /// Create or replace. An empty secret is rejected; use [`Store::delete`].
    pub fn set(&self, account: &str, secret: &Secret) -> Result<()> {
        if secret.is_empty() {
            return Err(Error::Invalid {
                what: "secret",
                reason: "empty (use delete)".into(),
            });
        }
        let s = secret.clone();
        self.run("set", account, true, move |b, ns, a, m| {
            b.set(&ns, &a, &s, m)
        })
    }
    pub fn set_str(&self, account: &str, value: &str) -> Result<()> {
        self.set(account, &Secret::from_string(value))
    }
    /// `Ok(true)` if an item was removed.
    pub fn delete(&self, account: &str) -> Result<bool> {
        self.run("delete", account, true, |b, ns, a, m| b.delete(&ns, &a, m))
    }
    /// Remove the item only if it still holds `expected`, as one step the store's writers cannot
    /// interleave with (see [`Backend::delete_if_equals`]). [`ConditionalDelete::Unsupported`]
    /// means nothing was removed.
    pub fn delete_if_equals(&self, account: &str, expected: &Secret) -> Result<ConditionalDelete> {
        let e = expected.clone();
        self.run("delete", account, true, move |b, ns, a, m| {
            b.delete_if_equals(&ns, &a, &e, m)
        })
    }
    pub fn add_if_absent(&self, account: &str, secret: &Secret) -> Result<bool> {
        let s = secret.clone();
        self.run("add", account, true, move |b, ns, a, m| {
            b.add_if_absent(&ns, &a, &s, m)
        })
    }

    /// Get-or-create a random key (SQLCipher DB key etc.), returned as standard base64 text
    /// (compatible with the `<prefix>/<account>` shape). A read failure is NEVER treated as absence: only an
    /// authoritative "absent" mints, and creation is add-if-absent so concurrent processes adopt
    /// the winner's key. Returns the key and whether this call created it.
    pub fn get_or_create_key(&self, account: &str, bytes: usize) -> Result<(Secret, bool)> {
        if !(16..=1024).contains(&bytes) {
            return Err(Error::Invalid {
                what: "key length",
                reason: "must be 16..=1024 bytes".into(),
            });
        }
        let _mint = MINT_LOCK
            .lock()
            .map_err(|_| Error::Unavailable("mint lock poisoned".into()))?;
        if let Some(existing) = self.get(account)?.filter(|s| !s.is_empty()) {
            return Ok((existing, false));
        }
        let mut raw = zeroize::Zeroizing::new(vec![0u8; bytes]);
        getrandom::fill(&mut raw).map_err(|_| Error::Unavailable("OS CSPRNG failed".into()))?;
        let fresh = Secret::from_string(base64_standard(&raw));
        if self.add_if_absent(account, &fresh)? {
            return Ok((fresh, true));
        }
        match self.get(account)?.filter(|s| !s.is_empty()) {
            Some(winner) => Ok((winner, false)),
            None => Err(Error::Backend {
                op: "get_or_create_key",
                code: 0,
            }),
        }
    }
}

#[cfg(feature = "settings-probe")]
impl rightkit_settings::SecretProbe for Store {
    fn presence(&self, secret: &rightkit_settings::SecretRef) -> rightkit_settings::Presence {
        use rightkit_settings::Presence;
        let Ok(ns) = Namespace::new(secret.service.clone()) else {
            return Presence::Unknown;
        };
        match self.with_namespace(ns).exists(&secret.account) {
            Ok(true) => Presence::Present,
            Ok(false) => Presence::Absent,
            Err(_) => Presence::Unknown,
        }
    }
}

#[cfg(test)]
mod base64_tests {
    use super::base64_standard;

    #[test]
    fn matches_rfc4648_vectors() {
        for (raw, want) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
            (&[0xfb, 0xff, 0xbf], "+/+/"),
        ] {
            assert_eq!(base64_standard(raw), want);
        }
    }
}
