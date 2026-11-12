//! A [`PendingReleaseQueue`] over one credential-store item.
//!
//! The rows are one JSON array in one item (`account` of the `Store` the host passes), in the
//! format HeardRight stored before it adopted the client, so an installed app's pending removals
//! survive the move: `[{"kind":"legacy","activation_token":"..."},
//! {"kind":"lease","device_id_hash":"...","lease_id":"..."}]`. The item is absent when the queue is
//! empty. Legacy rows carry an activation token, which is why the queue lives in the credential
//! store. The host names the item (the store's namespace and the account); this crate invents none.
//!
//! The client calls the queue under one lock per client (see `release_queue`), so a read-modify-write
//! here needs no lock of its own. A second process writing the same item is not serialised.

use rightkit_credentials::Store;
use serde::{Deserialize, Serialize};

use super::release_queue::{PendingReleaseEntry, PendingReleaseId, PendingReleaseQueue};
use super::types::{PendingLicenseRelease, SecretString};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Row {
    Legacy {
        activation_token: String,
    },
    Lease {
        device_id_hash: String,
        lease_id: String,
    },
}

impl Row {
    fn from_release(release: &PendingLicenseRelease) -> Self {
        match release {
            PendingLicenseRelease::Activation(token) => Self::Legacy {
                activation_token: token.expose().to_string(),
            },
            PendingLicenseRelease::Lease {
                lease_id,
                device_id_hash,
            } => Self::Lease {
                device_id_hash: device_id_hash.clone(),
                lease_id: lease_id.clone(),
            },
        }
    }

    fn release(&self) -> PendingLicenseRelease {
        match self {
            Self::Legacy { activation_token } => {
                PendingLicenseRelease::Activation(SecretString::new(activation_token.clone()))
            }
            Self::Lease {
                device_id_hash,
                lease_id,
            } => PendingLicenseRelease::Lease {
                lease_id: lease_id.clone(),
                device_id_hash: device_id_hash.clone(),
            },
        }
    }

    /// The row's name for the client: never the token itself. A lease row is named by its two
    /// (non-secret) ids; a legacy row by a 64-bit FNV-1a of its token.
    fn id(&self) -> PendingReleaseId {
        PendingReleaseId::new(match self {
            Self::Lease {
                device_id_hash,
                lease_id,
            } => format!("lease:{device_id_hash}:{lease_id}"),
            Self::Legacy { activation_token } => {
                let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
                for byte in activation_token.bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
                }
                format!("legacy:{hash:016x}")
            }
        })
    }
}

/// The pending-release queue kept in one credential-store item.
pub struct CredentialReleaseQueue {
    store: Store,
    account: String,
}

impl CredentialReleaseQueue {
    /// The queue in item `account` of `store`. For an app that already has a queue in the
    /// credential store, pass the same namespace and account so its rows are read in place.
    pub fn new(store: Store, account: impl Into<String>) -> Self {
        Self {
            store,
            account: account.into(),
        }
    }

    fn load(&self) -> Result<Vec<Row>, ()> {
        match self.store.get(&self.account).map_err(|_| ())? {
            Some(secret) => {
                serde_json::from_str(secret.expose_str().map_err(|_| ())?).map_err(|_| ())
            }
            None => Ok(Vec::new()),
        }
    }

    fn save(&self, rows: &[Row]) -> Result<(), ()> {
        if rows.is_empty() {
            return self.store.delete(&self.account).map(|_| ()).map_err(|_| ());
        }
        let raw = serde_json::to_string(rows).map_err(|_| ())?;
        self.store.set_str(&self.account, &raw).map_err(|_| ())
    }
}

#[allow(clippy::result_unit_err)] // the trait's own error shape
impl PendingReleaseQueue for CredentialReleaseQueue {
    fn enqueue(&self, release: &PendingLicenseRelease) -> Result<PendingReleaseId, ()> {
        let row = Row::from_release(release);
        let id = row.id();
        let mut rows = self.load()?;
        if !rows.contains(&row) {
            rows.push(row);
            self.save(&rows)?;
        }
        Ok(id)
    }

    fn pending(&self) -> Result<Vec<PendingReleaseEntry>, ()> {
        Ok(self
            .load()?
            .iter()
            .map(|row| PendingReleaseEntry {
                id: row.id(),
                release: row.release(),
            })
            .collect())
    }

    fn acknowledge(&self, id: &PendingReleaseId) -> Result<(), ()> {
        let mut rows = self.load()?;
        let before = rows.len();
        rows.retain(|row| row.id() != *id);
        if rows.len() == before {
            return Ok(());
        }
        self.save(&rows)
    }
}
