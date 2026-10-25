//! Native engine composition over the shared lifecycle evaluator.
//! Sources are native adapters, never renderer snapshots. One verifier owns one
//! source for its entire lifetime; every authorization rereads that source.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use crate::{
    evaluate_lifecycle, system_now_unix, AntiRollbackClock, ClockError, ClockState, ClockStore,
    LicenseRequirement, Lifecycle, LifecycleContext, LifecyclePolicy, LifecycleReport,
    ProcessAnchor, RevokedIds, StoredCredential, MAX_TOKEN_BYTES,
};

/// One coherent read of the native record & native-derived device binding.
/// Deliberately has no Debug or serialization implementation.
pub struct NativeSignedCredential {
    pub token: String,
    pub device_binding: String,
    pub clock_initialized: bool,
    /// Opaque native revision used for conditional initialized-marker persistence.
    pub revision: String,
}

pub enum NativeCredential {
    Absent,
    Revoked,
    LegacyUnsigned,
    Unsigned,
    Signed(NativeSignedCredential),
}

/// A transient failure may preserve a previously authenticated final lifetime
/// entitlement. Refused means authoritative refusal & always clears continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSourceError {
    Unavailable,
    Refused,
}

pub trait NativeEntitlementSource: Send + Sync {
    /// Read native storage & derive device binding in this process. Never accept
    /// a caller-supplied device id or paid verdict. Absence/revocation are values.
    fn read(&self) -> Result<NativeCredential, NativeSourceError>;

    /// Mark only this exact native revision initialized, preserving every other
    /// field. A concurrent replacement/deactivation must return Refused, never
    /// overwrite newer storage. Implement with the native writer's shared lock.
    fn mark_clock_initialized(
        &self,
        credential: &NativeSignedCredential,
    ) -> Result<(), NativeSourceError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEntitlementError {
    Source(NativeSourceError),
    Clock(String),
    InvalidInput,
    NotEntitled,
    Poisoned,
}

impl fmt::Display for NativeEntitlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(_) => f.write_str("native entitlement source unavailable or refused"),
            Self::Clock(_) => f.write_str("native entitlement clock refused"),
            Self::InvalidInput => f.write_str("native entitlement input invalid"),
            Self::NotEntitled => f.write_str("native entitlement does not grant access"),
            Self::Poisoned => f.write_str("native entitlement state unavailable"),
        }
    }
}
impl std::error::Error for NativeEntitlementError {}

/// No ordinary verdict cache: authoritative native changes apply on every call.
/// Continuity retains an authenticated report, never an externally supplied bool.
pub struct NativeEngineEntitlements<N, C: ClockStore> {
    source: N,
    clock: AntiRollbackClock<ObservedClockStore<C>>,
    pubkey: [u8; 32],
    app: String,
    policy: LifecyclePolicy,
    revoked: RevokedIds,
    observed_time: Arc<Mutex<ObservedTime>>,
    continuity: Mutex<Option<Continuity>>,
    transient_clock_error: fn(&C::Error) -> bool,
}

struct Continuity {
    report: LifecycleReport,
    // Private native identity, never formatted or serialized.
    identity: (String, String, String),
}

// Retain every time observed by the shared evaluator, including a ledger read
// followed by a failed commit. A storage outage cannot erase observed expiry.
struct ObservedClockStore<C> {
    inner: C,
    observed_time: Arc<Mutex<ObservedTime>>,
}

struct ObservedTime {
    highest: i64,
    server_iat: i64,
}

impl ObservedTime {
    fn observe(&mut self, state: &ClockState, loaded: bool) {
        // Only a persisted newer server anchor supersedes an inflated mark.
        // Uncommitted proposals can increase observations but cannot lower them.
        if loaded && state.last_server_iat > self.server_iat {
            self.highest = state.highest_seen;
            self.server_iat = state.last_server_iat;
        } else if state.last_server_iat >= self.server_iat {
            self.highest = self.highest.max(state.highest_seen);
        }
    }
}

impl<C: ClockStore> ClockStore for ObservedClockStore<C> {
    type Error = C::Error;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        let state = self.inner.load()?;
        if let Some(state) = &state {
            self.observed_time
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .observe(state, true);
        }
        Ok(state)
    }
    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        self.observed_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(state, false);
        self.inner.save(state)
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        self.observed_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(new, false);
        self.inner.compare_and_save(expected, new)
    }
}

impl<N: NativeEntitlementSource, C: ClockStore> NativeEngineEntitlements<N, C>
where
    C::Error: fmt::Display,
{
    pub fn new(
        source: N,
        store: C,
        anchor: ProcessAnchor,
        pubkey: [u8; 32],
        app: impl Into<String>,
        policy: LifecyclePolicy,
    ) -> Self {
        let observed_time = Arc::new(Mutex::new(ObservedTime {
            highest: i64::MIN,
            server_iat: i64::MIN,
        }));
        let clock = AntiRollbackClock::new(
            ObservedClockStore {
                inner: store,
                observed_time: observed_time.clone(),
            },
            anchor,
        );
        Self {
            source,
            clock,
            pubkey,
            app: app.into(),
            policy,
            revoked: RevokedIds::new(),
            observed_time,
            continuity: Mutex::new(None),
            transient_clock_error: |_| false,
        }
    }

    /// An incident denylist: an authenticated entitlement whose id is listed reports
    /// [`Lifecycle::Revoked`] and never grants access, even when its token has not expired. The
    /// list is native configuration, never an input from the window.
    pub fn with_revoked_ids(mut self, revoked: RevokedIds) -> Self {
        self.revoked = revoked;
        self
    }

    /// Opt in only positively transient native storage errors. Refusal is default.
    pub fn with_transient_clock_errors(mut self, classifier: fn(&C::Error) -> bool) -> Self {
        self.transient_clock_error = classifier;
        self
    }

    /// Invalidation only revokes continuity; it cannot grant access.
    pub fn invalidate(&self) {
        if let Ok(mut cached) = self.continuity.lock() {
            *cached = None;
        }
    }

    pub fn current(&self) -> Result<LifecycleReport, NativeEntitlementError> {
        let wall = system_now_unix().map_err(NativeEntitlementError::Clock)?;
        self.current_at(wall)
    }

    /// Native clock injection for deterministic hosts/tests, never IPC input.
    pub fn current_at(&self, wall: i64) -> Result<LifecycleReport, NativeEntitlementError> {
        let mut cached = self
            .continuity
            .lock()
            .map_err(|_| NativeEntitlementError::Poisoned)?;
        let mut identity = None;
        let result = match self.source.read() {
            Ok(native) => {
                if let NativeCredential::Signed(record) = &native {
                    identity = Some((
                        record.revision.clone(),
                        record.token.clone(),
                        record.device_binding.clone(),
                    ));
                }
                if cached
                    .as_ref()
                    .is_some_and(|previous| Some(&previous.identity) != identity.as_ref())
                {
                    *cached = None;
                }
                self.evaluate(&native, wall)
            }
            Err(error) => Err((
                NativeEntitlementError::Source(error),
                error == NativeSourceError::Unavailable,
            )),
        };
        match result {
            Ok(report) => {
                *cached = identity
                    .filter(|_| report.lifecycle == Lifecycle::Lifetime && report.grants_access())
                    .map(|identity| Continuity {
                        report: report.clone(),
                        identity,
                    });
                Ok(report)
            }
            Err((error, transient)) => {
                if transient {
                    if let Some(previous) = cached.as_ref() {
                        let now = wall
                            .max(self.clock.monotonic_now())
                            .max(
                                self.observed_time
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .highest,
                            )
                            .max(previous.report.effective_now.unwrap_or(wall));
                        if let Some(entitlement) = previous.report.entitlement.as_ref() {
                            if entitlement.is_final_lifetime()
                                && entitlement.lifecycle(now, &self.policy) == Lifecycle::Lifetime
                            {
                                let mut report = previous.report.clone();
                                report.effective_now = Some(now);
                                let identity = previous.identity.clone();
                                *cached = Some(Continuity {
                                    report: report.clone(),
                                    identity,
                                });
                                return Ok(report);
                            }
                        }
                    }
                }
                *cached = None;
                Err(error)
            }
        }
    }

    pub fn require_pro(&self) -> Result<LifecycleReport, NativeEntitlementError> {
        let report = self.current()?;
        if report.grants_access() {
            Ok(report)
        } else {
            Err(NativeEntitlementError::NotEntitled)
        }
    }

    fn evaluate(
        &self,
        native: &NativeCredential,
        wall: i64,
    ) -> Result<LifecycleReport, (NativeEntitlementError, bool)> {
        let source_error = |error| {
            (
                NativeEntitlementError::Source(error),
                error == NativeSourceError::Unavailable,
            )
        };
        let device = match native {
            NativeCredential::Signed(record) => {
                if record.token.is_empty()
                    || record.token.len() as u64 > MAX_TOKEN_BYTES
                    || record.device_binding.trim().is_empty()
                {
                    return Err((NativeEntitlementError::InvalidInput, false));
                }
                record.device_binding.as_str()
            }
            _ => "",
        };
        let credential = match native {
            NativeCredential::Absent => StoredCredential::Absent,
            NativeCredential::Revoked => StoredCredential::Revoked,
            NativeCredential::LegacyUnsigned => StoredCredential::LegacyUnsigned,
            NativeCredential::Unsigned => StoredCredential::Unsigned,
            NativeCredential::Signed(record) => StoredCredential::Signed {
                token: &record.token,
                clock_initialized: record.clock_initialized,
            },
        };
        let requirement = LicenseRequirement::pro();
        let context = LifecycleContext::new(&self.pubkey, &self.app, device)
            .with_policy(self.policy)
            .with_requirement(&requirement)
            .with_revoked_ids(&self.revoked);
        let report = evaluate_lifecycle(&context, &self.clock, credential, wall)
            .map_err(|error| {
                let transient = matches!(&error, ClockError::Storage(inner) if (self.transient_clock_error)(inner));
                (NativeEntitlementError::Clock(error.to_string()), transient)
            })?;
        if let NativeCredential::Signed(record) = native {
            if report.clock_initialized && !record.clock_initialized {
                self.source
                    .mark_clock_initialized(record)
                    .map_err(source_error)?;
            }
        }
        Ok(report)
    }
}
