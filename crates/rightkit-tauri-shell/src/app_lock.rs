//! PK08-7 native app-lock coordination. Host commands must call `service_for` first.
//! Owner sessions & challenges are native capabilities: neither is serializable or Clone.
//! OS verification is performed here; renderer-supplied booleans/proofs cannot unlock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rightkit_credentials::os_auth::{
    Availability, OsAuthError, OsAuthenticator, Request, SystemAuthenticator,
};
use serde::Serialize;

use crate::caller_gate::{origin_of, CallerFacts};
use crate::host::{HostServices, ServiceId};

pub const SERVICE: ServiceId = ServiceId::new("app-lock");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineLockError;

/// Product adapter over existing daemon/engine privilege admission. Calls must be bounded,
/// non-reentrant, & serialized with every privileged admission in that engine.
pub trait EngineLock: Send + Sync + 'static {
    /// Return success only after privileged access, including cached/session authority, is
    /// invalidated. On failure native engine must retain its own deny fence.
    fn revoke(&self) -> Result<(), EngineLockError>;
    /// Re-enable local named-action admission after coordinator-owned authentication. This must
    /// not mint remote approval/PIN/signature authority. Failure must deny admission.
    fn resume(&self) -> Result<(), EngineLockError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Locked,
    Unlocking,
    Unlocked,
    /// Revoke could not prove engine exclusion. Never announce this as `Locked` or `Unlocked`.
    Faulted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    Forbidden,
    Busy,
    Invalid,
    Stale,
    Expired,
    AlreadyUnlocked,
    Engine,
    Poisoned,
    Authentication(OsAuthError),
}

impl LockError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Forbidden => "permission_denied",
            Self::Busy => "busy",
            Self::Invalid => "invalid",
            Self::Stale => "stale_unlock",
            Self::Expired => "expired_unlock",
            Self::AlreadyUnlocked => "already_unlocked",
            Self::Engine => "engine_lock_failed",
            Self::Poisoned => "lock_state_unavailable",
            Self::Authentication(error) => error.code(),
        }
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LockError {}

struct Owner {
    instance: Arc<()>,
    label: String,
    origin: String,
    active: AtomicBool,
}

/// Minted only after HostServices caller authorization. Keep one per native webview document
/// lifetime; revoke on navigation/destruction. Never reconstruct from renderer labels or IDs.
pub struct OwnerSession(Arc<Owner>);

/// Single-use native challenge bound to coordinator instance, document session, generation &
/// monotonic expiry. Keep in native task state; no IPC DTO can stand in for it.
pub struct UnlockChallenge {
    owner: Arc<Owner>,
    nonce: Arc<()>,
    expires: Instant,
}

struct Pending {
    owner: Arc<Owner>,
    nonce: Arc<()>,
    expires: Instant,
    running: bool,
}

struct State {
    status: Status,
    pending: Option<Pending>,
}

pub struct AppLockService {
    instance: Arc<()>,
    state: Mutex<State>,
    engine: Arc<dyn EngineLock>,
    auth: Arc<dyn OsAuthenticator>,
    challenge_ttl: Duration,
    proof_max_age: Duration,
}

impl AppLockService {
    /// Every construction/restart revokes engine authority before returning a locked service.
    /// No persisted unlocked state is accepted. Application supplies timeout/freshness policy.
    pub fn new(
        engine: Arc<dyn EngineLock>,
        challenge_ttl: Duration,
        proof_max_age: Duration,
    ) -> Result<Self, LockError> {
        Self::with_auth(
            engine,
            Arc::new(SystemAuthenticator),
            challenge_ttl,
            proof_max_age,
        )
    }

    // Private: no release constructor injects a success substitute for native authentication.
    fn with_auth(
        engine: Arc<dyn EngineLock>,
        auth: Arc<dyn OsAuthenticator>,
        challenge_ttl: Duration,
        proof_max_age: Duration,
    ) -> Result<Self, LockError> {
        if challenge_ttl.is_zero()
            || proof_max_age.is_zero()
            || challenge_ttl > Request::MAX_TIMEOUT
            || proof_max_age > Request::MAX_TIMEOUT
        {
            return Err(LockError::Invalid);
        }
        engine.revoke().map_err(|_| LockError::Engine)?;
        Ok(Self {
            instance: Arc::new(()),
            state: Mutex::new(State {
                status: Status::Locked,
                pending: None,
            }),
            engine,
            auth,
            challenge_ttl,
            proof_max_age,
        })
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, LockError> {
        // Never recover an uncertain authorization state into unlocked success.
        self.state.lock().map_err(|_| LockError::Poisoned)
    }

    fn check_owner(&self, owner: &OwnerSession) -> Result<(), LockError> {
        if !Arc::ptr_eq(&self.instance, &owner.0.instance)
            || !owner.0.active.load(Ordering::Acquire)
        {
            return Err(LockError::Stale);
        }
        Ok(())
    }

    pub fn availability(&self) -> Availability {
        self.auth.availability()
    }

    pub fn status(&self) -> Result<Status, LockError> {
        let mut state = self.state()?;
        if state
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.expires)
        {
            state.pending = None;
            state.status = Status::Locked;
        }
        Ok(state.status)
    }

    /// Native lifecycle lock (also used on startup, close, background & product policy events).
    /// Invalidates pending challenges before engine revocation; success is announced afterward.
    pub fn lock(&self) -> Result<Status, LockError> {
        let mut state = self.state()?;
        state.pending = None;
        state.status = Status::Faulted;
        self.engine.revoke().map_err(|_| LockError::Engine)?;
        state.status = Status::Locked;
        Ok(state.status)
    }

    /// Document close/navigation invalidates this owner, even if native prompt is in flight.
    pub fn close_owner(&self, owner: &OwnerSession) -> Result<Status, LockError> {
        let mut state = self.state()?;
        self.check_owner(owner)?;
        owner.0.active.store(false, Ordering::Release);
        state.pending = None;
        state.status = Status::Faulted;
        self.engine.revoke().map_err(|_| LockError::Engine)?;
        state.status = Status::Locked;
        Ok(state.status)
    }

    /// Invalidate document capability & its pending challenge without choosing product lock
    /// policy. An already-unlocked engine remains unchanged; caller may explicitly call lock.
    pub fn invalidate_owner(&self, owner: &OwnerSession) -> Result<Status, LockError> {
        let mut state = self.state()?;
        self.check_owner(owner)?;
        owner.0.active.store(false, Ordering::Release);
        if state
            .pending
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(&p.owner, &owner.0))
        {
            state.pending = None;
            if state.status == Status::Unlocking {
                state.status = Status::Locked;
            }
        }
        Ok(state.status)
    }

    pub fn begin_unlock(&self, owner: &OwnerSession) -> Result<UnlockChallenge, LockError> {
        let mut state = self.state()?;
        self.check_owner(owner)?;
        if state.status == Status::Faulted {
            return Err(LockError::Engine);
        }
        if state.status == Status::Unlocked {
            return Err(LockError::AlreadyUnlocked);
        }
        let now = Instant::now();
        if state.pending.as_ref().is_some_and(|p| now < p.expires) {
            return Err(LockError::Busy);
        }
        let expires = now
            .checked_add(self.challenge_ttl)
            .ok_or(LockError::Invalid)?;
        let nonce = Arc::new(());
        state.pending = Some(Pending {
            owner: Arc::clone(&owner.0),
            nonce: Arc::clone(&nonce),
            expires,
            running: false,
        });
        state.status = Status::Unlocking;
        Ok(UnlockChallenge {
            owner: Arc::clone(&owner.0),
            nonce,
            expires,
        })
    }

    fn matches(pending: &Pending, owner: &OwnerSession, challenge: &UnlockChallenge) -> bool {
        Arc::ptr_eq(&pending.owner, &owner.0)
            && Arc::ptr_eq(&challenge.owner, &owner.0)
            && Arc::ptr_eq(&pending.nonce, &challenge.nonce)
            && pending.expires == challenge.expires
    }

    /// Cancellation invalidates authority immediately. An already-running OS prompt retains
    /// credentials' native prompt lease until it settles; its late result cannot unlock.
    pub fn cancel_unlock(
        &self,
        owner: &OwnerSession,
        challenge: &UnlockChallenge,
    ) -> Result<Status, LockError> {
        let mut state = self.state()?;
        self.check_owner(owner)?;
        if !state
            .pending
            .as_ref()
            .is_some_and(|p| Self::matches(p, owner, challenge))
        {
            return Err(LockError::Stale);
        }
        state.pending = None;
        state.status = Status::Locked;
        Ok(state.status)
    }

    /// Run off UI thread. Request reason, timeout & HWND come from native product configuration /
    /// window context, never IPC arguments. No externally supplied Verified value is accepted.
    pub fn complete_unlock(
        &self,
        owner: &OwnerSession,
        challenge: UnlockChallenge,
        request: &Request<'_>,
    ) -> Result<Status, LockError> {
        {
            let mut state = self.state()?;
            self.check_owner(owner)?;
            let pending = state.pending.as_mut().ok_or(LockError::Stale)?;
            if !Self::matches(pending, owner, &challenge) {
                return Err(LockError::Stale);
            }
            if Instant::now() >= pending.expires {
                state.pending = None;
                state.status = Status::Locked;
                return Err(LockError::Expired);
            }
            if pending.running {
                return Err(LockError::Busy);
            }
            pending.running = true;
        }
        // Release coordinator mutex: lock/navigation/cancel must invalidate a running prompt.
        let started = Instant::now();
        let proof = self.auth.authenticate(request);
        let mut state = self.state()?;
        self.check_owner(owner)?;
        if !state
            .pending
            .as_ref()
            .is_some_and(|p| Self::matches(p, owner, &challenge))
        {
            return Err(LockError::Stale);
        }
        state.pending = None; // consume before every success/failure path
        state.status = Status::Locked;
        if Instant::now() >= challenge.expires {
            return Err(LockError::Expired);
        }
        let proof = proof.map_err(LockError::Authentication)?;
        if !proof_is_fresh(
            proof.verified_at(),
            started,
            Instant::now(),
            self.proof_max_age,
        ) {
            return Err(LockError::Expired);
        }
        // Serialize native engine grant with lock/revoke; status follows settled grant only.
        if self.engine.resume().is_err() {
            state.status = Status::Faulted;
            if self.engine.revoke().is_ok() {
                state.status = Status::Locked;
            }
            return Err(LockError::Engine);
        }
        state.status = Status::Unlocked;
        Ok(state.status)
    }
}

fn proof_is_fresh(at: Instant, started: Instant, now: Instant, max_age: Duration) -> bool {
    at >= started && now >= at && now.duration_since(at) < max_age
}

/// CallerGate + per-service policy admission. Run on EVERY command, including status & lock.
/// Identity comes from caller_gate::gather(Webview), never renderer-owned label/origin fields.
pub fn service_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
    owner: Option<&OwnerSession>,
) -> Result<Arc<AppLockService>, LockError> {
    let service = host
        .service_for::<AppLockService>(SERVICE, action, facts)
        .map_err(|_| LockError::Forbidden)?;
    if let Some(owner) = owner {
        service.check_owner(owner)?;
        if owner.0.label != facts.label
            || Some(owner.0.origin.clone()) != facts.url.and_then(origin_of)
        {
            return Err(LockError::Forbidden);
        }
    }
    Ok(service)
}

/// Bind one authorized native document lifetime. Keep returned owner in native document state,
/// not command arguments. New webview/reload/navigation must get a new OwnerSession.
pub fn bind_owner(
    host: &HostServices,
    facts: &CallerFacts<'_>,
) -> Result<(Arc<AppLockService>, OwnerSession), LockError> {
    let service = service_for(host, "begin_unlock", facts, None)?;
    let origin = facts.url.and_then(origin_of).ok_or(LockError::Forbidden)?;
    let owner = OwnerSession(Arc::new(Owner {
        instance: Arc::clone(&service.instance),
        label: facts.label.into(),
        origin,
        active: AtomicBool::new(true),
    }));
    Ok((service, owner))
}

#[cfg(all(test, debug_assertions))]
mod tests;
