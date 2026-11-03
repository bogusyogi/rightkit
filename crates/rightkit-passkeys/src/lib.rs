//! A passkey authenticator for Right Suite apps (#968), as an adapter over `passkey-rs` 0.6.
//!
//! The consumer (a vault such as RightPass) implements two small traits:
//!
//! * [`CredentialStore`]: find, save and update-counter for [`Passkey`] records. It is
//!   synchronous and may block briefly (a vault read); the adapter calls it directly.
//! * [`UserVerifier`]: the user interaction. It reports what actually happened. The adapter never
//!   claims presence or verification the verifier did not perform (UP and UV flags are truthful):
//!   a request that requires verification is refused when the verifier does not support it.
//!
//! [`authenticator`] joins them to `passkey_authenticator::Authenticator`. [`normalize_rp_id`]
//! normalises a relying-party id before a request is handed over. The crate holds no key material
//! of its own: private keys live only inside the consumer's store.
//!
//! The Windows plugin-authenticator glue (packaged MSIX app, COM `IPluginAuthenticator`) and the
//! Apple credential-provider extension are app-side and in `RightKitCredentialProvider`; the
//! authenticator core is this crate.

use std::fmt;

use async_trait::async_trait;
use passkey_authenticator::{
    Authenticator, CredentialStore as PasskeyStore, DiscoverabilitySupport, StoreInfo, UiHint,
    UserCheck, UserValidationMethod,
};
use passkey_crypto::CryptoBackend;
use passkey_types::ctap2::aaguid::Aaguid;
use passkey_types::ctap2::make_credential::Options;
use passkey_types::ctap2::{Ctap2Error, StatusCode};
use passkey_types::webauthn::{
    PublicKeyCredentialDescriptor, PublicKeyCredentialRpEntity, PublicKeyCredentialUserEntity,
};

pub use passkey_types::Passkey;

/// Why the store could not do what it was asked. The authenticator maps every variant to a CTAP
/// "other" error; the text is for the consumer's own log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

/// The consumer's credential storage.
pub trait CredentialStore: Send + Sync + 'static {
    /// The passkeys for `rp_id` (already normalised). `credential_ids`, when given and not empty,
    /// restricts the result to those ids (an allow list); `user_handle`, when given, restricts it
    /// to that user. An empty result is not an error.
    fn find(
        &self,
        credential_ids: Option<&[Vec<u8>]>,
        rp_id: &str,
        user_handle: Option<&[u8]>,
    ) -> Result<Vec<Passkey>, StoreError>;

    /// Stores a new passkey. A passkey with the same credential id replaces the old one.
    fn save(&mut self, passkey: Passkey) -> Result<(), StoreError>;

    /// Records the new signature counter of the passkey with `credential_id`.
    fn update_counter(&mut self, credential_id: &[u8], counter: u32) -> Result<(), StoreError>;
}

/// What the user interface is asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRequest {
    /// Permission to save a new credential for `rp_id` and `user_name`.
    Create {
        rp_id: String,
        user_name: Option<String>,
    },
    /// Permission to sign in with the existing credential for `rp_id`.
    Use {
        rp_id: String,
        user_name: Option<String>,
    },
    /// Tell the user a credential already exists (the exclude list matched).
    InformExcluded { rp_id: String },
    /// Tell the user no credential matches.
    InformNoCredentials,
}

/// What the user interface actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserOutcome {
    /// The user was present (a gesture was made).
    pub presence: bool,
    /// The user was verified (biometric, PIN or passcode, performed now).
    pub verification: bool,
}

/// The consumer's user interaction.
#[async_trait]
pub trait UserVerifier: Send + Sync + 'static {
    /// Whether this verifier can verify the user at all (biometric, PIN). `false` makes every
    /// request that requires verification fail instead of claiming UV.
    fn supports_verification(&self) -> bool;

    /// Performs the interaction `request` and reports what happened. `require_presence` and
    /// `require_verification` say what the relying party needs. `Ok` must carry only what was
    /// really done. `Err` means the user declined or the interaction could not run.
    async fn confirm(
        &self,
        request: UserRequest,
        require_presence: bool,
        require_verification: bool,
    ) -> Result<UserOutcome, StoreError>;
}

/// Normalises a relying-party id: lowercase ASCII, one trailing dot removed. Refuses an empty id,
/// one over 253 bytes, non-ASCII (the caller must send the punycode form), and any character that
/// is not a letter, digit, `-`, `_` or `.`, so a scheme, port, path or space never reaches the
/// store.
pub fn normalize_rp_id(raw: &str) -> Result<String, StoreError> {
    let trimmed = raw.trim();
    let id = trimmed
        .strip_suffix('.')
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if id.is_empty() || id.len() > 253 {
        return Err(StoreError("relying party id is empty or too long".into()));
    }
    if id.starts_with('.') || id.contains("..") {
        return Err(StoreError("relying party id has an empty label".into()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(StoreError(
            "relying party id must be ASCII letters, digits, '-', '_' and '.'".into(),
        ));
    }
    Ok(id)
}

fn other() -> StatusCode {
    Ctap2Error::Other.into()
}

/// Adapts a [`CredentialStore`] to passkey-rs.
pub struct StoreAdapter<S>(S);

impl<S: CredentialStore> StoreAdapter<S> {
    pub fn new(store: S) -> Self {
        Self(store)
    }

    /// The consumer's store.
    pub fn store(&self) -> &S {
        &self.0
    }
}

#[async_trait]
impl<S: CredentialStore> PasskeyStore for StoreAdapter<S> {
    type PasskeyItem = Passkey;

    async fn find_credentials(
        &self,
        ids: Option<&[PublicKeyCredentialDescriptor]>,
        rp_id: &str,
        user_handle: Option<&[u8]>,
    ) -> Result<Vec<Self::PasskeyItem>, StatusCode> {
        let rp_id = normalize_rp_id(rp_id).map_err(|_| other())?;
        let wanted: Option<Vec<Vec<u8>>> = ids
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.iter().map(|d| d.id.to_vec()).collect());
        self.0
            .find(wanted.as_deref(), &rp_id, user_handle)
            .map_err(|_| other())
    }

    async fn save_credential(
        &mut self,
        cred: Passkey,
        _user: PublicKeyCredentialUserEntity,
        _rp: PublicKeyCredentialRpEntity,
        _options: Options,
    ) -> Result<(), StatusCode> {
        let mut cred = cred;
        cred.rp_id = normalize_rp_id(&cred.rp_id).map_err(|_| other())?;
        self.0.save(cred).map_err(|_| other())
    }

    async fn update_credential(&mut self, cred: &Self::PasskeyItem) -> Result<(), StatusCode> {
        match cred.counter {
            Some(counter) => self
                .0
                .update_counter(&cred.credential_id, counter)
                .map_err(|_| other()),
            None => Ok(()),
        }
    }

    async fn get_info(&self) -> StoreInfo {
        StoreInfo {
            discoverability: DiscoverabilitySupport::Full,
        }
    }
}

/// Adapts a [`UserVerifier`] to passkey-rs.
pub struct VerifierAdapter<U>(U);

impl<U: UserVerifier> VerifierAdapter<U> {
    pub fn new(verifier: U) -> Self {
        Self(verifier)
    }
}

#[async_trait]
impl<U: UserVerifier> UserValidationMethod for VerifierAdapter<U> {
    type PasskeyItem = Passkey;

    async fn check_user<'a>(
        &self,
        hint: UiHint<'a, Self::PasskeyItem>,
        presence: bool,
        verification: bool,
    ) -> Result<UserCheck, Ctap2Error> {
        if verification && !self.0.supports_verification() {
            return Err(Ctap2Error::UnsupportedOption);
        }
        let request = match hint {
            UiHint::InformExcludedCredentialFound(passkey) => UserRequest::InformExcluded {
                rp_id: passkey.rp_id.clone(),
            },
            UiHint::InformNoCredentialsFound => UserRequest::InformNoCredentials,
            UiHint::RequestNewCredential(user, rp) => UserRequest::Create {
                rp_id: rp.id.clone(),
                user_name: Some(user.name.clone()),
            },
            UiHint::RequestExistingCredential(passkey) => UserRequest::Use {
                rp_id: passkey.rp_id.clone(),
                user_name: passkey.username.clone(),
            },
        };
        let outcome = self
            .0
            .confirm(request, presence, verification)
            .await
            .map_err(|_| Ctap2Error::OperationDenied)?;
        // Truthful flags: what the verifier did, and a refusal when it fell short of what the
        // relying party required.
        if (presence && !outcome.presence) || (verification && !outcome.verification) {
            return Err(Ctap2Error::OperationDenied);
        }
        Ok(UserCheck {
            presence: outcome.presence,
            verification: outcome.verification,
        })
    }

    fn is_presence_enabled(&self) -> bool {
        true
    }

    fn is_verification_enabled(&self) -> Option<bool> {
        Some(self.0.supports_verification())
    }
}

/// The authenticator type [`authenticator`] returns.
pub type RightKitAuthenticator<S, U, C> = Authenticator<StoreAdapter<S>, VerifierAdapter<U>, C>;

/// Builds a passkey authenticator over the consumer's store and verifier. `crypto` is the
/// passkey-rs crypto backend the app chooses.
pub fn authenticator<S, U, C>(
    aaguid: Aaguid,
    store: S,
    verifier: U,
    crypto: C,
) -> RightKitAuthenticator<S, U, C>
where
    S: CredentialStore,
    U: UserVerifier,
    C: CryptoBackend,
{
    Authenticator::new(
        aaguid,
        StoreAdapter::new(store),
        VerifierAdapter::new(verifier),
        crypto,
    )
}
