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
/// Values must be nonempty UTF-8 without control characters. A stored value that is empty or
/// only whitespace is treated as absent: `get` returns `None` and `set_if_absent` replaces it,
/// so a blank slot cannot wedge device creation forever. Any other existing malformed value
/// (a control character, non-UTF-8) fails closed instead of being treated as missing or
/// replaced, and a non-blank value is never replaced by `set_if_absent`.
#[derive(Clone)]
pub struct CredentialStore {
    store: Store,
    account: String,
    codec: IdCodec,
}

/// How the device id is laid out in the credential's secret.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdCodec {
    /// The id string itself is the secret (the default).
    #[default]
    Raw,
    /// A compact JSON object `{"id":"<id>"}`, the format an app that predates this crate wrote
    /// into its credential item. Reading accepts any JSON object with a string `id` (other fields
    /// are ignored); a secret that is not such an object fails with an error (never treated as
    /// absent), and a blank `id` reads as absent. A blank secret reads as absent, as for `Raw`.
    JsonIdField,
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
            codec: IdCodec::Raw,
        })
    }

    /// Choose how the id is laid out in the secret (default [`IdCodec::Raw`]). Use
    /// [`IdCodec::JsonIdField`] to read and write an existing `{"id":"..."}` item byte for byte.
    #[must_use]
    pub fn with_codec(mut self, codec: IdCodec) -> Self {
        self.codec = codec;
        self
    }

    fn encode(&self, value: &str) -> Result<Secret, Error> {
        validate_value(value)?;
        Ok(match self.codec {
            IdCodec::Raw => Secret::from_bytes(value.as_bytes().to_vec()),
            IdCodec::JsonIdField => {
                Secret::from_bytes(serde_json::json!({ "id": value }).to_string().into_bytes())
            }
        })
    }

    fn decode(&self, stored: &str) -> Result<Option<String>, Error> {
        if is_blank(stored) {
            return Ok(None);
        }
        match self.codec {
            IdCodec::Raw => {
                validate_value(stored)?;
                Ok(Some(stored.to_owned()))
            }
            IdCodec::JsonIdField => {
                let malformed = |reason: String| Error::Invalid {
                    what: "device credential value",
                    reason,
                };
                let value: serde_json::Value = serde_json::from_str(stored)
                    .map_err(|e| malformed(format!("stored device id is malformed: {e}")))?;
                let id = value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| malformed("stored device id is malformed: no id".into()))?;
                if is_blank(id) {
                    return Ok(None);
                }
                validate_value(id)?;
                Ok(Some(id.to_owned()))
            }
        }
    }
}

fn is_blank(value: &str) -> bool {
    value.trim().is_empty()
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
                        self.decode(value)
                    })
                    .transpose()
                    .map(Option::flatten)
            })
            .map_err(|error| error.to_string())
    }

    fn set(&self, value: &String) -> Result<(), String> {
        let secret = self.encode(value).map_err(|error| error.to_string())?;
        self.store
            .set(&self.account, &secret)
            .map_err(|error| error.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        self.store
            .delete(&self.account)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn set_if_absent(&self, value: &String) -> Result<bool, String> {
        let secret = self.encode(value).map_err(|error| error.to_string())?;
        let added = self
            .store
            .add_if_absent(&self.account, &secret)
            .map_err(|error| error.to_string())?;
        if added {
            return Ok(true);
        }
        // Something occupies the slot. Only a blank value is replaced; the caller holds the
        // device creation lock (and the cross-process file lock when configured).
        match SecureStore::get(self)? {
            Some(_) => Ok(false),
            None => {
                self.store
                    .set(&self.account, &secret)
                    .map_err(|error| error.to_string())?;
                Ok(true)
            }
        }
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
