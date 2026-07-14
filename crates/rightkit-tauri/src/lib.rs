//! Native host services shared by Right Suite Tauri applications.
//!
//! Apps retain their platform keyring implementation and product-specific side
//! effects. This crate owns mutation ordering, device reconciliation, and the
//! fail-closed bridge to `rightkit-license` verification.

use rightkit_license::{verify_token, Entitlement, VerifyError};
use std::marker::PhantomData;

pub trait SecureStore<T> {
    fn get(&self) -> Result<Option<T>, String>;
    fn set(&self, value: &T) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

pub trait MutationHook<T> {
    fn after_set(&self, value: &T) -> Result<(), String>;
    fn after_delete(&self) -> Result<(), String>;
}

pub struct NoopHook;

impl<T> MutationHook<T> for NoopHook {
    fn after_set(&self, _value: &T) -> Result<(), String> {
        Ok(())
    }

    fn after_delete(&self) -> Result<(), String> {
        Ok(())
    }
}

pub struct SecureService<'a, T, S, H> {
    store: &'a S,
    hook: &'a H,
    value: PhantomData<fn() -> T>,
}

impl<'a, T, S, H> SecureService<'a, T, S, H>
where
    S: SecureStore<T>,
    H: MutationHook<T>,
{
    pub fn new(store: &'a S, hook: &'a H) -> Self {
        Self {
            store,
            hook,
            value: PhantomData,
        }
    }

    pub fn get(&self) -> Result<Option<T>, String> {
        self.store.get()
    }

    pub fn set(&self, value: &T) -> Result<(), String> {
        self.store.set(value)?;
        self.hook.after_set(value)
    }

    pub fn delete(&self) -> Result<(), String> {
        self.store.delete()?;
        self.hook.after_delete()
    }
}

pub fn get_or_create_device<T: Clone>(
    store: &impl SecureStore<T>,
    fallback: T,
) -> Result<T, String> {
    if let Some(existing) = store.get()? {
        return Ok(existing);
    }
    store.set(&fallback)?;
    Ok(fallback)
}

pub trait LicenseRecord {
    fn signed_token(&self) -> Option<&str>;
    fn device_id(&self) -> &str;
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerificationError {
    MissingToken,
    Invalid(VerifyError),
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingToken => formatter.write_str("signed entitlement token is required"),
            Self::Invalid(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for VerificationError {}

pub fn verified_entitlement(
    license: &impl LicenseRecord,
    public_key: &[u8; 32],
    expected_app: &str,
    now_unix: i64,
) -> Result<Entitlement, VerificationError> {
    let token = license
        .signed_token()
        .ok_or(VerificationError::MissingToken)?;
    verify_token(
        token,
        public_key,
        expected_app,
        license.device_id(),
        now_unix,
    )
    .map_err(VerificationError::Invalid)
}
