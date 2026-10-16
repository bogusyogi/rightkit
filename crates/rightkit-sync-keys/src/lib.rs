//! End-to-end sync key management for Right Suite apps.
//!
//! * [`KeyManager`] (#144): the account [`DataKey`], generated once on the first device and kept
//!   in the OS credential store through `rightkit-credentials` only (this crate has no keystore
//!   code of its own, so a daemon host identity key and the sync key share one surface);
//!   versions, [`KeyManager::rotate`], install of a transferred key.
//! * [`DataKey::zone_key`] and [`KeyProvider`]: per-zone XChaCha20-Poly1305 keys for an
//!   encrypting sync backend, and [`account_scope`] for the engine's scope string.
//! * [`transfer`] (#145): device-approval key transfer, `rightkit-sync-keys-transfer/1`:
//!   the `rk-pair/1` short-code commit-reveal with X25519 and an AEAD wrap on top.
//!
//! Not here: the recovery-phrase (Argon2id) wrap of the design, which is a separate path that
//! needs a new dependency; and the relay, which only carries the opaque messages of [`transfer`].

mod error;
mod key;
mod manager;
mod pairing_sas;
mod provider;
mod util;

pub mod transfer;

pub use error::{Error, Result};
pub use key::{account_scope, DataKey, KeyId, ZoneKey, KEY_LEN};
pub use manager::{InstallOutcome, KeyManager};
pub use provider::{KeyProvider, StaticKeys, ZoneKeys};
pub use transfer::{format_code, Approver, Requester, TransferContext};
