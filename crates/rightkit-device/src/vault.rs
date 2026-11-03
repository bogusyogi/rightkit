//! Native licence vault: the signed licence token is stored, verified and used in Rust.
//!
//! The webview never receives the token. [`LicenseVault::install`] verifies a token before it
//! is written, [`LicenseVault::status`] returns [`LicenseStatus`] (state, tier, features,
//! expiry, no token), and [`LicenseVault::require`] returns the verified [`Entitlement`] to
//! native callers only. A token that fails any check is never stored.
//!
//! The vault holds no secret of its own: the token lives behind the app's `SecureStore<String>`
//! (for example the `credentials` feature's `CredentialStore`) and the device identity behind a
//! `DeviceService`. Apps keep their own `#[tauri::command]` wrappers and command names; a wrapper
//! returns [`LicenseStatus`] or `()`, never the token.

use crate::{device_id_hash, DeviceService, SecureStore, StorageError, StorageOperation};
use rightkit_license::{
    verify_token_not_revoked, Entitlement, LicenseRequirement, RevocationError, RevokedIds,
    VerifyError,
};
use serde::Serialize;

/// Whether the vault's stored licence grants anything right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenseState {
    /// A verified, unexpired, unrevoked token is stored for this device.
    Active,
    /// No token is stored, or no device identity exists yet.
    Missing,
    /// A token is stored but fails verification (signature, app, device, time or lifecycle).
    Invalid,
    /// A token is stored and authentic, but its entitlement id is on the revocation list.
    Revoked,
}

/// Everything a webview may see about the licence. It carries no token material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub state: LicenseState,
    pub tier: Option<String>,
    pub features: Vec<String>,
    pub expires_at: Option<i64>,
    /// Why the state is not `active`, in words that contain no token material.
    pub reason: Option<String>,
}

impl LicenseStatus {
    fn missing(reason: &str) -> Self {
        Self {
            state: LicenseState::Missing,
            tier: None,
            features: Vec::new(),
            expires_at: None,
            reason: Some(reason.to_owned()),
        }
    }

    fn from_entitlement(entitlement: &Entitlement) -> Self {
        Self {
            state: LicenseState::Active,
            tier: Some(entitlement.tier.clone()),
            features: entitlement.features.clone(),
            expires_at: Some(entitlement.exp),
            reason: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    /// The store or device identity failed. Nothing was granted or written.
    Storage(StorageError),
    /// The trusted clock failed; the check was not made.
    Clock(String),
    /// No device identity exists, so no token can be bound to this device.
    NoDevice,
    /// The token failed verification. Nothing was stored.
    Rejected(VerifyError),
    /// The token is authentic but revoked. Nothing was stored.
    Revoked(String),
    /// The stored token is authentic and current but does not satisfy the requirement.
    RequirementNotMet(LicenseRequirement),
    /// Nothing is stored, or the stored token does not verify (see [`LicenseVault::status`]).
    Unlicensed(LicenseState),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::Clock(error) => write!(formatter, "licence clock failed: {error}"),
            Self::NoDevice => write!(formatter, "no device identity exists for this licence"),
            Self::Rejected(error) => write!(formatter, "licence token refused: {error}"),
            Self::Revoked(id) => write!(formatter, "licence {id} is revoked"),
            Self::RequirementNotMet(_) => write!(formatter, "licence does not grant this feature"),
            Self::Unlicensed(state) => write!(formatter, "no usable licence ({state:?})"),
        }
    }
}

impl std::error::Error for VaultError {}

/// One vault per app, held in the app's managed native state.
pub struct LicenseVault<'a, L, D> {
    licenses: &'a L,
    devices: &'a DeviceService<D>,
    public_key: &'a [u8; 32],
    expected_app: &'a str,
}

impl<'a, L, D> LicenseVault<'a, L, D>
where
    L: SecureStore<String>,
    D: SecureStore<String>,
{
    pub fn new(
        licenses: &'a L,
        devices: &'a DeviceService<D>,
        public_key: &'a [u8; 32],
        expected_app: &'a str,
    ) -> Self {
        Self {
            licenses,
            devices,
            public_key,
            expected_app,
        }
    }

    /// Verify `token` for this device and app, then store it. A refused token is never written,
    /// and a previously stored token is left in place when the new one is refused.
    pub fn install(
        &self,
        token: &str,
        revoked: &RevokedIds,
        clock: impl FnOnce() -> Result<i64, String>,
    ) -> Result<LicenseStatus, VaultError> {
        let now_unix = clock().map_err(VaultError::Clock)?;
        let device_id = self.device_id()?.ok_or(VaultError::NoDevice)?;
        let entitlement = self.verify(token, &device_id, revoked, now_unix)?;
        self.licenses.set(&token.to_owned()).map_err(|error| {
            VaultError::Storage(StorageError::new(StorageOperation::Set, error))
        })?;
        Ok(LicenseStatus::from_entitlement(&entitlement))
    }

    /// What the stored licence grants now. A stored token that fails verification is reported
    /// as `invalid` or `revoked`, never as an error, so the UI can explain it.
    pub fn status(
        &self,
        revoked: &RevokedIds,
        clock: impl FnOnce() -> Result<i64, String>,
    ) -> Result<LicenseStatus, VaultError> {
        let now_unix = clock().map_err(VaultError::Clock)?;
        let Some(token) = self.stored_token()? else {
            return Ok(LicenseStatus::missing("no licence is stored"));
        };
        let Some(device_id) = self.device_id()? else {
            return Ok(LicenseStatus::missing("no device identity exists"));
        };
        match self.verify(&token, &device_id, revoked, now_unix) {
            Ok(entitlement) => Ok(LicenseStatus::from_entitlement(&entitlement)),
            Err(VaultError::Revoked(id)) => Ok(LicenseStatus {
                state: LicenseState::Revoked,
                tier: None,
                features: Vec::new(),
                expires_at: None,
                reason: Some(format!("licence {id} is revoked")),
            }),
            Err(VaultError::Rejected(error)) => Ok(LicenseStatus {
                state: LicenseState::Invalid,
                tier: None,
                features: Vec::new(),
                expires_at: None,
                reason: Some(error.to_string()),
            }),
            Err(other) => Err(other),
        }
    }

    /// The verified entitlement, for native callers that gate a feature. The token stays native.
    pub fn require(
        &self,
        requirement: LicenseRequirement,
        revoked: &RevokedIds,
        clock: impl FnOnce() -> Result<i64, String>,
    ) -> Result<Entitlement, VaultError> {
        let now_unix = clock().map_err(VaultError::Clock)?;
        let token = self
            .stored_token()?
            .ok_or(VaultError::Unlicensed(LicenseState::Missing))?;
        let device_id = self
            .device_id()?
            .ok_or(VaultError::Unlicensed(LicenseState::Missing))?;
        let entitlement = self.verify(&token, &device_id, revoked, now_unix)?;
        if !entitlement.satisfies(&requirement) {
            return Err(VaultError::RequirementNotMet(requirement));
        }
        Ok(entitlement)
    }

    /// Remove the stored token (sign-out or deactivation).
    pub fn remove(&self) -> Result<(), VaultError> {
        self.licenses.delete().map_err(|error| {
            VaultError::Storage(StorageError::new(StorageOperation::Delete, error))
        })
    }

    fn stored_token(&self) -> Result<Option<String>, VaultError> {
        self.licenses
            .get()
            .map_err(|error| VaultError::Storage(StorageError::new(StorageOperation::Get, error)))
    }

    fn device_id(&self) -> Result<Option<String>, VaultError> {
        self.devices.get().map_err(VaultError::Storage)
    }

    /// Candidate device ids are the raw identity and its hash, the same binding the
    /// lease and link tokens use. Each must match exactly.
    fn verify(
        &self,
        token: &str,
        device_id: &str,
        revoked: &RevokedIds,
        now_unix: i64,
    ) -> Result<Entitlement, VaultError> {
        let hashed = device_id_hash(device_id);
        let candidates: [&str; 2] = [device_id, hashed.as_str()];
        verify_token_not_revoked(
            token,
            self.public_key,
            self.expected_app,
            &candidates,
            now_unix,
            revoked,
        )
        .map_err(|error| match error {
            RevocationError::Verify(error) => VaultError::Rejected(error),
            RevocationError::Revoked { id } => VaultError::Revoked(id),
        })
    }
}
