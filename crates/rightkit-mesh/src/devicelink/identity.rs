//! Device identity: an Ed25519 key whose seed lives in the OS credential store.
//!
//! The identity key signs pairing confirmations, resume tickets, revoke messages and link
//! requests. It never leaves the credential store except as the 32-byte seed the process
//! reads into memory to build the signing key; the registry file holds public keys only.

use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use super::LinkError;
use rightkit_crypto::sha256;
use rightkit_device_link_types::DEVICE_REF_SCHEMA_VERSION;
use rightkit_device_link_types::{DevicePlatform, DeviceRef};
use rightkit_fs::digest::hex;

/// Credential-store service name the OS vault uses unless the host passes its own.
pub const DEFAULT_VAULT_SERVICE: &str = "pulse.device-link";

const SEED_ACCOUNT: &str = "device-link-identity-v1";
const SEED_DOMAIN: &[u8] = b"pulse-link/identity-seed/v1";
const DEVICE_ID_DOMAIN: &[u8] = b"pulse-link/device-id/v1";

/// Where the identity seed is kept. The OS implementation is [`OsKeyVault`].
pub trait KeyVault {
    /// Return the 32-byte identity seed, creating and storing a random one on first use.
    /// A read failure is an error, never "absent": an unreadable store must not mint a new
    /// identity over the old one.
    fn load_or_create_seed(&self) -> Result<[u8; 32], LinkError>;
}

/// The OS credential store (Keychain, Credential Manager, Secret Service) through
/// `rightkit-credentials`. Refuses a store that is not durable (a guard or a missing store
/// would put the key in memory and lose it, which would silently end every pairing).
pub struct OsKeyVault {
    store: rightkit_credentials::Store,
}

impl OsKeyVault {
    pub fn open(service: &str) -> Result<Self, LinkError> {
        let namespace = rightkit_credentials::Namespace::new(service.to_string())
            .map_err(|e| LinkError::Vault(e.to_string()))?;
        let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
        if !store.is_durable() {
            return Err(LinkError::Vault(
                "the OS credential store is guarded or unavailable".to_string(),
            ));
        }
        Ok(Self { store })
    }
}

impl KeyVault for OsKeyVault {
    fn load_or_create_seed(&self) -> Result<[u8; 32], LinkError> {
        // `get_or_create_key` mints only on an authoritative "absent" and adopts the winner
        // of a concurrent create. The stored text is random; the seed is its domain-separated
        // SHA-256 so the key is a fixed 32 bytes whatever the text encoding.
        let (secret, _created) = self
            .store
            .get_or_create_key(SEED_ACCOUNT, 32)
            .map_err(|e| LinkError::Vault(e.to_string()))?;
        let mut input = SEED_DOMAIN.to_vec();
        input.extend_from_slice(secret.expose_bytes());
        Ok(sha256(&input))
    }
}

/// The identity seed in an owner-only file, for a headless Linux host with no Secret Service
/// (a server: no desktop session, no keyring). Used only when the OS store is unavailable there.
/// The file is created once with `create_new` and mode 0600 inside the engine's private folder;
/// a file that exists but cannot be read is an error, never a reason to mint a new identity.
#[cfg(target_os = "linux")]
pub struct FileKeyVault {
    path: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl KeyVault for FileKeyVault {
    fn load_or_create_seed(&self) -> Result<[u8; 32], LinkError> {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        let vault = |e: std::io::Error| LinkError::Vault(format!("identity file: {e}"));
        let mut secret = [0u8; 32];
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.path)
        {
            Ok(mut file) => {
                secret = super::wire::random::<32>()?;
                file.write_all(&secret).map_err(vault)?;
                file.sync_all().map_err(vault)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::File::open(&self.path)
                    .and_then(|mut f| f.read_exact(&mut secret))
                    .map_err(vault)?;
            }
            Err(e) => return Err(vault(e)),
        }
        let mut input = SEED_DOMAIN.to_vec();
        input.extend_from_slice(&secret);
        Ok(sha256(&input))
    }
}

/// The OS credential store, or on a headless Linux host without one, the owner-only file in
/// `private_dir` (`FileKeyVault`). Every other platform fails closed when the store is missing.
pub fn open_vault(
    service: &str,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] private_dir: &std::path::Path,
) -> Result<Box<dyn KeyVault + Send + Sync>, LinkError> {
    #[cfg(target_os = "linux")]
    {
        // A headless host: the file, once it exists, is the identity. Otherwise the OS store
        // is tried first and the file is created only when that store cannot answer.
        let file = FileKeyVault {
            path: private_dir.join("device-link-identity.key"),
        };
        if file.path.exists() {
            return Ok(Box::new(file));
        }
        Ok(Box::new(LinuxVault {
            os: OsKeyVault::open(service).ok(),
            file,
        }))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(Box::new(OsKeyVault::open(service)?))
}

/// The OS store when it answers, else the owner-only file (headless Linux).
#[cfg(target_os = "linux")]
struct LinuxVault {
    os: Option<OsKeyVault>,
    file: FileKeyVault,
}

#[cfg(target_os = "linux")]
impl KeyVault for LinuxVault {
    fn load_or_create_seed(&self) -> Result<[u8; 32], LinkError> {
        if let Some(os) = &self.os
            && let Ok(seed) = os.load_or_create_seed()
        {
            return Ok(seed);
        }
        self.file.load_or_create_seed()
    }
}

/// `dev_` plus the first 32 hex characters of SHA-256 over the public key. The id is derived
/// from the key, so a peer cannot claim an id it does not hold the key for, and every message
/// carrying an id is checked against the key it arrives with.
pub fn device_id_for_key(public_key: &[u8]) -> String {
    let mut input = DEVICE_ID_DOMAIN.to_vec();
    input.extend_from_slice(public_key);
    let digest = sha256(&input);
    format!("dev_{}", hex(&digest[..16]))
}

/// This device's identity: signing key, public key and the [`DeviceRef`] it presents.
pub struct DeviceIdentity {
    key: Ed25519KeyPair,
    public: [u8; 32],
    device: DeviceRef,
}

impl DeviceIdentity {
    /// Load (or create on first run) the identity from `vault`.
    pub fn load(
        vault: &dyn KeyVault,
        platform: DevicePlatform,
        display_name: Option<String>,
    ) -> Result<Self, LinkError> {
        let seed = vault.load_or_create_seed()?;
        Self::from_seed(&seed, platform, display_name)
    }

    pub fn from_seed(
        seed: &[u8; 32],
        platform: DevicePlatform,
        display_name: Option<String>,
    ) -> Result<Self, LinkError> {
        let key = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| LinkError::Vault("identity seed rejected".to_string()))?;
        let mut public = [0u8; 32];
        public.copy_from_slice(key.public_key().as_ref());
        let device = DeviceRef {
            schema_version: DEVICE_REF_SCHEMA_VERSION,
            device_id: device_id_for_key(&public),
            platform,
            display_name,
        };
        Ok(Self {
            key,
            public,
            device,
        })
    }

    pub fn device(&self) -> &DeviceRef {
        &self.device
    }

    pub fn device_id(&self) -> &str {
        &self.device.device_id
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    pub fn public_key_hex(&self) -> String {
        hex(&self.public)
    }

    /// Sign canonical bytes built with `Enc`; returns the signature as hex.
    pub(crate) fn sign(&self, signing_input: &[u8]) -> String {
        hex(self.key.sign(signing_input).as_ref())
    }
}

/// Verify an Ed25519 signature over canonical bytes.
pub(crate) fn verify(
    public_key: &[u8],
    signing_input: &[u8],
    signature: &[u8],
) -> Result<(), LinkError> {
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(signing_input, signature)
        .map_err(|_| LinkError::SignatureInvalid)
}
