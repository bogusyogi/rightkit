//! Opt-in OS credential adapter for native device storage.

use crate::SecureStore;
use rightkit_credentials::{windows_credential_identity, Error, Secret, Store};

/// One string credential in one pinned credential store.
///
/// Apps supply namespace, account, interaction & guard policy through [`Store`].
/// Construction resolves its backend once, so a changing OS default cannot split
/// a device creation race across stores. Backend failures remain failures, never
/// absence. The backend must honor its atomic `add_if_absent` contract against
/// every writer; this adapter never substitutes a read followed by a write.
///
/// Values must be nonempty UTF-8 without control characters. Existing malformed
/// values fail closed instead of being treated as missing or replaced.
#[derive(Clone)]
pub struct CredentialStore {
    store: Store,
    account: String,
}

impl CredentialStore {
    /// Validate the account without touching its value, then pin the backend.
    /// No credential value is included in an error.
    pub fn new(store: Store, account: impl Into<String>) -> Result<Self, Error> {
        let account = account.into();
        // Uses the same namespace/account/target validation as Store operations.
        windows_credential_identity(store.namespace(), &account)?;
        Ok(Self {
            store: store.pinned()?,
            account,
        })
    }
}

fn validate_value(value: &str) -> Result<(), Error> {
    let reason = if value.is_empty() {
        "empty"
    } else if value.chars().any(char::is_control) {
        "contains a control character"
    } else {
        return Ok(());
    };
    Err(Error::Invalid {
        what: "device credential value",
        reason: reason.into(),
    })
}

impl SecureStore<String> for CredentialStore {
    fn get(&self) -> Result<Option<String>, String> {
        self.store
            .get(&self.account)
            .and_then(|secret| {
                secret
                    .map(|secret| {
                        let value = secret.expose_str()?;
                        validate_value(value)?;
                        Ok(value.to_owned())
                    })
                    .transpose()
            })
            .map_err(|error| error.to_string())
    }

    fn set(&self, value: &String) -> Result<(), String> {
        validate_value(value).map_err(|error| error.to_string())?;
        self.store
            .set(
                &self.account,
                &Secret::from_bytes(value.as_bytes().to_vec()),
            )
            .map_err(|error| error.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        self.store
            .delete(&self.account)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn set_if_absent(&self, value: &String) -> Result<bool, String> {
        validate_value(value).map_err(|error| error.to_string())?;
        self.store
            .add_if_absent(
                &self.account,
                &Secret::from_bytes(value.as_bytes().to_vec()),
            )
            .map_err(|error| error.to_string())
    }
}

/// The licence client's device source over the credential-backed device service: the one per-install
/// device id, created once and read the same way by every caller, instead of an adapter each app wrote
/// by hand. `device_id` is `get_or_create` (a failed store is an error, never a new id) and
/// `device_id_hash` is the lowercase hex SHA-256 of it that the lease endpoints take
/// ([`crate::device_id_hash`]).
#[cfg(feature = "license-client")]
impl rightkit_license::DeviceIdSource for crate::DeviceService<CredentialStore> {
    fn device_id(&self) -> Result<String, rightkit_license::DeviceIdError> {
        self.get_or_create()
            .map_err(|_| rightkit_license::DeviceIdError)
    }

    fn device_id_hash(&self) -> Result<String, rightkit_license::DeviceIdError> {
        self.device_id().map(|id| crate::device_id_hash(&id))
    }
}
