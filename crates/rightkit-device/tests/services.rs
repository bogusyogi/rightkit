use rightkit_device::{
    device_id_hash, guard_paid_command, AuthorizationDenial, AuthorizationError,
    AuthorizationService, DeviceService, DiagnosticHost, DiagnosticService, HostOperation,
    LicenseRecord, LicenseRequirement, LogHost, LogService, MutationHook, PaidCommandError,
    RevokedIds, SecureService, SecureStore, StorageOperation,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier, Mutex,
};

const PUBKEY: [u8; 32] = [
    108, 142, 220, 5, 154, 87, 124, 180, 244, 31, 25, 204, 0, 155, 84, 165, 16, 227, 134, 87, 61,
    66, 199, 238, 168, 219, 128, 145, 39, 235, 80, 211,
];
const TOKEN: &str = "eyJ2IjoyLCJpZCI6ImxpY18xIiwiYXBwIjoiaGVhcmRyaWdodCIsInRpZXIiOiJwcm8iLCJmZWF0dXJlcyI6WyJ3YWtlX3dvcmQiLCJ2b2ljZV9jb21tYW5kcyJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MTAwMCwiZXhwIjo5OTk5OTk5OTk5fQ.JNmWEwG16VehpwJz9SooQIia0k1Ht0c150jca86h7sAnFj9tKuPO8yZhfyR48vx3Yd_03RQyYYtbKm08TCgsBg";
const EXPIRED_PUBKEY: [u8; 32] = [
    142, 24, 236, 181, 218, 14, 202, 189, 202, 2, 108, 170, 217, 217, 172, 191, 22, 225, 191, 4,
    143, 122, 95, 151, 60, 37, 3, 150, 181, 215, 204, 164,
];
const EXPIRED_TOKEN: &str = "eyJ2IjoyLCJpZCI6ImV4cGlyZWQiLCJhcHAiOiJoZWFyZHJpZ2h0IiwidGllciI6InBybyIsImZlYXR1cmVzIjpbIndha2Vfd29yZCJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MCwiZXhwIjoxfQ.sdzQvYk5XwdqanGMs10neqaoMY5YwyIY57S4PlcObJgx7xDAFN057YjlLfXGwB90EkEdPIjBzdTdyK5LK28ECw";

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredLicense(Option<String>);

impl LicenseRecord for StoredLicense {
    fn signed_token(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

struct MemoryStoreState<T> {
    value: Mutex<Option<T>>,
    fail_get: bool,
    fail_set: bool,
    fail_delete: bool,
    writes: AtomicUsize,
}

#[derive(Clone)]
struct MemoryStore<T> {
    state: Arc<MemoryStoreState<T>>,
}

impl<T> Default for MemoryStore<T> {
    fn default() -> Self {
        Self {
            state: Arc::new(MemoryStoreState {
                value: Mutex::new(None),
                fail_get: false,
                fail_set: false,
                fail_delete: false,
                writes: AtomicUsize::new(0),
            }),
        }
    }
}

impl<T> MemoryStore<T> {
    fn with(value: T) -> Self {
        Self {
            state: Arc::new(MemoryStoreState {
                value: Mutex::new(Some(value)),
                fail_get: false,
                fail_set: false,
                fail_delete: false,
                writes: AtomicUsize::new(0),
            }),
        }
    }

    fn failing(operation: StorageOperation) -> Self {
        Self {
            state: Arc::new(MemoryStoreState {
                value: Mutex::new(None),
                fail_get: operation == StorageOperation::Get,
                fail_set: operation == StorageOperation::Set,
                fail_delete: operation == StorageOperation::Delete,
                writes: AtomicUsize::new(0),
            }),
        }
    }

    fn writes(&self) -> usize {
        self.state.writes.load(Ordering::SeqCst)
    }
}

impl<T: Clone> SecureStore<T> for MemoryStore<T> {
    fn get(&self) -> Result<Option<T>, String> {
        if self.state.fail_get {
            return Err("get failed".into());
        }
        Ok(self.state.value.lock().unwrap().clone())
    }

    fn set(&self, value: &T) -> Result<(), String> {
        if self.state.fail_set {
            return Err("set failed".into());
        }
        *self.state.value.lock().unwrap() = Some(value.clone());
        self.state.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        if self.state.fail_delete {
            return Err("delete failed".into());
        }
        *self.state.value.lock().unwrap() = None;
        Ok(())
    }

    fn set_if_absent(&self, value: &T) -> Result<bool, String> {
        if self.state.fail_set {
            return Err("set failed".into());
        }
        // Check and store under the one lock every writer of this store takes.
        let mut slot = self.state.value.lock().unwrap();
        if slot.is_some() {
            return Ok(false);
        }
        *slot = Some(value.clone());
        self.state.writes.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

#[derive(Default)]
struct RecordingHook(Mutex<Vec<&'static str>>);

impl<T> MutationHook<T> for RecordingHook {
    fn after_set(&self, _value: &T) {
        self.0.lock().unwrap().push("set");
    }

    fn after_delete(&self) {
        self.0.lock().unwrap().push("delete");
    }
}

#[test]
fn mutation_hook_is_infallible_and_only_runs_after_commit() {
    let failed_store = MemoryStore::<String>::failing(StorageOperation::Set);
    let hook = RecordingHook::default();
    let service = SecureService::new(&failed_store, &hook);
    let error = service.set(&"license-1".into()).unwrap_err();
    assert_eq!(error.operation, StorageOperation::Set);
    assert!(hook.0.lock().unwrap().is_empty());

    let store = MemoryStore::default();
    let service = SecureService::new(&store, &hook);
    service.set(&"license-1".to_string()).unwrap();
    service.delete().unwrap();
    assert_eq!(&*hook.0.lock().unwrap(), &["set", "delete"]);
}

#[test]
fn secure_service_preserves_typed_storage_operation_failures() {
    let hook = RecordingHook::default();

    let get_store = MemoryStore::<String>::failing(StorageOperation::Get);
    assert_eq!(
        SecureService::new(&get_store, &hook)
            .get()
            .unwrap_err()
            .operation,
        StorageOperation::Get
    );

    let set_store = MemoryStore::<String>::failing(StorageOperation::Set);
    assert_eq!(
        SecureService::new(&set_store, &hook)
            .set(&"license-1".into())
            .unwrap_err()
            .operation,
        StorageOperation::Set
    );

    let delete_store = MemoryStore::<String>::failing(StorageOperation::Delete);
    assert_eq!(
        SecureService::new(&delete_store, &hook)
            .delete()
            .unwrap_err()
            .operation,
        StorageOperation::Delete
    );
}

#[test]
fn device_get_or_create_is_serialized_and_owns_uuid_v4_generation() {
    let store = MemoryStore::<String>::default();
    let service = Arc::new(DeviceService::new(store.clone()));
    let barrier = Arc::new(Barrier::new(9));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            service.get_or_create().unwrap()
        }));
    }
    barrier.wait();
    let values: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(values.iter().all(|value| value == &values[0]));
    let generated = &values[0];
    assert_eq!(generated.len(), 36);
    assert_eq!(generated.as_bytes()[14], b'4');
    assert!(matches!(
        generated.as_bytes()[19],
        b'8' | b'9' | b'a' | b'b'
    ));
    assert!(generated
        .chars()
        .enumerate()
        .all(
            |(index, character)| matches!(index, 8 | 13 | 18 | 23) && character == '-'
                || !matches!(index, 8 | 13 | 18 | 23) && character.is_ascii_hexdigit()
        ));
    assert_eq!(service.get().unwrap().as_deref(), Some(values[0].as_str()));
    assert_eq!(store.writes(), 1);
}

#[test]
fn authorization_preserves_app_verification_denial() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));

    let wrong_app = AuthorizationService::new(&licenses, &devices, &PUBKEY, "viewright")
        .authorize(LicenseRequirement::pro())
        .unwrap_err();
    assert!(matches!(
        wrong_app,
        AuthorizationError::Denied(AuthorizationDenial::InvalidToken(
            rightkit_license::VerifyError::AppMismatch { .. }
        ))
    ));
}

#[test]
fn authorization_uses_native_device_store_not_license_record() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("different-device".into()));
    let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");

    assert!(matches!(
        authorization.authorize(LicenseRequirement::pro()),
        Err(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(_)
        ))
    ));
}

#[test]
fn authorization_supports_pro_and_named_feature_requirements() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");

    assert!(authorization.authorize(LicenseRequirement::pro()).is_ok());
    assert!(authorization
        .authorize(LicenseRequirement::feature("wake_word"))
        .is_ok());
    assert_eq!(
        authorization
            .authorize(LicenseRequirement::feature("cloud_sync"))
            .unwrap_err(),
        AuthorizationError::Denied(AuthorizationDenial::RequirementNotMet(
            LicenseRequirement::feature("cloud_sync")
        ))
    );
}

#[test]
fn public_paid_guard_rejects_expired_token_without_caller_supplied_time() {
    let licenses = MemoryStore::with(StoredLicense(Some(EXPIRED_TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization =
        AuthorizationService::new(&licenses, &devices, &EXPIRED_PUBKEY, "heardright");
    let ran = Mutex::new(false);

    let result: Result<(), PaidCommandError<()>> =
        guard_paid_command(&authorization, LicenseRequirement::pro(), |_| {
            *ran.lock().unwrap() = true;
            Ok(())
        });

    assert!(matches!(
        result,
        Err(PaidCommandError::Authorization(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(rightkit_license::VerifyError::Expired { .. })
        )))
    ));
    assert!(!*ran.lock().unwrap());
}

#[test]
fn paid_command_guard_never_runs_command_when_authorization_is_denied() {
    let licenses = MemoryStore::with(StoredLicense(None));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");
    let ran = Mutex::new(false);

    let result: Result<(), PaidCommandError<&str>> =
        guard_paid_command(&authorization, LicenseRequirement::pro(), |_| {
            *ran.lock().unwrap() = true;
            Err("command failed")
        });
    assert!(matches!(result, Err(PaidCommandError::Authorization(_))));
    assert!(!*ran.lock().unwrap());
}

#[derive(Default)]
struct TestLogs(Mutex<Vec<String>>);

impl LogHost<String, Vec<String>> for TestLogs {
    fn write(&self, entry: &String) -> Result<(), String> {
        self.0.lock().unwrap().push(entry.clone());
        Ok(())
    }
    fn collect(&self) -> Result<Vec<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn clear(&self) -> Result<(), String> {
        self.0.lock().unwrap().clear();
        Ok(())
    }
}

#[derive(Default)]
struct TestDiagnostics(Mutex<bool>);

impl DiagnosticHost<String> for TestDiagnostics {
    fn collect(&self) -> Result<String, String> {
        Ok("bundle.zip".into())
    }
    fn clear(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = true;
        Ok(())
    }
}

#[test]
fn typed_log_and_diagnostic_services_bridge_host_operations() {
    let logs = TestLogs::default();
    let log_service = LogService::new(&logs);
    log_service.write(&"started".into()).unwrap();
    assert_eq!(log_service.collect().unwrap(), vec!["started"]);
    log_service.clear().unwrap();
    assert!(log_service.collect().unwrap().is_empty());

    let diagnostics = TestDiagnostics::default();
    let diagnostic_service = DiagnosticService::new(&diagnostics);
    assert_eq!(diagnostic_service.collect().unwrap(), "bundle.zip");
    diagnostic_service.clear().unwrap();
    assert!(*diagnostics.0.lock().unwrap());

    let failing = FailingLogs;
    assert_eq!(
        LogService::new(&failing).collect().unwrap_err().operation,
        HostOperation::CollectLogs
    );
}

struct FailingLogs;
impl LogHost<String, Vec<String>> for FailingLogs {
    fn write(&self, _entry: &String) -> Result<(), String> {
        Err("failed".into())
    }
    fn collect(&self) -> Result<Vec<String>, String> {
        Err("failed".into())
    }
    fn clear(&self) -> Result<(), String> {
        Err("failed".into())
    }
}

fn pro_authorization<'a>(
    licenses: &'a MemoryStore<StoredLicense>,
    devices: &'a DeviceService<MemoryStore<String>>,
) -> AuthorizationService<'a, StoredLicense, MemoryStore<StoredLicense>, MemoryStore<String>> {
    AuthorizationService::new(licenses, devices, &PUBKEY, "heardright")
}

#[test]
fn injected_trusted_clock_decides_authorization() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = pro_authorization(&licenses, &devices);

    assert!(authorization
        .authorize_with_clock(LicenseRequirement::pro(), || Ok(2_000))
        .is_ok());
    // A trusted clock past exp denies even though the wall clock would accept.
    assert!(matches!(
        authorization.authorize_with_clock(LicenseRequirement::pro(), || Ok(9_999_999_999)),
        Err(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(rightkit_license::VerifyError::Expired { .. })
        ))
    ));
    // A trusted clock far before iat denies.
    assert!(matches!(
        authorization.authorize_with_clock(LicenseRequirement::pro(), || Ok(0)),
        Err(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(_)
        ))
    ));
}

/// A store with no atomic create: it must inherit the fail-closed default.
#[derive(Default)]
struct NonAtomicStore(Mutex<Option<String>>);

impl SecureStore<String> for NonAtomicStore {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn set(&self, value: &String) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(value.clone());
        Ok(())
    }
    fn delete(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

#[test]
fn set_if_absent_default_fails_closed_and_stores_nothing() {
    let store = NonAtomicStore::default();
    assert!(store.set_if_absent(&"device-1".to_string()).is_err());
    assert_eq!(store.get().unwrap(), None, "no check-then-set fallback");

    // Device creation over such a store refuses instead of minting non-atomically.
    let service = DeviceService::new(NonAtomicStore::default());
    let error = service.get_or_create().unwrap_err();
    assert_eq!(error.operation, StorageOperation::Set);
    assert_eq!(service.get().unwrap(), None);
}

#[test]
fn atomic_set_if_absent_creates_once_and_never_overwrites() {
    let store = MemoryStore::<String>::default();
    assert!(store.set_if_absent(&"first".to_string()).unwrap());
    assert!(!store.set_if_absent(&"second".to_string()).unwrap());
    assert_eq!(store.get().unwrap().as_deref(), Some("first"));
    assert_eq!(store.writes(), 1);
}

#[test]
fn alternate_identity_grants_only_when_not_revoked() {
    // TOKEN is bound to raw device "dev-1" (entitlement id "lic_1"); this device's
    // primary id differs, so only a caller-supplied alternate can match.
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("primary-device".into()));
    let authorization = pro_authorization(&licenses, &devices);

    // No alternate supplied: the primary (and its derived hash) do not match.
    assert!(matches!(
        authorization.authorize_any_device_with_clock(
            LicenseRequirement::pro(),
            &[],
            &RevokedIds::new(),
            || Ok(2_000),
        ),
        Err(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(_)
        ))
    ));

    // The exact alternate matches and grants when nothing is revoked.
    assert!(authorization
        .authorize_any_device_with_clock(
            LicenseRequirement::pro(),
            &["dev-1"],
            &RevokedIds::new(),
            || Ok(2_000),
        )
        .is_ok());

    // The same authentic token, matched through the alternate, is refused once its
    // entitlement id is revoked: the denylist is consulted before any grant.
    assert_eq!(
        authorization
            .authorize_any_device_with_clock(
                LicenseRequirement::pro(),
                &["dev-1"],
                &RevokedIds::new().with("lic_1"),
                || Ok(2_000),
            )
            .unwrap_err(),
        AuthorizationError::Denied(AuthorizationDenial::RevokedToken("lic_1".into()))
    );

    // This token binds the raw id; supplying only its hash form never grants.
    let other_hash = device_id_hash("dev-1");
    assert!(matches!(
        authorization.authorize_any_device_with_clock(
            LicenseRequirement::pro(),
            &[other_hash.as_str()],
            &RevokedIds::new(),
            || Ok(2_000),
        ),
        Err(AuthorizationError::Denied(
            AuthorizationDenial::InvalidToken(_)
        ))
    ));
}

#[test]
fn revoked_token_is_refused_on_the_primary_identity_too() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = pro_authorization(&licenses, &devices);

    assert_eq!(
        authorization
            .authorize_any_device_with_clock(
                LicenseRequirement::pro(),
                &[],
                &RevokedIds::new().with("lic_1"),
                || Ok(2_000),
            )
            .unwrap_err(),
        AuthorizationError::Denied(AuthorizationDenial::RevokedToken("lic_1".into()))
    );
    assert!(authorization
        .authorize_any_device_with_clock(
            LicenseRequirement::pro(),
            &[],
            &RevokedIds::new().with("some_other_license"),
            || Ok(2_000),
        )
        .is_ok());
}

#[test]
fn trusted_clock_failure_denies_before_touching_stores() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = pro_authorization(&licenses, &devices);

    assert_eq!(
        authorization
            .authorize_with_clock(
                LicenseRequirement::pro(),
                || Err("rollback detected".into())
            )
            .unwrap_err(),
        AuthorizationError::Clock("rollback detected".into())
    );
}
