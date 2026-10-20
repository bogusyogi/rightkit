use super::*;
use rightkit_credentials::os_auth::{ScriptedAuthenticator, Verified};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Engine {
    revokes: AtomicUsize,
    resumes: AtomicUsize,
    revoke_fails: AtomicBool,
    resume_fails: AtomicBool,
}

impl EngineLock for Engine {
    fn revoke(&self) -> Result<(), EngineLockError> {
        self.revokes.fetch_add(1, Ordering::SeqCst);
        if self.revoke_fails.load(Ordering::SeqCst) {
            Err(EngineLockError)
        } else {
            Ok(())
        }
    }
    fn resume(&self) -> Result<(), EngineLockError> {
        self.resumes.fetch_add(1, Ordering::SeqCst);
        if self.resume_fails.load(Ordering::SeqCst) {
            Err(EngineLockError)
        } else {
            Ok(())
        }
    }
}

fn owner(service: &AppLockService) -> OwnerSession {
    OwnerSession(Arc::new(Owner {
        instance: Arc::clone(&service.instance),
        label: "main".into(),
        origin: "tauri://localhost".into(),
        active: AtomicBool::new(true),
    }))
}

fn setup(outcomes: Vec<Result<(), OsAuthError>>) -> (AppLockService, Arc<Engine>) {
    let engine = Arc::new(Engine::default());
    let auth = Arc::new(ScriptedAuthenticator::new(
        Availability::Available,
        outcomes,
    ));
    let service = AppLockService::with_auth(
        engine.clone(),
        auth,
        Duration::from_secs(60),
        Duration::from_secs(5),
    )
    .unwrap();
    (service, engine)
}

#[test]
fn unlock_lock_restart_requires_new_os_proof() {
    let (service, engine) = setup(vec![Ok(())]);
    let caller = owner(&service);
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 1);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(service.status().unwrap(), Status::Unlocking);
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Ok(Status::Unlocked)
    );
    assert_eq!(service.lock(), Ok(Status::Locked));
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 2);
    let restarted = AppLockService::with_auth(
        engine.clone(),
        Arc::new(ScriptedAuthenticator::new(Availability::Available, vec![])),
        Duration::from_secs(60),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(restarted.status().unwrap(), Status::Locked);
    assert_eq!(
        restarted.begin_unlock(&caller).err(),
        Some(LockError::Stale)
    );
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 3);
}

#[test]
fn cancellation_failure_timeout_unsupported_never_resume_engine() {
    for error in [
        OsAuthError::Cancelled,
        OsAuthError::SystemCancelled,
        OsAuthError::Failed,
        OsAuthError::Timeout,
        OsAuthError::Unsupported,
        OsAuthError::NotEnrolled,
    ] {
        let (service, engine) = setup(vec![Err(error)]);
        let caller = owner(&service);
        let challenge = service.begin_unlock(&caller).unwrap();
        assert_eq!(
            service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
            Err(LockError::Authentication(error))
        );
        assert_eq!(service.status().unwrap(), Status::Locked);
        assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn owner_binding_expiry_cancellation_and_replay_refuse() {
    let (service, engine) = setup(vec![Ok(())]);
    let caller = owner(&service);
    let other = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(
        service.cancel_unlock(&other, &challenge),
        Err(LockError::Stale)
    );
    assert_eq!(
        service.cancel_unlock(&caller, &challenge),
        Ok(Status::Locked)
    );
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Stale)
    );
    let mut challenge = service.begin_unlock(&caller).unwrap();
    challenge.expires = Instant::now();
    service
        .state
        .lock()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .expires = challenge.expires;
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Expired)
    );
    assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
    let challenge = service.begin_unlock(&caller).unwrap();
    service.close_owner(&caller).unwrap();
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Stale)
    );
}

#[test]
fn document_invalidation_does_not_choose_product_relock_policy() {
    let (service, engine) = setup(vec![Ok(())]);
    let caller = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Ok(Status::Unlocked)
    );
    assert_eq!(service.invalidate_owner(&caller), Ok(Status::Unlocked));
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 1);
    assert_eq!(service.begin_unlock(&caller).err(), Some(LockError::Stale));
    assert_eq!(service.lock(), Ok(Status::Locked));
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 2);
}

#[test]
fn document_invalidation_consumes_challenge_without_engine_resume() {
    let (service, engine) = setup(vec![Ok(())]);
    let caller = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(service.invalidate_owner(&caller), Ok(Status::Locked));
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Stale)
    );
    assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
    assert_eq!(engine.revokes.load(Ordering::SeqCst), 1);
}

/// Hold successful native reply until another thread has invalidated its authority.
struct HeldAuth {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl OsAuthenticator for HeldAuth {
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn authenticate(&self, request: &Request<'_>) -> Result<Verified, OsAuthError> {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        ScriptedAuthenticator::new(Availability::Available, vec![Ok(())]).authenticate(request)
    }
}

#[test]
fn late_os_success_after_lock_cannot_restore_privilege() {
    let engine = Arc::new(Engine::default());
    let (entered_tx, entered) = std::sync::mpsc::sync_channel(1);
    let (release, release_rx) = std::sync::mpsc::channel();
    let service = Arc::new(
        AppLockService::with_auth(
            engine.clone(),
            Arc::new(HeldAuth {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
            Duration::from_secs(60),
            Duration::from_secs(5),
        )
        .unwrap(),
    );
    let caller = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    let worker_service = service.clone();
    let worker = std::thread::spawn(move || {
        worker_service.complete_unlock(&caller, challenge, &Request::new("Unlock test"))
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(service.lock(), Ok(Status::Locked));
    release.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(LockError::Stale));
    assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
}

#[test]
fn engine_failure_cannot_be_announced_as_locked_or_unlocked() {
    let (service, engine) = setup(vec![Ok(())]);
    engine.revoke_fails.store(true, Ordering::SeqCst);
    assert_eq!(service.lock(), Err(LockError::Engine));
    assert_eq!(service.status().unwrap(), Status::Faulted);
    engine.revoke_fails.store(false, Ordering::SeqCst);
    service.lock().unwrap();
    engine.resume_fails.store(true, Ordering::SeqCst);
    let caller = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Engine)
    );
    assert_eq!(service.status().unwrap(), Status::Locked);
}

#[test]
fn proof_freshness_is_checked_at_completion_without_sleep() {
    let start = Instant::now();
    let proof_at = start + Duration::from_secs(1);
    let max_age = Duration::from_secs(5);
    assert!(proof_is_fresh(proof_at, start, proof_at, max_age));
    assert!(!proof_is_fresh(start, proof_at, proof_at, max_age));
    assert!(!proof_is_fresh(
        proof_at,
        start,
        proof_at + max_age,
        max_age
    ));
    assert!(!proof_is_fresh(proof_at, start, start, max_age));
}

#[test]
fn startup_does_not_return_service_until_engine_revoke_succeeds() {
    let engine = Arc::new(Engine::default());
    engine.revoke_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        AppLockService::new(
            engine.clone(),
            Duration::from_secs(60),
            Duration::from_secs(5)
        )
        .err(),
        Some(LockError::Engine)
    );
    assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
}

#[test]
fn foreign_content_caller_cannot_bind_native_owner() {
    let (service, _) = setup(vec![Ok(())]);
    let host = HostServices::builder()
        .service(
            SERVICE,
            crate::host::ServiceTrust::for_labels(["main"]),
            service,
        )
        .build()
        .unwrap();
    let nav = crate::NavPolicy::default();
    let url = tauri::Url::parse("tauri://localhost/").unwrap();
    let content = vec!["main".into()];
    let facts = CallerFacts {
        label: "main",
        is_app_window: true,
        content_labels: &content,
        url: Some(&url),
        nav: &nav,
    };
    assert_eq!(bind_owner(&host, &facts).err(), Some(LockError::Forbidden));
    let foreign = tauri::Url::parse("https://example.invalid/").unwrap();
    let facts = CallerFacts {
        content_labels: &[],
        url: Some(&foreign),
        ..facts
    };
    assert_eq!(bind_owner(&host, &facts).err(), Some(LockError::Forbidden));
}

#[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
#[test]
fn real_unsupported_backend_never_unlocks() {
    let engine = Arc::new(Engine::default());
    let service = AppLockService::new(
        engine.clone(),
        Duration::from_secs(60),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(service.availability(), Availability::Unsupported);
    let caller = owner(&service);
    let challenge = service.begin_unlock(&caller).unwrap();
    assert_eq!(
        service.complete_unlock(&caller, challenge, &Request::new("Unlock test")),
        Err(LockError::Authentication(OsAuthError::Unsupported))
    );
    assert_eq!(engine.resumes.load(Ordering::SeqCst), 0);
}
