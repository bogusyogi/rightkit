mod common;

use common::{pubkey, Claims, APP, DEVICE};
use rightkit_license::{
    ClockState, ClockStore, Lifecycle, LifecyclePolicy, MemoryClockStore, NativeCredential,
    NativeEngineEntitlements, NativeEntitlementSource, NativeSignedCredential, NativeSourceError,
    ProcessAnchor, RevokedIds,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
enum Input {
    Token {
        token: String,
        device: String,
        initialized: bool,
        revision: String,
    },
    Absent,
    Revoked,
    Error(NativeSourceError),
}
#[derive(Clone)]
struct Source(Arc<Mutex<Input>>);
impl Source {
    fn token(claims: Claims) -> Self {
        Self(Arc::new(Mutex::new(Input::Token {
            token: claims.token(),
            device: DEVICE.into(),
            initialized: false,
            revision: "1".into(),
        })))
    }
    fn replace(&self, input: Input) {
        *self.0.lock().unwrap() = input;
    }
}
impl NativeEntitlementSource for Source {
    fn read(&self) -> Result<NativeCredential, NativeSourceError> {
        Ok(match &*self.0.lock().unwrap() {
            Input::Token {
                token,
                device,
                initialized,
                revision,
            } => NativeCredential::Signed(NativeSignedCredential {
                token: token.clone(),
                device_binding: device.clone(),
                clock_initialized: *initialized,
                revision: revision.clone(),
            }),
            Input::Absent => NativeCredential::Absent,
            Input::Revoked => NativeCredential::Revoked,
            Input::Error(error) => return Err(*error),
        })
    }
    fn mark_clock_initialized(
        &self,
        credential: &NativeSignedCredential,
    ) -> Result<(), NativeSourceError> {
        let mut input = self.0.lock().unwrap();
        match &mut *input {
            Input::Token {
                revision,
                initialized,
                ..
            } if revision == &credential.revision => {
                *initialized = true;
                Ok(())
            }
            _ => Err(NativeSourceError::Refused),
        }
    }
}
fn verifier(source: Source) -> NativeEngineEntitlements<Source, MemoryClockStore> {
    NativeEngineEntitlements::new(
        source,
        MemoryClockStore::new(),
        ProcessAnchor::with_id("native-test", 1000),
        pubkey(),
        APP,
        LifecyclePolicy {
            grace_secs: 0,
            lifetime_survives_rollback: true,
        },
    )
}

#[test]
fn signed_native_binding_expiry_and_missing_initialized_ledger() {
    let source = Source::token(Claims::legacy(900, 1100));
    let gate = verifier(source.clone());
    assert!(gate.current_at(1000).unwrap().grants_access());
    assert!(!gate.current_at(1100).unwrap().grants_access());

    source.replace(Input::Token {
        token: Claims::legacy(900, 2000).token(),
        device: "wrong-device".into(),
        initialized: false,
        revision: "2".into(),
    });
    assert_eq!(gate.current_at(1101).unwrap().lifecycle, Lifecycle::Invalid);
    source.replace(Input::Token {
        token: "forged.signature".into(),
        device: DEVICE.into(),
        initialized: false,
        revision: "3".into(),
    });
    assert_eq!(gate.current_at(1102).unwrap().lifecycle, Lifecycle::Invalid);

    let source = Source::token(Claims::legacy(900, 2000));
    if let Input::Token { initialized, .. } = &mut *source.0.lock().unwrap() {
        *initialized = true;
    }
    assert!(verifier(source).current_at(1000).is_err());
}

#[test]
fn lifetime_continuity_expires_and_authoritative_denials_clear_it() {
    for denial in [
        Input::Absent,
        Input::Revoked,
        Input::Error(NativeSourceError::Refused),
    ] {
        let source = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
        let gate = verifier(source.clone());
        assert!(gate.current_at(1000).unwrap().grants_access());
        source.replace(Input::Error(NativeSourceError::Unavailable));
        assert!(gate.current_at(1100).unwrap().grants_access());
        source.replace(denial);
        let result = gate.current_at(1200);
        assert!(result.is_err() || !result.unwrap().grants_access());
        source.replace(Input::Error(NativeSourceError::Unavailable));
        assert!(gate.current_at(1300).is_err());
    }
    let source = Source::token(Claims::final_lifetime(100, 500, 900, 1200));
    let gate = verifier(source.clone());
    assert!(gate.current_at(1000).unwrap().grants_access());
    source.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1200).is_err());
}

#[test]
fn transient_failure_never_promotes_term_or_forged_replacement() {
    let source = Source::token(Claims::legacy(900, 2000));
    let gate = verifier(source.clone());
    assert!(gate.current_at(1000).unwrap().grants_access());
    source.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1100).is_err());

    let source = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
    let gate = verifier(source.clone());
    assert!(gate.current_at(1000).unwrap().grants_access());
    source.replace(Input::Token {
        token: "forged.signature".into(),
        device: DEVICE.into(),
        initialized: false,
        revision: "2".into(),
    });
    assert_eq!(gate.current_at(1100).unwrap().lifecycle, Lifecycle::Invalid);
    source.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1200).is_err());
}

#[test]
fn explicit_native_hashed_binding_and_final_lifetime_rollback() {
    // Binding mode is selected by native record policy, never a renderer field.
    let mut claims = Claims::legacy(900, 2000);
    claims.device = "native-device-hash";
    let source = Source::token(claims);
    if let Input::Token { device, .. } = &mut *source.0.lock().unwrap() {
        *device = "native-device-hash".into();
    }
    assert!(verifier(source).current_at(1000).unwrap().grants_access());

    let source = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
    let gate = verifier(source);
    assert!(gate.current_at(1000).unwrap().grants_access());
    assert_eq!(gate.current_at(500).unwrap().lifecycle, Lifecycle::Lifetime);

    let source = Source::token(Claims::trial(900, 2000));
    let gate = verifier(source);
    assert!(gate.current_at(1000).unwrap().grants_access());
    assert_eq!(
        gate.current_at(500).unwrap().lifecycle,
        Lifecycle::ClockRepairRequired
    );
}

#[derive(Clone)]
struct FallibleClock(Arc<Mutex<Result<Option<ClockState>, &'static str>>>);
impl ClockStore for FallibleClock {
    type Error = &'static str;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        *self.0.lock().unwrap() = Ok(Some(state.clone()));
        Ok(())
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        let mut slot = self.0.lock().unwrap();
        match &*slot {
            Err(error) => Err(*error),
            Ok(current) if current.as_ref() == expected => {
                *slot = Ok(Some(new.clone()));
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[test]
fn clock_storage_failure_continues_only_same_authenticated_record_and_missing_ledger_denies() {
    let source = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
    let store = FallibleClock(Arc::new(Mutex::new(Ok(None))));
    let gate = NativeEngineEntitlements::new(
        source.clone(),
        store.clone(),
        ProcessAnchor::with_id("native-test", 1000),
        pubkey(),
        APP,
        LifecyclePolicy {
            grace_secs: 0,
            lifetime_survives_rollback: true,
        },
    )
    .with_transient_clock_errors(|_| true);
    assert!(gate.current_at(1000).unwrap().grants_access());
    *store.0.lock().unwrap() = Err("credential store unavailable");
    assert!(gate.current_at(1100).unwrap().grants_access());
    // Even another valid final lifetime record cannot inherit continuity before
    // its own lifecycle/ledger transaction succeeds.
    if let Input::Token { revision, .. } = &mut *source.0.lock().unwrap() {
        *revision = "replacement".into();
    }
    assert!(gate.current_at(1200).is_err());
    *store.0.lock().unwrap() = Ok(None);
    assert!(gate.current_at(1300).is_err());
}

struct FailCommitClock {
    inner: FallibleClock,
    fail: Arc<std::sync::atomic::AtomicBool>,
}
impl ClockStore for FailCommitClock {
    type Error = &'static str;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        let observed = self.inner.load();
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            *self.inner.0.lock().unwrap() = Err("store unavailable after read");
        }
        observed
    }
    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        self.inner.save(state)
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            // Once the commit fails, even another read cannot recover observed time.
            *self.inner.0.lock().unwrap() = Err("store unavailable");
            return Err("commit failed");
        }
        self.inner.compare_and_save(expected, new)
    }
}

#[test]
fn failed_commit_cannot_forget_expiry_observed_from_newer_ledger() {
    let source = Source::token(Claims::final_lifetime(100, 500, 900, 1200));
    let store = FallibleClock(Arc::new(Mutex::new(Ok(None))));
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = NativeEngineEntitlements::new(
        source.clone(),
        FailCommitClock {
            inner: store.clone(),
            fail: fail.clone(),
        },
        ProcessAnchor::with_id("native-test", 1000),
        pubkey(),
        APP,
        LifecyclePolicy {
            grace_secs: 0,
            lifetime_survives_rollback: true,
        },
    )
    .with_transient_clock_errors(|_| true);
    assert!(gate.current_at(1000).unwrap().grants_access());
    *store.0.lock().unwrap() = Ok(Some(ClockState {
        highest_seen: 1250,
        trusted_process: "native-test".into(),
        last_server_iat: 0,
    }));
    // Reconcile observes 1250; commit's subsequent load then fails.
    fail.store(true, std::sync::atomic::Ordering::Relaxed);
    let result = gate.current_at(1100);
    assert!(result.is_err() || !result.unwrap().grants_access());
    source.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1100).is_err());
}

#[test]
fn successful_newer_server_anchor_supersedes_inflated_observation() {
    let source = Source::token(Claims::final_lifetime(100, 500, 900, 3000));
    let store = FallibleClock(Arc::new(Mutex::new(Ok(None))));
    let gate = NativeEngineEntitlements::new(
        source.clone(),
        store.clone(),
        ProcessAnchor::with_id("native-test", 1000),
        pubkey(),
        APP,
        LifecyclePolicy {
            grace_secs: 0,
            lifetime_survives_rollback: true,
        },
    )
    .with_transient_clock_errors(|_| true);
    assert!(gate.current_at(2500).unwrap().grants_access());
    *store.0.lock().unwrap() = Ok(Some(ClockState {
        highest_seen: 1500,
        trusted_process: "native-test".into(),
        last_server_iat: 1500,
    }));
    source.replace(Input::Token {
        token: Claims::final_lifetime(100, 500, 1500, 2000).token(),
        device: DEVICE.into(),
        initialized: true,
        revision: "server-repair".into(),
    });
    assert!(gate.current_at(1600).unwrap().grants_access());
    source.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1700).unwrap().grants_access());
}

#[test]
fn unclassified_clock_storage_failure_cannot_continue_authenticated_lifetime() {
    let source = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
    let store = FallibleClock(Arc::new(Mutex::new(Ok(None))));
    let gate = NativeEngineEntitlements::new(
        source,
        store.clone(),
        ProcessAnchor::with_id("native-test", 1000),
        pubkey(),
        APP,
        LifecyclePolicy {
            grace_secs: 0,
            lifetime_survives_rollback: true,
        },
    );
    assert!(gate.current_at(1000).unwrap().grants_access());
    *store.0.lock().unwrap() = Err("denied or unknown storage failure");
    assert!(gate.current_at(1100).is_err());
}

#[test]
fn an_incident_denylist_revokes_an_authentic_token_and_clears_lifetime_continuity() {
    let source = Source::token(Claims::legacy(900, 2000));
    let open = verifier(source.clone());
    assert!(open.current_at(1000).unwrap().grants_access());
    assert_eq!(open.current_at(2100).unwrap().lifecycle, Lifecycle::Expired);

    let listed = verifier(source.clone()).with_revoked_ids(RevokedIds::new().with("lic_1"));
    let report = listed.current_at(1000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Revoked);
    assert!(!report.grants_access());
    assert_eq!(
        listed.current_at(2100).unwrap().lifecycle,
        Lifecycle::Revoked
    );
    // An id that is not this licence changes nothing (a source of its own: the first verifier marked
    // this record's ledger as initialised, which a fresh in-memory ledger rightly refuses).
    let elsewhere = verifier(Source::token(Claims::legacy(900, 2000)))
        .with_revoked_ids(RevokedIds::new().with("lic_other"));
    assert!(elsewhere.current_at(1000).unwrap().grants_access());

    // A revoked final lifetime licence is never carried over a transient storage failure.
    let lifetime = Source::token(Claims::final_lifetime(100, 500, 900, 2000));
    let gate = verifier(lifetime.clone()).with_revoked_ids(RevokedIds::new().with("lic_1"));
    assert_eq!(gate.current_at(1000).unwrap().lifecycle, Lifecycle::Revoked);
    lifetime.replace(Input::Error(NativeSourceError::Unavailable));
    assert!(gate.current_at(1100).is_err());
}
