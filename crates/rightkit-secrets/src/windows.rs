//! Windows Credential Manager backend (generic credentials, per-user, persisted locally).
//! Credential Manager never prompts. It has no attribute-only query, so presence reads the
//! credential through `CredReadW` and drops it without copying the blob.
//!
//! Credential Manager has no atomic "write if absent" or "delete if equal", so every mutation
//! (`set`, `add_if_absent`, `delete`, `delete_if_equals`) runs under a named mutex keyed on the
//! normalised native target name (see `locked.rs`); reads take no lock. The target name is the
//! validated one (`Namespace::windows_target_checked`): no NUL or control character, within the
//! native length limit.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::keyring_compat;
use crate::locked::{self, CredentialOps};
use crate::namespace::{check_target, BlobCodec, Namespace};
use crate::secret::Secret;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

/// How long a mutation waits for another writer of the same target before giving up.
const LOCK_WAIT_MS: u32 = 10_000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn code(op: &'static str, err: u32) -> Error {
    Error::Backend {
        op,
        code: err as i64,
    }
}

pub struct CredentialManagerBackend;

/// The named mutex for one normalised native target: `Local\rightkit-secrets-t-<hash>`. `Local\`
/// scopes it to the logon session, which is also the scope of Credential Manager entries.
fn lock_name(normalized_target: &str) -> Result<Vec<u16>> {
    let input = format!("rightkit-secrets/target\0{normalized_target}");
    let mut digest = [0u8; 32];
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            std::ptr::null(),
            0,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(Error::Backend {
            op: "lock hash",
            code: status as i64,
        });
    }
    let suffix: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(wide(&format!("Local\\rightkit-secrets-t-{suffix}")))
}

/// Held while one thread mutates one target. Named mutexes are owned by a thread, so the guard
/// is released on the thread that took it (the worker thread of one `Store` call).
pub(crate) struct TargetLock {
    handle: HANDLE,
    acquired: bool,
}

impl TargetLock {
    fn acquire(normalized_target: &str, wait_ms: u32) -> Result<Self> {
        let name = lock_name(normalized_target)?;
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Err(code("lock create", GetLastError()));
            }
            // `guard` closes the handle on every path below.
            let mut guard = Self {
                handle,
                acquired: false,
            };
            match WaitForSingleObject(handle, wait_ms) {
                WAIT_OBJECT_0 | WAIT_ABANDONED => {
                    guard.acquired = true;
                    Ok(guard)
                }
                WAIT_TIMEOUT => Err(Error::Timeout {
                    op: "lock",
                    secs: u64::from(wait_ms / 1000),
                }),
                WAIT_FAILED => Err(code("lock wait", GetLastError())),
                other => Err(code("lock wait", other)),
            }
        }
    }
}

impl Drop for TargetLock {
    fn drop(&mut self) {
        unsafe {
            if self.acquired {
                ReleaseMutex(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

/// The unlocked Credential Manager primitives. Callers hold the target lock (`locked.rs`).
struct Win32;

impl CredentialOps for Win32 {
    type Guard = TargetLock;

    fn lock(&self, normalized_target: &str) -> Result<TargetLock> {
        TargetLock::acquire(normalized_target, LOCK_WAIT_MS)
    }

    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
        let name = wide(&ns.windows_target_checked(account)?);
        unsafe {
            let mut p: *mut CREDENTIALW = std::ptr::null_mut();
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
                let e = GetLastError();
                return if e == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(code("read", e))
                };
            }
            if p.is_null() {
                return Err(Error::Backend {
                    op: "read",
                    code: 0,
                });
            }
            let out: Result<Secret> = if want_blob {
                let c = &*p;
                let raw = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                        .to_vec()
                };
                let raw = zeroize::Zeroizing::new(raw);
                match ns.blob_codec() {
                    BlobCodec::KeyringUtf16 => keyring_compat::decode(&raw).map(Secret::from_bytes),
                    BlobCodec::Raw | BlobCodec::LegacyUtf8 => Ok(Secret::from_bytes(raw.to_vec())),
                }
            } else {
                Ok(Secret::from_bytes(Vec::new()))
            };
            CredFree(p as _);
            out.map(Some)
        }
    }

    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()> {
        let name = wide(&ns.windows_target_checked(account)?);
        let user = wide(account);
        let blob = zeroize::Zeroizing::new(match ns.blob_codec() {
            BlobCodec::Raw => secret.expose_bytes().to_vec(),
            BlobCodec::KeyringUtf16 => keyring_compat::encode(secret.expose_bytes())?,
            BlobCodec::LegacyUtf8 => {
                return Err(Error::Invalid {
                    what: "windows target",
                    reason: "KeyringCompatLegacyUtf8 is a read-only migration view; write \
                             through KeyringCompat"
                        .into(),
                })
            }
        });
        unsafe {
            let mut cred: CREDENTIALW = std::mem::zeroed();
            cred.Type = CRED_TYPE_GENERIC;
            cred.TargetName = name.as_ptr() as *mut u16;
            cred.UserName = user.as_ptr() as *mut u16;
            cred.CredentialBlobSize = blob.len() as u32;
            cred.CredentialBlob = blob.as_ptr() as *mut u8;
            cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
            if CredWriteW(&cred, 0) == 0 {
                return Err(code("set", GetLastError()));
            }
        }
        Ok(())
    }

    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool> {
        let name = wide(&ns.windows_target_checked(account)?);
        unsafe {
            if CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) == 0 {
                let e = GetLastError();
                return if e == ERROR_NOT_FOUND {
                    Ok(false)
                } else {
                    Err(code("delete", e))
                };
            }
        }
        Ok(true)
    }
}

impl Backend for CredentialManagerBackend {
    fn name(&self) -> &'static str {
        "windows-credential-manager"
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Win32.read(ns, account, true)
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Ok(Win32.read(ns, account, false)?.is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        check_target(ns, account)?;
        locked::set(&Win32, ns, account, secret)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        locked::add_if_absent(&Win32, ns, account, secret)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        locked::delete(&Win32, ns, account)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        locked::delete_if_equals(&Win32, ns, account, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::{normalize_target_name, WindowsTarget};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique(tag: &str) -> String {
        format!(
            "rk-secrets-unit-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn the_lock_is_named_by_the_normalised_target_not_by_the_service() {
        let a = Namespace::new("svc-a")
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some("Shared".into()),
            });
        let b = Namespace::new("svc-b")
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some("SHARED".into()),
            });
        let name_a = lock_name(&normalize_target_name(
            &a.windows_target_checked("acct").unwrap(),
        ))
        .unwrap();
        let name_b = lock_name(&normalize_target_name(
            &b.windows_target_checked("acct").unwrap(),
        ))
        .unwrap();
        assert_eq!(name_a, name_b);
        let other = lock_name(&normalize_target_name(
            &a.windows_target_checked("other").unwrap(),
        ))
        .unwrap();
        assert_ne!(name_a, other);
    }

    /// Audit finding: `set` and `delete` took no lock, so they ran beside `add_if_absent`.
    #[test]
    fn a_second_thread_cannot_take_a_target_while_another_holds_it() {
        let target = normalize_target_name(&format!("{}/acct", unique("held")));
        let held = TargetLock::acquire(&target, 1_000).unwrap();
        let (tx, rx) = mpsc::channel();
        let name = target.clone();
        let contender = std::thread::spawn(move || {
            tx.send(TargetLock::acquire(&name, 200).map(|_| ()))
                .unwrap();
        });
        let outcome = rx.recv().unwrap();
        contender.join().unwrap();
        assert!(
            matches!(outcome, Err(Error::Timeout { op: "lock", .. })),
            "{outcome:?}"
        );
        drop(held);
        // Free again once released.
        let name = target.clone();
        std::thread::spawn(move || TargetLock::acquire(&name, 1_000).map(|_| ()))
            .join()
            .unwrap()
            .unwrap();
    }
}
