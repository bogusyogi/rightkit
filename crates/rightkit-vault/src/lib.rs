//! Split-key secret sync for a small map of secrets (for example a user's own API keys).
//!
//! The payload is sealed under a random data key; that key is wrapped under
//! `HKDF(server_key || client_secret)`. The server half is released by the entitlement service
//! to the signed-in Google subject; the client half lives only in the user's OS credential
//! stores; the sealed blob sits in the user's own cloud app folder. No single party can
//! decrypt. See `README.md` (construction, threat model, trust path, rotation, lapse and
//! deletion, offline cache) and `API.md`.

mod blob;
mod error;
mod http;
mod keys;
mod vault;

pub use blob::{Blob, VERSION};
pub use error::{Error, Result};
pub use http::HttpServerKeySource;
pub use keys::{
    CachedKeys, ClientSecret, ClientSecretStore, CredentialsClientSecret, CredentialsKeyCache,
    ServerKey, ServerKeyCache, ServerKeySource, ServerKeys, KEY_LEN,
};
pub use vault::{BlobStore, Config, IdToken, SecretMap, Vault, MAX_PAYLOAD_BYTES};
