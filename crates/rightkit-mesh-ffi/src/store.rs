//! The paired computers this phone knows, kept in `<state dir>/hosts.json` (public data only: keys,
//! ids, names and relay URLs; never the seed), and the mesh's trust question answered from it.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rightkit_mesh::{IntroOutcome, Introduction, MeshError, PeerInfo, PeerTrust};
use serde::{Deserialize, Serialize};

use crate::pairing::{PmError, unhex};
use rightkit_fs::digest::hex;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// The computer's endpoint id: its device-link public key, 64 hex.
    pub endpoint_id: String,
    pub device_id: String,
    pub name: Option<String>,
    pub platform: String,
    pub relay: Option<String>,
    pub paired_at_ms: u64,
    pub pairing_id: String,
    /// The computer that introduced this one through the circle (`circle.rs`); `None` when this
    /// phone paired with it directly.
    #[serde(default)]
    pub introduced_by: Option<String>,
    /// The owner-check key of this computer's owner, pinned at pairing from the signature that
    /// admitted this phone (`circle::host_admit_check`). Only a computer holding it may share the
    /// circle; `None` for a computer added without one (a PIN, a typed code with no signature).
    #[serde(default)]
    pub admit_check: Option<crate::circle::CheckKey>,
    /// The owner-check key that signed the endorsement batch which introduced this computer through
    /// the circle; `None` for a computer this phone paired with directly. It scopes a signed
    /// removal (`circle::verify_revokes`): the owner of that key may remove what it endorsed.
    #[serde(default)]
    pub endorsed_by: Option<crate::circle::CheckKey>,
    /// The pairing id of the token or QR grant (`circle::GrantPair`) that vouched for this computer
    /// when it joined that way; `None` otherwise. One pairing id has one joiner: a list carrying a
    /// second one for it removes this host and adds neither.
    #[serde(default)]
    pub grant_pairing_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    hosts: Vec<Host>,
    /// Device ids removed by a signed circle revoke. A later list never adds one again (a revoked
    /// record is terminal); pairing with it directly again clears the entry.
    #[serde(default)]
    revoked: Vec<String>,
}

#[derive(Debug)]
pub struct Trust {
    path: PathBuf,
    hosts: Mutex<Vec<Host>>,
    revoked: Mutex<Vec<String>>,
}

impl Trust {
    /// Read the file; a missing file is an empty list, an unreadable one is an error (never "no
    /// computers": that would hide a pairing).
    pub fn load(path: PathBuf) -> Result<Self, PmError> {
        let file = match std::fs::read(&path) {
            Ok(raw) => serde_json::from_slice::<File>(&raw)
                .map_err(|e| PmError::new("store", format!("hosts file: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(PmError::new("store", e.to_string())),
        };
        Ok(Self {
            path,
            hosts: Mutex::new(file.hosts),
            revoked: Mutex::new(file.revoked),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Host>> {
        self.hosts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn revoked_lock(&self) -> MutexGuard<'_, Vec<String>> {
        self.revoked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, hosts: &[Host]) -> Result<(), PmError> {
        let raw = serde_json::to_vec_pretty(&File {
            hosts: hosts.to_vec(),
            revoked: self.revoked_lock().clone(),
        })
        .map_err(|e| PmError::new("store", e.to_string()))?;
        rightkit_fs::atomic_write(&self.path, &raw)
            .map_err(|e| PmError::new("store", e.to_string()))
    }

    pub fn all(&self) -> Vec<Host> {
        self.lock().clone()
    }

    /// The ids a signed circle revoke removed from this phone.
    pub fn revoked(&self) -> Vec<String> {
        self.revoked_lock().clone()
    }

    pub fn by_device(&self, device_id: &str) -> Option<Host> {
        self.lock()
            .iter()
            .find(|h| h.device_id == device_id)
            .cloned()
    }

    /// The relay stored with the most recently paired computer that has one. Computers added
    /// through another's introduction carry no relay; taking the newest record outright let one of
    /// those turn the phone's relay off (Pulse #595).
    pub fn latest_relay(&self) -> Option<String> {
        self.lock()
            .iter()
            .filter(|h| h.relay.is_some())
            .max_by_key(|h| h.paired_at_ms)
            .and_then(|h| h.relay.clone())
    }

    /// Record a pairing (replacing an older one with the same device id).
    pub fn add(&self, host: Host) -> Result<(), PmError> {
        let mut hosts = self.lock();
        // A new direct pairing is a new record generation: it clears an earlier revoke.
        self.revoked_lock().retain(|id| *id != host.device_id);
        hosts.retain(|h| h.device_id != host.device_id);
        hosts.push(host);
        self.save(&hosts)
    }

    /// Record a verified circle list: each host in `add` that is not known yet. A list never
    /// removes a host it omits (only a valid signed revoke may). A known host (paired directly or
    /// already introduced) is never replaced. One save. Returns how many were added.
    pub fn apply_circle(&self, add: Vec<Host>) -> Result<usize, PmError> {
        let mut hosts = self.lock();
        let mut added = 0;
        let revoked = self.revoked_lock().clone();
        for host in add {
            if hosts.iter().all(|h| h.device_id != host.device_id)
                && !revoked.contains(&host.device_id)
            {
                hosts.push(host);
                added += 1;
            }
        }
        if added > 0 {
            self.save(&hosts)?;
        }
        Ok(added)
    }

    /// Remove the hosts a verified signed revoke named (`circle::verify_revokes`) and remember their
    /// ids, so no later list adds them again. One save. Returns how many hosts were removed.
    pub fn apply_revokes(&self, device_ids: &[String]) -> Result<usize, PmError> {
        if device_ids.is_empty() {
            return Ok(0);
        }
        let mut hosts = self.lock();
        let before = hosts.len();
        hosts.retain(|h| !device_ids.contains(&h.device_id));
        let removed = before - hosts.len();
        {
            let mut revoked = self.revoked_lock();
            for id in device_ids {
                if !revoked.contains(id) {
                    revoked.push(id.clone());
                }
            }
        }
        self.save(&hosts)?;
        Ok(removed)
    }

    pub fn remove(&self, device_id: &str) -> Result<bool, PmError> {
        let mut hosts = self.lock();
        let before = hosts.len();
        hosts.retain(|h| h.device_id != device_id);
        let removed = hosts.len() != before;
        if removed {
            self.save(&hosts)?;
        }
        Ok(removed)
    }
}

fn info(host: &Host) -> Option<PeerInfo> {
    Some(PeerInfo {
        endpoint: unhex::<32>(&host.endpoint_id).ok()?,
        device_id: host.device_id.clone(),
        platform: host.platform.clone(),
        display_name: host.name.clone(),
        guest: false,
    })
}

impl PeerTrust for Trust {
    fn peer(&self, endpoint: &[u8; 32]) -> Option<PeerInfo> {
        let id = hex(endpoint);
        self.lock()
            .iter()
            .find(|h| h.endpoint_id == id)
            .and_then(info)
    }

    fn peer_by_device(&self, device_id: &str) -> Option<PeerInfo> {
        self.lock()
            .iter()
            .find(|h| h.device_id == device_id)
            .and_then(info)
    }

    fn members(&self) -> Vec<PeerInfo> {
        self.lock().iter().filter_map(info).collect()
    }

    /// A phone takes no introductions: it pairs with each computer itself.
    fn apply_introduction(
        &self,
        _from: &PeerInfo,
        _introduction: &Introduction,
    ) -> Result<IntroOutcome, MeshError> {
        Err(MeshError::NotPaired)
    }

    fn apply_revoke(
        &self,
        _from: &PeerInfo,
        _target_device_id: &str,
        _now_ms: u64,
    ) -> Result<(), MeshError> {
        Err(MeshError::NotPaired)
    }
}
