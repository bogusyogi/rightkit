use rightkit_tauri::{
    guard_paid_command, AuthorizationDenial, AuthorizationError, AuthorizationService,
    DeviceService, DiagnosticHost, DiagnosticService, HostOperation, LicenseRecord,
    LicenseRequirement, LogHost, LogService, MutationHook, PaidCommandError, SecureService,
    SecureStore, StorageOperation,
};
use std::sync::{Arc, Barrier, Mutex};

const PUBKEY: [u8; 32] = [
    108, 142, 220, 5, 154, 87, 124, 180, 244, 31, 25, 204, 0, 155, 84, 165, 16, 227, 134, 87, 61,
    66, 199, 238, 168, 219, 128, 145, 39, 235, 80, 211,
];
const TOKEN: &str = "eyJ2IjoyLCJpZCI6ImxpY18xIiwiYXBwIjoiaGVhcmRyaWdodCIsInRpZXIiOiJwcm8iLCJmZWF0dXJlcyI6WyJ3YWtlX3dvcmQiLCJ2b2ljZV9jb21tYW5kcyJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MTAwMCwiZXhwIjo5OTk5OTk5OTk5fQ.JNmWEwG16VehpwJz9SooQIia0k1Ht0c150jca86h7sAnFj9tKuPO8yZhfyR48vx3Yd_03RQyYYtbKm08TCgsBg";

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredLicense(Option<String>);

impl LicenseRecord for StoredLicense {
    fn signed_token(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

struct MemoryStore<T> {
    value: Mutex<Option<T>>,
    fail_set: bool,
}

impl<T> Default for MemoryStore<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            fail_set: false,
        }
    }
}

impl<T> MemoryStore<T> {
    fn with(value: T) -> Self {
        Self {
            value: Mutex::new(Some(value)),
            fail_set: false,
        }
    }
}

impl<T: Clone> SecureStore<T> for MemoryStore<T> {
    fn get(&self) -> Result<Option<T>, String> {
        Ok(self.value.lock().unwrap().clone())
    }

    fn set(&self, value: &T) -> Result<(), String> {
        if self.fail_set {
            return Err("set failed".into());
        }
        *self.value.lock().unwrap() = Some(value.clone());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.value.lock().unwrap() = None;
        Ok(())
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
    let failed_store = MemoryStore::<String> {
        fail_set: true,
        ..Default::default()
    };
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
fn device_get_or_create_is_serialized_and_owns_uuid_v4_generation() {
    let service = Arc::new(DeviceService::new(MemoryStore::<String>::default()));
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
}

#[test]
fn authorization_preserves_app_and_expiry_verification_denials() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));

    let wrong_app = AuthorizationService::new(&licenses, &devices, &PUBKEY, "viewright")
        .authorize(LicenseRequirement::Pro, 2_000)
        .unwrap_err();
    assert!(matches!(
        wrong_app,
        AuthorizationError::Denied(AuthorizationDenial::InvalidToken(
            rightkit_license::VerifyError::AppMismatch { .. }
        ))
    ));

    let expired = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright")
        .authorize(LicenseRequirement::Pro, 10_000_000_000)
        .unwrap_err();
    assert!(matches!(
        expired,
        AuthorizationError::Denied(AuthorizationDenial::InvalidToken(
            rightkit_license::VerifyError::Expired { .. }
        ))
    ));
}

#[test]
fn authorization_uses_native_device_store_not_license_record() {
    let licenses = MemoryStore::with(StoredLicense(Some(TOKEN.into())));
    let devices = DeviceService::new(MemoryStore::with("different-device".into()));
    let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");

    assert!(matches!(
        authorization.authorize(LicenseRequirement::Pro, 2_000),
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

    assert!(authorization
        .authorize(LicenseRequirement::Pro, 2_000)
        .is_ok());
    assert!(authorization
        .authorize(LicenseRequirement::Feature("wake_word".into()), 2_000)
        .is_ok());
    assert_eq!(
        authorization
            .authorize(LicenseRequirement::Feature("cloud_sync".into()), 2_000)
            .unwrap_err(),
        AuthorizationError::Denied(AuthorizationDenial::RequirementNotMet(
            LicenseRequirement::Feature("cloud_sync".into())
        ))
    );
}

#[test]
fn paid_command_guard_never_runs_command_when_authorization_is_denied() {
    let licenses = MemoryStore::with(StoredLicense(None));
    let devices = DeviceService::new(MemoryStore::with("dev-1".into()));
    let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");
    let ran = Mutex::new(false);

    let result: Result<(), PaidCommandError<&str>> =
        guard_paid_command(&authorization, LicenseRequirement::Pro, 2_000, |_| {
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
