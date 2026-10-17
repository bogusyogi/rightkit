//! Installation identity, startup claim and clone detection (Orthic-Labs/rightkit#420).
//!
//! The identity file records the machine binding it was minted on. When the stored binding
//! differs from the current one, the data directory was copied to another machine (a clone): the
//! store mints a fresh installation id and reports the previous one so the app can re-register.

use crate::util::{hex, random_bytes, sha256};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationIdentity {
    pub installation_id: String,
    pub machine_binding: String,
    pub created_unix: u64,
    /// 1 for the first identity on a data directory; +1 for each clone re-mint.
    pub generation: u64,
    /// Append-only startup counter: advanced under the identity lock by every successful
    /// startup claim and never decreased. A claim that records a value this identity did not
    /// write exposes a copied directory or a restored snapshot.
    #[serde(default)]
    pub startup_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupClaim {
    pub installation_id: String,
    pub pid: u32,
    pub claimed_unix: u64,
    pub nonce: String,
    /// The identity `startup_generation` this claim advanced to.
    #[serde(default)]
    pub startup_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup {
    Fresh(InstallationIdentity),
    Resumed(InstallationIdentity),
    CloneDetected {
        previous_installation_id: String,
        previous_binding: String,
        identity: InstallationIdentity,
    },
}

impl Startup {
    pub fn identity(&self) -> &InstallationIdentity {
        match self {
            Startup::Fresh(i) | Startup::Resumed(i) => i,
            Startup::CloneDetected { identity, .. } => identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Acquired,
    /// A claim from a process that is gone remained: the previous run did not release it.
    PreviousUncleanExit(StartupClaim),
    /// A live process holds the claim.
    HeldByLiveProcess(StartupClaim),
    /// The claim on disk records a startup generation this identity did not write (a copied
    /// directory or a restored snapshot). The colliding claim was quarantined beside the
    /// identity and a fresh identity was minted and claimed.
    CloneDetected {
        previous_installation_id: String,
        quarantined_claim: StartupClaim,
        identity: InstallationIdentity,
    },
}

/// Hash a raw machine identifier (hardware UUID, machine-id, MachineGuid) so the raw value is
/// never stored.
pub fn machine_binding_hash(raw: &str) -> String {
    let mut data = b"rightkit-identity/machine/v1\0".to_vec();
    data.extend_from_slice(raw.trim().as_bytes());
    hex(&sha256(&data))
}

/// Normalise a host name for display: control characters dropped, whitespace collapsed, a
/// `.local` / `.lan` suffix removed, at most 64 characters, `This device` when nothing remains.
pub fn host_display_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter_map(|c| {
            if c.is_whitespace() {
                Some(' ')
            } else if c.is_control() {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    for suffix in [".local", ".lan"] {
        if name.to_ascii_lowercase().ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
        }
    }
    let name: String = name.chars().take(64).collect();
    let name = name.trim().to_string();
    if name.is_empty() {
        "This device".to_string()
    } else {
        name
    }
}

pub struct InstallationStore {
    dir: PathBuf,
}

impl InstallationStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        InstallationStore { dir: dir.into() }
    }

    fn identity_path(&self) -> PathBuf {
        self.dir.join("installation-identity.json")
    }

    fn claim_path(&self) -> PathBuf {
        self.dir.join("startup-claim.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("installation.lock")
    }

    /// Exclusive transaction lock over identity and claim files. `flock` excludes other
    /// descriptors in this process as well as other processes.
    fn lock(&self) -> Result<rightkit_fs::FileLock> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::Io(e.to_string()))?;
        rightkit_fs::FileLock::acquire_kind(
            &self.lock_path(),
            rightkit_fs::LockKind::Flock,
            &rightkit_fs::LockOptions::wait(std::time::Duration::from_secs(10))
                .labeled("installation-identity"),
        )
        .map_err(|e| Error::Io(e.to_string()))
    }

    /// Load the identity, minting it on first run and re-minting it on a machine-binding change.
    /// `machine_binding` comes from [`machine_binding_hash`]. The whole read-decide-write runs
    /// under an exclusive file lock, so concurrent first runs agree on one identity.
    pub fn load_or_create(&self, machine_binding: &str, now_unix: u64) -> Result<Startup> {
        let _lock = self.lock()?;
        match read_json::<InstallationIdentity>(&self.identity_path())? {
            None => {
                let id = mint(machine_binding, now_unix, 1)?;
                self.write_identity(&id)?;
                Ok(Startup::Fresh(id))
            }
            Some(stored) if stored.machine_binding == machine_binding => {
                Ok(Startup::Resumed(stored))
            }
            Some(stored) => {
                let id = mint(machine_binding, now_unix, stored.generation + 1)?;
                self.write_identity(&id)?;
                Ok(Startup::CloneDetected {
                    previous_installation_id: stored.installation_id,
                    previous_binding: stored.machine_binding,
                    identity: id,
                })
            }
        }
    }

    fn write_identity(&self, id: &InstallationIdentity) -> Result<()> {
        write_json(&self.identity_path(), id)
    }

    /// The claim currently on disk, for reading back its nonce after [`Self::claim_startup`].
    pub fn read_claim(&self) -> Result<Option<StartupClaim>> {
        let _lock = self.lock()?;
        read_json::<StartupClaim>(&self.claim_path())
    }

    /// Claim startup for this process. `is_alive` reports whether a pid is running (for example
    /// `rightkit_fs::process_alive`). A claim by this same pid is replaced.
    ///
    /// Under the exclusive lock the claim advances the identity's append-only
    /// `startup_generation` and records it. A claim whose generation differs from the
    /// identity's while its process is live, or exceeds it once the process is gone (identity
    /// rolled back by a snapshot), is a collision: the claim is quarantined, a fresh identity is
    /// minted and [`ClaimOutcome::CloneDetected`] is returned.
    pub fn claim_startup(
        &self,
        installation_id: &str,
        pid: u32,
        now_unix: u64,
        is_alive: impl Fn(u32) -> bool,
    ) -> Result<ClaimOutcome> {
        let _lock = self.lock()?;
        let existing = read_json::<StartupClaim>(&self.claim_path())?;
        let identity = read_json::<InstallationIdentity>(&self.identity_path())?
            .filter(|i| i.installation_id == installation_id);
        let id_gen = identity
            .as_ref()
            .map(|i| i.startup_generation)
            .or(existing.as_ref().map(|c| c.startup_generation))
            .unwrap_or(0);
        let mut collision = None;
        let outcome = match existing {
            None => ClaimOutcome::Acquired,
            Some(c) if c.pid == pid && c.startup_generation == id_gen => ClaimOutcome::Acquired,
            Some(c) if c.pid != pid && is_alive(c.pid) && c.startup_generation == id_gen => {
                return Ok(ClaimOutcome::HeldByLiveProcess(c))
            }
            Some(c) if c.installation_id == installation_id && identity.is_some() => {
                let live = c.pid != pid && is_alive(c.pid);
                if (live && c.startup_generation != id_gen) || c.startup_generation > id_gen {
                    collision = Some(c);
                    ClaimOutcome::Acquired
                } else {
                    ClaimOutcome::PreviousUncleanExit(c)
                }
            }
            Some(c) => ClaimOutcome::PreviousUncleanExit(c),
        };
        let nonce = hex(&random_bytes::<16>()?);
        if let Some(bad) = collision {
            let stored = identity.expect("collision implies identity");
            let mut quarantine = self.dir.join(format!(
                "startup-claim.quarantined-g{}-p{}.json",
                bad.startup_generation, bad.pid
            ));
            let mut n = 0;
            while quarantine.exists() {
                n += 1;
                quarantine = self.dir.join(format!(
                    "startup-claim.quarantined-g{}-p{}-{n}.json",
                    bad.startup_generation, bad.pid
                ));
            }
            std::fs::rename(self.claim_path(), &quarantine)
                .map_err(|e| Error::Io(e.to_string()))?;
            let mut fresh = mint(&stored.machine_binding, now_unix, stored.generation + 1)?;
            fresh.startup_generation = stored.startup_generation.max(bad.startup_generation) + 1;
            self.write_identity(&fresh)?;
            let claim = StartupClaim {
                installation_id: fresh.installation_id.clone(),
                pid,
                claimed_unix: now_unix,
                nonce,
                startup_generation: fresh.startup_generation,
            };
            write_json(&self.claim_path(), &claim)?;
            return Ok(ClaimOutcome::CloneDetected {
                previous_installation_id: stored.installation_id,
                quarantined_claim: bad,
                identity: fresh,
            });
        }
        let generation = match identity {
            Some(mut i) => {
                i.startup_generation = i
                    .startup_generation
                    .checked_add(1)
                    .ok_or(Error::Refused("startup generation overflow"))?;
                self.write_identity(&i)?;
                i.startup_generation
            }
            None => id_gen + 1,
        };
        let claim = StartupClaim {
            installation_id: installation_id.to_string(),
            pid,
            claimed_unix: now_unix,
            nonce,
            startup_generation: generation,
        };
        write_json(&self.claim_path(), &claim)?;
        Ok(outcome)
    }

    /// Release the claim on clean shutdown. Only the claiming pid removes it.
    pub fn release_claim(&self, pid: u32) -> Result<bool> {
        let _lock = self.lock()?;
        match read_json::<StartupClaim>(&self.claim_path())? {
            Some(c) if c.pid == pid => self.remove_claim(),
            _ => Ok(false),
        }
    }

    /// Release the claim only when pid and nonce both match the claim on disk, under the lock.
    /// A newer claim by a successor is never removed by a stale holder.
    pub fn release_claim_nonce(&self, pid: u32, nonce: &str) -> Result<bool> {
        let _lock = self.lock()?;
        match read_json::<StartupClaim>(&self.claim_path())? {
            Some(c) if c.pid == pid && c.nonce == nonce => self.remove_claim(),
            _ => Ok(false),
        }
    }

    fn remove_claim(&self) -> Result<bool> {
        std::fs::remove_file(self.claim_path()).map_err(|e| Error::Io(e.to_string()))?;
        Ok(true)
    }
}

fn mint(binding: &str, now: u64, generation: u64) -> Result<InstallationIdentity> {
    Ok(InstallationIdentity {
        installation_id: hex(&random_bytes::<16>()?),
        machine_binding: binding.to_string(),
        created_unix: now,
        generation,
        startup_generation: 0,
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e.to_string())),
    }
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| Error::Corrupt(e.to_string()))?;
    rightkit_fs::atomic_write_private(path, &bytes).map_err(|e| Error::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_resume_clone() {
        let dir = tempfile::tempdir().unwrap();
        let s = InstallationStore::new(dir.path());
        let a = machine_binding_hash("machine-a");
        let b = machine_binding_hash("machine-b");
        let first = s.load_or_create(&a, 10).unwrap();
        assert!(matches!(first, Startup::Fresh(_)));
        let again = s.load_or_create(&a, 20).unwrap();
        assert_eq!(again.identity(), first.identity());
        assert!(matches!(again, Startup::Resumed(_)));
        match s.load_or_create(&b, 30).unwrap() {
            Startup::CloneDetected {
                previous_installation_id,
                identity,
                ..
            } => {
                assert_eq!(previous_installation_id, first.identity().installation_id);
                assert_ne!(identity.installation_id, previous_installation_id);
                assert_eq!(identity.generation, 2);
            }
            other => panic!("expected clone, got {other:?}"),
        }
    }

    #[test]
    fn claim_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let s = InstallationStore::new(dir.path());
        assert_eq!(
            s.claim_startup("i", 100, 1, |_| false).unwrap(),
            ClaimOutcome::Acquired
        );
        assert!(matches!(
            s.claim_startup("i", 200, 2, |_| true).unwrap(),
            ClaimOutcome::HeldByLiveProcess(_)
        ));
        assert!(matches!(
            s.claim_startup("i", 200, 3, |_| false).unwrap(),
            ClaimOutcome::PreviousUncleanExit(_)
        ));
        assert!(!s.release_claim(100).unwrap());
        assert!(s.release_claim(200).unwrap());
    }

    #[test]
    fn nonce_release_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let s = InstallationStore::new(dir.path());
        let id = match s.load_or_create(&machine_binding_hash("m"), 1).unwrap() {
            Startup::Fresh(i) => i,
            o => panic!("{o:?}"),
        };
        assert_eq!(
            s.claim_startup(&id.installation_id, 100, 2, |_| false)
                .unwrap(),
            ClaimOutcome::Acquired
        );
        let c = s.read_claim().unwrap().unwrap();
        assert_eq!(c.startup_generation, 1);
        assert!(!s.release_claim_nonce(100, "wrong").unwrap());
        assert!(s.release_claim_nonce(100, &c.nonce).unwrap());
        s.claim_startup(&id.installation_id, 101, 3, |_| false)
            .unwrap();
        assert_eq!(s.read_claim().unwrap().unwrap().startup_generation, 2);
    }

    #[test]
    fn rolled_back_identity_is_clone() {
        let dir = tempfile::tempdir().unwrap();
        let s = InstallationStore::new(dir.path());
        let id = s
            .load_or_create(&machine_binding_hash("m"), 1)
            .unwrap()
            .identity()
            .clone();
        s.claim_startup(&id.installation_id, 100, 2, |_| false)
            .unwrap();
        // A snapshot restore rolls the identity back while the newer claim survives.
        std::fs::write(
            dir.path().join("installation-identity.json"),
            serde_json::to_vec(&id).unwrap(),
        )
        .unwrap();
        match s
            .claim_startup(&id.installation_id, 200, 3, |_| false)
            .unwrap()
        {
            ClaimOutcome::CloneDetected {
                previous_installation_id,
                identity,
                ..
            } => {
                assert_eq!(previous_installation_id, id.installation_id);
                assert_ne!(identity.installation_id, id.installation_id);
            }
            o => panic!("{o:?}"),
        }
        let quarantined = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("quarantined"));
        assert!(quarantined);
    }

    #[test]
    fn concurrent_first_run_agrees() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let b = machine_binding_hash("m");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (p, b) = (path.clone(), b.clone());
                std::thread::spawn(move || {
                    InstallationStore::new(p)
                        .load_or_create(&b, 1)
                        .unwrap()
                        .identity()
                        .installation_id
                        .clone()
                })
            })
            .collect();
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(ids.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn display_name() {
        assert_eq!(host_display_name("  Adrian's\tMac.local "), "Adrian's Mac");
        assert_eq!(host_display_name("\u{7}"), "This device");
    }
}
