use rightkit_tauri::{
    get_or_create_device, verified_entitlement, LicenseRecord, MutationHook, SecureService,
    SecureStore, VerificationError,
};
use std::cell::RefCell;

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredLicense {
    token: Option<String>,
    device_id: String,
}

impl LicenseRecord for StoredLicense {
    fn signed_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn device_id(&self) -> &str {
        &self.device_id
    }
}

#[derive(Default)]
struct MemoryStore<T>(RefCell<Option<T>>);

impl<T: Clone> SecureStore<T> for MemoryStore<T> {
    fn get(&self) -> Result<Option<T>, String> {
        Ok(self.0.borrow().clone())
    }

    fn set(&self, value: &T) -> Result<(), String> {
        *self.0.borrow_mut() = Some(value.clone());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.0.borrow_mut() = None;
        Ok(())
    }
}

#[derive(Default)]
struct RecordingHook(RefCell<Vec<&'static str>>);

impl<T> MutationHook<T> for RecordingHook {
    fn after_set(&self, _value: &T) -> Result<(), String> {
        self.0.borrow_mut().push("set");
        Ok(())
    }

    fn after_delete(&self) -> Result<(), String> {
        self.0.borrow_mut().push("delete");
        Ok(())
    }
}

#[test]
fn secure_service_runs_app_hook_only_after_successful_storage_mutation() {
    let store = MemoryStore::default();
    let hook = RecordingHook::default();
    let service = SecureService::new(&store, &hook);

    service.set(&"license-1".to_string()).unwrap();
    assert_eq!(service.get().unwrap().as_deref(), Some("license-1"));
    service.delete().unwrap();
    assert_eq!(service.get().unwrap(), None);
    assert_eq!(&*hook.0.borrow(), &["set", "delete"]);
}

#[test]
fn device_get_or_create_preserves_existing_or_persists_fallback() {
    let store = MemoryStore::default();
    assert_eq!(
        get_or_create_device(&store, "fallback".to_string()).unwrap(),
        "fallback"
    );
    assert_eq!(
        get_or_create_device(&store, "other".to_string()).unwrap(),
        "fallback"
    );
}

#[test]
fn missing_signed_token_fails_closed() {
    let license = StoredLicense {
        token: None,
        device_id: "dev-1".into(),
    };
    assert!(matches!(
        verified_entitlement(&license, &[0; 32], "heardright", 2_000),
        Err(VerificationError::MissingToken)
    ));
}
