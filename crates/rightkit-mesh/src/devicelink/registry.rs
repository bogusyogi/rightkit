//! The paired-device registry file.
//!
//! The record types live in `rightkit-device-link-types` and are re-exported here. The registry
//! holds public keys, grants and state only; there is no secret in it. Every change is a
//! read-modify-write under an exclusive file lock (`registry.lock` beside the file), so two
//! processes writing at once keep both updates. Every authorization re-reads the file, so a
//! revocation made by another process (the CLI, a UI) takes effect on the next request.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub use rightkit_device_link_types::registry::{
    GUEST_IDLE_MS, GUEST_LEASE_DEFAULT_MS, GUEST_LEASE_MAX_MS, GUEST_REFUSED, GUEST_SEEN_WRITE_MS,
    GuestPass, MemberRole, PairedDevice, PairedState, REGISTRY_SCHEMA_VERSION, RedeemedPairing,
    Registry,
};

use super::{DEVICE_LINK_SCHEMA_VERSION, LinkError};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The registry file. Owner-only, replaced atomically and durably.
#[derive(Clone, Debug)]
pub struct RegistryStore {
    path: PathBuf,
}

impl RegistryStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// A missing file is an empty registry. An unreadable or unknown-version file is an
    /// error: guessing could drop a revocation.
    pub fn load(&self) -> Result<Registry, LinkError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(e) => return Err(LinkError::Store(e.to_string())),
        };
        let registry: Registry =
            serde_json::from_slice(&bytes).map_err(|e| LinkError::Store(e.to_string()))?;
        if registry.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(LinkError::SchemaVersion(registry.schema_version));
        }
        if registry
            .devices
            .iter()
            .any(|d| d.schema_version == 0 || d.schema_version > DEVICE_LINK_SCHEMA_VERSION)
        {
            return Err(LinkError::Store("unknown record version".to_string()));
        }
        Ok(registry)
    }

    /// A guest's successful call: renews its idle window and "last seen" in the host app > Guests.
    /// Reads first and writes only when the stored time is older than `GUEST_SEEN_WRITE_MS`, so a
    /// busy guest costs one read per call and one write every five minutes. A pending, revoked or
    /// unknown record is left alone. Returns whether a write happened.
    pub fn note_guest_seen(&self, device_id: &str) -> bool {
        let now = now_ms();
        let stale = self.load().is_ok_and(|registry| {
            registry.devices.iter().any(|d| {
                d.device.device_id == device_id
                    && d.is_unrevoked()
                    && d.guest
                        .as_ref()
                        .is_some_and(|g| g.admits(now) && g.seen_is_stale(now))
            })
        });
        if !stale {
            return false;
        }
        self.update(|registry| {
            if let Some(pass) = registry
                .devices
                .iter_mut()
                .find(|d| d.device.device_id == device_id && d.is_unrevoked())
                .and_then(|d| d.guest.as_mut())
            {
                pass.last_seen_ms = Some(now);
            }
            Ok(())
        })
        .is_ok()
    }

    /// Read, change and write the registry under an exclusive file lock shared by every process
    /// that writes it. Bumps the registry revision on success; a refused change writes nothing.
    /// A lock that stays busy for three seconds is an error, never a write without the lock.
    pub fn update<R>(
        &self,
        change: impl FnOnce(&mut Registry) -> Result<R, LinkError>,
    ) -> Result<R, LinkError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LinkError::Store(e.to_string()))?;
        }
        let options = rightkit_fs::LockOptions {
            timeout: Duration::from_secs(3),
            poll: Duration::from_millis(20),
            label: "device-registry".into(),
        };
        let _lock = rightkit_fs::FileLock::acquire(&self.path.with_extension("lock"), &options)
            .map_err(|_| LinkError::Store("the registry is busy in another process".to_string()))?;
        let mut registry = self.load()?;
        let out = change(&mut registry)?;
        registry.revision += 1;
        self.save(&registry)?;
        Ok(out)
    }

    fn save(&self, registry: &Registry) -> Result<(), LinkError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LinkError::Store(e.to_string()))?;
        }
        let bytes =
            serde_json::to_vec_pretty(registry).map_err(|e| LinkError::Store(e.to_string()))?;
        rightkit_fs::atomic_write_with_options(
            &self.path,
            rightkit_fs::AtomicWriteOptions {
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| std::io::Write::write_all(file, &bytes),
        )
        .map_err(|e| LinkError::Store(e.to_string()))
    }
}
